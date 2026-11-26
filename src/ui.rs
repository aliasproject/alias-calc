//! The calculator screen: the expression line, the result, and the keypad.
//!
//! Layout is in logical pixels and multiplied by the scale when drawn, so
//! it's the same at every scale and sharp at all of them.

use crate::engine::{Key, Op};
use crate::render::{Canvas, Color, Face, Fonts};

/// The keypad, row by row, as its keys are labelled.
pub const KEYS: [[&str; 4]; 5] = [
    ["C", "±", "%", "÷"],
    ["7", "8", "9", "×"],
    ["4", "5", "6", "−"],
    ["1", "2", "3", "+"],
    ["0", ".", "⌫", "="],
];

/// The calculator key each keypad button presses, in the same order as
/// [`KEYS`].
const ACTIONS: [[Key; 4]; 5] = [
    [Key::Clear, Key::Sign, Key::Percent, Key::Op(Op::Div)],
    [
        Key::Digit(7),
        Key::Digit(8),
        Key::Digit(9),
        Key::Op(Op::Mul),
    ],
    [
        Key::Digit(4),
        Key::Digit(5),
        Key::Digit(6),
        Key::Op(Op::Sub),
    ],
    [
        Key::Digit(1),
        Key::Digit(2),
        Key::Digit(3),
        Key::Op(Op::Add),
    ],
    [Key::Digit(0), Key::Point, Key::Backspace, Key::Equals],
];

/// What the button at (row, column) presses.
pub fn action(cell: (usize, usize)) -> Key {
    ACTIONS[cell.0][cell.1]
}

/// The button for a key, to light it up while its keyboard key is held.
/// `^` has no button.
pub fn cell_of(key: Key) -> Option<(usize, usize)> {
    ACTIONS
        .iter()
        .enumerate()
        .find_map(|(row, keys)| keys.iter().position(|&k| k == key).map(|col| (row, col)))
}

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
    /// The result and digit labels.
    pub foreground: Color,
    /// The `=` key's fill.
    pub accent: Color,
    /// The `=` key's fill while held.
    pub accent_pressed: Color,
    /// "Copied": the accent, made readable as small text on the background.
    pub notice: Color,
    /// ÷ × − +: the accent, made readable on the keys.
    pub operator: Color,
    /// The expression line.
    pub muted: Color,
    /// C ± % ⌫: muted, made readable on the keys.
    pub function: Color,
    pub key: Color,
    pub key_pressed: Color,
    /// The label on the accent-filled `=` key.
    pub on_accent: Color,
    /// The keyboard focus ring: 3:1 against the background it's drawn on.
    pub focus: Color,
}

/// WCAG 2.2 AA for non-text (1.4.11): the focus ring, which uses the digit
/// colour and so already has 4.5:1. Keys need no outline: each is
/// identified by its label, not its boundary.
#[cfg(test)]
const NON_TEXT_CONTRAST: f32 = 3.0;

/// WCAG 2.2 AA for text: 4.5:1. Large text (24 px and up) only needs 3:1,
/// but nothing here is large at every size: the result shrinks to fit a long
/// number, and key labels drop to ~22 px at the minimum window. So all text
/// gets 4.5:1.
const TEXT_CONTRAST: f32 = 4.5;

const WHITE: Color = Color::rgb(255, 255, 255);
const BLACK: Color = Color::rgb(0, 0, 0);

