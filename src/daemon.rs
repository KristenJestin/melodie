//! Le démon de lecture (`poste-music daemon`, sous-commande cachée) : un seul
//! processus, libmpv dans le même processus, contrôle par socket Unix, service
//! MPRIS, écoutes signalées à Navidrome.
//!
//! Un seul fil (le moteur) possède l'état de la file ; les autres (socket,
//! événements de mpv, D-Bus) lui envoient des messages.

use crate::api::Client;
use crate::autosync;
use crate::cover;
use crate::db;
use crate::mpris::{self, MetaInfo, Snapshot};
use crate::notify;
use crate::prefs::{self, Prefs};
use crate::proto::{self, Now, Request, Response};
use crate::rules::{self, ScrobbleClock};
use crate::state::{self, Failure};
use crate::store;
use crate::track::TrackInfo;
use anyhow::{bail, Context, Result};
use libmpv2::events::{Event, PropertyData};
use libmpv2::{Format, Mpv};
use poste_core::time::{now_secs, rfc3339};
use poste_core::Env;
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Écrit une ligne datée dans le journal (la sortie d'erreur du démon). Jamais de jeton.
macro_rules! dlog {
    ($($arg:tt)*) => {
        eprintln!("{} {}", rfc3339(now_secs()), format_args!($($arg)*))
    };
}

/// Pause ou arrêt tolérés avant que le démon quitte.
const IDLE_TIMEOUT: Duration = Duration::from_secs(600);
/// Au-delà de ce temps de lecture, « précédent » recommence le morceau.
const RESTART_AFTER_SECS: f64 = 3.0;
/// Attente avant de conclure que mpv est vraiment retombé au repos.
const FINISH_GRACE: Duration = Duration::from_millis(800);
/// Délai avant d'écrire un changement de volume (regroupe les crans successifs).
const VOLUME_SAVE_DELAY: Duration = Duration::from_secs(1);

/// Fin de lecture demandée par SIGTERM ou SIGINT.
static TERMINATED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    TERMINATED.store(true, Ordering::SeqCst);
}

/// Ce que reçoit le moteur.
pub enum Msg {
    Req(Request, Sender<Response>),
    Mpv(MpvEvent),
    /// Une pochette vient d'arriver : republier les métadonnées.
    Cover,
    /// Le serveur a refusé le jeton.
    AuthRefused,
}

pub enum MpvEvent {
    /// Une propriété observée a changé (l'état est relu à la source).
    Prop,
    /// Fin d'un fichier, avec la raison de mpv.
    EndFile(u32),
    /// mpv signale une erreur (ouverture d'un flux impossible, par exemple).
    Failure(String),
}

struct Entry {
    seq: u64,
    info: TrackInfo,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Status {
    Playing,
    Paused,
    Stopped,
}

impl Status {
    fn mpris(self) -> &'static str {
        match self {
            Status::Playing => "Playing",
            Status::Paused => "Paused",
            Status::Stopped => "Stopped",
        }
    }

    fn word(self) -> &'static str {
        match self {
            Status::Playing => "playing",
            Status::Paused => "paused",
            Status::Stopped => "stopped",
        }
    }
}

/// Verrou exclusif sur `music.lock` ; `None` si un autre démon le tient.
fn acquire_lock(path: &std::path::Path) -> Result<Option<std::fs::File>> {
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("ouverture de {}", path.display()))?;
    // SAFETY : descripteur valide, détenu par `f`.
    let r = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if r == 0 {
        return Ok(Some(f));
    }
    let err = std::io::Error::last_os_error();
    if err.kind() == std::io::ErrorKind::WouldBlock {
        Ok(None)
    } else {
        Err(err).context("verrou du démon")
    }
}

