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
use std::collections::{HashMap, HashSet};

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

/// Applies an edit to a playlist in the local copy, as the server just did it: its counts and
/// its tracks, together, so a reader never sees one without the other.
fn edit_local(
    conn: &Connection,
    pid: &str,
    d_count: i64,
    d_secs: i64,
    at: u64,
    f: impl FnOnce(&mut Vec<String>),
) -> Result<(), Failure> {
    db::atomically(conn, || {
        db::playlist_adjust(conn, pid, d_count, d_secs, &rfc3339(at))?;
        // A copy that does not hold the tracks yet (made by an older version) would keep only
        // this edit: the next sync reads the playlist whole.
        if !db::playlist_is_synced(conn, pid)? {
            return Ok(());
        }
        let mut ids = db::playlist_track_ids(conn, pid)?;
        f(&mut ids);
        db::replace_playlist_tracks(conn, pid, &ids)
    })?;
    Ok(())
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
        edit_local(c, pid, tracks.len() as i64, secs(tracks), at, |ids| {
            ids.extend(tracks.iter().map(|t| t.id.clone()))
        })?;
    }
    Ok(())
}

/// What `add_checked` did: the tracks sent to the server, and those found already in the
/// playlist (left out unless duplicates were allowed).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AddOutcome {
    pub added: Vec<TrackInfo>,
    pub already: Vec<TrackInfo>,
}

/// `add`, but a track already in the playlist (by id, as the server lists it now) is left out
/// and reported, unless `allow_duplicates`. With nothing left to add, nothing is written.
pub fn add_checked(
    client: &Client,
    conn: Option<&Connection>,
    pid: &str,
    tracks: &[TrackInfo],
    allow_duplicates: bool,
    at: u64,
) -> Result<AddOutcome, Failure> {
    let mut out = AddOutcome::default();
    if allow_duplicates {
        out.added = tracks.to_vec();
    } else {
        // Read from the server: the local copy holds only the counts, and would be stale anyway.
        let mut seen: HashSet<String> = entries(client, pid)?.into_iter().map(|t| t.id).collect();
        for t in tracks {
            if seen.insert(t.id.clone()) {
                out.added.push(t.clone());
            } else {
                out.already.push(t.clone());
            }
        }
    }
    if !out.added.is_empty() {
        add(client, conn, pid, &out.added, at)?;
    }
    Ok(out)
}

/// The tracks of a playlist, in the server's order.
fn entries(client: &Client, pid: &str) -> Result<Vec<TrackInfo>, Failure> {
    let r = call(client, "getPlaylist", &[("id", pid.to_string())])?;
    Ok(r["playlist"]["entry"]
        .as_array()
        .map(|a| a.iter().filter_map(TrackInfo::from_api).collect())
        .unwrap_or_default())
}

/// A track as the `--json` outputs of `playlist add` and `playlist duplicates` show it.
pub fn track_json(t: &TrackInfo) -> Value {
    json!({"id": t.id, "title": t.title, "artist": t.artist, "album": t.album})
}

impl AddOutcome {
    pub fn to_json(&self) -> Value {
        json!({
            "added": self.added.iter().map(track_json).collect::<Vec<_>>(),
            "already": self.already.iter().map(track_json).collect::<Vec<_>>(),
        })
    }
}

/// A track met again in a playlist: its position, and the position of the occurrence it repeats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Duplicate {
    pub track: TrackInfo,
    pub index: usize,
    pub first: usize,
}

/// The repeats of a playlist: `exact` share a track id with an earlier entry, `similar` have
/// another id but the same title and artist.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Duplicates {
    pub total: usize,
    pub exact: Vec<Duplicate>,
    pub similar: Vec<Duplicate>,
}

impl Duplicates {
    pub fn to_json(&self) -> Value {
        let list = |l: &[Duplicate]| {
            l.iter()
                .map(|d| json!({"index": d.index, "first": d.first, "track": track_json(&d.track)}))
                .collect::<Vec<_>>()
        };
        json!({"total": self.total, "exact": list(&self.exact), "similar": list(&self.similar)})
    }
}

