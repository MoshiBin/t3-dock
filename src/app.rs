//! What the keys show and what the controls do.
//!
//! Screen keys open their thread. The other keys and the dials do what the user's bindings say
//! (`config.rs`). By default the keys without screens open a new thread, open the next thread
//! that needs the user, and settle the thread T3 shows. The top dial scrolls a row and goes home
//! when pressed. The left dial sets the brightness and resets it when pressed. The right dial
//! picks a project and shows project names while held.
//!
//! A model with a side display, such as the 293S, shows notices and counts there instead of on
//! key 1. It has no other controls, so its keys only open threads.

use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use jiff::Timestamp;

use crate::board::{Board, Entry, NEEDS_USER};
use crate::config::{self, Config, Press, Turn};
use crate::focus::Shortcut;
use crate::input::{Action, Control, Direction};
use crate::model::{Layout, Model};
use crate::render::{Accent, Tile};
use crate::status::{self, Status};
use crate::t3::{Phase, View, split_key};

/// Idle and monitoring threads.
const IDLE_URGENCY: u8 = 4;
const WORKING_URGENCY: u8 = 2;

#[derive(Clone, PartialEq, Eq)]
pub struct Screen {
    /// The model the tiles are for. A device of another model waits for its own.
    pub layout: Layout,
    /// The screen keys in reading order.
    pub tiles: Vec<Tile>,
    /// The side display, from the top.
    pub side: Vec<Tile>,
    pub brightness: u8,
    /// A change makes the device redraw everything, as after sleep.
    pub generation: u64,
}

/// What a press opens. No `Debug`: both ids are private.
pub struct Target {
    pub environment_id: String,
    pub thread_id: String,
}

/// What the focus worker does for a press.
pub enum Command {
    Open(Target),
    Send(Shortcut),
}

/// A relevant thread.
struct Ranked {
    entry: Entry,
    project: Option<String>,
}

pub struct App {
    view: View,
    ranked: Vec<Ranked>,
    /// One per project filter, so leaving a filter brings back the same keys.
    boards: HashMap<Option<String>, Board>,
    filter: Option<String>,
    offset: usize,
    layout: Layout,
    held: Vec<bool>,
    showing_projects: bool,
    /// Briefly covers key 1, or the top of the side display.
    message: Option<(Tile, Instant)>,
    last_needing: Option<String>,
    brightness: u8,
    generation: u64,
    /// When the threads were last ranked. Ended snoozes depend on it.
    now: Timestamp,
    config: Config,
}

impl App {
    pub fn new(brightness: u8, config: Config) -> Self {
        let layout = Model::N3.layout();
        Self {
            layout,
            view: View::default(),
            ranked: Vec::new(),
            boards: HashMap::new(),
            filter: None,
            offset: 0,
            held: vec![false; layout.keys],
            showing_projects: false,
            message: None,
            last_needing: None,
            brightness: brightness.clamp(config::BRIGHTNESS_RANGE.0, config::BRIGHTNESS_RANGE.1),
            generation: 0,
            now: Timestamp::UNIX_EPOCH,
            config,
        }
    }

    /// New bindings or preferences, from the settings window.
    pub fn set_config(&mut self, config: Config, now: Timestamp) {
        self.config = config;
        // The control holding the names down may do something else now.
        self.showing_projects = false;
        self.refresh(now);
    }

    /// The connected device's layout. Another model starts over from home.
    pub fn set_layout(&mut self, layout: Layout) {
        if layout == self.layout {
            return;
        }
        self.layout = layout;
        self.boards.clear();
        self.offset = 0;
        self.held = vec![false; layout.keys];
        self.message = None;
        self.lay_out();
    }

    /// Whether messages cover key 1, because there is no side display.
    fn covers_key(&self) -> bool {
        self.layout.side == 0
    }

    fn message_time(&self) -> Duration {
        Duration::from_millis(self.config.preferences.message_ms.into())
    }

    pub fn update(&mut self, view: View, now: Timestamp) {
        self.view = view;
        self.refresh(now);
    }

