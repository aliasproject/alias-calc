//! The calculator screen: the expression line, the result, and the keypad.
//!
//! Layout is in logical pixels and multiplied by the scale when drawn, so
//! it's the same at every scale and sharp at all of them.

use crate::render::{Canvas, Color, Face, Fonts};

/// The keypad, row by row, as its keys are labelled.
pub const KEYS: [[&str; 4]; 5] = [
    ["C", "±", "%", "÷"],
    ["7", "8", "9", "×"],
    ["4", "5", "6", "−"],
    ["1", "2", "3", "+"],
    ["0", ".", "⌫", "="],
];

/// How a key looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Digit,
    /// C, ± and % and ⌫.
    Function,
    /// ÷ × − +.
    Operator,
    Equals,
}

fn role(label: &str) -> Role {
    match label {
        "=" => Role::Equals,
        "÷" | "×" | "−" | "+" => Role::Operator,
        "C" | "±" | "%" | "⌫" => Role::Function,
        _ => Role::Digit,
    }
}

/// The colors the screen is drawn with, worked out from a theme's
/// background, foreground and accent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub background: Color,
    pub foreground: Color,
    pub accent: Color,
    /// The expression line and the function keys' labels.
    pub muted: Color,
    pub key: Color,
    pub key_pressed: Color,
    /// The label on the accent-filled `=` key.
    pub on_accent: Color,
}

impl Palette {
    pub fn new(background: Color, foreground: Color, accent: Color) -> Self {
        let dark = background.luminance() < 0.5;
        let key = background.mix(foreground, if dark { 0.08 } else { 0.06 });
        let key_pressed = background.mix(foreground, if dark { 0.18 } else { 0.14 });
        // Whichever of background and foreground reads better on the accent.
        let on_accent = if contrast(accent, background) >= contrast(accent, foreground) {
            background
        } else {
            foreground
        };
        Self {
            background,
            foreground,
            accent,
            muted: background.mix(foreground, 0.55),
            key,
            key_pressed,
            on_accent,
        }
    }
}

impl Default for Palette {
    /// Used until the theme is read, and when it can't be.
    fn default() -> Self {
        Palette::new(
            Color::rgb(0x1a, 0x1b, 0x26),
            Color::rgb(0xc0, 0xca, 0xf5),
            Color::rgb(0x7a, 0xa2, 0xf7),
        )
    }
}

/// WCAG contrast ratio between two colors.
fn contrast(a: Color, b: Color) -> f32 {
    let (la, lb) = (a.luminance(), b.luminance());
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// What the screen shows.
pub struct View<'a> {
    pub expression: &'a str,
    pub display: &'a str,
    /// The key held down with the mouse, as (row, column).
    pub pressed: Option<(usize, usize)>,
}

// Logical sizes.
const PADDING: f32 = 12.0;
const GAP: f32 = 6.0;
const RADIUS: f32 = 4.0;
const EXPRESSION_SIZE: f32 = 18.0;
const DISPLAY_SIZE: f32 = 60.0;
const KEY_SIZE: f32 = 24.0;
/// The share of the height above the keypad.
const DISPLAY_SHARE: f32 = 0.3;

/// Where things go for a given logical size.
struct Layout {
    /// The keypad's top-left corner and one key's size.
    grid: (f32, f32),
    key: (f32, f32),
    /// Baselines of the expression line and the result.
    expression_baseline: f32,
    display_baseline: f32,
}

