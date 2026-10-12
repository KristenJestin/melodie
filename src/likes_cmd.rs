//! Commandes `like`, `unlike` et `like --toggle` : le côté ligne de commande de `likes`.

use crate::ctl::{self, Oops};
use crate::db::{self, StarKind};
use crate::env::Env;
use crate::likes::{Mode, Target, apply, message};
use crate::notify;
use crate::state::{self, Failure};
use crate::time::now_secs;
use std::process::ExitCode;

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
