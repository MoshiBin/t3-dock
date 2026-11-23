//! Start at login, as a value under the user's Run key. Off unless the user turns it on.

use crate::win;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_NAME: &str = "t3-dock";

/// The Run value that starts this exe.
fn command() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(format!("\"{}\"", exe.to_str()?))
}

pub fn is_on() -> bool {
    command().is_some_and(|command| win::registry_string(RUN_KEY, RUN_NAME) == Some(command))
}

/// Only ever called for something the user clicked.
pub fn set(on: bool) -> bool {
    if on {
        command().is_some_and(|command| win::set_registry_string(RUN_KEY, RUN_NAME, &command))
    } else {
        win::delete_registry_value(RUN_KEY, RUN_NAME)
    }
}
