//! `melodie import-poste` against a fake old layout: temporary HOME and XDG folders, keyrings
//! replaced by files (`MELODIE_TEST_SECRET`, `MELODIE_TEST_OLD_SECRET`), a fake old socket.
//! Nothing here touches the real folders, the real keyring or the real old player.

use melodie_core::env::Env;
use melodie_core::fake::{self, Fake};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TOKEN: &str = "tok3f9a1c77";
const SALT: &str = "selb81d02";

struct Sandbox {
    dir: tempfile::TempDir,
    fake: Fake,
}

impl Sandbox {
    /// A machine set up by the old player: connection, volume, logs, library copy with skips,
    /// covers, keyring entry, and files of the desktop beside them.
    fn old_setup() -> Sandbox {
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::start(0, TOKEN);
        *fake.state.songs.lock().unwrap() = vec![fake::song(1), fake::song(2)];
        let sb = Sandbox { dir, fake };
        std::fs::create_dir_all(sb.run_dir()).unwrap();

        // The library copy is built by a real sync, then put where the old player kept it.
        let conf = sb.config().join("melodie");
        std::fs::create_dir_all(&conf).unwrap();
        std::fs::write(conf.join("config.toml"), sb.settings()).unwrap();
        std::fs::write(sb.new_secret(), sb.creds().to_string()).unwrap();
        sb.ok(&["sync", "--full"]);
        let env = Env::with_home(sb.home());
        assert!(melodie_core::db::record_skip(&env, "s1", 1_000).unwrap());
        assert!(melodie_core::db::record_skip(&env, "s2", 2_000).unwrap());
        let covers = sb.cache().join("melodie/covers/thumbs");
        std::fs::create_dir_all(&covers).unwrap();
        std::fs::write(sb.cache().join("melodie/covers/al-1.img"), b"img").unwrap();
        std::fs::write(covers.join("al-1.img"), b"thumb").unwrap();
        std::fs::create_dir_all(sb.cache().join("poste")).unwrap();
        std::fs::rename(sb.cache().join("melodie"), sb.cache().join("poste/music")).unwrap();
        std::fs::remove_dir_all(conf).unwrap();
        std::fs::remove_file(sb.new_secret()).unwrap();
        std::fs::remove_dir_all(sb.state().join("melodie")).ok();

        write(&sb.config().join("poste/music.toml"), &sb.settings());
        write(&sb.config().join("poste/config.toml"), "desktop = true\n");
        write(&sb.state().join("poste/music.toml"), "volume = 0.42\n");
        write(&sb.state().join("poste/music.log"), "old daemon log\n");
        write(&sb.state().join("poste/music-sync.log"), "old sync log\n");
        write(&sb.state().join("poste/timers.json"), "[]\n");
        write(&sb.old_secret(), &sb.creds().to_string());
        sb
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }
    fn config(&self) -> PathBuf {
        self.home().join(".config")
    }
    fn state(&self) -> PathBuf {
        self.home().join(".local/state")
    }
    fn cache(&self) -> PathBuf {
        self.home().join(".cache")
    }
    fn run_dir(&self) -> PathBuf {
        self.dir.path().join("run")
    }
    fn new_secret(&self) -> PathBuf {
        self.dir.path().join("secret.json")
    }
    fn old_secret(&self) -> PathBuf {
        self.dir.path().join("old-secret.json")
    }
    fn settings(&self) -> String {
        format!("url = \"{}\"\nuser = \"alice\"\n", self.fake.url)
    }
    fn creds(&self) -> Value {
        json!({"u": "alice", "t": TOKEN, "s": SALT})
    }

