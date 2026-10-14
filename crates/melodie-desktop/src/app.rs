//! The state every view reads, and what keeps it in step: the daemon's stream, the covers,
//! Omarchy's theme, the window's appearance.

use crate::cover_cache::CoverCache;
use crate::wave_cache::WaveCache;
use futures::StreamExt as _;
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;
use melodie_core::env::Env;
use melodie_core::proto::{QueueEntry, Request};
use melodie_desktop::browse::Place;
use melodie_desktop::covers::Use;
use melodie_desktop::credits::{self, Credits};
use melodie_desktop::launch::{self, Job, Kind, Lanes};
use melodie_desktop::link::{self, Msg};
use melodie_desktop::model::{Clock, Connection, Player};
use melodie_desktop::playing::{self, Playing, Ranks};
use melodie_desktop::quick::Seen;
use melodie_desktop::selection::Played;
use melodie_desktop::theme::{self, Active, Choice, Omarchy, Palette, Settings, Tint};
use melodie_desktop::thumbs::{self, Thumbs};
use melodie_desktop::tint;
use melodie_desktop::wave;
use std::collections::VecDeque;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

/// How long a failed action is said at the bottom of the window.
const NOTICE: Duration = Duration::from_secs(6);
/// Covers kept decoded for the cross-fade; older ones leave the GPU too.
const KEEP_COVERS: usize = 3;

/// A cover on screen, with the colour it gives the base theme.
#[derive(Clone)]
pub struct Shown {
    pub image: Option<Arc<RenderImage>>,
    pub tint: Option<Tint>,
}

/// The covers of the last few tracks. `step` grows each time another one is shown: the player
/// cross-fades towards it, through the ones in between.
#[derive(Default)]
pub struct Art {
    pub step: u64,
    shown: VecDeque<(u64, Shown)>,
    /// The track (its change count) and the cover file being loaded.
    want: Option<(u64, Option<String>)>,
    /// What the base theme is tinted by: the colour of the cover shown last.
    tint: Option<Tint>,
}

impl Art {
    pub fn at(&self, step: u64) -> Option<&Shown> {
        self.shown.iter().find(|(s, _)| *s == step).map(|(_, a)| a)
    }

    pub fn tint(&self) -> Option<Tint> {
        self.tint
    }
}

/// What the theme depends on.
pub struct ThemeState {
    pub omarchy: Omarchy,
    pub stored: Option<Choice>,
    pub detected: bool,
    /// The current Omarchy palette, while its file reads.
    pub palette: Option<Palette>,
}

impl ThemeState {
    pub fn active(&self) -> Active {
        theme::active(self.stored, self.detected, self.palette)
    }

    /// The choice the settings show as selected: before any is made, the one in use.
    pub fn selected(&self) -> Choice {
        match self.active() {
            Active::Base => Choice::Base,
            Active::Omarchy(_) | Active::BaseOmarchyGone => Choice::Omarchy,
        }
    }
}

pub struct Shared {
    pub player: Player,
    /// Which rows of the lists are the music playing now.
    pub playing: Playing,
    /// The play of some rows asked from a playlist's page, until it runs: the job, the playlist,
    /// the playlist's own rank of each row sent.
    played_from: Option<(Job, String, Vec<usize>)>,
    pub art: Art,
    /// The picture of each row of the queue, by entry; kept up to date away from the render path.
    pub thumbs: Arc<Thumbs>,
    /// The tracks `thumbs` is being (or was) computed for.
    thumbs_of: Option<Arc<[QueueEntry]>>,
    /// Which lookup of `thumbs` is the latest: an older one finishing late says nothing.
    thumbs_gen: u64,
    /// The artists credited on each track of the queue, by track; read away from the render path.
    pub credits: Arc<Credits>,
    /// The tracks `credits` is being (or was) read for.
    credits_of: Option<Arc<[QueueEntry]>>,
    /// Which read of `credits` is the latest.
    credits_gen: u64,
    /// The cover files of the whole window: the queue, the player and the library views ask
    /// it, and it fetches what is missing.
    pub covers: Entity<CoverCache>,
    /// The waveforms of the progress bar: the current track's and the next one's.
    pub waves: Entity<WaveCache>,
    pub theme: ThemeState,
    /// The desktop asks for dark colours.
    pub dark: bool,
    /// The queue is shown: a column beside the main area, or over it on a narrow window. Kept
    /// between launches.
    pub queue_open: bool,
    pub settings_open: bool,
    /// A failed action, said for a few seconds.
    failure: Option<(u64, SharedString)>,
    failures: u64,
    socket: PathBuf,
    env: Env,
    /// The `melodie` the jobs run.
    program: PathBuf,
    _watch: Option<notify::RecommendedWatcher>,
    /// The playlist jobs running, and those waiting behind them.
    playlist_jobs: Lanes<(Job, Done)>,
    /// The jobs that change what plays, one lane (`PLAYER`): a quicker job started later would
    /// otherwise be overtaken by an earlier, slower one.
    play_jobs: Lanes<Job>,
}

/// The lane of `Shared::play_jobs`.
const PLAYER: &str = "player";

