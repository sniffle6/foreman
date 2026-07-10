# Project Review Findings - 2026-07-09

Scope: source review of the current Foreman project, focused on correctness risks
in the terminal, recursive window manager, control pipe, input routing, and
persistence paths.

Verification run during review:

- `cargo test` - passed: 480 passed, 0 failed, 7 ignored.
- `cargo build` - passed.
- `cargo clippy --all-targets` - passed with warnings only.

## Findings

### High: `send` / `snapshot` can self-target the wrong project

Fixed 2026-07-09, commit 7bbe9e1.

When `--terminal` is omitted, `parse_send_args` fills `terminal` from
`FOREMAN_TERMINAL_ID`, but keeps an explicit `--project` if one was supplied.
Because terminal ids are only unique within a project, a command like this from
`p1/t4` can write to or read from `p2/t4`:

```powershell
foreman send --project p2 --text "..."
foreman snapshot --project p2
```

Relevant code:

- `src/control.rs:592` - `parse_send_args` self-target fill.
- `src/control.rs:657` - `parse_snapshot_args` self-target fill.
- `src/control.rs:513` - `parse_close_args` already rejects the same ambiguity for bare close.

Recommendation: make `send` and `snapshot` match `close`: if `--project` is
explicit, require an explicit `--terminal`; bare self-target should always use
the caller's own `FOREMAN_PROJECT_ID`.

### High: right-click paste bypasses bracketed paste

Fixed 2026-07-09, commit e920d3a.

Right-click paste sends raw clipboard bytes directly to the PTY, while keyboard
paste goes through `paste_seq`, which honors bracketed-paste mode and strips ESC.
That means multi-line right-click paste can submit line by line, and embedded
escape bytes are treated differently from Ctrl+V / paste events.

Relevant code:

- `src/terminal.rs:1189` - right-click paste writes `txt.as_bytes()`.
- `src/terminal.rs:1099` - keyboard clipboard paste uses `paste_seq`.
- `src/input.rs:176` - `paste_seq` handles bracketed paste and ESC stripping.

Recommendation: route right-click paste through
`crate::input::paste_seq(*self.term.mode(), &txt)` before writing to the PTY.

### Medium: structured snapshots are not atomic

Fixed 2026-07-09 (architecture deepen 02): `snapshot_dispatch` calls
`Session::snapshot_all(attrs, cursor)`, which pumps once then derives text /
optional cells / optional cursor from that grid state. Single-field accessors
still pump per call (documented); multi-field Inspection must use `snapshot_all`.

## Non-blocking Notes

- `cargo build` reports existing warnings, including deprecated egui APIs and the
  intentional `Session.job` ownership field being reported as unread.
- `cargo clippy --all-targets` reports style and cleanup warnings only; no clippy
  warning changed the severity of the findings above.
