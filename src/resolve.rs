//! De « kind + id » à la liste des morceaux à jouer, d'après l'API : l'ordre
//! des disques et des pistes vient du serveur, les métadonnées sont à jour.

use crate::api::Client;
use crate::search::Kind;
use crate::state::{self, Failure};
use crate::track::TrackInfo;
use serde_json::Value;

fn call(client: &Client, method: &str, id: &str) -> Result<Value, Failure> {
    client
        .call(method, &[("id", id.to_string())])
        .map_err(|e| state::from_api(e, client.url()))
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
/// l'ordre) ou d'un morceau seul. Une liste vide est une erreur.
pub fn resolve(client: &Client, kind: Kind, id: &str) -> Result<Vec<TrackInfo>, Failure> {
    let tracks = match kind {
        Kind::Track => {
            let r = call(client, "getSong", id)?;
            TrackInfo::from_api(&r["song"]).into_iter().collect()
        }
        Kind::Album => tracks_of(&call(client, "getAlbum", id)?["album"]["song"]),
        Kind::Playlist => tracks_of(&call(client, "getPlaylist", id)?["playlist"]["entry"]),
        Kind::Artist => {
            let r = call(client, "getArtist", id)?;
            let albums = r["artist"]["album"].as_array().cloned().unwrap_or_default();
            let mut out = Vec::new();
            for album in album_order(&albums) {
                out.extend(tracks_of(
                    &call(client, "getAlbum", &album)?["album"]["song"],
                ));
            }
            out
        }
    };
    if tracks.is_empty() {
        return Err(Failure::Internal(anyhow::anyhow!(
            "rien à lire dans {} {id}",
            kind.word()
        )));
    }
    Ok(tracks)
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
            u: "kris".into(),
            t: "tok".into(),
            s: "sel".into(),
        };
        let client = Client::new(&fake.url, creds);
        let ids = |kind, id| {
            resolve(&client, kind, id)
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
        assert!(matches!(
            resolve(&client, Kind::Album, "inconnu"),
            Err(Failure::Internal(_))
        ));
        assert!(resolve(&client, Kind::Track, "zzz").is_err());
    }
}
