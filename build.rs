fn main() {
    // Embed the app icon + VERSIONINFO as PE resources so Explorer, Start,
    // and the taskbar show the real icon (the runtime egui icon only covers
    // the live window). Soft-fails: a broken windres never blocks the build.
    #[cfg(windows)]
    {
        // NOTE: VERSIONINFO via res.set(...) does not survive the GNU/windres
        // path (verified empty on this toolchain) - icon only, on purpose.
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/icons/app-icon.ico");
        if let Err(e) = res.compile() {
            println!("cargo:warning=icon resource embed failed: {e}");
        }
    }
    println!("cargo:rerun-if-changed=assets/icons/app-icon.ico");
}
