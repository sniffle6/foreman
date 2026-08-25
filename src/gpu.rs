//! GPU device-loss handling: detection flags, the crash-log file, the pure
//! crash-loop guard, and the ordered self-restart. See `docs/gpu-device-loss.md`.
//!
//! The short version: waking from sleep removes the GPU device; wgpu marks the
//! `Device` permanently invalid and every later staging-buffer allocation fails.
//! Stock egui-wgpu `panic!`s on that; our `vendor/egui-wgpu` fork instead sets a
//! sticky flag and limps through the frame, and `App::logic` (src/main.rs) sees
//! the flag on the next frame, saves the workspace and respawns the process.
//!
//! Nothing here is GPU-specific in the "talks to wgpu" sense — it is all
//! atomics and file I/O, so it is unit-testable and safe to call from a panic
//! hook.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Set by `Device::set_device_lost_callback` (an EARLY HINT — wgpu-core
/// documents the closure "might never be called", and `device.destroy()` never
/// fires it at all). Never the sole detector.
static LOST_HINT: AtomicBool = AtomicBool::new(false);
/// True once the first frame has been fully set up. Gates the panic hook's
/// respawn so unit-test panics and worker-thread panics can never fork a GUI,
/// and cleared on every deliberate quit path.
static HANDOFF_ARMED: AtomicBool = AtomicBool::new(false);
/// CAS'd by [`respawn`] so two threads racing (paint thread + panic hook) spawn
/// at most one successor.
static RESPAWNED: AtomicBool = AtomicBool::new(false);
/// This process was started by a device-loss restart (drives the toast only).
pub static FROM_GPU_RESTART: AtomicBool = AtomicBool::new(false);

pub fn mark_lost_hint() {
    LOST_HINT.store(true, Ordering::Release);
}

pub fn arm_handoff() {
    HANDOFF_ARMED.store(true, Ordering::Release);
}

pub fn disarm_handoff() {
    HANDOFF_ARMED.store(false, Ordering::Release);
}

pub fn handoff_armed() -> bool {
    HANDOFF_ARMED.load(Ordering::Acquire)
}

/// AUTHORITATIVE device-lost predicate: the fork's flag (set at the actual
/// failure site in `Renderer::update_buffers`) OR the callback hint.
pub fn lost() -> bool {
    eframe::egui_wgpu::device_lost() || LOST_HINT.load(Ordering::Acquire)
}

/// The crash/evidence log: `%APPDATA%\foreman\foreman_panic.log`. Falls back to
/// a CWD-relative file when `APPDATA` is unset — better than losing the record.
pub fn crash_log_path() -> PathBuf {
    crash_log_path_in(crate::config::config_dir())
}

/// Pure seam for [`crash_log_path`], so the fallback is testable without
/// mutating the process environment (`set_var` is `unsafe` in edition 2024 and
/// racy against parallel tests).
fn crash_log_path_in(dir: Option<PathBuf>) -> PathBuf {
    dir.map(|d| d.join(LOG_FILE))
        .unwrap_or_else(|| PathBuf::from(LOG_FILE))
}

const LOG_FILE: &str = "foreman_panic.log";

/// Append one line, best effort. Never panics — it runs on unwinding threads.
pub fn append_log(path: &Path, line: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = f.write_all(line.as_bytes());
        let _ = f.flush();
    }
}

/// Append one timestamped evidence line to the crash log. Local time, matching
/// how Kernel-Power sleep/resume events are read in Event Viewer.
pub fn log_line(msg: &str) {
    let ts = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z");
    append_log(&crash_log_path(), &format!("=== foreman {ts} === {msg}\n"));
}

/// Where the crash-loop guard's record lives (`%APPDATA%\foreman\`).
pub const CRASH_FILE: &str = "gpu-crash.json";
/// Losses inside this window count against [`RESPAWN_MAX`].
pub const RESPAWN_WINDOW_MS: u64 = 600_000; // 10 minutes
/// After this many losses in one window we stop restarting and stay dead, so a
/// genuinely broken GPU does not spin up an unbounded chain of processes.
pub const RESPAWN_MAX: u32 = 3;

/// The persisted crash-loop record. Deliberately NOT reset by a long-lived
/// successor: decay is by the time window only, so a slow crash chain (a child
/// that lives minutes, then loses the device, forever) is still caught.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct CrashLog {
    pub version: u32,
    pub last_unix_ms: u64,
    pub count: u32,
}

/// Pure: returns `(allow_respawn, record_to_persist)`. Outside the window the
/// count resets to 1; inside it increments and is capped at [`RESPAWN_MAX`].
/// Clock skew (`now_ms < last_unix_ms`) is treated as "outside the window" —
/// a resume can resync the clock backwards, and punishing the user for that
/// would be exactly the wrong call on the one path that matters.
pub fn decide(prev: &CrashLog, now_ms: u64) -> (bool, CrashLog) {
    let fresh = prev.version != 1
        || now_ms < prev.last_unix_ms
        || now_ms - prev.last_unix_ms > RESPAWN_WINDOW_MS;
    let count = if fresh {
        1
    } else {
        prev.count.saturating_add(1)
    };
    (
        count <= RESPAWN_MAX,
        CrashLog {
            version: 1,
            last_unix_ms: now_ms,
            count,
        },
    )
}

