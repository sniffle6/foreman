# Foreman

A fast, native desktop for running many AI-agent terminal sessions — a window
manager where each **project** is a window, and each project is a sandbox hosting
**terminal** sub-windows (a browser pane later). Rust + egui, real PTYs
(`portable-pty`), full terminal emulation (`alacritty_terminal`).

**Start here:** [`docs/HANDOFF.md`](docs/HANDOFF.md) — full state, architecture,
the build/verify loop, the gotchas, and what's next.

## Run

```
cargo run            # debug
cargo run --release  # the "is it fast" build
```

Windows toolchain note: builds use the GNU toolchain + the w64devkit linker on
PATH. See `docs/HANDOFF.md` § "Gotchas" if a fresh machine won't link.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option. Bundled third-party components
keep their own licenses (Hack font: `assets/fonts/LICENSE-Hack.md`; ConPTY:
`assets/conpty/LICENSE`).
