//! Un morceau tel que le lecteur le connaît, et le texte de sa qualité.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Ce qui décrit un morceau dans la file. Aucun secret : l'adresse du flux est
/// construite par le démon au dernier moment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TrackInfo {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    /// Durée annoncée par Navidrome, en secondes.
    pub duration: u64,
    pub suffix: String,
    pub bit_rate: u64,
    pub bit_depth: u64,
    pub sampling_rate: u64,
    /// Identifiant de pochette (`coverArt`).
    pub cover: Option<String>,
    /// Morceau aimé (favori).
    #[serde(default)]
    pub starred: bool,
}

fn text(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn num(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(Value::as_u64).unwrap_or(0)
}

impl TrackInfo {
    /// Morceau d'après un objet `song`/`child` de l'API. `None` sans identifiant.
    pub fn from_api(v: &Value) -> Option<Self> {
        let id = match v.get("id")? {
            Value::String(s) if !s.is_empty() => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => return None,
        };
        let artist = text(v, "artist");
        // OpenSubsonic : `displayAlbumArtist` ; à défaut, l'artiste du morceau.
        let mut album_artist = text(v, "displayAlbumArtist");
        if album_artist.is_empty() {
            album_artist = artist.clone();
        }
        let cover = match v.get("coverArt") {
            Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
            Some(Value::Number(n)) => Some(n.to_string()),
            _ => None,
        };
        Some(Self {
            id,
            title: text(v, "title"),
            artist,
            album: text(v, "album"),
            album_artist,
            duration: num(v, "duration"),
            suffix: text(v, "suffix"),
            bit_rate: num(v, "bitRate"),
            bit_depth: num(v, "bitDepth"),
            sampling_rate: num(v, "samplingRate"),
            cover,
            starred: v.get("starred").is_some_and(|s| !s.is_null()),
        })
    }

    pub fn quality(&self) -> String {
        quality(
            &self.suffix,
            self.bit_depth,
            self.sampling_rate,
            self.bit_rate,
        )
    }
}

/// Fréquence en kHz sans zéro inutile : 96000 donne « 96 », 44100 donne « 44.1 ».
fn khz(hz: u64) -> String {
    let tenths = (hz + 50) / 100;
    if tenths.is_multiple_of(10) {
        (tenths / 10).to_string()
    } else {
        format!("{}.{}", tenths / 10, tenths % 10)
    }
}

/// Texte de qualité : « FLAC · 24 bit / 96 kHz · 2304 kbps », « MP3 · 320 kbps ».
/// Les parties inconnues (0 ou vides) sont omises ; texte vide si tout manque.
pub fn quality(suffix: &str, bit_depth: u64, sampling_rate: u64, bit_rate: u64) -> String {
    let mut parts = Vec::new();
    if !suffix.is_empty() {
        parts.push(suffix.to_uppercase());
    }
    match (bit_depth, sampling_rate) {
        (0, 0) => {}
        (d, 0) => parts.push(format!("{d} bit")),
        (0, r) if suffix_is_lossless(suffix) => parts.push(format!("{} kHz", khz(r))),
        (0, _) => {}
        (d, r) => parts.push(format!("{d} bit / {} kHz", khz(r))),
    }
    if bit_rate > 0 {
        parts.push(format!("{bit_rate} kbps"));
    }
    parts.join(" · ")
}

fn suffix_is_lossless(suffix: &str) -> bool {
    matches!(
        suffix.to_ascii_lowercase().as_str(),
        "flac" | "wav" | "alac" | "aiff" | "aif" | "ape" | "wv" | "dsf"
    )
}

/// `3:05` ou `1:02:03`.
pub fn clock(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn qualite_sans_perte_et_avec_perte() {
        assert_eq!(
            quality("flac", 24, 96000, 2304),
            "FLAC · 24 bit / 96 kHz · 2304 kbps"
        );
        assert_eq!(
            quality("flac", 16, 44100, 900),
            "FLAC · 16 bit / 44.1 kHz · 900 kbps"
        );
        assert_eq!(quality("mp3", 0, 44100, 320), "MP3 · 320 kbps");
        assert_eq!(quality("opus", 0, 48000, 128), "OPUS · 128 kbps");
        assert_eq!(quality("mp3", 0, 0, 0), "MP3");
        assert_eq!(quality("", 0, 0, 0), "");
        assert_eq!(quality("wav", 0, 48000, 0), "WAV · 48 kHz");
        assert_eq!(quality("flac", 24, 0, 0), "FLAC · 24 bit");
    }

    #[test]
    fn morceau_depuis_l_api() {
        let v = json!({"id":"s1","title":"T","artist":"A","album":"B","duration":200,
            "suffix":"flac","bitRate":900,"bitDepth":16,"samplingRate":44100,"coverArt":"mf-s1","starred":"2026-01-01T00:00:00Z"});
        let t = TrackInfo::from_api(&v).unwrap();
        assert!(t.starred);
        assert_eq!(t.album_artist, "A");
        assert_eq!(t.cover.as_deref(), Some("mf-s1"));
        assert_eq!(t.quality(), "FLAC · 16 bit / 44.1 kHz · 900 kbps");
        let v = json!({"id":"s2","title":"T","artist":"A","displayAlbumArtist":"Various"});
        assert_eq!(TrackInfo::from_api(&v).unwrap().album_artist, "Various");
        assert!(TrackInfo::from_api(&json!({"title":"x"})).is_none());
        assert!(!TrackInfo::from_api(&json!({"id":"s3"})).unwrap().starred);
    }

    #[test]
    fn horloge() {
        assert_eq!(clock(0.0), "0:00");
        assert_eq!(clock(185.7), "3:05");
        assert_eq!(clock(3723.0), "1:02:03");
    }
}
