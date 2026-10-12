//! Mixes à la manière de Symfonium, calculés sur la copie locale (aucun appel
//! réseau, aucune radio) : mix personnel, mixes instantanés (morceau, album,
//! artiste, genre) et redécouvertes.
//!
//! Tout part d'un ensemble candidat de morceaux, passé au générateur commun :
//! exclusions, trois paniers (aimés, familiers, découverte) tirés au hasard avec
//! des poids, puis un ordre qui étale les artistes et les albums.

use anyhow::Result;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

/// Taille par défaut d'un mix.
pub const DEFAULT_SIZE: usize = 50;
const DAY: i64 = 86_400;
/// Écart minimal entre deux morceaux du même artiste / du même album.
const ARTIST_GAP: usize = 4;
const ALBUM_GAP: usize = 6;
/// Part maximale d'un artiste dans un mix général, et de l'artiste source d'un mix d'artiste.
const ARTIST_CAP: f64 = 0.10;
const SOURCE_ARTIST_CAP: f64 = 0.30;
/// En dessous, un mix instantané est complété (même artiste, puis mix personnel).
const MIN_INSTANT_POOL: usize = 20;
const SKIP_WINDOW: i64 = 7 * DAY;
const RECENT_WINDOW: i64 = DAY;
const REDISCOVER_AFTER: i64 = 90 * DAY;
const SOURCE_ARTIST_WEIGHT: f64 = 3.0;

// ---- hasard reproductible ----

/// Générateur splitmix64 : petit, rapide, reproductible avec `--seed`.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }

    /// Graine tirée du système.
    pub fn from_system() -> Self {
        let mut b = [0u8; 8];
        let _ = getrandom::fill(&mut b);
        Rng(u64::from_le_bytes(b))
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Dans `(0, 1]`.
    fn unit(&mut self) -> f64 {
        ((self.next_u64() >> 11) + 1) as f64 / (1u64 << 53) as f64
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n.max(1) as u64) as usize
    }

    fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.below(i + 1);
            items.swap(i, j);
        }
    }

    /// Clé de tirage pondéré sans remise (course exponentielle) : les plus petites d'abord.
    fn race_key(&mut self, weight: f64) -> f64 {
        -self.unit().ln() / weight.max(1e-9)
    }
}

// ---- dates ----

/// Jours depuis 1970-01-01 d'une date du calendrier grégorien.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Secondes depuis 1970 d'une date RFC 3339 (`2026-10-02T14:03:22.5Z`, `+02:00` accepté).
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || !(b[10] == b'T' || b[10] == b' ') {
        return None;
    }
    let n = |a: usize, z: usize| -> Option<i64> { s.get(a..z)?.parse::<i64>().ok() };
    let (y, mo, d, h, mi, se) = (
        n(0, 4)?,
        n(5, 7)?,
        n(8, 10)?,
        n(11, 13)?,
        n(14, 16)?,
        n(17, 19)?,
    );
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let mut rest = &s[19..];
    if let Some(r) = rest.strip_prefix('.') {
        let digits = r.bytes().take_while(u8::is_ascii_digit).count();
        rest = &r[digits..];
    }
    let offset = match rest {
        "" | "Z" | "z" => 0,
        o if o.len() == 6 && (o.starts_with('+') || o.starts_with('-')) => {
            let hh: i64 = o.get(1..3)?.parse().ok()?;
            let mm: i64 = o.get(4..6)?.parse().ok()?;
            let v = hh * 3600 + mm * 60;
            if o.starts_with('-') {
                -v
            } else {
                v
            }
        }
        _ => return None,
    };
    Some(days_from_civil(y, mo, d) * DAY + h * 3600 + mi * 60 + se - offset)
}

// ---- la copie en mémoire ----

/// Un morceau, réduit à ce que les règles utilisent. Les identifiants texte sont
/// remplacés par des numéros pour que 40 000 morceaux se traitent vite.
#[derive(Debug, Clone)]
pub struct Track {
    pub rowid: i64,
    pub artist: u32,
    pub album: u32,
    /// Numéro de l'ensemble de genres dans `Library::genre_sets`.
    pub genres: u32,
    pub plays: u32,
    pub rating: u8,
    pub starred: bool,
    /// Dernière écoute (secondes), 0 si inconnue.
    pub played: i64,
}

