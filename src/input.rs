//! Linux wire-key mapping and per-control bookkeeping (no GUI enum values on wire).
use crate::protocol::InputEvent;

#[derive(Default)]
pub struct Control {
    pub buttons: [bool; 3],
    pub left_button: u8,
    pub touch: bool,
    pub last: Option<(u16, u16)>,
    modifiers: [bool; 4],
}
impl Control {
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    pub fn is_held(&self) -> bool {
        self.touch || self.buttons.iter().any(|v| *v) || self.modifiers.iter().any(|v| *v)
    }
    pub fn modifiers(&mut self, held: [bool; 4]) -> Vec<InputEvent> {
        let mut events = Vec::new();
        for (i, code) in [42, 29, 56, 125].into_iter().enumerate() {
            if self.modifiers[i] != held[i] {
                events.push(key_event(code, held[i]));
            }
        }
        self.modifiers = held;
        events
    }
    pub fn key(&mut self, name: &str, held: [bool; 4]) -> Vec<InputEvent> {
        let mut events = self.modifiers(held);
        if let Some((code, shift)) = key(name) {
            let synth = shift && !held[0] && !held[1..].iter().any(|v| *v);
            if synth {
                events.push(key_event(42, true));
            }
            events.extend([key_event(code, true), key_event(code, false)]);
            if synth {
                events.push(key_event(42, false));
            }
        }
        events
    }
}
fn key_event(code: u16, down: bool) -> InputEvent {
    InputEvent {
        kind: if down { 8 } else { 9 },
        id: 0,
        x_or_key: code,
        y: 0,
        value: 0,
    }
}
pub fn pointer(kind: u8, id: u8, p: (u16, u16), value: i16) -> InputEvent {
    InputEvent {
        kind,
        id,
        x_or_key: p.0,
        y: p.1,
        value,
    }
}
pub fn key(name: &str) -> Option<(u16, bool)> {
    let special = match name {
        "backspace" => 14,
        "escape" => 1,
        "delete" => 111,
        "insert" => 110,
        "home" => 102,
        "end" => 107,
        "pageup" => 104,
        "pagedown" => 109,
        "left" => 105,
        "right" => 106,
        "up" => 103,
        "down" => 108,
        "tab" => 15,
        "enter" | "return" => 28,
        "space" => 57,
        "f1" => 59,
        "f2" => 60,
        "f3" => 61,
        "f4" => 62,
        "f5" => 63,
        "f6" => 64,
        "f7" => 65,
        "f8" => 66,
        "f9" => 67,
        "f10" => 68,
        // Only explicit keypad identities map to keypad codes. Some platforms
        // collapse these to ordinary keys; that cannot be recovered from text.
        "numpad0" => 82,
        "numpad1" => 79,
        "numpad2" => 80,
        "numpad3" => 81,
        "numpad4" => 75,
        "numpad5" => 76,
        "numpad6" => 77,
        "numpad7" => 71,
        "numpad8" => 72,
        "numpad9" => 73,
        "numpaddecimal" => 83,
        "numpadadd" => 78,
        "numpadsubtract" => 74,
        "numpadmultiply" => 55,
        "numpaddivide" => 98,
        "numpadenter" => 96,
        "numpadequal" => 117,
        _ => 0,
    };
    if special != 0 {
        return Some((special, false));
    }
    let mut chars = name.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    if c.is_ascii_alphabetic() {
        let codes = [
            30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47,
            17, 45, 21, 44,
        ];
        return Some((
            codes[(c.to_ascii_lowercase() as u8 - b'a') as usize],
            c.is_ascii_uppercase(),
        ));
    }
    for (plain, shifted, code) in [
        ('0', ')', 11),
        ('1', '!', 2),
        ('2', '@', 3),
        ('3', '#', 4),
        ('4', '$', 5),
        ('5', '%', 6),
        ('6', '^', 7),
        ('7', '&', 8),
        ('8', '*', 9),
        ('9', '(', 10),
        ('-', '_', 12),
        ('=', '+', 13),
        ('[', '{', 26),
        (']', '}', 27),
        (';', ':', 39),
        ('\'', '"', 40),
        ('`', '~', 41),
        ('\\', '|', 43),
        (',', '<', 51),
        ('.', '>', 52),
        ('/', '?', 53),
        (' ', ' ', 57),
    ] {
        if c == plain || c == shifted {
            return Some((code, c != plain));
        }
    }
    None
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mapping_and_shift_safety() {
        assert_eq!(key("a"), Some((30, false)));
        assert_eq!(key("!"), Some((2, true)));
        assert_eq!(key("numpad1"), Some((79, false)));
        assert_eq!(key("f12"), None);
        assert_eq!(key("é"), None);
        let mut control = Control::default();
        assert_eq!(
            control
                .key("A", [false; 4])
                .iter()
                .map(|e| (e.kind, e.x_or_key))
                .collect::<Vec<_>>(),
            vec![(8, 42), (8, 30), (9, 30), (9, 42)]
        );
        assert_eq!(control.key("A", [true, false, false, false]).len(), 3);
        assert_eq!(control.key("A", [true, false, false, false]).len(), 2);
        assert_eq!(control.modifiers([false; 4])[0].kind, 9);
        control.clear();
        assert!(control.modifiers([false; 4]).is_empty());
    }
}
