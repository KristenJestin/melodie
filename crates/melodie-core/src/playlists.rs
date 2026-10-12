//! Écriture des playlists (`playlist add|remove|create|rename|delete|pick`) :
//! l'API Subsonic d'abord, puis la copie locale, sans synchronisation complète.

use crate::api::Client;
use crate::db;
use crate::search::{human_duration, shell_word};
use crate::state::{self, Failure};
use crate::time::rfc3339;
use crate::track::TrackInfo;
use anyhow::anyhow;
use rusqlite::Connection;
use serde_json::{Value, json};

const ICON_NEW: &str = "\u{f0415}";
const ICON_PLAYLIST: &str = "\u{f0cb8}";

/// Ce qu'on ajoute à une playlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Current,
    Track(String),
    Album(String),
}

impl Target {
    /// `[]` ou `current`, `track <id>`, `album <id>`.
    pub fn parse(args: &[String]) -> Result<Target, String> {
        match args {
            [] => Ok(Target::Current),
            [w] if w == "current" => Ok(Target::Current),
            [k, id] if k == "track" => Ok(Target::Track(id.clone())),
            [k, id] if k == "album" => Ok(Target::Album(id.clone())),
            _ => Err("cible attendue : current, track <id> ou album <id>".into()),
        }
    }

    /// Fin de commande équivalente (pour les actions du lanceur).
    pub fn words(&self) -> String {
        match self {
            Target::Current => "current".into(),
            Target::Track(id) => format!("track {}", shell_word(id)),
            Target::Album(id) => format!("album {}", shell_word(id)),
        }
    }
}

fn call(client: &Client, method: &str, extra: &[(&str, String)]) -> Result<Value, Failure> {
    client
        .call(method, extra)
        .map_err(|e| state::from_api(e, client.url()))
}

fn secs(tracks: &[TrackInfo]) -> i64 {
    tracks.iter().map(|t| t.duration as i64).sum()
}

/// `updatePlaylist … songIdToAdd=…` (autant de fois que de morceaux), puis la copie.
pub fn add(
    client: &Client,
    conn: Option<&Connection>,
    pid: &str,
    tracks: &[TrackInfo],
    at: u64,
) -> Result<(), Failure> {
    let mut params = vec![("playlistId", pid.to_string())];
    params.extend(tracks.iter().map(|t| ("songIdToAdd", t.id.clone())));
    call(client, "updatePlaylist", &params)?;
    if let Some(c) = conn {
        db::playlist_adjust(c, pid, tracks.len() as i64, secs(tracks), &rfc3339(at))?;
    }
    Ok(())
}

/// Retire le morceau de rang `index` (à partir de 0, comme l'API). Renvoie
/// le morceau retiré. Le rang est vérifié sur la playlist du serveur.
pub fn remove(
    client: &Client,
    conn: Option<&Connection>,
    pid: &str,
    index: usize,
    at: u64,
) -> Result<TrackInfo, Failure> {
    let r = call(client, "getPlaylist", &[("id", pid.to_string())])?;
    let entries = r["playlist"]["entry"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let Some(track) = entries.get(index).and_then(TrackInfo::from_api) else {
        return Err(Failure::Internal(anyhow!(
            "la playlist n'a pas de morceau au rang {index} (elle en compte {}, rangs à partir de 0)",
            entries.len()
        )));
    };
    call(
        client,
        "updatePlaylist",
        &[
            ("playlistId", pid.to_string()),
            ("songIndexToRemove", index.to_string()),
        ],
    )?;
    if let Some(c) = conn {
        db::playlist_adjust(c, pid, -1, -(track.duration as i64), &rfc3339(at))?;
    }
    Ok(track)
}

/// `createPlaylist` : renvoie l'identifiant créé. La copie reçoit la playlist.
pub fn create(
    client: &Client,
    conn: Option<&Connection>,
    owner: &str,
    name: &str,
    tracks: &[TrackInfo],
    at: u64,
) -> Result<String, Failure> {
    let mut params = vec![("name", name.to_string())];
    params.extend(tracks.iter().map(|t| ("songId", t.id.clone())));
    let r = call(client, "createPlaylist", &params)?;
    let mut p = r["playlist"].clone();
    let Some(id) = p["id"].as_str().map(str::to_string) else {
        return Err(Failure::Internal(anyhow!(
            "le serveur n'a pas renvoyé la playlist créée (la copie se met à jour toute seule, réessaie)"
        )));
    };
    if let Some(c) = conn {
        p["name"] = json!(name);
        if p.get("songCount").is_none() {
            p["songCount"] = json!(tracks.len());
            p["duration"] = json!(secs(tracks));
        }
        if p.get("owner").is_none() {
            p["owner"] = json!(owner);
        }
        if p.get("changed").is_none() {
            p["changed"] = json!(rfc3339(at));
        }
        db::playlist_insert(c, &p)?;
    }
    Ok(id)
}

pub fn rename(
    client: &Client,
    conn: Option<&Connection>,
    pid: &str,
    name: &str,
    at: u64,
) -> Result<(), Failure> {
    call(
        client,
        "updatePlaylist",
        &[("playlistId", pid.to_string()), ("name", name.to_string())],
    )?;
    if let Some(c) = conn {
        db::playlist_rename(c, pid, name, &rfc3339(at))?;
    }
    Ok(())
}

pub fn delete(client: &Client, conn: Option<&Connection>, pid: &str) -> Result<(), Failure> {
    call(client, "deletePlaylist", &[("id", pid.to_string())])?;
    if let Some(c) = conn {
        db::playlist_delete(c, pid)?;
    }
    Ok(())
}

/// Entrées du sous-menu « Ajouter à une playlist… » : « Nouvelle playlist… »,
/// puis les playlists de l'utilisateur (celles des autres ne s'éditent pas).
pub fn pick(conn: &Connection, user: &str, target: &Target) -> anyhow::Result<Vec<Value>> {
    let mut out = vec![json!({
        "id": "playlist-new",
        "name": "Nouvelle playlist…",
        "subtitle": "Créer une playlist et y ajouter",
        "icon": ICON_NEW,
        "action": {"type": "command",
            "command": format!("xdg-terminal-exec melodie playlist create --prompt {}", target.words())}
    })];
    let mut st = conn.prepare(
        "SELECT id, name, songCount, duration, coverArt FROM playlists
         WHERE owner IS NULL OR owner = ?1 ORDER BY name COLLATE NOCASE",
    )?;
    let rows = st.query_map([user], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, Option<String>>(4)?,
        ))
    })?;
    for row in rows {
        let (id, name, count, duration, cover) = row?;
        let mut v = json!({
            "id": format!("playlist-add:{id}"),
            "name": name,
            "subtitle": format!("Playlist · {count} {} · {}", if count == 1 { "track" } else { "tracks" }, human_duration(duration)),
            "icon": ICON_PLAYLIST,
            "action": {"type": "command",
                "command": format!("melodie playlist add {} {}", shell_word(&id), target.words())}
        });
        if let Some(c) = cover {
            v["_cover"] = json!(c);
        }
        out.push(v);
    }
    Ok(out)
}

