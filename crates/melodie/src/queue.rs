//! Index arithmetic of the queue: where the current track ends up when the
//! queue is edited, and how a move is said to mpv. Kept apart from the daemon
//! so that the traps (the current track moving, tracks before it going away)
//! are tested without mpv.

/// Index of the current track once the entry at `from` has moved to `to`.
pub fn moved(cur: Option<usize>, from: usize, to: usize) -> Option<usize> {
    let c = cur?;
    Some(if c == from {
        to
    } else if from < c && to >= c {
        c - 1
    } else if from > c && to <= c {
        c + 1
    } else {
        c
    })
}

/// Index of the current track once the entry at `index` is removed. When the
/// current track itself goes, the next one takes its place (`None` past the end).
pub fn removed(cur: Option<usize>, index: usize, len_before: usize) -> Option<usize> {
    let c = cur?;
    if index < c {
        Some(c - 1)
    } else if index == c && index + 1 >= len_before {
        None
    } else {
        Some(c)
    }
}

/// Second argument of mpv's `playlist-move` for a move from `from` to `to`:
/// mpv names the entry whose place is taken, not the final index.
pub fn mpv_move_target(from: usize, to: usize) -> usize {
    if from < to { to + 1 } else { to }
}

/// Where added tracks start: right after the current track for "next",
/// otherwise at the end (also "next" when nothing is current).
pub fn insert_at(cur: Option<usize>, len: usize, next: bool) -> usize {
    match cur {
        Some(c) if next => c + 1,
        _ => len,
    }
}

/// The queue once what comes after the current track is shuffled: `order[i]` is the old index
/// of the entry that ends up at `i`. What was played and the current track stay in place.
pub fn shuffled_after(
    cur: Option<usize>,
    len: usize,
    below: impl FnMut(usize) -> usize,
) -> Vec<usize> {
    let mut order: Vec<usize> = (0..len).collect();
    if let Some(c) = cur {
        crate::rules::shuffle(&mut order[(c + 1).min(len)..], below);
    }
    order
}

