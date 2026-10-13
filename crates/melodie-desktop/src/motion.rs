//! Every animation of the window takes its duration and easing from here
//! (`docs/gui-toolkit.md`, conventions): short, ease-out, interruptible. The transitions honour
//! the desktop's reduced-motion setting (gpui-kit reads it from the portal).

use gpui_kit::base::animation::ease_out_cubic;
use gpui_kit::base::{Interpolate, Transition, TransitionId, transition};
use gpui_kit::{Animation, App, Window};
use std::time::Duration;

/// A view, a text or a panel that appears.
pub const ENTER: Duration = Duration::from_millis(240);
/// The queue panel sliding, a popover opening.
pub const PANEL: Duration = Duration::from_millis(280);
/// The cover and the colours: a track change, a theme change.
pub const COVER: Duration = Duration::from_millis(420);
/// Hover.
pub const HOVER: Duration = Duration::from_millis(140);
/// Press: quicker than the hover, so a click is felt at once.
pub const PRESS: Duration = Duration::from_millis(90);

/// The value moving towards `target`; a new target starts from where it is now.
pub fn to<T>(id: impl Into<TransitionId>, target: T, d: Duration, w: &mut Window, cx: &mut App) -> T
where
    T: Interpolate + PartialEq + 'static,
{
    transition(id, target, Transition::new(d).ease(ease_out_cubic), w, cx)
}

/// 0 to 1 as `on` becomes true, back to 0 as it becomes false.
pub fn flag(
    id: impl Into<TransitionId>,
    on: bool,
    d: Duration,
    w: &mut Window,
    cx: &mut App,
) -> f32 {
    to(id, if on { 1.0f32 } else { 0.0 }, d, w, cx)
}

/// A one-shot entrance (for `with_animation`).
pub fn enter() -> Animation {
    Animation::new(ENTER).with_easing(ease_out_cubic)
}
