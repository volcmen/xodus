use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc as std_mpsc;
use std::thread::{self, JoinHandle};

use thiserror::Error;
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{Mutex, mpsc, watch};
use xodus_social_protocol::{
    FrameWriter, ProtocolError, SocialRequest, SocialResponse, read_frame,
};

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

enum ClientTarget {
    DefaultSocket,
    Stream(StdUnixStream),
}

pub struct ClientWorker {
    requests: Option<mpsc::UnboundedSender<SocialRequest>>,
    shutdown: watch::Sender<bool>,
    thread: Option<JoinHandle<()>>,
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("Xodus social client worker has stopped")]
pub struct ClientWorkerStopped;

impl ClientWorker {
    #[must_use]
    pub fn start_default() -> (
        Self,
        std_mpsc::Receiver<Result<SocialResponse, ClientError>>,
    ) {
        Self::start(ClientTarget::DefaultSocket)
    }

    #[must_use]
    pub fn from_stream(
        stream: StdUnixStream,
    ) -> (
        Self,
        std_mpsc::Receiver<Result<SocialResponse, ClientError>>,
    ) {
        Self::start(ClientTarget::Stream(stream))
    }

    fn start(
        target: ClientTarget,
    ) -> (
        Self,
        std_mpsc::Receiver<Result<SocialResponse, ClientError>>,
    ) {
        let (requests, incoming) = mpsc::unbounded_channel();
        let (shutdown, cancellation) = watch::channel(false);
        let (outgoing, responses) = std_mpsc::channel();
        let thread = thread::spawn(move || run_worker(target, incoming, cancellation, outgoing));
        (
            Self {
                requests: Some(requests),
                shutdown,
                thread: Some(thread),
            },
            responses,
        )
    }

    pub fn request(&self, request: SocialRequest) -> Result<(), ClientWorkerStopped> {
        self.requests
            .as_ref()
            .ok_or(ClientWorkerStopped)?
            .send(request)
            .map_err(|_| ClientWorkerStopped)
    }

    pub fn stop(&mut self) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        let _ = self.shutdown.send(true);
        self.requests.take();
        let _ = thread.join();
    }
}

impl Drop for ClientWorker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_worker(
    target: ClientTarget,
    incoming: mpsc::UnboundedReceiver<SocialRequest>,
    cancellation: watch::Receiver<bool>,
    outgoing: std_mpsc::Sender<Result<SocialResponse, ClientError>>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            let _ = outgoing.send(Err(ClientError::Unavailable));
            return;
        }
    };
    runtime.block_on(worker_loop(target, incoming, cancellation, outgoing));
}

async fn worker_loop(
    target: ClientTarget,
    mut incoming: mpsc::UnboundedReceiver<SocialRequest>,
    mut cancellation: watch::Receiver<bool>,
    outgoing: std_mpsc::Sender<Result<SocialResponse, ClientError>>,
) {
    let client = tokio::select! {
        biased;
        _ = cancellation.changed() => return,
        client = connect_target(target) => match client {
            Ok(client) => client,
            Err(error) => {
                let _ = outgoing.send(Err(error));
                return;
            }
        },
    };

    loop {
        let request = tokio::select! {
            biased;
            _ = cancellation.changed() => return,
            request = incoming.recv() => match request {
                Some(request) => request,
                None => return,
            },
        };
        let response = tokio::select! {
            biased;
            _ = cancellation.changed() => return,
            response = client.request(request) => response,
        };
        let disconnected = response.is_err();
        if outgoing.send(response).is_err() || disconnected {
            return;
        }
    }
}

async fn connect_target(target: ClientTarget) -> Result<SocialClient, ClientError> {
    match target {
        ClientTarget::DefaultSocket => SocialClient::connect_default().await,
        ClientTarget::Stream(stream) => {
            stream
                .set_nonblocking(true)
                .map_err(|_| ClientError::Unavailable)?;
            UnixStream::from_std(stream)
                .map(SocialClient::from_stream)
                .map_err(|_| ClientError::Unavailable)
        }
    }
}