fn new_mpv(volume: u32) -> Result<Mpv> {
    let ao = std::env::var("POSTE_MUSIC_AO")
        .ok()
        .filter(|s| !s.is_empty());
    Mpv::with_initializer(|init| {
        init.set_option("vid", "no")?;
        init.set_option("audio-display", "no")?;
        init.set_option("gapless-audio", "weak")?;
        init.set_option("prefetch-playlist", "yes")?;
        init.set_option("cache", "yes")?;
        init.set_option("ytdl", "no")?;
        init.set_option("idle", "yes")?;
        init.set_option("audio-client-name", "Musique")?;
        init.set_option("network-timeout", "15")?;
        init.set_option("volume", i64::from(volume))?;
        // Normalisation entre morceaux avec les tags ReplayGain des fichiers
        // (sans tag : aucune correction). ReplayGain vise -18 LUFS ; +4 dB
        // ramène à -14 LUFS, le niveau des services de streaming et du reste
        // du PC. Le gain est réduit si le morceau écrêterait.
        init.set_option("replaygain", "track")?;
        init.set_option("replaygain-preamp", "4")?;
        init.set_option("replaygain-clip", "yes")?;
        init.set_option("replaygain-fallback", "0")?;
        if let Some(ao) = &ao {
            init.set_option("ao", ao.as_str())?;
        }
        Ok(())
    })
    .map_err(|e| anyhow::anyhow!("initialisation de libmpv : {e}"))
}

/// Fil des événements de mpv, sur un client séparé.
fn spawn_events(mpv: &Mpv, tx: Sender<Msg>) -> Result<()> {
    let ev = mpv
        .create_client(Some("poste-events"))
        .map_err(|e| anyhow::anyhow!("client mpv : {e}"))?;
    for (id, name, fmt) in [
        (1, "playlist-pos", Format::Int64),
        (2, "pause", Format::Flag),
        (3, "volume", Format::Double),
        (4, "idle-active", Format::Flag),
    ] {
        ev.observe_property(name, fmt, id)
            .map_err(|e| anyhow::anyhow!("observation de {name} : {e}"))?;
    }
    std::thread::spawn(move || loop {
        let msg = match ev.wait_event(1.0) {
            None => continue,
            Some(Ok(Event::PropertyChange { change, .. })) => {
                let _ = matches!(change, PropertyData::Flag(_));
                MpvEvent::Prop
            }
            Some(Ok(Event::EndFile(reason))) => MpvEvent::EndFile(reason),
            Some(Ok(Event::Shutdown)) => return,
            Some(Ok(_)) => continue,
            Some(Err(e)) => MpvEvent::Failure(e.to_string()),
        };
        if tx.send(Msg::Mpv(msg)).is_err() {
            return;
        }
    });
    Ok(())
}

/// Un client du socket : une requête par ligne, une réponse par ligne.
fn serve_connection(stream: UnixStream, tx: Sender<Msg>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    let mut out = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    for line in BufReader::new(&stream).lines() {
        let Ok(line) = line else { return };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match proto::decode::<Request>(&line) {
            Ok(req) => {
                let (rtx, rrx) = channel();
                if tx.send(Msg::Req(req, rtx)).is_err() {
                    return;
                }
                rrx.recv_timeout(Duration::from_secs(30))
                    .unwrap_or_else(|_| Response::err("le démon n'a pas répondu"))
            }
            Err(e) => Response::err(format!("{e:#}")),
        };
        let Ok(mut text) = proto::encode(&reply) else {
            return;
        };
        text.push('\n');
        if out.write_all(text.as_bytes()).is_err() {
            return;
        }
    }
}

struct Engine {
    env: Env,
    mpv: Arc<Mpv>,
    client: Arc<Client>,
    tx: Sender<Msg>,
    shared: Arc<Mutex<Snapshot>>,
    conn: Option<zbus::blocking::Connection>,
    queue: Vec<Entry>,
    cur: Option<usize>,
    next_seq: u64,
    clock: Option<ScrobbleClock>,
    last_tick: Instant,
    idle_since: Option<Instant>,
    finish_candidate: Option<Instant>,
    covers_pending: HashSet<String>,
    last_ping: Option<Instant>,
    idle_timeout: Duration,
    /// Dernier volume écrit dans `music.toml` d'état (pourcentage).
    saved_volume: u32,
    /// Depuis quand le volume actuel diffère de celui qui est écrit.
    volume_dirty: Option<Instant>,
    next_sync: Instant,
    quit: bool,
}

