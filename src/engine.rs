//! The calculator itself: keys in, expression and display text out. No UI.
//!
//! It behaves like omacalc (the keypad, precedence, percent and chaining
//! rules), with one big difference: numbers are exact decimals
//! (`rust_decimal`), so `0.1 + 0.2` is exactly `0.3` and money stays exact.
//! A decimal holds 28–29 significant digits up to about 7.9 × 10²⁸; past
//! that, or below 10⁻²⁸, a value falls back to `f64` so very large and very
//! small numbers still work (shown to 15 significant digits, as omacalc does).
//!
//! The display rounds decimal values to 16 significant digits, so every
//! integer up to 9,999,999,999,999,999 shows exactly.
//!
//! # How exact a value is
//!
//! rust_decimal rounds silently: `checked_add`, `checked_mul` and the rest
//! don't fail when a result needs more than 28 places or 96 bits, and
//! `from_str` rounds a long number the same way. So whether a value is exact
//! is decided by comparing it with the true value, worked out in 256 bits
//! (see [`Value`]), never by undoing an operation with another rounding one.
//!
//! Every value carries one of three levels, and a result takes the highest
//! level among its inputs and its operation:
//!
//! * **Exact**: what was typed or pasted (when a decimal holds it in full),
//!   and any + − × or whole power ≥ 0 of exact values whose true result a
//!   decimal holds, including a division that ends (1 ÷ 4, 1 ÷ 100000000).
//!   Exact values are never rounded and never snapped, so
//!   `1000000000 + 0.000000001 − 1000000000` is 1e-09 and
//!   `8403911115566009 + 2.4118675 − 8403911115566009` is 2.4118675.
//! * **Guarded**: a division that doesn't end (1 ÷ 3), a negative power (a
//!   reciprocal), any operation the decimal had to round, and anything worked
//!   out from those. A guarded value keeps every digit the decimal holds, at
//!   least 26 significant ones (see below), so each rounding is off by at
//!   most about 10⁻²⁶ of the value. A guarded sum or difference that's at
//!   most 10⁻²⁴ of its larger operand is that residue, and is 0: so
//!   1 ÷ 3 × 3 − 1, 1 ÷ 3 ÷ 3 × 3 × 3 − 1 and 34 ÷ 7.67 + 9.3 ÷ 7.67 −
//!   43.3 ÷ 7.67 are exactly 0, while a real difference like
//!   1 ÷ 3 − 0.333333333333333333 (3 × 10⁻¹⁹) is kept.
//! * **Approx**: anything that went through `f64` (a fractional power, a
//!   rounded value too small to hold 26 digits, a value out of a decimal's
//!   range), even when it's turned back into a decimal, and anything worked
//!   out from it. It's shown to 15 digits when it's an `f64`, and a sum or
//!   difference that's at most 10⁻¹⁵ of its larger operand is 0, which
//!   clears an `f64`'s last-place error but keeps a real difference like
//!   `2 ^ 0.5 − 1.41421356237309`.
//!
//! A decimal keeps 28 places rather than 28 significant digits, so a small
//! value holds few digits. A rounded product, power or quotient below 10⁻³
//! (fewer than 26 digits in 28 places) is worked out in `f64` instead, and
//! so becomes Approx. A fractional power is worked out in `f64` too;
//! `4 ^ 0.5` is still shown as 2.
//!
//! The one remaining limit is a sum of exact values whose true result needs
//! more than 28 digits: `1000000000000000 + 0.0000000000000001` can't be
//! held, so it's rounded and becomes Guarded, and taking 1000000000000000
//! back off gives 0, as omacalc's double does. Fixing that would take a
//! wider number type.
//!
//! Like C's `%g`, a value switches to exponent form (`1.5e+20`, `1e-05`)
//! when its exponent is below −4 or at least the precision (16 for decimal
//! values, 15 for `f64`).

use std::fmt::Write as _;
use std::str::FromStr;

use rust_decimal::Decimal;

/// Significant digits shown for a decimal value, and the most that can be typed.
const EXACT_DIGITS: u32 = 16;
/// Significant digits shown for an `f64` value: what a double holds reliably.
const FLOAT_DIGITS: u32 = 15;
/// The fewest significant digits a rounded product or quotient may hold and
/// stay a decimal (see [`settle`]). Its rounding error is then at most
/// 5 × 10⁻²⁶ of it, far enough below the 10⁻²⁴ snap that a chain of rounded
/// steps still cancels to 0.
const GUARDED_DIGITS: u32 = 26;
/// A decimal keeps this many places after the point, whatever its size.
const DECIMAL_PLACES: i32 = 28;
/// The smallest power of ten whose value 28 places hold to 26 significant
/// digits: 10⁻³.
const SMALLEST_GUARDED_EXPONENT: i32 = GUARDED_DIGITS as i32 - 1 - DECIMAL_PLACES;
/// Cancellation snapping for a guarded sum or difference: this small
/// relative to its larger operand, it's residue from rounding, so it's 0.
/// A 28-digit value leaves about 10⁻²⁷ a step, and a real difference like
/// `1 ÷ 3 − 0.333333333333333333` (3 × 10⁻¹⁹) is far above it.
const GUARDED_CANCELLATION: Decimal = Decimal::from_parts(1, 0, 0, false, 24);
/// The same for an approximate one. An `f64`'s own error is about 10⁻¹⁶, so
/// a few units in the last place snap, but a real difference against a
/// number typed to 15 digits (`2 ^ 0.5 − 1.41421356237309`, 3.6 × 10⁻¹⁵ of
/// it) is kept, as omacalc shows it.
const APPROX_CANCELLATION: f64 = 1e-15;

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

/// How exact a value is (see the module docs). Ordered, so a result's level
/// is the `max` of its inputs' and its operation's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Level {
    Exact,
    Guarded,
    Approx,
}

/// A number: a decimal with its [`Level`], or an `f64` (always Approx)
/// when a decimal can't hold it.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Num {
    Dec(Decimal, Level),
    Float(f64),
}

/// A calculation with no answer: division by zero, overflow past `f64`, or a
/// negative number raised to a fractional power.
#[derive(Debug, PartialEq)]
struct NoAnswer;

type Answer = Result<Num, NoAnswer>;

impl Num {
    const ZERO: Num = Num::Dec(Decimal::ZERO, Level::Exact);

    fn exact(value: Decimal) -> Num {
        Num::Dec(value, Level::Exact)
    }

    fn level(self) -> Level {
        match self {
            Num::Dec(_, level) => level,
            Num::Float(_) => Level::Approx,
        }
    }

