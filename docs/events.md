# The queue and the event stream

The daemon's socket (`$XDG_RUNTIME_DIR/melodie/melodie.sock`) speaks one JSON object per line,
UTF-8, ended by `\n`. A request is an object with a `cmd` field; the answer is one line
`{"ok":true,…}` or `{"ok":false,"error":"…"}`. This page is the contract for the clients that
show and follow the player: the window, `melodie watch`, a launcher. Clients **ignore the event
types and the fields they do not know**: new ones are added without notice, existing ones keep
their meaning.

## The queue

Positions on the socket start at **0**. (On the command line they start at 1, and so does the
older `index` of `now`.)

| Request | Effect | Answer |
|---|---|---|
| `{"cmd":"queue_list"}` | nothing | `{"ok":true,"queue":<queue>}` |
| `{"cmd":"queue_play","index":2}` | plays the track at `index` (from the start if it is the current one) | `{"ok":true}` |
| `{"cmd":"queue_move","from":4,"to":1}` | moves a track so that it ends up at `to` | `{"ok":true}` |
| `{"cmd":"queue_remove","index":0}` | removes a track | `{"ok":true}` |
| `{"cmd":"queue_clear_after"}` | removes every track after the current one | `{"ok":true}` |
| `{"cmd":"queue_shuffle_after"}` | shuffles, once and in place, the tracks after the current one | `{"ok":true}` |
| `{"cmd":"queue","tracks":[…],"next":true}` | adds tracks right after the current one (`next`) or at the end (`next` false or absent); plays the first of them if nothing plays | `{"ok":true}` |
| `{"cmd":"play","tracks":[…],"shuffle":false,"start":2}` | replaces the queue and plays it from the track at `start` (from 0, 0 when absent; ignored with `shuffle`): the tracks before it never start | `{"ok":true}` |

A position outside the queue is refused (`ok` false), never guessed.

What happens to the current track:

- Moving the current track keeps it playing: only its index changes.
- Moving or removing a track before the current one shifts the current index.
- Removing the current track plays the next one. If it was the last, the player stops and the
  rest of the queue stays: `current` is null, and `queue_play` starts again from any track. The
  daemon then quits after its idle delay, like any stop.
- `queue_clear_after` keeps what was played and the current track.
- `queue_shuffle_after` keeps what was played and the current track (not reloaded, same place in
  the song) and reorders only the tracks after it, duplicates included. It is a one-shot action,
  not a mode: what is added afterwards goes where it is put. With fewer than two tracks after the
  current one it changes nothing; with no current track it is refused. The watchers get the new
  queue as one `queue` event.

`<queue>` is:

```json
{"current": 1,
 "tracks": [{"entry": 7, "id": "t1", "title": "A Title", "artist": "An Artist",
             "album": "An Album", "album_artist": "An Artist", "duration": 225.0,
             "quality": "FLAC · 16 bit / 44.1 kHz", "cover": "/home/user/.cache/melodie/covers/c1.img",
             "cover_id": "c1", "starred": false}]}
```

- `current`: index of the current track, or null when nothing is current.
- `entry`: a number unique for the daemon's life, kept when the track moves. The same track
  queued twice gives two entries; use `entry` to tell rows apart and to animate a move.
- `duration` in seconds; `cover` is the local cover file once it is downloaded, else null;
  `cover_id` is the server's cover id. The daemon downloads the cover of the current track only;
  the others are there if something else (the launcher, an earlier play) fetched them. A queue
  never holds a stream address.

## The event stream

Send `{"cmd":"watch"}`. The daemon answers with no `ok` line: from then on, the connection
carries events only, one per line, each with a `type` field. Anything else the client writes on
this connection is ignored; use another connection for requests.

### On connect: the snapshot

The first event is always the snapshot, everything a client needs to start from:

```json
{"type":"snapshot","now":<now>,"queue":<queue>,"connection":"ok"}
```

`<now>` is the object of `melodie now --json` (state, track, position, volume…). Then the client
applies the events that follow, in order. Every event carries the **new value**, never a
difference: applying one twice is harmless.

### Event types

| `type` | Fields | When |
|---|---|---|
| `snapshot` | `now`, `queue`, `connection` | first, once per connection |
| `queue` | `queue` (`<queue>`) | the tracks of the queue changed (added, moved, removed, replaced, cleared), or a move changed the current index |
| `track` | `index` (number or null), `track` (a queue entry or null) | another track became current, or none; sent again when the cover of the current track has been downloaded (`cover` was null) |
| `state` | `state` (`playing`, `paused`, `stopped`), `position` (seconds) | playing, paused or stopped |
| `position` | `position`, `duration` (seconds) | while playing, at most once a second; never while paused or stopped |
| `volume` | `volume` (0 to 1.3; 1 = 100 %) | the volume changed (command line, MPRIS, socket) |
| `starred` | `id`, `starred` | a track became a favourite or stopped being one; update every entry with this `id` |
| `connection` | `state` (`ok`, `unreachable`, `auth-refused`) | what the last call to the server says changed; after `auth-refused` the daemon stops and quits. An id the server does not know (Subsonic code 70) is not a connection change: no event |

The current index is the one of the latest `queue` or `track` event. Playing another position
is a `track` event, not a `queue` event.

### Order

- Events come in the order the daemon made the changes; one thread makes them all.
- Within one change, `queue` comes before `track`, then `state`, `volume`, `connection`: a
  client knows the tracks before it hears which one is current.
- The events of a request are queued before the answer to that request is sent. Answers and
  events travel on different connections, so a client must not expect them to interleave in any
  given way.

### A slow client

Each client has its own buffer (64 events, plus the socket's). A client that stops reading
never holds up the player or the other clients: when its buffer is full, the daemon closes its
connection. It reconnects and starts again from a snapshot.

### When the daemon goes away

The connection ends (end of file), whether the daemon quit or crashed; when it quits, it
leaves its clients up to one second to receive the last events. A client then waits for the
socket to come back and subscribes again.

The daemon quits after ten minutes without music (paused or stopped), **whether or not clients
are watching**: a window or `melodie watch` only follows the player, it never keeps it alive.
The daemon exists to make sound; a client that wants music starts it with `play`.

## `melodie watch`

Prints the stream as it comes, one JSON object per line, unchanged. It never starts the daemon.
When there is no daemon, at start or when the connection ends, it prints once

```json
{"type":"gone"}
```

and waits, looking for the daemon every half second; when one starts, it subscribes again and
prints a new snapshot. It stops on Ctrl-C or when its output is closed.
