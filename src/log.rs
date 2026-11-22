//! A small log of events. Callers pass no titles, ids, tokens, URLs or message text.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

const ROTATE_AT: u64 = 1024 * 1024;

static FILE: OnceLock<Mutex<File>> = OnceLock::new();

pub fn init(directory: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(directory)?;
    let path = directory.join("t3-dock.log");
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > ROTATE_AT) {
        std::fs::rename(&path, directory.join("t3-dock.old.log"))?;
    }
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    let _ = FILE.set(Mutex::new(file));
    Ok(())
}

pub fn write(message: std::fmt::Arguments<'_>) {
    let time = jiff::ZonedDateTime::now().strftime("%Y-%m-%d %H:%M:%S%.3f");
    let line = format!("{time} {message}\n");
    if let Some(file) = FILE.get()
        && let Ok(mut file) = file.lock()
    {
        let _ = file.write_all(line.as_bytes());
    }
    // eprintln! panics without a usable stderr, which a background process may not have.
    let _ = std::io::stderr().write_all(line.as_bytes());
}

macro_rules! log {
    ($($arg:tt)*) => { $crate::log::write(format_args!($($arg)*)) };
}
pub(crate) use log;