/// Spawn a successor process through the same handshake the one-click update
/// uses (`FOREMAN_WAIT_PID`: the child waits, bounded, for us to exit before
/// touching the control pipe). Returns whether a successor was actually
/// spawned; the caller exits either way.
///
/// Deliberately NOT refactored to share code with `App::restart_for_update` —
/// that path is live-verified in production and is not worth destabilizing for
/// eight shared lines.
pub fn respawn(reason: &str) -> bool {
    if RESPAWNED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return false; // another thread already spawned the successor
    }
    let now_ms = chrono::Local::now().timestamp_millis().max(0) as u64;
    let prev: CrashLog = crate::config::load_json(CRASH_FILE);
    let (allow, rec) = decide(&prev, now_ms);
    let _ = crate::config::save_json(CRASH_FILE, &rec);
    if !allow {
        log_line(&format!(
            "{reason}: NOT respawning — {} device losses within {}s",
            rec.count,
            RESPAWN_WINDOW_MS / 1000
        ));
        return false;
    }
    let Ok(exe) = std::env::current_exe() else {
        log_line(&format!("{reason}: current_exe() failed; not respawning"));
        return false;
    };
    match std::process::Command::new(&exe)
        .env("FOREMAN_WAIT_PID", std::process::id().to_string())
        .env("FOREMAN_GPU_RESTART", "1")
        .spawn()
    {
        Ok(_) => {
            log_line(&format!(
                "{reason}: respawned (loss {} of {RESPAWN_MAX} in window)",
                rec.count
            ));
            true
        }
        Err(e) => {
            log_line(&format!("{reason}: respawn failed: {e}"));
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(count: u32, last_unix_ms: u64) -> CrashLog {
        CrashLog {
            version: 1,
            last_unix_ms,
            count,
        }
    }

    #[test]
    fn decide_starts_a_fresh_window_on_a_missing_record() {
        // `load_json` hands us `CrashLog::default()` (version 0) when the file
        // is absent or corrupt — that must read as "first loss", not "unknown".
        let (allow, next) = decide(&CrashLog::default(), 1_000_000);
        assert!(allow);
        assert_eq!(next, rec(1, 1_000_000));
    }

    #[test]
    fn decide_counts_up_inside_the_window_then_refuses() {
        let now = 1_000_000;
        for c in 1..RESPAWN_MAX {
            let (allow, next) = decide(&rec(c, now), now + 1_000);
            assert!(allow, "loss {} of {RESPAWN_MAX} must still respawn", c + 1);
            assert_eq!(next.count, c + 1);
        }
        // The (MAX+1)-th loss inside the window is the one we refuse. The
        // record still advances, so the window keeps sliding forward while the
        // failures keep coming.
        let (allow, next) = decide(&rec(RESPAWN_MAX, now), now + 1_000);
        assert!(!allow);
        assert_eq!(next.count, RESPAWN_MAX + 1);
    }

    #[test]
    fn decide_resets_after_the_window_elapses() {
        let now = 1_000_000;
        let (allow, next) = decide(&rec(RESPAWN_MAX, now), now + RESPAWN_WINDOW_MS + 1);
        assert!(allow);
        assert_eq!(next, rec(1, now + RESPAWN_WINDOW_MS + 1));
    }

    #[test]
    fn decide_treats_a_backwards_clock_as_a_fresh_window() {
        // A resume can resync the wall clock backwards by minutes (that is how
        // the original panics were dated). Never punish the user for it.
        let now = 1_000_000;
        let (allow, next) = decide(&rec(RESPAWN_MAX, now), now - 5_000);
        assert!(allow);
        assert_eq!(next.count, 1);
    }

    #[test]
    fn crash_log_path_prefers_the_config_dir_and_falls_back_to_cwd() {
        let dir = std::path::PathBuf::from(r"C:\fake\foreman");
        assert_eq!(crash_log_path_in(Some(dir.clone())), dir.join(LOG_FILE));
        // APPDATA unset (config_dir() == None): a bare relative name, so the
        // evidence still lands somewhere rather than being dropped.
        assert_eq!(crash_log_path_in(None), std::path::PathBuf::from(LOG_FILE));
    }

    #[test]
    fn append_log_creates_then_appends() {
        let dir = std::env::temp_dir().join("foreman-test-gpu-crash-log");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("append.log");
        let _ = std::fs::remove_file(&path);
        append_log(&path, "one\n");
        append_log(&path, "two\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\ntwo\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