type Res<T> = std::result::Result<T, String>;

fn mpv_err(e: libmpv2::Error) -> String {
    format!("mpv : {e}")
}

impl Engine {
    fn status(&self) -> Status {
        if self.cur.is_none() {
            return Status::Stopped;
        }
        match self.mpv.get_property::<bool>("pause") {
            Ok(true) => Status::Paused,
            _ => Status::Playing,
        }
    }

    fn position(&self) -> f64 {
        self.mpv.get_property::<f64>("time-pos").unwrap_or(0.0)
    }

    fn volume(&self) -> f64 {
        self.mpv
            .get_property::<f64>("volume")
            .map(|v| v / 100.0)
            .unwrap_or(f64::from(prefs::DEFAULT_VOLUME) / 100.0)
    }

    /// Note un changement de volume pour l'écrire bientôt (regroupé : un geste
    /// sur la molette donne de nombreux crans, une seule écriture).
    fn track_volume(&mut self, volume: f64) {
        let pct = (volume * 100.0).round() as u32;
        if pct != self.saved_volume && self.volume_dirty.is_none() {
            self.volume_dirty = Some(Instant::now());
        }
        if pct == self.saved_volume {
            self.volume_dirty = None;
        }
    }

    /// Écrit le volume si un changement attend depuis assez longtemps (`force` : tout de suite).
    fn flush_volume(&mut self, force: bool) {
        let Some(t) = self.volume_dirty else { return };
        if !force && t.elapsed() < VOLUME_SAVE_DELAY {
            return;
        }
        let pct = (self.volume() * 100.0).round() as u32;
        let mut p = Prefs::load(&self.env);
        p.volume = Some(pct);
        match p.save(&self.env) {
            Ok(()) => {
                self.saved_volume = pct;
                self.volume_dirty = None;
            }
            Err(e) => {
                dlog!("volume non enregistré : {e:#}");
                self.volume_dirty = Some(Instant::now());
            }
        }
    }

    fn current(&self) -> Option<&Entry> {
        self.cur.and_then(|i| self.queue.get(i))
    }

    fn meta_of(&self, e: &Entry) -> MetaInfo {
        let art = e
            .info
            .cover
            .as_deref()
            .map(|c| cover::path(&self.env, c))
            .filter(|p| p.is_file())
            .map(|p| cover::file_url(&p));
        MetaInfo {
            seq: e.seq,
            title: e.info.title.clone(),
            artist: e.info.artist.clone(),
            album: e.info.album.clone(),
            album_artist: e.info.album_artist.clone(),
            length_us: (e.info.duration * 1_000_000) as i64,
            art_url: art,
            quality: e.info.quality(),
            starred: e.info.starred,
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            status: self.status().mpris(),
            meta: self.current().map(|e| self.meta_of(e)),
            volume: self.volume(),
        }
    }

    /// Met à jour l'état partagé et signale les changements sur D-Bus.
    fn publish(&mut self) {
        let new = self.snapshot();
        self.track_volume(new.volume);
        let old = {
            let mut g = self.shared.lock().unwrap_or_else(|e| e.into_inner());
            if *g == new {
                return;
            }
            std::mem::replace(&mut *g, new.clone())
        };
        if let Some(conn) = &self.conn {
            if let Err(e) = mpris::emit_changes(conn, &old, &new) {
                dlog!("MPRIS : signal impossible : {e}");
            }
        }
    }

