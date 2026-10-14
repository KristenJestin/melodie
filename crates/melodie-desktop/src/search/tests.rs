// not `super::*`: it would bring the `test` macro of GPUI along
use super::SearchView;
use crate::app;
use crate::catalog::Catalog;
use gpui_kit::component::input::InputEvent;
use gpui_kit::{
    AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement, Modifiers,
    MouseButton, ParentElement as _, Render, Styled as _, TestAppContext, VisualTestContext,
    Window, div,
};
use melodie_core::db;
use melodie_core::env::Env;
use melodie_desktop::browse::Place;
use melodie_desktop::launch::Kind;
use serde_json::json;

/// The search page and the catalog it follows, as `main` builds them, under a root that takes
/// the focus from any press nobody prevented, as the window's root does.
struct Page {
    search: Entity<SearchView>,
    catalog: Entity<Catalog>,
    root: FocusHandle,
}

impl Render for Page {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("root")
            .track_focus(&self.root)
            .size_full()
            .child(self.search.clone())
    }
}

/// A local copy with six albums and two tracks about the rain, two albums about the snow.
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
        ("al6", "Rain Check"),
        ("al7", "Rainmaker"),
        ("al8", "Rain Man"),
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
        Page {
            search,
            catalog,
            root: cx.focus_handle(),
        }
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

    // "See all" of the albums
    go(&page, Place::Found(Kind::Album), cx);
    let (here, text, found) = shown(&page, cx);
    assert_eq!(here, Place::Found(Kind::Album));
    assert_eq!(view(&page, cx), before, "the page is not opened anew");
    assert_eq!(text, "rain", "the same field, the same text");
    assert_eq!(found, Some((Kind::Album, 6)), "the albums found");

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

/// The page with the text "rain" typed in, as `open` and `type_in` leave it.
fn rain<'a>(
    dir: &std::path::Path,
    app: &'a mut TestAppContext,
) -> (Entity<Page>, &'a mut VisualTestContext) {
    let (page, cx) = open(app, &library(dir));
    go(&page, Place::Search, cx);
    type_in(&page, "rain", cx);
    (page, cx)
}

/// Presses and releases the left button on the element drawn under `selector`.
fn click(selector: &'static str, cx: &mut VisualTestContext) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is not drawn"));
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
}

fn field_has_focus(page: &Entity<Page>, cx: &mut VisualTestContext) -> bool {
    let search = page.read_with(cx, |p, _| p.search.clone());
    cx.update(|window, cx| search.read(cx).has_focus(window, cx))
}

fn focus_field(page: &Entity<Page>, cx: &mut VisualTestContext) {
    let search = page.read_with(cx, |p, _| p.search.clone());
    cx.update(|window, cx| search.update(cx, |s, cx| s.focus(window, cx)));
    cx.run_until_parked();
    assert!(field_has_focus(page, cx), "the field starts with the focus");
}

#[test]
fn the_chips_are_drawn_and_a_click_goes_to_the_filter() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = TestAppContext::single();
    let (page, cx) = rain(dir.path(), &mut app);
    for chip in [
        "search-filter-All",
        "search-filter-Tracks",
        "search-filter-Albums",
        "search-filter-Artists",
        "search-filter-Playlists",
    ] {
        assert!(cx.debug_bounds(chip).is_some(), "{chip} is drawn");
    }

    click("search-filter-Albums", cx);
    let (here, text, found) = shown(&page, cx);
    assert_eq!(here, Place::Found(Kind::Album));
    assert_eq!(text, "rain", "the text stays");
    assert_eq!(found, Some((Kind::Album, 6)));

    click("search-filter-Tracks", cx);
    assert_eq!(shown(&page, cx).0, Place::Found(Kind::Track));
}

#[test]
fn see_all_is_a_click_on_the_group_and_leaves_the_field_focused() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = TestAppContext::single();
    let (page, cx) = rain(dir.path(), &mut app);
    focus_field(&page, cx);

    // six albums found, five shown
    click("found-album-all", cx);
    let (here, text, found) = shown(&page, cx);
    assert_eq!(here, Place::Found(Kind::Album));
    assert_eq!(text, "rain");
    assert_eq!(found, Some((Kind::Album, 6)));
    assert!(
        field_has_focus(&page, cx),
        "the press took the focus from the field"
    );
}

#[test]
fn a_chip_press_leaves_the_field_focused() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = TestAppContext::single();
    let (page, cx) = rain(dir.path(), &mut app);
    focus_field(&page, cx);

    click("search-filter-Tracks", cx);
    assert_eq!(shown(&page, cx).0, Place::Found(Kind::Track));
    assert!(
        field_has_focus(&page, cx),
        "the press took the focus from the field"
    );
}

#[test]
fn the_title_of_a_filter_counts_what_was_found() {
    assert_eq!(super::count_label(36, "snow"), "36 for “snow”");
    assert_eq!(super::count_label(0, "snow"), "0 for “snow”");

    let dir = tempfile::tempdir().unwrap();
    let mut app = TestAppContext::single();
    let (page, cx) = rain(dir.path(), &mut app);
    click("search-filter-Albums", cx);
    assert!(
        cx.debug_bounds("found-title").is_some(),
        "the title is drawn"
    );
    // the number in it is the one of the list: the albums found, not the five of the group
    assert_eq!(shown(&page, cx).2, Some((Kind::Album, 6)));
}

#[test]
fn going_back_from_a_filter_to_all_leaves_the_cursor_in_the_field() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = TestAppContext::single();
    let (page, cx) = rain(dir.path(), &mut app);
    focus_field(&page, cx);
    go(&page, Place::Found(Kind::Album), cx);

    // what the window's root does with a press on the mouse's back button
    let root = page.read_with(cx, |p, _| p.root.clone());
    cx.update(|window, cx| window.focus(&root, cx));
    assert!(!field_has_focus(&page, cx));

    let catalog = page.read_with(cx, |p, _| p.catalog.clone());
    catalog.update(cx, |c, cx| c.back(cx));
    cx.run_until_parked();
    assert_eq!(shown(&page, cx).0, Place::Search);
    assert!(field_has_focus(&page, cx), "the field lost the cursor");
}

#[test]
fn pressing_the_back_button_keeps_the_cursor_in_the_field() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = TestAppContext::single();
    let (page, cx) = rain(dir.path(), &mut app);
    go(&page, Place::Found(Kind::Album), cx);
    focus_field(&page, cx);

    let at = cx.debug_bounds("search-back").expect("drawn").center();
    cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::none());
    assert!(
        field_has_focus(&page, cx),
        "the press alone took the focus from the field"
    );
    cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(shown(&page, cx).0, Place::Search);
    assert!(field_has_focus(&page, cx));
}
