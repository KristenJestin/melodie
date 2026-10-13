//! Which key does what in the window, given the place and whether the search field has the
//! focus: kept away from GPUI so that it is tested.

use crate::browse::Place;

/// A key press, as far as the window cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Ctrl+F.
    Find,
    /// The `/` character.
    Slash,
    Escape,
    Space,
    Enter,
    Other,
}

impl Key {
    /// `key` is the key name and `key_char` the text it types, as GPUI gives them.
    pub fn read(ctrl: bool, key: &str, key_char: Option<&str>) -> Self {
        if ctrl && key == "f" {
            Self::Find
        } else if key_char == Some("/") {
            Self::Slash
        } else if key == "escape" {
            Self::Escape
        } else if key == "space" {
            Self::Space
        } else if key == "enter" {
            Self::Enter
        } else {
            Self::Other
        }
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
    /// The search field has the window focus.
    pub field_focused: bool,
    /// The queue or the settings card is open.
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
    CloseOverlays,
    CloseDialog,
    /// What the dialog's main button does.
    Confirm,
    ClearSelection,
    ClearField,
    Back,
    Forward,
    PlayPause,
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
            Key::Enter => Act::Confirm,
            _ => Act::Pass,
        };
    }
    match key {
        Key::Find => Act::OpenSearch,
        Key::Slash if !typing => Act::OpenSearch,
        Key::Escape if s.overlay_open => Act::CloseOverlays,
        Key::Escape if s.selection => Act::ClearSelection,
        // the field empties first, then the search is left
        Key::Escape if *s.here == Place::Search && !s.field_empty => Act::ClearField,
        Key::Escape => Act::Back,
        Key::Space if !typing => Act::PlayPause,
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
    fn keys_are_read_from_the_keystroke() {
        assert_eq!(Key::read(true, "f", Some("f")), Key::Find);
        assert_eq!(Key::read(false, "f", Some("f")), Key::Other);
        assert_eq!(Key::read(false, "/", Some("/")), Key::Slash);
        assert_eq!(Key::read(false, "escape", None), Key::Escape);
        assert_eq!(Key::read(false, "space", Some(" ")), Key::Space);
        assert_eq!(Key::read(false, "enter", None), Key::Enter);
    }
}
