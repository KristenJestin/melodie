# Mélodie

A music player for a Navidrome server (Subsonic / OpenSubsonic API, tried on Navidrome 0.63) on
Linux (Wayland), written in Rust: a background daemon, a command line, a local copy of the library
and mixes computed locally, and a window (`melodie-desktop`), one more client of the same daemon.

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
- `melodie-desktop`: the window (gpui-kit on GPUI Fast, see [docs/gui-toolkit.md](docs/gui-toolkit.md)).
  A program of its own, so that the command line stays small and quick to start and does not pay
  for the window's build. Its first build is long (several minutes); without the default
  `gpui-fast` feature it builds on upstream GPUI from the same source.

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
  `melodie login --password-stdin --server=<address> --user=<name>` asks nothing: it reads the
  password from its standard input (one line), never from its command line, which every process
  can read. This is how the window signs in. The same exit codes as the other commands: 0 signed
  in, 1 refused, 2 unreachable.
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
- `melodie queue [--next] <kind> <id>`: adds to the queue, at the end or, with `--next`, right
  after the current track (starts playing if nothing plays).
- `melodie open <address>`: plays or queues what an address names (see "Addresses"). Same checks
  as `play`, same daemon start.
- `melodie queue --list [--json] | --play <pos> | --move <from> <to> | --remove <pos> |
  --clear-after`: shows the queue (the current track marked `▶`) and edits it; positions start
  at 1. Like the controls, they never start the daemon; without one, `--list` shows an empty
  queue (`{"current":null,"tracks":[]}`). See [docs/events.md](docs/events.md) for what happens to
  the current track.
- `melodie watch`: follows what the player does, one JSON event per line (state, track,
  position, volume, queue, favourites, connection). Never starts the daemon; without one, prints
  `{"type":"gone"}` and waits for it. The format is in [docs/events.md](docs/events.md).
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

`play`, `queue` and `open` start the daemon if it is not running, but only if the connection is good:
`no-login`, `auth-refused` and `unreachable` give a clear message (code 1, 1 or 2) and a
`notify-send -a "Mélodie"` notification, without starting the daemon. An id the server does not
have is "not found" (code 5, see "Connection states"), also without starting the daemon.

## The window

`melodie-desktop` opens the library, the player and its queue. It needs a Wayland or X11 session with a Vulkan
driver, and the `melodie` program beside it (same folder, as a build leaves them) or on the
`PATH`.

Screenshots and a recording of the animations, made with invented data, are in
[docs/window](docs/window).

- **A client, nothing more.** It follows the daemon through the event stream
  ([docs/events.md](docs/events.md)) and sends its requests on the socket; it never plays sound,
  never reads the keyring and never holds a token or a stream address. Closing it never stops the
  music; opening it shows the current state at once (the snapshot). Without a daemon it says
  "Nothing playing" and looks for one every half second.
- **The player**: the cover, large; title, artist, album; the quality of the stream; a position
  bar to drag; play/pause, previous, next (play/pause on the axis of the cover, the other buttons
  placed with the same room on each side); the volume; the heart. Play without a daemon starts the
  personal mix (`melodie mix me`, which starts the daemon); the heart runs `melodie like track
  <id>` or `unlike`: what needs the server goes through `melodie`. A failure is said at the
  bottom of the window for a few seconds, and so is an `unreachable` server.
