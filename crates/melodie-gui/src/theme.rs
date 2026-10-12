//! The window's colours: the base theme (tinted by the cover, like Amberol), or the current
//! Omarchy theme when Omarchy is there (`docs/gui-toolkit.md`, "Following the theme").

use anyhow::{Context, Result};
use gpui_kit::{Hsla, Rgba, hsla};
use melodie_core::env::Env;
use melodie_core::store::write_atomic;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::Duration;

/// The only colours the views use.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub bg: Hsla,
    /// Panels and popovers.
    pub surface: Hsla,
    pub fg: Hsla,
    /// Secondary text: built from the background and the text, readable on both.
    pub secondary: Hsla,
    pub accent: Hsla,
    /// Text on the accent.
    pub on_accent: Hsla,
    pub dark: bool,
}

/// Linear mix in RGB, `t` from 0 (`a`) to 1 (`b`).
pub fn mix(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let (a, b): (Rgba, Rgba) = (a.into(), b.into());
    Rgba {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a + (b.a - a.a) * t,
    }
    .into()
}

/// The colour of a cover, enough to tint the window: `None` for a cover without colour.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tint {
    /// 0 to 1.
    pub hue: f32,
    /// How colourful the cover is, 0 to 1.
    pub strength: f32,
}

impl Palette {
    /// The base theme: dark or light as the desktop asks, tinted by the cover's colour.
    pub fn base(tint: Option<Tint>, dark: bool) -> Self {
        // Without colour, a neutral blue-grey rather than a dead grey.
        let Tint { hue, strength } = tint.unwrap_or(Tint {
            hue: 0.62,
            strength: 0.15,
        });
        let k = strength.clamp(0.0, 1.0);
        let (bg, surface, fg, accent) = if dark {
            (
                hsla(hue, 0.34 * k, 0.11, 1.0),
                hsla(hue, 0.30 * k, 0.17, 1.0),
                hsla(hue, 0.15 * k, 0.94, 1.0),
                hsla(hue, 0.25 + 0.55 * k, 0.68, 1.0),
            )
        } else {
            (
                hsla(hue, 0.40 * k, 0.95, 1.0),
                hsla(hue, 0.36 * k, 0.90, 1.0),
                hsla(hue, 0.30 * k, 0.12, 1.0),
                hsla(hue, 0.25 + 0.50 * k, 0.40, 1.0),
            )
        };
        let on_accent = bg;
        Self {
            bg,
            surface,
            fg,
            secondary: mix(bg, fg, 0.68),
            accent: readable(accent, on_accent, 4.5),
            on_accent,
            dark,
        }
    }

    pub fn mix(&self, other: &Self, t: f32) -> Self {
        Self {
            bg: mix(self.bg, other.bg, t),
            surface: mix(self.surface, other.surface, t),
            fg: mix(self.fg, other.fg, t),
            secondary: mix(self.secondary, other.secondary, t),
            accent: mix(self.accent, other.accent, t),
            on_accent: mix(self.on_accent, other.on_accent, t),
            dark: if t < 0.5 { self.dark } else { other.dark },
        }
    }
}

/// `c` made lighter or darker, whichever moves it away from `against`, until their contrast
/// reaches `min`: a yellow accent on a light background must darken to stay readable.
fn readable(c: Hsla, against: Hsla, min: f32) -> Hsla {
    let step = if against.l > 0.5 { -0.02 } else { 0.02 };
    let mut c = c;
    while contrast(c, against) < min && (0.0..=1.0).contains(&(c.l + step)) {
        c.l += step;
    }
    c
}

fn hex(s: &str) -> Option<Hsla> {
    let s = s.trim().strip_prefix('#')?;
    if s.len() != 6 {
        return None;
    }
    Some(gpui_kit::rgb(u32::from_str_radix(s, 16).ok()?).into())
}

