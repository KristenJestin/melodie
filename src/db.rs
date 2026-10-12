//! Copie locale de la bibliothèque : SQLite + index plein texte FTS5.

use anyhow::{Context, Result};
use poste_core::Env;
use rusqlite::{params, Connection, Transaction};
use serde_json::Value;
use std::path::PathBuf;

/// `$XDG_CACHE_HOME/poste/music`.
pub fn cache_dir(env: &Env) -> PathBuf {
    env.cache_dir().join("music")
}

pub fn db_path(env: &Env) -> PathBuf {
    cache_dir(env).join("library.db")
}

/// Index insensibles à la casse et aux accents. `id` n'est pas indexé :
/// il ne sert qu'à retrouver la ligne de la table de base.
pub const SCHEMA: &str = "
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE artists (
  id TEXT PRIMARY KEY, name TEXT NOT NULL, albumCount INTEGER NOT NULL DEFAULT 0,
  starred TEXT, userRating INTEGER NOT NULL DEFAULT 0, coverArt TEXT
);
CREATE TABLE albums (
  id TEXT PRIMARY KEY, name TEXT NOT NULL, artist TEXT NOT NULL DEFAULT '', artistId TEXT,
  year INTEGER NOT NULL DEFAULT 0, genre TEXT, songCount INTEGER NOT NULL DEFAULT 0,
  duration INTEGER NOT NULL DEFAULT 0, playCount INTEGER NOT NULL DEFAULT 0, played TEXT,
  starred TEXT, userRating INTEGER NOT NULL DEFAULT 0, coverArt TEXT
);
CREATE TABLE tracks (
  id TEXT PRIMARY KEY, title TEXT NOT NULL, artist TEXT NOT NULL DEFAULT '', artistId TEXT,
  album TEXT NOT NULL DEFAULT '', albumId TEXT, genre TEXT, genres TEXT,
  year INTEGER NOT NULL DEFAULT 0, track INTEGER NOT NULL DEFAULT 0,
  duration INTEGER NOT NULL DEFAULT 0, suffix TEXT, bitRate INTEGER NOT NULL DEFAULT 0,
  samplingRate INTEGER NOT NULL DEFAULT 0, bitDepth INTEGER NOT NULL DEFAULT 0,
  channelCount INTEGER NOT NULL DEFAULT 0, playCount INTEGER NOT NULL DEFAULT 0, played TEXT,
  starred TEXT, userRating INTEGER NOT NULL DEFAULT 0, coverArt TEXT
);
CREATE TABLE skips (id TEXT NOT NULL, at INTEGER NOT NULL);
CREATE INDEX skips_at ON skips(at);
CREATE INDEX tracks_album ON tracks(albumId);
CREATE INDEX tracks_artist ON tracks(artistId);
CREATE TABLE playlists (
  id TEXT PRIMARY KEY, name TEXT NOT NULL, songCount INTEGER NOT NULL DEFAULT 0,
  duration INTEGER NOT NULL DEFAULT 0, changed TEXT, coverArt TEXT, owner TEXT
);
CREATE VIRTUAL TABLE fts_playlists USING fts5(id UNINDEXED, name, tokenize='unicode61 remove_diacritics 2');
CREATE VIRTUAL TABLE fts_artists USING fts5(id UNINDEXED, name, tokenize='unicode61 remove_diacritics 2');
CREATE VIRTUAL TABLE fts_albums USING fts5(id UNINDEXED, name, artist, tokenize='unicode61 remove_diacritics 2');
CREATE VIRTUAL TABLE fts_tracks USING fts5(id UNINDEXED, title, artist, album, tokenize='unicode61 remove_diacritics 2');
";

/// Crée une base neuve avec son schéma.
pub fn create(path: &std::path::Path) -> Result<Connection> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let _ = std::fs::remove_file(path);
    let conn = Connection::open(path).with_context(|| format!("création de {}", path.display()))?;
    conn.execute_batch("PRAGMA journal_mode = OFF; PRAGMA synchronous = OFF;")?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