- **The queue**, shown or hidden by its button (in the player and at the bottom of the sidebar)
  or the `q` key: a column on the right of the window, beside the pages and the player, which
  narrow to make room; nothing is covered. Below 1160 pixels of width (the sidebar's 220, the
  queue's 380 and 560 for the main area) it slides over the main area instead, and Escape or a
  click beside it hides it. Shown or hidden is kept between launches. Past tracks greyed, the
  current one marked, the next ones. A click plays a track; drag a track onto another to move it
  there; the cross on a row removes it; the button at the top removes what comes after the
  current track.
- **The library**, in the sidebar: Home, Albums, Artists, Tracks, and the playlists. Everything is
  read from the local copy, away from the window's thread: browsing never touches the network.
  - Home: a greeting by the time of day, then a large card for something worth playing now (the
    album added last, the personal mix, the album played last, the album played most), in the
    colours of its cover, browsed with its arrows or its dots. Under it, rows that scroll sideways
    (shift and the wheel, a touchpad, or a drag; the wheel alone scrolls the page): the albums
    added last, the mixes as coloured cards (the personal mix, the rediscoveries, and a mix for
    each of the six genres `melodie mixes` offers, found by the same code), the albums played
    most, the albums played last, the artists played most (in circles), the playlists. Each row
    holds 20 at most; "See all" opens the albums in that order, or the artists. "Played last"
    is the server's, as of the last copy.
  - Albums: a grid of covers, sorted by date added, name, artist, year, plays or last played (a
    second click on the sort turns it round). Artists and Tracks: long lists sorted by a click on a column.
  - A page for an album (its tracks; play, shuffle, play next, add to the queue, instant mix, add
    to a playlist), an artist (its top tracks by plays, its albums; play, shuffle, instant mix)
    and a playlist (its tracks, as the local copy holds them; play, shuffle, play next, add to the
    queue). A click on a track plays it; on a row's hover, play it next, add it to the queue, add
    it to a playlist, or start an instant mix from it. The back arrow (or Escape) goes back to where
    the page was opened from.
  - **Playlists**: the + beside "Playlists" in the sidebar creates one. The page of one of the
    user's playlists (other people's are only read) also renames it, deletes it (after a
    confirmation that says what goes), and finds its duplicates: they are listed (the exact ones,
    which go, and the likely ones, which stay) before "Remove" removes the exact ones shown, by
    rank, each checked to still hold the track shown (`playlist remove --expect`). On its rows,
    a button removes the track; a row dragged onto another moves it there.
  - **Picking rows**: Ctrl+click picks a track, Shift+click the tracks between the last one
    picked and this one (in Tracks, an album, a playlist). A bar then adds them to a playlist, or
    removes them from the user's playlist shown (after a confirmation naming them).
  - **Adding to a playlist** (a track, the rows picked, an album, the current track from the
    player): a dialog lists the user's playlists and "New playlist…". The playlist is read first
    (`playlist add --dry-run`): when nothing is there yet, it is added at once; otherwise the window
    asks, naming the tracks already there: skip them (add the others, or nothing when all are
    there) or add them anyway.
  - Actions run `melodie` (`play`, `queue`, `mix`, `playlist add|create|rename|delete|remove|move|
    duplicates`): what needs the server still goes through it, and the window reads the local copy
    again when it is done. The jobs of one playlist run one after the other, in the order asked. Covers come from `~/.cache/melodie/covers/` (the 128 px thumbnails
    first); the missing ones are fetched by `melodie cover-fetch`, in the background, by one process
    at a time fed from a queue of missing ids, for every place that shows a cover (grids, pages,
    search, the queue, the player, the bottom bar). An id is asked again until it is on disk, is
    the server's default image (the glyph stays) or came back missing three times from a finished
    fetch; a fetch that found the lock taken costs no try. What lands shows without a restart.
  - At the bottom, what plays: play or pause, next; a click opens the player.
- **The search**, first in the sidebar: one field, and as you type the artists, albums, tracks and
  playlists of the local copy, grouped by kind. The kind whose best result is the most relevant
  comes first (at equal relevance: artists, albums, tracks, playlists), each with its five best
  results, ranked as `melodie search` ranks them; "See all" lists every result of that kind. A
  click on a track plays it, on the rest opens its page; on a row's hover: play, shuffle (not for a
  single track), play next, add to the queue, open the album (a track), add to a playlist (a track
  or an album). The local copy is read away from the window's thread, and the answer to an older
  keystroke is never shown over a newer one; the search never touches the network.
- **Keys**: Ctrl+F, or / when not typing, opens the search and puts the cursor in its field;
  Space plays or pauses (in the field, it is a space); `q` shows or hides the queue (in the field,
  it is a letter); Escape closes a dialog, else the queue laid over a narrow window or the
  settings, else unpicks the rows picked, else empties the search field, else goes back. In a
  dialog, Enter does what its main button does (Escape cancels).
- **The mouse's back and forward buttons** go back and forward between the pages, like the
  browser's: back goes back a page (it closes the queue laid over a narrow window or the settings
  first), forward returns to the page back left, and does nothing when there is none (a page
  opened after going back forgets it). They do nothing behind a dialog or the sign-in screen.
- **Tooltips**: every button that shows an icon alone says what it does when the pointer rests on
  it for about 0.4 s: a short fade and rise, beside the button, never over it. They go at a press
  or a key.