/// So that a whole palette can be animated as one value.
impl gpui_kit::base::Interpolate for Palette {
    fn interpolate(&self, target: &Self, progress: f32) -> Self {
        self.mix(target, progress)
    }
}

/// WCAG contrast ratio of two colours (1 to 21).
pub fn contrast(a: Hsla, b: Hsla) -> f32 {
    fn lum(c: Hsla) -> f32 {
        let c: Rgba = c.into();
        let ch = |v: f32| {
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * ch(c.r) + 0.7152 * ch(c.g) + 0.0722 * ch(c.b)
    }
    let (la, lb) = (lum(a), lum(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Reads the keys the window needs from Omarchy's `colors.toml`: `background`, `foreground`
/// and `accent` are required, the rest is derived when missing.
pub fn parse_colors(text: &str) -> Result<Palette> {
    let t: toml::Table = text.parse().context("colors.toml is not TOML")?;
    let get = |k: &str| t.get(k).and_then(|v| v.as_str()).and_then(hex);
    let need = |k: &str| get(k).with_context(|| format!("colors.toml has no colour `{k}`"));
    let (bg, fg, accent) = (need("background")?, need("foreground")?, need("accent")?);
    let dark = t.get("mode").and_then(|v| v.as_str()) != Some("light");
    // Not `muted` for the secondary text: Omarchy means it for comments and dividers, too faint
    // for text.
    let on_accent = if contrast(bg, accent) >= contrast(fg, accent) {
        bg
    } else {
        fg
    };
    Ok(Palette {
        bg,
        surface: get("lighter_background").unwrap_or_else(|| mix(bg, fg, 0.08)),
        fg,
        secondary: mix(bg, fg, 0.7),
        accent,
        on_accent,
        dark,
    })
}

/// Where Omarchy says what it is and what its current theme is.
#[derive(Debug, Clone)]
pub struct Omarchy {
    pub os_release: PathBuf,
    /// `…/omarchy/current/theme/colors.toml`.
    pub colors: PathBuf,
}

impl Omarchy {
    /// The real places: `/etc/os-release` and `$XDG_STATE_HOME/omarchy/current/theme/colors.toml`.
    pub fn system(env: &Env) -> Self {
        Self {
            os_release: PathBuf::from("/etc/os-release"),
            colors: env.state_home.join("omarchy/current/theme/colors.toml"),
        }
    }

    /// The operating system is Omarchy (`ID=omarchy`).
    pub fn detected(&self) -> bool {
        std::fs::read_to_string(&self.os_release).is_ok_and(|s| {
            s.lines()
                .filter_map(|l| l.trim().strip_prefix("ID="))
                .any(|id| id.trim_matches('"') == "omarchy")
        })
    }

    /// The palette of the current theme.
    pub fn read(&self) -> Result<Palette> {
        let text = std::fs::read_to_string(&self.colors)
            .with_context(|| format!("{}", self.colors.display()))?;
        parse_colors(&text)
    }

    /// The palette when Omarchy is detected and its theme file reads; else `None`.
    pub fn offered(&self) -> Option<Palette> {
        if self.detected() {
            self.read().ok()
        } else {
            None
        }
    }

    /// The folder to watch: Omarchy replaces the whole `theme/` folder by a rename, so a watch
    /// on the file or on `theme/` would be lost.
    pub fn watched(&self) -> Option<PathBuf> {
        Some(self.colors.parent()?.parent()?.to_path_buf())
    }
}

/// The theme the user chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Choice {
    Base,
    Omarchy,
}

/// What the window shows of the choice: the base theme always, Omarchy only when offered.
pub fn choices(omarchy_offered: bool) -> Vec<Choice> {
    if omarchy_offered {
        vec![Choice::Base, Choice::Omarchy]
    } else {
        vec![Choice::Base]
    }
}

/// The colours in use, from the stored choice and what Omarchy offers now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Active {
    Base,
    Omarchy(Palette),
    /// Omarchy chosen, its files gone: the base theme, and the window says so.
    BaseOmarchyGone,
}

pub fn active(stored: Option<Choice>, detected: bool, offered: Option<Palette>) -> Active {
    match (stored, offered) {
        (Some(Choice::Base), _) => Active::Base,
        (_, Some(p)) => Active::Omarchy(p),
        (Some(Choice::Omarchy), None) if detected => Active::BaseOmarchyGone,
        _ => Active::Base,
    }
}

/// The window's own settings, `$XDG_STATE_HOME/melodie/window.toml`. A file of its own: the
/// daemon rewrites `state.toml` for the volume, and two writers of one file lose each other's
/// keys.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// Absent until the user chooses: the first launch then follows Omarchy when it is there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<Choice>,
}

impl Settings {
    pub fn path(env: &Env) -> PathBuf {
        env.state_dir().join("window.toml")
    }

    /// Missing or unreadable settings are the defaults: they never keep the window closed.
    pub fn load(env: &Env) -> Self {
        std::fs::read_to_string(Self::path(env))
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, env: &Env) -> Result<()> {
        write_atomic(&Self::path(env), toml::to_string(self)?.as_bytes())
    }
}

/// How long the theme folder must stay quiet before it is read again: Omarchy removes the old
/// folder before it renames the new one, and an editor saves through a rename too.
pub const SETTLE: Duration = Duration::from_millis(250);

/// Watches Omarchy's theme from a thread of its own and sends the palette (or `None` when the
/// file is gone or unreadable) after each change, once the folder has settled. Stops when
/// `send` returns false. `None` when the folder cannot be watched.
pub fn watch(
    omarchy: Omarchy,
    settle: Duration,
    mut send: impl FnMut(Option<Palette>) -> bool + Send + 'static,
) -> Option<notify::RecommendedWatcher> {
    use notify::{RecursiveMode, Watcher};
    let dir = omarchy.watched()?;
    let (tx, rx) = channel::<()>();
    let mut watcher = notify::recommended_watcher(move |_: notify::Result<notify::Event>| {
        let _ = tx.send(());
    })
    .ok()?;
    watcher.watch(&dir, RecursiveMode::Recursive).ok()?;
    // Read once the watch is armed and before the thread starts, so that no change made after
    // this call is taken for the starting point.
    let mut last = omarchy.read().ok();
    std::thread::spawn(move || {
        // Ends when the watcher is dropped: its sender goes with it.
        while rx.recv().is_ok() {
            loop {
                match rx.recv_timeout(settle) {
                    Ok(()) => {}
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
            let now = omarchy.read().ok();
            if now != last {
                last = now;
                if !send(now) {
                    return;
                }
            }
        }
    });
    Some(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::mpsc::Receiver;

    const TOKYO: &str = r##"
accent = "#7aa2f7"
cursor = "#c0caf5"
foreground = "#a9b1d6"
background = "#1a1b26"
selection_foreground = "#c0caf5"
selection_background = "#7aa2f7"
muted = "#565f89"
lighter_background = "#24283b"
"##;

    const LATTE: &str = r##"
mode = "light"
accent = "#1e66f5"
foreground = "#4c4f69"
background = "#eff1f5"
"##;

    /// A copy of Omarchy's layout in a temporary folder.
    fn fake_omarchy(dir: &Path, os_id: &str, colors: Option<&str>) -> Omarchy {
        std::fs::create_dir_all(dir).unwrap();
        let os = dir.join("os-release");
        std::fs::write(&os, format!("NAME=\"Something\"\nID={os_id}\n")).unwrap();
        let theme = dir.join("state/omarchy/current/theme");
        std::fs::create_dir_all(&theme).unwrap();
        if let Some(c) = colors {
            std::fs::write(theme.join("colors.toml"), c).unwrap();
        }
        Omarchy {
            os_release: os,
            colors: theme.join("colors.toml"),
        }
    }

    /// What `omarchy-theme-set` does: build the next theme, remove the current one, rename.
    fn switch_theme(o: &Omarchy, colors: &str) {
        let current = o.watched().unwrap();
        let next = current.join("next-theme");
        std::fs::create_dir_all(&next).unwrap();
        std::fs::write(next.join("colors.toml"), colors).unwrap();
        std::fs::remove_dir_all(current.join("theme")).unwrap();
        std::fs::rename(&next, current.join("theme")).unwrap();
    }

    fn hex(c: Hsla) -> String {
        let c: Rgba = c.into();
        format!(
            "#{:02x}{:02x}{:02x}",
            (c.r * 255.0).round() as u8,
            (c.g * 255.0).round() as u8,
            (c.b * 255.0).round() as u8
        )
    }

    #[test]
    fn a_sample_colors_toml_is_read() {
        let p = parse_colors(TOKYO).unwrap();
        assert_eq!(hex(p.bg), "#1a1b26");
        assert_eq!(hex(p.fg), "#a9b1d6");
        assert_eq!(hex(p.accent), "#7aa2f7");
        assert_eq!(hex(p.surface), "#24283b");
        assert!(p.dark);
        let l = parse_colors(LATTE).unwrap();
        assert!(!l.dark);
        assert!(contrast(l.secondary, l.bg) >= 3.0);
    }

    #[test]
    fn a_colors_toml_without_the_required_keys_is_refused() {
        assert!(parse_colors("background = \"#000000\"\nforeground = \"#ffffff\"").is_err());
        assert!(parse_colors("not toml at all [").is_err());
        assert!(
            parse_colors("background = \"red\"\nforeground = \"#ffffff\"\naccent = \"#ff0000\"")
                .is_err()
        );
    }

    #[test]
    fn dark_and_light_base_themes_are_readable() {
        for tint in [
            None,
            Some(Tint {
                hue: 0.0,
                strength: 1.0,
            }),
            Some(Tint {
                hue: 0.16,
                strength: 1.0,
            }),
            Some(Tint {
                hue: 0.62,
                strength: 0.3,
            }),
        ] {
            for dark in [true, false] {
                let p = Palette::base(tint, dark);
                assert_eq!(p.dark, dark);
                assert!(contrast(p.fg, p.bg) >= 7.0, "{tint:?} {dark}");
                assert!(contrast(p.secondary, p.bg) >= 4.5, "{tint:?} {dark}");
                assert!(contrast(p.fg, p.surface) >= 4.5, "{tint:?} {dark}");
                assert!(contrast(p.on_accent, p.accent) >= 4.5, "{tint:?} {dark}");
            }
        }
    }

    #[test]
    fn the_cover_tints_the_base_theme() {
        let red = Palette::base(
            Some(Tint {
                hue: 0.0,
                strength: 1.0,
            }),
            true,
        );
        let blue = Palette::base(
            Some(Tint {
                hue: 0.62,
                strength: 1.0,
            }),
            true,
        );
        assert_ne!(red.bg, blue.bg);
        assert_ne!(red.accent, blue.accent);
    }

    #[test]
    fn detection_true_and_false() {
        let dir = tempfile::tempdir().unwrap();
        let on = fake_omarchy(&dir.path().join("a"), "omarchy", Some(TOKYO));
        assert!(on.detected());
        assert!(on.offered().is_some());
        let quoted = fake_omarchy(&dir.path().join("q"), "\"omarchy\"", Some(TOKYO));
        assert!(quoted.detected());
        let arch = fake_omarchy(&dir.path().join("b"), "arch", Some(TOKYO));
        assert!(!arch.detected(), "a colors.toml left over is not Omarchy");
        assert!(arch.offered().is_none());
        let no_file = fake_omarchy(&dir.path().join("c"), "omarchy", None);
        assert!(no_file.detected());
        assert!(
            no_file.offered().is_none(),
            "Omarchy without its theme file"
        );
        let mut no_os = on.clone();
        no_os.os_release = dir.path().join("missing");
        assert!(!no_os.detected());
    }

    #[test]
    fn the_omarchy_choice_is_hidden_when_not_detected() {
        assert_eq!(choices(false), vec![Choice::Base]);
        assert_eq!(choices(true), vec![Choice::Base, Choice::Omarchy]);
    }

    #[test]
    fn first_launch_follows_omarchy_when_it_is_there() {
        let p = parse_colors(TOKYO).unwrap();
        assert_eq!(active(None, true, Some(p)), Active::Omarchy(p));
        assert_eq!(active(None, false, None), Active::Base);
        assert_eq!(active(None, true, None), Active::Base);
    }

    #[test]
    fn the_stored_choice_wins() {
        let p = parse_colors(TOKYO).unwrap();
        assert_eq!(active(Some(Choice::Base), true, Some(p)), Active::Base);
        assert_eq!(
            active(Some(Choice::Omarchy), true, Some(p)),
            Active::Omarchy(p)
        );
    }

    #[test]
    fn fallback_when_the_file_goes_away() {
        assert_eq!(
            active(Some(Choice::Omarchy), true, None),
            Active::BaseOmarchyGone
        );
        // not Omarchy any more at all: the choice is hidden, nothing to say
        assert_eq!(active(Some(Choice::Omarchy), false, None), Active::Base);
    }

    #[test]
    fn the_choice_is_kept_in_window_toml() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::with_home(dir.path());
        assert_eq!(Settings::load(&env), Settings::default());
        let s = Settings {
            theme: Some(Choice::Base),
        };
        s.save(&env).unwrap();
        assert_eq!(
            std::fs::read_to_string(env.state_dir().join("window.toml")).unwrap(),
            "theme = \"base\"\n"
        );
        assert_eq!(Settings::load(&env), s);
        std::fs::write(Settings::path(&env), "theme = 12").unwrap();
        assert_eq!(Settings::load(&env), Settings::default());
    }

    fn recv(rx: &Receiver<Option<Palette>>) -> Option<Palette> {
        rx.recv_timeout(Duration::from_secs(5))
            .expect("the watch reports a change")
    }

    #[test]
    fn a_change_on_disk_is_followed_live() {
        let dir = tempfile::tempdir().unwrap();
        let o = fake_omarchy(dir.path(), "omarchy", Some(TOKYO));
        let (tx, rx) = std::sync::mpsc::channel();
        let _w = watch(o.clone(), Duration::from_millis(100), move |p| {
            tx.send(p).is_ok()
        })
        .expect("a watch");
        switch_theme(&o, LATTE);
        let p = recv(&rx).expect("the new palette");
        assert_eq!(hex(p.bg), "#eff1f5");
        // the files go away
        std::fs::remove_dir_all(o.watched().unwrap().join("theme")).unwrap();
        assert_eq!(recv(&rx), None);
        // and come back
        let theme = o.watched().unwrap().join("theme");
        std::fs::create_dir_all(&theme).unwrap();
        std::fs::write(theme.join("colors.toml"), TOKYO).unwrap();
        assert_eq!(hex(recv(&rx).expect("back").bg), "#1a1b26");
    }

    #[test]
    fn a_theme_switch_is_one_change_not_a_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let o = fake_omarchy(dir.path(), "omarchy", Some(TOKYO));
        let (tx, rx) = std::sync::mpsc::channel();
        let _w = watch(o.clone(), Duration::from_millis(200), move |p| {
            tx.send(p).is_ok()
        })
        .expect("a watch");
        switch_theme(&o, LATTE);
        assert!(recv(&rx).is_some());
        assert!(
            rx.recv_timeout(Duration::from_millis(600)).is_err(),
            "the removal in the middle of the switch is not reported"
        );
    }
}
