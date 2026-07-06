# Sideloaded ConPTY host

`conpty.dll` + `OpenConsole.exe` are Microsoft's open-source console host
(MIT, https://github.com/microsoft/terminal). Current pair: **1.24.2605.12001**,
x64, from the official Microsoft redistributable NuGet package
`Microsoft.Windows.Console.ConPTY`
(https://www.nuget.org/packages/Microsoft.Windows.Console.ConPTY) —
`runtimes/win-x64/native/conpty.dll` + `build/native/runtimes/x64/OpenConsole.exe`.
Both are Authenticode-signed by Microsoft Corporation. Keep them a **matched
pair** (same version) — mixing versions is unsupported.

Why: the in-box Windows conhost strips kitty graphics APC sequences inside
ConPTY, so terminal images can never reach foreman through the system PTY.
This post-rearchitecture build (Windows Terminal 1.22+, "applications can
send unmodified VT directly to the terminal") passes them through.
portable-pty prefers a `conpty.dll` found beside the exe over kernel32 —
`src/conpty_install.rs` drops these two files there at startup.

History: we first vendored WezTerm's **1.22.2502** build (assets/windows/conhost,
2025-02-08). It passes the canary but has a host bug — a fixed **~3s stall on
every PTY spawn** (prompt in ~3.3s vs ~0.25s on in-box conhost). Bumped to
1.24.2605.12001, which fixes the stall and still passes the canary.

Two things to verify whenever you change this pair:
- Passthrough: `cargo test --release conpty_passes -- --ignored`
  (requires the pair beside the test exe in target/release/deps).
- Spawn speed: a fresh terminal must reach its prompt in ~0.25s, not seconds.
