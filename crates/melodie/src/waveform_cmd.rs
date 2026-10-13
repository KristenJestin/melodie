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
use melodie_core::waveform::{self, RATE};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

/// The longest a track may take to decode: a stream that stalls is given up.
const DECODE_MAX: Duration = Duration::from_secs(300);

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
    let mut input = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut input)?;
    let ids: Vec<String> = waveform::parse_ids(&input)
        .into_iter()
        .filter(|id| waveform::read(env, id).is_none())
        .collect();
    if ids.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }
    let Ok(client) = melodie_core::state::open_client(env) else {
        return Ok(ExitCode::SUCCESS);
    };
    for id in ids {
        match compute(&client, &dir, &id) {
            Ok(levels) => waveform::write(env, &id, &levels)?,
            Err(e) => eprintln!("melodie: waveform of {id}: {e:#}"),
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// The levels of the track `id`, decoded into a file of `dir` that is removed after.
fn compute(client: &Client, dir: &Path, id: &str) -> Result<Vec<u8>> {
    let pcm = dir.join(format!(".decode-{}.pcm", std::process::id()));
    let levels = decode(&client.stream_url(id), &pcm).and_then(|()| read_levels(&pcm));
    let _ = std::fs::remove_file(&pcm);
    levels
}

/// The levels of the decoded file `pcm`, read in one pass.
fn read_levels(pcm: &Path) -> Result<Vec<u8>> {
    let file = std::fs::File::open(pcm).context("opening the decoded audio")?;
    let samples = file.metadata()?.len() as usize / 2;
    waveform::levels_from(file, samples).context("reading the decoded audio")
}

/// Decodes the stream into `out`: mono, signed 16 bits at `RATE`, as fast as it comes.
fn decode(url: &SecretUrl, out: &Path) -> Result<()> {
    let file = out.to_str().context("a cache path that is not UTF-8")?;
    let mpv = Mpv::with_initializer(|init| {
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
    .map_err(|e| anyhow!("libmpv: {e}"))?;
    // the error of a command could hold its arguments: never said
    mpv.command("loadfile", &[url.expose()])
        .map_err(|_| anyhow!("the stream could not be opened"))?;
    let start = Instant::now();
    loop {
        match mpv.wait_event(1.0) {
            Some(Ok(Event::EndFile(mpv_end_file_reason::Eof))) => break,
            Some(Ok(Event::EndFile(_)) | Err(_)) => bail!("the stream could not be decoded"),
            Some(Ok(Event::Shutdown)) => bail!("libmpv stopped"),
            _ if start.elapsed() > DECODE_MAX => bail!("the stream took too long"),
            _ => {}
        }
    }
    // the file is complete once the audio output is closed
    drop(mpv);
    Ok(())
}
