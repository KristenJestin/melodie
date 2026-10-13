//! Test d'intégration du démon, sans son : `MELODIE_AO=null`, faux serveur
//! Navidrome local, HOME et XDG_* temporaires, trousseau remplacé par un fichier
//! (`MELODIE_TEST_SECRET`), nom MPRIS propre au test.
//!
//! Le service MPRIS vit sur un bus de session privé (`dbus-daemon` lancé par le test, jamais le vrai bus) ; sans `dbus-daemon`,
//! les vérifications MPRIS sont sautées avec un message.

use melodie_core::fake::{self, Fake};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

const TOKEN: &str = "tok3f9a1c77";
const SALT: &str = "selb81d02";
static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct Sandbox {
    dir: tempfile::TempDir,
    fake: Fake,
    bus_name: String,
    bus_addr: Option<String>,
    bus: Option<std::process::Child>,
}

/// Attente active (sondage toutes les 50 ms) avec délai maximal. Le délai
/// demandé est un minimum de confort : sous forte charge (machine saturée,
/// tests en parallèle) un démon peut mettre bien plus de temps à répondre,
/// d'où un plancher large. Un test qui réussit ne l'attend jamais.
const WAIT_FLOOR_SECS: u64 = 30;

fn wait_until(secs: u64, what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs.max(WAIT_FLOOR_SECS));
    while Instant::now() < end {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("délai dépassé : {what}");
}

/// Bus de session privé du test (jamais le vrai bus de l'utilisateur, où le
/// faux lecteur apparaîtrait dans les lecteurs du bureau). `None` si `dbus-daemon` est absent.
fn private_bus() -> Option<(std::process::Child, String)> {
    let mut child = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address=1"])
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut line = String::new();
    BufReader::new(child.stdout.take()?)
        .read_line(&mut line)
        .ok()?;
    let addr = line.trim().to_string();
    if addr.is_empty() {
        let _ = child.kill();
        return None;
    }
    Some((child, addr))
}

fn song(n: usize, extra: Value) -> Value {
    let mut s = fake::song(n);
    s["id"] = json!(format!("t{n}"));
    s["title"] = json!(format!("Titre {n}"));
    s["duration"] = json!(3);
    s["coverArt"] = json!("al-al1");
    for (k, v) in extra.as_object().unwrap() {
        s[k] = v.clone();
    }
    s
}

impl Sandbox {
    fn new(with_login: bool) -> Sandbox {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::start(0, TOKEN);
        *fake.state.songs.lock().unwrap() = vec![
            song(
                1,
                json!({"suffix":"flac","bitDepth":24,"samplingRate":96000,"bitRate":2304}),
            ),
            song(2, json!({"suffix":"mp3","bitRate":320})),
            song(3, json!({"suffix":"wav","bitRate":128})),
        ];
        let (bus, bus_addr) = private_bus().unzip();
        let sb = Sandbox {
            bus_name: format!("melodie_test_{}_{n}", std::process::id()),
            bus_addr,
            bus,
            dir,
            fake,
        };
        for d in ["home", "run"] {
            std::fs::create_dir_all(sb.dir.path().join(d)).unwrap();
        }
        if with_login {
            let conf = sb.dir.path().join("home/.config/melodie");
            std::fs::create_dir_all(&conf).unwrap();
            std::fs::write(
                conf.join("config.toml"),
                format!("url = \"{}\"\nuser = \"alice\"\n", sb.fake.url),
            )
            .unwrap();
            std::fs::write(
                sb.secret_file(),
                json!({"u":"alice","t":TOKEN,"s":SALT}).to_string(),
            )
            .unwrap();
        }
        sb
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn run_dir(&self) -> PathBuf {
        self.dir.path().join("run")
    }

    fn secret_file(&self) -> PathBuf {
        self.dir.path().join("secret.json")
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_melodie"));
        let h = self.home();
        c.env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &h)
            .env("XDG_CONFIG_HOME", h.join(".config"))
            .env("XDG_CACHE_HOME", h.join(".cache"))
            .env("XDG_DATA_HOME", h.join(".local/share"))
            .env("XDG_STATE_HOME", h.join(".local/state"))
            .env("XDG_RUNTIME_DIR", self.run_dir())
            .env("MELODIE_TEST_SECRET", self.secret_file())
            .env("MELODIE_AO", "null")
            .env("MELODIE_MPRIS_NAME", &self.bus_name)
            .args(args);
        if let Some(a) = &self.bus_addr {
            c.env("DBUS_SESSION_BUS_ADDRESS", a);
        }
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let o = self.run(args);
        assert!(
            o.status.success(),
            "{args:?} a échoué : {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).to_string()
    }

    /// État du démon, ou `None` s'il ne répond pas (encore, ou plus) : pour les attentes.
    fn try_now(&self) -> Option<Value> {
        let o = self.run(&["now", "--json"]);
        if !o.status.success() {
            return None;
        }
        serde_json::from_slice(&o.stdout).ok()
    }

    fn now(&self) -> Value {
        serde_json::from_str(&self.ok(&["now", "--json"])).unwrap()
    }

    fn socket(&self) -> PathBuf {
        self.run_dir().join("melodie/melodie.sock")
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.home().join(".local/state/melodie/melodie.log"))
            .unwrap_or_default()
    }

    fn db(&self) -> PathBuf {
        self.home().join(".cache/melodie/library.db")
    }

    fn calls(&self) -> Vec<String> {
        self.fake.state.calls.lock().unwrap().clone()
    }

    fn scrobbled(&self, id: &str, submission: bool) -> bool {
        self.calls().iter().any(|c| {
            c.starts_with("scrobble?")
                && c.contains(&format!("id={id}&"))
                && c.contains(&format!("submission={submission}"))
        })
    }
}

impl Drop for Sandbox {
    /// Ne laisse jamais un démon derrière un test qui échoue.
    fn drop(&mut self) {
        if let Ok(mut s) = UnixStream::connect(self.socket()) {
            let _ = s.write_all(b"{\"cmd\":\"quit\"}\n");
            let mut l = String::new();
            let _ = BufReader::new(&s).read_line(&mut l);
        }
        if let Some(b) = &mut self.bus {
            let _ = b.kill();
            let _ = b.wait();
        }
    }
}

/// Mandataire MPRIS sans cache de propriétés : une lecture interroge toujours
/// le démon. Avec le cache de zbus, une valeur peut rester périmée si le
/// signal `PropertiesChanged` croise le premier chargement (course sous charge).
fn mpris(conn: &Connection, name: &str) -> Proxy<'static> {
    zbus::blocking::proxy::Builder::new(conn)
        .destination(name.to_string())
        .unwrap()
        .path("/org/mpris/MediaPlayer2")
        .unwrap()
        .interface("org.mpris.MediaPlayer2.Player")
        .unwrap()
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .unwrap()
}

