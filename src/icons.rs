//! Icons rasterized from embedded SVGs into cached egui textures: the tab
//! icons (official app/agent logos) and the Material Symbols the Changes
//! toolbar draws. The embedded SVGs are monochrome white silhouettes, so
//! callers tint them at paint time. The texture for a given (name, pixel-size)
//! is rasterized once via resvg and cached in egui's per-context data — it
//! costs nothing after the first frame and re-rasterizes crisply when the
//! DPI/zoom asks for a new pixel size.

use eframe::egui;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const CLAUDE_SVG: &str = include_str!("../assets/icons/claude.svg");
const CODEX_SVG: &str = include_str!("../assets/icons/codex.svg");
const GROK_SVG: &str = include_str!("../assets/icons/grok.svg");
const TERMINAL_SVG: &str = include_str!("../assets/icons/terminal.svg");
const FOLDER_SVG: &str = include_str!("../assets/icons/folder.svg");

/// The Remote Session badge (see [`paint_globe_badge`]); `(name, svg)` like
/// the Material Symbols.
pub const GLOBE: (&str, &str) = ("globe", include_str!("../assets/icons/globe.svg"));

/// Tint multiplied onto the globe's white silhouette: a sky blue that reads
/// apart from every agent and shell tint it sits over.
pub const GLOBE_TINT: egui::Color32 = egui::Color32::from_rgb(110, 180, 255);

