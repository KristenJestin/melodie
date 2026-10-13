//! The tracks picked in a list (Ctrl+click, Shift+click), to add or remove them together. By
//! rank in the list shown: a playlist may hold a track twice, and a removal names ranks. A list
//! read again (a sort, an edit) starts with nothing picked.

use crate::browse::Place;
use std::collections::BTreeSet;

/// How a row is clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    /// Ctrl+click: this row in or out.
    Toggle,
    /// Shift+click: every row from the last one clicked to this one.
    Range,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    /// The list the ranks belong to.
    of: Option<Place>,
    picked: BTreeSet<usize>,
    anchor: Option<usize>,
}

impl Selection {
    pub fn click(&mut self, list: &Place, ix: usize, how: How) {
        if self.of.as_ref() != Some(list) {
            *self = Self {
                of: Some(list.clone()),
                ..Self::default()
            };
        }
        match (how, self.anchor) {
            (How::Range, Some(a)) => self.picked.extend(a.min(ix)..=a.max(ix)),
            _ => {
                if !self.picked.remove(&ix) {
                    self.picked.insert(ix);
                }
            }
        }
        self.anchor = Some(ix);
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

    /// The ranks picked in `list`, in its order.
    pub fn ranks(&self, list: &Place) -> Vec<usize> {
        self.of(list)
            .map(|p| p.iter().copied().collect())
            .unwrap_or_default()
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

    fn here() -> Place {
        Place::Playlist("p1".into())
    }

    #[test]
    fn ctrl_click_picks_and_unpicks_one_row() {
        let mut s = Selection::default();
        s.click(&here(), 3, How::Toggle);
        s.click(&here(), 1, How::Toggle);
        assert_eq!(s.ranks(&here()), [1, 3]);
        s.click(&here(), 3, How::Toggle);
        assert_eq!(s.ranks(&here()), [1]);
        assert!(s.has(&here(), 1) && !s.has(&here(), 3));
    }

    #[test]
    fn shift_click_picks_the_rows_between_the_last_click_and_this_one() {
        let mut s = Selection::default();
        s.click(&here(), 5, How::Toggle);
        s.click(&here(), 2, How::Range);
        assert_eq!(s.ranks(&here()), [2, 3, 4, 5]);
        // without a first click, it is one row
        let mut s = Selection::default();
        s.click(&here(), 4, How::Range);
        assert_eq!(s.ranks(&here()), [4]);
    }

    #[test]
    fn another_list_starts_with_nothing_picked() {
        let mut s = Selection::default();
        s.click(&here(), 2, How::Toggle);
        assert!(s.ranks(&Place::Tracks).is_empty());
        s.click(&Place::Tracks, 7, How::Toggle);
        assert_eq!(s.ranks(&Place::Tracks), [7]);
        assert!(s.ranks(&here()).is_empty(), "one list at a time");
        assert_eq!(s.count(&Place::Tracks), 1);
        s.clear();
        assert_eq!(s.count(&Place::Tracks), 0);
    }

    #[test]
    fn the_ids_and_picks_follow_the_ranks() {
        let mut s = Selection::default();
        s.click(&here(), 2, How::Toggle);
        s.click(&here(), 0, How::Toggle);
        let ids = ["a", "b", "c"];
        assert_eq!(s.ids(&here(), &ids), ["a", "c"], "in the list's order");
        assert_eq!(
            s.picks(&here(), &ids),
            [(0, "a".to_string()), (2, "c".to_string())]
        );
        // a rank past the end of a list read again is dropped
        assert_eq!(s.ids(&here(), &["a"]), ["a"]);
    }
}
