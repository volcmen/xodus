mod api;
mod ipc;
mod model;
mod notification;
mod rta;

pub use api::{ForzaSession, XboxServiceError};
pub use ipc::{
    FriendAction, FriendView, InviteView, IpcError, MAX_SOCIAL_FRAME_BYTES, SOCIAL_API_MAGIC,
    SocialRequest, SocialResponse, SocialSnapshot, SocialStatus, decode_frame, encode_frame,
};
pub use model::{
    FORZA_CLIENT_ID, FORZA_TITLE_ID, ForzaActivity, ModelError, XBOX_RELYING_PARTY, XboxFriend,
    activity_connection_string, build_invite_accept_uri, parse_friends,
};
pub use notification::{
    MultiplayerInvite, SafeInvite, notification_resource_uri, parse_multiplayer_invite,
    registration_payload,
};
pub use rta::{RTA_SIGNING_URI, RTA_SUBPROTOCOL, RTA_URI, RtaEvent, RtaMachine, RtaProtocolError};
