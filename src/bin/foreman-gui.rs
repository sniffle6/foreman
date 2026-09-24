//! GUI-subsystem entry point for the console-subsystem foreman executable.
#![windows_subsystem = "windows"]

use std::os::windows::process::CommandExt;

fn main() {
    let result = std::env::current_exe().and_then(|launcher| {
        std::process::Command::new(launcher.with_file_name("foreman.exe"))
            .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
            .spawn()
    });
    if let Err(error) = result {
        let message: Vec<u16> = format!("Could not start Foreman: {error}\0")
            .encode_utf16()
            .collect();
        let title: Vec<u16> = "Foreman\0".encode_utf16().collect();
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
                std::ptr::null_mut(),
                message.as_ptr(),
                title.as_ptr(),
                windows_sys::Win32::UI::WindowsAndMessaging::MB_OK
                    | windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
            );
        }
        std::process::exit(1);
    }
}
