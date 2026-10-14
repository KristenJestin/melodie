//! Le démon de lecture (`melodie daemon`, sous-commande cachée) : un seul
//! processus, libmpv dans le même processus, contrôle par socket Unix, service
//! MPRIS, écoutes signalées à Navidrome.
//!
//! Un seul fil (le moteur) possède l'état de la file ; les autres (socket,
//! événements de mpv, D-Bus) lui envoient des messages.

use crate::autosync;
use crate::mpris::{self, MetaInfo, Snapshot};
use crate::notify;
use crate::queue;
use crate::rules::{self, ScrobbleClock};
use anyhow::{Context, Result, bail};
use libmpv2::events::{Event as MpvRaw, PropertyData};
use libmpv2::{Format, Mpv};
use melodie_core::address::Address;
use melodie_core::api::{Client, SecretUrl};
use melodie_core::cover;
use melodie_core::db;
use melodie_core::env::Env;
use melodie_core::prefs::{self, Prefs};
use melodie_core::proto::{self, Event, Now, QueueEntry, QueueView, Request, Response};
use melodie_core::resolve;
use melodie_core::state::{self, Failure};
use melodie_core::store;
use melodie_core::time::{now_secs, rfc3339};
use melodie_core::track::TrackInfo;
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{
    Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError, channel, sync_channel,
};
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
/// Events waiting for one subscriber; past this, it is too slow and let go
/// (the socket's own buffer comes on top).
const SUBSCRIBER_BUFFER: usize = 64;
/// Least time between two `position` events.
const POSITION_EVERY: Duration = Duration::from_secs(1);
/// Time left to subscribers to receive the last events when the daemon quits.
const FAREWELL: Duration = Duration::from_secs(1);
/// Longest wait for mpv's core to start its playlist before the daemon answers anyway.
const MPV_START: Duration = Duration::from_secs(2);

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
    /// What the last call to the server says of the connection (`ok`, `unreachable`).
    Link(&'static str),
    /// A client asks for the event stream.
    Watch(Subscriber),
}

/// A client of the event stream, fed by the engine without ever waiting.
pub struct Subscriber {
    tx: SyncSender<Arc<str>>,
    /// The same socket, to close it when the client is let go.
    stream: UnixStream,
    /// Disconnected once the writing thread has finished.
    done: Receiver<()>,
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

/// Verrou exclusif sur `melodie.lock` ; `None` si un autre démon le tient.
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
    let ao = std::env::var("MELODIE_AO").ok().filter(|s| !s.is_empty());
    build_mpv(volume, ao.as_deref())
}

fn build_mpv(volume: u32, ao: Option<&str>) -> Result<Mpv> {
    Mpv::with_initializer(|init| {
        init.set_option("vid", "no")?;
        init.set_option("audio-display", "no")?;
        init.set_option("gapless-audio", "weak")?;
        init.set_option("prefetch-playlist", "yes")?;
        init.set_option("cache", "yes")?;
        init.set_option("ytdl", "no")?;
        init.set_option("load-scripts", "no")?;
        init.set_option("idle", "yes")?;
        init.set_option("audio-client-name", "Mélodie")?;
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
        if let Some(ao) = ao {
            init.set_option("ao", ao)?;
        }
        Ok(())
    })
    .map_err(|e| anyhow::anyhow!("initialisation de libmpv : {e}"))
}

/// The event thread waits on the main handle, not on a second client: libmpv2 6.0.0
/// `create_client` frees the name before mpv reads it, and when that memory starts with a zero
/// byte mpv refuses the client (NULL), which the crate turns into a `NonNull` unchecked: an
/// abort, or undefined behaviour in release. Only this thread calls `mpv_wait_event` on it.
fn event_handle(mpv: &Arc<Mpv>) -> Arc<Mpv> {
    mpv.clone()
}

