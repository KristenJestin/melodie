//! `melodie import-poste`: takes over a setup of the old player, the `music` module of the poste
//! desktop. Its files are moved (never copied) to Mélodie's places and its keyring entry is
//! copied to Mélodie's, then removed. The name `poste` lives here and nowhere else in the code.

use crate::api::Creds;
use crate::env::Env;
use crate::proto;
use crate::store;
use anyhow::{Context, Result, anyhow, bail};
use secret_service::EncryptionType;
use secret_service::blocking::SecretService;
use std::collections::HashMap;
use std::io::ErrorKind;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

/// The `service` attribute of the old player's keyring entries.
const OLD_SERVICE: &str = "poste-music";

/// Reserved for tests: a file standing for the old player's keyring entry, as
/// `MELODIE_TEST_SECRET` stands for Mélodie's.
pub const TEST_OLD_SECRET_VAR: &str = "MELODIE_TEST_OLD_SECRET";

/// What happened to one thing of the old setup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Moved {
        from: PathBuf,
        to: PathBuf,
    },
    /// The new place was already taken: kept, and the old one left where it is.
    Kept {
        from: PathBuf,
        to: PathBuf,
    },
    SecretMoved,
    /// Mélodie already has a secret for this server: kept, the old entry left.
    SecretKept,
}

/// The old player's socket, `$XDG_RUNTIME_DIR/poste/music.sock`.
pub fn old_socket(runtime_base: &Path) -> PathBuf {
    runtime_base.join("poste/music.sock")
}

/// The old daemon is running when something accepts on its socket: a socket file left by a crash
/// refuses the connection. Whether it then answers changes nothing: a process holds the socket,
/// and it could write behind our back.
pub fn old_daemon_running(socket: &Path) -> bool {
    UnixStream::connect(socket).is_ok()
}

/// The files of the old player, old place → new place. The cache folder is handled entry by
/// entry so that its folder can be removed once empty.
fn file_moves(env: &Env) -> Vec<(PathBuf, PathBuf)> {
    let old_state = env.state_home.join("poste");
    let new_state = env.state_dir();
    vec![
        (
            env.config_home.join("poste/music.toml"),
            store::Settings::path(env),
        ),
        (old_state.join("music.toml"), new_state.join("state.toml")),
        // The logs are the player's too: moved, so that nothing of it stays in the desktop's folder.
        (old_state.join("music.log"), proto::log_path(env)),
        (old_state.join("music-sync.log"), new_state.join("sync.log")),
    ]
}

fn old_cache(env: &Env) -> PathBuf {
    env.cache_home.join("poste/music")
}

/// Runs the import. `runtime_base` is `$XDG_RUNTIME_DIR` (to find the old socket).
pub fn import(env: &Env, runtime_base: Option<&Path>) -> Result<Vec<Outcome>> {
    if let Some(base) = runtime_base
        && old_daemon_running(&old_socket(base))
    {
        bail!(
            "l'ancien lecteur (poste music) tourne encore : quitte-le (Quitter depuis le \
             lecteur du bureau, ou il quitte seul 10 minutes après l'arrêt de la musique), \
             puis relance l'import"
        );
    }

    let mut done = Vec::new();
    let mut moves = file_moves(env);
    let cache = old_cache(env);
    if let Ok(entries) = std::fs::read_dir(&cache) {
        let mut names: Vec<_> = entries
            .filter_map(|e| e.ok().map(|e| e.file_name()))
            .collect();
        names.sort();
        for name in names {
            moves.push((cache.join(&name), env.cache_dir().join(&name)));
        }
    }
    for (from, to) in moves {
        if let Some(o) = move_path(&from, &to)? {
            done.push(o);
        }
    }

    // Only empty folders go: the desktop keeps its own files in `poste/`.
    for dir in [
        cache,
        env.cache_home.join("poste"),
        env.config_home.join("poste"),
        env.state_home.join("poste"),
    ] {
        let _ = std::fs::remove_dir(dir);
    }

    if let Some(o) = move_secret(env)? {
        done.push(o);
    }
    Ok(done)
}

/// Moves `from` to `to`, unless `from` is absent (`None`) or `to` exists (kept).
pub fn move_path(from: &Path, to: &Path) -> Result<Option<Outcome>> {
    if std::fs::symlink_metadata(from).is_err() {
        return Ok(None);
    }
    let outcome = |moved: bool| {
        let (from, to) = (from.to_path_buf(), to.to_path_buf());
        Some(if moved {
            Outcome::Moved { from, to }
        } else {
            Outcome::Kept { from, to }
        })
    };
    if std::fs::symlink_metadata(to).is_ok() {
        return Ok(outcome(false));
    }
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("création de {}", dir.display()))?;
    }
    match std::fs::rename(from, to) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::CrossesDevices => copy_then_remove(from, to)?,
        Err(e) => {
            return Err(e).with_context(|| {
                format!("déplacement de {} vers {}", from.display(), to.display())
            });
        }
    }
    Ok(outcome(true))
}