/// Sortie lisible de `pick`.
pub fn render_pick(entries: &[Value]) -> String {
    entries
        .iter()
        .map(|e| {
            format!(
                "{}\n    {}\n",
                e["name"].as_str().unwrap_or(""),
                e["subtitle"].as_str().unwrap_or("")
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Creds, token};
    use crate::env::Env;
    use crate::fake::{Fake, params};
    use crate::search::Kind;
    use crate::sync;

    fn setup() -> (tempfile::TempDir, Env, Fake, Client) {
        let d = tempfile::tempdir().unwrap();
        let env = Env::with_home(d.path());
        let creds = Creds {
            u: "kris".into(),
            t: token("sesame", "c19b2d"),
            s: "c19b2d".into(),
        };
        let fake = Fake::start(4, &creds.t);
        let client = Client::new(&fake.url, creds);
        sync::run(&env, &client, true).unwrap();
        (d, env, fake, client)
    }

    fn last(fake: &Fake) -> String {
        fake.state.calls.lock().unwrap().last().unwrap().clone()
    }

    fn info(id: &str, duration: u64) -> TrackInfo {
        TrackInfo {
            id: id.into(),
            duration,
            ..TrackInfo::default()
        }
    }

    fn counts(conn: &Connection, pid: &str) -> (i64, i64) {
        conn.query_row(
            "SELECT songCount, duration FROM playlists WHERE id = ?1",
            [pid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    }

    #[test]
    fn cibles() {
        let w = |a: &[&str]| Target::parse(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(w(&[]), Ok(Target::Current));
        assert_eq!(w(&["current"]), Ok(Target::Current));
        assert_eq!(w(&["track", "t1"]), Ok(Target::Track("t1".into())));
        assert_eq!(w(&["album", "a b"]).unwrap().words(), "album 'a b'");
        assert!(w(&["playlist", "x"]).is_err());
        assert!(w(&["track"]).is_err());
    }

    #[test]
    fn ajout_retrait_et_compteurs_locaux() {
        let (_d, env, fake, client) = setup();
        let conn = db::open_existing(&env).unwrap().unwrap();
        assert_eq!(counts(&conn, "p1"), (42, 7500));
        add(
            &client,
            Some(&conn),
            "p1",
            &[info("s0", 200), info("s1", 100)],
            1_790_000_000,
        )
        .unwrap();
        let q = last(&fake);
        assert!(q.starts_with("updatePlaylist?"), "{q}");
        assert!(q.contains("playlistId=p1"), "{q}");
        let ids = params(q.split_once('?').unwrap().1, "songIdToAdd");
        assert_eq!(ids, ["s0", "s1"]);
        assert_eq!(counts(&conn, "p1"), (44, 7800));

        // Retrait : le rang est vérifié, le morceau retiré connu.
        let t = remove(&client, Some(&conn), "p1", 1, 1_790_000_000).unwrap();
        assert_eq!(t.id, "s1");
        let q = last(&fake);
        assert!(
            q.contains("songIndexToRemove=1") && q.contains("playlistId=p1"),
            "{q}"
        );
        assert_eq!(counts(&conn, "p1"), (43, 7600));
        let before = fake.state.calls.lock().unwrap().len();
        assert!(matches!(
            remove(&client, Some(&conn), "p1", 99, 0),
            Err(Failure::Internal(_))
        ));
        let calls = fake.state.calls.lock().unwrap().clone();
        assert!(
            calls[before..]
                .iter()
                .all(|c| !c.starts_with("updatePlaylist")),
            "pas d'écriture hors rang"
        );
    }

    #[test]
    fn creation_renommage_suppression() {
        let (_d, env, fake, client) = setup();
        let conn = db::open_existing(&env).unwrap().unwrap();
        let id = create(
            &client,
            Some(&conn),
            "kris",
            "Ma liste",
            &[info("s2", 150)],
            1_790_000_000,
        )
        .unwrap();
        assert_eq!(id, "p9");
        let q = last(&fake);
        let args = q.split_once('?').unwrap().1;
        assert!(q.starts_with("createPlaylist?"), "{q}");
        assert_eq!(params(args, "name"), ["Ma liste"]);
        assert_eq!(params(args, "songId"), ["s2"]);
        assert_eq!(counts(&conn, "p9"), (1, 200)); // chiffres du serveur
        let hits = crate::search::search(&conn, "ma lis").unwrap();
        assert!(
            hits.iter()
                .any(|h| h.kind == Kind::Playlist && h.name == "Ma liste")
        );

        rename(&client, Some(&conn), "p9", "Autre nom", 1_790_000_001).unwrap();
        let q = last(&fake);
        let args = q.split_once('?').unwrap().1;
        assert_eq!(params(args, "playlistId"), ["p9"]);
        assert_eq!(params(args, "name"), ["Autre nom"]);
        assert!(
            crate::search::search(&conn, "ma lis")
                .unwrap()
                .iter()
                .all(|h| h.kind != Kind::Playlist)
        );
        assert!(
            crate::search::search(&conn, "autre")
                .unwrap()
                .iter()
                .any(|h| h.name == "Autre nom")
        );

        delete(&client, Some(&conn), "p9").unwrap();
        assert!(last(&fake).starts_with("deletePlaylist?") && last(&fake).contains("id=p9"));
        assert!(
            crate::search::search(&conn, "autre")
                .unwrap()
                .iter()
                .all(|h| h.kind != Kind::Playlist)
        );
        // Playlist vide
        create(&client, Some(&conn), "kris", "Vide", &[], 0).unwrap();
        assert_eq!(counts(&conn, "p9"), (0, 0));
    }

    #[test]
    fn echecs_de_connexion() {
        let (_d, env, fake, client) = setup();
        let conn = db::open_existing(&env).unwrap().unwrap();
        *fake.state.expected_token.lock().unwrap() = "autre".into();
        assert!(matches!(
            add(&client, Some(&conn), "p1", &[info("s0", 1)], 0),
            Err(Failure::AuthRefused(_))
        ));
        assert_eq!(counts(&conn, "p1"), (42, 7500));
        *fake.state.raw.lock().unwrap() = Some((503, "x".into()));
        assert!(matches!(
            delete(&client, Some(&conn), "p1"),
            Err(Failure::Unreachable { .. })
        ));
        assert_eq!(counts(&conn, "p1"), (42, 7500));
    }

    #[test]
    fn menu_de_choix() {
        let (_d, env, _fake, _client) = setup();
        let conn = db::open_existing(&env).unwrap().unwrap();
        conn.execute(
            "INSERT INTO playlists(id, name, owner) VALUES ('px', 'Chez un autre', 'jean')",
            [],
        )
        .unwrap();
        let v = pick(&conn, "kris", &Target::Track("s1".into())).unwrap();
        assert_eq!(v.len(), 2, "playlist d'un autre utilisateur écartée");
        assert_eq!(v[0]["name"], "Nouvelle playlist…");
        assert_eq!(v[0]["icon"], "󰐕");
        assert_eq!(v[0]["action"]["type"], "command");
        assert_eq!(
            v[0]["action"]["command"],
            "xdg-terminal-exec melodie playlist create --prompt track s1"
        );
        assert_eq!(v[1]["name"], "Daft Mix");
        assert_eq!(
            v[1]["action"]["command"],
            "melodie playlist add p1 track s1"
        );
        assert_eq!(v[1]["subtitle"], "Playlist · 42 tracks · 2 h 05");
        let v = pick(&conn, "kris", &Target::Current).unwrap();
        assert_eq!(v[1]["action"]["command"], "melodie playlist add p1 current");
        let v = pick(&conn, "kris", &Target::Album("al1".into())).unwrap();
        assert!(
            v[0]["action"]["command"]
                .as_str()
                .unwrap()
                .ends_with("--prompt album al1")
        );
        assert!(render_pick(&v).contains("Daft Mix"));
    }
}
