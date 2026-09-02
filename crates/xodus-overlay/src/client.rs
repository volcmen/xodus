use std::path::{Path, PathBuf};
use std::sync::Arc;

use thiserror::Error;
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::Mutex;
use xodus::forza::ipc::{FrameWriter, ProtocolError, read_frame};
use xodus::forza::{SocialRequest, SocialResponse};

struct Transport {
    reader: OwnedReadHalf,
    writer: FrameWriter<OwnedWriteHalf>,
}

#[derive(Clone)]
pub struct SocialClient {
    transport: Arc<Mutex<Transport>>,
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ClientError {
    #[error("Xodus social service is unavailable")]
    Unavailable,
    #[error("Xodus social service disconnected")]
    Disconnected,
    #[error("Xodus social service returned an invalid response")]
    InvalidResponse,
}

impl SocialClient {
    #[must_use]
    pub fn from_stream(stream: UnixStream) -> Self {
        let (reader, writer) = stream.into_split();
        Self {
            transport: Arc::new(Mutex::new(Transport {
                reader,
                writer: FrameWriter::new(writer),
            })),
        }
    }

    pub async fn connect(path: impl AsRef<Path>) -> Result<Self, ClientError> {
        UnixStream::connect(path)
            .await
            .map(Self::from_stream)
            .map_err(|_| ClientError::Unavailable)
    }

    pub async fn connect_default() -> Result<Self, ClientError> {
        Self::connect(default_socket_path()?).await
    }

    pub async fn request(&self, request: SocialRequest) -> Result<SocialResponse, ClientError> {
        let mut transport = self.transport.lock().await;
        transport
            .writer
            .write(&request)
            .await
            .map_err(|_| ClientError::Disconnected)?;
        read_frame(&mut transport.reader)
            .await
            .map_err(|error| match error {
                ProtocolError::Io(_) | ProtocolError::Truncated => ClientError::Disconnected,
                _ => ClientError::InvalidResponse,
            })
    }
}

fn default_socket_path() -> Result<PathBuf, ClientError> {
    if let Some(path) = std::env::var_os("XODUS_SOCKET") {
        return Ok(PathBuf::from(path));
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or(ClientError::Unavailable)?;
    Ok(PathBuf::from(runtime).join("xodus.sock"))
}
