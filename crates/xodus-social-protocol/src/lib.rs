use std::io;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::Mutex;
use uuid::Uuid;

const MAGIC: [u8; 4] = *b"XDSO";
const VERSION: u16 = 1;
const HEADER_BYTES: usize = 10;
pub const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct FriendHandle(Uuid);

impl FriendHandle {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for FriendHandle {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SocialPresence {
    Online,
    Offline,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SocialFriend {
    pub handle: FriendHandle,
    pub display_label: String,
    pub presence: SocialPresence,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SocialSnapshot {
    pub friends: Vec<SocialFriend>,
    pub status: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SocialRequest {
    Snapshot,
    RefreshFriends,
    InviteFriend { friend: FriendHandle },
    JoinFriend { friend: FriendHandle },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SocialResponse {
    Snapshot(SocialSnapshot),
    Success { message: String },
    Error { message: String },
}

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("invalid social protocol magic")]
    InvalidMagic,
    #[error("unsupported social protocol version")]
    UnsupportedVersion,
    #[error("social protocol frame exceeds the payload limit")]
    FrameTooLarge,
    #[error("truncated social protocol frame")]
    Truncated,
    #[error("social protocol payload length does not match the frame")]
    LengthMismatch,
    #[error("invalid social protocol payload")]
    InvalidPayload,
    #[error("social protocol I/O failed")]
    Io(#[source] io::Error),
}

pub fn encode_frame<T: Serialize>(value: &T) -> Result<Vec<u8>, ProtocolError> {
    let payload = serde_json::to_vec(value).map_err(|_| ProtocolError::InvalidPayload)?;
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(ProtocolError::FrameTooLarge);
    }
    let payload_length = u32::try_from(payload.len()).map_err(|_| ProtocolError::FrameTooLarge)?;
    let mut frame = Vec::with_capacity(HEADER_BYTES + payload.len());
    frame.extend_from_slice(&MAGIC);
    frame.extend_from_slice(&VERSION.to_le_bytes());
    frame.extend_from_slice(&payload_length.to_le_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

pub fn decode_frame<T: DeserializeOwned>(frame: &[u8]) -> Result<T, ProtocolError> {
    if frame.len() < HEADER_BYTES {
        return Err(ProtocolError::Truncated);
    }
    if frame[..4] != MAGIC {
        return Err(ProtocolError::InvalidMagic);
    }
    let version = u16::from_le_bytes(frame[4..6].try_into().expect("fixed version width"));
    if version != VERSION {
        return Err(ProtocolError::UnsupportedVersion);
    }
    let payload_length =
        u32::from_le_bytes(frame[6..10].try_into().expect("fixed payload length width")) as usize;
    if payload_length > MAX_PAYLOAD_BYTES {
        return Err(ProtocolError::FrameTooLarge);
    }
    if frame.len() - HEADER_BYTES != payload_length {
        return Err(ProtocolError::LengthMismatch);
    }
    serde_json::from_slice(&frame[HEADER_BYTES..]).map_err(|_| ProtocolError::InvalidPayload)
}

pub async fn read_frame<R, T>(reader: &mut R) -> Result<T, ProtocolError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut magic = [0_u8; 4];
    read_exact(reader, &mut magic).await?;
    read_frame_after_magic(reader, magic).await
}

pub async fn read_frame_after_magic<R, T>(
    reader: &mut R,
    magic: [u8; 4],
) -> Result<T, ProtocolError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    if magic != MAGIC {
        return Err(ProtocolError::InvalidMagic);
    }
    let mut header = [0_u8; 6];
    read_exact(reader, &mut header).await?;
    let version = u16::from_le_bytes(header[..2].try_into().expect("fixed version width"));
    if version != VERSION {
        return Err(ProtocolError::UnsupportedVersion);
    }
    let payload_length =
        u32::from_le_bytes(header[2..].try_into().expect("fixed payload length width")) as usize;
    if payload_length > MAX_PAYLOAD_BYTES {
        return Err(ProtocolError::FrameTooLarge);
    }
    let mut payload = vec![0_u8; payload_length];
    read_exact(reader, &mut payload).await?;
    serde_json::from_slice(&payload).map_err(|_| ProtocolError::InvalidPayload)
}

async fn read_exact<R>(reader: &mut R, bytes: &mut [u8]) -> Result<(), ProtocolError>
where
    R: AsyncRead + Unpin,
{
    reader.read_exact(bytes).await.map(|_| ()).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            ProtocolError::Truncated
        } else {
            ProtocolError::Io(error)
        }
    })
}

pub struct FrameWriter<W> {
    writer: Arc<Mutex<W>>,
}

impl<W> FrameWriter<W> {
    #[must_use]
    pub fn new(writer: W) -> Self {
        Self {
            writer: Arc::new(Mutex::new(writer)),
        }
    }
}

impl<W> Clone for FrameWriter<W> {
    fn clone(&self) -> Self {
        Self {
            writer: Arc::clone(&self.writer),
        }
    }
}

impl<W> FrameWriter<W>
where
    W: AsyncWrite + Unpin,
{
    pub async fn write<T: Serialize>(&self, value: &T) -> Result<(), ProtocolError> {
        let frame = encode_frame(value)?;
        self.writer
            .lock()
            .await
            .write_all(&frame)
            .await
            .map_err(ProtocolError::Io)
    }
}
