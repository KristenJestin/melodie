//! Côté client des commandes de lecture : parle au démon par le socket, le
//! démarre au besoin pour `play` et `queue`.

use crate::notify;
use crate::rules;
use anyhow::{Context, Result, anyhow, bail};
use melodie_core::address::{Address, Place};
use melodie_core::api::Client;
use melodie_core::env::Env;
use melodie_core::prefs::{self, Prefs};
use melodie_core::proto::{self, Now, QueueView, Request, Response};
use melodie_core::search::Kind;
use melodie_core::state::{self, Failure};
use melodie_core::track::clock;
use melodie_core::{db, mix, resolve};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

/// Temps laissé au démon pour répondre après son lancement.
const START_WAIT: Duration = Duration::from_secs(3);
/// Temps laissé à une requête (une grande file se charge morceau par morceau).
const REPLY_WAIT: Duration = Duration::from_secs(30);

/// Connexion au démon s'il tourne.
fn connect() -> Option<UnixStream> {
    UnixStream::connect(proto::socket_path().ok()?).ok()
}

/// Lance le démon, détaché (nouvelle session), journal écrasé à chaque démarrage.
fn spawn_daemon(env: &Env) -> Result<std::process::Child> {
    let log = proto::log_path(env);
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let out =
        std::fs::File::create(&log).with_context(|| format!("ouverture de {}", log.display()))?;
    let err = out.try_clone()?;
    let exe = std::env::current_exe().context("chemin du programme")?;
    let mut c = Command::new(exe);
    c.arg("daemon").stdin(Stdio::null()).stdout(out).stderr(err);
    // SAFETY : `setsid` est sûr entre `fork` et `exec`.
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    c.spawn().context("lancement du démon")
}