/// Note qu'un morceau a été passé (`next` trop tôt). La table est créée si
/// la copie date d'avant son apparition. Ne crée pas la base : sans copie
/// locale, rien n'est noté (`false`).
pub fn record_skip(env: &Env, track_id: &str, at: u64) -> Result<bool> {
    let Some(conn) = open_existing(env)? else {
        return Ok(false);
    };
    conn.busy_timeout(std::time::Duration::from_secs(2))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS skips (id TEXT NOT NULL, at INTEGER NOT NULL);
         CREATE INDEX IF NOT EXISTS skips_at ON skips(at);",
    )?;
    conn.execute(
        "INSERT INTO skips(id, at) VALUES (?1, ?2)",
        params![track_id, at as i64],
    )?;
    Ok(true)
}

/// Reporte les morceaux passés de l'ancienne copie dans la nouvelle (une
/// synchronisation complète recrée le fichier).
pub fn carry_skips(old: &std::path::Path, new: &std::path::Path) -> Result<()> {
    if !old.is_file() {
        return Ok(());
    }
    let conn = Connection::open(new)?;
    conn.execute(
        "ATTACH DATABASE ?1 AS old",
        [old.to_string_lossy().as_ref()],
    )?;
    let has: bool = conn.query_row(
        "SELECT count(*) > 0 FROM old.sqlite_master WHERE type = 'table' AND name = 'skips'",
        [],
        |r| r.get(0),
    )?;
    if has {
        conn.execute("INSERT INTO skips(id, at) SELECT id, at FROM old.skips", [])?;
    }
    conn.execute_batch("DETACH DATABASE old")?;
    Ok(())
}

/// Ouvre la copie existante ; `None` si elle n'existe pas.
pub fn open_existing(env: &Env) -> Result<Option<Connection>> {
    let p = db_path(env);
    if !p.is_file() {
        return Ok(None);
    }
    let conn = Connection::open(&p).with_context(|| format!("ouverture de {}", p.display()))?;
    Ok(Some(conn))
}

pub fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
        params![key, value],
    )?;
    Ok(())
}

pub fn get_meta(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .ok()
}

fn text(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_string)
}

fn num(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(Value::as_i64).unwrap_or(0)
}

/// Les identifiants Subsonic sont des chaînes, mais certains serveurs
/// envoient des nombres.
fn id(v: &Value) -> Option<String> {
    match v.get("id")? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn cover(v: &Value) -> Option<String> {
    match v.get("coverArt") {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

/// Liste des genres d'un morceau (OpenSubsonic `genres`, sinon `genre`), en JSON.
fn genres(v: &Value) -> Option<String> {
    let list: Vec<String> = match v.get("genres").and_then(Value::as_array) {
        Some(a) => a
            .iter()
            .filter_map(|g| g.get("name").and_then(Value::as_str).map(str::to_string))
            .collect(),
        None => text(v, "genre").into_iter().collect(),
    };
    (!list.is_empty()).then(|| serde_json::to_string(&list).unwrap_or_default())
}

/// Insère un artiste ; renvoie `true` s'il est nouveau.
pub fn insert_artist(tx: &Transaction, v: &Value) -> Result<bool> {
    let (Some(id), Some(name)) = (id(v), text(v, "name")) else {
        return Ok(false);
    };
    let n = tx.execute(
        "INSERT OR IGNORE INTO artists(id, name, albumCount, starred, userRating, coverArt)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            id,
            name,
            num(v, "albumCount"),
            text(v, "starred"),
            num(v, "userRating"),
            cover(v)
        ],
    )?;
    if n == 1 {
        tx.execute(
            "INSERT INTO fts_artists(id, name) VALUES (?1, ?2)",
            params![id, name],
        )?;
    }
    Ok(n == 1)
}

pub fn insert_album(tx: &Transaction, v: &Value) -> Result<bool> {
    let Some(id) = id(v) else { return Ok(false) };
    let name = text(v, "name")
        .or_else(|| text(v, "title"))
        .unwrap_or_default();
    let artist = text(v, "artist").unwrap_or_default();
    let n = tx.execute(
        "INSERT OR IGNORE INTO albums(id, name, artist, artistId, year, genre, songCount, duration,
           playCount, played, starred, userRating, coverArt)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            id,
            name,
            artist,
            text(v, "artistId"),
            num(v, "year"),
            text(v, "genre"),
            num(v, "songCount"),
            num(v, "duration"),
            num(v, "playCount"),
            text(v, "played"),
            text(v, "starred"),
            num(v, "userRating"),
            cover(v)
        ],
    )?;
    if n == 1 {
        tx.execute(
            "INSERT INTO fts_albums(id, name, artist) VALUES (?1, ?2, ?3)",
            params![id, name, artist],
        )?;
    }
    Ok(n == 1)
}

