// No new image crate dep: return raw RGBA bytes + dimensions.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbaGlyph {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>, // w*h*4
}

pub trait EmojiRaster: Send {
    fn color_glyph(&mut self, ch: char, px: u32) -> Option<RgbaGlyph>;
}

pub struct FakeEmojiRaster {
    pub map: std::collections::HashMap<char, RgbaGlyph>,
}

impl EmojiRaster for FakeEmojiRaster {
    fn color_glyph(&mut self, ch: char, px: u32) -> Option<RgbaGlyph> {
        let _ = px;
        self.map.get(&ch).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_returns_fixture() {
        let g = RgbaGlyph {
            w: 1,
            h: 1,
            rgba: vec![0, 255, 0, 255],
        };
        let mut fake = FakeEmojiRaster {
            map: [('🥒', g)].into_iter().collect(),
        };
        let got = fake.color_glyph('🥒', 16).unwrap();
        assert_eq!(got.rgba, vec![0, 255, 0, 255]);
        assert!(fake.color_glyph('A', 16).is_none());
    }
}
