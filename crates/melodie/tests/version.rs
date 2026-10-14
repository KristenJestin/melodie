use std::process::Command;

// `Cargo.toml` stays at 0.0.0: a release build names its version in MELODIE_VERSION (the tag
// without its `v`), a local build is `0.0.0-dev`.
#[test]
fn version_comes_from_the_build_not_from_cargo_toml() {
    let output = Command::new(env!("CARGO_BIN_EXE_melodie"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success());
    let expected = option_env!("MELODIE_VERSION").unwrap_or("0.0.0-dev");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("melodie {expected}\n")
    );
}