pub fn find_duplicates(entries: &[TrackInfo]) -> Duplicates {
    let mut first_of_id: HashMap<&str, usize> = HashMap::new();
    let mut first_of_name: HashMap<(String, String), usize> = HashMap::new();
    let mut out = Duplicates {
        total: entries.len(),
        ..Duplicates::default()
    };
    for (index, track) in entries.iter().enumerate() {
        if let Some(&first) = first_of_id.get(track.id.as_str()) {
            out.exact.push(Duplicate {
                track: track.clone(),
                index,
                first,
            });
            continue;
        }
        first_of_id.insert(&track.id, index);
        // Without a title there is nothing to compare.
        if track.title.trim().is_empty() {
            continue;
        }
        let name = (
            track.title.trim().to_lowercase(),
            track.artist.trim().to_lowercase(),
        );
        match first_of_name.get(&name) {
            Some(&first) => out.similar.push(Duplicate {
                track: track.clone(),
                index,
                first,
            }),
            None => {
                first_of_name.insert(name, index);
            }
        }
    }
    out
}

/// The duplicates of the playlist as the server lists it now.
pub fn duplicates(client: &Client, pid: &str) -> Result<Duplicates, Failure> {
    Ok(find_duplicates(&entries(client, pid)?))
}

/// Removes the exact duplicates, keeping the first occurrence of each track and the order of
/// the rest. Similar tracks (other id, same title and artist) are never removed. Returns what
/// was found: `exact` is what went, `similar` what stayed.
pub fn remove_duplicates(
    client: &Client,
    conn: Option<&Connection>,
    pid: &str,
    at: u64,
) -> Result<Duplicates, Failure> {
    let found = duplicates(client, pid)?;
    let gone = &found.exact;
    if gone.is_empty() {
        return Ok(found);
    }
    // Navidrome reads every `songIndexToRemove` against the order before the call, so they all
    // go in one request, as read.
    let mut params = vec![("playlistId", pid.to_string())];
    params.extend(
        gone.iter()
            .map(|d| ("songIndexToRemove", d.index.to_string())),
    );
    call(client, "updatePlaylist", &params)?;
    if let Some(c) = conn {
        let secs: i64 = gone.iter().map(|d| d.track.duration as i64).sum();
        edit_local(c, pid, -(gone.len() as i64), -secs, at, |ids| {
            let mut i = 0;
            ids.retain(|_| {
                i += 1;
                !gone.iter().any(|d| d.index == i - 1)
            })
        })?;
    }
    Ok(found)
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
        edit_local(c, pid, -1, -(track.duration as i64), at, |ids| {
            if index < ids.len() {
                ids.remove(index);
            }
        })?;
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
        let ids: Vec<String> = tracks.iter().map(|t| t.id.clone()).collect();
        db::atomically(c, || {
            db::playlist_insert(c, &p)?;
            db::replace_playlist_tracks(c, &id, &ids)?;
            db::mark_playlist_synced(c, &id, p["changed"].as_str())
        })?;
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
            u: "alice".into(),
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
            "alice",
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
        create(&client, Some(&conn), "alice", "Vide", &[], 0).unwrap();
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
        let v = pick(&conn, "alice", &Target::Track("s1".into())).unwrap();
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
        let v = pick(&conn, "alice", &Target::Current).unwrap();
        assert_eq!(v[1]["action"]["command"], "melodie playlist add p1 current");
        let v = pick(&conn, "alice", &Target::Album("al1".into())).unwrap();
        assert!(
            v[0]["action"]["command"]
                .as_str()
                .unwrap()
                .ends_with("--prompt album al1")
        );
        assert!(render_pick(&v).contains("Daft Mix"));
    }

    fn local_ids(conn: &Connection, pid: &str) -> Vec<String> {
        db::playlist_track_ids(conn, pid).unwrap()
    }

    #[test]
    fn the_local_copy_follows_the_edits_of_a_playlist() {
        let (_d, env, fake, client) = setup();
        let conn = db::open_existing(&env).unwrap().unwrap();
        assert_eq!(local_ids(&conn, "p1"), ["s0", "s1", "s2", "s3"]);
        add(&client, Some(&conn), "p1", &[info("s1", 200)], 0).unwrap();
        assert_eq!(local_ids(&conn, "p1"), ["s0", "s1", "s2", "s3", "s1"]);
        remove(&client, Some(&conn), "p1", 0, 0).unwrap();
        assert_eq!(local_ids(&conn, "p1"), ["s1", "s2", "s3", "s1"]);

        seed(
            &fake,
            "p1",
            &[("s1", "A", "B"), ("s2", "C", "D"), ("s1", "A", "B")],
        );
        db::replace_playlist_tracks(&conn, "p1", &["s1".into(), "s2".into(), "s1".into()]).unwrap();
        remove_duplicates(&client, Some(&conn), "p1", 0).unwrap();
        assert_eq!(local_ids(&conn, "p1"), ["s1", "s2"]);

        let id = create(&client, Some(&conn), "alice", "New", &[info("s3", 200)], 0).unwrap();
        assert_eq!(local_ids(&conn, &id), ["s3"]);
        delete(&client, Some(&conn), &id).unwrap();
        assert!(local_ids(&conn, &id).is_empty());
    }

    #[test]
    fn a_local_edit_that_fails_changes_neither_the_tracks_nor_the_counts() {
        let (_d, env, _fake, client) = setup();
        let conn = db::open_existing(&env).unwrap().unwrap();
        conn.execute_batch(
            "CREATE TRIGGER refuse BEFORE INSERT ON playlist_tracks WHEN NEW.track = 'boom'
             BEGIN SELECT RAISE(ABORT, 'refused'); END;",
        )
        .unwrap();
        let before = (counts(&conn, "p1"), local_ids(&conn, "p1"));
        assert!(add(&client, Some(&conn), "p1", &[info("boom", 200)], 0).is_err());
        assert_eq!((counts(&conn, "p1"), local_ids(&conn, "p1")), before);
    }

    #[test]
    fn an_edit_of_a_playlist_whose_tracks_are_not_held_yet_leaves_them_to_the_sync() {
        let (_d, env, _fake, client) = setup();
        let conn = db::open_existing(&env).unwrap().unwrap();
        // The copy of an older version: it has the playlist, not its tracks.
        conn.execute_batch("DELETE FROM playlist_tracks; DELETE FROM playlist_synced;")
            .unwrap();
        add(&client, Some(&conn), "p1", &[info("s1", 200)], 0).unwrap();
        assert_eq!(counts(&conn, "p1"), (43, 7700));
        assert!(
            local_ids(&conn, "p1").is_empty(),
            "one track is not the playlist"
        );
    }

    // --- duplicates ---

    fn seed(fake: &Fake, pid: &str, entries: &[(&str, &str, &str)]) {
        let list = entries
            .iter()
            .map(|(id, title, artist)| {
                json!({"id": id, "title": title, "artist": artist, "duration": 200})
            })
            .collect();
        fake.state
            .playlists
            .lock()
            .unwrap()
            .insert(pid.to_string(), list);
    }

    fn server_ids(fake: &Fake, pid: &str) -> Vec<String> {
        fake.state.playlists.lock().unwrap()[pid]
            .iter()
            .map(|e| e["id"].as_str().unwrap().to_string())
            .collect()
    }

    fn writes(fake: &Fake) -> usize {
        fake.calls_of("updatePlaylist")
    }

    fn ids(tracks: &[TrackInfo]) -> Vec<&str> {
        tracks.iter().map(|t| t.id.as_str()).collect()
    }

    #[test]
    fn adding_a_present_track_skips_it_and_says_so() {
        let (_d, env, fake, client) = setup();
        let conn = db::open_existing(&env).unwrap().unwrap();
        seed(&fake, "p1", &[("s1", "Titre numéro 1", "Beyoncé")]);
        let r = add_checked(&client, Some(&conn), "p1", &[info("s1", 200)], false, 0).unwrap();
        assert!(r.added.is_empty());
        assert_eq!(ids(&r.already), ["s1"]);
        assert_eq!(writes(&fake), 0, "nothing left to add, nothing written");
        assert_eq!(counts(&conn, "p1"), (42, 7500));
        assert_eq!(server_ids(&fake, "p1"), ["s1"]);
    }

    #[test]
    fn allow_duplicates_adds_a_present_track() {
        let (_d, env, fake, client) = setup();
        let conn = db::open_existing(&env).unwrap().unwrap();
        seed(&fake, "p1", &[("s1", "Titre numéro 1", "Beyoncé")]);
        let r = add_checked(&client, Some(&conn), "p1", &[info("s1", 200)], true, 0).unwrap();
        assert_eq!(ids(&r.added), ["s1"]);
        assert!(r.already.is_empty(), "the playlist is not even read");
        assert_eq!(server_ids(&fake, "p1"), ["s1", "s1"]);
        assert_eq!(counts(&conn, "p1"), (43, 7700));
    }

    #[test]
    fn an_album_with_2_of_10_tracks_present_adds_8_and_reports_2() {
        let d = tempfile::tempdir().unwrap();
        let env = Env::with_home(d.path());
        let creds = Creds {
            u: "alice".into(),
            t: token("sesame", "c19b2d"),
            s: "c19b2d".into(),
        };
        let fake = Fake::start(10, &creds.t);
        let client = Client::new(&fake.url, creds);
        sync::run(&env, &client, true).unwrap();
        let conn = db::open_existing(&env).unwrap().unwrap();
        seed(
            &fake,
            "p1",
            &[("s3", "Titre numéro 3", "Beyoncé"), ("s7", "x", "y")],
        );
        let album = crate::resolve::resolve(&client, Kind::Album, "al1").unwrap();
        assert_eq!(album.len(), 10);
        let r = add_checked(&client, Some(&conn), "p1", &album, false, 0).unwrap();
        assert_eq!(r.added.len(), 8);
        assert_eq!(ids(&r.already), ["s3", "s7"]);
        let q = last(&fake);
        assert_eq!(params(q.split_once('?').unwrap().1, "songIdToAdd").len(), 8);
        assert_eq!(counts(&conn, "p1"), (42 + 8, 7500 + 8 * 200));
        assert_eq!(server_ids(&fake, "p1").len(), 10);
    }

    #[test]
    fn a_track_repeated_in_the_request_is_added_once() {
        let (_d, _env, fake, client) = setup();
        seed(&fake, "p1", &[("s0", "Titre numéro 0", "Daft Punk")]);
        let t = [info("s1", 200), info("s1", 200)];
        let r = add_checked(&client, None, "p1", &t, false, 0).unwrap();
        assert_eq!(ids(&r.added), ["s1"]);
        assert_eq!(ids(&r.already), ["s1"]);
        assert_eq!(server_ids(&fake, "p1"), ["s0", "s1"]);
    }

    #[test]
    fn adding_with_a_refused_token_fails_without_writing() {
        let (_d, _env, fake, client) = setup();
        seed(&fake, "p1", &[]);
        *fake.state.expected_token.lock().unwrap() = "autre".into();
        assert!(matches!(
            add_checked(&client, None, "p1", &[info("s0", 1)], false, 0),
            Err(Failure::AuthRefused(_))
        ));
    }

    fn entry(id: &str, title: &str, artist: &str) -> TrackInfo {
        TrackInfo {
            id: id.into(),
            title: title.into(),
            artist: artist.into(),
            duration: 200,
            ..TrackInfo::default()
        }
    }

    #[test]
    fn listing_duplicates_tells_exact_ones_from_similar_ones() {
        let list = [
            entry("a", "Around the World", "Daft Punk"),
            entry("b", "Digital Love", "Daft Punk"),
            entry("a", "Around the World", "Daft Punk"),
            entry("c", " around the world", "daft punk"),
            entry("a", "Around the World", "Daft Punk"),
            entry("d", "Digital Love", "Someone Else"),
            entry("e", "", ""),
            entry("f", "", ""),
        ];
        let d = find_duplicates(&list);
        assert_eq!(d.total, 8);
        let exact: Vec<_> = d.exact.iter().map(|x| (x.index, x.first)).collect();
        assert_eq!(exact, [(2, 0), (4, 0)]);
        let similar: Vec<_> = d.similar.iter().map(|x| (x.index, x.first)).collect();
        assert_eq!(similar, [(3, 0)], "other artist or no title: not similar");
        assert_eq!(
            find_duplicates(&list[..2]),
            Duplicates {
                total: 2,
                ..Duplicates::default()
            }
        );
    }

    #[test]
    fn listing_duplicates_reads_the_server() {
        let (_d, _env, fake, client) = setup();
        seed(
            &fake,
            "p1",
            &[("s0", "T", "A"), ("s1", "U", "A"), ("s0", "T", "A")],
        );
        let d = duplicates(&client, "p1").unwrap();
        assert_eq!(d.exact.len(), 1);
        assert_eq!(d.exact[0].index, 2);
        assert_eq!(writes(&fake), 0);
    }

    #[test]
    fn removing_duplicates_keeps_the_first_occurrence_and_the_order() {
        let (_d, env, fake, client) = setup();
        let conn = db::open_existing(&env).unwrap().unwrap();
        seed(
            &fake,
            "p1",
            &[
                ("s0", "T0", "A"),
                ("s1", "T1", "A"),
                ("s0", "T0", "A"),
                ("s2", "T2", "A"),
                ("s1", "T1", "A"),
                ("s0", "T0", "A"),
            ],
        );
        let gone = remove_duplicates(&client, Some(&conn), "p1", 0)
            .unwrap()
            .exact;
        assert_eq!(gone.iter().map(|d| d.index).collect::<Vec<_>>(), [2, 4, 5]);
        assert_eq!(server_ids(&fake, "p1"), ["s0", "s1", "s2"]);
        assert_eq!(writes(&fake), 1, "one call, indexes of the order before it");
        let q = fake
            .state
            .calls
            .lock()
            .unwrap()
            .iter()
            .rfind(|c| c.starts_with("updatePlaylist"))
            .unwrap()
            .clone();
        assert_eq!(
            params(q.split_once('?').unwrap().1, "songIndexToRemove"),
            ["2", "4", "5"]
        );
        assert_eq!(counts(&conn, "p1"), (42 - 3, 7500 - 600));
    }

    #[test]
    fn removing_duplicates_leaves_similar_tracks_alone() {
        let (_d, _env, fake, client) = setup();
        seed(
            &fake,
            "p1",
            &[
                ("s0", "Same", "A"),
                ("s9", "Same", "A"),
                ("s0", "Same", "A"),
            ],
        );
        let found = remove_duplicates(&client, None, "p1", 0).unwrap();
        assert_eq!(server_ids(&fake, "p1"), ["s0", "s9"]);
        assert_eq!(found.exact.len(), 1);
        assert_eq!(found.similar.len(), 1, "reported, and left in place");
    }

    #[test]
    fn removing_duplicates_without_any_writes_nothing() {
        let (_d, _env, fake, client) = setup();
        seed(&fake, "p1", &[("s0", "T0", "A"), ("s1", "T1", "A")]);
        let found = remove_duplicates(&client, None, "p1", 0).unwrap();
        assert!(found.exact.is_empty());
        assert_eq!(writes(&fake), 0);
    }
}
