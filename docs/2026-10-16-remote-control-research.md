# Foreman remote control feasibility

Research snapshot: 2026-10-16. Proposal, not an approved implementation plan.
External API and product behavior below was checked against primary sources.
Design recommendations are inferences; no networking prototype was run.

## Finding

Remote Foreman-to-Foreman viewing and control is technically feasible without
changing Foreman's native Rust product constraint. The useful first distinction
is between copying the host application's pixels and reproducing its Projects,
Wins, live Sessions, and other Content in the receiving Foreman.

The recommended direction is a host-authoritative application protocol. The
other PC keeps running the processes and interpreting their terminal output.
The receiving Foreman presents a remote Desktop using native rendering, sends
authorized user actions to the host, and receives display-state changes.

This recommendation assumes the user principally wants the same ongoing agent
Sessions and Project arrangement on either PC. If exact reproduction of every
dialog, hover effect, image, and future embedded application is the requirement,
a captured-window stream is the more direct match. Neither choice is a small
extension to a text-only snapshot command.

Terminal and layout replication alone does **not** mirror Board, Chat, Git,
Image, Settings, menus, or modals. Each needs an explicit live representation
or a separate display strategy. A staged native implementation must describe
itself as a partial mirror until those surfaces are covered.

## Current Foreman audit

The following findings were verified against local source at `e2f1b36` during
this investigation. Unrelated working-tree edits were preserved; `src/wm.rs`
also changed concurrently during research. The audit describes the inspected
symbols rather than claiming an immutable checkout snapshot.

| Existing seam | What it establishes | What remote mirroring still needs |
|---|---|---|
| `src/control.rs`, `serve`, `CtrlMsg` | A local named-pipe request/reply boundary delivers work to the GUI through a channel. | A persistent authenticated stream; framing caps and read deadlines. The existing `read_line` request handling is not a network exposure boundary. |
| `src/wm.rs`, `handle_ctrl`, `CtrlMsg::Send` | Existing send operations support automation and quiescence settling. | An interactive input lane without per-key synchronous CLI calls or settle waiting. |
| `src/terminal.rs`, `Session`, `pump_at` | The host owns emulator, parser, process, PTY, graphics, and query replies. | A remote display representation that preserves host authority. |
| `src/terminal.rs`, `snapshot_all`; `src/inspect.rs`, `CellData`, `snapshot_cells_span` | A single-pump coherent view includes text/cells/cursor. | Full display and interaction metadata. This is not a complete emulator checkpoint; wide spacer skipping means the cell vector is not a rectangular one-cell-per-column array. Use explicit row, column, and width. |
| `src/terminal.rs`, `resize` | Session size follows rendered geometry; the code documents ConPTY reflow hazards. | Keep viewer geometry from resizing the host until ownership is explicit. |
| `src/workspace.rs`, `WorkspaceSnapshot`, `ContentSnap`, `ManagerSnap`; `src/wm.rs`, `capture_manager`, `apply_workspace`, `apply_manager` | Persistence records recursive layout and restores it by spawning new shells with regenerated IDs. | Live attach identities and metadata. The snapshot omits TaskManager/Settings and does not preserve managed titles as live display metadata. Loading a workspace is not attachment. |
| `src/frame.rs`, `PaintPlan`, `plan_paint` | Pure glyph placement is already separated from painting. | An adapter or display model; the current seam still assumes `Grid<Cell>`. |
| `src/job.rs`, `Job`; `src/main.rs`, `App::logic`, `service_background` | Job teardown kills owned work; background logic pumps hidden/occluded content. | Keep the host application running and PC awake. Moving ownership to a daemon is a later persistence project, not necessary for first remote attachment. |

These are useful starting seams, but they do not constitute an existing remote
protocol. This research made no production changes and did not run builds or
network benchmarks. Locked-screen behavior and minimized captured-window
streaming remain experiment questions.

## What existing systems establish

### Native terminal attachment has a strong precedent

