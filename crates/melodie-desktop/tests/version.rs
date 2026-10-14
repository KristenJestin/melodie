use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A temporary home that goes away even when the test panics.
struct TempHome(PathBuf);

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// Same rule as `melodie`: the version is the build's (MELODIE_VERSION), not Cargo.toml's. The
// flag is answered before anything else starts, so no window opens and no daemon is asked.
#[test]
fn version_comes_from_the_build_not_from_cargo_toml() {
    // Neither the real session nor the real configuration, whatever the program does with it.
    let home = TempHome(
        std::env::temp_dir().join(format!("melodie-desktop-version-{}", std::process::id())),
    );
    std::fs::create_dir_all(&home.0).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_melodie-desktop"))
        .arg("--version")
        .env_clear()
        .env("HOME", &home.0)
        .env("XDG_CONFIG_HOME", home.0.join("config"))
        .env("XDG_CACHE_HOME", home.0.join("cache"))
        .env("XDG_STATE_HOME", home.0.join("state"))
        .env("XDG_RUNTIME_DIR", &home.0)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    // A program that ignored the flag would start for real and never end: wait with a deadline,
    // and stop that very process.
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("melodie-desktop --version did not end within 10 s");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success());
    let mut said = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut said)
        .unwrap();
    let expected = option_env!("MELODIE_VERSION").unwrap_or("0.0.0-dev");
    assert_eq!(said, format!("melodie-desktop {expected}\n"));
}
