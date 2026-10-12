//! Ce que le module garde entre deux lancements : réglages non secrets
//! (`music.toml`), secret dans le trousseau (Secret Service), dernier refus connu.

use crate::api::Creds;
use anyhow::{anyhow, Context, Result};
use poste_core::Env;
use secret_service::blocking::SecretService;
use secret_service::EncryptionType;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Valeur de l'attribut `service` des secrets de ce module.
pub const SERVICE: &str = "poste-music";

/// `~/.config/poste/music.toml` : adresse et utilisateur, rien de secret.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Settings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

impl Settings {
    pub fn path(env: &Env) -> PathBuf {
        env.config_home.join("poste/music.toml")
    }

    /// Réglages lus ; un fichier absent donne des réglages vides.
    pub fn load(env: &Env) -> Result<Self> {
        let p = Self::path(env);
        match std::fs::read_to_string(&p) {
            Ok(text) => toml::from_str(&text).with_context(|| format!("{} invalide", p.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("lecture de {}", p.display())),
        }
    }

    pub fn save(&self, env: &Env) -> Result<()> {
        let p = Self::path(env);
        let text = toml::to_string(self)?;
        write_atomic(&p, text.as_bytes())
    }

    /// Adresse et utilisateur, s'ils sont tous deux présents et non vides.
    pub fn login(&self) -> Option<(&str, &str)> {
        match (self.url.as_deref(), self.user.as_deref()) {
            (Some(u), Some(n)) if !u.is_empty() && !n.is_empty() => Some((u, n)),
            _ => None,
        }
    }
}

/// Écrit un fichier via un fichier temporaire voisin puis renommage.
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("création de {}", dir.display()))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, data).with_context(|| format!("écriture de {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("remplacement de {}", path.display()))
}

fn ss_err(e: secret_service::Error) -> anyhow::Error {
    anyhow!("trousseau (Secret Service) : {e}")
}

fn attrs(url: Option<&str>) -> HashMap<&str, &str> {
    let mut a = HashMap::from([("service", SERVICE)]);
    if let Some(u) = url {
        a.insert("url", u);
    }
    a
}

/// Réservé aux tests : si `POSTE_MUSIC_TEST_SECRET` est défini, il désigne un
/// fichier qui tient lieu de trousseau (jamais le vrai trousseau).
pub const TEST_SECRET_VAR: &str = "POSTE_MUSIC_TEST_SECRET";

fn test_secret_file() -> Option<PathBuf> {
    std::env::var_os(TEST_SECRET_VAR)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Range le secret `{"u","t","s"}` dans le trousseau, en remplaçant l'ancien.
pub fn save_secret(url: &str, creds: &Creds) -> Result<()> {
    if let Some(f) = test_secret_file() {
        return write_atomic(&f, &serde_json::to_vec(creds)?);
    }
    let ss = SecretService::connect(EncryptionType::Dh).map_err(ss_err)?;
    let col = ss.get_default_collection().map_err(ss_err)?;
    col.unlock().map_err(ss_err)?;
    let json = serde_json::to_vec(creds)?;
    col.create_item(
        &format!("Navidrome ({url})"),
        attrs(Some(url)),
        &json,
        true,
        "application/json",
    )
    .map_err(ss_err)?;
    Ok(())
}

/// Lit le secret du serveur `url`, `None` s'il n'y en a pas.
pub fn load_secret(url: &str) -> Result<Option<Creds>> {
    if let Some(f) = test_secret_file() {
        return Ok(std::fs::read(&f)
            .ok()
            .and_then(|raw| serde_json::from_slice(&raw).ok()));
    }
    let ss = SecretService::connect(EncryptionType::Dh).map_err(ss_err)?;
    let found = ss.search_items(attrs(Some(url))).map_err(ss_err)?;
    let item = match (found.unlocked.first(), found.locked.first()) {
        (Some(i), _) => i,
        (None, Some(i)) => {
            i.unlock().map_err(ss_err)?;
            i
        }
        (None, None) => return Ok(None),
    };
    let raw = item.get_secret().map_err(ss_err)?;
    // Un secret illisible n'est pas un secret : on redemande une connexion.
    Ok(serde_json::from_slice(&raw).ok())
}

/// Supprime les secrets de ce module (tous serveurs). Renvoie leur nombre.
pub fn delete_secrets() -> Result<usize> {
    if let Some(f) = test_secret_file() {
        return Ok(usize::from(std::fs::remove_file(&f).is_ok()));
    }
    let ss = SecretService::connect(EncryptionType::Dh).map_err(ss_err)?;
    let found = ss.search_items(attrs(None)).map_err(ss_err)?;
    let mut n = 0;
    for item in found.unlocked.iter().chain(found.locked.iter()) {
        item.unlock().map_err(ss_err)?;
        item.delete().map_err(ss_err)?;
        n += 1;
    }
    Ok(n)
}

/// Marqueur du dernier refus d'authentification, pour que `search` (qui ne
/// touche pas au réseau) sache qu'il faut se reconnecter.
fn refused_marker(env: &Env) -> PathBuf {
    crate::db::cache_dir(env).join("auth-refused")
}

pub fn mark_auth_refused(env: &Env) {
    let _ = write_atomic(&refused_marker(env), b"1\n");
}

pub fn clear_auth_refused(env: &Env) {
    let _ = std::fs::remove_file(refused_marker(env));
}

pub fn auth_refused_marked(env: &Env) -> bool {
    refused_marker(env).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reglages_aller_retour() {
        let d = tempfile::tempdir().unwrap();
        let env = Env::with_home(d.path());
        assert_eq!(Settings::load(&env).unwrap(), Settings::default());
        assert!(Settings::default().login().is_none());
        let s = Settings {
            url: Some("http://h:4533".into()),
            user: Some("kris".into()),
        };
        s.save(&env).unwrap();
        let back = Settings::load(&env).unwrap();
        assert_eq!(back, s);
        assert_eq!(back.login(), Some(("http://h:4533", "kris")));
    }

    #[test]
    fn marqueur_de_refus() {
        let d = tempfile::tempdir().unwrap();
        let env = Env::with_home(d.path());
        assert!(!auth_refused_marked(&env));
        mark_auth_refused(&env);
        assert!(auth_refused_marked(&env));
        clear_auth_refused(&env);
        assert!(!auth_refused_marked(&env));
    }
}