/// Something the user started playing from the window.
pub struct Started(pub Seen);

impl EventEmitter<Started> for Shared {}

/// What a job run by `run_then` calls with its outcome.
type Done = Box<dyn FnOnce(Result<String, String>, &mut App)>;

impl Shared {
    /// The colours the window is heading to.
    pub fn target_palette(&self) -> Palette {
        match self.theme.active() {
            Active::Omarchy(p) => p,
            Active::Base | Active::BaseOmarchyGone => Palette::base(self.art.tint(), self.dark),
        }
    }

    /// What the bottom of the window says, the most important first.
    pub fn notice(&self) -> Option<SharedString> {
        if let Some((_, f)) = &self.failure {
            return Some(f.clone());
        }
        match self.player.connection {
            Connection::AuthRefused => {
                return Some("Navidrome refused the login: sign in again".into());
            }
            Connection::Unreachable => return Some("Navidrome is unreachable".into()),
            Connection::Ok => {}
        }
        (self.theme.active() == Active::BaseOmarchyGone)
            .then(|| "Omarchy theme files are gone: using the base theme".into())
    }

    /// Sends a request to the daemon; a refusal is said in the window.
    pub fn send(&self, req: Request, cx: &mut Context<Self>) {
        let socket = self.socket.clone();
        let task = cx.background_spawn(async move { link::request(&socket, &req) });
        cx.spawn(async move |this, cx| {
            if let Err(e) = task.await {
                let _ = this.update(cx, |s, cx| s.fail(format!("The player said no: {e}"), cx));
            }
        })
        .detach();
    }

    /// Play or pause; with nothing current, the queue from its start, or with no daemon the
    /// personal mix.
    pub fn play_or_pause(&mut self, cx: &mut Context<Self>) {
        match (
            self.player.following,
            self.player.current,
            self.player.tracks.len(),
        ) {
            (true, Some(_), _) => self.send(Request::Toggle, cx),
            (true, None, n) if n > 0 => self.send(Request::QueuePlay { index: 0 }, cx),
            _ => self.run(Job::PlayMix, cx),
        }
    }

    /// Sends an action on a row of the queue, unless that row no longer holds the entry the user
    /// clicked: another client (or the user's own earlier click) changed the queue since, and a
    /// position alone would act on a neighbour.
    pub fn send_for_row(&self, ix: usize, entry: u64, req: Request, cx: &mut Context<Self>) {
        if self.player.holds(ix, entry) {
            self.send(req, cx);
        }
    }

    /// Runs `melodie` for what needs the server; its first error line is said in the window.
    pub fn run(&mut self, job: Job, cx: &mut Context<Self>) {
        if !job.plays() {
            return self.spawn_run(job, cx);
        }
        // at most one job runs ahead of the newest click
        if job.replaces() {
            self.play_jobs.drop_waiting(PLAYER);
        }
        if let Some(job) = self.play_jobs.start(PLAYER, job) {
            self.spawn_run(job, cx);
        }
    }

    /// Runs `played`, rows of the list shown at `here`: when that list is a playlist, the queue
    /// it makes is the playlist's (`playing` marks the playlist, and which occurrence).
    pub fn play_from(&mut self, here: &Place, played: Played, cx: &mut Context<Self>) {
        let Played { job, ranks } = played;
        self.played_from = match (here, &job) {
            (Place::Playlist(id), Job::PlayTracks { .. }) => Some((job.clone(), id.clone(), ranks)),
            _ => None,
        };
        self.run(job, cx);
    }

