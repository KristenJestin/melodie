//! What the window asks of the `melodie` program: what needs the server (and so the keyring)
//! goes through it, so the window never holds a token.

use std::path::{Path, PathBuf};

/// What the window runs `melodie` for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    /// Nothing to play and no daemon: the personal mix (it starts the daemon).
    PlayMix,
    /// The heart of a track.
    Star { id: String, starred: bool },
}

impl Job {
    pub fn args(&self) -> Vec<String> {
        match self {
            Self::PlayMix => vec!["mix".into(), "me".into()],
            Self::Star { id, starred } => vec![
                if *starred { "like" } else { "unlike" }.into(),
                "track".into(),
                // an id may start with a dash
                "--".into(),
                id.clone(),
            ],
        }
    }
}

/// The `melodie` beside this program when there is one (a build or an install puts both in the
/// same folder), else the one on the `PATH`.
pub fn program(this_exe: Option<&Path>) -> PathBuf {
    this_exe
        .and_then(Path::parent)
        .map(|dir| dir.join("melodie"))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("melodie"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_arguments_of_each_job() {
        assert_eq!(Job::PlayMix.args(), ["mix", "me"]);
        let like = Job::Star {
            id: "t1".into(),
            starred: true,
        };
        assert_eq!(like.args(), ["like", "track", "--", "t1"]);
        let unlike = Job::Star {
            id: "t1".into(),
            starred: false,
        };
        assert_eq!(unlike.args(), ["unlike", "track", "--", "t1"]);
    }

    #[test]
    fn an_id_that_starts_with_a_dash_is_not_an_option() {
        let like = Job::Star {
            id: "-t1".into(),
            starred: true,
        };
        let args = like.args();
        let dashes = args.iter().position(|a| a == "--").unwrap();
        assert_eq!(args.iter().position(|a| a == "-t1"), Some(dashes + 1));
    }

    #[test]
    fn melodie_beside_the_window_first() {
        let dir = tempfile::tempdir().unwrap();
        let gui = dir.path().join("melodie-gui");
        assert_eq!(program(Some(&gui)), PathBuf::from("melodie"));
        std::fs::write(dir.path().join("melodie"), "").unwrap();
        assert_eq!(program(Some(&gui)), dir.path().join("melodie"));
        assert_eq!(program(None), PathBuf::from("melodie"));
    }
}
