//! What the keys show and what the controls do.
//!
//! The keys without screens are previous page, next thread that needs the user, and next page.
//! The top dial scrolls a row and goes home when pressed. The left dial sets the brightness and
//! resets it when pressed. The right dial picks a project and shows project names while held.

use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use jiff::Timestamp;

use crate::board::{Board, Entry, NEEDS_USER};
use crate::input::{Action, Control, Dial, Direction, SCREEN_KEYS};
use crate::render::Tile;
use crate::status::{self, Status};
use crate::t3::{Phase, View, split_key};

const PREVIOUS: usize = 0;
const NEEDS_YOU: usize = 1;
const NEXT: usize = 2;

const ROW: usize = 3;
const MESSAGE_TIME: Duration = Duration::from_millis(1500);

pub const DEFAULT_BRIGHTNESS: u8 = 60;
const MIN_BRIGHTNESS: u8 = 10;
const BRIGHTNESS_STEP: u8 = 5;

#[derive(Clone, PartialEq, Eq)]
pub struct Screen {
    pub tiles: [Tile; SCREEN_KEYS],
    pub brightness: u8,
    /// A change makes the device redraw everything, as after sleep.
    pub generation: u64,
}

/// What a press opens. No `Debug`: both ids are private.
pub struct Target {
    pub environment_id: String,
    pub thread_id: String,
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
    held: [bool; SCREEN_KEYS],
    showing_projects: bool,
    /// Briefly covers key 1.
    message: Option<(Tile, Instant)>,
    last_needing: Option<String>,
    brightness: u8,
    generation: u64,
    /// When the threads were last ranked. Ended snoozes depend on it.
    now: Timestamp,
}

