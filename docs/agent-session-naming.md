# Agent Session naming

Foreman can replace a generic agent tab label such as `Claude  ·  #3` with a
short task title derived from that Session's first meaningful user prompt. The
feature is off by default and supports the locally installed Codex, Claude, and
Grok CLIs as independent naming providers.

## How it works

Enable **Automatically name agent Sessions** in Settings → Agents, choose a
provider, and enter an exact model id. A blank model uses that CLI's default.
The default is Codex with `gpt-5.6-luna`. Switching to Claude or Grok clears
the model override so that CLI starts from its own configured default; the
field remains editable for an exact provider-specific model id. Switching back
to Codex restores the Luna default.

On enable, Foreman installs one guarded global `UserPromptSubmit` hook for each
supported source agent:

- Claude: `CLAUDE_CONFIG_DIR/settings.json` or `~/.claude/settings.json`
- Codex: `CODEX_HOME/hooks.json` or `~/.codex/hooks.json`
- Grok: `GROK_HOME/hooks/foreman-session-naming.json` or the default equivalent

Claude and Codex files are merged semantically. Unrelated hooks are preserved,
Foreman's entry is deduplicated, malformed configuration is refused, and the
original gets a one-time `.pre-foreman.bak` backup. Grok gets its own file.
Disabling naming leaves these guarded entries installed. They may still launch
the small local helper, but Foreman discards the event and never starts a naming
provider while the setting is off. Claude and Codex run the passive hook
asynchronously; Grok documents `UserPromptSubmit` itself as non-blocking. This
is separate from **Install agent skills on launch**.

The hook sends at most 2,000 characters of the current meaningful prompt, the
source Session identity, and the agent-provided transcript path over a bounded,
one-way local pipe. It never reads the transcript or calls a model. Listener
connections have a one-second read deadline and at most eight may be active. The GUI
accepts at most one naming attempt per upstream agent session, then a single
background worker may add up to three 600-character opening prompts and one
prior title from a bounded transcript prefix. At most 3,800 user-prompt
characters plus the prior title reach the selected provider. A Claude Session
may therefore be named by Codex or Grok, and vice versa.

The worker runs in the empty `%APPDATA%\foreman\title-namer` directory so the
naming CLI does not discover or inspect the active repository. Tools, approvals,
session persistence, and repository rules are disabled where the CLI supports
that. On Windows, native provider executables launch directly. Codex and Claude
can retry through a safe npm `.cmd` shim; Grok requires a native executable
because its current `--single` interface carries the context as an argument.
Codex and Claude prompts stay on stdin and are never part of their command
lines. Output, queue age, pipe-reader waits, and runtime are bounded. One shared
30-second provider deadline includes stdin delivery, process execution, and
stdout/stderr collection. Foreman kills the root child at that deadline and,
when Windows Job assignment succeeds, its process tree as well.
Missing login, missing CLI,
invalid model, timeout, overload, or bad output keeps the generic title, shows
a content-free rate-limited warning, and does not retry that upstream session.

For a fresh conversation, the current prompt is the task context. For a resumed
conversation, the worker adds up to the first three genuine user prompts and an
existing agent-generated session title when available. Agent instructions,
environment metadata, slash commands, tool results, subagent traffic, and
assistant output are excluded. Codex and Claude supply the transcript path in
their hook payloads; Grok is resolved by session id inside its documented local
session store, scanning at most 512 workspace entries. Transcript parsing is
tolerant and falls back to the current prompt when a vendor format changes.

## Cost, authentication, and prompt privacy

Naming consumes the selected CLI account's allowance. Foreman does not read or
store provider credentials and does not inject an API key. It removes known API
and gateway environment overrides before starting the naming process, then lets
the installed CLI use its own existing login. A user-modified CLI configuration
can still intentionally route that CLI elsewhere.

Release decision, reviewed 2026-08-26: keep Claude available as an explicit
opt-in local-CLI provider. Anthropic says `claude -p` and third-party app use
currently draw from Claude subscription usage while API authentication is its
preferred route for third-party products. Foreman identifies itself normally,
uses the installed CLI's existing login, warns that allowance is consumed, and
never silently switches to a paid Anthropic API key. Recheck this policy at each
public release; disable the provider if subscription-backed third-party use is
withdrawn.

Only bounded user prompts and a prior agent-generated title are sent to the
selected provider; assistant output is not. Foreman does not log prompt or
model-response content. Grok's current CLI places its single-turn context on
the process command line, so same-user process-inspection tools can see it while
the request is running; Codex and Claude receive it over stdin. The naming
instruction requires a two-to-five-word Title Case task label rather than a
conversational sentence. Model output is stripped of terminal controls and only
the first non-empty line is considered. It is rejected—not truncated—when it is
outside two-to-five words, exceeds about 64 characters, uses punctuation beyond
`-`, `/`, or `+`, lacks alphabetic text in any word, is lower-case prose, begins
with a conversational gerund such as `Reviewing`/`Assessing`, or tries to spoof
Foreman's member suffix. The trusted suffix (` · #N`) is appended locally.

## Title lifecycle

- Empty prompts, bare slash commands, punctuation-only input, follow-up prompts,
  and subagent prompts do not start another request.
- A genuinely new upstream session id in the same terminal can be named once.
- A resumed session uses its opening user context instead of treating the first
  post-restart follow-up as the conversation topic.
- Manual titles and dispatch-provided titles are never overwritten.
- Provider/model/enable changes invalidate in-flight results.
- Generated task titles are not authoritative workspace state. The snapshot
  stores a generic source/member label plus `managed_title = true` so older
  Foreman versions do not restore a blank tab; current restore discards that
  generic text and starts with a fresh generic label and runtime `Waiting`
  state for the new member id.

## Key files

- `src/title_notify.rs` — silent hook helper and bounded one-way local pipe
- `src/agent_hooks.rs` — guarded, recoverable hook installation
- `src/terminal_titles.rs` — lifecycle state, provider commands, worker, cleanup
- `src/wm.rs` — project/member routing and tab-title ownership
- `src/config.rs`, `src/settings_menu.rs` — persisted choices and UI
- `src/workspace.rs` — managed-title restore semantics

## Primary provider references

- [Claude Code hooks](https://code.claude.com/docs/en/hooks)
- [Anthropic: Agent SDK and `claude -p` plan usage](https://support.claude.com/en/articles/15036540-use-the-claude-agent-sdk-with-your-claude-plan)
- [Anthropic: authentication for third-party tools](https://support.claude.com/en/articles/13189465-log-in-to-your-claude-account)
- [Codex hook implementation](https://github.com/openai/codex/tree/main/codex-rs/hooks)
- [Grok hooks guide](https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-pager/docs/user-guide/10-hooks.md)
