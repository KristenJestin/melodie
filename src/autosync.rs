//! Synchronisation automatique de la copie locale : décisions (âge, verrou),
//! lancement détaché et corps de la synchronisation de fond.
//!
//! Jamais d'identifiant dans les arguments ni l'environnement du processus
//! détaché : il relit lui-même le trousseau.

use crate::state::{self, Failure};
use crate::{db, proto, sync};
use anyhow::{Context, Result};
use poste_core::time::{now_secs, rfc3339};
use poste_core::Env;
use rusqlite::Connection;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Âge au-delà duquel `search`, `home`, `playlists`, `mixes` et `mix` relancent une synchronisation.
pub const STALE_SECS: u64 = 3600;
/// Âge de la copie au-delà duquel le démon synchronise à son démarrage.
pub const DAEMON_FRESH_SECS: u64 = 900;
/// Période des synchronisations du démon.
pub const DAEMON_EVERY_SECS: u64 = 1800;

/// Taille au-delà de laquelle le journal est repris à zéro.
const LOG_MAX: u64 = 64 * 1024;

/// Seuil réglable par une variable d'environnement (réservée aux tests).
fn secs_env(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

pub fn stale_secs() -> u64 {
    secs_env("POSTE_MUSIC_STALE_SECS", STALE_SECS)
}

pub fn daemon_fresh_secs() -> u64 {
    secs_env("POSTE_MUSIC_DAEMON_FRESH_SECS", DAEMON_FRESH_SECS)
}

pub fn daemon_every_secs() -> u64 {
    secs_env("POSTE_MUSIC_DAEMON_EVERY_SECS", DAEMON_EVERY_SECS)
}

/// `$XDG_RUNTIME_DIR/poste/music-sync.lock`.
pub fn lock_path() -> Result<PathBuf> {
    Ok(proto::runtime_dir()?.join("music-sync.lock"))
}

/// `~/.local/state/poste/music-sync.log`.
pub fn log_path(env: &Env) -> PathBuf {
    env.state_dir().join("music-sync.log")
}

/// Prend le verrou de synchronisation ; `None` si une autre synchronisation le tient.
pub fn try_lock(path: &Path) -> Result<Option<std::fs::File>> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("création de {}", dir.display()))?;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("ouverture de {}", path.display()))?;
    // SAFETY : descripteur valide, détenu par `f`.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(Some(f));
    }
    let err = std::io::Error::last_os_error();
    if err.kind() == std::io::ErrorKind::WouldBlock {
        Ok(None)
    } else {
        Err(err).context("verrou de synchronisation")
    }
}

/// Une synchronisation tourne-t-elle ? (Verrou pris puis relâché aussitôt.)
pub fn is_running() -> bool {
    matches!(lock_path().and_then(|p| try_lock(&p)), Ok(None))
}

/// Âge de la copie en secondes ; une copie sans date compte comme très vieille.
pub fn age_secs(conn: &Connection) -> u64 {
    db::get_meta(conn, "synced_at")
        .and_then(|s| s.parse::<u64>().ok())
        .map_or(u64::MAX, |t| now_secs().saturating_sub(t))
}

/// Ce qu'il faut lancer.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Plan {
    Nothing,
    Incremental,
    Full,
}

/// Décision : `age` = `None` sans copie. Une synchronisation en cours suffit toujours.
pub fn plan(age: Option<u64>, running: bool, max_age: u64) -> Plan {
    if running {
        return Plan::Nothing;
    }
    match age {
        None => Plan::Full,
        Some(a) if a > max_age => Plan::Incremental,
        Some(_) => Plan::Nothing,
    }
}

