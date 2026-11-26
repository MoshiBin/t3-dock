//! What the controls do and the user's preferences, saved as `config.json` in the data folder.
//!
//! The settings window writes the file and the running app reloads it when it changes. A missing
//! or unreadable file, or any field in it, falls back to the defaults.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::input::{Dial, Direction};
use crate::log::log;

pub const FILE: &str = "config.json";

pub const BRIGHTNESS_RANGE: (u8, u8) = (10, 100);
pub const STEP_RANGE: (u8, u8) = (1, 25);
pub const MESSAGE_RANGE: (u32, u32) = (500, 5000);

/// What pressing a key without a screen, or a dial, does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Press {
    #[default]
    Nothing,
    /// T3's new-thread shortcut.
    NewThread,
    /// T3's settle shortcut, for the thread it shows.
    Settle,
    PreviousPage,
    NextPage,
    PreviousRow,
    NextRow,
    NeedsYou,
    Home,
    ResetBrightness,
    /// Project names replace titles while the control is held.
    ShowProjects,
    PreviousProject,
    NextProject,
}

impl Press {
    pub const ALL: [Self; 13] = [
        Self::Nothing,
        Self::NewThread,
        Self::Settle,
        Self::PreviousPage,
        Self::NextPage,
        Self::PreviousRow,
        Self::NextRow,
        Self::NeedsYou,
        Self::Home,
        Self::ResetBrightness,
        Self::ShowProjects,
        Self::PreviousProject,
        Self::NextProject,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Nothing => "Nothing",
            Self::NewThread => "New thread in T3",
            Self::Settle => "Settle the thread T3 shows",
            Self::PreviousPage => "Previous page",
            Self::NextPage => "Next page",
            Self::PreviousRow => "Previous row",
            Self::NextRow => "Next row",
            Self::NeedsYou => "Open next thread that needs you",
            Self::Home => "Home: first page, all projects",
            Self::ResetBrightness => "Reset brightness",
            Self::ShowProjects => "Show project names while held",
            Self::PreviousProject => "Previous project",
            Self::NextProject => "Next project",
        }
    }
}

/// What turning a dial does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Turn {
    #[default]
    Nothing,
    ScrollRows,
    ScrollPages,
    Brightness,
    Project,
}

impl Turn {
    pub const ALL: [Self; 5] = [
        Self::Nothing,
        Self::ScrollRows,
        Self::ScrollPages,
        Self::Brightness,
        Self::Project,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Nothing => "Nothing",
            Self::ScrollRows => "Scroll by a row",
            Self::ScrollPages => "Scroll by a page",
            Self::Brightness => "Brightness",
            Self::Project => "Show one project's threads",
        }
    }
}

/// What a tile on a side display shows, such as the 293S's. A notice from t3-dock, such as
/// "Offline", covers the top tile whatever it shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SideTile {
    #[default]
    Blank,
    NeedsYou,
    Working,
    /// Threads that have no key on this page.
    WithoutKey,
    /// What is left of the Devin ACU allowance. Needs a Devin sign-in.
    DevinAcus,
    Clock,
}

impl SideTile {
    pub const ALL: [Self; 6] = [
        Self::NeedsYou,
        Self::Working,
        Self::WithoutKey,
        Self::DevinAcus,
        Self::Clock,
        Self::Blank,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Blank => "Nothing",
            Self::NeedsYou => "Threads that need you",
            Self::Working => "Threads working",
            Self::WithoutKey => "Threads without a key",
            Self::DevinAcus => "Devin ACUs left",
            Self::Clock => "Clock",
        }
    }
}

pub const DEFAULT_SIDE: [SideTile; 3] =
    [SideTile::NeedsYou, SideTile::Working, SideTile::WithoutKey];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DialBinding {
    pub turn: Turn,
    pub press: Press,
    /// Swaps left and right turns.
    pub reverse: bool,
}

impl DialBinding {
    const fn new(turn: Turn, press: Press) -> Self {
        Self {
            turn,
            press,
            reverse: false,
        }
    }

    pub fn direction(&self, turned: Direction) -> Direction {
        match (self.reverse, turned) {
            (false, direction) => direction,
            (true, Direction::Left) => Direction::Right,
            (true, Direction::Right) => Direction::Left,
        }
    }
}

impl Default for DialBinding {
    fn default() -> Self {
        Self::new(Turn::Nothing, Press::Nothing)
    }
}

/// The keys without screens, left to right, and the three dials. Screen keys always open their
/// thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Bindings {
    pub left_key: Press,
    pub middle_key: Press,
    pub right_key: Press,
    pub top_dial: DialBinding,
    /// The bottom-left dial.
    pub left_dial: DialBinding,
    /// The bottom-right dial.
    pub right_dial: DialBinding,
}

impl Bindings {
    pub fn key(&self, index: usize) -> Press {
        match index {
            0 => self.left_key,
            1 => self.middle_key,
            2 => self.right_key,
            _ => Press::Nothing,
        }
    }

    pub fn dial(&self, dial: Dial) -> &DialBinding {
        match dial {
            Dial::Top => &self.top_dial,
            Dial::Left => &self.left_dial,
            Dial::Right => &self.right_dial,
        }
    }
}

