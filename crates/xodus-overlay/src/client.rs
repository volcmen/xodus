use std::path::{Path, PathBuf};

use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};
use xodus::forza::{
    MAX_SOCIAL_FRAME_BYTES, SOCIAL_API_MAGIC, SocialRequest, SocialResponse, decode_frame,
    encode_frame,
};

#[derive(Clone)]
pub struct SocialClient {
    socket_path: PathBuf,
}

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("Xodus service connection failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Xodus social protocol failed: {0}")]
    Protocol(#[from] xodus::forza::IpcError),
    #[error("Xodus social response is too large")]
    TooLarge,
}

impl SocialClient {
    pub fn new(socket_path: impl AsRef<Path>) -> Self {
        Self {
            socket_path: socket_path.as_ref().to_owned(),
        }
    }

    pub fn from_runtime_dir() -> Result<Self, std::env::VarError> {
        let runtime_dir = std::env::var("XDG_RUNTIME_DIR")?;
        Ok(Self::new(Path::new(&runtime_dir).join("xodus.sock")))
    }

    pub async fn request(&self, request: SocialRequest) -> Result<SocialResponse, ClientError> {
        let mut socket = UnixStream::connect(&self.socket_path).await?;
        socket.write_u32_le(SOCIAL_API_MAGIC).await?;
        socket.write_all(&encode_frame(&request)?).await?;

        let length = socket.read_u32_le().await? as usize;
        if length > MAX_SOCIAL_FRAME_BYTES {
            return Err(ClientError::TooLarge);
        }
        let mut payload = vec![0; length];
        socket.read_exact(&mut payload).await?;
        let mut frame = (length as u32).to_le_bytes().to_vec();
        frame.extend_from_slice(&payload);
        Ok(decode_frame(&frame)?)
    }
}

#[cfg(test)]
mod tests {
    use super::SocialClient;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::UnixListener,
    };
    use xodus::forza::{
        SOCIAL_API_MAGIC, SocialRequest, SocialResponse, SocialSnapshot, SocialStatus,
        decode_frame, encode_frame,
    };

    #[tokio::test]
    async fn exchanges_a_real_versioned_unix_socket_frame() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("xodus.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            assert_eq!(socket.read_u32_le().await.unwrap(), SOCIAL_API_MAGIC);
            let length = socket.read_u32_le().await.unwrap() as usize;
            let mut payload = vec![0; length];
            socket.read_exact(&mut payload).await.unwrap();
            let mut frame = (length as u32).to_le_bytes().to_vec();
            frame.extend_from_slice(&payload);
            assert_eq!(
                decode_frame::<SocialRequest>(&frame).unwrap(),
                SocialRequest::Snapshot
            );

            let response = SocialResponse::Snapshot(SocialSnapshot {
                status: SocialStatus::Connected,
                invitations: vec![],
                friends: vec![],
            });
            socket
                .write_all(&encode_frame(&response).unwrap())
                .await
                .unwrap();
        });

        let response = SocialClient::new(path)
            .request(SocialRequest::Snapshot)
            .await
            .unwrap();
        assert_eq!(
            response,
            SocialResponse::Snapshot(SocialSnapshot {
                status: SocialStatus::Connected,
                invitations: vec![],
                friends: vec![],
            })
        );
        server.await.unwrap();
    }
}