impl Layout {
    fn new(fonts: &Fonts, width: f32, height: f32) -> Self {
        let display_height = (height * DISPLAY_SHARE).max(EXPRESSION_SIZE + DISPLAY_SIZE);
        let grid_top = PADDING + display_height;
        let rows = KEYS.len() as f32;
        let cols = KEYS[0].len() as f32;
        let key_w = ((width - 2.0 * PADDING - (cols - 1.0) * GAP) / cols).max(1.0);
        let key_h = ((height - grid_top - PADDING - (rows - 1.0) * GAP) / rows).max(1.0);
        let display_baseline = grid_top - GAP - 4.0;
        let expression_baseline = display_baseline
            - fonts.digit_height(Face::Light, DISPLAY_SIZE)
            - EXPRESSION_SIZE * 0.6;
        Self {
            grid: (PADDING, grid_top),
            key: (key_w, key_h),
            expression_baseline: expression_baseline.max(PADDING + EXPRESSION_SIZE),
            display_baseline,
        }
    }

    /// A key's rectangle: x, y, width, height.
    fn key_rect(&self, row: usize, col: usize) -> (f32, f32, f32, f32) {
        let x = self.grid.0 + col as f32 * (self.key.0 + GAP);
        let y = self.grid.1 + row as f32 * (self.key.1 + GAP);
        (x, y, self.key.0, self.key.1)
    }
}

/// Draws the whole screen. `scale` is buffer pixels per logical pixel.
pub fn draw(canvas: &mut Canvas, fonts: &mut Fonts, palette: &Palette, view: &View, scale: f32) {
    let width = canvas.width() as f32 / scale;
    let height = canvas.height() as f32 / scale;
    let layout = Layout::new(fonts, width, height);
    let s = |v: f32| v * scale;

    canvas.fill(palette.background);

    // The expression line and the result, right-aligned.
    let max_text = s(width - 2.0 * PADDING);
    let right = s(width - PADDING);
    if !view.expression.is_empty() {
        let size = fonts.fit(Face::Regular, s(EXPRESSION_SIZE), view.expression, max_text);
        let w = fonts.width(Face::Regular, size, view.expression);
        let baseline = s(layout.expression_baseline);
        canvas.text(
            fonts,
            Face::Regular,
            size,
            view.expression,
            (right - w, baseline),
            palette.muted,
        );
    }
    let size = fonts.fit(Face::Light, s(DISPLAY_SIZE), view.display, max_text);
    let w = fonts.width(Face::Light, size, view.display);
    let baseline = s(layout.display_baseline);
    canvas.text(
        fonts,
        Face::Light,
        size,
        view.display,
        (right - w, baseline),
        palette.foreground,
    );

    // The keypad.
    for (row, labels) in KEYS.iter().enumerate() {
        for (col, &label) in labels.iter().enumerate() {
            let (x, y, w, h) = layout.key_rect(row, col);
            let pressed = view.pressed == Some((row, col));
            let (fill, ink) = match role(label) {
                Role::Equals => (
                    if pressed {
                        palette.accent.mix(palette.background, 0.25)
                    } else {
                        palette.accent
                    },
                    palette.on_accent,
                ),
                other => {
                    let fill = if pressed {
                        palette.key_pressed
                    } else {
                        palette.key
                    };
                    let ink = match other {
                        Role::Operator => palette.accent,
                        Role::Function => palette.muted,
                        _ => palette.foreground,
                    };
                    (fill, ink)
                }
            };
            // Snap keys to whole pixels so their straight edges stay crisp.
            let (px, py) = (s(x).round(), s(y).round());
            let (pw, ph) = (s(x + w).round() - px, s(y + h).round() - py);
            canvas.rounded_rect(px, py, pw, ph, s(RADIUS), fill);

            // JetBrains Mono draws × ÷ − + small next to its digits.
            let size = match role(label) {
                Role::Operator | Role::Equals => s(KEY_SIZE) * 1.3,
                _ => s(KEY_SIZE),
            }
            .min(ph * 0.5);
            let tw = fonts.width(Face::Regular, size, label);
            let th = fonts.digit_height(Face::Regular, size);
            let tx = px + (pw - tw) / 2.0;
            let baseline = py + (ph + th) / 2.0;
            canvas.text(
                fonts,
                Face::Regular,
                size,
                label,
                (tx, baseline.round()),
                ink,
            );
        }
    }
}

