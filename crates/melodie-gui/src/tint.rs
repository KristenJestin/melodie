//! The colour of a cover, which tints the base theme.

use crate::theme::Tint;
use std::path::Path;

/// The dominant colour of the cover file at `path`: `None` when it cannot be read or has no
/// colour to speak of (black and white, grey).
pub fn of_file(path: &Path) -> Option<Tint> {
    let bytes = std::fs::read(path).ok()?;
    of_image(&image::load_from_memory(&bytes).ok()?)
}

/// The dominant colour of a decoded cover.
pub fn of_image(img: &image::DynamicImage) -> Option<Tint> {
    // A few hundred pixels say the colour as well as the whole cover.
    let small = img.thumbnail(24, 24).to_rgb8();
    let (mut x, mut y, mut chroma_sum, mut weight) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    for p in small.pixels() {
        let [r, g, b] = p.0.map(|v| f32::from(v) / 255.0);
        let max = r.max(g).max(b);
        let chroma = max - r.min(g).min(b);
        if chroma <= f32::EPSILON {
            continue;
        }
        let hue = if max == r {
            ((g - b) / chroma).rem_euclid(6.0)
        } else if max == g {
            (b - r) / chroma + 2.0
        } else {
            (r - g) / chroma + 4.0
        } / 6.0;
        // Colourful pixels count most: a small red patch on a grey cover gives a red tint.
        let w = chroma * chroma;
        let a = hue * std::f32::consts::TAU;
        x += w * a.cos();
        y += w * a.sin();
        chroma_sum += chroma;
        weight += w;
    }
    let n = small.pixels().len() as f32;
    if n == 0.0 || chroma_sum / n < 0.02 {
        return None;
    }
    Some(Tint {
        hue: (y.atan2(x) / std::f32::consts::TAU).rem_euclid(1.0),
        strength: (weight / chroma_sum).clamp(0.0, 1.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    fn save(dir: &Path, name: &str, f: impl Fn(u32, u32) -> [u8; 3]) -> std::path::PathBuf {
        let img = RgbImage::from_fn(64, 64, |x, y| Rgb(f(x, y)));
        // the daemon names every cover `.img`, whatever its format
        let p = dir.join(name);
        img.save_with_format(&p, image::ImageFormat::Png).unwrap();
        p
    }

    #[test]
    fn a_red_cover_and_a_blue_cover() {
        let dir = tempfile::tempdir().unwrap();
        let red = of_file(&save(dir.path(), "red.img", |_, _| [200, 30, 40])).unwrap();
        assert!(red.hue < 0.03 || red.hue > 0.97, "{red:?}");
        assert!(red.strength > 0.5);
        let blue = of_file(&save(dir.path(), "blue.img", |_, _| [30, 60, 210])).unwrap();
        assert!((blue.hue - 0.63).abs() < 0.04, "{blue:?}");
    }

    #[test]
    fn the_colour_wins_over_a_grey_background() {
        let dir = tempfile::tempdir().unwrap();
        // a small green square on a large grey cover
        let p = save(dir.path(), "g.img", |x, y| {
            if x < 20 && y < 20 {
                [40, 190, 60]
            } else {
                [128, 128, 128]
            }
        });
        let t = of_file(&p).unwrap();
        assert!((t.hue - 0.36).abs() < 0.05, "{t:?}");
    }

    #[test]
    fn a_grey_cover_has_no_tint() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            of_file(&save(dir.path(), "grey.img", |x, _| {
                let v = (x * 4) as u8;
                [v, v, v]
            })),
            None
        );
    }

    #[test]
    fn an_unreadable_cover_has_no_tint() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bad.img");
        std::fs::write(&p, b"not an image").unwrap();
        assert_eq!(of_file(&p), None);
        assert_eq!(of_file(&dir.path().join("missing.img")), None);
    }
}
