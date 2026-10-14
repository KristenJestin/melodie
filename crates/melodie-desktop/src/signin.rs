//! The login from the window: the sign-in screen, and what the settings say of the account and
//! of the local copy. Everything that touches the keyring or the server is `melodie`'s
//! (`melodie_desktop::account`).

use crate::app::Shared;
use crate::motion;
use crate::player::palette;
use crate::widgets::IconButton;
use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::Icon;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use melodie_core::db::Counts;
use melodie_core::env::Env;
use melodie_core::store;
use melodie_desktop::account::{self, Outcome, Screen, Standing};
use melodie_desktop::launch;
use melodie_desktop::model::Connection;
use melodie_desktop::theme::Palette;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How often, and how long, the window looks at a copy it waits for: the first copy fills the
/// library as it runs, and the one `melodie login` starts runs in the background, not waited for.
const COPY_EVERY: Duration = Duration::from_secs(1);
const COPY_FOR: Duration = Duration::from_secs(15 * 60);

fn program() -> PathBuf {
    launch::program(std::env::current_exe().ok().as_deref())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The account as the window knows it: no secret, only what `melodie` left in the
/// configuration, the refusal marker and the local copy.
pub struct Account {
    env: Env,
    shared: Entity<Shared>,
    pub standing: Standing,
    pub server: Option<String>,
    pub user: Option<String>,
    pub synced_at: Option<u64>,
    /// `melodie sync` runs, or a copy is followed (the first one, the one after a sign-in).
    pub syncing: bool,
    /// What a first copy under way holds so far.
    pub copying: Option<Counts>,
    /// The window started the first copy itself: once.
    first_started: bool,
    /// Grows each time the window starts following a copy, or stops: an older follower ends.
    following: u64,
    pub signing_out: bool,
    /// Grows each time a new copy is there: the library reads it again.
    pub copy_step: u64,
    _subs: Vec<Subscription>,
}

impl Account {
    pub fn new(env: Env, shared: Entity<Shared>, cx: &mut Context<Self>) -> Self {
        let mut was_open = false;
        let subs = vec![cx.observe(&shared, move |this: &mut Self, shared, cx| {
            let s = shared.read(cx);
            let open = s.settings_open;
            // the daemon was refused: the window shows the sign-in screen
            if s.player.connection == Connection::AuthRefused
                && this.standing != Standing::AuthRefused
            {
                this.standing = Standing::AuthRefused;
                cx.notify();
            }
            if open && !was_open {
                this.read_copy(cx);
            }
            was_open = open;
        })];
        let mut this = Self {
            standing: Standing::known(&env),
            server: None,
            user: None,
            synced_at: None,
            syncing: false,
            copying: None,
            first_started: false,
            following: 0,
            signing_out: false,
            copy_step: 0,
            env,
            shared,
            _subs: subs,
        };
        this.read_settings();
        this.read_copy(cx);
        if this.standing == Standing::Checking {
            this.check(cx);
        }
        this
    }

    pub fn screen(&self, cx: &App) -> Screen {
        account::screen(self.standing, self.shared.read(cx).player.connection)
    }

    fn read_settings(&mut self) {
        let s = store::Settings::load(&self.env).unwrap_or_default();
        self.server = s.url;
        self.user = s.user.filter(|u| !u.is_empty());
    }

    /// The date of the local copy, read away from the window.
    fn read_copy(&mut self, cx: &mut Context<Self>) {
        let env = self.env.clone();
        let task = cx.background_spawn(async move { account::synced_at(&env) });
        cx.spawn(async move |this, cx| {
            let at = task.await;
            let _ = this.update(cx, |a, cx| {
                if a.synced_at != at {
                    a.synced_at = at;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Asks the server, through `melodie status`, whether the kept login still holds.
    fn check(&mut self, cx: &mut Context<Self>) {
        let env = self.env.clone();
        let task = cx.background_spawn(async move {
            let code = std::process::Command::new(program())
                .arg("status")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .ok()
                .and_then(|s| s.code());
            Standing::after(code, store::auth_refused_marked(&env))
        });
        cx.spawn(async move |this, cx| {
            let found = task.await;
            let _ = this.update(cx, |a, cx| match found {
                Some(s) => a.set_standing(s, cx),
                None => a.say("Could not check the connection to Navidrome", cx),
            });
        })
        .detach();
    }

    fn set_standing(&mut self, s: Standing, cx: &mut Context<Self>) {
        self.standing = s;
        if s == Standing::Ok {
            self.first_copy(cx);
        }
        if matches!(s, Standing::Ok | Standing::NoLogin) {
            // what the daemon said of an older login no longer holds
            self.shared.update(cx, |sh, cx| {
                if sh.player.connection != Connection::Ok {
                    sh.player.connection = Connection::Ok;
                    cx.notify();
                }
            });
        }
        cx.notify();
    }

    fn say(&self, what: impl Into<String>, cx: &mut Context<Self>) {
        let what = what.into();
        self.shared.update(cx, |s, cx| s.fail(what, cx));
    }

    /// After `melodie login` said yes: the library follows the copy it started as it comes.
    /// `since` is when the attempt began (`now_secs`).
    pub fn signed_in(&mut self, since: u64, cx: &mut Context<Self>) {
        self.read_settings();
        self.follow(since, cx);
        self.set_standing(Standing::Ok, cx);
    }

    /// Signed in with no complete copy: the window starts the first one itself, once, whoever
    /// made the login.
    fn first_copy(&mut self, cx: &mut Context<Self>) {
        if self.first_started || self.syncing {
            return;
        }
        let env = self.env.clone();
        let task = cx.background_spawn(async move { account::copy_state(&env) });
        cx.spawn(async move |this, cx| {
            let copy = task.await;
            let _ = this.update(cx, |a, cx| {
                if account::starts_first_copy(a.standing, copy, a.first_started) {
                    a.first_started = true;
                    a.sync_now(cx);
                }
            });
        })
        .detach();
    }

    /// Follows the local copy until one made since `since` is complete (or for a quarter of an
    /// hour): while a first copy fills, the library is read again at each step it takes. Whoever
    /// runs the copy: this window, `melodie login`, the daemon.
    fn follow(&mut self, since: u64, cx: &mut Context<Self>) {
        self.following += 1;
        let me = self.following;
        self.syncing = true;
        cx.notify();
        let env = self.env.clone();
        cx.spawn(async move |this, cx| {
            let started = std::time::Instant::now();
            loop {
                let env = env.clone();
                let copy = cx
                    .background_spawn(async move { account::copy_state(&env) })
                    .await;
                let late = started.elapsed() > COPY_FOR;
                let go_on = this.update(cx, |a, cx| {
                    if a.following != me {
                        return false;
                    }
                    match copy {
                        account::CopyState::Partial(so_far) if a.copying != Some(so_far) => {
                            a.copying = Some(so_far);
                            a.copy_step += 1;
                            cx.notify();
                        }
                        account::CopyState::Complete(at)
                            if account::is_new_copy(Some(at), since) =>
                        {
                            a.synced_at = Some(at);
                            a.copy_step += 1;
                            a.stop_following(cx);
                            return false;
                        }
                        _ => {}
                    }
                    if late {
                        a.stop_following(cx);
                        // the copy did not come: the server says why, if it is about the login
                        a.check(cx);
                    }
                    !late
                });
                if !matches!(go_on, Ok(true)) {
                    return;
                }
                cx.background_executor().timer(COPY_EVERY).await;
            }
        })
        .detach();
    }

    fn stop_following(&mut self, cx: &mut Context<Self>) {
        self.following += 1;
        self.syncing = false;
        self.copying = None;
        cx.notify();
    }

    /// `melodie sync`: the same copy as the command line's. The copy is followed while it runs;
    /// its end says only whether it failed.
    pub fn sync_now(&mut self, cx: &mut Context<Self>) {
        if self.syncing {
            return;
        }
        self.follow(now_secs(), cx);
        let env = self.env.clone();
        let task = cx.background_spawn(async move {
            let out = std::process::Command::new(program())
                .arg("sync")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .output();
            match out {
                Ok(o) => (
                    o.status.code(),
                    store::auth_refused_marked(&env),
                    String::from_utf8_lossy(&o.stderr)
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .to_string(),
                ),
                Err(e) => (None, false, format!("melodie: {}", e.kind())),
            }
        });
        cx.spawn(async move |this, cx| {
            let (code, refused, said) = task.await;
            let _ = this.update(cx, |a, cx| {
                match account::sync_end(code, refused, &said) {
                    // the follower sees the new copy, or the one another run is making
                    account::SyncEnd::Done => a.set_standing(Standing::Ok, cx),
                    account::SyncEnd::Standing(s) => {
                        a.stop_following(cx);
                        a.set_standing(s, cx);
                    }
                    account::SyncEnd::Failed(said) => {
                        a.stop_following(cx);
                        if said.is_empty() {
                            a.say("Could not sync the library", cx)
                        } else {
                            a.say(format!("Could not sync the library: {said}"), cx)
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `melodie logout`: the secret leaves the keyring; the address stays for the next sign-in.
    pub fn sign_out(&mut self, cx: &mut Context<Self>) {
        if self.signing_out {
            return;
        }
        self.signing_out = true;
        cx.notify();
        let task = cx.background_spawn(async move {
            std::process::Command::new(program())
                .arg("logout")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        });
        cx.spawn(async move |this, cx| {
            let done = task.await;
            let _ = this.update(cx, |a, cx| {
                a.signing_out = false;
                if done {
                    a.read_settings();
                    a.set_standing(Standing::NoLogin, cx);
                } else {
                    a.say("Could not sign out", cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// The sign-in screen. The password lives in its field only, until the attempt starts: then the
/// field is emptied (its undo history too) and the text goes to `melodie`'s input, dropped once
/// written.
pub struct SignInView {
    shared: Entity<Shared>,
    account: Entity<Account>,
    server: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    busy: bool,
    /// Why the last attempt failed, kept while it fades out.
    error: Option<SharedString>,
    /// Grows at every failure, so that each one is shown coming in.
    failures: u64,
    shown: bool,
    _subs: Vec<Subscription>,
}

impl SignInView {
    pub fn new(
        shared: Entity<Shared>,
        account: Entity<Account>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let server =
            cx.new(|cx| InputState::new(window, cx).placeholder("https://music.example.org"));
        let user = cx.new(|cx| InputState::new(window, cx).placeholder("User"));
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Password")
                .masked(true)
        });
        let subs = vec![
            cx.subscribe_in(&server, window, |this, _, ev: &InputEvent, window, cx| {
                match ev {
                    InputEvent::PressEnter { .. } => {
                        this.user.update(cx, |i, cx| i.focus(window, cx))
                    }
                    // the plain-http warning follows what is typed
                    InputEvent::Change => cx.notify(),
                    _ => {}
                }
            }),
            cx.subscribe_in(&user, window, |this, _, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    this.password.update(cx, |i, cx| i.focus(window, cx));
                }
            }),
            cx.subscribe_in(&password, window, |this, _, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    this.submit(window, cx);
                }
            }),
            // filled and focused each time the screen comes
            cx.observe_in(&account, window, |this, account, window, cx| {
                let shown = account.read(cx).screen(cx) == Screen::SignIn;
                if shown && !this.shown {
                    this.prepare(window, cx);
                }
                this.shown = shown;
            }),
        ];
        let mut this = Self {
            shared,
            account,
            server,
            user,
            password,
            busy: false,
            error: None,
            failures: 0,
            shown: false,
            _subs: subs,
        };
        if this.account.read(cx).screen(cx) == Screen::SignIn {
            this.shown = true;
            // after the root, which takes the focus when it is made
            cx.defer_in(window, |this, window, cx| this.prepare(window, cx));
        }
        this
    }

    pub fn has_focus(&self, window: &Window, cx: &App) -> bool {
        [&self.server, &self.user, &self.password]
            .iter()
            .any(|i| i.read(cx).focus_handle(cx).is_focused(window))
    }

    /// Puts back what the configuration keeps and focuses the first field to fill.
    fn prepare(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (server, user) = {
            let a = self.account.read(cx);
            (a.server.clone(), a.user.clone())
        };
        self.error = None;
        self.server.update(cx, |i, cx| {
            i.set_value(server.clone().unwrap_or_default(), window, cx)
        });
        self.user.update(cx, |i, cx| {
            i.set_value(user.clone().unwrap_or_default(), window, cx)
        });
        self.password
            .update(cx, |i, cx| i.set_value("", window, cx));
        let first = if server.is_none() {
            &self.server
        } else if user.is_none() {
            &self.user
        } else {
            &self.password
        };
        first.update(cx, |i, cx| i.focus(window, cx));
        cx.notify();
    }

    fn fail(&mut self, why: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.error = Some(why.into());
        self.failures += 1;
        cx.notify();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let checked =
            account::check_form(&self.server.read(cx).value(), &self.user.read(cx).value());
        let (server, user) = match checked {
            Ok(v) => v,
            Err(why) => return self.fail(why, cx),
        };
        let password = self.password.read(cx).value();
        let since = now_secs();
        // `set_value` drops the text and the undo history: the field no longer holds it
        self.password
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.busy = true;
        self.error = None;
        cx.notify();
        let task = cx.background_spawn(async move {
            let out = account::sign_in(&program(), &server, &user, &password);
            drop(password);
            out
        });
        cx.spawn_in(window, async move |this, cx| {
            let out = task.await;
            let _ = this.update_in(cx, |v, window, cx| {
                v.busy = false;
                match out {
                    Outcome::SignedIn => {
                        v.account.update(cx, |a, cx| a.signed_in(since, cx));
                    }
                    other => {
                        v.fail(other.message(), cx);
                        v.password.update(cx, |i, cx| i.focus(window, cx));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn field(
        &self,
        label: &'static str,
        icon: IconName,
        input: &Entity<InputState>,
        pal: Palette,
    ) -> impl IntoElement {
        v_flex()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(pal.secondary)
                    .child(label),
            )
            .child(
                Input::new(input)
                    .prefix(Icon::new(icon).size(px(16.)).text_color(pal.secondary))
                    .when(label == "PASSWORD", |i| i.mask_toggle())
                    .disabled(self.busy)
                    .h(px(42.))
                    .rounded_lg()
                    .bg(Hsla { a: 0.06, ..pal.fg }),
            )
    }
}

impl Render for SignInView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = palette(&self.shared, window, cx);
        let refused = self.account.read(cx).standing == Standing::AuthRefused
            || self.shared.read(cx).player.connection == Connection::AuthRefused;
        let e = motion::flag(
            "sign-in-error",
            self.error.is_some(),
            motion::ENTER,
            window,
            cx,
        );
        let clear = account::token_goes_in_clear(&self.server.read(cx).value());
        let w = motion::flag("sign-in-clear", clear, motion::ENTER, window, cx);
        let b = motion::flag("sign-in-busy", self.busy, motion::HOVER, window, cx);
        let view = cx.entity();
        div()
            .id("sign-in")
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                v_flex()
                    .w(px(380.))
                    .p_6()
                    .gap_4()
                    .rounded_2xl()
                    .bg(pal.surface)
                    .border_1()
                    .border_color(Hsla { a: 0.10, ..pal.fg })
                    .shadow_xl()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Sign in to Navidrome"),
                            )
                            .child(div().text_sm().text_color(pal.secondary).child(if refused {
                                "The server refused the saved login: sign in again."
                            } else {
                                "Your password is used once, to make a token for the \
                                     keyring, then forgotten."
                            })),
                    )
                    .child(self.field("SERVER", IconName::Server, &self.server, pal))
                    .when(w > 0.0, |d| {
                        d.child(
                            div()
                                .id("sign-in-clear")
                                .text_sm()
                                .text_color(pal.accent)
                                .opacity(w)
                                .mt(px(-6. * (1. - w)))
                                .child(
                                    "This address is plain http: the login would cross the \
                                     network in clear. Prefer https.",
                                ),
                        )
                    })
                    .child(self.field("USER", IconName::CircleUser, &self.user, pal))
                    .child(self.field("PASSWORD", IconName::KeyRound, &self.password, pal))
                    .when(e > 0.0, |d| {
                        d.child(
                            div()
                                .id(("sign-in-why", self.failures))
                                .text_sm()
                                .text_color(pal.accent)
                                .opacity(e)
                                .mt(px(-6. * (1. - e)))
                                .children(self.error.clone()),
                        )
                    })
                    .child(
                        h_flex().justify_end().child(
                            IconButton::new("sign-in-go", IconName::LogIn, pal)
                                .label(if self.busy {
                                    "Signing in…"
                                } else {
                                    "Sign in"
                                })
                                .primary()
                                .disabled(b > 0.5)
                                .on_click(move |_, window, cx| {
                                    view.update(cx, |v, cx| v.submit(window, cx))
                                }),
                        ),
                    ),
            )
    }
}

/// What the settings say of the account and of the local copy.
pub fn settings_section(account: &Entity<Account>, pal: Palette, cx: &App) -> impl IntoElement {
    let a = account.read(cx);
    let heading = |t: &'static str| {
        div()
            .px_2()
            .pt_3()
            .pb_1()
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(pal.secondary)
            .child(t)
    };
    let line = |main: SharedString, about: SharedString| {
        v_flex()
            .flex_1()
            .min_w_0()
            .child(div().text_sm().text_color(pal.fg).truncate().child(main))
            .child(
                div()
                    .text_xs()
                    .text_color(pal.secondary)
                    .truncate()
                    .child(about),
            )
    };
    let state = match a.standing {
        Standing::Ok => "Connected",
        Standing::Checking => "Checking…",
        Standing::Unreachable => "Unreachable",
        Standing::NoLogin => "Signed out",
        Standing::AuthRefused => "Login refused",
    };
    let (out, sync) = (account.clone(), account.clone());
    let synced = if let Some(so_far) = a.copying {
        account::copy_notice(so_far)
    } else if a.syncing {
        "Syncing…".to_string()
    } else {
        account::synced_text(a.synced_at, now_secs())
    };
    v_flex()
        .child(heading("ACCOUNT"))
        .child(
            h_flex()
                .px_2()
                .gap_3()
                .items_center()
                .child(line(
                    a.server.clone().unwrap_or_default().into(),
                    format!("{} · {state}", a.user.as_deref().unwrap_or("no user")).into(),
                ))
                .child(
                    IconButton::new("sign-out", IconName::LogOut, pal)
                        .diameter(34.)
                        .label("Sign out")
                        .disabled(a.signing_out || a.user.is_none())
                        .on_click(move |_, _, cx| out.update(cx, |a, cx| a.sign_out(cx))),
                ),
        )
        .child(heading("LIBRARY"))
        .child(
            h_flex()
                .px_2()
                .gap_3()
                .items_center()
                .child(line("Local copy".into(), synced.into()))
                .child(
                    IconButton::new("sync-now", IconName::RefreshCw, pal)
                        .diameter(34.)
                        .label("Sync now")
                        .disabled(a.syncing || a.standing == Standing::NoLogin)
                        .on_click(move |_, _, cx| sync.update(cx, |a, cx| a.sync_now(cx))),
                ),
        )
}
