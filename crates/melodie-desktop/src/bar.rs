//! What the player bar at the bottom of the window shows at each width. Kept away from GPUI so
//! that it is tested.
//!
//! The bar is three columns: the track on the left, the controls in the middle, the volume and
//! the buttons of the window on the right. The two sides get the same room, so that play/pause
//! sits on the axis of the main area whatever each side holds. A narrow window drops the less
//! needed parts first, in this order: the volume, the times, the title and artist, the like
//! button; play/pause and the position bar never go.

/// The bar's height: two lines in the middle (the buttons, the position), room around them.
pub const HEIGHT: f32 = 88.0;

/// The play/pause button, the gap under it, and the row of the position bar and the times.
pub const PLAY: f32 = 44.0;
pub const ROWS_GAP: f32 = 10.0;
pub const POSITION: f32 = 20.0;

/// The widest the middle gets: past it, the room goes to the sides.
pub const CENTER_MAX: f32 = 560.0;

const COVER: f32 = 44.0;
const COVER_SMALL: f32 = 40.0;
/// The title and the artist, with room for a few words before they are cut.
const TEXT_MIN: f32 = 96.0;
const LIKE: f32 = 32.0;
/// The queue button and the button of the full player.
const BUTTONS: f32 = 2.0 * 36.0 + 4.0;
const BUTTONS_SMALL: f32 = 2.0 * 32.0 + 2.0;
/// The mute icon and the slider.
const VOLUME: f32 = 8.0 + 32.0 + 6.0 + 90.0;
/// The buttons, the position bar under them, and the times on each side of it.
const CENTER_TIMES: f32 = 240.0;
const CENTER: f32 = 150.0;
const CENTER_SMALL: f32 = 116.0;
/// The room at the bar's two ends, and the gaps between the columns.
const AROUND: f32 = 2.0 * 12.0 + 2.0 * 8.0;
const AROUND_SMALL: f32 = 2.0 * 8.0 + 2.0 * 4.0;

/// What the bar shows in a main area of some width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Parts {
    pub volume: bool,
    pub times: bool,
    /// The title and the artist beside the cover.
    pub text: bool,
    pub like: bool,
    /// The smallest buttons and gaps, for the narrowest window.
    pub small: bool,
    /// The width of the middle column; each side gets half of what is left.
    pub center: f32,
}

impl Parts {
    const FULL: Self = Self {
        volume: true,
        times: true,
        text: true,
        like: true,
        small: false,
        center: 0.0,
    };

    /// What the left column holds at the least.
    fn left(&self) -> f32 {
        let cover = if self.small { COVER_SMALL } else { COVER };
        cover
            + if self.text { 12.0 + TEXT_MIN } else { 0.0 }
            + if self.like { 6.0 + LIKE } else { 0.0 }
    }

    fn right(&self) -> f32 {
        let buttons = if self.small { BUTTONS_SMALL } else { BUTTONS };
        buttons + if self.volume { VOLUME } else { 0.0 }
    }

    fn middle(&self) -> f32 {
        if self.small {
            CENTER_SMALL
        } else if self.times {
            CENTER_TIMES
        } else {
            CENTER
        }
    }

    fn around(&self) -> f32 {
        if self.small { AROUND_SMALL } else { AROUND }
    }

    /// The side every column keeps: the same for both, so that the middle is centered.
    pub fn side(&self) -> f32 {
        self.left().max(self.right())
    }

    /// The width the bar needs to show exactly this.
    pub fn needs(&self) -> f32 {
        2.0 * self.side() + self.middle() + self.around()
    }
}

