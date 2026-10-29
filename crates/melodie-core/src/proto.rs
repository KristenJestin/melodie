//! Protocole du socket de contrôle : une requête JSON par ligne, une réponse
//! JSON par ligne. Emplacements des fichiers du démon.

use crate::env::Env;
use crate::track::TrackInfo;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

/// `$XDG_RUNTIME_DIR/melodie`.
pub fn runtime_dir() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .context("la variable XDG_RUNTIME_DIR n'est pas définie")?;
    Ok(PathBuf::from(base).join("melodie"))
}

pub fn socket_path() -> Result<PathBuf> {
    Ok(runtime_dir()?.join("melodie.sock"))
}

pub fn lock_path() -> Result<PathBuf> {
    Ok(runtime_dir()?.join("melodie.lock"))
}

/// `~/.local/state/melodie/melodie.log`.
pub fn log_path(env: &Env) -> PathBuf {
    env.state_dir().join("melodie.log")
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// Ce qu'on peut demander au démon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// Remplace la file et lance la lecture.
    Play {
        tracks: Vec<TrackInfo>,
        #[serde(default)]
        shuffle: bool,
        /// The track to start from (from 0): the ones before it stay in the queue, for Previous.
        #[serde(default, skip_serializing_if = "is_zero")]
        start: usize,
    },
    /// Adds to the queue: at the end, or right after the current track with
    /// `next` (starts playing if nothing plays).
    Queue {
        tracks: Vec<TrackInfo>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        next: bool,
    },
    /// The whole queue (answer: `queue`).
    QueueList,
    /// Plays the track at `index` (from 0).
    QueuePlay {
        index: usize,
    },
    /// Moves the track at `from` so that it ends up at `to` (both from 0).
    QueueMove {
        from: usize,
        to: usize,
    },
    /// Removes the track at `index` (from 0).
    QueueRemove {
        index: usize,
    },
    /// Removes every track after the current one.
    QueueClearAfter,
    /// Shuffles, once and in place, the tracks after the current one.
    QueueShuffleAfter,
    /// Turns the connection into the event stream (see `docs/events.md`).
    Watch,
    Toggle,
    Pause,
    Resume,
    Next,
    Prev,
    Stop,
    Now,
    /// Quitte le démon.
    Quit,
    /// Déplacement relatif (MPRIS `Seek`), en microsecondes.
    Seek {
        offset_us: i64,
    },
    /// Position absolue dans le morceau de numéro `track` (MPRIS `SetPosition`).
    SetPosition {
        track: u64,
        position_us: i64,
    },
    /// Volume de 0 à 1,3 (MPRIS `Volume`) ; 1 = 100 % de mpv.
    SetVolume {
        volume: f64,
    },
    /// Un morceau vient d'être aimé ou ne l'est plus : met à jour la file.
    SetStarred {
        id: String,
        starred: bool,
    },
}

/// État de lecture, pour `now`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Now {
    /// `playing`, `paused` ou `stopped`.
    pub state: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub album_artist: String,
    /// Secondes.
    #[serde(default)]
    pub duration: f64,
    /// Secondes.
    #[serde(default)]
    pub position: f64,
    #[serde(default)]
    pub quality: String,
    /// Chemin de la pochette locale, si elle est déjà là.
    #[serde(default)]
    pub cover: Option<String>,
    /// Rang du morceau dans la file (à partir de 1).
    #[serde(default)]
    pub index: usize,
    #[serde(default)]
    pub queue_len: usize,
    /// De 0 à 1 (1 = 100 %, jusqu'à 1,3).
    #[serde(default)]
    pub volume: f64,
    /// Le morceau en cours est un favori.
    #[serde(default)]
    pub starred: bool,
}

/// One track of the queue, as a client shows it. Never a stream address.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct QueueEntry {
    /// Number of the entry, unique for the daemon's life and kept when it moves
    /// (the same track queued twice gives two entries).
    pub entry: u64,
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub album_artist: String,
    /// Seconds.
    #[serde(default)]
    pub duration: f64,
    #[serde(default)]
    pub quality: String,
    /// Local path of the cover, if it is already there.
    #[serde(default)]
    pub cover: Option<String>,
    /// Cover id on the server (`coverArt`).
    #[serde(default)]
    pub cover_id: Option<String>,
    #[serde(default)]
    pub starred: bool,
}