/// The key at logical position (x, y), as (row, column).
#[allow(dead_code)] // Mouse input uses it (ALI-2101).
pub fn hit_test(fonts: &Fonts, width: f32, height: f32, x: f32, y: f32) -> Option<(usize, usize)> {
    let layout = Layout::new(fonts, width, height);
    for (row, labels) in KEYS.iter().enumerate() {
        for col in 0..labels.len() {
            let (kx, ky, kw, kh) = layout.key_rect(row, col);
            if x >= kx && x < kx + kw && y >= ky && y < ky + kh {
                return Some((row, col));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn hit_test_finds_the_corner_keys_and_misses_the_gaps() {
        let fonts = Fonts::load();
        let (w, h) = (320.0, 480.0);
        let layout = Layout::new(&fonts, w, h);
        let (x, y, kw, kh) = layout.key_rect(0, 0);
        assert_eq!(hit_test(&fonts, w, h, x + 1.0, y + 1.0), Some((0, 0)));
        let (x, y, ..) = layout.key_rect(4, 3);
        assert_eq!(hit_test(&fonts, w, h, x + 1.0, y + 1.0), Some((4, 3)));
        // Between the first two keys of the first row.
        assert_eq!(
            hit_test(&fonts, w, h, PADDING + kw + GAP / 2.0, y - kh),
            None
        );
        // The display area.
        assert_eq!(hit_test(&fonts, w, h, w / 2.0, PADDING), None);
    }

    #[test]
    fn keypad_fits_inside_the_window() {
        let fonts = Fonts::load();
        for (w, h) in [(260.0, 380.0), (320.0, 480.0), (900.0, 1200.0)] {
            let layout = Layout::new(&fonts, w, h);
            let (x, y, kw, kh) = layout.key_rect(4, 3);
            assert!(
                x + kw <= w - PADDING + 0.01 && y + kh <= h - PADDING + 0.01,
                "{w}x{h}"
            );
            assert!(layout.expression_baseline < layout.display_baseline);
        }
    }

    #[test]
    fn on_accent_picks_the_readable_label() {
        let dark = Palette::new(
            Color::rgb(0x0b, 0x11, 0x1a),
            Color::rgb(0xc5, 0xce, 0xd6),
            Color::rgb(0x5e, 0x92, 0xc6),
        );
        assert_eq!(dark.on_accent, dark.background);
        let light = Palette::new(
            Color::rgb(0xfa, 0xfa, 0xfa),
            Color::rgb(0x20, 0x20, 0x20),
            Color::rgb(0x1d, 0x4e, 0xd8),
        );
        assert_eq!(light.on_accent, light.background);
    }

    /// The renderer's budget: a full frame in under 1 ms at 4K scale 2
    /// (640x960 px). Optimised builds only: `cargo test --release -- --ignored`.
    #[test]
    #[ignore]
    fn budget_full_frame_under_1ms() {
        let mut fonts = Fonts::load();
        let (w, h) = (640, 960);
        let mut bytes = vec![0; w * h * 4];
        let palette = Palette::default();
        let view = View {
            expression: "1,234 × 5,678 +",
            display: "7,006,652",
            pressed: Some((2, 1)),
        };
        // Warm the glyph cache, as every frame after the first finds it.
        draw(
            &mut Canvas::new(&mut bytes, w, h),
            &mut fonts,
            &palette,
            &view,
            2.0,
        );
        let runs = 200;
        let start = Instant::now();
        for _ in 0..runs {
            draw(
                &mut Canvas::new(&mut bytes, w, h),
                &mut fonts,
                &palette,
                &view,
                2.0,
            );
        }
        let per_frame = start.elapsed() / runs;
        println!("full frame at 640x960: {per_frame:?}");
        assert!(per_frame.as_micros() < 1000, "{per_frame:?}");
    }
}
