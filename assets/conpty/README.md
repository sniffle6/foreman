# Sideloaded ConPTY host

`conpty.dll` + `OpenConsole.exe` are Microsoft's open-source console host
(MIT, https://github.com/microsoft/terminal), as vendored and built by the
WezTerm project (https://github.com/wezterm/wezterm, assets/windows/conhost,
"update bundled conpty build", 2025-02-08).

Why: the in-box Windows conhost strips kitty graphics APC sequences inside
ConPTY, so terminal images can never reach foreman through the system PTY.
This post-rearchitecture build (Windows Terminal 1.22+, "applications can
send unmodified VT directly to the terminal") passes them through.
portable-pty prefers a `conpty.dll` found beside the exe over kernel32 —
`src/conpty_install.rs` drops these two files there at startup.

Proof/regression: `cargo test --release conpty_passes -- --ignored`
(requires the pair beside the test exe in target/release/deps).
