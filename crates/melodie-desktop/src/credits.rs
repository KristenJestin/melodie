//! The artists credited on tracks known only by their id (the queue, the search), read from the
//! local copy away from the window: a queue entry or a search hit has only their names, joined.

use melodie_core::db;
use melodie_core::env::Env;
use melodie_core::library::{self, Credit};
use std::collections::HashMap;

/// The artists of each track, by its id.
pub type Credits = HashMap<String, Vec<Credit>>;

/// The artists of the tracks `ids`. Does file I/O: never call it while drawing. Empty without a
/// copy; a track the copy does not hold is left out.
pub fn read(env: &Env, ids: impl IntoIterator<Item = String>) -> Credits {
    let mut ids: Vec<String> = ids.into_iter().collect();
    ids.sort();
    ids.dedup();
    let Some(conn) = db::open_existing(env).ok().flatten() else {
        return Credits::new();
    };
    library::credits(&conn, &ids).unwrap_or_default()
}

/// The artists of the track `id`: as the copy credits them, else its own `artist`, linked when
/// its id is known.
pub fn of(credits: &Credits, id: &str, artist: &str, artist_id: Option<&str>) -> Vec<Credit> {
    if let Some(found) = credits.get(id) {
        return found.clone();
    }
    if artist.is_empty() && artist_id.is_none() {
        return Vec::new();
    }
    vec![Credit {
        id: artist_id.map(str::to_string),
        name: artist.to_string(),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_artists_of_tracks_known_by_id_come_from_the_copy_else_from_their_own_name() {
        let d = tempfile::tempdir().unwrap();
        let env = Env::with_home(d.path());
        assert!(read(&env, ["t1".to_string()]).is_empty(), "no copy");
        let mut conn = db::create(&db::db_path(&env)).unwrap();
        let tx = conn.transaction().unwrap();
        for (id, name) in [
            ("ar5", "Nova Reyes"),
            ("ar1", "Lior Castell"),
            ("ar3", "Ada Marlow"),
        ] {
            db::insert_artist(&tx, &json!({"id": id, "name": name})).unwrap();
        }
        db::insert_track(
            &tx,
            &json!({"id":"t1","title":"Crossing","artist":"Nova Reyes, Lior Castell, Ada Marlow",
                    "artistId":"ar5","artists":[{"id":"ar5","name":"Nova Reyes"},
                    {"id":"ar1","name":"Lior Castell"},{"id":"ar3","name":"Ada Marlow"}]}),
        )
        .unwrap();
        tx.commit().unwrap();
        drop(conn);

        let found = read(&env, ["t1", "gone", "t1"].map(String::from));
        let names: Vec<String> = of(&found, "t1", "ignored", None)
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, ["Nova Reyes", "Lior Castell", "Ada Marlow"]);
        // not in the copy: the name it came with, one artist, linked only with an id
        let own = |id: Option<&str>| of(&found, "gone", "Ilse Varga", id);
        assert_eq!(
            own(None),
            [Credit {
                id: None,
                name: "Ilse Varga".into()
            }]
        );
        assert_eq!(own(Some("ar2"))[0].id.as_deref(), Some("ar2"));
        assert!(of(&found, "gone", "", None).is_empty());
    }
}
