//! The address of something to play: `melodie:<kind>/<id>[?<options>]`.
//!
//! It comes from outside (another program, `xdg-open`, a D-Bus call), so it is read strictly:
//! anything not in the grammar is refused with a message that says why, and nothing is guessed.
//!
//! ```text
//! melodie:album/4Zq0Aw-9
//! melodie:playlist/p1?shuffle=true
//! melodie:artist/ar1?queue=next
//! ```
//!
//! `kind` is `track`, `album`, `artist` or `playlist`. The options:
//!
//! - `shuffle=true|false`: shuffles what is added (default `false`).
//! - `queue=next|last`: adds to the queue, right after the current track or at its end, instead
//!   of replacing it (default: replaces it and starts playing).

use crate::search::Kind;
use std::fmt;

/// The scheme, as MPRIS' `SupportedUriSchemes` lists it.
pub const SCHEME: &str = "melodie";
/// Longest id accepted; the servers' ids are far shorter.
const MAX_ID: usize = 128;

/// Where the tracks go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// Replaces the queue and plays.
    Replace,
    /// Right after the current track.
    Next,
    /// At the end of the queue.
    Last,
}

/// What an address asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub kind: Kind,
    pub id: String,
    pub shuffle: bool,
    pub place: Place,
}

/// Why an address was refused. `Display` names the part that is wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressError(String);

impl fmt::Display for AddressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid address: {}", self.0)
    }
}

impl std::error::Error for AddressError {}

fn refuse<T>(why: impl Into<String>) -> Result<T, AddressError> {
    Err(AddressError(why.into()))
}

/// The characters the servers' ids are made of (Navidrome, Gonic, Nextcloud Music, Ampache…).
fn id_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"-_.".contains(&b)
}

/// Undoes `%XX` once. What it returns is only ever checked byte by byte, never trusted.
fn decode(s: &str, what: &str) -> Result<Vec<u8>, AddressError> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'%' {
            out.push(b[i]);
            i += 1;
            continue;
        }
        let hex = b
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok())
            .filter(|_| b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit());
        let Some(byte) = hex else {
            return refuse(format!("bad percent-encoding in the {what}"));
        };
        out.push(byte);
        i += 3;
    }
    Ok(out)
}

fn decode_id(raw: &str) -> Result<String, AddressError> {
    let bytes = decode(raw, "id")?;
    if bytes.is_empty() {
        return refuse("the id is empty");
    }
    if bytes.len() > MAX_ID {
        return refuse(format!("the id is longer than {MAX_ID} characters"));
    }
    if !bytes.iter().copied().all(id_byte) {
        return refuse("the id has a character no server id has (letters, digits, - _ . only)");
    }
    // Only ASCII bytes got through.
    Ok(bytes.into_iter().map(char::from).collect())
}

fn decode_option(raw: &str, key: &str) -> Result<String, AddressError> {
    let bytes = decode(raw, &format!("option {key}"))?;
    String::from_utf8(bytes).or_else(|_| refuse(format!("the value of {key} is not text")))
}

fn kind_of(word: &str) -> Result<Kind, AddressError> {
    match word {
        "track" => Ok(Kind::Track),
        "album" => Ok(Kind::Album),
        "artist" => Ok(Kind::Artist),
        "playlist" => Ok(Kind::Playlist),
        "" => refuse("the kind is missing (track, album, artist or playlist)"),
        other => refuse(format!(
            "unknown kind {other:?} (track, album, artist or playlist)"
        )),
    }
}

fn boolean(key: &str, value: &str) -> Result<bool, AddressError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => refuse(format!("{key} must be true or false, not {value:?}")),
    }
}

