# Mélodie

A music player for a Navidrome server (Subsonic / OpenSubsonic API, tried on Navidrome 0.63) on
Linux (Wayland), written in Rust: a background daemon, a command line, a local copy of the library
and mixes computed locally. A window is to come; it will be one more client of the same daemon and
the same local copy.

Agents read `AGENTS.md`. Work in progress: see the issues.

## Build and layout

```
cargo build            # libmpv (libmpv-dev, mpv) is needed to build and to play
cargo test
```

A Cargo workspace under `crates/`:

- `melodie-core`: what every Mélodie program shares: Subsonic client, local copy, synchronisation,
  search, mixes, playlists, likes, covers, the keyring store, the socket protocol and the saved
  preferences. Its `fake` feature exposes the fake local server the tests talk to.
- `melodie`: the program: command line, daemon (libmpv), MPRIS, desktop notifications and the
  background synchronisation launcher.

The tests never touch a real server, real sound, the session bus, the keyring or the real
configuration (see `AGENTS.md`). The daemon integration test starts its own private
`dbus-daemon`: it must be installed, or the MPRIS checks are skipped with a message.

## Commands

- `melodie login`: asks for the address (the value of `config.toml` is offered), the user, then
  the password without echo. Computes the Subsonic token `t = md5(password + salt)` (random
  16-character alphanumeric salt), forgets the password, checks with `ping.view`, then stores
  `{"u","t","s"}` in the keyring (Secret Service, attributes `service=melodie` and
  `url=<server>`). Then starts the copy of the library in the background ("Copie de la
  bibliothèque en cours en arrière-plan").
- `melodie logout`: removes the secret from the keyring and the user from `config.toml`.
- `melodie status`: server, user, connection state, size, date and age of the copy, whether a
  synchronisation is running.
- `melodie sync [--full]`: updates the local copy by hand. Not needed in normal use (see
  "Automatic synchronisation").
- `melodie search <text…> [--json]`: searches the local copy.
- `melodie playlists [--json]`: lists the playlists.
- `melodie play <kind> <id> [--shuffle]`: `kind` is `playlist`, `album`, `artist` or `track`.
  Replaces the queue and starts playing. An artist gives all their albums (year, then name), tracks
  in the server's order; a track plays alone. The list comes from the API (`getPlaylist`,
  `getAlbum`, `getArtist`, `getSong`), not from the local copy.
- `melodie queue <kind> <id>`: appends to the queue (starts playing if the queue is empty).
- `melodie toggle | pause | resume | next | prev | stop`: controls. They never start the daemon
  (without a daemon: "Rien en lecture", code 1). `prev` restarts the track after 3 s of playing,
  else goes back to the previous one. `next` on the last track ends the queue.
- `melodie volume [<0-100> | +5 | -5]`: without argument, shows the volume; otherwise sets it
  (0 to 130 %, `+5` and `-5` are relative). The volume is remembered in
  `~/.local/state/melodie/state.toml` (written about 1 s after the last change, whether it comes
  from the command or from MPRIS) and restored when the daemon starts; 50 % by default. Without a
  daemon, the command only writes the remembered value.
- `melodie now [--json]`: track, position, quality, state. Without a daemon: state `stopped`.
  JSON: `state` (`playing`, `paused`, `stopped`), `id`, `title`, `artist`, `album`,
  `album_artist`, `duration` and `position` (seconds), `quality`, `cover` (local path or null),
  `index`, `queue_len`, `volume` (0 to 1 like MPRIS; 1 = 100 %, up to 1.3), `starred` (the track is
  a favourite).
- `melodie like [<kind> <id>]`, `unlike [<kind> <id>]`, `like --toggle`: favourites (see
  "Favourites and playlists").
- `melodie playlist add|remove|duplicates|create|rename|delete|pick`: writes the playlists.
- `melodie home [--json]`: the launcher content without typing: first the entries of `mixes`,
  then the playlists (same connection states as `search`).
- `melodie mix me|rediscover|track|album|artist|genre …` and `melodie mixes [--json]`: local mixes
  (see "Mixes").

