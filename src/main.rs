//! `poste-music` : client Navidrome (API Subsonic/OpenSubsonic), copie locale
//! de la bibliothèque, recherche, et lecteur de fond (démon libmpv + MPRIS).

mod api;
mod autosync;
mod cover;
mod ctl;
mod daemon;
mod db;
#[cfg(test)]
mod fake;
mod likes;
mod mix;
mod mixcmd;
mod mpris;
mod notify;
mod playlists;
mod prefs;
mod proto;
mod resolve;
mod rules;
mod search;
mod state;
mod store;
mod sync;
mod track;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand, ValueEnum};
use poste_core::time::rfc3339;
use poste_core::Env;
use rusqlite::Connection;
use state::Failure;
use std::io::{BufRead, Write};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "poste music",
    bin_name = "poste music",
    version,
    about = "Client Navidrome : connexion, copie locale, recherche"
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

/// Ce qu'on peut lire.
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

/// Ce qu'on peut aimer.
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

/// Un mix.
#[derive(Subcommand)]
enum MixWhat {
    /// Mix personnel sur toute la bibliothèque
    Me,
    /// Albums écoutés mais pas depuis 90 jours
    Rediscover,
    /// Mix instantané autour d'un morceau (identifiant ou titre)
    Track {
        #[arg(required = true, num_args = 1..)]
        id: Vec<String>,
    },
    /// Mix instantané autour d'un album (identifiant ou nom)
    Album {
        #[arg(required = true, num_args = 1..)]
        id: Vec<String>,
    },
    /// Mix instantané autour d'un artiste (identifiant ou nom)
    Artist {
        #[arg(required = true, num_args = 1..)]
        id: Vec<String>,
    },
    /// Mix d'un genre (nom)
    Genre {
        #[arg(required = true, num_args = 1..)]
        name: Vec<String>,
    },
}

/// Écriture des playlists.
#[derive(Subcommand)]
enum PlaylistCmd {
    /// Ajoute un morceau, un album ou le morceau en cours (défaut) à une playlist
    Add {
        playlist_id: String,
        /// `current`, `track <id>` ou `album <id>`
        target: Vec<String>,
    },
    /// Retire le morceau de rang <index> (à partir de 0)
    Remove { playlist_id: String, index: usize },
    /// Crée une playlist, éventuellement avec un morceau, un album ou le morceau en cours
    Create {
        /// <nom> puis `current`, `track <id>` ou `album <id>` (avec --prompt : la cible seule)
        args: Vec<String>,
        /// Demande le nom au clavier
        #[arg(long)]
        prompt: bool,
    },
    /// Renomme une playlist
    Rename {
        playlist_id: String,
        #[arg(required = true, num_args = 1..)]
        name: Vec<String>,
    },
    /// Supprime une playlist
    Delete { playlist_id: String },
    /// Liste des playlists de l'utilisateur, avec l'action d'ajout (sous-menu du lanceur)
    Pick {
        #[arg(long)]
        json: bool,
        /// `current` (défaut), `track <id>` ou `album <id>`
        target: Vec<String>,
    },
}

