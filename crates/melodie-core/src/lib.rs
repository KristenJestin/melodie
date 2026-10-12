//! What every Mélodie program shares: the Subsonic client, the local copy of the library, its
//! synchronisation, search, mixes, playlists, likes and the keyring store. The daemon, the
//! command line and, later, a window are built on it.

pub mod address;
pub mod api;
pub mod cover;
pub mod db;
pub mod env;
#[cfg(any(test, feature = "fake"))]
pub mod fake;
pub mod likes;
pub mod mix;
pub mod playlists;
pub mod prefs;
pub mod proto;
pub mod resolve;
pub mod search;
pub mod state;
pub mod store;
pub mod sync;
pub mod time;
pub mod track;
