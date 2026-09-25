//! Console-subsystem front door for the CLI, shipped as `foreman.com` next to
//! the GUI-subsystem `foreman.exe`. PowerShell and cmd resolve `foreman` to
//! `.com` first (PATHEXT order) and only wait for console executables, so
//! this shim is what gives CLI verbs a waited-on process and `$LASTEXITCODE`.
//! Shortcuts and double-clicks target `foreman.exe` and never touch it, so
//! launching the app never opens a console window.

use std::os::windows::process::CommandExt;

// Ctrl+C reaches every process on the console. Swallow it here so the shim
// outlives foreman.exe and still reports its exit code; the child decides
// what Ctrl+C means. A handler (unlike SetConsoleCtrlHandler(NULL, TRUE)) is
// not inherited, so the child keeps default Ctrl+C behavior.
unsafe extern "system" fn ignore_ctrl(_: u32) -> i32 {
    1
}

fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let exe = match std::env::current_exe() {
        Ok(p) => p.with_file_name("foreman.exe"),
        Err(e) => {
            eprintln!("foreman: cannot locate foreman.exe: {e}");
            std::process::exit(1);
        }
    };
    let mut cmd = std::process::Command::new(&exe);
    cmd.args(&args);
    // No verb means "open the app": start it detached and return, the way a
    // GUI executable behaves when typed at a prompt.
    if args.is_empty() {
        cmd.creation_flags(windows_sys::Win32::System::Threading::DETACHED_PROCESS);
        if let Err(e) = cmd.spawn() {
            eprintln!("foreman: cannot start {}: {e}", exe.display());
            std::process::exit(1);
        }
        return;
    }
    unsafe {
        windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(ignore_ctrl), 1);
    }
    match cmd.status() {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("foreman: cannot start {}: {e}", exe.display());
            std::process::exit(1);
        }
    }
}
