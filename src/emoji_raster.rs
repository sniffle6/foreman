// No new image crate dep: return raw RGBA bytes + dimensions.
//
// RGBA contract: unpremultiplied sRGBA (byte order R,G,B,A) suitable for
// `egui::ColorImage::from_rgba_unmultiplied`. The Windows path reads
// premultiplied BGRA from D2D/WIC and converts.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbaGlyph {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>, // w*h*4 unpremultiplied RGBA
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

/// Always-None fallback when DirectWrite is unavailable or init fails.
struct NullEmojiRaster;

impl EmojiRaster for NullEmojiRaster {
    fn color_glyph(&mut self, _ch: char, _px: u32) -> Option<RgbaGlyph> {
        None
    }
}

/// System color-emoji raster for GUI wiring. Prefer DirectWrite; else Null.
pub fn system_emoji_raster() -> Box<dyn EmojiRaster> {
    #[cfg(windows)]
    {
        if let Ok(r) = DirectWriteEmojiRaster::new() {
            return Box::new(r);
        }
    }
    Box::new(NullEmojiRaster)
}

// ---------------------------------------------------------------------------
// DirectWrite + Direct2D color glyph path (Windows)
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod dwrite_impl {
    use super::{EmojiRaster, RgbaGlyph};
    use windows::core::w;
    use windows::Win32::Graphics::Direct2D::Common::{
        D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
    };
    use windows::Win32::Graphics::Direct2D::{
        D2D1CreateFactory, ID2D1Factory, ID2D1RenderTarget, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
        D2D1_FACTORY_TYPE_MULTI_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
        D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
        D2D1_RENDER_TARGET_USAGE_NONE,
    };
    use windows::Win32::Graphics::DirectWrite::{
        DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, IDWriteTextLayout,
        DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
        DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_METRICS,
    };
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, IWICBitmap, IWICImagingFactory, GUID_WICPixelFormat32bppPBGRA,
        WICBitmapCacheOnDemand,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
    };

    /// Color-glyph rasterizer via Segoe UI Emoji + D2D color-font draw.
    ///
    /// Fail-open: any COM/font/draw failure yields `None` from `color_glyph`.
    /// Does not panic.
    pub struct DirectWriteEmojiRaster {
        dwrite: IDWriteFactory,
        d2d: ID2D1Factory,
        wic: IWICImagingFactory,
    }

    // COM factories are used only on the GUI thread in practice; the trait bound
    // requires Send. windows-rs interface pointers are Send+Sync.
    unsafe impl Send for DirectWriteEmojiRaster {}

    impl DirectWriteEmojiRaster {
        /// Build DWrite / D2D / WIC factories. Err if COM or factories fail.
        pub fn new() -> Result<Self, String> {
            unsafe {
                // S_OK, S_FALSE (already init), RPC_E_CHANGED_MODE (other apt) —
                // all fine if subsequent CoCreate still works.
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

                let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)
                    .map_err(|e| format!("DWriteCreateFactory: {e}"))?;

                let d2d: ID2D1Factory =
                    D2D1CreateFactory(D2D1_FACTORY_TYPE_MULTI_THREADED, None)
                        .map_err(|e| format!("D2D1CreateFactory: {e}"))?;

                let wic: IWICImagingFactory = CoCreateInstance(
                    &CLSID_WICImagingFactory,
                    None,
                    CLSCTX_INPROC_SERVER,
                )
                .map_err(|e| format!("WIC CoCreateInstance: {e}"))?;

                Ok(Self { dwrite, d2d, wic })
            }
        }

        fn rasterize(&self, ch: char, px: u32) -> Option<RgbaGlyph> {
            if px == 0 {
                return None;
            }
            // Control / non-printables: never a color emoji stamp.
            if ch.is_control() {
                return None;
            }

            self.rasterize_inner(ch, px).ok().flatten()
        }

        fn rasterize_inner(&self, ch: char, px: u32) -> Result<Option<RgbaGlyph>, String> {
            let font_size = px as f32;
            // Generous layout box so metrics aren't clipped.
            let layout_box = (px as f32 * 2.0).max(font_size + 4.0);

            // UTF-16 code units for the single scalar (emoji may be one or two units).
            let mut utf16_buf = [0u16; 2];
            let utf16 = ch.encode_utf16(&mut utf16_buf);

            // All COM/DWrite/D2D calls live in one unsafe block (edition 2024).
            unsafe {
                let format: IDWriteTextFormat = self
                    .dwrite
                    .CreateTextFormat(
                        w!("Segoe UI Emoji"),
                        None,
                        DWRITE_FONT_WEIGHT_NORMAL,
                        DWRITE_FONT_STYLE_NORMAL,
                        DWRITE_FONT_STRETCH_NORMAL,
                        font_size,
                        w!("en-us"),
                    )
                    .map_err(|e| format!("CreateTextFormat: {e}"))?;

                let layout: IDWriteTextLayout = self
                    .dwrite
                    .CreateTextLayout(utf16, &format, layout_box, layout_box)
                    .map_err(|e| format!("CreateTextLayout: {e}"))?;

                let mut metrics = DWRITE_TEXT_METRICS::default();
                layout
                    .GetMetrics(&mut metrics)
                    .map_err(|e| format!("GetMetrics: {e}"))?;

                // Include left/top overhang; add 2px pad so antialias isn't clipped.
                let pad = 2.0f32;
                let origin_x = (-metrics.left + pad).max(0.0);
                let origin_y = (-metrics.top + pad).max(0.0);
                let w_f = metrics.left + metrics.width + origin_x + pad;
                let h_f = metrics.top + metrics.height + origin_y + pad;
                let bw = w_f.ceil().max(1.0) as u32;
                let bh = h_f.ceil().max(1.0) as u32;
                // Cap absurd sizes (broken metrics).
                if bw > 512 || bh > 512 {
                    return Ok(None);
                }

                let bitmap: IWICBitmap = self
                    .wic
                    .CreateBitmap(bw, bh, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnDemand)
                    .map_err(|e| format!("CreateBitmap: {e}"))?;

                let rt_props = D2D1_RENDER_TARGET_PROPERTIES {
                    r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                    },
                    dpiX: 96.0,
                    dpiY: 96.0,
                    usage: D2D1_RENDER_TARGET_USAGE_NONE,
                    minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
                };

                let rt: ID2D1RenderTarget = self
                    .d2d
                    .CreateWicBitmapRenderTarget(&bitmap, &rt_props)
                    .map_err(|e| format!("CreateWicBitmapRenderTarget: {e}"))?;

                let clear = D2D1_COLOR_F {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.0,
                };
                let brush_color = D2D1_COLOR_F {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                };

                rt.BeginDraw();
                rt.Clear(Some(&clear));
                let brush = rt
                    .CreateSolidColorBrush(&brush_color, None)
                    .map_err(|e| format!("CreateSolidColorBrush: {e}"))?;

                // Color layers come from the COLR/CPAL (or CBDT) font; brush is for
                // monochrome fallback outlines only.
                rt.DrawTextLayout(
                    windows_numerics::Vector2 {
                        X: origin_x,
                        Y: origin_y,
                    },
                    &layout,
                    &brush,
                    D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
                );
                rt.EndDraw(None, None)
                    .map_err(|e| format!("EndDraw: {e}"))?;

                let stride = bw.checked_mul(4).ok_or_else(|| "stride overflow".to_string())?;
                let nbytes = (stride as usize)
                    .checked_mul(bh as usize)
                    .ok_or_else(|| "buf overflow".to_string())?;
                let mut bgra = vec![0u8; nbytes];
                // Full-bitmap copy: null rect.
                bitmap
                    .CopyPixels(std::ptr::null(), stride, &mut bgra)
                    .map_err(|e| format!("CopyPixels: {e}"))?;

                let rgba = pbgra_premul_to_rgba_straight(&bgra);
                if rgba.iter().all(|&b| b == 0) {
                    // Empty / missing glyph → fail-open to mono.
                    return Ok(None);
                }

                Ok(Some(RgbaGlyph {
                    w: bw,
                    h: bh,
                    rgba,
                }))
            }
        }
    }

    impl EmojiRaster for DirectWriteEmojiRaster {
        fn color_glyph(&mut self, ch: char, px: u32) -> Option<RgbaGlyph> {
            self.rasterize(ch, px)
        }
    }

    /// Premultiplied BGRA8 → unpremultiplied RGBA8 for egui ColorImage.
    fn pbgra_premul_to_rgba_straight(bgra: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(bgra.len());
        for px in bgra.chunks_exact(4) {
            let b = px[0] as u32;
            let g = px[1] as u32;
            let r = px[2] as u32;
            let a = px[3] as u32;
            if a == 0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
            } else if a == 255 {
                out.extend_from_slice(&[r as u8, g as u8, b as u8, 255]);
            } else {
                // straight = premul * 255 / a, clamped
                let ur = ((r * 255 + a / 2) / a).min(255) as u8;
                let ug = ((g * 255 + a / 2) / a).min(255) as u8;
                let ub = ((b * 255 + a / 2) / a).min(255) as u8;
                out.extend_from_slice(&[ur, ug, ub, a as u8]);
            }
        }
        out
    }
}

