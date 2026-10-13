//! `melodie cover-fetch` tells "another one holds the lock, nothing done" from "done": the
//! window asks again for the first and not for the second. HOME and XDG_* are temporary.

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
