# Kanban reference: orchestrating a plan

Execute related approved cards yourself in the current Session by default.
Keep the conversation, checkout, and build artifacts across cards. Delegate
independent work only when useful and authorized. Concurrent writers need
separate checkouts; ordinary sequential execution does not.

## Before beginning

Read the plan goal, constraints, acceptance criteria, and relevant repo rules.
Confirm existing authorization covers commits and local integration. If either
is missing, ask before beginning dependent execution. Publication requires its
own authorization. This skill does not grant permission to push.

Use independently acceptable outcomes as cards. Keep implementation, regression
tests, and necessary documentation together. Do not expand approved scope when
new cards appear. Preserve unrelated tracked and untracked files.

## Continue in this Session

Use `foreman kanban assignment` through `FOREMAN_EXE`. It requires a native
root Codex or Claude CLI under this terminal, an explicit instance pipe,
project/terminal identity, and the provider's current session identity.
Codex reads `CODEX_THREAD_ID`; Claude reads `CLAUDE_CODE_SESSION_ID`.
An inherited terminal ID alone is insufficient. If identity cannot be verified,
report the unsupported route; do not copy another session's identity.

For example (PowerShell; substitute approved IDs and the actual target):

```powershell
& $env:FOREMAN_EXE kanban assignment begin --id plan-run --request-id begin-1 --provider PROVIDER --cards a1,b2 --target main
```

Replace `PROVIDER` with `codex` or `claude`. `--plan "Plan name"` may replace
`--cards`; it captures the currently unfinished membership once. Save the JSON
result's generation and revision. Every mutation requires a new request ID and
those expected values. Retry a lost response with the identical command,
including its original request ID, generation, revision, and payload.

1. Begin returns `work` only after preparing `card/<id>` with no upstream and
   claiming the card. Implement only the returned brief on that feature branch.
2. Follow repo TDD, checks, and required review. Fix findings in this Session.
3. Checkpoint consequential decisions and verification gaps during long cards.
   Use `checkpoint --note "..."`; it replaces the note, not acceptance evidence.
4. When authorized, commit with the repo's required `Card: <id>` trailer.
5. Call `continue --submit` with the latest generation/revision and a new
   request ID. This submits committed work to the serialized integration queue.
6. Use `assignment wait --id plan-run --provider PROVIDER --timeout 30` while
   integration is queued. Waiting is bounded and read-only; timeout grants no
   permission to advance. Follow it with Continue to reconcile durable evidence.
7. Continue returns the current brief, the next prepared card, or completion.
   Never implement a successor while its predecessor is integrating.

Mutation example:

```powershell
& $env:FOREMAN_EXE kanban assignment continue --id plan-run --request-id submit-a1 --provider PROVIDER --generation 1 --revision 2 --submit
```

Read the result, not just the exit code. `work`, `waiting`, `blocked`,
`scope_changed`, `paused`, `complete`, `released`, and `recovery_required` are
distinct. Continue without `--submit` never queues work; after a cancelled
integration it returns `recovery_required`, and resubmitting is your decision.
Current wave eligibility is preserved; ties use creation time then card ID.
Completion covers the approved assignment, not all future plan membership.

A changed brief requires scope review. Blocked work is not automatically
reclaimed. Resolve obvious conflicts yourself, preserve others' work, rerun
checks, and resubmit. Ask the user only for unclear intent, consequential choices,
or destructive actions. Dirty files alone do not block integration: configured
checks use isolated committed inputs when necessary.

## Pause and recover

`pause` prevents the next claim after the current safe checkpoint. It does not
cancel in-flight integration or release the checkout. Silence, a dead process,
and compaction never grant another writer ownership.

Use `assignment status --id plan-run --provider PROVIDER` to inspect revision,
generation, observed branch, and queue phase without changing anything. Resume
with the observed generation/revision. A replacement Session must use
`resume --recover` explicitly after inspecting unfinished work. Resume fences
the previous owner, repairs interrupted claims, and returns recovery guidance.
Then Continue reconciles integration or finishes branch preparation.

