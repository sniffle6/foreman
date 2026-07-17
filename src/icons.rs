//! Tab icons: official app/agent logos rasterized from embedded SVGs into cached
//! egui textures. The embedded SVGs are monochrome white silhouettes, so callers
//! tint them to a brand color at paint time. The texture for a given (kind,
//! pixel-size) is rasterized once via resvg and cached in egui's per-context data
//! — it costs nothing after the first frame and re-rasterizes crisply when the
//! DPI/zoom asks for a new pixel size.

use eframe::egui;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const CLAUDE_SVG: &str = include_str!("../assets/icons/claude.svg");
const CODEX_SVG: &str = include_str!("../assets/icons/codex.svg");
const GROK_SVG: &str = include_str!("../assets/icons/grok.svg");
const TERMINAL_SVG: &str = include_str!("../assets/icons/terminal.svg");
const FOLDER_SVG: &str = include_str!("../assets/icons/folder.svg");

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum IconKind {
    // Agents — official brand logos.
    Claude,
    Codex,
    Grok,
    // Plain shells — a shared terminal-prompt glyph, tinted per shell.
    PowerShell,
    Cmd,
    Bash,
    // A project tab.
    Folder,
}

impl IconKind {
    fn svg(self) -> &'static str {
        match self {
            IconKind::Claude => CLAUDE_SVG,
            IconKind::Codex => CODEX_SVG,
            IconKind::Grok => GROK_SVG,
            IconKind::PowerShell | IconKind::Cmd | IconKind::Bash => TERMINAL_SVG,
            IconKind::Folder => FOLDER_SVG,
        }
    }

    /// Brand tint multiplied onto the white silhouette at paint time.
    pub fn tint(self) -> egui::Color32 {
        match self {
            IconKind::Claude => egui::Color32::from_rgb(217, 119, 87), // Claude clay
            IconKind::Codex => egui::Color32::from_rgb(236, 236, 236), // near-white
            IconKind::Grok => egui::Color32::from_rgb(250, 250, 250),  // Grok white
            IconKind::PowerShell => egui::Color32::from_rgb(83, 145, 254), // PS blue
            IconKind::Cmd => egui::Color32::from_rgb(206, 206, 206),   // console gray
            IconKind::Bash => egui::Color32::from_rgb(106, 190, 48),   // bash green
            IconKind::Folder => egui::Color32::from_rgb(220, 180, 110), // folder amber
        }
    }

    /// Icon for a plain shell terminal.
    pub fn for_shell(shell: crate::terminal::Shell) -> Self {
        match shell {
            crate::terminal::Shell::PowerShell => IconKind::PowerShell,
            crate::terminal::Shell::Cmd => IconKind::Cmd,
            crate::terminal::Shell::Bash => IconKind::Bash,
        }
    }

    /// Human label for an agent icon (tab auto-title, etc.). `None` for shells
    /// and non-agent chrome icons.
    pub fn agent_label(self) -> Option<&'static str> {
        match self {
            IconKind::Claude => Some("Claude"),
            IconKind::Codex => Some("Codex"),
            IconKind::Grok => Some("Grok"),
            IconKind::PowerShell | IconKind::Cmd | IconKind::Bash | IconKind::Folder => None,
        }
    }

    /// Map a dispatched program's argv to an agent icon, if recognized. Scans
    /// every token (so `npx @anthropic-ai/claude-code` and a bare `claude` both
    /// hit) for a `claude`/`codex`/`grok` substring.
    pub fn from_argv(argv: &[String]) -> Option<Self> {
        let hay = argv.join(" ").to_ascii_lowercase();
        if hay.contains("claude") {
            Some(IconKind::Claude)
        } else if hay.contains("codex") {
            Some(IconKind::Codex)
        } else if hay.contains("grok") {
            Some(IconKind::Grok)
        } else {
            None
        }
    }

    /// Map a program's OSC window title to an agent icon. Used for a hand-launched
    /// agent (e.g. `claude` typed at a shell prompt): the program sets its own
    /// title — observed values are the program's path/name (`…\claude.EXE`,
    /// `claude`; a shell sets its own exe). We match on the title's *file stem* so
    /// a folder named "claude" elsewhere in a path can't false-positive (the user
    /// here works in `H:\claude code\…`).
    pub fn from_title(title: &str) -> Option<Self> {
        let stem = std::path::Path::new(title.trim())
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_else(|| title.trim())
            .to_ascii_lowercase();
        if stem.contains("claude") {
            Some(IconKind::Claude)
        } else if stem.contains("codex") {
            Some(IconKind::Codex)
        } else if stem.contains("grok") {
            Some(IconKind::Grok)
        } else {
            None
        }
    }
}

type Cache = Arc<Mutex<HashMap<(IconKind, u32), egui::TextureHandle>>>;

fn cache_id() -> egui::Id {
    egui::Id::new("foreman::icon_cache")
}

