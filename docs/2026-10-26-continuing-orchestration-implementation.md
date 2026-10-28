# Continuing plan orchestration implementation

## Purpose

An agent can execute related approved cards in its existing Foreman Session,
checkout, and conversation. Per-card branches and integration checkpoints remain;
ordinary sequential execution no longer requires a coordinator or a fresh worker
and worktree for every card. This retains useful context and build artifacts.
No measured performance improvement is claimed.

The governing design is
[Continuing plan orchestration](superpowers/specs/2026-10-26-continuing-plan-orchestration-design.md).
The implementation follows the later correction that unrelated dirty work must
be preserved and must not, by itself, block integration. Obvious conflicts return
to the same agent for resolution; only ambiguous or destructive choices require
the user's decision.

## What changed

- `src/assignments.rs` owns durable approved scope, canonical checkout/board
  identity, owner generations, revisions, retry receipts, preparation intent,
  continuation notes, and accepted per-card evidence.
- `src/control.rs` exposes Begin, Continue, Checkpoint, Pause, Release, Resume,
  Status, and bounded read-only Wait through the existing control plane. Its optional
  assignment payload preserves legacy request bytes when absent.
- `src/wm.rs` resolves the project and terminal on the GUI thread, then validates
  identity and executes assignment work off-thread with bounded concurrency.
- `src/proc.rs` checks fresh native provider ancestry and creation time, then
  matches the requested session to the actual caller's provider environment.
  Nested native providers and ambiguous daemon routes fail closed.
- `src/kanban.rs` and `src/integrate.rs` honor persistent reservations across
  legacy claims, dispatch, worktree preparation, closeout, teardown, and queue
  execution. Integration results survive removal of finished queue entries.
- `src/integrate.rs` isolates configured branch checks when dirty inputs would
  affect them, explicitly separates the Cargo target directory, and rejects
  checks that change tracked inputs or HEAD.
- `src/assignment_hook.rs` returns bounded read-only compaction context through
  the instance request/reply pipe. `src/agent_hooks.rs` installs the handler
  independently of optional naming/badges, preserving unrelated hook order and
  trust positions. It never enables trust.
- Claude and Codex kanban instructions now default to continued execution in
  the current Session. Optional independent delegation remains documented.
  The glossary and existing kanban, integration, and agent-state docs describe
  the new behavior and limits.

## Expected workflow

Establish commit and local integration permission before dependent execution.
Begin captures an approved card subset or current plan membership and reserves
the checkout. It returns work only after preparing and verifying `card/<id>`
with `--no-track` and recording the claim. Eligibility follows the existing wave
rule, with creation time and card ID as stable tie-breakers.

Implement, test, review, and commit the current outcome under repository rules.
Checkpoint consequential reasoning during long cards. Continue with `--submit`
enqueues the committed result. Wait polls read-only status within a bounded
client timeout. Continue then reconciles verified integration, archives evidence,
and either prepares the next card or returns a non-work outcome. It never starts
a successor while the current card is still integrating.

Every mutation uses a request ID and the last returned owner generation/revision.
Retry an uncertain response with the identical payload. A new reconciliation
uses a new request ID. Replays cannot skip cards, repeat preparation, or submit
the successor's unfinished work. Changed briefs do not silently expand scope.

Pause keeps the reservation and prevents the next claim. Release explicitly ends
an assignment that cannot continue and frees the checkout without changing Git
or the board; it is refused while integration or preparation is in flight.
A failed request no longer blocks the next one unless it claimed a successor or
was an interrupted Resume. Continue without `--submit` never queues work.
Status inspects without
mutating. Resume reconciles unfinished state; replacement Sessions explicitly use
`--recover`, fence the former generation, and repair interrupted claims. Native
provider compaction can return a read-only brief, but Continue and Resume remain
the execution authority when hooks are unavailable.

## Persistence and safety