    fn run(&self, args: &[&str]) -> Output {
        let h = self.home();
        Command::new(env!("CARGO_BIN_EXE_melodie"))
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &h)
            .env("XDG_CONFIG_HOME", self.config())
            .env("XDG_CACHE_HOME", self.cache())
            .env("XDG_STATE_HOME", self.state())
            .env("XDG_RUNTIME_DIR", self.run_dir())
            .env("MELODIE_TEST_SECRET", self.new_secret())
            .env("MELODIE_TEST_OLD_SECRET", self.old_secret())
            .env("MELODIE_AO", "null")
            .args(args)
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let o = self.run(args);
        assert!(
            o.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).to_string()
    }

    fn skips(&self) -> i64 {
        let conn = rusqlite::Connection::open(self.cache().join("melodie/library.db")).unwrap();
        conn.query_row("SELECT count(*) FROM skips", [], |r| r.get(0))
            .unwrap()
    }

    /// Every folder and regular file of the sandbox, with its content.
    fn snapshot(&self) -> Vec<(PathBuf, Vec<u8>)> {
        fn walk(p: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
            for e in std::fs::read_dir(p).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    out.push((p.clone(), Vec::new()));
                    walk(&p, out);
                } else if p.symlink_metadata().unwrap().is_file() {
                    out.push((p.clone(), std::fs::read(&p).unwrap()));
                }
            }
        }
        let mut out = Vec::new();
        walk(self.dir.path(), &mut out);
        out.sort();
        out
    }
}

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// The old daemon, played by a socket that answers. The thread is not joined: when the import
/// never connects, it is left waiting until the test process ends.
fn answering_old_socket(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let listener = UnixListener::bind(path).unwrap();
    std::thread::spawn(move || {
        while let Ok((s, _)) = listener.accept() {
            let mut line = String::new();
            let _ = BufReader::new(&s).read_line(&mut line);
            let _ = (&s).write_all(b"{\"ok\":true,\"now\":{\"state\":\"playing\"}}\n");
        }
    });
}

#[test]
fn everything_ends_up_in_the_new_places() {
    let sb = Sandbox::old_setup();
    write(&sb.cache().join("poste/music/auth-refused"), "1\n");
    let out = sb.ok(&["import-poste"]);

    assert_eq!(
        read(&sb.config().join("melodie/config.toml")),
        sb.settings()
    );
    assert_eq!(
        read(&sb.state().join("melodie/state.toml")),
        "volume = 0.42\n"
    );
    assert_eq!(
        read(&sb.state().join("melodie/melodie.log")),
        "old daemon log\n"
    );
    assert_eq!(read(&sb.state().join("melodie/sync.log")), "old sync log\n");
    assert!(sb.cache().join("melodie/library.db").is_file());
    assert_eq!(read(&sb.cache().join("melodie/covers/al-1.img")), "img");
    assert_eq!(
        read(&sb.cache().join("melodie/covers/thumbs/al-1.img")),
        "thumb"
    );
    assert!(sb.cache().join("melodie/auth-refused").is_file());
    let secret: Value = serde_json::from_str(&read(&sb.new_secret())).unwrap();
    assert_eq!(secret, sb.creds());

    // Moved, not copied: nothing of the player is left in the old places.
    assert!(!sb.config().join("poste/music.toml").exists());
    assert!(!sb.state().join("poste/music.toml").exists());
    assert!(!sb.state().join("poste/music.log").exists());
    assert!(!sb.state().join("poste/music-sync.log").exists());
    assert!(!sb.cache().join("poste").exists());
    assert!(!sb.old_secret().exists());

    // The output says what moved and never holds the secret.
    assert!(out.contains("config.toml"), "{out}");
    assert!(out.contains("library.db"), "{out}");
    assert!(out.contains("trousseau"), "{out}");
    let all = format!(
        "{out}{}",
        String::from_utf8_lossy(&sb.run(&["import-poste"]).stderr)
    );
    assert!(!all.contains(TOKEN) && !all.contains(SALT), "{all}");
}

#[test]
fn the_desktop_files_in_the_old_folders_are_left() {
    let sb = Sandbox::old_setup();
    sb.ok(&["import-poste"]);
    assert_eq!(
        read(&sb.config().join("poste/config.toml")),
        "desktop = true\n"
    );
    assert_eq!(read(&sb.state().join("poste/timers.json")), "[]\n");
}

