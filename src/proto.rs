//! Protocole du socket de contrôle : une requête JSON par ligne, une réponse
//! JSON par ligne. Emplacements des fichiers du démon.

use crate::track::TrackInfo;
use anyhow::{bail, Context, Result};
use poste_core::Env;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

/// `$XDG_RUNTIME_DIR/poste`.
pub fn runtime_dir() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .context("la variable XDG_RUNTIME_DIR n'est pas définie")?;
    Ok(PathBuf::from(base).join("poste"))
}

pub fn socket_path() -> Result<PathBuf> {
    Ok(runtime_dir()?.join("music.sock"))
}

pub fn lock_path() -> Result<PathBuf> {
    Ok(runtime_dir()?.join("music.lock"))
}

/// `~/.local/state/poste/music.log`.
pub fn log_path(env: &Env) -> PathBuf {
    env.state_dir().join("music.log")
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
    },
    /// Ajoute à la fin de la file (lance la lecture si rien ne joue).
    Queue {
        tracks: Vec<TrackInfo>,
    },
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub now: Option<Now>,
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
            error: None,
        }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(msg.into()),
            now: None,
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
            },
            Request::Queue { tracks: vec![t] },
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
                shuffle: false
            }
        );
        assert!(decode::<Request>(r#"{"cmd":"explose"}"#).is_err());
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
