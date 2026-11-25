//! A small CPU renderer: solid fills, anti-aliased rounded rectangles and
//! text, straight into the `wl_shm` buffer. No toolkit, no fontconfig.
//!
//! Colors are opaque; only coverage (from the rectangle edges and the glyph
//! bitmaps) is blended. The buffer is ARGB8888, which is B, G, R, A in memory.

use std::collections::HashMap;

use fontdue::{Font, FontSettings, Metrics};

/// An opaque color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Parses `#rrggbb` (the `#` is optional).
    pub fn from_hex(hex: &str) -> Option<Self> {
        let hex = hex.trim();
        let hex = hex.strip_prefix('#').unwrap_or(hex);
        // from_str_radix alone would take a leading '+'.
        if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        Some(Self::rgb(channel(0)?, channel(2)?, channel(4)?))
    }

    /// `self` moved `t` (0..=1) of the way towards `other`.
    pub fn mix(self, other: Color, t: f32) -> Color {
        let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Color::rgb(
            lerp(self.r, other.r),
            lerp(self.g, other.g),
            lerp(self.b, other.b),
        )
    }

    /// Relative luminance (WCAG), 0 for black to 1 for white.
    pub fn luminance(self) -> f32 {
        let linear = |c: u8| {
            let c = c as f32 / 255.0;
            if c <= 0.040_45 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(self.r) + 0.7152 * linear(self.g) + 0.0722 * linear(self.b)
    }

    const fn pixel(self) -> [u8; 4] {
        [self.b, self.g, self.r, 0xff]
    }
}

/// Blends `color` over `dst` with coverage `a` (0..=255).
#[inline]
fn blend(dst: &mut [u8; 4], color: [u8; 4], a: u32) {
    if a >= 255 {
        *dst = color;
    } else if a > 0 {
        for i in 0..3 {
            let d = dst[i] as u32;
            let s = color[i] as u32;
            // d + (s - d) * a / 255, rounded, without going negative.
            dst[i] = ((d * (255 - a) + s * a + 127) / 255) as u8;
        }
    }
}

/// A pixel buffer to draw into.
pub struct Canvas<'a> {
    px: &'a mut [[u8; 4]],
    width: usize,
    height: usize,
}

impl<'a> Canvas<'a> {
    /// Wraps a `width * height` ARGB8888 buffer.
    pub fn new(bytes: &'a mut [u8], width: usize, height: usize) -> Self {
        let (px, _) = bytes.as_chunks_mut::<4>();
        let px = &mut px[..width * height];
        Self { px, width, height }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn fill(&mut self, color: Color) {
        self.px.fill(color.pixel());
    }

    /// Fills a rectangle with corners of radius `r`, anti-aliased. All
    /// values are in pixels and may be fractional.
    pub fn rounded_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, color: Color) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let pixel = color.pixel();
        let hw = w / 2.0;
        let hh = h / 2.0;
        let r = r.clamp(0.0, hw.min(hh));
        let cx = x + hw;
        let cy = y + hh;

        let x0 = (x.floor().max(0.0)) as usize;
        let y0 = (y.floor().max(0.0)) as usize;
        let x1 = ((x + w).ceil() as usize).min(self.width);
        let y1 = ((y + h).ceil() as usize).min(self.height);