#[test]
fn the_skips_survive() {
    let sb = Sandbox::old_setup();
    sb.ok(&["import-poste"]);
    assert_eq!(sb.skips(), 2);
}

#[test]
fn the_moved_copy_works_without_logging_in_again() {
    let sb = Sandbox::old_setup();
    sb.ok(&["import-poste"]);
    let title = fake::song(1)["title"].as_str().unwrap().to_string();
    let hits: Value = serde_json::from_str(&sb.ok(&["search", &title, "--json"])).unwrap();
    assert!(
        hits.as_array()
            .unwrap()
            .iter()
            .any(|h| h.to_string().contains(&title)),
        "{hits}"
    );
    let now: Value = serde_json::from_str(&sb.ok(&["now", "--json"])).unwrap();
    assert_eq!(now["state"], "stopped");
    // The moved token is accepted by the server: no new login needed.
    let status = sb.ok(&["status"]);
    assert!(
        status.contains("connecté") && !status.contains("refus"),
        "{status}"
    );
}

#[test]
fn a_second_run_changes_nothing() {
    let sb = Sandbox::old_setup();
    sb.ok(&["import-poste"]);
    let before = sb.snapshot();
    let out = sb.ok(&["import-poste"]);
    assert_eq!(sb.snapshot(), before);
    assert!(out.contains("Rien à importer"), "{out}");
}

#[test]
fn what_is_already_in_place_is_kept() {
    let sb = Sandbox::old_setup();
    write(
        &sb.config().join("melodie/config.toml"),
        "url = \"http://new.example\"\n",
    );
    let other = json!({"u": "bob", "t": "tnew", "s": "snew"}).to_string();
    write(&sb.new_secret(), &other);
    let out = sb.ok(&["import-poste"]);
    assert_eq!(
        read(&sb.config().join("melodie/config.toml")),
        "url = \"http://new.example\"\n"
    );
    assert_eq!(read(&sb.config().join("poste/music.toml")), sb.settings());
    assert_eq!(read(&sb.new_secret()), other);
    assert!(sb.old_secret().exists());
    assert!(out.contains("laissé"), "{out}");
    // Everything else moved.
    assert!(sb.cache().join("melodie/library.db").is_file());
}

#[test]
fn a_running_old_daemon_stops_the_import() {
    let sb = Sandbox::old_setup();
    let before = sb.snapshot();
    answering_old_socket(&sb.run_dir().join("poste/music.sock"));
    let o = sb.run(&["import-poste"]);
    assert_eq!(o.status.code(), Some(1));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("tourne encore"), "{err}");
    let mut after = sb.snapshot();
    after.retain(|(p, _)| !p.starts_with(sb.run_dir()));
    let mut before = before;
    before.retain(|(p, _)| !p.starts_with(sb.run_dir()));
    assert_eq!(after, before);
}

#[test]
fn a_stale_old_socket_does_not_block_the_import() {
    let sb = Sandbox::old_setup();
    let path = sb.run_dir().join("poste/music.sock");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    // A socket file nobody listens on, as left by a crash.
    drop(UnixListener::bind(&path).unwrap());
    assert!(path.exists());
    sb.ok(&["import-poste"]);
    assert!(sb.cache().join("melodie/library.db").is_file());
    // The runtime folder of the desktop is not ours to clean.
    assert!(path.exists());
}

#[test]
fn the_old_secret_is_kept_when_the_new_one_cannot_be_written() {
    let sb = Sandbox::old_setup();
    // A folder where the new keyring file should be: writing it fails.
    std::fs::create_dir_all(sb.new_secret()).unwrap();
    let o = sb.run(&["import-poste"]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(read(&sb.old_secret()), sb.creds().to_string());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!err.contains(TOKEN) && !err.contains(SALT), "{err}");
}
