//! `melodie cover-fetch` tells "another one holds the lock, nothing done" from "done": the
//! window asks again for the first and not for the second. HOME and XDG_* are temporary.

use melodie_core::cover::{self, Tier};
use melodie_core::env::Env;
use melodie_core::fake::Fake;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::process::{Command, Stdio};

/// `EX_TEMPFAIL`: the same value as `melodie_core::cover::LOCKED_EXIT`, written out so that a
/// change of it is seen here.
const LOCKED: i32 = 75;

fn cover_fetch(home: &std::path::Path, ids: &str) -> i32 {
    let mut child = Command::new(env!("CARGO_BIN_EXE_melodie"))
        .arg("cover-fetch")
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(ids.as_bytes())
        .unwrap();
    child.wait().unwrap().code().unwrap()
}

fn hold_lock(home: &std::path::Path) -> std::fs::File {
    let dir = home.join(".cache/melodie/covers");
    std::fs::create_dir_all(&dir).unwrap();
    let lock = std::fs::File::create(dir.join(".fetch.lock")).unwrap();
    // SAFETY: valid descriptor, owned by `lock`.
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    lock
}

#[test]
fn a_cover_fetch_that_finds_the_lock_taken_says_so() {
    let home = tempfile::tempdir().unwrap();
    let _lock = hold_lock(home.path());
    assert_eq!(cover_fetch(home.path(), "al-1\nal-2"), LOCKED);
}

#[test]
fn a_cover_fetch_that_ran_exits_successfully() {
    let home = tempfile::tempdir().unwrap();
    // no login: it has nothing to download with, and it did run
    assert_eq!(cover_fetch(home.path(), "al-1"), 0);
    assert_eq!(cover_fetch(home.path(), ""), 0);
}

const TOKEN: &str = "tok3f9a1c77";
const SALT: &str = "selb81d02";

/// A home with a login on the fake server (a temporary keyring file), and that server.
struct Logged {
    dir: tempfile::TempDir,
    fake: Fake,
}

impl Logged {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::start(0, TOKEN);
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

    fn fetch(&self, input: &str) -> i32 {
        let home = self.dir.path();
        let mut child = Command::new(env!("CARGO_BIN_EXE_melodie"))
            .arg("cover-fetch")
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("MELODIE_TEST_SECRET", home.join("secret.json"))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait().unwrap().code().unwrap()
    }

    fn sizes_asked(&self) -> Vec<String> {
        let mut sizes: Vec<String> = self
            .fake
            .state
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.contains("getCoverArt"))
            .filter_map(|c| c.split("size=").nth(1))
            .map(|s| s.split('&').next().unwrap().to_string())
            .collect();
        sizes.sort();
        sizes
    }
}

#[test]
fn cover_fetch_without_a_size_fetches_the_launchers_thumbnail_and_nothing_else() {
    let t = Logged::new();
    assert_eq!(t.fetch("al-1\nal-2\n"), 0);
    let env = t.env();
    for id in ["al-1", "al-2"] {
        assert!(cover::tier_path(&env, id, Tier::Thumb).is_file());
        assert!(!cover::tier_path(&env, id, Tier::Row).exists());
        assert!(!cover::tier_path(&env, id, Tier::Large).exists());
    }
    assert_eq!(t.sizes_asked(), ["128", "128"]);
}

#[test]
fn cover_fetch_with_a_size_fetches_that_tier_beside_the_thumbnail() {
    let t = Logged::new();
    assert_eq!(t.fetch("al-1\t300\nal-2\t512\nal-3\t77\n"), 0);
    let env = t.env();
    assert!(cover::tier_path(&env, "al-1", Tier::Row).is_file());
    assert!(cover::tier_path(&env, "al-2", Tier::Large).is_file());
    assert!(
        !cover::tier_path(&env, "al-3", Tier::Thumb).exists(),
        "a size that is no tier is not fetched"
    );
    // the thumbnail comes first, to know the server's default image: 2 thumbnails, 2 tiers
    assert_eq!(t.sizes_asked(), ["128", "128", "300", "512"]);
}

#[test]
fn cover_fetch_does_not_fetch_a_tier_that_is_there() {
    let t = Logged::new();
    assert_eq!(t.fetch("al-1\t300\n"), 0);
    t.fake.clear_calls();
    assert_eq!(t.fetch("al-1\t300\nal-1\n"), 0);
    assert!(
        t.sizes_asked().is_empty(),
        "both the tier and the thumbnail are on disk"
    );
}