    /// Also needed as time passes, because snoozes end.
    pub fn refresh(&mut self, now: Timestamp) {
        // While T3 is away the boards are kept, so its restart doesn't shuffle the keys.
        if self.view.phase != Phase::Live {
            return;
        }
        self.now = now;
        let requests = status::child_requests(&self.view.threads);
        let hide_idle = self.config.preferences.hide_idle;
        let mut ranked: Vec<_> = self
            .view
            .threads
            .values()
            .filter_map(|thread| {
                let child = requests.get(thread.id.as_str()).copied();
                if !status::is_relevant(thread, &self.view.threads, now, child) {
                    return None;
                }
                let status = status::classify(thread, child, now);
                (!hide_idle || status.urgency() < IDLE_URGENCY).then_some((status, thread))
            })
            .collect();
        // T3's sidebar order, so the keys start out as T3 lists the threads.
        ranked.sort_by(|(a_status, a), (b_status, b)| {
            (status::sidebar_order(a, *a_status), &a.id)
                .cmp(&(status::sidebar_order(b, *b_status), &b.id))
        });
        self.ranked = ranked
            .into_iter()
            .map(|(status, thread)| Ranked {
                entry: Entry {
                    id: thread.id.clone(),
                    urgency: status.urgency(),
                },
                project: thread.project_id.clone(),
            })
            .collect();
        self.lay_out();
    }

    /// Keys only move at home. A page the user scrolled to stays as it is.
    fn lay_out(&mut self) {
        if self.view.phase != Phase::Live {
            return;
        }
        let entries: Vec<Entry> = self
            .ranked
            .iter()
            .filter(|ranked| self.filter.is_none() || ranked.project == self.filter)
            .map(|ranked| ranked.entry.clone())
            .collect();
        let Layout { keys, columns, .. } = self.layout;
        let board = self
            .boards
            .entry(self.filter.clone())
            .or_insert_with(|| Board::new(keys, columns));
        if self.offset > 0 {
            board.freeze(&entries);
            self.offset = self.offset.min(board.last_offset());
        }
        if self.offset == 0 {
            board.settle(&entries);
        }
    }

    pub fn redraw(&mut self) {
        self.generation += 1;
    }

    pub fn release_all(&mut self) {
        self.held.fill(false);
        self.showing_projects = false;
    }

    /// Expires the message.
    pub fn tick(&mut self, now: Instant) {
        if self
            .message
            .as_ref()
            .is_some_and(|(_, until)| *until <= now)
        {
            self.message = None;
        }
    }

    pub fn brightness(&self) -> u8 {
        self.brightness
    }

    pub fn input(&mut self, action: Action, now: Instant) -> Option<Command> {
        self.tick(now);
        match action {
            Action::Down(Control::Screen(key)) => {
                if std::mem::replace(self.held.get_mut(key)?, true) {
                    return None;
                }
                // A covered key doesn't open the thread that is hidden under the message.
                if key == 0 && self.covers_key() && self.message.take().is_some() {
                    return None;
                }
                let id = self.thread_on(key)?.to_owned();
                return Some(Command::Open(self.target(id)));
            }
            Action::Up(Control::Screen(key)) => {
                if let Some(held) = self.held.get_mut(key) {
                    *held = false;
                }
            }
            Action::Down(control) => return self.press(self.binding(control), now),
            Action::Up(control) => {
                if self.binding(control) == Press::ShowProjects {
                    self.showing_projects = false;
                }
            }
            Action::Turn(dial, direction) => {
                let binding = self.config.bindings.dial(dial);
                let (turn, direction) = (binding.turn, binding.direction(direction));
                self.turn(turn, direction, now);
            }
            Action::Unknown => {}
        }
        None
    }

    fn binding(&self, control: Control) -> Press {
        match control {
            Control::Screen(_) => Press::Nothing,
            Control::Button(index) => self.config.bindings.key(index),
            Control::Dial(dial) => self.config.bindings.dial(dial).press,
        }
    }

    fn press(&mut self, press: Press, now: Instant) -> Option<Command> {
        match press {
            Press::Nothing => {}
            Press::NewThread => return Some(Command::Send(Shortcut::NewThread)),
            Press::Settle => return Some(Command::Send(Shortcut::Settle)),
            Press::PreviousPage => self.scroll(Direction::Left, self.layout.keys),
            Press::NextPage => self.scroll(Direction::Right, self.layout.keys),
            Press::PreviousRow => self.scroll(Direction::Left, self.layout.columns),
            Press::NextRow => self.scroll(Direction::Right, self.layout.columns),
            Press::NeedsYou => return self.next_needing_user(now).map(Command::Open),
            Press::Home => {
                self.filter = None;
                self.offset = 0;
                self.message = None;
                self.lay_out();
            }
            Press::ResetBrightness => {
                self.set_brightness(self.config.preferences.default_brightness, now);
            }
            Press::ShowProjects => {
                self.showing_projects = true;
                self.message = None;
            }
            Press::PreviousProject => self.cycle_project(Direction::Left, now),
            Press::NextProject => self.cycle_project(Direction::Right, now),
        }
        None
    }