/// The fallback of a rename across filesystems. The copy is made beside the target and renamed
/// in, so an interrupted copy never looks like a finished one.
pub fn copy_then_remove(from: &Path, to: &Path) -> Result<()> {
    let mut tmp = to.as_os_str().to_owned();
    tmp.push(".import-tmp");
    let tmp = PathBuf::from(tmp);
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::remove_file(&tmp);
    copy_tree(from, &tmp).with_context(|| format!("copie de {}", from.display()))?;
    std::fs::rename(&tmp, to).with_context(|| format!("remplacement de {}", to.display()))?;
    if std::fs::symlink_metadata(from)?.is_dir() {
        std::fs::remove_dir_all(from)
    } else {
        std::fs::remove_file(from)
    }
    .with_context(|| format!("suppression de {}", from.display()))
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(from)?;
    if meta.is_dir() {
        std::fs::create_dir(to)?;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            copy_tree(&e.path(), &to.join(e.file_name()))?;
        }
        Ok(())
    } else if meta.file_type().is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(from)?, to)
    } else {
        std::fs::copy(from, to).map(|_| ())
    }
}

/// Copies the old keyring entry of the configured server to Mélodie's, reads it back, and only
/// then removes the old one. The secret never leaves memory.
fn move_secret(env: &Env) -> Result<Option<Outcome>> {
    let settings = store::Settings::load(env)?;
    let Some(url) = settings.url.as_deref().filter(|u| !u.is_empty()) else {
        return Ok(None);
    };
    let Some(old) = old_secret(url)? else {
        return Ok(None);
    };
    if store::load_secret(url)?.is_some() {
        return Ok(Some(Outcome::SecretKept));
    }
    store::save_secret(url, &old).context("écriture du secret de Mélodie (l'ancien est gardé)")?;
    if store::load_secret(url)?.as_ref() != Some(&old) {
        bail!("le secret relu dans le trousseau diffère : l'ancien est gardé");
    }
    delete_old_secret(url)?;
    Ok(Some(Outcome::SecretMoved))
}

/// Where the old entry is in tests. With Mélodie's test keyring set and not this one, the old
/// entry is absent: a test never reaches the real keyring.
fn test_old_secret_file() -> Option<Option<PathBuf>> {
    let old = std::env::var_os(TEST_OLD_SECRET_VAR).filter(|v| !v.is_empty());
    let new = std::env::var_os(store::TEST_SECRET_VAR).filter(|v| !v.is_empty());
    match (old, new) {
        (Some(f), _) => Some(Some(PathBuf::from(f))),
        (None, Some(_)) => Some(None),
        (None, None) => None,
    }
}

fn ss_err(e: secret_service::Error) -> anyhow::Error {
    anyhow!("trousseau (Secret Service) : {e}")
}

fn old_attrs(url: &str) -> HashMap<&str, &str> {
    HashMap::from([("service", OLD_SERVICE), ("url", url)])
}

fn old_secret(url: &str) -> Result<Option<Creds>> {
    if let Some(f) = test_old_secret_file() {
        return Ok(f
            .and_then(|f| std::fs::read(f).ok())
            .and_then(|raw| serde_json::from_slice(&raw).ok()));
    }
    let ss = SecretService::connect(EncryptionType::Dh).map_err(ss_err)?;
    let found = ss.search_items(old_attrs(url)).map_err(ss_err)?;
    let item = match (found.unlocked.first(), found.locked.first()) {
        (Some(i), _) => i,
        (None, Some(i)) => {
            i.unlock().map_err(ss_err)?;
            i
        }
        (None, None) => return Ok(None),
    };
    let raw = item.get_secret().map_err(ss_err)?;
    Ok(serde_json::from_slice(&raw).ok())
}

fn delete_old_secret(url: &str) -> Result<()> {
    if let Some(f) = test_old_secret_file() {
        if let Some(f) = f {
            std::fs::remove_file(f).context("suppression de l'ancien secret")?;
        }
        return Ok(());
    }
    let ss = SecretService::connect(EncryptionType::Dh).map_err(ss_err)?;
    let found = ss.search_items(old_attrs(url)).map_err(ss_err)?;
    for item in found.unlocked.iter().chain(found.locked.iter()) {
        item.unlock().map_err(ss_err)?;
        item.delete().map_err(ss_err)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_move_across_filesystems_copies_then_removes() {
        let d = tempfile::tempdir().unwrap();
        let from = d.path().join("old");
        std::fs::create_dir_all(from.join("covers/thumbs")).unwrap();
        std::fs::write(from.join("library.db"), b"db").unwrap();
        std::fs::write(from.join("covers/thumbs/a.img"), b"a").unwrap();
        let to = d.path().join("new/music");
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        copy_then_remove(&from, &to).unwrap();
        assert!(!from.exists());
        assert_eq!(std::fs::read(to.join("library.db")).unwrap(), b"db");
        assert_eq!(std::fs::read(to.join("covers/thumbs/a.img")).unwrap(), b"a");
        assert!(!d.path().join("new/music.import-tmp").exists());
    }

    #[test]
    fn a_taken_place_is_kept_and_the_old_file_left() {
        let d = tempfile::tempdir().unwrap();
        let (from, to) = (d.path().join("a"), d.path().join("b"));
        std::fs::write(&from, b"old").unwrap();
        std::fs::write(&to, b"new").unwrap();
        assert!(matches!(
            move_path(&from, &to).unwrap(),
            Some(Outcome::Kept { .. })
        ));
        assert_eq!(std::fs::read(&from).unwrap(), b"old");
        assert_eq!(std::fs::read(&to).unwrap(), b"new");
        assert_eq!(move_path(&d.path().join("absent"), &to).unwrap(), None);
    }

    #[test]
    fn a_stale_socket_file_is_not_a_running_daemon() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("music.sock");
        assert!(!old_daemon_running(&p));
        drop(std::os::unix::net::UnixListener::bind(&p).unwrap());
        assert!(p.exists());
        assert!(!old_daemon_running(&p));
    }
}
