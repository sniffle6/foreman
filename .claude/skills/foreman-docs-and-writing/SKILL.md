---
name: foreman-docs-and-writing
description: Use when writing or updating any foreman doc, epic, contract, plan, spec, CONTEXT.md glossary entry, ADR, or skill; when deciding whether HANDOFF.md, CLAUDE.md, or a feature doc can be trusted; when a doc contradicts code (docs/foreman.md, epic status headers); when citing code from prose (the no-line-numbers, no-counts doctrine); when composing commit messages (type(scope) subject, Co-Authored-By trailer, the literal-"@" subject incident); or when editing .claude/skills or .codex/skills copies.
---

# foreman-docs-and-writing — the docs of record

How foreman's documentation system works: which doc to trust, which docs are
traps, the house style for writing new ones, the citation doctrine, glossary
discipline, commit-message conventions, and how the repo's skills are
maintained. Repo root: `H:/claude code/foreman`.

## The citation doctrine (read this before you write a sentence about code)

**No doc or skill may write down a fact that the code already holds in
machine-readable form.** No counts, no enumerations, no `file.rs:NNN`. Cite
`file.rs` plus the **symbol**, or cite the **command** that derives the fact.

- ✅ `src/wm.rs` `fn term_env`  •  ✅ `rg -c '#\[test\]' src/ | sort -t: -k2 -rn`
- ❌ `src/wm.rs:2106`  •  ❌ "N tests green"  •  ❌ "the six control verbs are …" <!-- cite-guard: ok -->


The evidence: an audit of every markdown file in this repo found that **nearly
every wrong claim was a number, an enumeration, or a status assertion — almost
none were reasoning.** Symbol names survive; line numbers do not. A census is a
*query*, not a fact; the moment you write one down it starts rotting, and the
reader has no way to tell a stale count from a fresh one. (If a commit span
genuinely matters to a point you are making, cite the command that derives it —
`git rev-list --count <baseline>..HEAD` — not the digits it printed today.)

**Half of this rule is enforced, so you will hear about it.** A PostToolUse hook,
`.claude/hooks/cite-guard.ps1`, runs after every `.md` edit under `.claude/skills/`,
`.codex/skills/`, `.claude/agents/` and `docs/`, and reports two things: a
`src/foo.rs:NNN` cite into a module that actually exists, and a backticked symbol
named beside a `src/*.rs` path that has zero hits in the tree. It derives the
symbols from your text rather than checking a list of known-dead names — a list
would be a census, which is the thing this section bans.