    fn turn(&mut self, turn: Turn, direction: Direction, now: Instant) {
        match turn {
            Turn::Nothing => {}
            Turn::ScrollRows => self.scroll(direction, self.layout.columns),
            Turn::ScrollPages => self.scroll(direction, self.layout.keys),
            Turn::Brightness => {
                let step = self.config.preferences.brightness_step;
                let next = match direction {
                    Direction::Left => self.brightness.saturating_sub(step),
                    Direction::Right => self.brightness.saturating_add(step),
                };
                self.set_brightness(next, now);
            }
            Turn::Project => self.cycle_project(direction, now),
        }
    }

    /// A remote thread's key names its environment. Any other thread is local.
    fn target(&self, key: String) -> Target {
        match split_key(&key) {
            Some((environment, thread)) => Target {
                environment_id: environment.to_owned(),
                thread_id: thread.to_owned(),
            },
            None => Target {
                environment_id: self.view.environment_id.clone(),
                thread_id: key,
            },
        }
    }

    fn scroll(&mut self, direction: Direction, by: usize) {
        if self.view.phase != Phase::Live {
            return;
        }
        let Some(board) = self.boards.get(&self.filter) else {
            return;
        };
        self.offset = match direction {
            Direction::Left => self.offset.saturating_sub(by),
            Direction::Right => (self.offset + by).min(board.last_offset()),
        };
        self.message = None;
        self.lay_out();
    }

    fn set_brightness(&mut self, percent: u8, now: Instant) {
        self.brightness = percent.clamp(config::BRIGHTNESS_RANGE.0, config::BRIGHTNESS_RANGE.1);
        let tile = Tile::notice("BRIGHT", &format!("{}%", self.brightness));
        self.message = Some((tile, now + self.message_time()));
    }

    fn project_name(&self, project: Option<&str>) -> &str {
        project
            .and_then(|id| self.view.projects.get(id))
            .map_or("No project", String::as_str)
    }

    fn cycle_project(&mut self, direction: Direction, now: Instant) {
        if self.view.phase != Phase::Live {
            return;
        }
        let used: BTreeSet<&str> = self
            .ranked
            .iter()
            .filter_map(|ranked| ranked.project.as_deref())
            .collect();
        let mut projects: Vec<&str> = used.into_iter().collect();
        projects.sort_by_key(|id| self.project_name(Some(id)).to_lowercase());
        let options: Vec<Option<&str>> = std::iter::once(None)
            .chain(projects.into_iter().map(Some))
            .collect();

        let current = options
            .iter()
            .position(|option| *option == self.filter.as_deref())
            .unwrap_or(0);
        let next = match direction {
            Direction::Left => (current + options.len() - 1) % options.len(),
            Direction::Right => (current + 1) % options.len(),
        };
        let filter = options[next].map(str::to_owned);
        let name = match &filter {
            Some(id) => self.project_name(Some(id)),
            None => "All projects",
        };
        self.message = Some((Tile::notice("PROJECT", name), now + self.message_time()));
        self.filter = filter;
        self.offset = 0;
        self.lay_out();
    }

    /// Cycles through threads that need the user, across all projects and pages.
    fn next_needing_user(&mut self, now: Instant) -> Option<Target> {
        if self.view.phase != Phase::Live {
            return None;
        }
        let mut waiting: Vec<&str> = self
            .ranked
            .iter()
            .filter(|ranked| ranked.entry.urgency == NEEDS_USER)
            .map(|ranked| ranked.entry.id.as_str())
            .collect();
        waiting.sort_unstable();
        let next = match &self.last_needing {
            Some(last) => waiting
                .iter()
                .find(|id| **id > last.as_str())
                .or(waiting.first()),
            None => waiting.first(),
        };
        let Some(id) = next.map(|id| (*id).to_owned()) else {
            let tile = Tile::notice("ALL CLEAR", "Nothing needs you");
            self.message = Some((tile, now + self.message_time()));
            return None;
        };
        self.message = None;
        self.last_needing = Some(id.clone());
        Some(self.target(id))
    }

