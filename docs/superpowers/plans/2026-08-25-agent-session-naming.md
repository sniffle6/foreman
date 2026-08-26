# Agent Session Naming Implementation Plan

Date: 2026-08-25

Status: accepted design, not implemented

## Goal

Give each new auto-managed Claude, Codex, or Grok terminal Session one useful
AI-generated title based on its first meaningful user prompt. The naming model
is selected in Foreman settings and may be Codex, Claude, or Grok independently
of the agent running in the terminal.

The default selection is Codex with `gpt-5.6-luna`, but automatic naming is
opt-in. Foreman invokes the user's already-installed, already-authenticated
native CLI so the request consumes that account's subscription or plan
allowance. Foreman does not accept, store, or inject API keys for this feature.

Until a generated title succeeds, the current `Claude · #N`, `Codex · #N`, or
`Grok · #N` label remains visible. Failure leaves that label unchanged.

## Accepted decisions

- Trigger from the first meaningful `UserPromptSubmit` event for a new vendor
  session, not from terminal output and not after the assistant finishes.
- The hook only sends a small local notification. It never starts the naming
  model and must not delay the agent prompt on network or model work.
- Use a dedicated, instance-specific, one-way named pipe. Do not extend the
  shared request/reply Control plane protocol for this feature.
- Run at most one naming request per vendor session. Do not retry or silently
  fall back to another provider, model, deterministic prompt title, or paid API.
- Support Codex, Claude, and Grok as both source agents and selectable naming
  providers. Source agent and naming provider are independent.
- Keep provider details behind one internal title-namer interface. The window
  manager owns title eligibility and stale-result rejection; it does not know
  CLI flags or authentication details.
- Use one worker thread, one in-flight child, and a bounded queue of four. Queue
  pressure drops naming work and leaves the generic title.
- Run naming CLIs from an empty Foreman-owned directory, outside the user's
  repository, with tools disabled or read-only where the CLI supports it.
- A manual or dispatch title always wins. A result is applied only after a
  second ownership, session, generation, and settings-epoch check.
- Persist only whether a terminal title is Foreman-managed. Never persist the
  prompt, generated semantic title, provider output, or runtime job state.
- Keep hook installation separate from skill installation. Skills teach agents
  how to use Foreman; hooks are executable user configuration and require their
  own status, merge, backup, and error handling.

## Out of scope for v1

- API-key billing mode, direct HTTP provider clients, token storage, or OAuth
  brokerage by Foreman.
- Renaming existing, manual, fixed, dispatch-created, or non-agent terminals.
- Regeneration, retry buttons, title history, or per-agent provider settings.
- Persisting generated titles across workspace restart.
- Reading terminal scrollback to infer prompts.
- Changing the shared Control plane wire format.

## Runtime flow

```text
Claude/Codex/Grok UserPromptSubmit hook
  -> `foreman title-event --agent <source>` reads capped JSON stdin
  -> instance-specific one-way FOREMAN_TITLE_PIPE
  -> bounded GUI ingress channel + repaint
  -> WindowManager validates member/session/ownership
  -> bounded title-worker queue
  -> selected local CLI in %APPDATA%/foreman/title-namer
  -> bounded, sanitized result
  -> WindowManager revalidates member/session/generation/epoch
  -> "semantic title · #<term_id>"
```

No part of the hook path waits for the naming CLI. The hook helper connects,
writes one bounded event, closes without reading a reply, and exits successfully
even when Foreman is absent or overloaded.

## Module and ownership map

| Module | Owns | Must not own |
|---|---|---|
| `src/agent_hooks.rs` | Hook discovery, semantic merge, marker/status, backup, atomic replacement | Provider invocation or GUI state |
| `src/title_notify.rs` | Pipe name, bounded event schema, helper input cap, one-way server, fail-open transport | Title eligibility or provider logic |
| `src/terminal_titles.rs` | Prompt eligibility, provider adapters, worker queue, child limits, response sanitizer | Tabs, layout, persistence, or settings UI |
| `src/wm.rs` | Per-tab ownership/state, source-agent recognition, request/result validation, visible title | CLI authentication and hook file formats |
| `src/settings.rs` and settings UI | Persisted choice, sanitization, disclosure, install/auth status | Runtime child processes |
| `src/main.rs` | Channel/thread lifecycle, repaint wake-up, settings epoch propagation | Vendor-specific behavior |
| `src/workspace.rs` | Managed-title ownership bit and compatibility | Generated title text or job state |

