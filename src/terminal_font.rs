//! Terminal font registration: four matched Hack faces + system fallbacks.
//!
//! egui 0.34 has no `FontId` weight and no real synthetic bold; italic via
//! `TextFormat::italics` is a mesh shear. Terminal bold/italic therefore need
//! real faces registered as named families. Default Monospace / Proportional
//! stay unchanged for non-terminal UI chrome.

use eframe::egui::{self, FontData, FontDefinitions, FontFamily, FontId};

/// Named family for regular terminal glyphs.
pub const FAMILY_REGULAR: &str = "foreman_term_regular";
/// Named family for bold terminal glyphs.
pub const FAMILY_BOLD: &str = "foreman_term_bold";
/// Named family for italic terminal glyphs.
pub const FAMILY_ITALIC: &str = "foreman_term_italic";
/// Named family for bold-italic terminal glyphs.
pub const FAMILY_BOLD_ITALIC: &str = "foreman_term_bold_italic";

const HACK_REGULAR_NAME: &str = "Hack-Regular";
const HACK_BOLD_NAME: &str = "Hack-Bold";
const HACK_ITALIC_NAME: &str = "Hack-Italic";
const HACK_BOLD_ITALIC_NAME: &str = "Hack-BoldItalic";

const HACK_REGULAR: &[u8] = include_bytes!("../assets/fonts/Hack-Regular.ttf");
const HACK_BOLD: &[u8] = include_bytes!("../assets/fonts/Hack-Bold.ttf");
const HACK_ITALIC: &[u8] = include_bytes!("../assets/fonts/Hack-Italic.ttf");
const HACK_BOLD_ITALIC: &[u8] = include_bytes!("../assets/fonts/Hack-BoldItalic.ttf");

/// Known Windows system fonts used as glyph fallbacks (CJK + emoji shapes).
/// Order: CJK first, emoji second (both lowest priority after defaults).
pub fn windows_fallback_font_paths() -> &'static [(&'static str, &'static str)] {
    &[
        ("yahei", r"C:\Windows\Fonts\msyh.ttc"),
        ("seguiemj", r"C:\Windows\Fonts\seguiemj.ttf"),
    ]
}

/// Append named font blobs as lowest-priority fallbacks for Monospace and
/// Proportional. Empty blobs are skipped. Existing primary fonts stay first.
/// Pure: no filesystem, no Context — unit-tested with fake bytes.
pub fn append_font_fallbacks(
    fonts: &mut FontDefinitions,
    named_fonts: impl IntoIterator<Item = (String, Vec<u8>)>,
) {
    for (name, bytes) in named_fonts {
        if bytes.is_empty() {
            continue;
        }
        fonts.font_data.insert(
            name.clone(),
            std::sync::Arc::new(FontData::from_owned(bytes)),
        );
        for family in [FontFamily::Monospace, FontFamily::Proportional] {
            if let Some(list) = fonts.families.get_mut(&family) {
                if !list.iter().any(|n| n == &name) {
                    list.push(name.clone());
                }
            }
        }
    }
}

/// Cached named-family handles — `FontFamily::Name` stores an `Arc<str>`;
/// rebuild it once, not per metrics probe / layout call.
fn family_regular() -> FontFamily {
    use std::sync::OnceLock;
    static F: OnceLock<FontFamily> = OnceLock::new();
    F.get_or_init(|| FontFamily::Name(FAMILY_REGULAR.into()))
        .clone()
}
fn family_bold() -> FontFamily {
    use std::sync::OnceLock;
    static F: OnceLock<FontFamily> = OnceLock::new();
    F.get_or_init(|| FontFamily::Name(FAMILY_BOLD.into()))
        .clone()
}
fn family_italic() -> FontFamily {
    use std::sync::OnceLock;
    static F: OnceLock<FontFamily> = OnceLock::new();
    F.get_or_init(|| FontFamily::Name(FAMILY_ITALIC.into()))
        .clone()
}
fn family_bold_italic() -> FontFamily {
    use std::sync::OnceLock;
    static F: OnceLock<FontFamily> = OnceLock::new();
    F.get_or_init(|| FontFamily::Name(FAMILY_BOLD_ITALIC.into()))
        .clone()
}

