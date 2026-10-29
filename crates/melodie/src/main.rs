//! `melodie`: Navidrome client (Subsonic/OpenSubsonic API), local copy of the
//! library, search, and background player (libmpv daemon + MPRIS).

mod autosync;
mod cover_cmd;
mod ctl;
mod daemon;
mod likes_cmd;
mod mixcmd;
mod mpris;
mod notify;
mod playlists_cmd;
mod queue;
mod rules;
mod waveform_cmd;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use melodie_core::env::Env;
use melodie_core::state::{self, Failure};
use melodie_core::time::rfc3339;
use melodie_core::{api, db, likes, mix, playlists, proto, search, store, sync};
use rusqlite::Connection;
use std::io::{BufRead, IsTerminal, Write};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "melodie",
    bin_name = "melodie",
    version = option_env!("MELODIE_VERSION").unwrap_or("0.0.0-dev"),
    about = "Navidrome client: login, local copy, search"
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

/// What can be played.
#[derive(Clone, Copy, ValueEnum)]
enum KindArg {
    Playlist,
    Album,
    Artist,
    Track,
}

impl From<KindArg> for search::Kind {
    fn from(k: KindArg) -> Self {
        match k {
            KindArg::Playlist => search::Kind::Playlist,
            KindArg::Album => search::Kind::Album,
            KindArg::Artist => search::Kind::Artist,
            KindArg::Track => search::Kind::Track,
        }
    }
}

/// What can be liked.
#[derive(Clone, Copy, ValueEnum)]
enum StarArg {
    Track,
    Album,
    Artist,
}

impl From<StarArg> for db::StarKind {
    fn from(k: StarArg) -> Self {
        match k {
            StarArg::Track => db::StarKind::Track,
            StarArg::Album => db::StarKind::Album,
            StarArg::Artist => db::StarKind::Artist,
        }
    }
}

/// A mix.
#[derive(Subcommand)]
enum MixWhat {
    /// Personal mix over the whole library
    Me,
    /// Albums played before, but not in the last 90 days
    Rediscover,
    /// Instant mix around a track (id or title)
    Track {
        #[arg(required = true, num_args = 1..)]
        id: Vec<String>,
    },
    /// Instant mix around an album (id or name)
    Album {
        #[arg(required = true, num_args = 1..)]
        id: Vec<String>,
    },
    /// Instant mix around an artist (id or name)
    Artist {
        #[arg(required = true, num_args = 1..)]
        id: Vec<String>,
    },
    /// Mix of a genre (name)
    Genre {
        #[arg(required = true, num_args = 1..)]
        name: Vec<String>,
    },
}

/// Playlist editing.
#[derive(Subcommand)]
enum PlaylistCmd {
    /// Adds a track, an album or the current track (default) to a playlist
    Add {
        playlist_id: String,
        /// `current`, `track <id>…` or `album <id>`
        target: Vec<String>,
        /// Also adds the tracks already in the playlist (otherwise they are skipped and counted)
        #[arg(long)]
        allow_duplicates: bool,
        /// Says what would be added and what is already there, without writing anything
        #[arg(long, conflicts_with = "allow_duplicates")]
        dry_run: bool,
        /// JSON output: tracks added and tracks already present
        #[arg(long)]
        json: bool,
    },
    /// Lists the duplicates of a playlist; with --remove, removes the exact ones
    Duplicates {
        playlist_id: String,
        /// Removes exact duplicates (same id) and keeps the first occurrence
        #[arg(long)]
        remove: bool,
        /// JSON output
        #[arg(long)]
        json: bool,
    },
    /// Removes the tracks at these ranks (counted from 1, like `duplicates` prints them)
    Remove {
        playlist_id: String,
        #[arg(required = true, num_args = 1..)]
        rank: Vec<usize>,
        /// The id each rank should hold, in the same order: nothing is removed otherwise
        #[arg(long)]
        expect: Vec<String>,
    },
    /// Moves the track at rank <from> to rank <to> (counted from 1)
    Move {
        playlist_id: String,
        from: usize,
        to: usize,
        /// The id rank <from> should hold: nothing is moved otherwise
        #[arg(long)]
        expect: Option<String>,
    },
    /// Creates a playlist, optionally with a track, an album or the current track
    Create {
        /// <name>, then `current`, `track <id>` or `album <id>` (with --prompt: the target only)
        args: Vec<String>,
        /// Asks for the name on the keyboard
        #[arg(long)]
        prompt: bool,
    },
    /// Renames a playlist
    Rename {
        playlist_id: String,
        #[arg(required = true, num_args = 1..)]
        name: Vec<String>,
    },
    /// Deletes a playlist
    Delete { playlist_id: String },
    /// Lists the user's playlists with the add action (launcher submenu)
    Pick {
        #[arg(long)]
        json: bool,
        /// `current` (default), `track <id>` or `album <id>`
        target: Vec<String>,
    },
}