The narrow internal seam should describe an operation such as “generate a
title from this bounded prompt under these settings.” Codex, Claude, and Grok
adapters remain private implementations so vendor flag churn cannot spread into
the window manager.

## Data contracts

### Settings

Add serde-compatible fields with defaults:

```rust
auto_name_agent_sessions: bool, // default false
title_provider: NamingProvider, // default Codex
title_model: String,            // default "gpt-5.6-luna"
```

`NamingProvider` has `Codex`, `Claude`, and `Grok`. Trim `title_model`, cap it
at 128 characters, and treat an empty value as “use that CLI's configured
default.” Never interpret it as permission to choose a different provider.

Changing enabled state, provider, or model increments a process-local settings
epoch. Queued work from an old epoch is dropped. The worker checks the epoch
immediately before spawning a child; a late result is also rejected on the GUI
thread. At most the already-running child is allowed to finish.

### Hook notification

The helper normalizes vendor payload differences into one capped event:

```rust
TitlePromptEvent {
    source_agent,
    vendor_session_id,
    project_id,
    member_id,
    prompt,
}
```

Read JSON stdin with a hard byte limit rather than unbounded `read_line`. Accept
only known prompt/session-id fields found in Phase 0 fixtures, cap normalized
prompt text at 2,000 characters, and ignore subagent events. Require
`FOREMAN=1`, `FOREMAN_EXE`, and `FOREMAN_TITLE_PIPE`; missing or invalid routing
is a quiet successful no-op.

The pipe name contains a random per-process component. The listener reads with
a hard input cap and hands events to a bounded `sync_channel` using `try_send`.
Full channels drop the event. The helper has a short connect/write deadline,
emits no stdout or stderr, waits for no reply, and always fails open.

### Per-tab state

Keep `Tab::auto_title` as the hard ownership gate and add runtime state:

```rust
enum AgentTitleState {
    Waiting,
    Pending { vendor_session_id, generation, epoch },
    Settled { vendor_session_id },
}
```

- `Waiting`: no eligible prompt for the current session yet.
- `Pending`: one request was accepted. Duplicate events are ignored.
- `Settled`: a result was applied or the one allowed attempt failed/dropped.
- A low-information event remains `Waiting`; it does not consume the attempt.
- A newly observed vendor session id resets the managed title to the current
  generic source-agent label and begins a new one-attempt lifecycle.
- Manual rename sets `auto_title = false` and permanently rejects late results.
- Closing the tab, changing content, changing member identity, or changing the
  settings epoch makes old results stale.
- `refresh_auto_titles()` may update the generic source label while waiting or
  pending but must not overwrite a settled generated title.

The worker returns only a candidate and request identity. The GUI thread is the
sole writer of `Tab::title`.

### Visible title and sanitization

The model is asked for a short task label only. From its response:

- take the first meaningful line;
- remove ANSI escapes, C0/C1 controls, newlines, bidi/isolate controls, and
  zero-width formatting characters;
- trim surrounding quotes, label prefixes, and terminal punctuation;
- keep at most eight words and approximately 80 display characters;
- reject empty or generic boilerplate output.

Foreman appends ` · #<term_id>` after sanitization. The stable member suffix is
never supplied by or trusted to the model.

### Workspace snapshot

Add `managed_title: bool` to `TabSnap` with `#[serde(default)]`.

- For an auto-managed terminal, persist `managed_title = true` but do not
  persist a generated semantic title as authoritative user state.
- Restore it with a fresh generic shell/agent label for its new member id and
  runtime state `Waiting`.
- Persist and restore fixed/manual titles verbatim with
  `managed_title = false`.
- Older snapshots remain readable. A version bump is unnecessary if the
  optional field is sufficient; prove this with compatibility fixtures.

## Provider process contract

Phase 0 must lock the exact supported flags against installed CLI versions. The
expected shapes are:

| Provider | Expected noninteractive command | Authentication source |
|---|---|---|
| Codex | `codex exec --model <id> ...` | Existing Codex CLI login/subscription |
| Claude | `claude -p ... --model <id> --no-session-persistence` | Existing Claude Code login/subscription |
| Grok | `grok --no-auto-update -p ... --model <id>` | Existing Grok CLI login/plan |

Each adapter must:

- use the empty `%APPDATA%\foreman\title-namer` working directory;
- use an ephemeral/non-persistent session and disable tools, shell access, repo
  discovery, and approval prompts where supported;
