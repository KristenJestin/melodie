//! The waveform of a track: its loudness along the track, a few hundred levels kept in
//! `~/.cache/melodie/waveforms/<id>.wave`, beside the covers. Navidrome gives none: the levels
//! are computed from the audio by `melodie waveform-fetch`, once per track. The window draws its
//! progress bar from them.

use crate::cover;
use crate::db;
use crate::env::Env;
use crate::store::write_atomic;
use anyhow::Result;
use std::io::{BufReader, ErrorKind, Read};
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
    levels_from(pcm, pcm.len() / 2).expect("reading a slice")
}

/// [`levels`] read from `pcm` in one pass, `samples` long, without holding it in memory: an hour
/// of decoded audio is 58 MB.
pub fn levels_from(pcm: impl Read, samples: usize) -> std::io::Result<Vec<u8>> {
    let bounds = |i: usize| i * samples / LEVELS;
    let mut sums = vec![0.0f64; LEVELS];
    let mut pcm = BufReader::new(pcm);
    let mut sample = [0u8; 2];
    let mut i = 0;
    for j in 0..samples {
        match pcm.read_exact(&mut sample) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
        while j >= bounds(i + 1) {
            i += 1;
        }
        sums[i] += f64::from(i16::from_le_bytes(sample)).powi(2);
    }
    let rms: Vec<f64> = (0..LEVELS)
        .map(|i| match bounds(i + 1) - bounds(i) {
            0 => 0.0,
            len => (sums[i] / len as f64).sqrt(),
        })
        .collect();
    let top = rms.iter().copied().fold(0.0, f64::max);
    if top <= 0.0 {
        return Ok(vec![0; LEVELS]);
    }
    Ok(rms
        .iter()
        .map(|r| (r / top * 255.0).round() as u8)
        .collect())
}

/// A track `waveform-fetch` is asked for: one line of its input, `id` or `id<TAB>seconds`.
#[derive(Debug, Clone, PartialEq)]
pub struct Ask {
    pub id: String,
    /// The length of the track, when the asker knows it: a stream that ends well before it is
    /// not kept.
    pub duration: Option<f64>,
}

impl Ask {
    /// Its line of `waveform-fetch`'s input.
    pub fn line(&self) -> String {
        match self.duration {
            Some(d) => format!("{}\t{d}", self.id),
            None => self.id.clone(),
        }
    }
}

/// The tracks of `waveform-fetch`'s input, one per line, each once.
pub fn parse_asks(input: &str) -> Vec<Ask> {
    let mut asks: Vec<Ask> = Vec::new();
    for line in input.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let (id, duration) = match line.split_once('\t') {
            Some((id, secs)) => (
                id.trim(),
                secs.trim().parse().ok().filter(|d: &f64| *d > 0.0),
            ),
            None => (line, None),
        };
        if !asks.iter().any(|a| a.id == id) {
            asks.push(Ask {
                id: id.to_string(),
                duration,
            });
        }
    }
    asks
}

/// Whether `decoded` seconds of audio are the whole of a track of `expected` seconds: mpv ends a
/// stream whose connection dropped as if it were complete. Short by 2 s or 3 % at most (the
/// larger), as the length the server gives is rounded and decoders trim a little.
pub fn whole(decoded: f64, expected: Option<f64>) -> bool {
    expected.is_none_or(|e| e - decoded <= (e * 0.03).max(2.0))
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

    /// Hands its bytes out a few at a time, in odd pieces that cut samples in half.
    struct Trickle<'a>(&'a [u8]);

    impl Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.0.len().min(buf.len()).min(3);
            buf[..n].copy_from_slice(&self.0[..n]);
            self.0 = &self.0[n..];
            Ok(n)
        }
    }

    #[test]
    fn levels_read_in_one_pass_are_those_of_the_whole_sound() {
        let mut pcm = tone(7.0, 0.0, 1.0);
        pcm.extend(tone(3.3, 0.4, 0.1));
        let n = pcm.len() / 2;
        assert_eq!(levels_from(Trickle(&pcm), n).unwrap(), levels(&pcm));
        let mut l = levels(&pcm);
        l.sort();
        assert!(l[LEVELS / 2] > 0 && l[LEVELS - 1] == 255);
        // a file shorter than said: what is there, the rest silent
        let short = levels_from(Trickle(&pcm[..pcm.len() / 2]), n).unwrap();
        assert_eq!(short.len(), LEVELS);
        assert!(short[LEVELS - 1] == 0);
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
        let ask = |id: &str, duration| Ask {
            id: id.into(),
            duration,
        };
        assert_eq!(
            parse_asks(" t1\t241.5\n\nt2\nt1\nt3\tsoon\nt4\t0\n"),
            vec![
                ask("t1", Some(241.5)),
                ask("t2", None),
                ask("t3", None),
                ask("t4", None)
            ]
        );
        let asks = vec![ask("t1", Some(200.0)), ask("t2", None)];
        let lines: Vec<String> = asks.iter().map(Ask::line).collect();
        assert_eq!(parse_asks(&lines.join("\n")), asks);
    }

    #[test]
    fn a_stream_that_ended_early_is_not_whole() {
        // a 4 minute track: 3 % (7.2 s), more than 2 s
        assert!(whole(240.0, Some(240.0)));
        assert!(whole(233.0, Some(240.0)));
        assert!(!whole(232.0, Some(240.0)));
        assert!(!whole(120.0, Some(240.0)));
        // a short track: 2 s
        assert!(whole(28.1, Some(30.0)));
        assert!(!whole(27.9, Some(30.0)));
        // longer than said, or no length to compare with
        assert!(whole(250.0, Some(240.0)));
        assert!(whole(3.0, None));
    }
}