- **Signing in**: with no login, or when the server refused it (`no-login`, `auth-refused`), the
  window shows a sign-in screen: the server's address, the user, the password. It runs `melodie
  login --password-stdin`, the password written on its input: the window keeps no password and no
  token, and empties the password field (its undo history too) as the attempt starts. A refused
  password and an unreachable server are told apart. An unreachable server never brings the
  sign-in screen: the window says "Navidrome is unreachable" and shows the local copy. At start,
  with a login kept, the window asks `melodie status` whether it still holds.
- **Settings** (the button at the bottom of the sidebar): the theme; the server, the user and the
  connection state, with "Sign out" (`melodie logout`: the address stays for the next sign-in);
  when the local copy was last made, with "Sync now" (`melodie sync`). The library is read again
  when a new copy is there, the first one after a sign-in included.
- **Colours**: the button at the top right chooses the theme. "Mélodie", the base theme, takes its
  colours from the cover and follows the desktop's dark or light setting. "Omarchy" is offered
  only on Omarchy (`ID=omarchy` in `/etc/os-release` and a readable
  `$XDG_STATE_HOME/omarchy/current/theme/colors.toml`); it follows a theme change live, and falls
  back to the base theme, saying so, while the theme file is missing. Before any choice, the
  window uses Omarchy when it is offered, else the base theme. The choice is kept in
  `~/.local/state/melodie/window.toml` (`theme = "base"` or `"omarchy"`), a file of the window's
  own, beside `queue = true` while the queue is shown.
- **Animations**: every change of view, panel or state is short and interruptible, and follows
  the desktop's reduced-motion setting. An unfocused window keeps animating at the screen's rate.
- Measuring: built with the `frame-stats` feature and run with `MELODIE_DESKTOP_FRAME_STATS=<file>`,
  the window writes the time of each frame and how long it took to draw, and says on its error
  output how long each view took to read the local copy (for measurements only).

## Addresses

The standard way to say "play this", for any program: a `melodie:` address, given to
`melodie open <address>` or to MPRIS `OpenUri`.

```
melodie:<kind>/<id>[?<option>=<value>[&<option>=<value>]]
```

- `kind`: `track`, `album`, `artist` or `playlist` (lowercase). `id`: the server's id, as the
  Subsonic API gives it (`melodie:album/4Zq0Aw-9`). What `play <kind> <id>` plays, `open` plays:
  an album in the server's order, an artist's albums by year then name, a playlist in its order.
- `shuffle=true|false`: shuffles what is played or added. Default `false`.
- `queue=next|last`: adds to the queue instead of replacing it, right after the current track
  (`next`) or at the end (`last`); starts playing if nothing plays. Without it, the address
  replaces the queue and plays. With `shuffle=true`, the added tracks are shuffled among
  themselves, the rest of the queue stays as it is.

```
melodie open 'melodie:album/4Zq0Aw-9'
melodie open 'melodie:playlist/p1?shuffle=true'
melodie open 'melodie:track/9fQ2?queue=next'
gdbus call --session --dest org.mpris.MediaPlayer2.melodie \
  --object-path /org/mpris/MediaPlayer2 \
  --method org.mpris.MediaPlayer2.Player.OpenUri 'melodie:artist/ar1?queue=last'
```

An address comes from outside, so it is read strictly and refused with a message that says why
(`invalid address: …`) when it is not exactly in this grammar. Nothing is guessed:

- the scheme is `melodie` (any case); the kind and the options are case-sensitive; an unknown
  kind, an unknown or repeated option, an option without a value, a value other than the ones
  above, a fragment (`#…`) and a missing id are refused;
- the id is made of ASCII letters, digits, `-`, `_` and `.`, 128 characters at most. Percent
  encoding (`%2D`) is accepted and undone once, then the result is checked like any id: `%2F` or
  `%2541` do not get through. An id is never put in a URL unescaped;
- a refused address changes nothing: `melodie open` exits with code 1 before it reads the keyring,
  asks the server or starts the daemon; `OpenUri` answers `org.freedesktop.DBus.Error.InvalidArgs`
  and the queue is untouched.

`melodie open` checks the connection like `play` does: `no-login`, `auth-refused` (both code 1) and
`unreachable` (code 2) are told apart, with a notification, and the daemon is not started; an id
the server does not know is "not found" (code 5), not a connection state. It has
no `--json`: like `play`, it prints one line ("Lecture : 3 morceaux") and acts.