    /// Runs `job` now; when it is done, the next job that changes what plays, if it is one.
    fn spawn_run(&mut self, job: Job, cx: &mut Context<Self>) {
        // noted when it runs: a job dropped while it waited was never played
        if let Some(seen) = Seen::of_job(&job) {
            cx.emit(Started(seen));
        }
        if job.replaces() {
            // a play from a playlist's rows waiting behind it was dropped, or is this one
            let from = self.played_from.take().filter(|(from, ..)| *from == job);
            match (&job, from) {
                (
                    Job::Play {
                        kind: Kind::Playlist,
                        id,
                        shuffle,
                    },
                    _,
                ) => self.playing.start_playlist(
                    id,
                    if *shuffle {
                        Ranks::Shuffled
                    } else {
                        Ranks::Own
                    },
                ),
                (Job::PlayTracks { .. }, Some((_, id, ranks))) => {
                    self.playing.start_playlist(&id, Ranks::Of(ranks.into()))
                }
                _ => self.playing.play_elsewhere(),
            }
        }
        let replaces = job.replaces();
        let lane = job.plays();
        let program = self.program.clone();
        let what = match &job {
            Job::PlayMix => "Could not start your mix",
            Job::Rediscover => "Could not start the rediscoveries",
            Job::Star { .. } => "Could not change the favourite",
            Job::Play { .. } | Job::PlayTracks { .. } => "Could not play it",
            Job::Queue { .. } | Job::QueueTracks { .. } => "Could not add it to the queue",
            Job::Mix { .. } => "Could not make the mix",
            Job::AddToPlaylist { .. } => "Could not add to the playlist",
            Job::MixGenre { .. } => "Could not make the mix",
            Job::QueueMix { .. } => "Could not add the mix to the queue",
            // run by `run_then`, which says what they did
            Job::CheckAdd { .. }
            | Job::CreatePlaylist { .. }
            | Job::RenamePlaylist { .. }
            | Job::DeletePlaylist { .. }
            | Job::RemoveFromPlaylist { .. }
            | Job::MoveInPlaylist { .. }
            | Job::Duplicates { .. } => "Could not change the playlist",
        };
        let adds = matches!(job, Job::AddToPlaylist { .. });
        let task = cx.background_spawn(async move {
            let input = job.stdin();
            let out = launch::command(&program)
                .args(job.args())
                .stdin(match input {
                    Some(_) => Stdio::piped(),
                    None => Stdio::null(),
                })
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .and_then(|mut child| {
                    if let (Some(text), Some(mut pipe)) = (input, child.stdin.take()) {
                        // a `melodie` that fails before reading says why on its stderr
                        let _ = pipe.write_all(text.as_bytes());
                    }
                    child.wait_with_output()
                });
            match out {
                // `--json` says how it went on its output, even when it failed
                Ok(o) if o.status.success() && adds => {
                    let said = launch::added_message(&String::from_utf8_lossy(&o.stdout));
                    Ok(Some(said.unwrap_or_else(|why| why)))
                }
                Ok(o) if o.status.success() => Ok(None),
                Ok(o) => Err(String::from_utf8_lossy(&o.stderr)
                    .lines()
                    .next()
                    .map(str::to_string)),
                Err(e) => Err(Some(format!("{}: {}", program.display(), e.kind()))),
            }
        });
        cx.spawn(async move |this, cx| {
            let msg = match task.await {
                Ok(None) => None,
                Ok(Some(said)) => Some(said),
                Err(Some(why)) if !why.is_empty() => Some(format!("{what}: {why}")),
                Err(_) => Some(what.to_string()),
            };
            let _ = this.update(cx, |s, cx| {
                if let Some(msg) = msg {
                    if replaces {
                        s.playing.play_failed();
                    }
                    s.fail(msg, cx);
                }
                if lane && let Some(next) = s.play_jobs.done(PLAYER) {
                    s.spawn_run(next, cx);
                }
            });
        })
        .detach();
    }

    /// Runs `melodie`, then `done` with what it printed, or the first line of its error. The jobs
    /// of a playlist run one after the other, in the order asked.
    pub fn run_then(
        &mut self,
        job: Job,
        cx: &mut Context<Self>,
        done: impl FnOnce(Result<String, String>, &mut App) + 'static,
    ) {
        let done: Done = Box::new(done);
        match job.playlist().map(str::to_string) {
            Some(playlist) => {
                if let Some((job, done)) = self.playlist_jobs.start(&playlist, (job, done)) {
                    self.spawn_job(job, Some(playlist), done, cx);
                }
            }
            None => self.spawn_job(job, None, done, cx),
        }
    }

