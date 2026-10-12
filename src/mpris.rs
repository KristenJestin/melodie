//! Service MPRIS 2 (`org.mpris.MediaPlayer2.poste_music`) : le démon se montre
//! au bureau (touches multimédia, barre, `playerctl`). Aucune adresse de flux
//! n'y figure : pas de `xesam:url`.

use crate::daemon::Msg;
use crate::proto::Request;
use libmpv2::Mpv;
use std::collections::HashMap;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use zbus::blocking::{connection::Builder, Connection};
use zbus::fdo;
use zbus::interface;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

pub const OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";
pub const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";
const NO_TRACK: &str = "/org/mpris/MediaPlayer2/TrackList/NoTrack";
const TRACK_PREFIX: &str = "/org/poste/music/track/";

/// Ce que MPRIS dit du morceau en cours.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MetaInfo {
    /// Numéro unique de l'entrée dans la file (sert d'identifiant D-Bus).
    pub seq: u64,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub length_us: i64,
    pub art_url: Option<String>,
    pub quality: String,
    /// Morceau aimé (clé `poste:starred`).
    pub starred: bool,
}

/// État publié : lu par les propriétés, comparé pour signaler les changements.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// `Playing`, `Paused` ou `Stopped`.
    pub status: &'static str,
    pub meta: Option<MetaInfo>,
    /// De 0 à 1.
    pub volume: f64,
}

impl Snapshot {
    pub fn stopped() -> Self {
        Self {
            status: "Stopped",
            meta: None,
            volume: 1.0,
        }
    }
}

pub fn track_path(seq: u64) -> String {
    format!("{TRACK_PREFIX}{seq}")
}

/// Numéro d'entrée contenu dans un identifiant de morceau, s'il en est un.
pub fn seq_of_path(path: &str) -> Option<u64> {
    path.strip_prefix(TRACK_PREFIX)?.parse().ok()
}

fn owned(v: Value<'_>) -> OwnedValue {
    OwnedValue::try_from(v).expect("valeur sans descripteur de fichier")
}

fn string_list(items: Vec<String>) -> OwnedValue {
    owned(Value::from(items))
}