`play` and `queue` start the daemon if it is not running, but only if the connection is good:
`no-login`, `auth-refused` and `unreachable` give a clear message (code 1, 1 or 2) and a
`notify-send -a "Mélodie"` notification, without starting the daemon.

## The daemon

`melodie daemon` (a hidden subcommand) is started detached (new session) by `play` or `queue`.
Only one at a time (lock `$XDG_RUNTIME_DIR/melodie/melodie.lock`). It holds libmpv (crate
`libmpv2`, default `ao`, so PipeWire), plays the streams
`<server>/rest/stream.view?id=…&format=raw&…` handed to mpv in memory (never written, logged or
exposed), and keeps the queue in step with mpv's playlist.

- Control: Unix socket `$XDG_RUNTIME_DIR/melodie/melodie.sock` (0600, folder 0700), one JSON
  request per line (`{"cmd":"next"}`), one JSON answer per line (`{"ok":true}` or
  `{"ok":false,"error":"…"}`; `now` adds the `now` object).
- MPRIS 2: `org.mpris.MediaPlayer2.melodie`, identity "Mélodie", no DesktopEntry. Metadata:
  `mpris:trackid` (`/org/melodie/track/<n>`), `mpris:length`, `xesam:title`, `xesam:artist`,
  `xesam:album`, `xesam:albumArtist`, `mpris:artUrl` (local cover), `xesam:comment`,
  `melodie:starred` (boolean, the track is a favourite) and `melodie:quality` (quality, for example
  "FLAC · 24 bit / 96 kHz · 2304 kbps", "MP3 · 320 kbps"). No `xesam:url`. `PropertiesChanged` on
  every change of track, state or volume; `Seeked` on a jump.
- Volume: starts at the remembered volume (100 % the first time, like other applications). mpv
  applies ReplayGain per track (`replaygain=track`, `replaygain-preamp=4`, `replaygain-clip=yes`,
  `replaygain-fallback=0`): files without tags are not corrected.
- Covers: `getCoverArt` (512) in `~/.cache/melodie/covers/<id>.img`, once, in the background.
- Plays: `scrobble` with `submission=false` at the start of each track, `submission=true` at 50 %
  of the duration or 4 minutes actually played (pauses excluded). A failure is ignored.
- Skipped tracks: a `next` before 50 % of the duration (30 s at most) records `(id, date)` in the
  `skips` table of `library.db` (kept by full synchronisations), to leave these tracks out of the
  mixes for 7 days. Nothing is recorded without a local copy.
- Errors: an unreadable stream is noted in the log and mpv goes on to the next one; a `ping` then
  tests the server. Authentication refused: stop, `auth-refused` marker, notification "Navidrome :
  reconnexion requise" (a click runs `xdg-terminal-exec melodie login`).
- End: queue finished, MPRIS `Quit`, or paused/stopped for 10 minutes: the daemon quits (MPRIS
  service removed, socket deleted). SIGTERM too.
- Log: `~/.local/state/melodie/melodie.log`, overwritten at every start, without token.
- Variables: `MELODIE_AO` sets mpv's audio output (`null` for tests). Reserved for tests:
  `MELODIE_TEST_SECRET` (a file standing for the keyring; with it set, no notification is sent),
  `MELODIE_MPRIS_NAME` (replaces the bus name suffix) and `MELODIE_IDLE_SECS` (the idle delay).

## Favourites and playlists

All these commands write to the server first (Subsonic API), then update the local copy without a
full synchronisation (without a copy, only the server is written). They go through the usual three
connection states (see below), with a notification.

- `melodie like [<kind> <id>]` / `unlike [<kind> <id>]`: `kind` is `track`, `album` or `artist`
  (`star.view` / `unstar.view` with `id`, `albumId`, `artistId`). Without argument, the current
  track (asked of the daemon; when nothing plays: "Rien en lecture", code 1, nothing is sent to
  the server). `like --toggle` flips the state. Notification "♥ Ajouté aux favoris : <title>" or
  "Retiré des favoris". The daemon updates `starred` (`now --json`, MPRIS `melodie:starred`) of the
  current track; on a track change it reads the state again from the copy.