/// Connexion au démon, qu'on démarre s'il ne tourne pas.
fn ensure_daemon(env: &Env) -> Result<UnixStream> {
    if let Some(s) = connect() {
        return Ok(s);
    }
    let mut child = spawn_daemon(env)?;
    let start = Instant::now();
    loop {
        if let Some(s) = connect() {
            return Ok(s);
        }
        if let Ok(Some(status)) = child.try_wait() {
            // Un autre démon a pu prendre la place pendant ce temps.
            if let Some(s) = connect() {
                return Ok(s);
            }
            bail!(
                "le démon s'est arrêté ({status}) : voir {}",
                proto::log_path(env).display()
            );
        }
        if start.elapsed() > START_WAIT {
            bail!(
                "le démon ne répond pas : voir {}",
                proto::log_path(env).display()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn exchange(stream: &UnixStream, req: &Request) -> Result<Response> {
    proto::exchange(stream, req, REPLY_WAIT)
}

/// Échec présenté à l'utilisateur : message, notification (depuis le lanceur),
/// code de sortie.
pub fn report(f: &Failure) -> ExitCode {
    eprintln!("{f}");
    let summary = match f {
        Failure::NoLogin | Failure::AuthRefused(_) => "Navidrome : connexion requise",
        Failure::Unreachable { .. } => "Navidrome injoignable",
        Failure::NotFound(_) => "Mélodie : introuvable sur le serveur",
        Failure::Syncing => "Mélodie : copie de la bibliothèque en cours",
        Failure::Internal(_) => "Mélodie : erreur",
    };
    notify::send(summary, &f.to_string());
    ExitCode::from(f.exit_code())
}

/// Échec d'une commande qui peut viser le morceau en cours.
pub enum Oops {
    /// Le démon ne tourne pas, ou la file est vide.
    NothingPlaying,
    Failure(Failure),
}

impl From<Failure> for Oops {
    fn from(f: Failure) -> Self {
        Oops::Failure(f)
    }
}

impl From<anyhow::Error> for Oops {
    fn from(e: anyhow::Error) -> Self {
        Oops::Failure(Failure::Internal(e))
    }
}

/// Présente un échec : « Rien en lecture » (code 1) ou l'état de connexion.
pub fn report_oops(o: &Oops) -> ExitCode {
    match o {
        Oops::NothingPlaying => {
            eprintln!("Rien en lecture (le démon ne tourne pas ou la file est vide)");
            notify::send("Mélodie", "Rien en lecture");
            ExitCode::from(1)
        }
        Oops::Failure(f) => report(f),
    }
}

/// Morceau en cours d'après le démon, sans jamais le démarrer.
pub fn current_track() -> Result<Now, Oops> {
    let Some(stream) = connect() else {
        return Err(Oops::NothingPlaying);
    };
    match exchange(&stream, &Request::Now) {
        Ok(Response { now: Some(n), .. }) if n.state != "stopped" && !n.id.is_empty() => Ok(n),
        Ok(Response { now: Some(_), .. }) => Err(Oops::NothingPlaying),
        Ok(r) => Err(Oops::Failure(Failure::Internal(anyhow!(
            "{}",
            r.error
                .unwrap_or_else(|| "réponse inattendue du démon".into())
        )))),
        Err(e) => Err(Oops::Failure(Failure::Internal(e))),
    }
}

/// Prévient le démon (s'il tourne) qu'un morceau a changé d'état de favori.
pub fn tell_starred(id: &str, starred: bool) {
    if let Some(stream) = connect() {
        let _ = exchange(
            &stream,
            &Request::SetStarred {
                id: id.to_string(),
                starred,
            },
        );
    }
}

/// `play` et `queue` : prépare la liste, démarre le démon si besoin, l'envoie.
/// `next` puts the tracks right after the current one instead of at the end.
pub fn play_or_queue(
    env: &Env,
    kind: Kind,
    id: &str,
    shuffle: bool,
    queue: bool,
    next: bool,
) -> ExitCode {
    match try_play_or_queue(env, kind, id, shuffle, queue, next) {
        Ok(n) => {
            announce(n, queue);
            ExitCode::SUCCESS
        }
        Err(f) => report(&f),
    }
}

fn announce(n: usize, queue: bool) {
    println!(
        "{} {n} morceau{}",
        if queue {
            "Ajouté à la file :"
        } else {
            "Lecture :"
        },
        if n > 1 { "x" } else { "" }
    );
}

/// Connexion vérifiée avant d'envoyer quoi que ce soit au démon : le démon ne
/// démarre que si le serveur est joignable et le jeton accepté.
pub struct Ready {
    running: Option<UnixStream>,
}

pub fn prepare(env: &Env) -> Result<(Client, Ready), Failure> {
    let client = client(env)?;
    let ready = reach(env, &client)?;
    Ok((client, ready))
}

/// The client of the server, from the keyring.
fn client(env: &Env) -> Result<Client, Failure> {
    state::quick_check(env)?;
    state::open_client(env)
}

/// The daemon if it runs; else the server checked first, for the daemon to start only if it
/// can play.
fn reach(env: &Env, client: &Client) -> Result<Ready, Failure> {
    let running = connect();
    if running.is_none() {
        let r = client.ping().map_err(|e| state::from_api(e, client.url()));
        state::record(env, &r);
        r?;
    }
    Ok(Ready { running })
}

/// Envoie des morceaux au démon (démarré au besoin) : remplace la file ou l'allonge.
pub fn deliver(
    env: &Env,
    ready: Ready,
    tracks: Vec<melodie_core::track::TrackInfo>,
    shuffle: bool,
    queue: bool,
    next: bool,
) -> Result<usize, Failure> {
    let n = tracks.len();
    let req = if queue {
        Request::Queue { tracks, next }
    } else {
        Request::Play {
            tracks,
            shuffle,
            start: 0,
        }
    };
    send(env, ready, &req)?;
    Ok(n)
}

/// Sends a request that loads tracks to the daemon (started if needed).
fn send(env: &Env, ready: Ready, req: &Request) -> Result<UnixStream, Failure> {
    let stream = match ready.running {
        Some(s) => s,
        None => ensure_daemon(env)?,
    };
    let reply = exchange(&stream, req)?;
    if !reply.ok {
        return Err(Failure::Internal(anyhow!(
            "{}",
            reply.error.unwrap_or_else(|| "refusé par le démon".into())
        )));
    }
    Ok(stream)
}

fn try_play_or_queue(
    env: &Env,
    kind: Kind,
    id: &str,
    shuffle: bool,
    queue: bool,
    next: bool,
) -> Result<usize, Failure> {
    let (client, ready) = prepare(env)?;
    let result = resolve::resolve(&client, kind, id);
    state::record(env, &result);
    deliver(env, ready, result?, shuffle, queue, next)
}

/// Plays tracks from the one at `start` (from 0), shuffled or not, or queues them, in this
/// order: one request, the tracks read from the local copy, as a mix's are; the server is asked
/// only for those the copy does not hold yet, before the daemon is reached.
pub fn play_tracks(
    env: &Env,
    ids: Vec<String>,
    start: usize,
    shuffle: bool,
    queue: bool,
    next: bool,
) -> ExitCode {
    match try_play_tracks(env, ids, start, shuffle, queue, next) {
        Ok(n) => {
            announce(n, queue);
            ExitCode::SUCCESS
        }
        Err(f) => report(&f),
    }
}

fn try_play_tracks(
    env: &Env,
    ids: Vec<String>,
    start: usize,
    shuffle: bool,
    queue: bool,
    next: bool,
) -> Result<usize, Failure> {
    let known = match db::open_existing(env)? {
        Some(conn) => mix::infos(&conn, &ids)?,
        None => vec![None; ids.len()],
    };
    let client = client(env)?;
    let mut tracks = Vec::with_capacity(ids.len());
    for (id, info) in ids.iter().zip(known) {
        match info {
            Some(t) => tracks.push(t),
            None => {
                let r = resolve::resolve(&client, Kind::Track, id);
                state::record(env, &r);
                tracks.extend(r?);
            }
        }
    }
    let ready = reach(env, &client)?;
    let n = tracks.len();
    let req = if queue {
        Request::Queue { tracks, next }
    } else {
        Request::Play {
            tracks,
            shuffle,
            start,
        }
    };
    let stream = send(env, ready, &req)?;
    if start > 0 && !shuffle && !queue {
        started_at(&stream, start)?;
    }
    Ok(n)
}

/// A daemon older than `start` reads the request without it and plays from the first track:
/// said, rather than playing what was not asked.
fn started_at(stream: &UnixStream, start: usize) -> Result<(), Failure> {
    let current = exchange(stream, &Request::QueueList)?
        .queue
        .and_then(|q| q.current);
    match current {
        Some(c) if c != start => Err(Failure::Internal(anyhow!(
            "the player running is older than this melodie and started from the first track: \
             stop it once with `melodie quit`, then play again"
        ))),
        _ => Ok(()),
    }
}

/// `open`: plays or queues what an address names. A malformed address is refused before
/// anything else happens: no keyring, no network, no daemon.
pub fn open(env: &Env, uri: &str) -> ExitCode {
    let addr = match Address::parse(uri) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            notify::send("Mélodie : adresse refusée", &e.to_string());
            return ExitCode::from(1);
        }
    };
    match try_open(env, &addr) {
        Ok(n) => {
            announce(n, addr.place != Place::Replace);
            ExitCode::SUCCESS
        }
        Err(f) => report(&f),
    }
}

fn try_open(env: &Env, addr: &Address) -> Result<usize, Failure> {
    let (client, ready) = prepare(env)?;
    let result = resolve::resolve(&client, addr.kind, &addr.id);
    state::record(env, &result);
    let tracks = result?;
    let n = tracks.len();
    send(env, ready, &rules::open_request(addr, tracks))?;
    Ok(n)
}

/// `mix` : envoie une liste déjà calculée localement.
pub fn play_list(
    env: &Env,
    tracks: Vec<melodie_core::track::TrackInfo>,
    queue: bool,
) -> Result<usize, Failure> {
    let (_client, ready) = prepare(env)?;
    let n = deliver(env, ready, tracks, false, queue, false)?;
    announce(n, queue);
    Ok(n)
}

/// Contrôles simples (`toggle`, `pause`…) : ne démarrent jamais le démon.
pub fn control(req: Request) -> ExitCode {
    let Some(stream) = connect() else {
        eprintln!("Rien en lecture (le démon ne tourne pas)");
        return ExitCode::from(1);
    };
    match exchange(&stream, &req) {
        Ok(r) if r.ok => ExitCode::SUCCESS,
        Ok(r) => {
            eprintln!(
                "{}",
                r.error.unwrap_or_else(|| "refusé par le démon".into())
            );
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("melodie : {e:#}");
            ExitCode::from(1)
        }
    }
}

/// `volume` : sans démon, lit et écrit seulement le volume mémorisé (appliqué au
/// prochain démarrage) ; avec démon, le règle tout de suite. Ne démarre jamais le démon.
pub fn volume(env: &Env, change: Option<&str>) -> ExitCode {
    let stream = connect();
    let current = match &stream {
        Some(s) => match exchange(s, &Request::Now) {
            Ok(Response { now: Some(n), .. }) => (n.volume * 100.0).round() as u32,
            _ => Prefs::load(env).volume(),
        },
        None => Prefs::load(env).volume(),
    };
    let Some(arg) = change else {
        println!("Volume : {current} %");
        return ExitCode::SUCCESS;
    };
    let target = match prefs::parse_change(arg, current) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    match stream {
        Some(s) => {
            let req = Request::SetVolume {
                volume: f64::from(target) / 100.0,
            };
            match exchange(&s, &req) {
                Ok(r) if r.ok => {}
                Ok(r) => {
                    eprintln!(
                        "{}",
                        r.error.unwrap_or_else(|| "refusé par le démon".into())
                    );
                    return ExitCode::from(1);
                }
                Err(e) => {
                    eprintln!("melodie : {e:#}");
                    return ExitCode::from(1);
                }
            }
        }
        None => {
            let mut p = Prefs::load(env);
            p.volume = Some(target);
            if let Err(e) = p.save(env) {
                eprintln!("melodie : {e:#}");
                return ExitCode::from(3);
            }
        }
    }
    println!("Volume : {target} %");
    ExitCode::SUCCESS
}

/// Texte de `now`.
pub fn render_now(n: &Now) -> String {
    let state = match n.state.as_str() {
        "playing" => "En lecture",
        "paused" => "En pause",
        _ => return "Rien en lecture\n".to_string(),
    };
    let mut out = format!("{state} : {}\n", n.title);
    let who = [n.artist.as_str(), n.album.as_str()]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" · ");
    if !who.is_empty() {
        out.push_str(&format!("  {who}\n"));
    }
    let mut line = format!("  {} / {}", clock(n.position), clock(n.duration));
    if !n.quality.is_empty() {
        line.push_str(&format!(" · {}", n.quality));
    }
    out.push_str(&line);
    out.push('\n');
    out.push_str(&format!("  Morceau {} sur {}\n", n.index, n.queue_len));
    out
}

/// `now` : ne démarre pas le démon ; sans démon, état arrêté.
pub fn now(json: bool) -> ExitCode {
    let n = match connect() {
        None => Now {
            state: "stopped".into(),
            ..Now::default()
        },
        Some(stream) => match exchange(&stream, &Request::Now) {
            Ok(Response { now: Some(n), .. }) => n,
            Ok(r) => {
                eprintln!("{}", r.error.unwrap_or_else(|| "réponse inattendue".into()));
                return ExitCode::from(1);
            }
            Err(e) => {
                eprintln!("melodie : {e:#}");
                return ExitCode::from(1);
            }
        },
    };
    if json {
        match serde_json::to_string(&n) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("melodie : {e}");
                return ExitCode::from(3);
            }
        }
    } else {
        print!("{}", render_now(&n));
    }
    ExitCode::SUCCESS
}

