# Terminal Images (kitty graphics, image paste, alt-key routing)

Status: in progress — see docs/superpowers/specs/2026-07-02-terminal-image-support-design.md

## Performance

Flood benchmark: median of 3 runs of `cmd /c type` on a 200k-line file (120 cols)
inside a release foreman pane (`foreman send` dispatch, `foreman snapshot` readback).

| Point | TotalSeconds (runs) | Median |
|---|---|---|
| Baseline (pre-change, eed13bf) | 0.782 / 0.793 / 0.766 | **0.782** |
| After (feat/terminal-images complete) | pending | pending |

Scanner micro-benchmark (Task 11): pending
