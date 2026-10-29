//! What the window asks of the `melodie` program: what needs the server (and so the keyring)
//! goes through it, so the window never holds a token.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

/// What can be played, queued or mixed from the library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Track,
    Album,
    Artist,
    Playlist,
}

impl Kind {
    pub fn word(self) -> &'static str {
        match self {
            Self::Track => "track",
            Self::Album => "album",
            Self::Artist => "artist",
            Self::Playlist => "playlist",
        }
    }
}

/// What the window runs `melodie` for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    /// Nothing to play and no daemon: the personal mix (it starts the daemon).
    PlayMix,
    /// Albums played before, but not lately.
    Rediscover,
    /// The heart of a track, an album or an artist.
    Star {
        kind: Kind,
        id: String,
        starred: bool,
    },
    /// Replaces the queue with it and plays it.
    Play {
        kind: Kind,
        id: String,
        shuffle: bool,
    },
    /// Adds it after the current track (`next`) or at the end.
    Queue {
        kind: Kind,
        id: String,
        next: bool,
    },
    /// Replaces the queue with these tracks, in this order, and plays from the one at `from`
    /// (from 0): the ones before it stay, for Previous.
    PlayTracks {
        ids: Vec<String>,
        from: usize,
    },
    /// Replaces the queue with these tracks, shuffled, and plays it.
    ShuffleTracks {
        ids: Vec<String>,
    },
    /// Adds these tracks, in this order, after the current track (`next`) or at the end.
    QueueTracks {
        ids: Vec<String>,
        next: bool,
    },
    /// An instant mix around it.
    Mix {
        kind: Kind,
        id: String,
    },
    /// Adds to a playlist; the tracks already there are left out unless `allow_duplicates`.
    AddToPlaylist {
        playlist: String,
        what: Adding,
        allow_duplicates: bool,
    },
    /// What adding would do, written nowhere: the duplicate question is asked from it.
    CheckAdd {
        playlist: String,
        what: Adding,
    },
    /// A new playlist, empty or with what is being added.
    CreatePlaylist {
        name: String,
        with: Option<Adding>,
    },
    RenamePlaylist {
        id: String,
        name: String,
    },
    DeletePlaylist {
        id: String,
    },
    /// Removes the tracks at these ranks (from 0), each with the id the window saw there.
    RemoveFromPlaylist {
        id: String,
        picks: Vec<(usize, String)>,
    },
    /// Moves the track at `from` to `to` (from 0); `track` is the id the window saw at `from`.
    MoveInPlaylist {
        id: String,
        from: usize,
        to: usize,
        track: String,
    },
    /// Lists the duplicates of a playlist, or removes the exact ones.
    Duplicates {
        id: String,
        remove: bool,
    },
    /// The mix of a genre.
    MixGenre {
        name: String,
    },
    /// A mix (`PlayMix`, `Rediscover` or `MixGenre`) added after the current track (`next`) or
    /// at the end of the queue.
    QueueMix {
        mix: Box<Job>,
        next: bool,
    },
}

/// What is added to a playlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Adding {
    Album(String),
    /// One track or several (a selection, the current track), in this order.
    Tracks(Vec<String>),
}

impl Adding {
    fn words(&self) -> Vec<String> {
        match self {
            Self::Album(id) => vec!["album".into(), id.clone()],
            Self::Tracks(ids) => std::iter::once("track".to_string())
                .chain(ids.iter().cloned())
                .collect(),
        }
    }
}

impl Job {
    /// The playlist it reads or writes.
    pub fn playlist(&self) -> Option<&str> {
        match self {
            Self::AddToPlaylist { playlist, .. } | Self::CheckAdd { playlist, .. } => {
                Some(playlist)
            }
            Self::RenamePlaylist { id, .. }
            | Self::DeletePlaylist { id }
            | Self::RemoveFromPlaylist { id, .. }
            | Self::MoveInPlaylist { id, .. }
            | Self::Duplicates { id, .. } => Some(id),
            Self::PlayMix
            | Self::Rediscover
            | Self::Star { .. }
            | Self::Play { .. }
            | Self::Queue { .. }
            | Self::PlayTracks { .. }
            | Self::ShuffleTracks { .. }
            | Self::QueueTracks { .. }
            | Self::Mix { .. }
            | Self::CreatePlaylist { .. }
            | Self::MixGenre { .. }
            | Self::QueueMix { .. } => None,
        }
    }