Records live in `<git-common-dir>/foreman/assignments/`, shared across project
windows and Foreman instances. OS file locks coordinate operations; reservations
persist after process exit and never expire by timer. Persisted intent bridges
Git and card-file writes. No automatic stash, reset, clean, or unrelated staging
is introduced. Manual Git remains possible and is checked at transitions.

Dirty-input checks use one private clone at
`<git-common-dir>/foreman/validation/checkout` and one Cargo target at
`foreman/validation/target`, reused across checks. The clone stays until the
next check for diagnosis. It does not register a worktree or overwrite user files.
Integration turns check the assignment lock at the start and across the final
preflight and ref move, never through checks.
A check policy that hard-codes external source/cache paths can defeat isolation;
those repositories need a suitable policy. Ordinary clean checks retain the
working checkout's build artifacts. Neither path guarantees incremental reuse
when inputs change.

## Validation evidence

The tests exercise persistence after each assignment write, identical retry
replay, competing processes, legacy reservations, wave/scope checks, blocked
cards, paused transitions, moved checkouts/targets, archived integration,
resubmission interruption, and repeated replacement-owner crashes. Target
rollback cannot release an assignment whose accepted commits disappeared.
The implementation was independently reviewed; reported recovery defects were
reproduced before fixes and retested afterward.

Commands and results on Windows, 2026-10-26:

| Check | Evidence |
|---|---|
| `cargo check --target-dir target/agent` | Passed with the existing warning baseline. |
| `cargo build --target-dir target/agent` | Passed; both provider skill copies and Codex metadata were rebuilt into the binary. |
| `cargo test --target-dir target/agent` | Passed: 1,716 tests, no failures, 19 ignored (16 existing and three explicit native-provider probes). |
| Real control-pipe roundtrip and legacy wire bytes | Passed. Named-pipe tests required execution outside the sandbox. |
| Dirty-input isolation and checks modifying tracked inputs | Passed, including tracked inputs inside `.foreman`. Git's helper pipes required execution outside the sandbox. |
| Native Claude lifecycle probe | Passed: two fixture outcomes committed and integrated through one Session and one registered checkout; a nested same-provider caller was rejected. |
| Native Claude compaction probe | Passed: an actual `SessionStart` event with `source: compact` returned the continuation note; the subsequent model response reported a marker present only in that note. The assignment remained mid-card. |
| Built hook helper failure paths | Missing route, unreachable pipe, malformed input, duplicate calls, and blocked stdin exited quietly. The blocked-stdin process exited in approximately 1.7 seconds including startup. |
| Codex automatic mid-turn compaction probe | Prepared; requires explicit approval for the isolated one-run hook-trust test. Normal trust settings are untouched. |

The installed native versions inspected were Claude Code 2.1.310 and Codex CLI
0.164.0. The Claude provider probes are explicit ignored tests under `wm::tests`
because they depend on installed/authenticated external CLIs; compaction uses
short model requests. The Codex probe is separately opt-in and refuses to run
without its explicit acceptance environment flag. Normal test runs never make
those model requests or bypass hook trust.

The final full suite includes the recovery and tracked-input regressions added
after the initial green run. Some early focused runs were blocked by sandbox
pipe permissions or a test executable still holding the linker output; those
checks were rerun after resolving the environment issue. The native Claude
lifecycle/nested-provider probe was also rerun against the final binary.

## Operational limits

Use a rebuilt Foreman host; an older running host does not understand the new
assignment commands. No GUI controls or rendering were added, and the running
host was not replaced or killed during implementation.

Native Windows Claude/Codex routing is supported by this implementation. WSL,
ambiguous daemon routes, missing provider session environment, and multiple root
providers fail closed. Hook support and trust remain provider prerequisites;
installation does not establish delivery, and no normal trust decision is changed
automatically. Status/Resume/Continue provide manual recovery without hooks.

The native tests establish the specific routes above. They are not a claim that
every provider version, daemon, or fork mode was exercised. No representative
before/after plan benchmark or token-saving comparison was performed; retaining
context and build artifacts is a behavior guarantee, not a promised speedup.
