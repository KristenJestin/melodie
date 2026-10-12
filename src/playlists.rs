//! Écriture des playlists (`playlist add|remove|create|rename|delete|pick`) :
//! l'API Subsonic d'abord, puis la copie locale, sans synchronisation complète.

use crate::api::Client;
use crate::ctl::{self, Oops};
use crate::db;
use crate::notify;
use crate::resolve;
use crate::search::{human_duration, shell_word, Kind};
use crate::state::{self, Failure};
use crate::store::Settings;
use crate::track::TrackInfo;
use anyhow::anyhow;
use poste_core::time::{now_secs, rfc3339};
use poste_core::Env;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::process::ExitCode;

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

/// Morceaux visés. Le morceau en cours vient du démon, un morceau de la copie
/// locale (durée), un album de l'API (ordre du serveur).
fn tracks_of(
    target: &Target,
    client: &Client,
    conn: Option<&Connection>,
) -> Result<Vec<TrackInfo>, Oops> {
    match target {
        Target::Current => {
            let n = ctl::current_track()?;
            Ok(vec![TrackInfo {
                id: n.id,
                title: n.title,
                duration: n.duration as u64,
                ..TrackInfo::default()
            }])
        }
        Target::Track(id) => {
            let title = conn
                .and_then(|c| db::name_of(c, db::StarKind::Track, id))
                .unwrap_or_else(|| id.clone());
            let duration = conn.and_then(|c| db::track_duration(c, id)).unwrap_or(0);
            Ok(vec![TrackInfo {
                id: id.clone(),
                title,
                duration: duration.max(0) as u64,
                ..TrackInfo::default()
            }])
        }
        Target::Album(id) => Ok(resolve::resolve(client, Kind::Album, id)?),
    }
}

fn call(client: &Client, method: &str, extra: &[(&str, String)]) -> Result<Value, Failure> {
    client
        .call(method, extra)
        .map_err(|e| state::from_api(e, client.url()))
}

fn playlist_name(conn: Option<&Connection>, id: &str) -> String {
    conn.and_then(|c| {
        c.query_row("SELECT name FROM playlists WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .ok()
    })
    .unwrap_or_else(|| id.to_string())
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

/// Ce qui est commun aux commandes d'écriture : connexion, copie ouverte.
struct Session {
    client: Client,
    conn: Option<Connection>,
}

fn session(env: &Env) -> Result<Session, Failure> {
    state::quick_check(env)?;
    let client = state::open_client(env)?;
    let conn = db::open_existing(env)?;
    if let Some(c) = &conn {
        c.busy_timeout(std::time::Duration::from_secs(2))?;
    }
    Ok(Session { client, conn })
}

fn finish(summary: &str, body: &str) {
    println!(
        "{summary}{}",
        if body.is_empty() {
            String::new()
        } else {
            format!(" : {body}")
        }
    );
    notify::send(summary, body);
}

fn tracks_label(tracks: &[TrackInfo]) -> String {
    match tracks {
        [t] => t.title.clone(),
        _ => format!("{} morceaux", tracks.len()),
    }
}

pub fn add_cmd(env: &Env, pid: &str, target: &Target) -> ExitCode {
    let r = (|| -> Result<(), Oops> {
        let s = session(env)?;
        let tracks = tracks_of(target, &s.client, s.conn.as_ref())?;
        let r = add(&s.client, s.conn.as_ref(), pid, &tracks, now_secs());
        state::record(env, &r);
        r?;
        finish(
            &format!("Ajouté à « {} »", playlist_name(s.conn.as_ref(), pid)),
            &tracks_label(&tracks),
        );
        Ok(())
    })();
    finish_result(r)
}

fn finish_result(r: Result<(), Oops>) -> ExitCode {
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(o) => ctl::report_oops(&o),
    }
}

fn plain(r: Result<(), Failure>) -> ExitCode {
    finish_result(r.map_err(Oops::Failure))
}

pub fn remove_cmd(env: &Env, pid: &str, index: usize) -> ExitCode {
    plain((|| {
        let s = session(env)?;
        let r = remove(&s.client, s.conn.as_ref(), pid, index, now_secs());
        state::record(env, &r);
        let t = r?;
        finish(
            &format!("Retiré de « {} »", playlist_name(s.conn.as_ref(), pid)),
            &t.title,
        );
        Ok(())
    })())
}

pub fn create_cmd(env: &Env, name: &str, target: Option<&Target>) -> ExitCode {
    let r = (|| -> Result<(), Oops> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Oops::Failure(Failure::Internal(anyhow!(
                "nom de playlist vide"
            ))));
        }
        let s = session(env)?;
        let tracks = match target {
            Some(t) => tracks_of(t, &s.client, s.conn.as_ref())?,
            None => Vec::new(),
        };
        let owner = Settings::load(env)?.user.unwrap_or_default();
        let r = create(
            &s.client,
            s.conn.as_ref(),
            &owner,
            name,
            &tracks,
            now_secs(),
        );
        state::record(env, &r);
        r?;
        let body = if tracks.is_empty() {
            "vide".to_string()
        } else {
            tracks_label(&tracks)
        };
        finish(&format!("Playlist créée : {name}"), &body);
        Ok(())
    })();
    finish_result(r)
}

