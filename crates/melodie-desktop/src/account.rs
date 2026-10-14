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
use std::time::{Duration, Instant};

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

/// Whether `address` sends the token in clear over a network: plain `http://` to a host that is
/// neither this machine nor a private network. A warning, never a refusal.
pub fn token_goes_in_clear(address: &str) -> bool {
    let address = address.trim();
    let Some(rest) = address
        .get(..7)
        .filter(|scheme| scheme.eq_ignore_ascii_case("http://"))
        .map(|_| &address[7..])
    else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit('@').next().unwrap_or_default();
    let host = match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => host.split(':').next().unwrap_or_default(),
    };
    let host = host.to_ascii_lowercase();
    if host.is_empty() {
        return false;
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(v4) => {
                !(v4.is_loopback() || v4.is_private() || v4.is_link_local())
            }
            std::net::IpAddr::V6(v6) => {
                let first = v6.segments()[0];
                // unique local (fc00::/7) and link-local (fe80::/10)
                !(v6.is_loopback() || first & 0xfe00 == 0xfc00 || first & 0xffc0 == 0xfe80)
            }
        };
    }
    // a name on a home network: `localhost`, a bare machine name, `.local`, `.lan`
    let local_name = !host.contains('.')
        || ["localhost", "local", "lan", "home.arpa"]
            .iter()
            .any(|tld| host.ends_with(&format!(".{tld}")));
    !local_name
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

/// Whether the local copy dates from the sign-in or after: the first copy `melodie login` started.
/// Dates are whole seconds, so a copy made in the second the sign-in began counts.
pub fn is_new_copy(synced_at: Option<u64>, since: u64) -> bool {
    synced_at.is_some_and(|at| at >= since)
}

/// How a `melodie sync` run the window waited for ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncEnd {
    /// The copy is made (or another run holds it): the library is read again, nothing more awaited.
    Done,
    /// The server said something about the login.
    Standing(Standing),
    /// A local problem, in `melodie`'s own words (empty when it said none).
    Failed(String),
}

/// What `melodie sync` exiting with `code` means.
pub fn sync_end(code: Option<i32>, refused_marked: bool, said: &str) -> SyncEnd {
    match Standing::after(code, refused_marked) {
        Some(Standing::Ok) => SyncEnd::Done,
        Some(s) => SyncEnd::Standing(s),
        None => SyncEnd::Failed(said.to_string()),
    }
}

/// When the local copy was last made, in seconds since the epoch.
pub fn synced_at(env: &Env) -> Option<u64> {
    let conn = db::open_existing(env).ok()??;
    db::get_meta(&conn, "synced_at")?.parse().ok()
}

/// The local copy, as the window follows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyState {
    Missing,
    /// A first copy not finished yet (still running, or cut off): what it holds so far.
    Partial(db::Counts),
    /// Made at this date, in seconds since the epoch (0 when the copy does not say).
    Complete(u64),
}

/// Where the local copy stands; `None` when it could not be read this time (a copy busy being
/// written): no news, not a change. Blocking: run it away from the window.
pub fn copy_state(env: &Env) -> Option<CopyState> {
    let Some(conn) = db::open_existing(env).ok()? else {
        return Some(CopyState::Missing);
    };
    if db::is_partial(&conn) {
        return db::counts(&conn).ok().map(CopyState::Partial);
    }
    Some(CopyState::Complete(
        db::get_meta(&conn, "synced_at")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0),
    ))
}

/// Whether the window starts the first copy itself: the server said the login holds, there is no
/// complete copy, and the window did not start one yet. However the login came: in the window,
/// with `melodie login`, or a keyring brought from another machine.
pub fn starts_first_copy(standing: Standing, copy: CopyState, started: bool) -> bool {
    standing == Standing::Ok && !started && !matches!(copy, CopyState::Complete(_))
}

