//! `melodie waveform-fetch`, started by the window: the waveforms of the track ids read on its
//! standard input (the current track and the next one), computed from their streams and kept
//! beside the covers. The audio is decoded by a private libmpv into a file (`ao=pcm`): never a
//! sound output, never the daemon's mpv.

use anyhow::{Context, Result, anyhow, bail};
use libmpv2::events::Event;
use libmpv2::{Mpv, mpv_end_file_reason};
use melodie_core::api::{Client, SecretUrl};
use melodie_core::cover::LOCKED_EXIT;
use melodie_core::env::Env;
use melodie_core::waveform::{self, Ask, RATE};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

/// How long the decoded file may stop growing before the stream is given up: a long track on a
/// slow link still finishes, a stream that stalls does not hold the lock for ever.
const STALL: Duration = Duration::from_secs(30);

/// The progress of a decoding: the size of its file, and since when it has not changed.
struct Progress {
    size: u64,
    since: Instant,
}

impl Progress {
    fn new(at: Instant) -> Self {
        Self { size: 0, since: at }
    }

    /// Takes the file's `size` at `at` in; true once it has not grown for [`STALL`].
    fn stalled(&mut self, size: u64, at: Instant) -> bool {
        if size != self.size {
            self.size = size;
            self.since = at;
        }
        at.duration_since(self.since) > STALL
    }
}

/// One at a time (a lock), else at once `LOCKED_EXIT`: the window asks again a little later.
/// A waveform already kept is not computed again; one that fails is not kept.
pub fn fetch_cmd(env: &Env) -> Result<ExitCode> {
    // the music playing, and the window, come first
    // SAFETY: `nice` only lowers the priority of this process.
    unsafe { libc::nice(19) };
    let dir = waveform::dir(env);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(dir.join(".fetch.lock"))?;
    // SAFETY: valid descriptor, owned by `lock`.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Ok(ExitCode::from(LOCKED_EXIT));
    }
    remove_decoded(&dir);
    let mut input = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut input)?;
    let asks: Vec<Ask> = waveform::parse_asks(&input)
        .into_iter()
        .filter(|a| waveform::read(env, &a.id).is_none())
        .collect();
    if asks.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }
    let Ok(client) = melodie_core::state::open_client(env) else {
        return Ok(ExitCode::SUCCESS);
    };
    for ask in asks {
        match compute(&client, &dir, &ask) {
            Ok(levels) => waveform::write(env, &ask.id, &levels)?,
            Err(e) => eprintln!("melodie: waveform of {}: {e:#}", ask.id),
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Removes the decoded files a killed run left behind: with the lock held, none is in use.
fn remove_decoded(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(".decode-") && name.ends_with(".pcm") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The levels of the track asked: from a light transcoded stream, else, once, from the file
/// itself (a server that cannot transcode).
fn compute(client: &Client, dir: &Path, ask: &Ask) -> Result<Vec<u8>> {
    compute_from(&client.light_stream_url(&ask.id), dir, ask)
        .or_else(|_| compute_from(&client.stream_url(&ask.id), dir, ask))
}

/// The levels of the stream `url`, decoded into a file of `dir` that is removed after.
fn compute_from(url: &SecretUrl, dir: &Path, ask: &Ask) -> Result<Vec<u8>> {
    let pcm = dir.join(format!(".decode-{}.pcm", std::process::id()));
    let levels = decode(url, &pcm).and_then(|()| read_levels(&pcm, ask.duration));
    let _ = std::fs::remove_file(&pcm);
    levels
}

/// The levels of the decoded file `pcm`, read in one pass, unless it is shorter than the track's
/// `duration`: a stream cut short would be kept stretched for ever.
fn read_levels(pcm: &Path, duration: Option<f64>) -> Result<Vec<u8>> {
    let file = std::fs::File::open(pcm).context("opening the decoded audio")?;
    let samples = file.metadata()?.len() as usize / 2;
    if !waveform::whole(samples as f64 / f64::from(RATE), duration) {
        bail!("the stream ended early");
    }
    waveform::levels_from(file, samples).context("reading the decoded audio")
}

/// A private libmpv that only writes decoded audio into `out`: no video, no scripts, no
/// configuration, and a file writer alone as its audio output.
fn build_mpv(out: &Path) -> Result<Mpv> {
    let file = out.to_str().context("a cache path that is not UTF-8")?;
    Mpv::with_initializer(|init| {
        init.set_option("config", "no")?;
        init.set_option("vid", "no")?;
        init.set_option("audio-display", "no")?;
        init.set_option("ytdl", "no")?;
        init.set_option("load-scripts", "no")?;
        init.set_option("network-timeout", "15")?;
        // a file writer, alone in the list: mpv never falls back to a device that makes sound
        init.set_option("ao", "pcm")?;
        init.set_option("ao-pcm-file", file)?;
        init.set_option("ao-pcm-waveheader", "no")?;
        init.set_option("audio-format", "s16")?;
        init.set_option("audio-channels", "mono")?;
        init.set_option("audio-samplerate", i64::from(RATE))?;
        init.set_option("untimed", "yes")?;
        Ok(())
    })
    .map_err(|e| anyhow!("libmpv: {e}"))
}

/// Decodes the stream into `out`: mono, signed 16 bits at `RATE`, as fast as it comes.
fn decode(url: &SecretUrl, out: &Path) -> Result<()> {
    let mpv = build_mpv(out)?;
    // the error of a command could hold its arguments: never said
    mpv.command("loadfile", &[url.expose()])
        .map_err(|_| anyhow!("the stream could not be opened"))?;
    let mut progress = Progress::new(Instant::now());
    loop {
        match mpv.wait_event(1.0) {
            Some(Ok(Event::EndFile(mpv_end_file_reason::Eof))) => break,
            Some(Ok(Event::EndFile(_)) | Err(_)) => bail!("the stream could not be decoded"),
            Some(Ok(Event::Shutdown)) => bail!("libmpv stopped"),
            _ => {
                let size = std::fs::metadata(out).map_or(0, |m| m.len());
                if progress.stalled(size, Instant::now()) {
                    bail!("the stream stalled");
                }
            }
        }
    }
    // the file is complete once the audio output is closed
    drop(mpv);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_decoding_is_given_up_only_when_it_stops_making_progress() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut p = Progress::new(t0);
        // slow but steady, far past any total cap
        for s in 1..=1000 {
            assert!(!p.stalled(s * 100, at(s * 20)));
        }
        let size = 100_001;
        assert!(!p.stalled(size, at(20_001)));
        assert!(!p.stalled(size, at(20_001 + STALL.as_secs())));
        assert!(p.stalled(size, at(20_002 + STALL.as_secs())));
        // nothing at all from the start
        let mut p = Progress::new(t0);
        assert!(!p.stalled(0, at(STALL.as_secs())));
        assert!(p.stalled(0, at(STALL.as_secs() + 1)));
    }

    #[test]
    fn the_waveform_decoder_makes_no_sound_and_loads_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mpv = build_mpv(&dir.path().join("out.pcm")).expect("libmpv");
        assert_eq!(mpv.get_property::<String>("ao").unwrap(), "pcm");
        assert_eq!(mpv.get_property::<String>("vid").unwrap(), "no");
        for flag in ["load-scripts", "ytdl", "config"] {
            assert!(!mpv.get_property::<bool>(flag).expect(flag), "{flag}");
        }
    }
}
