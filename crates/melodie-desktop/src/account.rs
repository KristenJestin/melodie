//! Signing in from the window. The window never holds a token: it runs `melodie login
//! --password-stdin` (the password on its standard input, never on its command line, which every
//! process can read), `melodie logout`, `melodie status` and `melodie sync`, and reads what they
//! leave: the address and the user in the configuration, the refusal marker, the local copy.

use crate::model::Connection;
use melodie_core::env::Env;
use melodie_core::{api, db, store};
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

/// What the window knows of the login.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// A login is kept; the server was not asked yet.
    Checking,
    Ok,
    NoLogin,
    /// The server refused the token (Subsonic 40 to 44): handled like no login.
    AuthRefused,
    /// The server did not answer: the login is kept, and never asked for again.
    Unreachable,
}

impl Standing {
    /// What the configuration and the refusal marker say, without the keyring or the network.
    pub fn known(env: &Env) -> Standing {
        let logged = store::Settings::load(env).is_ok_and(|s| s.login().is_some());
        if !logged {
            Standing::NoLogin
        } else if store::auth_refused_marked(env) {
            Standing::AuthRefused
        } else {
            Standing::Checking
        }
    }

    /// After `melodie status` or `melodie login` exited with `code` (`Failure::exit_code`); `None`
    /// when it does not tell (a local problem).
    pub fn after(code: Option<i32>, refused_marked: bool) -> Option<Standing> {
        match code? {
            0 => Some(Standing::Ok),
            // the two are one exit code; the marker set by the same run tells them apart
            1 if refused_marked => Some(Standing::AuthRefused),
            1 => Some(Standing::NoLogin),
            2 => Some(Standing::Unreachable),
            _ => None,
        }
    }
}

/// What fills the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Library,
    SignIn,
}

/// The sign-in screen when there is no login or the server refused it, whoever said so last:
/// the window's own check or the daemon.
pub fn screen(standing: Standing, daemon: Connection) -> Screen {
    match (standing, daemon) {
        (Standing::NoLogin | Standing::AuthRefused, _) | (_, Connection::AuthRefused) => {
            Screen::SignIn
        }
        _ => Screen::Library,
    }
}

/// What the bottom of the window says of the connection.
pub fn notice(standing: Standing) -> Option<&'static str> {
    (standing == Standing::Unreachable).then_some("Navidrome is unreachable")
}

/// What the sign-in screen asks: a correct address and a user, or why not.
pub fn check_form(server: &str, user: &str) -> Result<(String, String), &'static str> {
    let url = api::server_address(server)
        .ok_or("The address starts with http:// or https://, then the server's name")?;
    let user = user.trim();
    if user.is_empty() {
        return Err("Who are you on this server?");
    }
    Ok((url, user.to_string()))
}

/// How a sign-in ended. Holds no password, and no token: `melodie` kept it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    SignedIn,
    Refused,
    Unreachable,
    /// Anything else, in `melodie`'s own words.
    Failed(String),
}

impl Outcome {
    pub fn message(&self) -> String {
        match self {
            Self::SignedIn => "Signed in".into(),
            Self::Refused => "The server refused this user or password".into(),
            Self::Unreachable => {
                "Could not reach the server: check the address, or try again later".into()
            }
            Self::Failed(why) => why.clone(),
        }
    }
}

/// The arguments of `melodie login` for this server and user; the password goes on its input.
pub fn login_args(server: &str, user: &str) -> Vec<String> {
    // `--user=…` in one word: a name may start with a dash
    vec![
        "login".into(),
        "--password-stdin".into(),
        format!("--server={server}"),
        format!("--user={user}"),
    ]
}