/// What the bottom of the window says while a first copy fills the library.
pub fn copy_notice(c: db::Counts) -> String {
    let n = |n: i64, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
    // the artists come first and are few: the albums and the tracks say how far it is
    match (c.albums, c.tracks) {
        (0, 0) => "Copying the library…".into(),
        (a, 0) => format!("Copying the library: {} so far", n(a, "album")),
        (a, t) => format!(
            "Copying the library: {}, {} so far",
            n(a, "album"),
            n(t, "track")
        ),
    }
}

/// A copy the window follows waits this long without moving before the window asks the server why.
pub const COPY_FOR: Duration = Duration::from_secs(15 * 60);
/// Looks at the copy that did not move, after this many looks in a row, ask whether a run still
/// holds the sync lock.
const STILL_LOOKS: u32 = 5;
/// The library is read again at most this often while a first copy fills: each read takes the
/// whole current view, the playlists and Home.
pub const RELOAD_EVERY: Duration = Duration::from_secs(4);

/// What the window knows of a copy it follows, look after look.
#[derive(Debug)]
pub struct Follower {
    since: u64,
    /// The window's own `melodie sync` runs: its end says when the copy stops, not the clock.
    pub own: bool,
    seen: Option<CopyState>,
    /// The copy grew since the library was last read again.
    unshown: bool,
    still: u32,
    moved: Instant,
    shown: Option<Instant>,
}

/// What a look at the copy gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Wait,
    /// The first copy grew: said at the bottom of the window.
    Moved(db::Counts),
    /// It grew, and the last read of the library is old enough: read it again.
    Grew(db::Counts),
    /// A copy made since the following began is complete, at this date.
    Done(u64),
    /// A copy the window did not start has not moved for `COPY_FOR`.
    Late,
    /// It has not moved for a while, and no run holds the sync lock: no one is making it.
    Gone,
}

impl Follower {
    /// Follows from `now`: a copy complete since `since` (seconds since the epoch) ends it.
    pub fn new(since: u64, own: bool, now: Instant) -> Self {
        Self {
            since,
            own,
            seen: None,
            unshown: false,
            still: 0,
            moved: now,
            shown: None,
        }
    }

    /// The last copy seen.
    pub fn seen(&self) -> Option<CopyState> {
        self.seen
    }

    /// Whether the next look should ask if a run holds the sync lock: only for a run the window
    /// cannot see end, once the copy has stood still for a few looks.
    pub fn probe_due(&self) -> bool {
        !self.own && (self.still + 1).is_multiple_of(STILL_LOOKS)
    }

    /// One look at the copy, at `now`; `running` says whether a run holds the sync lock, when it
    /// was asked (`probe_due`).
    pub fn step(&mut self, copy: CopyState, now: Instant, running: Option<bool>) -> Step {
        if let CopyState::Complete(at) = copy
            && is_new_copy(Some(at), self.since)
        {
            self.seen = Some(copy);
            return Step::Done(at);
        }
        if self.seen != Some(copy) {
            self.seen = Some(copy);
            self.still = 0;
            self.moved = now;
            let CopyState::Partial(c) = copy else {
                return Step::Wait;
            };
            if self
                .shown
                .is_some_and(|at| now.duration_since(at) < RELOAD_EVERY)
            {
                self.unshown = true;
                return Step::Moved(c);
            }
            self.shown = Some(now);
            self.unshown = false;
            return Step::Grew(c);
        }
        self.still += 1;
        // the last pages came too soon after a read: shown once it is old enough, not at the end
        if let CopyState::Partial(c) = copy
            && self.unshown
            && self
                .shown
                .is_some_and(|at| now.duration_since(at) >= RELOAD_EVERY)
        {
            self.shown = Some(now);
            self.unshown = false;
            return Step::Grew(c);
        }
        if self.own {
            return Step::Wait;
        }
        if running == Some(false) && self.still >= STILL_LOOKS {
            return Step::Gone;
        }
        if now.duration_since(self.moved) > COPY_FOR {
            return Step::Late;
        }
        Step::Wait
    }
}