    /// Keys are only pressable while the stream is live.
    fn thread_on(&self, key: usize) -> Option<&str> {
        if self.view.phase != Phase::Live {
            return None;
        }
        let id = self.boards.get(&self.filter)?.get(self.offset + key)?;
        self.view.threads.contains_key(id).then_some(id)
    }

    fn statuses(&self) -> Vec<Option<Status>> {
        let requests = status::child_requests(&self.view.threads);
        (0..self.layout.keys)
            .map(|key| {
                let thread = self.view.threads.get(self.thread_on(key)?)?;
                Some(status::classify(
                    thread,
                    requests.get(thread.id.as_str()).copied(),
                    self.now,
                ))
            })
            .collect()
    }

    /// What is on screen, without titles, names or ids.
    pub fn summary(&self) -> String {
        let keys: Vec<String> = self
            .statuses()
            .iter()
            .map(|status| status.map_or("-".to_owned(), |status| format!("{status:?}")))
            .collect();
        format!(
            "keys: {} (offset={} filtered={} projects_shown={})",
            keys.join(" "),
            self.offset,
            self.filter.is_some(),
            self.showing_projects
        )
    }

    fn notice(&self) -> Option<Tile> {
        match self.view.phase {
            Phase::Live if (0..self.layout.keys).all(|key| self.thread_on(key).is_none()) => {
                Some(match &self.filter {
                    Some(id) => Tile::notice("NO THREADS", self.project_name(Some(id))),
                    None => Tile::notice("T3", "No threads"),
                })
            }
            Phase::Live => None,
            Phase::Connecting => Some(Tile::notice("T3", "Connecting")),
            Phase::Offline => Some(Tile::notice("OFFLINE", "T3 Code not reachable")),
            Phase::NeedsToken => Some(Tile::notice("NO TOKEN", "Use the tray menu")),
            Phase::Unsupported => Some(Tile::notice("T3", "Unsupported T3 version")),
        }
    }

    pub fn screen(&self) -> Screen {
        let cover = self
            .message
            .as_ref()
            .map(|(tile, _)| tile.clone())
            .or_else(|| self.notice());
        let statuses = self.statuses();
        let covers_key = self.covers_key();
        let tiles = (0..self.layout.keys).map(|key| {
            let thread = self.thread_on(key).and_then(|id| self.view.threads.get(id));
            let mut tile = match (&cover, thread, statuses[key]) {
                (Some(cover), _, _) if key == 0 && covers_key => cover.clone(),
                (_, Some(thread), Some(status)) => {
                    let text = if self.showing_projects {
                        self.project_name(thread.project_id.as_deref())
                    } else {
                        thread.title.as_deref().unwrap_or_default()
                    };
                    Tile::thread(text, status)
                }
                _ => Tile::blank(),
            };
            tile.pressed = self.held[key];
            tile
        });
        Screen {
            layout: self.layout,
            tiles: tiles.collect(),
            side: self.side(cover),
            brightness: self.brightness,
            generation: self.generation,
        }
    }