pub fn rename_cmd(env: &Env, pid: &str, name: &str) -> ExitCode {
    plain((|| {
        let name = name.trim();
        if name.is_empty() {
            return Err(Failure::Internal(anyhow!("nom de playlist vide")));
        }
        let s = session(env)?;
        let old = playlist_name(s.conn.as_ref(), pid);
        let r = rename(&s.client, s.conn.as_ref(), pid, name, now_secs());
        state::record(env, &r);
        r?;
        finish("Playlist renommée", &format!("{old} → {name}"));
        Ok(())
    })())
}

pub fn delete_cmd(env: &Env, pid: &str) -> ExitCode {
    plain((|| {
        let s = session(env)?;
        let old = playlist_name(s.conn.as_ref(), pid);
        let r = delete(&s.client, s.conn.as_ref(), pid);
        state::record(env, &r);
        r?;
        finish("Playlist supprimée", &old);
        Ok(())
    })())
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
            "command": format!("xdg-terminal-exec poste music playlist create --prompt {}", target.words())}
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
                "command": format!("poste music playlist add {} {}", shell_word(&id), target.words())}
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

pub fn pick_cmd(env: &Env, target: &Target) -> Result<Vec<Value>, Failure> {
    let conn = crate::library(env)?;
    let user = Settings::load(env)?.user.unwrap_or_default();
    Ok(pick(&conn, &user, target)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{token, Creds};
    use crate::fake::{params, Fake};
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
        assert!(hits
            .iter()
            .any(|h| h.kind == Kind::Playlist && h.name == "Ma liste"));

        rename(&client, Some(&conn), "p9", "Autre nom", 1_790_000_001).unwrap();
        let q = last(&fake);
        let args = q.split_once('?').unwrap().1;
        assert_eq!(params(args, "playlistId"), ["p9"]);
        assert_eq!(params(args, "name"), ["Autre nom"]);
        assert!(crate::search::search(&conn, "ma lis")
            .unwrap()
            .iter()
            .all(|h| h.kind != Kind::Playlist));
        assert!(crate::search::search(&conn, "autre")
            .unwrap()
            .iter()
            .any(|h| h.name == "Autre nom"));

        delete(&client, Some(&conn), "p9").unwrap();
        assert!(last(&fake).starts_with("deletePlaylist?") && last(&fake).contains("id=p9"));
        assert!(crate::search::search(&conn, "autre")
            .unwrap()
            .iter()
            .all(|h| h.kind != Kind::Playlist));
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
            "xdg-terminal-exec poste music playlist create --prompt track s1"
        );
        assert_eq!(v[1]["name"], "Daft Mix");
        assert_eq!(
            v[1]["action"]["command"],
            "poste music playlist add p1 track s1"
        );
        assert_eq!(v[1]["subtitle"], "Playlist · 42 tracks · 2 h 05");
        let v = pick(&conn, "kris", &Target::Current).unwrap();
        assert_eq!(
            v[1]["action"]["command"],
            "poste music playlist add p1 current"
        );
        let v = pick(&conn, "kris", &Target::Album("al1".into())).unwrap();
        assert!(v[0]["action"]["command"]
            .as_str()
            .unwrap()
            .ends_with("--prompt album al1"));
        assert!(render_pick(&v).contains("Daft Mix"));
    }
}