        for py in y0..y1 {
            let qy = ((py as f32 + 0.5) - cy).abs();
            let row = &mut self.px[py * self.width..(py + 1) * self.width];
            // Rows clear of the corners and the top and bottom edges are
            // solid except for their two end pixels.
            let solid_row = qy + 0.5 <= hh - r && qy + 0.5 <= hh;
            for (px, dst) in row.iter_mut().enumerate().take(x1).skip(x0) {
                let qx = ((px as f32 + 0.5) - cx).abs();
                if solid_row && qx + 0.5 <= hw {
                    *dst = pixel;
                    continue;
                }
                // Signed distance from the pixel center to the shape.
                let dx = qx - (hw - r);
                let dy = qy - (hh - r);
                let outside = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt();
                let inside = dx.max(dy).min(0.0);
                let distance = outside + inside - r;
                let coverage = (0.5 - distance).clamp(0.0, 1.0);
                blend(dst, pixel, (coverage * 255.0).round() as u32);
            }
        }
    }

    /// Draws `text` with its left edge and baseline at `origin`.
    pub fn text(
        &mut self,
        fonts: &mut Fonts,
        face: Face,
        size: f32,
        text: &str,
        origin: (f32, f32),
        color: Color,
    ) {
        let (x, baseline) = origin;
        let size = quantize(size);
        let pixel = color.pixel();
        let mut pen = x;
        for ch in text.chars() {
            let glyph = fonts.glyph(face, ch, size);
            let left = (pen + glyph.metrics.xmin as f32).round() as isize;
            let top = (baseline - glyph.metrics.ymin as f32 - glyph.metrics.height as f32).round()
                as isize;
            for gy in 0..glyph.metrics.height {
                let py = top + gy as isize;
                if py < 0 || py as usize >= self.height {
                    continue;
                }
                let src = &glyph.coverage[gy * glyph.metrics.width..(gy + 1) * glyph.metrics.width];
                let row = &mut self.px[py as usize * self.width..(py as usize + 1) * self.width];
                for (gx, &a) in src.iter().enumerate() {
                    let px = left + gx as isize;
                    if px < 0 || px as usize >= self.width {
                        continue;
                    }
                    blend(&mut row[px as usize], pixel, a as u32);
                }
            }
            pen += glyph.metrics.advance_width.round();
        }
    }
}

/// Text sizes snap to whole pixels, so the glyph cache holds one entry per
/// size actually drawn, and what's measured is what's rasterized.
pub fn quantize(size: f32) -> f32 {
    size.round().max(1.0)
}

/// More cached glyphs than this (a long run of resizes) empties the cache.
const CACHE_LIMIT: usize = 512;

/// The two embedded weights. Both are subsets of JetBrains Mono, the aliasOS
/// monospace font: printable ASCII plus × ÷ − ± ⌫ … (see fonts/README.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Face {
    Regular,
    Light,
}

static REGULAR: &[u8] = include_bytes!("../fonts/JetBrainsMono-Regular-subset.ttf");
static LIGHT: &[u8] = include_bytes!("../fonts/JetBrainsMono-Light-subset.ttf");

struct Glyph {
    metrics: Metrics,
    coverage: Vec<u8>,
}

/// The fonts and a cache of every glyph rasterized so far, keyed by face,
/// character and whole-pixel size. A calculator shows a few dozen glyphs at
/// three or four sizes, so a frame almost never rasterizes; resizing through
/// many sizes is bounded by `CACHE_LIMIT`.
pub struct Fonts {
    regular: Font,
    light: Font,
    cache: HashMap<(Face, char, u32), Glyph>,
}

impl Fonts {
    pub fn load() -> Self {
        let settings = || FontSettings {
            collection_index: 0,
            scale: 40.0,
            load_substitutions: false,
        };
        // The fonts are compiled in and tested below, so they always parse.
        let parse =
            |bytes: &'static [u8]| Font::from_bytes(bytes, settings()).expect("embedded font");
        Self {
            regular: parse(REGULAR),
            light: parse(LIGHT),
            cache: HashMap::new(),
        }
    }

    fn font(&self, face: Face) -> &Font {
        match face {
            Face::Regular => &self.regular,
            Face::Light => &self.light,
        }
    }

    /// `size` must already be quantized.
    fn glyph(&mut self, face: Face, ch: char, size: f32) -> &Glyph {
        let key = (face, ch, size as u32);
        if self.cache.len() >= CACHE_LIMIT && !self.cache.contains_key(&key) {
            self.cache.clear();
        }
        let font = match face {
            Face::Regular => &self.regular,
            Face::Light => &self.light,
        };
        self.cache.entry(key).or_insert_with(|| {
            let (metrics, coverage) = font.rasterize(ch, size);
            Glyph { metrics, coverage }
        })
    }