/// Runs `program login --password-stdin` and writes `password` on its input. Blocking: run it away
/// from the window. The password is written straight from the caller's buffer, never copied.
pub fn sign_in(program: &Path, server: &str, user: &str, password: &str) -> Outcome {
    let child = Command::new(program)
        .args(login_args(server, user))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return Outcome::Failed(format!("{}: {}", program.display(), e.kind())),
    };
    if let Some(mut input) = child.stdin.take() {
        // a failed write shows as the child's own failure below
        let _ = input
            .write_all(password.as_bytes())
            .and_then(|()| input.write_all(b"\n"));
    }
    let out = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => return Outcome::Failed(format!("{}: {}", program.display(), e.kind())),
    };
    let said = String::from_utf8_lossy(&out.stderr);
    let said = said.lines().next().filter(|l| !l.is_empty());
    match out.status.code() {
        Some(0) => Outcome::SignedIn,
        Some(1) => Outcome::Refused,
        // clap's usage error (an old `melodie` on the PATH) is also 2, and always says "error:"
        Some(2) if !said.is_some_and(|l| l.starts_with("error:")) => Outcome::Unreachable,
        _ => Outcome::Failed(said.unwrap_or("Could not sign in").to_string()),
    }
}

/// When the local copy was last made, in seconds since the epoch.
pub fn synced_at(env: &Env) -> Option<u64> {
    let conn = db::open_existing(env).ok()??;
    db::get_meta(&conn, "synced_at")?.parse().ok()
}

