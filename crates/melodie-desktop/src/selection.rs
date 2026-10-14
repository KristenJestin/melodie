//! The tracks picked in a list (a click, Ctrl+click, Shift+click), to play, add or remove them
//! together. By rank in the list shown: a playlist may hold a track twice, and a removal names
//! ranks. A list read again (a sort, an edit) starts with nothing picked.

use crate::browse::Place;
use crate::table::Shown;
use melodie_core::library::Track;
use std::collections::BTreeSet;

/// What a left click on a row does: a click picks it, a double click plays it, as in a file
/// manager; a click by mistake starts no music.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Click {
    Pick(How),
    Play,
}

impl Click {
    /// `count`: the click's rank in a series of quick clicks (2 for a double click).
    pub fn of(ctrl: bool, shift: bool, count: usize) -> Self {
        if shift {
            Self::Pick(How::Range)
        } else if ctrl {
            Self::Pick(How::Toggle)
        } else if count >= 2 {
            Self::Play
        } else {
            Self::Pick(How::Only)
        }
    }
}

/// How a row is clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    /// A plain click: this row alone.
    Only,
    /// Ctrl+click: this row in or out.
    Toggle,
    /// Shift+click: every row from the last one clicked to this one.
    Range,
}

#[derive(Debug, Clone, Default)]
pub struct Selection {
    /// The list the ranks belong to.
    of: Option<Place>,
    /// The rows of that list as they were shown when picked: the search keeps its tables apart
    /// from the catalog's.
    shown: Option<Shown>,
    picked: BTreeSet<usize>,
    anchor: Option<usize>,
}

impl Selection {
    /// A click on the row `ix` of `shown`, the list at `list`.
    pub fn click(&mut self, list: &Place, shown: &Shown, ix: usize, how: How) {
        if self.of.as_ref() != Some(list) {
            *self = Self {
                of: Some(list.clone()),
                ..Self::default()
            };
        }
        self.shown = Some(shown.clone());
        match (how, self.anchor) {
            (How::Only, _) => self.picked = BTreeSet::from([ix]),
            (How::Range, Some(a)) => self.picked.extend(a.min(ix)..=a.max(ix)),
            _ => {
                if !self.picked.remove(&ix) {
                    self.picked.insert(ix);
                }
            }
        }
        self.anchor = Some(ix);
    }