pub struct Library {
    /// Triés par `rowid`.
    pub tracks: Vec<Track>,
    pub artists: HashMap<String, u32>,
    pub albums: HashMap<String, u32>,
    /// Nom d'affichage de chaque genre.
    pub genre_names: Vec<String>,
    /// Nom en minuscules vers numéro de genre.
    pub genre_index: HashMap<String, u32>,
    /// Ensembles de genres (numéros triés), `genre_sets[0]` est vide.
    pub genre_sets: Vec<Vec<u32>>,
    /// `rowid` des morceaux passés dans les 7 derniers jours.
    pub skipped: HashSet<i64>,
    /// Nombre d'écoutes à partir duquel un morceau est parmi les 20 % les plus écoutés.
    pub hot_plays: u32,
    pub now: i64,
}

fn intern(map: &mut HashMap<String, u32>, key: &str) -> u32 {
    if let Some(&i) = map.get(key) {
        return i;
    }
    let i = map.len() as u32;
    map.insert(key.to_string(), i);
    i
}

/// Seuil d'écoutes des 20 % les plus écoutés, parmi les morceaux écoutés.
fn hot_threshold(tracks: &[Track]) -> u32 {
    let mut counts: Vec<u32> = tracks
        .iter()
        .filter(|t| t.plays > 0)
        .map(|t| t.plays)
        .collect();
    if counts.is_empty() {
        return u32::MAX;
    }
    counts.sort_unstable_by(|a, b| b.cmp(a));
    let k = counts.len().div_ceil(5).max(1);
    counts[k - 1]
}

impl Library {
    /// Lit toute la copie. `now` en secondes (injecté pour les tests).
    pub fn load(conn: &Connection, now: i64) -> Result<Library> {
        let mut lib = Library {
            tracks: Vec::new(),
            artists: HashMap::new(),
            albums: HashMap::new(),
            genre_names: Vec::new(),
            genre_index: HashMap::new(),
            genre_sets: vec![Vec::new()],
            skipped: HashSet::new(),
            hot_plays: u32::MAX,
            now,
        };
        let mut raw_sets: HashMap<String, u32> = HashMap::new();
        let mut st = conn.prepare(
            "SELECT rowid, artistId, artist, albumId, album, genres, genre, playCount, played,
                    starred IS NOT NULL, userRating FROM tracks ORDER BY rowid",
        )?;
        let mut rows = st.query([])?;
        while let Some(r) = rows.next()? {
            let key = |a: usize, b: usize| -> Result<String> {
                Ok(match r.get_ref(a)?.as_str_or_null()? {
                    Some(s) if !s.is_empty() => s.to_string(),
                    _ => format!("\0{}", r.get_ref(b)?.as_str_or_null()?.unwrap_or("")),
                })
            };
            let artist = {
                let k = key(1, 2)?;
                intern(&mut lib.artists, &k)
            };
            let album = {
                let k = key(3, 4)?;
                intern(&mut lib.albums, &k)
            };
            let raw = match r.get_ref(5)?.as_str_or_null()? {
                Some(s) if !s.is_empty() => Some(s),
                _ => r.get_ref(6)?.as_str_or_null()?.filter(|s| !s.is_empty()),
            };
            let genres = match raw {
                None => 0,
                Some(raw) => match raw_sets.get(raw) {
                    Some(&i) => i,
                    None => {
                        let i = lib.add_genre_set(raw);
                        raw_sets.insert(raw.to_string(), i);
                        i
                    }
                },
            };
            let played = r
                .get_ref(8)?
                .as_str_or_null()?
                .and_then(parse_rfc3339)
                .unwrap_or(0);
            lib.tracks.push(Track {
                rowid: r.get(0)?,
                artist,
                album,
                genres,
                plays: r.get::<_, i64>(7)?.max(0) as u32,
                rating: r.get::<_, i64>(10)?.clamp(0, 5) as u8,
                starred: r.get(9)?,
                played,
            });
        }
        lib.hot_plays = hot_threshold(&lib.tracks);
        let has_skips: bool = conn.query_row(
            "SELECT count(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = 'skips'",
            [],
            |r| r.get(0),
        )?;
        if has_skips {
            let mut st = conn.prepare(
                "SELECT t.rowid FROM tracks t JOIN skips s ON s.id = t.id WHERE s.at >= ?1",
            )?;
            let rows = st.query_map([now - SKIP_WINDOW], |r| r.get::<_, i64>(0))?;
            for id in rows {
                lib.skipped.insert(id?);
            }
        }
        Ok(lib)
    }