pub fn insert_track(tx: &Transaction, v: &Value) -> Result<bool> {
    let Some(id) = id(v) else { return Ok(false) };
    let title = text(v, "title").unwrap_or_default();
    let artist = text(v, "artist").unwrap_or_default();
    let album = text(v, "album").unwrap_or_default();
    let n = tx.execute(
        "INSERT OR IGNORE INTO tracks(id, title, artist, artistId, album, albumId, genre, genres,
           year, track, duration, suffix, bitRate, samplingRate, bitDepth, channelCount,
           playCount, played, starred, userRating, coverArt)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
           ?19, ?20, ?21)",
        params![
            id,
            title,
            artist,
            text(v, "artistId"),
            album,
            text(v, "albumId"),
            text(v, "genre"),
            genres(v),
            num(v, "year"),
            num(v, "track"),
            num(v, "duration"),
            text(v, "suffix"),
            num(v, "bitRate"),
            num(v, "samplingRate"),
            num(v, "bitDepth"),
            num(v, "channelCount"),
            num(v, "playCount"),
            text(v, "played"),
            text(v, "starred"),
            num(v, "userRating"),
            cover(v)
        ],
    )?;
    if n == 1 {
        tx.execute(
            "INSERT INTO fts_tracks(id, title, artist, album) VALUES (?1, ?2, ?3, ?4)",
            params![id, title, artist, album],
        )?;
    }
    Ok(n == 1)
}

/// Remplace toutes les playlists.
pub fn replace_playlists(tx: &Transaction, list: &[Value]) -> Result<usize> {
    tx.execute("DELETE FROM playlists", [])?;
    tx.execute("DELETE FROM fts_playlists", [])?;
    let mut n = 0;
    for v in list {
        let (Some(id), Some(name)) = (id(v), text(v, "name")) else {
            continue;
        };
        let k = tx.execute(
            "INSERT OR IGNORE INTO playlists(id, name, songCount, duration, changed, coverArt, owner)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, name, num(v, "songCount"), num(v, "duration"), text(v, "changed"),
                    cover(v), text(v, "owner")],
        )?;
        if k == 1 {
            tx.execute(
                "INSERT INTO fts_playlists(id, name) VALUES (?1, ?2)",
                params![id, name],
            )?;
            n += 1;
        }
    }
    Ok(n)
}

/// Remet à jour les favoris : tout est effacé puis reposé d'après `getStarred2`.
pub fn apply_starred(tx: &Transaction, starred: &Value) -> Result<()> {
    for table in ["artists", "albums", "tracks"] {
        tx.execute(
            &format!("UPDATE {table} SET starred = NULL WHERE starred IS NOT NULL"),
            [],
        )?;
    }
    for (key, table) in [
        ("artist", "artists"),
        ("album", "albums"),
        ("song", "tracks"),
    ] {
        let Some(list) = starred.get(key).and_then(Value::as_array) else {
            continue;
        };
        let mut st = tx.prepare(&format!("UPDATE {table} SET starred = ?2 WHERE id = ?1"))?;
        for v in list {
            if let Some(i) = id(v) {
                st.execute(params![i, text(v, "starred").unwrap_or_else(|| "1".into())])?;
            }
        }
    }
    Ok(())
}