- `melodie playlist add [--allow-duplicates] [--json] <playlistId> [track <id> | album <id> | current]`:
  `updatePlaylist` with `songIdToAdd` repeated (`current`, the current track, is the default).
  The playlist is first read from the server (`getPlaylist`, not the local copy, which holds only
  the counts and may be stale): a track already in it, by id, is skipped, and the output says how
  many ("Ajouté à « X » : 8 morceaux (2 déjà présents, ignorés)", or "Déjà dans « X »" when
  nothing is left to add, code 0, nothing written). `--allow-duplicates` adds them anyway, without
  reading the playlist. A track repeated in the request itself is added once.
- `melodie playlist duplicates [--remove] [--json] <playlistId>`: lists the repeats of a playlist,
  by rank (from 0, like `remove`) and the rank of the occurrence they repeat. Two kinds, told
  apart: *exact* (same track id) and *likely* (another id, same title and artist, ignoring case
  and surrounding spaces; entries without a title are not compared). `--remove` drops the exact
  ones only, keeping the first occurrence and the order of the rest, in one `updatePlaylist`
  whose `songIndexToRemove` are all ranks of the order before the call (Navidrome's reading,
  checked in its source and tested on the fake server). The likely ones are listed and left.
- `melodie playlist remove <playlistId> <index>`: `songIndexToRemove`. The index starts at 0 (like
  the API) and is checked against the server's playlist before writing.
- `melodie playlist create <name> [current | track <id> | album <id>]`,
  `playlist rename <id> <name…>`, `playlist delete <id>` (no confirmation).
  `create --prompt [target]` asks for the name on the keyboard (for `xdg-terminal-exec`).