    /// Ensemble de genres d'un texte `["Rock","Pop"]` (ou d'un genre simple).
    fn add_genre_set(&mut self, raw: &str) -> u32 {
        let names: Vec<String> = if raw.starts_with('[') {
            serde_json::from_str(raw).unwrap_or_default()
        } else {
            vec![raw.to_string()]
        };
        let mut ids: Vec<u32> = Vec::new();
        for n in names {
            let n = n.trim();
            if n.is_empty() {
                continue;
            }
            let low = n.to_lowercase();
            let id = match self.genre_index.get(&low) {
                Some(&i) => i,
                None => {
                    let i = self.genre_names.len() as u32;
                    self.genre_names.push(n.to_string());
                    self.genre_index.insert(low, i);
                    i
                }
            };
            ids.push(id);
        }
        ids.sort_unstable();
        ids.dedup();
        if ids.is_empty() {
            return 0;
        }
        self.genre_sets.push(ids);
        (self.genre_sets.len() - 1) as u32
    }

    /// Morceau non éligible : noté 1 ou 2 étoiles, ou passé dans les 7 derniers jours.
    pub fn excluded(&self, t: &Track) -> bool {
        (1..=2).contains(&t.rating) || self.skipped.contains(&t.rowid)
    }

    /// Joué dans les dernières 24 h.
    fn recent(&self, t: &Track) -> bool {
        t.played > 0 && self.now - t.played < RECENT_WINDOW
    }

    /// Aimé, bien noté ou très écouté (panier A).
    pub fn is_hot(&self, t: &Track) -> bool {
        t.starred || t.rating >= 4 || (t.plays > 0 && t.plays >= self.hot_plays)
    }

    fn basket(&self, t: &Track) -> usize {
        if self.is_hot(t) {
            0
        } else if t.plays > 0 || t.rating >= 3 {
            1
        } else {
            2
        }
    }

    pub fn index_of(&self, rowid: i64) -> Option<usize> {
        self.tracks.binary_search_by_key(&rowid, |t| t.rowid).ok()
    }

    fn weight(&self, t: &Track, basket: usize, boost: Option<u32>) -> f64 {
        let ln = (1.0 + f64::from(t.plays)).ln();
        let w = match basket {
            0 => {
                1.0 + 0.5 * ln
                    + if t.starred { 1.0 } else { 0.0 }
                    + if t.rating >= 4 { 0.5 } else { 0.0 }
            }
            1 => 1.0 + 0.5 * ln + 0.25 * f64::from(t.rating),
            _ => 1.0,
        };
        if boost == Some(t.artist) {
            w * SOURCE_ARTIST_WEIGHT
        } else {
            w
        }
    }
}

// ---- générateur commun ----

/// Un panier trié par clé de tirage, avec les morceaux écartés par le plafond d'artiste.
struct Basket {
    items: Vec<usize>,
    ptr: usize,
    deferred: Vec<usize>,
    dptr: usize,
}

struct Caps {
    base: usize,
    boost: Option<(u32, usize)>,
    count: HashMap<u32, usize>,
}

impl Caps {
    fn limit(&self, artist: u32) -> usize {
        match self.boost {
            Some((a, c)) if a == artist => c,
            _ => self.base,
        }
    }

    fn open(&self, artist: u32) -> bool {
        self.count.get(&artist).copied().unwrap_or(0) < self.limit(artist)
    }
}

/// Réglages d'un tirage.
pub struct Draw {
    /// Artiste source d'un mix d'artiste : poids ×3, plafond à 30 %.
    pub boost_artist: Option<u32>,
    /// Morceaux déjà pris (l'ouverture), qui comptent dans les plafonds.
    pub taken: Vec<usize>,
}

/// Part de chaque panier : 40/35/25, ou 30/30/40 si l'inconnu domine.
pub fn shares(a: usize, b: usize, c: usize) -> [f64; 3] {
    if c > 3 * (a + b) {
        [0.30, 0.30, 0.40]
    } else {
        [0.40, 0.35, 0.25]
    }
}