/// `FontId` for a terminal cell given size and SGR bold/italic.
pub fn font_id(size: f32, bold: bool, italic: bool) -> FontId {
    let family = match (bold, italic) {
        (false, false) => family_regular(),
        (true, false) => family_bold(),
        (false, true) => family_italic(),
        (true, true) => family_bold_italic(),
    };
    FontId::new(size, family)
}

/// Build default `FontDefinitions` plus system fallbacks and the four terminal
/// style families. Inject `read` so tests never touch the real disk.
pub fn load_font_definitions(
    read: &dyn Fn(&std::path::Path) -> std::io::Result<Vec<u8>>,
) -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let mut loaded = Vec::new();
    for &(name, path) in windows_fallback_font_paths() {
        match read(std::path::Path::new(path)) {
            Ok(bytes) if !bytes.is_empty() => loaded.push((name.to_string(), bytes)),
            Ok(_) => {}
            Err(_) => {}
        }
    }
    append_font_fallbacks(&mut fonts, loaded);
    install_terminal_families(&mut fonts);
    fonts
}

/// Register the four matched Hack faces as named families. Each family starts
/// with its face, then the **entire** Monospace fallback list (symbols, emoji
/// defaults, and any system fallbacks already appended). Named families do not
/// inherit Monospace implicitly.
fn install_terminal_families(fonts: &mut FontDefinitions) {
    fonts.font_data.insert(
        HACK_REGULAR_NAME.to_string(),
        std::sync::Arc::new(FontData::from_static(HACK_REGULAR)),
    );
    fonts.font_data.insert(
        HACK_BOLD_NAME.to_string(),
        std::sync::Arc::new(FontData::from_static(HACK_BOLD)),
    );
    fonts.font_data.insert(
        HACK_ITALIC_NAME.to_string(),
        std::sync::Arc::new(FontData::from_static(HACK_ITALIC)),
    );
    fonts.font_data.insert(
        HACK_BOLD_ITALIC_NAME.to_string(),
        std::sync::Arc::new(FontData::from_static(HACK_BOLD_ITALIC)),
    );

    let mono_tail: Vec<String> = fonts
        .families
        .get(&FontFamily::Monospace)
        .cloned()
        .unwrap_or_default();

    let mk = |primary: &str| {
        let mut list = Vec::with_capacity(1 + mono_tail.len());
        list.push(primary.to_string());
        list.extend(mono_tail.iter().cloned());
        list
    };

    fonts.families.insert(
        FontFamily::Name(FAMILY_REGULAR.into()),
        mk(HACK_REGULAR_NAME),
    );
    fonts
        .families
        .insert(FontFamily::Name(FAMILY_BOLD.into()), mk(HACK_BOLD_NAME));
    fonts
        .families
        .insert(FontFamily::Name(FAMILY_ITALIC.into()), mk(HACK_ITALIC_NAME));
    fonts.families.insert(
        FontFamily::Name(FAMILY_BOLD_ITALIC.into()),
        mk(HACK_BOLD_ITALIC_NAME),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mono_names(fonts: &FontDefinitions) -> Vec<String> {
        fonts
            .families
            .get(&FontFamily::Monospace)
            .cloned()
            .unwrap_or_default()
    }

    fn prop_names(fonts: &FontDefinitions) -> Vec<String> {
        fonts
            .families
            .get(&FontFamily::Proportional)
            .cloned()
            .unwrap_or_default()
    }

    fn family_names(fonts: &FontDefinitions, name: &str) -> Vec<String> {
        fonts
            .families
            .get(&FontFamily::Name(name.into()))
            .cloned()
            .unwrap_or_default()
    }

    #[test]
    fn font_id_maps_all_four_style_combinations() {
        let r = font_id(13.0, false, false);
        let b = font_id(13.0, true, false);
        let i = font_id(13.0, false, true);
        let bi = font_id(13.0, true, true);
        assert_eq!(r.family, FontFamily::Name(FAMILY_REGULAR.into()));
        assert_eq!(b.family, FontFamily::Name(FAMILY_BOLD.into()));
        assert_eq!(i.family, FontFamily::Name(FAMILY_ITALIC.into()));
        assert_eq!(bi.family, FontFamily::Name(FAMILY_BOLD_ITALIC.into()));
        assert_eq!(r.size, 13.0);
    }

    #[test]
    fn append_fallbacks_pushes_name_to_mono_and_proportional() {
        let mut fonts = FontDefinitions::default();
        let before_mono = mono_names(&fonts);
        let before_prop = prop_names(&fonts);

        append_font_fallbacks(&mut fonts, [("yahei".into(), vec![0u8, 1, 2, 3])]);

        let after_mono = mono_names(&fonts);
        let after_prop = prop_names(&fonts);
        assert_eq!(&after_mono[..before_mono.len()], &before_mono[..]);
        assert_eq!(after_mono.last().map(String::as_str), Some("yahei"));
        assert_eq!(&after_prop[..before_prop.len()], &before_prop[..]);
        assert_eq!(after_prop.last().map(String::as_str), Some("yahei"));
        assert!(fonts.font_data.contains_key("yahei"));
    }

    #[test]
    fn append_fallbacks_preserves_primary_first() {
        let mut fonts = FontDefinitions::default();
        let primary = mono_names(&fonts)
            .first()
            .cloned()
            .expect("default mono family non-empty");

        append_font_fallbacks(&mut fonts, [("seguiemj".into(), vec![9u8, 9, 9])]);

        assert_eq!(
            mono_names(&fonts).first().map(String::as_str),
            Some(primary.as_str())
        );
        assert_eq!(
            mono_names(&fonts).last().map(String::as_str),
            Some("seguiemj")
        );
    }

    #[test]
    fn append_fallbacks_skips_empty_blob() {
        let mut fonts = FontDefinitions::default();
        let before = mono_names(&fonts);

        append_font_fallbacks(&mut fonts, [("empty".into(), Vec::new())]);

        assert_eq!(mono_names(&fonts), before);
        assert!(!fonts.font_data.contains_key("empty"));
    }

    #[test]
    fn append_fallbacks_two_fonts_order_stable() {
        let mut fonts = FontDefinitions::default();
        append_font_fallbacks(
            &mut fonts,
            [("yahei".into(), vec![1u8]), ("seguiemj".into(), vec![2u8])],
        );
        let mono = mono_names(&fonts);
        let n = mono.len();
        assert!(n >= 2);
        assert_eq!(mono[n - 2], "yahei");
        assert_eq!(mono[n - 1], "seguiemj");
    }

    #[test]
    fn append_fallbacks_does_not_duplicate_name_in_family() {
        let mut fonts = FontDefinitions::default();
        append_font_fallbacks(&mut fonts, [("yahei".into(), vec![1u8])]);
        append_font_fallbacks(&mut fonts, [("yahei".into(), vec![2u8])]);
        let count = mono_names(&fonts)
            .iter()
            .filter(|n| n.as_str() == "yahei")
            .count();
        assert_eq!(count, 1);
        assert!(fonts.font_data.contains_key("yahei"));
    }

    #[test]
    fn windows_fallback_paths_name_yahei_and_seguiemj() {
        let paths = windows_fallback_font_paths();
        let names: Vec<&str> = paths.iter().map(|(n, _)| *n).collect();
        assert!(names.contains(&"yahei"));
        assert!(names.contains(&"seguiemj"));
        for (_, p) in paths {
            assert!(p.starts_with(r"C:\Windows\Fonts\"), "{p}");
        }
    }

    #[test]
    fn load_font_definitions_skips_missing_files() {
        let fonts = load_font_definitions(&|_| {
            Err(std::io::Error::new(std::io::ErrorKind::NotFound, "nope"))
        });
        assert!(!fonts.font_data.contains_key("yahei"));
        assert!(!fonts.font_data.contains_key("seguiemj"));
        assert!(
            fonts
                .families
                .get(&FontFamily::Monospace)
                .map(|v| !v.is_empty())
                .unwrap_or(false)
        );
        // Terminal families still registered with Hack primaries.
        assert_eq!(
            family_names(&fonts, FAMILY_REGULAR)
                .first()
                .map(String::as_str),
            Some(HACK_REGULAR_NAME)
        );
    }

    #[test]
    fn load_font_definitions_installs_readable_fonts() {
        let fonts = load_font_definitions(&|path| {
            let s = path.to_string_lossy();
            if s.contains("msyh") {
                Ok(vec![0xAA])
            } else if s.contains("seguiemj") {
                Ok(vec![0xBB])
            } else {
                Err(std::io::Error::new(std::io::ErrorKind::NotFound, "x"))
            }
        });
        assert!(fonts.font_data.contains_key("yahei"));
        assert!(fonts.font_data.contains_key("seguiemj"));
        let mono = mono_names(&fonts);
        assert_eq!(mono.last().map(String::as_str), Some("seguiemj"));
        assert!(mono.iter().any(|n| n == "yahei"));
    }

    #[test]
    fn default_mono_and_proportional_match_constructed_fallbacks() {
        // Baseline = today's construction without terminal families.
        let mut baseline = FontDefinitions::default();
        append_font_fallbacks(
            &mut baseline,
            [
                ("yahei".into(), vec![0xAA]),
                ("seguiemj".into(), vec![0xBB]),
            ],
        );
        let fonts = load_font_definitions(&|path| {
            let s = path.to_string_lossy();
            if s.contains("msyh") {
                Ok(vec![0xAA])
            } else if s.contains("seguiemj") {
                Ok(vec![0xBB])
            } else {
                Err(std::io::Error::new(std::io::ErrorKind::NotFound, "x"))
            }
        });
        assert_eq!(mono_names(&fonts), mono_names(&baseline));
        assert_eq!(prop_names(&fonts), prop_names(&baseline));
    }

    #[test]
    fn terminal_families_primary_then_full_mono_tail() {
        let fonts = load_font_definitions(&|path| {
            let s = path.to_string_lossy();
            if s.contains("msyh") {
                Ok(vec![0xAA])
            } else if s.contains("seguiemj") {
                Ok(vec![0xBB])
            } else {
                Err(std::io::Error::new(std::io::ErrorKind::NotFound, "x"))
            }
        });
        let mono = mono_names(&fonts);
        for (fam, primary) in [
            (FAMILY_REGULAR, HACK_REGULAR_NAME),
            (FAMILY_BOLD, HACK_BOLD_NAME),
            (FAMILY_ITALIC, HACK_ITALIC_NAME),
            (FAMILY_BOLD_ITALIC, HACK_BOLD_ITALIC_NAME),
        ] {
            let list = family_names(&fonts, fam);
            assert_eq!(list.first().map(String::as_str), Some(primary));
            assert_eq!(
                &list[1..],
                &mono[..],
                "family {fam} must carry full mono tail"
            );
        }
    }

    #[test]
    fn bundled_hack_faces_are_nonempty() {
        assert!(!HACK_REGULAR.is_empty());
        assert!(!HACK_BOLD.is_empty());
        assert!(!HACK_ITALIC.is_empty());
        assert!(!HACK_BOLD_ITALIC.is_empty());
    }

    #[test]
    fn bundled_hack_faces_are_pairwise_distinct() {
        // Fallback to regular would still layout; identity of the four face
        // blobs must differ so bold/italic cannot silently share Regular.
        use std::collections::HashSet;
        let hashes: HashSet<u64> = [HACK_REGULAR, HACK_BOLD, HACK_ITALIC, HACK_BOLD_ITALIC]
            .into_iter()
            .map(|bytes| {
                // Cheap stable fingerprint (not crypto): size + first/last dwords.
                let mut h = bytes.len() as u64;
                if bytes.len() >= 8 {
                    h ^= u64::from_le_bytes(bytes[0..8].try_into().unwrap());
                    let n = bytes.len();
                    h ^= u64::from_le_bytes(bytes[n - 8..n].try_into().unwrap()).rotate_left(17);
                }
                for (i, chunk) in bytes.chunks(4096).enumerate().take(8) {
                    let mut acc = 0u64;
                    for b in chunk {
                        acc = acc.wrapping_mul(16777619).wrapping_add(*b as u64);
                    }
                    h ^= acc.rotate_left((i as u32 * 7) % 32);
                }
                h
            })
            .collect();
        assert_eq!(
            hashes.len(),
            4,
            "each Hack face blob must be byte-distinct from the others"
        );
        // Registered families must point at different primary names.
        let fonts = load_font_definitions(&|_| {
            Err(std::io::Error::new(std::io::ErrorKind::NotFound, "skip"))
        });
        let primaries: HashSet<String> = [
            FAMILY_REGULAR,
            FAMILY_BOLD,
            FAMILY_ITALIC,
            FAMILY_BOLD_ITALIC,
        ]
        .into_iter()
        .map(|fam| {
            family_names(&fonts, fam)
                .into_iter()
                .next()
                .expect("family non-empty")
        })
        .collect();
        assert_eq!(primaries.len(), 4);
        // font_data entries for the four names must not alias the same Arc contents.
        let reg = fonts.font_data.get(HACK_REGULAR_NAME).unwrap();
        let bold = fonts.font_data.get(HACK_BOLD_NAME).unwrap();
        let ital = fonts.font_data.get(HACK_ITALIC_NAME).unwrap();
        let bi = fonts.font_data.get(HACK_BOLD_ITALIC_NAME).unwrap();
        assert_ne!(reg.font.len(), 0);
        assert_ne!(
            bold.font.as_ref() as *const [u8],
            reg.font.as_ref() as *const [u8]
        );
        assert_ne!(
            ital.font.as_ref() as *const [u8],
            reg.font.as_ref() as *const [u8]
        );
        assert_ne!(
            bi.font.as_ref() as *const [u8],
            reg.font.as_ref() as *const [u8]
        );
        assert_ne!(&*bold.font, &*reg.font);
        assert_ne!(&*ital.font, &*reg.font);
        assert_ne!(&*bi.font, &*reg.font);
    }

    #[test]
    fn headless_four_families_layout_compatible_metrics() {
        let fonts = load_font_definitions(&|_| {
            Err(std::io::Error::new(std::io::ErrorKind::NotFound, "skip"))
        });
        let ctx = egui::Context::default();
        ctx.set_fonts(fonts);
        let _ = ctx.run_ui(egui::RawInput::default(), |_| {});

        let size = 14.0;
        let sample = "MmWi0";
        let mut advances = Vec::new();
        let mut heights = Vec::new();
        for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
            let id = font_id(size, bold, italic);
            let galley =
                ctx.fonts_mut(|f| f.layout_no_wrap(sample.to_string(), id, egui::Color32::WHITE));
            assert!(
                !galley.rect.is_negative() && galley.rect.width() > 0.0,
                "nonzero glyphs for bold={bold} italic={italic}"
            );
            // Per-char advance for first char of sample.
            let g0 = ctx.fonts_mut(|f| {
                f.layout_no_wrap(
                    "M".to_string(),
                    font_id(size, bold, italic),
                    egui::Color32::WHITE,
                )
            });
            advances.push(g0.rect.width());
            heights.push(g0.rect.height());
        }
        // Matched mono faces: advance within a tight band (subpixel ok).
        let min_a = advances.iter().cloned().fold(f32::INFINITY, f32::min);
        let max_a = advances.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        assert!(
            (max_a - min_a) < 0.75,
            "monospaced advance drift too large: {advances:?}"
        );
        let min_h = heights.iter().cloned().fold(f32::INFINITY, f32::min);
        let max_h = heights.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        assert!(
            (max_h - min_h) < 1.5,
            "line height drift too large: {heights:?}"
        );
    }
}
