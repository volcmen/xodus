use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use xodus::forza::InviteActivationUri;

pub(crate) const MAX_ACTIVATION_URI_BYTES: usize = 4 * 1024;
// Keep at most one pending activation so a stalled subscriber cannot accumulate URIs.
const SUBSCRIBER_QUEUE_CAPACITY: usize = 1;
const PUBLISH_OK: u8 = 0;
const PUBLISH_FAILED: u8 = 1;

struct Subscriber {
    id: u64,
    sender: mpsc::Sender<InviteActivationUri>,
}

#[derive(Default)]
struct RelayInner {
    next_id: AtomicU64,
    subscriber: Mutex<Option<Subscriber>>,
}

#[derive(Clone, Default)]
pub(crate) struct InviteActivationRelay {
    inner: Arc<RelayInner>,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum PublishError {
    InvalidLength,
    NoSubscriber,
    Closed,
    Full,
}

#[derive(Debug)]
pub(crate) enum SubscribeError {
    AlreadyRegistered,
}

impl std::fmt::Display for SubscribeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a game activation subscriber is already registered")
    }
}

impl std::error::Error for SubscribeError {}

impl InviteActivationRelay {
    pub(crate) fn register(
        &self,
    ) -> Result<(u64, mpsc::Receiver<InviteActivationUri>), SubscribeError> {
        let mut subscriber = self.inner.subscriber.lock().expect("activation relay lock");
        if subscriber.is_some() {
            return Err(SubscribeError::AlreadyRegistered);
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let (sender, receiver) = mpsc::channel(SUBSCRIBER_QUEUE_CAPACITY);
        *subscriber = Some(Subscriber { id, sender });
        Ok((id, receiver))
    }

    pub(crate) fn publish(&self, uri: InviteActivationUri) -> Result<(), PublishError> {
        if !(1..=MAX_ACTIVATION_URI_BYTES).contains(&uri.expose_for_activation().len()) {
            return Err(PublishError::InvalidLength);
        }
        let mut subscriber = self.inner.subscriber.lock().expect("activation relay lock");
        let Some(active) = subscriber.as_ref() else {
            return Err(PublishError::NoSubscriber);
        };
        match active.sender.try_send(uri) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => Err(PublishError::Full),
            Err(mpsc::error::TrySendError::Closed(_)) => {
                *subscriber = None;
                Err(PublishError::Closed)
            }
        }
    }

    fn unregister(&self, id: u64) {
        let mut subscriber = self.inner.subscriber.lock().expect("activation relay lock");
        if subscriber.as_ref().is_some_and(|active| active.id == id) {
            *subscriber = None;
        }
    }

    #[cfg(test)]
    pub(crate) fn has_subscriber(&self) -> bool {
        self.inner
            .subscriber
            .lock()
            .expect("activation relay lock")
            .is_some()
    }
}

pub(crate) async fn handle_subscriber(
    socket: &mut tokio::net::UnixStream,
    relay: &InviteActivationRelay,
    cancellation: &CancellationToken,
) -> io::Result<()> {
    let (id, mut activations) = relay.register().map_err(io::Error::other)?;
    let result = loop {
        let mut disconnected = [0_u8; 1];
        tokio::select! {
            _ = cancellation.cancelled() => break Ok(()),
            read = socket.read(&mut disconnected) => {
                break read.map(|_| ());
            }
            activation = activations.recv() => {
                let Some(activation) = activation else {
                    break Ok(());
                };
                let bytes = activation.expose_for_activation().as_bytes();
                if bytes.len() > MAX_ACTIVATION_URI_BYTES {
                    break Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "activation URI exceeds protocol limit",
                    ));
                }
                if let Err(error) = socket.write_u32_le(bytes.len() as u32).await {
                    break Err(error);
                }
                if let Err(error) = socket.write_all(bytes).await {
                    break Err(error);
                }
            }
        }
    };
    relay.unregister(id);
    result
}

