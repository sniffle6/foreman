//! Kill-on-close Job Objects: each `Session` puts its child in one, so
//! dropping the Session — closing the pane, ending a test, quitting the
//! app — takes the child's whole process tree with it, like closing a
//! Windows Terminal tab. Without this every closed pane leaked its shell:
//! an interactive cmd.exe does not exit when its PTY goes away (one debug
//! session found 2,000+ orphaned cmd/conhost pairs, mostly from test runs).

use std::ffi::c_void;
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

/// Owns one kill-on-close job. Dropping it kills every process in the job.
pub struct Job(*mut c_void);

// A HANDLE is a process-global token, not a thread-affine pointer.
unsafe impl Send for Job {}

impl Job {
    /// Create a kill-on-close job and assign `pid`'s process to it; processes
    /// it spawns from then on join automatically. (Anything it spawned in the
    /// microseconds before assignment does not — accepted, same as Windows
    /// Terminal.) Best-effort: `None` means the session simply runs unreaped,
    /// exactly the pre-Job behavior — never a failed spawn.
    pub fn assign(pid: u32) -> Option<Job> {
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            let job = Job(job); // owned from here — early returns close the handle
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) == 0
            {
                return None;
            }
            let proc = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if proc.is_null() {
                return None;
            }
            let ok = AssignProcessToJobObject(job.0, proc);
            CloseHandle(proc);
            (ok != 0).then_some(job)
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // Kill-on-close: this CloseHandle terminates the job's process tree.
        unsafe { CloseHandle(self.0) };
    }
}

/// Test-only: a process handle opened BEFORE the kill, so waiting for death
/// can't be fooled by pid reuse.
#[cfg(test)]
pub struct DeathWatch(*mut c_void);

#[cfg(test)]
impl DeathWatch {
    pub fn open(pid: u32) -> Option<DeathWatch> {
        use windows_sys::Win32::System::Threading::PROCESS_SYNCHRONIZE;
        let h = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        (!h.is_null()).then(|| DeathWatch(h))
    }

    pub fn dead_within_ms(&self, ms: u32) -> bool {
        use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
        use windows_sys::Win32::System::Threading::WaitForSingleObject;
        unsafe { WaitForSingleObject(self.0, ms) == WAIT_OBJECT_0 }
    }
}

#[cfg(test)]
impl Drop for DeathWatch {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropping_the_job_kills_the_assigned_process() {
        // `pause` blocks on the piped stdin we never write to.
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/c", "pause"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("spawn failed");
        let pid = child.id();
        let watch = DeathWatch::open(pid).expect("cannot open watch");
        let job = Job::assign(pid).expect("assign failed");
        assert!(
            !watch.dead_within_ms(300),
            "child died before the job closed"
        );
        drop(job);
        assert!(
            watch.dead_within_ms(5000),
            "job close did not kill the child"
        );
        let _ = child.wait();
    }
}
