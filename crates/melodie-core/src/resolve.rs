//! De « kind + id » à la liste des morceaux à jouer, d'après l'API : l'ordre
//! des disques et des pistes vient du serveur, les métadonnées sont à jour.

use crate::api::{ApiError, Client};
use crate::env::Env;
use crate::search::Kind;
use crate::state::{self, Failure};
use crate::track::TrackInfo;
use crate::{db, library, mix};
use serde_json::Value;

/// `what` is told to the user when the server does not know the id ("album al9").
fn call(client: &Client, method: &str, id: &str, what: &str) -> Result<Value, Failure> {
    client
        .call(method, &[("id", id.to_string())])
        .map_err(|e| match e {
            ApiError::NotFound(_) => Failure::NotFound(what.to_string()),
            e => state::from_api(e, client.url()),
        })
}

fn tracks_of(list: &Value) -> Vec<TrackInfo> {
    list.as_array()
        .map(|a| a.iter().filter_map(TrackInfo::from_api).collect())
        .unwrap_or_default()
}

/// Les albums d'un artiste : année croissante, puis nom (sans casse).
pub fn album_order(albums: &[Value]) -> Vec<String> {
    let mut v: Vec<(i64, String, String)> = albums
        .iter()
        .filter_map(|a| {
            let id = a["id"].as_str()?.to_string();
            Some((
                a["year"].as_i64().unwrap_or(0),
                a["name"].as_str().unwrap_or("").to_lowercase(),
                id,
            ))
        })
        .collect();
    v.sort();
    v.into_iter().map(|(_, _, id)| id).collect()
}

/// Morceaux d'une playlist, d'un album, d'un artiste (tous ses albums dans
/// l'ordre) ou d'un morceau seul. An artist without albums of their own (only featured on the
/// tracks of others) gives the tracks the local copy credits them on: the server lists no
/// such tracks. Une liste vide est une erreur, which names what was asked by its name.
pub fn resolve(
    client: &Client,
    env: &Env,
    kind: Kind,
    id: &str,
) -> Result<Vec<TrackInfo>, Failure> {
    let what = format!("{} {id}", kind.word());
    let name = |v: &Value, key: &str| v[key].as_str().map(str::to_string);
    let (named, tracks) = match kind {
        Kind::Track => {
            let r = call(client, "getSong", id, &what)?;
            let t: Vec<_> = TrackInfo::from_api(&r["song"]).into_iter().collect();
            (name(&r["song"], "title"), t)
        }
        Kind::Album => {
            let r = call(client, "getAlbum", id, &what)?;
            (name(&r["album"], "name"), tracks_of(&r["album"]["song"]))
        }
        Kind::Playlist => {
            let r = call(client, "getPlaylist", id, &what)?;
            (
                name(&r["playlist"], "name"),
                tracks_of(&r["playlist"]["entry"]),
            )
        }
        Kind::Artist => {
            let r = call(client, "getArtist", id, &what)?;
            let albums = r["artist"]["album"].as_array().cloned().unwrap_or_default();
            let mut out = Vec::new();
            for album in album_order(&albums) {
                out.extend(tracks_of(
                    &call(client, "getAlbum", &album, &what)?["album"]["song"],
                ));
            }
            if albums.is_empty() {
                out = appearances(env, id)?;
            }
            (name(&r["artist"], "name"), out)
        }
    };
    if tracks.is_empty() {
        return Err(Failure::Internal(anyhow::anyhow!(
            "Nothing to play for {}",
            named.unwrap_or_else(|| format!("this {}", kind.word()))
        )));
    }
    Ok(tracks)
}

