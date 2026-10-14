//! What the round play button of a card asks for: what the "Play" of the right-click menu on the
//! same item asks for, because it is that line of the menu, not a copy of it.

use crate::browse::Place;
use crate::launch::Job;
use crate::menu::{self, Action, Effect, Item, Spot, Target};

/// The job of the play button on the card of `item`; `None` for an item the menu cannot play.
pub fn play_job(item: &Item) -> Option<Job> {
    let target = Target {
        item: item.clone(),
        spot: Spot::Library,
        // the place only decides which "go to" lines a menu offers
        here: Place::Home,
        starred: None,
    };
    match menu::effect(Action::Play, &target)? {
        Effect::Run(job) => Some(job),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::Kind;

    fn artist() -> Item {
        Item::Artist {
            id: "ar1".into(),
            name: "Ana".into(),
        }
    }

    /// What the menu's "Play" runs on `item`, asked the way the menu asks it.
    fn menus_play(item: &Item) -> Option<Job> {
        let target = Target {
            item: item.clone(),
            spot: Spot::Library,
            here: Place::Artists,
            starred: None,
        };
        match menu::effect(Action::Play, &target)? {
            Effect::Run(job) => Some(job),
            _ => None,
        }
    }

    #[test]
    fn the_artist_card_and_the_menu_ask_the_daemon_for_the_same_thing() {
        let item = artist();
        let job = play_job(&item).expect("an artist plays");
        assert_eq!(Some(job.clone()), menus_play(&item));
        // `melodie play artist -- ar1`: the artist's tracks, in order, not shuffled
        assert_eq!(
            job,
            Job::Play {
                kind: Kind::Artist,
                id: "ar1".into(),
                shuffle: false
            }
        );
        assert_eq!(job.args(), ["play", "artist", "--", "ar1"]);
    }

    #[test]
    fn the_card_plays_where_the_menu_was_opened_from_anywhere() {
        // the card is on the home and on the Artists page: the place does not change the job
        let item = artist();
        for here in [Place::Home, Place::Artists, Place::Search] {
            let target = Target {
                item: item.clone(),
                spot: Spot::Library,
                here,
                starred: Some(true),
            };
            assert_eq!(
                menu::effect(Action::Play, &target),
                play_job(&item).map(Effect::Run)
            );
        }
    }
}