    fn now(&self) -> Now {
        let status = self.status();
        let Some(e) = self.current() else {
            return Now {
                state: status.word().into(),
                volume: self.volume(),
                queue_len: self.queue.len(),
                ..Now::default()
            };
        };
        let cover = e
            .info
            .cover
            .as_deref()
            .map(|c| cover::path(&self.env, c))
            .filter(|p| p.is_file())
            .map(|p| p.to_string_lossy().to_string());
        Now {
            state: status.word().into(),
            id: e.info.id.clone(),
            title: e.info.title.clone(),
            artist: e.info.artist.clone(),
            album: e.info.album.clone(),
            album_artist: e.info.album_artist.clone(),
            duration: e.info.duration as f64,
            position: self.position(),
            quality: e.info.quality(),
            cover,
            index: self.cur.map_or(0, |i| i + 1),
            queue_len: self.queue.len(),
            volume: self.volume(),
            starred: e.info.starred,
        }
    }

    // ---- requêtes ----

    fn handle(&mut self, req: Request) -> Response {
        let r: Res<()> = match req {
            Request::Play {
                mut tracks,
                shuffle,
            } => {
                if shuffle {
                    rules::shuffle(&mut tracks, rules::random_below);
                }
                self.load(tracks, true)
            }
            Request::Queue { tracks } => {
                let replace = self.queue.is_empty();
                self.load(tracks, replace)
            }
            Request::Toggle => match self.status() {
                Status::Playing => self.set_pause(true),
                Status::Paused => self.set_pause(false),
                Status::Stopped => Err("Rien en lecture".into()),
            },
            Request::Pause => self.require_track().and_then(|_| self.set_pause(true)),
            Request::Resume => self.require_track().and_then(|_| self.set_pause(false)),
            Request::Next => self.next(),
            Request::Prev => self.prev(),
            Request::Stop => self.stop(),
            Request::Now => return Response::now(self.now()),
            Request::Quit => {
                self.quit = true;
                Ok(())
            }
            Request::Seek { offset_us } => self.seek_relative(offset_us),
            Request::SetPosition { track, position_us } => self.seek_absolute(track, position_us),
            Request::SetStarred { id, starred } => {
                for e in self.queue.iter_mut().filter(|e| e.info.id == id) {
                    e.info.starred = starred;
                }
                Ok(())
            }
            Request::SetVolume { volume } => {
                let v = (volume.clamp(0.0, f64::from(prefs::MAX_VOLUME) / 100.0) * 100.0).round();
                self.mpv.set_property("volume", v).map_err(mpv_err)
            }
        };
        self.publish();
        match r {
            Ok(()) => Response::ok(),
            Err(e) => Response::err(e),
        }
    }

    fn require_track(&self) -> Res<()> {
        if self.cur.is_some() {
            Ok(())
        } else {
            Err("Rien en lecture".into())
        }
    }

    fn set_pause(&mut self, pause: bool) -> Res<()> {
        self.mpv.set_property("pause", pause).map_err(mpv_err)
    }

    /// Charge des morceaux dans mpv : remplace la file, ou l'allonge.
    fn load(&mut self, tracks: Vec<TrackInfo>, replace: bool) -> Res<()> {
        if tracks.is_empty() {
            return Err("aucun morceau".into());
        }
        let mut first = replace;
        if replace {
            self.queue.clear();
            self.cur = None;
            self.clock = None;
        }
        self.finish_candidate = None;
        for info in tracks {
            let url = self.client.stream_url(&info.id);
            let mode = if first { "replace" } else { "append" };
            self.mpv
                .command("loadfile", &[url.expose(), mode])
                .map_err(|e| format!("lecture impossible : {e}"))?;
            if first {
                first = false;
                self.set_pause(false)?;
            }
            self.next_seq += 1;
            self.queue.push(Entry {
                seq: self.next_seq,
                info,
            });
        }
        self.reconcile();
        Ok(())
    }

    fn next(&mut self) -> Res<()> {
        self.require_track()?;
        if let Some(e) = self.current() {
            let pos = self.position();
            if rules::is_skip(pos, e.info.duration as f64) {
                let id = e.info.id.clone();
                match db::record_skip(&self.env, &id, now_secs()) {
                    Ok(true) => dlog!("morceau passé : {id}"),
                    Ok(false) => dlog!("morceau passé non noté (pas de copie locale) : {id}"),
                    Err(e) => dlog!("morceau passé non noté : {e:#}"),
                }
            }
        }
        // `force` : sur le dernier morceau, la lecture se termine.
        self.mpv
            .command("playlist-next", &["force"])
            .map_err(mpv_err)
    }