/// Dictionnaire `Metadata`.
pub fn metadata(meta: &Option<MetaInfo>) -> HashMap<String, OwnedValue> {
    let mut m = HashMap::new();
    let Some(t) = meta else {
        m.insert(
            "mpris:trackid".to_string(),
            owned(Value::ObjectPath(ObjectPath::from_static_str_unchecked(
                NO_TRACK,
            ))),
        );
        return m;
    };
    let path = ObjectPath::try_from(track_path(t.seq)).expect("chemin D-Bus valide");
    m.insert("mpris:trackid".into(), owned(Value::ObjectPath(path)));
    m.insert("mpris:length".into(), owned(Value::I64(t.length_us)));
    m.insert("xesam:title".into(), owned(Value::from(t.title.clone())));
    if !t.artist.is_empty() {
        m.insert("xesam:artist".into(), string_list(vec![t.artist.clone()]));
    }
    m.insert("xesam:album".into(), owned(Value::from(t.album.clone())));
    if !t.album_artist.is_empty() {
        m.insert(
            "xesam:albumArtist".into(),
            string_list(vec![t.album_artist.clone()]),
        );
    }
    if let Some(u) = &t.art_url {
        m.insert("mpris:artUrl".into(), owned(Value::from(u.clone())));
    }
    m.insert("poste:starred".into(), owned(Value::Bool(t.starred)));
    if !t.quality.is_empty() {
        m.insert("xesam:comment".into(), string_list(vec![t.quality.clone()]));
        m.insert(
            "poste:quality".into(),
            owned(Value::from(t.quality.clone())),
        );
    }
    m
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

struct Root {
    tx: Sender<Msg>,
}

#[interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    fn quit(&self) {
        let (r, _) = channel();
        let _ = self.tx.send(Msg::Req(Request::Quit, r));
    }

    fn raise(&self) {}

    #[zbus(property)]
    fn can_quit(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_raise(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn identity(&self) -> String {
        "Musique".to_string()
    }

    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<String> {
        Vec::new()
    }

    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<String> {
        Vec::new()
    }
}

struct Player {
    tx: Sender<Msg>,
    shared: Arc<Mutex<Snapshot>>,
    mpv: Arc<Mpv>,
}

impl Player {
    fn send(&self, req: Request) {
        let (r, _) = channel();
        let _ = self.tx.send(Msg::Req(req, r));
    }
}

#[interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    fn play(&self) {
        self.send(Request::Resume);
    }

    fn pause(&self) {
        self.send(Request::Pause);
    }

    fn play_pause(&self) {
        self.send(Request::Toggle);
    }

    fn stop(&self) {
        self.send(Request::Stop);
    }

    fn next(&self) {
        self.send(Request::Next);
    }

    fn previous(&self) {
        self.send(Request::Prev);
    }

    fn seek(&self, offset: i64) {
        self.send(Request::Seek { offset_us: offset });
    }

    fn set_position(&self, track_id: ObjectPath<'_>, position: i64) {
        if let Some(track) = seq_of_path(track_id.as_str()) {
            self.send(Request::SetPosition {
                track,
                position_us: position,
            });
        }
    }

    fn open_uri(&self, _uri: String) -> fdo::Result<()> {
        Err(fdo::Error::NotSupported(
            "ouverture d'adresse non prise en charge".into(),
        ))
    }

    #[zbus(signal)]
    async fn seeked(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        position: i64,
    ) -> zbus::Result<()>;

    #[zbus(property)]
    fn playback_status(&self) -> String {
        lock(&self.shared).status.to_string()
    }

    #[zbus(property)]
    fn loop_status(&self) -> String {
        "None".to_string()
    }

    #[zbus(property)]
    fn set_loop_status(&self, _value: String) {}

    #[zbus(property)]
    fn rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn set_rate(&self, _value: f64) {}

    #[zbus(property)]
    fn shuffle(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn set_shuffle(&self, _value: bool) {}

    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        metadata(&lock(&self.shared).meta)
    }

    #[zbus(property)]
    fn volume(&self) -> f64 {
        lock(&self.shared).volume
    }

    #[zbus(property)]
    fn set_volume(&self, value: f64) {
        self.send(Request::SetVolume { volume: value });
    }

    #[zbus(property(emits_changed_signal = "false"))]
    fn position(&self) -> i64 {
        position_us(&self.mpv)
    }

    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_play(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_pause(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_seek(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_control(&self) -> bool {
        true
    }
}

/// Temps de lecture de mpv, en microsecondes.
pub fn position_us(mpv: &Mpv) -> i64 {
    mpv.get_property::<f64>("time-pos")
        .map(|s| (s * 1_000_000.0) as i64)
        .unwrap_or(0)
}

/// Nom de bus ; `POSTE_MUSIC_MPRIS_NAME` (réservé aux tests) remplace le suffixe.
pub fn bus_name() -> String {
    let suffix = std::env::var("POSTE_MUSIC_MPRIS_NAME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "poste_music".to_string());
    format!("org.mpris.MediaPlayer2.{suffix}")
}

/// Publie le service sur le bus de session. Échoue (sans gravité pour la
/// lecture) si le bus est absent ou le nom déjà pris.
pub fn start(
    tx: Sender<Msg>,
    shared: Arc<Mutex<Snapshot>>,
    mpv: Arc<Mpv>,
) -> zbus::Result<Connection> {
    Builder::session()?
        .name(bus_name())?
        .serve_at(OBJECT_PATH, Root { tx: tx.clone() })?
        .serve_at(OBJECT_PATH, Player { tx, shared, mpv })?
        .build()
}

/// Signale à `PropertiesChanged` les propriétés qui ont changé entre deux états.
pub fn emit_changes(conn: &Connection, old: &Snapshot, new: &Snapshot) -> zbus::Result<()> {
    let mut changed: HashMap<&str, Value<'_>> = HashMap::new();
    if old.status != new.status {
        changed.insert("PlaybackStatus", Value::from(new.status));
    }
    if old.meta != new.meta {
        let md: HashMap<String, Value<'_>> = metadata(&new.meta)
            .into_iter()
            .map(|(k, v)| (k, Value::from(v)))
            .collect();
        changed.insert("Metadata", Value::from(md));
    }
    if (old.volume - new.volume).abs() > f64::EPSILON {
        changed.insert("Volume", Value::F64(new.volume));
    }
    if changed.is_empty() {
        return Ok(());
    }
    let invalidated: Vec<&str> = Vec::new();
    conn.emit_signal(
        None::<&str>,
        OBJECT_PATH,
        "org.freedesktop.DBus.Properties",
        "PropertiesChanged",
        &(PLAYER_IFACE, changed, invalidated),
    )
}

/// Signal `Seeked`.
pub fn emit_seeked(conn: &Connection, position_us: i64) -> zbus::Result<()> {
    conn.emit_signal(
        None::<&str>,
        OBJECT_PATH,
        PLAYER_IFACE,
        "Seeked",
        &(position_us,),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> MetaInfo {
        MetaInfo {
            seq: 7,
            title: "Titre".into(),
            artist: "Artiste".into(),
            album: "Album".into(),
            album_artist: "Artiste".into(),
            length_us: 200_000_000,
            art_url: Some("file:///c/a.img".into()),
            quality: "FLAC · 16 bit / 44.1 kHz · 900 kbps".into(),
            starred: true,
        }
    }

    #[test]
    fn identifiants_de_morceau() {
        assert_eq!(track_path(7), "/org/poste/music/track/7");
        assert_eq!(seq_of_path("/org/poste/music/track/7"), Some(7));
        assert_eq!(seq_of_path("/autre/7"), None);
        assert!(ObjectPath::try_from(track_path(12345)).is_ok());
    }

    #[test]
    fn metadonnees_completes_sans_adresse_de_flux() {
        let m = metadata(&Some(meta()));
        let mut keys: Vec<_> = m.keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "mpris:artUrl",
                "mpris:length",
                "mpris:trackid",
                "poste:quality",
                "poste:starred",
                "xesam:album",
                "xesam:albumArtist",
                "xesam:artist",
                "xesam:comment",
                "xesam:title"
            ]
        );
        assert!(!m.contains_key("xesam:url"));
        assert!(m["poste:starred"].downcast_ref::<bool>().unwrap());
        let text = format!("{m:?}");
        assert!(!text.contains("stream.view") && !text.contains("http"));
        assert_eq!(
            m["mpris:length"].downcast_ref::<i64>().unwrap(),
            200_000_000
        );
    }

    #[test]
    fn sans_morceau() {
        let m = metadata(&None);
        assert_eq!(m.len(), 1);
        assert!(m.contains_key("mpris:trackid"));
    }
}
