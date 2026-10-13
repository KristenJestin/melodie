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
        } else {
            Self::Other
        }
    }
}

/// What the window knows when a key is pressed.
#[derive(Debug, Clone, Copy)]
pub struct Situation<'a> {
    pub here: &'a Place,
    /// The search field has the window focus.
    pub field_focused: bool,
    /// The queue or the settings card is open.
    pub overlay_open: bool,
    pub picker_open: bool,
    pub field_empty: bool,
}

/// What the key makes the window do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    OpenSearch,
    CloseOverlays,
    ClosePicker,
    ClearField,
    Back,
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
    let typing = typing(s.here, s.field_focused);
    match key {
        Key::Find => Act::OpenSearch,
        Key::Slash if !typing => Act::OpenSearch,
        Key::Escape if s.overlay_open => Act::CloseOverlays,
        Key::Escape if s.picker_open => Act::ClosePicker,
        // the field empties first, then the search is left
        Key::Escape if *s.here == Place::Search && !s.field_empty => Act::ClearField,
        Key::Escape => Act::Back,
        Key::Space if !typing => Act::PlayPause,
        _ => Act::Pass,
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
            picker_open: false,
            field_empty: true,
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
            picker_open: true,
            field_empty: false,
            ..at(&Place::Search, true)
        };
        assert_eq!(act(Key::Escape, open), Act::CloseOverlays);
        let picker = Situation {
            overlay_open: false,
            ..open
        };
        assert_eq!(act(Key::Escape, picker), Act::ClosePicker);
    }

    #[test]
    fn keys_are_read_from_the_keystroke() {
        assert_eq!(Key::read(true, "f", Some("f")), Key::Find);
        assert_eq!(Key::read(false, "f", Some("f")), Key::Other);
        assert_eq!(Key::read(false, "/", Some("/")), Key::Slash);
        assert_eq!(Key::read(false, "escape", None), Key::Escape);
        assert_eq!(Key::read(false, "space", Some(" ")), Key::Space);
    }
}
