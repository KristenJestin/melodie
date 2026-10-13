# AGENTS.md — Mélodie

Instructions for any coding agent working in this repository. `CLAUDE.md` is a symbolic link
to this file: there is one contract, not two that drift apart.

## Project overview

Mélodie is a music player for a Navidrome server (Subsonic / OpenSubsonic API), for Linux
(Wayland), written in Rust. It started as the `music` module of the maintainer's desktop
repository and lives here on its own.

- **A local copy of the library** (SQLite) makes search instant and feeds the mixes. It is
  synchronised in the background; a failed full copy keeps the previous one.
- **A daemon** plays: libmpv for the sound, MPRIS for the desktop, a Unix socket for control,
  plays reported to the server (scrobble), skips remembered. It starts on the first `play` and
  quits after ten minutes without music. It is the only process that makes sound.
- **The command line** (`melodie …`) drives everything: login, sync, search, play, queue,
  controls, likes, playlists, mixes. Its `--json` outputs are a contract read by a desktop
  launcher and a voice assistant.
- **The mixes** are computed locally, following Symfonium's method: a personal mix balancing
  favourites, familiar tracks and discovery, rediscoveries, and instant mixes from a track,
  album, artist or genre; artists and albums are spread out; recently skipped tracks are left
  out. There is no radio, on purpose.
- **A window** (to come) is one more client of the same daemon and the same local copy:
  closing it never stops the music.

Mélodie is used alone. The maintainer's desktop integration (launcher entries, the player
card, keys) lives in its own repository; nothing of it lives here.

## Commands

```
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
tools/commit-message.sh --range origin/dev..HEAD
git config core.hooksPath .githooks     # once per clone: the commit-msg and pre-commit hooks
```

## Principles

### 1. Think before coding

**Don't assume. Don't hide confusion. Surface tradeoffs.**

- State your assumptions explicitly. If uncertain, ask.
- If multiple interpretations exist, present them; don't pick silently.
- If a simpler approach exists, say so. Push back when warranted.
- If the code and the issue disagree, the issue wins; if the issue is wrong, say so instead of
  quietly deviating.

### 2. Simplicity first

**Minimum code that solves the problem. Nothing speculative.**

- No features beyond what the task asks. No abstractions for single-use code.
- No configurability that was not requested.
- No error handling for impossible scenarios. If 200 lines could be 50, rewrite.

### 3. Surgical changes

**Touch only what you must. Clean up only your own mess.**

- Don't "improve" adjacent code, comments or formatting. Don't refactor what isn't broken.
- Match existing style. If you notice unrelated dead code, mention it, don't delete it.
- Every changed line must trace directly to the task.

### 4. The failing test first

**Define success criteria. Loop until verified.**

- Write the test that fails first, see it fail, then make it pass.
- "Fix the bug" → a test that reproduces it, then the fix.
- Every scenario of the issue gets a test named after it. A scenario without a test is a
  defect, not a TODO.

## Code style

- Rust 2024, `cargo fmt`, `cargo clippy -- -D warnings` clean.
- Code, identifiers, comments, docs, issues and commits are in English. The code brought over
  from the maintainer's desktop repository has French comments and messages: translate what you
  touch, not the rest.
- No tokio, no libdbus: D-Bus and the Secret Service go through `zbus` (pure Rust).
- A comment says why, not what.

## Secrets (non-negotiable)

- The password exists only inside `login`, long enough to compute the Subsonic token: no
  structure, file or log keeps it. The token and the salt live only in the keyring (Secret
  Service) and in the request URL; never in a message, a `Display`, a `Debug`, a panic or an
  output.
- Never print a `ureq` error as is (it holds the URL): go through `ApiError`.
- A stream URL (token included) exists only in `api::SecretUrl` (`Display` and `Debug` masked)
  and in mpv: never in a log, MPRIS or a socket answer.