    /// A right click on the row `ix`: inside the selection, the menu is the selection's; outside
    /// it, the row is picked alone first.
    pub fn right_click(&mut self, list: &Place, shown: &Shown, ix: usize) {
        if !self.has(list, ix) {
            self.click(list, shown, ix, How::Only);
        }
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    fn of(&self, list: &Place) -> Option<&BTreeSet<usize>> {
        (self.of.as_ref() == Some(list)).then_some(&self.picked)
    }

    pub fn has(&self, list: &Place, ix: usize) -> bool {
        self.of(list).is_some_and(|p| p.contains(&ix))
    }

    pub fn count(&self, list: &Place) -> usize {
        self.of(list).map_or(0, BTreeSet::len)
    }

    /// The row the menu key opens the menu on: the one clicked last while it is picked, else
    /// the first one picked.
    pub fn focused(&self, list: &Place) -> Option<usize> {
        let picked = self.of(list)?;
        self.anchor
            .filter(|a| picked.contains(a))
            .or_else(|| picked.first().copied())
    }

    /// The ranks picked in `list`, in its order.
    pub fn ranks(&self, list: &Place) -> Vec<usize> {
        self.of(list)
            .map(|p| p.iter().copied().collect())
            .unwrap_or_default()
    }

    /// The list shown when the rows of `list` were picked.
    pub fn shown(&self, list: &Place) -> Option<&Shown> {
        self.of(list).and(self.shown.as_ref())
    }

    /// The tracks picked in `list`, in the order shown, each with its rank in the list's own
    /// order (what a removal names).
    pub fn tracks(&self, list: &Place) -> Vec<(usize, &Track)> {
        let Some(shown) = self.shown(list) else {
            return Vec::new();
        };
        self.ranks(list)
            .into_iter()
            .filter_map(|ix| shown.row(ix))
            .collect()
    }

    /// The track Enter plays: the first one picked, in the order shown.
    pub fn first(&self, list: &Place) -> Option<&Track> {
        self.tracks(list).first().map(|&(_, t)| t)
    }

    /// The ids of the rows picked, in the list's order; `ids` is the list shown.
    pub fn ids<S: AsRef<str>>(&self, list: &Place, ids: &[S]) -> Vec<String> {
        self.picks(list, ids)
            .into_iter()
            .map(|(_, id)| id)
            .collect()
    }

    /// Each rank picked with the id it holds (what a removal names).
    pub fn picks<S: AsRef<str>>(&self, list: &Place, ids: &[S]) -> Vec<(usize, String)> {
        self.ranks(list)
            .into_iter()
            .filter_map(|ix| ids.get(ix).map(|id| (ix, id.as_ref().to_string())))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::{Col, ColSort};
    use std::sync::Arc;

    fn here() -> Place {
        Place::Playlist("p1".into())
    }

    fn track(id: &str, title: &str) -> Track {
        Track {
            id: id.into(),
            title: title.into(),
            ..Track::default()
        }
    }

    /// Eight rows; most tests only need the ranks.
    fn rows() -> Shown {
        Shown::new((0..8).map(|i| track(&format!("t{i}"), "")).collect(), None)
    }

    #[test]
    fn ctrl_click_picks_and_unpicks_one_row() {
        let mut s = Selection::default();
        s.click(&here(), &rows(), 3, How::Toggle);
        s.click(&here(), &rows(), 1, How::Toggle);
        assert_eq!(s.ranks(&here()), [1, 3]);
        s.click(&here(), &rows(), 3, How::Toggle);
        assert_eq!(s.ranks(&here()), [1]);
        assert!(s.has(&here(), 1) && !s.has(&here(), 3));
    }

    #[test]
    fn shift_click_picks_the_rows_between_the_last_click_and_this_one() {
        let mut s = Selection::default();
        s.click(&here(), &rows(), 5, How::Toggle);
        s.click(&here(), &rows(), 2, How::Range);
        assert_eq!(s.ranks(&here()), [2, 3, 4, 5]);
        // without a first click, it is one row
        let mut s = Selection::default();
        s.click(&here(), &rows(), 4, How::Range);
        assert_eq!(s.ranks(&here()), [4]);
    }

    #[test]
    fn another_list_starts_with_nothing_picked() {
        let mut s = Selection::default();
        s.click(&here(), &rows(), 2, How::Toggle);
        assert!(s.ranks(&Place::Tracks).is_empty());
        s.click(&Place::Tracks, &rows(), 7, How::Toggle);
        assert_eq!(s.ranks(&Place::Tracks), [7]);
        assert!(s.ranks(&here()).is_empty(), "one list at a time");
        assert_eq!(s.count(&Place::Tracks), 1);
        s.clear();
        assert_eq!(s.count(&Place::Tracks), 0);
    }

    #[test]
    fn the_menu_key_takes_the_row_clicked_last_while_it_is_picked() {
        let mut s = Selection::default();
        assert_eq!(s.focused(&here()), None);
        s.click(&here(), &rows(), 2, How::Toggle);
        s.click(&here(), &rows(), 5, How::Toggle);
        assert_eq!(s.focused(&here()), Some(5));
        // unpicked by the last click: the first one picked
        s.click(&here(), &rows(), 5, How::Toggle);
        assert_eq!(s.focused(&here()), Some(2));
        assert_eq!(s.focused(&Place::Tracks), None, "another list");
    }

    #[test]
    fn the_ids_and_picks_follow_the_ranks() {
        let mut s = Selection::default();
        s.click(&here(), &rows(), 2, How::Toggle);
        s.click(&here(), &rows(), 0, How::Toggle);
        let ids = ["a", "b", "c"];
        assert_eq!(s.ids(&here(), &ids), ["a", "c"], "in the list's order");
        assert_eq!(
            s.picks(&here(), &ids),
            [(0, "a".to_string()), (2, "c".to_string())]
        );
        // a rank past the end of a list read again is dropped
        assert_eq!(s.ids(&here(), &["a"]), ["a"]);
    }

    #[test]
    fn click_selects_without_playing() {
        assert_eq!(Click::of(false, false, 1), Click::Pick(How::Only));
        let mut s = Selection::default();
        s.click(&here(), &rows(), 3, How::Toggle);
        s.click(&here(), &rows(), 5, How::Toggle);
        s.click(&here(), &rows(), 1, How::Only);
        assert_eq!(
            s.ranks(&here()),
            [1],
            "a click replaces the selection by this row"
        );
        s.click(&here(), &rows(), 1, How::Only);
        assert_eq!(s.ranks(&here()), [1], "a second click keeps it");
        // then Shift+click goes from it
        s.click(&here(), &rows(), 3, How::Range);
        assert_eq!(s.ranks(&here()), [1, 2, 3]);
    }

    #[test]
    fn double_click_plays_from_this_row() {
        assert_eq!(Click::of(false, false, 2), Click::Play);
        assert_eq!(
            Click::of(false, false, 3),
            Click::Play,
            "a third quick click too"
        );
    }

    #[test]
    fn ctrl_and_shift_clicks_keep_adding_to_the_selection() {
        assert_eq!(Click::of(true, false, 1), Click::Pick(How::Toggle));
        assert_eq!(Click::of(false, true, 1), Click::Pick(How::Range));
        assert_eq!(Click::of(true, true, 1), Click::Pick(How::Range));
        // a quick second Ctrl+click picks, it does not play
        assert_eq!(Click::of(true, false, 2), Click::Pick(How::Toggle));
    }

    #[test]
    fn the_tracks_picked_come_in_the_order_shown_with_their_ranks() {
        let tracks: Arc<[Track]> = [track("a", "A"), track("b", "B"), track("c", "C")].into();
        // shown as c, b, a
        let shown = Shown::new(
            tracks,
            Some(ColSort {
                col: Col::Title,
                descending: true,
            }),
        );
        let mut s = Selection::default();
        assert!(s.tracks(&here()).is_empty());
        s.click(&here(), &shown, 2, How::Toggle);
        s.click(&here(), &shown, 0, How::Toggle);
        let picked: Vec<_> = s
            .tracks(&here())
            .into_iter()
            .map(|(rank, t)| (rank, t.id.as_str()))
            .collect();
        assert_eq!(picked, [(2, "c"), (0, "a")]);
        assert!(s.tracks(&Place::Tracks).is_empty(), "another list");
    }

    #[test]
    fn a_right_click_inside_the_selection_keeps_it_outside_picks_the_row_alone() {
        let mut s = Selection::default();
        s.click(&here(), &rows(), 2, How::Toggle);
        s.click(&here(), &rows(), 4, How::Toggle);
        s.right_click(&here(), &rows(), 4);
        assert_eq!(s.ranks(&here()), [2, 4], "inside: the whole selection");
        s.right_click(&here(), &rows(), 6);
        assert_eq!(s.ranks(&here()), [6], "outside: this row alone");
        let mut s = Selection::default();
        s.right_click(&here(), &rows(), 1);
        assert_eq!(s.ranks(&here()), [1], "nothing picked: this row");
    }

    #[test]
    fn enter_plays_the_first_track_picked_in_the_order_shown() {
        let tracks: Arc<[Track]> = [track("a", "A"), track("b", "B"), track("c", "C")].into();
        let shown = Shown::new(
            tracks,
            Some(ColSort {
                col: Col::Title,
                descending: true,
            }),
        );
        let mut s = Selection::default();
        s.click(&here(), &shown, 2, How::Toggle);
        s.click(&here(), &shown, 1, How::Toggle);
        assert_eq!(s.first(&here()).map(|t| t.id.as_str()), Some("b"));
        assert_eq!(s.first(&Place::Tracks), None);
    }
}
