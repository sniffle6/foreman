//! `foreman icat <file.png>` — print an image into the current pane by
//! emitting the kitty graphics APC subset `graphics.rs` renders (Codex chunk
//! format: full header + m=1, bare m=1 continuations, m=0 final). No pipe,
//! no GUI change: stdout IS the PTY, so the running foreman's scanner picks
//! the bytes up like any other client. Also works in kitty/WezTerm.

use base64::Engine as _;

/// Kitty chunk payload cap (base64 chars per APC packet), per the protocol.
const CHUNK: usize = 4096;

/// Assumed cell aspect for scaling: a cell is ~8px wide, ~16px tall. Only the
/// 1:2 ratio matters (c/r are resolved to real pixels by the renderer); the
/// 8px base keeps tiny images from being stretched across the pane.
const CELL_W: u32 = 8;
const CELL_H: u32 = 16;

/// Chunked kitty transmit-and-display for a PNG, sized to `cols` x `rows`
/// cells. q=2 keeps the terminal from writing replies into the shell line.
pub fn encode(png: &[u8], id: u32, cols: u16, rows: u16) -> Vec<u8> {
    let b64 = base64::engine::general_purpose::STANDARD.encode(png);
    let chunks: Vec<&str> = b64
        .as_bytes()
        .chunks(CHUNK)
        .map(|c| std::str::from_utf8(c).unwrap()) // base64 is ASCII
        .collect();
    let mut out = Vec::with_capacity(b64.len() + chunks.len() * 32);
    for (n, chunk) in chunks.iter().enumerate() {
        let last = n + 1 == chunks.len();
        let packet = if n == 0 {
            let m = if last { "" } else { ",m=1" };
            format!("\x1b_Ga=T,t=d,f=100,c={cols},r={rows},q=2,i={id}{m};{chunk}\x1b\\")
        } else {
            let m = if last { 0 } else { 1 };
            format!("\x1b_Gm={m};{chunk}\x1b\\")
        };
        out.extend_from_slice(packet.as_bytes());
    }
    out
}

/// Cell span for an `img` (w, h in pixels) in a `pane_cols` x `pane_rows`
/// viewport: fill the width (minus a margin) but never stretch a small image
/// past its natural cell span, keep aspect via the 1:2 cell ratio, and cap
/// height below the viewport so the placement isn't immediately scrolled off
/// (scroll-off deletes it — graphics.rs placement semantics).
pub fn fit(img: (u32, u32), pane_cols: u16, pane_rows: u16) -> (u16, u16) {
    let (w, h) = (img.0.max(1), img.1.max(1));
    let natural_cols = w.div_ceil(CELL_W);
    let max_cols = u32::from(pane_cols.saturating_sub(2)).max(1);
    let max_rows = u32::from(pane_rows.saturating_sub(3)).max(1);
    let mut c = natural_cols.min(max_cols);
    // r from aspect: displayed px width = c*CELL_W, scale = that/w,
    // r = ceil(h * scale / CELL_H) = ceil(h*c*CELL_W / (w*CELL_H)).
    let mut r = (h * c * CELL_W).div_ceil(w * CELL_H);
    if r > max_rows {
        r = max_rows;
        c = (r * CELL_H * w / (h * CELL_W)).max(1);
    }
    (
        c.min(u32::from(u16::MAX)) as u16,
        r.min(u32::from(u16::MAX)) as u16,
    )
}

/// Downscale headroom for hi-DPI panes: the renderer may paint a cell at up
/// to ~2x the assumed 8px width, so only shrink sources larger than 2x the
/// displayed estimate — linear filtering handles minification below 2:1
/// without shimmer; past it, pre-shrinking is what keeps screenshots crisp.
const DPI_HEADROOM: u32 = 2;

/// Area-average (box) resize of tight RGBA pixels. Unweighted over the
/// covering source rect — exact for integer ratios, visually fine for the
/// large minifications this is used for.
pub fn downscale_rgba(rgba: &[u8], w: u32, h: u32, dw: u32, dh: u32) -> Vec<u8> {
    let (w, h, dw, dh) = (w as usize, h as usize, dw as usize, dh as usize);
    let mut out = Vec::with_capacity(dw * dh * 4);
    for y in 0..dh {
        let y0 = y * h / dh;
        let y1 = ((y + 1) * h / dh).max(y0 + 1);
        for x in 0..dw {
            let x0 = x * w / dw;
            let x1 = ((x + 1) * w / dw).max(x0 + 1);
            let mut acc = [0u64; 4];
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let p = (sy * w + sx) * 4;
                    for (a, &v) in acc.iter_mut().zip(&rgba[p..p + 4]) {
                        *a += u64::from(v);
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u64;
            out.extend(acc.iter().map(|&a| (a / n) as u8));
        }
    }
    out
}

/// Shrink a PNG to at most `max_w` pixels wide (aspect kept), re-encoded as
/// RGBA8. `None` = already small enough, transmit the original bytes.
pub fn shrink_to_fit(png: &[u8], max_w: u32) -> Result<Option<Vec<u8>>, String> {
    let (w, h, rgba) = crate::graphics::decode_png(png).map_err(str::to_owned)?;
    if w <= max_w.max(1) {
        return Ok(None);
    }
    let dw = max_w.max(1);
    let dh = (u64::from(h) * u64::from(dw) / u64::from(w)).max(1) as u32;
    let small = downscale_rgba(&rgba, w, h, dw, dh);
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, dw, dh);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&small).map_err(|e| e.to_string())?;
    }
    Ok(Some(out))
}