/// Met à jour les compteurs d'écoute d'un album d'après `getAlbumList2`.
/// Renvoie `true` si l'album est connu et que ses compteurs ont changé.
pub fn update_album_counters(tx: &Transaction, v: &Value) -> Result<bool> {
    let Some(i) = id(v) else { return Ok(false) };
    let old: Option<(i64, Option<String>)> = tx
        .query_row(
            "SELECT playCount, played FROM albums WHERE id = ?1",
            [&i],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    let Some((count, played)) = old else {
        return Ok(false);
    };
    let (ncount, nplayed) = (num(v, "playCount"), text(v, "played"));
    if count == ncount && played == nplayed {
        return Ok(false);
    }
    tx.execute(
        "UPDATE albums SET playCount = ?2, played = ?3, userRating = ?4, starred = ?5 WHERE id = ?1",
        params![i, ncount, nplayed, num(v, "userRating"), text(v, "starred")],
    )?;
    Ok(true)
}

/// Met à jour les compteurs des morceaux d'un album d'après `getAlbum`.
pub fn update_track_counters(tx: &Transaction, songs: &[Value]) -> Result<()> {
    let mut st = tx.prepare(
        "UPDATE tracks SET playCount = ?2, played = ?3, userRating = ?4, starred = ?5 WHERE id = ?1",
    )?;
    for v in songs {
        if let Some(i) = id(v) {
            st.execute(params![
                i,
                num(v, "playCount"),
                text(v, "played"),
                num(v, "userRating"),
                text(v, "starred")
            ])?;
        }
    }
    Ok(())
}

/// Table (et colonne de nom) d'un type de favori.
fn star_table(kind: StarKind) -> (&'static str, &'static str) {
    match kind {
        StarKind::Track => ("tracks", "title"),
        StarKind::Album => ("albums", "name"),
        StarKind::Artist => ("artists", "name"),
    }
}

/// Ce qu'on peut aimer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StarKind {
    Track,
    Album,
    Artist,
}

impl StarKind {
    /// Paramètre de `star.view` / `unstar.view`.
    pub fn api_param(self) -> &'static str {
        match self {
            StarKind::Track => "id",
            StarKind::Album => "albumId",
            StarKind::Artist => "artistId",
        }
    }
}

/// Pose (`Some(date)`) ou retire (`None`) le favori dans la copie. `false` si l'élément est inconnu.
pub fn set_starred(conn: &Connection, kind: StarKind, id: &str, at: Option<&str>) -> Result<bool> {
    let (table, _) = star_table(kind);
    let n = conn.execute(
        &format!("UPDATE {table} SET starred = ?2 WHERE id = ?1"),
        params![id, at],
    )?;
    Ok(n > 0)
}

/// État de favori local d'un élément ; `None` s'il est inconnu.
pub fn is_starred(conn: &Connection, kind: StarKind, id: &str) -> Option<bool> {
    let (table, _) = star_table(kind);
    conn.query_row(
        &format!("SELECT starred IS NOT NULL FROM {table} WHERE id = ?1"),
        [id],
        |r| r.get(0),
    )
    .ok()
}

/// Nom (titre) local d'un élément.
pub fn name_of(conn: &Connection, kind: StarKind, id: &str) -> Option<String> {
    let (table, col) = star_table(kind);
    conn.query_row(
        &format!("SELECT {col} FROM {table} WHERE id = ?1"),
        [id],
        |r| r.get(0),
    )
    .ok()
}

/// Favori d'un morceau d'après la copie, sans la créer (`None` : pas de copie ou morceau inconnu).
pub fn track_starred(env: &Env, id: &str) -> Result<Option<bool>> {
    let Some(conn) = open_existing(env)? else {
        return Ok(None);
    };
    Ok(is_starred(&conn, StarKind::Track, id))
}

/// Durée locale d'un morceau, en secondes.
pub fn track_duration(conn: &Connection, id: &str) -> Option<i64> {
    conn.query_row("SELECT duration FROM tracks WHERE id = ?1", [id], |r| {
        r.get(0)
    })
    .ok()
}

