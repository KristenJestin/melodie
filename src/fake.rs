//! Faux serveur Subsonic pour les tests : HTTP minimal sur un port local.

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

pub struct Fake {
    pub url: String,
    pub state: Arc<State>,
}

pub struct State {
    pub songs: Mutex<Vec<Value>>,
    pub last_scan: Mutex<String>,
    pub expected_token: Mutex<String>,
    /// Code d'erreur Subsonic à renvoyer pour tout, si présent.
    pub fail_code: Mutex<Option<i64>>,
    /// Réponse HTTP brute à renvoyer pour tout (état, corps), si présente.
    pub raw: Mutex<Option<(u16, String)>>,
    pub calls: Mutex<Vec<String>>,
    pub tokens_seen: Mutex<Vec<String>>,
    pub stop: AtomicBool,
    /// Durée en secondes des flux servis.
    pub stream_secs: AtomicUsize,
    pub n_requests: AtomicUsize,
    /// Délai en millisecondes avant chaque pochette servie.
    pub cover_delay_ms: AtomicUsize,
    /// Tant que vrai, le serveur retient ses réponses (20 s au plus) : sert à
    /// prouver qu'un appel ne dépend pas du réseau sans mesurer de durée.
    pub hold: AtomicBool,
}

/// Décode `%XX` et `+` d'une valeur de requête.
fn decode(v: &str) -> String {
    let b = v.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() && u8::from_str_radix(&v[i + 1..i + 3], 16).is_ok() => {
                out.push(u8::from_str_radix(&v[i + 1..i + 3], 16).unwrap());
                i += 2;
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| decode(v))
    })
}

/// Toutes les valeurs d'un paramètre répété.
pub fn params(query: &str, key: &str) -> Vec<String> {
    query
        .split('&')
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            (k == key).then(|| decode(v))
        })
        .collect()
}

fn page(items: &[Value], query: &str, count: &str, offset: &str) -> Vec<Value> {
    let c: usize = param(query, count)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let o: usize = param(query, offset)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    items.iter().skip(o).take(c).cloned().collect()
}

pub fn song(i: usize) -> Value {
    json!({"id": format!("s{i}"), "title": format!("Titre numéro {i}"),
           "artist": if i.is_multiple_of(2) {"Daft Punk"} else {"Beyoncé"}, "artistId": if i.is_multiple_of(2) {"ar1"} else {"ar2"},
           "album": "Discovery", "albumId": "al1", "year": 2001, "duration": 200,
           "suffix": "flac", "bitRate": 900, "playCount": 0, "coverArt": format!("mf-s{i}")})
}

impl Fake {
    pub fn start(n_songs: usize, token: &str) -> Fake {
        let state = Arc::new(State {
            songs: Mutex::new((0..n_songs).map(song).collect()),
            last_scan: Mutex::new("2026-10-01T10:00:00Z".into()),
            expected_token: Mutex::new(token.to_string()),
            fail_code: Mutex::new(None),
            raw: Mutex::new(None),
            calls: Mutex::new(Vec::new()),
            tokens_seen: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
            stream_secs: AtomicUsize::new(3),
            n_requests: AtomicUsize::new(0),
            cover_delay_ms: AtomicUsize::new(0),
            hold: AtomicBool::new(false),
        });
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let st = state.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if st.stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut s) = stream else { continue };
                let st = st.clone();
                std::thread::spawn(move || {
                    let mut buf = [0u8; 8192];
                    let n = s.read(&mut buf).unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    let line = req.lines().next().unwrap_or("").to_string();
                    let target = line.split(' ').nth(1).unwrap_or("");
                    let (path, query) = target.split_once('?').unwrap_or((target, ""));
                    let method = path
                        .rsplit('/')
                        .next()
                        .unwrap_or("")
                        .trim_end_matches(".view")
                        .to_string();
                    st.n_requests.fetch_add(1, Ordering::SeqCst);
                    st.calls.lock().unwrap().push(format!("{method}?{query}"));
                    if let Some(t) = param(query, "t") {
                        st.tokens_seen.lock().unwrap().push(t);
                    }
                    if matches!(method.as_str(), "stream" | "getCoverArt") {
                        let (status, ctype, body) = st.binary(&method, query);
                        let head = format!(
                            "HTTP/1.1 {status} X\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = s.write_all(head.as_bytes());
                        let _ = s.write_all(&body);
                        return;
                    }
                    let (status, body) = st.respond(&method, query);
                    let reply = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = s.write_all(reply.as_bytes());
                });
            }
        });
        Fake { url, state }
    }

    pub fn calls_of(&self, method: &str) -> usize {
        self.state
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.starts_with(&format!("{method}?")))
            .count()
    }

    pub fn clear_calls(&self) {
        self.state.calls.lock().unwrap().clear();
    }
}

fn ok(body: Value) -> (u16, String) {
    let mut r = json!({"status": "ok", "version": "1.16.1", "type": "navidrome"});
    for (k, v) in body.as_object().unwrap() {
        r[k] = v.clone();
    }
    (200, json!({"subsonic-response": r}).to_string())
}