/// The queue: every track, and the index (from 0) of the current one.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct QueueView {
    pub current: Option<usize>,
    pub tracks: Vec<QueueEntry>,
}

/// One line of the event stream (`watch`). Each event carries the new value,
/// never a difference: applying one twice is harmless.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Always first: everything a client needs to start from.
    Snapshot {
        now: Now,
        queue: QueueView,
        connection: String,
    },
    /// The queue changed (tracks or the index of the current one).
    Queue { queue: QueueView },
    /// Another track became current, or none (`index` and `track` null).
    Track {
        index: Option<usize>,
        track: Option<QueueEntry>,
    },
    /// `playing`, `paused` or `stopped`, with the position in seconds.
    State { state: String, position: f64 },
    /// Position while playing, at most once a second.
    Position { position: f64, duration: f64 },
    /// From 0 to 1.3 (1 = 100 %).
    Volume { volume: f64 },
    /// A track became a favourite or stopped being one.
    Starred { id: String, starred: bool },
    /// `ok`, `unreachable` or `auth-refused`.
    Connection { state: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub now: Option<Now>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<QueueView>,
}

impl Response {
    pub fn ok() -> Self {
        Self {
            ok: true,
            ..Self::default()
        }
    }

    pub fn now(now: Now) -> Self {
        Self {
            ok: true,
            now: Some(now),
            ..Self::default()
        }
    }

    pub fn queue(queue: QueueView) -> Self {
        Self {
            ok: true,
            queue: Some(queue),
            ..Self::default()
        }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(msg.into()),
            ..Self::default()
        }
    }
}

/// Ligne JSON (sans le saut de ligne) d'une valeur.
pub fn encode<T: Serialize>(v: &T) -> Result<String> {
    Ok(serde_json::to_string(v)?)
}

pub fn decode<T: for<'a> Deserialize<'a>>(line: &str) -> Result<T> {
    serde_json::from_str(line.trim()).context("message du socket invalide")
}