/// PNG signature + IHDR dimensions, without decoding pixel data.
pub fn png_dims(bytes: &[u8]) -> Result<(u32, u32), String> {
    const MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
    if bytes.len() < 8 || bytes[..8] != MAGIC {
        return Err("not a PNG (v1 supports PNG only)".into());
    }
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let reader = decoder.read_info().map_err(|e| e.to_string())?;
    let info = reader.info();
    Ok((info.width, info.height))
}

const HELP_ICAT: &str = "\
foreman icat — print an image into this terminal pane

USAGE
  foreman icat <file.png> [--cols N]

Reads the PNG and emits kitty graphics escapes on stdout, sized to the
console width (override with --cols). Works inside foreman panes (and any
kitty-graphics terminal); PNG only in v1. The image scrolls with the
buffer like ordinary output.";

pub fn icat_main(args: &[String]) -> i32 {
    if let Some("--help" | "-h") = args.first().map(String::as_str) {
        println!("{HELP_ICAT}");
        return 0;
    }
    let mut file: Option<&str> = None;
    let mut cols_override: Option<u16> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--cols" => match it.next().and_then(|v| v.parse::<u16>().ok()) {
                Some(n) if n > 0 => cols_override = Some(n),
                _ => {
                    eprintln!("foreman icat: --cols needs a positive number");
                    return 2;
                }
            },
            f if file.is_none() => file = Some(f),
            extra => {
                eprintln!("foreman icat: unexpected argument {extra:?}");
                return 2;
            }
        }
    }
    let Some(path) = file else {
        eprintln!("usage: foreman icat <file.png> [--cols N]");
        return 2;
    };
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("foreman icat: {path}: {e}");
            return 2;
        }
    };
    let dims = match png_dims(&bytes) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("foreman icat: {path}: {e}");
            return 2;
        }
    };
    let (pane_cols, pane_rows) = console_size().unwrap_or((80, 24));
    let (c, r) = match cols_override {
        // Explicit --cols keeps aspect but skips the width heuristics.
        Some(n) => fit(dims, n.saturating_add(2), pane_rows),
        None => fit(dims, pane_cols, pane_rows),
    };
    // Pre-shrink sources much larger than the displayed estimate so big
    // screenshots stay crisp (and cheap to transmit) under linear filtering.
    let bytes = match shrink_to_fit(&bytes, u32::from(c) * CELL_W * DPI_HEADROOM) {
        Ok(Some(smaller)) => smaller,
        Ok(None) => bytes,
        Err(e) => {
            eprintln!("foreman icat: {path}: {e}");
            return 2;
        }
    };
    use std::io::Write as _;
    let mut out = std::io::stdout().lock();
    let mut buf = encode(&bytes, std::process::id(), c, r);
    // The renderer doesn't advance the cursor past a placement (v1 limit),
    // so scroll the prompt below the image ourselves.
    buf.extend(std::iter::repeat_n(b'\n', usize::from(r)));
    if out.write_all(&buf).and_then(|()| out.flush()).is_err() {
        return 1;
    }
    0
}

