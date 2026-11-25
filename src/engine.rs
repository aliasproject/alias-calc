//! The calculator itself: keys in, expression and display text out. No UI.
//!
//! It behaves like omacalc (the keypad, precedence, percent and chaining
//! rules), with one big difference: numbers are exact decimals
//! (`rust_decimal`), so `0.1 + 0.2` is exactly `0.3` and money stays exact.
//! A decimal holds 28–29 significant digits up to about 7.9 × 10²⁸; past
//! that, or below 10⁻²⁸, a value falls back to `f64` so very large and very
//! small numbers still work (shown to 15 significant digits, as omacalc does).
//!
//! The display rounds exact values to 16 significant digits, so every
//! integer up to 9,999,999,999,999,999 shows exactly. A decimal keeps 28
//! places rather than 28 significant digits, so a ×, ÷ or ^ result below
//! 10⁻¹³ is worked out in `f64` instead and shows 15 digits. A fractional
//! power is approximate, so it stays an `f64` too, unless the answer is
//! exact (`4 ^ 0.5` is 2). Like C's `%g`, a value
//! switches to exponent form (`1.5e+20`, `1e-05`) when its exponent is below
//! −4 or at least the precision (16 for exact values, 15 for `f64`).

use std::fmt::Write as _;
use std::str::FromStr;

use rust_decimal::{Decimal, MathematicalOps};

/// Significant digits shown for an exact value, and the most that can be typed.
const EXACT_DIGITS: u32 = 16;
/// Significant digits shown for an `f64` value: what a double holds reliably.
const FLOAT_DIGITS: u32 = 15;
/// Significant digits every exact result except a division's is rounded to,
/// leaving guard digits below a decimal's 28 (see [`Num::combine`]).
const WORKING_DIGITS: u32 = 25;
/// The smallest power of ten a decimal's 28 places still hold to 16
/// significant digits (10⁻¹³ down to 10⁻²⁸).
const SMALLEST_FULL_EXPONENT: i32 = -13;

/// What the display says when a calculation has no answer.
pub const ERROR_TEXT: &str = "Error";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
}

impl Op {
    /// The sign shown in the expression line.
    pub fn symbol(self) -> &'static str {
        match self {
            Op::Add => "+",
            Op::Sub => "−",
            Op::Mul => "×",
            Op::Div => "÷",
            Op::Pow => "^",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// 0–9; anything larger is ignored.
    Digit(u8),
    Point,
    Op(Op),
    Equals,
    Percent,
    Sign,
    Backspace,
    Clear,
}

/// A number: exact when it fits a decimal, `f64` when it doesn't.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Num {
    Exact(Decimal),
    Float(f64),
}

/// A calculation with no answer: division by zero, overflow past `f64`, or a
/// negative number raised to a fractional power.
#[derive(Debug, PartialEq)]
struct NoAnswer;

type Answer = Result<Num, NoAnswer>;

impl Num {
    const ZERO: Num = Num::Exact(Decimal::ZERO);

    fn to_f64(self) -> f64 {
        match self {
            Num::Exact(d) => f64::from_str(&d.to_string()).unwrap_or(f64::NAN),
            Num::Float(f) => f,
        }
    }

    fn is_zero(self) -> bool {
        match self {
            Num::Exact(d) => d.is_zero(),
            Num::Float(f) => f == 0.0,
        }
    }

    /// An `f64` result. It stays approximate unless it's exactly what a
    /// decimal would hold: a whole number, or a value whose 15-digit text
    /// reads back as the very same `f64` (so `4 ^ 0.5` is exactly 2, and
    /// `0.25 ^ 0.5` is exactly 0.5). Anything else, like `2 ^ 0.5`, stays an
    /// `f64`, so arithmetic on it stays in `f64` and shows 15 digits, as
    /// omacalc does: `2 ^ 0.5 × 2 ^ 0.5` is 2, not 2.000000000000014.
    fn from_f64(value: f64) -> Answer {
        if !value.is_finite() {
            return Err(NoAnswer);
        }
        if value == 0.0 {
            return Ok(Num::ZERO);
        }
        if value.fract() == 0.0 {
            return Ok(match Decimal::from_f64_retain(value) {
                Some(d) => Num::Exact(d.normalize()),
                None => Num::Float(value),
            });
        }
        let text = format!("{:.*e}", FLOAT_DIGITS as usize - 1, value);
        let round_trips = f64::from_str(&text).is_ok_and(|back| back == value);
        match Decimal::from_scientific(&text) {
            // Below a decimal's 28 places the value would round away.
            Ok(d) if round_trips && !d.is_zero() && d.normalize().scale() <= 28 => {
                Ok(Num::Exact(d.normalize()))
            }
            _ => Ok(Num::Float(value)),
        }
    }

    /// Applies the exact operation when both sides are exact and the
    /// decimal holds the answer well; otherwise the `f64` one.
    ///
    /// `rounds`: round the result to [`WORKING_DIGITS`]. Division is the one
    /// exact operation that is itself inexact (1 ÷ 3 stops after 28 places),
    /// so its result keeps all 28 places, and every other result is rounded
    /// to 25 significant digits. The digits below that are guard digits:
    /// they absorb a division's last-place error, so 1 ÷ 3 × 3 is exactly 1
    /// and 1 ÷ 3 × 3 − 1 is exactly 0, not −1e-28. Rounding the division
    /// itself instead would keep its error (25 threes × 3 is 25 nines). 25
    /// digits is still far more than the 16 shown, so nothing a person can
    /// type or see is rounded by it.
    ///
    /// `loses_digits`: the operation can make more digits than it keeps
    /// (× and ÷). A decimal keeps 28 places, not 28 significant digits, so a
    /// result below 10⁻¹³ would show fewer than 16 digits, and one that
    /// rounds to zero would be lost. Those are redone in `f64`.
    fn combine(
        self,
        other: Num,
        exact: impl Fn(Decimal, Decimal) -> Option<Decimal>,
        float: impl Fn(f64, f64) -> f64,
        rounds: bool,
        loses_digits: bool,
    ) -> Answer {
        if let (Num::Exact(a), Num::Exact(b)) = (self, other)
            && let Some(result) = exact(a, b)
        {
            let result = if rounds {
                round_working(result)
            } else {
                result
            };
            let zero_ok = a.is_zero() || b.is_zero();
            if !loses_digits || keeps_full_precision(result, zero_ok) {
                return Ok(Num::Exact(result.normalize()));
            }
        }
        Num::from_f64(float(self.to_f64(), other.to_f64()))
    }

