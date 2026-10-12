//! Test d'intégration du démon, sans son : `POSTE_MUSIC_AO=null`, faux serveur
//! Navidrome local, HOME et XDG_* temporaires, trousseau remplacé par un fichier
//! (`POSTE_MUSIC_TEST_SECRET`), nom MPRIS propre au test.
//!
//! Le service MPRIS vit sur un bus de session privé (`dbus-daemon` lancé par le test, jamais le vrai bus) ; sans `dbus-daemon`,
//! les vérifications MPRIS sont sautées avec un message.

#[allow(dead_code)]
#[path = "../src/fake.rs"]
mod fake;

use fake::Fake;
use serde_json::{json, Value};
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
/// faux lecteur apparaîtrait dans l'île). `None` si `dbus-daemon` est absent.
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
            bus_name: format!("poste_music_test_{}_{n}", std::process::id()),
            bus_addr,
            bus,
            dir,
            fake,
        };
        for d in ["home", "run"] {
            std::fs::create_dir_all(sb.dir.path().join(d)).unwrap();
        }
        if with_login {
            let conf = sb.dir.path().join("home/.config/poste");
            std::fs::create_dir_all(&conf).unwrap();
            std::fs::write(
                conf.join("music.toml"),
                format!("url = \"{}\"\nuser = \"kris\"\n", sb.fake.url),
            )
            .unwrap();
            std::fs::write(
                sb.secret_file(),
                json!({"u":"kris","t":TOKEN,"s":SALT}).to_string(),
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
        let mut c = Command::new(env!("CARGO_BIN_EXE_poste-music"));
        let h = self.home();
        c.env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &h)
            .env("XDG_CONFIG_HOME", h.join(".config"))
            .env("XDG_CACHE_HOME", h.join(".cache"))
            .env("XDG_DATA_HOME", h.join(".local/share"))
            .env("XDG_STATE_HOME", h.join(".local/state"))
            .env("XDG_RUNTIME_DIR", self.run_dir())
            .env("POSTE_MUSIC_TEST_SECRET", self.secret_file())
            .env("POSTE_MUSIC_AO", "null")
            .env("POSTE_MUSIC_MPRIS_NAME", &self.bus_name)
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
        self.run_dir().join("poste/music.sock")
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.home().join(".local/state/poste/music.log"))
            .unwrap_or_default()
    }

    fn db(&self) -> PathBuf {
        self.home().join(".cache/poste/music/library.db")
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
    assert!(sock.parent().unwrap().join("music.lock").exists());

    // état en JSON, tel que l'île le lira
    let now = sb.now();
    println!("now --json : {now}");
    assert_eq!(now["title"], "Titre 1");
    assert_eq!(now["quality"], "FLAC · 24 bit / 96 kHz · 2304 kbps");
    assert_eq!(now["index"], 1);
    assert_eq!(now["queue_len"], 3);
    assert_eq!(now["duration"], 3.0);
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
        assert!(id.as_str().starts_with("/org/poste/music/track/"));
        assert_eq!(
            m["mpris:length"].try_clone().unwrap().try_into(),
            Ok(3_000_000i64)
        );
        assert_eq!(strings(&m["xesam:artist"]), ["Beyoncé"]);
        assert_eq!(strings(&m["xesam:albumArtist"]), ["Beyoncé"]);
        let album: String = m["xesam:album"].try_clone().unwrap().try_into().unwrap();
        assert_eq!(album, "Discovery");
        let q = "FLAC · 24 bit / 96 kHz · 2304 kbps";
        assert_eq!(strings(&m["xesam:comment"]), [q]);
        let pq: String = m["poste:quality"].try_clone().unwrap().try_into().unwrap();
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
        assert!(file.ends_with("/.cache/poste/music/covers/al-al1.img"));
        let ident: String = Proxy::new(
            conn.as_ref().unwrap(),
            format!("org.mpris.MediaPlayer2.{}", sb.bus_name),
            "/org/mpris/MediaPlayer2",
            "org.mpris.MediaPlayer2",
        )
        .unwrap()
        .get_property("Identity")
        .unwrap();
        assert_eq!(ident, "Musique");
        let vol: f64 = p.get_property("Volume").unwrap();
        assert!((vol - 1.0).abs() < 1e-9, "volume par défaut : {vol}");
        p.set_property("Volume", 0.8f64).unwrap();
        wait_until(3, "volume", || {
            (p.get_property::<f64>("Volume").unwrap() - 0.8).abs() < 1e-9
        });
    }

    // next tout de suite : morceau passé
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
        let q: String = m["poste:quality"].try_clone().unwrap().try_into().unwrap();
        assert_eq!(q, "MP3 · 320 kbps");
        assert!(p_changes
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .contains(&"Metadata".to_string()));
    }

    // pause par MPRIS, reprise par la commande
    if let Some(p) = &proxy {
        p.call_method("PlayPause", &()).unwrap();
        wait_until(3, "pause", || status(p) == "Paused");
        assert_eq!(sb.now()["state"], "paused");
        sb.ok(&["resume"]);
        wait_until(3, "reprise", || status(p) == "Playing");
        assert!(p_changes
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .contains(&"PlaybackStatus".to_string()));
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
    c.env("POSTE_MUSIC_IDLE_SECS", "2");
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
    assert!(sb.home().join(".cache/poste/music/auth-refused").exists());
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
    assert!(String::from_utf8_lossy(&o.stderr).contains("poste music login"));
    assert!(!sb.socket().exists());
    let o = sb.run(&["queue", "album", "al1"]);
    assert_eq!(o.status.code(), Some(1));

    // jeton refusé
    let sb = Sandbox::new(true);
    *sb.fake.state.expected_token.lock().unwrap() = "autre".into();
    let o = sb.run(&["play", "album", "al1"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(sb.home().join(".cache/poste/music/auth-refused").exists());
    assert!(!sb.socket().exists());

    // serveur injoignable
    let sb = Sandbox::new(true);
    *sb.fake.state.raw.lock().unwrap() = Some((503, "x".into()));
    let o = sb.run(&["play", "album", "al1"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("injoignable"));
    assert!(!sb.socket().exists());
    assert!(!sb.home().join(".cache/poste/music/auth-refused").exists());

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
    sb.ok(&["playlist", "add", "p1", "track", "t3"]);
    assert_eq!(query_count(&sb, "updatePlaylist", "songIdToAdd=t3"), 1);
    assert_eq!(count(), 43);
    sb.ok(&["playlist", "add", "p1", "album", "al1"]);
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
    sb.ok(&["playlist", "remove", "p1", "0"]);
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
        "xdg-terminal-exec poste music playlist create --prompt track t1"
    );
    assert_eq!(
        pick[1]["action"]["command"],
        "poste music playlist add p1 track t1"
    );

    // Mixes locaux.
    let mixes: Value = serde_json::from_str(&sb.ok(&["mixes", "--json"])).unwrap();
    assert_eq!(mixes[0]["name"], "Mix perso");
    assert_eq!(mixes[0]["icon"], "󰒝");
    assert_eq!(mixes[0]["action"]["command"], "poste music mix me");
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
    assert!(list[0]["action"]["command"]
        .as_str()
        .unwrap()
        .starts_with("poste music play track t"));
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
    assert!(!sb
        .run(&["mix", "genre", "Disco", "--print"])
        .status
        .success());

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
        metadata(p)["poste:starred"]
            .try_clone()
            .unwrap()
            .try_into()
            .unwrap()
    };
    if let Some(p) = &proxy {
        wait_until(5, "MPRIS", || metadata(p).contains_key("poste:starred"));
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
        wait_until(3, "poste:starred", || mpris_starred(p));
    }
    sb.ok(&["like", "--toggle"]);
    assert_eq!(sb.now()["starred"], false);
    if let Some(p) = &proxy {
        wait_until(3, "poste:starred retiré", || !mpris_starred(p));
    }
    let adds = query_count(&sb, "updatePlaylist", &format!("songIdToAdd={id}"));
    sb.ok(&["playlist", "add", "p1"]);
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
            String::from_utf8_lossy(&o.stderr).contains("poste music login"),
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
    assert!(sb.home().join(".cache/poste/music/auth-refused").exists());
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
    assert!(!String::from_utf8_lossy(&o.stderr).contains("poste music login"));
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
    assert!(!sb.home().join(".cache/poste/music/auth-refused").exists());
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
    std::fs::read_to_string(sb.home().join(".local/state/poste/music-sync.log")).unwrap_or_default()
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
    let dir = sb.run_dir().join("poste");
    std::fs::create_dir_all(&dir).unwrap();
    let f = std::fs::File::create(dir.join("music-sync.lock")).unwrap();
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
fn refus_en_fond_pose_le_marqueur_sans_jeton() {
    let sb = Sandbox::new(true);
    sb.ok(&["sync", "--full"]);
    set_synced_at(&sb, 1);
    *sb.fake.state.fail_code.lock().unwrap() = Some(40);
    sb.ok(&["search", "--json", "titre"]);
    let marker = sb.home().join(".cache/poste/music/auth-refused");
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
        c.env("POSTE_MUSIC_DAEMON_EVERY_SECS", every);
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
    std::fs::read_to_string(sb.home().join(".local/state/poste/music.toml")).unwrap_or_default()
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
