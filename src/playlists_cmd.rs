//! Commandes `playlist add|remove|create|rename|delete|pick` : le côté ligne de commande de
//! `playlists`.

use crate::api::Client;
use crate::ctl::{self, Oops};
use crate::db;
use crate::env::Env;
use crate::notify;
use crate::playlists::{Target, add, create, delete, pick, remove, rename};
use crate::resolve;
use crate::search::Kind;
use crate::state::{self, Failure};
use crate::store::Settings;
use crate::time::now_secs;
use crate::track::TrackInfo;
use anyhow::anyhow;
use rusqlite::Connection;
use serde_json::Value;
use std::process::ExitCode;

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

fn playlist_name(conn: Option<&Connection>, id: &str) -> String {
    conn.and_then(|c| {
        c.query_row("SELECT name FROM playlists WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .ok()
    })
    .unwrap_or_else(|| id.to_string())
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

pub fn pick_cmd(env: &Env, target: &Target) -> Result<Vec<Value>, Failure> {
    let conn = crate::library(env)?;
    let user = Settings::load(env)?.user.unwrap_or_default();
    Ok(pick(&conn, &user, target)?)
}