    /// The notice or message on top, then how many threads need the user, are working, and
    /// have no key on this page.
    fn side(&self, cover: Option<Tile>) -> Vec<Tile> {
        let mut side = Vec::with_capacity(self.layout.side);
        if self.layout.side == 0 {
            return side;
        }
        let count = |urgency| {
            self.ranked
                .iter()
                .filter(|ranked| ranked.entry.urgency == urgency)
                .count()
        };
        side.push(
            cover.unwrap_or_else(|| Tile::count("NEEDS YOU", count(NEEDS_USER), Accent::Attention)),
        );
        if self.view.phase == Phase::Live {
            side.push(Tile::count("WORKING", count(WORKING_URGENCY), Accent::Busy));
            let shown = (0..self.layout.keys)
                .filter(|key| self.thread_on(*key).is_some())
                .count();
            let listed = self
                .ranked
                .iter()
                .filter(|ranked| self.filter.is_none() || ranked.project == self.filter)
                .count();
            let more = listed.saturating_sub(shown);
            side.push(Tile::count("MORE", more, Accent::Plain));
        }
        side.resize(self.layout.side, Tile::blank());
        side
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::{Value, json};

    use super::*;
    use crate::config::Preferences;
    use crate::input::Dial;
    use crate::t3::wire::Thread;

    const NEW_THREAD: usize = 0;
    const NEEDS_YOU: usize = 1;
    const SETTLE: usize = 2;
    const MESSAGE_TIME: Duration = Duration::from_millis(1500);
    const DEFAULT_BRIGHTNESS: u8 = 60;
    const MIN_BRIGHTNESS: u8 = config::BRIGHTNESS_RANGE.0;

    fn view(phase: Phase, threads: &[Value]) -> View {
        let threads = threads
            .iter()
            .map(|value| {
                let thread: Thread = serde_json::from_value(value.clone()).ok().unwrap();
                (thread.id.clone(), Arc::new(thread))
            })
            .collect();
        View {
            phase,
            environment_id: "env-invented".into(),
            threads: Arc::new(threads),
            projects: Arc::new(HashMap::from([
                ("p1".to_owned(), "Alpha".to_owned()),
                ("p2".to_owned(), "Beta".to_owned()),
            ])),
            health: Default::default(),
        }
    }

    fn idle(id: &str, updated: &str) -> Value {
        json!({"id": id, "title": id, "status": "completed", "latestRunId": "r",
               "projectId": "p1", "updatedAt": updated})
    }

    fn working(id: &str) -> Value {
        json!({"id": id, "title": id, "status": "running", "latestRunId": "r", "projectId": "p2"})
    }

    fn asking(id: &str) -> Value {
        json!({"id": id, "title": id, "status": "running", "latestRunId": "r", "projectId": "p2",
               "pendingRuntimeRequest": {"kind": "command"}})
    }

    /// Idle threads `t00`, `t01`, ... with the first the most recent.
    fn many(count: usize) -> Vec<Value> {
        (0..count)
            .map(|i| {
                idle(
                    &format!("t{i:02}"),
                    &format!("2030-01-{:02}T00:00:00Z", 28 - i),
                )
            })
            .collect()
    }

    fn app_with(threads: &[Value]) -> App {
        let mut app = App::new(DEFAULT_BRIGHTNESS, Config::default());
        app.update(view(Phase::Live, threads), Timestamp::UNIX_EPOCH);
        app
    }

    fn act(app: &mut App, action: Action) -> Option<String> {
        match app.input(action, Instant::now())? {
            Command::Open(target) => Some(target.thread_id),
            Command::Send(_) => None,
        }
    }

    fn press(app: &mut App, key: usize) -> Option<String> {
        let opened = act(app, Action::Down(Control::Screen(key)));
        act(app, Action::Up(Control::Screen(key)));
        opened
    }

    fn button(app: &mut App, button: usize) -> Option<String> {
        act(app, Action::Down(Control::Button(button)))
    }

    #[test]
    fn urgent_then_recent_and_each_key_opens_its_own_thread() {
        let mut app = app_with(&[
            idle("old", "2030-01-01T00:00:00Z"),
            idle("new", "2030-01-02T00:00:00Z"),
            working("busy"),
        ]);
        assert_eq!(press(&mut app, 0).as_deref(), Some("busy"));
        assert_eq!(press(&mut app, 1).as_deref(), Some("new"));
        assert_eq!(press(&mut app, 2).as_deref(), Some("old"));
        assert_eq!(press(&mut app, 3), None);
    }

    #[test]
    fn subagents_settled_and_archived_threads_get_no_key() {
        let mut app = app_with(&[
            json!({"id": "child", "lineage": {"relationshipToParent": "subagent"}}),
            json!({"id": "settled", "settledOverride": "settled"}),
            json!({"id": "archived", "archivedAt": "2030-01-01T00:00:00Z"}),
        ]);
        assert_eq!(press(&mut app, 0), None);
    }

    #[test]
    fn a_remote_thread_opens_in_its_own_environment() {
        let key = crate::t3::merge_key_for_tests("env-remote", "thread:r");
        let mut app = app_with(&[working(&key), working("local")]);
        let targets: Vec<(String, String)> = (0..2)
            .filter_map(|position| {
                let id = app.thread_on(position)?.to_owned();
                let target = app.target(id);
                Some((target.environment_id, target.thread_id))
            })
            .collect();
        assert!(targets.contains(&("env-remote".into(), "thread:r".into())));
        assert!(targets.contains(&("env-invented".into(), "local".into())));
        assert!(press(&mut app, 0).is_some());
    }

    #[test]
    fn holding_a_key_opens_it_once() {
        let mut app = app_with(&[working("busy")]);
        assert!(act(&mut app, Action::Down(Control::Screen(0))).is_some());
        assert!(act(&mut app, Action::Down(Control::Screen(0))).is_none());
    }

    #[test]
    fn a_dead_stream_is_not_pressable() {
        for phase in [
            Phase::Connecting,
            Phase::Offline,
            Phase::NeedsToken,
            Phase::Unsupported,
        ] {
            let mut app = app_with(&[asking("busy")]);
            app.update(view(phase, &[asking("busy")]), Timestamp::UNIX_EPOCH);
            assert_eq!(press(&mut app, 0), None);
            assert_eq!(button(&mut app, NEEDS_YOU), None);
        }
    }

    #[test]
    fn a_t3_restart_does_not_shuffle_keys() {
        let mut app = app_with(&[working("first"), idle("second", "2030-01-01T00:00:00Z")]);
        app.update(view(Phase::Offline, &[]), Timestamp::UNIX_EPOCH);
        app.update(
            view(
                Phase::Live,
                &[idle("first", "2029-01-01T00:00:00Z"), working("second")],
            ),
            Timestamp::UNIX_EPOCH,
        );
        assert_eq!(press(&mut app, 0).as_deref(), Some("first"));
        assert_eq!(press(&mut app, 1).as_deref(), Some("second"));
    }

    #[test]
    fn scrolling_stops_at_both_ends() {
        let mut app = app_with(&many(14));
        let turn = |app: &mut App, direction| act(app, Action::Turn(Dial::Top, direction));
        for _ in 0..2 {
            turn(&mut app, Direction::Right);
        }
        assert_eq!(press(&mut app, 0).as_deref(), Some("t06"));
        turn(&mut app, Direction::Right);
        // The last page is full rows, so it starts at the third row.
        assert_eq!(press(&mut app, 0).as_deref(), Some("t09"));
        turn(&mut app, Direction::Right);
        assert_eq!(press(&mut app, 0).as_deref(), Some("t09"));
        assert_eq!(press(&mut app, 4).as_deref(), Some("t13"));
        assert_eq!(press(&mut app, 5), None);
        for _ in 0..5 {
            turn(&mut app, Direction::Left);
        }
        assert_eq!(press(&mut app, 0).as_deref(), Some("t00"));
    }

    #[test]
    fn the_outer_buttons_send_t3_shortcuts() {
        // Even while T3 is away: the focus worker finds out whether it is running.
        for phase in [Phase::Live, Phase::Offline] {
            let mut app = app_with(&[working("busy")]);
            app.update(view(phase, &[working("busy")]), Timestamp::UNIX_EPOCH);
            for (button, shortcut) in [
                (NEW_THREAD, Shortcut::NewThread),
                (SETTLE, Shortcut::Settle),
            ] {
                let sent = app.input(Action::Down(Control::Button(button)), Instant::now());
                assert!(matches!(sent, Some(Command::Send(s)) if s == shortcut));
                assert!(
                    app.input(Action::Up(Control::Button(button)), Instant::now())
                        .is_none()
                );
            }
        }
    }

    #[test]
    fn the_top_dial_scrolls_a_row_and_goes_home() {
        let mut app = app_with(&many(10));
        act(&mut app, Action::Turn(Dial::Top, Direction::Right));
        assert_eq!(press(&mut app, 0).as_deref(), Some("t03"));
        act(&mut app, Action::Down(Control::Dial(Dial::Top)));
        assert_eq!(press(&mut app, 0).as_deref(), Some("t00"));
    }

    #[test]
    fn one_page_does_not_scroll() {
        let mut app = app_with(&many(6));
        act(&mut app, Action::Turn(Dial::Top, Direction::Right));
        assert_eq!(press(&mut app, 0).as_deref(), Some("t00"));
    }

    #[test]
    fn nothing_moves_on_a_page_the_user_scrolled_to() {
        let mut threads = many(10);
        let mut app = app_with(&threads);
        for _ in 0..2 {
            act(&mut app, Action::Turn(Dial::Top, Direction::Right));
        }
        assert_eq!(press(&mut app, 2).as_deref(), Some("t08"));

        threads.push(working("new"));
        threads[8] = working("t08");
        app.update(view(Phase::Live, &threads), Timestamp::UNIX_EPOCH);
        assert_eq!(press(&mut app, 2).as_deref(), Some("t08"));
        assert_eq!(press(&mut app, 4).as_deref(), Some("new"));

        act(&mut app, Action::Down(Control::Dial(Dial::Top)));
        let home: Vec<String> = (0..app.layout.keys)
            .filter_map(|key| press(&mut app, key))
            .collect();
        assert!(home.contains(&"new".to_owned()) && home.contains(&"t08".to_owned()));
    }

    #[test]
    fn needs_you_cycles_and_ignores_the_filter() {
        let mut app = app_with(&[asking("ask-a"), asking("ask-b"), idle("other", "2030")]);
        act(&mut app, Action::Turn(Dial::Right, Direction::Right));
        assert_eq!(button(&mut app, NEEDS_YOU).as_deref(), Some("ask-a"));
        assert_eq!(button(&mut app, NEEDS_YOU).as_deref(), Some("ask-b"));
        assert_eq!(button(&mut app, NEEDS_YOU).as_deref(), Some("ask-a"));
    }

    #[test]
    fn projects_cycle_through_all_and_each() {
        let mut app = app_with(&[working("beta"), idle("alpha", "2030")]);
        let later = || Instant::now() + MESSAGE_TIME * 2;
        let first_key = |app: &mut App| {
            app.tick(later());
            press(app, 0)
        };
        assert_eq!(first_key(&mut app).as_deref(), Some("beta"));
        act(&mut app, Action::Turn(Dial::Right, Direction::Right));
        assert_eq!(first_key(&mut app).as_deref(), Some("alpha"));
        act(&mut app, Action::Turn(Dial::Right, Direction::Right));
        assert_eq!(first_key(&mut app).as_deref(), Some("beta"));
        assert_eq!(press(&mut app, 1), None);
        act(&mut app, Action::Turn(Dial::Right, Direction::Right));
        assert_eq!(press(&mut app, 1).as_deref(), Some("alpha"));
    }

    #[test]
    fn holding_the_right_dial_shows_project_names() {
        let mut app = app_with(&[working("thread title")]);
        act(&mut app, Action::Down(Control::Dial(Dial::Right)));
        assert!(app.screen().tiles[0] == Tile::thread("Beta", Status::Working));
        act(&mut app, Action::Up(Control::Dial(Dial::Right)));
        assert!(app.screen().tiles[0] == Tile::thread("thread title", Status::Working));
    }

    #[test]
    fn a_message_covers_key_one_without_opening_it() {
        let mut app = app_with(&[working("busy")]);
        assert_eq!(button(&mut app, NEEDS_YOU), None);
        assert!(app.screen().tiles[0] == Tile::notice("ALL CLEAR", "Nothing needs you"));
        assert_eq!(press(&mut app, 0), None);
        assert_eq!(press(&mut app, 0).as_deref(), Some("busy"));
    }

    #[test]
    fn brightness_stays_in_range() {
        let mut app = app_with(&[]);
        for _ in 0..30 {
            act(&mut app, Action::Turn(Dial::Left, Direction::Right));
        }
        assert_eq!(app.brightness(), 100);
        for _ in 0..30 {
            act(&mut app, Action::Turn(Dial::Left, Direction::Left));
        }
        assert_eq!(app.brightness(), MIN_BRIGHTNESS);
        act(&mut app, Action::Down(Control::Dial(Dial::Left)));
        assert_eq!(app.brightness(), DEFAULT_BRIGHTNESS);
    }

    #[test]
    fn rebound_controls_do_their_new_action() {
        let mut app = app_with(&many(10));
        let mut config = Config::default();
        config.bindings.left_key = Press::NextRow;
        config.bindings.top_dial.turn = Turn::ScrollPages;
        config.bindings.top_dial.reverse = true;
        config.bindings.right_dial.press = Press::Nothing;
        app.set_config(config, Timestamp::UNIX_EPOCH);

        // The left key, now next row.
        button(&mut app, NEW_THREAD);
        assert_eq!(press(&mut app, 0).as_deref(), Some("t03"));
        // Reversed, so a right turn goes back.
        act(&mut app, Action::Turn(Dial::Top, Direction::Right));
        assert_eq!(press(&mut app, 0).as_deref(), Some("t00"));
        act(&mut app, Action::Turn(Dial::Top, Direction::Left));
        assert_eq!(press(&mut app, 0).as_deref(), Some("t06"));
        act(&mut app, Action::Down(Control::Dial(Dial::Right)));
        assert!(!app.showing_projects);
    }

    #[test]
    fn a_new_config_releases_held_project_names() {
        let mut app = app_with(&[working("busy")]);
        act(&mut app, Action::Down(Control::Dial(Dial::Right)));
        assert!(app.showing_projects);
        let mut config = Config::default();
        config.bindings.right_dial.press = Press::Nothing;
        app.set_config(config, Timestamp::UNIX_EPOCH);
        assert!(!app.showing_projects);
    }

    #[test]
    fn hiding_idle_threads_frees_their_keys() {
        let mut app = app_with(&[idle("quiet", "2030"), working("busy")]);
        assert_eq!(press(&mut app, 1).as_deref(), Some("quiet"));
        let config = Config {
            preferences: Preferences {
                hide_idle: true,
                ..Preferences::default()
            },
            ..Config::default()
        };
        app.set_config(config, Timestamp::UNIX_EPOCH);
        assert_eq!(press(&mut app, 0).as_deref(), Some("busy"));
        assert_eq!(press(&mut app, 1), None);
    }

    fn wide_app_with(threads: &[Value]) -> App {
        let mut app = app_with(threads);
        app.set_layout(Model::Dock293sV3.layout());
        app
    }

    #[test]
    fn the_293s_shows_fifteen_threads_and_counts_beside_them() {
        let mut threads = many(17);
        threads.push(asking("ask"));
        threads.push(working("busy"));
        let mut app = wide_app_with(&threads);
        let screen = app.screen();
        assert_eq!((screen.tiles.len(), screen.side.len()), (15, 3));
        assert!(screen.side[0] == Tile::count("NEEDS YOU", 1, Accent::Attention));
        assert!(screen.side[1] == Tile::count("WORKING", 1, Accent::Busy));
        assert!(screen.side[2] == Tile::count("MORE", 4, Accent::Plain));
        assert_eq!(press(&mut app, 14).as_deref(), Some("t12"));
    }

    #[test]
    fn the_side_display_takes_notices_so_key_one_stays_pressable() {
        let mut app = wide_app_with(&[working("busy")]);
        assert_eq!(button(&mut app, NEEDS_YOU), None);
        let screen = app.screen();
        assert!(screen.side[0] == Tile::notice("ALL CLEAR", "Nothing needs you"));
        assert!(screen.tiles[0] == Tile::thread("busy", Status::Working));
        assert_eq!(press(&mut app, 0).as_deref(), Some("busy"));

        app.tick(Instant::now() + MESSAGE_TIME * 2);
        app.update(view(Phase::Offline, &[]), Timestamp::UNIX_EPOCH);
        let screen = app.screen();
        assert!(screen.side[0] == Tile::notice("OFFLINE", "T3 Code not reachable"));
        assert!(screen.side[1] == Tile::blank());
    }

    #[test]
    fn another_model_starts_from_home() {
        let mut app = app_with(&many(20));
        act(&mut app, Action::Turn(Dial::Top, Direction::Right));
        app.set_layout(Model::Dock293sV3.layout());
        assert_eq!(press(&mut app, 0).as_deref(), Some("t00"));
        assert_eq!(press(&mut app, 14).as_deref(), Some("t14"));
        app.set_layout(Model::N3.layout());
        assert_eq!(app.screen().tiles.len(), 6);
        assert_eq!(press(&mut app, 5).as_deref(), Some("t05"));
    }

    #[test]
    fn brightness_follows_the_preferences() {
        let mut app = app_with(&[]);
        let config = Config {
            preferences: Preferences {
                default_brightness: 80,
                brightness_step: 10,
                ..Preferences::default()
            },
            ..Config::default()
        };
        app.set_config(config, Timestamp::UNIX_EPOCH);
        act(&mut app, Action::Turn(Dial::Left, Direction::Left));
        assert_eq!(app.brightness(), DEFAULT_BRIGHTNESS - 10);
        act(&mut app, Action::Down(Control::Dial(Dial::Left)));
        assert_eq!(app.brightness(), 80);
    }
}