    fn to_f64(self) -> f64 {
        match self {
            Num::Dec(d, _) => f64::from_str(&d.to_string()).unwrap_or(f64::NAN),
            Num::Float(f) => f,
        }
    }

    fn is_zero(self) -> bool {
        match self {
            Num::Dec(d, _) => d.is_zero(),
            Num::Float(f) => f == 0.0,
        }
    }

    /// An `f64` result, which is Approx whatever it looks like. It's kept as
    /// a decimal when it's what a decimal would hold: a whole number up to
    /// 2^53, or a value whose 15-digit text reads back as the very same
    /// `f64` (`4 ^ 0.5` is 2, `0.25 ^ 0.5` is 0.5). Anything else, like
    /// `2 ^ 0.5`, stays an `f64`, so arithmetic on it stays in `f64` and
    /// shows 15 digits, as omacalc does: `2 ^ 0.5 × 2 ^ 0.5` is 2.
    fn from_f64(value: f64) -> Answer {
        if !value.is_finite() {
            return Err(NoAnswer);
        }
        if value == 0.0 {
            return Ok(Num::Dec(Decimal::ZERO, Level::Approx));
        }
        // Every whole f64 up to 2^53 is exact; above that a whole f64 carries
        // binary noise (1e30 × 1e-7 is 99999999999999991611392), so it goes
        // through the 15-digit test below like any other f64.
        if value.fract() == 0.0 && value.abs() <= 9_007_199_254_740_992.0 {
            return Ok(match Decimal::from_f64_retain(value) {
                Some(d) => Num::Dec(d.normalize(), Level::Approx),
                None => Num::Float(value),
            });
        }
        let text = format!("{:.*e}", FLOAT_DIGITS as usize - 1, value);
        let round_trips = f64::from_str(&text).is_ok_and(|back| back == value);
        match Decimal::from_scientific(&text) {
            // Below a decimal's 28 places the value would round away.
            Ok(d) if round_trips && !d.is_zero() && d.normalize().scale() <= 28 => {
                Ok(Num::Dec(d.normalize(), Level::Approx))
            }
            _ => Ok(Num::Float(value)),
        }
    }

    /// Applies the decimal operation when both sides are decimals and
    /// [`settle`] accepts its result; otherwise the `f64` one.
    ///
    /// `undo` checks the raw decimal result against the inputs: it's the
    /// inverse operation (r − b = a for +), true when nothing was lost.
    fn combine(
        self,
        other: Num,
        kind: Kind,
        exact: impl Fn(Decimal, Decimal) -> Option<Decimal>,
        undo: impl Fn(Decimal, Decimal, Decimal) -> bool,
        float: impl Fn(f64, f64) -> f64,
    ) -> Answer {
        let level = self.level().max(other.level());
        if let (Num::Dec(a, _), Num::Dec(b, _)) = (self, other)
            && let Some(raw) = exact(a, b)
        {
            let zero_ok = match kind {
                Kind::Sum => true,
                Kind::Product => a.is_zero() || b.is_zero(),
                Kind::Quotient => a.is_zero(),
            };
            if let Some((value, level)) = settle(kind, raw, undo(a, b, raw), level, zero_ok) {
                return Ok(snap(kind, Num::Dec(value, level), a.abs().max(b.abs())));
            }
        }
        let (a, b) = (self.to_f64(), other.to_f64());
        let result = float(a, b);
        if kind == Kind::Sum && result.abs() <= a.abs().max(b.abs()) * APPROX_CANCELLATION {
            return Ok(Num::Dec(Decimal::ZERO, Level::Approx));
        }
        Num::from_f64(result)
    }

    fn apply(self, op: Op, other: Num) -> Answer {
        match op {
            Op::Add => self.combine(
                other,
                Kind::Sum,
                Decimal::checked_add,
                lossless_add,
                |a, b| a + b,
            ),
            Op::Sub => self.combine(
                other,
                Kind::Sum,
                Decimal::checked_sub,
                lossless_sub,
                |a, b| a - b,
            ),
            Op::Mul => self.combine(
                other,
                Kind::Product,
                Decimal::checked_mul,
                lossless_mul,
                |a, b| a * b,
            ),
            Op::Div => {
                if other.is_zero() {
                    return Err(NoAnswer);
                }
                self.combine(
                    other,
                    Kind::Quotient,
                    Decimal::checked_div,
                    lossless_div,
                    |a, b| a / b,
                )
            }
            Op::Pow => self.pow(other),
        }
    }

    fn negate(self) -> Num {
        match self {
            Num::Dec(d, level) => Num::Dec(-d, level),
            Num::Float(f) => Num::Float(-f),
        }
    }

    fn pow(self, exponent: Num) -> Answer {
        let whole = match exponent {
            Num::Dec(e, _) => e.is_integer(),
            Num::Float(e) => e.fract() == 0.0,
        };
        let negative = match self {
            Num::Dec(d, _) => d.is_sign_negative() && !d.is_zero(),
            Num::Float(f) => f < 0.0,
        };
        if negative && !whole {
            return Err(NoAnswer);
        }
        if self.is_zero() && exponent.to_f64() < 0.0 {
            return Err(NoAnswer);
        }
        if let (Num::Dec(base, _), Num::Dec(e, _)) = (self, exponent)
            && whole
            && let Ok(e) = i64::try_from(e.mantissa() / 10i128.pow(e.scale()))
            && let Some((value, level)) = integer_power(base, e, self.level().max(exponent.level()))
        {
            return Ok(Num::Dec(value, level));
        }
        Num::from_f64(self.to_f64().powf(exponent.to_f64()))
    }

    /// The display text, without thousands separators.
    fn format(self) -> String {
        match self {
            // A decimal that came back from f64 carries its error, so it's
            // shown at f64's 15 digits, like a Float.
            Num::Dec(d, Level::Approx) => format_decimal(d, FLOAT_DIGITS),
            Num::Dec(d, _) => format_exact(d),
            Num::Float(f) => format_float(f),
        }
    }
}

/// What a decimal operation was, for [`settle`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// + or −.
    Sum,
    /// × or a power with a non-negative exponent.
    Product,
    /// ÷ or a power with a negative exponent (a reciprocal).
    Quotient,
}