#[derive(Subcommand)]
enum Cmd {
    /// Logs in to a Navidrome server (the password is not kept)
    Login {
        /// Reads the password from the standard input instead of asking for it (the window's
        /// way: never on the command line); needs --server and --user
        #[arg(long, requires_all = ["server", "user"])]
        password_stdin: bool,
        /// The server's address, with --password-stdin
        #[arg(long, requires = "password_stdin")]
        server: Option<String>,
        /// The user, with --password-stdin
        #[arg(long, requires = "password_stdin")]
        user: Option<String>,
    },
    /// Removes the secret from the keyring
    Logout,
    /// Server, user, connection state, local copy
    Status,
    /// Updates the local copy of the library
    Sync {
        /// Full copy, without comparing the server's last scan
        #[arg(long)]
        full: bool,
        /// Background sync (started by the player: silent, logged in sync.log)
        #[arg(long, hide = true)]
        background: bool,
    },
    /// Searches the local copy
    Search {
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
        /// JSON output in the launcher format
        #[arg(long)]
        json: bool,
    },
    /// Lists the playlists
    Playlists {
        #[arg(long)]
        json: bool,
    },
    /// Plays a playlist, an album, an artist or tracks (several track ids, or `-` to read them
    /// from stdin, one per line)
    Play {
        kind: KindArg,
        #[arg(required = true)]
        ids: Vec<String>,
        /// Shuffles the queue
        #[arg(long, conflicts_with = "from")]
        shuffle: bool,
        /// Starts at this track of the list (from 1); the ones before it stay in the queue
        #[arg(long, value_name = "POS", value_parser = clap::value_parser!(u64).range(1..))]
        from: Option<u64>,
    },
    /// Plays or queues what an address names: `melodie:<kind>/<id>[?shuffle=true][&queue=next|last]`
    Open {
        /// For example `melodie:album/4Zq0Aw`; kinds: track, album, artist, playlist
        address: String,
    },
    /// Adds to the queue, or shows and edits it (positions start at 1)
    #[command(group(
        clap::ArgGroup::new("action")
            .required(true)
            .args(["kind", "list", "play", "move_", "remove", "clear_after", "shuffle_after"])
    ))]
    Queue {
        #[arg(requires = "ids")]
        kind: Option<KindArg>,
        /// One id, or several track ids (`-`: read from stdin, one per line)
        ids: Vec<String>,
        /// Adds right after the current track instead of at the end
        #[arg(long, requires = "kind")]
        next: bool,
        /// Shows the queue and the current track
        #[arg(long)]
        list: bool,
        /// With --list: JSON output
        #[arg(long, requires = "list")]
        json: bool,
        /// Plays the track at this position
        #[arg(long, value_name = "POS", value_parser = clap::value_parser!(u64).range(1..))]
        play: Option<u64>,
        /// Moves the track at FROM so that it ends up at TO
        #[arg(long = "move", num_args = 2, value_names = ["FROM", "TO"],
              value_parser = clap::value_parser!(u64).range(1..))]
        move_: Option<Vec<u64>>,
        /// Removes the track at this position
        #[arg(long, value_name = "POS", value_parser = clap::value_parser!(u64).range(1..))]
        remove: Option<u64>,
        /// Removes every track after the current one
        #[arg(long)]
        clear_after: bool,
        /// Shuffles, once, every track after the current one
        #[arg(long)]
        shuffle_after: bool,
    },
    /// Follows what the player does: one JSON event per line (never starts the daemon)
    Watch,
    /// Plays or pauses
    Toggle,
    /// Pauses
    Pause,
    /// Resumes playback
    Resume,
    /// Next track
    Next,
    /// Previous track (or the start of the current one)
    Prev,
    /// Stops playback and empties the queue
    Stop,
    /// Quits the player (never starts it)
    Quit,
    /// Volume: without an argument shows it; `<0-100>`, `+5` or `-5` sets it (remembered)
    Volume {
        #[arg(allow_hyphen_values = true)]
        change: Option<String>,
    },
    /// Current track, position, quality, state
    Now {
        /// JSON output
        #[arg(long)]
        json: bool,
    },
    /// Likes the current track, or a track, album or artist
    Like {
        #[arg(requires = "id")]
        kind: Option<StarArg>,
        #[arg(requires = "kind")]
        id: Option<String>,
        /// Flips the current state instead of liking
        #[arg(long)]
        toggle: bool,
    },
    /// Removes the current track, or a track, album or artist, from the favourites
    Unlike {
        #[arg(requires = "id")]
        kind: Option<StarArg>,
        #[arg(requires = "kind")]
        id: Option<String>,
    },
    /// Edits the playlists on the server
    Playlist {
        #[command(subcommand)]
        cmd: PlaylistCmd,
    },
    /// Computes a mix on the local copy and plays it
    Mix {
        #[command(subcommand)]
        what: MixWhat,
        /// Adds to the queue instead of replacing it
        #[arg(long, global = true)]
        queue: bool,
        /// Adds right after the current track instead of replacing the queue
        #[arg(long, global = true)]
        next: bool,
        /// Prints the list without playing anything
        #[arg(long, global = true)]
        print: bool,
        /// JSON output in the launcher format (plays nothing)
        #[arg(long, global = true)]
        json: bool,
        /// Random seed (reproducible results)
        #[arg(long, global = true)]
        seed: Option<u64>,
        /// Number of tracks
        #[arg(long, global = true, default_value_t = mix::DEFAULT_SIZE)]
        size: usize,
    },
    /// Mixes offered to the launcher: personal mix, rediscoveries, genres
    Mixes {
        #[arg(long)]
        json: bool,
    },
    /// Launcher content without any typing: playlists, then mixes
    Home {
        #[arg(long)]
        json: bool,
    },
    /// Downloads thumbnails (ids on standard input, started by search)
    #[command(hide = true)]
    CoverFetch,
    /// Computes waveforms (track ids on standard input, started by the window)
    #[command(hide = true)]
    WaveformFetch,
    /// Playback daemon (started by `play` and `queue`)
    #[command(hide = true)]
    Daemon,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("melodie : {e:#}");
            ExitCode::from(1)
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let env = Env::from_process()?;
    match cli.command {
        Cmd::Login { server, user, .. } => login(&env, server.zip(user)),
        Cmd::Logout => logout(&env),
        Cmd::Status => status(&env),
        Cmd::Sync { full, background } => {
            if background {
                autosync::run_background(&env, full)?;
                Ok(ExitCode::SUCCESS)
            } else {
                Ok(done(sync_cmd(&env, full)))
            }
        }
        Cmd::Search { text, json } => {
            let text = text.join(" ");
            Ok(emit(&env, search_cmd(&env, &text), json))
        }
        Cmd::Playlists { json } => Ok(emit(&env, playlists_cmd(&env), json)),
        Cmd::Play {
            kind,
            ids,
            shuffle,
            from,
        } => {
            let ids = stdin_ids(ids)?;
            Ok(match (kind, from) {
                (KindArg::Track, from) => {
                    let start = match from {
                        Some(p) if p as usize > ids.len() => {
                            bail!("play: no track at position {p} of {}", ids.len())
                        }
                        Some(p) => p as usize - 1,
                        None => 0,
                    };
                    ctl::play_tracks(&env, ids, start, shuffle, false, false)
                }
                (_, None) if ids.len() == 1 => {
                    ctl::play_or_queue(&env, kind.into(), &ids[0], shuffle, false, false)
                }
                _ => bail!("play: several ids and --from are for tracks only"),
            })
        }
        Cmd::Open { address } => Ok(ctl::open(&env, &address)),
        Cmd::Queue {
            kind,
            ids,
            next,
            list,
            json,
            play,
            move_,
            remove,
            clear_after,
            shuffle_after,
        } => {
            // Positions start at 1 on the command line, at 0 on the socket.
            let at = |p: u64| p as usize - 1;
            Ok(if list {
                ctl::queue_list(json)
            } else if let Some(p) = play {
                ctl::control(proto::Request::QueuePlay { index: at(p) })
            } else if let Some(m) = move_ {
                ctl::control(proto::Request::QueueMove {
                    from: at(m[0]),
                    to: at(m[1]),
                })
            } else if let Some(p) = remove {
                ctl::control(proto::Request::QueueRemove { index: at(p) })
            } else if clear_after {
                ctl::control(proto::Request::QueueClearAfter)
            } else if shuffle_after {
                ctl::control(proto::Request::QueueShuffleAfter)
            } else {
                match (kind, stdin_ids(ids)?) {
                    (Some(KindArg::Track), ids) => {
                        ctl::play_tracks(&env, ids, 0, false, true, next)
                    }
                    (Some(kind), ids) if ids.len() == 1 => {
                        ctl::play_or_queue(&env, kind.into(), &ids[0], false, true, next)
                    }
                    (Some(_), _) => bail!("queue: several ids are for tracks only"),
                    _ => bail!("queue: what to add is missing"),
                }
            })
        }
        Cmd::Watch => Ok(ctl::watch()),
        Cmd::Toggle => Ok(ctl::control(proto::Request::Toggle)),
        Cmd::Pause => Ok(ctl::control(proto::Request::Pause)),
        Cmd::Resume => Ok(ctl::control(proto::Request::Resume)),
        Cmd::Next => Ok(ctl::control(proto::Request::Next)),
        Cmd::Prev => Ok(ctl::control(proto::Request::Prev)),
        Cmd::Stop => Ok(ctl::control(proto::Request::Stop)),
        Cmd::Quit => Ok(ctl::quit()),
        Cmd::Volume { change } => Ok(ctl::volume(&env, change.as_deref())),
        Cmd::Now { json } => Ok(ctl::now(json)),
        Cmd::Like { kind, id, toggle } => {
            let mode = if toggle {
                likes::Mode::Toggle
            } else {
                likes::Mode::Like
            };
            Ok(likes_cmd::run(&env, star_target(kind, id), mode))
        }
        Cmd::Unlike { kind, id } => Ok(likes_cmd::run(
            &env,
            star_target(kind, id),
            likes::Mode::Unlike,
        )),
        Cmd::Playlist { cmd } => Ok(playlist_cmd(&env, cmd)),
        Cmd::Mix {
            what,
            queue,
            next,
            print,
            json,
            seed,
            size,
        } => {
            let what = match what {
                MixWhat::Me => mixcmd::What::Me,
                MixWhat::Rediscover => mixcmd::What::Rediscover,
                MixWhat::Track { id } => mixcmd::What::Source(mix::SourceKind::Track, id.join(" ")),
                MixWhat::Album { id } => mixcmd::What::Source(mix::SourceKind::Album, id.join(" ")),
                MixWhat::Artist { id } => {
                    mixcmd::What::Source(mix::SourceKind::Artist, id.join(" "))
                }
                MixWhat::Genre { name } => {
                    mixcmd::What::Source(mix::SourceKind::Genre, name.join(" "))
                }
            };
            let o = mixcmd::Opts {
                queue: queue || next,
                next,
                print,
                json,
                seed,
                size: size.max(1),
            };
            Ok(mixcmd::run(&env, &what, &o))
        }
        Cmd::Mixes { json } => Ok(emit_values(&env, mixcmd::mixes(&env), json)),
        Cmd::Home { json } => Ok(emit_values(&env, mixcmd::home(&env), json)),
        Cmd::CoverFetch => cover_cmd::fetch_cmd(&env),
        Cmd::WaveformFetch => waveform_cmd::fetch_cmd(&env),
        Cmd::Daemon => {
            daemon::run(&env)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// The ids given, or read from stdin (one per line) when the only one is `-`: a list as long
/// as a whole library does not fit on a command line.
fn stdin_ids(ids: Vec<String>) -> Result<Vec<String>> {
    let stdin = std::io::stdin();
    let terminal = stdin.is_terminal();
    read_ids(ids, stdin.lock(), terminal)
}

/// `read_ids` with what stdin is: `terminal` when nothing is piped in.
fn read_ids(ids: Vec<String>, input: impl BufRead, terminal: bool) -> Result<Vec<String>> {
    if !ids.iter().any(|id| id == "-") {
        return Ok(ids);
    }
    if ids.len() > 1 {
        bail!("- reads every id from stdin: give no other id beside it");
    }
    if terminal {
        // waiting for Ctrl-D would look like a hang
        bail!("- reads the ids from stdin: pipe them in, one per line");
    }
    let mut read = Vec::new();
    for line in input.lines() {
        let line = line?;
        let id = line.trim();
        if !id.is_empty() {
            read.push(id.to_string());
        }
    }
    if read.is_empty() {
        bail!("no id on stdin");
    }
    Ok(read)
}

fn star_target(kind: Option<StarArg>, id: Option<String>) -> likes::Target {
    match (kind, id) {
        (Some(k), Some(id)) => likes::Target::Item(k.into(), id),
        _ => likes::Target::Current,
    }
}

fn playlist_cmd(env: &Env, cmd: PlaylistCmd) -> ExitCode {
    let target =
        |args: &[String]| playlists::Target::parse(args).map_err(|m| anyhow::anyhow!("{m}"));
    let r: Result<ExitCode> = (|| {
        Ok(match cmd {
            PlaylistCmd::Add {
                playlist_id,
                target: t,
                allow_duplicates,
                dry_run,
                json,
            } => playlists_cmd::add_cmd(
                env,
                &playlist_id,
                &target(&t)?,
                playlists_cmd::Duplicates {
                    allow: allow_duplicates,
                    dry_run,
                },
                json,
            ),
            PlaylistCmd::Duplicates {
                playlist_id,
                remove,
                json,
            } => playlists_cmd::duplicates_cmd(env, &playlist_id, remove, json),
            PlaylistCmd::Remove {
                playlist_id,
                rank,
                expect,
            } => playlists_cmd::remove_cmd(env, &playlist_id, &rank, &expect),
            PlaylistCmd::Move {
                playlist_id,
                from,
                to,
                expect,
            } => playlists_cmd::move_cmd(env, &playlist_id, from, to, expect.as_deref()),
            PlaylistCmd::Create { args, prompt } => {
                let (name, rest) = if prompt {
                    (ask("Nom de la playlist", None)?, &args[..])
                } else {
                    match args.split_first() {
                        Some((n, rest)) => (n.clone(), rest),
                        None => bail!("nom de playlist attendu (ou --prompt)"),
                    }
                };
                let t = if rest.is_empty() {
                    None
                } else {
                    Some(target(rest)?)
                };
                playlists_cmd::create_cmd(env, &name, t.as_ref())
            }
            PlaylistCmd::Rename { playlist_id, name } => {
                playlists_cmd::rename_cmd(env, &playlist_id, &name.join(" "))
            }
            PlaylistCmd::Delete { playlist_id } => playlists_cmd::delete_cmd(env, &playlist_id),
            PlaylistCmd::Pick { json, target: t } => {
                let t = target(&t)?;
                match (playlists_cmd::pick_cmd(env, &t), json) {
                    (Ok(v), true) => {
                        println!("{}", launcher_array(env, v));
                        ExitCode::SUCCESS
                    }
                    (Ok(v), false) => {
                        print!("{}", playlists::render_pick(&v));
                        ExitCode::SUCCESS
                    }
                    (Err(f), true) => {
                        println!("{}", serde_json::Value::Array(vec![f.launcher_entry()]));
                        ExitCode::SUCCESS
                    }
                    (Err(f), false) => fail(&f),
                }
            }
        })
    })();
    r.unwrap_or_else(|e| {
        eprintln!("melodie : {e:#}");
        ExitCode::from(1)
    })
}

/// Comme `emit`, pour des entrées JSON déjà prêtes.
fn emit_values(env: &Env, r: Result<Vec<serde_json::Value>, Failure>, json: bool) -> ExitCode {
    match (r, json) {
        (Ok(v), true) => {
            println!("{}", launcher_array(env, v));
            ExitCode::SUCCESS
        }
        (Ok(v), false) => {
            for e in v {
                println!(
                    "{}\n    {}",
                    e["name"].as_str().unwrap_or(""),
                    e["subtitle"].as_str().unwrap_or("")
                );
            }
            ExitCode::SUCCESS
        }
        (Err(f), true) => {
            println!("{}", serde_json::Value::Array(vec![f.launcher_entry()]));
            ExitCode::SUCCESS
        }
        (Err(f), false) => fail(&f),
    }
}

/// Tableau JSON du lanceur : `_cover` devient `icon` (chemin local) quand la pochette est là.
pub fn launcher_array(env: &Env, mut entries: Vec<serde_json::Value>) -> serde_json::Value {
    cover_cmd::apply(env, &mut entries);
    serde_json::Value::Array(entries)
}

/// Affiche un échec en sortie lisible : message sur stderr, code non nul.
fn fail(f: &Failure) -> ExitCode {
    eprintln!("{f}");
    ExitCode::from(f.exit_code())
}

fn done(r: Result<(), Failure>) -> ExitCode {
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(f) => fail(&f),
    }
}

/// Résultat de `search` ou `playlists` : JSON (échec = une entrée, code 0) ou texte.
fn emit(env: &Env, r: Result<Vec<search::Hit>, Failure>, json: bool) -> ExitCode {
    match (r, json) {
        (Ok(hits), true) => {
            println!("{}", launcher_array(env, search::to_json(&hits)));
            ExitCode::SUCCESS
        }
        (Ok(hits), false) => {
            print!("{}", search::render(&hits));
            ExitCode::SUCCESS
        }
        (Err(f), true) => {
            println!("{}", serde_json::Value::Array(vec![f.launcher_entry()]));
            ExitCode::SUCCESS
        }
        (Err(f), false) => fail(&f),
    }
}

/// Copie locale prête à interroger, sans jamais attendre le réseau : voir `autosync::library_with`.
fn library(env: &Env) -> Result<Connection, Failure> {
    autosync::library(env)
}

fn search_cmd(env: &Env, text: &str) -> Result<Vec<search::Hit>, Failure> {
    let conn = library(env)?;
    Ok(search::search(&conn, text)?)
}

fn playlists_cmd(env: &Env) -> Result<Vec<search::Hit>, Failure> {
    let conn = library(env)?;
    Ok(search::playlists(&conn)?)
}

fn sync_cmd(env: &Env, full: bool) -> Result<(), Failure> {
    // Une seule synchronisation à la fois, celle-ci comprise.
    let Some(_lock) = autosync::lock_soon(&autosync::lock_path()?)? else {
        println!("Une synchronisation est déjà en cours");
        return Ok(());
    };
    let r = state::open_client(env).and_then(|c| sync::run(env, &c, full));
    state::record(env, &r);
    let report = r?;
    let what = match report.kind {
        sync::Kind::Full => "Copie complète",
        sync::Kind::Light => "Copie à jour (playlists, favoris, écoutes)",
    };
    let c = report.counts;
    println!(
        "{what} : {} morceaux, {} albums, {} artistes, {} playlists",
        c.tracks, c.albums, c.artists, c.playlists
    );
    Ok(())
}

fn ask(label: &str, default: Option<&str>) -> Result<String> {
    let mut err = std::io::stderr();
    match default {
        Some(d) => write!(err, "{label} [{d}] : ")?,
        None => write!(err, "{label} : ")?,
    }
    err.flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let line = line.trim();
    Ok(if line.is_empty() {
        default.unwrap_or("").to_string()
    } else {
        line.to_string()
    })
}

/// The password written on the standard input, without its line end.
fn password_from_stdin() -> Result<String> {
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let len = line.trim_end_matches(['\n', '\r']).len();
    line.truncate(len);
    Ok(line)
}

/// `given`: the server and the user of `--password-stdin`, the password then read from the
/// standard input; otherwise all three are asked. Exit 1 means "no login or refused" and only
/// the server's refusal says it: a local failure (keyring, configuration, input) is an internal
/// error, or the window would ask again for a password that was right.
fn login(env: &Env, given: Option<(String, String)>) -> Result<ExitCode> {
    Ok(login_attempt(env, given).unwrap_or_else(|e| fail(&Failure::Internal(e))))
}

fn login_attempt(env: &Env, given: Option<(String, String)>) -> Result<ExitCode> {
    let settings = store::Settings::load(env)?;
    let typed = match &given {
        Some((server, _)) => server.clone(),
        None => ask("Adresse du serveur", settings.url.as_deref())?,
    };
    let Some(url) = api::server_address(&typed) else {
        bail!("the address must start with http:// or https://");
    };
    let from_stdin = given.is_some();
    let user = match given {
        Some((_, user)) => user,
        None => ask("Utilisateur", settings.user.as_deref())?,
    };
    if user.is_empty() {
        bail!("utilisateur vide");
    }
    // Le mot de passe ne vit que le temps du calcul du jeton.
    let creds = {
        let password = if from_stdin {
            password_from_stdin()?
        } else {
            rpassword::prompt_password("Mot de passe : ")?
        };
        api::Creds::from_password(&user, &password)?
    };
    let client = api::Client::new(&url, creds.clone());
    match client.ping() {
        Ok(()) => {
            store::save_secret(&url, &creds)?;
            store::Settings {
                url: Some(url.clone()),
                user: Some(user.clone()),
            }
            .save(env)?;
            store::clear_auth_refused(env);
            println!("Connecté à {url} en tant que {user}");
            match autosync::spawn_background(true) {
                Ok(()) => println!("Copie de la bibliothèque en cours en arrière-plan"),
                Err(e) => eprintln!("Copie de la bibliothèque impossible à lancer : {e:#}"),
            }
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            let f = state::from_api(e, &url);
            match &f {
                Failure::AuthRefused(_) => {
                    eprintln!("Utilisateur ou mot de passe refusé par {url}")
                }
                other => eprintln!("{other}"),
            }
            Ok(ExitCode::from(f.exit_code()))
        }
    }
}

fn logout(env: &Env) -> Result<ExitCode> {
    let n = store::delete_secrets()?;
    // Sans utilisateur, la configuration vaut « pas connecté » ; l'adresse reste proposée au prochain login.
    let mut settings = store::Settings::load(env)?;
    settings.user = None;
    settings.save(env)?;
    store::clear_auth_refused(env);
    if n == 0 {
        println!("Déjà déconnecté (aucun secret dans le trousseau)");
    } else {
        println!("Déconnecté : secret supprimé du trousseau");
    }
    Ok(ExitCode::SUCCESS)
}

fn megabytes(bytes: u64) -> String {
    format!("{:.1} Mo", bytes as f64 / 1_048_576.0)
}

fn human_age(secs: u64) -> String {
    match secs {
        0..=59 => "moins d'une minute".into(),
        60..=3599 => format!("{} min", secs / 60),
        3600..=86399 => format!("{} h {:02} min", secs / 3600, secs % 3600 / 60),
        _ => format!("{} j", secs / 86400),
    }
}

fn status(env: &Env) -> Result<ExitCode> {
    let settings = store::Settings::load(env)?;
    println!(
        "Serveur     : {}",
        settings.url.as_deref().unwrap_or("(non défini)")
    );
    println!(
        "Utilisateur : {}",
        settings.user.as_deref().unwrap_or("(non défini)")
    );

    let state = match state::open_client(env) {
        Ok(client) => {
            let r = client.ping().map_err(|e| state::from_api(e, client.url()));
            state::record(env, &r);
            r
        }
        Err(f) => Err(f),
    };
    match &state {
        Ok(()) => println!("Connexion   : connecté"),
        Err(f) => println!("Connexion   : {} ({f})", f.state()),
    }

    // The exit code below is the ping's: an unreadable copy is told here, never turned into "no login".
    let copy = db::open_existing(env).and_then(|conn| {
        conn.map(|conn| db::counts(&conn).map(|c| (conn, c)))
            .transpose()
    });
    match copy {
        Ok(Some((conn, c))) if db::is_partial(&conn) => println!(
            "Copie       : première copie pas finie : {} morceaux, {} albums, {} artistes jusqu'ici",
            c.tracks, c.albums, c.artists
        ),
        Ok(Some((conn, c))) => {
            let size = std::fs::metadata(db::db_path(env))
                .map(|m| m.len())
                .unwrap_or(0);
            let when = db::get_meta(&conn, "synced_at")
                .and_then(|s| s.parse::<u64>().ok())
                .map(rfc3339)
                .unwrap_or_else(|| "date inconnue".into());
            let age = match autosync::age_secs(&conn) {
                u64::MAX => String::new(),
                a => format!(" (il y a {})", human_age(a)),
            };
            println!(
                "Copie       : {} morceaux, {} albums, {} artistes, {} playlists ({}), synchronisée le {when}{age}",
                c.tracks,
                c.albums,
                c.artists,
                c.playlists,
                megabytes(size)
            );
        }
        Ok(None) => {
            println!("Copie       : aucune (elle se fait toute seule après la connexion)")
        }
        Err(e) => println!("Copie       : illisible ({e:#})"),
    }
    println!(
        "Synchro     : {}",
        if autosync::is_running() {
            "en cours"
        } else {
            "aucune en cours"
        }
    );
    Ok(match state {
        Ok(()) => ExitCode::SUCCESS,
        Err(f) => ExitCode::from(f.exit_code()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn ids_on_stdin_are_read_unless_it_is_a_terminal_or_mixed() {
        let read = |ids: &[&str], input: &str, terminal| {
            read_ids(
                ids.iter().map(|s| s.to_string()).collect(),
                input.as_bytes(),
                terminal,
            )
        };
        assert_eq!(read(&["t1", "t2"], "", true).unwrap(), ["t1", "t2"]);
        assert_eq!(read(&["-"], "t1\n\n t2 \n", false).unwrap(), ["t1", "t2"]);
        assert!(
            read(&["-"], "t1\n", true).is_err(),
            "a terminal: refused, not waited on"
        );
        assert!(
            read(&["t1", "-"], "t2\n", false).is_err(),
            "- among other ids"
        );
        assert!(read(&["-"], "", false).is_err(), "nothing on stdin");
    }

    /// The window runs `melodie like track -- <id>`: the id must come out whole even when it
    /// starts with a dash.
    #[test]
    fn an_id_after_double_dash_is_not_read_as_an_option() {
        for verb in ["like", "unlike"] {
            let cli = Cli::try_parse_from(["melodie", verb, "track", "--", "-7f3a"]).unwrap();
            let (kind, id) = match cli.command {
                Cmd::Like { kind, id, .. } | Cmd::Unlike { kind, id } => (kind, id),
                _ => panic!("{verb} is not a like"),
            };
            assert!(matches!(kind, Some(StarArg::Track)));
            assert_eq!(id.as_deref(), Some("-7f3a"));
        }
    }

    /// The window signs in with `login --password-stdin --server=… --user=…`; a plain `login`
    /// still asks for everything.
    #[test]
    fn login_asks_everything_unless_the_password_comes_on_stdin() {
        let parse = |a: &[&str]| Cli::try_parse_from([&["melodie", "login"][..], a].concat());
        match parse(&[]).unwrap().command {
            Cmd::Login {
                password_stdin,
                server,
                user,
            } => assert_eq!((password_stdin, server, user), (false, None, None)),
            _ => panic!("not a login"),
        }
        let window = [
            "--password-stdin",
            "--server=https://music.example.org",
            "--user=-alice",
        ];
        match parse(&window).unwrap().command {
            Cmd::Login {
                password_stdin,
                server,
                user,
            } => {
                assert!(password_stdin);
                assert_eq!(server.as_deref(), Some("https://music.example.org"));
                assert_eq!(user.as_deref(), Some("-alice"));
            }
            _ => panic!("not a login"),
        }
        for half in [
            &["--password-stdin"][..],
            &["--password-stdin", "--user=alice"],
            &["--server=https://music.example.org", "--user=alice"],
        ] {
            assert!(parse(half).is_err(), "{half:?}");
        }
    }

    /// What the window's library runs (`melodie_desktop::launch::Job`): each line must parse, with
    /// the id whole.
    #[test]
    fn the_library_commands_of_the_window_parse() {
        let parse = |a: &[&str]| {
            let mut v = vec!["melodie"];
            v.extend(a);
            Cli::try_parse_from(v).unwrap_or_else(|e| panic!("{a:?}: {e}"))
        };
        match parse(&["play", "album", "--shuffle", "--", "-al1"]).command {
            Cmd::Play { ids, shuffle, .. } => {
                assert_eq!((ids, shuffle), (vec!["-al1".into()], true))
            }
            _ => panic!("not a play"),
        }
        match parse(&["queue", "playlist", "--next", "--", "-p1"]).command {
            Cmd::Queue { ids, next, .. } => assert_eq!((ids, next), (vec!["-p1".into()], true)),
            _ => panic!("not a queue"),
        }
        match parse(&["play", "track", "--from", "2", "-"]).command {
            Cmd::Play { ids, from, .. } => assert_eq!((ids, from), (vec!["-".into()], Some(2))),
            _ => panic!("not a play"),
        }
        match parse(&["queue", "track", "--next", "--", "-t1", "t2"]).command {
            Cmd::Queue { ids, .. } => assert_eq!(ids, ["-t1", "t2"]),
            _ => panic!("not a queue"),
        }
        match parse(&["mix", "artist", "--", "-ar1"]).command {
            Cmd::Mix {
                what: MixWhat::Artist { id },
                ..
            } => assert_eq!(id, ["-ar1"]),
            _ => panic!("not an artist mix"),
        }
        assert!(matches!(
            parse(&["mix", "rediscover"]).command,
            Cmd::Mix {
                what: MixWhat::Rediscover,
                ..
            }
        ));
        match parse(&["playlist", "add", "--json", "--", "-p1", "album", "-al1"]).command {
            Cmd::Playlist {
                cmd:
                    PlaylistCmd::Add {
                        playlist_id,
                        target,
                        json,
                        ..
                    },
            } => {
                assert_eq!(playlist_id, "-p1");
                assert_eq!(target, ["album", "-al1"]);
                assert!(json);
            }
            _ => panic!("not a playlist add"),
        }
    }

    /// Words that exist in French and not in English; the help must hold none of them.
    const FRENCH_WORDS: &[&str] = &[
        "le", "la", "les", "un", "une", "des", "du", "ou", "et", "avec", "sans", "pour", "dans",
        "sur", "est", "pas", "qui", "que", "ce", "cette", "ces", "aux", "au", "morceau",
        "morceaux", "ajoute", "retire", "liste", "lecture", "sortie", "copie", "serveur",
    ];

    fn help_of(cmd: &mut clap::Command, out: &mut Vec<(String, String)>) {
        cmd.build();
        out.push((
            cmd.get_name().to_string(),
            cmd.render_long_help().to_string(),
        ));
        for sub in cmd.get_subcommands_mut() {
            help_of(sub, out);
        }
    }

    #[test]
    fn help_of_every_command_is_in_english() {
        let mut helps = Vec::new();
        help_of(&mut Cli::command(), &mut helps);
        // The hidden subcommands are rendered too: they are help text all the same.
        assert!(helps.len() > 30, "only {} helps rendered", helps.len());
        let mut french = Vec::new();
        for (name, help) in &helps {
            if let Some(c) = help.chars().find(|c| c.is_alphabetic() && !c.is_ascii()) {
                french.push(format!("{name}: non-ASCII letter {c:?}"));
            }
            for word in help.split(|c: char| !c.is_alphabetic()) {
                if FRENCH_WORDS.contains(&word.to_lowercase().as_str()) {
                    french.push(format!("{name}: French word {word:?}"));
                }
            }
        }
        assert!(
            french.is_empty(),
            "French left in the help:\n{}",
            french.join("\n")
        );
    }
}