It does **not** see the third rot class. Status assertions ("designed, not
built", "not yet honored", an epic header that still says PLANNED) read as
perfectly good prose to a grep, and they were the second-largest source of wrong
claims in the audit. Nothing but the discipline below catches those.

**Never read `cite-guard: clean` as "this doc is accurate."** It proves every
symbol the doc names still exists. It says nothing about whether the *behavior*
described around those symbols is still true, and a doc that names only live
symbols passes green while being wrong in every sentence that matters. That is
not hypothetical: `docs/project-directories.md` documented the directory picker
as a highlight-and-drill list — Right/Tab to descend, type to filter — long
after it was rewritten as an address-bar path field where Tab is eaten and Enter
accepts the field, not the highlight. Every symbol it cited was live, so the
hook reported clean the whole time; it was caught by a human reading it against
the running app. When you change how something *behaves* without renaming
anything, the guard will not save you — reread the feature doc yourself.

Three ways to say "I meant it", in order of preference: put a negation cue on the
line (`any hit means…`, `expect nothing`, `was deleted`, `no longer`), which is
how a negative probe or a removal record should read anyway; declare the whole
file non-current in its header (`**SUPERSEDED**`, `**Status: designed, not
built**`), which suppresses the symbol check but *not* the line-cite check,
because a line number is still a lie a reader may try to follow; or mark the one
line with `<!-- cite-guard: ok -->`. Reach for the marker last — needing it often
usually means the sentence should have named a symbol instead.

Run it over everything with `pwsh -File .claude/hooks/cite-guard.ps1 -All`. It is
clean as of the 2026-08-25 cleanup, so any output is something you just added.

The rider: **docs must not carry status.** In that same corpus, every file with
a status header had rotted and every file describing mechanism had held. If you
must record state, record it where state belongs — the issue tracker, a
tracker doc, or git — and let the doc describe how the thing works.

The only citations that keep their digits are pins into third-party crate
sources (a vendored egui version, say), because those are version-locked and
expensive to re-derive. Label such a block with the crate version so a bump
invalidates one section instead of the whole file.

## The doc map — trust table

**A doc's own status header is evidence, not truth.** Verify against code.

| Doc | Role | How to trust it |
|---|---|---|
| `CLAUDE.md` | **Router, not a library** (thinned 2026-08-24) | Deliberately minimal: identity, the destructive gotchas, the structural invariants, a skill routing table, working agreement. It carries no build loop, no gotcha dictionary, no module map — those live in skills and `docs/HANDOFF.md`. Rationale + the don't-re-fatten rule: `docs/agents/context-layout.md`. |
| `AGENTS.md` | Codex counterpart, same router shape (thinned 2026-08-24) | Mirrors CLAUDE.md's structure but routes by **file path** into `.claude/skills/`, since Codex does not get skill descriptions auto-injected. Carries the Codex-only sections with no other home: why `.claude/skills/` is readable by Codex, and the paired skill-copy rules. Adding a project skill means adding a row to **both** tables. |
| `docs/HANDOFF.md` | Authoritative deep doc (CLAUDE.md: "HANDOFF.md wins on any conflict") | **Trust section-by-section**, and check §2's "Architecture / files" against `ls src/*.rs` — that list is the single complete module map and it drifts whenever a module is added. |
| `CONTEXT.md` | **Glossary of record** (ubiquitous language) | Glossary ONLY, by its own charter (stated in its opening lines). |
| `docs/adr/` | Numbered architecture decision records | The decision + its rejected alternatives. Long-lived: an ADR is superseded by a later ADR, never edited to match new reality. |
| `docs/<feature>.md` | Subsystem feature docs — one per subsystem, `ls docs/*.md` for the live set | Generally current. Spot-check by grepping the symbols the doc's "Key files" section names; if a named symbol is gone, the doc is describing deleted code. |
| `docs/foreman.md` | **TRAP** (older narrative notes) | Carries a partial-supersession banner: only the "Tiling tree + floating" section near the end is current; the snap-zone sections above it describe deleted code. Read the banner first. |
| `docs/epics/*.md` | Design + decision history | **Decision history reliable; status headers LAG code.** See below. |
| `docs/contracts/` | Pinned seam agreements + status trackers | When a contract's header and its remaining-work tracker disagree, **the tracker wins over the contract header** — the tracker is edited as work lands, the header is written once. |
| `docs/superpowers/specs/` | Design records — the decision AND its rejected alternatives | **Tier-D history: read for *why*, never for *how*.** Kept permanently; do not edit them to match later reality. Some `src/` module headers cite one by path (`rg -n 'docs/superpowers' src/`). |
| `docs/superpowers/plans/`, `docs/plans/` | Checkbox-executable plans for work that has **not shipped yet** | A plan is deleted once its work lands (`docs/superpowers/README.md`), so anything still here is unbuilt — read it as a proposal, never as a description of the tree. |
| Dated snapshot docs (`docs/YYYY-MM-DD-*.md`) | Point-in-time session findings | Historical by construction. The date in the filename is the claim's expiry warning. |
| `docs/followups-latency-and-control.md`, `docs/chat-missing-features.md`, `docs/chat-persistence.md` | Gap lists and session snapshots that are *not* date-named | Historical — the filename carries no expiry warning, so check the date in the title line. Believe their own headers ("designed, not built"); do not read them as current state. Other skills cite `followups-…` as live evidence; it is a session snapshot, so re-verify against code before acting on it. |

### Epic status headers lag code

The failure is structural, not incidental: an epic's `**Status:**` line is
written once at design time and nobody goes back. `keyboard-control-epic.md`
still opens "designed, not started" while `src/keymap.rs` ships and the leader
key is in daily use. Never take an epic header as current — grep for the
symbols the epic proposes and let the tree answer:

```powershell
Set-Location "H:/claude code/foreman"
Get-ChildItem docs/epics/*.md | ForEach-Object { "$($_.Name): $((Get-Content $_ -TotalCount 4) -match 'Status')" }
```

This is the rider from the citation doctrine in its most expensive form. When
you write an epic, put the state in the tracker, not the header.

## The precedence rule (verified reality)

The manifest (CLAUDE.md) declares "HANDOFF.md wins on any conflict". That rule
predates the drift documented above. Do not rewrite the manifest; **operate by
this order instead**:

1. **Code** (`src/`) — always wins.
2. **Subsystem feature doc** (`docs/<feature>.md`) — freshest prose.
3. **HANDOFF.md** — deep architecture, coordinate model, build loop, gotchas.
4. **CLAUDE.md / AGENTS.md** — routing and invariants only; they no longer
   attempt to describe modules.
5. **Epics / contracts** — decision history yes; status headers only after
   checking the tracker or code.

**Before acting on any doc claim, verify it against code.** One grep is cheaper
than one wrong build. Example — is a symbol a doc names still real?

```powershell
Get-ChildItem src/*.rs | Select-String -Pattern "compose_zone|snap_or_tab" | Select-Object -First 5   # empty = the doc is describing deleted code
```

## House style for feature docs

- **One doc per feature, not per commit.** Named after the feature:
  `docs/tiling-tree.md`, `docs/chat-delivery.md`.
- **Update the existing doc** when a feature changes; check `docs/` for an
  existing home before creating a new file.
- **Plain language** (grug-brain): short sentences, no jargon walls, written so
  a cold reader or a Sonnet-class model gets it. The repo's docs already read
  this way — match them.
- **Standard shape:** `## What it does`, how to use it, gotchas, and always a
  **`## Key files`** section listing the main src files with the load-bearing
  symbols. That section is what makes a doc verifiable later — it is the
  handle a future reader greps to find out whether the doc still describes
  living code.
- **Skip docs for trivial changes** (typo fixes, one-liners).
- **Code comments explain WHY, not WHAT.** No internal task/PR references in
  comments; external upstream links (an upstream issue number, a spec) are fine.
  Look at the module-level comments in `src/wm.rs`, `src/frame.rs`, or
  `src/control.rs` for the register — each opens by explaining why the module
  exists before it explains what it does.
- Doc changes ride the feature commit or get their own `docs(scope):` commit —
  see commit conventions below. Doc updates are part of a change's definition of
  done; see **foreman-change-control** for the gating.

### Checklist — when a feature ships

- [ ] Feature doc created or updated (with `## Key files`).
- [ ] The plan that drove the work is **deleted** — after its durable content
      moved into that feature doc, and after any inbound reference to it is
      rewritten (`rg -n 'docs/superpowers/plans' . src/`).
- [ ] Any doc the change supersedes gets a **banner** (next section).
- [ ] Epic status header updated if the epic's phase state changed.
- [ ] New named seam? Add a CONTEXT.md glossary entry (below).
- [ ] Project skill added/renamed? Add a row to the routing table in **both**
      `CLAUDE.md` (by skill name) and `AGENTS.md` (by `SKILL.md` path).
- [ ] Module added/renamed? Update the `docs/HANDOFF.md` §2 "Architecture /
      files" list — **not** CLAUDE.md. CLAUDE.md carries no module map by
      design (`docs/agents/context-layout.md`); adding one line "just this once"
      is exactly how it bloated to the size that forced the 2026-08-24 thinning.
- [ ] Touched an **embedded** skill (`foreman-dispatch`, `foreman-chat`,
      `foreman-icat`)? Sync the `.codex/skills` twin, its `agents/openai.yaml`,
      and **rebuild** — see Skill maintenance.

## Supersession discipline

When a doc's subject is replaced, **add an explicit banner at the top naming
the successor and the date.** Do not delete the doc (history has value); do not
leave it bannerless (it becomes a trap).

- **Good example** — `docs/epics/window-tabbing-split-epic.md` opens with a
  `PARTIALLY SUPERSEDED` banner that names what died, what still holds, and the
  successor. That is the shape to copy.
- **The incident that produced this rule** — commit `b42e92e` ("supersede
  zone-snap docs") bannered the epic and updated HANDOFF/CLAUDE.md but never
  touched `docs/snap-tiling.md` itself, so for two months that file read as live
  documentation of code (`compose_zone`, `snap_or_tab`, `zone_rect`, `Zone`)
  that had been deleted. Its banner finally landed 2026-08-24 using the template
  below. The lesson stands: **the supersede commit must banner every doc the
  change kills, not just the one you were looking at.**

Banner template:

```markdown
> **SUPERSEDED (YYYY-MM-DD):** <what replaced this and why, one sentence>.
> See `docs/<successor>.md`. Kept for decision history only.
```

## CONTEXT.md glossary discipline

`CONTEXT.md` is the vocabulary of record and **glossary only** — terms, one-line
meanings, and `_Avoid_:` synonym lists. No implementation detail; that lives in
`docs/` (CONTEXT.md's opening lines say so itself).

- **Add an entry when you introduce a named seam.** Observed pattern: each seam
  entry landed alongside the commit that created the seam, not later.
- **Use the glossary terms exactly** in docs, skills, commit messages, and code
  comments — Win (not pane), Session (not terminal/PTY), Project (not
  workspace), and the rest. Read the file for the live set rather than trusting
  a list quoted here; it grows.
- **Never use an entry's `_Avoid_:` synonyms.** If you catch a doc using one,
  fixing it is in-scope for any docs pass.
- Entry shape: bold term, 1-3 line meaning, `_Avoid_:` line. Match the file.

## Commit conventions

Read the live convention rather than trusting a list: `git log --oneline -40`.

- **Format:** `type(scope): subject`. Types in use include `feat`, `fix`,
  `docs`, `chore`, `refactor`, `perf`, `style`. Scope is the module or feature
  touched.
- **Body says why** — what moved, and an evidence line.
- **Trailer:** `Co-Authored-By: Claude <model> <noreply@anthropic.com>` on every
  agent-authored commit.
- **Prefer a new commit over `--amend`.**
- **The literal-"@" subject incident (`37687b5`):** its subject is a lone `@`,
  the real subject is buried in the body, and the body ends with another `@`.
  The pattern matches PowerShell here-string delimiters (`@'` / `'@`) being
  passed through a shell that treated them as literal text. Guard rails:
  - Write multi-line messages with a real PowerShell here-string — opening
    `@'` at end of the `git commit -m` line, closing `'@` at **column 0** on its
    own line — or use `git commit -F <msgfile>`. Never paste a here-string into
    a POSIX (Git Bash) shell.
  - **Verify after committing:** `git log -1 --format=%B` — three seconds, and
    it would have caught `37687b5`.

## Skill maintenance

Repo skills live in `.claude/skills/`. Four of them are **twinned** into
`.codex/skills/`: `foreman-dispatch`, `foreman-chat`, `foreman-icat`, and
`build-screenshot`. Three of those four are **embedded in the exe**:
`foreman-dispatch`, `foreman-chat`, `foreman-icat` — `build-screenshot` is not.
Derive the embed list, never trust a list written down:

```powershell
Select-String -Path src/skills_install.rs -Pattern 'include_str!'
```

| Rule | Detail |
|---|---|
| `.claude/skills/` is the source; `.codex/skills/` is the Codex adaptation | Keep them **semantically synced** when behavior changes; adapt commands, don't copy text: Claude variants use `claude` / `claude -p`, Codex variants use `codex` / `codex exec`. Codex variants also carry `agents/openai.yaml`, which repeats the description — update it when the description moves. |
| **Editing an embedded skill requires a REBUILD to propagate** | `src/skills_install.rs` `include_str!`s every embedded source (Claude SKILL.md, Codex SKILL.md, Codex openai.yaml) at compile time, lists them in `CLAUDE_SKILLS` / `CODEX_SKILLS`, and `install()` — gated on `Settings::install_skills` — overwrites the **global** Claude and Codex skill dirs at GUI startup, best-effort, never blocking launch. A source edit does nothing globally until the exe is rebuilt and run: the installed copy keeps serving the old text to every agent on the machine while the repo looks fixed. This is the failure this row exists to prevent. |
| Renaming or dropping a shipped skill | Add the OLD directory name to `OBSOLETE_SKILLS` in `src/skills_install.rs` so stale global installs are deleted on next launch. The directory name MUST equal the frontmatter `name:`. |
| `build-screenshot` is twinned but NOT embedded | It exists in both `.claude/skills/` and `.codex/skills/`; editing it needs no rebuild. The Claude copy carries `disable-model-invocation: true` (it spawns a real window on the user's desktop, so it is user-triggered only); the Codex copy does not. |
| User-facing skills keep their **stop sign** | dispatch and chat both open with "**This skill is complete. Do NOT read foreman source or docs to learn … mechanics**". Preserve that line in any edit — those skills serve agents who must not spelunk src. The developer-facing skill library must likewise defer operational usage of dispatch/chat/icat to them. |

## Plans, specs, sessions — write like the repo does

The lifecycle itself (hunch → spec → plan → result) is owned by
**foreman-research-methodology**; this is only the house format:

- **Specs** (`docs/superpowers/specs/`) record **rejected alternatives and
  accepted tradeoffs**, not just the winner — the inspection epic's
  "design-it-twice … three parallel interface explorations → the hybrid" and the
  keyboard-control epic's "Decision history (settled with the user): …
  → **rejected**" blocks are the model.
- **Plans** (`docs/superpowers/plans/`, `docs/plans/`) are checkbox-executable:
  `- [ ] **Step N:** …` steps a cold session can run top-to-bottom. **A plan is
  scaffolding, and it is deleted when its work ships** — the code becomes the
  truth and `docs/<feature>.md` becomes the explanation, so a surviving plan
  would be a third, stale copy a reader can mistake for current design. That
  makes shipping a feature a two-part act: move whatever a future reader needs
  into the feature doc *first*, then `git rm` the plan. `docs/superpowers/README.md`
  carries the rule and the `git log --diff-filter=D` recipe for reading one back.
- All are dated `YYYY-MM-DD-<slug>.md`. A spec becomes history the moment the
  work lands — never edit it to match later reality; supersede or let it age.

## When NOT to use this skill

- Whether a change is *allowed* and how it is gated → **foreman-change-control**
  (this skill owns how to write it up; that one owns the gate).
- What counts as evidence for a claim → **foreman-validation-and-qa**.

## Provenance and maintenance

Re-verify these before leaning on them — they are the claims here that code can
move out from under. Each is written so a stale answer *fails visibly*; a check
that greps for a symbol which no longer exists would pass silently and is worse
than no check at all.

| Claim | Re-verify with |
|---|---|
| The embedded-skill list drives the rebuild rule | `Select-String -Path src/skills_install.rs -Pattern 'include_str!'` — the file list it prints IS the answer |
| HANDOFF §2's module map still covers the tree | `(Get-ChildItem src/*.rs).Name` vs `Select-String -Path docs/HANDOFF.md -Pattern 'src/\w+\.rs'` — compare the two sets |
| CLAUDE.md has not been re-fattened (routing only) | `(Get-Content CLAUDE.md).Count` — over ~110 lines means something belongs in a skill |
| Epic status headers lag | `Get-Content docs/epics/keyboard-control-epic.md -TotalCount 4` vs `Test-Path src/keymap.rs` |
| The "@" commit incident | `git show 37687b5 --no-patch --format=%B` |