WezTerm's official multiplexing documentation describes attaching remote
multiplexer windows and tabs to a local native UI, including local mouse,
clipboard, and scrollback interaction. Its SSH domains require a compatible
remote installation. TLS domains can bootstrap credentials over SSH and
reconnect after interruptions. This demonstrates the product architecture,
not drop-in compatibility with Foreman.
[WezTerm multiplexing](https://wezterm.org/multiplexing.html)

WezTerm's actual codec is more informative than a raw-output forwarding demo.
`GetPaneRenderChangesResponse` includes dirty line ranges, cursor, dimensions,
alternate-screen state, mouse state, title, working directory, sequence number,
and optional line data. It also defines requests for lines, key events, mouse,
paste, resizing, and protocol-version negotiation. Foreman can study this
division of responsibilities without adopting the code or terminal engine.
[WezTerm codec source](https://github.com/wezterm/wezterm/blob/main/codec/src/lib.rs)

Multiple viewers do not give a process multiple simultaneous terminal sizes.
tmux explicitly offers largest, smallest, manual, and most-recently-active-client
size policies. Foreman therefore needs a deliberate policy when PCs have
different displays. For an initial mirror, retain the host Session dimensions
and fit the remote view; let a controller explicitly acquire resize ownership.
The latter is a recommendation, not tmux's prescribed policy.
[tmux manual, window-size](https://man.openbsd.org/tmux.1)

### The terminal library supplies useful access, not a remote product

The documented `alacritty_terminal::Term` API exposes grid access, renderable
content, modes, colors, cursor style, and damage tracking. Its documented trait
implementations do not provide serialization of the whole `Term`. A crate-wide
Serde feature must not be mistaken for a supported emulator checkpoint format.
Damage excludes user-controlled selection and vi cursor state, and reading it
is followed by reset. A remote subscriber cannot independently consume and
reset that shared state without coordination with local rendering.
[Term API](https://docs.rs/alacritty_terminal/0.26.0/alacritty_terminal/term/struct.Term.html)

Inference: a versioned Foreman display schema is a better boundary than dumping
the emulator's private representation. A display replica need not itself be a
fully restorable emulator. That avoids making every hidden parser or terminal
mode field part of Foreman's wire compatibility contract.

### Literal window streaming is possible, but solves a different problem

Windows provides capture frames from an application window through
`Windows.Graphics.Capture`. The API uses a D3D frame pool and requires handling
content-size changes, device loss, and frame lifetimes. Microsoft recommends
keeping heavy frame work away from the UI thread. The API supplies frames;
encoding, transport, decoding, and interactive control remain application work.
[Microsoft screen capture](https://learn.microsoft.com/en-us/windows/apps/develop/media-authoring-processing/screen-capture)

For a Win32 app, `IGraphicsCaptureItemInterop::CreateForWindow` targets an HWND;
Microsoft lists Windows 10 version 1903 as its minimum client. Capturing only
Foreman's window can avoid sharing the entire desktop.
[CreateForWindow](https://learn.microsoft.com/en-us/windows/win32/api/windows.graphics.capture.interop/nf-windows-graphics-capture-interop-igraphicscaptureiteminterop-createforwindow)

OS-level input injection adds constraints. `SendInput` writes into the keyboard
and mouse input stream, and UIPI prevents injection into a higher-integrity
application. Existing pressed keys may interfere. For Foreman-to-Foreman
control, explicit application actions avoid depending on desktop focus and
global input injection. Captured-window control could also use application
input handling, but would still require coordinate and focus mapping.
[SendInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput)

Streaming egui drawing output is a third possibility, but should not be assumed
to be a ready-made remote backend. Its `FullOutput` contains shapes, texture
deltas including font textures, platform output, pixels-per-point, and viewport
output. Texture updates and lifetimes matter. Inference: a remote drawing
backend would need texture bootstrap, input integration, DPI handling, custom
rendering support, and compatibility rules, in addition to networking. The
current upstream API illustrates the responsibilities; exact compatibility
must be checked against Foreman's pinned egui version before implementation.
[egui FullOutput](https://docs.rs/egui/latest/egui/struct.FullOutput.html)

## Proposed behavior contract

These are candidate requirements to validate, not established repo behavior.

| Area | Proposed initial behavior | Why it matters |
|---|---|---|
| Process ownership | The host alone owns shells, agents, PTYs, and emulator state. | Connecting must attach to existing work. |
| Project mirror | Replicate host Project/Win identities, structure, titles, and active Content. | A remote shell alone does not mirror Foreman. |
| Terminal display | Transfer styled cells, cursor, dimensions, modes needed for interaction, and bounded history. | Plain text loses observable behavior. |
| Control | Begin as a viewer; acquire one explicit controller lease. Local takeover revokes it. | Prevent interleaved typing and conflicting resize actions. |
| Geometry | Match host layout in a fitted viewport initially. | Different monitor sizes must not silently reflow host Sessions. |
| Selection | Keep copy selection and scroll position local by default. | Reading history remotely should not move the host user's view. |
| Clipboard | Explicit copy/paste only initially; do not silently synchronize all clipboard content. | Device clipboard and remote process clipboard requests are different capabilities. |
| Remote files | Open paths on the host through host actions; transfer bytes for remote image previews when supported. | A host path is not necessarily valid on the receiving PC. |
| Disconnection | Keep host work running; mark the remote view stale and disable input. | Old output must not appear live. |
| Reconnection | Resynchronize from a consistent checkpoint or bounded retained changes. | A new connection cannot assume its display is current. |

## Protocol details that determine correctness

**Late attachment is the first hard problem.** Forwarding output only after a
client joins omits everything that established the screen. Keeping the last
arbitrary byte window is insufficient: a cut can occur within an escape
sequence, and earlier output may have set persistent modes. A complete history
replay can become unbounded and must also include resize ordering. A display
snapshot followed by ordered display updates avoids replaying the process's
entire lifetime. A raw-output approach needs a genuinely sufficient emulator
checkpoint instead.

**Snapshots need an ordering boundary.** Capture display state and its revision
together, retain updates produced during the snapshot, then send only later
updates. Include a connection/server generation so reused IDs or restarted
hosts cannot be mistaken for the old stream. Each Session also needs a
generation when it exits and is replaced. Detect missing changes and request a
fresh snapshot. Atomically publishing a snapshot is a protocol property, not
merely sending several queries quickly.

**Only one emulator answers the process.** The host must remain the sole
query-response authority. Clipboard requests need an explicit capability policy;
this is not a claim that Foreman's current Listener handles every emulator
event or supports OSC 52 clipboard operations. A remote display replica must
not independently send another answer. This is especially easy to get wrong if
the client parses the same raw PTY stream. `Term` documents terminal
identification and query handlers; Foreman's required startup query response is
also called out in the repository guide.
[Term query methods](https://docs.rs/alacritty_terminal/0.26.0/alacritty_terminal/term/struct.Term.html)

**Slow viewers must not stall the host.** Use bounded queues outside the GUI
thread. Coalesce superseded display changes and resnapshot if necessary.
Preserve ordered input and explicit command results separately from disposable
intermediate display frames. Never automatically resend an uncertain shell
input or process-spawn request after reconnect: it may already have executed.

**Interaction needs terminal modes.** Mouse reporting, bracketed paste, alternate
screen, focus reporting, and application cursor modes affect input meaning.
Either send structured actions for host encoding, or ensure the remote encoder
has current mode state. Avoid a mixture with unclear ownership.

**Authentication is separate from reachability.** View access exposes source,
logs, paths, and secrets printed in terminals. Control access can execute
commands. The protocol needs explicit host opt-in, trusted-device identity,
view/control authorization, revocation, size limits, and version negotiation.
An authenticated network is useful but does not settle application permissions.

## Connection recommendation

For a first personal two-PC experiment, use an existing authenticated transport
rather than building NAT traversal, relay infrastructure, and device identity.

An SSH-forwarded loopback endpoint is one practical choice. OpenSSH's `-L`
forwards a local socket or port through its secure channel, and permits an
explicit localhost bind. Windows has an OpenSSH Server optional feature with
documented installation and firewall configuration. Verify attachment to the
existing interactive Foreman instance; spawning a fresh Foreman under the SSH
login is not equivalent to reaching that instance.
[OpenSSH forwarding](https://man.openbsd.org/ssh),
[Windows OpenSSH Server](https://learn.microsoft.com/en-us/windows-server/administration/openssh/openssh_install_firstuse)

Tailscale is another deployment choice. Its direct and relayed connections are
end-to-end encrypted; relays preserve reachability when NAT prevents a direct
connection. Direct links generally have better latency and throughput. It can
reduce networking setup for the two PCs, but requires installing and configuring
an external service and restricting access to the intended devices. Do not
expose Foreman's local automation endpoint to the whole network merely because
the network is encrypted.
[Tailscale connection types](https://tailscale.com/docs/reference/connection-types)

Neither choice requires a browser frontend or a hosted Foreman cloud service.
A polished built-in pairing experience can follow only if the experiment shows
the workflow is useful.

## Experiments before a production design

1. Attach a second native viewer to a Session already deep into an interactive
   program. Compare cells, attributes, cursor, and alternate-screen state.
2. Generate output while attaching and reconnecting. Verify revision continuity
   and identical final displays, including Unicode and wrapped lines.
3. Exercise query/response programs with both viewers attached. Verify exactly
   one correct host response and no duplicate input.
4. Move between PCs with different DPI and window sizes. Verify a viewer does
   not resize the process; verify controller takeover follows the chosen policy.
5. Disconnect and reconnect around input and process creation. Verify no
   automatic duplicate execution and a clear uncertain-result state.
6. Throttle the viewer and flood several Sessions. Measure host UI frame time,
   input-to-echo latency, bytes transferred, queue memory, and catch-up time.
   Compare against the same host workload with no remote client.
7. Try the non-terminal Content the user actually uses. Mark unsupported Content
   explicitly before calling the result a mirror of the whole application.

Pre-register acceptable latency and host-overhead limits after measuring the
local baseline. No performance figures or implementation-duration estimate are
claimed by this research. The first experiment should answer whether the
display boundary can support faithful late attachment without invasive terminal
engine changes; failure there changes the architecture and project cost.

## Key files

The current-audit table above identifies the code symbols that support the
recommendation. `docs/HANDOFF.md` supplies the native-performance constraint;
`CONTEXT.md` supplies the Project/Win/Session vocabulary. Recheck the source
symbols before turning this dated snapshot into an implementation plan.
