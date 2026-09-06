use evdev::{EventType, InputEvent};

pub const KEY_LEFT: u16 = 105;
pub const KEY_RIGHT: u16 = 106;
pub const CONTRACT_KEYS: [u16; 11] = [305, 304, 307, 308, 316, 314, 315, 103, 108, 105, 106];

pub fn focus_walk(count: usize) -> impl Iterator<Item = u16> {
    (0..count).map(|index| if index % 2 == 0 { KEY_RIGHT } else { KEY_LEFT })
}

#[must_use]
pub fn encoded_press(code: u16) -> [InputEvent; 2] {
    [
        InputEvent::new(EventType::KEY.0, code, 1),
        InputEvent::new(EventType::KEY.0, code, 0),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_walk_alternates_visible_focus_moves() {
        assert_eq!(focus_walk(5).collect::<Vec<_>>(), [106, 105, 106, 105, 106]);
    }

    #[test]
    fn encoding_is_a_key_press_then_release() {
        let events = encoded_press(KEY_RIGHT);
        assert_eq!(events[0].event_type(), EventType::KEY);
        assert_eq!(events[0].code(), KEY_RIGHT);
        assert_eq!(events[0].value(), 1);
        assert_eq!(events[1].code(), KEY_RIGHT);
        assert_eq!(events[1].value(), 0);
    }
}
