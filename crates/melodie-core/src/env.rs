//! Where the configuration, state and cache folders are.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Set by the window on every `melodie` it runs: it tells a failure in its own toast, so the
/// command sends no desktop notification for a failure on top; a success still notifies. The
/// launcher and the keys leave it unset.
pub const QUIET_FAILURES_VAR: &str = "MELODIE_QUIET_FAILURES";

/// The XDG base folders, read once and passed to the functions, so tests never touch the real
/// environment.
#[derive(Debug, Clone)]
pub struct Env {
    pub config_home: PathBuf,
    pub state_home: PathBuf,
    pub cache_home: PathBuf,
}

/// The value of an XDG variable when it is set and absolute, else the default.
fn xdg(var: &str, fallback: PathBuf) -> PathBuf {
    match std::env::var_os(var) {
        Some(v) if !v.is_empty() && Path::new(&v).is_absolute() => PathBuf::from(v),
        _ => fallback,
    }
}

impl Env {
    /// Reads the process environment (`HOME`, `XDG_*`).
    pub fn from_process() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .context("la variable HOME n'est pas définie")?;
        Ok(Self {
            config_home: xdg("XDG_CONFIG_HOME", home.join(".config")),
            state_home: xdg("XDG_STATE_HOME", home.join(".local/state")),
            cache_home: xdg("XDG_CACHE_HOME", home.join(".cache")),
        })
    }

    /// The default folders under `home`.
    pub fn with_home(home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        Self {
            config_home: home.join(".config"),
            state_home: home.join(".local/state"),
            cache_home: home.join(".cache"),
        }
    }

    /// `$XDG_STATE_HOME/melodie`.
    pub fn state_dir(&self) -> PathBuf {
        self.state_home.join("melodie")
    }

    /// `$XDG_CACHE_HOME/melodie`.
    pub fn cache_dir(&self) -> PathBuf {
        self.cache_home.join("melodie")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_folders_under_home() {
        let e = Env::with_home("/h");
        assert_eq!(e.config_home, PathBuf::from("/h/.config"));
        assert_eq!(e.state_dir(), PathBuf::from("/h/.local/state/melodie"));
        assert_eq!(e.cache_dir(), PathBuf::from("/h/.cache/melodie"));
    }
}
