//! What the window asks of the `melodie` program: what needs the server (and so the keyring)
//! goes through it, so the window never holds a token.

use std::path::{Path, PathBuf};

/// What can be played, queued or mixed from the library.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// The heart of a track.
    Star { id: String, starred: bool },
    /// Replaces the queue with it and plays it.
    Play {
        kind: Kind,
        id: String,
        shuffle: bool,
    },
    /// Adds it after the current track (`next`) or at the end.
    Queue { kind: Kind, id: String, next: bool },
    /// An instant mix around it.
    Mix { kind: Kind, id: String },
    /// Adds a track or an album to a playlist; the tracks already there are left out.
    AddToPlaylist {
        playlist: String,
        kind: Kind,
        id: String,
    },
}

impl Job {
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
            Self::Mix { kind, id } => {
                let mut a = words(&["mix", kind.word(), "--"]);
                a.push(id.clone());
                a
            }
            Self::AddToPlaylist { playlist, kind, id } => {
                let mut a = words(&["playlist", "add", "--json", "--"]);
                a.extend([playlist.clone(), kind.word().into(), id.clone()]);
                a
            }
            Self::Star { id, starred } => vec![
                if *starred { "like" } else { "unlike" }.into(),
                "track".into(),
                // an id may start with a dash
                "--".into(),
                id.clone(),
            ],
        }
    }
}

/// What the window says after `playlist add --json` printed `out`: what went in, what was
/// already there, or why nothing went in (a failure is one launcher entry, read by its id: its
/// text is for the launcher and may hold the server's address).
pub fn added_message(out: &str) -> Result<String, String> {
    let v: serde_json::Value = serde_json::from_str(out.trim())
        .map_err(|_| "Could not add to the playlist".to_string())?;
    if let Some(entry) = v.as_array().and_then(|a| a.first()) {
        return Err(match entry["id"].as_str() {
            Some("login") => "Navidrome wants a login: run melodie login",
            Some("unreachable") => "Navidrome is unreachable",
            Some("not-found") => "Not found on the server",
            _ => "Could not add to the playlist",
        }
        .to_string());
    }
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
    fn the_arguments_of_each_job() {
        assert_eq!(Job::PlayMix.args(), ["mix", "me"]);
        let like = Job::Star {
            id: "t1".into(),
            starred: true,
        };
        assert_eq!(like.args(), ["like", "track", "--", "t1"]);
        let unlike = Job::Star {
            id: "t1".into(),
            starred: false,
        };
        assert_eq!(unlike.args(), ["unlike", "track", "--", "t1"]);
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
            kind: Kind::Album,
            id: id(),
        };
        assert_eq!(
            add.args(),
            ["playlist", "add", "--json", "--", "-p1", "album", "-al1"]
        );
    }

    #[test]
    fn an_id_that_starts_with_a_dash_is_not_an_option() {
        let like = Job::Star {
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
        let gui = dir.path().join("melodie-gui");
        assert_eq!(program(Some(&gui)), PathBuf::from("melodie"));
        std::fs::write(dir.path().join("melodie"), "").unwrap();
        assert_eq!(program(Some(&gui)), dir.path().join("melodie"));
        assert_eq!(program(None), PathBuf::from("melodie"));
    }
}
