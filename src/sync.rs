//! Mise à jour de la copie locale depuis le serveur.

use crate::api::Client;
use crate::db;
use crate::state::{from_api, Failure};
use poste_core::time::now_secs;
use poste_core::Env;
use rusqlite::{Connection, Transaction};
use serde_json::Value;
use std::collections::HashSet;
use std::io::IsTerminal;

const PAGE: usize = 500;

#[derive(Debug, PartialEq, Eq)]
pub enum Kind {
    Full,
    Light,
}

#[derive(Debug)]
pub struct Report {
    pub kind: Kind,
    pub counts: db::Counts,
}

fn call(client: &Client, method: &str, extra: &[(&str, String)]) -> Result<Value, Failure> {
    client
        .call(method, extra)
        .map_err(|e| from_api(e, client.url()))
}

fn list(v: &Value, path: &[&str]) -> Vec<Value> {
    let mut cur = v;
    for k in path {
        cur = &cur[*k];
    }
    cur.as_array().cloned().unwrap_or_default()
}

/// `lastScan` du serveur ; chaîne vide s'il n'en donne pas.
fn last_scan(client: &Client) -> Result<String, Failure> {
    let r = call(client, "getScanStatus", &[])?;
    Ok(r["scanStatus"]["lastScan"]
        .as_str()
        .unwrap_or("")
        .to_string())
}

/// Parcourt `search3` avec une requête vide, par pages de 500, jusqu'à épuisement.
/// `kind` : `artist`, `album` ou `song`. `insert` renvoie `true` pour un élément nouveau.
fn page_through(
    client: &Client,
    kind: &str,
    label: &str,
    tx: &Transaction,
    insert: fn(&Transaction, &Value) -> anyhow::Result<bool>,
) -> Result<usize, Failure> {
    let tty = std::io::stderr().is_terminal();
    let mut total = 0;
    let mut offset = 0;
    loop {
        let mut p = vec![("query", String::new())];
        for k in ["artist", "album", "song"] {
            p.push((
                match k {
                    "artist" => "artistCount",
                    "album" => "albumCount",
                    _ => "songCount",
                },
                if k == kind {
                    PAGE.to_string()
                } else {
                    "0".into()
                },
            ));
        }
        p.push((
            match kind {
                "artist" => "artistOffset",
                "album" => "albumOffset",
                _ => "songOffset",
            },
            offset.to_string(),
        ));
        let r = call(client, "search3", &p)?;
        let items = list(&r, &["searchResult3", kind]);
        if items.is_empty() {
            break;
        }
        let mut fresh = 0;
        for v in &items {
            if insert(tx, v)? {
                fresh += 1;
            }
        }
        // Un serveur qui ignore le décalage renverrait toujours la même page.
        if fresh == 0 {
            break;
        }
        total += fresh;
        offset += items.len();
        if tty {
            eprint!("\r{label} : {total}   ");
        }
    }
    if tty {
        eprintln!("\r{label} : {total}   ");
    } else {
        eprintln!("{label} : {total}");
    }
    Ok(total)
}

fn refresh_playlists_and_starred(client: &Client, tx: &Transaction) -> Result<(), Failure> {
    let pl = call(client, "getPlaylists", &[])?;
    db::replace_playlists(tx, &list(&pl, &["playlists", "playlist"]))?;
    let st = call(client, "getStarred2", &[])?;
    db::apply_starred(tx, &st["starred2"])?;
    Ok(())
}