    pub fn args(&self) -> Vec<String> {
        let words = |w: &[&str]| w.iter().map(|w| w.to_string()).collect::<Vec<_>>();
        match self {
            Self::PlayMix => vec!["mix".into(), "me".into()],
            Self::Rediscover => words(&["mix", "rediscover"]),
            // `--` before every id: an id may start with a dash
            Self::Play { kind, id, shuffle } => {
                let mut a = words(&["play", kind.word()]);
                if *shuffle {
                    a.push("--shuffle".into());
                }
                a.extend(["--".into(), id.clone()]);
                a
            }
            Self::Queue { kind, id, next } => {
                let mut a = words(&["queue", kind.word()]);
                if *next {
                    a.push("--next".into());
                }
                a.extend(["--".into(), id.clone()]);
                a
            }
            // the ids on stdin (`-`): a whole library does not fit on a command line
            Self::PlayTracks { from, .. } => {
                words(&["play", "track", "--from", &(from + 1).to_string(), "-"])
            }
            Self::ShuffleTracks { .. } => words(&["play", "track", "--shuffle", "-"]),
            Self::QueueTracks { next, .. } => {
                let mut a = words(&["queue", "track"]);
                if *next {
                    a.push("--next".into());
                }
                a.push("-".into());
                a
            }
            Self::Mix { kind, id } => {
                let mut a = words(&["mix", kind.word(), "--"]);
                a.push(id.clone());
                a
            }
            Self::AddToPlaylist {
                playlist,
                what,
                allow_duplicates,
            } => {
                let mut a = words(&["playlist", "add", "--json"]);
                if *allow_duplicates {
                    a.push("--allow-duplicates".into());
                }
                a.extend(["--".into(), playlist.clone()]);
                a.extend(what.words());
                a
            }
            Self::CheckAdd { playlist, what } => {
                let mut a = words(&["playlist", "add", "--dry-run", "--json", "--"]);
                a.push(playlist.clone());
                a.extend(what.words());
                a
            }
            Self::CreatePlaylist { name, with } => {
                let mut a = words(&["playlist", "create", "--"]);
                a.push(name.clone());
                a.extend(with.iter().flat_map(Adding::words));
                a
            }
            Self::RenamePlaylist { id, name } => {
                let mut a = words(&["playlist", "rename", "--"]);
                a.extend([id.clone(), name.clone()]);
                a
            }
            Self::DeletePlaylist { id } => {
                let mut a = words(&["playlist", "delete", "--"]);
                a.push(id.clone());
                a
            }
            // `--expect=<id>` in one word: an id may start with a dash
            Self::RemoveFromPlaylist { id, picks } => {
                let mut a = words(&["playlist", "remove"]);
                a.extend(picks.iter().map(|(_, t)| format!("--expect={t}")));
                a.extend(["--".into(), id.clone()]);
                a.extend(picks.iter().map(|(ix, _)| (ix + 1).to_string()));
                a
            }
            Self::MoveInPlaylist {
                id,
                from,
                to,
                track,
            } => {
                let mut a = words(&["playlist", "move"]);
                a.extend([format!("--expect={track}"), "--".into(), id.clone()]);
                a.extend([(from + 1).to_string(), (to + 1).to_string()]);
                a
            }
            Self::Duplicates { id, remove } => {
                let mut a = words(&["playlist", "duplicates", "--json"]);
                if *remove {
                    a.push("--remove".into());
                }
                a.extend(["--".into(), id.clone()]);
                a
            }
            Self::MixGenre { name } => {
                let mut a = words(&["mix", "genre", "--"]);
                a.push(name.clone());
                a
            }
            // `--next` and `--queue` belong to `mix`: before the dashes that end the options
            Self::QueueMix { mix, next } => {
                let mut a = mix.args();
                a.insert(1, if *next { "--next" } else { "--queue" }.into());
                a
            }
            Self::Star { kind, id, starred } => vec![
                if *starred { "like" } else { "unlike" }.into(),
                kind.word().into(),
                // an id may start with a dash
                "--".into(),
                id.clone(),
            ],
        }
    }
}

