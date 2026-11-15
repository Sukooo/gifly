//! One-shot import of Discord's favorite GIFs, from one of two places:
//!
//! - `local`: the copy of the account settings the Discord app keeps on this PC. No network.
//! - `api`: Discord's settings endpoint, using a token the user pastes in. The token is used for
//!   that one request and never saved or logged.
//!
//! Both hold the same "frecency" settings protobuf, decoded by `proto`.
//!
//! Also renews expired Discord file links (`renew_links`), with a bot token from Settings.

mod api;
mod local;
mod proto;

use std::collections::HashSet;

pub use api::{RENEW_MAX, RenewError, clean_bot_token, renew_links};
pub use local::remove_leftovers;

/// One favorite GIF as Discord stores it.
pub struct Fav {
    /// What Discord sends (usually a tenor.com/view or giphy.com link).
    pub url: String,
    /// The underlying media file (often an mp4).
    pub src: String,
    pub order: u64,
    pub width: u16,
    pub height: u16,
}

/// Where to read favorites from. Deliberately not `Debug`, so a token can't end up in a log.
pub enum Source {
    ThisPc,
    Token(String),
}

/// Reads the favorites, without duplicates. Errors are ready to show to the user.
pub fn favorites(source: Source) -> Result<Vec<Fav>, String> {
    let (mut favs, none_found) = match source {
        Source::ThisPc => {
            (local::favorites()?, "No favorites found. Open Discord, check you're signed in, then try again.")
        }
        Source::Token(token) => (api::favorites(&token)?, "That account has no favorite GIFs."),
    };
    let mut seen = HashSet::new();
    favs.retain(|f| seen.insert(f.url.clone()));
    if favs.is_empty() {
        return Err(none_found.into());
    }
    Ok(favs)
}