/// Lit une propriété MPRIS en patientant tant que le service n'est pas
/// (encore) sur le bus : il s'inscrit un peu après le démarrage du démon.
fn prop<T>(p: &Proxy<'_>, name: &str) -> T
where
    T: TryFrom<OwnedValue>,
    T::Error: Into<zbus::Error>,
{
    let end = Instant::now() + Duration::from_secs(WAIT_FLOOR_SECS);
    loop {
        match p.get_property::<T>(name) {
            Ok(v) => return v,
            Err(e) if Instant::now() >= end => panic!("propriété MPRIS {name} : {e}"),
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn metadata(p: &Proxy<'_>) -> HashMap<String, OwnedValue> {
    prop(p, "Metadata")
}

fn strings(v: &OwnedValue) -> Vec<String> {
    v.try_clone().unwrap().try_into().unwrap()
}

fn title(p: &Proxy<'_>) -> String {
    let m = metadata(p);
    m.get("xesam:title")
        .map(|v| v.try_clone().unwrap().try_into().unwrap())
        .unwrap_or_default()
}

fn status(p: &Proxy<'_>) -> String {
    prop(p, "PlaybackStatus")
}

#[test]
fn lecture_complete_mpris_scrobble_skip_et_fin() {
    let sb = Sandbox::new(true);
    sb.fake.state.songs.lock().unwrap()[0]["duration"] = json!(60);
    sb.fake
        .state
        .stream_secs_of
        .lock()
        .unwrap()
        .insert("t1".into(), 60);
    sb.ok(&["sync", "--full"]);
    assert!(sb.db().is_file());

    // play : le démon démarre, la file est chargée.
    let out = sb.ok(&["play", "album", "al1"]);
    assert!(out.contains("3 morceaux"), "{out}");
    wait_until(5, "premier morceau", || sb.now()["state"] == "playing");

    // Fichiers du démon : droits et contenu.
    let sock = sb.socket();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&sock), 0o600);
    assert_eq!(mode(sock.parent().unwrap()), 0o700);
    assert!(sock.parent().unwrap().join("melodie.lock").exists());

    // état en JSON, tel que le lanceur le lira
    let now = sb.now();
    println!("now --json : {now}");
    assert_eq!(now["title"], "Titre 1");
    assert_eq!(now["quality"], "FLAC · 24 bit / 96 kHz · 2304 kbps");
    assert_eq!(now["index"], 1);
    assert_eq!(now["queue_len"], 3);
    assert_eq!(now["duration"], 60.0);
    let text = sb.ok(&["now"]);
    assert!(text.starts_with("En lecture : Titre 1\n"), "{text}");
    assert!(text.contains("FLAC · 24 bit / 96 kHz · 2304 kbps"));

    let mut p_changes: Option<Arc<Mutex<Vec<String>>>> = None;
    let conn = sb.bus_addr.as_ref().map(|a| {
        zbus::blocking::connection::Builder::address(a.as_str())
            .unwrap()
            .build()
            .unwrap()
    });
    let proxy = conn.as_ref().map(|c| {
        let name = format!("org.mpris.MediaPlayer2.{}", sb.bus_name);
        let p = mpris(c, &name);
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let props = zbus::blocking::fdo::PropertiesProxy::builder(c)
            .destination(name)
            .unwrap()
            .path("/org/mpris/MediaPlayer2")
            .unwrap()
            .build()
            .unwrap();
        let sink = seen.clone();
        std::thread::spawn(move || {
            for sig in props.receive_properties_changed().unwrap() {
                let args = sig.args().unwrap();
                let mut s = sink.lock().unwrap();
                for k in args.changed_properties().keys() {
                    s.push(k.to_string());
                }
            }
        });
        p_changes = Some(seen);
        p
    });
    if proxy.is_none() {
        eprintln!("NOTE : dbus-daemon absent, vérifications MPRIS sautées");
    }

    if let Some(p) = &proxy {
        wait_until(5, "MPRIS", || title(p) == "Titre 1");
        let m = metadata(p);
        assert_eq!(status(p), "Playing");
        let id: OwnedObjectPath = m["mpris:trackid"].try_clone().unwrap().try_into().unwrap();
        assert!(id.as_str().starts_with("/org/melodie/track/"));
        assert_eq!(
            m["mpris:length"].try_clone().unwrap().try_into(),
            Ok(60_000_000i64)
        );
        assert_eq!(strings(&m["xesam:artist"]), ["Beyoncé"]);
        assert_eq!(strings(&m["xesam:albumArtist"]), ["Beyoncé"]);
        let album: String = m["xesam:album"].try_clone().unwrap().try_into().unwrap();
        assert_eq!(album, "Discovery");
        let q = "FLAC · 24 bit / 96 kHz · 2304 kbps";
        assert_eq!(strings(&m["xesam:comment"]), [q]);
        let pq: String = m["melodie:quality"]
            .try_clone()
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(pq, q);
        assert!(!m.contains_key("xesam:url"));
        for (k, v) in &m {
            let t = format!("{k}={v:?}");
            assert!(!t.contains("stream.view") && !t.contains(TOKEN), "{t}");
        }
        // pochette locale téléchargée (en arrière-plan) puis annoncée
        wait_until(5, "pochette", || metadata(p).contains_key("mpris:artUrl"));
        let art: String = metadata(p)["mpris:artUrl"]
            .try_clone()
            .unwrap()
            .try_into()
            .unwrap();
        let file = art.strip_prefix("file://").expect("file://");
        let png = std::fs::read(file).unwrap();
        assert_eq!(&png[1..4], b"PNG");
        assert!(file.ends_with("/.cache/melodie/covers/al-al1.img"));
        let ident: String = Proxy::new(
            conn.as_ref().unwrap(),
            format!("org.mpris.MediaPlayer2.{}", sb.bus_name),
            "/org/mpris/MediaPlayer2",
            "org.mpris.MediaPlayer2",
        )
        .unwrap()
        .get_property("Identity")
        .unwrap();
        assert_eq!(ident, "Mélodie");
        let vol: f64 = p.get_property("Volume").unwrap();
        assert!((vol - 1.0).abs() < 1e-9, "volume par défaut : {vol}");
        p.set_property("Volume", 0.8f64).unwrap();
        wait_until(3, "volume", || {
            (p.get_property::<f64>("Volume").unwrap() - 0.8).abs() < 1e-9
        });
    }

    // next tout de suite : morceau passé
    // Le premier morceau est long (durée annoncée et flux) : sous charge, la
    // lecture peut dépasser la moitié d'un morceau de 3 s avant ce `next`.
    // Les suivants durent 3 s, flux compris, pour que la file se termine.
    sb.ok(&["next"]);
    wait_until(5, "titre 2", || sb.now()["title"] == "Titre 2");
    // Le saut est écrit dans la copie un peu après le changement de titre.
    let skips = || -> Vec<String> {
        let c = rusqlite::Connection::open(sb.db()).unwrap();
        let mut st = c.prepare("SELECT id FROM skips").unwrap();
        let rows = st.query_map([], |r| r.get(0)).unwrap();
        rows.map(|r| r.unwrap()).collect()
    };
    wait_until(5, "saut enregistré", || !skips().is_empty());
    assert_eq!(skips(), ["t1"]);
    if let Some(p) = &proxy {
        wait_until(3, "MPRIS titre 2", || title(p) == "Titre 2");
        let m = metadata(p);
        let q: String = m["melodie:quality"]
            .try_clone()
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(q, "MP3 · 320 kbps");
        assert!(
            p_changes
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .contains(&"Metadata".to_string())
        );
    }

    // pause par MPRIS, reprise par la commande
    if let Some(p) = &proxy {
        p.call_method("PlayPause", &()).unwrap();
        wait_until(3, "pause", || status(p) == "Paused");
        assert_eq!(sb.now()["state"], "paused");
        sb.ok(&["resume"]);
        wait_until(3, "reprise", || status(p) == "Playing");
        assert!(
            p_changes
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .contains(&"PlaybackStatus".to_string())
        );
    } else {
        sb.ok(&["pause"]);
        wait_until(3, "pause", || sb.now()["state"] == "paused");
        sb.ok(&["resume"]);
        wait_until(3, "reprise", || sb.now()["state"] == "playing");
    }
    sb.ok(&["toggle"]);
    assert_eq!(sb.now()["state"], "paused");
    sb.ok(&["toggle"]);
    assert_eq!(sb.now()["state"], "playing");

    // t2 joue au-delà de 50 % : écoute signalée ; t3 suit
    wait_until(8, "scrobble t2", || sb.scrobbled("t2", true));
    assert!(sb.scrobbled("t1", false) && sb.scrobbled("t2", false));
    assert!(
        !sb.scrobbled("t1", true),
        "un morceau passé n'est pas signalé"
    );
    wait_until(8, "titre 3", || sb.now()["title"] == "Titre 3");
    wait_until(8, "scrobble t3", || sb.scrobbled("t3", true));
    assert!(
        sb.calls()
            .iter()
            .filter(|c| c.starts_with("stream?"))
            .count()
            >= 3
    );

    // fin de la file : le démon s'arrête, le service MPRIS et le socket disparaissent
    wait_until(15, "fin du démon", || !sb.socket().exists());
    if let Some(c) = &conn {
        let dbus = zbus::blocking::fdo::DBusProxy::new(c).unwrap();
        let name = format!("org.mpris.MediaPlayer2.{}", sb.bus_name);
        wait_until(5, "service MPRIS retiré", || {
            !dbus
                .name_has_owner(name.as_str().try_into().unwrap())
                .unwrap()
        });
    }
    assert_eq!(sb.now()["state"], "stopped");
    let out = sb.run(&["toggle"]);
    assert!(!out.status.success());

    // journal : écrasé à chaque démarrage, sans jeton
    let log = sb.log();
    assert!(
        log.contains("démarrage du démon") && log.contains("arrêt du démon"),
        "{log}"
    );
    assert!(
        !log.contains(TOKEN) && !log.contains(SALT) && !log.contains("stream.view"),
        "{log}"
    );
}

#[test]
fn stop_puis_inactivite_arrete_le_demon() {
    let sb = Sandbox::new(true);
    let mut c = sb.cmd(&["play", "track", "t2"]);
    c.env("MELODIE_IDLE_SECS", "2");
    let o = c.output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    wait_until(5, "lecture", || {
        sb.try_now().is_some_and(|v| v["state"] == "playing")
    });
    // file ajoutée à la suite
    sb.ok(&["queue", "track", "t3"]);
    assert_eq!(sb.now()["queue_len"], 2);
    sb.ok(&["stop"]);
    assert_eq!(sb.now()["state"], "stopped");
    assert_eq!(sb.now()["queue_len"], 0);
    wait_until(10, "arrêt après inactivité", || !sb.socket().exists());
}

#[test]
fn jeton_refuse_pendant_la_lecture() {
    let sb = Sandbox::new(true);
    sb.ok(&["play", "album", "al1"]);
    wait_until(5, "lecture", || sb.now()["state"] == "playing");
    // le serveur refuse désormais le jeton : le prochain flux échoue
    *sb.fake.state.expected_token.lock().unwrap() = "autre".into();
    wait_until(20, "arrêt sur refus", || !sb.socket().exists());
    assert!(sb.home().join(".cache/melodie/auth-refused").exists());
    assert!(
        sb.log().contains("authentification refusée"),
        "{}",
        sb.log()
    );
}

#[test]
fn pas_de_demarrage_sans_connexion_ni_serveur() {
    // pas connecté
    let sb = Sandbox::new(false);
    let o = sb.run(&["play", "album", "al1"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("melodie login"));
    assert!(!sb.socket().exists());
    let o = sb.run(&["queue", "album", "al1"]);
    assert_eq!(o.status.code(), Some(1));

    // jeton refusé
    let sb = Sandbox::new(true);
    *sb.fake.state.expected_token.lock().unwrap() = "autre".into();
    let o = sb.run(&["play", "album", "al1"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(sb.home().join(".cache/melodie/auth-refused").exists());
    assert!(!sb.socket().exists());

    // serveur injoignable
    let sb = Sandbox::new(true);
    *sb.fake.state.raw.lock().unwrap() = Some((503, "x".into()));
    let o = sb.run(&["play", "album", "al1"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("injoignable"));
    assert!(!sb.socket().exists());
    assert!(!sb.home().join(".cache/melodie/auth-refused").exists());

    // sans démon : état arrêté, contrôles refusés
    assert_eq!(sb.now()["state"], "stopped");
    for c in ["toggle", "pause", "resume", "next", "prev", "stop"] {
        assert_eq!(sb.run(&[c]).status.code(), Some(1), "{c}");
    }
}

fn query_count(sb: &Sandbox, method: &str, needle: &str) -> usize {
    sb.calls()
        .iter()
        .filter(|c| c.starts_with(&format!("{method}?")) && c.contains(needle))
        .count()
}

fn db_value<T: rusqlite::types::FromSql>(sb: &Sandbox, sql: &str) -> T {
    rusqlite::Connection::open(sb.db())
        .unwrap()
        .query_row(sql, [], |r| r.get(0))
        .unwrap()
}

/// Seeds the fake server's playlist `p1` with these song ids.
fn seed_playlist(sb: &Sandbox, ids: &[&str]) {
    let songs = sb.fake.state.songs.lock().unwrap().clone();
    let list = ids
        .iter()
        .map(|id| songs.iter().find(|s| s["id"] == *id).unwrap().clone())
        .collect();
    sb.fake
        .state
        .playlists
        .lock()
        .unwrap()
        .insert("p1".into(), list);
}

fn server_playlist(sb: &Sandbox) -> Vec<String> {
    sb.fake.state.playlists.lock().unwrap()["p1"]
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn playlist_edits_work_on_a_copy_made_before_the_library_browsing() {
    let sb = Sandbox::new(true);
    sb.ok(&["sync", "--full"]);
    seed_playlist(&sb, &["t1"]);
    // The copy of an older version: no `playlist_tracks`, no `albums.created`.
    let c = rusqlite::Connection::open(sb.db()).unwrap();
    c.execute_batch(
        "DROP TABLE playlist_tracks; DROP TABLE playlist_synced; ALTER TABLE albums DROP COLUMN created;",
    )
    .unwrap();
    drop(c);

    // The server applies the edit; the command must not fail afterwards on the local copy.
    sb.ok(&["playlist", "add", "p1", "track", "t2"]);
    assert_eq!(server_playlist(&sb), ["t1", "t2"]);
    sb.ok(&["playlist", "create", "Ma liste", "track", "t1"]);
    sb.ok(&["playlist", "rename", "p9", "Autre", "nom"]);
    sb.ok(&["playlist", "delete", "p9"]);
    assert_eq!(
        db_value::<i64>(&sb, "SELECT count(*) FROM playlists WHERE id = 'p9'"),
        0
    );
}

#[test]
fn playlist_remove_rank_1_removes_the_first_track() {
    let sb = Sandbox::new(true);
    sb.ok(&["sync", "--full"]);
    seed_playlist(&sb, &["t1", "t2", "t3"]);

    sb.ok(&["playlist", "remove", "p1", "1"]);
    assert_eq!(query_count(&sb, "updatePlaylist", "songIndexToRemove=0"), 1);
    assert_eq!(server_playlist(&sb), ["t2", "t3"]);
}

#[test]
fn playlist_remove_rank_0_is_refused() {
    let sb = Sandbox::new(true);
    sb.ok(&["sync", "--full"]);
    seed_playlist(&sb, &["t1", "t2", "t3"]);

    let o = sb.run(&["playlist", "remove", "p1", "0"]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("counted from 1"), "{err}");
    assert_eq!(sb.fake.calls_of("updatePlaylist"), 0);
    assert_eq!(server_playlist(&sb), ["t1", "t2", "t3"]);
}

#[test]
fn playlist_remove_rank_past_the_end_is_refused() {
    let sb = Sandbox::new(true);
    sb.ok(&["sync", "--full"]);
    seed_playlist(&sb, &["t1", "t2", "t3"]);

    let o = sb.run(&["playlist", "remove", "p1", "4"]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("rank 4") && err.contains("3 tracks"), "{err}");
    assert_eq!(sb.fake.calls_of("updatePlaylist"), 0);
    assert_eq!(server_playlist(&sb), ["t1", "t2", "t3"]);
}

#[test]
fn playlist_remove_takes_the_rank_printed_by_duplicates() {
    let sb = Sandbox::new(true);
    sb.ok(&["sync", "--full"]);
    seed_playlist(&sb, &["t1", "t2", "t3", "t2"]);

    let out = sb.ok(&["playlist", "duplicates", "p1"]);
    let rank: String = out
        .split("rang ")
        .nth(1)
        .unwrap()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    assert_eq!(rank, "4", "{out}");

    sb.ok(&["playlist", "remove", "p1", &rank]);
    assert_eq!(server_playlist(&sb), ["t1", "t2", "t3"]);
}

#[test]
fn playlist_duplicates_from_the_command_line() {
    let sb = Sandbox::new(true);
    sb.ok(&["sync", "--full"]);
    seed_playlist(&sb, &["t1"]);
    let writes = || sb.fake.calls_of("updatePlaylist");

    // A track already there is skipped and said so; nothing is written.
    let out = sb.ok(&["playlist", "add", "p1", "track", "t1"]);
    assert!(out.contains("Déjà dans « Daft Mix »"), "{out}");
    assert_eq!(writes(), 0);

    // An album with one of its three tracks present: two added, one reported, as JSON too.
    let out = sb.ok(&["playlist", "add", "p1", "album", "al1", "--json"]);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["playlist"], json!({"id": "p1", "name": "Daft Mix"}));
    let ids = |k: &str| -> Vec<String> {
        v[k].as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(ids("added"), ["t2", "t3"]);
    assert_eq!(ids("already"), ["t1"]);
    assert_eq!(v["added"][0]["title"], "Titre 2");
    assert_eq!(v["allowDuplicates"], false);
    assert_eq!(server_playlist(&sb), ["t1", "t2", "t3"]);

    // --allow-duplicates adds it anyway.
    let out = sb.ok(&["playlist", "add", "p1", "track", "t1", "--allow-duplicates"]);
    assert!(out.contains("Ajouté à « Daft Mix »"), "{out}");
    assert_eq!(server_playlist(&sb), ["t1", "t2", "t3", "t1"]);

    // The duplicates command lists, then --remove keeps the first occurrence and the order.
    let out = sb.ok(&["playlist", "duplicates", "p1"]);
    assert!(out.contains("rang 4 : Titre 1 (comme le rang 1)"), "{out}");
    assert_eq!(server_playlist(&sb).len(), 4, "listing writes nothing");
    let out = sb.ok(&["playlist", "duplicates", "p1", "--remove", "--json"]);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["total"], 4);
    assert_eq!(v["removed"][0]["index"], 3);
    assert_eq!(v["removed"][0]["first"], 0);
    assert_eq!(server_playlist(&sb), ["t1", "t2", "t3"]);
    let out = sb.ok(&["playlist", "duplicates", "p1"]);
    assert!(out.contains("Aucun doublon"), "{out}");
}

#[test]
fn playlist_duplicates_text_counts_ranks_from_one() {
    let sb = Sandbox::new(true);
    sb.ok(&["sync", "--full"]);
    seed_playlist(&sb, &["t1", "t1"]);

    // The text counts ranks from 1; the JSON keeps the 0-based index and first.
    let out = sb.ok(&["playlist", "duplicates", "p1"]);
    assert!(out.contains("rang 2 : Titre 1 (comme le rang 1)"), "{out}");
    let out = sb.ok(&["playlist", "duplicates", "p1", "--json"]);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["exact"][0]["index"], 1);
    assert_eq!(v["exact"][0]["first"], 0);
}

#[test]
fn playlist_add_dry_run_says_what_would_go_and_writes_nothing() {
    let sb = Sandbox::new(true);
    sb.ok(&["sync", "--full"]);
    seed_playlist(&sb, &["t1"]);
    let out = sb.ok(&[
        "playlist",
        "add",
        "--dry-run",
        "--json",
        "--",
        "p1",
        "track",
        "t1",
        "t2",
    ]);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["added"][0]["id"], "t2");
    assert_eq!(v["already"][0]["id"], "t1");
    assert_eq!(v["dryRun"], true);
    assert_eq!(v["playlist"]["name"], "Daft Mix");
    assert_eq!(sb.fake.calls_of("updatePlaylist"), 0);
    assert_eq!(server_playlist(&sb), ["t1"]);
}

#[test]
fn playlist_add_takes_several_tracks() {
    let sb = Sandbox::new(true);
    sb.ok(&["sync", "--full"]);
    seed_playlist(&sb, &["t1"]);
    let out = sb.ok(&[
        "playlist", "add", "--json", "--", "p1", "track", "t1", "t2", "t3",
    ]);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert!(
        v.get("dryRun").is_none(),
        "the output of a real add is unchanged"
    );
    assert_eq!(server_playlist(&sb), ["t1", "t2", "t3"]);
    sb.ok(&[
        "playlist",
        "add",
        "--allow-duplicates",
        "--",
        "p1",
        "track",
        "t1",
        "t2",
    ]);
    assert_eq!(server_playlist(&sb), ["t1", "t2", "t3", "t1", "t2"]);
}

#[test]
fn playlist_remove_takes_several_ranks_in_one_call() {
    let sb = Sandbox::new(true);
    sb.ok(&["sync", "--full"]);
    seed_playlist(&sb, &["t1", "t2", "t3"]);
    sb.ok(&[
        "playlist", "remove", "--expect", "t1", "--expect", "t3", "--", "p1", "1", "3",
    ]);
    assert_eq!(sb.fake.calls_of("updatePlaylist"), 1);
    assert_eq!(server_playlist(&sb), ["t2"]);
}

#[test]
fn playlist_remove_refuses_a_rank_holding_another_track() {
    let sb = Sandbox::new(true);
    sb.ok(&["sync", "--full"]);
    seed_playlist(&sb, &["t1", "t2", "t3"]);
    let o = sb.run(&["playlist", "remove", "--expect", "t3", "--", "p1", "2"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("rank 2"));
    // one id per rank, or none
    let o = sb.run(&["playlist", "remove", "--expect", "t1", "--", "p1", "1", "2"]);
    assert!(!o.status.success());
    assert_eq!(sb.fake.calls_of("updatePlaylist"), 0);
    assert_eq!(server_playlist(&sb), ["t1", "t2", "t3"]);
}

#[test]
fn playlist_move_puts_a_track_at_another_rank() {
    let sb = Sandbox::new(true);
    sb.ok(&["sync", "--full"]);
    seed_playlist(&sb, &["t1", "t2", "t3"]);
    sb.ok(&["playlist", "move", "--expect", "t3", "--", "p1", "3", "1"]);
    assert_eq!(server_playlist(&sb), ["t3", "t1", "t2"]);
    let o = sb.run(&["playlist", "move", "--expect", "t1", "--", "p1", "3", "1"]);
    assert!(!o.status.success(), "rank 3 holds t2 now");
    let o = sb.run(&["playlist", "move", "--", "p1", "0", "1"]);
    assert!(!o.status.success(), "ranks are counted from 1");
    assert_eq!(server_playlist(&sb), ["t3", "t1", "t2"]);
}

#[test]
fn playlist_json_failure_is_one_launcher_entry() {
    let sb = Sandbox::new(false);
    for args in [
        &["playlist", "add", "p1", "track", "t1", "--json"][..],
        &["playlist", "duplicates", "p1", "--json"][..],
    ] {
        let o = sb.run(args);
        assert_eq!(o.status.code(), Some(0), "{args:?}");
        let v: Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1);
        assert_eq!(v[0]["id"], "login");
    }
}

#[test]
fn favoris_playlists_et_mixes_de_bout_en_bout() {
    let sb = Sandbox::new(true);
    sb.ok(&["sync", "--full"]);
    let starred = |id: &str| -> bool {
        db_value(
            &sb,
            &format!("SELECT starred IS NOT NULL FROM tracks WHERE id = '{id}'"),
        )
    };

    // Favoris explicites : requête reçue, copie locale mise à jour sans resynchronisation.
    let before = sb.calls().len();
    let out = sb.ok(&["like", "track", "t2"]);
    assert!(out.contains("♥ Ajouté aux favoris : Titre 2"), "{out}");
    assert!(starred("t2"));
    assert_eq!(query_count(&sb, "star", "&id=t2"), 1);
    assert_eq!(
        sb.calls().len(),
        before + 1,
        "un seul appel, pas de synchronisation"
    );
    sb.ok(&["unlike", "track", "t2"]);
    assert!(!starred("t2"));
    assert_eq!(query_count(&sb, "unstar", "&id=t2"), 1);
    sb.ok(&["like", "--toggle", "track", "t2"]);
    assert!(starred("t2"));
    sb.ok(&["like", "--toggle", "track", "t2"]);
    assert!(!starred("t2"));
    sb.ok(&["like", "album", "al1"]);
    sb.ok(&["like", "artist", "ar1"]);
    assert_eq!(query_count(&sb, "star", "&albumId=al1"), 1);
    assert_eq!(query_count(&sb, "star", "&artistId=ar1"), 1);
    assert!(db_value::<bool>(
        &sb,
        "SELECT starred IS NOT NULL FROM albums WHERE id = 'al1'"
    ));
    // Sans argument et sans lecture : « Rien en lecture », code 1, aucun appel au serveur.
    let n = sb.calls().len();
    let o = sb.run(&["like"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("Rien en lecture"));
    assert_eq!(sb.calls().len(), n);
    // kind sans id : refusé par la ligne de commande.
    assert!(!sb.run(&["like", "track"]).status.success());

    // Playlists.
    let count = || db_value::<i64>(&sb, "SELECT songCount FROM playlists WHERE id = 'p1'");
    assert_eq!(count(), 42);
    sb.ok(&["playlist", "add", "p1", "track", "t3", "--allow-duplicates"]);
    assert_eq!(query_count(&sb, "updatePlaylist", "songIdToAdd=t3"), 1);
    assert_eq!(count(), 43);
    sb.ok(&[
        "playlist",
        "add",
        "p1",
        "album",
        "al1",
        "--allow-duplicates",
    ]);
    let album_add = sb
        .calls()
        .into_iter()
        .rev()
        .find(|c| c.starts_with("updatePlaylist?"))
        .unwrap();
    for id in ["t1", "t2", "t3"] {
        assert!(
            album_add.contains(&format!("songIdToAdd={id}")),
            "{album_add}"
        );
    }
    assert_eq!(count(), 46);
    sb.ok(&["playlist", "remove", "p1", "1"]);
    assert_eq!(query_count(&sb, "updatePlaylist", "songIndexToRemove=0"), 1);
    assert_eq!(count(), 45);
    let o = sb.run(&["playlist", "remove", "p1", "99"]);
    assert!(!o.status.success());
    assert_eq!(count(), 45);
    sb.ok(&["playlist", "create", "Ma liste", "track", "t1"]);
    assert_eq!(query_count(&sb, "createPlaylist", "songId=t1"), 1);
    assert_eq!(
        db_value::<String>(&sb, "SELECT name FROM playlists WHERE id = 'p9'"),
        "Ma liste"
    );
    sb.ok(&["playlist", "rename", "p9", "Autre", "nom"]);
    assert_eq!(
        db_value::<String>(&sb, "SELECT name FROM playlists WHERE id = 'p9'"),
        "Autre nom"
    );
    sb.ok(&["playlist", "delete", "p9"]);
    assert_eq!(
        db_value::<i64>(&sb, "SELECT count(*) FROM playlists WHERE id = 'p9'"),
        0
    );
    // Sous-menu du lanceur.
    let pick: Value =
        serde_json::from_str(&sb.ok(&["playlist", "pick", "--json", "track", "t1"])).unwrap();
    assert_eq!(pick[0]["name"], "Nouvelle playlist…");
    assert_eq!(
        pick[0]["action"]["command"],
        "xdg-terminal-exec melodie playlist create --prompt track t1"
    );
    assert_eq!(
        pick[1]["action"]["command"],
        "melodie playlist add p1 track t1"
    );

    // Mixes locaux.
    let mixes: Value = serde_json::from_str(&sb.ok(&["mixes", "--json"])).unwrap();
    assert_eq!(mixes[0]["name"], "Mix perso");
    assert_eq!(mixes[0]["icon"], "󰒝");
    assert_eq!(mixes[0]["action"]["command"], "melodie mix me");
    assert_eq!(mixes[1]["name"], "Redécouvertes");
    assert_eq!(mixes[1]["icon"], "󰄉");
    let streams = sb
        .calls()
        .iter()
        .filter(|c| c.starts_with("stream?"))
        .count();
    let printed = sb.ok(&["mix", "me", "--print", "--seed", "1"]);
    assert_eq!(printed.lines().count(), 3, "{printed}");
    assert_eq!(
        printed,
        sb.ok(&["mix", "me", "--print", "--seed", "1"]),
        "même graine, même mix"
    );
    let list: Value =
        serde_json::from_str(&sb.ok(&["mix", "me", "--json", "--seed", "1"])).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 3);
    assert!(
        list[0]["action"]["command"]
            .as_str()
            .unwrap()
            .starts_with("melodie play track t")
    );
    assert_eq!(
        sb.calls()
            .iter()
            .filter(|c| c.starts_with("stream?"))
            .count(),
        streams
    );
    assert!(
        !sb.socket().exists(),
        "--print et --json ne lancent pas le démon"
    );
    assert!(
        !sb.run(&["mix", "genre", "Disco", "--print"])
            .status
            .success()
    );

    // Lecture d'un mix, favori du morceau en cours (now, MPRIS), ajout à une playlist.
    let n_api = sb
        .calls()
        .iter()
        .filter(|c| c.starts_with("getSong?") || c.starts_with("getAlbum?"))
        .count();
    let out = sb.ok(&["mix", "me", "--seed", "1"]);
    assert!(out.contains("3 morceaux"), "{out}");
    wait_until(5, "lecture du mix", || sb.now()["state"] == "playing");
    assert_eq!(
        sb.calls()
            .iter()
            .filter(|c| c.starts_with("getSong?") || c.starts_with("getAlbum?"))
            .count(),
        n_api,
        "les morceaux du mix sont résolus localement"
    );
    let cur = sb.now();
    let id = cur["id"].as_str().unwrap().to_string();
    assert_eq!(cur["starred"], false);
    let conn = sb.bus_addr.as_ref().map(|a| {
        zbus::blocking::connection::Builder::address(a.as_str())
            .unwrap()
            .build()
            .unwrap()
    });
    let proxy = conn
        .as_ref()
        .map(|c| mpris(c, &format!("org.mpris.MediaPlayer2.{}", sb.bus_name)));
    let mpris_starred = |p: &Proxy<'_>| -> bool {
        metadata(p)["melodie:starred"]
            .try_clone()
            .unwrap()
            .try_into()
            .unwrap()
    };
    if let Some(p) = &proxy {
        wait_until(5, "MPRIS", || metadata(p).contains_key("melodie:starred"));
        assert!(!mpris_starred(p));
    }
    let out = sb.ok(&["like"]);
    assert!(out.contains("Ajouté aux favoris"), "{out}");
    assert_eq!(sb.now()["starred"], true);
    assert_eq!(query_count(&sb, "star", &format!("&id={id}")), 1);
    assert!(db_value::<bool>(
        &sb,
        &format!("SELECT starred IS NOT NULL FROM tracks WHERE id = '{id}'")
    ));
    if let Some(p) = &proxy {
        wait_until(3, "melodie:starred", || mpris_starred(p));
    }
    sb.ok(&["like", "--toggle"]);
    assert_eq!(sb.now()["starred"], false);
    if let Some(p) = &proxy {
        wait_until(3, "melodie:starred retiré", || !mpris_starred(p));
    }
    let adds = query_count(&sb, "updatePlaylist", &format!("songIdToAdd={id}"));
    sb.ok(&["playlist", "add", "p1", "--allow-duplicates"]);
    assert_eq!(
        query_count(&sb, "updatePlaylist", &format!("songIdToAdd={id}")),
        adds + 1
    );
    let searched: Value = serde_json::from_str(&sb.ok(&["search", "--json", "Titre"])).unwrap();
    let labels: Vec<&str> = searched
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|e| e["actions"].as_array().unwrap())
        .map(|a| a["label"].as_str().unwrap())
        .collect();
    assert!(labels.contains(&"J'aime") && labels.contains(&"Ajouter à une playlist…"));
    // Mix instantané à la suite de la file.
    let len = sb.now()["queue_len"].as_u64().unwrap();
    sb.ok(&["mix", "album", "al1", "--queue", "--seed", "2"]);
    assert!(sb.now()["queue_len"].as_u64().unwrap() > len);
    sb.ok(&["stop"]);
    assert!(!sb.log().contains(TOKEN));
}

#[test]
fn favoris_et_mixes_selon_l_etat_de_connexion() {
    // Pas connecté.
    let sb = Sandbox::new(false);
    for args in [
        &["like", "track", "t1"][..],
        &["playlist", "add", "p1", "track", "t1"],
        &["playlist", "create", "x"],
        &["playlist", "delete", "p1"],
    ] {
        let o = sb.run(args);
        assert_eq!(o.status.code(), Some(1), "{args:?}");
        assert!(
            String::from_utf8_lossy(&o.stderr).contains("melodie login"),
            "{args:?}"
        );
    }
    for args in [
        &["mixes", "--json"][..],
        &["mix", "me", "--json"],
        &["playlist", "pick", "--json"],
    ] {
        let o = sb.run(args);
        assert_eq!(o.status.code(), Some(0), "{args:?}");
        let v: Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1);
        assert_eq!(v[0]["id"], "login");
    }
    assert_eq!(sb.run(&["mix", "me"]).status.code(), Some(1));

    // Jeton refusé : une seule entrée en JSON, code 1 sinon, marqueur posé.
    let sb = Sandbox::new(true);
    sb.ok(&["sync", "--full"]);
    *sb.fake.state.expected_token.lock().unwrap() = "autre".into();
    let o = sb.run(&["like", "track", "t1"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(sb.home().join(".cache/melodie/auth-refused").exists());
    assert!(!db_value::<bool>(
        &sb,
        "SELECT starred IS NOT NULL FROM tracks WHERE id = 't1'"
    ));
    let o = sb.run(&["mixes", "--json"]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(
        serde_json::from_slice::<Value>(&o.stdout).unwrap()[0]["id"],
        "login"
    );

    // Serveur injoignable : code 2, copie inchangée, pas de demande de reconnexion.
    let sb = Sandbox::new(true);
    sb.ok(&["sync", "--full"]);
    *sb.fake.state.raw.lock().unwrap() = Some((503, "x".into()));
    let o = sb.run(&["like", "track", "t1"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&o.stderr).contains("melodie login"));
    let o = sb.run(&["playlist", "add", "p1", "track", "t1"]);
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(
        db_value::<i64>(&sb, "SELECT songCount FROM playlists WHERE id = 'p1'"),
        42
    );
    // Les mixes ne dépendent que de la copie : ils marchent hors ligne ; jouer demande le serveur.
    assert_eq!(
        sb.ok(&["mix", "me", "--print", "--seed", "1"])
            .lines()
            .count(),
        3
    );
    let o = sb.run(&["mix", "me", "--seed", "1"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(!sb.socket().exists());
    assert!(!sb.home().join(".cache/melodie/auth-refused").exists());
}

// ---- login with the password on stdin (the window's way) ----

const PASSWORD: &str = "correct horse battery";

/// `melodie login --password-stdin`, the password written on its standard input.
fn login_stdin(sb: &Sandbox, server: &str, password: &str) -> Output {
    let mut child = sb
        .cmd(&[
            "login",
            "--password-stdin",
            &format!("--server={server}"),
            "--user=alice",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{password}\n").as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

/// Every file under `dir` holding `needle`.
fn files_holding(dir: &Path, needle: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            found.extend(files_holding(&p, needle));
        } else if std::fs::read(&p).is_ok_and(|b| String::from_utf8_lossy(&b).contains(needle)) {
            found.push(p);
        }
    }
    found
}

#[test]
fn login_with_the_password_on_stdin_keeps_the_token_and_forgets_the_password() {
    let sb = Sandbox::new(false);
    *sb.fake.state.password.lock().unwrap() = Some(PASSWORD.into());
    let o = login_stdin(&sb, &sb.fake.url, PASSWORD);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let secret: Value = serde_json::from_slice(&std::fs::read(sb.secret_file()).unwrap()).unwrap();
    assert_eq!(secret["u"], "alice");
    let salt = secret["s"].as_str().unwrap();
    assert_eq!(
        secret["t"].as_str().unwrap(),
        melodie_core::api::token(PASSWORD, salt)
    );
    let conf = std::fs::read_to_string(sb.home().join(".config/melodie/config.toml")).unwrap();
    assert!(
        conf.contains(&sb.fake.url) && conf.contains("alice"),
        "{conf}"
    );
    // the first copy starts on its own, as after an interactive login
    wait_until(30, "the first copy", || {
        rusqlite::Connection::open_with_flags(sb.db(), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .and_then(|c| {
                c.query_row("SELECT value FROM meta WHERE key='synced_at'", [], |r| {
                    r.get::<_, String>(0)
                })
            })
            .is_ok()
    });
    assert!(files_holding(sb.dir.path(), PASSWORD).is_empty());
    for out in [&o.stdout, &o.stderr] {
        assert!(!String::from_utf8_lossy(out).contains(PASSWORD));
    }
}

#[test]
fn login_with_a_wrong_password_on_stdin_is_refused_and_keeps_nothing() {
    let sb = Sandbox::new(false);
    *sb.fake.state.password.lock().unwrap() = Some(PASSWORD.into());
    let o = login_stdin(&sb, &sb.fake.url, "wrong");
    assert_eq!(o.status.code(), Some(1));
    assert!(!sb.secret_file().exists());
    assert!(!sb.home().join(".config/melodie/config.toml").exists());
    assert!(!String::from_utf8_lossy(&o.stderr).contains("wrong"));
}

#[test]
fn a_secret_that_cannot_be_saved_is_not_a_refused_password() {
    let sb = Sandbox::new(false);
    *sb.fake.state.password.lock().unwrap() = Some(PASSWORD.into());
    // the server accepts the password; the place for the secret is unusable
    std::fs::create_dir(sb.secret_file()).unwrap();
    let o = login_stdin(&sb, &sb.fake.url, PASSWORD);
    // 1 is "no login or refused", which would make the window ask for the password again
    assert_eq!(o.status.code(), Some(3));
    assert!(!sb.home().join(".config/melodie/config.toml").exists());
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(!stderr.is_empty() && !stderr.contains(PASSWORD), "{stderr}");
    // a configuration that cannot be read is local too
    let sb = Sandbox::new(false);
    std::fs::create_dir_all(sb.home().join(".config/melodie")).unwrap();
    std::fs::write(sb.home().join(".config/melodie/config.toml"), "url = [").unwrap();
    let o = login_stdin(&sb, &sb.fake.url, PASSWORD);
    assert_eq!(o.status.code(), Some(3));
}

#[test]
fn login_on_stdin_to_an_unreachable_server_says_so_and_keeps_nothing() {
    let sb = Sandbox::new(false);
    *sb.fake.state.raw.lock().unwrap() = Some((503, "x".into()));
    let o = login_stdin(&sb, &sb.fake.url, PASSWORD);
    assert_eq!(o.status.code(), Some(2));
    assert!(!sb.secret_file().exists());
    assert!(!String::from_utf8_lossy(&o.stderr).contains("melodie login"));
}

// ---- synchronisation automatique de la copie ----

fn set_synced_at(sb: &Sandbox, t: u64) {
    let conn = rusqlite::Connection::open(sb.db()).unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('synced_at', ?1)",
        [t.to_string()],
    )
    .unwrap();
}

fn synced_at(sb: &Sandbox) -> u64 {
    let conn = rusqlite::Connection::open(sb.db()).unwrap();
    conn.query_row("SELECT value FROM meta WHERE key='synced_at'", [], |r| {
        r.get::<_, String>(0)
    })
    .unwrap()
    .parse()
    .unwrap()
}

fn scans(sb: &Sandbox) -> usize {
    sb.calls()
        .iter()
        .filter(|c| c.starts_with("getScanStatus"))
        .count()
}

fn sync_log(sb: &Sandbox) -> String {
    std::fs::read_to_string(sb.home().join(".local/state/melodie/sync.log")).unwrap_or_default()
}

fn quit_daemon(sb: &Sandbox) {
    let mut s = UnixStream::connect(sb.socket()).unwrap();
    s.write_all(b"{\"cmd\":\"quit\"}\n").unwrap();
    let mut l = String::new();
    let _ = BufReader::new(&s).read_line(&mut l);
    wait_until(10, "arrêt du démon", || !sb.socket().exists());
}

/// Tient le verrou de synchronisation comme le ferait une synchronisation en cours.
fn hold_sync_lock(sb: &Sandbox) -> std::fs::File {
    use std::os::fd::AsRawFd;
    let dir = sb.run_dir().join("melodie");
    std::fs::create_dir_all(&dir).unwrap();
    let f = std::fs::File::create(dir.join("sync.lock")).unwrap();
    assert_eq!(
        unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    f
}

#[test]
fn sans_copie_entree_en_cours_puis_copie_en_fond() {
    let sb = Sandbox::new(true);
    // Le serveur retient ses réponses : si `search` l'attendait, il resterait
    // bloqué jusqu'à la fin de la retenue (20 s). La borne ne mesure donc que
    // « n'a pas attendu le serveur », avec une large marge sous charge.
    sb.fake.state.hold.store(true, Ordering::SeqCst);
    let t = Instant::now();
    let out = sb.ok(&["search", "--json", "titre"]);
    assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["name"], "Copie de la bibliothèque en cours…");
    assert_eq!(v[0]["icon"], "󰑓");
    assert!(v[0].get("action").is_none());
    // Sortie lisible : le dire, sans bloquer.
    let o = sb.run(&["playlists"]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("Copie de la bibliothèque en cours"));
    sb.fake.state.hold.store(false, Ordering::SeqCst);
    // La copie arrive toute seule.
    wait_until(10, "copie complète en fond", || sb.db().is_file());
    wait_until(10, "synchronisation terminée", || {
        sync_log(&sb).contains("terminée")
    });
    let out = sb.ok(&["search", "--json", "titre"]);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert!(v.as_array().unwrap().iter().any(|e| e["id"] == "track:t1"));
}

#[test]
fn copie_vieille_repond_tout_de_suite_et_se_met_a_jour() {
    let sb = Sandbox::new(true);
    sb.ok(&["sync", "--full"]);
    // Copie jeune : aucun appel au serveur.
    sb.fake.clear_calls();
    sb.ok(&["search", "--json", "titre"]);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(scans(&sb), 0);
    // Copie vieille : réponse immédiate avec la copie actuelle, puis sync incrémentale.
    set_synced_at(&sb, 1);
    let out = sb.ok(&["home", "--json"]);
    assert!(out.contains("playlist:") || out.contains("mix:me"), "{out}");
    wait_until(10, "synchronisation de fond", || synced_at(&sb) > 1);
    assert!(scans(&sb) >= 1);
    // Le journal s'écrit un peu après la mise à jour de la date.
    wait_until(10, "journal de la sync", || {
        sync_log(&sb).contains("incrémentale")
    });
}

#[test]
fn une_sync_a_la_fois() {
    let sb = Sandbox::new(true);
    sb.ok(&["sync", "--full"]);
    set_synced_at(&sb, 1);
    sb.fake.clear_calls();
    let lock = hold_sync_lock(&sb);
    sb.ok(&["search", "--json", "titre"]);
    sb.ok(&["mixes", "--json"]);
    let out = sb.ok(&["sync"]);
    assert!(out.contains("déjà en cours"), "{out}");
    assert!(sb.ok(&["status"]).contains("Synchro     : en cours"));
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(scans(&sb), 0);
    assert_eq!(synced_at(&sb), 1);
    drop(lock);
    sb.ok(&["search", "--json", "titre"]);
    wait_until(10, "sync une fois le verrou libre", || synced_at(&sb) > 1);
    let st = sb.ok(&["status"]);
    assert!(st.contains("Synchro     : aucune en cours"), "{st}");
    assert!(st.contains("il y a"), "{st}");
}

#[test]
fn an_unreadable_local_copy_does_not_turn_a_good_login_into_no_login() {
    let sb = Sandbox::new(true);
    std::fs::create_dir_all(sb.db().parent().unwrap()).unwrap();
    std::fs::write(sb.db(), "this is not a database").unwrap();
    let o = sb.run(&["status"]);
    let said = String::from_utf8_lossy(&o.stdout);
    // the ping's answer is the exit code: exit 1 would bring up the sign-in screen
    assert_eq!(
        o.status.code(),
        Some(0),
        "{said}{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(said.contains("Connexion   : connecté"), "{said}");
    assert!(said.contains("Copie       : illisible"), "{said}");
    assert!(said.contains("Synchro     :"), "{said}");
    // an unreachable server still wins, whatever the copy is
    *sb.fake.state.raw.lock().unwrap() = Some((503, "x".into()));
    assert_eq!(sb.run(&["status"]).status.code(), Some(2));
}

#[test]
fn refus_en_fond_pose_le_marqueur_sans_jeton() {
    let sb = Sandbox::new(true);
    sb.ok(&["sync", "--full"]);
    set_synced_at(&sb, 1);
    *sb.fake.state.fail_code.lock().unwrap() = Some(40);
    sb.ok(&["search", "--json", "titre"]);
    let marker = sb.home().join(".cache/melodie/auth-refused");
    wait_until(10, "marqueur auth-refused", || marker.exists());
    wait_until(5, "journal", || sync_log(&sb).contains("auth-refused"));
    let log = sync_log(&sb);
    assert!(!log.contains(TOKEN) && !log.contains(SALT), "{log}");
    let out = sb.ok(&["search", "--json", "titre"]);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v[0]["name"], "Se connecter à Navidrome");
}

#[test]
fn demon_synchronise_au_demarrage_puis_periodiquement() {
    let sb = Sandbox::new(true);
    sb.ok(&["sync", "--full"]);
    // Copie jeune (moins de 15 min) : rien au démarrage, ni dans la période par défaut.
    sb.fake.clear_calls();
    let play = |sb: &Sandbox, every: &str| {
        let mut c = sb.cmd(&["play", "album", "al1"]);
        c.env("MELODIE_DAEMON_EVERY_SECS", every);
        let o = c.output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        wait_until(5, "démon", || {
            sb.try_now().is_some_and(|v| v["state"] == "playing")
        });
    };
    play(&sb, "1000");
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(scans(&sb), 0);
    quit_daemon(&sb);

    // Copie vieille : synchronisation au démarrage.
    set_synced_at(&sb, 1);
    play(&sb, "1000");
    wait_until(10, "sync au démarrage", || synced_at(&sb) > 1);
    quit_daemon(&sb);

    // Copie jeune, période de 1 s : une sync périodique suit.
    sb.fake.clear_calls();
    play(&sb, "1");
    wait_until(10, "sync périodique", || scans(&sb) >= 1);
    quit_daemon(&sb);
}

fn volume_file(sb: &Sandbox) -> String {
    std::fs::read_to_string(sb.home().join(".local/state/melodie/state.toml")).unwrap_or_default()
}

#[test]
fn volume_defaut_commande_mpris_et_memoire() {
    let sb = Sandbox::new(true);
    sb.ok(&["sync", "--full"]);
    // Sans démon : valeur par défaut, et un réglage est mémorisé pour le prochain démarrage.
    assert_eq!(sb.ok(&["volume"]), "Volume : 100 %\n");
    assert_eq!(sb.ok(&["volume", "-5"]), "Volume : 95 %\n");
    assert!(volume_file(&sb).contains("volume = 95"));
    assert!(!sb.run(&["volume", "fort"]).status.success());

    // Le démon démarre au volume mémorisé.
    sb.ok(&["play", "album", "al1"]);
    wait_until(5, "démon", || {
        sb.try_now().is_some_and(|v| v["state"] == "playing")
    });
    assert!((sb.now()["volume"].as_f64().unwrap() - 0.95).abs() < 1e-9);

    // Par la commande : absolu, relatif, bornes.
    assert_eq!(sb.ok(&["volume", "30"]), "Volume : 30 %\n");
    assert_eq!(sb.ok(&["volume", "+5"]), "Volume : 35 %\n");
    assert_eq!(sb.ok(&["volume", "-90"]), "Volume : 0 %\n");
    assert_eq!(sb.ok(&["volume", "250"]), "Volume : 130 %\n");
    assert_eq!(sb.ok(&["volume", "60"]), "Volume : 60 %\n");
    assert!((sb.now()["volume"].as_f64().unwrap() - 0.6).abs() < 1e-9);
    assert_eq!(sb.ok(&["volume"]), "Volume : 60 %\n");

    // Par MPRIS (bus privé).
    if let Some(addr) = &sb.bus_addr {
        let conn = zbus::blocking::connection::Builder::address(addr.as_str())
            .unwrap()
            .build()
            .unwrap();
        let p = mpris(&conn, &format!("org.mpris.MediaPlayer2.{}", sb.bus_name));
        // Le service MPRIS s'inscrit sur le bus un peu après le démarrage du démon.
        wait_until(5, "service MPRIS", || {
            p.set_property("Volume", 0.7f64).is_ok()
        });
        wait_until(3, "volume MPRIS", || {
            sb.try_now()
                .is_some_and(|v| (v["volume"].as_f64().unwrap() - 0.7).abs() < 1e-9)
        });
    } else {
        eprintln!("dbus-daemon absent : volume MPRIS sauté");
        sb.ok(&["volume", "70"]);
    }

    // Écriture regroupée, puis restauration après redémarrage du démon.
    wait_until(5, "volume écrit", || {
        volume_file(&sb).contains("volume = 70")
    });
    quit_daemon(&sb);
    sb.ok(&["play", "album", "al1"]);
    wait_until(5, "démon relancé", || {
        sb.try_now().is_some_and(|v| v["state"] == "playing")
    });
    assert!((sb.now()["volume"].as_f64().unwrap() - 0.7).abs() < 1e-9);
}

// ---- the queue and the event stream (issue #5) ----

/// Five tracks of five minutes in album `al1`: nothing ends on its own during a test.
fn long_album(sb: &Sandbox) {
    sb.fake.state.stream_secs.store(300, Ordering::SeqCst);
    *sb.fake.state.songs.lock().unwrap() =
        (1..=5).map(|i| song(i, json!({"duration": 300}))).collect();
}

/// `melodie watch` running in the background; its lines are kept with their arrival time.
struct Watch {
    child: std::process::Child,
    lines: Arc<Mutex<Vec<(Instant, String)>>>,
}

impl Sandbox {
    fn watch(&self) -> Watch {
        let mut child = self
            .cmd(&["watch"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let out = child.stdout.take().unwrap();
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = lines.clone();
        std::thread::spawn(move || {
            for l in BufReader::new(out).lines() {
                let Ok(l) = l else { return };
                sink.lock().unwrap().push((Instant::now(), l));
            }
        });
        Watch { child, lines }
    }

    fn queue_list(&self) -> Value {
        serde_json::from_str(&self.ok(&["queue", "--list", "--json"])).unwrap()
    }

    /// One request on the socket, as a client other than the command line would send it.
    fn request(&self, req: Value) -> Value {
        let s = UnixStream::connect(self.socket()).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        (&s).write_all(format!("{req}\n").as_bytes()).unwrap();
        let mut l = String::new();
        BufReader::new(&s).read_line(&mut l).unwrap();
        serde_json::from_str(&l).unwrap()
    }

    fn play_long_album(&self) {
        long_album(self);
        self.ok(&["play", "album", "al1"]);
        wait_until(5, "lecture", || {
            self.try_now()
                .is_some_and(|v| v["state"] == "playing" && v["id"] == "t1")
        });
    }
}

impl Watch {
    fn raw(&self) -> Vec<String> {
        self.lines
            .lock()
            .unwrap()
            .iter()
            .map(|(_, l)| l.clone())
            .collect()
    }

    fn events(&self) -> Vec<Value> {
        self.raw()
            .iter()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("not JSON ({e}): {l}")))
            .collect()
    }

    fn of_type(&self, t: &str) -> Vec<Value> {
        self.events()
            .into_iter()
            .filter(|e| e["type"] == t)
            .collect()
    }

    fn wait_for(&self, what: &str, f: impl Fn(&[Value]) -> bool) {
        wait_until(5, what, || f(&self.events()));
    }

    /// The queue as a client sees it: the last snapshot, then the `queue` events after it.
    fn latest_queue(&self) -> Value {
        let events = self.events();
        let from = events.iter().rposition(|e| e["type"] == "snapshot");
        let mut queue = json!({"tracks": []});
        for e in &events[from.unwrap_or(0)..] {
            if e["type"] == "snapshot" || e["type"] == "queue" {
                queue = e["queue"].clone();
            }
        }
        queue
    }

    /// Waits for one more event of this type than `before`.
    fn wait_count(&self, t: &str, n: usize) {
        wait_until(5, &format!("{n} events {t}"), || self.of_type(t).len() >= n);
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn ids(queue: &Value) -> Vec<String> {
    queue["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap().to_string())
        .collect()
}

fn ids_of(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// No stream address, token or salt in what a client can read.
fn assert_no_secret(text: &str) {
    for bad in [TOKEN, SALT, "stream.view", "t=", "s=", "http"] {
        assert!(!text.contains(bad), "{bad:?} in {text}");
    }
}

#[test]
fn each_queue_change_is_listed_and_announced_in_order() {
    let sb = Sandbox::new(true);
    sb.play_long_album();
    let w = sb.watch();
    w.wait_count("snapshot", 1);

    // The stream starts with a snapshot of everything.
    let first = &w.events()[0];
    assert_eq!(first["type"], "snapshot", "{first}");
    assert_eq!(first["now"]["id"], "t1");
    assert_eq!(first["now"]["state"], "playing");
    assert_eq!(first["queue"]["current"], 0);
    assert_eq!(ids(&first["queue"]), ["t1", "t2", "t3", "t4", "t5"]);
    assert_eq!(first["connection"], "ok");

    // The listing, as a client shows it.
    let list = sb.queue_list();
    assert_eq!(list["current"], 0);
    let t = &list["tracks"][1];
    assert_eq!(t["id"], "t2");
    assert_eq!(t["title"], "Titre 2");
    assert_eq!(t["artist"], "Daft Punk");
    assert_eq!(t["album"], "Discovery");
    assert_eq!(t["duration"], 300.0);
    assert_eq!(t["cover_id"], "al-al1");
    assert!(t["entry"].as_u64().is_some());
    let text = sb.ok(&["queue", "--list"]);
    assert!(
        text.contains("Titre 1") && text.contains("Titre 5"),
        "{text}"
    );

    // Each change, as the command line makes it (positions from 1), then as listed.
    let steps: [(&[&str], &[&str], Option<u64>); 5] = [
        (
            &["--move", "4", "2"],
            &["t1", "t4", "t2", "t3", "t5"],
            Some(0),
        ),
        (&["--remove", "3"], &["t1", "t4", "t3", "t5"], Some(0)),
        (
            &["--next", "track", "t2"],
            &["t1", "t2", "t4", "t3", "t5"],
            Some(0),
        ),
        (
            &["track", "t2"],
            &["t1", "t2", "t4", "t3", "t5", "t2"],
            Some(0),
        ),
        (
            &["--play", "3"],
            &["t1", "t2", "t4", "t3", "t5", "t2"],
            Some(2),
        ),
    ];
    for (args, expected, current) in steps {
        let mut a = vec!["queue"];
        a.extend_from_slice(args);
        sb.ok(&a);
        let list = sb.queue_list();
        assert_eq!(ids(&list), ids_of(expected), "{args:?}");
        assert_eq!(list["current"].as_u64(), current, "{args:?}");
    }
    wait_until(5, "t4 playing", || sb.now()["id"] == "t4");
    sb.ok(&["queue", "--clear-after"]);
    let list = sb.queue_list();
    assert_eq!(ids(&list), ["t1", "t2", "t4"]);
    assert_eq!(list["current"], 2);
    assert_eq!(sb.now()["queue_len"], 3);

    // The same changes on the stream, in the same order.
    w.wait_count("queue", 5);
    let seen: Vec<Vec<String>> = w
        .of_type("queue")
        .iter()
        .map(|e| ids(&e["queue"]))
        .collect();
    assert_eq!(
        seen,
        [
            ids_of(&["t1", "t4", "t2", "t3", "t5"]),
            ids_of(&["t1", "t4", "t3", "t5"]),
            ids_of(&["t1", "t2", "t4", "t3", "t5"]),
            ids_of(&["t1", "t2", "t4", "t3", "t5", "t2"]),
            ids_of(&["t1", "t2", "t4"]),
        ]
    );
    // Playing another position is a change of track, not of queue.
    w.wait_for("track t4", |ev| {
        ev.iter()
            .any(|e| e["type"] == "track" && e["track"]["id"] == "t4" && e["index"] == 2)
    });
    let order: Vec<String> = w
        .events()
        .iter()
        .filter(|e| e["type"] == "queue" || e["type"] == "track")
        .map(|e| e["type"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        order,
        ["queue", "queue", "queue", "queue", "track", "queue"]
    );

    // Volume and favourites are announced too.
    sb.ok(&["volume", "40"]);
    w.wait_for("volume", |ev| {
        ev.iter()
            .any(|e| e["type"] == "volume" && (e["volume"].as_f64().unwrap() - 0.4).abs() < 1e-9)
    });
    let r = sb.request(json!({"cmd": "set_starred", "id": "t2", "starred": true}));
    assert_eq!(r["ok"], true);
    w.wait_for("starred", |ev| {
        ev.iter()
            .any(|e| e["type"] == "starred" && e["id"] == "t2" && e["starred"] == true)
    });
    assert_eq!(sb.queue_list()["tracks"][1]["starred"], true);
}

#[test]
fn moving_the_current_track_keeps_it_playing() {
    let sb = Sandbox::new(true);
    sb.play_long_album();
    let w = sb.watch();
    w.wait_count("snapshot", 1);
    let before = sb.now()["position"].as_f64().unwrap();

    sb.ok(&["queue", "--move", "1", "4"]);
    let list = sb.queue_list();
    assert_eq!(ids(&list), ["t2", "t3", "t4", "t1", "t5"]);
    assert_eq!(list["current"], 3);
    let now = sb.now();
    assert_eq!(now["id"], "t1");
    assert_eq!(now["state"], "playing");
    assert_eq!(now["index"], 4);
    assert!(now["position"].as_f64().unwrap() >= before, "{now}");

    // Back to the front: still the same track, never restarted.
    sb.ok(&["queue", "--move", "4", "1"]);
    assert_eq!(sb.queue_list()["current"], 0);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(sb.now()["id"], "t1");
    assert!(w.of_type("track").is_empty(), "{:?}", w.of_type("track"));
    let now_playing = sb
        .calls()
        .iter()
        .filter(|c| {
            c.starts_with("scrobble?") && c.contains("id=t1&") && c.contains("submission=false")
        })
        .count();
    assert_eq!(now_playing, 1);
    assert_eq!(w.of_type("queue").last().unwrap()["queue"]["current"], 0);
}

#[test]
fn removing_the_current_track_plays_the_next_one() {
    let sb = Sandbox::new(true);
    sb.play_long_album();
    let w = sb.watch();
    w.wait_count("snapshot", 1);

    sb.ok(&["queue", "--remove", "1"]);
    let list = sb.queue_list();
    assert_eq!(ids(&list), ["t2", "t3", "t4", "t5"]);
    assert_eq!(list["current"], 0);
    wait_until(5, "t2 playing", || {
        let n = sb.now();
        n["id"] == "t2" && n["state"] == "playing"
    });
    wait_until(5, "now playing t2", || sb.scrobbled("t2", false));
    w.wait_for("track t2", |ev| {
        ev.iter()
            .any(|e| e["type"] == "track" && e["track"]["id"] == "t2" && e["index"] == 0)
    });

    // The last one, removed while current: the player stops, the rest stays.
    sb.ok(&["queue", "--play", "4"]);
    wait_until(5, "t5 playing", || sb.now()["id"] == "t5");
    sb.ok(&["queue", "--remove", "4"]);
    let list = sb.queue_list();
    assert_eq!(ids(&list), ["t2", "t3", "t4"]);
    assert_eq!(list["current"], Value::Null);
    wait_until(5, "stopped", || sb.now()["state"] == "stopped");
    w.wait_for("stopped", |ev| {
        ev.iter()
            .any(|e| e["type"] == "state" && e["state"] == "stopped")
    });
    std::thread::sleep(Duration::from_millis(1500));
    assert!(sb.socket().exists(), "the daemon waits, it does not quit");

    // And it plays again from the list.
    sb.ok(&["queue", "--play", "2"]);
    wait_until(5, "t3 playing", || {
        let n = sb.now();
        n["id"] == "t3" && n["state"] == "playing"
    });
}

#[test]
fn moving_or_removing_before_the_current_track_shifts_its_index() {
    let sb = Sandbox::new(true);
    sb.play_long_album();
    sb.ok(&["queue", "--play", "3"]);
    wait_until(5, "t3 playing", || sb.now()["id"] == "t3");
    let w = sb.watch();
    w.wait_count("snapshot", 1);

    sb.ok(&["queue", "--remove", "1"]);
    let list = sb.queue_list();
    assert_eq!(ids(&list), ["t2", "t3", "t4", "t5"]);
    assert_eq!(list["current"], 1);
    assert_eq!(sb.now()["index"], 2);

    sb.ok(&["queue", "--move", "4", "1"]);
    let list = sb.queue_list();
    assert_eq!(ids(&list), ["t5", "t2", "t3", "t4"]);
    assert_eq!(list["current"], 2);

    sb.ok(&["queue", "--move", "1", "4"]);
    let list = sb.queue_list();
    assert_eq!(ids(&list), ["t2", "t3", "t4", "t5"]);
    assert_eq!(list["current"], 1);

    let now = sb.now();
    assert_eq!(now["id"], "t3");
    assert_eq!(now["state"], "playing");
    w.wait_count("queue", 3);
    let currents: Vec<Value> = w
        .of_type("queue")
        .iter()
        .map(|e| e["queue"]["current"].clone())
        .collect();
    assert_eq!(currents, [json!(1), json!(2), json!(1)]);
    assert!(w.of_type("track").is_empty());
}

#[test]
fn clear_after_keeps_past_and_current() {
    let sb = Sandbox::new(true);
    sb.play_long_album();
    sb.ok(&["queue", "--play", "2"]);
    wait_until(5, "t2 playing", || sb.now()["id"] == "t2");
    sb.ok(&["queue", "--clear-after"]);
    let list = sb.queue_list();
    assert_eq!(ids(&list), ["t1", "t2"]);
    assert_eq!(list["current"], 1);
    let now = sb.now();
    assert_eq!(now["id"], "t2");
    assert_eq!(now["state"], "playing");
    // Adding after a clear goes after the current track again.
    sb.ok(&["queue", "track", "t5"]);
    assert_eq!(ids(&sb.queue_list()), ["t1", "t2", "t5"]);
    sb.ok(&["next"]);
    wait_until(5, "t5 playing", || sb.now()["id"] == "t5");
}

#[test]
fn queue_commands_never_start_the_daemon() {
    let sb = Sandbox::new(true);
    let list = sb.queue_list();
    assert_eq!(list, json!({"current": null, "tracks": []}));
    assert!(sb.ok(&["queue", "--list"]).contains("empty"));
    for args in [
        vec!["queue", "--play", "1"],
        vec!["queue", "--move", "1", "2"],
        vec!["queue", "--remove", "1"],
        vec!["queue", "--clear-after"],
    ] {
        assert_eq!(sb.run(&args).status.code(), Some(1), "{args:?}");
    }
    assert!(!sb.socket().exists());
    // A position outside the queue is refused by the daemon, not guessed.
    sb.play_long_album();
    let o = sb.run(&["queue", "--remove", "9"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(!sb.run(&["queue", "--play", "0"]).status.success());
    assert_eq!(ids(&sb.queue_list()).len(), 5);
}

#[test]
fn watch_survives_a_daemon_restart_by_saying_the_daemon_is_gone() {
    let sb = Sandbox::new(true);
    long_album(&sb);
    let mut w = sb.watch();
    w.wait_for("gone", |ev| ev.first().is_some_and(|e| e["type"] == "gone"));
    std::thread::sleep(Duration::from_millis(1500));
    assert!(!sb.socket().exists(), "watch never starts the daemon");
    assert_eq!(w.events().len(), 1, "said once: {:?}", w.raw());

    sb.ok(&["play", "album", "al1"]);
    w.wait_count("snapshot", 1);
    quit_daemon(&sb);
    w.wait_count("gone", 2);

    sb.ok(&["play", "track", "t3"]);
    w.wait_count("snapshot", 2);
    // The daemon listens before `play` has filled the queue: the snapshot can
    // be empty, and the queue then arrives as an event. A client's view is the
    // snapshot plus the events after it.
    wait_until(5, "the new daemon's queue", || {
        ids(&w.latest_queue()) == ["t3"]
    });
    assert!(w.child.try_wait().unwrap().is_none(), "watch still runs");
}

#[test]
fn watch_does_not_hold_off_the_idle_quit() {
    let sb = Sandbox::new(true);
    long_album(&sb);
    let mut c = sb.cmd(&["play", "track", "t2"]);
    c.env("MELODIE_IDLE_SECS", "2");
    let o = c.output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let w = sb.watch();
    w.wait_count("snapshot", 1);
    sb.ok(&["pause"]);
    wait_until(10, "idle quit with a watcher", || !sb.socket().exists());
    w.wait_count("gone", 1);
}

#[test]
fn position_events_at_most_once_a_second_and_not_while_paused() {
    let sb = Sandbox::new(true);
    sb.play_long_album();
    let w = sb.watch();
    w.wait_count("snapshot", 1);
    let start = Instant::now();
    std::thread::sleep(Duration::from_millis(3500));
    let window = start.elapsed().as_secs_f64();
    let n = w.of_type("position").len();
    assert!(n >= 1, "positions are sent while playing");
    assert!(
        (n as f64) <= window.ceil() + 1.0,
        "{n} position events in {window:.1} s"
    );
    for e in w.of_type("position") {
        assert!(
            e["position"].as_f64().is_some() && e["duration"] == 300.0,
            "{e}"
        );
    }

    sb.ok(&["pause"]);
    w.wait_for("paused", |ev| {
        ev.iter()
            .any(|e| e["type"] == "state" && e["state"] == "paused")
    });
    let paused_at = w.events().len();
    std::thread::sleep(Duration::from_millis(2500));
    let after: Vec<Value> = w.events().split_off(paused_at);
    assert!(after.iter().all(|e| e["type"] != "position"), "{after:?}");
}

#[test]
fn no_event_or_listing_holds_a_secret() {
    let sb = Sandbox::new(true);
    sb.play_long_album();
    let w = sb.watch();
    w.wait_count("snapshot", 1);
    sb.ok(&["queue", "--next", "track", "t3"]);
    sb.ok(&["queue", "--play", "2"]);
    sb.ok(&["queue", "--move", "1", "3"]);
    sb.ok(&["queue", "--remove", "2"]);
    w.wait_count("queue", 3);
    wait_until(3, "position", || !w.of_type("position").is_empty());
    for line in w.raw() {
        assert_no_secret(&line);
    }
    assert_no_secret(&sb.ok(&["queue", "--list", "--json"]));
    assert_no_secret(&sb.ok(&["queue", "--list"]));
    assert_no_secret(&sb.ok(&["now", "--json"]));
    assert_no_secret(&sb.request(json!({"cmd": "queue_list"})).to_string());
}

#[test]
fn a_stalled_subscriber_never_blocks_the_daemon() {
    let sb = Sandbox::new(true);
    sb.play_long_album();
    // Subscribes, then never reads.
    let stalled = UnixStream::connect(sb.socket()).unwrap();
    (&stalled).write_all(b"{\"cmd\":\"watch\"}\n").unwrap();

    // Many queue changes, each one a `queue` event of the whole queue.
    let s = UnixStream::connect(sb.socket()).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let mut reader = BufReader::new(&s);
    for i in 0..1500 {
        let (from, to) = if i % 2 == 0 { (1, 2) } else { (2, 1) };
        (&s).write_all(
            format!("{{\"cmd\":\"queue_move\",\"from\":{from},\"to\":{to}}}\n").as_bytes(),
        )
        .unwrap();
        let mut l = String::new();
        reader.read_line(&mut l).unwrap();
        assert!(l.contains("\"ok\":true"), "{l}");
    }
    // The daemon still answers and still plays.
    let p1 = sb.now()["position"].as_f64().unwrap();
    wait_until(5, "still playing", || {
        sb.now()["position"].as_f64().unwrap() > p1
    });
    // The stalled subscriber was let go: what it finally reads ends.
    stalled
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut n = 0;
    for l in BufReader::new(&stalled).lines() {
        match l {
            Ok(_) => n += 1,
            Err(e) => panic!("the stalled subscriber was never closed ({n} lines): {e}"),
        }
    }
    assert!(n < 1500, "{n} lines");
    assert!(sb.log().contains("subscriber"), "{}", sb.log());
    // A new client is served as usual.
    let w = sb.watch();
    w.wait_count("snapshot", 1);
}

#[test]
fn connection_state_is_announced() {
    let sb = Sandbox::new(true);
    sb.play_long_album();
    let w = sb.watch();
    w.wait_count("snapshot", 1);
    // The server now refuses the token: the next stream opened fails, the daemon says so and
    // quits. Not `next`: mpv may already hold the next track, prefetched with the good token.
    *sb.fake.state.expected_token.lock().unwrap() = "autre".into();
    sb.ok(&["queue", "--play", "4"]);
    w.wait_for("auth-refused", |ev| {
        ev.iter()
            .any(|e| e["type"] == "connection" && e["state"] == "auth-refused")
    });
    w.wait_count("gone", 1);
}

#[test]
fn the_cover_of_the_current_track_is_announced_when_it_arrives() {
    let sb = Sandbox::new(true);
    long_album(&sb);
    for (i, s) in sb.fake.state.songs.lock().unwrap().iter_mut().enumerate() {
        s["coverArt"] = json!(format!("c{}", i + 1));
    }
    sb.ok(&["play", "album", "al1"]);
    wait_until(5, "lecture", || {
        sb.try_now().is_some_and(|v| v["state"] == "playing")
    });
    let w = sb.watch();
    w.wait_count("snapshot", 1);
    sb.fake.state.cover_delay_ms.store(500, Ordering::SeqCst);
    sb.ok(&["queue", "--play", "3"]);
    w.wait_for("cover of t3", |ev| {
        ev.iter().any(|e| {
            e["type"] == "track"
                && e["track"]["id"] == "t3"
                && e["track"]["cover"]
                    .as_str()
                    .is_some_and(|c| c.ends_with("/covers/c3.img"))
        })
    });
}

/// Five tracks of five minutes: `al1` holds t1 to t3, `al2` (older) t4 and t5, the playlist
/// `p1` holds t5 then t1.
fn addressable_library(sb: &Sandbox) {
    sb.fake.state.stream_secs.store(300, Ordering::SeqCst);
    *sb.fake.state.songs.lock().unwrap() = (1..=5)
        .map(|i| {
            let album = if i > 3 { "al2" } else { "al1" };
            song(i, json!({"duration": 300, "albumId": album}))
        })
        .collect();
    seed_playlist(sb, &["t5", "t1"]);
}

/// The address of each kind and what the fake server answers for it. The artist's albums come
/// oldest first (`al2`, then `al1`).
const ADDRESSES: [(&str, &[&str]); 4] = [
    ("melodie:track/t2", &["t2"]),
    ("melodie:album/al1", &["t1", "t2", "t3"]),
    ("melodie:playlist/p1", &["t5", "t1"]),
    ("melodie:artist/ar1", &["t4", "t5", "t1", "t2", "t3"]),
];

const MALFORMED: [&str; 9] = [
    "melodie:radio/r1",
    "melodie:album/",
    "melodie:album",
    "melodie:album/a%2Fb",
    "melodie:album/al1?shuffle=maybe",
    "melodie:album/al1?queue=now",
    "melodie:album/al1?speed=2",
    "melodie:album/al1#top",
    "https://example.org/album/al1",
];

/// Waits for the copy the daemon's first background synchronisation makes: it reads the
/// playlists (`getPlaylist`), which `lookups` would otherwise count against `open`.
fn wait_for_the_first_copy(sb: &Sandbox) {
    let db = sb.home().join(".cache/melodie/library.db");
    let end = Instant::now() + Duration::from_secs(20);
    while !db.exists() {
        assert!(Instant::now() < end, "no local copy was made");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Requests that look something up or check the connection; the player's own reports to the
/// server (now playing, scrobble, cover) come on their own time and are not counted.
fn lookups(sb: &Sandbox) -> usize {
    sb.calls()
        .iter()
        .filter(|c| {
            [
                "ping?",
                "getSong?",
                "getAlbum?",
                "getArtist?",
                "getPlaylist?",
            ]
            .iter()
            .any(|m| c.starts_with(m))
        })
        .count()
}

fn queue_ids(sb: &Sandbox) -> Vec<String> {
    ids(&sb.queue_list())
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[test]
fn open_plays_each_kind_by_its_address() {
    let sb = Sandbox::new(true);
    addressable_library(&sb);
    for (uri, expected) in ADDRESSES {
        let out = sb.ok(&["open", uri]);
        assert!(out.starts_with("Lecture :"), "{uri}: {out}");
        assert_eq!(queue_ids(&sb), ids_of(expected), "{uri}");
        wait_until(5, "lecture", || sb.now()["state"] == "playing");
        assert_eq!(sb.now()["id"], expected[0], "{uri}");
    }
}

#[test]
fn open_adds_to_the_queue_next_or_last() {
    let sb = Sandbox::new(true);
    addressable_library(&sb);
    sb.ok(&["open", "melodie:album/al2"]);
    sb.ok(&["open", "melodie:track/t1?queue=last"]);
    assert_eq!(queue_ids(&sb), ids_of(&["t4", "t5", "t1"]));
    let out = sb.ok(&["open", "melodie:track/t2?queue=next"]);
    assert!(out.starts_with("Ajouté à la file :"), "{out}");
    assert_eq!(queue_ids(&sb), ids_of(&["t4", "t2", "t5", "t1"]));
    // What plays is untouched.
    assert_eq!(sb.now()["id"], "t4");
}

#[test]
fn open_shuffles_what_it_plays_or_adds() {
    let sb = Sandbox::new(true);
    addressable_library(&sb);
    let natural = ids_of(ADDRESSES[3].1);
    // A shuffle can keep the natural order by chance (1 in 120): try again a few times.
    let mut moved = false;
    for _ in 0..8 {
        sb.ok(&["open", "melodie:artist/ar1?shuffle=true"]);
        let q = queue_ids(&sb);
        assert_eq!(sorted(q.clone()), sorted(natural.clone()));
        if q != natural {
            moved = true;
            break;
        }
    }
    assert!(moved, "never shuffled");
    // Explicitly not shuffled.
    sb.ok(&["open", "melodie:artist/ar1?shuffle=false"]);
    assert_eq!(queue_ids(&sb), natural);
    // Shuffling an addition adds the same tracks, and leaves the queue as it was before them.
    sb.ok(&["open", "melodie:album/al1?shuffle=true&queue=last"]);
    let q = queue_ids(&sb);
    assert_eq!(q[..5], natural[..]);
    assert_eq!(sorted(q[5..].to_vec()), ids_of(&["t1", "t2", "t3"]));
}

#[test]
fn open_refuses_a_malformed_address_and_changes_nothing() {
    let sb = Sandbox::new(true);
    addressable_library(&sb);
    // No daemon: none is started, and the server is not even asked.
    for uri in MALFORMED {
        let o = sb.run(&["open", uri]);
        assert_eq!(o.status.code(), Some(1), "{uri}");
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(err.contains("invalid address"), "{uri}: {err}");
        assert!(!sb.socket().exists(), "{uri}");
    }
    assert!(sb.calls().is_empty(), "{:?}", sb.calls());
    // With a daemon: the queue and the playing track stay as they are.
    sb.ok(&["open", "melodie:album/al1"]);
    wait_for_the_first_copy(&sb);
    let calls = lookups(&sb);
    for uri in MALFORMED {
        assert_eq!(sb.run(&["open", uri]).status.code(), Some(1), "{uri}");
    }
    assert_eq!(queue_ids(&sb), ids_of(&["t1", "t2", "t3"]));
    assert_eq!(sb.now()["id"], "t1");
    assert_eq!(lookups(&sb), calls);
}

#[test]
fn open_of_something_the_server_does_not_have_changes_nothing() {
    let sb = Sandbox::new(true);
    addressable_library(&sb);
    sb.ok(&["open", "melodie:album/al1"]);
    for uri in ["melodie:album/nope", "melodie:track/nope"] {
        let o = sb.run(&["open", uri]);
        assert!(!o.status.success(), "{uri}");
        assert!(!o.stderr.is_empty(), "{uri}");
    }
    assert_eq!(queue_ids(&sb), ids_of(&["t1", "t2", "t3"]));
}

/// What each command that resolves an id on the server says for an id it does not have.
const UNKNOWN_ID_COMMANDS: [(&[&str], &str); 5] = [
    (&["play", "album", "nope"], "album nope"),
    (&["play", "track", "nope"], "track nope"),
    (&["queue", "album", "nope"], "album nope"),
    (&["open", "melodie:album/nope"], "album nope"),
    (&["playlist", "add", "p1", "album", "nope"], "album nope"),
];

fn assert_not_found(o: &Output, what: &str, args: &[&str]) {
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(5), "{args:?}: {err}");
    assert!(
        err.contains(&format!("Introuvable sur le serveur : {what}")),
        "{args:?}: {err}"
    );
    assert!(
        !err.contains("injoignable") && !err.contains("melodie login"),
        "{args:?}: {err}"
    );
}

#[test]
fn an_unknown_id_is_not_found_and_not_an_unreachable_server() {
    let sb = Sandbox::new(true);
    addressable_library(&sb);
    for (args, what) in UNKNOWN_ID_COMMANDS {
        assert_not_found(&sb.run(args), what, args);
    }
    // The server is fine: no daemon was started for nothing, no login is asked again.
    assert!(!sb.socket().exists());
    assert!(!sb.home().join(".cache/melodie/auth-refused").exists());
    // And the next command still works.
    assert!(
        sb.ok(&["open", "melodie:album/al1"])
            .starts_with("Lecture :")
    );
}

#[test]
fn an_unknown_id_never_changes_the_connection_the_daemon_keeps() {
    let sb = Sandbox::new(true);
    addressable_library(&sb);
    sb.ok(&["open", "melodie:album/al1"]);
    let w = sb.watch();
    w.wait_count("snapshot", 1);
    assert_eq!(w.of_type("snapshot")[0]["connection"], "ok");
    for (args, what) in UNKNOWN_ID_COMMANDS {
        assert_not_found(&sb.run(args), what, args);
    }
    // A later event proves the stream was open and the daemon alive for all of the above.
    sb.ok(&["pause"]);
    w.wait_for("state", |ev| {
        ev.iter()
            .any(|e| e["type"] == "state" && e["state"] == "paused")
    });
    assert!(w.of_type("connection").is_empty(), "{:?}", w.raw());
    assert_eq!(queue_ids(&sb), ids_of(&["t1", "t2", "t3"]));
}

#[test]
fn an_unknown_id_is_one_launcher_entry_in_json() {
    let sb = Sandbox::new(true);
    addressable_library(&sb);
    let args = &["playlist", "add", "p1", "album", "nope", "--json"][..];
    let o = sb.run(args);
    assert_eq!(o.status.code(), Some(0));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["id"], "not-found");
    assert_eq!(v[0]["name"], "Introuvable sur le serveur");
    assert_eq!(v[0]["subtitle"], "album nope");
    assert!(v[0].get("action").is_none());
}

#[test]
fn a_refused_token_and_a_dead_server_keep_their_answers_for_an_unknown_id() {
    let sb = Sandbox::new(true);
    *sb.fake.state.expected_token.lock().unwrap() = "autre".into();
    for (args, _) in UNKNOWN_ID_COMMANDS {
        assert_eq!(sb.run(args).status.code(), Some(1), "{args:?}");
    }
    let sb = Sandbox::new(true);
    *sb.fake.state.raw.lock().unwrap() = Some((503, "x".into()));
    for (args, _) in UNKNOWN_ID_COMMANDS {
        let o = sb.run(args);
        assert_eq!(o.status.code(), Some(2), "{args:?}");
        assert!(String::from_utf8_lossy(&o.stderr).contains("injoignable"));
    }
}

#[test]
fn open_without_login_says_no_login_and_starts_no_daemon() {
    let sb = Sandbox::new(false);
    let o = sb.run(&["open", "melodie:album/al1"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("melodie login"));
    assert!(!sb.socket().exists());
    assert!(sb.calls().is_empty());

    // A refused token is told like no login, and is remembered.
    let sb = Sandbox::new(true);
    *sb.fake.state.expected_token.lock().unwrap() = "autre".into();
    let o = sb.run(&["open", "melodie:album/al1"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(sb.home().join(".cache/melodie/auth-refused").exists());
    assert!(!sb.socket().exists());

    // An unreachable server is not a reason to log in again.
    let sb = Sandbox::new(true);
    *sb.fake.state.raw.lock().unwrap() = Some((503, "x".into()));
    let o = sb.run(&["open", "melodie:album/al1"]);
    assert_eq!(o.status.code(), Some(2));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("injoignable") && !err.contains("melodie login"),
        "{err}"
    );
    assert!(!sb.socket().exists());
    assert!(!sb.home().join(".cache/melodie/auth-refused").exists());
}

/// A client of the player on the private bus, or `None` (and a note) without `dbus-daemon`.
fn bus_player(sb: &Sandbox) -> Option<(Connection, Proxy<'static>)> {
    let Some(addr) = &sb.bus_addr else {
        eprintln!("NOTE : dbus-daemon absent, vérifications MPRIS sautées");
        return None;
    };
    let conn = zbus::blocking::connection::Builder::address(addr.as_str())
        .unwrap()
        .build()
        .unwrap();
    let p = mpris(&conn, &format!("org.mpris.MediaPlayer2.{}", sb.bus_name));
    Some((conn, p))
}

fn open_uri(p: &Proxy<'_>, uri: &str) -> zbus::Result<()> {
    p.call("OpenUri", &(uri,))
}

#[test]
fn open_uri_on_mpris_plays_each_kind() {
    let sb = Sandbox::new(true);
    addressable_library(&sb);
    // Any command that starts the daemon will do: the bus is served by it.
    sb.ok(&["play", "track", "t3"]);
    let Some((_conn, p)) = bus_player(&sb) else {
        return;
    };
    for (uri, expected) in ADDRESSES {
        open_uri(&p, uri).unwrap();
        wait_until(5, uri, || queue_ids(&sb) == ids_of(expected));
        wait_until(5, "lecture", || {
            sb.now()["state"] == "playing" && sb.now()["id"] == expected[0]
        });
        assert_eq!(status(&p), "Playing");
    }
    // The options are the same as on the command line.
    open_uri(&p, "melodie:album/al2").unwrap();
    wait_until(5, "al2", || queue_ids(&sb) == ids_of(&["t4", "t5"]));
    open_uri(&p, "melodie:track/t1?queue=last").unwrap();
    wait_until(5, "last", || queue_ids(&sb) == ids_of(&["t4", "t5", "t1"]));
    open_uri(&p, "melodie:track/t2?queue=next").unwrap();
    wait_until(5, "next", || {
        queue_ids(&sb) == ids_of(&["t4", "t2", "t5", "t1"])
    });
    open_uri(&p, "melodie:artist/ar1?shuffle=true").unwrap();
    wait_until(5, "shuffled", || queue_ids(&sb).len() == 5);
    assert_eq!(sorted(queue_ids(&sb)), sorted(ids_of(ADDRESSES[3].1)),);
    // Two addresses in a row are served in that order.
    open_uri(&p, "melodie:album/al2").unwrap();
    open_uri(&p, "melodie:track/t1?queue=last").unwrap();
    wait_until(5, "in order", || {
        queue_ids(&sb) == ids_of(&["t4", "t5", "t1"])
    });
    assert_no_secret(&sb.log());
}

#[test]
fn open_uri_on_mpris_refuses_a_malformed_address_and_changes_nothing() {
    let sb = Sandbox::new(true);
    addressable_library(&sb);
    sb.ok(&["open", "melodie:album/al1"]);
    let Some((_conn, p)) = bus_player(&sb) else {
        return;
    };
    wait_for_the_first_copy(&sb);
    let calls = lookups(&sb);
    for uri in MALFORMED {
        let err = open_uri(&p, uri).expect_err(uri);
        let zbus::Error::MethodError(name, text, _) = &err else {
            panic!("{uri}: {err}");
        };
        assert_eq!(
            name.as_str(),
            "org.freedesktop.DBus.Error.InvalidArgs",
            "{uri}"
        );
        assert!(
            text.as_deref().unwrap_or("").contains("invalid address"),
            "{uri}: {err}"
        );
    }
    // Refused on the spot: no request left for the server, nothing queued, still playing.
    assert_eq!(queue_ids(&sb), ids_of(&["t1", "t2", "t3"]));
    assert_eq!(sb.now()["id"], "t1");
    assert_eq!(status(&p), "Playing");
    assert_eq!(lookups(&sb), calls);
}

#[test]
fn open_uri_on_mpris_of_something_the_server_does_not_have_changes_nothing() {
    let sb = Sandbox::new(true);
    addressable_library(&sb);
    sb.ok(&["open", "melodie:album/al1"]);
    let Some((_conn, p)) = bus_player(&sb) else {
        return;
    };
    // The answer comes before the server's, so the call itself succeeds; the log tells.
    open_uri(&p, "melodie:track/nope").unwrap();
    wait_until(5, "log", || sb.log().contains("OpenUri"));
    assert!(
        sb.log().contains("track/nope") || sb.log().contains("track nope"),
        "{}",
        sb.log()
    );
    assert_eq!(queue_ids(&sb), ids_of(&["t1", "t2", "t3"]));
    assert_no_secret(&sb.log());
}

#[test]
fn open_uri_on_mpris_of_an_unknown_id_is_logged_as_not_found_and_changes_nothing() {
    let sb = Sandbox::new(true);
    addressable_library(&sb);
    sb.ok(&["open", "melodie:album/al1"]);
    let Some((_conn, p)) = bus_player(&sb) else {
        return;
    };
    let w = sb.watch();
    w.wait_count("snapshot", 1);
    open_uri(&p, "melodie:album/nope").unwrap();
    wait_until(5, "log", || sb.log().contains("OpenUri"));
    let log = sb.log();
    assert!(
        log.contains("Introuvable sur le serveur : album nope"),
        "{log}"
    );
    assert!(!log.contains("injoignable"), "{log}");
    // The daemon is still there, still playing, and said nothing about the connection.
    open_uri(&p, "melodie:track/t2?queue=last").unwrap();
    wait_until(5, "queued", || {
        queue_ids(&sb) == ids_of(&["t1", "t2", "t3", "t2"])
    });
    assert!(w.of_type("connection").is_empty(), "{:?}", w.raw());
    assert!(!sb.home().join(".cache/melodie/auth-refused").exists());
    assert_no_secret(&log);
}

#[test]
fn open_uri_on_mpris_with_a_refused_token_quits_like_a_refused_stream() {
    let sb = Sandbox::new(true);
    addressable_library(&sb);
    sb.ok(&["open", "melodie:album/al1"]);
    let Some((_conn, p)) = bus_player(&sb) else {
        return;
    };
    *sb.fake.state.expected_token.lock().unwrap() = "autre".into();
    open_uri(&p, "melodie:album/al2").unwrap();
    wait_until(20, "daemon quit", || !sb.socket().exists());
    assert!(sb.home().join(".cache/melodie/auth-refused").exists());
}

#[test]
fn mpris_lists_the_scheme_it_opens() {
    let sb = Sandbox::new(true);
    addressable_library(&sb);
    sb.ok(&["open", "melodie:track/t1"]);
    let Some((conn, _p)) = bus_player(&sb) else {
        return;
    };
    let root = Proxy::new(
        &conn,
        format!("org.mpris.MediaPlayer2.{}", sb.bus_name),
        "/org/mpris/MediaPlayer2",
        "org.mpris.MediaPlayer2",
    )
    .unwrap();
    let schemes: Vec<String> = root.get_property("SupportedUriSchemes").unwrap();
    assert_eq!(schemes, ["melodie"]);
    // Only addresses are opened, never a file: no media type to announce.
    let types: Vec<String> = root.get_property("SupportedMimeTypes").unwrap();
    assert!(types.is_empty());
}