    /// The advance width of `text`. The font is monospaced, so this is the
    /// character count times one advance.
    pub fn width(&self, face: Face, size: f32, text: &str) -> f32 {
        let font = self.font(face);
        let size = quantize(size);
        text.chars()
            .map(|ch| font.metrics(ch, size).advance_width.round())
            .sum()
    }

    /// The height of the digits above the baseline, for centring numbers
    /// and symbols optically.
    pub fn digit_height(&self, face: Face, size: f32) -> f32 {
        let m = self.font(face).metrics('0', quantize(size));
        m.height as f32 + m.ymin as f32
    }

    /// The largest whole-pixel size, from `preferred` down to `min`, at
    /// which `text` fits `max_width`; `min` if it doesn't fit even then.
    pub fn fit(&self, face: Face, preferred: f32, min: f32, text: &str, max_width: f32) -> f32 {
        let preferred = quantize(preferred);
        let min = quantize(min).min(preferred);
        let width = self.width(face, preferred, text);
        if width <= max_width || width <= 0.0 {
            return preferred;
        }
        // Advance scales about linearly with size; rounding can leave it a
        // pixel over, so step down until it fits.
        let mut size = (preferred * max_width / width)
            .floor()
            .clamp(min, preferred);
        while size > min && self.width(face, size, text) > max_width {
            size -= 1.0;
        }
        size
    }

