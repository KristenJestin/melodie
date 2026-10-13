//! Pochettes du côté ligne de commande : remplacement de `_cover` dans les entrées du lanceur,
//! lancement détaché de `cover-fetch` et sa sous-commande cachée.

use anyhow::{Context, Result};
use melodie_core::cover::{BUDGET, LOCKED_EXIT, Local, apply_with, local, spawn_downloads};
use melodie_core::db;
use melodie_core::env::Env;
use serde_json::Value;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode, Stdio};
use std::sync::Arc;
use std::sync::mpsc::channel;
use std::time::Duration;

/// Remplace `_cover` par `icon` (chemin local) dans les entrées du lanceur.
/// Version de production : trousseau lu seulement s'il manque des vignettes,
/// reste du téléchargement confié à un processus détaché.
pub fn apply(env: &Env, entries: &mut [Value]) {
    apply_with(
        env,
        entries,
        BUDGET,
        || melodie_core::state::open_client(env).ok(),
        |ids| {
            if let Err(e) = spawn_background(ids) {
                eprintln!("melodie : pochettes en arrière-plan : {e:#}");
            }
        },
    );
}

/// Lance `melodie cover-fetch` détaché ; les identifiants passent par son
/// entrée standard et le processus relit lui-même le trousseau (aucun jeton
/// dans les arguments ni l'environnement).
fn spawn_background(ids: &[String]) -> Result<()> {
    let exe = std::env::current_exe().context("chemin du programme")?;
    let mut c = Command::new(exe);
    c.arg("cover-fetch")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY : `setsid` est sûr entre `fork` et `exec`.
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = c.spawn().context("lancement de cover-fetch")?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(ids.join("\n").as_bytes());
    }
    // Pas d'attente : le processus se termine seul.
    Ok(())
}

/// Sous-commande cachée `cover-fetch` : télécharge les vignettes lues sur l'entrée
/// standard. Un seul à la fois (verrou), sinon sortie immédiate avec `LOCKED_EXIT` : le
/// lanceur redemande à la frappe suivante, la fenêtre un peu plus tard.
pub fn fetch_cmd(env: &Env) -> Result<ExitCode> {
    let dir = db::cache_dir(env).join("covers");
    std::fs::create_dir_all(&dir).with_context(|| format!("création de {}", dir.display()))?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(dir.join(".fetch.lock"))?;
    // SAFETY : descripteur valide, détenu par `lock`.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Ok(ExitCode::from(LOCKED_EXIT));
    }
    let mut input = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut input)?;
    let ids: Vec<String> = input
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter(|id| matches!(local(env, id), Local::Missing))
        .map(String::from)
        .collect();
    if ids.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }
    let client = match melodie_core::state::open_client(env) {
        Ok(c) => Arc::new(c),
        Err(_) => return Ok(ExitCode::SUCCESS),
    };
    let (tx, rx) = channel();
    let n = ids.len();
    spawn_downloads(env, client, ids, tx);
    for _ in 0..n {
        if rx.recv_timeout(Duration::from_secs(30)).is_err() {
            break;
        }
    }
    Ok(ExitCode::SUCCESS)
}