    fn prev(&mut self) -> Res<()> {
        self.require_track()?;
        if self.cur == Some(0) || self.position() > RESTART_AFTER_SECS {
            self.mpv
                .command("seek", &["0", "absolute"])
                .map_err(mpv_err)
        } else {
            self.mpv
                .command("playlist-prev", &["force"])
                .map_err(mpv_err)
        }
    }

    fn stop(&mut self) -> Res<()> {
        self.queue.clear();
        self.cur = None;
        self.clock = None;
        self.finish_candidate = None;
        self.mpv.command("stop", &[]).map_err(mpv_err)?;
        let _ = self.mpv.set_property("pause", false);
        Ok(())
    }

    fn seek_relative(&mut self, offset_us: i64) -> Res<()> {
        self.require_track()?;
        let target = (self.position() + offset_us as f64 / 1e6).max(0.0);
        self.mpv
            .command(
                "seek",
                &[&format!("{}", offset_us as f64 / 1e6), "relative"],
            )
            .map_err(mpv_err)?;
        self.signal_seeked(target);
        Ok(())
    }

    fn seek_absolute(&mut self, track: u64, position_us: i64) -> Res<()> {
        let Some(e) = self.current() else {
            return Ok(());
        };
        if e.seq != track || position_us < 0 {
            return Ok(());
        }
        let secs = position_us as f64 / 1e6;
        self.mpv
            .command("seek", &[&format!("{secs}"), "absolute"])
            .map_err(mpv_err)?;
        self.signal_seeked(secs);
        Ok(())
    }

    fn signal_seeked(&self, secs: f64) {
        if let Some(conn) = &self.conn {
            let _ = mpris::emit_seeked(conn, (secs * 1e6) as i64);
        }
    }

    // ---- événements de mpv ----

    fn on_mpv(&mut self, ev: MpvEvent) {
        match ev {
            MpvEvent::Prop => self.reconcile(),
            MpvEvent::EndFile(reason) => {
                if reason == libmpv2::mpv_end_file_reason::Error {
                    self.on_failure("fichier illisible".into());
                }
                self.reconcile();
            }
            MpvEvent::Failure(e) => self.on_failure(e),
        }
    }

