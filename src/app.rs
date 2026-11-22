//! Milestone 1: one thread on key 1, numbered tiles on the rest.

use jiff::Timestamp;

use crate::input::{Action, Control};
use crate::render::Tile;
use crate::status::{self, Status};
use crate::t3::wire::{Thread, Threads};
use crate::t3::{Phase, View};

pub const KEYS: usize = 6;
pub type Screen = [Tile; KEYS];

/// What a press opens. No `Debug`: both ids are private.
pub struct Target {
    pub environment_id: String,
    pub thread_id: String,
}

pub struct App {
    /// A thread id given on the command line.
    wanted: Option<String>,
    shown: Option<String>,
    held: [bool; KEYS],
    view: View,
}

impl App {
    pub fn new(wanted: Option<String>) -> Self {
        Self {
            wanted,
            shown: None,
            held: [false; KEYS],
            view: View::default(),
        }
    }

    pub fn update(&mut self, view: View, now: Timestamp) {
        self.view = view;
        self.refresh(now);
    }

    /// Re-picks the thread. Also needed as time passes, because snoozes end.
    pub fn refresh(&mut self, now: Timestamp) {
        if self.view.phase == Phase::Live {
            self.shown = pick(
                &self.view.threads,
                self.wanted.as_deref(),
                self.shown.as_deref(),
                now,
            );
        }
    }

    pub fn release_all(&mut self) {
        self.held = [false; KEYS];
    }

    pub fn input(&mut self, action: Action) -> Option<Target> {
        match action {
            Action::Down(Control::Screen(key)) => {
                let repeated = std::mem::replace(&mut self.held[key], true);
                if repeated || key != 0 {
                    return None;
                }
                let thread = self.thread()?;
                Some(Target {
                    environment_id: self.view.environment_id.clone(),
                    thread_id: thread.id.clone(),
                })
            }
            Action::Up(Control::Screen(key)) => {
                self.held[key] = false;
                None
            }
            _ => None,
        }
    }

    /// The thread on key 1, which is only pressable while the stream is live.
    fn thread(&self) -> Option<&Thread> {
        if self.view.phase != Phase::Live {
            return None;
        }
        let id = self.shown.as_deref()?;
        self.view.threads.get(id).map(|thread| &**thread)
    }

    /// What key 1 shows, in a form that is safe to log.
    pub fn status(&self) -> Option<Status> {
        self.thread().map(|thread| self.classify(thread))
    }

    fn classify(&self, thread: &Thread) -> Status {
        let approval = status::approval_parents(&self.view.threads).contains(thread.id.as_str());
        status::classify(thread, approval)
    }

    pub fn screen(&self) -> Screen {
        let first = match (self.view.phase, self.thread()) {
            (_, Some(thread)) => Tile::thread(
                thread.title.as_deref().unwrap_or_default(),
                self.classify(thread),
            ),
            (Phase::Live, None) if self.wanted.is_some() => Tile::notice("T3", "Thread not found"),
            (Phase::Live, None) => Tile::notice("T3", "No threads"),
            (Phase::Connecting, None) => Tile::notice("T3", "Connecting"),
            (Phase::Offline, None) => Tile::notice("OFFLINE", "T3 Code not reachable"),
            (Phase::NeedsToken, None) => Tile::notice("NO TOKEN", "Run t3-dock auth"),
            (Phase::Unsupported, None) => Tile::notice("T3", "Unsupported T3 version"),
        };
        std::array::from_fn(|key| {
            let mut tile = if key == 0 {
                first.clone()
            } else {
                Tile::numbered(key + 1)
            };
            tile.pressed = self.held[key];
            tile
        })
    }
}

