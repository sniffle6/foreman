# One-shot AI CLI launcher

## What it does

Runs an installed AI CLI (`codex exec`, `claude -p`, or `grok --single`) once,
feeds it a prompt, and hands back whatever it printed. Nothing else: no parsing,
no retry, no thread.

## Why it exists

Session titles and the theme expert each had their own copy of "launch the
provider CLI". The copies drifted: different flags, different isolation, and the
theme expert's Grok command line was broken outright (`--single` swallowed the
next flag as its prompt, so every Grok theme request failed). A commit-message
feature was about to become copy three. One launcher means one place to fix a
flag when a CLI changes.

## How to use it

```rust
let text = ai_oneshot::run(&ai_oneshot::Request {
    provider,              // config::NamingProvider
    model: "",             // blank = the CLI's default
    system: Some(RULES),   // or None to send only the prompt
    prompt: &prompt,
    cwd: &empty_dir,       // keep it out of the user's repo
    timeout: Duration::from_secs(30),
    max_output: 16 * 1024,
})?;
```

- It blocks. Call it from your own background thread; never from the GUI thread.
- Validate the text yourself. Treat it as untrusted.
- Map `LaunchError` (`Unavailable`, `Failed`, `Timeout`, `TooLarge`) to your
  own user-facing message.

## What it guarantees

- **No tools.** Claude: `--safe-mode --tools ""`. Grok: `--tools ""`, no web
  search, no subagents, one turn. Codex: read-only sandbox, shell tools
  disabled, user config and rules ignored.
- **Nothing persisted.** Codex `--ephemeral`, Claude `--no-session-persistence`.
- **One deadline** covers spawn, stdin, run, and draining stdout/stderr. At the
  deadline the child (and its Job tree on Windows) is killed.
- **Bounded output.** Stdout over `max_output` is `TooLarge`, not truncated.
- **Scrubbed env.** Foreman routing vars and provider API keys are removed, so
  the CLI uses its own login and cannot talk back to Foreman.
- **No console window** (`CREATE_NO_WINDOW`).

## Gotchas

- **Codex has no system-prompt flag.** `system` is prepended to the prompt on
  stdin. Claude gets `--system-prompt`, Grok `--system-prompt-override`.
- **Codex ignores `config.toml`** (`--ignore-user-config`). A blank model is
  Codex's built-in default, not the user's configured one.
- **Grok's prompt rides on argv**, the others use stdin. If a CLI is only
  reachable through a `.cmd` shim, the cmd.exe fallback refuses any argv word
  with shell metacharacters, so a Grok `.cmd` shim can't take a JSON prompt.
  npm's `codex.cmd` is resolved to `node codex.js` first and never needs cmd.

## Key files

- `src/ai_oneshot.rs` — flag lists, process run, tests
- `src/terminal_titles.rs` — Session-title caller (`generate_title`)
- `src/theme_expert.rs` — theme-expert caller (`generate`)
- `src/git_history/commit.rs` — commit-message caller (`draft`)
- `src/agent_command.rs` — `npm_codex` shim resolution