    /// Runs `job`; when it is done, the next job waiting on `playlist`, if any.
    fn spawn_job(&self, job: Job, playlist: Option<String>, done: Done, cx: &mut Context<Self>) {
        let program = self.program.clone();
        let task = cx.background_spawn(async move {
            let out = launch::command(&program)
                .args(job.args())
                .stdin(std::process::Stdio::null())
                .output();
            match out {
                Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
                Ok(o) => Err(String::from_utf8_lossy(&o.stderr)
                    .lines()
                    .next()
                    .unwrap_or("it failed")
                    .to_string()),
                Err(e) => Err(format!("{}: {}", program.display(), e.kind())),
            }
        });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            cx.update(|cx| done(r, cx));
            if let Some(playlist) = playlist {
                let _ = this.update(cx, |s, cx| {
                    if let Some((job, done)) = s.playlist_jobs.done(&playlist) {
                        s.spawn_job(job, Some(playlist), done, cx);
                    }
                });
            }
        })
        .detach();
    }

    /// Says something at the bottom of the window for a few seconds.
    pub fn say(&mut self, msg: impl Into<String>, cx: &mut Context<Self>) {
        self.fail(msg.into(), cx);
    }

    /// Says something at the bottom of the window for a few seconds (a failure, or what an
    /// action did).
    pub fn fail(&mut self, msg: String, cx: &mut Context<Self>) {
        self.failures += 1;
        let n = self.failures;
        self.failure = Some((n, msg.into()));
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(NOTICE).await;
            let _ = this.update(cx, |s, cx| {
                if s.failure.as_ref().is_some_and(|(m, _)| *m == n) {
                    s.failure = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub fn choose(&mut self, choice: Choice, cx: &mut Context<Self>) {
        self.theme.stored = Some(choice);
        if let Err(e) = Settings::update(&self.env, |s| s.theme = Some(choice)) {
            self.fail(format!("Could not keep the choice: {e:#}"), cx);
        }
        apply_component_theme(self.target_palette(), cx);
        cx.notify();
    }

    /// Shows or hides the queue: the one action behind its buttons and its key.
    pub fn toggle_queue(&mut self, cx: &mut Context<Self>) {
        self.settings_open = false;
        self.show_queue(!self.queue_open, cx);
    }

    pub fn show_queue(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.queue_open = shown;
        if let Err(e) = Settings::update(&self.env, |s| s.queue = shown) {
            self.fail(format!("Could not keep the queue's place: {e:#}"), cx);
        }
        cx.notify();
    }

    /// After the player changed, or a cover landed: look up the pictures of the rows. A new queue
    /// asks for the covers it misses (the playing one first). The rows keep what they had
    /// meanwhile (entries are kept when they move).
    fn follow_thumbs(&mut self, force: bool, cx: &mut Context<Self>) {
        let tracks = self.player.tracks.clone();
        let same = self
            .thumbs_of
            .as_ref()
            .is_some_and(|t| Arc::ptr_eq(t, &tracks));
        if same && !force {
            return;
        }
        if !same {
            let playing = self.player.track().and_then(|t| t.cover_id.clone());
            let wanted = thumbs::wanted(&tracks);
            self.covers.update(cx, |c, cx| {
                c.want(playing, Use::SMALL, true, cx);
                c.want(wanted, Use::SMALL, false, cx);
            });
        }
        self.thumbs_of = Some(tracks.clone());
        self.thumbs_gen += 1;
        let latest = self.thumbs_gen;
        let env = self.env.clone();
        let task = cx.background_spawn(async move { thumbs::resolve(&env, &tracks) });
        cx.spawn(async move |this, cx| {
            let found = task.await;
            let _ = this.update(cx, |s, cx| {
                // Another lookup came meanwhile: it will say.
                if s.thumbs_gen == latest {
                    s.thumbs = Arc::new(found);
                    // the player shows a thumbnail while the daemon fetches its own cover
                    s.follow_cover(cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// After the player changed: the artists of the tracks of a new queue, from the local copy.
    /// The rows show the names the daemon gave meanwhile.
    fn follow_credits(&mut self, cx: &mut Context<Self>) {
        let tracks = self.player.tracks.clone();
        if self
            .credits_of
            .as_ref()
            .is_some_and(|t| Arc::ptr_eq(t, &tracks))
        {
            return;
        }
        self.credits_of = Some(tracks.clone());
        self.credits_gen += 1;
        let latest = self.credits_gen;
        let env = self.env.clone();
        let task =
            cx.background_spawn(
                async move { credits::read(&env, tracks.iter().map(|t| t.id.clone())) },
            );
        cx.spawn(async move |this, cx| {
            let found = task.await;
            let _ = this.update(cx, |s, cx| {
                if s.credits_gen == latest {
                    s.credits = Arc::new(found);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// After the player changed: which rows are the music playing. The album and the artist of a
    /// track that begins are read from the local copy, away from the window.
    fn follow_playing(&mut self, cx: &mut Context<Self>) {
        self.playing.follow(&self.player);
        let Some(id) = self.playing.wants_origin() else {
            return;
        };
        let env = self.env.clone();
        let task = cx.background_spawn(async move {
            let origin = playing::read_origin(&env, &id);
            (id, origin)
        });
        cx.spawn(async move |this, cx| {
            let (id, origin) = task.await;
            let _ = this.update(cx, |s, cx| {
                s.playing.set_origin(&id, origin);
                cx.notify();
            });
        })
        .detach();
    }

    /// After the player changed: the waveforms of the current track and of the next one.
    fn follow_waves(&mut self, cx: &mut Context<Self>) {
        let ids = wave::wanted(&self.player);
        self.waves.update(cx, |w, cx| w.want(ids, cx));
    }

    /// After the player changed: load the new cover if there is one.
    fn follow_cover(&mut self, cx: &mut Context<Self>) {
        let cover = self
            .player
            .track()
            .and_then(|t| thumbs::big(t, &self.thumbs));
        let key = (self.player.track_change, cover.clone());
        if self.art.want.as_ref() == Some(&key) {
            return;
        }
        self.art.want = Some(key.clone());
        let Some(path) = cover else {
            self.show(
                key,
                Shown {
                    image: None,
                    tint: None,
                },
                cx,
            );
            return;
        };
        // Decoded away from the window, and shown only once ready: the cross-fade never fades
        // towards an empty frame.
        let task = cx.background_spawn(async move { load_cover(&PathBuf::from(path)) });
        cx.spawn(async move |this, cx| {
            let shown = task.await;
            let _ = this.update(cx, |s, cx| s.show(key, shown, cx));
        })
        .detach();
    }

    fn show(&mut self, key: (u64, Option<String>), shown: Shown, cx: &mut Context<Self>) {
        if self.art.want.as_ref() != Some(&key) {
            return; // another track came meanwhile
        }
        self.art.tint = shown.tint;
        // The base theme follows the cover: gpui-kit's own colours (ring, primary, popover) must
        // follow too, or they keep the previous tint.
        if self.theme.active().follows_cover() {
            apply_component_theme(self.target_palette(), cx);
        }
        self.art.step += 1;
        self.art.shown.push_back((self.art.step, shown));
        while self.art.shown.len() > KEEP_COVERS {
            if let Some((_, Shown { image: Some(i), .. })) = self.art.shown.pop_front() {
                cx.drop_image(i, None);
            }
        }
        cx.notify();
    }
}

/// Reads a cover file into an image ready for the GPU, and its colour.
fn load_cover(path: &std::path::Path) -> Shown {
    let decoded = std::fs::read(path)
        .ok()
        .and_then(|b| image::load_from_memory(&b).ok());
    let Some(img) = decoded else {
        return Shown {
            image: None,
            tint: None,
        };
    };
    let tint = tint::of_image(&img);
    let mut buf = img.into_rgba8();
    // GPUI draws BGRA.
    for p in buf.pixels_mut() {
        p.0.swap(0, 2);
    }
    Shown {
        image: Some(Arc::new(RenderImage::new([image::Frame::new(buf)]))),
        tint,
    }
}

/// gpui-kit's own components (the sliders) take their colours from its theme: written once per
/// change of theme, never per frame.
pub fn apply_component_theme(p: Palette, cx: &mut App) {
    Theme::change(
        if p.dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        },
        None,
        cx,
    );
    let clear = hsla(0., 0., 0., 0.);
    Theme::update(cx, |t| {
        let c = &mut t.colors;
        c.background = clear;
        c.foreground = p.fg;
        c.muted_foreground = p.secondary;
        c.border = Hsla { a: 0.12, ..p.fg };
        c.ring = p.accent;
        c.primary = p.accent;
        c.primary_foreground = p.on_accent;
        c.popover = p.surface;
        c.popover_foreground = p.fg;
        c.list = clear;
        c.scrollbar = clear;
        c.scrollbar_thumb = Hsla { a: 0.3, ..p.fg };
        c.scrollbar_thumb_hover = Hsla { a: 0.5, ..p.fg };
    });
}

/// The window's state without a daemon, a theme to follow or a window: the jobs run `program`,
/// never the `melodie` of the machine.
#[cfg(test)]
pub fn bare(env: Env, program: PathBuf, cx: &mut App) -> Entity<Shared> {
    let covers = cx.new(|_| CoverCache::new(env.clone(), 1.));
    let waves = cx.new(|_| WaveCache::new(env.clone()));
    cx.new(|_| Shared {
        player: Player::default(),
        playing: Playing::default(),
        played_from: None,
        art: Art::default(),
        thumbs: Arc::default(),
        thumbs_of: None,
        thumbs_gen: 0,
        credits: Arc::default(),
        credits_of: None,
        credits_gen: 0,
        covers,
        waves,
        theme: ThemeState {
            omarchy: Omarchy::system(&env),
            stored: None,
            detected: false,
            palette: None,
        },
        dark: true,
        queue_open: false,
        settings_open: false,
        failure: None,
        failures: 0,
        socket: env.state_dir().join("no-daemon.sock"),
        env,
        program,
        _watch: None,
        playlist_jobs: Lanes::default(),
        play_jobs: Lanes::default(),
    })
}

/// A `melodie` in `dir` that writes down what it is asked (its arguments, then what it reads),
/// for `bare`.
#[cfg(test)]
pub fn fake_melodie(dir: &std::path::Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let program = dir.join("melodie");
    std::fs::write(&program, "#!/bin/sh\n{ echo \"$*\"; cat; } >> \"$0.ran\"\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    program
}

/// What the `fake_melodie` at `program` was asked so far.
#[cfg(test)]
pub fn ran(program: &std::path::Path) -> String {
    std::fs::read_to_string(program.with_extension("ran")).unwrap_or_default()
}

pub fn is_dark(a: WindowAppearance) -> bool {
    matches!(a, WindowAppearance::Dark | WindowAppearance::VibrantDark)
}

/// The state of a window without a screen nor a daemon, for the window's own tests.
#[cfg(test)]
pub fn for_tests(env: Env, cx: &mut App) -> Entity<Shared> {
    let covers = cx.new(|_| CoverCache::new(env.clone(), 1.));
    let waves = cx.new(|_| WaveCache::new(env.clone()));
    cx.new(|_| Shared {
        player: Player::default(),
        playing: Playing::default(),
        played_from: None,
        art: Art::default(),
        thumbs: Arc::default(),
        thumbs_of: None,
        thumbs_gen: 0,
        credits: Arc::default(),
        credits_of: None,
        credits_gen: 0,
        covers,
        waves,
        theme: ThemeState {
            omarchy: Omarchy::system(&env),
            stored: None,
            detected: false,
            palette: None,
        },
        dark: true,
        queue_open: false,
        settings_open: false,
        failure: None,
        failures: 0,
        socket: env.state_dir().join("no-daemon.sock"),
        // never a real `melodie`
        program: env.state_dir().join("no-melodie"),
        env,
        _watch: None,
        playlist_jobs: Lanes::default(),
        play_jobs: Lanes::default(),
    })
}

/// Builds the state and starts following the daemon and the theme.
pub fn start(
    env: Env,
    socket: PathBuf,
    window: &mut Window,
    cx: &mut App,
) -> (Entity<Shared>, Entity<Clock>) {
    let omarchy = Omarchy::system(&env);
    let detected = omarchy.detected();
    let settings = Settings::load(&env);
    let theme = ThemeState {
        stored: settings.theme,
        detected,
        palette: detected.then(|| omarchy.read().ok()).flatten(),
        omarchy,
    };
    let clock = cx.new(|_| Clock::default());
    let covers = cx.new({
        let env = env.clone();
        let scale = window.scale_factor();
        move |_| CoverCache::new(env, scale)
    });
    let waves = cx.new({
        let env = env.clone();
        move |_| WaveCache::new(env)
    });
    let shared = cx.new(|_| Shared {
        player: Player::default(),
        playing: Playing::default(),
        played_from: None,
        art: Art::default(),
        thumbs: Arc::default(),
        thumbs_of: None,
        thumbs_gen: 0,
        credits: Arc::default(),
        credits_of: None,
        credits_gen: 0,
        covers: covers.clone(),
        waves,
        theme,
        dark: is_dark(window.appearance()),
        queue_open: settings.queue,
        settings_open: false,
        failure: None,
        failures: 0,
        socket: socket.clone(),
        env,
        program: launch::program(std::env::current_exe().ok().as_deref()),
        _watch: None,
        playlist_jobs: Lanes::default(),
        play_jobs: Lanes::default(),
    });
    apply_component_theme(shared.read(cx).target_palette(), cx);
    shared.update(cx, |s, cx| s.follow_cover(cx));
    // a cover that lands shows on the rows of the queue and in the player, without a restart
    let s = shared.clone();
    cx.observe(&covers, move |_, cx| {
        s.update(cx, |s, cx| s.follow_thumbs(true, cx));
    })
    .detach();

    // The daemon: its thread never touches the window, it hands lines over.
    let (tx, mut rx) = futures::channel::mpsc::unbounded();
    link::follow(socket, Arc::new(AtomicBool::new(false)), move |m| {
        tx.unbounded_send(m).is_ok()
    });
    let (s, c) = (shared.clone(), clock.clone());
    cx.spawn(async move |cx| {
        while let Some(msg) = rx.next().await {
            let at = Instant::now();
            s.update(cx, |s, cx| {
                let changed = c.update(cx, |clock, cx| {
                    let ch = match &msg {
                        Msg::Event(e) => s.player.apply(e, clock, at),
                        Msg::Gone => s.player.gone(clock),
                    };
                    if ch.clock {
                        cx.notify();
                    }
                    ch
                });
                if changed.player {
                    s.follow_playing(cx);
                    s.follow_thumbs(false, cx);
                    s.follow_credits(cx);
                    s.follow_cover(cx);
                    s.follow_waves(cx);
                    cx.notify();
                }
            });
        }
    })
    .detach();

    // Omarchy's theme, live.
    if detected {
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let omarchy = shared.read(cx).theme.omarchy.clone();
        let watcher = theme::watch(omarchy, theme::SETTLE, move |p| {
            tx.unbounded_send(p).is_ok()
        });
        shared.update(cx, |s, _| s._watch = watcher);
        let s = shared.clone();
        cx.spawn(async move |cx| {
            while let Some(p) = rx.next().await {
                s.update(cx, |s, cx| {
                    s.theme.palette = p;
                    apply_component_theme(s.target_palette(), cx);
                    cx.notify();
                });
            }
        })
        .detach();
    }
    (shared, clock)
}

#[cfg(test)]
mod tests {
    use super::{Played, Seen, Shared, Started, bare, fake_melodie, ran};
    use gpui_kit::Entity;
    use gpui_kit::{TestAppContext, TestDispatcher};
    use melodie_core::db;
    use melodie_core::env::Env;
    use melodie_core::proto::QueueEntry;
    use melodie_desktop::browse::Place;
    use melodie_desktop::launch::Job;
    use melodie_desktop::launch::Kind;
    use melodie_desktop::model::{PlayState, Player};
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn the_plays_run_one_after_the_other_in_the_order_clicked() {
        // the test executor runs the tasks ready in an order drawn from its seed
        for seed in 0..8 {
            let dir = tempfile::tempdir().unwrap();
            let program = fake_melodie(dir.path());
            let env = Env::with_home(dir.path().join("home"));
            let mut app = TestAppContext::build(TestDispatcher::new(seed), None);
            let shared = app.update(|cx| bare(env, program.clone(), cx));
            shared.update(&mut app, |s, cx| {
                let play = |id: &str| Job::Play {
                    kind: Kind::Album,
                    id: id.into(),
                    shuffle: false,
                };
                s.run(play("first"), cx);
                s.run(
                    Job::QueueTracks {
                        ids: vec!["second".into()],
                        next: true,
                    },
                    cx,
                );
                s.run(play("third"), cx);
            });
            app.run_until_parked();
            assert_eq!(
                ran(&program),
                "play album -- first\nplay album -- third\n",
                "seed {seed}: the last click wins, and what it replaces never runs"
            );
        }
    }

    /// A player whose queue holds `ids` (entry numbers from `first`), at the first one.
    fn playing_queue(first: u64, ids: &[&str]) -> Player {
        Player {
            following: true,
            state: PlayState::Playing,
            tracks: ids
                .iter()
                .zip(first..)
                .map(|(id, entry)| QueueEntry {
                    entry,
                    id: (*id).into(),
                    ..QueueEntry::default()
                })
                .collect(),
            current: Some(0),
            ..Player::default()
        }
    }

    #[test]
    fn a_playlist_is_marked_when_the_window_played_it_whatever_the_way() {
        let dir = tempfile::tempdir().unwrap();
        let program = fake_melodie(dir.path());
        let env = Env::with_home(dir.path().join("home"));
        let mut app = TestAppContext::single();
        let shared = app.update(|cx| bare(env, program, cx));
        let tracks = || Played {
            job: Job::PlayTracks {
                ids: vec!["a".into(), "b".into()],
                from: 0,
            },
            ranks: vec![0, 1],
        };
        let marked = |app: &mut TestAppContext, shared: &Entity<Shared>, queue: Player| {
            shared.update(app, |s, cx| {
                s.player = queue;
                s.follow_playing(cx);
                s.playing.playlist("pl1").is_some()
            })
        };
        // the play button of the playlist's card, or its menu
        shared.update(&mut app, |s, cx| {
            s.run(
                Job::Play {
                    kind: Kind::Playlist,
                    id: "pl1".into(),
                    shuffle: false,
                },
                cx,
            )
        });
        app.run_until_parked();
        assert!(marked(&mut app, &shared, playing_queue(1, &["a", "b"])));
        // a double click on a row of an album does not make it the playlist's
        shared.update(&mut app, |s, cx| {
            s.play_from(&Place::Album("al1".into()), tracks(), cx)
        });
        app.run_until_parked();
        assert!(!marked(&mut app, &shared, playing_queue(3, &["a", "b"])));
        // a double click on a row of the playlist's page does
        shared.update(&mut app, |s, cx| {
            s.play_from(&Place::Playlist("pl1".into()), tracks(), cx)
        });
        app.run_until_parked();
        assert!(marked(&mut app, &shared, playing_queue(5, &["a", "b"])));
    }

    /// Whether the playlist `pl1` is marked once the queue is `queue`.
    fn marks_pl1(app: &mut TestAppContext, shared: &Entity<Shared>, queue: Player) -> bool {
        shared.update(app, |s, cx| {
            s.player = queue;
            s.follow_playing(cx);
            s.playing.playlist("pl1").is_some()
        })
    }

    fn play_pl1() -> Job {
        Job::Play {
            kind: Kind::Playlist,
            id: "pl1".into(),
            shuffle: false,
        }
    }

    #[test]
    fn another_play_forgets_the_playlist_waiting_for_its_queue() {
        let dir = tempfile::tempdir().unwrap();
        let program = fake_melodie(dir.path());
        let env = Env::with_home(dir.path().join("home"));
        let mut app = TestAppContext::single();
        let shared = app.update(|cx| bare(env, program, cx));
        // the playlist's play ran, its queue is not seen yet, and an album's play follows
        shared.update(&mut app, |s, cx| {
            s.run(play_pl1(), cx);
            s.run(
                Job::Play {
                    kind: Kind::Album,
                    id: "al1".into(),
                    shuffle: false,
                },
                cx,
            );
        });
        app.run_until_parked();
        assert!(!marks_pl1(&mut app, &shared, playing_queue(1, &["a", "b"])));
    }

    #[test]
    fn a_failed_play_of_a_playlist_does_not_claim_the_next_queue() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("melodie");
        std::fs::write(&program, "#!/bin/sh\necho 'no daemon' >&2\nexit 1\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let env = Env::with_home(dir.path().join("home"));
        let mut app = TestAppContext::single();
        let shared = app.update(|cx| bare(env, program, cx));
        shared.update(&mut app, |s, cx| s.run(play_pl1(), cx));
        app.run_until_parked();
        // another client plays something
        assert!(!marks_pl1(&mut app, &shared, playing_queue(1, &["a", "b"])));
    }

    #[test]
    fn a_play_of_a_playlist_s_rows_dropped_from_the_lane_is_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        let program = fake_melodie(dir.path());
        let env = Env::with_home(dir.path().join("home"));
        let mut app = TestAppContext::build(TestDispatcher::new(0), None);
        let shared = app.update(|cx| bare(env, program.clone(), cx));
        let rows = Job::PlayTracks {
            ids: vec!["a".into(), "b".into()],
            from: 0,
        };
        shared.update(&mut app, |s, cx| {
            s.run(Job::Rediscover, cx);
            // waits behind the rediscoveries, then dropped by the mix
            s.play_from(
                &Place::Playlist("pl1".into()),
                Played {
                    job: rows.clone(),
                    ranks: vec![0, 1],
                },
                cx,
            );
            s.run(Job::PlayMix, cx);
        });
        app.run_until_parked();
        // the same rows played later from elsewhere (a menu) are not the playlist's
        shared.update(&mut app, |s, cx| s.run(rows, cx));
        app.run_until_parked();
        assert!(
            ran(&program).starts_with("mix rediscover\nmix me\nplay "),
            "the rows waiting were dropped: {}",
            ran(&program)
        );
        assert!(!marks_pl1(&mut app, &shared, playing_queue(1, &["a", "b"])));
    }

    #[test]
    fn the_album_and_the_artist_of_the_track_are_read_when_it_begins() {
        let dir = tempfile::tempdir().unwrap();
        let program = fake_melodie(dir.path());
        let env = Env::with_home(dir.path().join("home"));
        let path = db::db_path(&env);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut conn = db::create(&path).unwrap();
        let tx = conn.transaction().unwrap();
        db::insert_artist(&tx, &serde_json::json!({"id":"ar1","name":"Nightfall"})).unwrap();
        db::insert_album(
            &tx,
            &serde_json::json!({"id":"al1","name":"Album 1","artist":"Nightfall","artistId":"ar1"}),
        )
        .unwrap();
        db::insert_track(&tx, &serde_json::json!({"id":"t1","title":"Low Tide","artist":"Nightfall","artistId":"ar1","album":"Album 1","albumId":"al1"})).unwrap();
        tx.commit().unwrap();
        let mut app = TestAppContext::single();
        let shared = app.update(|cx| bare(env, program, cx));
        shared.update(&mut app, |s, cx| {
            s.player = playing_queue(1, &["t1"]);
            s.follow_playing(cx);
            assert_eq!(s.playing.album("al1"), None, "not before it is read");
        });
        app.run_until_parked();
        shared.read_with(&app, |s, _| {
            assert!(s.playing.album("al1").is_some());
            assert!(s.playing.artist("ar1").is_some());
            assert!(s.playing.album("al2").is_none());
        });
    }

    #[test]
    fn the_artists_of_the_tracks_of_the_queue_are_read_when_it_changes() {
        let dir = tempfile::tempdir().unwrap();
        let program = fake_melodie(dir.path());
        let env = Env::with_home(dir.path().join("home"));
        let path = db::db_path(&env);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut conn = db::create(&path).unwrap();
        let tx = conn.transaction().unwrap();
        for (id, name) in [("ar5", "Nova Reyes"), ("ar1", "Lior Castell")] {
            db::insert_artist(&tx, &serde_json::json!({"id":id,"name":name})).unwrap();
        }
        db::insert_track(&tx, &serde_json::json!({"id":"t1","title":"Crossing","artist":"Nova Reyes, Lior Castell","artistId":"ar5","artists":[{"id":"ar5","name":"Nova Reyes"},{"id":"ar1","name":"Lior Castell"}]})).unwrap();
        tx.commit().unwrap();
        let mut app = TestAppContext::single();
        let shared = app.update(|cx| bare(env, program, cx));
        shared.update(&mut app, |s, cx| {
            s.player = playing_queue(1, &["t1", "elsewhere"]);
            s.follow_credits(cx);
        });
        app.run_until_parked();
        shared.read_with(&app, |s, _| {
            let names: Vec<&str> = s.credits["t1"].iter().map(|c| c.name.as_str()).collect();
            assert_eq!(names, ["Nova Reyes", "Lior Castell"]);
            assert!(!s.credits.contains_key("elsewhere"), "not in the copy");
        });
    }

    #[test]
    fn a_job_that_adds_after_a_running_play_still_runs() {
        let dir = tempfile::tempdir().unwrap();
        let program = fake_melodie(dir.path());
        let env = Env::with_home(dir.path().join("home"));
        let mut app = TestAppContext::build(TestDispatcher::new(0), None);
        let shared = app.update(|cx| bare(env, program.clone(), cx));
        shared.update(&mut app, |s, cx| {
            s.run(Job::PlayMix, cx);
            s.run(
                Job::QueueMix {
                    mix: Box::new(Job::Rediscover),
                    next: true,
                },
                cx,
            );
        });
        app.run_until_parked();
        assert_eq!(ran(&program), "mix me\nmix --next rediscover\n");
    }

    #[test]
    fn the_recents_note_what_ran_not_what_was_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let program = fake_melodie(dir.path());
        let env = Env::with_home(dir.path().join("home"));
        let mut app = TestAppContext::build(TestDispatcher::new(0), None);
        let shared = app.update(|cx| bare(env, program.clone(), cx));
        let seen = Rc::new(RefCell::new(Vec::new()));
        app.update(|cx| {
            let seen = seen.clone();
            cx.subscribe(&shared, move |_, Started(s): &Started, _| {
                seen.borrow_mut().push(s.clone())
            })
            .detach();
        });
        let play = |id: &str| Job::Play {
            kind: Kind::Album,
            id: id.into(),
            shuffle: false,
        };
        shared.update(&mut app, |s, cx| {
            s.run(play("first"), cx);
            s.run(play("second"), cx);
        });
        // the second waits behind the first: not noted yet
        assert_eq!(*seen.borrow(), [Seen::Album("first".into())]);
        shared.update(&mut app, |s, cx| s.run(play("third"), cx));
        app.run_until_parked();
        assert_eq!(
            *seen.borrow(),
            [Seen::Album("first".into()), Seen::Album("third".into())]
        );
    }
}