    /// `text`, with characters dropped from the front and an ellipsis put
    /// in their place until it fits `max_width`. The end of an expression
    /// is the part being worked on, so it's the part kept.
    pub fn elide_start<'t>(
        &self,
        face: Face,
        size: f32,
        text: &'t str,
        max_width: f32,
    ) -> std::borrow::Cow<'t, str> {
        if self.width(face, size, text) <= max_width {
            return text.into();
        }
        let mut chars = text.chars();
        while chars.next().is_some() {
            let rest = chars.as_str();
            let candidate = format!("…{rest}");
            if rest.is_empty() || self.width(face, size, &candidate) <= max_width {
                return candidate.into();
            }
        }
        "…".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLACK: Color = Color::rgb(0, 0, 0);
    const WHITE: Color = Color::rgb(255, 255, 255);

    fn canvas_bytes(w: usize, h: usize) -> Vec<u8> {
        vec![0; w * h * 4]
    }

    fn red_at(bytes: &[u8], w: usize, x: usize, y: usize) -> u8 {
        bytes[(y * w + x) * 4 + 2]
    }

    #[test]
    fn parses_hex_colors() {
        assert_eq!(
            Color::from_hex("#5E92C6"),
            Some(Color::rgb(0x5e, 0x92, 0xc6))
        );
        assert_eq!(
            Color::from_hex("0b111a"),
            Some(Color::rgb(0x0b, 0x11, 0x1a))
        );
        assert_eq!(Color::from_hex("#fff"), None);
        assert_eq!(Color::from_hex("#gg0000"), None);
        assert_eq!(Color::from_hex("#ééé"), None);
        assert_eq!(Color::from_hex("#+1+2+3"), None);
        assert_eq!(Color::from_hex("##ffffff"), None);
    }

    #[test]
    fn mixes_and_measures_luminance() {
        assert_eq!(BLACK.mix(WHITE, 0.5), Color::rgb(128, 128, 128));
        assert!(WHITE.luminance() > 0.99);
        assert!(BLACK.luminance() < 0.01);
    }

    #[test]
    fn writes_bgra() {
        let mut bytes = canvas_bytes(1, 1);
        Canvas::new(&mut bytes, 1, 1).fill(Color::rgb(1, 2, 3));
        assert_eq!(bytes, [3, 2, 1, 255]);
    }

    #[test]
    fn rounded_rect_is_solid_inside_and_soft_at_the_corners() {
        let (w, h) = (40, 40);
        let mut bytes = canvas_bytes(w, h);
        let mut canvas = Canvas::new(&mut bytes, w, h);
        canvas.fill(BLACK);
        canvas.rounded_rect(10.0, 10.0, 20.0, 20.0, 6.0, WHITE);
        assert_eq!(red_at(&bytes, w, 20, 20), 255, "center");
        assert_eq!(red_at(&bytes, w, 10, 20), 255, "left edge, mid-height");
        assert_eq!(red_at(&bytes, w, 9, 20), 0, "just outside");
        assert_eq!(red_at(&bytes, w, 10, 10), 0, "the corner pixel is cut away");
        let edge = red_at(&bytes, w, 11, 12);
        assert!(
            edge > 0 && edge < 255,
            "corner arc is anti-aliased, got {edge}"
        );
    }

    #[test]
    fn rounded_rect_clips_to_the_canvas() {
        let mut bytes = canvas_bytes(10, 10);
        let mut canvas = Canvas::new(&mut bytes, 10, 10);
        canvas.rounded_rect(-5.0, -5.0, 30.0, 30.0, 4.0, WHITE);
        assert!(bytes.iter().all(|&b| b == 255));
    }

    #[test]
    fn fonts_cover_every_symbol_the_app_shows() {
        let fonts = Fonts::load();
        for face in [Face::Regular, Face::Light] {
            for ch in "0123456789.,+−×÷^%=()±⌫…CEeor-".chars() {
                assert_ne!(
                    fonts.font(face).lookup_glyph_index(ch),
                    0,
                    "{face:?} is missing {ch:?}"
                );
            }
        }
    }

    #[test]
    fn text_draws_and_fits() {
        let mut fonts = Fonts::load();
        let (w, h) = (200, 60);
        let mut bytes = canvas_bytes(w, h);
        let mut canvas = Canvas::new(&mut bytes, w, h);
        canvas.fill(BLACK);
        canvas.text(&mut fonts, Face::Regular, 32.0, "1,234", (4.0, 40.0), WHITE);
        assert!(
            bytes.chunks(4).any(|p| p[2] == 255),
            "some pixels are fully covered"
        );

        let wide = "123,456,789,012,345";
        let size = fonts.fit(Face::Light, 64.0, 8.0, wide, 200.0);
        assert!(size < 64.0 && size == size.round());
        assert!(fonts.width(Face::Light, size, wide) <= 200.0);
        assert_eq!(fonts.fit(Face::Light, 64.0, 8.0, "0", 200.0), 64.0);
        // Too long to fit even at the minimum: the minimum, not 1 px.
        assert_eq!(
            fonts.fit(Face::Light, 64.0, 20.0, &"9".repeat(200), 200.0),
            20.0
        );
    }

    #[test]
    fn elides_from_the_start() {
        let fonts = Fonts::load();
        let text = "1 + 2 + 3 + 4 + 5 + 6 + 7 + 8 + 9";
        let advance = fonts.width(Face::Regular, 20.0, "0");
        let max = advance * 10.0;
        let elided = fonts.elide_start(Face::Regular, 20.0, text, max);
        assert!(
            elided.starts_with('…') && elided.ends_with("8 + 9"),
            "{elided}"
        );
        assert!(fonts.width(Face::Regular, 20.0, &elided) <= max);
        assert_eq!(fonts.elide_start(Face::Regular, 20.0, "12", max), "12");
    }

    #[test]
    fn sizes_and_advances_are_whole_pixels() {
        let fonts = Fonts::load();
        // Measuring at 23.4 and 23.0 agree: both draw at 23.
        assert_eq!(
            fonts.width(Face::Regular, 23.4, "1234"),
            fonts.width(Face::Regular, 23.0, "1234")
        );
        let w = fonts.width(Face::Regular, 23.0, "1234");
        assert_eq!(w, w.round());
    }

    #[test]
    fn glyph_cache_is_bounded() {
        let mut fonts = Fonts::load();
        for size in 1..2000 {
            fonts.glyph(Face::Regular, '8', size as f32);
        }
        assert!(fonts.cache.len() <= CACHE_LIMIT);
    }
}