pub(crate) async fn handle_publisher(
    socket: &mut tokio::net::UnixStream,
    relay: &InviteActivationRelay,
) -> io::Result<()> {
    let length = socket.read_u32_le().await? as usize;
    if length > MAX_ACTIVATION_URI_BYTES {
        socket.write_u8(PUBLISH_FAILED).await?;
        return Ok(());
    }
    let mut payload = vec![0_u8; length];
    socket.read_exact(&mut payload).await?;
    let published = std::str::from_utf8(&payload)
        .ok()
        .and_then(|value| InviteActivationUri::parse(value).ok())
        .is_some_and(|uri| relay.publish(uri).is_ok());
    socket
        .write_u8(if published {
            PUBLISH_OK
        } else {
            PUBLISH_FAILED
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::{
        InviteActivationRelay, MAX_ACTIVATION_URI_BYTES, PublishError, handle_publisher,
        handle_subscriber,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;
    use tokio::sync::mpsc::error::TryRecvError;
    use tokio_util::sync::CancellationToken;
    use xodus::forza::InviteActivationUri;

    const VALID_URI: &str = "ms-xbl-multiplayer://inviteAccept?invitedUser=2810000000000000&sender=2810000000000001&connectionString=synthetic-session";

    fn activation_uri_with_length(length: usize, fill: char) -> InviteActivationUri {
        const PREFIX: &str = "ms-xbl-multiplayer://inviteAccept?invitedUser=2810000000000000&sender=2810000000000001&connectionString=";
        assert!(length > PREFIX.len());
        let value = format!("{PREFIX}{}", fill.to_string().repeat(length - PREFIX.len()));
        let uri = InviteActivationUri::parse(&value).unwrap();
        assert_eq!(uri.expose_for_activation().len(), length);
        uri
    }

    async fn wait_for_subscriber(relay: &InviteActivationRelay) {
        for _ in 0..100 {
            if relay.has_subscriber() {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("subscriber was not registered");
    }

    #[tokio::test]
    async fn invite_activation_publisher_before_subscriber_fails() {
        let relay = InviteActivationRelay::default();
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let task = tokio::spawn(async move { handle_publisher(&mut server, &relay).await });
        client.write_u32_le(VALID_URI.len() as u32).await.unwrap();
        client.write_all(VALID_URI.as_bytes()).await.unwrap();

        assert_eq!(client.read_u8().await.unwrap(), 1);
        task.await.unwrap().unwrap();
    }

    #[test]
    fn invite_activation_publish_enforces_internal_uri_length_before_enqueue() {
        let relay = InviteActivationRelay::default();
        let (_subscription, mut activations) = relay.register().unwrap();
        let oversized = activation_uri_with_length(MAX_ACTIVATION_URI_BYTES + 1, 'o');

        assert_eq!(relay.publish(oversized), Err(PublishError::InvalidLength));
        assert_eq!(activations.try_recv(), Err(TryRecvError::Empty));

        let boundary = activation_uri_with_length(MAX_ACTIVATION_URI_BYTES, 'b');
        relay.publish(boundary.clone()).unwrap();
        assert_eq!(activations.try_recv().unwrap(), boundary);
    }

    #[test]
    fn invite_activation_full_queue_preserves_subscriber_and_recovers() {
        let relay = InviteActivationRelay::default();
        let (_subscription, mut activations) = relay.register().unwrap();
        let first = InviteActivationUri::parse(VALID_URI).unwrap();
        let second = activation_uri_with_length(VALID_URI.len(), 'r');

        relay.publish(first.clone()).unwrap();
        assert_eq!(relay.publish(second.clone()), Err(PublishError::Full));
        assert!(relay.has_subscriber());
        assert_eq!(activations.try_recv().unwrap(), first);
        assert_eq!(activations.try_recv(), Err(TryRecvError::Empty));

        relay.publish(second.clone()).unwrap();
        assert_eq!(activations.try_recv().unwrap(), second);
    }

    #[test]
    fn invite_activation_closed_subscriber_is_distinct_and_removed() {
        let relay = InviteActivationRelay::default();
        let (_subscription, activations) = relay.register().unwrap();
        drop(activations);

        assert_eq!(
            relay.publish(InviteActivationUri::parse(VALID_URI).unwrap()),
            Err(PublishError::Closed)
        );
        assert!(!relay.has_subscriber());
    }

    #[tokio::test]
    async fn invite_activation_invalid_scheme_is_rejected() {
        let relay = InviteActivationRelay::default();
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let task = tokio::spawn(async move { handle_publisher(&mut server, &relay).await });
        let invalid = b"https://example.invalid/invite";
        client.write_u32_le(invalid.len() as u32).await.unwrap();
        client.write_all(invalid).await.unwrap();

        assert_eq!(client.read_u8().await.unwrap(), 1);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn invite_activation_oversized_publisher_is_rejected_before_payload_read() {
        let relay = InviteActivationRelay::default();
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let task = tokio::spawn(async move { handle_publisher(&mut server, &relay).await });
        client
            .write_u32_le((MAX_ACTIVATION_URI_BYTES + 1) as u32)
            .await
            .unwrap();

        assert_eq!(client.read_u8().await.unwrap(), 1);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn invite_activation_subscriber_disconnect_cleans_up_registration() {
        let relay = InviteActivationRelay::default();
        let task_relay = relay.clone();
        let token = CancellationToken::new();
        let task_token = token.clone();
        let (client, mut server) = UnixStream::pair().unwrap();
        let task =
            tokio::spawn(
                async move { handle_subscriber(&mut server, &task_relay, &task_token).await },
            );
        wait_for_subscriber(&relay).await;

        drop(client);
        task.await.unwrap().unwrap();
        assert!(!relay.has_subscriber());
    }

    #[tokio::test]
    async fn invite_activation_subscriber_io_error_cleans_up_registration() {
        let relay = InviteActivationRelay::default();
        let task_relay = relay.clone();
        let token = CancellationToken::new();
        let task_token = token.clone();
        let (_client, mut server) = UnixStream::pair().unwrap();
        server.shutdown().await.unwrap();
        let task =
            tokio::spawn(
                async move { handle_subscriber(&mut server, &task_relay, &task_token).await },
            );
        wait_for_subscriber(&relay).await;

        relay
            .publish(InviteActivationUri::parse(VALID_URI).unwrap())
            .unwrap();
        assert!(task.await.unwrap().is_err());
        assert!(!relay.has_subscriber());
    }

    #[tokio::test]
    async fn invite_activation_xdap_relays_to_xdsi_with_length_prefix() {
        let relay = InviteActivationRelay::default();
        let subscriber_relay = relay.clone();
        let token = CancellationToken::new();
        let subscriber_token = token.clone();
        let (mut subscriber_client, mut subscriber_server) = UnixStream::pair().unwrap();
        let subscriber = tokio::spawn(async move {
            handle_subscriber(&mut subscriber_server, &subscriber_relay, &subscriber_token).await
        });
        wait_for_subscriber(&relay).await;

        let publisher_relay = relay.clone();
        let (mut publisher_client, mut publisher_server) = UnixStream::pair().unwrap();
        let publisher =
            tokio::spawn(
                async move { handle_publisher(&mut publisher_server, &publisher_relay).await },
            );
        publisher_client
            .write_u32_le(VALID_URI.len() as u32)
            .await
            .unwrap();
        publisher_client
            .write_all(VALID_URI.as_bytes())
            .await
            .unwrap();

        assert_eq!(publisher_client.read_u8().await.unwrap(), 0);
        publisher.await.unwrap().unwrap();
        let length = subscriber_client.read_u32_le().await.unwrap() as usize;
        assert!(length <= MAX_ACTIVATION_URI_BYTES);
        let mut delivered = vec![0_u8; length];
        subscriber_client.read_exact(&mut delivered).await.unwrap();
        assert_eq!(delivered, VALID_URI.as_bytes());

        token.cancel();
        subscriber.await.unwrap().unwrap();
        assert!(!relay.has_subscriber());
    }
}
