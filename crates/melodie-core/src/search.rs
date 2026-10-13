//! Recherche dans la copie locale (FTS5) et mise en forme des résultats.

use anyhow::Result;
use rusqlite::{Connection, params};
use serde_json::{Value, json};

pub const LIMIT_PLAYLISTS: usize = 5;
pub const LIMIT_ARTISTS: usize = 5;
pub const LIMIT_ALBUMS: usize = 8;
pub const LIMIT_TRACKS: usize = 12;

/// Icônes Nerd Font des actions secondaires.
pub const ICON_PLAY: &str = "\u{f040a}";
const ICON_SHUFFLE: &str = "\u{f049f}";
pub const ICON_QUEUE: &str = "\u{f0415}";
pub const ICON_MIX: &str = "\u{f049d}";
const ICON_LIKE: &str = "\u{f02d1}";
const ICON_UNLIKE: &str = "\u{f02d5}";
const ICON_TO_PLAYLIST: &str = "\u{f0cb8}";

/// Candidats lus dans l'index pour chaque type, avant le tri par pertinence.
const CANDIDATES: usize = 200;

/// Dans l'ordre d'affichage à pertinence égale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Playlist,
    Artist,
    Album,
    Track,
}

impl Kind {
    /// Résultats gardés pour ce type.
    fn limit(self) -> usize {
        match self {
            Kind::Playlist => LIMIT_PLAYLISTS,
            Kind::Artist => LIMIT_ARTISTS,
            Kind::Album => LIMIT_ALBUMS,
            Kind::Track => LIMIT_TRACKS,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Kind::Playlist => "playlist",
            Kind::Artist => "artist",
            Kind::Album => "album",
            Kind::Track => "track",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Kind::Playlist => "\u{f0cb8}",
            Kind::Artist => "\u{f0803}",
            Kind::Album => "\u{f0025}",
            Kind::Track => "\u{f0388}",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub kind: Kind,
    pub id: String,
    pub name: String,
    pub artist: String,
    pub album: String,
    pub year: i64,
    /// Albums d'un artiste, ou morceaux d'une playlist.
    pub count: i64,
    pub duration: i64,
    pub cover: Option<String>,
    /// Favori d'après la copie locale.
    pub starred: bool,
    /// The album of a track.
    pub album_id: Option<String>,
    /// The artist of an album or a track.
    pub artist_id: Option<String>,
}

/// Requête FTS5 à préfixes : « daf pun » donne `"daf"* "pun"*`.
/// `None` si le texte ne contient aucun mot.
pub fn fts_query(input: &str) -> Option<String> {
    let words: Vec<String> = input
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{w}\"*"))
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

fn hits(
    conn: &Connection,
    sql: &str,
    q: &str,
    limit: Option<usize>,
    map: impl Fn(&rusqlite::Row) -> rusqlite::Result<Hit>,
) -> Result<Vec<Hit>> {
    let mut st = conn.prepare_cached(sql)?;
    // a negative limit is no limit in SQLite
    let limit = limit.map_or(-1, |n| n as i64);
    let rows = st.query_map(params![q, limit], |r| map(r))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The results of one kind for an FTS query, in the index's order; `limit` of them, or all.
fn candidates(conn: &Connection, q: &str, kind: Kind, limit: Option<usize>) -> Result<Vec<Hit>> {
    let blank = |kind, id: String, name: String| Hit {
        kind,
        id,
        name,
        artist: String::new(),
        album: String::new(),
        year: 0,
        count: 0,
        duration: 0,
        cover: None,
        starred: false,
        album_id: None,
        artist_id: None,
    };
    match kind {
        Kind::Playlist => hits(
            conn,
            "SELECT p.id, p.name, p.songCount, p.duration, p.coverArt
             FROM fts_playlists f JOIN playlists p ON p.id = f.id
             WHERE fts_playlists MATCH ?1 ORDER BY f.rank LIMIT ?2",
            q,
            limit,
            |r| {
                Ok(Hit {
                    count: r.get(2)?,
                    duration: r.get(3)?,
                    cover: r.get(4)?,
                    ..blank(Kind::Playlist, r.get(0)?, r.get(1)?)
                })
            },
        ),
        Kind::Artist => hits(
            conn,
            "SELECT a.id, a.name, a.albumCount, a.coverArt, a.starred IS NOT NULL
             FROM fts_artists f JOIN artists a ON a.id = f.id
             WHERE fts_artists MATCH ?1 ORDER BY f.rank LIMIT ?2",
            q,
            limit,
            |r| {
                Ok(Hit {
                    count: r.get(2)?,
                    cover: r.get(3)?,
                    starred: r.get(4)?,
                    ..blank(Kind::Artist, r.get(0)?, r.get(1)?)
                })
            },
        ),
        Kind::Album => hits(
            conn,
            "SELECT a.id, a.name, a.artist, a.year, a.coverArt, a.starred IS NOT NULL, a.artistId
             FROM fts_albums f JOIN albums a ON a.id = f.id
             WHERE fts_albums MATCH ?1 ORDER BY f.rank LIMIT ?2",
            q,
            limit,
            |r| {
                Ok(Hit {
                    artist: r.get(2)?,
                    year: r.get(3)?,
                    cover: r.get(4)?,
                    starred: r.get(5)?,
                    artist_id: r.get(6)?,
                    ..blank(Kind::Album, r.get(0)?, r.get(1)?)
                })
            },
        ),
        Kind::Track => hits(
            conn,
            "SELECT t.id, t.title, t.artist, t.album, t.coverArt, t.starred IS NOT NULL,
                    t.duration, t.albumId, t.artistId
             FROM fts_tracks f JOIN tracks t ON t.id = f.id
             WHERE fts_tracks MATCH ?1 ORDER BY f.rank, t.playCount DESC LIMIT ?2",
            q,
            limit,
            |r| {
                Ok(Hit {
                    artist: r.get(2)?,
                    album: r.get(3)?,
                    cover: r.get(4)?,
                    starred: r.get(5)?,
                    duration: r.get(6)?,
                    album_id: r.get(7)?,
                    artist_id: r.get(8)?,
                    ..blank(Kind::Track, r.get(0)?, r.get(1)?)
                })
            },
        ),
    }
}

const KINDS: [Kind; 4] = [Kind::Playlist, Kind::Artist, Kind::Album, Kind::Track];

/// Résultats du plus pertinent au moins pertinent (voir `relevance`) ; à
/// pertinence égale : playlists, artistes, albums, morceaux.
pub fn search(conn: &Connection, input: &str) -> Result<Vec<Hit>> {
    let Some(q) = fts_query(input) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for kind in KINDS {
        out.extend(candidates(conn, &q, kind, Some(CANDIDATES))?);
    }
    let query = words(input);
    let mut ranked: Vec<(u8, Hit)> = out
        .into_iter()
        .map(|h| (relevance(&query, &h), h))
        .collect();
    // Les meilleurs de chaque type, puis tous ensemble par pertinence ; à
    // pertinence égale, l'ordre des types et celui de l'index.
    ranked.sort_by_key(|(r, h)| (h.kind, *r));
    let mut seen = [0usize; 4];
    ranked.retain(|(_, h)| {
        let n = &mut seen[h.kind as usize];
        *n += 1;
        *n <= h.kind.limit()
    });
    ranked.sort_by_key(|(r, _)| *r);
    Ok(ranked.into_iter().map(|(_, h)| h).collect())
}

/// The results of one kind, as the window shows them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub kind: Kind,
    /// The best ones first.
    pub hits: Vec<Hit>,
    /// There are more than these (see `of_kind`).
    pub more: bool,
}

/// Ranks hits of one kind, the most relevant first; at equal relevance, the index's order.
fn ranked(input: &str, hits: Vec<Hit>) -> Vec<(u8, Hit)> {
    let query = words(input);
    let mut out: Vec<(u8, Hit)> = hits
        .into_iter()
        .map(|h| (relevance(&query, &h), h))
        .collect();
    out.sort_by_key(|(r, _)| *r);
    out
}

/// The order of the kinds at equal relevance in the window.
fn tie_rank(kind: Kind) -> u8 {
    match kind {
        Kind::Artist => 0,
        Kind::Album => 1,
        Kind::Track => 2,
        Kind::Playlist => 3,
    }
}

/// The results grouped by kind, `shown` of each, ranked as by `search`; the kind whose best
/// result is the most relevant first, then artists, albums, tracks, playlists at equal
/// relevance. Leaves `search`, which the launcher reads (playlists first), as it is.
pub fn grouped(conn: &Connection, input: &str, shown: usize) -> Result<Vec<Group>> {
    let Some(q) = fts_query(input) else {
        return Ok(Vec::new());
    };
    let mut groups: Vec<(u8, Group)> = Vec::new();
    for kind in KINDS {
        let mut hits = ranked(input, candidates(conn, &q, kind, Some(CANDIDATES))?);
        let Some(best) = hits.first().map(|(r, _)| *r) else {
            continue;
        };
        let more = hits.len() > shown;
        hits.truncate(shown);
        groups.push((
            best,
            Group {
                kind,
                hits: hits.into_iter().map(|(_, h)| h).collect(),
                more,
            },
        ));
    }
    groups.sort_by_key(|(best, g)| (*best, tie_rank(g.kind)));
    Ok(groups.into_iter().map(|(_, g)| g).collect())
}

/// Every result of one kind, ranked as by `search`.
pub fn of_kind(conn: &Connection, input: &str, kind: Kind) -> Result<Vec<Hit>> {
    let Some(q) = fts_query(input) else {
        return Ok(Vec::new());
    };
    let hits = ranked(input, candidates(conn, &q, kind, None)?);
    Ok(hits.into_iter().map(|(_, h)| h).collect())
}

/// Minuscules sans accents, comme l'index (`remove_diacritics`).
fn fold(c: char) -> char {
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
        'ç' => 'c',
        'è' | 'é' | 'ê' | 'ë' => 'e',
        'ì' | 'í' | 'î' | 'ï' => 'i',
        'ñ' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' => 'o',
        'ù' | 'ú' | 'û' | 'ü' => 'u',
        'ý' | 'ÿ' => 'y',
        c => c,
    }
}

