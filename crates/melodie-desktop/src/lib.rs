//! The window of Mélodie: the library, the player and the queue. One more client of the daemon
//! (socket and event stream); it never plays sound and never holds a token or a stream address.

pub mod account;
pub mod bar;
pub mod browse;
pub mod card;
pub mod covers;
pub mod credits;
pub mod edit;
pub mod figures;
pub mod find;
pub mod home;
pub mod keys;
pub mod launch;
pub mod layout;
pub mod link;
pub mod menu;
pub mod model;
pub mod playing;
pub mod playlists;
pub mod quick;
pub mod reveal;
#[cfg(test)]
mod scan;
pub mod selection;
pub mod table;
pub mod theme;
pub mod thumbs;
pub mod tint;
pub mod wave;