    fn apply(self, op: Op, other: Num) -> Answer {
        match op {
            Op::Add => self.combine(other, Decimal::checked_add, |a, b| a + b, true, false),
            Op::Sub => self.combine(other, Decimal::checked_sub, |a, b| a - b, true, false),
            Op::Mul => self.combine(other, Decimal::checked_mul, |a, b| a * b, true, true),
            Op::Div => {
                if other.is_zero() {
                    return Err(NoAnswer);
                }
                self.combine(other, Decimal::checked_div, |a, b| a / b, false, true)
            }
            Op::Pow => self.pow(other),
        }
    }

    fn pow(self, exponent: Num) -> Answer {
        let whole = match exponent {
            Num::Exact(e) => e.is_integer(),
            Num::Float(e) => e.fract() == 0.0,
        };
        let negative = match self {
            Num::Exact(d) => d.is_sign_negative() && !d.is_zero(),
            Num::Float(f) => f < 0.0,
        };
        if negative && !whole {
            return Err(NoAnswer);
        }
        if self.is_zero() && exponent.to_f64() < 0.0 {
            return Err(NoAnswer);
        }
        if let (Num::Exact(base), Num::Exact(e)) = (self, exponent)
            && whole
            && let Ok(e) = i64::try_from(e.mantissa() / 10i128.pow(e.scale()))
            && let Some(result) = base.checked_powi(e).map(round_working)
            && keeps_full_precision(result, base.is_zero())
        {
            return Ok(Num::Exact(result.normalize()));
        }
        Num::from_f64(self.to_f64().powf(exponent.to_f64()))
    }

    /// The display text, without thousands separators.
    fn format(self) -> String {
        match self {
            Num::Exact(d) => format_exact(d),
            Num::Float(f) => format_float(f),
        }
    }
}

/// Rounds a result to the working precision (see [`Num::combine`]).
fn round_working(value: Decimal) -> Decimal {
    value.round_sf(WORKING_DIGITS).unwrap_or(value)
}

/// Whether an exact result of ×, ÷ or ^ still has every digit the display
/// needs: it's at least 10⁻¹³, so 28 places hold 16 significant digits, or
/// it's zero and zero is the true answer.
fn keeps_full_precision(value: Decimal, zero_ok: bool) -> bool {
    if value.is_zero() {
        return zero_ok;
    }
    let normal = value.normalize();
    let digits = normal.mantissa().unsigned_abs().ilog10() as i32 + 1;
    // The first digit is worth 10^(digits − 1 − scale).
    digits - 1 - normal.scale() as i32 >= SMALLEST_FULL_EXPONENT
}

/// Rounds to 16 significant digits and lays the digits out.
fn format_exact(value: Decimal) -> String {
    if value.is_zero() {
        return "0".into();
    }
    let rounded = value.round_sf(EXACT_DIGITS).unwrap_or(value).normalize();
    let mantissa = rounded.mantissa();
    let digits = mantissa.unsigned_abs().to_string();
    // value = digits × 10^(−scale), so the first digit's power of ten is:
    let exponent = digits.len() as i32 - 1 - rounded.scale() as i32;
    layout(mantissa < 0, &digits, exponent, EXACT_DIGITS)
}

