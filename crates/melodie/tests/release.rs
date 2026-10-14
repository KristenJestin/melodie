//! The release scripts, run for real on stand-in programs: the archive `tools/package.sh` makes, the
//! `install.sh` inside it and the version of a beta. Nothing here touches the real `~/.local`, the
//! real configuration or a running player: every install goes into a temporary prefix, with the
//! stand-ins standing for the older player too.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A program that says its version, and logs `<name> quit` into $LOG when it is asked to quit.
fn stand_in(path: &Path, name: &str) {
    let script = format!(
        "#!/bin/sh\n\
         case \"$1\" in\n\
           --version) echo \"{name} 1.2.3\" ;;\n\
           quit) echo \"{name} quit\" >> \"$LOG\"; exit \"${{QUIT_STATUS:-0}}\" ;;\n\
         esac\n"
    );
    fs::write(path, script).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// The two programs of a build, as `cargo build --release` leaves them in a folder.
fn built(dir: &Path, marker: &str) {
    fs::create_dir_all(dir).unwrap();
    stand_in(&dir.join("melodie"), marker);
    stand_in(&dir.join("melodie-desktop"), marker);
}

fn run(command: &mut Command) -> Output {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// Packs the stand-ins as `version` and unpacks the archive: the folder an install starts from.
fn unpacked(work: &Path, version: &str, marker: &str) -> PathBuf {
    let binaries = work.join("target");
    built(&binaries, marker);
    let out = work.join("dist");
    let packed = run(Command::new(root().join("tools/package.sh"))
        .arg(version)
        .arg(&binaries)
        .arg(&out));
    let archive = String::from_utf8(packed.stdout).unwrap().trim().to_string();
    assert_eq!(archive, format!("melodie-{version}-linux-x86_64.tar.gz"));
    let extracted = work.join("extracted");
    fs::create_dir(&extracted).unwrap();
    run(Command::new("tar")
        .arg("-xzf")
        .arg(out.join(&archive))
        .arg("-C")
        .arg(&extracted));
    extracted.join(archive.trim_end_matches(".tar.gz"))
}

fn install(folder: &Path, work: &Path, prefix: &Path, quit_status: &str) -> Output {
    let home = work.join("home");
    fs::create_dir_all(&home).unwrap();
    run(Command::new("sh")
        .arg(folder.join("install.sh"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap())
        .env("HOME", &home)
        .env("PREFIX", prefix)
        .env("LOG", work.join("log"))
        .env("QUIT_STATUS", quit_status))
}

fn log(work: &Path) -> String {
    fs::read_to_string(work.join("log")).unwrap_or_default()
}

#[test]
fn the_archive_holds_both_programs_and_the_install_script_with_its_checksum() {
    let work = tempfile::tempdir().unwrap();
    let folder = unpacked(work.path(), "0.1.0", "new");
    let mut names: Vec<_> = fs::read_dir(&folder)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["install.sh", "melodie", "melodie-desktop"]);

    // Checked from the folder the two files are in, as a person who downloaded them does.
    run(Command::new("sha256sum")
        .arg("--check")
        .arg("melodie-0.1.0-linux-x86_64.tar.gz.sha256")
        .current_dir(work.path().join("dist")));
}

#[test]
fn install_puts_both_programs_in_the_prefix_and_leaves_the_user_alone() {
    let work = tempfile::tempdir().unwrap();
    let folder = unpacked(work.path(), "0.1.0", "new");
    let prefix = work.path().join("prefix");
    let output = install(&folder, work.path(), &prefix, "0");
    let said = String::from_utf8(output.stdout).unwrap();

    for program in ["melodie", "melodie-desktop"] {
        let installed = prefix.join("bin").join(program);
        assert!(installed.is_file(), "{program}");
        assert_eq!(
            fs::metadata(&installed).unwrap().permissions().mode() & 0o755,
            0o755
        );
        assert!(said.contains(&installed.display().to_string()), "{said}");
    }
    assert!(said.contains("new 1.2.3"), "{said}");
    // Not a file in the home folder: no configuration, no copy of the library, nothing.
    assert!(
        fs::read_dir(work.path().join("home"))
            .unwrap()
            .next()
            .is_none()
    );
    // And no player was asked to quit, as there was none.
    assert_eq!(log(work.path()), "");
}

#[test]
fn install_stops_the_older_player_before_replacing_it() {
    let work = tempfile::tempdir().unwrap();
    let folder = unpacked(work.path(), "0.2.0", "new");
    let prefix = work.path().join("prefix");
    built(&prefix.join("bin"), "old");

    let output = install(&folder, work.path(), &prefix, "0");

    assert_eq!(log(work.path()), "old quit\n");
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("Stopped the running player")
    );
    let installed = fs::read_to_string(prefix.join("bin/melodie")).unwrap();
    assert!(
        installed.contains("new 1.2.3"),
        "the older one was replaced"
    );
}

#[test]
fn install_goes_on_when_no_player_runs() {
    let work = tempfile::tempdir().unwrap();
    let folder = unpacked(work.path(), "0.2.0", "new");
    let prefix = work.path().join("prefix");
    built(&prefix.join("bin"), "old");

    // `melodie quit` exits 1 when there is nothing to quit.
    let output = install(&folder, work.path(), &prefix, "1");

    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("No player was running")
    );
    assert!(
        fs::read_to_string(prefix.join("bin/melodie-desktop"))
            .unwrap()
            .contains("new 1.2.3")
    );
}

/// A repository holding `tags`, one commit each, for `tools/beta-version.sh` to read.
fn beta_version(tags: &[&str], run_number: &str) -> Output {
    let repo = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        run(Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(["-c", "tag.gpgSign=false", "-c", "commit.gpgSign=false"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .current_dir(repo.path()))
    };
    git(&["init", "-q"]);
    for tag in tags {
        git(&["commit", "-q", "--allow-empty", "-m", tag]);
        git(&["tag", tag]);
    }
    Command::new(root().join("tools/beta-version.sh"))
        .arg(run_number)
        .current_dir(repo.path())
        .output()
        .unwrap()
}

#[test]
fn a_beta_is_the_patch_after_the_last_release_numbered_by_the_run() {
    let said = |tags: &[&str]| {
        let output = beta_version(tags, "21");
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap()
    };
    assert_eq!(said(&[]), "0.0.1-beta.21\n");
    assert_eq!(said(&["v0.1.0"]), "0.1.1-beta.21\n");
    // Sorted as versions, not as text; an older beta never counts as a release.
    assert_eq!(
        said(&["v0.9.0", "v0.10.0", "v0.10.1-beta.4"]),
        "0.10.1-beta.21\n"
    );
}

#[test]
fn a_beta_needs_the_number_of_a_run() {
    for bad in ["", "0", "x", "1.5"] {
        assert_eq!(beta_version(&[], bad).status.code(), Some(2), "{bad:?}");
    }
}