/// Copie complète dans un fichier temporaire, puis remplacement atomique.
fn full(env: &Env, client: &Client) -> Result<Report, Failure> {
    let target = db::db_path(env);
    let mut tmp = target.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    let result = full_into(client, &tmp);
    match result {
        Ok(report) => {
            // Les morceaux passés ne viennent pas du serveur : on les garde.
            if let Err(e) = db::carry_skips(&target, &tmp) {
                let _ = std::fs::remove_file(&tmp);
                return Err(e.into());
            }
            std::fs::rename(&tmp, &target)
                .map_err(|e| anyhow::anyhow!("remplacement de {} : {e}", target.display()))?;
            Ok(report)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn full_into(client: &Client, path: &std::path::Path) -> Result<Report, Failure> {
    // Lu avant la copie : un scan survenu pendant la copie déclenchera la suivante.
    let scan = last_scan(client)?;
    let mut conn = db::create(path)?;
    {
        let tx = conn.transaction()?;
        page_through(client, "artist", "Artistes", &tx, db::insert_artist)?;
        page_through(client, "album", "Albums", &tx, db::insert_album)?;
        page_through(client, "song", "Morceaux", &tx, db::insert_track)?;
        refresh_playlists_and_starred(client, &tx)?;
        db::set_meta(&tx, "lastScan", &scan)?;
        db::set_meta(&tx, "synced_at", &now_secs().to_string())?;
        db::set_meta(&tx, "full_synced_at", &now_secs().to_string())?;
        tx.commit()?;
    }
    let counts = db::counts(&conn)?;
    drop(conn);
    Ok(Report {
        kind: Kind::Full,
        counts,
    })
}

/// Playlists, favoris et compteurs d'écoute seulement.
fn light(client: &Client, conn: &mut Connection) -> Result<Report, Failure> {
    let mut lists = Vec::new();
    for t in ["recent", "frequent"] {
        let r = call(
            client,
            "getAlbumList2",
            &[("type", t.to_string()), ("size", PAGE.to_string())],
        )?;
        lists.extend(list(&r, &["albumList2", "album"]));
    }
    let tx = conn.transaction()?;
    refresh_playlists_and_starred(client, &tx)?;
    let mut seen = HashSet::new();
    for a in &lists {
        let Some(id) = a["id"].as_str() else { continue };
        if !seen.insert(id.to_string()) {
            continue;
        }
        // Les compteurs des morceaux ne sont relus que pour les albums qui ont bougé.
        if db::update_album_counters(&tx, a)? {
            let r = call(client, "getAlbum", &[("id", id.to_string())])?;
            db::update_track_counters(&tx, &list(&r, &["album", "song"]))?;
        }
    }
    db::set_meta(&tx, "synced_at", &now_secs().to_string())?;
    tx.commit()?;
    Ok(Report {
        kind: Kind::Light,
        counts: db::counts(conn)?,
    })
}

/// Met la copie à jour. `force_full` : copie complète sans comparer `lastScan`.
pub fn run(env: &Env, client: &Client, force_full: bool) -> Result<Report, Failure> {
    if force_full {
        return full(env, client);
    }
    let Some(mut conn) = db::open_existing(env)? else {
        return full(env, client);
    };
    let stored = db::get_meta(&conn, "lastScan").unwrap_or_default();
    let current = last_scan(client)?;
    if current.is_empty() || current != stored {
        drop(conn);
        return full(env, client);
    }
    light(client, &mut conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{token, ApiError, Creds};
    use crate::fake::Fake;
    use crate::search;
    use std::sync::atomic::Ordering;

    fn setup(n: usize) -> (tempfile::TempDir, Env, Fake, Client) {
        let d = tempfile::tempdir().unwrap();
        let env = Env::with_home(d.path());
        let creds = Creds {
            u: "kris".into(),
            t: token("sesame", "c19b2d"),
            s: "c19b2d".into(),
        };
        let fake = Fake::start(n, &creds.t);
        let client = Client::new(&fake.url, creds);
        (d, env, fake, client)
    }

    #[test]
    fn copie_complete_paginee() {
        let (_d, env, fake, client) = setup(1203);
        let r = run(&env, &client, false).unwrap();
        assert_eq!(r.kind, Kind::Full);
        assert_eq!(r.counts.tracks, 1203);
        assert_eq!(r.counts.artists, 2);
        assert_eq!(r.counts.albums, 1);
        assert_eq!(r.counts.playlists, 1);
        // 3 pages de morceaux + la page vide.
        assert_eq!(fake.calls_of("search3"), 4 + 2 + 2);
        assert!(!db::db_path(&env).with_extension("db.tmp").exists());
        let conn = db::open_existing(&env).unwrap().unwrap();
        assert_eq!(
            db::get_meta(&conn, "lastScan").as_deref(),
            Some("2026-10-01T10:00:00Z")
        );
        let starred: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tracks WHERE starred IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(starred, 1);
        let hits = search::search(&conn, "daf pun").unwrap();
        assert!(hits.iter().any(|h| h.name == "Daft Punk"));
        let hits = search::search(&conn, "daft mi").unwrap();
        assert!(hits.iter().any(|h| h.name == "Daft Mix"));
    }

    #[test]
    fn incrementale_puis_nouveau_scan() {
        let (_d, env, fake, client) = setup(10);
        run(&env, &client, false).unwrap();
        fake.clear_calls();
        let r = run(&env, &client, false).unwrap();
        assert_eq!(r.kind, Kind::Light);
        assert_eq!(fake.calls_of("search3"), 0);
        assert_eq!(fake.calls_of("getAlbumList2"), 2);
        assert_eq!(fake.calls_of("getAlbum"), 1);
        let conn = db::open_existing(&env).unwrap().unwrap();
        let (pc, al): (i64, i64) = conn
            .query_row(
                "SELECT (SELECT playCount FROM tracks WHERE id='s0'), (SELECT playCount FROM albums WHERE id='al1')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((pc, al), (5, 7));
        drop(conn);

        *fake.state.last_scan.lock().unwrap() = "2026-10-02T10:00:00Z".into();
        fake.clear_calls();
        assert_eq!(run(&env, &client, false).unwrap().kind, Kind::Full);
        assert!(fake.calls_of("search3") > 0);
        fake.clear_calls();
        assert_eq!(run(&env, &client, true).unwrap().kind, Kind::Full);
    }

    #[test]
    fn jeton_refuse() {
        let (_d, env, fake, client) = setup(3);
        for code in 40..=44 {
            *fake.state.fail_code.lock().unwrap() = Some(code);
            assert!(matches!(
                run(&env, &client, true),
                Err(Failure::AuthRefused(_))
            ));
        }
        // Rien n'a été remplacé.
        assert!(!db::db_path(&env).exists());
        *fake.state.fail_code.lock().unwrap() = None;
        *fake.state.expected_token.lock().unwrap() = "autre".into();
        assert!(matches!(client.ping(), Err(ApiError::AuthRefused(_))));
    }

    #[test]
    fn injoignable() {
        let (_d, env, fake, client) = setup(3);
        *fake.state.raw.lock().unwrap() = Some((500, "boom".into()));
        assert!(matches!(
            run(&env, &client, true),
            Err(Failure::Unreachable { .. })
        ));
        *fake.state.raw.lock().unwrap() = Some((200, "pas du json".into()));
        assert!(matches!(client.ping(), Err(ApiError::Unreachable(_))));
        *fake.state.raw.lock().unwrap() = Some((404, "{}".into()));
        assert!(matches!(client.ping(), Err(ApiError::Unreachable(_))));
        // Port fermé.
        let dead = Client::new(
            "http://127.0.0.1:1",
            Creds {
                u: "k".into(),
                t: "t0k3n".into(),
                s: "s4lt".into(),
            },
        );
        match dead.ping() {
            Err(ApiError::Unreachable(m)) => {
                assert!(!m.contains("t0k3n") && !m.contains("s4lt") && !m.contains("127.0.0.1"));
            }
            other => panic!("attendu injoignable : {other:?}"),
        }
        assert!(!db::db_path(&env).exists());
        assert!(fake.state.n_requests.load(Ordering::SeqCst) > 0);
    }

    #[test]
    fn echec_en_cours_de_copie_garde_l_ancienne() {
        let (_d, env, fake, client) = setup(10);
        run(&env, &client, true).unwrap();
        *fake.state.raw.lock().unwrap() = Some((500, "boom".into()));
        assert!(run(&env, &client, true).is_err());
        let conn = db::open_existing(&env).unwrap().unwrap();
        assert_eq!(db::counts(&conn).unwrap().tracks, 10);
        assert!(!db::db_path(&env).with_extension("db.tmp").exists());
    }

    #[test]
    fn temps_de_recherche() {
        let (_d, env, _fake, client) = setup(20000);
        run(&env, &client, true).unwrap();
        let conn = db::open_existing(&env).unwrap().unwrap();
        search::search(&conn, "daf pun").unwrap();
        // « daf pun » : 10 000 morceaux de Daft Punk ; « zzz » : aucun ; « titre num 19 » : sélectif.
        for q in ["daf pun", "titre num 19", "zzz", "d"] {
            let t = std::time::Instant::now();
            let n = 20;
            for _ in 0..n {
                search::search(&conn, q).unwrap();
            }
            let per = t.elapsed() / n;
            eprintln!("recherche « {q} » sur 20000 morceaux : {per:?}");
            if !cfg!(debug_assertions) {
                assert!(per.as_millis() < 30, "trop lent pour « {q} » : {per:?}");
            }
        }
    }
}