- `melodie playlist pick [--json] [current | track <id> | album <id>]`: the user's playlists
  (other people's cannot be edited), each with the action `melodie playlist add <pid> <target>`,
  after a "Nouvelle playlist…" entry that runs
  `xdg-terminal-exec melodie playlist create --prompt <target>`.

## Mixes

Computed on `library.db`, without network and without radio, in about twenty milliseconds for
40,000 tracks (loading included). `mix` resolves the tracks in the copy (ids of the copy, no API
call per track) and sends them to the daemon like `play`; the stream address is still built by the
daemon.

Options of `mix`: `--queue` (appends to the queue), `--print` (prints the list without playing),
`--json` (track entries in the launcher format, without playing), `--seed N` (reproducible
result), `--size N` (50 by default).

**The common generator**, from a set of candidates:

1. Left out: tracks rated 1 or 2 stars, skipped (table `skips`) in the last 7 days, and played in
   the last 24 hours as long as enough candidates remain (at least the requested size).
2. Three baskets: A "liked" (favourite, rated 4 or more, or among the 20 % most played of the
   played tracks of the library), B "familiar" (already played or rated 3, outside A), C
   "discovery" (the rest).
3. Shares A/B/C: 40/35/25; 30/30/40 if C holds more than 3 times A + B. An empty or too small
   basket is redistributed to the others in proportion to their shares.
4. Weighted draw without replacement in each basket (more plays, favourite, good rating: more
   weight), a cap of 10 % per artist (it gives way if there are not enough alternatives), then an
   order that never brings two tracks of the same artist closer than 4 positions, nor of the same
   album closer than 6, as long as that is possible.
5. 50 tracks by default.

- `mix me`: the whole library.
- `mix track|album|artist <id or name>`: candidates are the tracks sharing at least one genre with
  the source (the union of the genres of its tracks). The source opens the mix (the track, or the
  liked, best rated or most played track of the album or artist). From an artist, their tracks
  weigh 3 times more and their cap is 30 %. With fewer than 20 candidates by genre: completed
  with the tracks of the same artist, then with the personal mix, up to the requested size.
- `mix genre <name>`: the same mechanism from a genre.
- `mix rediscover`: albums with at least one play but not played for 90 days; 1 to 3 tracks per
  album, spread out.
- `melodie mixes --json`: entries "Mix perso", "Redécouvertes" and "Mix <genre>" for the 6 genres
  most present among liked or frequent tracks. Each entry has the actions "Lire" and "Ajouter à la
  file".

## Files

- `~/.config/melodie/config.toml`: `url` and `user` (nothing secret). `XDG_CONFIG_HOME` is honoured.
- `~/.cache/melodie/library.db`: the local copy (SQLite, FTS5 `unicode61 remove_diacritics 2`),
  `XDG_CACHE_HOME` honoured. Recreated at every full synchronisation (a `.tmp` file, then a
  rename).
- `~/.cache/melodie/covers/`: local covers (512 px, daemon); `covers/thumbs/`: 128 px thumbnails
  for the launcher; `<id>.img.none`: Navidrome's default image (set aside).
- `~/.cache/melodie/auth-refused`: marker of the last refusal by the server (see below).
- `~/.local/state/melodie/state.toml`: remembered volume; `melodie.log`: the daemon's log;
  `sync.log`: the log of background synchronisations. `XDG_STATE_HOME` honoured.
- `$XDG_RUNTIME_DIR/melodie/melodie.sock`, `melodie.lock`: the daemon's socket and lock;
  `sync.lock`: the synchronisation lock.

## Connection states

| State | Cause | Readable output | With `--json` (`search`, `playlists`, `playlist pick`, `playlist add`, `playlist duplicates`, `mixes`, `home`, `mix`) |
| --- | --- | --- | --- |
| `no-login` | no secret or no configuration | "Pas connecté : lance `melodie login`", code 1 | entry "Se connecter à Navidrome" (`xdg-terminal-exec melodie login`), code 0 |
| `auth-refused` | `status="failed"`, Subsonic code 40 to 44 | same (handled like `no-login`), code 1 | same |
| `unreachable` | network, 8 s timeout, HTTP not 200, invalid JSON, other Subsonic error | "Serveur injoignable", code 2, never asks to log in again | entry "Navidrome injoignable" (subtitle: the address), code 0 |

A local problem (missing keyring, disk, database) gives code 3 and a message; with `--json`, an
entry "Erreur du module music". Never a silent error.

`like`, `unlike` and `playlist add|remove|duplicates|create|rename|delete` call the server: same states, same
codes (1, 1, 2), same notifications as `play`. `mix`, `mixes` and `playlist pick` only read the
local copy; `mix` needs the server only to play (offline, `--print` and `--json` work).

`search` and `playlists` query the local copy without network or keyring: the state is therefore
known only from `config.toml` (absent or without a user: `no-login`) and from the `auth-refused`
marker, set by `status`, `sync` or `login` when the server refuses the token, cleared at the first
success. Without a local copy, they start a full synchronisation in the background and answer at
once "Copie de la bibliothèque en cours…" (see "Automatic synchronisation").

## The local copy

- Full sync: `search3` with an empty query, in pages of 500 (artists, albums, tracks),
  `getPlaylists`, `getStarred2`, all in one transaction on a temporary file.
- Incremental sync: `getScanStatus` (`lastScan`) compared with the stored value; unchanged:
  playlists, favourites, play counters (`getAlbumList2` `recent` and `frequent`, 500; the tracks of
  an album whose counters moved are read again with `getAlbum`); changed: full sync.
- Search: FTS5 prefixes ("daf pun" finds "Daft Punk"), case and accent insensitive. Order:
  playlists (5), artists (5), albums (8), tracks (12).

## Automatic synchronisation

The user never has to run `melodie sync`. Every background synchronisation is a detached process
(`melodie sync --background`, new session, inputs and outputs closed) that reads the keyring
itself: no credentials in the arguments or the environment.

- After a successful `login`: a full synchronisation in the background.
- `search`, `home`, `playlists`, `mixes`, `mix`: with a copy older than 1 hour (`synced_at` of
  `meta`), the answer comes at once from the current copy and an incremental synchronisation starts
  in the background. Without a copy: a full synchronisation starts in the background (the caller
  does not wait) and the answer is the entry "Copie de la bibliothèque en cours…" (no action) with
  `--json`, this message on stderr (code 4) in readable output. A synchronisation already running
  is not started again.
- Daemon: incremental at its start if the copy is missing or older than 15 minutes, then every 30
  minutes while it runs (play counters, "recently played" of the mixes).
- One synchronisation at a time (`$XDG_RUNTIME_DIR/melodie/sync.lock`, `flock`); a request made
  while another one runs is ignored (the `sync` command by hand says so).
- Background errors: `~/.local/state/melodie/sync.log` (one line per synchronisation, no token,
  started again from zero beyond 64 KB). An authentication refusal sets the `auth-refused` marker:
  the launcher then shows "Se connecter". No new background synchronisation while the marker is
  there.
- Reserved for tests: `MELODIE_STALE_SECS` (1 h), `MELODIE_DAEMON_FRESH_SECS` (15 min),
  `MELODIE_DAEMON_EVERY_SECS` (30 min).

## JSON results

The `--json` outputs are a contract read by a desktop launcher and a voice assistant: changing a
format is said in the pull request.

One entry per result, in the launcher format: `id` (`<kind>:<id>`), `name`, `subtitle`, `icon`,
`verb: "Play"`, `action` (`melodie play <kind> <id> --shuffle`, without `--shuffle` for a track),
`actions` and, when the cover exists, `icon` = local path of the image (see "Launcher covers").
Each secondary action is `{"label", "icon" (Nerd Font glyph), "action":
{"type":"command","command":…}}`: "Shuffle" then "Dans l'ordre" ("Lire" for a track), "Ajouter à la
file", then, outside playlists, "Mix instantané" (`mix <kind> <id>`) and "J'aime" or "Je n'aime
plus" depending on the local state; for tracks and albums, "Ajouter à une playlist…" whose action
is `{"type":"submenu","command":"melodie playlist pick --json <kind> <id>"}`. The subtitle of a
track is "Track · Artist · Album".

### `playlist add --json` and `playlist duplicates --json`

Unlike the launcher lists above, these two answer one object, with camel-case keys and tracks as
`{"id","title","artist","album"}`:

```json
{"playlist": {"id": "p1", "name": "Daft Mix"},
 "added": [{"id": "t2", "title": "Titre 2", "artist": "Beyoncé", "album": "Discovery"}],
 "already": [{"id": "t1", "title": "Titre 1", "artist": "Daft Punk", "album": "Discovery"}],
 "allowDuplicates": false}
```

`added` is what was sent to the server, `already` what was found in the playlist and left out
(always empty with `--allow-duplicates`, which does not read the playlist). The launcher or the
window can offer to add `already` again with `--allow-duplicates`.

```json
{"playlist": {"id": "p1", "name": "Daft Mix"}, "total": 6,
 "exact":   [{"index": 3, "first": 0, "track": {"id": "t1", "title": "Titre 1", "artist": "Daft Punk", "album": "Discovery"}}],
 "similar": [{"index": 5, "first": 1, "track": {"id": "t9", "title": "Titre 2", "artist": "Beyoncé", "album": "Single"}}],
 "removed": []}
```

`index` is the rank of the repeat, `first` the rank of the occurrence it repeats (from 0).
`removed` is empty without `--remove`, and with it a copy of `exact` (what went).

A failure state gives, as everywhere, a single launcher entry (see the table) and code 0. The
existing outputs, `playlist pick --json` included, are unchanged.

## Launcher covers

For every `--json` output (`search`, `playlists`, `home`, `mixes`, `mix`, `playlist pick`), the
internal cover id (`_cover`) is replaced by `"icon": "<local path>"`: first the thumbnail
`covers/thumbs/<id>.img` (128 px), else the daemon's cover (`covers/<id>.img`). What is missing is
downloaded in parallel (8 requests, `getCoverArt` 128) within a total budget of 250 ms; beyond
that, the entry keeps its glyph and the rest goes to a detached process (`melodie cover-fetch`,
new session, hidden subcommand) for the next keystroke. The ids reach it on its standard input and
it reads the keyring itself: no token in the arguments, the environment or the output. One
`cover-fetch` at a time (lock `covers/.fetch.lock`). The keyring is read only if a thumbnail is
missing. Navidrome's default image (md5 `fcec14e686ad5159a8ed7c51936116d7`) is never shown: a
`.none` marker avoids downloading it again.

## Security

The token appears in no message, log or output: `ureq` errors (which contain the URL) are never
printed, only the method and the kind of error are.