/// Text of `queue --list`.
pub fn render_queue(q: &QueueView) -> String {
    if q.tracks.is_empty() {
        return "The queue is empty\n".to_string();
    }
    let width = q.tracks.len().to_string().len();
    let mut out = String::new();
    for (i, t) in q.tracks.iter().enumerate() {
        let mark = if q.current == Some(i) { "▶" } else { " " };
        let mut line = format!("{mark} {:>width$}. {}", i + 1, t.title);
        if !t.artist.is_empty() {
            line.push_str(&format!(" · {}", t.artist));
        }
        line.push_str(&format!("  {}\n", clock(t.duration)));
        out.push_str(&line);
    }
    out
}

/// `queue --list`: never starts the daemon; without one, an empty queue.
pub fn queue_list(json: bool) -> ExitCode {
    let q = match connect() {
        None => QueueView::default(),
        Some(stream) => match exchange(&stream, &Request::QueueList) {
            Ok(Response { queue: Some(q), .. }) => q,
            Ok(r) => {
                eprintln!("{}", r.error.unwrap_or_else(|| "réponse inattendue".into()));
                return ExitCode::from(1);
            }
            Err(e) => {
                eprintln!("melodie : {e:#}");
                return ExitCode::from(1);
            }
        },
    };
    if json {
        match serde_json::to_string(&q) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("melodie : {e}");
                return ExitCode::from(3);
            }
        }
    } else {
        print!("{}", render_queue(&q));
    }
    ExitCode::SUCCESS
}