impl Default for Bindings {
    fn default() -> Self {
        Self {
            left_key: Press::NewThread,
            middle_key: Press::NeedsYou,
            right_key: Press::Settle,
            top_dial: DialBinding::new(Turn::ScrollRows, Press::Home),
            left_dial: DialBinding::new(Turn::Brightness, Press::ResetBrightness),
            right_dial: DialBinding::new(Turn::Project, Press::ShowProjects),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    /// Percent. Where resetting the brightness goes.
    pub default_brightness: u8,
    /// Percent per dial click.
    pub brightness_step: u8,
    /// How long a message covers key 1.
    pub message_ms: u32,
    /// Idle and monitoring threads get no key.
    pub hide_idle: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            default_brightness: 60,
            brightness_step: 5,
            message_ms: 1500,
            hide_idle: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub bindings: Bindings,
    pub preferences: Preferences,
    /// The side display's tiles, from the top.
    pub side: [SideTile; 3],
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bindings: Bindings::default(),
            preferences: Preferences::default(),
            side: DEFAULT_SIDE,
        }
    }
}

impl Config {
    /// Brings hand-edited numbers into range.
    pub fn clamped(mut self) -> Self {
        let prefs = &mut self.preferences;
        prefs.default_brightness = prefs
            .default_brightness
            .clamp(BRIGHTNESS_RANGE.0, BRIGHTNESS_RANGE.1);
        prefs.brightness_step = prefs.brightness_step.clamp(STEP_RANGE.0, STEP_RANGE.1);
        prefs.message_ms = prefs.message_ms.clamp(MESSAGE_RANGE.0, MESSAGE_RANGE.1);
        self
    }

    pub fn parse(text: &str) -> Option<Self> {
        serde_json::from_str::<Self>(text).ok().map(Self::clamped)
    }

    /// The defaults when the file is missing or unreadable.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).unwrap_or_else(|| {
                log!("config: cannot parse {FILE}, using the defaults");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// Writes a temporary file and renames it, so a reader never sees half a file.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|_| "cannot encode the settings")?;
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, text).map_err(|_| "cannot write the settings")?;
        std::fs::rename(&temporary, path).map_err(|_| "cannot replace the settings".to_owned())
    }
}

/// Notices when the file changes.
pub struct Watch {
    path: PathBuf,
    seen: Option<SystemTime>,
}

impl Watch {
    pub fn new(path: PathBuf) -> Self {
        let seen = modified(&path);
        Self { path, seen }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The new settings, once per change.
    pub fn changed(&mut self) -> Option<Config> {
        let now = modified(&self.path);
        if now == self.seen {
            return None;
        }
        self.seen = now;
        Some(Config::load(&self.path))
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_the_documented_layout() {
        let bindings = Bindings::default();
        assert_eq!(bindings.key(0), Press::NewThread);
        assert_eq!(bindings.key(1), Press::NeedsYou);
        assert_eq!(bindings.key(2), Press::Settle);
        assert_eq!(bindings.dial(Dial::Top).turn, Turn::ScrollRows);
        assert_eq!(bindings.dial(Dial::Left).press, Press::ResetBrightness);
        assert_eq!(bindings.dial(Dial::Right).press, Press::ShowProjects);
    }

    #[test]
    fn missing_fields_keep_their_defaults() {
        let config = Config::parse(r#"{"bindings": {"middle_key": "home"}}"#).unwrap();
        assert_eq!(config.bindings.middle_key, Press::Home);
        assert_eq!(config.bindings.left_key, Press::NewThread);
        assert_eq!(config.preferences, Preferences::default());
    }

    #[test]
    fn side_tiles_are_kept() {
        assert_eq!(Config::default().side, DEFAULT_SIDE);
        let config = Config::parse(r#"{"side": ["clock", "devin_acus", "blank"]}"#).unwrap();
        assert_eq!(
            config.side,
            [SideTile::Clock, SideTile::DevinAcus, SideTile::Blank]
        );
        assert_eq!(config.bindings, Bindings::default());
    }

    #[test]
    fn numbers_are_clamped() {
        let config = Config::parse(
            r#"{"preferences": {"default_brightness": 0, "brightness_step": 200, "message_ms": 1}}"#,
        )
        .unwrap();
        assert_eq!(config.preferences.default_brightness, BRIGHTNESS_RANGE.0);
        assert_eq!(config.preferences.brightness_step, STEP_RANGE.1);
        assert_eq!(config.preferences.message_ms, MESSAGE_RANGE.0);
    }

    #[test]
    fn unknown_actions_are_rejected() {
        assert!(Config::parse(r#"{"bindings": {"left_key": "launch_rockets"}}"#).is_none());
    }

    #[test]
    fn reverse_swaps_directions() {
        let mut dial = DialBinding::new(Turn::Brightness, Press::Nothing);
        assert_eq!(dial.direction(Direction::Left), Direction::Left);
        dial.reverse = true;
        assert_eq!(dial.direction(Direction::Left), Direction::Right);
        assert_eq!(dial.direction(Direction::Right), Direction::Left);
    }

    #[test]
    fn save_and_watch_round_trip() {
        let folder = std::env::temp_dir().join(format!("t3-dock-config-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join(FILE);
        let mut watch = Watch::new(path.clone());
        assert_eq!(watch.changed(), None);

        let mut config = Config::default();
        config.preferences.hide_idle = true;
        config.bindings.top_dial.reverse = true;
        config.save(&path).unwrap();
        assert_eq!(watch.changed(), Some(config));
        assert_eq!(watch.changed(), None);
        assert_eq!(Config::load(&path), config);
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
