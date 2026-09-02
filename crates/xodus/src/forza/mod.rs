mod api;
mod model;

pub use api::{ForzaSession, XboxServiceError};
pub use model::{
    ForzaActivity, InviteActivationUri, ModelError, Presence, XboxFriend, Xuid,
    build_invite_accept_uri, parse_friends,
};

pub const FORZA_CLIENT_ID: &str = "000000004CC9D265";
pub const FORZA_TITLE_ID: u32 = 1_842_668_909;
pub const XBOX_RELYING_PARTY: &str = "http://xboxlive.com";