/// Choisit `need` morceaux dans `pool` (indices dans `lib.tracks`) : exclusions,
/// paniers, tirage pondéré, plafonds d'artiste. L'ordre du résultat est celui du
/// tirage, pas encore étalé.
pub fn select(
    lib: &Library,
    pool: &[usize],
    draw: &Draw,
    need: usize,
    rng: &mut Rng,
) -> Vec<usize> {
    if need == 0 {
        return Vec::new();
    }
    let mut taken_set: HashSet<usize> = draw.taken.iter().copied().collect();
    let mut eligible: Vec<usize> = pool
        .iter()
        .copied()
        .filter(|i| !taken_set.contains(i) && !lib.excluded(&lib.tracks[*i]))
        .collect();
    // Les écoutes des dernières 24 h ne comptent que s'il reste assez de candidats.
    let fresh = eligible
        .iter()
        .filter(|i| !lib.recent(&lib.tracks[**i]))
        .count();
    if fresh >= need {
        eligible.retain(|i| !lib.recent(&lib.tracks[*i]));
    }

    let mut lists: [Vec<usize>; 3] = Default::default();
    for &i in &eligible {
        lists[lib.basket(&lib.tracks[i])].push(i);
    }
    let share = shares(lists[0].len(), lists[1].len(), lists[2].len());

    let mut baskets: Vec<Basket> = Vec::with_capacity(3);
    for (b, list) in lists.iter().enumerate() {
        let mut keyed: Vec<(f64, usize)> = list
            .iter()
            .map(|&i| {
                (
                    rng.race_key(lib.weight(&lib.tracks[i], b, draw.boost_artist)),
                    i,
                )
            })
            .collect();
        keyed.sort_unstable_by(|x, y| x.0.total_cmp(&y.0).then(x.1.cmp(&y.1)));
        baskets.push(Basket {
            items: keyed.into_iter().map(|(_, i)| i).collect(),
            ptr: 0,
            deferred: Vec::new(),
            dptr: 0,
        });
    }

    let size_total = need + draw.taken.len();
    let base = ((size_total as f64 * ARTIST_CAP).ceil() as usize).max(1);
    let mut caps = Caps {
        base,
        boost: draw.boost_artist.map(|a| {
            (
                a,
                ((size_total as f64 * SOURCE_ARTIST_CAP).ceil() as usize).max(1),
            )
        }),
        count: HashMap::new(),
    };
    for &i in &draw.taken {
        *caps.count.entry(lib.tracks[i].artist).or_insert(0) += 1;
    }

    let mut chosen: Vec<usize> = Vec::with_capacity(need);
    let mut got = [0usize; 3];
    for relaxed in [false, true] {
        while chosen.len() < need {
            // Panier le plus en retard sur sa part, parmi ceux qui ont encore un morceau.
            let mut best: Option<(usize, usize, f64)> = None;
            for b in 0..3 {
                let Some(i) = peek(&mut baskets[b], lib, &caps, relaxed) else {
                    continue;
                };
                let ratio = got[b] as f64 / share[b];
                if best.is_none_or(|(_, _, r)| ratio < r) {
                    best = Some((b, i, ratio));
                }
            }
            let Some((b, i, _)) = best else { break };
            advance(&mut baskets[b], relaxed);
            got[b] += 1;
            *caps.count.entry(lib.tracks[i].artist).or_insert(0) += 1;
            taken_set.insert(i);
            chosen.push(i);
        }
    }
    chosen
}

/// Prochain morceau du panier sous le plafond (ou sans plafond si `relaxed`), sans le consommer.
fn peek(b: &mut Basket, lib: &Library, caps: &Caps, relaxed: bool) -> Option<usize> {
    if relaxed {
        if b.dptr < b.deferred.len() {
            return Some(b.deferred[b.dptr]);
        }
        return b.items.get(b.ptr).copied();
    }
    while let Some(&i) = b.items.get(b.ptr) {
        if caps.open(lib.tracks[i].artist) {
            return Some(i);
        }
        b.deferred.push(i);
        b.ptr += 1;
    }
    None
}

fn advance(b: &mut Basket, relaxed: bool) {
    if relaxed && b.dptr < b.deferred.len() {
        b.dptr += 1;
    } else {
        b.ptr += 1;
    }
}