fn format_float(value: f64) -> String {
    if value == 0.0 {
        return "0".into();
    }
    // `{:e}` with 14 places rounds to 15 significant digits.
    let text = format!("{:.*e}", FLOAT_DIGITS as usize - 1, value);
    let (mantissa, exponent) = text.split_once('e').unwrap_or((&text, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let negative = mantissa.starts_with('-');
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    layout(negative, &digits, exponent, FLOAT_DIGITS)
}

/// Lays out significant `digits` whose first digit is worth 10^`exponent`,
/// `%g`-style: plain unless the exponent is below −4 or at least `precision`.
fn layout(negative: bool, digits: &str, exponent: i32, precision: u32) -> String {
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let mut out = String::with_capacity(digits.len() + 8);
    if negative {
        out.push('-');
    }
    if exponent < -4 || exponent >= precision as i32 {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let sign = if exponent < 0 { '-' } else { '+' };
        let _ = write!(out, "e{sign}{:02}", exponent.unsigned_abs());
    } else if exponent >= 0 {
        let whole = exponent as usize + 1;
        if digits.len() <= whole {
            out.push_str(digits);
            out.extend(std::iter::repeat_n('0', whole - digits.len()));
        } else {
            out.push_str(&digits[..whole]);
            out.push('.');
            out.push_str(&digits[whole..]);
        }
    } else {
        out.push_str("0.");
        out.extend(std::iter::repeat_n('0', (-exponent - 1) as usize));
        out.push_str(digits);
    }
    out
}

/// Separates the whole-number digits into groups of three with commas:
/// `1234567.891` reads `1,234,567.891`. The fraction and any exponent stay
/// as they are, and text that isn't a number (like "Error") passes through.
pub fn group_digits(number: &str) -> String {
    let start = usize::from(number.starts_with('-'));
    let end = start
        + number[start..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
    let count = end - start;
    if count <= 3 {
        return number.to_owned();
    }
    let mut out = String::with_capacity(number.len() + count / 3);
    out.push_str(&number[..start]);
    for (i, digit) in number[start..end].chars().enumerate() {
        if i > 0 && (count - i) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out.push_str(&number[end..]);
    out
}

/// Parses a plain number (`-12.5`, `.5`, `1e-05`, `1.5e+20`). Nothing else:
/// no separators, spaces, signs other than a leading `-`, or `inf`/`nan`.
fn parse_number(text: &str) -> Option<Num> {
    let body = text.strip_prefix('-').unwrap_or(text);
    let (mantissa, exponent) = match body.split_once(['e', 'E']) {
        Some((m, e)) => (m, Some(e)),
        None => (body, None),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits_ok = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty() && fraction.is_empty()
        || !digits_ok(whole)
        || !digits_ok(fraction)
        || mantissa.matches('.').count() > 1
    {
        return None;
    }
    if let Some(e) = exponent {
        let e = e.strip_prefix(['+', '-']).unwrap_or(e);
        if e.is_empty() || !digits_ok(e) {
            return None;
        }
    }

    let exact = if exponent.is_some() {
        Decimal::from_scientific(text).ok()
    } else {
        Decimal::from_str(text).ok()
    };
    match exact {
        Some(d) if !(d.is_zero() && text.bytes().any(|b| (b'1'..=b'9').contains(&b))) => {
            Some(Num::Exact(d))
        }
        _ => Num::from_f64(f64::from_str(text).ok()?).ok(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Token {
    Num(Num),
    Op(Op),
}

/// The calculator's state. Feed it keys with [`Calc::press`] and read
/// [`Calc::expression`] and [`Calc::display`] back.
#[derive(Debug, Default)]
pub struct Calc {
    /// Numbers and operators entered so far, always starting with a number.
    tokens: Vec<Token>,
    /// The number being typed, as typed ("5.", "-0", "1.50"), or a whole
    /// value in exponent form from a percent or paste.
    entry: String,
    /// After `=`: the exact result, its display text, and the expression.
    result: Option<(Num, String)>,
    evaluated: String,
    errored: bool,
}

impl Calc {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn press(&mut self, key: Key) {
        match key {
            Key::Digit(d) if d <= 9 => self.press_digit(char::from(b'0' + d)),
            Key::Digit(_) => {}
            Key::Point => self.press_point(),
            Key::Op(op) => self.press_op(op),
            Key::Equals => self.press_equals(),
            Key::Percent => self.press_percent(),
            Key::Sign => self.press_sign(),
            Key::Backspace => self.press_backspace(),
            Key::Clear => self.clear(),
        }
    }

    /// The line above the display: what's been entered, or after `=`, the
    /// calculation that gave the result. Empty at the start.
    pub fn expression(&self) -> String {
        if self.errored || self.result.is_some() {
            return self.evaluated.clone();
        }
        pretty(&self.tokens)
    }

    /// The big number, with thousands separators.
    pub fn display(&self) -> String {
        group_digits(&self.copy_text())
    }

    /// The display without separators: what copy puts on the clipboard, as
    /// a spreadsheet or terminal reads `1234567` more reliably than `1,234,567`.
    pub fn copy_text(&self) -> String {
        if self.errored {
            return ERROR_TEXT.into();
        }
        if !self.entry.is_empty() {
            return self.entry.clone();
        }
        if let Some((_, text)) = &self.result {
            return text.clone();
        }
        self.current_value().format()
    }

    /// Whether the display shows an error.
    #[allow(dead_code)] // Copy and paste use it (ALI-2102).
    pub fn is_error(&self) -> bool {
        self.errored
    }

    /// Replaces the number being typed with a pasted one. Accepts spaces
    /// around it, thousands separators in groups of three (`1,234,567.89`),
    /// a decimal comma (`1,5`) and the `−` this calculator shows. Anything
    /// else, including a lone comma at either end (`1,`, `,5`), is ignored,
    /// and the return value says whether it was taken. The number is shown
    /// as the display shows it, so one with more than 16 significant digits
    /// (a long integer, say) is rounded to 16.
    #[allow(dead_code)] // Copy and paste use it (ALI-2102).
    pub fn paste(&mut self, text: &str) -> bool {
        let mut text: String = text
            .trim()
            .replace('−', "-")
            .chars()
            .filter(|c| *c != ' ')
            .collect();
        let body = text.strip_prefix('-').unwrap_or(&text);
        if body.starts_with(',') || body.ends_with(',') {
            return false;
        }
        if is_grouped(&text) {
            text.retain(|c| c != ',');
        }
        let value = parse_number(&text).or_else(|| parse_number(&text.replace(',', ".")));
        let Some(value) = value else { return false };

        if self.errored || self.result.is_some() {
            self.clear();
        }
        self.entry = value.format();
        true
    }

    fn press_digit(&mut self, digit: char) {
        if self.errored || self.result.is_some() {
            // A digit after `=` starts a new calculation.
            self.clear();
        }
        if in_exponent_form(&self.entry) {
            self.entry.clear();
        }
        match self.entry.as_str() {
            "0" => {
                self.entry.clear();
                self.entry.push(digit);
            }
            "-0" => {
                self.entry.truncate(1);
                self.entry.push(digit);
            }
            _ if significant_digits(&self.entry) < EXACT_DIGITS as usize => {
                self.entry.push(digit);
            }
            // Past 16 significant digits the display would round what was typed.
            _ => {}
        }
    }

    fn press_point(&mut self) {
        if self.errored || self.result.is_some() {
            self.clear();
        }
        if in_exponent_form(&self.entry) {
            self.entry.clear();
        }
        if self.entry.is_empty() {
            self.entry.push_str("0.");
        } else if !self.entry.contains('.') {
            self.entry.push('.');
        }
    }

    fn press_op(&mut self, op: Op) {
        if self.errored {
            return;
        }
        if let Some((value, _)) = self.result.take() {
            // Chain from the exact result, not its rounded display, so
            // 1 ÷ 3 = × 3 comes back as exactly 1.
            self.tokens.clear();
            self.tokens.push(Token::Num(value));
            self.evaluated.clear();
        }
        if !self.entry.is_empty() {
            let value = seal(&self.entry);
            self.entry.clear();
            self.tokens.push(Token::Num(value));
            self.tokens.push(Token::Op(op));
        } else if self.tokens.is_empty() {
            self.tokens.push(Token::Num(Num::ZERO));
            self.tokens.push(Token::Op(op));
        } else if let Some(Token::Op(last)) = self.tokens.last_mut() {
            // A second operator in a row replaces the first: 4 + × 2 is 4 × 2.
            *last = op;
        } else {
            self.tokens.push(Token::Op(op));
        }
    }

    fn press_equals(&mut self) {
        if self.errored || self.result.is_some() {
            return;
        }
        let mut tokens = std::mem::take(&mut self.tokens);
        if !self.entry.is_empty() {
            tokens.push(Token::Num(seal(&self.entry)));
            self.entry.clear();
        }
        // A trailing operator is dropped: 9 + = is 9.
        while let Some(Token::Op(_)) = tokens.last() {
            tokens.pop();
        }
        if tokens.is_empty() {
            return;
        }
        self.evaluated = pretty(&tokens);
        match evaluate(&tokens) {
            Ok(value) => self.result = Some((value, value.format())),
            Err(NoAnswer) => self.errored = true,
        }
    }

    /// With a pending + or −, x % is x percent of the running total, so
    /// 200 + 10 % = 220. Otherwise (after ×, ÷ or ^, or on its own) it's x ÷ 100.
    fn press_percent(&mut self) {
        if self.errored {
            return;
        }
        if let Some((value, _)) = self.result.take() {
            self.evaluated.clear();
            match value.apply(Op::Div, Num::Exact(Decimal::ONE_HUNDRED)) {
                Ok(percent) => self.entry = percent.format(),
                Err(NoAnswer) => self.errored = true,
            }
            return;
        }

        let value = self.current_value();
        let hundred = Num::Exact(Decimal::ONE_HUNDRED);
        let percent = match self.tokens.last() {
            Some(Token::Op(Op::Add | Op::Sub)) => {
                let total = &self.tokens[..self.tokens.len() - 1];
                evaluate(total).and_then(|base| {
                    // Multiplying first keeps a tiny percentage exact; dividing
                    // first keeps 1e308 + 100 % from overflowing on the way.
                    base.apply(Op::Mul, value)
                        .and_then(|p| p.apply(Op::Div, hundred))
                        .or_else(|_| base.apply(Op::Mul, value.apply(Op::Div, hundred)?))
                })
            }
            _ => value.apply(Op::Div, hundred),
        };
        match percent {
            Ok(percent) => self.entry = percent.format(),
            Err(NoAnswer) => {
                let mut shown = self.tokens.clone();
                shown.push(Token::Num(value));
                self.evaluated = format!("{} %", pretty(&shown));
                self.errored = true;
                self.tokens.clear();
                self.entry.clear();
            }
        }
    }

    fn press_sign(&mut self) {
        if self.errored {
            return;
        }
        if self.result.is_some() {
            self.edit_result();
        }
        // With nothing typed, start a fresh negative number: 4 + ± 2 is 4 + -2.
        if self.entry.is_empty() {
            self.entry.push_str("-0");
        } else if self.entry.starts_with('-') {
            self.entry.remove(0);
        } else {
            self.entry.insert(0, '-');
        }
    }

    fn press_backspace(&mut self) {
        if self.errored {
            self.clear();
            return;
        }
        if self.result.is_some() {
            self.edit_result();
        }
        if in_exponent_form(&self.entry) {
            // Backspace on 1e+15 would leave 1e+1, which is 10: clear it.
            self.entry.clear();
            self.entry.push('0');
            return;
        }
        self.entry.pop();
        if self.entry == "-" {
            self.entry.clear();
        }
    }

    fn clear(&mut self) {
        self.tokens.clear();
        self.entry.clear();
        self.result = None;
        self.evaluated.clear();
        self.errored = false;
    }

    /// Editing after `=` starts from the result's digits, with the old
    /// expression cleared.
    fn edit_result(&mut self) {
        if let Some((_, text)) = self.result.take() {
            self.entry = text;
        }
        self.tokens.clear();
        self.evaluated.clear();
    }

    /// The number the calculator is at: the one being typed, else the last
    /// one entered, else zero.
    fn current_value(&self) -> Num {
        if !self.entry.is_empty() {
            return seal(&self.entry);
        }
        self.tokens
            .iter()
            .rev()
            .find_map(|t| match t {
                Token::Num(n) => Some(*n),
                Token::Op(_) => None,
            })
            .unwrap_or(Num::ZERO)
    }
}

/// Turns a half-typed entry ("5.", "-", "-0") into a number.
fn seal(entry: &str) -> Num {
    let entry = entry.strip_suffix('.').unwrap_or(entry);
    if entry.is_empty() || entry == "-" {
        return Num::ZERO;
    }
    parse_number(entry).unwrap_or(Num::ZERO)
}

fn in_exponent_form(entry: &str) -> bool {
    entry.contains('e')
}

/// Significant digits typed so far; the zero in a leading "0." doesn't count.
fn significant_digits(entry: &str) -> usize {
    let digits = entry.bytes().filter(u8::is_ascii_digit).count();
    let leading_zero = entry.starts_with("0.") || entry.starts_with("-0.");
    digits - usize::from(leading_zero)
}

/// `1,234` or `-1,234,567.89`: commas exactly every three digits.
#[allow(dead_code)] // Only `paste` uses it so far.
fn is_grouped(text: &str) -> bool {
    let body = text.strip_prefix('-').unwrap_or(text);
    let whole = body.split_once('.').map_or(body, |(w, _)| w);
    let fraction_ok = body
        .split_once('.')
        .is_none_or(|(_, f)| f.bytes().all(|b| b.is_ascii_digit()));
    let mut groups = whole.split(',');
    let Some(first) = groups.next() else {
        return false;
    };
    let first_ok = (1..=3).contains(&first.len()) && first.bytes().all(|b| b.is_ascii_digit());
    let mut rest = groups.peekable();
    let has_rest = rest.peek().is_some();
    first_ok
        && has_rest
        && fraction_ok
        && rest.all(|g| g.len() == 3 && g.bytes().all(|b| b.is_ascii_digit()))
}

/// The expression line: every number at display precision, grouped.
fn pretty(tokens: &[Token]) -> String {
    let mut out = String::new();
    for token in tokens {
        if !out.is_empty() {
            out.push(' ');
        }
        match token {
            Token::Num(n) => out.push_str(&group_digits(&n.format())),
            Token::Op(op) => out.push_str(op.symbol()),
        }
    }
    out
}

/// Evaluates number, operator, number, …: `^` first and right to left
/// (2 ^ 3 ^ 2 is 2 ^ 9), then × and ÷, then + and −, each left to right.
fn evaluate(tokens: &[Token]) -> Answer {
    let mut values = Vec::with_capacity(tokens.len() / 2 + 1);
    let mut ops = Vec::with_capacity(tokens.len() / 2);
    for (i, token) in tokens.iter().enumerate() {
        match (i % 2, token) {
            (0, Token::Num(n)) => values.push(*n),
            (1, Token::Op(op)) => ops.push(*op),
            _ => return Err(NoAnswer),
        }
    }
    if values.len() != ops.len() + 1 {
        return Err(NoAnswer);
    }

    for i in (0..ops.len()).rev() {
        if ops[i] == Op::Pow {
            values[i] = values[i].pow(values[i + 1])?;
            values.remove(i + 1);
            ops.remove(i);
        }
    }

    let mut terms = vec![values[0]];
    let mut signs = Vec::new();
    for (op, value) in ops.iter().zip(&values[1..]) {
        match op {
            Op::Mul | Op::Div => {
                let last = terms.last_mut().ok_or(NoAnswer)?;
                *last = last.apply(*op, *value)?;
            }
            _ => {
                signs.push(*op);
                terms.push(*value);
            }
        }
    }
    let mut total = terms[0];
    for (op, term) in signs.iter().zip(&terms[1..]) {
        total = total.apply(*op, *term)?;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Presses space-separated keys, the way omacalc's tests do.
    fn press(calc: &mut Calc, keys: &str) {
        for key in keys.split_whitespace() {
            let key = match key {
                "+" => Key::Op(Op::Add),
                "-" | "−" => Key::Op(Op::Sub),
                "×" | "*" => Key::Op(Op::Mul),
                "÷" | "/" => Key::Op(Op::Div),
                "^" => Key::Op(Op::Pow),
                "=" => Key::Equals,
                "%" => Key::Percent,
                "." => Key::Point,
                "sign" => Key::Sign,
                "backspace" => Key::Backspace,
                "clear" => Key::Clear,
                d if d.len() == 1 && d.as_bytes()[0].is_ascii_digit() => {
                    Key::Digit(d.as_bytes()[0] - b'0')
                }
                other => panic!("unknown key {other}"),
            };
            calc.press(key);
        }
    }

    fn shows(keys: &str) -> String {
        let mut calc = Calc::new();
        press(&mut calc, keys);
        calc.display()
    }

    #[test]
    fn starts_at_zero() {
        let calc = Calc::new();
        assert_eq!(calc.display(), "0");
        assert_eq!(calc.expression(), "");
    }

    #[test]
    fn readme_examples() {
        let mut calc = Calc::new();
        press(&mut calc, "4 2 × 3 + 7 =");
        assert_eq!(calc.display(), "133");
        assert_eq!(calc.expression(), "42 × 3 + 7");
        assert_eq!(shows("2 × 3 ^ 2 ="), "18");
        assert_eq!(shows("2 0 0 + 1 0 % ="), "220");
        assert_eq!(shows("1 2 3 4 5 6 7 . 8 9"), "1,234,567.89");
    }

    #[test]
    fn exact_decimals() {
        assert_eq!(shows("0 . 1 + 0 . 2 ="), "0.3");
        assert_eq!(shows(". 1 × 3 ="), "0.3");
        assert_eq!(shows("1 . 1 × 1 . 1 ="), "1.21");
        assert_eq!(shows("1 ÷ 3 ="), "0.3333333333333333");
        assert_eq!(shows("2 ÷ 3 ="), "0.6666666666666667");
        // 16 digits exactly, where omacalc's doubles show 9.89999999999999e+15.
        assert_eq!(
            shows("9 9 9 9 9 9 9 9 9 9 9 9 9 9 × 9 9 ="),
            "9,899,999,999,999,901"
        );
        assert_eq!(shows("1 0 . 0 5 - 1 0 ="), "0.05");
    }

    #[test]
    fn powers() {
        assert_eq!(shows("2 ^ 2 ="), "4");
        assert_eq!(shows("2 × 3 ^ 2 ="), "18");
        assert_eq!(shows("2 ^ 3 × 2 ="), "16");
        assert_eq!(shows("2 ^ 3 ^ 2 ="), "512");
        assert_eq!(shows("2 ^ 0 ="), "1");
        assert_eq!(shows("2 ^ 3 = ^ 2 ="), "64");
        assert_eq!(shows("2 sign ^ 3 ="), "-8");
        assert_eq!(shows("0 ^ 0 ="), "1");
        assert_eq!(shows("0 ^ 1 sign ="), "Error");
        assert_eq!(shows("2 sign ^ 0 . 5 ="), "Error");
        assert_eq!(shows("2 ^ 1 sign ="), "0.5");
        assert_eq!(shows("4 ^ 0 . 5 ="), "2");
        assert_eq!(shows("2 ^ 0 . 1 ="), "1.07177346253629");
        // Past a decimal's range, powers fall back to f64.
        assert_eq!(shows("2 ^ 1 0 0 ="), "1.26765060022823e+30");
        assert_eq!(shows("1 0 ^ 3 0 8 ="), "1e+308");
        assert_eq!(shows("1 0 ^ 3 0 9 ="), "Error");
        assert_eq!(shows(". 1 ^ 4 0 ="), "1e-40");
    }

    /// A decimal keeps 28 places, not 28 significant digits: small results
    /// of ×, ÷ and ^ are worked out in f64 so they keep their digits.
    #[test]
    fn small_results_keep_their_digits() {
        assert_eq!(
            shows("1 ÷ 3 0 0 0 0 0 0 0 0 0 0 0 0 0 0 ="),
            format_float(1.0 / 300000000000000.0)
        );
        assert_eq!(
            shows("1 ÷ 3 0 0 0 0 0 0 0 0 0 0 0 0 0 0 ="),
            "3.33333333333333e-15"
        );
        assert_eq!(shows(". 7 ^ 1 0 0 ="), format_float(0.7f64.powf(100.0)));
        assert_eq!(shows(". 7 ^ 1 0 0 =").len(), "3.23447650962474e-16".len());
        // Exact small values that fit stay exact.
        assert_eq!(shows(". 0 0 0 1 × . 0 0 0 1 ="), "1e-08");
        assert_eq!(shows("1 ÷ 8 ="), "0.125");
    }

    /// Guard digits absorb a division's last-place error.
    #[test]
    fn division_residue_cancels() {
        assert_eq!(shows("1 ÷ 3 × 3 - 1 ="), "0");
        assert_eq!(shows("2 ÷ 3 × 3 - 2 ="), "0");
        assert_eq!(shows("1 ÷ 3 = × 3 = - 1 ="), "0");
        assert_eq!(shows("1 ÷ 7 × 7 - 1 ="), "0");
        assert_eq!(shows("1 0 ÷ 3 × 3 - 1 0 ="), "0");
        assert_eq!(shows("1 ÷ 3 = × 3 ="), "1");
        assert_eq!(shows("0 . 1 + 0 . 2 ="), "0.3");
        assert_eq!(shows("0 . 1 + 0 . 2 - 0 . 3 ="), "0");
    }

    /// A fractional power is approximate, so arithmetic on it stays in f64.
    #[test]
    fn fractional_powers_stay_approximate() {
        assert_eq!(shows("2 ^ . 5 × 2 ^ . 5 ="), "2");
        let cubed = 10f64.powf(0.3) * 10f64.powf(0.3) * 10f64.powf(0.3);
        assert_eq!(
            shows("1 0 ^ . 3 × 1 0 ^ . 3 × 1 0 ^ . 3 ="),
            format_float(cubed)
        );
        assert_eq!(shows("4 ^ . 5 ="), "2");
        assert_eq!(shows("9 ^ . 5 ="), "3");
        assert_eq!(shows(". 2 5 ^ . 5 ="), "0.5");
        // An exact power result stays exact in later steps.
        assert_eq!(shows("4 ^ . 5 + 0 . 1 ="), "2.1");
    }

    #[test]
    fn error_then_digit_starts_fresh() {
        let mut calc = Calc::new();
        press(&mut calc, "2 sign ^ 0 . 5 =");
        assert!(calc.is_error());
        press(&mut calc, "5");
        assert_eq!(calc.display(), "5");
        assert!(!calc.is_error());
    }

    #[test]
    fn precedence() {
        assert_eq!(shows("2 + 3 × 4 ="), "14");
        assert_eq!(shows("1 0 - 4 ÷ 2 ="), "8");
        assert_eq!(shows("1 0 - 2 - 3 ="), "5");
        assert_eq!(shows("1 2 ÷ 2 ÷ 3 ="), "2");
    }

    #[test]
    fn shows_entry_while_typing() {
        let mut calc = Calc::new();
        press(&mut calc, "4 2 ×");
        assert_eq!(calc.display(), "42");
        assert_eq!(calc.expression(), "42 ×");
        press(&mut calc, "3");
        assert_eq!(calc.display(), "3");
    }

    #[test]
    fn decimals() {
        assert_eq!(shows(". 5 + . 2 5 ="), "0.75");
        assert_eq!(shows("1 . 5 . 5"), "1.55");
        // What was typed stays as typed until it's used.
        assert_eq!(shows("1 . 5 0"), "1.50");
        assert_eq!(shows("5 ."), "5.");
        assert_eq!(shows("5 . + 1 ="), "6");
    }

    #[test]
    fn division_by_zero() {
        let mut calc = Calc::new();
        press(&mut calc, "1 ÷ 0 =");
        assert_eq!(calc.display(), "Error");
        assert_eq!(calc.copy_text(), "Error");
        assert_eq!(calc.expression(), "1 ÷ 0");
        press(&mut calc, "5");
        assert_eq!(calc.display(), "5");
    }

    #[test]
    fn keys_after_error() {
        let mut calc = Calc::new();
        press(&mut calc, "1 ÷ 0 = + sign %");
        assert_eq!(calc.display(), "Error");
        press(&mut calc, "backspace");
        assert_eq!(calc.display(), "0");
        press(&mut calc, "1 ÷ 0 = .");
        assert_eq!(calc.display(), "0.");
    }

    #[test]
    fn percent() {
        assert_eq!(shows("2 0 0 + 1 0 % ="), "220");
        assert_eq!(shows("2 0 0 - 1 0 % ="), "180");
        assert_eq!(shows("2 0 0 × 1 0 % ="), "20");
        assert_eq!(shows("2 0 0 ÷ 1 0 % ="), "2,000");
        assert_eq!(shows("2 ^ 1 0 % ="), "1.07177346253629");
        assert_eq!(shows("5 0 %"), "0.5");
        assert_eq!(shows("4 0 + 1 0 = %"), "0.5");
        // The running total includes everything before the + or −.
        assert_eq!(shows("1 0 0 + 1 0 0 + 1 0 % ="), "220");
        assert_eq!(shows("1 0 × 1 0 + 5 % ="), "105");
    }

    #[test]
    fn percent_of_a_huge_total() {
        assert_eq!(shows("1 0 ^ 3 0 8 = - 1 0 0 % ="), "0");
        let mut calc = Calc::new();
        press(&mut calc, "1 0 ^ 3 0 8 = + 1 0 0 0 %");
        assert_eq!(calc.display(), "Error");
        assert_eq!(calc.expression(), "1e+308 + 1,000 %");
    }

    #[test]
    fn sign() {
        let mut calc = Calc::new();
        press(&mut calc, "8 sign");
        assert_eq!(calc.display(), "-8");
        press(&mut calc, "sign");
        assert_eq!(calc.display(), "8");
        assert_eq!(shows("4 + 8 sign ="), "-4");
    }

    #[test]
    fn sign_starts_a_new_operand() {
        let mut calc = Calc::new();
        press(&mut calc, "4 + sign 2 =");
        assert_eq!(calc.display(), "2");
        assert_eq!(calc.expression(), "4 + -2");
        let mut calc = Calc::new();
        press(&mut calc, "sign");
        assert_eq!(calc.display(), "-0");
        press(&mut calc, "5");
        assert_eq!(calc.display(), "-5");
    }

    #[test]
    fn sign_after_result_edits_it() {
        let mut calc = Calc::new();
        press(&mut calc, "6 × 7 = sign");
        assert_eq!(calc.display(), "-42");
        assert_eq!(calc.expression(), "");
        press(&mut calc, "+ 2 =");
        assert_eq!(calc.display(), "-40");
    }

    #[test]
    fn chains_with_full_precision() {
        assert_eq!(shows("1 ÷ 3 = × 3 ="), "1");
        assert_eq!(shows("2 ÷ 3 = × 3 ="), "2");
        assert_eq!(shows("9 9 9 9 9 9 9 9 9 9 9 9 9 9 ="), "99,999,999,999,999");
    }

    #[test]
    fn caps_entry_at_sixteen_digits() {
        assert_eq!(
            shows("1 2 3 4 5 6 7 8 9 1 2 3 4 5 6 7 8"),
            "1,234,567,891,234,567"
        );
        assert_eq!(
            shows(". 1 2 3 4 5 6 7 8 9 1 2 3 4 5 6 7 8"),
            "0.1234567891234567"
        );
        assert_eq!(
            shows("sign 1 2 3 4 5 6 7 8 9 1 2 3 4 5 6 7 8"),
            "-1,234,567,891,234,567"
        );
    }

    #[test]
    fn backspace() {
        let mut calc = Calc::new();
        press(&mut calc, "1 2 3 backspace");
        assert_eq!(calc.display(), "12");
        press(&mut calc, "backspace backspace backspace");
        assert_eq!(calc.display(), "0");
        assert_eq!(shows("5 sign backspace"), "0");
        assert_eq!(shows("6 × 7 = backspace"), "4");
    }

    #[test]
    fn exponent_form_is_a_whole_value() {
        assert_eq!(shows(". 0 0 1 % 5"), "5");
        assert_eq!(shows(". 0 0 1 % ."), "0.");
        assert_eq!(shows(". 0 0 0 0 1 %"), "1e-07");
        assert_eq!(
            shows("1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 × 1 0 0 = backspace"),
            "0"
        );
        assert_eq!(shows(". 0 0 0 0 1 % sign × 2 ="), "-2e-07");
        assert_eq!(shows("2 + . 0 0 0 0 1 % 5 ="), "7");
        assert_eq!(shows("2 + . 0 0 0 0 1 % . 5 ="), "2.5");
        let mut calc = Calc::new();
        press(&mut calc, "2 + . 0 0 0 0 1 % backspace");
        assert_eq!(calc.display(), "0");
        assert_eq!(calc.expression(), "2 +");
    }

    #[test]
    fn chains_from_result() {
        let mut calc = Calc::new();
        press(&mut calc, "6 × 7 = × 2 =");
        assert_eq!(calc.display(), "84");
        assert_eq!(calc.expression(), "42 × 2");
        press(&mut calc, "9");
        assert_eq!(calc.display(), "9");
        assert_eq!(calc.expression(), "");
    }

    #[test]
    fn equals_twice_does_nothing() {
        assert_eq!(shows("2 + 3 = ="), "5");
        assert_eq!(shows("="), "0");
    }

    #[test]
    fn replaces_dangling_operator() {
        assert_eq!(shows("4 + × 2 ="), "8");
        assert_eq!(shows("9 + ="), "9");
        let mut calc = Calc::new();
        press(&mut calc, "×");
        assert_eq!(calc.expression(), "0 ×");
    }

    #[test]
    fn pastes_numbers() {
        let mut calc = Calc::new();
        assert!(calc.paste(" 42.5 "));
        assert_eq!(calc.display(), "42.5");
        press(&mut calc, "+ . 5 =");
        assert_eq!(calc.display(), "43");
        assert!(calc.paste("1,5"));
        assert_eq!(calc.display(), "1.5");
        assert!(!calc.paste("not a number"));
        assert_eq!(calc.display(), "1.5");
        assert!(calc.paste("1,234,567.89"));
        assert_eq!(calc.display(), "1,234,567.89");
        assert!(calc.paste("−12,000"));
        assert_eq!(calc.display(), "-12,000");
        assert!(calc.paste("1e-9"));
        assert_eq!(calc.display(), "1e-09");
        for junk in [
            "", "-", ".", "1.2.3", "1,2,3", "inf", "NaN", "1e", "--1", "+1", "0x10",
        ] {
            assert!(!calc.paste(junk), "{junk:?} should be refused");
        }
    }

    #[test]
    fn paste_refuses_a_lone_end_comma() {
        let mut calc = Calc::new();
        for junk in ["1,", ",5", "5,", "-,5", "-5,", ",", "1,234,"] {
            assert!(!calc.paste(junk), "{junk:?} should be refused");
        }
        assert_eq!(calc.display(), "0");
        assert!(calc.paste("12,34"));
        assert_eq!(calc.display(), "12.34");
        assert!(calc.paste("1,234"));
        assert_eq!(calc.display(), "1,234");
        assert!(calc.paste("12345678901234567890"));
        assert_eq!(calc.display(), "1.234567890123457e+19");
    }

    #[test]
    fn paste_replaces_a_result() {
        let mut calc = Calc::new();
        press(&mut calc, "1 ÷ 0 =");
        assert!(calc.paste("7"));
        assert_eq!(calc.display(), "7");
        assert_eq!(calc.expression(), "");
    }

    #[test]
    fn groups_thousands() {
        let mut calc = Calc::new();
        press(&mut calc, "1 2 3 4");
        assert_eq!(calc.display(), "1,234");
        press(&mut calc, ". 5 6 7 8");
        assert_eq!(calc.display(), "1,234.5678");
        press(&mut calc, "× 1 0 0 0 =");
        assert_eq!(calc.display(), "1,234,567.8");
        assert_eq!(calc.expression(), "1,234.5678 × 1,000");
        press(&mut calc, "+ 1 =");
        assert_eq!(calc.display(), "1,234,568.8");
        press(&mut calc, "backspace backspace");
        assert_eq!(calc.display(), "1,234,568");
        assert_eq!(shows("1 0 0 0 0 0 0 sign"), "-1,000,000");
    }

    #[test]
    fn copies_without_separators() {
        let mut calc = Calc::new();
        press(&mut calc, "1 2 3 4 5 6 7 . 5");
        assert_eq!(calc.copy_text(), "1234567.5");
    }

    #[test]
    fn group_digits_cases() {
        assert_eq!(group_digits("0"), "0");
        assert_eq!(group_digits("999"), "999");
        assert_eq!(group_digits("1000"), "1,000");
        assert_eq!(group_digits("-100000"), "-100,000");
        assert_eq!(group_digits("0.123456"), "0.123456");
        assert_eq!(group_digits("1.5e+20"), "1.5e+20");
        assert_eq!(group_digits("Error"), "Error");
        assert_eq!(group_digits("-0"), "-0");
    }

    #[test]
    fn formats_numbers() {
        let exact = |s: &str| Num::Exact(Decimal::from_str(s).unwrap()).format();
        assert_eq!(exact("133"), "133");
        assert_eq!(exact("-0"), "0");
        assert_eq!(exact("0.0001"), "0.0001");
        assert_eq!(exact("0.00001"), "1e-05");
        assert_eq!(exact("1000000000000000"), "1000000000000000");
        assert_eq!(exact("10000000000000000"), "1e+16");
        assert_eq!(exact("12345678901234567"), "1.234567890123457e+16");
        assert_eq!(exact("1.50"), "1.5");
        assert_eq!(exact("-2.5"), "-2.5");
        assert_eq!(format_float(1e15), "1e+15");
        assert_eq!(format_float(123456789012345.0), "123456789012345");
        assert_eq!(format_float(-1.5e300), "-1.5e+300");
        assert_eq!(format_float(0.1 + 0.2), "0.3");
    }

    #[test]
    fn evaluate_tokens() {
        let n = |v: i64| Token::Num(Num::Exact(Decimal::from(v)));
        let t = [n(2), Token::Op(Op::Add), n(3), Token::Op(Op::Mul), n(4)];
        assert_eq!(evaluate(&t), Ok(Num::Exact(Decimal::from(14))));
        let t = [n(2), Token::Op(Op::Pow), n(3), Token::Op(Op::Pow), n(2)];
        assert_eq!(evaluate(&t), Ok(Num::Exact(Decimal::from(512))));
        assert_eq!(evaluate(&[n(1), Token::Op(Op::Add)]), Err(NoAnswer));
        assert_eq!(evaluate(&[]), Err(NoAnswer));
    }
}
