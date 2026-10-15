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

/// Runs `install.sh`, with `extra_path` (a folder of stand-in system commands) ahead of the real
/// ones, and returns whatever it did, failure included.
fn try_install(
    folder: &Path,
    work: &Path,
    prefix: &Path,
    quit_status: &str,
    extra_path: Option<&Path>,
) -> Output {
    let home = work.join("home");
    fs::create_dir_all(&home).unwrap();
    let mut path = std::env::var("PATH").unwrap();
    if let Some(dir) = extra_path {
        path = format!("{}:{path}", dir.display());
    }
    Command::new("sh")
        .arg(folder.join("install.sh"))
        .env_clear()
        .env("PATH", path)
        .env("HOME", &home)
        .env("PREFIX", prefix)
        .env("LOG", work.join("log"))
        .env("QUIT_STATUS", quit_status)
        .output()
        .unwrap()
}

fn install(folder: &Path, work: &Path, prefix: &Path, quit_status: &str) -> Output {
    let output = try_install(folder, work, prefix, quit_status, None);
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn log(work: &Path) -> String {
    fs::read_to_string(work.join("log")).unwrap_or_default()
}

#[test]
fn the_archive_holds_both_programs_the_desktop_entry_and_the_install_script_with_its_checksum() {
    let work = tempfile::tempdir().unwrap();
    let folder = unpacked(work.path(), "0.1.0", "new");
    let mut names: Vec<_> = fs::read_dir(&folder)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "install.sh",
            "melodie",
            "melodie-desktop",
            "melodie.desktop"
        ]
    );

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
    // Each program was copied beside its place and renamed: nothing of the copy stays.
    let mut names: Vec<_> = fs::read_dir(prefix.join("bin"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["melodie", "melodie-desktop"]);
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

// The desktop finds the window (and matches MPRIS's DesktopEntry) through this file.
#[test]
fn install_puts_the_desktop_entry_where_desktops_look_for_it() {
    let work = tempfile::tempdir().unwrap();
    let folder = unpacked(work.path(), "0.1.0", "new");
    // `share/applications` does not exist in a fresh prefix: install creates it.
    let prefix = work.path().join("prefix");

    let output = install(&folder, work.path(), &prefix, "0");

    let entry = prefix.join("share/applications/melodie.desktop");
    let text = fs::read_to_string(&entry).unwrap();
    assert!(text.contains("\nExec=melodie-desktop\n"), "{text}");
    assert!(text.contains("\nName=Mélodie\n"), "{text}");
    assert!(text.contains("\nStartupWMClass=melodie\n"), "{text}");
    // What lets a launcher show the icon and find the player by typing "music".
    assert!(text.contains("\nIcon=melodie\n"), "{text}");
    assert!(text.contains("\nGenericName=Music player\n"), "{text}");
    assert!(
        text.contains("\nKeywords=music;musique;navidrome;melodie;\n"),
        "{text}"
    );
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains(&entry.display().to_string())
    );
    // A real desktop-file-validate, when the machine has one.
    if Command::new("desktop-file-validate")
        .arg("--version")
        .output()
        .is_ok()
    {
        run(Command::new("desktop-file-validate").arg(&entry));
    }
}

#[test]
fn install_refreshes_the_desktop_database_when_the_tool_exists_and_does_without() {
    let work = tempfile::tempdir().unwrap();
    let folder = unpacked(work.path(), "0.1.0", "new");
    let system = work.path().join("system");
    fs::create_dir(&system).unwrap();
    let tool = system.join("update-desktop-database");
    fs::write(
        &tool,
        "#!/bin/sh\necho \"database $1\" >> \"$LOG\"\nexit 3\n",
    )
    .unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
    let prefix = work.path().join("prefix");

    // A tool that fails does not fail the install: the entry is there all the same.
    let output = try_install(&folder, work.path(), &prefix, "0", Some(&system));

    assert!(output.status.success());
    assert!(prefix.join("share/applications/melodie.desktop").is_file());
    assert_eq!(
        log(work.path()),
        format!("database {}\n", prefix.join("share/applications").display())
    );
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

// `melodie quit` exits 1 only when nothing ran. An older `melodie` that does not know `quit` (clap
// exits 2) or one that cannot load a library (127) says nothing about the player still running.
#[test]
fn install_does_not_call_it_no_player_when_quit_failed_otherwise() {
    for status in ["2", "127"] {
        let work = tempfile::tempdir().unwrap();
        let folder = unpacked(work.path(), "0.2.0", "new");
        let prefix = work.path().join("prefix");
        built(&prefix.join("bin"), "old");

        let output = install(&folder, work.path(), &prefix, status);

        let said = String::from_utf8(output.stdout).unwrap();
        let warned = String::from_utf8(output.stderr).unwrap();
        assert!(!said.contains("No player was running"), "{status}: {said}");
        assert!(!said.contains("Stopped the running player"), "{status}");
        assert!(
            warned.contains("Could not stop the player"),
            "{status}: {warned}"
        );
        // The new programs are in place all the same: the warning tells to stop the player.
        assert!(
            fs::read_to_string(prefix.join("bin/melodie"))
                .unwrap()
                .contains("new 1.2.3")
        );
    }
}

// Without libmpv the new `melodie` does not even answer `--version`: the hint about the library
// has to come before, or it never shows.
#[test]
fn install_tells_about_libmpv_when_the_new_melodie_cannot_start() {
    let work = tempfile::tempdir().unwrap();
    let folder = unpacked(work.path(), "0.2.0", "new");
    // What the loader does for a missing library: no answer, status 127.
    fs::write(
        folder.join("melodie"),
        "#!/bin/sh\necho 'error while loading shared libraries: libmpv.so.2' >&2\nexit 127\n",
    )
    .unwrap();
    let system = work.path().join("system");
    fs::create_dir(&system).unwrap();
    let ldd = system.join("ldd");
    fs::write(&ldd, "#!/bin/sh\necho '\tlibmpv.so.2 => not found'\n").unwrap();
    fs::set_permissions(&ldd, fs::Permissions::from_mode(0o755)).unwrap();
    let prefix = work.path().join("prefix");

    let output = try_install(&folder, work.path(), &prefix, "0", Some(&system));

    let warned = String::from_utf8(output.stderr).unwrap();
    assert!(warned.contains("install libmpv"), "{warned}");
    assert!(warned.contains("melodie does not start"), "{warned}");
    // The files are installed; the exit status says the install is not usable yet.
    assert!(!output.status.success());
    assert!(prefix.join("bin/melodie-desktop").is_file());
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
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .current_dir(repo.path()))
    };
    git(&["init", "-q"]);
    for tag in tags {
        git(&["commit", "-q", "--allow-empty", "-m", tag]);
        git(&["tag", tag]);
    }
    Command::new(root().join("tools/beta-version.sh"))
        .arg(run_number)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
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