A failed request does not block the next one. Replay the identical request only
when its response was lost, or when the refusal says it must be reconciled.

Use `release` with the latest generation/revision to end an assignment that
cannot continue, such as `blocked` or `scope_changed`, after the user decides.
It frees the checkout and leaves Git and the board unchanged. It is refused
while the current card has an integration request or branch preparation is
unfinished.

Maintain one concise continuation note: intent, decisions with reasons,
unfinished steps, useful discoveries, and verification gaps. Native provider
compaction may load a read-only recovery brief through SessionStart/compact.
Missing or untrusted hooks do not release ownership; use status/resume/continue.
Every assignment result has a `recovery` line saying whether this provider can
deliver the brief (not installed, disabled, needs Codex trust, or installed).
If it cannot, or no brief arrives after compaction, run `assignment status`
before acting on what you remember.
Never enable trust automatically or treat installation as proof of delivery.

## Optional independent delegation

The sections below apply only when delegation is authorized and useful.
Use `kanban dispatch` for delegated card workers. Do not hand-roll worktrees or
use `open` to bypass claims. Keep each concurrent writer in its own checkout.
No permanent coordinator or per-card worker is required for sequential work.

## Choose a model for each card

Honor an explicit user model/provider and budget first. Otherwise use this
routing policy per card; the orchestrator's provider does not constrain its
workers. Cards have no pre-dispatch provider/model preference; a claim records
the provider once launched. Record provider, exact model ID, reason, and any
fallback in your dispatch ledger.

| Model | Agent / exact model ID | Use when |
|---|---|---|
| Haiku 5.5 | `claude` / `claude-haiku-5-5` | Small, well-specified edits, extraction, summaries, inventories, or repetitive work with cheap, decisive checks. Keep the brief and context small. |
| Sonnet 5.5 | `claude` / `claude-sonnet-5-5` | Routine implementation, localized bugs, tests, UI iteration, and document polish. A balanced option for well-scoped cards. |
| Sol 6.1 | `codex` / `gpt-6.1-sol` | Ordinary coding, debugging, repository investigation, and evidence-based review. A balanced option at the same base API rates as Sonnet. |
| Opus 5.5 | `claude` / `claude-opus-5-5` | Ambiguous architecture, difficult cross-module bugs, long agentic implementations, or consequential review needing careful judgment. Preferred escalation within Claude. |
| Astra | `codex` / `gpt-6-astra` | Difficult research synthesis, conflicting evidence, novel technical questions, or demanding reasoning/coding where a wrong conclusion would cause substantial rework. Preferred escalation within Codex. |

There is no fixed provider default. Choose by task fit, actual cost/quota,
latency needs, and demonstrated results. When options are comparable, preserve
the user's configured provider/model preference rather than switching vendors.
If there is no preference or evidence separating them, either balanced model
is reasonable; record the choice without claiming it is proven better.

These are routing heuristics inferred from vendor guidance, not a measured
cross-provider ranking on this repository. Sonnet and Sol can both implement,
research, and review. Do not choose by provider stereotype or spend extra
just to include both providers. A large research task that is mostly collection
can use Haiku for bounded extraction or Sol/Sonnet for investigation; reserve
Astra for the hard synthesis. Split collection and synthesis into separate
cards/waves only when their outputs and ownership are independently useful.

Choose a capable model upfront for high-risk or poorly understood work.
For a bounded card that stalls, first distinguish missing context, permission
prompts, tool/build failures, and model reasoning failures. Correct the brief
or environment where needed; escalate the model after a substantive failed
attempt rather than paying for repeated identical retries. Keep the board's
restart rules below: do not launch a duplicate worker to bypass a live claim.

### Cost and speed evidence