- Never look for credentials outside `login` (no reading of other applications' configuration).

## Connection states

- Three states, always told apart: `no-login`, `auth-refused` (Subsonic codes 40 to 44, handled
  like `no-login`), `unreachable` (never ask to log in again). No silent error.
- In `--json`, a failure state gives a single launcher entry and exit code 0.
- `search` touches neither the network nor the keyring when the local copy exists (target:
  under 30 ms).

## Testing

- **Tests never touch the real server, real sound, the real session bus, the real keyring or
  the real configuration.** They talk to the fake local server (`fake`), play with the null
  audio output, run their own private `dbus-daemon`, and use temporary folders.
- For a manual try, use `dbus-run-session -- <command>` and a temporary configuration; stop
  every test daemon you started.
- The test-only environment variables (fake keyring file, MPRIS name, idle delay, audio output)
  are documented in the README and used by tests only.
- What only a real machine can show (sound, the desktop's player card, smoothness of a long
  list) is checked by hand and reported in the pull request.
- A test that fails only under load is rerun alone before the failure is called real.

## How the work is organised

| Who | What |
|---|---|
| The maintainer | decides what is built, tries each tranche on their machine, and merges what they have not delegated |
| The lead agent | turns the maintainer's decisions into issues, launches the developing agents, reviews their pull requests, keeps the tracker honest, and never hides a failure |
| Developing agents | implement one issue each: the failing test first, then the code, the verification and a pull request; they never merge |
| CI | `verify`, `commit-messages` and `shape` on every pull request |

### The tracker

- **Issues are the tickets, and a ticket stands alone.** It holds the what, the where, the "how
  to verify" and the base branch, with no local path. If it lacks one of them, ask rather than
  guess.
- The plan is cut into **tranches**, labels `tranche:1`, `tranche:2`…, beside `type:*` and
  `area:*`. A tranche ends on something the maintainer can use, and they accept it on their
  machine.
- **`dev` is the integration branch** and the default branch: a branch starts from
  `origin/dev` and its pull request targets it. `main` holds released versions.

## Git and pull requests (non-negotiable)

- **Never commit, merge, rebase, push or force-push on `main` or `dev` directly.** If you are
  on one of these branches, create a branch first (`feat/<n>-<topic>`, `fix/<n>-<topic>`). The
  `pre-commit` hook refuses it.
- Never rewrite published history. No `--no-verify`.
- One commit = one intent. No `wip` commits. Don't mix formatting and logic in one commit.
- Commits use the Git user configured on the machine.
- The subject is `<type>(<scope>): <subject>`: the **scope is required**, the subject is 72
  characters at most, starts lowercase, has no final period, and the type is one of feat, fix,
  refactor, test, docs, chore, build, ci, perf. `tools/commit-message.sh` is the judge (the
  `commit-msg` hook and CI run it).
- Scopes: `library`, `api`, `sync`, `search`, `mix`, `playlists`, `daemon`, `cli`, `migrate`,
  `desktop`, `repo`, `ci`.
- **Pull requests are merged by squash only.** The title is a plain Angular subject. The
  description is written in the first person as the maintainer and ends with `Closes #<n>`.

## Security and privacy

- **This repository is public.** Nothing private goes into it, an issue, a pull request or a
  commit: no server address, user name, library content, listening history, private path or
  host. Examples use invented data.
- Secrets are never printed, logged or committed.
- Nothing is published outside the pull request: no gist, no upload, no paste service.
- **A permission denial is never worked around**: do what remains, say what was refused.

## Working on a machine

- The machine is the maintainer's own, in use while you work, and **music may be playing**.
  Never stop, restart, signal or replace their running player, never use its socket or its
  MPRIS name, never touch their configuration, local copy or keyring entry. Run your own
  instance with a temporary configuration, runtime folder and MPRIS name.
- Never play sound on the real output unless the issue asks for a listening check and the
  maintainer agreed.
- No artificial load: no stress runs, no parallel benchmark loops. Heavy builds run at low
  priority (`nice -n 19`), one at a time.
- Never send requests to the maintainer's server from tests; a manual check against it is read
  only unless the issue says otherwise.
- Worktrees, logs and every other scratch file live outside the clone.
- Before removing a worktree, check it holds no uncommitted change and no unpushed commit
  (`git log HEAD --not --remotes`).

## GitHub gotchas

- Prefer REST (`gh api repos/…`) to GraphQL, which gets rate-limited.
- A body holding backticks or apostrophes is written to a file and passed with `--body-file`.

## When done

Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test`, plus the
manual checks the issue asks for, and report the real output. If something fails, say so;
don't claim green.