`OpenUri` runs in the daemon and does not wait for the server: the address is read on the D-Bus
thread (so a malformed one is an error for the caller), then handed to a thread of the daemon that
asks the API for the tracks, one address after the other and in the order received, and gives the
queue request to the player. So the call returns before the music changes, and a valid address
that the server cannot serve (unknown id, server down) is not an error of the call: it is written
in the log (`OpenUri <kind> <id> : …`, without the server's address) and the queue stays as it was.
A refused token is handled as for a stream the server refused: `auth-refused` marker, notification,
the daemon stops.

Mélodie does not register itself as the handler of `melodie:` links: it has no installer, and a
`.desktop` file written into the user's folders by the program would be a side effect nobody asked
for. A desktop that wants `xdg-open melodie:album/…` to work adds the entry itself:

```
[Desktop Entry]
Type=Application
Name=Mélodie (addresses)
Exec=melodie open %u
MimeType=x-scheme-handler/melodie;
NoDisplay=true
```

then `xdg-mime default <that file>.desktop x-scheme-handler/melodie`. The address survives the
trip through `xdg-open` and `%u` as it is (no `//` is needed after `melodie:`).

## The daemon

`melodie daemon` (a hidden subcommand) is started detached (new session) by `play`, `queue` or `open`.
Only one at a time (lock `$XDG_RUNTIME_DIR/melodie/melodie.lock`). It holds libmpv (crate
`libmpv2`, default `ao`, so PipeWire), plays the streams
`<server>/rest/stream.view?id=…&format=raw&…` handed to mpv in memory (never written, logged or
exposed), and keeps the queue in step with mpv's playlist. The daemon does not load mpv scripts
(`load-scripts=no`): it has no window, terminal or key bindings for them.

- Control: Unix socket `$XDG_RUNTIME_DIR/melodie/melodie.sock` (0600, folder 0700), one JSON
  request per line (`{"cmd":"next"}`), one JSON answer per line (`{"ok":true}` or
  `{"ok":false,"error":"…"}`; `now` adds the `now` object). The queue requests and the event
  stream (`{"cmd":"watch"}`) are described in [docs/events.md](docs/events.md).
- MPRIS 2: `org.mpris.MediaPlayer2.melodie`, identity "Mélodie", no DesktopEntry. Metadata:
  `mpris:trackid` (`/org/melodie/track/<n>`), `mpris:length`, `xesam:title`, `xesam:artist`,
  `xesam:album`, `xesam:albumArtist`, `mpris:artUrl` (local cover), `xesam:comment`,
  `melodie:starred` (boolean, the track is a favourite) and `melodie:quality` (quality, for example
  "FLAC · 24 bit / 96 kHz · 2304 kbps", "MP3 · 320 kbps"). No `xesam:url`. `PropertiesChanged` on
  every change of track, state or volume; `Seeked` on a jump. `OpenUri` takes a `melodie:`
  address (see "Addresses"), and `SupportedUriSchemes` lists `melodie`. `SupportedMimeTypes`
  stays empty: no file is opened, only addresses.
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
  service removed, socket deleted). SIGTERM too. Clients following the event stream do not keep
  it alive.
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
- `melodie playlist add [--allow-duplicates | --dry-run] [--json] <playlistId> [track <id>… | album <id> | current]`:
  `updatePlaylist` with `songIdToAdd` repeated (`current`, the current track, is the default).
  The playlist is first read from the server (`getPlaylist`, not the local copy, which may be
  stale): a track already in it, by id, is skipped, and the output says how
  many ("Ajouté à « X » : 8 morceaux (2 déjà présents, ignorés)", or "Déjà dans « X »" when
  nothing is left to add, code 0, nothing written). `--allow-duplicates` adds them anyway, without
  reading the playlist. A track repeated in the request itself is added once. `track` takes one
  id or several (a selection), added in that order. `--dry-run` reads the playlist and says what
  would be added and what is already there, without writing anything (the window asks its
  question from it).
- `melodie playlist duplicates [--remove] [--json] <playlistId>`: lists the repeats of a playlist,
  by rank and the rank of the occurrence they repeat. The text output counts ranks from 1; the
  `--json` `index` and `first` count from 0, like Subsonic's `songIndexToRemove` (unlike `remove`,
  which takes the rank printed in the text output). Two kinds, told apart: *exact* (same track id) and *likely* (another id,
  same title and artist, ignoring case
  and surrounding spaces; entries without a title are not compared). `--remove` drops the exact
  ones only, keeping the first occurrence and the order of the rest, in one `updatePlaylist`
  whose `songIndexToRemove` are all ranks of the order before the call (Navidrome's reading,
  checked in its source and tested on the fake server). The likely ones are listed and left.
- `melodie playlist remove [--expect <id>]… <playlistId> <rank>…`: removes the tracks at these
  ranks, counted from 1 like what `duplicates` prints (rank 0 is refused). They are converted once
  to Subsonic's 0-based `songIndexToRemove`, checked against the server's playlist and sent in one
  `updatePlaylist` (every index refers to the order before the call). With `--expect`, given once
  per rank, each rank must still hold that track id, or nothing is removed: a window shows the
  playlist as it last read it.
