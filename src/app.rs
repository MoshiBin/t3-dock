//! What the keys show and what a press does.

use std::cmp::Reverse;

use jiff::Timestamp;

use crate::board::{Board, Entry};
use crate::input::{Action, Control, SCREEN_KEYS};
use crate::render::Tile;
use crate::status::{self, Status};
use crate::t3::{Phase, View};

#[derive(Clone, PartialEq, Eq)]
pub struct Screen {
    pub tiles: [Tile; SCREEN_KEYS],
    /// A change makes the device redraw everything, as after sleep.
    pub generation: u64,
}

/// What a press opens. No `Debug`: both ids are private.
pub struct Target {
    pub environment_id: String,
    pub thread_id: String,
}

#[derive(Default)]
pub struct App {
    view: View,
    board: Board,
    held: [bool; SCREEN_KEYS],
    generation: u64,
}

impl App {
    pub fn update(&mut self, view: View, now: Timestamp) {
        self.view = view;
        self.refresh(now);
    }

    /// Also needed as time passes, because snoozes end.
    pub fn refresh(&mut self, now: Timestamp) {
        // While T3 is away the board is kept, so its restart doesn't shuffle the keys.
        if self.view.phase != Phase::Live {
            return;
        }
        let approvals = status::approval_parents(&self.view.threads);
        let mut ranked: Vec<_> = self
            .view
            .threads
            .values()
            .filter_map(|thread| {
                let approval = approvals.contains(thread.id.as_str());
                status::is_relevant(thread, now, approval)
                    .then(|| (status::classify(thread, approval).urgency(), thread))
            })
            .collect();
        ranked.sort_by(|(a_urgency, a), (b_urgency, b)| {
            (a_urgency, Reverse(&a.updated_at), &a.id).cmp(&(
                b_urgency,
                Reverse(&b.updated_at),
                &b.id,
            ))
        });
        let ranked: Vec<Entry> = ranked
            .into_iter()
            .map(|(urgency, thread)| Entry {
                id: thread.id.clone(),
                urgency,
            })
            .collect();
        self.board.settle(&ranked);
    }

    pub fn redraw(&mut self) {
        self.generation += 1;
    }

    pub fn release_all(&mut self) {
        self.held = [false; SCREEN_KEYS];
    }

    pub fn input(&mut self, action: Action) -> Option<Target> {
        match action {
            Action::Down(Control::Screen(key)) => {
                if std::mem::replace(&mut self.held[key], true) {
                    return None;
                }
                Some(Target {
                    environment_id: self.view.environment_id.clone(),
                    thread_id: self.thread_on(key)?.to_owned(),
                })
            }
            Action::Up(Control::Screen(key)) => {
                self.held[key] = false;
                None
            }
            _ => None,
        }
    }

    /// Keys are only pressable while the stream is live.
    fn thread_on(&self, key: usize) -> Option<&str> {
        if self.view.phase != Phase::Live {
            return None;
        }
        let id = self.board.get(key)?;
        self.view.threads.contains_key(id).then_some(id)
    }

    /// Each key's status, which is safe to log.
    pub fn statuses(&self) -> [Option<Status>; SCREEN_KEYS] {
        let approvals = status::approval_parents(&self.view.threads);
        std::array::from_fn(|key| {
            let thread = self.view.threads.get(self.thread_on(key)?)?;
            Some(status::classify(
                thread,
                approvals.contains(thread.id.as_str()),
            ))
        })
    }

    fn notice(&self) -> Option<Tile> {
        match self.view.phase {
            Phase::Live if (0..SCREEN_KEYS).all(|key| self.thread_on(key).is_none()) => {
                Some(Tile::notice("T3", "No threads"))
            }
            Phase::Live => None,
            Phase::Connecting => Some(Tile::notice("T3", "Connecting")),
            Phase::Offline => Some(Tile::notice("OFFLINE", "T3 Code not reachable")),
            Phase::NeedsToken => Some(Tile::notice("NO TOKEN", "Run t3-dock auth")),
            Phase::Unsupported => Some(Tile::notice("T3", "Unsupported T3 version")),
        }
    }

    pub fn screen(&self) -> Screen {
        let notice = self.notice();
        let statuses = self.statuses();
        let tiles = std::array::from_fn(|key| {
            let thread = self.thread_on(key).and_then(|id| self.view.threads.get(id));
            let mut tile = match (&notice, thread, statuses[key]) {
                (Some(notice), _, _) if key == 0 => notice.clone(),
                (None, Some(thread), Some(status)) => {
                    Tile::thread(thread.title.as_deref().unwrap_or_default(), status)
                }
                _ => Tile::blank(),
            };
            tile.pressed = self.held[key];
            tile
        });
        Screen {
            tiles,
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
        }
    }

    fn idle(id: &str, updated: &str) -> Value {
        json!({"id": id, "title": id, "status": "completed", "latestRunId": "r", "updatedAt": updated})
    }

    fn working(id: &str) -> Value {
        json!({"id": id, "title": id, "status": "running", "latestRunId": "r"})
    }

    fn app_with(threads: &[Value]) -> App {
        let mut app = App::default();
        app.update(view(Phase::Live, threads), Timestamp::UNIX_EPOCH);
        app
    }

    fn press(app: &mut App, key: usize) -> Option<String> {
        let target = app.input(Action::Down(Control::Screen(key)));
        app.input(Action::Up(Control::Screen(key)));
        target.map(|target| target.thread_id)
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
    fn holding_a_key_opens_it_once() {
        let mut app = app_with(&[working("busy")]);
        assert!(app.input(Action::Down(Control::Screen(0))).is_some());
        assert!(app.input(Action::Down(Control::Screen(0))).is_none());
    }

    #[test]
    fn a_dead_stream_is_not_pressable() {
        for phase in [
            Phase::Connecting,
            Phase::Offline,
            Phase::NeedsToken,
            Phase::Unsupported,
        ] {
            let mut app = app_with(&[working("busy")]);
            app.update(view(phase, &[working("busy")]), Timestamp::UNIX_EPOCH);
            assert_eq!(press(&mut app, 0), None);
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
}