impl Palette {
    pub fn new(background: Color, foreground: Color, accent: Color) -> Self {
        // 0.18 is where a color contrasts equally with black and white.
        let dark = background.luminance() < 0.18;
        let key = background.mix(foreground, if dark { 0.08 } else { 0.06 });
        let key_pressed = background.mix(foreground, if dark { 0.18 } else { 0.14 });
        // Whichever of background and foreground reads better on the accent,
        // made readable on it.
        let on_accent = readable(
            if contrast(accent, background) >= contrast(accent, foreground) {
                background
            } else {
                foreground
            },
            &[accent],
            TEXT_CONTRAST,
        );
        // A held = moves away from its label (lighter under a dark label,
        // darker under a light one), so holding it only raises the label's
        // contrast. Moving towards the background could land on a mid-tone
        // where neither a dark nor a light label reaches 4.5:1.
        let away = if on_accent.luminance() < accent.luminance() {
            WHITE
        } else {
            BLACK
        };
        let accent_pressed = accent.mix(away, 0.25);
        // Every text colour is checked against every fill it's drawn on, and
        // moved towards white or black until it passes, so a theme nobody
        // tried still meets AA. Most themes need no change. 65% of the way to
        // the foreground clears 4.5:1 on the background for every aliasOS
        // theme as it is.
        let muted = readable(
            background.mix(foreground, 0.65),
            &[background],
            TEXT_CONTRAST,
        );
        let notice = readable(accent, &[background], TEXT_CONTRAST);
        // On a mid-tone background a key fill can sit where no label reaches
        // 4.5:1 however it's moved. Then the fills step away from the text
        // (darker under light text, lighter under dark) until every label
        // passes; on real themes the first try already does.
        let text_is_light =
            readable(foreground, &[background], TEXT_CONTRAST).luminance() > background.luminance();
        let away_from_text = if text_is_light { BLACK } else { WHITE };
        let (mut key, mut key_pressed) = (key, key_pressed);
        let mut labels = KeyLabels::on(background, foreground, muted, accent, key, key_pressed);
        for _ in 0..20 {
            if labels.pass(background, key, key_pressed) {
                break;
            }
            key = key.mix(away_from_text, 0.1);
            key_pressed = key_pressed.mix(away_from_text, 0.1);
            labels = KeyLabels::on(background, foreground, muted, accent, key, key_pressed);
        }
        // Never seen to fail over 4 million random themes; if a change to the
        // steps or mixes ever breaks that, debug builds and the tests say so.
        debug_assert!(
            labels.pass(background, key, key_pressed),
            "key labels under 4.5:1"
        );
        Self {
            background,
            foreground: labels.foreground,
            accent,
            accent_pressed,
            notice,
            operator: labels.operator,
            muted,
            function: labels.function,
            key,
            key_pressed,
            on_accent,
            // The digits' colour already has 4.5:1 on the background.
            focus: labels.foreground,
        }
    }
}

/// The text drawn on keys (and, for the result, on the background), each
/// made readable on every fill it sits on.
struct KeyLabels {
    /// Digits and the result.
    foreground: Color,
    function: Color,
    operator: Color,
}

impl KeyLabels {
    fn on(
        background: Color,
        foreground: Color,
        muted: Color,
        accent: Color,
        key: Color,
        pressed: Color,
    ) -> Self {
        Self {
            foreground: readable(foreground, &[background, key, pressed], TEXT_CONTRAST),
            function: readable(muted, &[key, pressed], TEXT_CONTRAST),
            operator: readable(accent, &[key, pressed], TEXT_CONTRAST),
        }
    }

    fn pass(&self, background: Color, key: Color, pressed: Color) -> bool {
        let ok = |ink: Color, grounds: &[Color]| {
            grounds.iter().all(|&g| contrast(ink, g) >= TEXT_CONTRAST)
        };
        ok(self.foreground, &[background, key, pressed])
            && ok(self.function, &[key, pressed])
            && ok(self.operator, &[key, pressed])
    }
}