Researched 2026-10-24. Standard API USD per million tokens, uncached input /
output: Haiku **$0.10 / $0.50** for prompts up to 100K input tokens; Sonnet
**$2 / $10**; Sol **$2 / $10**; Opus **$4 / $20**; Astra **$10 / $50**.
Haiku above 100K is **$0.50 / $2.50**. Sol and Astra above 272K input tokens
charge 2x input and 1.5x output for the full request. Cache, processing tier,
tools, and regional charges can change the bill. Subscription CLI usage and
quotas are not equivalent to these API rates; use the user's actual plan.

Optimize cost per accepted result and time to completion, including retries,
tool calls, review, and rework. At identical uncached token counts Astra costs
5x Sol, but it can be economical if it avoids enough failed work. No local
latency or success-rate comparison has been measured. Anthropic positions
Haiku for latency-sensitive work and Sonnet for fast everyday iteration;
OpenAI positions Sol as the balanced option and Astra for demanding work.
Do not turn those descriptions into unsupported tokens/second rankings.

Use the CLI's normal effort initially; lower effort can save time on bounded
work and higher effort can help difficult reasoning. Foreman's `--model`
does not set effort. Respect an explicit user effort setting and use only
controls supported by the installed CLI. Record model/effort, elapsed time,
acceptance result, retries, and usage/cost when exposed by the CLI on real
cards; compare similar tasks before changing these defaults.

Sources (recheck when names, pricing, or availability change):

