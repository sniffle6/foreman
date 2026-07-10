# Domain Docs

How the engineering skills should consume this repo's domain documentation when exploring the codebase.

## Layout

**Single-context** — one glossary and one ADR tree for the whole app.

```
/
├── CONTEXT.md                 ← domain glossary (terms + avoid-list)
├── docs/adr/                  ← architectural decision records
│   ├── 0001-control-reply-stays-a-presence-discriminated-bag.md
│   ├── 0002-frame-and-inspect-walks-stay-separate.md
│   └── 0003-windowmanager-stays-one-uniform-recursive-type.md
└── src/
```

There is no `CONTEXT-MAP.md`.

## Before exploring, read these

- **`CONTEXT.md`** at the repo root
- **`docs/adr/`** — ADRs that touch the area you're about to work in
- For deep architecture/session context, also **`docs/HANDOFF.md`** (foreman-specific; not required by generic skills, but authoritative for this repo)

If any of these files don't exist, **proceed silently**. Don't flag their absence; don't suggest creating them upfront. The `/domain-modeling` skill creates them lazily when terms or decisions actually get resolved.

## Use the glossary's vocabulary

When your output names a domain concept (in an issue title, a refactor proposal, a hypothesis, a test name), use the term as defined in `CONTEXT.md`. Don't drift to synonyms the glossary explicitly avoids.

If the concept you need isn't in the glossary yet, that's a signal — either you're inventing language the project doesn't use (reconsider) or there's a real gap (note it for `/domain-modeling`).

## Flag ADR conflicts

If your output contradicts an existing ADR, surface it explicitly rather than silently overriding:

> _Contradicts ADR-0003 (WindowManager stays one uniform recursive type) — but worth reopening because…_
