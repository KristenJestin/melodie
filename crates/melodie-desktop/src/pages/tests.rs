// not `super::*`: it would bring the `test` macro of GPUI along
use super::{APPEARANCES_SHOWN, PageView, SelectionBar, and_more, unplayable_tip};
use crate::catalog::Catalog;
use gpui_kit::{AppContext as _, TestAppContext, VisualTestContext};
use melodie_core::db;
use melodie_core::env::Env;
use melodie_desktop::browse::Place;
use melodie_desktop::selection::How;
use std::time::Duration;

/// `ar9` featured on `featured` tracks of `ar1`; `ar8` credited on nothing.
fn copy(env: &Env, featured: usize) {
    let conn = db::create(&db::db_path(env)).unwrap();
    conn.execute_batch(
        "INSERT INTO artists(id, name, albumCount) VALUES ('ar1', 'Ada Marlow', 1),
           ('ar9', 'Lior Castell', 0), ('ar8', 'Nova Reyes', 0);
         INSERT INTO albums(id, name, artistId) VALUES ('al1', 'Lanterns', 'ar1');",
    )
    .unwrap();
    for n in 0..featured {
        conn.execute(
            "INSERT INTO tracks(id, title, artistId, albumId, track)
             VALUES (?1, ?1, 'ar1', 'al1', ?2)",
            (format!("t{n:03}"), n as i64),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO track_artists(track, artist) VALUES (?1, 'ar9')",
            [format!("t{n:03}")],
        )
        .unwrap();
    }
}

/// The page of the artist `id`, drawn without a screen: which of the elements the tests look
/// for are there.
fn drawn(featured: usize, id: &str, found: &[&'static str]) -> Vec<bool> {
    let dir = tempfile::tempdir().unwrap();
    let env = Env::with_home(dir.path());
    copy(&env, featured);
    let mut app = TestAppContext::single();
    let (shared, catalog) = app.update(|cx| {
        let shared = crate::app::for_tests(env.clone(), cx);
        let catalog = cx.new(|cx| Catalog::new(env.clone(), shared.clone(), cx));
        (shared, catalog)
    });
    catalog.update(&mut app, |c, cx| c.start_at(Place::Artist(id.into()), cx));
    app.run_until_parked();
    let (_, cx): (_, &mut VisualTestContext) =
        app.add_window_view(|_, _| PageView::new(shared.clone(), catalog.clone()));
    cx.run_until_parked();
    found
        .iter()
        .map(|selector| cx.debug_bounds(selector).is_some())
        .collect()
}

#[test]
fn at_most_a_hundred_appearances_are_drawn_and_the_rest_is_counted() {
    assert_eq!(APPEARANCES_SHOWN, 100);
    let rows = ["appears-track-0", "appears-track-99", "appears-track-100"];
    let found = [rows.as_slice(), &["appears-more"]].concat();
    assert_eq!(
        drawn(APPEARANCES_SHOWN + 50, "ar9", &found),
        [true, true, false, true]
    );
    // a list within the cap says nothing more
    assert_eq!(
        drawn(APPEARANCES_SHOWN, "ar9", &found),
        [true, true, false, false]
    );
    assert_eq!(
        and_more(APPEARANCES_SHOWN + 50).as_deref(),
        Some("and 50 more")
    );
    assert_eq!(and_more(APPEARANCES_SHOWN), None);
}

#[test]
fn an_artist_with_nothing_at_all_says_so_in_a_line_of_the_page() {
    let nothing = ["artist-nothing"];
    assert_eq!(drawn(2, "ar8", &nothing), [true]);
    assert_eq!(drawn(2, "ar9", &nothing), [false], "appears somewhere");
    assert_eq!(drawn(2, "ar1", &nothing), [false], "has an album");
}

#[test]
fn the_buttons_that_play_say_why_they_cannot_be_pressed() {
    assert_eq!(unplayable_tip(false), Some("Nothing to play"));
    assert_eq!(unplayable_tip(true), None);
}

/// The buttons the selection bar draws for three tracks picked in `place`, a list of a copy
/// with the playlist `p1` (the user's) holding `s0` to `s3`.
fn bar_of(place: Place, found: &[&'static str]) -> Vec<bool> {
    let dir = tempfile::tempdir().unwrap();
    let env = Env::with_home(dir.path());
    let conn = db::create(&db::db_path(&env)).unwrap();
    conn.execute_batch("INSERT INTO playlists(id, name, songCount) VALUES ('p1', 'Road', 4);")
        .unwrap();
    for n in 0..4 {
        conn.execute(
            "INSERT INTO tracks(id, title) VALUES (?1, ?1)",
            [format!("s{n}")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO playlist_tracks(playlist, pos, track) VALUES ('p1', ?1, ?2)",
            (n, format!("s{n}")),
        )
        .unwrap();
    }
    drop(conn);
    let mut app = TestAppContext::single();
    let (shared, catalog) = app.update(|cx| {
        let shared = crate::app::for_tests(env.clone(), cx);
        let catalog = cx.new(|cx| Catalog::new(env.clone(), shared.clone(), cx));
        (shared, catalog)
    });
    catalog.update(&mut app, |c, cx| c.start_at(place.clone(), cx));
    app.run_until_parked();
    catalog.update(&mut app, |c, _| {
        let shown = c.views[&place].clone();
        c.selection.click(&place, &shown, 0, How::Toggle);
        c.selection.click(&place, &shown, 2, How::Range);
    });
    let (_, cx): (_, &mut VisualTestContext) =
        app.add_window_view(|_, _| SelectionBar::new(shared.clone(), catalog.clone()));
    // the bar rises: it is drawn once it has begun to
    for _ in 0..10 {
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.run_until_parked();
    }
    found
        .iter()
        .map(|selector| cx.debug_bounds(selector).is_some())
        .collect()
}

#[test]
fn the_selection_bar_plays_shuffles_and_queues_the_tracks_picked() {
    let buttons = [
        "picked-play",
        "picked-shuffle",
        "picked-next",
        "picked-last",
        "picked-add",
    ];
    assert_eq!(bar_of(Place::Tracks, &buttons), [true; 5]);
    assert_eq!(bar_of(Place::Playlist("p1".into()), &buttons), [true; 5]);
}

#[test]
fn the_selection_bar_removes_only_in_a_playlist_of_the_user() {
    let remove = ["picked-remove"];
    assert_eq!(bar_of(Place::Tracks, &remove), [false]);
    assert_eq!(bar_of(Place::Playlist("p1".into()), &remove), [true]);
}
