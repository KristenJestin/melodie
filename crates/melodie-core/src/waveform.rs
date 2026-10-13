//! The waveform of a track: its loudness along the track, a few hundred levels kept in
//! `~/.cache/melodie/waveforms/<id>.wave`, beside the covers. Navidrome gives none: the levels
//! are computed from the audio by `melodie waveform-fetch`, once per track. The window draws its
//! progress bar from them.

use crate::cover;
use crate::db;
use crate::env::Env;
use crate::store::write_atomic;
use anyhow::Result;
use std::path::PathBuf;

/// The levels of a track, whatever its length: enough for a bar a few hundred pixels wide.
pub const LEVELS: usize = 400;
/// The rate the audio is decoded at: mono, low enough to keep the decoded file small, high
/// enough for the loudness of the voice and the instruments.
pub const RATE: u32 = 8000;
/// The first bytes of a waveform file: a file of another shape is computed again.
const MAGIC: &[u8; 4] = b"MWV1";

/// Where the waveform of the track `id` is kept.
pub fn path(env: &Env, id: &str) -> PathBuf {
    dir(env).join(PathBuf::from(cover::file_name(id)).with_extension("wave"))
}

/// The folder of the waveforms (and of the lock of `waveform-fetch`).
pub fn dir(env: &Env) -> PathBuf {
    db::cache_dir(env).join("waveforms")
}

/// The levels kept for the track `id`, if they are there and whole.
pub fn read(env: &Env, id: &str) -> Option<Vec<u8>> {
    let bytes = std::fs::read(path(env, id)).ok()?;
    let levels = bytes.strip_prefix(MAGIC)?;
    (levels.len() == LEVELS).then(|| levels.to_vec())
}

/// Keeps the levels of the track `id`.
pub fn write(env: &Env, id: &str, levels: &[u8]) -> Result<()> {
    write_atomic(&path(env, id), &[MAGIC.as_slice(), levels].concat())
}

/// [`LEVELS`] levels from decoded audio (mono, signed 16 bits, little-endian): the loudness (root
/// mean square) of each stretch of the track, from 0 to 255, the loudest stretch at 255. A silent
/// track gives only zeros.
pub fn levels(pcm: &[u8]) -> Vec<u8> {
    let n = pcm.len() / 2;
    let rms: Vec<f64> = (0..LEVELS)
        .map(|i| {
            let part = &pcm[2 * (i * n / LEVELS)..2 * ((i + 1) * n / LEVELS)];
            if part.is_empty() {
                return 0.0;
            }
            let sum: f64 = part
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| f64::from(i16::from_le_bytes(*b)).powi(2))
                .sum();
            (sum / (part.len() / 2) as f64).sqrt()
        })
        .collect();
    let top = rms.iter().copied().fold(0.0, f64::max);
    if top <= 0.0 {
        return vec![0; LEVELS];
    }
    rms.iter()
        .map(|r| (r / top * 255.0).round() as u8)
        .collect()
}

/// The track ids of `waveform-fetch`'s input, one per line, each once.
pub fn parse_ids(input: &str) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for id in input.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if !ids.iter().any(|i| i == id) {
            ids.push(id.to_string());
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 440 Hz tone at RATE whose amplitude goes from `from` to `to` (0 to 1) over `secs`.
    fn tone(secs: f32, from: f32, to: f32) -> Vec<u8> {
        let n = (secs * RATE as f32) as usize;
        let mut out = Vec::with_capacity(n * 2);
        for i in 0..n {
            let t = i as f32 / RATE as f32;
            let amp = from + (to - from) * i as f32 / n as f32;
            let s = (amp * 30000.0 * (t * 440.0 * std::f32::consts::TAU).sin()) as i16;
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }

    #[test]
    fn a_tone_growing_louder_gives_rising_levels_up_to_the_top() {
        let l = levels(&tone(20.0, 0.0, 1.0));
        assert_eq!(l.len(), LEVELS);
        assert!(l[0] < 5, "first {}", l[0]);
        assert_eq!(*l.iter().max().unwrap(), 255);
        assert!(l[LEVELS - 1] >= 250, "last {}", l[LEVELS - 1]);
        let mid = l[LEVELS / 2];
        assert!((118..=138).contains(&mid), "middle {mid}");
        assert!(l.windows(10).all(|w| w[9] >= w[0]), "{l:?}");
    }

    #[test]
    fn a_quiet_part_is_lower_than_a_loud_one() {
        let mut pcm = tone(5.0, 1.0, 1.0);
        pcm.extend(tone(5.0, 0.25, 0.25));
        let l = levels(&pcm);
        let (loud, quiet) = (l[LEVELS / 4], l[3 * LEVELS / 4]);
        assert_eq!(loud, 255);
        assert!((58..=70).contains(&quiet), "quiet {quiet}");
    }

    #[test]
    fn silence_gives_only_zeros() {
        assert_eq!(levels(&vec![0; RATE as usize * 4]), vec![0; LEVELS]);
        assert_eq!(levels(&[]), vec![0; LEVELS]);
    }

    #[test]
    fn a_sound_shorter_than_the_levels_still_gives_them_all() {
        let l = levels(&tone(0.01, 1.0, 1.0));
        assert_eq!(l.len(), LEVELS);
    }

    #[test]
    fn levels_written_are_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::with_home(dir.path());
        assert_eq!(read(&env, "tr/1"), None);
        let l: Vec<u8> = (0..LEVELS).map(|i| i as u8).collect();
        write(&env, "tr/1", &l).unwrap();
        assert_eq!(read(&env, "tr/1"), Some(l));
        assert!(path(&env, "tr/1").ends_with("waveforms/tr_1.wave"));
    }

    #[test]
    fn a_file_of_another_shape_is_not_a_waveform() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::with_home(dir.path());
        write_atomic(&path(&env, "t1"), b"MWV1short").unwrap();
        assert_eq!(read(&env, "t1"), None);
        write_atomic(&path(&env, "t2"), &vec![7; LEVELS + 4]).unwrap();
        assert_eq!(read(&env, "t2"), None);
    }

    #[test]
    fn each_id_is_read_once() {
        assert_eq!(
            parse_ids(" t1\n\nt2\nt1\n"),
            vec!["t1".to_string(), "t2".to_string()]
        );
    }
}