/// Lance `poste-music sync --background [--full]` détaché (nouvelle session, aucune
/// entrée ni sortie). Il relit le trousseau lui-même.
pub fn spawn_background(full: bool) -> Result<()> {
    let exe = std::env::current_exe().context("chemin du programme")?;
    let mut c = Command::new(exe);
    c.arg("sync").arg("--background");
    if full {
        c.arg("--full");
    }
    c.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY : `setsid` est sûr entre `fork` et `exec`.
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = c.spawn().context("lancement de la synchronisation")?;
    // Évite un zombie quand le lanceur est le démon, qui vit longtemps.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Ajoute une ligne au journal de fond (sans jeton : seuls les messages de `Failure` y vont).
pub fn log_line(env: &Env, msg: &str) {
    let path = log_path(env);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let big = std::fs::metadata(&path).is_ok_and(|m| m.len() > LOG_MAX);
    let mut o = std::fs::OpenOptions::new();
    o.create(true).mode(0o600);
    if big {
        o.write(true).truncate(true);
    } else {
        o.append(true);
    }
    if let Ok(mut f) = o.open(&path) {
        let _ = writeln!(f, "{} {msg}", rfc3339(now_secs()));
    }
}

/// Corps de `sync --background` : une seule à la fois, erreurs au journal,
/// refus d'authentification noté par le marqueur habituel.
pub fn run_background(env: &Env, full: bool) -> Result<()> {
    let Some(_lock) = try_lock(&lock_path()?)? else {
        return Ok(());
    };
    let r = state::open_client(env).and_then(|c| sync::run(env, &c, full));
    state::record(env, &r);
    match r {
        Ok(rep) => log_line(
            env,
            &format!(
                "synchronisation {} terminée : {} morceaux",
                if rep.kind == sync::Kind::Full {
                    "complète"
                } else {
                    "incrémentale"
                },
                rep.counts.tracks
            ),
        ),
        Err(f) => log_line(
            env,
            &format!("échec de la synchronisation ({}) : {f}", f.state()),
        ),
    }
    Ok(())
}

/// Lance ce que `plan` décide ; renvoie l'échec du lancement, s'il y en a un.
pub fn launch(plan: Plan, spawn: &dyn Fn(bool) -> Result<()>) -> Result<()> {
    match plan {
        Plan::Nothing => Ok(()),
        Plan::Incremental => spawn(false),
        Plan::Full => spawn(true),
    }
}

/// Copie locale pour les commandes de lecture : l'existante, tout de suite, avec une
/// synchronisation de fond si elle est trop vieille ; sinon `Failure::Syncing`
/// après avoir lancé la première copie.
pub fn library_with(
    env: &Env,
    running: bool,
    spawn: &dyn Fn(bool) -> Result<()>,
) -> Result<Connection, Failure> {
    state::quick_check(env)?;
    let conn = db::open_existing(env)?;
    let age = conn.as_ref().map(age_secs);
    let p = plan(age, running, stale_secs());
    match conn {
        Some(conn) => {
            if let Err(e) = launch(p, spawn) {
                log_line(env, &format!("lancement impossible : {e:#}"));
            }
            Ok(conn)
        }
        None => {
            launch(p, spawn)?;
            Err(Failure::Syncing)
        }
    }
}

pub fn library(env: &Env) -> Result<Connection, Failure> {
    library_with(env, is_running(), &spawn_background)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Settings;
    use std::cell::RefCell;

    #[test]
    fn decision_selon_age_et_verrou() {
        assert_eq!(plan(None, false, 3600), Plan::Full);
        assert_eq!(plan(None, true, 3600), Plan::Nothing);
        assert_eq!(plan(Some(3600), false, 3600), Plan::Nothing);
        assert_eq!(plan(Some(3601), false, 3600), Plan::Incremental);
        assert_eq!(plan(Some(3601), true, 3600), Plan::Nothing);
        assert_eq!(plan(Some(0), false, 0), Plan::Nothing);
        assert_eq!(plan(Some(u64::MAX), false, 3600), Plan::Incremental);
    }

    #[test]
    fn verrou_exclusif() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("run/music-sync.lock");
        let a = try_lock(&p).unwrap();
        assert!(a.is_some());
        assert!(try_lock(&p).unwrap().is_none());
        drop(a);
        assert!(try_lock(&p).unwrap().is_some());
    }

    fn env_connecte() -> (tempfile::TempDir, Env) {
        let d = tempfile::tempdir().unwrap();
        let env = Env::with_home(d.path());
        Settings {
            url: Some("http://127.0.0.1:1".into()),
            user: Some("kris".into()),
        }
        .save(&env)
        .unwrap();
        (d, env)
    }

    fn copie(env: &Env, synced_at: u64) {
        let mut conn = db::create(&db::db_path(env)).unwrap();
        let tx = conn.transaction().unwrap();
        db::set_meta(&tx, "synced_at", &synced_at.to_string()).unwrap();
        tx.commit().unwrap();
    }

    fn essai(env: &Env, running: bool) -> (Result<Connection, Failure>, Vec<bool>) {
        let calls = RefCell::new(Vec::new());
        let r = library_with(env, running, &|full| {
            calls.borrow_mut().push(full);
            Ok(())
        });
        (r, calls.into_inner())
    }

    #[test]
    fn sans_copie_lance_la_complete_et_renvoie_l_entree() {
        let (_d, env) = env_connecte();
        let (r, calls) = essai(&env, false);
        assert!(matches!(r, Err(Failure::Syncing)));
        assert_eq!(calls, [true]);
        // Une sync déjà en cours : rien de plus lancé, même réponse.
        let (r, calls) = essai(&env, true);
        assert!(matches!(r, Err(Failure::Syncing)));
        assert!(calls.is_empty());
    }

    #[test]
    fn copie_recente_ne_lance_rien() {
        let (_d, env) = env_connecte();
        copie(&env, now_secs() - 600);
        let (r, calls) = essai(&env, false);
        assert!(r.is_ok());
        assert!(calls.is_empty());
    }

    #[test]
    fn copie_vieille_repond_et_lance_l_incrementale() {
        let (_d, env) = env_connecte();
        copie(&env, now_secs() - 3601 - 5);
        let (r, calls) = essai(&env, false);
        assert!(r.is_ok());
        assert_eq!(calls, [false]);
        let (r, calls) = essai(&env, true);
        assert!(r.is_ok());
        assert!(calls.is_empty());
    }

    #[test]
    fn refus_memorise_passe_avant() {
        let (_d, env) = env_connecte();
        copie(&env, 1);
        crate::store::mark_auth_refused(&env);
        let (r, calls) = essai(&env, false);
        assert!(matches!(r, Err(Failure::AuthRefused(_))));
        assert!(calls.is_empty());
    }

    #[test]
    fn journal_borne() {
        let (_d, env) = env_connecte();
        log_line(&env, "a");
        log_line(&env, "b");
        let t = std::fs::read_to_string(log_path(&env)).unwrap();
        assert_eq!(t.lines().count(), 2);
        std::fs::write(log_path(&env), vec![b'x'; LOG_MAX as usize + 1]).unwrap();
        log_line(&env, "c");
        assert_eq!(
            std::fs::read_to_string(log_path(&env))
                .unwrap()
                .lines()
                .count(),
            1
        );
    }
}