- close or null stdin after supplying the prompt;
- cap stdout and stderr, kill the child at a 30-second deadline, and avoid a
  visible console window on Windows;
- remove `FOREMAN`, `FOREMAN_TITLE_PIPE`, and other Foreman hook-routing vars so
  the naming subprocess cannot recursively enqueue another naming request;
- remove known API-routing variables before spawn:
  `FOREMAN_OPENAI_API_KEY`, `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`,
  `ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_BASE_URL`, `XAI_API_KEY`,
  `GROK_CODE_XAI_API_KEY`, and `GROK_DEPLOYMENT_KEY`;
- never read credentials or config files into Foreman memory;
- never fall back to a different executable, provider, or model.

This prevents accidental use of common API-key environment overrides. It cannot
promise that a user-modified provider config file does not intentionally route
that CLI elsewhere, so the UI and docs must state the narrower guarantee:
Foreman does not read, store, or inject an API key and strips known overrides
from the child environment.

Missing executable, missing login, invalid model, child failure, timeout, old
queue item, or full queue settles the attempt and keeps the generic title. Show
one rate-limited diagnostic/status; never log prompt or response content.

Use a maximum queue age of 30 seconds. One worker and queue capacity four put a
hard ceiling on concurrent usage and memory without adding a pool or scheduler.

## Hook installation contract

Hook installation runs only after the user opts in. Enabling it from settings
starts background install/update work; the GUI remains responsive. Disabling
leaves the guarded managed hooks installed so user files are not repeatedly
rewritten. With `FOREMAN` or the instance pipe absent, those hooks do nothing.

- **Claude:** semantically merge one exact managed `UserPromptSubmit` entry
  into the user settings location, respecting `CLAUDE_CONFIG_DIR`.
- **Codex:** semantically merge the managed entry into
  `$CODEX_HOME/hooks.json` or the default location. Trust/feature approval stays
  a user action; Foreman must not report the hook active until it really is.
- **Grok:** own only
  `$GROK_HOME/hooks/foreman-session-naming.json` or the default equivalent.
  Do not rewrite unrelated Grok hook files.

Use a stable marker and stable command bytes. Parse and modify structured JSON,
preserve unrelated entries, compare before replace, write through a same-folder
temporary file, and create a recoverable one-time pre-Foreman backup. Refuse to
overwrite malformed user configuration and surface an actionable status.

Grok can consume Claude-compatible hooks. Avoid double submission: the Claude
managed command identifies itself as `--agent claude` and exits quietly when a
Grok-specific session environment shows that Grok invoked it; the dedicated
Grok hook uses `--agent grok`. Phase 0 must prove the actual environment marker
before implementation relies on it.

Claude may inject `UserPromptSubmit` stdout into model context. Every managed
hook path therefore produces no stdout/stderr under both success and failure.

`install_skills` and `src/skills_install.rs` remain unchanged.

## Settings UI

Extend the existing Agents pane with:

- `Automatically name new agent Sessions` toggle;
- provider choice: Codex, Claude, or Grok;
- editable model id, with empty meaning provider default;
- hook installation and CLI/auth readiness status;
- provider-specific allowance/policy help text.

When enabled, show this dynamic disclosure before or directly under the
controls:

> Sends up to 2,000 characters of the first meaningful prompt from every
> auto-managed Claude, Codex, or Grok Session to {provider} through your
> installed CLI; consumes that account's allowance.

Also state that a Claude terminal may be sent to Codex or Grok, and vice versa.
Do not describe subscription-backed CLI use as free.

Claude's subscription authentication has a distribution/policy caveat for
third-party products even though Foreman launches the local native CLI without
handling its token. Resolving that is a release gate: if the intended use is
not acceptable under Anthropic's current terms, hide/disable the Claude
subscription backend for public builds. Do not silently replace it with a paid
API path. The UI/docs must also mention any separate Claude Code/Agent SDK
allowance that the tested CLI version consumes.

## Implementation sequence

### 0. Prove live vendor contracts before production code

- [ ] Capture sanitized fixtures for initial prompt, follow-up prompt, `/clear`,
  compact, resume, and noninteractive operation from current Claude, Codex, and
  Grok `UserPromptSubmit` hooks.
- [ ] Record the reliable session-id, prompt, subagent, and Grok-vs-Claude
  discriminator fields. Add `SessionStart` only if prompt events cannot prove a
  session boundary. Do not add `SessionEnd` for naming.
- [ ] Verify the three exact headless naming commands, model flag behavior,
  tool/repo isolation flags, stdout format, exit codes, login failures, and
  child termination behavior.