/// Les mots d'un texte, découpés comme la requête FTS, sans casse ni accents.
fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.chars().flat_map(char::to_lowercase).map(fold).collect())
        .collect()
}

/// Pertinence d'un résultat pour la requête, de 0 (la meilleure) à 6 : nom
/// identique, nom qui commence par la requête, mots entiers dans le nom, nom qui
/// commence par la requête dont le dernier mot est en cours de frappe, débuts de
/// mots dans le nom, mots entiers dans l'artiste ou l'album, le reste (débuts
/// de mots dans l'artiste ou l'album).
fn relevance(query: &[String], hit: &Hit) -> u8 {
    let name = words(&hit.name);
    let whole = |ws: &[String]| query.iter().all(|q| ws.contains(q));
    let starts = match query.split_last() {
        Some((last, head)) => {
            name.len() >= query.len()
                && name.starts_with(head)
                && name[head.len()].starts_with(last.as_str())
        }
        None => false,
    };
    if name == query {
        0
    } else if name.starts_with(query) {
        1
    } else if whole(&name) {
        2
    } else if starts {
        3
    } else if query
        .iter()
        .all(|q| name.iter().any(|w| w.starts_with(q.as_str())))
    {
        4
    } else {
        let mut all = name;
        all.extend(words(&hit.artist));
        all.extend(words(&hit.album));
        if whole(&all) { 5 } else { 6 }
    }
}

