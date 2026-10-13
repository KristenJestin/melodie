//! Commandes `playlist add|remove|create|rename|delete|pick` : le côté ligne de commande de
//! `playlists`.

use crate::ctl::{self, Oops};
use crate::notify;
use anyhow::anyhow;
use melodie_core::api::Client;
use melodie_core::db;
use melodie_core::env::Env;
use melodie_core::playlists::{
    AddOutcome, Duplicate, Target, add_checked, check_add, create, delete, duplicates, move_track,
    pick, remove_duplicates, remove_many, rename,
};
use melodie_core::resolve;
use melodie_core::search::Kind;
use melodie_core::state::{self, Failure};
use melodie_core::store::Settings;
use melodie_core::time::now_secs;
use melodie_core::track::TrackInfo;
use rusqlite::Connection;
use serde_json::{Value, json};
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
                artist: n.artist,
                album: n.album,
                duration: n.duration as u64,
                ..TrackInfo::default()
            }])
        }
        Target::Track(id) => Ok(vec![local_track(conn, id)]),
        Target::Tracks(ids) => Ok(ids.iter().map(|id| local_track(conn, id)).collect()),
        Target::Album(id) => Ok(resolve::resolve(client, Kind::Album, id)?),
    }
}

/// A track as the local copy knows it (its title and duration), or its id alone.
fn local_track(conn: Option<&Connection>, id: &str) -> TrackInfo {
    let title = conn
        .and_then(|c| db::name_of(c, db::StarKind::Track, id))
        .unwrap_or_else(|| id.to_string());
    let duration = conn.and_then(|c| db::track_duration(c, id)).unwrap_or(0);
    TrackInfo {
        id: id.to_string(),
        title,
        duration: duration.max(0) as u64,
        ..TrackInfo::default()
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

/// `{"playlist": {"id", "name"}, …}`: what the `--json` outputs of `add` and `duplicates` share.
fn with_playlist(conn: Option<&Connection>, pid: &str, mut body: Value) -> Value {
    body["playlist"] = json!({"id": pid, "name": playlist_name(conn, pid)});
    body
}

/// In `--json` a failure state is one launcher entry and code 0, like the other JSON outputs.
fn json_failure(o: &Oops) -> Option<ExitCode> {
    let Oops::Failure(f) = o else { return None };
    println!("{}", Value::Array(vec![f.launcher_entry()]));
    Some(ExitCode::SUCCESS)
}

/// What `playlist add` does with the tracks already in the playlist.
#[derive(Debug, Clone, Copy)]
pub struct Duplicates {
    /// Added anyway.
    pub allow: bool,
    /// Nothing is written: the output says what would be added and what is there already.
    pub dry_run: bool,
}

pub fn add_cmd(env: &Env, pid: &str, target: &Target, dup: Duplicates, json: bool) -> ExitCode {
    let r = (|| -> Result<(), Oops> {
        let s = session(env)?;
        let tracks = tracks_of(target, &s.client, s.conn.as_ref())?;
        let r = if dup.dry_run {
            check_add(&s.client, pid, &tracks)
        } else {
            add_checked(
                &s.client,
                s.conn.as_ref(),
                pid,
                &tracks,
                dup.allow,
                now_secs(),
            )
        };
        state::record(env, &r);
        let done = r?;
        let name = playlist_name(s.conn.as_ref(), pid);
        if json {
            let mut body = with_playlist(s.conn.as_ref(), pid, done.to_json());
            body["allowDuplicates"] = json!(dup.allow);
            if dup.dry_run {
                body["dryRun"] = json!(true);
            }
            println!("{body}");
        } else if dup.dry_run {
            print_dry_run(&name, &done);
        } else if done.added.is_empty() {
            finish(
                &format!("Déjà dans « {name} »"),
                &tracks_label(&done.already),
            );
        } else {
            let mut body = tracks_label(&done.added);
            if !done.already.is_empty() {
                body.push_str(&format!(" ({} déjà présents, ignorés)", done.already.len()));
            }
            finish(&format!("Ajouté à « {name} »"), &body);
        }
        Ok(())
    })();
    match r {
        Err(o) if json => json_failure(&o).unwrap_or_else(|| ctl::report_oops(&o)),
        r => finish_result(r),
    }
}

fn print_dry_run(name: &str, done: &AddOutcome) {
    println!(
        "À ajouter à « {name} » : {} ; déjà présents : {}",
        done.added.len(),
        done.already.len()
    );
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

/// Ranks are counted from 1 for a person; the `--json` output keeps the 0-based index.
fn duplicate_lines(list: &[Duplicate]) -> String {
    list.iter()
        .map(|d| {
            format!(
                "  rang {} : {} (comme le rang {})\n",
                d.index + 1,
                d.track.title,
                d.first + 1
            )
        })
        .collect()
}

/// `playlist duplicates <id> [--remove]`: lists the repeats, and with `remove` drops the exact
/// ones. The similar ones (other id, same title and artist) are only ever listed.
pub fn duplicates_cmd(env: &Env, pid: &str, remove: bool, json: bool) -> ExitCode {
    let r = (|| -> Result<(), Failure> {
        let s = session(env)?;
        let r = if remove {
            remove_duplicates(&s.client, s.conn.as_ref(), pid, now_secs())
        } else {
            duplicates(&s.client, pid)
        };
        state::record(env, &r);
        let found = r?;
        let name = playlist_name(s.conn.as_ref(), pid);
        if json {
            let mut body = with_playlist(s.conn.as_ref(), pid, found.to_json());
            body["removed"] = if remove {
                body["exact"].clone()
            } else {
                json!([])
            };
            println!("{body}");
            return Ok(());
        }
        let (n, m) = (found.exact.len(), found.similar.len());
        if n + m == 0 {
            println!("Aucun doublon dans « {name} » ({} morceaux)", found.total);
            return Ok(());
        }
        if n > 0 {
            println!("Doublons exacts dans « {name} » (même identifiant) :");
            print!("{}", duplicate_lines(&found.exact));
        }
        if m > 0 {
            println!("Doublons probables (autre identifiant, même titre et même artiste) :");
            print!("{}", duplicate_lines(&found.similar));
        }
        if remove && n > 0 {
            let left = if m > 0 {
                format!(" ; {m} doublons probables laissés")
            } else {
                String::new()
            };
            finish(
                &format!("Doublons retirés de « {name} »"),
                &format!("{n} morceaux{left}"),
            );
        } else if remove {
            println!("Rien à retirer : les doublons probables ne sont jamais retirés");
        }
        Ok(())
    })();
    match r {
        Err(f) if json => json_failure(&Oops::Failure(f)).unwrap(),
        r => plain(r),
    }
}

/// Ranks are what a person reads (from 1, like `duplicates`); the 0-based indexes of Subsonic's
/// `songIndexToRemove` are computed here, once. `expect`, when given, holds the id of each rank.
pub fn remove_cmd(env: &Env, pid: &str, ranks: &[usize], expect: &[String]) -> ExitCode {
    plain((|| {
        if !expect.is_empty() && expect.len() != ranks.len() {
            return Err(Failure::Internal(anyhow!(
                "--expect is given once per rank ({} ranks, {} ids)",
                ranks.len(),
                expect.len()
            )));
        }
        let picks = ranks
            .iter()
            .enumerate()
            .map(|(n, &rank)| Ok((index_of(rank)?, expect.get(n).map(String::as_str))))
            .collect::<Result<Vec<_>, Failure>>()?;
        let s = session(env)?;
        let r = remove_many(&s.client, s.conn.as_ref(), pid, &picks, now_secs());
        state::record(env, &r);
        let gone = r?;
        finish(
            &format!("Retiré de « {} »", playlist_name(s.conn.as_ref(), pid)),
            &tracks_label(&gone),
        );
        Ok(())
    })())
}

/// The 0-based index of a rank counted from 1.
fn index_of(rank: usize) -> Result<usize, Failure> {
    rank.checked_sub(1).ok_or_else(|| {
        Failure::Internal(anyhow!("rank 0 does not exist: ranks are counted from 1"))
    })
}

/// `playlist move <id> <from> <to>`: ranks from 1.
pub fn move_cmd(env: &Env, pid: &str, from: usize, to: usize, expect: Option<&str>) -> ExitCode {
    plain((|| {
        let (f, t) = (index_of(from)?, index_of(to)?);
        let s = session(env)?;
        let r = move_track(&s.client, s.conn.as_ref(), pid, f, t, expect, now_secs());
        state::record(env, &r);
        let moved = r?;
        finish(
            &format!("Déplacé dans « {} »", playlist_name(s.conn.as_ref(), pid)),
            &format!("{} (rang {from} → {to})", moved.title),
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
