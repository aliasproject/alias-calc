//! Keyboard mapping: which calculator key a keysym presses.
//!
//! Keysyms, not text, so the mapping follows the keyboard layout and the
//! numpad works with NumLock on (KP_1) or off (KP_End).

use smithay_client_toolkit::seat::keyboard::Keysym;

use crate::engine::{Key, Op};

/// The calculator key for a keysym pressed with no Ctrl, Alt or Super.
pub fn key_for(sym: Keysym) -> Option<Key> {
    let key = match sym {
        Keysym::_0 | Keysym::KP_0 | Keysym::KP_Insert => Key::Digit(0),
        Keysym::_1 | Keysym::KP_1 | Keysym::KP_End => Key::Digit(1),
        Keysym::_2 | Keysym::KP_2 | Keysym::KP_Down => Key::Digit(2),
        Keysym::_3 | Keysym::KP_3 | Keysym::KP_Next => Key::Digit(3),
        Keysym::_4 | Keysym::KP_4 | Keysym::KP_Left => Key::Digit(4),
        Keysym::_5 | Keysym::KP_5 | Keysym::KP_Begin => Key::Digit(5),
        Keysym::_6 | Keysym::KP_6 | Keysym::KP_Right => Key::Digit(6),
        Keysym::_7 | Keysym::KP_7 | Keysym::KP_Home => Key::Digit(7),
        Keysym::_8 | Keysym::KP_8 | Keysym::KP_Up => Key::Digit(8),
        Keysym::_9 | Keysym::KP_9 | Keysym::KP_Prior => Key::Digit(9),
        // KP_Separator is the numpad's decimal key on layouts with a decimal
        // comma (German, French).
        Keysym::period | Keysym::KP_Decimal | Keysym::KP_Separator | Keysym::KP_Delete => {
            Key::Point
        }
        Keysym::plus | Keysym::KP_Add => Key::Op(Op::Add),
        Keysym::minus | Keysym::KP_Subtract => Key::Op(Op::Sub),
        Keysym::asterisk | Keysym::KP_Multiply | Keysym::x | Keysym::X => Key::Op(Op::Mul),
        Keysym::slash | Keysym::KP_Divide => Key::Op(Op::Div),
        // ^ is a dead key on many European layouts.
        Keysym::asciicircum | Keysym::dead_circumflex => Key::Op(Op::Pow),
        Keysym::percent => Key::Percent,
        Keysym::Return | Keysym::KP_Enter | Keysym::equal | Keysym::KP_Equal => Key::Equals,
        Keysym::BackSpace => Key::Backspace,
        Keysym::c | Keysym::C | Keysym::Escape => Key::Clear,
        Keysym::s | Keysym::S => Key::Sign,
        _ => return None,
    };
    Some(key)
}

/// Keys that repeat while held. Only Backspace: repeating a digit or an
/// operator by holding it is never what anyone wants on a calculator.
pub fn repeats(key: Key) -> bool {
    key == Key::Backspace
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numpad_works_with_and_without_numlock() {
        for (on, off, digit) in [
            (Keysym::KP_0, Keysym::KP_Insert, 0),
            (Keysym::KP_1, Keysym::KP_End, 1),
            (Keysym::KP_5, Keysym::KP_Begin, 5),
            (Keysym::KP_9, Keysym::KP_Prior, 9),
        ] {
            assert_eq!(key_for(on), Some(Key::Digit(digit)));
            assert_eq!(key_for(off), Some(Key::Digit(digit)));
        }
        assert_eq!(key_for(Keysym::KP_Delete), Some(Key::Point));
        assert_eq!(key_for(Keysym::KP_Enter), Some(Key::Equals));
        assert_eq!(key_for(Keysym::KP_Separator), Some(Key::Point));
        assert_eq!(key_for(Keysym::dead_circumflex), Some(Key::Op(Op::Pow)));
    }

    #[test]
    fn maps_the_documented_keys() {
        assert_eq!(key_for(Keysym::x), Some(Key::Op(Op::Mul)));
        assert_eq!(key_for(Keysym::asterisk), Some(Key::Op(Op::Mul)));
        assert_eq!(key_for(Keysym::asciicircum), Some(Key::Op(Op::Pow)));
        assert_eq!(key_for(Keysym::percent), Some(Key::Percent));
        assert_eq!(key_for(Keysym::Escape), Some(Key::Clear));
        assert_eq!(key_for(Keysym::C), Some(Key::Clear));
        assert_eq!(key_for(Keysym::s), Some(Key::Sign));
        assert_eq!(key_for(Keysym::a), None);
        assert!(repeats(Key::Backspace));
        assert!(!repeats(Key::Digit(1)));
    }
}
