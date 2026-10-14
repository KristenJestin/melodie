//! Which key does what in the window, given the place and whether the search field has the
//! focus: kept away from GPUI so that it is tested.

use crate::browse::Place;
use crate::quick::How;

/// A key press, as far as the window cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Ctrl+F.
    Find,
    /// Ctrl+K: the search palette.
    Palette,
    /// The `/` character.
    Slash,
    Escape,
    Space,
    Enter,
    ShiftEnter,
    CtrlEnter,
    Up,
    Down,
    /// The `q` character: shows or hides the queue.
    Queue,
    /// The menu key, or Shift+F10: the menu of the row picked.
    Menu,
    Other,
}

impl Key {
    /// `key` is the key name and `key_char` the text it types, as GPUI gives them.
    pub fn read(ctrl: bool, shift: bool, key: &str, key_char: Option<&str>) -> Self {
        if ctrl && key == "f" {
            Self::Find
        } else if ctrl && key == "k" {
            Self::Palette
        } else if key_char == Some("/") {
            Self::Slash
        } else if key == "escape" {
            Self::Escape
        } else if key == "space" {
            Self::Space
        } else if key == "enter" && ctrl {
            Self::CtrlEnter
        } else if key == "enter" && shift {
            Self::ShiftEnter
        } else if key == "enter" {
            Self::Enter
        } else if key == "up" {
            Self::Up
        } else if key == "down" {
            Self::Down
        } else if !ctrl && key_char == Some("q") {
            Self::Queue
        } else {
            Self::Other
        }
    }
}

impl Key {
    /// The keys that open the menu of a row, as a right click does.
    pub fn opens_menu(shift: bool, key: &str) -> bool {
        key == "menu" || (shift && key == "f10")
    }
}

/// The mouse buttons that move through the places, as far as the window cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Thumb {
    /// The thumb button towards the back of the mouse.
    Back,
    Forward,
}

/// What the window knows when a key is pressed.
#[derive(Debug, Clone, Copy)]
pub struct Situation<'a> {
    pub here: &'a Place,
    /// The search palette is open, over the current place.
    pub palette_open: bool,
    /// A context menu is open, over everything else.
    pub menu_open: bool,
    /// The search field has the window focus.
    pub field_focused: bool,
    /// The settings card is open, or the queue is shown over the main area (a narrow window).
    pub overlay_open: bool,
    /// A dialog (a playlist to choose, a name, a confirmation) is open: it is modal.
    pub dialog_open: bool,
    /// Rows of the current list are picked.
    pub selection: bool,
    pub field_empty: bool,
    /// The sign-in screen covers the window.
    pub sign_in: bool,
    /// The focus is in one of the sign-in screen's fields.
    pub sign_in_field_focused: bool,
}

/// What the key makes the window do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    OpenSearch,
    /// Opens the search palette, or closes it.
    TogglePalette,
    /// Moves through the palette's results.
    PaletteUp,
    PaletteDown,
    /// Runs one of the actions of the palette's result.
    PaletteRun(How),
    /// Closes the context menu, and opens the search palette in its place.
    MenuToPalette,
    /// Closes the context menu.
    CloseMenu,
    CloseOverlays,
    CloseDialog,
    /// What the dialog's main button does.
    Confirm,
    ClearSelection,
    ClearField,
    Back,
    Forward,
    PlayPause,
    /// Show or hide the queue.
    ToggleQueue,
    /// The menu of the row picked.
    OpenMenu,
    /// Plays the first track picked, as a double click on it does.
    PlayPicked,
    /// Not ours: the key goes on (typed in the field).
    Pass,
}

/// The field is shown on the search and on its "see all" pages only; elsewhere it cannot be
/// typed in, whatever the focus still says.
pub fn typing(here: &Place, field_focused: bool) -> bool {
    field_focused && in_search(here)
}

pub fn in_search(here: &Place) -> bool {
    matches!(here, Place::Search | Place::Found(_))
}

/// What a right click does on the open palette: nothing inside it, whatever the row; outside it
/// the palette closes, as a left click does, and no menu opens on what was behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteClick {
    Keep,
    Close,
}

pub fn palette_right_click(inside: bool) -> PaletteClick {
    if inside {
        PaletteClick::Keep
    } else {
        PaletteClick::Close
    }
}

