fn main() {
    // Embed the app icon as a PE resource (Explorer / Start / taskbar read
    // it from the exe; the runtime egui icon only covers the live window).
    // windres directly: winres 0.1 "succeeded" on the GNU toolchain without
    // ever linking a .rsrc section (verified by PE section dump), so the
    // object is compiled and handed to the linker explicitly instead. The
    // toolchain is GNU-only by project requirement (CLAUDE.md), so windres
    // is always present (w64devkit locally, mingw64 on CI).
    println!("cargo:rerun-if-changed=foreman.rc");
    println!("cargo:rerun-if-changed=assets/icons/app-icon.ico");
    let out = std::env::var("OUT_DIR").unwrap();
    let obj = format!("{out}/foreman-res.o");
    match std::process::Command::new("windres")
        .args(["foreman.rc", "-O", "coff", "-o", &obj])
        .status()
    {
        Ok(s) if s.success() => println!("cargo:rustc-link-arg={obj}"),
        other => println!("cargo:warning=windres icon embed failed: {other:?}"),
    }
}