/// The `playlist-move from target` calls that bring mpv's playlist, in the old order, to
/// `order` (see `shuffled_after`), one entry at a time from the front.
pub fn mpv_reorder(order: &[usize]) -> Vec<(usize, usize)> {
    let mut mpv: Vec<usize> = (0..order.len()).collect();
    let mut moves = Vec::new();
    for (i, want) in order.iter().enumerate() {
        let from = i + mpv[i..].iter().position(|x| x == want).unwrap_or(0);
        if from != i {
            let x = mpv.remove(from);
            mpv.insert(i, x);
            moves.push((from, mpv_move_target(from, i)));
        }
    }
    moves
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Applies a move to a list the way the daemon does to its queue.
    fn apply_move<T>(v: &mut Vec<T>, from: usize, to: usize) {
        let x = v.remove(from);
        v.insert(to, x);
    }

    /// Applies a move to a list the way mpv does with `playlist-move from target`.
    fn mpv_move<T: Copy>(v: &mut Vec<T>, from: usize, target: usize) {
        let x = v[from];
        v.insert(target, x);
        let old = if target <= from { from + 1 } else { from };
        v.remove(old);
    }

    #[test]
    fn moving_the_current_track_keeps_it_current() {
        assert_eq!(moved(Some(1), 1, 3), Some(3));
        assert_eq!(moved(Some(3), 3, 0), Some(0));
        assert_eq!(moved(Some(2), 2, 2), Some(2));
    }

    #[test]
    fn moving_a_track_across_the_current_one_shifts_its_index() {
        // from before to after: the current track comes one step closer
        assert_eq!(moved(Some(2), 0, 4), Some(1));
        // from after to before: one step further
        assert_eq!(moved(Some(2), 4, 0), Some(3));
        // to the very place of the current track
        assert_eq!(moved(Some(2), 0, 2), Some(1));
        assert_eq!(moved(Some(2), 4, 2), Some(3));
    }

    #[test]
    fn moving_tracks_on_one_side_leaves_the_current_index() {
        assert_eq!(moved(Some(2), 0, 1), Some(2));
        assert_eq!(moved(Some(2), 3, 4), Some(2));
        assert_eq!(moved(Some(2), 4, 3), Some(2));
        assert_eq!(moved(None, 0, 3), None);
    }

    #[test]
    fn removing_a_track_before_the_current_shifts_its_index() {
        assert_eq!(removed(Some(2), 0, 5), Some(1));
        assert_eq!(removed(Some(2), 4, 5), Some(2));
        assert_eq!(removed(None, 1, 5), None);
    }

    #[test]
    fn removing_the_current_track_makes_the_next_one_current() {
        assert_eq!(removed(Some(2), 2, 5), Some(2));
        assert_eq!(removed(Some(0), 0, 2), Some(0));
    }

    #[test]
    fn removing_the_current_last_track_leaves_nothing_current() {
        assert_eq!(removed(Some(4), 4, 5), None);
        assert_eq!(removed(Some(0), 0, 1), None);
    }

    #[test]
    fn a_move_said_to_mpv_lands_where_the_queue_does() {
        let start: Vec<char> = "abcde".chars().collect();
        for from in 0..5 {
            for to in 0..5 {
                let mut ours = start.clone();
                apply_move(&mut ours, from, to);
                let mut theirs = start.clone();
                mpv_move(&mut theirs, from, mpv_move_target(from, to));
                assert_eq!(ours, theirs, "move {from} -> {to}");
            }
        }
    }

    #[test]
    fn add_next_goes_right_after_the_current_track() {
        assert_eq!(insert_at(Some(1), 5, true), 2);
        assert_eq!(insert_at(Some(4), 5, true), 5);
    }

    #[test]
    fn add_last_or_with_nothing_current_goes_at_the_end() {
        assert_eq!(insert_at(Some(1), 5, false), 5);
        assert_eq!(insert_at(None, 3, true), 3);
        assert_eq!(insert_at(None, 0, false), 0);
    }

    /// A fixed "random" source: the same sequence every time.
    fn counter() -> impl FnMut(usize) -> usize {
        let mut k = 0;
        move |n| {
            k += 7;
            k % n
        }
    }

    #[test]
    fn shuffling_after_keeps_the_past_and_the_current_track_in_place() {
        for len in 0..9 {
            for c in 0..len {
                let order = shuffled_after(Some(c), len, counter());
                assert_eq!(
                    order[..=c],
                    (0..=c).collect::<Vec<_>>(),
                    "len {len} cur {c}"
                );
                let mut tail = order[c + 1..].to_vec();
                tail.sort();
                assert_eq!(tail, (c + 1..len).collect::<Vec<_>>(), "len {len} cur {c}");
            }
        }
    }

    #[test]
    fn shuffling_after_with_nothing_current_changes_nothing() {
        assert_eq!(shuffled_after(None, 4, counter()), [0, 1, 2, 3]);
        assert_eq!(shuffled_after(None, 0, counter()), Vec::<usize>::new());
    }

    #[test]
    fn shuffling_one_track_or_none_after_the_current_changes_nothing() {
        assert_eq!(shuffled_after(Some(2), 3, counter()), [0, 1, 2]);
        assert_eq!(shuffled_after(Some(1), 3, counter()), [0, 1, 2]);
        assert_eq!(shuffled_after(Some(0), 1, counter()), [0]);
    }

    #[test]
    fn the_moves_said_to_mpv_land_on_the_shuffled_order() {
        for len in 0..8 {
            for c in 0..len.max(1) {
                let order = shuffled_after(Some(c), len, counter());
                let mut mpv: Vec<usize> = (0..len).collect();
                for (from, target) in mpv_reorder(&order) {
                    mpv_move(&mut mpv, from, target);
                }
                assert_eq!(mpv, order, "len {len} cur {c}");
            }
        }
        // Every permutation of a tail, not only the ones a source draws.
        let order = [0, 4, 2, 5, 1, 3];
        let mut mpv: Vec<usize> = (0..6).collect();
        for (from, target) in mpv_reorder(&order) {
            mpv_move(&mut mpv, from, target);
        }
        assert_eq!(mpv, order);
    }
}
