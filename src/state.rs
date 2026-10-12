//! Les trois états de connexion et leur présentation (texte et JSON du lanceur).
//!
//! 1. `no-login` : pas de secret dans le trousseau (ou configuration absente).
//! 2. `auth-refused` : le serveur refuse le jeton (codes 40 à 44), traité comme `no-login`.
//! 3. `unreachable` : réseau, délai, HTTP non 200, JSON invalide ; on ne demande
//!    surtout pas de se reconnecter.

use crate::api::{ApiError, Client};
use crate::store::{self, Settings};
use anyhow::anyhow;
use poste_core::Env;
use serde_json::{json, Value};
use std::fmt;

#[derive(Debug)]
pub enum Failure {
    NoLogin,
    AuthRefused(String),
    Unreachable {
        url: String,
        detail: String,
    },
    /// Aucune copie locale : la première synchronisation tourne en arrière-plan.
    Syncing,
    /// Problème local (trousseau, disque, base) : jamais muet, jamais confondu avec un état.
    Internal(anyhow::Error),
}

impl Failure {
    /// Nom de l'état, tel qu'écrit par `status`.
    pub fn state(&self) -> &'static str {
        match self {
            Failure::NoLogin => "no-login",
            Failure::AuthRefused(_) => "auth-refused",
            Failure::Unreachable { .. } => "unreachable",
            Failure::Syncing => "syncing",
            Failure::Internal(_) => "error",
        }
    }

    /// Code de sortie en sortie lisible.
    pub fn exit_code(&self) -> u8 {
        match self {
            Failure::NoLogin | Failure::AuthRefused(_) => 1,
            Failure::Unreachable { .. } => 2,
            Failure::Internal(_) => 3,
            Failure::Syncing => 4,
        }
    }

    /// Entrée unique au format du lanceur, à la place des résultats.
    pub fn launcher_entry(&self) -> Value {
        match self {
            Failure::NoLogin | Failure::AuthRefused(_) => json!({
                "id": "login",
                "name": "Se connecter à Navidrome",
                "subtitle": "Connexion requise",
                "icon": "󰍂",
                "action": {"type": "command", "command": "xdg-terminal-exec poste music login"}
            }),
            Failure::Unreachable { url, .. } => json!({
                "id": "unreachable",
                "name": "Navidrome injoignable",
                "subtitle": url,
                "icon": "󰅛"
            }),
            Failure::Syncing => json!({
                "id": "syncing",
                "name": "Copie de la bibliothèque en cours…",
                "subtitle": "Les résultats arrivent dans un instant",
                "icon": "󰑓"
            }),
            Failure::Internal(e) => json!({
                "id": "error",
                "name": "Erreur du module music",
                "subtitle": format!("{e:#}"),
                "icon": "󰅛"
            }),
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::NoLogin => write!(f, "Pas connecté : lance `poste music login`"),
            Failure::AuthRefused(detail) => write!(
                f,
                "Connexion refusée par le serveur ({detail}) : lance `poste music login`"
            ),
            Failure::Unreachable { url, detail } => {
                write!(f, "Serveur injoignable ({url}) : {detail}")
            }
            Failure::Syncing => write!(
                f,
                "Copie de la bibliothèque en cours en arrière-plan : réessaie dans un instant"
            ),
            Failure::Internal(e) => write!(f, "{e:#}"),
        }
    }
}

impl From<anyhow::Error> for Failure {
    fn from(e: anyhow::Error) -> Self {
        Failure::Internal(e)
    }
}

impl From<rusqlite::Error> for Failure {
    fn from(e: rusqlite::Error) -> Self {
        Failure::Internal(anyhow!("base locale : {e}"))
    }
}

/// Convertit une erreur d'API en état, en connaissant l'adresse du serveur.
pub fn from_api(e: ApiError, url: &str) -> Failure {
    match e {
        ApiError::AuthRefused(m) => Failure::AuthRefused(m),
        ApiError::Unreachable(detail) => Failure::Unreachable {
            url: url.to_string(),
            detail,
        },
    }
}

/// Client prêt à l'emploi d'après la configuration et le trousseau.
pub fn open_client(env: &Env) -> Result<Client, Failure> {
    let settings = Settings::load(env)?;
    let Some((url, _user)) = settings.login() else {
        return Err(Failure::NoLogin);
    };
    match store::load_secret(url)? {
        Some(creds) => Ok(Client::new(url, creds)),
        None => Err(Failure::NoLogin),
    }
}

/// Garde-fou rapide, sans trousseau ni réseau : réglages absents ou dernier
/// refus connu. Sert à `search` pour répondre vite sur la copie locale.
pub fn quick_check(env: &Env) -> Result<Settings, Failure> {
    let settings = Settings::load(env)?;
    if settings.login().is_none() {
        return Err(Failure::NoLogin);
    }
    if store::auth_refused_marked(env) {
        return Err(Failure::AuthRefused("refus mémorisé".into()));
    }
    Ok(settings)
}

/// Note ou efface le refus mémorisé selon le résultat d'un appel.
pub fn record<T>(env: &Env, r: &Result<T, Failure>) {
    match r {
        Ok(_) => store::clear_auth_refused(env),
        Err(Failure::AuthRefused(_)) => store::mark_auth_refused(env),
        Err(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entree_de_connexion() {
        for f in [Failure::NoLogin, Failure::AuthRefused("x".into())] {
            let e = f.launcher_entry();
            assert_eq!(e["id"], "login");
            assert_eq!(e["name"], "Se connecter à Navidrome");
            assert_eq!(e["subtitle"], "Connexion requise");
            assert_eq!(e["icon"], "󰍂");
            assert_eq!(e["action"]["type"], "command");
            assert_eq!(
                e["action"]["command"],
                "xdg-terminal-exec poste music login"
            );
        }
    }

    #[test]
    fn entree_copie_en_cours() {
        let e = Failure::Syncing.launcher_entry();
        assert_eq!(e["id"], "syncing");
        assert_eq!(e["name"], "Copie de la bibliothèque en cours…");
        assert_eq!(e["icon"], "󰑓");
        assert!(e.get("action").is_none());
        assert!(!Failure::Syncing.to_string().contains("poste music login"));
    }

    #[test]
    fn entree_injoignable_sans_reconnexion() {
        let f = Failure::Unreachable {
            url: "http://h:4533".into(),
            detail: "délai".into(),
        };
        let e = f.launcher_entry();
        assert_eq!(e["id"], "unreachable");
        assert_eq!(e["name"], "Navidrome injoignable");
        assert_eq!(e["subtitle"], "http://h:4533");
        assert_eq!(e["icon"], "󰅛");
        assert!(e.get("action").is_none());
        assert!(!f.to_string().contains("poste music login"));
    }

    #[test]
    fn classement_des_erreurs_api() {
        assert!(matches!(
            from_api(ApiError::AuthRefused("x".into()), "u"),
            Failure::AuthRefused(_)
        ));
        assert!(matches!(
            from_api(ApiError::Unreachable("x".into()), "u"),
            Failure::Unreachable { .. }
        ));
    }
}
