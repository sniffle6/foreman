# Landing-Page Marketing Pass — Design

Date: 2026-08-07. Approved by Andy in-session.

## Problem

The GitHub repo page is foreman's only landing page and it is unmarketed:
empty description, no topics, no homepage, default social preview, and a
39-line README with zero images whose first pointer is a contributor doc
(`docs/HANDOFF.md`). Nothing on the page shows *why* the product exists.

## Goal

Make `github.com/sniffle6/foreman` sell the product to a human who just
landed: what it is, why it's different, one screenshot that proves it, and
an install line they can run in 10 seconds.

## Scope

1. **README rewrite** (structure, top to bottom):
   - Title + tagline: "A fast, native desktop for running fleets of AI
     coding agents — tmux built for AI." Minimal badges (release, license,
     Windows).
   - Install one-liner (existing) directly under the tagline — per Andy
     mid-session: install is the hero; convert first, explain after.
   - Hero screenshot right below install.
   - 3-sentence pitch: the many-agents supervision problem; native Rust
     speed (no Electron); agent-native control plane.
   - Screenshots are provided by Andy into `assets/media/` (staged-demo
     automation was abandoned: trust-dialog seeding raced live agent
     config rewrites; Andy opted to capture shots himself). Final set:
     `projects.png` (hero, under install), `landing-page.png` (launch
     screen — prompted adding a "Start in about five seconds" section,
     since one-click Claude/Codex/Grok launch + recents was missing from
     the feature list), `popout-windows.png` (floating windows at both
     levels, in the window-manager section).
   - `social-preview.png` (1280×640) is cropped from `landing-page.png`,
     not the workspace shot — at card size the workspace shot degrades to
     illegible text, while the launch screen keeps the logo and tagline
     readable.
   - Feature sections, most differentiated first:
     1. Agents can drive it — `foreman open/status/snapshot/chat`,
        per-project chat rooms (posts injected into member PTYs),
        auto-installed `foreman-dispatch`/`foreman-chat` skills.
     2. A window manager for projects — recursive compositor, tiling tree +
        floating + tabs, tmux-style zoom, leader-key control.
     3. A real terminal, native speed — ConPTY + alacritty_terminal
        emulation, mouse reporting, scrollback search, real bold/italic.
     4. Sessions panel — every project/agent at a glance.
   - Keyboard cheat-sheet table (leader `Ctrl+B` chords, the common ones).
   - Status & roadmap — honest: v0.2.x, Windows-only today.
   - Contributing → `docs/HANDOFF.md` (demoted to bottom); License.

2. **Staged demo screenshots** (no private data):
   - Method: temporary spawn code in `main.rs` (`if !self.started` block)
     creates 2–3 generically named demo projects with terminals; agent
     launch commands (`claude`, `codex`) written into the PTYs so real
     welcome screens render. Build, run, capture headlessly (HANDOFF § 3
     script), then REVERT the temp code.
   - Constraint: must NOT use the `foreman` CLI to stage — the control
     pipe is owned by the user's live instance; dispatches would land in
     the real workspace.
   - Constraint: never kill the user's running foreman (installed copy);
     kill only the demo exe + its child shells.
   - Output: 1 hero + 2–3 feature shots committed under `assets/media/`.

3. **Repo metadata** (via `gh repo edit`):
   - Description: "Fast, native desktop for running fleets of AI-agent
     terminal sessions — tmux built for AI. Rust + egui, real PTYs."
   - Topics: terminal-emulator, rust, egui, ai-agents, claude-code, codex,
     tmux, windows, conpty, developer-tools.
   - Social preview: 1280×640 PNG composed from the hero shot; upload is
     manual (GitHub web UI only) — hand the file to the user.

## Non-goals

Demo GIF/video (follow-up), GitHub Pages site, permanent app-code changes,
badges beyond the basic three.

## Risks / notes

- Screenshots go stale as the UI evolves; the staging method is documented
  above so they can be retaken each release.
- Work happens on the current branch (`feat/appearance-polish`) working
  tree; committing is a separate decision per the working agreement.
