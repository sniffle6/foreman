# docs/superpowers — specs, and the plans that have not run yet

## What lives here

- **`specs/`** — design documents. A spec records the decision *and the
  alternatives that were rejected, with why*. That reasoning is the expensive
  thing to re-derive, so specs are kept permanently, even long after the code
  they describe has moved on. Several `src/` module headers cite a spec by path
  (`rg -n 'docs/superpowers' src/`). The `.py` files under `specs/` are the
  frozen paste-simulation research models; `docs/wide-chars.md` names them as
  the surviving record.
- **`plans/`** — checkbox-executable implementation plans, and **only for work
  that has not shipped yet.** `ls docs/superpowers/plans/` is the live list.

## The rule: a plan is deleted when its work ships

A plan is scaffolding for building the thing. Once the thing exists, the code is
the truth and `docs/<feature>.md` is the explanation — the plan is a third,
stale copy that a reader can mistake for current design. So when a plan's work
lands, the plan goes, and whatever a future reader actually needs moves into the
feature doc.

Most of `plans/` was removed on 2026-08-25 under this rule. Nothing was lost:
git keeps every one of them.

```sh
# every plan ever deleted, with the commit that removed it
git log --diff-filter=D --name-only -- docs/superpowers/plans/

# read one back
git show <commit>^:docs/superpowers/plans/<name>.md
```

A handful of history documents still cite a plan path that no longer resolves in
the working tree. Those citations were deliberately left alone — a spec is a
record of what was true when it was written, and rewriting it to match a later
deletion would be falsifying the record. Use the commands above to follow one.

## Where to go instead

| You want | Read |
|---|---|
| How a shipped feature works | `docs/<feature>.md` — `ls docs/*.md` |
| Why a design went the way it did | `specs/`, or `docs/adr/` for the big ones |
| The lifecycle these documents belong to | the **foreman-research-methodology** skill |
| How to write one | the **foreman-docs-and-writing** skill |
