//! Frame statistics, to measure the window (feature `frame-stats`, not in the program people
//! run). With `MELODIE_GUI_FRAME_STATS` set, prints one JSON line to the standard error for
//! each second in which something was animated, from GPUI's own frame trace.

use gpui_kit::profiler::{FrameEvent, FrameTimingCollector, set_trace_enabled};
use gpui_kit::*;
use std::time::{Duration, Instant};

pub fn start(cx: &mut App) {
    if std::env::var_os("MELODIE_GUI_FRAME_STATS").is_none() {
        return;
    }
    set_trace_enabled(true);
    let t0 = Instant::now();
    cx.spawn(async move |cx| {
        let mut collector = FrameTimingCollector::new();
        loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let (mut draws, mut intervals, mut refresh) = (vec![], vec![], None);
            for e in collector.collect_unseen() {
                match e {
                    FrameEvent::Draw(t) => {
                        draws.push((t.draw_end - t.draw_start).as_secs_f64() * 1e3);
                        refresh = t.refresh_interval.or(refresh);
                    }
                    FrameEvent::Present(p) => {
                        if let Some(i) = p.animation_interval {
                            intervals.push(i.as_secs_f64() * 1e3);
                        }
                    }
                }
            }
            if intervals.is_empty() {
                continue;
            }
            let refresh = refresh.map_or(1000.0 / 60.0, |r| r.as_secs_f64() * 1e3);
            // An interval of k refreshes means k - 1 frames that were not drawn in time.
            let missed: f64 = intervals
                .iter()
                .map(|i| ((i / refresh).round() - 1.0).max(0.0))
                .sum();
            draws.sort_by(f64::total_cmp);
            intervals.sort_by(f64::total_cmp);
            let pct = |v: &[f64], p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
            eprintln!(
                "{{\"t\":{:.0},\"refresh_hz\":{:.1},\"animated_frames\":{},\"missed\":{},\
                 \"interval_ms\":{{\"p50\":{:.2},\"max\":{:.2}}},\
                 \"draw_ms\":{{\"mean\":{:.3},\"p99\":{:.3},\"max\":{:.3}}}}}",
                t0.elapsed().as_secs_f64(),
                1000.0 / refresh,
                intervals.len(),
                missed,
                pct(&intervals, 0.5),
                intervals[intervals.len() - 1],
                draws.iter().sum::<f64>() / draws.len().max(1) as f64,
                if draws.is_empty() {
                    0.0
                } else {
                    pct(&draws, 0.99)
                },
                draws.last().copied().unwrap_or(0.0),
            );
        }
    })
    .detach();
}