/// Google Material Symbols Outlined (Apache 2.0; `assets/icons/material/README.md`).
/// Each is `(name, svg)`: the name keys the texture cache.
pub mod material {
    pub const REFRESH: (&str, &str) = (
        "material-refresh",
        include_str!("../assets/icons/material/refresh.svg"),
    );
    pub const UPLOAD: (&str, &str) = (
        "material-upload",
        include_str!("../assets/icons/material/upload.svg"),
    );
    pub const ADD: (&str, &str) = (
        "material-add",
        include_str!("../assets/icons/material/add.svg"),
    );
    pub const VISIBILITY: (&str, &str) = (
        "material-visibility",
        include_str!("../assets/icons/material/visibility.svg"),
    );
    pub const UNFOLD_MORE: (&str, &str) = (
        "material-unfold_more",
        include_str!("../assets/icons/material/unfold_more.svg"),
    );
    pub const UNFOLD_LESS: (&str, &str) = (
        "material-unfold_less",
        include_str!("../assets/icons/material/unfold_less.svg"),
    );
    pub const CHEVRON_RIGHT: (&str, &str) = (
        "material-chevron_right",
        include_str!("../assets/icons/material/chevron_right.svg"),
    );
    pub const MORE_HORIZ: (&str, &str) = (
        "material-more_horiz",
        include_str!("../assets/icons/material/more_horiz.svg"),
    );
    /// All eight, for the rasterization test.
    #[cfg(test)]
    pub const ALL: [(&str, &str); 8] = [
        REFRESH,
        UPLOAD,
        ADD,
        VISIBILITY,
        UNFOLD_MORE,
        UNFOLD_LESS,
        CHEVRON_RIGHT,
        MORE_HORIZ,
    ];
}

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
    /// The SVG and its cache name; the three shells share one silhouette.
    fn svg(self) -> (&'static str, &'static str) {
        match self {
            IconKind::Claude => ("claude", CLAUDE_SVG),
            IconKind::Codex => ("codex", CODEX_SVG),
            IconKind::Grok => ("grok", GROK_SVG),
            IconKind::PowerShell | IconKind::Cmd | IconKind::Bash => ("terminal", TERMINAL_SVG),
            IconKind::Folder => ("folder", FOLDER_SVG),
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

    /// The provider name an agent icon stands for, as `board::AGENTS` spells
    /// it. `None` for shells and chrome icons.
    pub fn provider(self) -> Option<&'static str> {
        match self {
            IconKind::Claude => Some("claude"),
            IconKind::Codex => Some("codex"),
            IconKind::Grok => Some("grok"),
            IconKind::PowerShell | IconKind::Cmd | IconKind::Bash | IconKind::Folder => None,
        }
    }

    /// Map a dispatched program's argv to an agent icon, if recognized. Scans
    /// every token (so `npx @anthropic-ai/claude-code` and a bare `claude` both
    /// hit) for a `claude`/`codex`/`grok` substring. Reconnect and the
    /// workspace snapshot run and save whatever this returns, so the provider
    /// is the one that launches, not the one a prompt mentions: a program
    /// whose file stem IS a provider wins (`C:\claude code\bin\codex.exe` is
    /// Codex), then the earliest mention in argv order (`wsl codex "compare
    /// with claude"` is Codex).
    pub fn from_argv(argv: &[String]) -> Option<Self> {
        const AGENTS: [(&str, IconKind); 3] = [
            ("claude", IconKind::Claude),
            ("codex", IconKind::Codex),
            ("grok", IconKind::Grok),
        ];
        let stem = argv.first().and_then(|p| {
            std::path::Path::new(p)
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_ascii_lowercase)
        });
        if let Some(&(_, k)) = AGENTS.iter().find(|(n, _)| stem.as_deref() == Some(*n)) {
            return Some(k);
        }
        let hay = argv.join(" ").to_ascii_lowercase();
        AGENTS
            .iter()
            .filter_map(|&(n, k)| hay.find(n).map(|at| (at, k)))
            .min_by_key(|&(at, _)| at)
            .map(|(_, k)| k)
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

type Cache = Arc<Mutex<HashMap<(&'static str, u32), egui::TextureHandle>>>;

fn cache_id() -> egui::Id {
    egui::Id::new("foreman::icon_cache")
}

/// Texture for `kind` rendered at `px`×`px` device pixels, cached per context.
pub fn texture(ctx: &egui::Context, kind: IconKind, px: u32) -> egui::TextureHandle {
    let (name, svg) = kind.svg();
    texture_svg(ctx, name, svg, px)
}

/// Texture for an embedded white-silhouette `svg` rendered at `px`×`px`
/// device pixels, cached per context under `name` (unique per SVG).
pub fn texture_svg(
    ctx: &egui::Context,
    name: &'static str,
    svg: &'static str,
    px: u32,
) -> egui::TextureHandle {
    let key = (name, px);
    let cache: Cache = ctx.data_mut(|d| d.get_temp_mut_or_default::<Cache>(cache_id()).clone());
    if let Some(h) = cache.lock().unwrap().get(&key) {
        return h.clone();
    }
    let img = rasterize(svg, px);
    let handle = ctx.load_texture(
        format!("foreman-icon-{name}-{px}"),
        img,
        egui::TextureOptions::LINEAR,
    );
    cache.lock().unwrap().insert(key, handle.clone());
    handle
}

/// Paint the Remote badge over the bottom-right corner of a tab/row `icon`:
/// a globe on a disc of `backing` (the colour behind the icon), so it reads
/// over the logo and the logo stays visible around it. Returns the badge's
/// rect, for a hover test.
pub fn paint_globe_badge(
    p: &egui::Painter,
    ctx: &egui::Context,
    icon: egui::Rect,
    tint: egui::Color32,
    backing: egui::Color32,
) -> egui::Rect {
    let d = (icon.width() * 0.62).max(7.0);
    // Mostly inside the icon, a little past its corner: enough logo stays
    // uncovered to tell which agent it is.
    let c = icon.right_bottom() - egui::vec2(d * 0.38, d * 0.38);
    let rect = egui::Rect::from_center_size(c, egui::vec2(d, d));
    p.circle_filled(c, d / 2.0 + 1.0, backing);
    let px = (d * ctx.pixels_per_point()).round().max(1.0) as u32;
    let tex = texture_svg(ctx, GLOBE.0, GLOBE.1, px);
    p.image(
        tex.id(),
        rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        tint,
    );
    rect
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
        // (PowerShell), the folder, the Remote globe, and the eight Material
        // Symbols.
        let tabs = [
            IconKind::Claude,
            IconKind::Codex,
            IconKind::Grok,
            IconKind::PowerShell,
            IconKind::Folder,
        ]
        .map(IconKind::svg);
        for (name, svg) in tabs
            .iter()
            .chain(std::iter::once(&GLOBE))
            .chain(material::ALL.iter())
        {
            let img = rasterize(svg, 32);
            assert_eq!(img.size, [32, 32]);
            // A parse failure or all-transparent fill would leave zero ink; even
            // the thinnest real glyph (the chevron, ~76 px) inks well over 40 of
            // the 1024-pixel canvas.
            assert!(
                opaque_pixels(&img) > 40,
                "{name} rendered nearly blank ({} opaque px)",
                opaque_pixels(&img)
            );
            // The silhouette must be white so the tint multiplies cleanly; a
            // Material path without `fill` would rasterize black.
            assert!(
                img.pixels
                    .iter()
                    .filter(|p| p.a() == 255)
                    .all(|p| p.r() == 255 && p.g() == 255 && p.b() == 255),
                "{name} is not a white silhouette"
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
        // The launched agent, not the one a prompt or a folder names.
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            IconKind::from_argv(&argv(&["codex", "exec", "compare with claude"])),
            Some(IconKind::Codex)
        );
        assert_eq!(
            IconKind::from_argv(&argv(&[r"C:\claude code\bin\codex.exe"])),
            Some(IconKind::Codex)
        );
        assert_eq!(
            IconKind::from_argv(&argv(&["wsl", "grok", "ask claude"])),
            Some(IconKind::Grok)
        );
    }

    #[test]
    fn agent_providers_are_the_board_s_agents() {
        let providers: Vec<_> = [IconKind::Claude, IconKind::Codex, IconKind::Grok]
            .into_iter()
            .map(|k| k.provider().unwrap())
            .collect();
        assert_eq!(providers, crate::board::AGENTS);
        for k in [IconKind::PowerShell, IconKind::Cmd, IconKind::Bash] {
            assert_eq!(k.provider(), None);
        }
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
