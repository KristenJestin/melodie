// not `super::*`: it would bring the `test` macro of GPUI along
use super::SearchView;
use crate::app;
use crate::catalog::Catalog;
use gpui_kit::component::input::InputEvent;
use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, Render, TestAppContext, VisualTestContext,
    Window,
};
use melodie_core::db;
use melodie_core::env::Env;
use melodie_desktop::browse::Place;
use melodie_desktop::launch::Kind;
use serde_json::json;

/// The search page and the catalog it follows, as `main` builds them.
struct Page {
    search: Entity<SearchView>,
    catalog: Entity<Catalog>,
}

impl Render for Page {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.search.clone()
    }
}

/// A local copy with three albums and two tracks about the rain, two albums about the snow.
fn library(dir: &std::path::Path) -> Env {
    let env = Env::with_home(dir);
    let path = db::db_path(&env);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut conn = db::create(&path).unwrap();
    let tx = conn.transaction().unwrap();
    for (id, name) in [
        ("al1", "Rain Songs"),
        ("al2", "Rain Dance"),
        ("al3", "Winter Rain"),
        ("al4", "Snow Songs"),
        ("al5", "Snowfall"),
    ] {
        db::insert_album(&tx, &json!({"id": id, "name": name, "artist": "Nobody"})).unwrap();
    }
    for (id, title) in [("t1", "Rain on the roof"), ("t2", "Rainbow")] {
        db::insert_track(&tx, &json!({"id": id, "title": title})).unwrap();
    }
    tx.commit().unwrap();
    env
}

fn open<'a>(app: &'a mut TestAppContext, env: &Env) -> (Entity<Page>, &'a mut VisualTestContext) {
    app.update(gpui_kit::init);
    let env = env.clone();
    let socket = env.state_dir().join("no-daemon.sock");
    app.add_window_view(move |window, cx| {
        let (shared, _) = app::start(env.clone(), socket, window, cx);
        let catalog = cx.new(|cx| Catalog::new(env.clone(), shared.clone(), cx));
        let search = cx.new(|cx| SearchView::new(shared, catalog.clone(), env, window, cx));
        Page { search, catalog }
    })
}

/// Types `text` in the field, as the user does, and waits for the answer.
fn type_in(page: &Entity<Page>, text: &str, cx: &mut VisualTestContext) {
    let input = page.read_with(cx, |p, cx| p.search.read(cx).input.clone());
    cx.update(|window, cx| input.update(cx, |i, cx| i.set_value(text, window, cx)));
    input.update(cx, |_, cx| cx.emit(InputEvent::Change));
    cx.run_until_parked();
}

/// The filter and the text of the page, and how many of the filtered kind were found.
fn shown(page: &Entity<Page>, cx: &VisualTestContext) -> (Place, String, Option<(Kind, usize)>) {
    page.read_with(cx, |p, cx| {
        let search = p.search.read(cx);
        let found = search
            .answer
            .as_ref()
            .and_then(|a| a.all.as_ref().map(|(k, hits)| (*k, hits.len())));
        let field = search.input.read(cx).value().to_string();
        assert_eq!(field, search.text, "the field and the search agree");
        (p.catalog.read(cx).here().clone(), field, found)
    })
}

fn go(page: &Entity<Page>, to: Place, cx: &mut VisualTestContext) {
    let catalog = page.read_with(cx, |p, _| p.catalog.clone());
    catalog.update(cx, |c, cx| c.go(to, cx));
    cx.run_until_parked();
}

fn view(page: &Entity<Page>, cx: &VisualTestContext) -> u64 {
    page.read_with(cx, |p, cx| p.catalog.read(cx).nav.view)
}

#[test]
fn see_all_selects_the_filter_without_leaving_the_page() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = TestAppContext::single();
    let (page, cx) = open(&mut app, &library(dir.path()));
    go(&page, Place::Search, cx);
    type_in(&page, "rain", cx);
    let (here, text, found) = shown(&page, cx);
    assert_eq!((here, text.as_str(), found), (Place::Search, "rain", None));
    let before = view(&page, cx);

    // « See all » of the albums
    go(&page, Place::Found(Kind::Album), cx);
    let (here, text, found) = shown(&page, cx);
    assert_eq!(here, Place::Found(Kind::Album));
    assert!(here.in_search(), "still the search page");
    assert_eq!(view(&page, cx), before, "the page is not opened anew");
    assert_eq!(text, "rain", "the same field, the same text");
    assert_eq!(found, Some((Kind::Album, 3)), "the albums found");

    // another chip replaces the filter
    go(&page, Place::Found(Kind::Track), cx);
    let (here, text, found) = shown(&page, cx);
    assert_eq!((here, text.as_str()), (Place::Found(Kind::Track), "rain"));
    assert_eq!(found, Some((Kind::Track, 2)));
}

#[test]
fn back_returns_to_all_with_the_text() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = TestAppContext::single();
    let (page, cx) = open(&mut app, &library(dir.path()));
    go(&page, Place::Search, cx);
    type_in(&page, "rain", cx);
    go(&page, Place::Found(Kind::Album), cx);
    go(&page, Place::Found(Kind::Track), cx);

    let catalog = page.read_with(cx, |p, _| p.catalog.clone());
    catalog.update(cx, |c, cx| c.back(cx));
    cx.run_until_parked();
    let (here, text, _) = shown(&page, cx);
    assert_eq!((here, text.as_str()), (Place::Search, "rain"));
    assert!(
        !catalog.read_with(cx, |c, _| c.nav.can_go_back()),
        "All is the way's start"
    );
}

#[test]
fn a_new_text_keeps_the_filter() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = TestAppContext::single();
    let (page, cx) = open(&mut app, &library(dir.path()));
    go(&page, Place::Search, cx);
    type_in(&page, "rain", cx);
    go(&page, Place::Found(Kind::Album), cx);

    type_in(&page, "snow", cx);
    let (here, text, found) = shown(&page, cx);
    assert_eq!(here, Place::Found(Kind::Album), "the filter stays");
    assert_eq!(text, "snow");
    assert_eq!(
        found,
        Some((Kind::Album, 2)),
        "the albums found for the new text"
    );
}