impl App {
    pub fn new(brightness: u8) -> Self {
        Self {
            view: View::default(),
            ranked: Vec::new(),
            boards: HashMap::new(),
            filter: None,
            offset: 0,
            held: [false; SCREEN_KEYS],
            showing_projects: false,
            message: None,
            last_needing: None,
            brightness: brightness.clamp(MIN_BRIGHTNESS, 100),
            generation: 0,
            now: Timestamp::UNIX_EPOCH,
        }
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
        let mut ranked: Vec<_> = self
            .view
            .threads
            .values()
            .filter_map(|thread| {
                let child = requests.get(thread.id.as_str()).copied();
                status::is_relevant(thread, &self.view.threads, now, child)
                    .then(|| (status::classify(thread, child, now).urgency(), thread))
            })
            .collect();
        ranked.sort_by(|(a_urgency, a), (b_urgency, b)| {
            (a_urgency, Reverse(&a.updated_at), &a.id).cmp(&(
                b_urgency,
                Reverse(&b.updated_at),
                &b.id,
            ))
        });
        self.ranked = ranked
            .into_iter()
            .map(|(urgency, thread)| Ranked {
                entry: Entry {
                    id: thread.id.clone(),
                    urgency,
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
        let board = self.boards.entry(self.filter.clone()).or_default();
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
        self.held = [false; SCREEN_KEYS];
        self.showing_projects = false;
    }

    /// Expires the message on key 1.
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

    pub fn input(&mut self, action: Action, now: Instant) -> Option<Target> {
        self.tick(now);
        match action {
            Action::Down(Control::Screen(key)) => {
                if std::mem::replace(&mut self.held[key], true) {
                    return None;
                }
                // A covered key doesn't open the thread that is hidden under the message.
                if key == 0 && self.message.take().is_some() {
                    return None;
                }
                let id = self.thread_on(key)?.to_owned();
                return Some(self.target(id));
            }
            Action::Up(Control::Screen(key)) => self.held[key] = false,

            Action::Down(Control::Button(PREVIOUS)) => self.scroll(Direction::Left, SCREEN_KEYS),
            Action::Down(Control::Button(NEXT)) => self.scroll(Direction::Right, SCREEN_KEYS),
            Action::Down(Control::Button(NEEDS_YOU)) => return self.next_needing_user(now),

            Action::Turn(Dial::Top, direction) => self.scroll(direction, ROW),
            Action::Down(Control::Dial(Dial::Top)) => {
                self.filter = None;
                self.offset = 0;
                self.message = None;
                self.lay_out();
            }

            Action::Turn(Dial::Left, direction) => {
                let next = match direction {
                    Direction::Left => self.brightness.saturating_sub(BRIGHTNESS_STEP),
                    Direction::Right => self.brightness.saturating_add(BRIGHTNESS_STEP),
                };
                self.set_brightness(next, now);
            }
            Action::Down(Control::Dial(Dial::Left)) => self.set_brightness(DEFAULT_BRIGHTNESS, now),

            Action::Turn(Dial::Right, direction) => self.cycle_project(direction, now),
            Action::Down(Control::Dial(Dial::Right)) => {
                self.showing_projects = true;
                self.message = None;
            }
            Action::Up(Control::Dial(Dial::Right)) => self.showing_projects = false,
            _ => {}
        }
        None
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
        self.brightness = percent.clamp(MIN_BRIGHTNESS, 100);
        let tile = Tile::notice("BRIGHT", &format!("{}%", self.brightness));
        self.message = Some((tile, now + MESSAGE_TIME));
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
        self.message = Some((Tile::notice("PROJECT", name), now + MESSAGE_TIME));
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
            self.message = Some((tile, now + MESSAGE_TIME));
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

    fn statuses(&self) -> [Option<Status>; SCREEN_KEYS] {
        let requests = status::child_requests(&self.view.threads);
        std::array::from_fn(|key| {
            let thread = self.view.threads.get(self.thread_on(key)?)?;
            Some(status::classify(
                thread,
                requests.get(thread.id.as_str()).copied(),
                self.now,
            ))
        })
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
            Phase::Live if (0..SCREEN_KEYS).all(|key| self.thread_on(key).is_none()) => {
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
        let tiles = std::array::from_fn(|key| {
            let thread = self.thread_on(key).and_then(|id| self.view.threads.get(id));
            let mut tile = match (&cover, thread, statuses[key]) {
                (Some(cover), _, _) if key == 0 => cover.clone(),
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
            tiles,
            brightness: self.brightness,
            generation: self.generation,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::{Value, json};

    use super::*;
    use crate::t3::wire::Thread;

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
        let mut app = App::new(DEFAULT_BRIGHTNESS);
        app.update(view(Phase::Live, threads), Timestamp::UNIX_EPOCH);
        app
    }

    fn act(app: &mut App, action: Action) -> Option<String> {
        app.input(action, Instant::now())
            .map(|target| target.thread_id)
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
    fn pages_stop_at_both_ends() {
        let mut app = app_with(&many(14));
        button(&mut app, NEXT);
        assert_eq!(press(&mut app, 0).as_deref(), Some("t06"));
        button(&mut app, NEXT);
        // The last page is full rows, so it starts at the third row.
        assert_eq!(press(&mut app, 0).as_deref(), Some("t09"));
        assert_eq!(press(&mut app, 4).as_deref(), Some("t13"));
        assert_eq!(press(&mut app, 5), None);
        for _ in 0..3 {
            button(&mut app, PREVIOUS);
        }
        assert_eq!(press(&mut app, 0).as_deref(), Some("t00"));
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
        button(&mut app, NEXT);
        assert_eq!(press(&mut app, 0).as_deref(), Some("t00"));
    }

    #[test]
    fn nothing_moves_on_a_page_the_user_scrolled_to() {
        let mut threads = many(10);
        let mut app = app_with(&threads);
        button(&mut app, NEXT);
        assert_eq!(press(&mut app, 2).as_deref(), Some("t08"));

        threads.push(working("new"));
        threads[8] = working("t08");
        app.update(view(Phase::Live, &threads), Timestamp::UNIX_EPOCH);
        assert_eq!(press(&mut app, 2).as_deref(), Some("t08"));
        assert_eq!(press(&mut app, 4).as_deref(), Some("new"));

        act(&mut app, Action::Down(Control::Dial(Dial::Top)));
        let home: Vec<String> = (0..SCREEN_KEYS)
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
}
