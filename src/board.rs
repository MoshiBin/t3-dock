//! Which thread sits on which position.
//!
//! The first page is home. An active thread keeps its key there, so nothing moves under the
//! user's finger. Only an idle thread gives its key up to an active one, and a finished one also
//! to a thread that needs the user. Later pages follow the ranking, as long as they are off screen.
//!
//! The ranking is T3's sidebar order, not urgency, so every thread not yet home gets its chance at
//! a key, and the one it takes is the lowest ranked it may displace.

use crate::input::SCREEN_KEYS;

pub const NEEDS_USER: u8 = 0;
const FINISHED: u8 = 3;
const IDLE: u8 = 4;

/// A thread as the board sees it. Callers pass these best first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub urgency: u8,
}

#[derive(Default)]
pub struct Board {
    slots: Vec<Option<String>>,
}

fn may_displace(candidate: u8, shown: u8) -> bool {
    (candidate < IDLE && shown == IDLE) || (candidate == NEEDS_USER && shown >= FINISHED)
}

impl Board {
    pub fn get(&self, position: usize) -> Option<&str> {
        self.slots.get(position)?.as_deref()
    }

    /// The furthest scroll, which keeps the last thread on screen and moves by whole rows.
    pub fn last_offset(&self) -> usize {
        let used = self
            .slots
            .iter()
            .rposition(Option::is_some)
            .map_or(0, |last| last + 1);
        used.saturating_sub(SCREEN_KEYS).div_ceil(3) * 3
    }

    /// Updates the board while another page is on screen: nothing moves.
    pub fn freeze(&mut self, ranked: &[Entry]) {
        for slot in &mut self.slots {
            if slot
                .as_ref()
                .is_some_and(|id| !ranked.iter().any(|entry| entry.id == *id))
            {
                *slot = None;
            }
        }
        for entry in ranked {
            if !self.slots.iter().flatten().any(|id| *id == entry.id) {
                self.slots.push(Some(entry.id.clone()));
            }
        }
    }

    /// Updates the board while home is on screen.
    pub fn settle(&mut self, ranked: &[Entry]) {
        let rank = |id: &str| ranked.iter().position(|entry| entry.id == id);

        let mut home: [Option<String>; SCREEN_KEYS] = std::array::from_fn(|key| {
            self.get(key)
                .filter(|id| rank(id).is_some())
                .map(str::to_owned)
        });
        for candidate in ranked {
            if home.iter().flatten().any(|id| *id == candidate.id) {
                continue;
            }
            if let Some(hole) = home.iter_mut().find(|slot| slot.is_none()) {
                *hole = Some(candidate.id.clone());
                continue;
            }
            let victim = home
                .iter()
                .enumerate()
                .filter_map(|(key, id)| Some((key, rank(id.as_deref()?)?)))
                .filter(|&(_, rank)| may_displace(candidate.urgency, ranked[rank].urgency))
                .max_by_key(|&(_, rank)| rank);
            if let Some((key, _)) = victim {
                home[key] = Some(candidate.id.clone());
            }
        }

        let rest: Vec<Option<String>> = ranked
            .iter()
            .filter(|entry| !home.iter().flatten().any(|id| *id == entry.id))
            .map(|entry| Some(entry.id.clone()))
            .collect();
        self.slots = home.into_iter().chain(rest).collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKING: u8 = 2;

    fn entries(list: &[(&str, u8)]) -> Vec<Entry> {
        let mut entries: Vec<Entry> = list
            .iter()
            .map(|&(id, urgency)| Entry {
                id: id.to_owned(),
                urgency,
            })
            .collect();
        entries.sort_by_key(|entry| entry.urgency);
        entries
    }

    fn shown(board: &Board) -> Vec<&str> {
        (0..board.slots.len())
            .map(|position| board.get(position).unwrap_or("-"))
            .collect()
    }

    fn full(urgency: u8) -> Vec<(&'static str, u8)> {
        ["a", "b", "c", "d", "e", "f"]
            .into_iter()
            .map(|id| (id, urgency))
            .collect()
    }

    #[test]
    fn a_low_ranked_request_still_takes_an_idle_key() {
        let mut board = Board::default();
        let mut list: Vec<Entry> = entries(&full(IDLE));
        board.settle(&list);
        list.insert(0, entry("newer-idle", IDLE));
        list.push(entry("ask", NEEDS_USER));
        board.settle(&list);
        assert_eq!(shown(&board)[..6], ["a", "b", "c", "d", "e", "ask"]);
    }

    fn entry(id: &str, urgency: u8) -> Entry {
        Entry {
            id: id.to_owned(),
            urgency,
        }
    }

    #[test]
    fn starts_in_rank_order() {
        let mut board = Board::default();
        board.settle(&entries(&[
            ("idle", IDLE),
            ("ask", NEEDS_USER),
            ("busy", WORKING),
        ]));
        assert_eq!(shown(&board), ["ask", "busy", "idle", "-", "-", "-"]);
    }

    #[test]
    fn a_change_of_rank_does_not_move_a_key() {
        let mut board = Board::default();
        board.settle(&entries(&[("a", WORKING), ("b", IDLE)]));
        board.settle(&entries(&[("a", IDLE), ("b", NEEDS_USER)]));
        assert_eq!(shown(&board)[..2], ["a", "b"]);
    }

    #[test]
    fn a_removed_thread_leaves_a_hole_that_the_next_one_fills() {
        let mut board = Board::default();
        board.settle(&entries(&[("a", WORKING), ("b", WORKING), ("c", WORKING)]));
        board.settle(&entries(&[("a", WORKING), ("c", WORKING)]));
        assert_eq!(shown(&board)[..3], ["a", "-", "c"]);
        board.settle(&entries(&[("a", WORKING), ("c", WORKING), ("new", IDLE)]));
        assert_eq!(shown(&board)[..3], ["a", "new", "c"]);
    }

    #[test]
    fn a_new_active_thread_takes_only_the_least_recent_idle_key() {
        let mut board = Board::default();
        let mut list = full(IDLE);
        list[1].1 = WORKING;
        board.settle(&entries(&list));
        let before: Vec<String> = shown(&board).iter().map(|id| id.to_string()).collect();

        list.push(("new", WORKING));
        board.settle(&entries(&list));
        let mut expected = before;
        let replaced = expected.iter().position(|id| id == "f").unwrap();
        expected[replaced] = "new".into();
        expected.push("f".into());
        assert_eq!(shown(&board), expected);
    }

    #[test]
    fn working_threads_keep_their_keys_from_everyone() {
        let mut board = Board::default();
        let mut list = full(WORKING);
        board.settle(&entries(&list));
        list.push(("ask", NEEDS_USER));
        board.settle(&entries(&list));
        assert_eq!(shown(&board), ["a", "b", "c", "d", "e", "f", "ask"]);
    }

    #[test]
    fn needing_the_user_beats_finished_but_working_does_not() {
        let mut board = Board::default();
        let mut list = full(FINISHED);
        board.settle(&entries(&list));
        list.push(("busy", WORKING));
        board.settle(&entries(&list));
        assert_eq!(shown(&board)[6], "busy");

        list.push(("ask", NEEDS_USER));
        board.settle(&entries(&list));
        assert_eq!(shown(&board)[..6], ["a", "b", "c", "d", "e", "ask"]);
    }
}