/// The one rule for decimal results. Takes the raw result of a decimal
/// operation, whether it lost nothing (`lossless`: a sum or product a
/// decimal holds in full, or a quotient that ends), and the inputs' level.
/// Returns the value and its level, or `None` to redo it in `f64`.
///
/// * A lossless result keeps its inputs' level; anything else is at least
///   Guarded.
/// * No result is rounded beyond what the decimal itself did: a Guarded
///   result keeps all 28–29 digits, and cancellation snapping (see [`snap`])
///   clears the residue a rounded step leaves.
/// * A decimal keeps 28 places rather than 28 significant digits, so a small
///   rounded result holds few digits. A rounded (Guarded or Approx) product,
///   power or cut-off quotient that holds fewer than 26 significant digits
///   ([`GUARDED_DIGITS`], so below 10⁻³) goes to `f64` instead. A zero is
///   kept only when zero is the true answer (`zero_ok`), not when a tiny
///   product rounded away.
fn settle(
    kind: Kind,
    raw: Decimal,
    lossless: bool,
    level: Level,
    zero_ok: bool,
) -> Option<(Decimal, Level)> {
    let level = if lossless {
        level
    } else {
        level.max(Level::Guarded)
    };
    if raw.is_zero() {
        return zero_ok.then_some((Decimal::ZERO, level));
    }
    if level == Level::Exact {
        return Some((raw.normalize(), level));
    }
    let smallest = match kind {
        Kind::Sum => i32::MIN,
        Kind::Quotient if lossless => i32::MIN,
        Kind::Product | Kind::Quotient => SMALLEST_GUARDED_EXPONENT,
    };
    if first_digit_exponent(raw) < smallest {
        return None;
    }
    Some((raw.normalize(), level))
}

/// Cancellation snapping for + and −: a non-zero sum or difference this
/// small next to its larger operand (`largest`) is residue from rounding,
/// so it's 0. Exact results never snap; Guarded ones at 10⁻²⁴ of `largest`
/// (a 28-digit value's residue is about 10⁻²⁷ a step), Approx ones at 10⁻¹⁵.
fn snap(kind: Kind, value: Num, largest: Decimal) -> Num {
    let Num::Dec(result, level) = value else {
        return value;
    };
    if kind != Kind::Sum || result.is_zero() {
        return value;
    }
    let threshold = match level {
        Level::Exact => return value,
        Level::Guarded => GUARDED_CANCELLATION,
        Level::Approx => Decimal::new(1, 15),
    };
    if result.abs() <= largest * threshold {
        Num::Dec(Decimal::ZERO, level)
    } else {
        value
    }
}

/// The power of ten the first significant digit of a non-zero value is worth.
fn first_digit_exponent(value: Decimal) -> i32 {
    let normal = value.normalize();
    let digits = normal.mantissa().unsigned_abs().ilog10() as i32 + 1;
    digits - 1 - normal.scale() as i32
}

/// `base ^ e` for a whole `e` as a decimal and its level, or `None` to work
/// it out in `f64`. A negative power is the reciprocal of the positive one,
/// so it's settled as a quotient and gets the same guard digits as 1 ÷ x.
fn integer_power(base: Decimal, e: i64, level: Level) -> Option<(Decimal, Level)> {
    let (raw, lossless) = power(base, e.unsigned_abs())?;
    let (raw, level) = settle(Kind::Product, raw, lossless, level, base.is_zero())?;
    if e >= 0 {
        return Some((raw, level));
    }
    let reciprocal = Decimal::ONE.checked_div(raw)?;
    let ends = lossless_div(Decimal::ONE, raw, reciprocal);
    settle(Kind::Quotient, reciprocal, ends, level, false)
}

/// A 256-bit unsigned integer: wide enough for the exact sum or product of
/// two decimal mantissas (96 bits each, aligned by up to 10²⁸), so the
/// lossless checks below can compare rust_decimal's result with the true one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Wide([u64; 4]);

impl Wide {
    fn from_u128(x: u128) -> Wide {
        Wide([x as u64, (x >> 64) as u64, 0, 0])
    }

    fn is_zero(self) -> bool {
        self.0 == [0; 4]
    }

    fn mul_u64(self, m: u64) -> Option<Wide> {
        let mut out = [0u64; 4];
        let mut carry = 0u128;
        for (o, limb) in out.iter_mut().zip(self.0) {
            let t = u128::from(limb) * u128::from(m) + carry;
            *o = t as u64;
            carry = t >> 64;
        }
        (carry == 0).then_some(Wide(out))
    }

    fn mul_u128(self, m: u128) -> Option<Wide> {
        let low = self.mul_u64(m as u64)?;
        let high = self.mul_u64((m >> 64) as u64)?;
        if high.0[3] != 0 {
            return None;
        }
        low.add(Wide([0, high.0[0], high.0[1], high.0[2]]))
    }

    fn mul_pow10(mut self, mut k: u32) -> Option<Wide> {
        // Zero times anything is zero; don't loop through a huge exponent.
        if self.0 == [0; 4] {
            return Some(self);
        }
        while k > 0 {
            let step = k.min(19);
            self = self.mul_u64(10u64.pow(step))?;
            k -= step;
        }
        Some(self)
    }

    fn add(self, other: Wide) -> Option<Wide> {
        let mut out = [0u64; 4];
        let mut carry = false;
        for (o, (x, y)) in out.iter_mut().zip(self.0.into_iter().zip(other.0)) {
            let (s, c1) = x.overflowing_add(y);
            let (s, c2) = s.overflowing_add(u64::from(carry));
            *o = s;
            carry = c1 || c2;
        }
        (!carry).then_some(Wide(out))
    }

    /// `self − other`, for `self ≥ other`.
    fn sub(self, other: Wide) -> Wide {
        let mut out = [0u64; 4];
        let mut borrow = false;
        for (o, (x, y)) in out.iter_mut().zip(self.0.into_iter().zip(other.0)) {
            let (d, b1) = x.overflowing_sub(y);
            let (d, b2) = d.overflowing_sub(u64::from(borrow));
            *o = d;
            borrow = b1 || b2;
        }
        Wide(out)
    }

    fn less_than(self, other: Wide) -> bool {
        self.0.iter().rev().lt(other.0.iter().rev())
    }

    fn div_rem_10(self) -> (Wide, u64) {
        let mut out = [0u64; 4];
        let mut rem = 0u128;
        for i in (0..4).rev() {
            let cur = (rem << 64) | u128::from(self.0[i]);
            out[i] = (cur / 10) as u64;
            rem = cur % 10;
        }
        (Wide(out), rem as u64)
    }
}

/// An exact decimal value, `mag × 10^−scale`, with trailing zeros removed
/// (down to scale 0), so equal values compare equal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Value {
    negative: bool,
    mag: Wide,
    scale: u32,
}

