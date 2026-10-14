//! Frame times, to measure the window (feature `frame-stats`, not in the program people run).
//! With `MELODIE_DESKTOP_FRAME_STATS=<file>`, writes one line per frame from GPUI's own frame trace:
//! the time it was presented (seconds since the window started) and how long it took to draw
//! (milliseconds). Telling a dropped frame from a pause between two animations is left to the
//! reader, who knows what the window was asked to do.

use gpui_kit::profiler::{FrameEvent, FrameTimingCollector, set_trace_enabled};
use gpui_kit::*;
use std::io::Write as _;
use std::time::{Duration, Instant};

pub fn start(cx: &mut App) {
    let Some(path) = std::env::var_os("MELODIE_DESKTOP_FRAME_STATS") else {
        return;
    };
    let Ok(mut out) = std::fs::File::create(path) else {
        return;
    };
    set_trace_enabled(true);
    let t0 = Instant::now();
    cx.spawn(async move |cx| {
        let mut collector = FrameTimingCollector::new();
        let mut draw_ms = 0.0;
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            for e in collector.collect_unseen() {
                match e {
                    FrameEvent::Draw(t) => {
                        draw_ms = (t.draw_end - t.draw_start).as_secs_f64() * 1e3;
                    }
                    FrameEvent::Present(p) => {
                        let at = p.present_end.saturating_duration_since(t0).as_secs_f64();
                        let _ = writeln!(out, "{at:.6} {draw_ms:.3}");
                    }
                }
            }
        }
    })
    .detach();
}