/// What a bar `width` pixels wide shows: the most it can, the parts going in a fixed order.
pub fn parts(width: f32) -> Parts {
    let tiers = [
        Parts::FULL,
        Parts {
            volume: false,
            ..Parts::FULL
        },
        Parts {
            volume: false,
            times: false,
            ..Parts::FULL
        },
        Parts {
            volume: false,
            times: false,
            text: false,
            ..Parts::FULL
        },
        Parts {
            volume: false,
            times: false,
            text: false,
            like: false,
            small: true,
            center: 0.0,
        },
    ];
    let p = tiers
        .into_iter()
        .find(|p| p.needs() <= width)
        .unwrap_or(tiers[tiers.len() - 1]);
    // the room left over goes to the middle, up to its limit; the sides keep what they need
    let spare = width - p.around() - 2.0 * p.side();
    Parts {
        center: spare.clamp(p.middle(), CENTER_MAX.max(p.middle())),
        ..p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The narrowest main area there is: the window's minimum width (360) less the compact sidebar.
    const NARROWEST: f32 = 296.0;

    fn shown(p: Parts) -> [bool; 4] {
        [p.volume, p.times, p.text, p.like]
    }

    /// The least room above and below the middle column.
    const MARGIN_MIN: f32 = 6.0;

    #[test]
    fn the_middle_column_fits_the_bar_with_room_around_it() {
        let content = PLAY + ROWS_GAP + POSITION;
        assert!(
            content + 2.0 * MARGIN_MIN <= HEIGHT,
            "{content} px of content in a bar of {HEIGHT}"
        );
    }

    #[test]
    fn a_wide_window_shows_everything() {
        // the window's default width without the queue beside the main area
        let p = parts(1180.0 - 220.0);
        assert_eq!(shown(p), [true; 4]);
        assert!(!p.small);
    }

    #[test]
    fn with_the_queue_docked_at_the_default_width_the_title_stays() {
        let p = parts(1180.0 - 220.0 - 380.0);
        assert_eq!(shown(p), [false, false, true, true]);
    }

    #[test]
    fn the_narrowest_window_keeps_play_pause_and_the_position() {
        let p = parts(NARROWEST);
        assert!(p.small);
        assert!(p.needs() <= NARROWEST);
        assert!(p.center >= CENTER_SMALL);
    }

    #[test]
    fn a_narrow_window_drops_the_volume_first_then_the_times() {
        let full = Parts::FULL.needs();
        let no_volume = parts(full - 1.0);
        assert_eq!(shown(no_volume), [false, true, true, true]);
        let no_times = parts(
            Parts {
                volume: false,
                ..Parts::FULL
            }
            .needs()
                - 1.0,
        );
        assert_eq!(shown(no_times), [false, false, true, true]);
    }

    #[test]
    fn then_the_title_and_the_like_button_and_the_buttons_shrink() {
        let no_times = Parts {
            volume: false,
            times: false,
            ..Parts::FULL
        };
        let no_text = parts(no_times.needs() - 1.0);
        assert_eq!(shown(no_text), [false, false, false, true]);
        assert!(!no_text.small);
        let smallest = parts(
            Parts {
                text: false,
                ..no_times
            }
            .needs()
                - 1.0,
        );
        assert_eq!(shown(smallest), [false, false, false, false]);
        assert!(smallest.small);
    }

    #[test]
    fn what_goes_never_comes_back_as_the_window_narrows() {
        let mut last = shown(parts(2400.0));
        let mut w = 2400.0;
        while w >= NARROWEST {
            let now = shown(parts(w));
            for (before, after) in last.iter().zip(now) {
                assert!(
                    *before || !after,
                    "a part came back at {w}: {last:?} {now:?}"
                );
            }
            last = now;
            w -= 1.0;
        }
    }

    #[test]
    fn at_every_width_the_middle_fits_and_both_sides_have_the_same_room() {
        let mut w = NARROWEST;
        while w <= 2400.0 {
            let p = parts(w);
            assert!(p.needs() <= w, "{w}: needs {}", p.needs());
            assert!(p.center >= p.middle(), "{w}: the middle is squeezed");
            assert!(p.center <= CENTER_MAX.max(p.middle()));
            // the room each side gets is the same for both, and holds what either needs
            let side = (w - p.around() - p.center) / 2.0;
            assert!(side + 0.001 >= p.side(), "{w}: a side is squeezed");
            w += 1.0;
        }
    }

    #[test]
    fn the_middle_takes_the_spare_room_up_to_its_limit() {
        assert_eq!(parts(2400.0).center, CENTER_MAX);
        let p = parts(1000.0);
        assert!(p.center > p.middle() && p.center < CENTER_MAX);
    }
}
