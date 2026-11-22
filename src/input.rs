//! Decodes the N3's input reports.
//!
//! Codes are from MiraBox's MIT SDK (`StreamDockN3.py`). Byte offsets exclude the HID report id.

use std::fmt;

pub const SCREEN_KEYS: usize = 6;

const CODE: usize = 9;
const STATE: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dial {
    Left,
    Top,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    /// A screen key, 0-based, with the same index as its image.
    Screen(usize),
    /// A key without a screen, 0-based.
    Button(usize),
    Dial(Dial),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Down(Control),
    Up(Control),
    Turn(Dial, Direction),
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub code: u8,
    pub state: u8,
    pub action: Action,
}

/// `None` for anything that is not an input, such as a write acknowledgement.
pub fn decode(report: &[u8]) -> Option<Event> {
    if !report.starts_with(b"ACK") {
        return None;
    }
    let (&code, &state) = (report.get(CODE)?, report.get(STATE)?);
    if code == 0 {
        return None;
    }

    let turn = |dial, direction| {
        if state == 0 {
            Action::Turn(dial, direction)
        } else {
            Action::Unknown
        }
    };
    let press = |control| match state {
        1 => Action::Down(control),
        0 => Action::Up(control),
        _ => Action::Unknown,
    };
    let action = match code {
        0x01..=0x06 => press(Control::Screen(usize::from(code - 1))),
        0x25 => press(Control::Button(0)),
        0x30 => press(Control::Button(1)),
        0x31 => press(Control::Button(2)),
        0x33 => press(Control::Dial(Dial::Left)),
        0x35 => press(Control::Dial(Dial::Top)),
        0x34 => press(Control::Dial(Dial::Right)),
        0x90 => turn(Dial::Left, Direction::Left),
        0x91 => turn(Dial::Left, Direction::Right),
        0x50 => turn(Dial::Top, Direction::Left),
        0x51 => turn(Dial::Top, Direction::Right),
        0x60 => turn(Dial::Right, Direction::Left),
        0x61 => turn(Dial::Right, Direction::Right),
        _ => Action::Unknown,
    };
    Some(Event {
        code,
        state,
        action,
    })
}

impl fmt::Display for Control {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Screen(index) => write!(f, "screen key {}", index + 1),
            Self::Button(index) => write!(f, "button {}", index + 1),
            Self::Dial(dial) => write!(f, "{dial:?} dial"),
        }
    }
}

/// The log form: raw bytes first, so a wrong mapping can still be diagnosed.
impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "code={:02x} state={} ", self.code, self.state)?;
        match self.action {
            Action::Down(control) => write!(f, "{control} down"),
            Action::Up(control) => write!(f, "{control} up"),
            Action::Turn(dial, direction) => write!(f, "{dial:?} dial turned {direction:?}"),
            Action::Unknown => f.write_str("unknown"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(code: u8, state: u8) -> Vec<u8> {
        let mut report = vec![0; 512];
        report[..3].copy_from_slice(b"ACK");
        report[5..7].copy_from_slice(b"OK");
        report[CODE] = code;
        report[STATE] = state;
        report
    }

    fn action(code: u8, state: u8) -> Action {
        decode(&report(code, state)).unwrap().action
    }

    #[test]
    fn every_key_and_dial_press_has_down_and_up() {
        for (code, control) in [
            (0x01, Control::Screen(0)),
            (0x02, Control::Screen(1)),
            (0x03, Control::Screen(2)),
            (0x04, Control::Screen(3)),
            (0x05, Control::Screen(4)),
            (0x06, Control::Screen(5)),
            (0x25, Control::Button(0)),
            (0x30, Control::Button(1)),
            (0x31, Control::Button(2)),
            (0x33, Control::Dial(Dial::Left)),
            (0x35, Control::Dial(Dial::Top)),
            (0x34, Control::Dial(Dial::Right)),
        ] {
            assert_eq!(action(code, 1), Action::Down(control));
            assert_eq!(action(code, 0), Action::Up(control));
        }
    }

    #[test]
    fn every_dial_turns_both_ways() {
        for (code, dial, direction) in [
            (0x90, Dial::Left, Direction::Left),
            (0x91, Dial::Left, Direction::Right),
            (0x50, Dial::Top, Direction::Left),
            (0x51, Dial::Top, Direction::Right),
            (0x60, Dial::Right, Direction::Left),
            (0x61, Dial::Right, Direction::Right),
        ] {
            assert_eq!(action(code, 0), Action::Turn(dial, direction));
            assert_eq!(action(code, 1), Action::Unknown);
        }
    }

    #[test]
    fn surprises_are_kept_for_the_log() {
        assert_eq!(action(0x07, 1), Action::Unknown);
        assert_eq!(action(0x01, 2), Action::Unknown);
        let event = decode(&report(0x7f, 9)).unwrap();
        assert_eq!(event.to_string(), "code=7f state=9 unknown");
    }

    #[test]
    fn other_reports_are_not_inputs() {
        assert_eq!(decode(&report(0, 0)), None);
        assert_eq!(decode(b"ACK\0\0OK"), None);
        assert_eq!(decode(&[0; 512]), None);
        let mut with_report_id = vec![0];
        with_report_id.extend(report(1, 1));
        assert_eq!(decode(&with_report_id), None);
    }
}
