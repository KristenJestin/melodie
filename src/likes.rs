//! Favoris : `like`, `unlike`, `like --toggle`. L'écriture se fait sur le
//! serveur (`star.view`, `unstar.view`), puis dans la copie locale, sans
//! synchronisation complète.

use crate::api::Client;
use crate::ctl::{self, Oops};
use crate::db::{self, StarKind};
use crate::notify;
use crate::state::{self, Failure};
use poste_core::time::{now_secs, rfc3339};
use poste_core::Env;
use rusqlite::Connection;
use std::process::ExitCode;

/// Ce qu'on aime : le morceau en cours, ou un élément précis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Current,
    Item(StarKind, String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Like,
    Unlike,
    Toggle,
}

/// `star.view` / `unstar.view` puis mise à jour de la copie (si elle existe).
/// Renvoie `true` si la copie connaissait l'élément.
pub fn apply(
    client: &Client,
    conn: Option<&Connection>,
    kind: StarKind,
    id: &str,
    on: bool,
    at: u64,
) -> Result<bool, Failure> {
    client
        .call(
            if on { "star" } else { "unstar" },
            &[(kind.api_param(), id.to_string())],
        )
        .map_err(|e| state::from_api(e, client.url()))?;
    let Some(conn) = conn else { return Ok(false) };
    let stamp = rfc3339(at);
    Ok(db::set_starred(
        conn,
        kind,
        id,
        on.then_some(stamp.as_str()),
    )?)
}

/// Texte de la notification : (titre, corps).
pub fn message(on: bool, title: &str) -> (String, String) {
    if on {
        (format!("♥ Ajouté aux favoris : {title}"), String::new())
    } else {
        ("Retiré des favoris".to_string(), title.to_string())
    }
}

pub fn run(env: &Env, target: Target, mode: Mode) -> ExitCode {
    match try_run(env, target, mode) {
        Ok(()) => ExitCode::SUCCESS,
        Err(o) => ctl::report_oops(&o),
    }
}

fn try_run(env: &Env, target: Target, mode: Mode) -> Result<(), Oops> {
    // Le morceau en cours d'abord : rien à faire (ni réseau) si rien ne joue.
    let current = match &target {
        Target::Current => Some(ctl::current_track()?),
        Target::Item(..) => None,
    };
    state::quick_check(env)?;
    let client = state::open_client(env)?;
    let conn = db::open_existing(env)?;
    if let Some(c) = &conn {
        c.busy_timeout(std::time::Duration::from_secs(2))
            .map_err(Failure::from)?;
    }
    let (kind, id, title, was) = match (&target, &current) {
        (Target::Current, Some(n)) => (StarKind::Track, n.id.clone(), n.title.clone(), n.starred),
        (Target::Item(k, id), _) => {
            let title = conn
                .as_ref()
                .and_then(|c| db::name_of(c, *k, id))
                .unwrap_or_else(|| id.clone());
            let was = conn
                .as_ref()
                .and_then(|c| db::is_starred(c, *k, id))
                .unwrap_or(false);
            (*k, id.clone(), title, was)
        }
        (Target::Current, None) => return Err(Oops::NothingPlaying),
    };
    let on = match mode {
        Mode::Like => true,
        Mode::Unlike => false,
        Mode::Toggle => !was,
    };
    let r = apply(&client, conn.as_ref(), kind, &id, on, now_secs());
    state::record(env, &r);
    r?;
    if kind == StarKind::Track {
        ctl::tell_starred(&id, on);
    }
    let (summary, body) = message(on, &title);
    println!(
        "{}",
        if body.is_empty() {
            summary.clone()
        } else {
            format!("{summary} : {body}")
        }
    );
    notify::send(&summary, &body);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{token, Creds};
    use crate::fake::Fake;
    use crate::sync;

    fn setup() -> (tempfile::TempDir, Env, Fake, Client) {
        let d = tempfile::tempdir().unwrap();
        let env = Env::with_home(d.path());
        let creds = Creds {
            u: "kris".into(),
            t: token("sesame", "c19b2d"),
            s: "c19b2d".into(),
        };
        let fake = Fake::start(4, &creds.t);
        let client = Client::new(&fake.url, creds);
        sync::run(&env, &client, true).unwrap();
        (d, env, fake, client)
    }

    #[test]
    fn requetes_et_copie_locale() {
        let (_d, env, fake, client) = setup();
        let conn = db::open_existing(&env).unwrap().unwrap();
        fake.clear_calls();
        for (kind, id, param) in [
            (StarKind::Track, "s0", "id=s0"),
            (StarKind::Album, "al1", "albumId=al1"),
            (StarKind::Artist, "ar1", "artistId=ar1"),
        ] {
            assert_eq!(db::is_starred(&conn, kind, id), Some(false));
            assert!(apply(&client, Some(&conn), kind, id, true, 1_790_000_000).unwrap());
            assert_eq!(db::is_starred(&conn, kind, id), Some(true));
            let calls = fake.state.calls.lock().unwrap().clone();
            let last = calls.last().unwrap();
            assert!(
                last.starts_with("star?")
                    && (last.contains(&format!("&{param}")) || last.contains(&format!("?{param}"))),
                "{last}"
            );
            apply(&client, Some(&conn), kind, id, false, 1_790_000_000).unwrap();
            assert_eq!(db::is_starred(&conn, kind, id), Some(false));
            let calls = fake.state.calls.lock().unwrap().clone();
            assert!(calls.last().unwrap().starts_with("unstar?"));
        }
        // Sans copie : le serveur est quand même appelé.
        assert!(!apply(&client, None, StarKind::Track, "s0", true, 0).unwrap());
        // Élément inconnu de la copie.
        assert!(!apply(&client, Some(&conn), StarKind::Track, "zzz", true, 0).unwrap());
        assert_eq!(
            db::name_of(&conn, StarKind::Track, "s1").as_deref(),
            Some("Titre numéro 1")
        );
    }

    #[test]
    fn echec_d_authentification_ne_touche_pas_la_copie() {
        let (_d, env, fake, client) = setup();
        let conn = db::open_existing(&env).unwrap().unwrap();
        *fake.state.expected_token.lock().unwrap() = "autre".into();
        let r = apply(&client, Some(&conn), StarKind::Track, "s0", true, 0);
        assert!(matches!(r, Err(Failure::AuthRefused(_))));
        assert_eq!(db::is_starred(&conn, StarKind::Track, "s0"), Some(false));
        *fake.state.raw.lock().unwrap() = Some((503, "x".into()));
        let r = apply(&client, Some(&conn), StarKind::Track, "s0", true, 0);
        assert!(matches!(r, Err(Failure::Unreachable { .. })));
    }

    #[test]
    fn textes_de_notification() {
        assert_eq!(
            message(true, "Halo"),
            ("♥ Ajouté aux favoris : Halo".into(), String::new())
        );
        assert_eq!(message(false, "Halo").0, "Retiré des favoris");
    }
}