impl Job {
    /// It changes what plays or the queue: these run one after the other, in the order asked, so
    /// the last click wins.
    pub fn plays(&self) -> bool {
        matches!(
            self,
            Self::PlayMix
                | Self::Rediscover
                | Self::Play { .. }
                | Self::Queue { .. }
                | Self::PlayTracks { .. }
                | Self::ShuffleTracks { .. }
                | Self::QueueTracks { .. }
                | Self::Mix { .. }
                | Self::MixGenre { .. }
                | Self::QueueMix { .. }
        )
    }

    /// It replaces the queue: what waits to change the queue before it would be wiped anyway.
    pub fn replaces(&self) -> bool {
        matches!(
            self,
            Self::PlayMix
                | Self::Rediscover
                | Self::Play { .. }
                | Self::PlayTracks { .. }
                | Self::ShuffleTracks { .. }
                | Self::Mix { .. }
                | Self::MixGenre { .. }
        )
    }

    /// What `melodie` reads on its standard input: the ids of several tracks, one per line.
    pub fn stdin(&self) -> Option<String> {
        match self {
            Self::PlayTracks { ids, .. }
            | Self::ShuffleTracks { ids }
            | Self::QueueTracks { ids, .. } => {
                Some(ids.iter().map(|id| format!("{id}\n")).collect())
            }
            _ => None,
        }
    }
}

/// What the window says after `playlist add --json` printed `out`: what went in, what was
/// already there, or why nothing went in (a failure is one launcher entry, read by its id: its
/// text is for the launcher and may hold the server's address).
pub fn added_message(out: &str) -> Result<String, String> {
    let v = read_json(out, "Could not add to the playlist")?;
    let n = |k: &str| v[k].as_array().map_or(0, Vec::len);
    let name = v["playlist"]["name"].as_str().unwrap_or("the playlist");
    let tracks = |n: usize| {
        if n == 1 {
            "1 track".to_string()
        } else {
            format!("{n} tracks")
        }
    };
    Ok(match (n("added"), n("already")) {
        (0, 0) => format!("Nothing to add to {name}"),
        (0, 1) => format!("Already in {name}"),
        (0, k) => format!("All {k} tracks are already in {name}"),
        (a, 0) => format!("Added {} to {name}", tracks(a)),
        (a, k) => format!("Added {} to {name} ({} already there)", tracks(a), k),
    })
}

/// The object a `--json` output of `melodie playlist …` printed; a failure is one launcher entry,
/// read by its id (its text is for the launcher and may hold the server's address), else
/// `otherwise`.
pub fn read_json(out: &str, otherwise: &str) -> Result<serde_json::Value, String> {
    let v: serde_json::Value =
        serde_json::from_str(out.trim()).map_err(|_| otherwise.to_string())?;
    if let Some(entry) = v.as_array().and_then(|a| a.first()) {
        return Err(match entry["id"].as_str() {
            Some("login") => "Navidrome wants a login: run melodie login",
            Some("auth-refused") => "Navidrome refused the login: run melodie login",
            Some("unreachable") => "Navidrome is unreachable",
            Some("not-found") => "Not found on the server",
            _ => otherwise,
        }
        .to_string());
    }
    Ok(v)
}

/// The playlist jobs, one at a time per playlist: a move writes back the whole order it read, so
/// a job started while another of the same playlist runs could undo what that one wrote.
pub struct Lanes<T> {
    /// A playlist is here while one of its jobs runs, with the jobs that wait behind it.
    waiting: HashMap<String, VecDeque<T>>,
}

