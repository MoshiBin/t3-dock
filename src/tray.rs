//! The tray icon and its menu, on a thread of their own with a Win32 message loop.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::thread::JoinHandle;
use std::time::Duration;

use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder};

use crate::log::log;
use crate::{login, t3, win};

const POLL: Duration = Duration::from_millis(250);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);
pub const ICON_SIZE: u32 = 32;

pub struct Tray {
    status: Sender<String>,
    closed: Receiver<()>,
}

impl Tray {
    /// A failure is logged and leaves the rest of the app working.
    pub fn start(t3_home: PathBuf, data: PathBuf) -> Self {
        let (status, updates) = channel();
        let (closing, closed) = channel();
        std::thread::spawn(move || {
            if let Err(error) = serve(&t3_home, &data, &updates) {
                log!("tray: {error}");
            }
            drop(closing);
        });
        Self { status, closed }
    }

    pub fn set_status(&self, text: String) {
        let _ = self.status.send(text);
    }

    /// Removes the icon, so none is left behind for a process that has ended.
    pub fn close(self) {
        drop(self.status);
        if let Err(RecvTimeoutError::Timeout) = self.closed.recv_timeout(CLOSE_TIMEOUT) {
            log!("tray: did not close in time");
        }
    }
}

/// Only ever called for a click on the menu item.
fn toggle_login() {
    let turn_on = !login::is_on();
    let done = login::set(turn_on);
    log!("tray: start at login on={turn_on} ok={done}");
}

/// A process of its own, so the window's event loop stays out of the dock.
fn open_settings() {
    // An open window raises itself instead, which the foreground lock would otherwise stop.
    win::allow_foreground();
    let started = std::env::current_exe()
        .and_then(|exe| std::process::Command::new(exe).arg("settings").spawn());
    if started.is_err() {
        log!("tray: cannot open the settings");
    }
}

fn serve(t3_home: &Path, data: &Path, updates: &Receiver<String>) -> Result<(), String> {
    let menu = Menu::new();
    let status = MenuItem::new("Starting", false, None);
    let settings = MenuItem::new("Settings...", true, None);
    let token = MenuItem::new("Set up or renew T3 token", true, None);
    let open_log = MenuItem::new("Open log", true, None);
    let login = CheckMenuItem::new("Start at login", true, login::is_on(), None);
    let quit = MenuItem::new("Quit", true, None);
    menu.append_items(&[
        &status,
        &PredefinedMenuItem::separator(),
        &settings,
        &token,
        &open_log,
        &login,
        &PredefinedMenuItem::separator(),
        &quit,
    ])
    .map_err(|_| "cannot build the menu")?;

    let icon = Icon::from_rgba(icon_pixels(), ICON_SIZE, ICON_SIZE)
        .map_err(|_| "cannot build the icon")?;
    let tray = TrayIconBuilder::new()
        .with_icon(icon)
        .with_tooltip("t3-dock")
        .with_menu(Box::new(menu))
        .build()
        .map_err(|_| "cannot create the icon")?;
    log!("tray: ready");

    let mut setup: Option<JoinHandle<()>> = None;
    loop {
        win::pump_messages(POLL);

        loop {
            match updates.try_recv() {
                Ok(text) => {
                    status.set_text(&text);
                    let _ = tray.set_tooltip(Some(format!("t3-dock: {text}")));
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }

        // The settings window can change it too.
        let login_on = login::is_on();
        if login.is_checked() != login_on {
            login.set_checked(login_on);
        }

        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == *settings.id() {
                open_settings();
            } else if event.id == *quit.id() {
                log!("tray: quit");
                win::signal_stop();
            } else if event.id == *open_log.id() {
                let opened = data
                    .join("t3-dock.log")
                    .to_str()
                    .is_some_and(win::shell_open);
                if !opened {
                    log!("tray: cannot open the log");
                }
            } else if event.id == *login.id() {
                toggle_login();
                // The menu flips its own check mark, so show what the registry really says.
                login.set_checked(login::is_on());
            } else if event.id == *token.id() && setup.is_none() {
                token.set_enabled(false);
                let (home, data) = (t3_home.to_owned(), data.to_owned());
                setup = Some(std::thread::spawn(move || {
                    if let Err(error) = t3::auth::setup(&home, &data) {
                        log!("auth: {error}");
                    }
                }));
            }
        }

        if setup.as_ref().is_some_and(JoinHandle::is_finished) {
            setup = None;
            token.set_enabled(true);
        }
    }
}

/// Six little keys, the top-left one lit.
pub fn icon_pixels() -> Vec<u8> {
    let mut pixels = Vec::with_capacity((ICON_SIZE * ICON_SIZE * 4) as usize);
    for y in 0..ICON_SIZE {
        for x in 0..ICON_SIZE {
            let column = (0..3).find(|i| (2 + i * 10..10 + i * 10).contains(&x));
            let row = (0..2).find(|i| (6 + i * 11..15 + i * 11).contains(&y));
            pixels.extend_from_slice(&match (column, row) {
                (Some(0), Some(0)) => [255, 176, 32, 255],
                (Some(_), Some(_)) => [60, 140, 255, 255],
                _ => [0, 0, 0, 0],
            });
        }
    }
    pixels
}