/// `ink`, or the nearest colour towards white or black that has at least
/// `min` contrast with every one of `grounds`. It moves in 5% steps and stops
/// at the first step where either direction passes, taking the direction
/// with more contrast there. If neither gets there it returns the better of white
/// and black; for 4.5:1 that can't happen with the grounds used here (white
/// or black reaches 4.58:1 on any one colour, and each set is close
/// together), which `any_theme_meets_aa` checks over 20,000 random themes.
fn readable(ink: Color, grounds: &[Color], min: f32) -> Color {
    let worst = |c: Color| {
        grounds
            .iter()
            .map(|&g| contrast(c, g))
            .fold(f32::INFINITY, f32::min)
    };
    if worst(ink) >= min {
        return ink;
    }
    for step in 1..=20 {
        let t = step as f32 / 20.0;
        let candidates = [ink.mix(WHITE, t), ink.mix(BLACK, t)];
        let best = candidates
            .into_iter()
            .filter(|&c| worst(c) >= min)
            .max_by(|&a, &b| worst(a).total_cmp(&worst(b)));
        if let Some(c) = best {
            return c;
        }
    }
    if worst(WHITE) >= worst(BLACK) {
        WHITE
    } else {
        BLACK
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
    /// The key drawn pressed, held with the mouse or the keyboard, as
    /// (row, column).
    pub pressed: Option<(usize, usize)>,
    /// A short message ("Copied") shown in the notice colour (the accent,
    /// made readable) in place of
    /// the expression line.
    pub notice: Option<&'a str>,
    /// The key with keyboard focus, when the focus ring should show.
    pub focus: Option<(usize, usize)>,
    /// The aliasOS text size as a factor of the default (1.0 = 12 px).
    pub text_scale: f32,
}

// Logical sizes.
const PADDING: f32 = 12.0;
const GAP: f32 = 6.0;
const RADIUS: f32 = 4.0;
const EXPRESSION_SIZE: f32 = 18.0;
const DISPLAY_SIZE: f32 = 60.0;
const KEY_SIZE: f32 = 24.0;
const EXPRESSION_MIN_SIZE: f32 = 13.0;
const DISPLAY_MIN_SIZE: f32 = 16.0;
/// The smallest a key may be in either direction: the house rule is 44 px
/// (WCAG 2.5.8 asks 24). MIN_SIZE in src/window.rs is chosen to keep it, and
/// larger text never takes keypad height below it.
const MIN_KEY: f32 = 44.0;

/// Focus ring: its width and its gap from the key, inside the 6 px between
/// keys.
const FOCUS_WIDTH: f32 = 2.0;
const FOCUS_OFFSET: f32 = 2.0;
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
    fn new(fonts: &Fonts, width: f32, height: f32, text_scale: f32) -> Self {
        // Larger text wants a taller display, but never at the keys' cost:
        // they keep MIN_KEY each, and the display text fits what's left.
        let keypad_min = 5.0 * MIN_KEY + 4.0 * GAP;
        let display_height = (height * DISPLAY_SHARE)
            .max((EXPRESSION_SIZE + DISPLAY_SIZE) * text_scale)
            .min((height - 2.0 * PADDING - keypad_min).max(height * DISPLAY_SHARE));
        let grid_top = PADDING + display_height;
        let rows = KEYS.len() as f32;
        let cols = KEYS[0].len() as f32;
        let key_w = ((width - 2.0 * PADDING - (cols - 1.0) * GAP) / cols).max(1.0);
        let key_h = ((height - grid_top - PADDING - (rows - 1.0) * GAP) / rows).max(1.0);
        let display_baseline = grid_top - GAP - 4.0;
        let expression_baseline = display_baseline
            - fonts.digit_height(Face::Light, DISPLAY_SIZE * text_scale)
            - EXPRESSION_SIZE * text_scale * 0.6;
        Self {
            grid: (PADDING, grid_top),
            key: (key_w, key_h),
            expression_baseline: expression_baseline.max(PADDING + EXPRESSION_SIZE * text_scale),
            display_baseline,
        }
    }

    /// A key's rectangle in buffer pixels, snapped to whole pixels so its
    /// straight edges stay crisp. Drawing and hit-testing both use this.
    fn key_pixels(&self, row: usize, col: usize, scale: f32) -> (f32, f32, f32, f32) {
        let (x, y, w, h) = self.key_rect(row, col);
        let (px, py) = ((x * scale).round(), (y * scale).round());
        let (pw, ph) = (
            ((x + w) * scale).round() - px,
            ((y + h) * scale).round() - py,
        );
        (px, py, pw, ph)
    }

    /// A key's rectangle in logical pixels: x, y, width, height.
    fn key_rect(&self, row: usize, col: usize) -> (f32, f32, f32, f32) {
        let x = self.grid.0 + col as f32 * (self.key.0 + GAP);
        let y = self.grid.1 + row as f32 * (self.key.1 + GAP);
        (x, y, self.key.0, self.key.1)
    }
}

