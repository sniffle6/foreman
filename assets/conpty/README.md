# Sideloaded ConPTY host

`conpty.dll` + `OpenConsole.exe` are Microsoft's open-source console host
(MIT, https://github.com/microsoft/terminal). Current pair:
**1.25.2605.12002-preview**, x64, from the official Microsoft redistributable
NuGet package `Microsoft.Windows.Console.ConPTY`
(https://www.nuget.org/packages/Microsoft.Windows.Console.ConPTY/1.25.260512002-preview) —
`runtimes/win-x64/native/conpty.dll` + `build/native/runtimes/x64/OpenConsole.exe`.
Both are Authenticode-signed by Microsoft Corporation. Keep them a **matched
pair** (same version) — mixing versions is unsupported.

Hashes for the vendored x64 files:

- `conpty.dll` (109,880 bytes):
  `78007C7BC710EAF09695C9DB329673470692824F41019ECA283134D263C03FBF`
- `OpenConsole.exe` (1,062,712 bytes):
  `16E087E44E992DACC83433E589A597BC09979BD364E58CAA67A69961C7E5A757`

The official PDB maps this build to microsoft/terminal commit
[`8214f66`](https://github.com/microsoft/terminal/commit/8214f66a61e17cc49025b58629e649225b73abc5).
That lineage contains the demand-triggered post-resize cursor synchronization
from [#19535](https://github.com/microsoft/terminal/pull/19535), the Win32-input
fix [#19620](https://github.com/microsoft/terminal/pull/19620), cursor recovery
after unknown VT such as kitty APC
[#20009](https://github.com/microsoft/terminal/pull/20009), and the active-buffer
follow-up [#20095](https://github.com/microsoft/terminal/pull/20095).

Why: the in-box Windows conhost strips kitty graphics APC sequences inside
ConPTY, so terminal images can never reach foreman through the system PTY.
This post-rearchitecture build (Windows Terminal 1.22+, "applications can
send unmodified VT directly to the terminal") passes them through.
portable-pty prefers a `conpty.dll` found beside the exe over kernel32 —
`src/conpty_install.rs` drops these two files there at startup.

History: we first vendored WezTerm's **1.22.2502** build (assets/windows/conhost,
2025-02-08). It passes the canary but has a host bug — a fixed **~3s stall on
every PTY spawn** (prompt in ~3.3s vs ~0.25s on in-box conhost). Bumped to
1.24.2605.12001, which fixed the stall and still passed the canary. That stable
package's PDB maps to the servicing-branch commit `b4e69c6`, which does **not**
contain #19535 even though the package was published after that PR merged.

The current 1.25 preview pair keeps the fast spawn and passthrough behavior and
adds #19535. Live verification on 2026-07-09: after kitty APC and after a resize,
Foreman observed `ESC[6n` on the next PowerShell screen-buffer query and returned
the matching CPR. The post-APC query completed in 3ms, below the affected host's
500ms timeout. See `docs/conpty-resize-reflow.md` for the repro matrix and
residual limitations.

`src/conpty_install.rs` serializes updaters with a Windows mutex, compares exact
bytes, and takes a write guard on the live DLL before changing either file. The
guard refuses a cross-version update while another Foreman has that DLL mapped
and prevents a racing `LoadLibrary`. The transaction stages both files, installs
OpenConsole first and the DLL last, and rolls filesystem changes back together
on failure. An unverified old pair is never considered safe: on any failed
update the installer disables its DLL and Foreman degrades to the in-box
ConPTY (text-only images); startup aborts only if that unsafe DLL cannot be
disabled or the pair is locked by another process mid-update. After success Foreman
holds read leases on both exact files for its process lifetime. This is required
because the 1.24 and 1.25 x64 DLLs have the **same byte length**: a size-only
version check would silently retain the old DLL and mix versions.

Rollback warning: do **not** run a historical 1.24 Foreman binary over an
already-installed 1.25 pair. Its old size-only installer would keep the
same-length 1.25 DLL while replacing OpenConsole with 1.24. Build the rollback
with the current transactional installer and the 1.24 assets, or delete **both**
sidecars beside `foreman.exe` before starting the historical binary.

Gates for every pair change:

- Package integrity: verify the NuGet signature, both Authenticode signatures,
  source/PDB lineage, and the hashes above.
- Passthrough and post-APC cursor latency:
  `cargo test --release conpty_passes -- --ignored` (requires the pair beside
  the test exe in `target/release/deps`). It must show a DSR, matching CPR, and
  complete the screen-buffer query below 400ms.
- Resize cursor synchronization: run `resize_recall_probe`,
  `typed_echo_lands_on_the_prompt_after_a_height_grow`, and `resize_drag_probe`
  as ignored release tests; inspect the full matrix in
  `docs/conpty-resize-reflow.md`.
- Spawn speed: a fresh terminal must reach its prompt in ~0.25s, not seconds.