pub fn act(key: Key, s: Situation) -> Act {
    // The sign-in screen covers the window and keeps every key but one: Space still plays and
    // pauses what the daemon plays on after a sign out, unless it is typed in a field.
    if s.sign_in {
        return match key {
            Key::Space if !s.sign_in_field_focused => Act::PlayPause,
            _ => Act::Pass,
        };
    }
    let typing = typing(s.here, s.field_focused);
    if s.dialog_open {
        return match key {
            Key::Escape => Act::CloseDialog,
            // as before the modifiers were read
            Key::Enter | Key::ShiftEnter | Key::CtrlEnter => Act::Confirm,
            _ => Act::Pass,
        };
    }
    // drawn above everything: its own keys go to it, Escape closes it before anything else,
    // and the palette takes its place
    if s.menu_open {
        return match key {
            Key::Palette => Act::MenuToPalette,
            Key::Escape => Act::CloseMenu,
            _ => Act::Pass,
        };
    }
    // over the current place: its keys first, and what is typed goes to its field
    if s.palette_open {
        return match key {
            Key::Palette | Key::Escape => Act::TogglePalette,
            Key::Up => Act::PaletteUp,
            Key::Down => Act::PaletteDown,
            Key::Enter => Act::PaletteRun(How::Main),
            Key::ShiftEnter => Act::PaletteRun(How::Queue),
            Key::CtrlEnter => Act::PaletteRun(How::Other),
            _ => Act::Pass,
        };
    }
    match key {
        Key::Palette => Act::TogglePalette,
        Key::Find => Act::OpenSearch,
        Key::Slash if !typing => Act::OpenSearch,
        Key::Escape if s.overlay_open => Act::CloseOverlays,
        Key::Escape if s.selection => Act::ClearSelection,
        // the field empties first, then the search is left
        Key::Escape if *s.here == Place::Search && !s.field_empty => Act::ClearField,
        Key::Escape => Act::Back,
        Key::Space if !typing => Act::PlayPause,
        Key::Queue if !typing => Act::ToggleQueue,
        Key::Menu if s.selection => Act::OpenMenu,
        // the field's own Enter while it has the focus: the list needs no focus of its own
        Key::Enter if s.selection && !typing => Act::PlayPicked,
        _ => Act::Pass,
    }
}