/// Ordonne `rest` après `first` : jamais le même artiste à moins de 4 positions
/// ni le même album à moins de 6, tant qu'une alternative existe. L'ordre de
/// départ (déjà mélangé) est respecté autant que possible.
pub fn arrange(lib: &Library, first: Option<usize>, mut rest: Vec<usize>) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::with_capacity(rest.len() + 1);
    let mut last_artist: HashMap<u32, usize> = HashMap::new();
    let mut last_album: HashMap<u32, usize> = HashMap::new();
    let mut left_artist: HashMap<u32, usize> = HashMap::new();
    let mut left_album: HashMap<u32, usize> = HashMap::new();
    for &i in &rest {
        *left_artist.entry(lib.tracks[i].artist).or_insert(0) += 1;
        *left_album.entry(lib.tracks[i].album).or_insert(0) += 1;
    }
    let place = |out: &mut Vec<usize>,
                 i: usize,
                 la: &mut HashMap<u32, usize>,
                 lb: &mut HashMap<u32, usize>| {
        let t = &lib.tracks[i];
        la.insert(t.artist, out.len());
        lb.insert(t.album, out.len());
        out.push(i);
    };
    if let Some(f) = first {
        place(&mut out, f, &mut last_artist, &mut last_album);
    }
    while !rest.is_empty() {
        let p = out.len();
        let left = rest.len() as isize;
        let dist = |m: &HashMap<u32, usize>, k: u32| m.get(&k).map_or(usize::MAX, |&l| p - l);
        let mut tier1: Option<usize> = None;
        let mut pressing: Option<(isize, usize)> = None;
        let mut tier2: Option<usize> = None;
        let mut tier3: Option<usize> = None;
        let mut best: (usize, usize) = (0, 0);
        for (pos, &i) in rest.iter().enumerate() {
            let t = &lib.tracks[i];
            let da = dist(&last_artist, t.artist);
            let db = dist(&last_album, t.album);
            let (ok_a, ok_b) = (da >= ARTIST_GAP, db >= ALBUM_GAP);
            if ok_a && ok_b {
                tier1.get_or_insert(pos);
                // Un artiste ou un album qui risque de ne plus avoir la place pour tous
                // ses morceaux passe en priorité (le plus pressé d'abord).
                let ra = left_artist[&t.artist] as isize;
                let rb = left_album[&t.album] as isize;
                let pressure = ((ra - 1) * ARTIST_GAP as isize + 1)
                    .max((rb - 1) * ALBUM_GAP as isize + 1)
                    - left;
                if pressure >= -(ARTIST_GAP as isize) && pressing.is_none_or(|(q, _)| pressure > q)
                {
                    pressing = Some((pressure, pos));
                }
            } else if ok_a {
                tier2.get_or_insert(pos);
            } else if ok_b {
                tier3.get_or_insert(pos);
            }
            let score = (da.saturating_mul(ALBUM_GAP)).min(db.saturating_mul(ARTIST_GAP));
            if pos == 0 || score > best.0 {
                best = (score, pos);
            }
        }
        let pos = pressing
            .map(|(_, p)| p)
            .or(tier1)
            .or(tier2)
            .or(tier3)
            .unwrap_or(best.1);
        let i = rest.remove(pos);
        if let Some(r) = left_artist.get_mut(&lib.tracks[i].artist) {
            *r -= 1;
        }
        if let Some(r) = left_album.get_mut(&lib.tracks[i].album) {
            *r -= 1;
        }
        place(&mut out, i, &mut last_artist, &mut last_album);
    }
    out
}

/// Générateur commun : sélection puis ordre étalé. `opener` ouvre le mix.
pub fn generate(
    lib: &Library,
    pool: &[usize],
    size: usize,
    opener: Option<usize>,
    boost_artist: Option<u32>,
    rng: &mut Rng,
) -> Vec<usize> {
    let draw = Draw {
        boost_artist,
        taken: opener.into_iter().collect(),
    };
    let need = size.saturating_sub(draw.taken.len());
    let mut chosen = select(lib, pool, &draw, need, rng);
    rng.shuffle(&mut chosen);
    arrange(lib, opener, chosen)
}

// ---- les sortes de mix ----

/// Source d'un mix instantané.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Track(usize),
    Album(Vec<usize>),
    Artist(Vec<usize>),
    Genre(u32),
}