/// mpv's event thread. The receiver hears once that mpv's core started its playlist: as it
/// starts, the core sends the playlist back to its first entry (`prepare_playlist`), so a play
/// handled before would start at the first track instead of the one asked. mpv tells it with
/// its `idle` event, deprecated but sent to every client unless refused. That first `idle`
/// comes only while mpv has nothing loaded, so nothing may be loaded before the wait (a
/// "restore the queue at start" would bring the race back, and a 2 s stall).
fn spawn_events(mpv: &Arc<Mpv>, tx: Sender<Msg>) -> Result<Receiver<()>> {
    let ev = event_handle(mpv);
    for (id, name, fmt) in [
        (1, "playlist-pos", Format::Int64),
        (2, "pause", Format::Flag),
        (3, "volume", Format::Double),
        (4, "idle-active", Format::Flag),
    ] {
        ev.observe_property(name, fmt, id)
            .map_err(|e| anyhow::anyhow!("observation de {name} : {e}"))?;
    }
    let (started, heard) = channel();
    let mut started = Some(started);
    std::thread::spawn(move || {
        loop {
            let msg = match ev.wait_event(1.0) {
                None => continue,
                Some(Ok(MpvRaw::Deprecated(e)))
                    if e.event_id == libmpv2_sys::mpv_event_id_MPV_EVENT_IDLE =>
                {
                    if let Some(s) = started.take() {
                        let _ = s.send(());
                    }
                    continue;
                }
                Some(Ok(MpvRaw::PropertyChange { change, .. })) => {
                    let _ = matches!(change, PropertyData::Flag(_));
                    MpvEvent::Prop
                }
                Some(Ok(MpvRaw::EndFile(reason))) => MpvEvent::EndFile(reason),
                Some(Ok(MpvRaw::Shutdown)) => return,
                Some(Ok(_)) => continue,
                Some(Err(e)) => MpvEvent::Failure(e.to_string()),
            };
            if tx.send(Msg::Mpv(msg)).is_err() {
                return;
            }
        }
    });
    Ok(heard)
}

/// Turns a connection into the event stream: the engine queues lines, this
/// thread writes them, so a client that stops reading only holds up itself.
/// Serves the addresses MPRIS `OpenUri` hands over, one at a time and in order: asks the
/// server for the tracks, then gives the engine the request. The wait for the server is here,
/// never in the engine nor on the D-Bus thread. What fails is logged (it is never a stream
/// address); a refused token is told to the engine like a stream that could not open.
fn spawn_opener(env: Env, client: Arc<Client>, tx: Sender<Msg>) -> Sender<Address> {
    let (open, rx) = channel::<Address>();
    std::thread::spawn(move || {
        for addr in rx {
            match resolve::resolve(&client, &env, addr.kind, &addr.id) {
                Ok(tracks) => {
                    let (reply, _) = channel();
                    let req = rules::open_request(&addr, tracks);
                    if tx.send(Msg::Req(req, reply)).is_err() {
                        return;
                    }
                }
                Err(f) => {
                    // Without the server's address, which does not belong in a log.
                    let why = match &f {
                        Failure::Unreachable { detail, .. } => format!("{} : {detail}", f.state()),
                        other => other.to_string(),
                    };
                    dlog!("OpenUri {} {} : {why}", addr.kind.word(), addr.id);
                    if matches!(f, Failure::AuthRefused(_)) && tx.send(Msg::AuthRefused).is_err() {
                        return;
                    }
                }
            }
        }
    });
    open
}

fn stream_events(out: UnixStream, tx: &Sender<Msg>) {
    let (etx, erx) = sync_channel::<Arc<str>>(SUBSCRIBER_BUFFER);
    let (done_tx, done) = channel::<()>();
    let Ok(stream) = out.try_clone() else { return };
    if tx
        .send(Msg::Watch(Subscriber {
            tx: etx,
            stream,
            done,
        }))
        .is_err()
    {
        return;
    }
    let mut out = out;
    for line in erx {
        if out.write_all(line.as_bytes()).is_err() {
            break;
        }
    }
    drop(done_tx);
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
            Ok(Request::Watch) => return stream_events(out, &tx),
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
    /// Dernier volume écrit dans `state.toml` (pourcentage).
    saved_volume: u32,
    /// Depuis quand le volume actuel diffère de celui qui est écrit.
    volume_dirty: Option<Instant>,
    next_sync: Instant,
    quit: bool,
    subs: Vec<Subscriber>,
    /// Bumped at every change of the queue's tracks.
    queue_rev: u64,
    /// What was last announced on the event stream.
    seen: Seen,
    last_position: Option<Instant>,
    /// Connection state: `ok`, `unreachable` or `auth-refused`.
    link: &'static str,
    /// mpv takes `loadfile <url> insert-at <index>`, until it refuses it once.
    insert_at: bool,
}

