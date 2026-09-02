use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;

pub const SOCIAL_API_MAGIC: u32 = u32::from_le_bytes(*b"XDSO");
pub const MAX_SOCIAL_FRAME_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FriendAction {
    Invite,
    Join,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FriendView {
    pub id: String,
    pub gamertag: String,
    pub presence: String,
    pub action: FriendAction,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InviteView {
    pub id: String,
    pub sender_gamertag: String,
    pub title_name: String,
    pub expires_at_unix: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SocialStatus {
    WaitingForGame,
    Connecting,
    Connected,
    Reconnecting,
    Error,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SocialSnapshot {
    pub status: SocialStatus,
    pub invitations: Vec<InviteView>,
    pub friends: Vec<FriendView>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum SocialRequest {
    Snapshot,
    RefreshFriends,
    JoinInvite { invite_id: String },
    DismissInvite { invite_id: String },
    InviteFriend { friend_id: String },
    JoinFriend { friend_id: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "result", content = "data", rename_all = "snake_case")]
pub enum SocialResponse {
    Snapshot(SocialSnapshot),
    Success { message: String },
    Error { message: String },
}

#[derive(Debug, Error)]
pub enum IpcError {
    #[error("social IPC frame is too short")]
    TooShort,
    #[error("social IPC frame is too large")]
    TooLarge,
    #[error("social IPC frame length does not match payload")]
    LengthMismatch,
    #[error("social IPC JSON is invalid: {0}")]
    InvalidJson(#[from] serde_json::Error),
}

pub fn encode_frame<T: Serialize>(message: &T) -> Result<Vec<u8>, IpcError> {
    let payload = serde_json::to_vec(message)?;
    if payload.len() > MAX_SOCIAL_FRAME_BYTES {
        return Err(IpcError::TooLarge);
    }
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

pub fn decode_frame<T: DeserializeOwned>(frame: &[u8]) -> Result<T, IpcError> {
    let length_bytes: [u8; 4] = frame
        .get(..4)
        .ok_or(IpcError::TooShort)?
        .try_into()
        .expect("four-byte slice");
    let length = u32::from_le_bytes(length_bytes) as usize;
    if length > MAX_SOCIAL_FRAME_BYTES {
        return Err(IpcError::TooLarge);
    }
    let payload = frame.get(4..).ok_or(IpcError::TooShort)?;
    if payload.len() != length {
        return Err(IpcError::LengthMismatch);
    }
    Ok(serde_json::from_slice(payload)?)
}

#[cfg(test)]
mod tests {
    use super::{
        FriendAction, FriendView, InviteView, SocialRequest, SocialResponse, SocialSnapshot,
        SocialStatus, decode_frame, encode_frame,
    };

    #[test]
    fn request_and_response_frames_round_trip() {
        let request = SocialRequest::JoinInvite {
            invite_id: "opaque-event-id".into(),
        };
        assert_eq!(
            decode_frame::<SocialRequest>(&encode_frame(&request).unwrap()).unwrap(),
            request
        );

        let response = SocialResponse::Snapshot(SocialSnapshot {
            status: SocialStatus::Connected,
            invitations: vec![InviteView {
                id: "opaque-event-id".into(),
                sender_gamertag: "SyntheticFriend".into(),
                title_name: "Forza Motorsport".into(),
                expires_at_unix: 1_788_120_930,
            }],
            friends: vec![FriendView {
                id: "opaque-friend-id".into(),
                gamertag: "SyntheticFriend".into(),
                presence: "Online in Forza Motorsport".into(),
                action: FriendAction::Join,
            }],
        });
        assert_eq!(
            decode_frame::<SocialResponse>(&encode_frame(&response).unwrap()).unwrap(),
            response
        );
    }

    #[test]
    fn safe_frames_have_no_connection_or_token_material() {
        let response = SocialResponse::Snapshot(SocialSnapshot {
            status: SocialStatus::Connected,
            invitations: vec![InviteView {
                id: "event-7".into(),
                sender_gamertag: "SyntheticFriend".into(),
                title_name: "Forza Motorsport".into(),
                expires_at_unix: 1_788_120_930,
            }],
            friends: vec![],
        });
        let encoded = String::from_utf8(encode_frame(&response).unwrap()[4..].to_vec()).unwrap();
        assert!(!encoded.contains("connectionString"));
        assert!(!encoded.contains("XBL3.0"));
        assert!(!encoded.contains("Signature"));
    }

    #[test]
    fn rejects_truncated_oversized_and_unknown_frames() {
        assert!(decode_frame::<SocialRequest>(&[1, 0, 0]).is_err());

        let mut oversized = vec![0u8; 4];
        oversized.copy_from_slice(&(300_000u32).to_le_bytes());
        assert!(decode_frame::<SocialRequest>(&oversized).is_err());

        let json = br#"{"action":"future_action"}"#;
        let mut unknown = (json.len() as u32).to_le_bytes().to_vec();
        unknown.extend_from_slice(json);
        assert!(decode_frame::<SocialRequest>(&unknown).is_err());
    }
}