/// Mix personnel : toute la bibliothèque.
pub fn personal(lib: &Library, size: usize, rng: &mut Rng) -> Vec<usize> {
    let pool: Vec<usize> = (0..lib.tracks.len()).collect();
    generate(lib, &pool, size, None, None, rng)
}

/// Mix instantané autour d'une source.
pub fn instant(lib: &Library, source: &Source, size: usize, rng: &mut Rng) -> Vec<usize> {
    let src_tracks: Vec<usize> = match source {
        Source::Track(i) => vec![*i],
        Source::Album(v) | Source::Artist(v) => v.clone(),
        Source::Genre(_) => Vec::new(),
    };
    // Genres de la source : union de ceux de ses morceaux.
    let mut wanted: HashSet<u32> = HashSet::new();
    match source {
        Source::Genre(g) => {
            wanted.insert(*g);
        }
        _ => {
            for &i in &src_tracks {
                wanted.extend(
                    lib.genre_sets[lib.tracks[i].genres as usize]
                        .iter()
                        .copied(),
                );
            }
        }
    }
    let matching: Vec<bool> = lib
        .genre_sets
        .iter()
        .map(|s| s.iter().any(|g| wanted.contains(g)))
        .collect();
    let mut in_pool = vec![false; lib.tracks.len()];
    let mut pool: Vec<usize> = Vec::new();
    for (i, t) in lib.tracks.iter().enumerate() {
        if matching[t.genres as usize] {
            in_pool[i] = true;
            pool.push(i);
        }
    }
    let count_ok = |pool: &[usize]| {
        pool.iter()
            .filter(|i| !lib.excluded(&lib.tracks[**i]))
            .count()
    };

    // Ouverture : le morceau, ou un titre aimé/fréquent de l'album ou de l'artiste.
    let opener = match source {
        Source::Track(i) => Some(*i),
        Source::Album(v) | Source::Artist(v) => open_with(lib, v, rng),
        Source::Genre(_) => None,
    };
    let boost = match source {
        Source::Artist(v) => v.first().map(|&i| lib.tracks[i].artist),
        _ => None,
    };

    // Peu de candidats par les genres : on complète (même artiste, puis mix personnel)
    // jusqu'à la taille demandée.
    let mut complete_with_personal = false;
    if count_ok(&pool) < MIN_INSTANT_POOL {
        complete_with_personal = true;
        // Compléter avec les morceaux du même artiste.
        let artists: HashSet<u32> = src_tracks.iter().map(|&i| lib.tracks[i].artist).collect();
        for (i, t) in lib.tracks.iter().enumerate() {
            if !in_pool[i] && artists.contains(&t.artist) {
                in_pool[i] = true;
                pool.push(i);
            }
        }
    }

    let draw = Draw {
        boost_artist: boost,
        taken: opener.into_iter().collect(),
    };
    let need = size.saturating_sub(draw.taken.len());
    let mut chosen = select(lib, &pool, &draw, need, rng);
    if complete_with_personal && chosen.len() < need {
        // Puis par le mix personnel, hors de ce qui est déjà pris.
        let mut taken = draw.taken.clone();
        taken.extend(chosen.iter().copied());
        let all: Vec<usize> = (0..lib.tracks.len()).collect();
        let more = select(
            lib,
            &all,
            &Draw {
                boost_artist: None,
                taken,
            },
            need - chosen.len(),
            rng,
        );
        chosen.extend(more);
    }
    rng.shuffle(&mut chosen);
    arrange(lib, opener, chosen)
}

/// Titre qui ouvre un mix d'album ou d'artiste : l'aimé, le mieux noté ou le plus
/// écouté parmi les morceaux éligibles (au hasard à égalité).
fn open_with(lib: &Library, tracks: &[usize], rng: &mut Rng) -> Option<usize> {
    let score = |i: usize| {
        let t = &lib.tracks[i];
        (
            u8::from(t.starred),
            u8::from(t.rating >= 4),
            u8::from(!lib.recent(t)),
            t.plays,
        )
    };
    let ok: Vec<usize> = tracks
        .iter()
        .copied()
        .filter(|&i| !lib.excluded(&lib.tracks[i]))
        .collect();
    let list = if ok.is_empty() { tracks } else { &ok };
    let top = list.iter().map(|&i| score(i)).max()?;
    let best: Vec<usize> = list.iter().copied().filter(|&i| score(i) == top).collect();
    Some(best[rng.below(best.len())])
}

