//! The pictures of the queue's rows, looked up away from the render path: finding a thumbnail
//! checks files, and a list redraws on every frame of a scroll or a fade.

use melodie_core::cover;
use melodie_core::env::Env;
use melodie_core::proto::QueueEntry;
use std::collections::HashMap;
use std::path::PathBuf;

/// The picture of each entry that has one, by entry number.
pub type Thumbs = HashMap<u64, PathBuf>;

/// The thumbnail of each track if it is on disk, else the cover the daemon gave. Does file I/O:
/// never call it while drawing.
pub fn resolve(env: &Env, tracks: &[QueueEntry]) -> Thumbs {
    tracks
        .iter()
        .filter_map(|t| {
            let local = t
                .cover_id
                .as_deref()
                .and_then(|id| match cover::local(env, id) {
                    cover::Local::Have(p) => Some(p),
                    _ => None,
                });
            local
                .or_else(|| t.cover.clone().map(PathBuf::from))
                .map(|p| (t.entry, p))
        })
        .collect()
}

/// The cover ids the rows of the queue show, once each.
pub fn wanted(tracks: &[QueueEntry]) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for id in tracks.iter().filter_map(|t| t.cover_id.as_ref()) {
        if !ids.contains(id) {
            ids.push(id.clone());
        }
    }
    ids
}

/// The file the player and the bottom bar show for a track: the daemon's cover, else the
/// thumbnail already on disk while the daemon is still fetching its own.
pub fn big(track: &QueueEntry, thumbs: &Thumbs) -> Option<String> {
    track.cover.clone().or_else(|| {
        thumbs
            .get(&track.entry)
            .map(|p| p.to_string_lossy().into_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(entry: u64, cover_id: Option<&str>, cover: Option<&str>) -> QueueEntry {
        QueueEntry {
            entry,
            id: format!("t{entry}"),
            cover_id: cover_id.map(Into::into),
            cover: cover.map(Into::into),
            ..QueueEntry::default()
        }
    }

    #[test]
    fn a_thumbnail_on_disk_wins_over_the_daemons_cover() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::with_home(dir.path());
        let thumb = cover::thumb_path(&env, "c1");
        std::fs::create_dir_all(thumb.parent().unwrap()).unwrap();
        std::fs::write(&thumb, "").unwrap();
        let thumbs = resolve(
            &env,
            &[
                track(1, Some("c1"), Some("/tmp/full.img")),
                track(2, Some("c2"), Some("/tmp/c2.img")),
                track(3, Some("c3"), None),
                track(4, None, None),
            ],
        );
        assert_eq!(thumbs.get(&1), Some(&thumb));
        assert_eq!(
            thumbs.get(&2),
            Some(&PathBuf::from("/tmp/c2.img")),
            "no thumbnail yet: the daemon's cover"
        );
        assert_eq!(thumbs.get(&3), None, "no picture at all: the glyph");
        assert_eq!(thumbs.get(&4), None);
    }

    #[test]
    fn the_server_default_image_keeps_the_glyph() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::with_home(dir.path());
        let thumb = cover::thumb_path(&env, "c1");
        std::fs::create_dir_all(thumb.parent().unwrap()).unwrap();
        std::fs::write(format!("{}.none", thumb.display()), "").unwrap();
        assert!(resolve(&env, &[track(1, Some("c1"), None)]).is_empty());
    }

    /// The rows are drawn on every frame of a scroll or a fade: the file checks belong to
    /// `resolve`, run when the queue arrives. A guard on the source, as drawing cannot be
    /// observed without a window.
    #[test]
    fn the_queue_rows_do_not_touch_the_disk_while_drawing() {
        let rows = include_str!("queue.rs");
        for call in ["cover::local", "is_file", "std::fs", "exists()"] {
            assert!(
                !rows.contains(call),
                "queue.rs calls {call} on the render path"
            );
        }
    }

    #[test]
    fn the_rows_of_the_queue_ask_for_their_covers_once_each() {
        let tracks = [
            track(1, Some("c1"), None),
            track(2, Some("c2"), Some("/tmp/c2.img")),
            track(3, Some("c1"), None),
            track(4, None, None),
        ];
        assert_eq!(wanted(&tracks), ["c1", "c2"]);
    }

    /// The covers missing at first launch are fetched, and the rows show them when they land,
    /// the way the grids do: the same queue of ids, the same lookup.
    #[test]
    fn the_rows_of_the_queue_get_their_covers_when_they_land() {
        use crate::covers::{Fetch, Queue, round};
        let dir = tempfile::tempdir().unwrap();
        let env = Env::with_home(dir.path());
        let tracks = [
            track(1, Some("c1"), None),
            track(2, Some("c2"), None),
            track(3, Some("c1"), None),
        ];
        assert!(resolve(&env, &tracks).is_empty(), "the glyph at first");
        let mut queue = Queue::default();
        let new = queue.see(wanted(&tracks));
        queue.missing(new);
        let batch = queue.start().unwrap();
        let r = round(&env, &batch, |ids| {
            for id in ids {
                let thumb = cover::thumb_path(&env, id);
                std::fs::create_dir_all(thumb.parent().unwrap()).unwrap();
                std::fs::write(thumb, "").unwrap();
            }
            Fetch::Done
        });
        queue.finish(&batch, &r);
        let thumbs = resolve(&env, &tracks);
        assert_eq!(thumbs.len(), 3, "every row has its picture, no restart");
        assert_eq!(thumbs[&1], cover::thumb_path(&env, "c1"));
        assert_eq!(thumbs[&2], cover::thumb_path(&env, "c2"));
    }

    #[test]
    fn the_player_shows_the_thumbnail_until_the_daemons_cover_is_there() {
        let thumbs: Thumbs = [(1, PathBuf::from("/t/c1.img"))].into();
        let with = track(1, Some("c1"), Some("/d/c1.img"));
        assert_eq!(big(&with, &thumbs).as_deref(), Some("/d/c1.img"));
        let without = track(1, Some("c1"), None);
        assert_eq!(big(&without, &thumbs).as_deref(), Some("/t/c1.img"));
        assert_eq!(big(&track(2, Some("c2"), None), &thumbs), None);
    }
}
