//! Notifications de bureau de l'application « Musique ».

use crate::store::TEST_SECRET_VAR;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

/// En test (secret de test défini), aucune notification réelle.
fn muted() -> bool {
    std::env::var_os(TEST_SECRET_VAR).is_some_and(|v| !v.is_empty())
}

/// `notify-send -a Musique -- <résumé> <corps>`.
pub fn command(summary: &str, body: &str) -> Command {
    let mut c = Command::new("notify-send");
    c.args(["-a", "Musique", "--", summary, body]);
    c
}

/// Notification simple ; un échec de `notify-send` est sans conséquence.
pub fn send(summary: &str, body: &str) {
    if muted() {
        return;
    }
    let _ = command(summary, body)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Script du processus détaché : attend le clic puis ouvre la reconnexion.
const RECONNECT_SCRIPT: &str = r#"a=$(notify-send -a Musique -t 60000 -A "default=Se reconnecter" -- "$1" "$2" 2>/dev/null)
[ "$a" = default ] && exec xdg-terminal-exec poste music login
exit 0"#;

/// Commande de la notification « reconnexion requise » : un clic lance
/// `xdg-terminal-exec poste music login`.
pub fn reconnect_command() -> Command {
    let mut c = Command::new("sh");
    c.args([
        "-c",
        RECONNECT_SCRIPT,
        "sh",
        "Navidrome : reconnexion requise",
        "Clique pour lancer poste music login",
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
        assert_eq!(args, ["-a", "Musique", "--", "Titre", "Corps"]);
    }

    #[test]
    fn reconnexion_lance_le_login() {
        let c = reconnect_command();
        let args: Vec<_> = c
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert!(args[1].contains("xdg-terminal-exec poste music login"));
        assert!(args[1].contains("-a Musique"));
        assert_eq!(args[3], "Navidrome : reconnexion requise");
    }
}