    /// Un flux n'a pas pu s'ouvrir : mpv passe au suivant ; on cherche seulement
    /// si le serveur a refusé le jeton.
    fn on_failure(&mut self, detail: String) {
        let title = self
            .current()
            .map(|e| e.info.title.clone())
            .unwrap_or_default();
        dlog!("lecture impossible ({title}) : {detail}");
        if self
            .last_ping
            .is_some_and(|t| t.elapsed() < Duration::from_secs(3))
        {
            return;
        }
        self.last_ping = Some(Instant::now());
        let client = self.client.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || match client.ping() {
            Err(crate::api::ApiError::AuthRefused(m)) => {
                dlog!("authentification refusée : {m}");
                let _ = tx.send(Msg::AuthRefused);
            }
            Err(e) => dlog!("test du serveur : {e}"),
            Ok(()) => {}
        });
    }

    /// Relit l'état de mpv (source de vérité) et suit le changement de morceau.
    fn reconcile(&mut self) {
        let idle = self
            .mpv
            .get_property::<bool>("idle-active")
            .unwrap_or(false);
        if idle {
            if self.cur.is_some() && self.finish_candidate.is_none() {
                self.finish_candidate = Some(Instant::now());
            }
        } else {
            self.finish_candidate = None;
            let pos = self
                .mpv
                .get_property::<i64>("playlist-pos")
                .ok()
                .filter(|p| *p >= 0)
                .map(|p| p as usize);
            if let Some(p) = pos {
                if Some(p) != self.cur && p < self.queue.len() {
                    self.start_track(p);
                }
            }
        }
        self.publish();
    }

    fn start_track(&mut self, index: usize) {
        self.cur = Some(index);
        // La copie locale est plus fraîche que la file (favoris changés ailleurs).
        if let Some(id) = self.queue.get(index).map(|e| e.info.id.clone()) {
            if let Ok(Some(s)) = db::track_starred(&self.env, &id) {
                self.queue[index].info.starred = s;
            }
        }
        let Some(e) = self.queue.get(index) else {
            return;
        };
        let (id, cover_id, duration) = (e.info.id.clone(), e.info.cover.clone(), e.info.duration);
        dlog!(
            "lecture {}/{} : {} - {}",
            index + 1,
            self.queue.len(),
            e.info.artist,
            e.info.title
        );
        self.clock = Some(ScrobbleClock::new(duration as f64));
        self.last_tick = Instant::now();
        self.scrobble(&id, false);
        if let Some(c) = cover_id {
            self.fetch_cover(c);
        }
    }

    fn scrobble(&self, id: &str, submission: bool) {
        let client = self.client.clone();
        let id = id.to_string();
        std::thread::spawn(move || {
            let r = client.call(
                "scrobble",
                &[("id", id), ("submission", submission.to_string())],
            );
            if let Err(e) = r {
                dlog!("écoute non signalée : {e}");
            }
        });
    }

    fn fetch_cover(&mut self, id: String) {
        if cover::path(&self.env, &id).is_file() || !self.covers_pending.insert(id.clone()) {
            return;
        }
        let (env, client, tx) = (self.env.clone(), self.client.clone(), self.tx.clone());
        std::thread::spawn(move || match cover::ensure(&env, &client, &id) {
            Ok(_) => {
                let _ = tx.send(Msg::Cover);
            }
            Err(e) => dlog!("pochette ignorée : {e:#}"),
        });
    }

    // ---- synchronisation de la copie ----

    /// Lance une synchronisation incrémentale détachée (ignorée si une autre tourne :
    /// le processus lancé prend le verrou). Rien après un refus d'authentification.
    fn spawn_sync(&self) {
        if store::auth_refused_marked(&self.env) || autosync::is_running() {
            return;
        }
        if let Err(e) = autosync::spawn_background(false) {
            dlog!("synchronisation non lancée : {e:#}");
        }
    }

    /// Au démarrage : synchronise si la copie manque ou a plus de 15 minutes.
    fn sync_at_start(&self) {
        let age = db::open_existing(&self.env)
            .ok()
            .flatten()
            .map(|c| autosync::age_secs(&c));
        if age.is_none_or(|a| a > autosync::daemon_fresh_secs()) {
            self.spawn_sync();
        }
    }

    // ---- cadence ----

    fn tick(&mut self) {
        let now = Instant::now();
        if now >= self.next_sync {
            self.next_sync = now + Duration::from_secs(autosync::daemon_every_secs().max(1));
            self.spawn_sync();
        }
        let dt = now.duration_since(self.last_tick).as_secs_f64().min(2.0);
        self.last_tick = now;

        let status = self.status();
        let moving = status == Status::Playing
            && !self.mpv.get_property::<bool>("core-idle").unwrap_or(false);
        if let Some(clock) = &mut self.clock {
            if clock.advance(dt, moving) {
                if let Some(id) = self.current().map(|e| e.info.id.clone()) {
                    self.scrobble(&id, true);
                }
            }
        }

        if status == Status::Playing {
            self.idle_since = None;
        } else {
            let since = *self.idle_since.get_or_insert(now);
            if now.duration_since(since) >= self.idle_timeout {
                dlog!("inactif depuis trop longtemps, arrêt");
                self.quit = true;
            }
        }

        if let Some(t) = self.finish_candidate {
            if now.duration_since(t) >= FINISH_GRACE {
                if self
                    .mpv
                    .get_property::<bool>("idle-active")
                    .unwrap_or(false)
                {
                    dlog!("file terminée, arrêt");
                    self.quit = true;
                } else {
                    self.finish_candidate = None;
                }
            }
        }

        if TERMINATED.load(Ordering::SeqCst) {
            dlog!("signal reçu, arrêt");
            self.quit = true;
        }
    }

    fn auth_refused(&mut self) {
        store::mark_auth_refused(&self.env);
        notify::reconnect_required();
        let _ = self.mpv.command("stop", &[]);
        self.queue.clear();
        self.cur = None;
        self.quit = true;
    }

    fn run(&mut self, rx: Receiver<Msg>) {
        loop {
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(Msg::Req(req, reply)) => {
                    let r = self.handle(req);
                    let _ = reply.send(r);
                }
                Ok(Msg::Mpv(ev)) => self.on_mpv(ev),
                Ok(Msg::Cover) => self.publish(),
                Ok(Msg::AuthRefused) => self.auth_refused(),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            self.tick();
            self.publish();
            self.flush_volume(self.quit);
            if self.quit {
                break;
            }
        }
    }
}