impl State {
    fn respond(&self, method: &str, query: &str) -> (u16, String) {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while self.hold.load(Ordering::SeqCst) && std::time::Instant::now() < end {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if let Some(r) = self.raw.lock().unwrap().clone() {
            return r;
        }
        if let Some(code) = *self.fail_code.lock().unwrap() {
            return (
                200,
                json!({"subsonic-response": {"status": "failed", "version": "1.16.1",
                       "error": {"code": code, "message": "refusé"}}})
                .to_string(),
            );
        }
        if param(query, "t").as_deref() != Some(self.expected_token.lock().unwrap().as_str()) {
            return (
                200,
                json!({"subsonic-response": {"status": "failed", "version": "1.16.1",
                       "error": {"code": 40, "message": "Wrong username or password"}}})
                .to_string(),
            );
        }
        let songs = self.songs.lock().unwrap().clone();
        match method {
            "ping" => ok(json!({})),
            "getScanStatus" => ok(json!({"scanStatus": {"scanning": false, "count": 1,
                "lastScan": self.last_scan.lock().unwrap().clone()}})),
            "search3" => {
                let artists = vec![
                    json!({"id":"ar1","name":"Daft Punk","albumCount":1,"coverArt":"ar-ar1"}),
                    json!({"id":"ar2","name":"Beyoncé","albumCount":1}),
                ];
                let albums = vec![json!({"id":"al1","name":"Discovery","artist":"Daft Punk",
                    "artistId":"ar1","year":2001,"songCount":songs.len(),"coverArt":"al-al1","playCount":1})];
                ok(json!({"searchResult3": {
                    "artist": page(&artists, query, "artistCount", "artistOffset"),
                    "album": page(&albums, query, "albumCount", "albumOffset"),
                    "song": page(&songs, query, "songCount", "songOffset"),
                }}))
            }
            "getPlaylists" => ok(json!({"playlists": {"playlist": [
                {"id":"p1","name":"Daft Mix","songCount":42,"duration":7500,"changed":"2026-10-01T10:00:00Z","owner":"kris","coverArt":"pl-p1"}]}})),
            "getStarred2" => ok(json!({"starred2": {"song": [
                {"id":"s1","starred":"2026-09-01T00:00:00Z"}]}})),
            "getAlbumList2" => ok(json!({"albumList2": {"album": [
                {"id":"al1","name":"Discovery","playCount":7,"played":"2026-10-02T00:00:00Z"}]}})),
            "getAlbum" => {
                let id = param(query, "id").unwrap_or_default();
                let mut in_album: Vec<Value> = songs
                    .iter()
                    .filter(|s| s["albumId"] == id.as_str())
                    .cloned()
                    .collect();
                for s in &mut in_album {
                    if s["id"] == "s0" {
                        s["playCount"] = json!(5);
                        s["played"] = json!("2026-10-02T00:00:00Z");
                    }
                }
                ok(json!({"album": {"id": id, "song": in_album}}))
            }
            "getSong" => {
                let id = param(query, "id").unwrap_or_default();
                match songs.iter().find(|s| s["id"] == id.as_str()) {
                    Some(s) => ok(json!({"song": s})),
                    None => no_such("getSong"),
                }
            }
            "getArtist" => ok(
                json!({"artist": {"id": "ar1", "name": "Daft Punk", "album": [
                {"id":"al1","name":"Discovery","year":2001},
                {"id":"al2","name":"Homework","year":1997}]}}),
            ),
            "getPlaylist" => ok(json!({"playlist": {"id": "p1", "name": "Daft Mix",
                "entry": songs}})),
            "scrobble" | "star" | "unstar" | "updatePlaylist" | "deletePlaylist" => ok(json!({})),
            "createPlaylist" => {
                let n = params(query, "songId").len();
                ok(
                    json!({"playlist": {"id": "p9", "name": param(query, "name").unwrap_or_default(),
                    "songCount": n, "duration": n * 200, "owner": "kris",
                    "changed": "2026-10-08T10:00:00Z"}}),
                )
            }
            _ => no_such(method),
        }
    }

    /// Réponses non JSON : flux audio (WAV de silence) et pochette (PNG).
    /// Le jeton est vérifié comme ailleurs ; sinon, erreur Subsonic en JSON.
    fn binary(&self, method: &str, query: &str) -> (u16, &'static str, Vec<u8>) {
        let (status, body) = self.respond("ping", query);
        if status != 200 || !body.contains("\"status\":\"ok\"") {
            return (200, "application/json", body.into_bytes());
        }
        if method == "stream" {
            let secs = self.stream_secs.load(Ordering::SeqCst) as u32;
            (200, "audio/wav", wav_silence(secs))
        } else {
            let ms = self.cover_delay_ms.load(Ordering::SeqCst) as u64;
            std::thread::sleep(std::time::Duration::from_millis(ms));
            (200, "image/png", PNG.to_vec())
        }
    }
}

fn no_such(method: &str) -> (u16, String) {
    (
        200,
        json!({"subsonic-response": {"status": "failed", "version": "1.16.1",
               "error": {"code": 70, "message": format!("inconnu : {method}")}}})
        .to_string(),
    )
}

/// WAV mono 8 kHz 16 bits de `secs` secondes de silence.
pub fn wav_silence(secs: u32) -> Vec<u8> {
    let rate = 8000u32;
    let data = rate * 2 * secs;
    let mut w = Vec::with_capacity(44 + data as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data.to_le_bytes());
    w.resize(44 + data as usize, 0);
    w
}

/// PNG 1x1 valide.
pub const PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8, 0xFF, 0xFF, 0x3F,
    0x00, 0x05, 0xFE, 0x02, 0xFE, 0xA7, 0x35, 0x81, 0x84, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E,
    0x44, 0xAE, 0x42, 0x60, 0x82,
];