/// Draws the whole screen. `scale` is buffer pixels per logical pixel.
/// `size` is the window's logical size, the same one `hit_test` is given,
/// so both lay the keys out identically.
pub fn draw(
    canvas: &mut Canvas,
    fonts: &mut Fonts,
    palette: &Palette,
    view: &View,
    (width, height): (f32, f32),
    scale: f32,
) {
    let layout = Layout::new(fonts, width, height, view.text_scale);
    let s = |v: f32| v * scale;
    // Text sizes follow the aliasOS text size; spacing doesn't.
    let t = |v: f32| v * scale * view.text_scale;
    // Minimums don't shrink below the defaults with smaller text.
    let floor = |v: f32| v * scale * view.text_scale.max(1.0);

    canvas.fill(palette.background);

    // The expression line and the result, right-aligned.
    let max_text = s(width - 2.0 * PADDING);
    let right = s(width - PADDING);
    if let Some(notice) = view.notice {
        let size = fonts.fit(
            Face::Regular,
            t(EXPRESSION_SIZE),
            floor(EXPRESSION_MIN_SIZE),
            notice,
            max_text,
        );
        let w = fonts.width(Face::Regular, size, notice);
        let baseline = s(layout.expression_baseline);
        canvas.text(
            fonts,
            Face::Regular,
            size,
            notice,
            (right - w, baseline),
            palette.notice,
        );
    } else if !view.expression.is_empty() {
        let size = fonts.fit(
            Face::Regular,
            t(EXPRESSION_SIZE),
            floor(EXPRESSION_MIN_SIZE),
            view.expression,
            max_text,
        );
        let expression = fonts.elide_start(Face::Regular, size, view.expression, max_text);
        let w = fonts.width(Face::Regular, size, &expression);
        let baseline = s(layout.expression_baseline);
        canvas.text(
            fonts,
            Face::Regular,
            size,
            &expression,
            (right - w, baseline),
            palette.muted,
        );
    }
    // The engine keeps a number to 16 digits plus separators and sign, which
    // fits at the minimum size in a window of the minimum width. A window
    // tiled narrower still shrinks the number further: a smaller number is
    // still right, a clipped one isn't.
    let mut size = fonts.fit(
        Face::Light,
        t(DISPLAY_SIZE),
        floor(DISPLAY_MIN_SIZE),
        view.display,
        max_text,
    );
    if fonts.width(Face::Light, size, view.display) > max_text {
        size = fonts.fit(Face::Light, size, 1.0, view.display, max_text);
    }
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
            let pressed = view.pressed == Some((row, col));
            let (fill, ink) = match role(label) {
                Role::Equals => (
                    if pressed {
                        palette.accent_pressed
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
                        Role::Operator => palette.operator,
                        Role::Function => palette.function,
                        _ => palette.foreground,
                    };
                    (fill, ink)
                }
            };
            let (px, py, pw, ph) = layout.key_pixels(row, col, scale);
            canvas.rounded_rect(px, py, pw, ph, s(RADIUS), fill);
            if view.focus == Some((row, col)) {
                focus_ring(canvas, (px, py, pw, ph), scale, palette.focus);
            }

            // JetBrains Mono draws × ÷ − + small next to its digits.
            let size = match role(label) {
                Role::Operator | Role::Equals => t(KEY_SIZE) * 1.3,
                _ => t(KEY_SIZE),
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

/// A ring around a key, in the gap between keys: an outline, so focus never
/// depends on colour alone.
fn focus_ring(
    canvas: &mut Canvas,
    (px, py, pw, ph): (f32, f32, f32, f32),
    scale: f32,
    color: Color,
) {
    let gap = (FOCUS_OFFSET * scale).round();
    let width = (FOCUS_WIDTH * scale).round().max(1.0);
    let out = gap + width;
    // Four edges, square-cornered like aliasOS windows, so the key inside
    // is never painted over.
    let (x0, y0, x1, y1) = (px - out, py - out, px + pw + out, py + ph + out);
    canvas.rounded_rect(x0, y0, x1 - x0, width, 0.0, color);
    canvas.rounded_rect(x0, y1 - width, x1 - x0, width, 0.0, color);
    canvas.rounded_rect(x0, y0, width, y1 - y0, 0.0, color);
    canvas.rounded_rect(x1 - width, y0, width, y1 - y0, 0.0, color);
}

/// Where keyboard focus goes from `from`. With no focus yet, every move
/// lands on the first key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    Next,
    Previous,
    Up,
    Down,
    Left,
    Right,
}

pub fn move_focus(from: Option<(usize, usize)>, how: Move) -> (usize, usize) {
    let (rows, cols) = (KEYS.len(), KEYS[0].len());
    let Some((r, c)) = from else { return (0, 0) };
    match how {
        // Tab order is reading order, wrapping at the end.
        Move::Next => {
            let i = (r * cols + c + 1) % (rows * cols);
            (i / cols, i % cols)
        }
        Move::Previous => {
            let i = (r * cols + c + rows * cols - 1) % (rows * cols);
            (i / cols, i % cols)
        }
        // Arrows stop at the edges.
        Move::Up => (r.saturating_sub(1), c),
        Move::Down => ((r + 1).min(rows - 1), c),
        Move::Left => (r, c.saturating_sub(1)),
        Move::Right => (r, (c + 1).min(cols - 1)),
    }
}

/// The key at logical position (x, y), as (row, column), in a window of
/// `width` by `height` logical pixels drawn at `scale`. It tests the same
/// pixel-snapped rectangles that are drawn, so a click on a key's edge pixel
/// presses that key.
pub fn hit_test(
    fonts: &Fonts,
    (width, height): (f32, f32),
    scale: f32,
    text_scale: f32,
    (x, y): (f32, f32),
) -> Option<(usize, usize)> {
    let layout = Layout::new(fonts, width, height, text_scale);
    let (x, y) = (x * scale, y * scale);
    for (row, labels) in KEYS.iter().enumerate() {
        for col in 0..labels.len() {
            let (kx, ky, kw, kh) = layout.key_pixels(row, col, scale);
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
        let layout = Layout::new(&fonts, w, h, 1.0);
        let (x, y, kw, kh) = layout.key_rect(0, 0);
        assert_eq!(
            hit_test(&fonts, (w, h), 1.0, 1.0, (x + 1.0, y + 1.0)),
            Some((0, 0))
        );
        let (x, y, ..) = layout.key_rect(4, 3);
        assert_eq!(
            hit_test(&fonts, (w, h), 1.0, 1.0, (x + 1.0, y + 1.0)),
            Some((4, 3))
        );
        // Between the first two keys of the first row.
        assert_eq!(
            hit_test(&fonts, (w, h), 1.0, 1.0, (PADDING + kw + GAP / 2.0, y - kh)),
            None
        );
        // The display area.
        assert_eq!(hit_test(&fonts, (w, h), 1.0, 1.0, (w / 2.0, PADDING)), None);
    }

    #[test]
    fn every_drawn_pixel_of_a_key_hits_that_key() {
        let fonts = Fonts::load();
        let (w, h) = (320.0, 480.0);
        let layout = Layout::new(&fonts, w, h, 1.0);
        for scale in [1.0, 1.25, 1.5, 1.75] {
            for (row, col) in [(0, 0), (2, 1), (4, 3)] {
                let (px, py, pw, ph) = layout.key_pixels(row, col, scale);
                // The four corner pixels of the drawn key, at their centres.
                for (x, y) in [
                    (px, py),
                    (px + pw - 1.0, py),
                    (px, py + ph - 1.0),
                    (px + pw - 1.0, py + ph - 1.0),
                ] {
                    let at = ((x + 0.5) / scale, (y + 0.5) / scale);
                    assert_eq!(
                        hit_test(&fonts, (w, h), scale, 1.0, at),
                        Some((row, col)),
                        "scale {scale}, pixel {x},{y}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_longest_number_fits_the_narrowest_window() {
        let fonts = Fonts::load();
        // 16 digits with separators and a sign, the most the engine shows
        // (exponent forms like -1.234567890123457e+19 are as long).
        let longest = "-9,999,999,999,999,999";
        for scale in [1.0, 1.5, 2.0] {
            let max = (260.0 - 2.0 * PADDING) * scale;
            let size = fonts.fit(
                Face::Light,
                DISPLAY_SIZE * scale,
                DISPLAY_MIN_SIZE * scale,
                longest,
                max,
            );
            assert!(
                fonts.width(Face::Light, size, longest) <= max,
                "scale {scale}"
            );
        }
    }

    #[test]
    fn every_button_presses_the_key_its_label_says() {
        for (row, labels) in KEYS.iter().enumerate() {
            for (col, label) in labels.iter().enumerate() {
                let key = action((row, col));
                assert_eq!(cell_of(key), Some((row, col)), "{label}");
                let expected = match *label {
                    "C" => Key::Clear,
                    "±" => Key::Sign,
                    "%" => Key::Percent,
                    "÷" => Key::Op(Op::Div),
                    "×" => Key::Op(Op::Mul),
                    "−" => Key::Op(Op::Sub),
                    "+" => Key::Op(Op::Add),
                    "." => Key::Point,
                    "⌫" => Key::Backspace,
                    "=" => Key::Equals,
                    digit => Key::Digit(digit.parse().unwrap()),
                };
                assert_eq!(key, expected, "{label}");
            }
        }
        assert_eq!(cell_of(Key::Op(Op::Pow)), None);
    }

    #[test]
    fn a_number_is_never_clipped_even_below_the_minimum_width() {
        let mut fonts = Fonts::load();
        let palette = Palette::default();
        let display = "-9,999,999,999,999,999";
        // Tiled to 180 px: narrower than MIN_SIZE allows.
        let (w, h) = (180usize, 480usize);
        let mut bytes = vec![0; w * h * 4];
        let view = View {
            expression: "",
            display,
            pressed: None,
            notice: None,
            focus: None,
            text_scale: 1.0,
        };
        draw(
            &mut Canvas::new(&mut bytes, w, h),
            &mut fonts,
            &palette,
            &view,
            (180.0, 480.0),
            1.0,
        );
        // Nothing of the result may touch the left padding.
        let padding = PADDING as usize;
        let display_rows = (PADDING as usize)..(480.0 * DISPLAY_SHARE) as usize;
        let bg = palette.background;
        for y in display_rows {
            for x in 0..padding {
                let p = &bytes[(y * w + x) * 4..(y * w + x) * 4 + 3];
                assert_eq!((p[2], p[1], p[0]), (bg.r, bg.g, bg.b), "ink at {x},{y}");
            }
        }
    }

    #[test]
    fn a_notice_replaces_the_expression_in_its_own_colour() {
        let mut fonts = Fonts::load();
        let palette = Palette::default();
        let (w, h) = (320usize, 480usize);
        let draw_with = |fonts: &mut Fonts, notice| {
            let mut bytes = vec![0; w * h * 4];
            let view = View {
                expression: "12 +",
                display: "12",
                pressed: None,
                notice,
                focus: None,
                text_scale: 1.0,
            };
            draw(
                &mut Canvas::new(&mut bytes, w, h),
                fonts,
                &palette,
                &view,
                (320.0, 480.0),
                1.0,
            );
            bytes
        };
        let has = |bytes: &[u8], c: Color| {
            bytes
                .chunks(4)
                .any(|p| (p[2], p[1], p[0]) == (c.r, c.g, c.b))
        };
        // The expression is drawn muted; with a notice, the notice colour shows in
        // the expression line's rows and the muted ink doesn't.
        let rows = |bytes: &[u8]| {
            let top = PADDING as usize * w * 4;
            let bottom = (h as f32 * DISPLAY_SHARE * 0.6) as usize * w * 4;
            bytes[top..bottom].to_vec()
        };
        let plain = rows(&draw_with(&mut fonts, None));
        let noted = rows(&draw_with(&mut fonts, Some("Copied")));
        assert!(has(&plain, palette.muted) && !has(&plain, palette.notice));
        assert!(has(&noted, palette.notice) && !has(&noted, palette.muted));
    }

    #[test]
    fn keypad_fits_inside_the_window() {
        let fonts = Fonts::load();
        for (w, h) in [(260.0, 400.0), (320.0, 480.0), (900.0, 1200.0)] {
            let layout = Layout::new(&fonts, w, h, 1.0);
            let (x, y, kw, kh) = layout.key_rect(4, 3);
            assert!(
                x + kw <= w - PADDING + 0.01 && y + kh <= h - PADDING + 0.01,
                "{w}x{h}"
            );
            assert!(layout.expression_baseline < layout.display_baseline);
        }
    }

    /// Every text colour against every fill it's drawn on, at 4.5:1.
    fn assert_aa(p: &Palette, theme: &str) {
        let check = |what: &str, ink: Color, ground: Color| {
            let c = contrast(ink, ground);
            assert!(
                c >= TEXT_CONTRAST,
                "{theme}: {what} is {c:.2}:1, needs 4.5:1"
            );
        };
        check("expression line", p.muted, p.background);
        check("Copied", p.notice, p.background);
        check("result", p.foreground, p.background);
        for (fill, name) in [(p.key, "key"), (p.key_pressed, "held key")] {
            check(&format!("digit on {name}"), p.foreground, fill);
            check(&format!("function label on {name}"), p.function, fill);
            check(&format!("operator on {name}"), p.operator, fill);
        }
        check("= label", p.on_accent, p.accent);
        check("= label held", p.on_accent, p.accent_pressed);
        // Non-text (WCAG 1.4.11, 2.4.7): the focus ring, 3:1 on the
        // background it's drawn on.
        let edge = |what: &str, c: Color| {
            let r = contrast(c, p.background);
            assert!(
                r >= NON_TEXT_CONTRAST,
                "{theme}: {what} is {r:.2}:1, needs 3:1"
            );
        };
        edge("focus ring", p.focus);
    }

    #[test]
    fn keys_are_at_least_44_px_at_the_minimum_window() {
        let fonts = Fonts::load();
        // MIN_SIZE (260 × 400), grown by the text scale as window.rs does.
        for text_scale in [0.75, 1.0, 1.5, 1.67, 2.0] {
            let k = f32::max(text_scale, 1.0);
            let (w, h) = ((260.0 * k).round(), (400.0 * k).round());
            let layout = Layout::new(&fonts, w, h, text_scale);
            let (_, _, kw, kh) = layout.key_rect(0, 0);
            assert!(
                kw >= MIN_KEY && kh >= MIN_KEY,
                "text {text_scale}: keys {kw}×{kh}"
            );
        }
    }

    #[test]
    fn larger_text_never_shrinks_keys_below_44_px() {
        let fonts = Fonts::load();
        // The window stays small while the setting goes up (Wayland apps
        // can't resize themselves).
        for (w, h) in [(260.0, 400.0), (320.0, 480.0)] {
            for text_scale in [1.0, 1.5, 2.0] {
                let layout = Layout::new(&fonts, w, h, text_scale);
                let (_, _, kw, kh) = layout.key_rect(4, 3);
                assert!(
                    kw >= MIN_KEY && kh >= MIN_KEY,
                    "{w}x{h} at {text_scale}: {kw}×{kh}"
                );
            }
        }
    }

    #[test]
    fn focus_moves_by_tab_and_arrows() {
        assert_eq!(move_focus(None, Move::Next), (0, 0));
        assert_eq!(
            move_focus(Some((0, 3)), Move::Next),
            (1, 0),
            "Tab wraps to the next row"
        );
        assert_eq!(
            move_focus(Some((4, 3)), Move::Next),
            (0, 0),
            "and back to the top"
        );
        assert_eq!(move_focus(Some((0, 0)), Move::Previous), (4, 3));
        assert_eq!(move_focus(Some((2, 1)), Move::Up), (1, 1));
        assert_eq!(
            move_focus(Some((0, 1)), Move::Up),
            (0, 1),
            "arrows stop at the edge"
        );
        assert_eq!(move_focus(Some((4, 3)), Move::Right), (4, 3));
        assert_eq!(move_focus(Some((4, 3)), Move::Left), (4, 2));
        assert_eq!(move_focus(Some((3, 0)), Move::Down), (4, 0));
    }

    #[test]
    fn focus_draws_a_ring_in_the_gap() {
        let mut fonts = Fonts::load();
        let palette = Palette::default();
        let (w, h) = (320usize, 480usize);
        let draw_with = |fonts: &mut Fonts, focus| {
            let mut bytes = vec![0; w * h * 4];
            let view = View {
                expression: "",
                display: "0",
                pressed: None,
                notice: None,
                focus,
                text_scale: 1.0,
            };
            draw(
                &mut Canvas::new(&mut bytes, w, h),
                fonts,
                &palette,
                &view,
                (320.0, 480.0),
                1.0,
            );
            bytes
        };
        let at = |bytes: &[u8], x: usize, y: usize| {
            let p = &bytes[(y * w + x) * 4..(y * w + x) * 4 + 3];
            Color::rgb(p[2], p[1], p[0])
        };
        let layout = Layout::new(&fonts, 320.0, 480.0, 1.0);
        let (kx, ky, _, kh) = layout.key_pixels(2, 1, 1.0);
        let plain = draw_with(&mut fonts, None);
        // The key is fill to its edge: no outline.
        let mid = (ky + kh / 2.0) as usize;
        assert_eq!(at(&plain, kx as usize, mid), palette.key);
        // Focus: the ring sits in the gap, 2 px out from the key, 2 px wide.
        let focused = draw_with(&mut fonts, Some((2, 1)));
        let ring_x = kx as usize - 3;
        assert_eq!(
            at(&plain, ring_x, mid),
            palette.background,
            "no ring without focus"
        );
        assert_eq!(at(&focused, ring_x, mid), palette.focus);
    }

    #[test]
    fn aliasos_themes_meet_aa_and_keep_their_colours() {
        let hex = |h: &str| Color::from_hex(h).unwrap();
        // The built-in palette and four themes from ~/.config/aliasos/themes.
        for (name, bg, fg, accent) in [
            ("built-in", "#1a1b26", "#c0caf5", "#7aa2f7"),
            ("tarnished", "#0b111a", "#c5ced6", "#5e92c6"),
            ("ember-night", "#081822", "#dce4e8", "#f0502a"),
            ("papercraft", "#1a1a1c", "#d8ccb8", "#d9692e"),
            ("night-city", "#0a0c10", "#d4e2e4", "#e8cf45"),
        ] {
            let p = Palette::new(hex(bg), hex(fg), hex(accent));
            assert_aa(&p, name);
            // On these themes the result, Copied and the expression line keep
            // their colours; operators and function labels may lift on the
            // keys, which are lighter than the background.
            assert_eq!(p.foreground, hex(fg), "{name}");
            assert_eq!(p.notice, hex(accent), "{name}");
            assert_eq!(p.muted, hex(bg).mix(hex(fg), 0.65), "{name}");
            // Under the dark = label these themes have, a held = lightens.
            assert!(
                p.accent_pressed.luminance() > p.accent.luminance(),
                "{name}"
            );
        }
        assert_eq!(Palette::default().muted, hex("#868dad"));
    }

    #[test]
    fn any_theme_meets_aa() {
        // xorshift, fixed seed: random dark, light and mid-tone themes.
        let mut x: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let mut color = || {
            let v = next();
            Color::rgb(v as u8, (v >> 8) as u8, (v >> 16) as u8)
        };
        for i in 0..20_000 {
            let (bg, fg, accent) = (color(), color(), color());
            assert_aa(
                &Palette::new(bg, fg, accent),
                &format!("theme {i} {bg:?} {fg:?} {accent:?}"),
            );
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
            notice: None,
            focus: None,
            text_scale: 1.0,
        };
        // Warm the glyph cache, as every frame after the first finds it.
        draw(
            &mut Canvas::new(&mut bytes, w, h),
            &mut fonts,
            &palette,
            &view,
            (320.0, 480.0),
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
                (320.0, 480.0),
                2.0,
            );
        }
        let per_frame = start.elapsed() / runs;
        println!("full frame at 640x960: {per_frame:?}");
        assert!(per_frame.as_micros() < 1000, "{per_frame:?}");
    }
}
