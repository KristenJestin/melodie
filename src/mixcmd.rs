//! Commandes `mix` et `mixes` : colle entre la copie locale, le générateur
//! (`mix.rs`) et le démon. Les morceaux sont résolus localement (identifiants
//! de la copie) : aucun appel à l'API par morceau.

use crate::ctl;
use crate::mix::{self, Detail, Library, Rng, Source, SourceKind};
use crate::search::{shell_word, Hit, Kind, ICON_PLAY, ICON_QUEUE};
use crate::state::Failure;
use anyhow::anyhow;
use poste_core::time::now_secs;
use poste_core::Env;
use serde_json::{json, Value};
use std::process::ExitCode;

const ICON_MIX: &str = "\u{f049d}";
const ICON_REDISCOVER: &str = "\u{f0109}";

/// Quel mix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum What {
    Me,
    Rediscover,
    Source(SourceKind, String),
}

pub struct Opts {
    pub queue: bool,
    pub print: bool,
    pub json: bool,
    pub seed: Option<u64>,
    pub size: usize,
}

fn rng(seed: Option<u64>) -> Rng {
    seed.map_or_else(Rng::from_system, Rng::new)
}

/// Calcule un mix sur la copie locale.
pub fn compute(
    env: &Env,
    what: &What,
    seed: Option<u64>,
    size: usize,
) -> Result<Vec<Detail>, Failure> {
    let conn = crate::library(env)?;
    let lib = Library::load(&conn, now_secs() as i64)?;
    let mut rng = rng(seed);
    let picked = match what {
        What::Me => mix::personal(&lib, size, &mut rng),
        What::Rediscover => {
            let stale = mix::stale_albums(&conn, &lib)?;
            mix::rediscover(&lib, &stale, size, &mut rng)
        }
        What::Source(kind, key) => {
            let source: Source = mix::find_source(&conn, &lib, *kind, key)
                .map_err(|m| Failure::Internal(anyhow!("{m}")))?;
            mix::instant(&lib, &source, size, &mut rng)
        }
    };
    if picked.is_empty() {
        return Err(Failure::Internal(anyhow!(
            "aucun morceau éligible pour ce mix"
        )));
    }
    Ok(mix::details(&conn, &lib, &picked)?)
}

fn hit(d: &Detail) -> Hit {
    Hit {
        kind: Kind::Track,
        id: d.info.id.clone(),
        name: d.info.title.clone(),
        artist: d.info.artist.clone(),
        album: d.info.album.clone(),
        year: 0,
        count: 0,
        duration: d.info.duration as i64,
        cover: d.info.cover.clone(),
        starred: d.info.starred,
    }
}

/// Liste lisible : « 1. Titre · Artiste · Album ».
pub fn render(list: &[Detail]) -> String {
    let width = list.len().to_string().len();
    let mut out = String::new();
    for (n, d) in list.iter().enumerate() {
        out.push_str(&format!(
            "{:>width$}. {} · {} · {}\n",
            n + 1,
            d.info.title,
            d.info.artist,
            d.info.album
        ));
    }
    out
}

pub fn run(env: &Env, what: &What, o: &Opts) -> ExitCode {
    let r = compute(env, what, o.seed, o.size);
    if o.json {
        return match r {
            Ok(list) => {
                let entries = list.iter().map(|d| hit(d).to_json()).collect();
                println!("{}", crate::launcher_array(env, entries));
                ExitCode::SUCCESS
            }
            Err(f) => {
                println!("{}", Value::Array(vec![f.launcher_entry()]));
                ExitCode::SUCCESS
            }
        };
    }
    let list = match r {
        Ok(l) => l,
        Err(f) => return ctl::report(&f),
    };
    if o.print {
        print!("{}", render(&list));
        return ExitCode::SUCCESS;
    }
    let tracks = list.into_iter().map(|d| d.info).collect();
    match ctl::play_list(env, tracks, o.queue) {
        Ok(_) => ExitCode::SUCCESS,
        Err(f) => ctl::report(&f),
    }
}

fn entry(id: &str, name: &str, subtitle: &str, icon: &str, command: &str) -> Value {
    let cmd = |c: String| json!({"type": "command", "command": c});
    json!({
        "id": id,
        "name": name,
        "subtitle": subtitle,
        "icon": icon,
        "verb": "Play",
        "action": cmd(command.to_string()),
        "actions": [
            {"label": "Lire", "icon": ICON_PLAY, "action": cmd(command.to_string())},
            {"label": "Ajouter à la file", "icon": ICON_QUEUE, "action": cmd(format!("{command} --queue"))},
        ],
    })
}

/// Entrées du lanceur : mix personnel, redécouvertes, six genres principaux.
pub fn mixes(env: &Env) -> Result<Vec<Value>, Failure> {
    let conn = crate::library(env)?;
    mixes_in(&conn)
}

/// Comme `mixes`, sur une copie déjà ouverte.
pub fn mixes_in(conn: &rusqlite::Connection) -> Result<Vec<Value>, Failure> {
    let lib = Library::load(conn, now_secs() as i64)?;
    let mut out = vec![
        entry(
            "mix:me",
            "Mix perso",
            "Mix · Favorites, familiar and new",
            ICON_MIX,
            "poste music mix me",
        ),
        entry(
            "mix:rediscover",
            "Redécouvertes",
            "Mix · Albums not played for 90 days",
            ICON_REDISCOVER,
            "poste music mix rediscover",
        ),
    ];
    for (name, _hot, total) in mix::top_genres(&lib, 6) {
        out.push(entry(
            &format!("mix:genre:{}", name.to_lowercase()),
            &format!("Mix {name}"),
            &format!(
                "Mix · {total} {}",
                if total == 1 { "track" } else { "tracks" }
            ),
            ICON_MIX,
            &format!("poste music mix genre {}", shell_word(&name)),
        ));
    }
    Ok(out)
}

/// Contenu de l'extension du lanceur sans saisie : les playlists, puis les mixes.
pub fn home(env: &Env) -> Result<Vec<Value>, Failure> {
    let conn = crate::library(env)?;
    home_in(&conn)
}

pub fn home_in(conn: &rusqlite::Connection) -> Result<Vec<Value>, Failure> {
    let mut out: Vec<Value> = crate::search::playlists(conn)?.iter().map(|h| h.to_json()).collect();
    out.extend(mixes_in(conn)?);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    #[test]
    fn accueil_mixes_puis_playlists() {
        let d = tempfile::tempdir().unwrap();
        let mut conn = db::create(&d.path().join("l.db")).unwrap();
        let tx = conn.transaction().unwrap();
        db::insert_track(
            &tx,
            &json!({"id":"t1","title":"A","artist":"X","album":"Y","genres":[{"name":"Electro"}]}),
        )
        .unwrap();
        db::replace_playlists(
            &tx,
            &[json!({"id":"p1","name":"Daft Mix","songCount":3,"duration":600,"coverArt":"pl-p1"})],
        )
        .unwrap();
        tx.commit().unwrap();
        let v = home_in(&conn).unwrap();
        let ids: Vec<&str> = v.iter().map(|e| e["id"].as_str().unwrap()).collect();
        assert_eq!(ids[0], "playlist:p1");
        assert_eq!(&ids[1..3], ["mix:me", "mix:rediscover"]);
        assert!(ids[1..].iter().all(|i| i.starts_with("mix:")));
        assert_eq!(v[0]["_cover"], "pl-p1");
    }
}