/// "Synced 5 min ago", for the settings.
pub fn synced_text(synced_at: Option<u64>, now: u64) -> String {
    let Some(at) = synced_at else {
        return "Not synced yet".into();
    };
    match now.saturating_sub(at) {
        0..60 => "Synced just now".into(),
        s @ 60..3_600 => format!("Synced {} min ago", s / 60),
        s @ 3_600..86_400 => format!("Synced {} h ago", s / 3_600),
        s if s < 2 * 86_400 => "Synced 1 day ago".into(),
        s => format!("Synced {} days ago", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const PASSWORD: &str = "correct horse battery";

    fn env_with(config: Option<&str>, refused: bool) -> (tempfile::TempDir, Env) {
        let d = tempfile::tempdir().unwrap();
        let env = Env::with_home(d.path());
        if let Some(c) = config {
            let p = store::Settings::path(&env);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, c).unwrap();
        }
        if refused {
            store::mark_auth_refused(&env);
        }
        (d, env)
    }

    const LOGGED_IN: &str = "url = \"https://music.example.org\"\nuser = \"alice\"\n";

    #[test]
    fn no_login_shows_the_sign_in_screen() {
        let (_d, env) = env_with(None, false);
        assert_eq!(Standing::known(&env), Standing::NoLogin);
        // after a logout the address stays, the user goes
        let (_d, env) = env_with(Some("url = \"https://music.example.org\"\n"), false);
        assert_eq!(Standing::known(&env), Standing::NoLogin);
        // a configuration without a secret in the keyring: `status` says no login
        assert_eq!(Standing::after(Some(1), false), Some(Standing::NoLogin));
        assert_eq!(screen(Standing::NoLogin, Connection::Ok), Screen::SignIn);
        assert_eq!(notice(Standing::NoLogin), None);
    }

    #[test]
    fn a_refused_login_shows_the_sign_in_screen() {
        let (_d, env) = env_with(Some(LOGGED_IN), true);
        assert_eq!(Standing::known(&env), Standing::AuthRefused);
        assert_eq!(Standing::after(Some(1), true), Some(Standing::AuthRefused));
        assert_eq!(
            screen(Standing::AuthRefused, Connection::Ok),
            Screen::SignIn
        );
        // the daemon says it while the window believed the login good
        assert_eq!(
            screen(Standing::Ok, Connection::AuthRefused),
            Screen::SignIn
        );
        assert_eq!(
            screen(Standing::Checking, Connection::AuthRefused),
            Screen::SignIn
        );
    }

    #[test]
    fn an_unreachable_server_is_said_and_never_asks_to_sign_in() {
        assert_eq!(Standing::after(Some(2), false), Some(Standing::Unreachable));
        // an old refusal marker does not turn an unreachable server into a refused login
        assert_eq!(Standing::after(Some(2), true), Some(Standing::Unreachable));
        assert_eq!(
            screen(Standing::Unreachable, Connection::Ok),
            Screen::Library
        );
        assert_eq!(
            screen(Standing::Ok, Connection::Unreachable),
            Screen::Library
        );
        let said = notice(Standing::Unreachable).unwrap();
        assert!(said.contains("unreachable"), "{said}");
        assert!(!said.to_lowercase().contains("sign in"), "{said}");
        assert!(!said.contains("login"), "{said}");
        assert!(!Outcome::Unreachable.message().contains("password"));
    }

    #[test]
    fn a_kept_login_shows_the_library_while_it_is_checked() {
        let (_d, env) = env_with(Some(LOGGED_IN), false);
        assert_eq!(Standing::known(&env), Standing::Checking);
        assert_eq!(screen(Standing::Checking, Connection::Ok), Screen::Library);
        assert_eq!(Standing::after(Some(0), false), Some(Standing::Ok));
        assert_eq!(screen(Standing::Ok, Connection::Ok), Screen::Library);
        // a local problem tells nothing of the login
        assert_eq!(Standing::after(Some(3), false), None);
        assert_eq!(Standing::after(None, false), None);
    }

    #[test]
    fn the_form_wants_an_address_and_a_user() {
        assert_eq!(
            check_form(" https://music.example.org/ ", " alice "),
            Ok(("https://music.example.org".into(), "alice".into()))
        );
        assert!(check_form("music.example.org", "alice").is_err());
        assert!(check_form("https://music.example.org", "  ").is_err());
    }

    #[test]
    fn the_password_is_never_an_argument() {
        let args = login_args("https://music.example.org", "-alice");
        assert_eq!(
            args,
            [
                "login",
                "--password-stdin",
                "--server=https://music.example.org",
                "--user=-alice"
            ]
        );
    }

    /// A stand-in for `melodie`: keeps its arguments and its input beside it, says `stderr`,
    /// exits `code`. One script, written once and linked from each test's folder: a file being
    /// written while another test forks cannot be run ("text file busy").
    fn stand_in(dir: &Path, code: i32, stderr: &str) -> std::path::PathBuf {
        static SCRIPT: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
        let shared = SCRIPT.get_or_init(|| {
            let d = tempfile::tempdir().unwrap();
            let p = d.path().join("melodie");
            std::fs::write(
                &p,
                "#!/bin/sh\nd=$(dirname \"$0\")\nprintf '%s\\n' \"$@\" > \"$d/args\"\n\
                 cat > \"$d/input\"\ncat \"$d/stderr\" >&2\nexit $(cat \"$d/code\")\n",
            )
            .unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            d
        });
        std::fs::write(dir.join("code"), code.to_string()).unwrap();
        std::fs::write(dir.join("stderr"), format!("{stderr}\n")).unwrap();
        let p = dir.join("melodie");
        if !p.exists() {
            std::os::unix::fs::symlink(shared.path().join("melodie"), &p).unwrap();
        }
        p
    }

    #[test]
    fn a_sign_in_gives_the_password_on_the_input_of_melodie_only() {
        let d = tempfile::tempdir().unwrap();
        let program = stand_in(d.path(), 0, "");
        let out = sign_in(&program, "https://music.example.org", "alice", PASSWORD);
        assert_eq!(out, Outcome::SignedIn);
        assert_eq!(
            std::fs::read_to_string(d.path().join("input")).unwrap(),
            format!("{PASSWORD}\n")
        );
        let args = std::fs::read_to_string(d.path().join("args")).unwrap();
        assert!(!args.contains(PASSWORD), "{args}");
        assert!(args.contains("--password-stdin"));
    }

    #[test]
    fn the_password_is_in_no_structure_that_outlives_the_sign_in() {
        let d = tempfile::tempdir().unwrap();
        for (code, said) in [
            (0, ""),
            (1, "refused"),
            (2, "unreachable"),
            (3, "keyring locked"),
        ] {
            let program = stand_in(d.path(), code, said);
            let out = sign_in(&program, "https://music.example.org", "alice", PASSWORD);
            for text in [format!("{out:?}"), out.message()] {
                assert!(!text.contains(PASSWORD), "{text}");
            }
        }
    }

    #[test]
    fn each_end_of_a_sign_in_is_told_apart() {
        let d = tempfile::tempdir().unwrap();
        let run = |code, said| {
            let program = stand_in(d.path(), code, said);
            sign_in(&program, "https://music.example.org", "alice", PASSWORD)
        };
        assert_eq!(run(1, "x"), Outcome::Refused);
        assert_eq!(run(2, "x"), Outcome::Unreachable);
        assert_eq!(
            run(3, "melodie : keyring locked"),
            Outcome::Failed("melodie : keyring locked".into())
        );
        assert!(Outcome::Refused.message().contains("refused"));
        let missing = sign_in(
            &d.path().join("nowhere"),
            "https://music.example.org",
            "alice",
            PASSWORD,
        );
        assert!(matches!(missing, Outcome::Failed(_)), "{missing:?}");
    }

    #[test]
    fn only_the_exit_codes_of_melodie_are_read_as_a_refusal_or_an_unreachable_server() {
        let d = tempfile::tempdir().unwrap();
        let run = |code, said| {
            let program = stand_in(d.path(), code, said);
            sign_in(&program, "https://music.example.org", "alice", PASSWORD)
        };
        // a local failure of `melodie login` (keyring, configuration, input) says itself
        assert_eq!(
            run(3, "keyring locked\nmore"),
            Outcome::Failed("keyring locked".into())
        );
        // codes `login` never gives: failed, with what it said, or a plain sentence
        for code in [4, 5, 101, 127] {
            assert_eq!(run(code, "boom"), Outcome::Failed("boom".into()), "{code}");
        }
        assert_eq!(run(4, ""), Outcome::Failed("Could not sign in".into()));
        // an old `melodie` without --password-stdin: clap's usage error also exits 2
        let usage = "error: unexpected argument '--password-stdin' found";
        assert_eq!(run(2, usage), Outcome::Failed(usage.into()));
        // and the real unreachable server (exit 2) is still told
        assert_eq!(
            run(
                2,
                "Serveur injoignable (https://music.example.org) : timeout"
            ),
            Outcome::Unreachable
        );
    }

    #[test]
    fn the_last_sync_is_read_from_the_local_copy() {
        let d = tempfile::tempdir().unwrap();
        let env = Env::with_home(d.path());
        assert_eq!(synced_at(&env), None);
        let mut conn = db::create(&db::db_path(&env)).unwrap();
        let tx = conn.transaction().unwrap();
        db::set_meta(&tx, "synced_at", "1760000000").unwrap();
        tx.commit().unwrap();
        assert_eq!(synced_at(&env), Some(1_760_000_000));
    }

    #[test]
    fn the_last_sync_is_said_in_words() {
        let now = 1_760_000_000;
        assert_eq!(synced_text(None, now), "Not synced yet");
        assert_eq!(synced_text(Some(now - 20), now), "Synced just now");
        assert_eq!(synced_text(Some(now - 300), now), "Synced 5 min ago");
        assert_eq!(synced_text(Some(now - 7_200), now), "Synced 2 h ago");
        assert_eq!(
            synced_text(Some(now - 3 * 86_400), now),
            "Synced 3 days ago"
        );
        assert_eq!(synced_text(Some(now - 86_400), now), "Synced 1 day ago");
        // a clock set back says nothing odd
        assert_eq!(synced_text(Some(now + 50), now), "Synced just now");
    }
}