#[cfg(windows)]
pub use dwrite_impl::DirectWriteEmojiRaster;

#[cfg(not(windows))]
pub struct DirectWriteEmojiRaster;

#[cfg(not(windows))]
impl DirectWriteEmojiRaster {
    pub fn new() -> Result<Self, String> {
        Err("DirectWrite color emoji is only available on Windows".into())
    }
}

#[cfg(not(windows))]
impl EmojiRaster for DirectWriteEmojiRaster {
    fn color_glyph(&mut self, _ch: char, _px: u32) -> Option<RgbaGlyph> {
        None
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

    #[test]
    fn system_emoji_raster_is_send() {
        // Compile-time: Box<dyn EmojiRaster> must be constructible.
        let _r: Box<dyn EmojiRaster> = system_emoji_raster();
    }

    #[test]
    #[ignore] // needs font; run: cargo test dwrite_cucumber -- --ignored --nocapture
    fn dwrite_cucumber_nonzero() {
        let mut r = DirectWriteEmojiRaster::new().expect("dwrite");
        let g = r.color_glyph('🥒', 32).expect("glyph");
        assert!(g.w > 0 && g.h > 0);
        assert_eq!(g.rgba.len(), (g.w * g.h * 4) as usize);
        // not all zeros
        assert!(g.rgba.iter().any(|&b| b != 0));
        eprintln!(
            "🥒 glyph {}x{}, non-zero bytes: {}, sample RGBA[0..4]={:?}",
            g.w,
            g.h,
            g.rgba.iter().filter(|&&b| b != 0).count(),
            &g.rgba[..4.min(g.rgba.len())]
        );
    }
}