/// Visible window size of the attached console, if any.
fn console_size() -> Option<(u16, u16)> {
    use windows_sys::Win32::System::Console::{
        CONSOLE_SCREEN_BUFFER_INFO, GetConsoleScreenBufferInfo, GetStdHandle, STD_OUTPUT_HANDLE,
    };
    unsafe {
        let h = GetStdHandle(STD_OUTPUT_HANDLE);
        if h.is_null() {
            return None;
        }
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
        if GetConsoleScreenBufferInfo(h, &mut info) == 0 {
            return None;
        }
        let w = info.srWindow.Right - info.srWindow.Left + 1;
        let hgt = info.srWindow.Bottom - info.srWindow.Top + 1;
        if w <= 0 || hgt <= 0 {
            return None;
        }
        Some((w as u16, hgt as u16))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics::{Graphics, TermView, ViewportView};

    fn png_of(w: u32, h: u32, pixel: impl Fn(u32) -> [u8; 4]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            let data: Vec<u8> = (0..w * h).flat_map(pixel).collect();
            writer.write_image_data(&data).unwrap();
        }
        out
    }

    fn view() -> TermView {
        TermView {
            cursor_col: 0,
            cursor_line: 0,
            alt_screen: false,
            history_size: 0,
        }
    }

    fn viewport() -> ViewportView {
        ViewportView {
            alt_screen: false,
            history_size: 0,
            display_offset: 0,
            screen_lines: 40,
        }
    }

    #[test]
    fn single_chunk_encode_round_trips_through_the_renderer() {
        let png = png_of(2, 2, |_| [255, 0, 0, 255]);
        let bytes = encode(&png, 7, 4, 2);
        let mut g = Graphics::default();
        let cuts = g.feed(&bytes);
        assert_eq!(cuts.len(), 1, "one completed command");
        let mut resp = Vec::new();
        g.apply(view(), &mut resp);
        assert!(resp.is_empty(), "q=2 must be silent");
        assert!(g.has_image(7));
        let vis = g.visible(&viewport());
        assert_eq!(vis.len(), 1);
        assert_eq!((vis[0].cols, vis[0].rows), (4, 2));
        assert_eq!(vis[0].rgba[0..4], [255, 0, 0, 255]);
    }

    #[test]
    fn large_png_is_chunked_and_still_round_trips() {
        // Incompressible-ish pattern so the PNG stream exceeds two chunks of
        // base64 (> 2*4096 chars ≈ > 6144 raw bytes).
        let png = png_of(64, 64, |i| {
            let b = (i * 7 % 251) as u8;
            [b, b.wrapping_add(85), b.wrapping_add(170), 255]
        });
        assert!(
            base64::engine::general_purpose::STANDARD.encode(&png).len() > 2 * CHUNK,
            "test PNG must span at least three chunks"
        );
        let bytes = encode(&png, 9, 60, 20);
        let starts = bytes.windows(3).filter(|w| w == b"\x1b_G").count();
        assert!(starts >= 3, "expected >= 3 APC packets, got {starts}");
        let mut g = Graphics::default();
        let cuts = g.feed(&bytes);
        assert_eq!(cuts.len(), 1, "the chain completes exactly once (m=0)");
        let mut resp = Vec::new();
        g.apply(view(), &mut resp);
        assert!(g.has_image(9));
        let vis = g.visible(&viewport());
        assert_eq!(vis.len(), 1);
        assert_eq!((vis[0].w, vis[0].h), (64, 64));
    }

    #[test]
    fn fit_fills_width_capped_by_pane_and_keeps_aspect() {
        // 1280x800 screenshot in a 120x40 pane: width-limited.
        assert_eq!(fit((1280, 800), 120, 40), (118, 37));
        // Tiny 16x16 icon must not stretch: natural span is 2 cols.
        assert_eq!(fit((16, 16), 120, 40), (2, 1));
        // Very tall image is height-capped with width re-derived from aspect.
        assert_eq!(fit((100, 2000), 80, 24), (2, 21));
    }

    #[test]
    fn downscale_averages_covered_source_pixels() {
        // 2x1 black+white -> 1x1 mid-gray (127 by integer division).
        let bw = [0, 0, 0, 255, 255, 255, 255, 255];
        assert_eq!(downscale_rgba(&bw, 2, 1, 1, 1), vec![127, 127, 127, 255]);
        // 4x4 of four solid 2x2 quadrants -> 2x2 of exactly those colors.
        let q = |c: [u8; 4]| c;
        let (red, grn, blu, wht) = (
            q([255, 0, 0, 255]),
            q([0, 255, 0, 255]),
            q([0, 0, 255, 255]),
            q([255, 255, 255, 255]),
        );
        let mut src = Vec::new();
        for row in [
            [red, red, grn, grn],
            [red, red, grn, grn],
            [blu, blu, wht, wht],
            [blu, blu, wht, wht],
        ] {
            for px in row {
                src.extend_from_slice(&px);
            }
        }
        let out = downscale_rgba(&src, 4, 4, 2, 2);
        assert_eq!(out[0..4], red);
        assert_eq!(out[4..8], grn);
        assert_eq!(out[8..12], blu);
        assert_eq!(out[12..16], wht);
    }

    #[test]
    fn shrink_to_fit_halves_a_large_png_and_leaves_small_ones_alone() {
        let big = png_of(100, 60, |_| [10, 200, 30, 255]);
        let shrunk = shrink_to_fit(&big, 50).unwrap().expect("must shrink");
        assert_eq!(png_dims(&shrunk), Ok((50, 30)));
        // Solid color must survive the round-trip exactly.
        let (_, _, rgba) = crate::graphics::decode_png(&shrunk).unwrap();
        assert!(rgba.chunks_exact(4).all(|p| p == [10, 200, 30, 255]));
        // Already small enough: untouched.
        assert!(shrink_to_fit(&big, 100).unwrap().is_none());
        assert!(shrink_to_fit(&big, 500).unwrap().is_none());
    }

    #[test]
    fn png_dims_reads_header_and_rejects_non_png() {
        let png = png_of(3, 5, |_| [0, 0, 0, 255]);
        assert_eq!(png_dims(&png), Ok((3, 5)));
        assert!(png_dims(b"JFIF not a png").is_err());
        assert!(png_dims(&[]).is_err());
    }
}