/// Un album avec au moins une écoute.
pub struct AlbumStat {
    pub key: u32,
    pub plays: u32,
}

/// Albums écoutés mais pas depuis 90 jours (`playCount > 0`).
pub fn stale_albums(conn: &Connection, lib: &Library) -> Result<Vec<AlbumStat>> {
    let mut st =
        conn.prepare("SELECT id, playCount, played FROM albums WHERE playCount > 0 ORDER BY id")?;
    let mut rows = st.query([])?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        let id: String = r.get(0)?;
        let Some(&key) = lib.albums.get(&id) else {
            continue;
        };
        let played = r
            .get_ref(2)?
            .as_str_or_null()?
            .and_then(parse_rfc3339)
            .unwrap_or(0);
        if played == 0 || lib.now - played >= REDISCOVER_AFTER {
            out.push(AlbumStat {
                key,
                plays: r.get::<_, i64>(1)?.max(0) as u32,
            });
        }
    }
    Ok(out)
}

/// Redécouvertes : 1 à 3 morceaux par album ancien, étalés.
pub fn rediscover(lib: &Library, albums: &[AlbumStat], size: usize, rng: &mut Rng) -> Vec<usize> {
    let wanted: HashMap<u32, usize> = albums.iter().enumerate().map(|(n, a)| (a.key, n)).collect();
    let mut by_album: Vec<Vec<usize>> = vec![Vec::new(); albums.len()];
    for (i, t) in lib.tracks.iter().enumerate() {
        if let Some(&n) = wanted.get(&t.album) {
            if !lib.excluded(t) {
                by_album[n].push(i);
            }
        }
    }
    let mut order: Vec<(f64, usize)> = albums
        .iter()
        .enumerate()
        .filter(|(n, _)| !by_album[*n].is_empty())
        .map(|(n, a)| (rng.race_key(1.0 + (1.0 + f64::from(a.plays)).ln()), n))
        .collect();
    order.sort_unstable_by(|x, y| x.0.total_cmp(&y.0).then(x.1.cmp(&y.1)));
    let mut picked: Vec<usize> = Vec::new();
    for (_, n) in order {
        if picked.len() >= size {
            break;
        }
        let k = (1 + rng.below(3)).min(by_album[n].len());
        let mut keyed: Vec<(f64, usize)> = by_album[n]
            .iter()
            .map(|&i| {
                let t = &lib.tracks[i];
                let w = 1.0
                    + 2.0 * f64::from(u8::from(t.starred))
                    + f64::from(u8::from(t.rating >= 4))
                    + (1.0 + f64::from(t.plays)).ln();
                (rng.race_key(w), i)
            })
            .collect();
        keyed.sort_unstable_by(|x, y| x.0.total_cmp(&y.0).then(x.1.cmp(&y.1)));
        picked.extend(keyed.into_iter().take(k).map(|(_, i)| i));
    }
    picked.truncate(size);
    rng.shuffle(&mut picked);
    arrange(lib, None, picked)
}

/// Les `n` genres les plus présents parmi les morceaux aimés ou fréquents :
/// (nom, morceaux aimés ou fréquents, morceaux au total).
pub fn top_genres(lib: &Library, n: usize) -> Vec<(String, usize, usize)> {
    let mut hot = vec![0usize; lib.genre_names.len()];
    let mut all = vec![0usize; lib.genre_names.len()];
    // Un ensemble de genres sert à beaucoup de morceaux : on compte par ensemble.
    let mut sets_hot = vec![0usize; lib.genre_sets.len()];
    let mut sets_all = vec![0usize; lib.genre_sets.len()];
    for t in &lib.tracks {
        sets_all[t.genres as usize] += 1;
        if lib.is_hot(t) && !lib.excluded(t) {
            sets_hot[t.genres as usize] += 1;
        }
    }
    for (s, genres) in lib.genre_sets.iter().enumerate() {
        for &g in genres {
            hot[g as usize] += sets_hot[s];
            all[g as usize] += sets_all[s];
        }
    }
    let mut v: Vec<(String, usize, usize)> = (0..hot.len())
        .filter(|&g| hot[g] > 0)
        .map(|g| (lib.genre_names[g].clone(), hot[g], all[g]))
        .collect();
    v.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
            .then_with(|| a.0.cmp(&b.0))
    });
    v.truncate(n);
    v
}