/// Whether a `melodie sync` holds the lock at `path` (`$XDG_RUNTIME_DIR/melodie/sync.lock`).
pub fn sync_running_at(path: &Path) -> bool {
    use std::os::fd::AsRawFd as _;
    let Ok(f) = std::fs::File::open(path) else {
        return false;
    };
    // shared and let go at once: a run starting in between would only find it taken a moment,
    // as with `melodie status`
    // SAFETY: a valid descriptor, owned by `f`.
    unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) != 0 }
}

/// The lock a running `melodie sync` holds, whoever started it.
pub fn sync_lock() -> Option<std::path::PathBuf> {
    melodie_core::proto::runtime_dir()
        .ok()
        .map(|d| d.join("sync.lock"))
}

/// What a `melodie sync` that exited with `code` said of its failure. Killed (no code), it said
/// nothing: its last lines are progress.
pub fn failure_said(code: Option<i32>, stderr: &str) -> String {
    if code.is_none() {
        return String::new();
    }
    last_said(stderr)
}

/// What a failed `melodie sync` said: its last line, the progress lines come before.
pub fn last_said(stderr: &str) -> String {
    stderr
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .unwrap_or_default()
        .to_string()
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
    fn plain_http_to_a_host_off_the_private_networks_warns() {
        for clear in [
            "http://music.example.org",
            "http://music.example.org:4533/navidrome",
            "http://203.0.113.7:4533",
            "http://172.32.0.1",
            "http://172.15.0.1",
            "http://[2001:db8::1]:4533",
            "HTTP://music.example.org",
            "http://user@music.example.org",
        ] {
            assert!(token_goes_in_clear(clear), "{clear}");
        }
        for safe in [
            "https://music.example.org",
            "http://localhost:4533",
            "http://127.0.0.1:4533",
            "http://127.1.2.3",
            "http://[::1]:4533",
            "http://192.168.1.20:4533",
            "http://10.0.0.5",
            "http://172.16.0.1",
            "http://172.31.255.255",
            "http://169.254.1.1",
            "http://[fd12:3456::1]",
            "http://[fe80::1]",
            "http://navidrome:4533",
            "http://nas.local",
            "http://nas.lan:4533/",
            "http://user@192.168.1.20",
            "not an address",
            "",
        ] {
            assert!(!token_goes_in_clear(safe), "{safe}");
        }
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
    fn the_first_copy_is_the_one_made_since_the_sign_in_began() {
        assert!(is_new_copy(Some(101), 100));
        // the same second counts: dates are whole seconds, a small library is copied in one
        assert!(is_new_copy(Some(100), 100));
        // the copy of an older login is not the first copy of this one
        assert!(!is_new_copy(Some(99), 100));
        assert!(!is_new_copy(None, 100));
    }

    #[test]
    fn a_sync_that_returns_is_over_with_its_message() {
        assert_eq!(sync_end(Some(0), false, ""), SyncEnd::Done);
        assert_eq!(
            sync_end(Some(1), true, ""),
            SyncEnd::Standing(Standing::AuthRefused)
        );
        assert_eq!(
            sync_end(Some(2), false, "Serveur injoignable"),
            SyncEnd::Standing(Standing::Unreachable)
        );
        assert_eq!(
            sync_end(Some(3), false, "base locale : disk full"),
            SyncEnd::Failed("base locale : disk full".into())
        );
        assert_eq!(sync_end(None, false, ""), SyncEnd::Failed(String::new()));
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
    fn signed_in_with_no_local_copy_one_first_copy_starts() {
        assert!(starts_first_copy(Standing::Ok, CopyState::Missing, false));
        // once
        assert!(!starts_first_copy(Standing::Ok, CopyState::Missing, true));
        // a first copy cut off is taken up again
        let some = db::Counts {
            tracks: 500,
            albums: 1,
            artists: 2,
            playlists: 0,
        };
        assert!(starts_first_copy(
            Standing::Ok,
            CopyState::Partial(some),
            false
        ));
        // not before the server said yes, nor when it cannot
        for s in [
            Standing::Checking,
            Standing::NoLogin,
            Standing::AuthRefused,
            Standing::Unreachable,
        ] {
            assert!(!starts_first_copy(s, CopyState::Missing, false), "{s:?}");
        }
    }

    #[test]
    fn with_a_local_copy_no_first_copy_starts() {
        assert!(!starts_first_copy(
            Standing::Ok,
            CopyState::Complete(1_760_000_000),
            false
        ));
    }

    #[test]
    fn a_first_copy_is_followed_as_it_fills() {
        use melodie_core::api::{Client, Creds, token};
        use melodie_core::{fake::Fake, sync};
        let d = tempfile::tempdir().unwrap();
        let env = Env::with_home(d.path());
        assert_eq!(copy_state(&env), Some(CopyState::Missing));
        let creds = Creds {
            u: "alice".into(),
            t: token("sesame", "c19b2d"),
            s: "c19b2d".into(),
        };
        let fake = Fake::start(1203, &creds.t);
        let client = Client::new(&fake.url, creds);
        *fake.state.fail_from_song.lock().unwrap() = Some(500);
        assert!(sync::run(&env, &client, true).is_err());
        let so_far = db::Counts {
            tracks: 500,
            albums: 1,
            artists: 2,
            playlists: 0,
        };
        assert_eq!(copy_state(&env), Some(CopyState::Partial(so_far)));
        assert_eq!(
            copy_notice(so_far),
            "Copying the library: 1 album, 500 tracks so far"
        );
        *fake.state.fail_from_song.lock().unwrap() = None;
        sync::run(&env, &client, false).unwrap();
        assert!(matches!(copy_state(&env), Some(CopyState::Complete(at)) if at > 0));
    }

    #[test]
    fn a_count_that_cannot_be_read_is_no_news_of_the_copy() {
        let d = tempfile::tempdir().unwrap();
        let env = Env::with_home(d.path());
        let conn = db::create(&db::db_path(&env)).unwrap();
        db::set_meta(&conn, db::COPYING, "song:0").unwrap();
        assert!(matches!(copy_state(&env), Some(CopyState::Partial(_))));
        conn.execute("DROP TABLE playlists", []).unwrap();
        assert_eq!(copy_state(&env), None);
    }

    #[test]
    fn the_last_pages_are_shown_once_the_last_read_is_old_enough_not_at_the_end() {
        let t0 = Instant::now();
        let at = |s: f32| t0 + Duration::from_secs_f32(s);
        let mut f = Follower::new(1_760_000_000, true, t0);
        assert!(matches!(f.step(grown(500), at(1.), None), Step::Grew(_)));
        assert!(matches!(f.step(grown(1000), at(2.), None), Step::Moved(_)));
        assert_eq!(f.step(grown(1000), at(3.), None), Step::Wait);
        assert!(matches!(f.step(grown(1000), at(5.), None), Step::Grew(c) if c.tracks == 1000));
        // shown: nothing more until it grows
        assert_eq!(f.step(grown(1000), at(10.), None), Step::Wait);
    }

    fn grown(tracks: i64) -> CopyState {
        CopyState::Partial(db::Counts {
            tracks,
            albums: 1,
            artists: 2,
            playlists: 0,
        })
    }

    #[test]
    fn the_follower_reads_the_library_again_as_it_grows_at_most_every_few_seconds() {
        let t0 = Instant::now();
        let at = |s: f32| t0 + Duration::from_secs_f32(s);
        let mut f = Follower::new(1_760_000_000, true, t0);
        assert_eq!(f.step(CopyState::Missing, at(0.), None), Step::Wait);
        assert!(matches!(f.step(grown(500), at(1.), None), Step::Grew(c) if c.tracks == 500));
        assert!(matches!(f.step(grown(1000), at(2.), None), Step::Moved(c) if c.tracks == 1000));
        assert_eq!(f.step(grown(1000), at(3.), None), Step::Wait);
        assert!(matches!(f.step(grown(1500), at(5.5), None), Step::Grew(c) if c.tracks == 1500));
        // an older complete copy is not the one awaited
        assert_eq!(
            f.step(CopyState::Complete(1_700_000_000), at(6.), None),
            Step::Wait
        );
        assert_eq!(
            f.step(CopyState::Complete(1_760_000_001), at(7.), None),
            Step::Done(1_760_000_001)
        );
    }

    #[test]
    fn the_follower_of_a_run_it_cannot_see_stops_when_no_run_holds_the_lock() {
        let t0 = Instant::now();
        let mut f = Follower::new(1_760_000_000, false, t0);
        assert!(matches!(f.step(grown(500), t0, None), Step::Grew(_)));
        let mut probes = Vec::new();
        let mut last = Step::Wait;
        for i in 1..=10 {
            let due = f.probe_due();
            probes.push(due);
            // the run holds the lock at the first probe, not at the second
            let running = due.then_some(probes.iter().filter(|p| **p).count() < 2);
            last = f.step(grown(500), t0 + Duration::from_secs(i), running);
            if last != Step::Wait {
                break;
            }
        }
        assert_eq!(last, Step::Gone);
        assert_eq!(probes.iter().filter(|p| **p).count(), 2);
        // its own run is followed until it ends: never asked
        let mut f = Follower::new(1_760_000_000, true, t0);
        f.step(grown(500), t0, None);
        assert!((1..=20).all(|_| !f.probe_due() && f.step(grown(500), t0, None) == Step::Wait));
    }

    #[test]
    fn the_follower_of_a_run_it_cannot_see_is_late_after_a_quarter_of_an_hour_still() {
        let t0 = Instant::now();
        let mut f = Follower::new(1_760_000_000, false, t0);
        f.step(grown(500), t0, None);
        // the limit counts from the last step, not from the start
        let later = t0 + COPY_FOR - Duration::from_secs(1);
        assert!(matches!(f.step(grown(1000), later, None), Step::Grew(_)));
        assert_eq!(
            f.step(grown(1000), later + Duration::from_secs(2), None),
            Step::Wait
        );
        assert_eq!(
            f.step(grown(1000), later + COPY_FOR + Duration::from_secs(1), None),
            Step::Late
        );
        let mut own = Follower::new(1_760_000_000, true, t0);
        own.step(grown(500), t0, None);
        assert_eq!(own.step(grown(500), t0 + COPY_FOR * 2, None), Step::Wait);
    }

    #[test]
    fn the_sync_lock_says_whether_a_run_holds_it() {
        use std::os::fd::AsRawFd as _;
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("sync.lock");
        // the other tests start processes: one forked while a lock is open holds it until it
        // runs its program, so what is let go may still be held a moment
        let soon = |ok: &dyn Fn() -> bool| {
            (0..100).any(|_| {
                let done = ok();
                if !done {
                    std::thread::sleep(Duration::from_millis(10));
                }
                done
            })
        };
        assert!(!sync_running_at(&path));
        let f = std::fs::File::create(&path).unwrap();
        assert!(soon(&|| !sync_running_at(&path)));
        assert!(soon(&|| unsafe {
            libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0
        }));
        assert!(sync_running_at(&path));
        drop(f);
        assert!(soon(&|| !sync_running_at(&path)));
    }

    #[test]
    fn a_failure_mid_copy_says_its_last_line_not_the_progress() {
        let said = "Artistes : 2\nAlbums : 1\nerreur : boom\n\n";
        assert_eq!(last_said(said), "erreur : boom");
        assert_eq!(last_said(""), "");
    }

    #[test]
    fn a_sync_killed_mid_copy_says_none_of_its_progress() {
        let said = "Artistes : 2\nAlbums : 720\n";
        assert_eq!(failure_said(None, said), "");
        assert_eq!(
            failure_said(Some(3), "Albums : 1\nerreur : boom\n"),
            "erreur : boom"
        );
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
