//! Notifications de bureau de l'application « Mélodie ».

use melodie_core::store::TEST_SECRET_VAR;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

/// En test (secret de test défini), aucune notification réelle.
fn muted() -> bool {
    std::env::var_os(TEST_SECRET_VAR).is_some_and(|v| !v.is_empty())
}

/// `notify-send -a Mélodie -- <résumé> <corps>`.
pub fn command(summary: &str, body: &str) -> Command {
    let mut c = Command::new("notify-send");
    c.args(["-a", "Mélodie", "--", summary, body]);
    c
}

/// Comme [`command`], avec le glyphe Nerd Font que le démon de notifications du bureau dessine
/// dans l'icône (`omarchy-glyph`) ; les autres démons ignorent l'indice.
pub fn command_with_glyph(summary: &str, body: &str, glyph: char) -> Command {
    let mut c = Command::new("notify-send");
    c.args(["-a", "Mélodie", "-h"])
        .arg(format!("string:omarchy-glyph:{glyph}"))
        .args(["--", summary, body]);
    c
}

/// Notification simple ; un échec de `notify-send` est sans conséquence.
pub fn send(summary: &str, body: &str) {
    run(command(summary, body));
}

/// Notification dont l'icône porte un glyphe.
pub fn send_with_glyph(summary: &str, body: &str, glyph: char) {
    run(command_with_glyph(summary, body, glyph));
}

fn run(mut c: Command) {
    if muted() {
        return;
    }
    let _ = c
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Script du processus détaché : attend le clic puis ouvre la reconnexion.
const RECONNECT_SCRIPT: &str = r#"a=$(notify-send -a Mélodie -t 60000 -A "default=Se reconnecter" -- "$1" "$2" 2>/dev/null)
[ "$a" = default ] && exec xdg-terminal-exec melodie login
exit 0"#;

/// Commande de la notification « reconnexion requise » : un clic lance
/// `xdg-terminal-exec melodie login`.
pub fn reconnect_command() -> Command {
    let mut c = Command::new("sh");
    c.args([
        "-c",
        RECONNECT_SCRIPT,
        "sh",
        "Navidrome : reconnexion requise",
        "Clique pour lancer melodie login",
    ]);
    c
}

/// Lance la notification dans un processus détaché (le démon va s'arrêter).
pub fn reconnect_required() {
    if muted() {
        return;
    }
    let mut c = reconnect_command();
    c.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY : `setsid` est sûr entre `fork` et `exec`.
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let _ = c.spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commande_de_notification() {
        let c = command("Titre", "Corps");
        assert_eq!(c.get_program(), "notify-send");
        let args: Vec<_> = c
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(args, ["-a", "Mélodie", "--", "Titre", "Corps"]);
    }

    #[test]
    fn commande_avec_glyphe() {
        let c = command_with_glyph("Titre", "Corps", '\u{F02D1}');
        assert_eq!(c.get_program(), "notify-send");
        let args: Vec<_> = c
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(
            args,
            [
                "-a",
                "Mélodie",
                "-h",
                "string:omarchy-glyph:\u{F02D1}",
                "--",
                "Titre",
                "Corps"
            ]
        );
    }

    #[test]
    fn reconnexion_lance_le_login() {
        let c = reconnect_command();
        let args: Vec<_> = c
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert!(args[1].contains("xdg-terminal-exec melodie login"));
        assert!(args[1].contains("-a Mélodie"));
        assert_eq!(args[3], "Navidrome : reconnexion requise");
    }
}