#[derive(Subcommand)]
enum Cmd {
    /// Se connecte à un serveur Navidrome (le mot de passe n'est pas conservé)
    Login,
    /// Supprime le secret du trousseau
    Logout,
    /// Serveur, utilisateur, état de la connexion, copie locale
    Status,
    /// Met à jour la copie locale de la bibliothèque
    Sync {
        /// Copie complète, sans comparer le dernier scan du serveur
        #[arg(long)]
        full: bool,
        /// Synchronisation de fond (lancée par le module : silencieuse, journal dans music-sync.log)
        #[arg(long, hide = true)]
        background: bool,
    },
    /// Cherche dans la copie locale
    Search {
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
        /// Sortie JSON au format du lanceur de l'île
        #[arg(long)]
        json: bool,
    },
    /// Liste les playlists
    Playlists {
        #[arg(long)]
        json: bool,
    },
    /// Lance la lecture d'une playlist, d'un album, d'un artiste ou d'un morceau
    Play {
        kind: KindArg,
        id: String,
        /// Mélange la file
        #[arg(long)]
        shuffle: bool,
    },
    /// Ajoute à la fin de la file
    Queue { kind: KindArg, id: String },
    /// Lecture ou pause
    Toggle,
    /// Met en pause
    Pause,
    /// Reprend la lecture
    Resume,
    /// Morceau suivant
    Next,
    /// Morceau précédent (ou début du morceau en cours)
    Prev,
    /// Arrête la lecture et vide la file
    Stop,
    /// Volume : sans argument l'affiche ; `<0-100>`, `+5` ou `-5` le règle (mémorisé)
    Volume {
        #[arg(allow_hyphen_values = true)]
        change: Option<String>,
    },
    /// Morceau en cours, position, qualité, état
    Now {
        /// Sortie JSON pour l'île
        #[arg(long)]
        json: bool,
    },
    /// Aime le morceau en cours, ou un morceau, album ou artiste
    Like {
        #[arg(requires = "id")]
        kind: Option<StarArg>,
        #[arg(requires = "kind")]
        id: Option<String>,
        /// Inverse l'état actuel au lieu d'aimer
        #[arg(long)]
        toggle: bool,
    },
    /// Retire des favoris le morceau en cours, ou un morceau, album ou artiste
    Unlike {
        #[arg(requires = "id")]
        kind: Option<StarArg>,
        #[arg(requires = "kind")]
        id: Option<String>,
    },
    /// Écrit dans les playlists du serveur
    Playlist {
        #[command(subcommand)]
        cmd: PlaylistCmd,
    },
    /// Calcule un mix sur la copie locale et le lit
    Mix {
        #[command(subcommand)]
        what: MixWhat,
        /// Ajoute à la file au lieu de la remplacer
        #[arg(long, global = true)]
        queue: bool,
        /// Affiche la liste sans rien jouer
        #[arg(long, global = true)]
        print: bool,
        /// Sortie JSON au format du lanceur (sans rien jouer)
        #[arg(long, global = true)]
        json: bool,
        /// Graine du hasard (résultats reproductibles)
        #[arg(long, global = true)]
        seed: Option<u64>,
        /// Nombre de morceaux
        #[arg(long, global = true, default_value_t = mix::DEFAULT_SIZE)]
        size: usize,
    },
    /// Mixes proposés au lanceur : mix perso, redécouvertes, genres
    Mixes {
        #[arg(long)]
        json: bool,
    },
    /// Contenu du lanceur sans saisie : playlists puis mixes
    Home {
        #[arg(long)]
        json: bool,
    },
    /// Télécharge des vignettes (identifiants sur l'entrée standard, lancé par la recherche)
    #[command(hide = true)]
    CoverFetch,
    /// Démon de lecture (lancé par `play` et `queue`)
    #[command(hide = true)]
    Daemon,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("poste music : {e:#}");
            ExitCode::from(1)
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let env = Env::from_process()?;
    match cli.command {
        Cmd::Login => login(&env),
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
        Cmd::Play { kind, id, shuffle } => {
            Ok(ctl::play_or_queue(&env, kind.into(), &id, shuffle, false))
        }
        Cmd::Queue { kind, id } => Ok(ctl::play_or_queue(&env, kind.into(), &id, false, true)),
        Cmd::Toggle => Ok(ctl::control(proto::Request::Toggle)),
        Cmd::Pause => Ok(ctl::control(proto::Request::Pause)),
        Cmd::Resume => Ok(ctl::control(proto::Request::Resume)),
        Cmd::Next => Ok(ctl::control(proto::Request::Next)),
        Cmd::Prev => Ok(ctl::control(proto::Request::Prev)),
        Cmd::Stop => Ok(ctl::control(proto::Request::Stop)),
        Cmd::Volume { change } => Ok(ctl::volume(&env, change.as_deref())),
        Cmd::Now { json } => Ok(ctl::now(json)),
        Cmd::Like { kind, id, toggle } => {
            let mode = if toggle {
                likes::Mode::Toggle
            } else {
                likes::Mode::Like
            };
            Ok(likes::run(&env, star_target(kind, id), mode))
        }
        Cmd::Unlike { kind, id } => {
            Ok(likes::run(&env, star_target(kind, id), likes::Mode::Unlike))
        }
        Cmd::Playlist { cmd } => Ok(playlist_cmd(&env, cmd)),
        Cmd::Mix {
            what,
            queue,
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
                queue,
                print,
                json,
                seed,
                size: size.max(1),
            };
            Ok(mixcmd::run(&env, &what, &o))
        }
        Cmd::Mixes { json } => Ok(emit_values(&env, mixcmd::mixes(&env), json)),
        Cmd::Home { json } => Ok(emit_values(&env, mixcmd::home(&env), json)),
        Cmd::CoverFetch => {
            cover::fetch_cmd(&env)?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Daemon => {
            daemon::run(&env)?;
            Ok(ExitCode::SUCCESS)
        }
    }
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
            } => playlists::add_cmd(env, &playlist_id, &target(&t)?),
            PlaylistCmd::Remove { playlist_id, index } => {
                playlists::remove_cmd(env, &playlist_id, index)
            }
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
                playlists::create_cmd(env, &name, t.as_ref())
            }
            PlaylistCmd::Rename { playlist_id, name } => {
                playlists::rename_cmd(env, &playlist_id, &name.join(" "))
            }
            PlaylistCmd::Delete { playlist_id } => playlists::delete_cmd(env, &playlist_id),
            PlaylistCmd::Pick { json, target: t } => {
                let t = target(&t)?;
                match (playlists::pick_cmd(env, &t), json) {
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
        eprintln!("poste music : {e:#}");
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
    cover::apply(env, &mut entries);
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
    let Some(_lock) = autosync::try_lock(&autosync::lock_path()?)? else {
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

fn login(env: &Env) -> Result<ExitCode> {
    let settings = store::Settings::load(env)?;
    let url = api::normalize_url(&ask("Adresse du serveur", settings.url.as_deref())?);
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        bail!("l'adresse doit commencer par http:// ou https://");
    }
    let user = ask("Utilisateur", settings.user.as_deref())?;
    if user.is_empty() {
        bail!("utilisateur vide");
    }
    // Le mot de passe ne vit que le temps du calcul du jeton.
    let creds = {
        let password = rpassword::prompt_password("Mot de passe : ")?;
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

    match db::open_existing(env)? {
        Some(conn) => {
            let c = db::counts(&conn)?;
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
                c.tracks, c.albums, c.artists, c.playlists, megabytes(size)
            );
        }
        None => println!("Copie       : aucune (elle se fait toute seule après la connexion)"),
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