- [Claude pricing](https://platform.claude.com/docs/en/about-claude/pricing),
  [Haiku 5.5](https://www.anthropic.com/claude-haiku-5-5),
  [Sonnet 5.5](https://www.anthropic.com/claude-sonnet-5-5),
  [Opus 5.5](https://www.anthropic.com/claude-opus-5-5).
- [Sol model and pricing](https://developers.openai.com/api/docs/models/gpt-6.1-sol),
  [Astra model and pricing](https://developers.openai.com/api/docs/models/gpt-6-astra),
  [OpenAI model guidance](https://developers.openai.com/api/docs/guides/latest-model).
- [Claude model controls](https://code.claude.com/docs/en/model-config),
  [Codex CLI controls](https://learn.chatgpt.com/docs/developer-commands?surface=cli).

### Dispatch the selected model

Use explicit IDs so rolling aliases or saved CLI defaults do not silently
change the choice. Examples (choose one per card):

```powershell
& $env:FOREMAN_EXE kanban dispatch <id> --agent claude --model claude-haiku-5-5
& $env:FOREMAN_EXE kanban dispatch <id> --agent claude --model claude-sonnet-5-5
& $env:FOREMAN_EXE kanban dispatch <id> --agent claude --model claude-opus-5-5
& $env:FOREMAN_EXE kanban dispatch <id> --agent codex --model gpt-6.1-sol
& $env:FOREMAN_EXE kanban dispatch <id> --agent codex --model gpt-6-astra
```

`--model` is a launch-local override for Claude/Codex, not persisted on the
card or in user settings. Omission preserves the CLI default; Grok does not
accept this Foreman flag. The provider CLI owns model access and validation:
a successful spawn is not proof the requested model is available. Inspect
the worker's startup/error report if it cannot begin, and record any reported
model substitution. Honor the user's exact selection; do not silently downgrade.

Both the CLI and running Foreman host must be updated for `--model`.
`kanban --help` checks the CLI only; it cannot prove host support. A new
CLI talking to an old host can have its model field silently ignored.
Use the host's `FOREMAN_EXE`, confirm the host was launched from the updated
build, and inspect the launched model before treating selection as honored.
If the host is old or its support is unknown, report that limitation rather
than claiming an exact selection. Do not invent flags, change global defaults,
or substitute `open` to bypass cards.
If a CLI/model is unavailable and the user did not require it, use an available
model appropriate to the same task and budget and record why.

## The delegated-worker loop

    find plan -> dispatch current wave -> wait --any <ids> -> review the
      worker -> act on the board -> close its terminal -> dispatch -> ...

Keep a short ledger as you go: card id, terminal id, provider/model, state,
and every board change you made. The ledger replaces re-reading the board.

1. **Find the plan.** One `list --json` at the start: a card's plan is
   `planned.name` and `planned.wave`. Copy the name from a card and note each
   wave's card ids. Do not re-list the whole board later; it costs every
   card's body.
2. **Current wave** = the lowest `planned.wave` among the plan's cards that
   are not `done`. Dispatch only that wave's `backlog` cards; never start a
   later wave early.
3. **Dispatch one at a time**, recording each reply's `terminal`:

       & $env:FOREMAN_EXE kanban dispatch <id> --agent $agent --model $model

   Set `$agent` and `$model` for this card using the policy above.
   `--agent` is required; `--model` pins the selected model for this launch.
4. **Wait on your in-flight cards by id**:
   `kanban wait --any <id> <id> ... --timeout 600`, listing every ledger
   card you have not yet handled. It prints the line of the card that moved
   and reports at once a listed card that finished while you were busy, so
   nothing is missed. Never use bare `wait --any` here: it only sees cards
   still in progress when it starts, and it also wakes on other people's
   cards. Do not run `integrate` or `done` for a worker — its prompt already
   does.
5. **Handle the card by its state** (next sections): review, act, close.
   Then drop it from the next wait's id list.
6. **Next**: when every card of the current wave is `done`, recompute the
   current wave (your board edits count) and dispatch it. Stop when no plan
   card is left that is not `done`, then report (see the end).

## A card came back done: review the worker

The worker's terminal holds its final report, and snapshot reads it
wherever its tab sits. Start with the tail:

    & $env:FOREMAN_EXE snapshot --terminal tN --tail 40

First check it is finished. A finished pane ends on the agent's final
message with an empty input prompt below it. A spinner, `esc to interrupt`,
or a tool call still printing means it is still in its last turn: handle
other cards and snapshot it again later. Raise `--tail` only when the
report is cut off. Use the report to direct verification proportional to
risk. For a narrow documentation change, inspect the diff and relevant checks.
For production behavior, inspect the changed logic, affected consumers, and
regression evidence; independently run focused checks where needed. A clear
report does not establish correctness. Read the transcript only when needed
to resolve missing evidence. Record material gaps before dispatching dependents.

Its dispatch prompt makes it end on four labeled lines; read those:

- **Landed:** what shipped. Check it matches the card; a gap is a
  follow-up.
- **Follow-ups:** deferred work, TODOs, bugs it found but left, checks it
  could not run.
- **Plan impact:** something that makes a later card wrong, redundant,
  already done, or dependent on new work.
- **Decisions:** what the human must decide, and assumptions to confirm.

A worker that skipped the lines still usually says these things; read its
last message for them.

## Act on the board

Turn each finding into one board change, or into a line for the human:

| Finding | Board action |
|---|---|
| Follow-up inside the plan's goal | `add` it, then `edit <new> --plan "<plan>" --wave N`: the current wave when the current wave's remaining cards need it, else the next wave |
| A later card must now wait for new work | `edit <later> --wave N` to move it behind that work; renumber later waves the same way |
| A later card is already done or no longer needed | `rm` it if it is `backlog`; note why for the report |
| A later card's body is now wrong | `list <id> --json`, then `edit <id> --body "<full corrected body>"` (the body replaces, never appends) |
| Work outside the plan's goal | `add` it with no plan; report it |
| Needs a human decision | No board change. Do not dispatch cards that depend on it; report it |

Rules:

- Base board changes on the worker's report, board state, or verified review
  findings. Do not add speculative cards or grow the plan past its goal.
- A card you add will be dispatched cold: its body is the worker's whole
  prompt. Give it what to do, file/doc pointers, acceptance criteria, and
  the gate commands, the way the plan's other cards do.
- Never edit, re-wave, or `rm` a card that is `in_progress`; its worker is
  already running from the old body. `rm` also refuses a card whose tree is
  dirty or ahead of its base.
- Record every change in the ledger.

## Close the worker's terminal

When the card is `done`, the pane is finished, and you have taken what you
need from it, close it before dispatching more:

    & $env:FOREMAN_EXE close tN

A finished worker's pane otherwise stays open forever, and a worktree card's
teardown waits for that pane to close. Rules:

- Always pass the id. A bare `close` closes YOUR OWN pane and kills you.
- Close only terminals from your ledger, and only for `done` cards.
- Never close a `blocked` worker's pane: the human reads it to decide.

## A card came back blocked, orphaned, or removed

- **Blocked**: snapshot the tail as above. If the blocker is a board
  problem you can fix from the report (the card needs splitting, it depends
  on a card in a later wave, its body points at the wrong file), make that
  board change and report it. Otherwise report the reason to the human.
  Leave its pane open either way. Restart is a board action — do not route
  around it with `open`.
- **Orphaned** (its terminal died) or **removed**: there is no pane to
  review. Report it.

Keep running cards that do not depend on it. Do not dispatch cards that
need the blocked card's work.

## Dispatch errors

- `foreman did not respond` or a bring-up error: worktree bring-up must
  finish inside foreman's 5 s reply window. A slow one spawns nothing; a
  reply lost after a spawn is undone and the card returns to Backlog. Check
  `list <id>`, then retry the same dispatch once.
- `held by live Session`, `integration is …`, not Backlog, cmd-shim: a real
  refusal. Report it. Do not retry blindly and do not fall back to `open`.

## Steering a running worker

A worker's card is its whole brief, and every post costs it a full turn.
Put scope, file ownership, and constraints in the card body before
dispatch, not in chat. Then:

- Post to a worker only when something changes what it must do next: a new
  owner, a changed plan, a decision it is waiting on.
- Never restate its card, the plan, or rules it already has. Never relay
  what it can read itself (the board, the repo, `chat --history`); name
  where it lives instead.
- Group related decisions into one post per worker. Avoid cosmetic
  follow-ups; promptly correct consequential errors or changed instructions.
- Do not invent a coordination protocol (leases, grants, release notices)
  on top of the cards. If two cards touch the same file, fix it on the
  board before dispatch: re-wave one, or give the file to one card's body.
- Routine acknowledgements are unnecessary. Silence does not prove progress;
  use the timeout and inspection steps below when progress is uncertain.

## A worker that never comes back

A worker stuck on a permission prompt stays `in_progress`, so `wait` only
times out. Exit 2 is a timeout or an unreachable foreman: run
`& $env:FOREMAN_EXE status --json`, and if that fails too, report foreman
unreachable and stop. Otherwise act on each ledger terminal's `agent_state`:

- `needs_you` — the worker is waiting on the human. Hand it over: snapshot
  its tail to read the prompt, post `@you` naming the terminal, its card,
  and what it asks, then `focus --terminal tN` so the human lands on it.
  Focus the next one only after the previous one has left `needs_you`; the
  human answers the prompt, never you. Keep waiting on the rest.
- `working` — still busy; wait again. If it is still `working` at the
  next timeout, snapshot the tail: Claude interrupted with Esc keeps
  reporting `working`, and Grok never reports `needs_you`.
- `idle` while its card is still `in_progress` — first `list <id> --json`:
  an `integration` whose `phase` is `queued` or `integrating` is landing,
  not stalled. Otherwise the worker ended its turn without closing out:
  snapshot the tail, answer a question the plan settles with a targeted
  post, else report it.
- No `agent_state` (state is off in Settings, or no hook has fired yet) —
  snapshot each ledger pane with `--tail 30` and report a prompt or a stall
  to the human.

Worktree cards cold-build their own `target/` and land through one queue a
card at a time, so a wave lands slowly; a timeout alone is not a stall.

## When the plan is finished

Report once, briefly: cards done, every board change from the ledger with
its reason, and every item that needs the human.