impl Value {
    fn new(negative: bool, mut mag: Wide, mut scale: u32) -> Value {
        while scale > 0 {
            let (q, r) = mag.div_rem_10();
            if r != 0 {
                break;
            }
            mag = q;
            scale -= 1;
        }
        let negative = negative && !mag.is_zero();
        let scale = if mag.is_zero() { 0 } else { scale };
        Value {
            negative,
            mag,
            scale,
        }
    }

    fn of(d: Decimal) -> Value {
        Value::new(
            d.is_sign_negative(),
            Wide::from_u128(d.mantissa().unsigned_abs()),
            d.scale(),
        )
    }

    /// The true `a ± b`.
    fn sum(a: Decimal, b: Decimal, subtract: bool) -> Option<Value> {
        let scale = a.scale().max(b.scale());
        let ma = Wide::from_u128(a.mantissa().unsigned_abs()).mul_pow10(scale - a.scale())?;
        let mb = Wide::from_u128(b.mantissa().unsigned_abs()).mul_pow10(scale - b.scale())?;
        let (na, nb) = (a.is_sign_negative(), b.is_sign_negative() ^ subtract);
        Some(if na == nb {
            Value::new(na, ma.add(mb)?, scale)
        } else if mb.less_than(ma) {
            Value::new(na, ma.sub(mb), scale)
        } else {
            Value::new(nb, mb.sub(ma), scale)
        })
    }

    /// The true `a × b`.
    fn product(a: Decimal, b: Decimal) -> Option<Value> {
        let mag =
            Wide::from_u128(a.mantissa().unsigned_abs()).mul_u128(b.mantissa().unsigned_abs())?;
        let negative = a.is_sign_negative() ^ b.is_sign_negative();
        Some(Value::new(negative, mag, a.scale() + b.scale()))
    }
}

// rust_decimal's checked_add, checked_sub, checked_mul and checked_div don't
// fail when a result needs more than 28 places or 96 bits: they round it,
// silently. So whether an operation lost anything is decided here by
// comparing its result with the true value, worked out exactly in 256 bits,
// and never by undoing it with another rounding operation.

/// Whether `r` is exactly `a + b`.
fn lossless_add(a: Decimal, b: Decimal, r: Decimal) -> bool {
    Value::sum(a, b, false) == Some(Value::of(r))
}

/// Whether `r` is exactly `a − b`.
fn lossless_sub(a: Decimal, b: Decimal, r: Decimal) -> bool {
    Value::sum(a, b, true) == Some(Value::of(r))
}

/// Whether `r` is exactly `a × b`.
fn lossless_mul(a: Decimal, b: Decimal, r: Decimal) -> bool {
    Value::product(a, b) == Some(Value::of(r))
}

/// Whether the quotient `q = a ÷ b` ends: `q × b` is exactly `a`.
fn lossless_div(a: Decimal, b: Decimal, q: Decimal) -> bool {
    Value::product(q, b) == Some(Value::of(a))
}

/// `base ^ n` by squaring, and whether every multiplication was lossless.
fn power(base: Decimal, mut n: u64) -> Option<(Decimal, bool)> {
    let (mut result, mut square, mut lossless) = (Decimal::ONE, base, true);
    while n > 0 {
        if n & 1 == 1 {
            let next = result.checked_mul(square)?;
            lossless &= lossless_mul(result, square, next);
            result = next;
        }
        n >>= 1;
        if n > 0 {
            let next = square.checked_mul(square)?;
            lossless &= lossless_mul(square, square, next);
            square = next;
        }
    }
    Some((result, lossless))
}

/// Rounds to 16 significant digits and lays the digits out.
fn format_exact(value: Decimal) -> String {
    format_decimal(value, EXACT_DIGITS)
}

/// Rounds to `precision` significant digits and lays the digits out.
fn format_decimal(value: Decimal, precision: u32) -> String {
    if value.is_zero() {
        return "0".into();
    }
    let normal = value.normalize();
    let mantissa = normal.mantissa();
    let all = mantissa.unsigned_abs().to_string();
    // value = digits × 10^(−scale), so the first digit's power of ten is:
    let exponent = all.len() as i32 - 1 - normal.scale() as i32;
    // Rounded by hand: `round_sf` can't round a value near Decimal::MAX.
    let (digits, carried) = round_digits(&all, precision as usize);
    layout(
        mantissa < 0,
        &digits,
        exponent + i32::from(carried),
        precision,
    )
}

/// Rounds a run of significant digits to `keep` of them, half away from
/// zero. Returns the digits and whether rounding carried into a new leading
/// digit (999… → 1000…).
fn round_digits(digits: &str, keep: usize) -> (String, bool) {
    if digits.len() <= keep {
        return (digits.to_owned(), false);
    }
    let mut kept: Vec<u8> = digits.as_bytes()[..keep].to_vec();
    if digits.as_bytes()[keep] >= b'5' {
        let mut i = keep;
        loop {
            if i == 0 {
                kept.insert(0, b'1');
                kept.pop();
                return (String::from_utf8(kept).unwrap_or_default(), true);
            }
            i -= 1;
            if kept[i] == b'9' {
                kept[i] = b'0';
            } else {
                kept[i] += 1;
                break;
            }
        }
    }
    (String::from_utf8(kept).unwrap_or_default(), false)
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

    let parsed = if exponent.is_some() {
        Decimal::from_scientific(text).ok()
    } else {
        Decimal::from_str(text).ok()
    };
    // from_str rounds a number with more digits or places than a decimal
    // holds, so it's exact only when it matches the text's true value.
    let exponent: i64 = match exponent {
        Some(e) => e.parse().ok()?,
        None => 0,
    };
    let scale = fraction.len() as i64 - exponent;
    let truth = text_value(text.starts_with('-'), &format!("{whole}{fraction}"), scale);
    match parsed {
        Some(d) if truth.is_some_and(|t| t == Value::of(d)) => Some(Num::exact(d)),
        _ => {
            let value = f64::from_str(text).ok()?;
            // 1e-400 reads as 0 in f64: refuse it rather than show 0.
            let nonzero = mantissa.bytes().any(|b| (b'1'..=b'9').contains(&b));
            if value == 0.0 && nonzero {
                return None;
            }
            Num::from_f64(value).ok()
        }
    }
}

