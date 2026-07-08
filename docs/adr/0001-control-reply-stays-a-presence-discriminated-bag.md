# ADR 0001 — The control reply stays a presence-discriminated bag (typed `Reply` sum rejected)

- **Status:** accepted (rejection recorded)
- **Date:** 2026-07-07
- **Source:** architecture-review candidate 05, killed by a 3-skeptic adversarial
  verification pass (all three: solution *harmful*, high confidence)

## Context

An architecture review proposed replacing `OpenReply` (one struct, `ok: bool` +
7 `Option` fields, discriminated by which field is `Some`) with a typed `Reply`
enum, and the six per-verb request structs with one `#[serde(tag = "cmd")]`
enum, so `report()` could match instead of sniffing and `serve()` could stop
double-parsing.

## Decision

Rejected. `OpenReply` stays a bag of optionals; `report()` keeps its explicit
presence checks; `serve()` keeps the Verb-first double parse.

## Why (the load-bearing facts)

1. **The v1 reply wire carries no discriminant — by design.** Three different
   verbs already produce byte-identical replies: status, `chat --history`, and
   a plain snapshot all serialize to `{"ok":true,"history":[...]}` (reply
   construction in `wm.rs::handle_ctrl`); a post-ack with `seq = None` is
   byte-identical to a send-ack (`{"ok":true}`). The variant is only knowable
   from which verb the client sent — information that lives *outside* the
   message. A typed enum therefore cannot round-trip from the bytes:
   - a serde tag adds a wire field → breaks the wire-compat-v1 non-negotiable
     (replies stay byte-identical; CLI and GUI can be different builds, and
     globally installed skills speak this protocol);
   - `#[serde(untagged)]` just relocates the presence-sniffing into serde's
     first-match variant resolution — harder to see than 10 explicit lines,
     with overlapping variants that don't round-trip stably.
2. **The existing compat tests do not pin bytes** (they assert key-absence +
   reparse only), so "the tests still pass" would not certify byte-identity.
3. **The double parse costs nanoseconds** once per CLI command on a
   per-connection background thread; and the Verb-first parse is what produces
   the deliberate `unknown cmd: X` wire error.
4. **No defect has ever been attributed to the bag.** `report()`'s if-chain is
   output-*format* selection (JSON vs line-per-line), used uniformly by all six
   verbs — the code honestly mirrors the wire's actual discrimination scheme.

## Consequences / revisit when

- Any future reply-shape change is a **wire-protocol change → ask-first, user
  decision** (see `foreman-change-control`).
- The request-side tagged enum is byte-feasible in principle (`cmd` is the
  first field of all six structs) but nets ~nothing: the 6-arm match survives
  (CtrlMsg carries per-verb reply senders), `cmd` must be deleted from six
  structs (~29 construction sites), and the `unknown cmd:` error text changes.
  If ever wanted: request enum only, gated by NEW byte-equality golden tests
  for all six request shapes, with explicit user sign-off first.
- Revisit only alongside a deliberate protocol v2.