/// Envoie une requête sur une connexion ouverte et lit la réponse.
pub fn exchange(stream: &UnixStream, req: &Request, timeout: Duration) -> Result<Response> {
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let mut w = stream;
    let mut line = encode(req)?;
    line.push('\n');
    w.write_all(line.as_bytes())?;
    w.flush()?;
    let mut reply = String::new();
    let n = BufReader::new(stream).read_line(&mut reply)?;
    if n == 0 {
        bail!("le démon a fermé la connexion sans répondre");
    }
    decode(&reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn requetes_sur_une_ligne() {
        let t = TrackInfo {
            id: "s1".into(),
            title: "Un\nTitre".into(),
            ..TrackInfo::default()
        };
        let reqs = [
            Request::Play {
                tracks: vec![t.clone()],
                shuffle: true,
                start: 0,
            },
            Request::Play {
                tracks: vec![t.clone(), t.clone()],
                shuffle: false,
                start: 1,
            },
            Request::Queue {
                tracks: vec![t.clone()],
                next: false,
            },
            Request::Queue {
                tracks: vec![t],
                next: true,
            },
            Request::QueueList,
            Request::QueuePlay { index: 2 },
            Request::QueueMove { from: 0, to: 3 },
            Request::QueueRemove { index: 1 },
            Request::QueueClearAfter,
            Request::QueueShuffleAfter,
            Request::Watch,
            Request::Toggle,
            Request::Pause,
            Request::Resume,
            Request::Next,
            Request::Prev,
            Request::Stop,
            Request::Now,
            Request::Quit,
            Request::Seek { offset_us: -5 },
            Request::SetPosition {
                track: 3,
                position_us: 10,
            },
            Request::SetVolume { volume: 0.5 },
            Request::SetStarred {
                id: "s1".into(),
                starred: true,
            },
        ];
        for r in reqs {
            let line = encode(&r).unwrap();
            assert!(!line.contains('\n'), "une requête tient sur une ligne");
            assert_eq!(decode::<Request>(&line).unwrap(), r);
        }
    }

    #[test]
    fn forme_des_requetes() {
        assert_eq!(encode(&Request::Next).unwrap(), r#"{"cmd":"next"}"#);
        let v: serde_json::Value =
            serde_json::from_str(&encode(&Request::Seek { offset_us: 7 }).unwrap()).unwrap();
        assert_eq!(v, json!({"cmd": "seek", "offset_us": 7}));
        let p: Request = decode(r#"{"cmd":"play","tracks":[]}"#).unwrap();
        assert_eq!(
            p,
            Request::Play {
                tracks: vec![],
                shuffle: false,
                start: 0
            }
        );
        assert!(decode::<Request>(r#"{"cmd":"explose"}"#).is_err());
        // a request written before `next` existed still adds at the end
        let q: Request = decode(r#"{"cmd":"queue","tracks":[]}"#).unwrap();
        assert_eq!(
            q,
            Request::Queue {
                tracks: vec![],
                next: false
            }
        );
        assert_eq!(
            encode(&Request::QueueMove { from: 1, to: 0 }).unwrap(),
            r#"{"cmd":"queue_move","from":1,"to":0}"#
        );
        assert_eq!(encode(&Request::Watch).unwrap(), r#"{"cmd":"watch"}"#);
        assert!(decode::<Request>("pas du json").is_err());
    }

    #[test]
    fn reponses() {
        assert_eq!(encode(&Response::ok()).unwrap(), r#"{"ok":true}"#);
        let e = Response::err("raté");
        let back: Response = decode(&encode(&e).unwrap()).unwrap();
        assert!(!back.ok);
        assert_eq!(back.error.as_deref(), Some("raté"));
        let now = Now {
            state: "playing".into(),
            title: "T".into(),
            position: 1.5,
            ..Now::default()
        };
        let back: Response = decode(&encode(&Response::now(now.clone())).unwrap()).unwrap();
        assert_eq!(back.now, Some(now));
    }

    #[test]
    fn events_are_one_line_tagged_by_type() {
        let e = Event::Position {
            position: 1.5,
            duration: 3.0,
        };
        assert_eq!(
            encode(&e).unwrap(),
            r#"{"type":"position","position":1.5,"duration":3.0}"#
        );
        let e = Event::Track {
            index: None,
            track: None,
        };
        assert_eq!(
            encode(&e).unwrap(),
            r#"{"type":"track","index":null,"track":null}"#
        );
        let q = Event::Queue {
            queue: QueueView {
                current: Some(0),
                tracks: vec![QueueEntry {
                    entry: 1,
                    id: "t1".into(),
                    title: "A\nB".into(),
                    ..QueueEntry::default()
                }],
            },
        };
        let line = encode(&q).unwrap();
        assert!(!line.contains('\n'));
        assert_eq!(decode::<Event>(&line).unwrap(), q);
        // a client ignores fields it does not know
        let v: Event = decode(r#"{"type":"volume","volume":0.5,"later":1}"#).unwrap();
        assert_eq!(v, Event::Volume { volume: 0.5 });
    }

    #[test]
    fn queue_answer() {
        let r = Response::queue(QueueView::default());
        assert_eq!(
            encode(&r).unwrap(),
            r#"{"ok":true,"queue":{"current":null,"tracks":[]}}"#
        );
    }

    #[test]
    fn echange_sur_une_paire_de_sockets() {
        let (a, b) = UnixStream::pair().unwrap();
        let srv = std::thread::spawn(move || {
            let mut line = String::new();
            BufReader::new(&b).read_line(&mut line).unwrap();
            assert_eq!(decode::<Request>(&line).unwrap(), Request::Pause);
            let mut w = &b;
            w.write_all(format!("{}\n", encode(&Response::ok()).unwrap()).as_bytes())
                .unwrap();
        });
        let r = exchange(&a, &Request::Pause, Duration::from_secs(2)).unwrap();
        assert!(r.ok);
        srv.join().unwrap();
    }
}