- [ ] Prove with scrubbed environments that each command uses its existing
  logged-in CLI account and does not require or select an API key.
- [ ] Measure the local hook helper path over at least 100 launches. Require p95
  startup + connect + write below 100 ms with no model invocation.
- [ ] Resolve the Anthropic third-party/subscription policy gate and record the
  shipping decision in this plan or an ADR.
- [ ] Stop and revise this plan if a provider lacks a safe observe-only prompt
  hook, a subscription-backed noninteractive command, or adequate isolation.

Deliverable: repository-local sanitized fixtures and a short contract table.
No production behavior is enabled by this step.

### 1. Add settings and provider domain types

- [ ] Add `NamingProvider` and the three serde-defaulted settings fields.
- [ ] Sanitize loaded provider/model values and preserve older settings files.
- [ ] Extend the Agents pane and generalize its text-edit path for model ids.
- [ ] Add disclosure, provider allowance/policy copy, and disabled/readiness
  states without doing process checks during immediate-mode draw.
- [ ] Add config default, round-trip, invalid-value, and old-fixture tests.
- [ ] Update `docs/settings-persistence.md` and `docs/settings-menu.md`.

### 2. Build bounded one-way title notification

- [ ] Add the random instance pipe and inject `FOREMAN_TITLE_PIPE` through
  `term_env`. Preserve it when constructing recursive project managers and
  restored terminals.
- [ ] Add the early internal CLI path
  `foreman title-event --agent claude|codex|grok` before GUI startup.
- [ ] Implement bounded stdin parsing, payload normalization, routing checks,
  silent fail-open connect/write, and no reply.
- [ ] Add the concurrent listener, bounded channel, repaint wake-up, and clean
  shutdown without touching the shared Control plane.
- [ ] Test absent/invalid env, oversize/invalid JSON, full channel, timeout,
  instance isolation, and zero stdout/stderr.

### 3. Install hooks safely

- [ ] Implement semantic Claude and Codex merges plus the dedicated Grok file.
- [ ] Preserve unrelated user hooks, replace only the exact managed marker, and
  make repeated installation byte-stable.
- [ ] Add atomic replacement, one-time backup, malformed-file refusal, and
  inspectable status values.
- [ ] Verify Grok/Claude compatibility filtering against Phase 0 fixtures.
- [ ] Connect opt-in enablement to a background install/update request. Keep
  disabled hooks installed but inert.
- [ ] Test all installers in temporary homes, including custom home env vars,
  missing parent directories, unrelated hooks, duplicate old managed entries,
  malformed JSON, backup-once behavior, and simulated write failure.

### 4. Add window-manager ownership and workspace semantics

- [ ] Add `AgentTitleState` to terminal tabs and initialize it for every tab
  creation, dispatch, restore, tab, and untab path.
- [ ] Match notifications by project/member id and source agent; accept only
  auto-managed terminal tabs with an eligible prompt.
- [ ] Make the first accepted event pending and reject duplicates.
- [ ] Reset on a genuinely new vendor session id.
- [ ] Make manual rename, tab close, content replacement, generation mismatch,
  session mismatch, and epoch mismatch reject late results.
- [ ] Update `refresh_auto_titles()` so it never overwrites a settled result.
- [ ] Append the stable terminal member id after sanitization.
- [ ] Add `TabSnap::managed_title` and restore managed terminals to a fresh
  generic label without generated text.
- [ ] Test manual-rename races, new-session reset, refresh behavior, dispatch
  exclusion, closed tabs, tab/untab identity, generated-title non-persistence,
  and old workspace snapshots.

### 5. Build the single title worker and private adapters

- [ ] Implement a four-item bounded request queue and one worker thread.
- [ ] Drop requests older than 30 seconds or from an old settings epoch before
  child spawn.
- [ ] Implement private Codex, Claude, and Grok command builders using the
  Phase 0 contract table.
- [ ] Create/use the empty Foreman title-namer directory and scrub routing/API
  env vars from every child.
- [ ] Enforce no-window spawn, stdin closure, bounded stdout/stderr, 30-second
  kill, and exactly one result message.
- [ ] Implement response sanitization and stable suffix assembly as pure code.
- [ ] Emit content-free, rate-limited diagnostics for executable/auth/model/
  timeout failures.
- [ ] Test with fake CLI executables. Automated tests must never consume a real
  provider allowance.

### 6. Wire lifecycle and cancellation