/// The exact value of `digits × 10^−scale`, or `None` when it's too long
/// to work out (and so can't be a decimal anyway).
fn text_value(negative: bool, digits: &str, scale: i64) -> Option<Value> {
    let digits = digits.trim_start_matches('0');
    let mut mag = Wide::from_u128(0);
    for b in digits.bytes() {
        mag = mag
            .mul_u64(10)?
            .add(Wide::from_u128(u128::from(b - b'0')))?;
    }
    if scale < 0 {
        mag = mag.mul_pow10(u32::try_from(-scale).ok()?)?;
    }
    Some(Value::new(negative, mag, u32::try_from(scale.max(0)).ok()?))
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
    /// The exact value behind `entry` when the calculator set it (a percent,
    /// a paste, or a result being edited), so using it doesn't lose what the
    /// 16-digit text rounds off. Typing into the entry drops it.
    entry_value: Option<Num>,
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
        self.entry_value = Some(value);
        true
    }

    fn press_digit(&mut self, digit: char) {
        if self.errored || self.result.is_some() {
            // A digit after `=` starts a new calculation.
            self.clear();
        }
        self.entry_value = None;
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
        self.entry_value = None;
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
            let value = self.entry_num();
            self.entry.clear();
            self.entry_value = None;
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
            tokens.push(Token::Num(self.entry_num()));
            self.entry.clear();
            self.entry_value = None;
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
            match value.apply(Op::Div, Num::exact(Decimal::ONE_HUNDRED)) {
                Ok(percent) => self.set_entry(percent),
                Err(NoAnswer) => self.errored = true,
            }
            return;
        }

        let value = self.current_value();
        let hundred = Num::exact(Decimal::ONE_HUNDRED);
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
            Ok(percent) => self.set_entry(percent),
            Err(NoAnswer) => {
                let mut shown = self.tokens.clone();
                shown.push(Token::Num(value));
                self.evaluated = format!("{} %", pretty(&shown));
                self.errored = true;
                self.tokens.clear();
                self.entry.clear();
                self.entry_value = None;
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
        self.entry_value = self.entry_value.map(Num::negate);
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
        self.entry_value = None;
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
        self.entry_value = None;
        self.result = None;
        self.evaluated.clear();
        self.errored = false;
    }

    /// Editing after `=` starts from the result's digits, with the old
    /// expression cleared.
    fn edit_result(&mut self) {
        if let Some((value, text)) = self.result.take() {
            self.entry = text;
            self.entry_value = Some(value);
        }
        self.tokens.clear();
        self.evaluated.clear();
    }

    /// Shows a value the calculator worked out as the entry, keeping it exact.
    fn set_entry(&mut self, value: Num) {
        self.entry = value.format();
        self.entry_value = Some(value);
    }

    /// The entry's number: its exact value if the calculator set it, else
    /// what was typed.
    fn entry_num(&self) -> Num {
        self.entry_value.unwrap_or_else(|| seal(&self.entry))
    }

    /// The number the calculator is at: the one being typed, else the last
    /// one entered, else zero.
    fn current_value(&self) -> Num {
        if !self.entry.is_empty() {
            return self.entry_num();
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

/// `1,234` or `-1,234,567.89`: commas exactly every three digits. A first
/// group of just `0` isn't a thousands group, so `0,500` is a decimal comma
/// (0.5), not 500.
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
    let first_ok =
        (1..=3).contains(&first.len()) && first.bytes().all(|b| b.is_ascii_digit()) && first != "0";
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

    /// A decimal back from f64 shows f64's 15 digits, so its rounding error
    /// doesn't: money divided by a large number and multiplied back.
    #[test]
    fn approx_decimals_hide_f64_noise() {
        assert_eq!(shows("8 . 8 8 ÷ 8 8 9 5 0 7 × 8 8 9 5 0 7 ="), "8.88");
        assert_eq!(shows("1 . 4 3 ÷ 3 1 2 5 5 4 × 1 2 5 0 2 1 6 ="), "5.72");
        assert_eq!(shows("5 8 1 . 2 ÷ 5 8 1 4 3 2 × 5 8 1 4 3 2 0 ="), "5,812");
    }

    /// A pasted zero with a huge exponent is a quick, exact 0.
    #[test]
    fn pasted_zero_with_a_huge_exponent() {
        let start = std::time::Instant::now();
        let mut calc = Calc::new();
        assert!(calc.paste("0e4294967295"));
        assert_eq!(calc.display(), "0");
        assert!(start.elapsed().as_millis() < 50, "{:?}", start.elapsed());
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
        // Small quotients get the same guard digits as large ones.
        assert_eq!(shows("1 ÷ 3 0 0 0 × 3 0 0 0 - 1 ="), "0");
        assert_eq!(shows("1 ÷ 3 0 0 0 0 × 3 0 0 0 0 - 1 ="), "0");
        assert_eq!(shows("2 ÷ 3 0 0 0 0 0 × 3 0 0 0 0 0 - 2 ="), "0");
        assert_eq!(shows("1 ÷ 7 0 0 0 × 7 0 0 0 - 1 ="), "0");
        assert_eq!(shows("1 ÷ 3 0 0 0 = × 3 0 0 0 = - 1 ="), "0");
        assert_eq!(shows("1 ÷ 3 0 0 0 0 × sign 3 0 0 0 0 + 1 ="), "0");
        // Below 1e-7 a cut-off quotient is an f64, shown at 15 digits.
        assert_eq!(
            shows("1 ÷ 3 0 0 0 0 0 0 0 0 0 0 0 0 × 3 0 0 0 0 0 0 0 0 0 0 0 0 ="),
            "1"
        );
        assert_eq!(
            shows("1 ÷ 3 0 0 0 0 0 0 0 0 0 0 0 0 = × 3 0 0 0 0 0 0 0 0 0 0 0 0 ="),
            "1"
        );
        // A quotient that ends stays exact at any size.
        assert_eq!(
            shows("1 ÷ 1 0 0 0 0 0 0 0 0 × 1 0 0 0 0 0 0 0 0 - 1 ="),
            "0"
        );
        assert_eq!(shows("1 9 . 9 9 × 3 ="), "59.97");
        assert_eq!(shows("1 ÷ 3 ÷ 7 × 2 1 - 1 ="), "0");
        assert_eq!(
            shows("1 0 ^ 2 7 + 1 - 1 0 ^ 2 7 ="),
            "1",
            "exact values are never rounded"
        );
    }

    /// Chained roundings cancel to exactly 0.
    #[test]
    fn cancellation_snaps_to_zero() {
        assert_eq!(shows("1 ÷ 3 ÷ 3 × 3 × 3 - 1 ="), "0");
        assert_eq!(shows("1 ÷ 7 ÷ 7 × 7 × 7 - 1 ="), "0");
        assert_eq!(
            shows("1 0 ÷ 3 ÷ 3 ÷ 3 ÷ 3 ÷ 3 × 3 × 3 × 3 × 3 × 3 - 1 0 ="),
            "0"
        );
        // On the f64 side of the quotient cutoff.
        assert_eq!(shows("1 ÷ 2 9 9 9 9 9 9 9 × 2 9 9 9 9 9 9 9 - 1 ="), "0");
        assert_eq!(shows("1 ÷ 1 0 0 0 0 0 0 4 × 1 0 0 0 0 0 0 4 - 1 ="), "0");
        assert_eq!(shows("3 ^ 4 0 sign × 3 ^ 4 0 - 1 ="), "0");
    }

    /// Snapping never hides a real difference between typed numbers.
    #[test]
    fn cancellation_keeps_real_differences() {
        assert_eq!(
            shows("1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 - 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 5 ="),
            "1"
        );
        assert_eq!(
            shows("1 - . 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1 ="),
            "0.9999999999999999"
        );
        assert_eq!(
            shows(". 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1 + . 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1 ="),
            "2e-16"
        );
        assert_eq!(shows("0 . 1 + 0 . 2 - 0 . 3 ="), "0");
        // A float against a number typed to 15 digits keeps its difference.
        let sqrt2 = shows("2 ^ . 5 - 1 . 4 1 4 2 1 3 5 6 2 3 7 3 0 9 =");
        assert!(
            sqrt2.starts_with("5.") && sqrt2.ends_with("e-15"),
            "{sqrt2}"
        );
        let rest = shows("1 ÷ 3 - . 3 3 3 3 3 3 3 3 3 3 3 3 3 3 3 3 =");
        assert!(
            rest.starts_with("3.333") && rest.ends_with("e-17"),
            "{rest}"
        );
    }

    /// Exact values are never rounded or snapped, so typed digits survive a
    /// chain however large the intermediate gets.
    #[test]
    fn exact_chains_keep_typed_digits() {
        assert_eq!(
            shows("1 0 0 0 0 0 0 0 0 0 + . 0 0 0 0 0 0 0 0 1 - 1 0 0 0 0 0 0 0 0 0 ="),
            "1e-09"
        );
        assert_eq!(
            shows("1 0 0 0 0 0 0 0 0 0 0 0 0 0 + . 0 0 0 1 - 1 0 0 0 0 0 0 0 0 0 0 0 0 0 ="),
            "0.0001"
        );
        assert_eq!(shows("2 ^ 6 0 + 1 - 2 ^ 6 0 ="), "1");
        assert_eq!(
            shows(
                "1 0 0 0 0 0 0 0 0 0 × 1 0 0 0 0 0 0 0 0 0 0 + 1 \
                 - 1 0 0 0 0 0 0 0 0 0 × 1 0 0 0 0 0 0 0 0 0 0 ="
            ),
            "1"
        );
        assert_eq!(
            shows(
                "8 4 0 3 9 1 1 1 1 5 5 6 6 0 0 9 + 2 . 4 1 1 8 6 7 5 \
                 - 8 4 0 3 9 1 1 1 1 5 5 6 6 0 0 9 ="
            ),
            "2.4118675"
        );
    }

    /// A quotient that went through f64 stays Approx when it's turned back
    /// into a decimal, so its f64 error snaps.
    #[test]
    fn approx_quotients_snap() {
        assert_eq!(shows("4 ÷ 5 0 6 1 9 4 6 3 × 5 0 6 1 9 4 6 3 - 4 ="), "0");
        assert_eq!(
            shows("3 1 9 ÷ 3 9 3 6 7 7 2 1 8 8 × 3 9 3 6 7 7 2 1 8 8 - 3 1 9 ="),
            "0"
        );
    }

    /// A small xorshift, so the property tests need no dependency and fail
    /// the same way every run.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }

        /// `len` random digits, the first non-zero.
        fn digits(&mut self, len: usize) -> String {
            (0..len)
                .map(|i| {
                    let d = if i == 0 {
                        1 + self.below(9)
                    } else {
                        self.below(10)
                    };
                    char::from(b'0' + d as u8)
                })
                .collect()
        }
    }

    /// The key presses that type `number` ("12.5" → "1 2 . 5").
    fn keys(number: &str) -> String {
        let mut out = String::new();
        for c in number.chars() {
            out.push(c);
            out.push(' ');
        }
        out
    }

    /// A typed number of 1–10 digits, sometimes with a decimal point.
    fn typed_operand(rng: &mut Rng) -> String {
        let len = 1 + rng.below(10) as usize;
        let mut number = rng.digits(len);
        if len > 1 && rng.below(3) == 0 {
            number.insert(1 + rng.below(len as u64 - 1) as usize, '.');
        }
        number
    }

    #[test]
    fn property_division_round_trips() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        for _ in 0..3000 {
            let (a, b) = (typed_operand(&mut rng), typed_operand(&mut rng));
            let (a, b) = (keys(&a), keys(&b));
            let once = format!("{a}÷ {b}× {b}- {a}=");
            assert_eq!(shows(&once), "0", "{once}");
            let twice = format!("{a}÷ {b}÷ {b}× {b}× {b}- {a}=");
            assert_eq!(shows(&twice), "0", "{twice}");
        }
    }

    #[test]
    fn property_adding_then_subtracting_returns_the_addend() {
        let mut rng = Rng(0x2545_f491_4f6c_dd1d);
        let mut checked = 0;
        for _ in 0..4000 {
            // a: up to 16 typed digits, the point anywhere.
            let len = 1 + rng.below(16) as usize;
            let mut a = rng.digits(len);
            if rng.below(2) == 0 {
                a.insert(rng.below(len as u64) as usize, '.');
                if a.starts_with('.') {
                    a.insert(0, '0');
                }
            }
            // c: from 1e-12 to 1e15, as many digits as the 16-digit entry
            // takes (the zeros after "0." count).
            let exponent = rng.below(28) as i32 - 12;
            let room = if exponent >= 0 { 16 } else { 17 + exponent } as u64;
            let sig_len = 1 + rng.below(room) as usize;
            let sig = rng.digits(sig_len);
            let c = if exponent >= 0 {
                let whole = (exponent + 1) as usize;
                if sig.len() <= whole {
                    format!("{sig}{}", "0".repeat(whole - sig.len()))
                } else {
                    format!("{}.{}", &sig[..whole], &sig[whole..])
                }
            } else {
                format!("0.{}{sig}", "0".repeat((-exponent - 1) as usize))
            };

            // Only when a decimal can hold a + c in full: its 28 digits must
            // cover the larger whole part and the longer fraction.
            let (av, cv) = (
                Decimal::from_str(&a).unwrap(),
                Decimal::from_str(&c).unwrap(),
            );
            let whole_digits = |d: Decimal| d.trunc().to_string().trim_start_matches('0').len();
            let fits = whole_digits(av + cv).max(1) as u32 + av.scale().max(cv.scale()) <= 28;
            if !fits {
                continue;
            }
            checked += 1;
            let keys_text = format!("{}+ {}- {}=", keys(&a), keys(&c), keys(&a));
            let expected = group_digits(&Num::exact(cv).format());
            assert_eq!(shows(&keys_text), expected, "{keys_text}");
        }
        assert!(checked > 3000, "only {checked} cases fit a decimal");
    }

    /// rust_decimal rounds silently; a rounded result is never marked Exact.
    #[test]
    fn silent_rounding_is_caught() {
        // 1.234567890123456e-20 needs 36 places: rounded, so not exact, and
        // too small to keep 26 digits, so it's worked out in f64.
        assert_eq!(
            shows(
                ". 0 0 0 0 0 0 0 0 0 1 × . 0 0 0 0 0 0 0 0 0 1 × 1 . 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 ="
            ),
            format_float(1.234567890123456e-20)
        );
        assert_eq!(
            shows("3 4 ÷ 7 . 6 7 + 9 . 3 ÷ 7 . 6 7 - 4 3 . 3 ÷ 7 . 6 7 ="),
            "0"
        );
        let sixths = "1 ÷ 1 . 2 + ".repeat(5) + "1 ÷ 1 . 2 - 5 =";
        assert_eq!(shows(&sixths), "0");
        assert_eq!(
            shows("2 . 4 7 5 8 ^ 2 sign × 2 . 4 7 5 8 × 2 . 4 7 5 8 - 1 ="),
            "0"
        );
        let mut calc = Calc::new();
        assert!(calc.paste("0.00000000000000000000000000015"));
        assert_eq!(calc.display(), "1.5e-28");
    }

    /// Guarded values keep every digit the decimal holds.
    #[test]
    fn guarded_values_keep_their_digits() {
        assert_eq!(
            shows("2 ÷ 3 + 1 0 0 0 0 0 0 0 0 0 0 - 1 0 0 0 0 0 0 0 0 0 0 ="),
            "0.6666666666666667"
        );
        let mut calc = Calc::new();
        press(&mut calc, "1 ÷ 3 -");
        assert!(calc.paste("0.333333333333333333"));
        press(&mut calc, "=");
        let rest = calc.display();
        assert!(
            rest.starts_with("3.333333333") && rest.ends_with("e-19"),
            "{rest}"
        );
    }

    /// The remaining limit: a sum of typed numbers that needs more than 28
    /// digits is rounded (Guarded), as omacalc's double rounds it too.
    #[test]
    fn sums_past_28_digits_round() {
        assert_eq!(
            shows(
                "1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 + . 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1 \
                 - 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 ="
            ),
            "0"
        );
    }

    /// Backspace over a whole new operand shows the previous one, as omacalc
    /// does (its display falls back to the last operand when nothing's typed).
    #[test]
    fn backspace_over_new_operand_shows_previous() {
        let mut calc = Calc::new();
        press(&mut calc, "1 2 + 3 backspace");
        assert_eq!(calc.display(), "12");
        assert_eq!(calc.expression(), "12 +");
        press(&mut calc, "4 =");
        assert_eq!(calc.display(), "16");
    }

    /// A reference value `m × 10^−s` (`s` may be negative), worked in i128,
    /// independent of rust_decimal.
    #[derive(Clone, Copy, Debug, PartialEq)]
    struct Ref {
        m: i128,
        s: i32,
    }

    impl Ref {
        fn of(d: Decimal) -> Ref {
            Ref {
                m: d.mantissa(),
                s: d.scale() as i32,
            }
            .normal()
        }

        fn normal(mut self) -> Ref {
            while self.m != 0 && self.m % 10 == 0 {
                self.m /= 10;
                self.s -= 1;
            }
            if self.m == 0 {
                self.s = 0;
            }
            self
        }

        fn scaled(self, s: i32) -> Option<i128> {
            self.m
                .checked_mul(10i128.checked_pow(u32::try_from(s - self.s).ok()?)?)
        }

        fn add(self, other: Ref, subtract: bool) -> Option<Ref> {
            let s = self.s.max(other.s);
            let (a, b) = (self.scaled(s)?, other.scaled(s)?);
            let m = if subtract {
                a.checked_sub(b)?
            } else {
                a.checked_add(b)?
            };
            Some(Ref { m, s }.normal())
        }

        fn mul(self, other: Ref) -> Option<Ref> {
            let m = self.m.checked_mul(other.m)?;
            Some(
                Ref {
                    m,
                    s: self.s + other.s,
                }
                .normal(),
            )
        }

        /// Whether a decimal can hold this value exactly: a 96-bit mantissa
        /// and at most 28 places.
        fn fits(self) -> bool {
            let r = self.normal();
            let m = if r.s < 0 {
                match r.scaled(0) {
                    Some(m) => m,
                    None => return false,
                }
            } else {
                r.m
            };
            r.s <= 28 && m.unsigned_abs() < (1u128 << 96)
        }
    }

    /// A typed operand: 1–16 significant digits, scale −10 to 16, either sign.
    fn reference_operand(rng: &mut Rng) -> Decimal {
        let len = 1 + rng.below(16) as usize;
        let mut m: i128 = rng.digits(len).parse().unwrap();
        if rng.below(2) == 0 {
            m = -m;
        }
        let scale = rng.below(27) as i32 - 10;
        if scale < 0 {
            Decimal::from_i128_with_scale(m * 10i128.pow(scale.unsigned_abs()), 0)
        } else {
            Decimal::from_i128_with_scale(m, scale as u32)
        }
    }

    /// The key test for precision: every result marked Exact is the true
    /// value, and every true value a decimal can hold is marked Exact.
    #[test]
    fn property_exact_means_exact() {
        let mut rng = Rng(0x1234_5678_9abc_def1);
        for op in [Op::Add, Op::Sub, Op::Mul] {
            let mut checked = 0;
            for _ in 0..20_000 {
                let (a, b) = (reference_operand(&mut rng), reference_operand(&mut rng));
                let truth = match op {
                    Op::Add => Ref::of(a).add(Ref::of(b), false),
                    Op::Sub => Ref::of(a).add(Ref::of(b), true),
                    _ => Ref::of(a).mul(Ref::of(b)),
                };
                let Some(truth) = truth else { continue };
                checked += 1;
                let result = Num::exact(a).apply(op, Num::exact(b));
                if let Ok(Num::Dec(d, Level::Exact)) = result {
                    assert_eq!(Ref::of(d), truth, "{a} {op:?} {b} marked Exact as {d}");
                }
                if truth.fits() {
                    assert!(
                        matches!(result, Ok(Num::Dec(_, Level::Exact))),
                        "{a} {op:?} {b} fits a decimal but is {result:?}"
                    );
                }
            }
            assert!(checked > 15_000, "{op:?}: only {checked} cases fit i128");
        }

        let mut checked = 0;
        for _ in 0..20_000 {
            let (a, b) = (reference_operand(&mut rng), reference_operand(&mut rng));
            if b.is_zero() {
                continue;
            }
            if let Ok(Num::Dec(q, Level::Exact)) = Num::exact(a).apply(Op::Div, Num::exact(b))
                && let Some(back) = Ref::of(q).mul(Ref::of(b))
            {
                checked += 1;
                assert_eq!(back, Ref::of(a), "{a} ÷ {b} marked Exact as {q}");
            }
        }
        assert!(checked > 1000, "only {checked} exact quotients checked");
    }

    /// Typed operands of 1–10 digits as numbers, for the residue shapes.
    fn residue_operand(rng: &mut Rng) -> Num {
        Num::exact(Decimal::from_str(&typed_operand(rng)).unwrap())
    }

    #[test]
    fn property_residue_shapes_cancel() {
        let mut rng = Rng(0x0dd_ba11_cafe_f00d);
        let (div, add, sub, mul) = (Op::Div, Op::Add, Op::Sub, Op::Mul);
        for _ in 0..5000 {
            let (a, b, c) = (
                residue_operand(&mut rng),
                residue_operand(&mut rng),
                residue_operand(&mut rng),
            );
            let q = |x: Num| x.apply(div, b).unwrap();

            let round_trip = q(a).apply(mul, b).unwrap().apply(sub, a).unwrap();
            assert!(
                round_trip.is_zero(),
                "{a:?} ÷ {b:?} × b − a = {round_trip:?}"
            );

            let split = q(a).apply(add, q(c)).unwrap();
            let whole = q(a.apply(add, c).unwrap());
            let left = split.apply(sub, whole).unwrap();
            assert!(
                left.is_zero(),
                "a÷b + c÷b − (a+c)÷b = {left:?} for {a:?} {b:?} {c:?}"
            );

            let mut sum = q(a);
            for _ in 0..6 {
                sum = sum.apply(add, q(a)).unwrap();
            }
            let seven = q(a.apply(mul, Num::exact(Decimal::from(7))).unwrap());
            let left = sum.apply(sub, seven).unwrap();
            assert!(
                left.is_zero(),
                "7 × (a÷b) summed − 7a÷b = {left:?} for {a:?} {b:?}"
            );
        }
    }

    /// A whole f64 past 2^53 isn't exact, so it isn't promoted with its noise.
    #[test]
    fn large_whole_floats_stay_approximate() {
        let mut calc = Calc::new();
        assert!(calc.paste("1e30"));
        press(&mut calc, "×");
        assert!(calc.paste("1e-7"));
        press(&mut calc, "=");
        assert_eq!(calc.display(), "1e+23");
        // Whole numbers up to 2^53 still become exact.
        assert_eq!(shows("4 ^ . 5 + 0 . 1 ="), "2.1");
    }

    /// A negative power is a reciprocal and gets a quotient's guard digits.
    #[test]
    fn negative_powers_cancel() {
        assert_eq!(shows("7 ^ 1 sign × 7 - 1 ="), "0");
        assert_eq!(shows("3 ^ 1 sign × 3 - 1 ="), "0");
        assert_eq!(shows("2 ^ 1 sign ="), "0.5");
        assert_eq!(shows("2 ^ 3 sign ="), "0.125");
        assert_eq!(shows("3 ^ 2 sign × 9 ="), "1");
    }

    /// % keeps the exact value, not its 16-digit text.
    #[test]
    fn percent_keeps_the_exact_value() {
        assert_eq!(shows("1 ÷ 3 = % × 3 0 0 ="), "1");
        assert_eq!(shows("1 ÷ 3 = % × 3 0 0 - 1 ="), "0");
        // Editing it by hand goes back to the typed digits.
        assert_eq!(shows("5 0 % 5"), "0.55");
        assert_eq!(shows("1 ÷ 3 = % sign × 3 0 0 ="), "-1");
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

    /// A first group of just 0 can't be a thousands group.
    #[test]
    fn paste_reads_a_zero_comma_as_decimal() {
        let mut calc = Calc::new();
        assert!(calc.paste("0,123"));
        assert_eq!(calc.display(), "0.123");
        assert!(calc.paste("0,500"));
        assert_eq!(calc.display(), "0.5");
        assert!(calc.paste("-0,500"));
        assert_eq!(calc.display(), "-0.5");
        assert!(calc.paste("1,234"));
        assert_eq!(calc.display(), "1,234");
        assert!(calc.paste("12,34"));
        assert_eq!(calc.display(), "12.34");
        assert!(calc.paste("1 234,5"));
        assert_eq!(calc.display(), "1,234.5");
        assert!(!calc.paste("0,123,456"));
        assert!(!calc.paste(",5"));
    }

    #[test]
    fn paste_extremes() {
        let mut calc = Calc::new();
        assert!(calc.paste("79228162514264337593543950335"));
        assert_eq!(calc.display(), "7.922816251426434e+28");
        assert!(calc.paste("99999999999999999999"));
        assert_eq!(calc.display(), "1e+20");
        assert!(!calc.paste("1e-400"));
        assert_eq!(calc.display(), "1e+20");
        // A pasted long number is used exactly, though it shows 16 digits.
        assert!(calc.paste("1234567890123456.7"));
        press(&mut calc, "- 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 =");
        assert_eq!(calc.display(), "0.7");
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
        let exact = |s: &str| Num::exact(Decimal::from_str(s).unwrap()).format();
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
        let n = |v: i64| Token::Num(Num::exact(Decimal::from(v)));
        let t = [n(2), Token::Op(Op::Add), n(3), Token::Op(Op::Mul), n(4)];
        assert_eq!(evaluate(&t), Ok(Num::exact(Decimal::from(14))));
        let t = [n(2), Token::Op(Op::Pow), n(3), Token::Op(Op::Pow), n(2)];
        assert_eq!(evaluate(&t), Ok(Num::exact(Decimal::from(512))));
        assert_eq!(evaluate(&[n(1), Token::Op(Op::Add)]), Err(NoAnswer));
        assert_eq!(evaluate(&[]), Err(NoAnswer));
    }
}