/// Ajoute `d_count` morceaux et `d_secs` secondes à une playlist de la copie.
pub fn playlist_adjust(
    conn: &Connection,
    id: &str,
    d_count: i64,
    d_secs: i64,
    at: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE playlists SET songCount = MAX(0, songCount + ?2), duration = MAX(0, duration + ?3),
           changed = ?4 WHERE id = ?1",
        params![id, d_count, d_secs, at],
    )?;
    Ok(())
}

/// Insère une playlist créée sur le serveur.
pub fn playlist_insert(conn: &Connection, v: &Value) -> Result<bool> {
    let (Some(pid), Some(name)) = (id(v), text(v, "name")) else {
        return Ok(false);
    };
    conn.execute("DELETE FROM playlists WHERE id = ?1", [&pid])?;
    conn.execute("DELETE FROM fts_playlists WHERE id = ?1", [&pid])?;
    conn.execute(
        "INSERT INTO playlists(id, name, songCount, duration, changed, coverArt, owner)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            pid,
            name,
            num(v, "songCount"),
            num(v, "duration"),
            text(v, "changed"),
            cover(v),
            text(v, "owner")
        ],
    )?;
    conn.execute(
        "INSERT INTO fts_playlists(id, name) VALUES (?1, ?2)",
        params![pid, name],
    )?;
    Ok(true)
}

pub fn playlist_rename(conn: &Connection, id: &str, name: &str, at: &str) -> Result<()> {
    conn.execute(
        "UPDATE playlists SET name = ?2, changed = ?3 WHERE id = ?1",
        params![id, name, at],
    )?;
    conn.execute("DELETE FROM fts_playlists WHERE id = ?1", [id])?;
    conn.execute(
        "INSERT INTO fts_playlists(id, name) VALUES (?1, ?2)",
        params![id, name],
    )?;
    Ok(())
}

pub fn playlist_delete(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM playlists WHERE id = ?1", [id])?;
    conn.execute("DELETE FROM fts_playlists WHERE id = ?1", [id])?;
    Ok(())
}

/// Effectifs de la copie.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub tracks: i64,
    pub albums: i64,
    pub artists: i64,
    pub playlists: i64,
}

pub fn counts(conn: &Connection) -> Result<Counts> {
    let n = |t: &str| -> Result<i64> {
        Ok(conn.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))?)
    };
    Ok(Counts {
        tracks: n("tracks")?,
        albums: n("albums")?,
        artists: n("artists")?,
        playlists: n("playlists")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn morceaux_passes_notes_et_reportes() {
        let d = tempfile::tempdir().unwrap();
        let env = Env::with_home(d.path());
        // Sans copie locale, rien n'est noté et rien n'est créé.
        assert!(!record_skip(&env, "t1", 10).unwrap());
        assert!(!db_path(&env).exists());

        create(&db_path(&env)).unwrap();
        assert!(record_skip(&env, "t1", 10).unwrap());
        assert!(record_skip(&env, "t2", 20).unwrap());

        // Une copie plus ancienne, sans la table, est complétée.
        let old = d.path().join("ancienne.db");
        Connection::open(&old)
            .unwrap()
            .execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);")
            .unwrap();
        let env2 = Env::with_home(d.path().join("autre"));
        std::fs::create_dir_all(cache_dir(&env2)).unwrap();
        std::fs::copy(&old, db_path(&env2)).unwrap();
        assert!(record_skip(&env2, "t9", 5).unwrap());

        // Une synchronisation complète recrée le fichier : les passés suivent.
        let fresh = d.path().join("neuve.db");
        create(&fresh).unwrap();
        carry_skips(&db_path(&env), &fresh).unwrap();
        let conn = Connection::open(&fresh).unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM skips", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2);
        // Ancienne copie sans table : rien à reporter, sans erreur.
        carry_skips(&old, &fresh).unwrap();
        carry_skips(&d.path().join("absente.db"), &fresh).unwrap();
    }
}