// ---- retrouver la source ----

/// Source d'après un identifiant, à défaut un nom (sans casse). `Err` : message lisible.
pub fn find_source(
    conn: &Connection,
    lib: &Library,
    kind: SourceKind,
    key: &str,
) -> std::result::Result<Source, String> {
    let rowids = |sql: &str, p: &str| -> Vec<usize> {
        conn.prepare(sql)
            .and_then(|mut st| {
                let rows = st.query_map([p], |r| r.get::<_, i64>(0))?;
                Ok(rows
                    .filter_map(|r| r.ok())
                    .filter_map(|id| lib.index_of(id))
                    .collect())
            })
            .unwrap_or_default()
    };
    let one =
        |sql: &str, p: &str| -> Option<String> { conn.query_row(sql, [p], |r| r.get(0)).ok() };
    match kind {
        SourceKind::Genre => lib
            .genre_index
            .get(&key.trim().to_lowercase())
            .map(|&g| Source::Genre(g))
            .ok_or_else(|| format!("genre inconnu : {key}")),
        SourceKind::Track => {
            let v = rowids("SELECT rowid FROM tracks WHERE id = ?1", key);
            let v = if v.is_empty() {
                rowids("SELECT rowid FROM tracks WHERE title = ?1 COLLATE NOCASE ORDER BY playCount DESC LIMIT 1", key)
            } else {
                v
            };
            v.first()
                .map(|&i| Source::Track(i))
                .ok_or_else(|| format!("morceau inconnu : {key}"))
        }
        SourceKind::Album => {
            let id = one("SELECT id FROM albums WHERE id = ?1", key)
                .or_else(|| one("SELECT id FROM albums WHERE name = ?1 COLLATE NOCASE ORDER BY playCount DESC LIMIT 1", key))
                .ok_or_else(|| format!("album inconnu : {key}"))?;
            let v = rowids("SELECT rowid FROM tracks WHERE albumId = ?1", &id);
            if v.is_empty() {
                Err(format!("album sans morceau dans la copie : {key}"))
            } else {
                Ok(Source::Album(v))
            }
        }
        SourceKind::Artist => {
            let id = one("SELECT id FROM artists WHERE id = ?1", key)
                .or_else(|| {
                    one(
                        "SELECT id FROM artists WHERE name = ?1 COLLATE NOCASE LIMIT 1",
                        key,
                    )
                })
                .ok_or_else(|| format!("artiste inconnu : {key}"))?;
            let v = rowids("SELECT rowid FROM tracks WHERE artistId = ?1", &id);
            if v.is_empty() {
                Err(format!("artiste sans morceau dans la copie : {key}"))
            } else {
                Ok(Source::Artist(v))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Track,
    Album,
    Artist,
    Genre,
}

/// Détails d'affichage des morceaux d'un mix (lus par `rowid`, dans l'ordre du mix).
pub struct Detail {
    pub info: crate::track::TrackInfo,
}

pub fn details(conn: &Connection, lib: &Library, picked: &[usize]) -> Result<Vec<Detail>> {
    let mut st = conn.prepare_cached(
        "SELECT t.id, t.title, t.artist, t.album, COALESCE(a.artist, t.artist), t.duration,
                t.suffix, t.bitRate, t.bitDepth, t.samplingRate, t.coverArt,
                t.starred IS NOT NULL
         FROM tracks t LEFT JOIN albums a ON a.id = t.albumId WHERE t.rowid = ?1",
    )?;
    let mut out = Vec::with_capacity(picked.len());
    for &i in picked {
        let d = st.query_row([lib.tracks[i].rowid], |r| {
            Ok(Detail {
                info: crate::track::TrackInfo {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    artist: r.get(2)?,
                    album: r.get(3)?,
                    album_artist: r.get(4)?,
                    duration: r.get::<_, i64>(5)?.max(0) as u64,
                    suffix: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
                    bit_rate: r.get::<_, i64>(7)?.max(0) as u64,
                    bit_depth: r.get::<_, i64>(8)?.max(0) as u64,
                    sampling_rate: r.get::<_, i64>(9)?.max(0) as u64,
                    cover: r.get(10)?,
                    starred: r.get(11)?,
                },
            })
        })?;
        out.push(d);
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