/// What a mouse button makes the window do: the same as Escape for back, once what is open is
/// closed, and forward only when `Nav` has a page ahead.
pub fn pointer_act(button: Thumb, s: Situation) -> Act {
    if s.sign_in || s.dialog_open {
        return Act::Pass;
    }
    match button {
        Thumb::Back if s.overlay_open => Act::CloseOverlays,
        Thumb::Back => Act::Back,
        Thumb::Forward if s.overlay_open => Act::Pass,
        Thumb::Forward => Act::Forward,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::Kind;

    fn at(here: &Place, field_focused: bool) -> Situation<'_> {
        Situation {
            here,
            palette_open: false,
            menu_open: false,
            field_focused,
            overlay_open: false,
            dialog_open: false,
            selection: false,
            field_empty: true,
            sign_in: false,
            sign_in_field_focused: false,
        }
    }

    #[test]
    fn space_plays_and_pauses_again_after_leaving_the_search() {
        // the field kept the focus when a sidebar entry was clicked
        assert_eq!(act(Key::Space, at(&Place::Albums, true)), Act::PlayPause);
        assert_eq!(
            act(Key::Space, at(&Place::Album("al1".into()), true)),
            Act::PlayPause
        );
    }

    #[test]
    fn space_still_plays_and_pauses_under_the_sign_in_screen_outside_its_fields() {
        // after a sign out the music plays on, behind the screen
        let covered = |sign_in_field_focused| Situation {
            sign_in: true,
            sign_in_field_focused,
            ..at(&Place::Albums, false)
        };
        assert_eq!(act(Key::Space, covered(false)), Act::PlayPause);
        // in a field it is a space of the address, the user or the password
        assert_eq!(act(Key::Space, covered(true)), Act::Pass);
        // the screen keeps every other key: nothing opens the search or goes back behind it
        for key in [Key::Find, Key::Slash, Key::Escape, Key::Enter, Key::Other] {
            assert_eq!(act(key, covered(false)), Act::Pass, "{key:?}");
            assert_eq!(act(key, covered(true)), Act::Pass, "{key:?}");
        }
    }

    #[test]
    fn the_mouse_back_and_forward_buttons_move_through_the_places() {
        for here in [Place::Home, Place::Album("al1".into()), Place::Search] {
            // the field's focus and a filled field do not matter: the mouse is not typing
            for focused in [false, true] {
                let s = Situation {
                    field_empty: false,
                    ..at(&here, focused)
                };
                assert_eq!(pointer_act(Thumb::Back, s), Act::Back);
                assert_eq!(pointer_act(Thumb::Forward, s), Act::Forward);
            }
        }
    }

    #[test]
    fn the_mouse_back_button_closes_the_queue_before_leaving_the_page() {
        let page = Place::Album("al1".into());
        let open = Situation {
            overlay_open: true,
            ..at(&page, false)
        };
        assert_eq!(pointer_act(Thumb::Back, open), Act::CloseOverlays);
        // forward has nothing to do behind a panel, and does not close it
        assert_eq!(pointer_act(Thumb::Forward, open), Act::Pass);
        // rows picked do not hold it: going back unpicks them with the page
        let picked = Situation {
            selection: true,
            ..at(&page, false)
        };
        assert_eq!(pointer_act(Thumb::Back, picked), Act::Back);
    }

    #[test]
    fn the_mouse_buttons_do_nothing_under_a_dialog_or_the_sign_in_screen() {
        let page = Place::Playlist("p1".into());
        let covered = [
            Situation {
                dialog_open: true,
                ..at(&page, false)
            },
            Situation {
                sign_in: true,
                ..at(&page, false)
            },
            Situation {
                dialog_open: true,
                overlay_open: true,
                ..at(&page, false)
            },
        ];
        for s in covered {
            assert_eq!(pointer_act(Thumb::Back, s), Act::Pass);
            assert_eq!(pointer_act(Thumb::Forward, s), Act::Pass);
        }
    }

    #[test]
    fn slash_opens_the_search_from_another_page_after_typing_in_it() {
        assert_eq!(act(Key::Slash, at(&Place::Albums, true)), Act::OpenSearch);
        assert_eq!(act(Key::Slash, at(&Place::Home, false)), Act::OpenSearch);
    }

    #[test]
    fn ctrl_f_focuses_the_search_from_anywhere() {
        for here in [
            Place::Player,
            Place::Albums,
            Place::Playlist("p1".into()),
            Place::Search,
            Place::Found(Kind::Track),
        ] {
            for focused in [false, true] {
                assert_eq!(act(Key::Find, at(&here, focused)), Act::OpenSearch);
            }
        }
    }

    #[test]
    fn space_in_the_field_is_text() {
        for here in [Place::Search, Place::Found(Kind::Album)] {
            assert_eq!(act(Key::Space, at(&here, true)), Act::Pass);
            assert_eq!(act(Key::Slash, at(&here, true)), Act::Pass);
        }
        // the field is on the search but the focus is elsewhere: Space plays
        assert_eq!(act(Key::Space, at(&Place::Search, false)), Act::PlayPause);
    }

    #[test]
    fn escape_clears_the_field_then_leaves_the_search() {
        let full = Situation {
            field_empty: false,
            ..at(&Place::Search, true)
        };
        assert_eq!(act(Key::Escape, full), Act::ClearField);
        assert_eq!(act(Key::Escape, at(&Place::Search, true)), Act::Back);
        // on a "see all" page, back goes to the search with its text
        let found = Situation {
            field_empty: false,
            ..at(&Place::Found(Kind::Track), true)
        };
        assert_eq!(act(Key::Escape, found), Act::Back);
    }

    #[test]
    fn escape_closes_what_is_open_before_anything_else() {
        let open = Situation {
            overlay_open: true,
            dialog_open: true,
            selection: true,
            field_empty: false,
            ..at(&Place::Search, true)
        };
        assert_eq!(act(Key::Escape, open), Act::CloseDialog);
        let overlay = Situation {
            dialog_open: false,
            ..open
        };
        assert_eq!(act(Key::Escape, overlay), Act::CloseOverlays);
        let picked = Situation {
            overlay_open: false,
            ..overlay
        };
        assert_eq!(act(Key::Escape, picked), Act::ClearSelection);
    }

    #[test]
    fn a_dialog_keeps_the_keys_to_itself() {
        let page = Place::Playlist("p1".into());
        let dialog = Situation {
            dialog_open: true,
            ..at(&page, false)
        };
        // Space and / are typed in its field, or do nothing behind it
        assert_eq!(act(Key::Space, dialog), Act::Pass);
        assert_eq!(act(Key::Slash, dialog), Act::Pass);
        assert_eq!(act(Key::Find, dialog), Act::Pass);
        assert_eq!(act(Key::Enter, dialog), Act::Confirm);
        assert_eq!(act(Key::Enter, at(&Place::Home, false)), Act::Pass);
    }

    #[test]
    fn escape_on_a_page_with_rows_picked_unpicks_them_before_going_back() {
        let page = Place::Playlist("p1".into());
        let picked = Situation {
            selection: true,
            ..at(&page, false)
        };
        assert_eq!(act(Key::Escape, picked), Act::ClearSelection);
        assert_eq!(act(Key::Escape, at(&page, false)), Act::Back);
    }

    #[test]
    fn q_shows_or_hides_the_queue_unless_it_is_typed() {
        for here in [Place::Player, Place::Albums, Place::Search] {
            assert_eq!(act(Key::Queue, at(&here, false)), Act::ToggleQueue);
        }
        // in the search field it is a letter
        assert_eq!(act(Key::Queue, at(&Place::Search, true)), Act::Pass);
        // the field kept the focus when a sidebar entry was clicked
        assert_eq!(act(Key::Queue, at(&Place::Albums, true)), Act::ToggleQueue);
        // shown over a narrow window, the same key hides it
        let over = Situation {
            overlay_open: true,
            ..at(&Place::Home, false)
        };
        assert_eq!(act(Key::Queue, over), Act::ToggleQueue);
        // a dialog and the sign-in screen keep it
        let dialog = Situation {
            dialog_open: true,
            ..at(&Place::Home, false)
        };
        assert_eq!(act(Key::Queue, dialog), Act::Pass);
        let covered = Situation {
            sign_in: true,
            ..at(&Place::Home, false)
        };
        assert_eq!(act(Key::Queue, covered), Act::Pass);
    }

    #[test]
    fn the_menu_key_or_shift_f10_opens_the_menu_of_the_row_picked() {
        assert!(Key::opens_menu(false, "menu"));
        assert!(Key::opens_menu(true, "f10"));
        assert!(!Key::opens_menu(false, "f10"));
        let picked = Situation {
            selection: true,
            ..at(&Place::Tracks, false)
        };
        assert_eq!(act(Key::Menu, picked), Act::OpenMenu);
        // nothing picked: nothing to open it on
        assert_eq!(act(Key::Menu, at(&Place::Tracks, false)), Act::Pass);
        // a dialog keeps it
        let dialog = Situation {
            dialog_open: true,
            ..picked
        };
        assert_eq!(act(Key::Menu, dialog), Act::Pass);
    }

    #[test]
    fn keys_are_read_from_the_keystroke() {
        assert_eq!(Key::read(false, false, "q", Some("q")), Key::Queue);
        assert_eq!(Key::read(true, false, "q", Some("q")), Key::Other);
        assert_eq!(Key::read(true, false, "f", Some("f")), Key::Find);
        assert_eq!(Key::read(false, false, "f", Some("f")), Key::Other);
        assert_eq!(Key::read(false, false, "/", Some("/")), Key::Slash);
        assert_eq!(Key::read(false, false, "escape", None), Key::Escape);
        assert_eq!(Key::read(false, false, "space", Some(" ")), Key::Space);
        assert_eq!(Key::read(false, false, "enter", None), Key::Enter);
    }

    fn places() -> [Place; 11] {
        [
            Place::Player,
            Place::Home,
            Place::Albums,
            Place::Artists,
            Place::Tracks,
            Place::Album("al1".into()),
            Place::Artist("ar1".into()),
            Place::Playlist("p1".into()),
            Place::Search,
            Place::Found(Kind::Track),
            Place::Found(Kind::Album),
        ]
    }

    #[test]
    fn ctrl_k_opens_and_closes_the_palette_from_every_place() {
        for here in places() {
            for focused in [false, true] {
                for (overlay_open, selection) in [(false, false), (true, false), (false, true)] {
                    let s = Situation {
                        overlay_open,
                        selection,
                        field_empty: false,
                        ..at(&here, focused)
                    };
                    assert_eq!(act(Key::Palette, s), Act::TogglePalette, "{here:?}");
                    let open = Situation {
                        palette_open: true,
                        ..s
                    };
                    assert_eq!(act(Key::Palette, open), Act::TogglePalette, "{here:?}");
                    // Escape closes it first, whatever else is open behind it
                    assert_eq!(act(Key::Escape, open), Act::TogglePalette, "{here:?}");
                }
            }
        }
    }

    #[test]
    fn the_palette_waits_behind_a_dialog_and_the_sign_in_screen() {
        let page = Place::Album("al1".into());
        let dialog = Situation {
            dialog_open: true,
            ..at(&page, false)
        };
        assert_eq!(act(Key::Palette, dialog), Act::Pass);
        let covered = Situation {
            sign_in: true,
            ..at(&page, false)
        };
        assert_eq!(act(Key::Palette, covered), Act::Pass);
    }

    #[test]
    fn the_arrows_and_enter_drive_the_open_palette() {
        for here in places() {
            let open = Situation {
                palette_open: true,
                field_empty: false,
                ..at(&here, false)
            };
            assert_eq!(act(Key::Up, open), Act::PaletteUp);
            assert_eq!(act(Key::Down, open), Act::PaletteDown);
            assert_eq!(act(Key::Enter, open), Act::PaletteRun(How::Main));
            assert_eq!(act(Key::ShiftEnter, open), Act::PaletteRun(How::Queue));
            assert_eq!(act(Key::CtrlEnter, open), Act::PaletteRun(How::Other));
            // what is typed goes to its field: a space, a slash, a q
            for key in [Key::Space, Key::Slash, Key::Queue, Key::Find, Key::Other] {
                assert_eq!(act(key, open), Act::Pass, "{key:?}");
            }
            // closed, the arrows are not ours
            let closed = at(&here, false);
            assert_eq!(act(Key::Up, closed), Act::Pass);
            assert_eq!(act(Key::Down, closed), Act::Pass);
        }
    }

    #[test]
    fn the_palette_keys_are_read_from_the_keystroke() {
        assert_eq!(Key::read(true, false, "k", Some("k")), Key::Palette);
        assert_eq!(Key::read(false, false, "k", Some("k")), Key::Other);
        assert_eq!(Key::read(false, false, "up", None), Key::Up);
        assert_eq!(Key::read(false, false, "down", None), Key::Down);
        assert_eq!(Key::read(false, true, "enter", None), Key::ShiftEnter);
        assert_eq!(Key::read(true, false, "enter", None), Key::CtrlEnter);
        assert_eq!(Key::read(false, false, "enter", None), Key::Enter);
    }

    #[test]
    fn ctrl_k_with_a_context_menu_open_closes_it_and_opens_the_palette() {
        for here in places() {
            let menu = Situation {
                menu_open: true,
                selection: true,
                ..at(&here, false)
            };
            assert_eq!(act(Key::Palette, menu), Act::MenuToPalette, "{here:?}");
        }
    }

    #[test]
    fn a_right_click_on_the_palette_opens_no_menu() {
        // on a row or on the card: nothing, and the click goes no further
        assert_eq!(palette_right_click(true), PaletteClick::Keep);
        // beside the card: it closes, like a left click, and the click goes no further
        assert_eq!(palette_right_click(false), PaletteClick::Close);
    }

    #[test]
    fn the_menu_key_does_nothing_while_the_palette_is_open() {
        for here in places() {
            let open = Situation {
                palette_open: true,
                selection: true,
                ..at(&here, false)
            };
            assert_eq!(act(Key::Menu, open), Act::Pass, "{here:?}");
        }
    }

    #[test]
    fn escape_closes_only_the_topmost_of_the_menu_and_the_palette() {
        for here in places() {
            // the menu is drawn above everything: it goes first, the queue, the selection
            // and the page stay
            let menu = Situation {
                menu_open: true,
                overlay_open: true,
                selection: true,
                field_empty: false,
                ..at(&here, false)
            };
            assert_eq!(act(Key::Escape, menu), Act::CloseMenu, "{here:?}");
            // then the palette, when it is the topmost
            let palette = Situation {
                menu_open: false,
                palette_open: true,
                ..menu
            };
            assert_eq!(act(Key::Escape, palette), Act::TogglePalette, "{here:?}");
        }
    }

    #[test]
    fn enter_plays_the_selections_first_track() {
        let page = Place::Album("al1".into());
        let picked = Situation {
            selection: true,
            ..at(&page, false)
        };
        assert_eq!(act(Key::Enter, picked), Act::PlayPicked);
        assert_eq!(
            act(Key::Enter, at(&page, false)),
            Act::Pass,
            "nothing picked"
        );
        // in the search, the field keeps its Enter while it has the focus
        for here in [Place::Search, Place::Found(Kind::Track)] {
            let typing = Situation {
                selection: true,
                ..at(&here, true)
            };
            assert_eq!(act(Key::Enter, typing), Act::Pass);
            let rows = Situation {
                selection: true,
                ..at(&here, false)
            };
            assert_eq!(act(Key::Enter, rows), Act::PlayPicked);
        }
        // a dialog, the menu or the palette take it first
        let menu = Situation {
            menu_open: true,
            ..picked
        };
        assert_eq!(act(Key::Enter, menu), Act::Pass);
        let dialog = Situation {
            dialog_open: true,
            ..picked
        };
        assert_eq!(act(Key::Enter, dialog), Act::Confirm);
    }
}