/// Line `watch` prints when there is no daemon to follow.
const GONE: &str = r#"{"type":"gone"}"#;
/// How often `watch` looks for a daemon while there is none.
const WATCH_RETRY: Duration = Duration::from_millis(500);

/// `watch`: prints the daemon's event stream as it comes. Never starts the
/// daemon: while there is none, says so once and waits for one.
pub fn watch() -> ExitCode {
    let mut out = std::io::stdout().lock();
    let mut said_gone = false;
    loop {
        if let Some(stream) = connect()
            && (&stream).write_all(b"{\"cmd\":\"watch\"}\n").is_ok()
        {
            for line in BufReader::new(&stream).lines() {
                let Ok(line) = line else { break };
                if writeln!(out, "{line}").and_then(|_| out.flush()).is_err() {
                    return ExitCode::SUCCESS;
                }
                said_gone = false;
            }
        }
        if !said_gone {
            if writeln!(out, "{GONE}").and_then(|_| out.flush()).is_err() {
                return ExitCode::SUCCESS;
            }
            said_gone = true;
        }
        std::thread::sleep(WATCH_RETRY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendu_de_now() {
        assert_eq!(
            render_now(&Now {
                state: "stopped".into(),
                ..Now::default()
            }),
            "Rien en lecture\n"
        );
        let n = Now {
            state: "paused".into(),
            title: "Titre".into(),
            artist: "Artiste".into(),
            album: "Album".into(),
            duration: 225.0,
            position: 83.4,
            quality: "MP3 · 320 kbps".into(),
            index: 2,
            queue_len: 3,
            ..Now::default()
        };
        assert_eq!(
            render_now(&n),
            "En pause : Titre\n  Artiste · Album\n  1:23 / 3:45 · MP3 · 320 kbps\n  Morceau 2 sur 3\n"
        );
    }

    #[test]
    fn queue_text_marks_the_current_track() {
        use melodie_core::proto::QueueEntry;
        assert_eq!(render_queue(&QueueView::default()), "The queue is empty\n");
        let t = |title: &str, artist: &str| QueueEntry {
            title: title.into(),
            artist: artist.into(),
            duration: 225.0,
            ..QueueEntry::default()
        };
        let q = QueueView {
            current: Some(1),
            tracks: vec![t("Un", "Artiste"), t("Deux", "")],
        };
        assert_eq!(
            render_queue(&q),
            "  1. Un · Artiste  3:45\n▶ 2. Deux  3:45\n"
        );
    }
}