- [ ] Start listener and worker infrastructure once per Foreman app instance.
- [ ] Drain ingress/result channels outside the draw traversal and dispatch to
  the recursive `WindowManager` by stable project/member identity.
- [ ] Propagate the settings epoch through requests and results.
- [ ] On disable/provider/model change, increment the epoch and drop queued or
  returning stale work. Do not block waiting for an in-flight child.
- [ ] Shut down pipe/listener/worker cleanly without hanging app exit.
- [ ] Confirm no draw path, PTY pump, or prompt hook waits on model work.

### 7. Validate end to end and document operations

- [ ] Run `cargo check` and the full `cargo test` suite using the Foreman-safe
  target directory when running inside Foreman.
- [ ] Capture and read back a screenshot of the Agents pane showing the toggle,
  three providers, editable model, disclosure, and error/status treatment.
- [ ] Manually test real Claude, Codex, and Grok source Sessions with each
  supported naming provider, including at least one cross-provider case.
- [ ] Test missing CLI, signed-out CLI, invalid model, timeout, disabled while
  pending, manual rename while pending, five rapid Sessions, and provider/model
  changes under load.
- [ ] Test outside-Foreman hook invocation and simultaneous installed/dev
  Foreman instances. Both must be harmless and isolated.
- [ ] Confirm prompt and generated response text never enter logs, settings,
  workspaces, crash diagnostics, or hook backups.
- [ ] Re-measure hook p95 below 100 ms and verify GUI frame time does not wait
  on naming latency.
- [ ] Update the session-naming research note, settings docs, and user-facing
  troubleshooting with exact supported CLI versions and allowance behavior.

## Required tests by seam

| Seam | Minimum evidence |
|---|---|
| Hook parsing | Three vendor fixtures, prompt cap, subagent exclusion, new-session behavior |
| Hook install | Preservation, idempotence, exact-marker replacement, backup, malformed refusal, Grok no-duplicate behavior |
| Pipe | No reply, input cap, deadline, channel full, two-instance isolation, silent fail-open |
| Title state | Duplicate, low-information, session reset, queue drop, stale epoch/generation, close, manual rename wins |
| Provider adapters | Exact args/cwd/env removal, missing exe, nonzero exit, timeout kill, bounded output, fake login/model errors |
| Sanitizer | ANSI/control/newline/bidi/zero-width removal, Unicode, quotes, eight-word/length caps, empty rejection |
| Persistence | Generated text absent, managed bit restored generic, fixed title preserved, old snapshot/config fixtures |
| GUI | Settings round-trip plus screenshot/read-back of disclosure and status states |

## Definition of done

- A new eligible Claude, Codex, or Grok Session makes no more than one naming
  request for its first meaningful prompt.
- The selected provider/model is honored exactly; default selection is Codex
  `gpt-5.6-luna`, and the feature is off until the user opts in.
- The agent begins handling the prompt without waiting for the naming model.
- No GUI path blocks, the queues and process output are bounded, and overload
  degrades only to the generic title.
- Manual/fixed titles cannot be overwritten by a late result.
- No direct provider API integration or API-key billing path exists.
- Prompt/result content is neither persisted nor logged.
- Hook installation preserves user configuration and remains harmless when
  Foreman is not running.
- All unit/integration tests use fake providers; real allowance is consumed only
  during explicit manual acceptance testing.
- The Anthropic policy gate is resolved before enabling Claude subscription
  naming in a public build.
- Build, tests, real-provider smoke matrix, and GUI screenshot evidence pass.

No commit is part of this planning task. Implementation commits should follow
the numbered seams above and must not include unrelated worktree changes.

## Primary references

- [Foreman open-source naming research](../../2026-08-24-agent-session-naming-research.md)
- [Claude Code hooks](https://code.claude.com/docs/en/hooks)
- [Claude Code CLI reference](https://code.claude.com/docs/en/cli-usage)
- [Claude Code environment variables](https://code.claude.com/docs/en/env-vars)
- [Claude Code legal and compliance](https://code.claude.com/docs/en/legal-and-compliance)
- [Claude Code cost management](https://code.claude.com/docs/en/costs)
- [Codex `UserPromptSubmit` command-input schema](https://github.com/openai/codex/blob/main/codex-rs/hooks/schema/generated/user-prompt-submit.command.input.schema.json)
- [Grok hook guide](https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-pager/docs/user-guide/10-hooks.md)
- [Grok headless scripting](https://docs.x.ai/build/cli/headless-scripting)