/// The tracks the local copy credits an artist on outside their albums; none without a copy.
fn appearances(env: &Env, id: &str) -> Result<Vec<TrackInfo>, Failure> {
    let Some(conn) = db::open_existing(env)? else {
        return Ok(Vec::new());
    };
    let ids: Vec<String> = library::artist_appearances(&conn, id)?
        .into_iter()
        .map(|t| t.id)
        .collect();
    Ok(mix::infos(&conn, &ids)?.into_iter().flatten().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Creds;
    use crate::fake::Fake;
    use serde_json::json;

    fn song_in(id: &str, album: &str) -> Value {
        let mut s = crate::fake::song(1);
        s["id"] = json!(id);
        s["albumId"] = json!(album);
        s
    }

    #[test]
    fn ordre_des_albums() {
        let albums = vec![
            json!({"id":"c","name":"b","year":2001}),
            json!({"id":"a","name":"Z","year":1997}),
            json!({"id":"b","name":"a","year":2001}),
            json!({"id":"n","name":"sans année"}),
        ];
        assert_eq!(album_order(&albums), ["n", "a", "b", "c"]);
    }

    #[test]
    fn kinds_resolus_par_l_api() {
        let fake = Fake::start(0, "tok");
        *fake.state.songs.lock().unwrap() = vec![
            song_in("s0", "al1"),
            song_in("s1", "al1"),
            song_in("h0", "al2"),
        ];
        let creds = Creds {
            u: "alice".into(),
            t: "tok".into(),
            s: "sel".into(),
        };
        let client = Client::new(&fake.url, creds);
        let d = tempfile::tempdir().unwrap();
        let env = Env::with_home(d.path());
        let ids = |kind, id| {
            resolve(&client, &env, kind, id)
                .unwrap()
                .into_iter()
                .map(|t| t.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(Kind::Track, "s1"), ["s1"]);
        assert_eq!(ids(Kind::Album, "al1"), ["s0", "s1"]);
        assert_eq!(ids(Kind::Playlist, "p1"), ["s0", "s1", "h0"]);
        // Homework (1997) avant Discovery (2001).
        assert_eq!(ids(Kind::Artist, "ar1"), ["h0", "s0", "s1"]);
    }

    fn client_of(fake: &Fake) -> Client {
        let creds = Creds {
            u: "alice".into(),
            t: "tok".into(),
            s: "sel".into(),
        };
        Client::new(&fake.url, creds)
    }

    /// `ar9` featured on `s1` and `s2`, `ar8` credited on nothing: neither has an album.
    fn featuring(fake: &Fake) {
        *fake.state.featured.lock().unwrap() = vec![
            json!({"id":"ar9","name":"Lior Castell","albumCount":0}),
            json!({"id":"ar8","name":"Nova Reyes","albumCount":0}),
        ];
        let mut songs = fake.state.songs.lock().unwrap();
        for s in songs.iter_mut().skip(1).take(2) {
            s["artists"] = json!([{"id": s["artistId"], "name": s["artist"]},
                                  {"id": "ar9", "name": "Lior Castell"}]);
        }
    }

    fn message(r: Result<Vec<TrackInfo>, Failure>) -> String {
        match r {
            Err(f) => f.to_string(),
            Ok(t) => panic!("{} tracks", t.len()),
        }
    }

    #[test]
    fn an_artist_without_albums_plays_the_tracks_they_appear_on() {
        let fake = Fake::start(4, "tok");
        featuring(&fake);
        let client = client_of(&fake);
        let d = tempfile::tempdir().unwrap();
        let env = Env::with_home(d.path());
        crate::sync::run(&env, &client, false).unwrap();
        let ids: Vec<String> = resolve(&client, &env, Kind::Artist, "ar9")
            .unwrap()
            .into_iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(ids, ["s1", "s2"]);
        let said = message(resolve(&client, &env, Kind::Artist, "ar8"));
        assert_eq!(said, "Nothing to play for Nova Reyes");
    }

    #[test]
    fn a_server_that_credits_no_one_leaves_an_artist_without_albums_nothing_to_play() {
        let fake = Fake::start(4, "tok");
        featuring(&fake);
        for s in fake.state.songs.lock().unwrap().iter_mut() {
            s.as_object_mut().unwrap().remove("artists");
        }
        let client = client_of(&fake);
        let d = tempfile::tempdir().unwrap();
        let env = Env::with_home(d.path());
        // with a copy, and without one
        let said = message(resolve(&client, &env, Kind::Artist, "ar9"));
        assert_eq!(said, "Nothing to play for Lior Castell");
        crate::sync::run(&env, &client, false).unwrap();
        let said = message(resolve(&client, &env, Kind::Artist, "ar9"));
        assert_eq!(said, "Nothing to play for Lior Castell");
    }

    #[test]
    fn un_identifiant_inconnu_est_introuvable_et_non_injoignable() {
        let fake = Fake::start(0, "tok");
        let creds = Creds {
            u: "alice".into(),
            t: "tok".into(),
            s: "sel".into(),
        };
        let client = Client::new(&fake.url, creds);
        for (kind, id, what) in [
            (Kind::Album, "inconnu", "album inconnu"),
            (Kind::Track, "zzz", "track zzz"),
        ] {
            match resolve(&client, &Env::with_home("/nonexistent"), kind, id) {
                Err(Failure::NotFound(w)) => assert_eq!(w, what),
                other => panic!("{kind:?} {id}: {other:?}"),
            }
        }
    }
}
