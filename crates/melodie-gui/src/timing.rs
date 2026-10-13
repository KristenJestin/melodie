//! How long each view takes to read the local copy, for measuring the window (feature
//! `frame-stats`, with `MELODIE_GUI_FRAME_STATS` set): one line on the standard error per read.

use std::time::Duration;

pub fn on() -> bool {
    cfg!(feature = "frame-stats") && std::env::var_os("MELODIE_GUI_FRAME_STATS").is_some()
}

pub fn note(what: &str, took: Duration) {
    eprintln!("load {what} {:.1} ms", took.as_secs_f64() * 1e3);
}