/// Texture for `kind` rendered at `px`×`px` device pixels, cached per context.
pub fn texture(ctx: &egui::Context, kind: IconKind, px: u32) -> egui::TextureHandle {
    let key = (kind, px);
    let cache: Cache = ctx.data_mut(|d| d.get_temp_mut_or_default::<Cache>(cache_id()).clone());
    if let Some(h) = cache.lock().unwrap().get(&key) {
        return h.clone();
    }
    let img = rasterize(kind.svg(), px);
    let handle = ctx.load_texture(
        format!("foreman-icon-{kind:?}-{px}"),
        img,
        egui::TextureOptions::LINEAR,
    );
    cache.lock().unwrap().insert(key, handle.clone());
    handle
}

/// Render an SVG (square viewBox) to a `px`×`px` unmultiplied-RGBA image.
fn rasterize(svg: &str, px: u32) -> egui::ColorImage {
    let mut pixmap = resvg::tiny_skia::Pixmap::new(px, px).expect("nonzero icon size");
    let opt = resvg::usvg::Options::default();
    match resvg::usvg::Tree::from_str(svg, &opt) {
        Ok(tree) => {
            let size = tree.size();
            let scale = px as f32 / size.width().max(size.height());
            let ts = resvg::tiny_skia::Transform::from_scale(scale, scale);
            resvg::render(&tree, ts, &mut pixmap.as_mut());
        }
        Err(e) => {
            // Embedded SVGs are known-good; degrade to a blank icon rather than
            // panic across the egui/winit callback if one ever fails to parse.
            eprintln!("foreman: icon SVG failed to parse: {e}");
        }
    }
    let mut rgba = Vec::with_capacity((px * px * 4) as usize);
    for p in pixmap.pixels() {
        let c = p.demultiply();
        rgba.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    egui::ColorImage::from_rgba_unmultiplied([px as usize, px as usize], &rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opaque_pixels(img: &egui::ColorImage) -> usize {
        img.pixels.iter().filter(|p| p.a() > 0).count()
    }

    #[test]
    fn embedded_svgs_rasterize_to_nonblank_icons() {
        // One per SVG file: Claude, Codex, Grok, the shared terminal glyph
        // (PowerShell), and the folder.
        for kind in [
            IconKind::Claude,
            IconKind::Codex,
            IconKind::Grok,
            IconKind::PowerShell,
            IconKind::Folder,
        ] {
            let img = rasterize(kind.svg(), 32);
            assert_eq!(img.size, [32, 32]);
            // A parse failure or all-transparent fill would leave zero ink; the
            // real logos cover a healthy chunk of the 1024-pixel canvas.
            assert!(
                opaque_pixels(&img) > 100,
                "{kind:?} rendered nearly blank ({} opaque px)",
                opaque_pixels(&img)
            );
        }
    }

    #[test]
    fn argv_detection_matches_known_agents() {
        assert_eq!(
            IconKind::from_argv(&["claude".into()]),
            Some(IconKind::Claude)
        );
        assert_eq!(
            IconKind::from_argv(&["npx".into(), "@anthropic-ai/claude-code".into()]),
            Some(IconKind::Claude)
        );
        assert_eq!(
            IconKind::from_argv(&["codex".into()]),
            Some(IconKind::Codex)
        );
        assert_eq!(IconKind::from_argv(&["grok".into()]), Some(IconKind::Grok));
        assert_eq!(IconKind::from_argv(&["powershell.exe".into()]), None);
    }

    #[test]
    fn title_detection_uses_program_stem_not_path() {
        // Observed real OSC titles.
        assert_eq!(
            IconKind::from_title(r"C:\Users\me\.local\bin\claude.EXE"),
            Some(IconKind::Claude)
        );
        assert_eq!(IconKind::from_title("claude"), Some(IconKind::Claude));
        assert_eq!(
            IconKind::from_title(r"C:\WINDOWS\System32\WindowsPowerShell\v1.0\powershell.exe"),
            None
        );
        // A shell sitting in a folder literally named "claude code" must NOT match
        // — only the title's file stem is considered, not the whole path.
        assert_eq!(IconKind::from_title(r"H:\claude code\foreman"), None);
        assert_eq!(IconKind::from_title("codex"), Some(IconKind::Codex));
        assert_eq!(
            IconKind::from_title(r"C:\Users\me\.grok\bin\grok.exe"),
            Some(IconKind::Grok)
        );
        assert_eq!(IconKind::from_title("grok"), Some(IconKind::Grok));
    }

    #[test]
    fn agent_label_covers_agents_only() {
        assert_eq!(IconKind::Claude.agent_label(), Some("Claude"));
        assert_eq!(IconKind::Codex.agent_label(), Some("Codex"));
        assert_eq!(IconKind::Grok.agent_label(), Some("Grok"));
        assert_eq!(IconKind::PowerShell.agent_label(), None);
        assert_eq!(IconKind::Folder.agent_label(), None);
    }
}
