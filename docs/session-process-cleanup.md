# Session process cleanup (Windows Job Objects)

## What it does

Closing a foreman terminal kills the whole process tree that ran in it — the
shell plus everything the shell started (agents, node, git, …). Same behavior
as closing a Windows Terminal tab.

## Why it exists

Dropping a `Session` used to end only the PTY; an interactive `cmd.exe` does
not exit when its PTY goes away, so every closed pane leaked its shell and
descendants. One debug session found 2,000+ orphaned cmd/conhost pairs
(mostly from test runs), which degraded ConPTY spawn from microseconds to
~3 seconds machine-wide. A full test-suite run leaked ~59 processes; with
Jobs it leaks zero.

## How it works

- `src/job.rs` — `Job::assign(pid)` creates a Job Object with
  `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` and assigns the child to it; processes
  the child spawns join the job automatically. Dropping the `Job` closes the
  handle → the OS kills every process still in the job. No polling, no
  PID-walking, no PID-reuse races.
- `Session.job: Option<Job>` (src/terminal.rs) — assigned at spawn in
  `spawn_with`. Kill-on-drop means every close path (close pane/tab/project,
  app exit, panic unwind) gets cleanup for free.

## Gotchas

- **Best-effort by design.** `Job::assign` returning `None` never fails the
  spawn — the session just runs unreaped (the old behavior).
- Anything the child spawned in the microseconds *before* assignment is not
  in the job — accepted; Windows Terminal has the same window.
- `DeathWatch` (cfg(test), src/job.rs) pre-opens a process handle so tests
  can wait on a child's death without racing PID reuse.
- If PTY tests ever get slow again, count orphaned conhost/OpenConsole
  processes first — an orphan swarm was the original symptom.

## Key files

- `src/job.rs` — `Job`, `DeathWatch`, unit test.
- `src/terminal.rs` — `Session.job` field, init in `spawn_with`, test
  `dropping_a_session_kills_its_child`.
- `Cargo.toml` — `windows-sys` (Foundation, Security, JobObjects, Threading).
