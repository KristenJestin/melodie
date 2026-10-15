# The logo of Mélodie

Two notes joined into an `m`: a pink stroke, two cream note heads and a cream accent over it. The
SVGs of this folder are the one source of the drawing; every other file is generated from them with
`tools/brand.sh`, and committed. To change the logo, edit the SVGs and run it again.

| Source | For |
| --- | --- |
| `mark.svg` | the mark alone, for a dark background (the default) |
| `mark-on-light.svg` | the mark alone, for a light background |
| `icon.svg` | the icon: the mark on a dark rounded square; the desktop entry's icon (`dist/melodie.svg`) |
| `social-preview.svg` | the social preview of the repository and the banner of the README: the mark, the name, the tagline, and the waveform of the track playing above a shelf of albums, drawn (no capture, invented albums) |

The colours: pink `#fb749f` on dark (`#c42c63` on light), cream `#ece5d8`, the dark of the icon
`#14120f`, and the plum black `#181216` of the social preview.

## What is generated

- `png/`: the icon at 128, 256 and 512 pixels (`icon-*.png`) and `social-preview.png`.
- `dist/melodie.svg`: a copy of `icon.svg`, the icon the desktop entry names and the archive
  installs.

The PNGs are rendered by `rsvg-convert` (librsvg). The text of the social preview is drawn with the
fonts of [`fonts/`](fonts) only (Young Serif for the name, Open Sauce Sans, JetBrains Mono), found
through a fontconfig made for the run: no font of the machine is used.

`png/social-preview.png` is the social preview of the repository: it is uploaded by hand in the
repository's settings on GitHub (Settings, General, Social preview), as GitHub offers no other way.