/// A requested thread always wins. Otherwise the shown one stays while it is relevant, so the key
/// does not change under the user's finger, and then the most urgent and most recent one takes it.
fn pick(
    threads: &Threads,
    wanted: Option<&str>,
    shown: Option<&str>,
    now: Timestamp,
) -> Option<String> {
    if let Some(id) = wanted {
        return threads.contains_key(id).then(|| id.to_owned());
    }
    let approvals = status::approval_parents(threads);
    let relevant =
        |thread: &Thread| status::is_relevant(thread, now, approvals.contains(thread.id.as_str()));

    if let Some(thread) = shown.and_then(|id| threads.get(id))
        && relevant(thread)
    {
        return Some(thread.id.clone());
    }
    threads
        .values()
        .filter(|thread| relevant(thread))
        .min_by(|a, b| {
            let urgency = |thread: &Thread| {
                status::classify(thread, approvals.contains(thread.id.as_str())).urgency()
            };
            urgency(a)
                .cmp(&urgency(b))
                .then_with(|| b.updated_at.cmp(&a.updated_at))
                .then_with(|| a.id.cmp(&b.id))
        })
        .map(|thread| thread.id.clone())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::{Value, json};

    use super::*;

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
        }
    }

    fn idle(id: &str, updated: &str) -> Value {
        json!({"id": id, "title": id, "status": "completed", "latestRunId": "r", "updatedAt": updated})
    }

    fn working(id: &str) -> Value {
        json!({"id": id, "title": id, "status": "running", "latestRunId": "r"})
    }

    fn press(app: &mut App, key: usize) -> Option<String> {
        app.input(Action::Down(Control::Screen(key)))
            .map(|target| target.thread_id)
    }

    fn app_with(wanted: Option<&str>, threads: &[Value]) -> App {
        let mut app = App::new(wanted.map(str::to_owned));
        app.update(view(Phase::Live, threads), Timestamp::UNIX_EPOCH);
        app
    }

    #[test]
    fn picks_urgent_then_recent() {
        let old = idle("old", "2030-01-01T00:00:00Z");
        let new = idle("new", "2030-01-02T00:00:00Z");
        let mut app = app_with(None, &[old.clone(), new.clone()]);
        assert_eq!(press(&mut app, 0).as_deref(), Some("new"));

        let mut app = app_with(None, &[old, new, working("busy")]);
        assert_eq!(press(&mut app, 0).as_deref(), Some("busy"));
    }

    #[test]
    fn keeps_its_thread_until_that_is_irrelevant() {
        let first = idle("first", "2030-01-01T00:00:00Z");
        let mut app = app_with(None, std::slice::from_ref(&first));
        app.update(
            view(Phase::Live, &[first, working("busy")]),
            Timestamp::UNIX_EPOCH,
        );
        assert_eq!(app.shown.as_deref(), Some("first"));

        app.update(view(Phase::Live, &[working("busy")]), Timestamp::UNIX_EPOCH);
        assert_eq!(app.shown.as_deref(), Some("busy"));
    }

    #[test]
    fn requested_thread_shows_even_if_settled() {
        let settled = json!({"id": "asked", "settledOverride": "settled"});
        let mut app = app_with(Some("asked"), &[settled, working("busy")]);
        assert_eq!(press(&mut app, 0).as_deref(), Some("asked"));
        assert!(app_with(Some("gone"), &[working("busy")]).shown.is_none());
    }

    #[test]
    fn only_key_one_focuses_and_only_once_per_press() {
        let mut app = app_with(None, &[working("busy")]);
        for key in 1..KEYS {
            assert_eq!(press(&mut app, key), None);
        }
        assert!(press(&mut app, 0).is_some());
        assert!(press(&mut app, 0).is_none());
        app.input(Action::Up(Control::Screen(0)));
        assert!(press(&mut app, 0).is_some());
    }

    #[test]
    fn a_dead_stream_is_not_pressable() {
        for phase in [
            Phase::Connecting,
            Phase::Offline,
            Phase::NeedsToken,
            Phase::Unsupported,
        ] {
            let mut app = app_with(None, &[working("busy")]);
            app.update(view(phase, &[working("busy")]), Timestamp::UNIX_EPOCH);
            assert_eq!(press(&mut app, 0), None);
        }
    }
}
