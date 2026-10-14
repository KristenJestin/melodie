use std::process::Command;

// Same rule as `melodie`: the version is the build's (MELODIE_VERSION), not Cargo.toml's. The
// flag is answered before anything else starts, so no window opens and no daemon is asked.
#[test]
fn version_comes_from_the_build_not_from_cargo_toml() {
    // Neither the real session nor the real configuration, whatever the program does with it.
    let home = std::env::temp_dir().join(format!("melodie-desktop-version-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_melodie-desktop"))
        .arg("--version")
        .env_clear()
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_RUNTIME_DIR", &home)
        .output()
        .unwrap();
    std::fs::remove_dir_all(&home).unwrap();
    assert!(output.status.success());
    let expected = option_env!("MELODIE_VERSION").unwrap_or("0.0.0-dev");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("melodie-desktop {expected}\n")
    );
}