/// Toutes les playlists, par nom.
pub fn playlists(conn: &Connection) -> Result<Vec<Hit>> {
    let mut st = conn.prepare(
        "SELECT id, name, songCount, duration, coverArt FROM playlists
         ORDER BY name COLLATE NOCASE",
    )?;
    let rows = st.query_map([], |r| {
        Ok(Hit {
            kind: Kind::Playlist,
            id: r.get(0)?,
            name: r.get(1)?,
            artist: String::new(),
            album: String::new(),
            year: 0,
            count: r.get(2)?,
            duration: r.get(3)?,
            cover: r.get(4)?,
            starred: false,
            album_id: None,
            artist_id: None,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn plural(n: i64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// `2 h 05` ou `45 min`.
pub fn human_duration(secs: i64) -> String {
    let mins = (secs.max(0) + 30) / 60;
    if mins >= 60 {
        format!("{} h {:02}", mins / 60, mins % 60)
    } else {
        format!("{mins} min")
    }
}

impl Hit {
    pub fn subtitle(&self) -> String {
        match self.kind {
            Kind::Playlist => format!(
                "Playlist · {} · {}",
                plural(self.count, "track", "tracks"),
                human_duration(self.duration)
            ),
            Kind::Artist => format!("Artist · {}", plural(self.count, "album", "albums")),
            Kind::Album => {
                let mut s = format!("Album · {}", self.artist);
                if self.year > 0 {
                    s.push_str(&format!(" · {}", self.year));
                }
                s
            }
            Kind::Track => format!("Track · {} · {}", self.artist, self.album),
        }
    }

    fn command(&self, verb: &str, flag: &str) -> String {
        let flag = if flag.is_empty() {
            String::new()
        } else {
            format!(" {flag}")
        };
        format!(
            "melodie {verb} {} {}{flag}",
            self.kind.word(),
            shell_word(&self.id)
        )
    }

    /// Entrée au format du lanceur.
    pub fn to_json(&self) -> Value {
        let cmd = |c: String| json!({"type": "command", "command": c});
        let act = |label: &str, icon: &str, c: String| json!({"label": label, "icon": icon, "action": cmd(c)});
        // Une playlist, un album ou un artiste se lit en aléatoire par défaut
        // (Entrée) ; l'ordre reste en action secondaire.
        let play = if self.kind == Kind::Track {
            self.command("play", "")
        } else {
            self.command("play", "--shuffle")
        };
        let mut actions = if self.kind == Kind::Track {
            vec![act("Lire", ICON_PLAY, play.clone())]
        } else {
            vec![
                act("Shuffle", ICON_SHUFFLE, play.clone()),
                act("Dans l'ordre", ICON_PLAY, self.command("play", "")),
            ]
        };
        actions.push(act(
            "Ajouter à la file",
            ICON_QUEUE,
            self.command("queue", ""),
        ));
        if self.kind != Kind::Playlist {
            actions.push(act("Mix instantané", ICON_MIX, self.command("mix", "")));
            let (label, icon, verb) = if self.starred {
                ("Je n'aime plus", ICON_UNLIKE, "unlike")
            } else {
                ("J'aime", ICON_LIKE, "like")
            };
            actions.push(act(label, icon, self.command(verb, "")));
        }
        if matches!(self.kind, Kind::Track | Kind::Album) {
            actions.push(json!({
                "label": "Ajouter à une playlist…",
                "icon": ICON_TO_PLAYLIST,
                "action": {"type": "submenu",
                    "command": format!("melodie playlist pick --json {} {}", self.kind.word(), shell_word(&self.id))}
            }));
        }
        let mut v = json!({
            "id": format!("{}:{}", self.kind.word(), self.id),
            "name": self.name,
            "subtitle": self.subtitle(),
            "icon": self.kind.icon(),
            "verb": "Play",
            "action": cmd(play),
            "actions": actions,
        });
        if let Some(c) = &self.cover {
            v["_cover"] = json!(c);
        }
        v
    }
}

/// Protège un mot pour le shell s'il contient autre chose que des caractères sûrs.
pub fn shell_word(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.:-".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

pub fn to_json(hits: &[Hit]) -> Vec<Value> {
    hits.iter().map(Hit::to_json).collect()
}

/// Sortie lisible.
pub fn render(hits: &[Hit]) -> String {
    if hits.is_empty() {
        return "Aucun résultat\n".to_string();
    }
    let mut out = String::new();
    for h in hits {
        out.push_str(&format!("{}\n    {}\n", h.name, h.subtitle()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use serde_json::json;

    fn sample() -> (tempfile::TempDir, Connection) {
        let d = tempfile::tempdir().unwrap();
        let mut conn = db::create(&d.path().join("l.db")).unwrap();
        let tx = conn.transaction().unwrap();
        db::insert_artist(
            &tx,
            &json!({"id":"ar1","name":"Daft Punk","albumCount":4,"coverArt":"ar-ar1"}),
        )
        .unwrap();
        db::insert_artist(&tx, &json!({"id":"ar2","name":"Beyoncé","albumCount":1})).unwrap();
        db::insert_album(&tx, &json!({"id":"al1","name":"Discovery","artist":"Daft Punk","artistId":"ar1","year":2001,"coverArt":"al-al1"})).unwrap();
        db::insert_track(&tx, &json!({"id":"t1","title":"One More Time","artist":"Daft Punk","album":"Discovery","albumId":"al1","coverArt":"mf-t1","genres":[{"name":"Electro"}],"playCount":3})).unwrap();
        db::insert_track(
            &tx,
            &json!({"id":"t2","title":"Halo","artist":"Beyoncé","album":"I Am... Sasha Fierce"}),
        )
        .unwrap();
        db::replace_playlists(
            &tx,
            &[json!({"id":"p1","name":"Daft Mix","songCount":42,"duration":7500})],
        )
        .unwrap();
        tx.commit().unwrap();
        (d, conn)
    }

    #[test]
    fn requete_fts() {
        assert_eq!(fts_query("daf pun").as_deref(), Some("\"daf\"* \"pun\"*"));
        assert_eq!(fts_query("  \"x\" -- ").as_deref(), Some("\"x\"*"));
        assert_eq!(fts_query(" ... "), None);
    }

    #[test]
    fn prefixes_casse_accents() {
        let (_d, conn) = sample();
        let r = search(&conn, "daf pun").unwrap();
        assert!(
            r.iter()
                .any(|h| h.kind == Kind::Artist && h.name == "Daft Punk")
        );
        assert!(
            r.iter()
                .any(|h| h.kind == Kind::Track && h.name == "One More Time")
        );
        let r = search(&conn, "BEYONCE").unwrap();
        assert!(r.iter().any(|h| h.name == "Beyoncé"));
        let r = search(&conn, "beyoncé hal").unwrap();
        assert!(r.iter().any(|h| h.name == "Halo"));
        assert!(search(&conn, "zzz").unwrap().is_empty());
        assert!(search(&conn, "").unwrap().is_empty());
    }

    #[test]
    fn ordre_des_types() {
        let (_d, conn) = sample();
        let kinds: Vec<_> = search(&conn, "daft")
            .unwrap()
            .iter()
            .map(|h| h.kind)
            .collect();
        assert_eq!(
            kinds,
            [Kind::Playlist, Kind::Artist, Kind::Album, Kind::Track]
        );
    }

    #[test]
    fn nom_exact_d_abord() {
        let d = tempfile::tempdir().unwrap();
        let mut conn = db::create(&d.path().join("l.db")).unwrap();
        let tx = conn.transaction().unwrap();
        db::insert_artist(&tx, &json!({"id":"ar1","name":"Reverb Man","albumCount":0})).unwrap();
        for (id, title, album) in [
            ("t1", "Wenches & Mead", "Captain Morgan's Revenge"),
            ("t2", "SLAY! (Slowed + Reverb)", "SLAY!"),
            ("t3", "Marche ou rêve", "Millénium"),
            ("t4", "Rêve mieux", "Civilisation"),
            ("t5", "Rêve", "Plaisir Risque Dépendance"),
        ] {
            db::insert_track(
                &tx,
                &json!({"id":id,"title":title,"artist":"X","album":album}),
            )
            .unwrap();
        }
        tx.commit().unwrap();
        let names: Vec<_> = search(&conn, "Rêve")
            .unwrap()
            .into_iter()
            .map(|h| h.name)
            .collect();
        assert_eq!(
            names,
            [
                "Rêve",
                "Rêve mieux",
                "Marche ou rêve",
                "Reverb Man",
                "SLAY! (Slowed + Reverb)",
                "Wenches & Mead"
            ]
        );
        // Un mot en cours de frappe reste un début de mot.
        assert_eq!(search(&conn, "rêv").unwrap()[0].name, "Reverb Man");
    }

    #[test]
    fn json_d_une_entree() {
        let (_d, conn) = sample();
        let r = search(&conn, "daft").unwrap();
        let v = to_json(&r);
        let pl = &v[0];
        assert_eq!(pl["id"], "playlist:p1");
        assert_eq!(pl["name"], "Daft Mix");
        assert_eq!(pl["subtitle"], "Playlist · 42 tracks · 2 h 05");
        assert_eq!(pl["verb"], "Play");
        assert_eq!(pl["action"]["type"], "command");
        assert_eq!(
            pl["action"]["command"],
            "melodie play playlist p1 --shuffle"
        );
        assert!(pl.get("_cover").is_none());
        let acts = pl["actions"].as_array().unwrap();
        let labels: Vec<_> = acts.iter().map(|a| a["label"].as_str().unwrap()).collect();
        assert_eq!(labels, ["Shuffle", "Dans l'ordre", "Ajouter à la file"]);
        for a in acts {
            assert!(a.get("name").is_none());
            assert!(!a["icon"].as_str().unwrap().is_empty());
            assert_eq!(a["action"]["type"], "command");
        }
        assert_eq!(acts[0]["icon"], "󰒟");
        assert_eq!(
            acts[0]["action"]["command"],
            "melodie play playlist p1 --shuffle"
        );
        assert_eq!(acts[1]["action"]["command"], "melodie play playlist p1");
        assert_eq!(acts[2]["icon"], "󰐕");
        assert_eq!(acts[2]["action"]["command"], "melodie queue playlist p1");
        // Un morceau n'a pas de lecture aléatoire.
        let t = v[3]["actions"].as_array().unwrap();
        let labels: Vec<_> = t.iter().map(|a| a["label"].as_str().unwrap()).collect();
        assert_eq!(
            labels,
            [
                "Lire",
                "Ajouter à la file",
                "Mix instantané",
                "J'aime",
                "Ajouter à une playlist…"
            ]
        );
        assert_eq!(t[2]["icon"], "󰒝");
        assert_eq!(t[2]["action"]["command"], "melodie mix track t1");
        assert_eq!(t[3]["icon"], "󰋑");
        assert_eq!(t[3]["action"]["command"], "melodie like track t1");
        assert_eq!(t[4]["icon"], "󰲸");
        assert_eq!(t[4]["action"]["type"], "submenu");
        assert_eq!(
            t[4]["action"]["command"],
            "melodie playlist pick --json track t1"
        );
        // Album : même sous-menu, avec `album <id>` ; artiste : pas de playlist ; playlist : ni l'un ni l'autre.
        let al: Vec<_> = v[2]["actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["label"].as_str().unwrap())
            .collect();
        assert_eq!(
            al,
            [
                "Shuffle",
                "Dans l'ordre",
                "Ajouter à la file",
                "Mix instantané",
                "J'aime",
                "Ajouter à une playlist…"
            ]
        );
        assert_eq!(
            v[2]["actions"][5]["action"]["command"],
            "melodie playlist pick --json album al1"
        );
        assert_eq!(
            v[2]["actions"][3]["action"]["command"],
            "melodie mix album al1"
        );
        let ar: Vec<_> = v[1]["actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["label"].as_str().unwrap())
            .collect();
        assert_eq!(
            ar,
            [
                "Shuffle",
                "Dans l'ordre",
                "Ajouter à la file",
                "Mix instantané",
                "J'aime"
            ]
        );
        assert_eq!(v[1]["subtitle"], "Artist · 4 albums");
        assert_eq!(v[1]["_cover"], "ar-ar1");
        assert_eq!(v[2]["subtitle"], "Album · Daft Punk · 2001");
        assert_eq!(v[3]["subtitle"], "Track · Daft Punk · Discovery");
        assert_eq!(v[3]["action"]["command"], "melodie play track t1");
        assert_eq!(v[3]["_cover"], "mf-t1");
    }

    #[test]
    fn etat_du_favori_dans_les_actions() {
        let (_d, conn) = sample();
        conn.execute(
            "UPDATE tracks SET starred = '2026-01-01T00:00:00Z' WHERE id = 't1'",
            [],
        )
        .unwrap();
        let r = search(&conn, "daft").unwrap();
        let t = r.iter().find(|h| h.kind == Kind::Track).unwrap();
        assert!(t.starred);
        let v = t.to_json();
        let a = &v["actions"][3];
        assert_eq!(a["label"], "Je n'aime plus");
        assert_eq!(a["icon"], "󰋕");
        assert_eq!(a["action"]["command"], "melodie unlike track t1");
        assert!(!r.iter().find(|h| h.kind == Kind::Album).unwrap().starred);
    }

    /// A library where "night" names one artist exactly, albums and tracks that start with it,
    /// and a playlist that only holds the word.
    fn night_library() -> (tempfile::TempDir, Connection) {
        let d = tempfile::tempdir().unwrap();
        let mut conn = db::create(&d.path().join("l.db")).unwrap();
        let tx = conn.transaction().unwrap();
        db::insert_artist(&tx, &json!({"id":"ar1","name":"Night","albumCount":2})).unwrap();
        db::insert_artist(
            &tx,
            &json!({"id":"ar2","name":"The Night Owls","albumCount":1}),
        )
        .unwrap();
        db::insert_album(
            &tx,
            &json!({"id":"al1","name":"Nightfall","artist":"Night","artistId":"ar1","year":2001}),
        )
        .unwrap();
        db::insert_album(
            &tx,
            &json!({"id":"al2","name":"Night","artist":"Night","artistId":"ar1","year":2003}),
        )
        .unwrap();
        for n in 0..30 {
            db::insert_track(
                &tx,
                &json!({"id":format!("t{n}"),"title":format!("Song {n}"),"artist":"Night",
                        "artistId":"ar1","album":"Nightfall","albumId":"al1"}),
            )
            .unwrap();
        }
        db::insert_track(&tx, &json!({"id":"tn","title":"Night","artist":"Night","artistId":"ar1","album":"Night","albumId":"al2"})).unwrap();
        db::replace_playlists(
            &tx,
            &[json!({"id":"p1","name":"Late night drive","songCount":3})],
        )
        .unwrap();
        tx.commit().unwrap();
        (d, conn)
    }

    #[test]
    fn results_are_grouped_by_kind_the_best_kind_first() {
        let (_d, conn) = night_library();
        let groups = grouped(&conn, "night", 5).unwrap();
        let kinds: Vec<_> = groups.iter().map(|g| g.kind).collect();
        // Artist, album and track each have an exact name: the tie order of the kinds breaks
        // it; the playlist only holds the word.
        assert_eq!(
            kinds,
            [Kind::Artist, Kind::Album, Kind::Track, Kind::Playlist]
        );
        let names = |k: Kind| -> Vec<String> {
            groups
                .iter()
                .find(|g| g.kind == k)
                .unwrap()
                .hits
                .iter()
                .map(|h| h.name.clone())
                .collect()
        };
        assert_eq!(names(Kind::Artist), ["Night", "The Night Owls"]);
        assert_eq!(names(Kind::Album), ["Night", "Nightfall"]);
        assert_eq!(names(Kind::Track)[0], "Night", "the exact name first");
        // A kind whose best result is weaker comes after.
        let kinds: Vec<_> = grouped(&conn, "nightf", 5)
            .unwrap()
            .iter()
            .map(|g| g.kind)
            .collect();
        assert_eq!(kinds, [Kind::Album, Kind::Track]);
        assert!(grouped(&conn, "  ", 5).unwrap().is_empty());
        assert!(grouped(&conn, "zzz", 5).unwrap().is_empty());
    }

    #[test]
    fn a_tie_puts_artists_before_albums_tracks_and_playlists() {
        let d = tempfile::tempdir().unwrap();
        let mut conn = db::create(&d.path().join("l.db")).unwrap();
        let tx = conn.transaction().unwrap();
        db::insert_artist(&tx, &json!({"id":"ar1","name":"Dusk","albumCount":1})).unwrap();
        db::insert_album(
            &tx,
            &json!({"id":"al1","name":"Dusk","artist":"Dusk","artistId":"ar1"}),
        )
        .unwrap();
        db::insert_track(
            &tx,
            &json!({"id":"t1","title":"Dusk","artist":"Dusk","artistId":"ar1",
                    "album":"Dusk","albumId":"al1"}),
        )
        .unwrap();
        db::replace_playlists(&tx, &[json!({"id":"p1","name":"Dusk","songCount":1})]).unwrap();
        tx.commit().unwrap();
        let kinds: Vec<_> = grouped(&conn, "dusk", 5)
            .unwrap()
            .iter()
            .map(|g| g.kind)
            .collect();
        assert_eq!(
            kinds,
            [Kind::Artist, Kind::Album, Kind::Track, Kind::Playlist]
        );
    }

    #[test]
    fn a_group_keeps_the_best_few_and_says_there_are_more() {
        let (_d, conn) = night_library();
        let groups = grouped(&conn, "night", 5).unwrap();
        let tracks = groups.iter().find(|g| g.kind == Kind::Track).unwrap();
        assert_eq!(tracks.hits.len(), 5);
        assert!(tracks.more);
        let artists = groups.iter().find(|g| g.kind == Kind::Artist).unwrap();
        assert!(!artists.more);
    }

    #[test]
    fn every_result_of_one_kind_can_be_seen() {
        let (_d, conn) = night_library();
        let all = of_kind(&conn, "night", Kind::Track).unwrap();
        assert_eq!(all.len(), 31);
        assert_eq!(all[0].name, "Night");
        assert!(all.iter().all(|h| h.kind == Kind::Track));
        // more than the launcher's candidates
        let d = tempfile::tempdir().unwrap();
        let mut conn = db::create(&d.path().join("l.db")).unwrap();
        let tx = conn.transaction().unwrap();
        for n in 0..(CANDIDATES + 50) {
            db::insert_track(
                &tx,
                &json!({"id":format!("t{n}"),"title":format!("Rain {n}")}),
            )
            .unwrap();
        }
        tx.commit().unwrap();
        assert_eq!(
            of_kind(&conn, "rain", Kind::Track).unwrap().len(),
            CANDIDATES + 50
        );
    }

    #[test]
    fn a_track_found_knows_its_album_and_artist() {
        let (_d, conn) = night_library();
        let all = of_kind(&conn, "night", Kind::Track).unwrap();
        assert_eq!(all[0].album_id.as_deref(), Some("al2"));
        assert_eq!(all[0].artist_id.as_deref(), Some("ar1"));
        let albums = of_kind(&conn, "nightfall", Kind::Album).unwrap();
        assert_eq!(albums[0].artist_id.as_deref(), Some("ar1"));
    }

    #[test]
    fn durees_et_mots() {
        assert_eq!(human_duration(7500), "2 h 05");
        assert_eq!(human_duration(2700), "45 min");
        assert_eq!(plural(1, "album", "albums"), "1 album");
        assert_eq!(shell_word("al-12"), "al-12");
        assert_eq!(shell_word("a b'c"), "'a b'\\''c'");
    }
}