/// Durée d'inactivité tolérée : 10 minutes (`POSTE_MUSIC_IDLE_SECS` la
/// remplace, pour les tests).
fn idle_timeout() -> Duration {
    std::env::var("POSTE_MUSIC_IDLE_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map_or(IDLE_TIMEOUT, Duration::from_secs)
}

/// Point d'entrée du démon.
pub fn run(env: &Env) -> Result<()> {
    // Fichiers créés en 0600 / 0700 dès l'origine.
    // SAFETY : `umask` ne touche qu'au masque du processus.
    unsafe {
        libc::umask(0o077);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
    }
    dlog!("démarrage du démon (pid {})", std::process::id());

    let dir = proto::runtime_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("création de {}", dir.display()))?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    let Some(_lock) = acquire_lock(&proto::lock_path()?)? else {
        dlog!("un autre démon tourne déjà");
        return Ok(());
    };

    let client = match state::open_client(env) {
        Ok(c) => c,
        Err(Failure::Internal(e)) => return Err(e),
        Err(f) => bail!("{f}"),
    };
    let client = Arc::new(client);

    let volume = Prefs::load(env).volume();
    let mpv = Arc::new(new_mpv(volume)?);
    let (tx, rx) = channel::<Msg>();
    spawn_events(&mpv, tx.clone())?;

    let shared = Arc::new(Mutex::new(Snapshot {
        volume: f64::from(volume) / 100.0,
        ..Snapshot::stopped()
    }));
    let conn = match mpris::start(tx.clone(), shared.clone(), mpv.clone()) {
        Ok(c) => {
            dlog!("MPRIS publié ({})", mpris::bus_name());
            Some(c)
        }
        Err(e) => {
            dlog!("MPRIS indisponible : {e}");
            None
        }
    };

    let sock = proto::socket_path()?;
    let _ = std::fs::remove_file(&sock);
    let listener =
        UnixListener::bind(&sock).with_context(|| format!("socket {}", sock.display()))?;
    std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600))?;
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let tx = tx.clone();
                std::thread::spawn(move || serve_connection(stream, tx));
            }
        });
    }

    let mut engine = Engine {
        env: env.clone(),
        mpv: mpv.clone(),
        client,
        tx,
        shared,
        conn,
        queue: Vec::new(),
        cur: None,
        next_seq: 0,
        clock: None,
        last_tick: Instant::now(),
        idle_since: Some(Instant::now()),
        finish_candidate: None,
        covers_pending: HashSet::new(),
        last_ping: None,
        idle_timeout: idle_timeout(),
        saved_volume: volume,
        volume_dirty: None,
        next_sync: Instant::now() + Duration::from_secs(autosync::daemon_every_secs().max(1)),
        quit: false,
    };
    engine.sync_at_start();
    engine.run(rx);

    // Sortie propre : plus de son, plus de service MPRIS, plus de socket.
    let _ = mpv.command("stop", &[]);
    let _ = std::fs::remove_file(&sock);
    let conn = engine.conn.take();
    drop(conn);
    let _ = std::fs::remove_file(proto::lock_path()?);
    dlog!("arrêt du démon");
    let _ = std::io::stderr().flush();
    std::process::exit(0);
}
