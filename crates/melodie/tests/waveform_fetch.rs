//! `melodie waveform-fetch`: the levels of a track computed from its stream, on the fake server,
//! with no sound on any output. HOME and XDG_* are temporary.

use melodie_core::env::Env;
use melodie_core::fake::Fake;
use melodie_core::waveform::{self, LEVELS};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::process::{Command, Stdio};

/// `EX_TEMPFAIL`, as `melodie_core::cover::LOCKED_EXIT`.
const LOCKED: i32 = 75;
const TOKEN: &str = "tok3f9a1c77";
const SALT: &str = "selb81d02";

/// A home with a login on the fake server (a temporary keyring file), and that server.
struct Logged {
    dir: tempfile::TempDir,
    fake: Fake,
}

/// How a run ended, and what it wrote.
struct Run {
    code: i32,
    output: String,
}

impl Logged {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::start(2, TOKEN);
        let conf = dir.path().join(".config/melodie");
        std::fs::create_dir_all(&conf).unwrap();
        std::fs::write(
            conf.join("config.toml"),
            format!("url = \"{}\"\nuser = \"alice\"\n", fake.url),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("secret.json"),
            format!(r#"{{"u":"alice","t":"{TOKEN}","s":"{SALT}"}}"#),
        )
        .unwrap();
        Self { dir, fake }
    }

    fn env(&self) -> Env {
        Env::with_home(self.dir.path())
    }

    fn fetch(&self, input: &str) -> Run {
        let home = self.dir.path();
        let mut child = Command::new(env!("CARGO_BIN_EXE_melodie"))
            .arg("waveform-fetch")
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("MELODIE_TEST_SECRET", home.join("secret.json"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        Run {
            code: out.status.code().unwrap(),
            output: format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        }
    }

    fn ramp(&self, id: &str) {
        self.fake.state.stream_ramp.lock().unwrap().push(id.into());
    }
}

#[test]
fn the_levels_of_a_known_sound_served_by_the_fake_server() {
    let l = Logged::new();
    l.ramp("s0");
    // the whole stream, as long as the track is said to be
    assert_eq!(l.fetch("s0\t3\n").code, 0);
    let levels = waveform::read(&l.env(), "s0").expect("waveform kept");
    assert_eq!(levels.len(), LEVELS);
    // the tone grows louder evenly: low at the start, full at the end, half way in the middle
    assert!(levels[2] < 20, "start {}", levels[2]);
    assert!(levels[LEVELS - 3] > 235, "end {}", levels[LEVELS - 3]);
    let mid = levels[LEVELS / 2];
    assert!((105..=150).contains(&mid), "middle {mid}");
}

#[test]
fn a_stream_cut_short_is_not_kept_and_leaves_no_file() {
    let l = Logged::new();
    l.ramp("s0");
    l.fake
        .state
        .stream_secs_of
        .lock()
        .unwrap()
        .insert("s0".into(), 20);
    l.fake.state.stream_cut.lock().unwrap().push("s0".into());
    let run = l.fetch("s0\t20\n");
    assert_eq!(run.code, 0);
    assert_eq!(waveform::read(&l.env(), "s0"), None, "{}", run.output);
    assert!(run.output.contains("ended early"), "{}", run.output);
    // the half that came was decoded into a file, removed after
    assert_eq!(left_in(&l), Vec::<std::ffi::OsString>::new());
}

/// The files of the waveforms' folder, but its lock.
fn left_in(l: &Logged) -> Vec<std::ffi::OsString> {
    std::fs::read_dir(waveform::dir(&l.env()))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name())
        .filter(|n| n != ".fetch.lock")
        .collect()
}

#[test]
fn a_waveform_kept_is_reused_without_fetching_the_track_again() {
    let l = Logged::new();
    l.ramp("s0");
    assert_eq!(l.fetch("s0").code, 0);
    assert_eq!(l.fake.calls_of("stream"), 1);
    let first = waveform::read(&l.env(), "s0");
    assert_eq!(l.fetch("s0").code, 0);
    assert_eq!(l.fake.calls_of("stream"), 1);
    assert_eq!(waveform::read(&l.env(), "s0"), first);
}

#[test]
fn the_current_track_and_the_next_one_are_computed_in_one_run() {
    let l = Logged::new();
    assert_eq!(l.fetch("s0\ns1\ns0\n").code, 0);
    assert_eq!(l.fake.calls_of("stream"), 2);
    // silence: a waveform all at zero, kept so that it is not computed again
    assert_eq!(waveform::read(&l.env(), "s1"), Some(vec![0; LEVELS]));
}

#[test]
fn a_waveform_fetch_that_finds_the_lock_taken_says_so() {
    let l = Logged::new();
    let dir = waveform::dir(&l.env());
    std::fs::create_dir_all(&dir).unwrap();
    let lock = std::fs::File::create(dir.join(".fetch.lock")).unwrap();
    // SAFETY: valid descriptor, owned by `lock`.
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    assert_eq!(l.fetch("s0").code, LOCKED);
    assert_eq!(l.fake.calls_of("stream"), 0);
}

#[test]
fn a_waveform_fetch_without_a_login_does_nothing_and_succeeds() {
    let l = Logged::new();
    std::fs::remove_file(l.dir.path().join(".config/melodie/config.toml")).unwrap();
    assert_eq!(l.fetch("s0").code, 0);
    assert_eq!(waveform::read(&l.env(), "s0"), None);
    assert_eq!(l.fake.calls_of("stream"), 0);
}

#[test]
fn a_stream_that_cannot_be_read_keeps_nothing_and_says_no_secret() {
    let l = Logged::new();
    *l.fake.state.fail_code.lock().unwrap() = Some(70);
    let run = l.fetch("s0");
    assert_eq!(run.code, 0);
    assert_eq!(waveform::read(&l.env(), "s0"), None);
    for secret in [TOKEN, SALT, "stream.view", "t="] {
        assert!(!run.output.contains(secret), "{secret} in {}", run.output);
    }
    // no decoded file left behind
    let left: Vec<_> = std::fs::read_dir(waveform::dir(&l.env()))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name())
        .filter(|n| n != ".fetch.lock")
        .collect();
    assert!(left.is_empty(), "{left:?}");
}
