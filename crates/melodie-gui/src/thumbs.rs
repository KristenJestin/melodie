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
}
