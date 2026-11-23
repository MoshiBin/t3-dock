//! Opens a thread in T3 Code and brings its window to the front.
//!
//! A HID report is not user input to Windows. The deep link alone raises T3 only when it was
//! minimized, so we raise it ourselves first. That also makes a press feel immediate.
//!
//! New thread and settle have no link, and t3-dock's token is read-only, so they are T3's own
//! keyboard shortcuts, typed only while T3 is in front.

use std::time::{Duration, Instant};

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    VIRTUAL_KEY, VK_CONTROL, VK_N, VK_S, VK_SHIFT,
};

use crate::app::Target;
use crate::log::log;
use crate::win;

const FRONT_TIMEOUT: Duration = Duration::from_millis(1500);
const POLL: Duration = Duration::from_millis(75);

/// Encodes everything but RFC 3986's unreserved characters.
pub fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn deep_link(target: &Target) -> Option<String> {
    let segment = |id: &str| {
        // A dot segment would change the path, and encoding does not prevent that.
        (!id.is_empty() && id != "." && id != "..").then(|| percent_encode(id))
    };
    Some(format!(
        "t3code://threads/{}/{}",
        segment(&target.environment_id)?,
        segment(&target.thread_id)?
    ))
}

/// Logs what focusing would work with, without touching any window.
pub fn describe() {
    let Some(exe) = win::protocol_handler() else {
        log!("focus: no t3code:// handler is registered");
        return;
    };
    match win::main_window(&exe) {
        Some(window) => log!(
            "focus: T3 window found, minimized={}",
            window.is_minimized()
        ),
        None => log!("focus: T3 window not found"),
    }
}

/// Raises T3 if needed, then retries until it is in front, for at most 1.5 seconds from `started`.
/// `before_wait` runs after the first raise, such as opening a link. Returns the retries.
fn bring_to_front(exe: &str, started: Instant, before_wait: impl FnOnce() -> bool) -> Option<u32> {
    let raise = || {
        if let Some(window) = win::main_window(exe) {
            window.raise();
        }
    };
    if !win::is_foreground(exe) {
        raise();
    }
    if !before_wait() {
        return None;
    }
    // Windows sometimes refuses the first attempt, and T3 may still be restoring its window.
    let mut retries = 0;
    while !win::is_foreground(exe) {
        if started.elapsed() >= FRONT_TIMEOUT {
            log!("focus: T3 did not come to the front, retries={retries}");
            return None;
        }
        std::thread::sleep(POLL);
        if !win::is_foreground(exe) {
            raise();
            retries += 1;
        }
    }
    Some(retries)
}

/// Blocks until T3 is in front, for at most 1.5 seconds.
pub fn open(target: &Target) {
    let (Some(link), Some(exe)) = (deep_link(target), win::protocol_handler()) else {
        log!("focus: cannot build or handle the link");
        return;
    };
    let started = Instant::now();
    let was_front = win::is_foreground(&exe);
    let opened = || {
        let opened = win::shell_open(&link);
        if !opened {
            log!("focus: opening the link failed");
        }
        opened
    };
    if let Some(retries) = bring_to_front(&exe, started, opened) {
        log!(
            "focus: T3 in front, was_front={was_front} retries={retries} total={} ms",
            started.elapsed().as_millis()
        );
    }
}

/// A T3 keyboard shortcut. Both are T3's defaults and do nothing while its terminal has focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shortcut {
    /// Ctrl+N, "Chat: New", in the project T3 has open.
    NewThread,
    /// Ctrl+Shift+S, "Thread: Settle", on the thread T3 shows. It unsettles a settled one.
    Settle,
}

impl Shortcut {
    fn keys(self) -> &'static [VIRTUAL_KEY] {
        match self {
            Self::NewThread => &[VK_CONTROL, VK_N],
            Self::Settle => &[VK_CONTROL, VK_SHIFT, VK_S],
        }
    }

    /// Settling acts on whatever thread T3 shows, so it needs T3 already in front, where the
    /// user can see that thread. Otherwise a press only raises T3.
    fn needs_t3_in_front(self) -> bool {
        self == Self::Settle
    }
}

/// Sends `shortcut` to T3, and never to another window.
pub fn send(shortcut: Shortcut) {
    let Some(exe) = win::protocol_handler() else {
        log!("focus: no t3code:// handler is registered");
        return;
    };
    let started = Instant::now();
    let was_front = win::is_foreground(&exe);
    if bring_to_front(&exe, started, || true).is_none() {
        return;
    }
    if shortcut.needs_t3_in_front() && !was_front {
        log!("focus: {shortcut:?} skipped, T3 was not in front, raised it");
        return;
    }
    if !was_front {
        // Let the restored window take keyboard focus before typing into it.
        std::thread::sleep(POLL);
    }
    // Checked again right before sending, because the keys go to whatever is in front.
    if !win::is_foreground(&exe) {
        log!("focus: {shortcut:?} skipped, T3 lost the front");
        return;
    }
    if win::send_chord(shortcut.keys()) {
        log!(
            "focus: sent {shortcut:?}, was_front={was_front} total={} ms",
            started.elapsed().as_millis()
        );
    } else {
        log!("focus: sending {shortcut:?} failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(environment_id: &str, thread_id: &str) -> Option<String> {
        deep_link(&Target {
            environment_id: environment_id.into(),
            thread_id: thread_id.into(),
        })
    }

    #[test]
    fn both_ids_are_encoded() {
        assert_eq!(
            link("env-1", "thread:abc:2").as_deref(),
            Some("t3code://threads/env-1/thread%3Aabc%3A2")
        );
    }

    #[test]
    fn hostile_ids_cannot_escape_their_segment() {
        assert_eq!(
            link("env", "a/../b?c#d \"&|%").as_deref(),
            Some("t3code://threads/env/a%2F..%2Fb%3Fc%23d%20%22%26%7C%25")
        );
        assert!(link("env", "..").is_none());
        assert!(link("", "thread").is_none());
    }
}
