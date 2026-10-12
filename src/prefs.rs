//! Réglages de lecture mémorisés entre deux démarrages du démon :
//! `~/.local/state/poste/music.toml` (aujourd'hui, seulement le volume).
//! Distinct de `~/.config/poste/music.toml` (connexion), que le module n'écrit pas ici.

use crate::store::write_atomic;
use anyhow::Result;
use poste_core::Env;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Volume au premier démarrage, en pourcentage (mpv : 100 = sans atténuation,
/// comme les autres applis : le niveau se règle ensuite au volume du système).
pub const DEFAULT_VOLUME: u32 = 100;
/// Plafond de mpv (`volume-max`), aussi celui de MPRIS ici (1,3).
pub const MAX_VOLUME: u32 = 130;

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prefs {
    /// Volume en pourcentage entier (0 à 130).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<u32>,
}

impl Prefs {
    pub fn path(env: &Env) -> PathBuf {
        env.state_dir().join("music.toml")
    }

    /// Réglages lus ; absents ou illisibles donnent les valeurs par défaut
    /// (un fichier abîmé ne doit jamais empêcher la musique de démarrer).
    pub fn load(env: &Env) -> Self {
        std::fs::read_to_string(Self::path(env))
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, env: &Env) -> Result<()> {
        write_atomic(&Self::path(env), toml::to_string(self)?.as_bytes())
    }

    /// Volume mémorisé, ou la valeur par défaut.
    pub fn volume(&self) -> u32 {
        self.volume.unwrap_or(DEFAULT_VOLUME).min(MAX_VOLUME)
    }
}

/// Applique `7`, `+5` ou `-5` à un volume courant ; borné à 0..=130.
pub fn parse_change(arg: &str, current: u32) -> Result<u32, String> {
    let a = arg.trim().trim_end_matches('%');
    let n: i64 = a
        .parse()
        .map_err(|_| format!("volume invalide : « {arg} » (attendu : 0 à 100, +5 ou -5)"))?;
    let v = if a.starts_with('+') || a.starts_with('-') {
        i64::from(current) + n
    } else {
        n
    };
    Ok(v.clamp(0, i64::from(MAX_VOLUME)) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changements_de_volume() {
        assert_eq!(parse_change("30", 50), Ok(30));
        assert_eq!(parse_change("+5", 50), Ok(55));
        assert_eq!(parse_change("-5", 50), Ok(45));
        assert_eq!(parse_change("-80", 50), Ok(0));
        assert_eq!(parse_change("500", 50), Ok(MAX_VOLUME));
        assert_eq!(parse_change("40%", 50), Ok(40));
        assert!(parse_change("fort", 50).is_err());
    }

    #[test]
    fn defaut_et_borne() {
        assert_eq!(Prefs::default().volume(), DEFAULT_VOLUME);
        assert_eq!(Prefs { volume: Some(999) }.volume(), MAX_VOLUME);
    }
}