- `melodie playlist move [--expect <id>] <playlistId> <from> <to>`: moves the track at rank
  `from` to rank `to` (from 1). Subsonic has no move: the whole order is written again by
  `createPlaylist` with the `playlistId` (Navidrome replaces the tracks and keeps the playlist,
  its name and its owner). `--expect` as for `remove`. Nothing is written when the playlist changed
  between the read the new order comes from and the write (it is read again just before), or when
  the server lists fewer tracks than its `songCount` (Navidrome leaves out the tracks whose file is
  missing, and rewriting the playlist would drop them).
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
- `~/.local/state/melodie/window.toml`: the window's theme choice, and whether the queue is shown.
- `~/.local/state/melodie/state.toml`: remembered volume; `melodie.log`: the daemon's log;
  `sync.log`: the log of background synchronisations. `XDG_STATE_HOME` honoured.
- `$XDG_RUNTIME_DIR/melodie/melodie.sock`, `melodie.lock`: the daemon's socket and lock;
  `sync.lock`: the synchronisation lock.

## Connection states

| State | Cause | Readable output | With `--json` (`search`, `playlists`, `playlist pick`, `playlist add`, `playlist duplicates`, `mixes`, `home`, `mix`) |
| --- | --- | --- | --- |
| `no-login` | no secret or no configuration | "Pas connecté : lance `melodie login`", code 1 | entry "Se connecter à Navidrome" (`xdg-terminal-exec melodie login`), code 0 |
| `auth-refused` | `status="failed"`, Subsonic code 40 to 44 | same (handled like `no-login`), code 1 | same |
| `unreachable` | network, 8 s timeout, HTTP 502 to 504 (a proxy without its server), invalid JSON, other Subsonic error | "Serveur injoignable", code 2, never asks to log in again | entry "Navidrome injoignable" (subtitle: the address), code 0 |

**An HTTP refusal is not a connection state either.** Any other HTTP answer than 200 (a 414
from a proxy, a 500 from the server) means the server answered: it is reported as an error (code
3, entry `error`), never as `unreachable`. The playlist writes that carry a list of tracks
(`playlist add`, `remove`, `move`, `create`, `duplicates --remove`) post their parameters as a
form (OpenSubsonic `formPost`), so a long playlist does not make a URL too long for a proxy.

**Not found is not a connection state.** When the server answers Subsonic code 70 ("data not
found", e.g. an album id it does not have), it is reachable and accepted the token: the item is
missing. Every command that asks the server about an id (`play`, `queue`, `open`, `like`,
`unlike`, `playlist …`) says "Introuvable sur le serveur : album al9" (for the ones that look an
id up, else the name of the failed request) with code 5 and a notification, and never asks to log
in. The daemon is not started for it. It is never
remembered: it neither sets nor clears the `auth-refused` marker, and the daemon's `connection`
(`docs/events.md`) does not change. With `--json` it is one entry
`{"id": "not-found", "name": "Introuvable sur le serveur", "subtitle": "album al9", "icon": …}`
with no action, code 0, like the other failure entries. MPRIS `OpenUri` of such an address logs it
and changes nothing.

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

- Full sync: `search3` with an empty query, in pages of 500 (artists, albums with the date each
  was added, tracks), `getPlaylists` and the tracks of each playlist (`getPlaylist`), `getStarred2`,
  all in one transaction on a temporary file.
- Incremental sync: `getScanStatus` (`lastScan`) compared with the stored value; unchanged:
  playlists (the tracks of a playlist are read again only when its `changed` date moved),
  favourites, play counters (`getAlbumList2` `recent` and `frequent`, 500; the tracks of an album
  whose counters moved are read again with `getAlbum`); changed: full sync. A copy made by an older
  version gets the new columns and tables there; the date an album was added comes with the next
  full sync (until then, those albums come last in "recently added").
- `melodie playlist add`, `remove`, `duplicates --remove` and `create` change the tracks of the
  playlist in the copy too, as the server did.
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
(always empty with `--allow-duplicates`, which does not read the playlist). With `--dry-run`,
nothing is sent: `added` is what would be, and the object has one more key, `"dryRun": true`. The launcher or the
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
`cover-fetch` at a time (lock `covers/.fetch.lock`); one that finds the lock taken exits at once
with code 75 and downloads nothing (the launcher ignores the code, the window asks again). The keyring is read only if a thumbnail is
missing. Navidrome's default image (md5 `fcec14e686ad5159a8ed7c51936116d7`) is never shown: a
`.none` marker avoids downloading it again.

## Security

The token appears in no message, log or output: `ureq` errors (which contain the URL) are never
printed, only the method and the kind of error are.