impl<T> Default for Lanes<T> {
    fn default() -> Self {
        Self {
            waiting: HashMap::new(),
        }
    }
}

impl<T> Lanes<T> {
    /// `item`, for `playlist`: given back to be started now, or kept until the job of that
    /// playlist that runs is done.
    pub fn start(&mut self, playlist: &str, item: T) -> Option<T> {
        match self.waiting.get_mut(playlist) {
            Some(behind) => {
                behind.push_back(item);
                None
            }
            None => {
                self.waiting.insert(playlist.to_string(), VecDeque::new());
                Some(item)
            }
        }
    }

    /// Forgets the jobs waiting on `lane`; the one running, if any, goes on.
    pub fn drop_waiting(&mut self, lane: &str) {
        if let Some(behind) = self.waiting.get_mut(lane) {
            behind.clear();
        }
    }

    /// The job of `playlist` that ran is done: the next one to start, if any.
    pub fn done(&mut self, playlist: &str) -> Option<T> {
        let next = self.waiting.get_mut(playlist)?.pop_front();
        if next.is_none() {
            self.waiting.remove(playlist);
        }
        next
    }
}

/// `melodie` run for an action of the window: the window tells its failure in its own toast, so
/// the command sends no desktop notification for a failure on top (a success still notifies).
pub fn command(program: &Path) -> std::process::Command {
    let mut c = std::process::Command::new(program);
    c.env(melodie_core::env::QUIET_FAILURES_VAR, "1");
    c
}