/// What the event stream last said, to announce only what changed.
#[derive(Clone, Copy, PartialEq)]
struct Seen {
    status: Status,
    entry: Option<u64>,
    volume: f64,
    queue_rev: u64,
    link: &'static str,
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
        self.announce();
        let new = self.snapshot();
        self.track_volume(new.volume);
        let old = {
            let mut g = self.shared.lock().unwrap_or_else(|e| e.into_inner());
            if *g == new {
                return;
            }
            std::mem::replace(&mut *g, new.clone())
        };
        if let Some(conn) = &self.conn
            && let Err(e) = mpris::emit_changes(conn, &old, &new)
        {
            dlog!("MPRIS : signal impossible : {e}");
        }
    }

    fn entry_view(&self, e: &Entry) -> QueueEntry {
        QueueEntry {
            entry: e.seq,
            id: e.info.id.clone(),
            title: e.info.title.clone(),
            artist: e.info.artist.clone(),
            album: e.info.album.clone(),
            album_artist: e.info.album_artist.clone(),
            duration: e.info.duration as f64,
            quality: e.info.quality(),
            cover: e
                .info
                .cover
                .as_deref()
                .map(|c| cover::path(&self.env, c))
                .filter(|p| p.is_file())
                .map(|p| p.to_string_lossy().to_string()),
            cover_id: e.info.cover.clone(),
            starred: e.info.starred,
        }
    }

    fn queue_view(&self) -> QueueView {
        QueueView {
            current: self.cur,
            tracks: self.queue.iter().map(|e| self.entry_view(e)).collect(),
        }
    }

    // ---- event stream ----

    fn seen_now(&self) -> Seen {
        Seen {
            status: self.status(),
            entry: self.current().map(|e| e.seq),
            volume: self.volume(),
            queue_rev: self.queue_rev,
            link: self.link,
        }
    }

    /// Sends one event to every subscriber; one that cannot take it is let go.
    fn broadcast(&mut self, ev: &Event) {
        if self.subs.is_empty() {
            return;
        }
        let Ok(mut line) = proto::encode(ev) else {
            return;
        };
        line.push('\n');
        let line: Arc<str> = line.into();
        self.subs.retain(|s| match s.tx.try_send(line.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                dlog!("event subscriber too slow, let go");
                let _ = s.stream.shutdown(Shutdown::Both);
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        });
    }

    /// Announces what changed since the last call, queue first so that a
    /// client knows the tracks before it hears which one is current.
    fn announce(&mut self) {
        let new = self.seen_now();
        let old = std::mem::replace(&mut self.seen, new);
        if self.subs.is_empty() || old == new {
            return;
        }
        if new.queue_rev != old.queue_rev {
            self.broadcast(&Event::Queue {
                queue: self.queue_view(),
            });
        }
        if new.entry != old.entry {
            self.announce_track();
        }
        if new.status != old.status {
            self.broadcast(&Event::State {
                state: new.status.word().into(),
                position: self.position(),
            });
        }
        if new.volume != old.volume {
            self.broadcast(&Event::Volume { volume: new.volume });
        }
        if new.link != old.link {
            self.broadcast(&Event::Connection {
                state: new.link.into(),
            });
        }
    }

    fn announce_track(&mut self) {
        let track = self.current().map(|e| self.entry_view(e));
        self.broadcast(&Event::Track {
            index: self.cur.filter(|_| track.is_some()),
            track,
        });
    }

    /// The position while playing, at most once a second.
    fn announce_position(&mut self) {
        if self.subs.is_empty()
            || self.status() != Status::Playing
            || self
                .last_position
                .is_some_and(|t| t.elapsed() < POSITION_EVERY)
        {
            return;
        }
        self.last_position = Some(Instant::now());
        let duration = self.current().map_or(0.0, |e| e.info.duration as f64);
        self.broadcast(&Event::Position {
            position: self.position(),
            duration,
        });
    }

    /// A new client: what changed is announced to the others first, then the
    /// newcomer gets a snapshot to start from.
    fn subscribe(&mut self, sub: Subscriber) {
        self.announce();
        let ev = Event::Snapshot {
            now: self.now(),
            queue: self.queue_view(),
            connection: self.link.into(),
        };
        let Ok(mut line) = proto::encode(&ev) else {
            return;
        };
        line.push('\n');
        if sub.tx.try_send(line.into()).is_ok() {
            self.subs.push(sub);
        }
    }

    /// Lets the subscribers receive the last events, a short while at most.
    fn farewell(&mut self) {
        let end = Instant::now() + FAREWELL;
        let subs = std::mem::take(&mut self.subs);
        let waits: Vec<Receiver<()>> = subs.into_iter().map(|s| s.done).collect();
        for done in waits {
            let left = end.saturating_duration_since(Instant::now());
            let _ = done.recv_timeout(left);
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
                start,
            } => {
                if shuffle {
                    rules::shuffle(&mut tracks, rules::random_below);
                }
                self.load(tracks, true, if shuffle { 0 } else { start })
            }
            Request::Queue { tracks, next } => self.insert(tracks, next),
            Request::QueueList => return Response::queue(self.queue_view()),
            Request::QueuePlay { index } => self.queue_play(index),
            Request::QueueMove { from, to } => self.queue_move(from, to),
            Request::QueueRemove { index } => self.queue_remove(index),
            Request::QueueClearAfter => self.queue_clear_after(),
            Request::Watch => Err("watch is only answered with the event stream".into()),
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
                self.broadcast(&Event::Starred { id, starred });
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

    /// Loads tracks into mpv: replaces the queue, or adds to its end. A queue replaced plays from
    /// `start` (from 0).
    fn load(&mut self, tracks: Vec<TrackInfo>, replace: bool, start: usize) -> Res<()> {
        if tracks.is_empty() {
            return Err("aucun morceau".into());
        }
        if start >= tracks.len() {
            return Err(format!("No track at position {} of the list", start + 1));
        }
        let mut first = replace;
        if replace {
            self.queue.clear();
            self.cur = None;
            self.clock = None;
        }
        self.finish_candidate = None;
        self.queue_rev += 1;
        let base = self.queue.len();
        // mpv plays the first file it loads: `start` goes in first, and the tracks before it are
        // put in front of it afterwards, so the first track never starts for an instant.
        let mut tracks: Vec<_> = tracks.into_iter().map(Some).collect();
        let order: Vec<usize> = (start..tracks.len()).chain(0..start).collect();
        for i in order {
            let Some(info) = tracks[i].take() else {
                continue;
            };
            let url = self.client.stream_url(&info.id);
            let pos = base + i;
            if i < start {
                self.load_at(&url, pos)?;
            } else {
                let mode = if first { "replace" } else { "append" };
                self.mpv
                    .command("loadfile", &[url.expose(), mode])
                    .map_err(|e| format!("lecture impossible : {e}"))?;
            }
            if first {
                first = false;
                self.set_pause(false)?;
            }
            self.next_seq += 1;
            let entry = Entry {
                seq: self.next_seq,
                info,
            };
            if i < start {
                self.queue.insert(pos, entry);
            } else {
                self.queue.push(entry);
            }
        }
        self.reconcile();
        Ok(())
    }

    /// Puts a track at `pos` of mpv's playlist: in one command where mpv knows `insert-at`
    /// (0.38 and later), else appended then moved.
    fn load_at(&mut self, url: &SecretUrl, pos: usize) -> Res<()> {
        let at = pos.to_string();
        if self.insert_at {
            match self
                .mpv
                .command("loadfile", &[url.expose(), "insert-at", &at])
            {
                Ok(()) => return Ok(()),
                // an older mpv refuses the flag and loads nothing
                Err(_) => self.insert_at = false,
            }
        }
        self.mpv
            .command("loadfile", &[url.expose(), "append"])
            .map_err(|e| format!("lecture impossible : {e}"))?;
        let end = self.queue.len().to_string();
        self.mpv
            .command("playlist-move", &[&end, &at])
            .map_err(mpv_err)
    }

    /// Adds tracks right after the current one (`next`) or at the end; starts
    /// playing the first of them if nothing plays.
    pub fn insert(&mut self, tracks: Vec<TrackInfo>, next: bool) -> Res<()> {
        if self.queue.is_empty() {
            return self.load(tracks, true, 0);
        }
        if tracks.is_empty() {
            return Err("aucun morceau".into());
        }
        let at = queue::insert_at(self.cur, self.queue.len(), next);
        let start = self.cur.is_none();
        self.queue_rev += 1;
        for (k, info) in tracks.into_iter().enumerate() {
            let url = self.client.stream_url(&info.id);
            self.mpv
                .command("loadfile", &[url.expose(), "append"])
                .map_err(|e| format!("lecture impossible : {e}"))?;
            let (end, pos) = (self.queue.len(), at + k);
            if pos != end {
                self.mpv
                    .command("playlist-move", &[&end.to_string(), &pos.to_string()])
                    .map_err(mpv_err)?;
            }
            self.next_seq += 1;
            self.queue.insert(
                pos,
                Entry {
                    seq: self.next_seq,
                    info,
                },
            );
        }
        if start {
            self.queue_play(at)?;
        }
        Ok(())
    }

    fn check_index(&self, index: usize) -> Res<()> {
        if index < self.queue.len() {
            Ok(())
        } else {
            Err(format!("No track at position {} of the queue", index + 1))
        }
    }

    fn queue_play(&mut self, index: usize) -> Res<()> {
        self.check_index(index)?;
        if self.cur == Some(index) {
            self.mpv
                .command("seek", &["0", "absolute"])
                .map_err(mpv_err)?;
            return self.set_pause(false);
        }
        self.finish_candidate = None;
        self.mpv
            .command("playlist-play-index", &[&index.to_string()])
            .map_err(mpv_err)?;
        self.set_pause(false)?;
        // mpv's `playlist-pos` already names this entry: no second start on the next reconcile.
        self.start_track(index);
        Ok(())
    }

    fn queue_move(&mut self, from: usize, to: usize) -> Res<()> {
        self.check_index(from)?;
        self.check_index(to)?;
        if from == to {
            return Ok(());
        }
        let target = queue::mpv_move_target(from, to);
        self.mpv
            .command("playlist-move", &[&from.to_string(), &target.to_string()])
            .map_err(mpv_err)?;
        let e = self.queue.remove(from);
        self.queue.insert(to, e);
        // The current track keeps playing; only its index changes.
        self.cur = queue::moved(self.cur, from, to);
        self.queue_rev += 1;
        Ok(())
    }

    fn queue_remove(&mut self, index: usize) -> Res<()> {
        self.check_index(index)?;
        let len_before = self.queue.len();
        self.mpv
            .command("playlist-remove", &[&index.to_string()])
            .map_err(mpv_err)?;
        self.queue.remove(index);
        self.queue_rev += 1;
        let cur = queue::removed(self.cur, index, len_before);
        if self.cur != Some(index) {
            self.cur = cur;
            return Ok(());
        }
        // mpv goes on with the next entry, at the same index (or stops past the end):
        // a new track for us even though the index did not change.
        self.cur = None;
        self.clock = None;
        self.finish_candidate = None;
        if let Some(i) = cur {
            self.start_track(i);
        }
        Ok(())
    }

    fn queue_clear_after(&mut self) -> Res<()> {
        let Some(c) = self.cur else {
            return Err("Rien en lecture".into());
        };
        if c + 1 >= self.queue.len() {
            return Ok(());
        }
        self.queue_rev += 1;
        for i in (c + 1..self.queue.len()).rev() {
            self.mpv
                .command("playlist-remove", &[&i.to_string()])
                .map_err(mpv_err)?;
            self.queue.truncate(i);
        }
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
        self.queue_rev += 1;
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
            Err(melodie_core::api::ApiError::AuthRefused(m)) => {
                dlog!("authentification refusée : {m}");
                let _ = tx.send(Msg::AuthRefused);
            }
            Err(e) => {
                dlog!("test du serveur : {e}");
                let _ = tx.send(Msg::Link("unreachable"));
            }
            Ok(()) => {
                let _ = tx.send(Msg::Link("ok"));
            }
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
            if let Some(p) = pos
                && Some(p) != self.cur
                && p < self.queue.len()
            {
                self.start_track(p);
            }
        }
        self.publish();
    }

    fn start_track(&mut self, index: usize) {
        self.cur = Some(index);
        // La copie locale est plus fraîche que la file (favoris changés ailleurs).
        if let Some(id) = self.queue.get(index).map(|e| e.info.id.clone())
            && let Ok(Some(s)) = db::track_starred(&self.env, &id)
            && s != self.queue[index].info.starred
        {
            self.queue[index].info.starred = s;
            self.broadcast(&Event::Starred { id, starred: s });
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
        let tx = self.tx.clone();
        let id = id.to_string();
        std::thread::spawn(move || {
            let r = client.call(
                "scrobble",
                &[("id", id), ("submission", submission.to_string())],
            );
            match r {
                Ok(_) => {
                    let _ = tx.send(Msg::Link("ok"));
                }
                Err(e) => {
                    dlog!("écoute non signalée : {e}");
                    if matches!(e, melodie_core::api::ApiError::Unreachable(_)) {
                        let _ = tx.send(Msg::Link("unreachable"));
                    }
                }
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
        if let Some(clock) = &mut self.clock
            && clock.advance(dt, moving)
            && let Some(id) = self.current().map(|e| e.info.id.clone())
        {
            self.scrobble(&id, true);
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

        if let Some(t) = self.finish_candidate
            && now.duration_since(t) >= FINISH_GRACE
        {
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

        if TERMINATED.load(Ordering::SeqCst) {
            dlog!("signal reçu, arrêt");
            self.quit = true;
        }
    }

    fn auth_refused(&mut self) {
        self.link = "auth-refused";
        self.queue_rev += 1;
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
                Ok(Msg::Cover) => {
                    // Its local path is part of the track: said again once the file is there.
                    self.announce_track();
                    self.publish();
                }
                Ok(Msg::AuthRefused) => self.auth_refused(),
                Ok(Msg::Link(state)) => self.link = state,
                Ok(Msg::Watch(sub)) => self.subscribe(sub),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            self.tick();
            self.publish();
            self.announce_position();
            self.flush_volume(self.quit);
            if self.quit {
                break;
            }
        }
    }
}

/// Durée d'inactivité tolérée : 10 minutes (`MELODIE_IDLE_SECS` la
/// remplace, pour les tests).
fn idle_timeout() -> Duration {
    std::env::var("MELODIE_IDLE_SECS")
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
    let mpv_started = spawn_events(&mpv, tx.clone())?;

    let shared = Arc::new(Mutex::new(Snapshot {
        volume: f64::from(volume) / 100.0,
        ..Snapshot::stopped()
    }));
    let open = spawn_opener(env.clone(), client.clone(), tx.clone());
    let conn = match mpris::start(tx.clone(), shared.clone(), mpv.clone(), open) {
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

    // Bounded: an mpv that no longer sends `idle` must not keep the daemon from answering.
    // This wiring is untested (`run` needs a real session); `spawn_events` is.
    if mpv_started.recv_timeout(MPV_START).is_err() {
        dlog!("mpv did not say its core started");
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
        subs: Vec::new(),
        queue_rev: 0,
        seen: Seen {
            status: Status::Stopped,
            entry: None,
            volume: f64::from(volume) / 100.0,
            queue_rev: 0,
            link: "ok",
        },
        last_position: None,
        insert_at: true,
        link: "ok",
    };
    engine.sync_at_start();
    engine.run(rx);

    // Sortie propre : plus de son, plus de service MPRIS, plus de socket.
    let _ = mpv.command("stop", &[]);
    let _ = std::fs::remove_file(&sock);
    engine.farewell();
    // A call still being served holds the connection, such as the `OpenUri` whose refused token
    // made the daemon quit: wait for its answer to be written, or the caller is disconnected
    // from the bus without one. Bounded, so nothing can keep the daemon from quitting.
    if let Some(conn) = engine.conn.take() {
        let (closed, wait) = channel::<()>();
        std::thread::spawn(move || {
            conn.graceful_shutdown();
            let _ = closed.send(());
        });
        let _ = wait.recv_timeout(FAREWELL);
    }
    let _ = std::fs::remove_file(proto::lock_path()?);
    dlog!("arrêt du démon");
    let _ = std::io::stderr().flush();
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn null_mpv() -> Arc<Mpv> {
        Arc::new(
            Mpv::with_initializer(|init| {
                init.set_option("vid", "no")?;
                init.set_option("ao", "null")?;
                Ok(())
            })
            .expect("libmpv"),
        )
    }

    /// libmpv2 6.0.0 frees the name it gives `mpv_create_client` before mpv reads it: mpv
    /// named the client from freed memory, and refused it (NULL, then an abort) whenever
    /// that memory began with a zero byte.
    #[test]
    fn the_event_handle_is_not_named_from_freed_memory() {
        let mpv = null_mpv();
        let ev = event_handle(&mpv);
        // SAFETY: the handle is alive, and mpv returns its own NUL-terminated copy of the name.
        let name = unsafe {
            std::ffi::CStr::from_ptr(libmpv2_sys::mpv_client_name(ev.ctx.as_ptr()))
                .to_string_lossy()
                .into_owned()
        };
        assert!(
            name == "main" || name == "melodie_events",
            "client named {name:?}"
        );
    }

    /// Lets the held core go: written by `now`, and when dropped, so a test that panics before
    /// it does not leave `Mpv`'s drop joining a core stuck in the hold. Declare it after the mpv
    /// it releases, so it is dropped first.
    struct Release(std::path::PathBuf);

    impl Release {
        fn now(&self) {
            std::fs::write(&self.0, "").unwrap();
        }
    }

    impl Drop for Release {
        fn drop(&mut self) {
            let _ = std::fs::write(&self.0, "");
        }
    }

    /// An mpv whose core is held before it starts its playlist, until `release` exists: mpv
    /// waits for the first lines of its scripts to run before it goes on. The hold gives up
    /// after about ten seconds on its own, in case even the guard did not run.
    fn held_mpv(dir: &std::path::Path) -> (Arc<Mpv>, Release) {
        let release = dir.join("release");
        let script = dir.join("hold.lua");
        std::fs::write(
            &script,
            format!(
                "local t = os.time()\nwhile not io.open({:?}) and os.time() - t < 10 do os.execute('sleep 0.01') end\n",
                release.to_str().unwrap()
            ),
        )
        .unwrap();
        let mpv = Mpv::with_initializer(|init| {
            init.set_option("vid", "no")?;
            init.set_option("ao", "null")?;
            init.set_option("idle", "yes")?;
            init.set_option("load-scripts", "no")?;
            init.set_option("scripts", script.to_str().unwrap())?;
            Ok(())
        })
        .expect("libmpv");
        (Arc::new(mpv), Release(release))
    }

    /// Two endless silent tracks, the second one asked first, as `load` does with a start of
    /// 1; appended then moved, which every mpv knows.
    fn load_from_the_second(mpv: &Mpv) {
        let silence = "av://lavfi:anullsrc";
        mpv.command("loadfile", &[silence, "replace"]).unwrap();
        mpv.command("loadfile", &[silence, "append"]).unwrap();
        mpv.command("playlist-move", &["1", "0"]).unwrap();
        assert_eq!(mpv.get_property::<i64>("playlist-pos").unwrap(), 1);
    }

    /// The entry mpv really started.
    fn playing(mpv: &Mpv) -> i64 {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let p = mpv.get_property::<i64>("playlist-playing-pos").unwrap();
            if p >= 0 || Instant::now() > deadline {
                return p;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The cause of #91: mpv's core, as it starts, sends its playlist back to the first entry,
    /// so what was loaded before plays from the first track whatever was asked. A canary on
    /// mpv, not on Mélodie: if a future mpv keeps `current` in `prepare_playlist`, it fails
    /// while the daemon did not regress.
    #[test]
    fn mpv_plays_from_the_first_entry_what_was_loaded_before_its_core_started() {
        let dir = tempfile::tempdir().unwrap();
        let (mpv, release) = held_mpv(dir.path());
        load_from_the_second(&mpv);
        release.now();
        assert_eq!(playing(&mpv), 0);
    }

    /// #91: no request is handled before mpv's core started its playlist, so a play from a
    /// start is never sent back to the first track.
    #[test]
    fn the_daemon_hears_when_mpv_started_its_playlist_and_not_before() {
        let dir = tempfile::tempdir().unwrap();
        let (mpv, release) = held_mpv(dir.path());
        let (tx, _rx) = channel();
        let started = spawn_events(&mpv, tx).unwrap();
        assert!(
            started.recv_timeout(Duration::from_millis(500)).is_err(),
            "heard before the core started"
        );
        release.now();
        started
            .recv_timeout(Duration::from_secs(10))
            .expect("never heard that the core started");
        load_from_the_second(&mpv);
        assert_eq!(playing(&mpv), 1);
    }

    /// The daemon has no window, terminal or key bindings: mpv's built-in Lua scripts (OSC,
    /// console, stats…) only make every start heavier.
    #[test]
    fn the_daemon_loads_no_mpv_scripts() {
        let mpv = build_mpv(100, Some("null")).expect("libmpv");
        assert!(
            !mpv.get_property::<bool>("load-scripts")
                .expect("load-scripts")
        );
    }
}