impl Address {
    pub fn parse(uri: &str) -> Result<Address, AddressError> {
        let Some((scheme, rest)) = uri.split_once(':') else {
            return refuse(format!("not a {SCHEME}: address"));
        };
        if !scheme.eq_ignore_ascii_case(SCHEME) {
            return refuse(format!("not a {SCHEME}: address"));
        }
        if rest.contains('#') {
            return refuse("a fragment (#…) has no meaning here");
        }
        let (path, query) = match rest.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (rest, None),
        };
        let Some((kind, id)) = path.split_once('/') else {
            return refuse("expected <kind>/<id>");
        };
        let mut a = Address {
            kind: kind_of(kind)?,
            id: decode_id(id)?,
            shuffle: false,
            place: Place::Replace,
        };
        let (mut shuffle_seen, mut queue_seen) = (false, false);
        for pair in query.into_iter().flat_map(|q| q.split('&')) {
            let Some((key, value)) = pair.split_once('=') else {
                return refuse(format!("option {pair:?} has no value"));
            };
            let value = decode_option(value, key)?;
            match key {
                "shuffle" if !shuffle_seen => {
                    shuffle_seen = true;
                    a.shuffle = boolean(key, &value)?;
                }
                "queue" if !queue_seen => {
                    queue_seen = true;
                    a.place = match value.as_str() {
                        "next" => Place::Next,
                        "last" => Place::Last,
                        _ => return refuse(format!("queue must be next or last, not {value:?}")),
                    };
                }
                "shuffle" | "queue" => return refuse(format!("option {key} given twice")),
                _ => return refuse(format!("unknown option {key:?} (shuffle, queue)")),
            }
        }
        Ok(a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(uri: &str) -> String {
        Address::parse(uri)
            .expect_err(&format!("{uri:?} should be refused"))
            .to_string()
    }

    #[test]
    fn each_kind_is_read() {
        for (word, kind) in [
            ("track", Kind::Track),
            ("album", Kind::Album),
            ("artist", Kind::Artist),
            ("playlist", Kind::Playlist),
        ] {
            let a = Address::parse(&format!("melodie:{word}/Ab-9_x.1")).unwrap();
            assert_eq!(a.kind, kind);
            assert_eq!(a.id, "Ab-9_x.1");
            assert!(!a.shuffle);
            assert_eq!(a.place, Place::Replace);
        }
    }

    #[test]
    fn options_are_read() {
        let a = Address::parse("melodie:album/al1?shuffle=true&queue=next").unwrap();
        assert!(a.shuffle);
        assert_eq!(a.place, Place::Next);
        let a = Address::parse("melodie:album/al1?queue=last&shuffle=false").unwrap();
        assert!(!a.shuffle);
        assert_eq!(a.place, Place::Last);
    }

    #[test]
    fn the_scheme_is_not_case_sensitive_but_the_rest_is() {
        assert!(Address::parse("MELODIE:album/al1").is_ok());
        assert!(refused("melodie:Album/al1").contains("unknown kind"));
    }

    #[test]
    fn the_id_is_decoded_once() {
        assert_eq!(Address::parse("melodie:album/al%2D1").unwrap().id, "al-1");
        // `%2541` is the text `%41`, not `A`: decoding twice would make it a valid id.
        assert!(refused("melodie:album/%2541").contains("character"));
    }

    #[test]
    fn a_malformed_address_is_refused_with_the_reason() {
        for (uri, why) in [
            ("", "not a melodie: address"),
            ("album/al1", "not a melodie: address"),
            ("http://example.org/album/al1", "not a melodie: address"),
            ("melodie:", "expected <kind>/<id>"),
            ("melodie:album", "expected <kind>/<id>"),
            ("melodie:/al1", "kind is missing"),
            ("melodie:radio/r1", "unknown kind"),
            ("melodie:album/", "id is empty"),
            ("melodie:album/%", "percent-encoding"),
            ("melodie:album/%zz", "percent-encoding"),
            ("melodie:album/%4", "percent-encoding"),
            ("melodie:album/%+1", "percent-encoding"),
            ("melodie:album/a/b", "character"),
            ("melodie:album/..%2F..", "character"),
            ("melodie:album/a%00b", "character"),
            ("melodie:album/a%20b", "character"),
            ("melodie:album/a b", "character"),
            ("melodie:album/al1\n", "character"),
            ("melodie:album/é", "character"),
            ("melodie:album/al1#x", "fragment"),
            ("melodie:album/al1?", "has no value"),
            ("melodie:album/al1?shuffle", "has no value"),
            ("melodie:album/al1?shuffle=", "true or false"),
            ("melodie:album/al1?shuffle=1", "true or false"),
            ("melodie:album/al1?queue=now", "next or last"),
            ("melodie:album/al1?queue=", "next or last"),
            ("melodie:album/al1?shuffle=true&shuffle=true", "twice"),
            ("melodie:album/al1?queue=next&queue=last", "twice"),
            ("melodie:album/al1?shufle=true", "unknown option"),
            ("melodie:album/al1?shuffle=true&", "has no value"),
            ("melodie:album/al1?queue=%zz", "percent-encoding"),
            ("melodie:album/al1?queue=%FF", "not text"),
        ] {
            let msg = refused(uri);
            assert!(msg.contains(why), "{uri:?}: {msg}");
        }
        assert!(refused(&format!("melodie:album/{}", "a".repeat(129))).contains("longer"));
        assert!(Address::parse(&format!("melodie:album/{}", "a".repeat(128))).is_ok());
    }
}