/// The `melodie` beside this program when there is one (a build or an install puts both in the
/// same folder), else the one on the `PATH`.
pub fn program(this_exe: Option<&Path>) -> PathBuf {
    this_exe
        .and_then(Path::parent)
        .map(|dir| dir.join("melodie"))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("melodie"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failing_play_started_by_the_window_sends_no_desktop_notification() {
        let c = command(Path::new("/bin/melodie"));
        assert_eq!(c.get_program(), "/bin/melodie");
        let env: Vec<_> = c.get_envs().collect();
        let var = std::ffi::OsStr::new(melodie_core::env::QUIET_FAILURES_VAR);
        assert_eq!(env, [(var, Some(std::ffi::OsStr::new("1")))]);
    }

    #[test]
    fn a_second_edit_waits_for_the_first() {
        let mut lanes = Lanes::default();
        assert_eq!(lanes.start("p1", "move"), Some("move"));
        assert_eq!(lanes.start("p1", "add"), None, "waits for the move");
        assert_eq!(lanes.start("p1", "remove"), None);
        // another playlist does not wait
        assert_eq!(lanes.start("p2", "rename"), Some("rename"));
        assert_eq!(lanes.done("p1"), Some("add"));
        assert_eq!(lanes.start("p1", "check"), None, "the add runs now");
        assert_eq!(lanes.done("p1"), Some("remove"));
        assert_eq!(lanes.done("p1"), Some("check"));
        assert_eq!(lanes.done("p1"), None);
        assert_eq!(lanes.start("p1", "again"), Some("again"), "nothing runs");
        assert_eq!(lanes.done("p2"), None);
    }

    #[test]
    fn the_jobs_waiting_in_a_lane_can_be_dropped_and_the_one_running_stays() {
        let mut lanes = Lanes::default();
        assert_eq!(lanes.start("player", "first"), Some("first"));
        assert_eq!(lanes.start("player", "second"), None);
        assert_eq!(lanes.start("other", "alone"), Some("alone"));
        lanes.drop_waiting("player");
        assert_eq!(lanes.start("player", "third"), None, "first still runs");
        assert_eq!(lanes.done("player"), Some("third"));
        assert_eq!(lanes.done("player"), None);
        // nothing runs: nothing to drop, and the next one starts at once
        lanes.drop_waiting("player");
        assert_eq!(lanes.start("player", "fourth"), Some("fourth"));
    }

    #[test]
    fn the_jobs_that_replace_the_queue_are_told_apart_from_those_that_add_to_it() {
        let id = || "x".to_string();
        for job in [
            Job::PlayMix,
            Job::Rediscover,
            Job::Play {
                kind: Kind::Album,
                id: id(),
                shuffle: false,
            },
            Job::PlayTracks {
                ids: vec![id()],
                from: 0,
            },
            Job::ShuffleTracks { ids: vec![id()] },
            Job::Mix {
                kind: Kind::Artist,
                id: id(),
            },
            Job::MixGenre { name: id() },
        ] {
            assert!(job.replaces(), "{job:?}");
            assert!(job.plays(), "{job:?}");
        }
        for job in [
            Job::Queue {
                kind: Kind::Track,
                id: id(),
                next: true,
            },
            Job::QueueTracks {
                ids: vec![id()],
                next: false,
            },
            Job::QueueMix {
                mix: Box::new(Job::PlayMix),
                next: true,
            },
            Job::Star {
                kind: Kind::Track,
                id: id(),
                starred: true,
            },
            Job::DeletePlaylist { id: id() },
        ] {
            assert!(!job.replaces(), "{job:?}");
        }
    }

    #[test]
    fn the_jobs_that_touch_a_playlist_name_it() {
        let id = || "p1".to_string();
        let adding = || Adding::Tracks(vec!["t1".into()]);
        for job in [
            Job::AddToPlaylist {
                playlist: id(),
                what: adding(),
                allow_duplicates: false,
            },
            Job::CheckAdd {
                playlist: id(),
                what: adding(),
            },
            Job::RenamePlaylist {
                id: id(),
                name: "N".into(),
            },
            Job::DeletePlaylist { id: id() },
            Job::RemoveFromPlaylist {
                id: id(),
                picks: vec![(0, "t1".into())],
            },
            Job::MoveInPlaylist {
                id: id(),
                from: 0,
                to: 1,
                track: "t1".into(),
            },
            Job::Duplicates {
                id: id(),
                remove: true,
            },
        ] {
            assert_eq!(job.playlist(), Some("p1"), "{job:?}");
        }
        let create = Job::CreatePlaylist {
            name: "N".into(),
            with: Some(adding()),
        };
        assert_eq!(create.playlist(), None, "a new playlist waits for nothing");
        assert_eq!(Job::PlayMix.playlist(), None);
    }

    #[test]
    fn the_arguments_of_each_job() {
        assert_eq!(Job::PlayMix.args(), ["mix", "me"]);
        let like = Job::Star {
            kind: Kind::Track,
            id: "t1".into(),
            starred: true,
        };
        assert_eq!(like.args(), ["like", "track", "--", "t1"]);
        let unlike = Job::Star {
            kind: Kind::Track,
            id: "t1".into(),
            starred: false,
        };
        assert_eq!(unlike.args(), ["unlike", "track", "--", "t1"]);
    }

    #[test]
    fn an_album_or_an_artist_is_liked_by_its_kind() {
        let album = Job::Star {
            kind: Kind::Album,
            id: "-al1".into(),
            starred: true,
        };
        assert_eq!(album.args(), ["like", "album", "--", "-al1"]);
        let artist = Job::Star {
            kind: Kind::Artist,
            id: "ar1".into(),
            starred: false,
        };
        assert_eq!(artist.args(), ["unlike", "artist", "--", "ar1"]);
    }

    #[test]
    fn a_mix_goes_next_or_at_the_end_of_the_queue_with_the_flag_before_the_dashes() {
        let queue = |mix: Job, next| Job::QueueMix {
            mix: Box::new(mix),
            next,
        };
        let genre = || Job::MixGenre {
            name: "-Hip Hop".into(),
        };
        assert_eq!(
            queue(genre(), false).args(),
            ["mix", "--queue", "genre", "--", "-Hip Hop"]
        );
        assert_eq!(
            queue(genre(), true).args(),
            ["mix", "--next", "genre", "--", "-Hip Hop"]
        );
        assert_eq!(queue(Job::PlayMix, false).args(), ["mix", "--queue", "me"]);
        assert_eq!(queue(Job::PlayMix, true).args(), ["mix", "--next", "me"]);
        assert_eq!(
            queue(Job::Rediscover, false).args(),
            ["mix", "--queue", "rediscover"]
        );
        assert_eq!(
            queue(Job::Rediscover, true).args(),
            ["mix", "--next", "rediscover"]
        );
        assert_eq!(queue(Job::PlayMix, true).playlist(), None);
    }

    #[test]
    fn the_arguments_of_the_library_jobs() {
        let id = || "-al1".to_string();
        let play = Job::Play {
            kind: Kind::Album,
            id: id(),
            shuffle: true,
        };
        assert_eq!(play.args(), ["play", "album", "--shuffle", "--", "-al1"]);
        let play = Job::Play {
            kind: Kind::Track,
            id: id(),
            shuffle: false,
        };
        assert_eq!(play.args(), ["play", "track", "--", "-al1"]);
        let next = Job::Queue {
            kind: Kind::Playlist,
            id: id(),
            next: true,
        };
        assert_eq!(next.args(), ["queue", "playlist", "--next", "--", "-al1"]);
        let last = Job::Queue {
            kind: Kind::Album,
            id: id(),
            next: false,
        };
        assert_eq!(last.args(), ["queue", "album", "--", "-al1"]);
        let mix = Job::Mix {
            kind: Kind::Artist,
            id: id(),
        };
        assert_eq!(mix.args(), ["mix", "artist", "--", "-al1"]);
        assert_eq!(Job::Rediscover.args(), ["mix", "rediscover"]);
        let add = Job::AddToPlaylist {
            playlist: "-p1".into(),
            what: Adding::Album(id()),
            allow_duplicates: false,
        };
        assert_eq!(
            add.args(),
            ["playlist", "add", "--json", "--", "-p1", "album", "-al1"]
        );
    }

    #[test]
    fn several_tracks_go_in_one_call_their_ids_on_stdin() {
        let ids = || vec!["-t1".to_string(), "t2".into(), "t3".into()];
        let play = Job::PlayTracks {
            ids: ids(),
            from: 2,
        };
        assert_eq!(play.args(), ["play", "track", "--from", "3", "-"]);
        assert_eq!(play.stdin().as_deref(), Some("-t1\nt2\nt3\n"));
        let shuffle = Job::ShuffleTracks { ids: ids() };
        assert_eq!(shuffle.args(), ["play", "track", "--shuffle", "-"]);
        assert_eq!(shuffle.stdin(), play.stdin());
        let next = Job::QueueTracks {
            ids: ids(),
            next: true,
        };
        assert_eq!(next.args(), ["queue", "track", "--next", "-"]);
        assert_eq!(next.stdin(), play.stdin());
        let last = Job::QueueTracks {
            ids: ids(),
            next: false,
        };
        assert_eq!(last.args(), ["queue", "track", "-"]);
        assert_eq!(Job::Rediscover.stdin(), None, "the others read nothing");
    }

    #[test]
    fn the_arguments_of_the_playlist_jobs() {
        let tracks = || Adding::Tracks(vec!["-t1".into(), "t2".into()]);
        let check = Job::CheckAdd {
            playlist: "p1".into(),
            what: tracks(),
        };
        assert_eq!(
            check.args(),
            [
                "playlist",
                "add",
                "--dry-run",
                "--json",
                "--",
                "p1",
                "track",
                "-t1",
                "t2"
            ]
        );
        let anyway = Job::AddToPlaylist {
            playlist: "p1".into(),
            what: tracks(),
            allow_duplicates: true,
        };
        assert_eq!(
            anyway.args(),
            [
                "playlist",
                "add",
                "--json",
                "--allow-duplicates",
                "--",
                "p1",
                "track",
                "-t1",
                "t2"
            ]
        );
        let create = Job::CreatePlaylist {
            name: "-Road trip".into(),
            with: Some(Adding::Album("al1".into())),
        };
        assert_eq!(
            create.args(),
            ["playlist", "create", "--", "-Road trip", "album", "al1"]
        );
        let empty = Job::CreatePlaylist {
            name: "Road".into(),
            with: None,
        };
        assert_eq!(empty.args(), ["playlist", "create", "--", "Road"]);
        let rename = Job::RenamePlaylist {
            id: "-p1".into(),
            name: "New name".into(),
        };
        assert_eq!(
            rename.args(),
            ["playlist", "rename", "--", "-p1", "New name"]
        );
        let delete = Job::DeletePlaylist { id: "-p1".into() };
        assert_eq!(delete.args(), ["playlist", "delete", "--", "-p1"]);
        let remove = Job::RemoveFromPlaylist {
            id: "p1".into(),
            picks: vec![(0, "-t1".into()), (4, "t5".into())],
        };
        assert_eq!(
            remove.args(),
            [
                "playlist",
                "remove",
                "--expect=-t1",
                "--expect=t5",
                "--",
                "p1",
                "1",
                "5"
            ],
            "ranks counted from 1, an id may start with a dash"
        );
        let moved = Job::MoveInPlaylist {
            id: "p1".into(),
            from: 3,
            to: 0,
            track: "-t4".into(),
        };
        assert_eq!(
            moved.args(),
            ["playlist", "move", "--expect=-t4", "--", "p1", "4", "1"]
        );
        let list = Job::Duplicates {
            id: "-p1".into(),
            remove: false,
        };
        assert_eq!(
            list.args(),
            ["playlist", "duplicates", "--json", "--", "-p1"]
        );
        let clean = Job::Duplicates {
            id: "p1".into(),
            remove: true,
        };
        assert_eq!(
            clean.args(),
            ["playlist", "duplicates", "--json", "--remove", "--", "p1"]
        );
        let genre = Job::MixGenre {
            name: "-Hip Hop".into(),
        };
        assert_eq!(genre.args(), ["mix", "genre", "--", "-Hip Hop"]);
    }

    #[test]
    fn an_id_that_starts_with_a_dash_is_not_an_option() {
        let like = Job::Star {
            kind: Kind::Track,
            id: "-t1".into(),
            starred: true,
        };
        let args = like.args();
        let dashes = args.iter().position(|a| a == "--").unwrap();
        assert_eq!(args.iter().position(|a| a == "-t1"), Some(dashes + 1));
    }

    #[test]
    fn adding_to_a_playlist_says_what_went_in_and_what_was_there() {
        let out = |added: usize, already: usize| {
            let t = serde_json::json!({"id": "t"});
            serde_json::json!({"added": vec![t.clone(); added], "already": vec![t; already],
                               "playlist": {"id": "p1", "name": "Road"}})
            .to_string()
        };
        assert_eq!(
            added_message(&out(8, 2)).unwrap(),
            "Added 8 tracks to Road (2 already there)"
        );
        assert_eq!(added_message(&out(1, 0)).unwrap(), "Added 1 track to Road");
        assert_eq!(added_message(&out(0, 1)).unwrap(), "Already in Road");
        assert_eq!(
            added_message(&out(0, 3)).unwrap(),
            "All 3 tracks are already in Road"
        );
    }

    #[test]
    fn a_failed_add_never_shows_the_launcher_text() {
        let fail = r#"[{"id":"unreachable","name":"Navidrome injoignable","subtitle":"https://music.example.org"}]"#;
        let msg = added_message(fail).unwrap_err();
        assert_eq!(msg, "Navidrome is unreachable");
        assert!(added_message("not json").is_err());
    }

    #[test]
    fn melodie_beside_the_window_first() {
        let dir = tempfile::tempdir().unwrap();
        let gui = dir.path().join("melodie-desktop");
        assert_eq!(program(Some(&gui)), PathBuf::from("melodie"));
        std::fs::write(dir.path().join("melodie"), "").unwrap();
        assert_eq!(program(Some(&gui)), dir.path().join("melodie"));
        assert_eq!(program(None), PathBuf::from("melodie"));
    }
}
