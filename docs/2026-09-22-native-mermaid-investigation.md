# Native Mermaid viewer: initial investigation

Investigation dated 2026-09-22 for card `3j0qmz`. This is a source review and
prototype recommendation, not measured Foreman performance or an implementation.
Upstream links describe the branches inspected; pin a release and revision before
running the proposed comparison.

## Recommendation

Try **mermaid-svg first**, with **mermaid-rs-renderer as the comparison candidate**.
Both can produce SVG entirely in Rust. Feed the result through Foreman's existing
resvg stack, then display a cached egui texture. This appears to be the shortest
route to a slim native viewer. Selecting the production dependency should wait
for a small compatibility and size benchmark.

Use a file-based `.mmd` / `.mermaid` viewer for the first experiment: fit, pan,
zoom, reload, and readable errors. Start acceptance testing with flowcharts and
sequence diagrams, then state, class, and ER diagrams. A renderer advertising a
diagram family does not establish support for every construct in that family.

## Candidate comparison

| Candidate | Evidence from upstream | Fit for Foreman |
|---|---|---|
| [mermaid-svg](https://github.com/xmiksay/mermaid) | Pure Rust SVG output; MIT. The inspected [manifest](https://raw.githubusercontent.com/xmiksay/mermaid/master/Cargo.toml) declares only `thiserror` as a runtime dependency. | Best first candidate for a small dependency footprint. That does not prove a small linked binary or accurate layouts. |
| [mermaid-rs-renderer](https://github.com/1jehuang/mermaid-rs-renderer) | Rust SVG renderer; MIT; upstream explicitly cautions that visual fidelity is still developing. | Strong comparison candidate. Use `default-features = false`; avoid importing its CLI and PNG stack. |
| [Merman](https://github.com/Latias94/merman) | Native renderer focused on agreement with upstream Mermaid. Its [current README](https://raw.githubusercontent.com/Latias94/merman/main/README.md) documents cancellation, deadlines, resource limits, and ongoing API changes. | Evaluate if the smaller candidates fail compatibility or bounded-work requirements. Do not assume it is too large without measuring. |
| [selkie-rs](https://github.com/btucker/selkie) | Native parser/layout/SVG renderer; MIT. Its [manifest](https://raw.githubusercontent.com/btucker/selkie/main/Cargo.toml) includes pest, fontdue, regex, chrono, and serialization; CLI and PNG are optional features enabled by default. | Viable reserve candidate, but no demonstrated size or fidelity advantage over the first two in this investigation. |

The [mermaid-svg README](https://raw.githubusercontent.com/xmiksay/mermaid/master/README.md)
describes a private Sugiyama layout engine, configurable themes/fonts, and broad
diagram support. Its responsive SVG sizing needs checking with a headless SVG
consumer. Test actual label measurement, wrapping, Unicode, nested subgraphs,
and unsupported syntax; dependency simplicity is not evidence of correctness.

The [mermaid-rs-renderer manifest](https://raw.githubusercontent.com/1jehuang/mermaid-rs-renderer/master/Cargo.toml)
keeps fontdb and ttf-parser even with defaults disabled. Optional PNG/scene paths
use a newer usvg line than Foreman's resvg dependency, so enabling them could
duplicate the SVG stack. Request SVG strings and use Foreman's rasterizer.
Its README's dramatic speedups compare against browser-backed CLI rendering;
those numbers do not establish Foreman latency, memory, or binary growth.

Merman's [feature manifest](https://raw.githubusercontent.com/Latias94/merman/main/crates/merman/Cargo.toml)
offers basic SVG without the default additional layout and math capabilities.
That is the appropriate starting point for a size comparison, with missing
capabilities explicitly recorded. Check the selected release's API rather than
copying examples from a moving branch.

## Existing Foreman pieces and the important gap

- `Cargo.toml` already includes resvg with default features disabled, and keeps
  eframe on glow. No new GPU backend is needed for an SVG-to-texture path.
- `src/icons.rs`, `rasterize`, already converts SVG into egui color pixels.
  It assumes small square icons and runs synchronously; it is a reference for
  conversion, not a diagram worker to reuse unchanged.
- `src/imageview.rs`, `ImageView`, provides a PNG viewer with texture caching
  and reusable fit/pan/zoom math. It does not accept Mermaid or general SVG.
- `src/control.rs`, `parse_view_args`, and `src/workspace.rs`, `ContentSnap`,
  are relevant if the experiment later becomes a persistent file viewer.

**Text is the gap:** resvg's [feature declarations](https://raw.githubusercontent.com/linebender/resvg/v0.45.1/crates/resvg/Cargo.toml)
put SVG text behind `text`, separately from system-font discovery. The icon
configuration does not supply the font-enabled diagram pipeline. Enable text
and supply a deliberate font database; match layout and rasterization fonts.
Try existing bundled fonts before adding another asset or scanning every system
font. Missing CJK/emoji glyphs and fallback metrics need explicit fixtures.

## Proposed implementation shape

Keep the content window thin. A bounded background worker reads capped source,
parses, lays out, emits SVG, and rasterizes. The GUI applies only the latest
generation and uploads the finished pixels. Source/theme/font changes invalidate
layout; zoom and DPI changes invalidate only raster scale. Idle windows do no
parsing or layout and do not schedule continuous repaints.

Cache a bounded raster at the current useful scale. During zoom, scale the
existing texture immediately and replace it after a debounced background render.
Never allocate a full diagram bitmap at an arbitrary zoom: RGBA storage alone is
`width * height * 4` bytes. A 4096-square raster consumes 64 MiB before any CPU
copies or GPU storage. Large diagrams need a pixel cap, with clipping or tiling
considered only if the first experiment demonstrates a need.

Bound source size, graph complexity, pending requests, output pixels, and cached
textures. Reject unsupported input with an explanation. Dropping a stale result
does not cancel the computation that produced it: investigate cancellation in
the selected library, and avoid promising hard timeouts for an ordinary thread.
Keep source available for copying and errors; a raster has no selectable labels
or useful diagram accessibility by itself.

## Alternatives to defer

- **Embedded browser / official Mermaid JS:** does not fit the requested native,
  slim path. An external renderer can be a reference oracle for the experiment.
- **A custom Mermaid parser and layout engine:** compatibility becomes Foreman's
  maintenance responsibility; existing native libraries deserve a trial first.
- **Direct egui vector painting:** potentially sharper at any zoom, but creates
  a larger integration. mermaid-rs-renderer's documented scene API normalizes
  SVG and requires paths, glyph outlines, clipping, gradients, and compositing;
  it is not a ready-made egui widget. Benchmark the texture route first.
- **Full Markdown rendering, editing, animated diagrams, clickable callbacks:**
  separate product work from opening a static diagram file.

## Next experiment and decision gates

Use isolated scratch binaries, the same Rust toolchain and release settings,
and pinned candidate versions. Compare an empty harness, each renderer, and
each renderer plus the font-enabled Foreman raster path. Then measure the
incremental linked cost in a Foreman branch before selecting a dependency.

Proposed targets below are discussion starting points, **not measurements or
approved requirements**:

| Measure | Initial target |
|---|---|
| Incremental stripped release executable size | At most 5 MiB |
| Cold first diagram, including fonts, raster and upload | Under 100 ms for a representative 50-node diagram |
| Warm update, same representative corpus | p95 under 50 ms |
| Added GUI work during pan/zoom | p95 under 1 ms, with layout off the GUI thread |
| Incremental CPU memory for one ordinary open viewer | Under 32 MiB; report GPU texture storage separately |
| Closed or unchanged viewer | No background polling/render loop; release owned textures and results |

Record cold and warm timings separately. Use small, 50-node, and 200-node
flowcharts plus sequences with loops/notes, composite states, class/ER labels,
cycles, long labels, Unicode, invalid syntax, and oversized inputs. Dense edges
and nested subgraphs matter more than node count alone. Include actual diagrams
from user workflows before calling coverage sufficient.

Visually compare readable labels, edge routing, clipping, themes, and zoom at
multiple DPI scales against an official Mermaid reference. Capture screenshots
of the prototype before claiming visual success. Exercise terminal output while
loading diagrams to verify the viewer does not steal frame time.

Choose mermaid-svg if it clears these checks. Move to mermaid-rs-renderer if
measured fidelity justifies its extra cost. Bring in Merman if parity or resource
control remains the blocker. No candidate was compiled or visually validated in
this initial investigation; binary size, RSS, Windows render latency, and actual
compatibility remain open measurements.
