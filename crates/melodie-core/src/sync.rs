//! Mise à jour de la copie locale depuis le serveur.

use crate::api::Client;
use crate::db;
use crate::env::Env;
use crate::state::{Failure, from_api};
use crate::time::now_secs;
use rusqlite::{Connection, Transaction};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

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

type Insert = fn(&Transaction, &Value) -> anyhow::Result<bool>;

/// What `search3` is paged through for, in order: kind, label, how a row is kept.
const KINDS: [(&str, &str, Insert); 3] = [
    ("artist", "Artistes", db::insert_artist),
    ("album", "Albums", db::insert_album),
    ("song", "Morceaux", db::insert_track),
];

/// Keeps a page of `items`; how many were new.
fn insert_all(tx: &Transaction, items: &[Value], insert: Insert) -> Result<usize, Failure> {
    let mut fresh = 0;
    for v in items {
        if insert(tx, v)? {
            fresh += 1;
        }
    }
    Ok(fresh)
}

/// Parcourt `search3` avec une requête vide, par pages de 500, à partir de `offset`, jusqu'à
/// épuisement. `kind` : `artist`, `album` ou `song`. `keep` garde une page, avec le décalage de la
/// suivante, et renvoie combien d'éléments étaient nouveaux.
fn page_through(
    client: &Client,
    kind: &str,
    label: &str,
    mut offset: usize,
    mut keep: impl FnMut(&[Value], usize) -> Result<usize, Failure>,
) -> Result<usize, Failure> {
    let tty = std::io::stderr().is_terminal();
    let mut total = 0;
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
        let fresh = keep(&items, offset + items.len())?;
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

/// The `changed` date each playlist had when the copy last read its tracks.
fn held_playlists(tx: &Transaction) -> Result<HashMap<String, Option<String>>, Failure> {
    let mut st = tx.prepare("SELECT playlist, changed FROM playlist_synced")?;
    let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// `all`: reads the tracks of every playlist; otherwise only of those whose `changed` moved or
/// whose tracks the copy does not hold.
fn refresh_playlists_and_starred(
    client: &Client,
    tx: &Transaction,
    all: bool,
) -> Result<(), Failure> {
    let held = if all {
        HashMap::new()
    } else {
        held_playlists(tx)?
    };
    let pl = call(client, "getPlaylists", &[])?;
    let lists = list(&pl, &["playlists", "playlist"]);
    db::replace_playlists(tx, &lists)?;
    for table in ["playlist_tracks", "playlist_synced"] {
        tx.execute(
            &format!("DELETE FROM {table} WHERE playlist NOT IN (SELECT id FROM playlists)"),
            [],
        )?;
    }
    for p in &lists {
        let Some(pid) = p["id"].as_str() else {
            continue;
        };
        let changed = p["changed"].as_str().map(str::to_string);
        if held.get(pid) == Some(&changed) {
            continue;
        }
        let r = match call(client, "getPlaylist", &[("id", pid.to_string())]) {
            Ok(r) => r,
            // Deleted since `getPlaylists` answered: it is gone, the next sync forgets it.
            Err(Failure::NotFound(_)) => continue,
            Err(e) => return Err(e),
        };
        let ids = db::entry_ids(&list(&r, &["playlist", "entry"]));
        db::replace_playlist_tracks(tx, pid, &ids)?;
        db::mark_playlist_synced(tx, pid, changed.as_deref())?;
    }
    let st = call(client, "getStarred2", &[])?;
    db::apply_starred(tx, &st["starred2"])?;
    Ok(())
}

fn tmp_path(target: &Path) -> PathBuf {
    let mut tmp = target.as_os_str().to_owned();
    tmp.push(".tmp");
    PathBuf::from(tmp)
}

/// Copie complète dans un fichier temporaire, puis remplacement atomique.
fn full(env: &Env, client: &Client) -> Result<Report, Failure> {
    let target = db::db_path(env);
    let tmp = tmp_path(&target);
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
        for (kind, label, insert) in KINDS {
            page_through(client, kind, label, 0, |items, _| {
                insert_all(&tx, items, insert)
            })?;
        }
        refresh_playlists_and_starred(client, &tx, true)?;
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

/// The `lastScan` the server gave when the first copy began, stored at its end: a scan made while
/// it ran, interruptions included, brings a full copy next.
const COPY_SCAN: &str = "copy_scan";

/// Starts a first copy: an empty copy that says it is not finished, put in place at once (made
/// aside then renamed, so that no reader ever sees half a schema).
fn start_first(client: &Client, target: &Path) -> Result<Connection, Failure> {
    let scan = last_scan(client)?;
    let tmp = tmp_path(target);
    {
        let conn = db::create(&tmp)?;
        db::set_meta(&conn, db::COPYING, &format!("{}:0", KINDS[0].0))?;
        db::set_meta(&conn, COPY_SCAN, &scan)?;
    }
    std::fs::rename(&tmp, target)
        .map_err(|e| anyhow::anyhow!("mise en place de {} : {e}", target.display()))?;
    let conn = Connection::open(target)?;
    conn.busy_timeout(std::time::Duration::from_secs(2))?;
    Ok(conn)
}

/// The first copy, made in place rather than aside: there is no previous copy to protect, and
/// each page is committed as it comes, so that the window shows the library while it fills. Where
/// it stands is committed with each page (`db::COPYING`): a copy cut off is taken up again there by
/// the next run, whatever asked for it.
fn first(env: &Env, client: &Client, partial: Option<Connection>) -> Result<Report, Failure> {
    let mut conn = match partial {
        Some(c) => c,
        None => start_first(client, &db::db_path(env))?,
    };
    let at = db::get_meta(&conn, db::COPYING).unwrap_or_default();
    let (from, offset) = at.split_once(':').unwrap_or_default();
    let offset: usize = offset.parse().unwrap_or(0);
    // `end`, or anything not understood: only what follows the pages is left.
    let start = KINDS
        .iter()
        .position(|k| k.0 == from)
        .unwrap_or(KINDS.len());
    for (i, (kind, label, insert)) in KINDS.into_iter().enumerate().skip(start) {
        let offset = if i == start { offset } else { 0 };
        page_through(client, kind, label, offset, |items, next| {
            let tx = conn.transaction()?;
            let fresh = insert_all(&tx, items, insert)?;
            db::set_meta(&tx, db::COPYING, &format!("{kind}:{next}"))?;
            tx.commit()?;
            Ok(fresh)
        })?;
        let then = KINDS.get(i + 1).map_or("end", |k| k.0);
        db::set_meta(&conn, db::COPYING, &format!("{then}:0"))?;
    }
    let tx = conn.transaction()?;
    refresh_playlists_and_starred(client, &tx, true)?;
    let scan = db::get_meta(&tx, COPY_SCAN).unwrap_or_default();
    db::set_meta(&tx, "lastScan", &scan)?;
    db::set_meta(&tx, "synced_at", &now_secs().to_string())?;
    db::set_meta(&tx, "full_synced_at", &now_secs().to_string())?;
    tx.execute(
        "DELETE FROM meta WHERE key IN (?1, ?2)",
        [db::COPYING, COPY_SCAN],
    )?;
    tx.commit()?;
    Ok(Report {
        kind: Kind::Full,
        counts: db::counts(&conn)?,
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
    refresh_playlists_and_starred(client, &tx, false)?;
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

/// Met la copie à jour. `force_full` : copie complète sans comparer `lastScan`. Without a copy, or
/// with a first copy left unfinished, the first copy (see `first`).
pub fn run(env: &Env, client: &Client, force_full: bool) -> Result<Report, Failure> {
    let mut conn = match db::open_existing(env) {
        Ok(None) => return first(env, client, None),
        Ok(Some(c)) if db::is_partial(&c) => return first(env, client, Some(c)),
        // Asked for, or over a copy that cannot be read: made aside, the copy kept if it fails.
        _ if force_full => return full(env, client),
        Ok(Some(c)) => c,
        Err(e) => return Err(e.into()),
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
    use crate::api::{ApiError, Creds, token};
    use crate::fake::{self, Fake};
    use crate::search;
    use std::sync::atomic::Ordering;

    fn setup(n: usize) -> (tempfile::TempDir, Env, Fake, Client) {
        let d = tempfile::tempdir().unwrap();
        let env = Env::with_home(d.path());
        let creds = Creds {
            u: "alice".into(),
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

    fn playlist_tracks(env: &Env) -> Vec<String> {
        let conn = db::open_existing(env).unwrap().unwrap();
        conn.prepare("SELECT track FROM playlist_tracks WHERE playlist = 'p1' ORDER BY pos")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn a_full_copy_keeps_the_tracks_of_each_playlist() {
        let (_d, env, fake, client) = setup(3);
        fake.state
            .playlists
            .lock()
            .unwrap()
            .insert("p1".into(), vec![fake::song(2), fake::song(0)]);
        run(&env, &client, true).unwrap();
        assert_eq!(playlist_tracks(&env), ["s2", "s0"]);
    }

    #[test]
    fn a_light_sync_reads_again_only_the_playlists_that_changed() {
        let (_d, env, fake, client) = setup(3);
        fake.state
            .playlists
            .lock()
            .unwrap()
            .insert("p1".into(), vec![fake::song(1)]);
        run(&env, &client, false).unwrap();
        fake.clear_calls();
        fake.state
            .playlists
            .lock()
            .unwrap()
            .insert("p1".into(), vec![fake::song(1), fake::song(2)]);
        assert_eq!(run(&env, &client, false).unwrap().kind, Kind::Light);
        assert_eq!(fake.calls_of("getPlaylist"), 0, "unchanged: not read again");
        assert_eq!(playlist_tracks(&env), ["s1"]);

        *fake.state.playlist_changed.lock().unwrap() = "2026-10-03T10:00:00Z".into();
        run(&env, &client, false).unwrap();
        assert_eq!(fake.calls_of("getPlaylist"), 1);
        assert_eq!(playlist_tracks(&env), ["s1", "s2"]);
    }

    #[test]
    fn an_empty_playlist_is_not_read_again_by_a_light_sync() {
        let (_d, env, fake, client) = setup(3);
        fake.state
            .playlists
            .lock()
            .unwrap()
            .insert("p1".into(), vec![]);
        run(&env, &client, true).unwrap();
        assert_eq!(fake.calls_of("getPlaylist"), 1);
        fake.clear_calls();
        assert_eq!(run(&env, &client, false).unwrap().kind, Kind::Light);
        assert_eq!(fake.calls_of("getPlaylist"), 0, "empty and unchanged");

        *fake.state.playlist_changed.lock().unwrap() = "2026-10-03T10:00:00Z".into();
        fake.state
            .playlists
            .lock()
            .unwrap()
            .insert("p1".into(), vec![fake::song(1)]);
        run(&env, &client, false).unwrap();
        assert_eq!(fake.calls_of("getPlaylist"), 1);
        assert_eq!(playlist_tracks(&env), ["s1"]);
    }

    #[test]
    fn a_playlist_deleted_during_the_sync_is_skipped() {
        let (_d, env, fake, client) = setup(3);
        fake.state.playlist_gone.store(true, Ordering::SeqCst);
        // Listed by `getPlaylists`, gone by the time `getPlaylist` asks: the copy is made without it.
        for force in [true, false] {
            run(&env, &client, force).unwrap();
            assert!(playlist_tracks(&env).is_empty());
            let conn = db::open_existing(&env).unwrap().unwrap();
            assert_eq!(db::counts(&conn).unwrap().tracks, 3);
        }
        // Any other failure still fails the copy.
        *fake.state.fail_code.lock().unwrap() = Some(10);
        assert!(run(&env, &client, true).is_err());
    }

    #[test]
    fn a_light_sync_fills_the_playlists_of_an_older_copy() {
        let (_d, env, fake, client) = setup(3);
        run(&env, &client, false).unwrap();
        let conn = db::open_existing(&env).unwrap().unwrap();
        conn.execute_batch("DROP TABLE playlist_tracks; DROP TABLE playlist_synced")
            .unwrap();
        drop(conn);
        assert_eq!(run(&env, &client, false).unwrap().kind, Kind::Light);
        assert_eq!(playlist_tracks(&env), ["s0", "s1", "s2"]);
        assert_eq!(fake.calls_of("getPlaylist"), 2, "once per sync");
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
        *fake.state.raw.lock().unwrap() = Some((503, "boom".into()));
        assert!(matches!(
            run(&env, &client, true),
            Err(Failure::Unreachable { .. })
        ));
        *fake.state.raw.lock().unwrap() = Some((200, "pas du json".into()));
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
    fn an_http_error_is_not_reported_as_unreachable() {
        let (_d, _env, fake, client) = setup(3);
        for code in [400, 404, 413, 414, 500] {
            *fake.state.raw.lock().unwrap() = Some((code, "{}".into()));
            match client.ping() {
                Err(ApiError::Http(m)) => assert!(m.contains(&code.to_string()), "{m}"),
                other => panic!("HTTP {code}: {other:?}"),
            }
        }
        // What a proxy answers when the server behind it is down.
        for code in [502, 503, 504] {
            *fake.state.raw.lock().unwrap() = Some((code, "{}".into()));
            assert!(matches!(client.ping(), Err(ApiError::Unreachable(_))));
        }
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
    fn a_first_copy_interrupted_halfway_stays_readable_and_the_next_one_completes_it() {
        let (_d, env, fake, client) = setup(1203);
        *fake.state.fail_from_song.lock().unwrap() = Some(500);
        assert!(run(&env, &client, false).is_err());
        // What came before the cut is there, and says it is not finished.
        let conn = db::open_existing(&env).unwrap().unwrap();
        assert!(db::is_partial(&conn));
        let c = db::counts(&conn).unwrap();
        assert_eq!((c.artists, c.albums, c.tracks), (2, 1, 500));
        assert_eq!(db::get_meta(&conn, "synced_at"), None);
        let hits = search::search(&conn, "daf pun").unwrap();
        assert!(hits.iter().any(|h| h.name == "Daft Punk"));
        drop(conn);

        // `login` asks for a full copy: it completes this one, from where it stopped.
        *fake.state.fail_from_song.lock().unwrap() = None;
        fake.clear_calls();
        let r = run(&env, &client, true).unwrap();
        assert_eq!(r.kind, Kind::Full);
        assert_eq!(r.counts.tracks, 1203);
        // Two more pages of songs and the empty one; the artists and albums are not read again.
        assert_eq!(fake.calls_of("search3"), 3);
        let conn = db::open_existing(&env).unwrap().unwrap();
        assert!(!db::is_partial(&conn));
        assert_eq!(
            db::get_meta(&conn, "lastScan").as_deref(),
            Some("2026-10-01T10:00:00Z")
        );
        assert!(db::get_meta(&conn, "synced_at").is_some());
        assert_eq!(playlist_tracks(&env).len(), 1203);
        drop(conn);
        // A complete copy: the next sync is a light one.
        assert_eq!(run(&env, &client, false).unwrap().kind, Kind::Light);
    }

    #[test]
    fn a_first_copy_cut_between_two_kinds_goes_on_with_the_next() {
        let (_d, env, fake, client) = setup(10);
        *fake.state.fail_from_song.lock().unwrap() = Some(0);
        assert!(run(&env, &client, false).is_err());
        let conn = db::open_existing(&env).unwrap().unwrap();
        let c = db::counts(&conn).unwrap();
        assert_eq!((c.artists, c.albums, c.tracks), (2, 1, 0));
        drop(conn);
        *fake.state.fail_from_song.lock().unwrap() = None;
        fake.clear_calls();
        assert_eq!(run(&env, &client, false).unwrap().counts.tracks, 10);
        assert_eq!(
            fake.calls_of("search3"),
            2,
            "one page of songs and the empty one"
        );
    }

    #[test]
    fn a_later_full_copy_that_fails_still_keeps_the_previous_one() {
        let (_d, env, fake, client) = setup(1203);
        run(&env, &client, false).unwrap();
        *fake.state.fail_from_song.lock().unwrap() = Some(500);
        assert!(run(&env, &client, true).is_err());
        let conn = db::open_existing(&env).unwrap().unwrap();
        assert!(!db::is_partial(&conn));
        assert_eq!(db::counts(&conn).unwrap().tracks, 1203);
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
