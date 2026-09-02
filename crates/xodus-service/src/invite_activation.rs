use std::io::{Error, ErrorKind};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    sync::{broadcast, watch},
};
use tokio_util::sync::CancellationToken;
use url::Url;

pub const SUBSCRIBE_MAGIC: u32 = u32::from_le_bytes(*b"XDSI");
pub const PUBLISH_MAGIC: u32 = u32::from_le_bytes(*b"XDAP");
const MAX_URI_BYTES: usize = 4096;

#[derive(Clone)]
pub struct InviteActivationRelay {
    sender: broadcast::Sender<String>,
    lifecycle: SubscriberLifecycle,
}

#[derive(Clone)]
pub struct SubscriberLifecycle {
    sender: watch::Sender<usize>,
}

pub struct SubscriberGuard {
    lifecycle: SubscriberLifecycle,
}

impl SubscriberLifecycle {
    pub fn current(&self) -> usize {
        *self.sender.borrow()
    }

    pub fn track(&self) -> SubscriberGuard {
        self.sender.send_modify(|count| *count += 1);
        SubscriberGuard {
            lifecycle: self.clone(),
        }
    }

    pub fn changed(&self) -> watch::Receiver<usize> {
        self.sender.subscribe()
    }

    #[cfg(test)]
    pub async fn wait_for_count(&self, expected: usize) -> Result<(), watch::error::RecvError> {
        let mut receiver = self.changed();
        loop {
            if *receiver.borrow_and_update() == expected {
                return Ok(());
            }
            receiver.changed().await?;
        }
    }
}

impl Drop for SubscriberGuard {
    fn drop(&mut self) {
        self.lifecycle
            .sender
            .send_modify(|count| *count = count.saturating_sub(1));
    }
}

impl InviteActivationRelay {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(8);
        let (lifecycle, _) = watch::channel(0);
        Self {
            sender,
            lifecycle: SubscriberLifecycle { sender: lifecycle },
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.sender.subscribe()
    }

    pub fn publish(&self, uri: &str) -> Result<usize, Error> {
        validate_invite_uri(uri)?;
        self.sender.send(uri.to_owned()).map_err(|_| {
            Error::new(
                ErrorKind::NotConnected,
                "Forza is not listening for invites",
            )
        })
    }

    pub fn subscriber_lifecycle(&self) -> SubscriberLifecycle {
        self.lifecycle.clone()
    }
}

pub fn validate_invite_uri(uri: &str) -> Result<(), Error> {
    if uri.len() > MAX_URI_BYTES {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "invite URI is too long",
        ));
    }
    let parsed = Url::parse(uri).map_err(|error| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("invalid invite URI: {error}"),
        )
    })?;
    if parsed.scheme() != "ms-xbl-multiplayer" || parsed.host_str() != Some("inviteAccept") {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "not an Xbox inviteAccept URI",
        ));
    }
    let mut invited_user = false;
    let mut sender = false;
    let mut connection_string = false;
    for (key, value) in parsed.query_pairs() {
        let present = !value.is_empty();
        match key.as_ref() {
            "invitedUser" => invited_user = present,
            "sender" => sender = present,
            "connectionString" => connection_string = present,
            _ => {}
        }
    }
    if invited_user && sender && connection_string {
        Ok(())
    } else {
        Err(Error::new(
            ErrorKind::InvalidInput,
            "invite URI is missing required fields",
        ))
    }
}

pub async fn serve_subscriber(
    mut socket: UnixStream,
    relay: InviteActivationRelay,
    cancellation: CancellationToken,
) -> Result<(), Error> {
    let _subscriber = relay.subscriber_lifecycle().track();
    let mut receiver = relay.subscribe();
    let mut disconnected = [0u8; 1];
    loop {
        tokio::select! {
            _ = cancellation.cancelled() => return Ok(()),
            read = socket.read(&mut disconnected) => {
                match read {
                    Ok(0) => return Ok(()),
                    Ok(_) => return Err(Error::new(ErrorKind::InvalidData, "unexpected subscriber data")),
                    Err(error) => return Err(error),
                }
            }
            event = receiver.recv() => {
                let uri = event.map_err(|error| Error::new(ErrorKind::BrokenPipe, error))?;
                socket.write_all(&(uri.len() as u32).to_le_bytes()).await?;
                socket.write_all(uri.as_bytes()).await?;
            }
        }
    }
}

pub async fn serve_publisher(
    mut socket: UnixStream,
    relay: InviteActivationRelay,
) -> Result<(), Error> {
    let length = socket.read_u32_le().await? as usize;
    if length == 0 || length > MAX_URI_BYTES {
        socket.write_u8(1).await?;
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "invalid invite URI length",
        ));
    }
    let mut bytes = vec![0u8; length];
    socket.read_exact(&mut bytes).await?;
    let uri =
        std::str::from_utf8(&bytes).map_err(|error| Error::new(ErrorKind::InvalidInput, error))?;
    let status = match relay.publish(uri) {
        Ok(_) => 0,
        Err(error) if error.kind() == ErrorKind::NotConnected => 2,
        Err(_) => 1,
    };
    socket.write_u8(status).await?;
    if status == 0 {
        Ok(())
    } else {
        Err(Error::new(
            if status == 2 {
                ErrorKind::NotConnected
            } else {
                ErrorKind::InvalidInput
            },
            if status == 2 {
                "Forza is not listening for invites"
            } else {
                "invalid invite URI"
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_forza_invite_activation_uri() {
        let uri = "ms-xbl-multiplayer://inviteAccept?invitedUser=111&sender=222&connectionString=abc%2Fdef%3D";

        assert!(validate_invite_uri(uri).is_ok());
    }

    #[test]
    fn rejects_non_invite_or_incomplete_activation_uri() {
        for uri in [
            "https://example.com/inviteAccept?invitedUser=111&sender=222&connectionString=abc",
            "ms-xbl-multiplayer://activityJoin?invitedUser=111&sender=222&connectionString=abc",
            "ms-xbl-multiplayer://inviteAccept?invitedUser=111&sender=222",
        ] {
            assert!(validate_invite_uri(uri).is_err(), "accepted {uri}");
        }
    }

    #[tokio::test]
    async fn relay_delivers_activation_to_a_subscriber() {
        let relay = InviteActivationRelay::new();
        let mut subscriber = relay.subscribe();
        let uri =
            "ms-xbl-multiplayer://inviteAccept?invitedUser=111&sender=222&connectionString=abc";

        assert_eq!(relay.publish(uri).unwrap(), 1);
        assert_eq!(subscriber.recv().await.unwrap(), uri);
    }

    #[test]
    fn subscriber_guards_publish_exact_reference_counts() {
        let relay = InviteActivationRelay::new();
        let lifecycle = relay.subscriber_lifecycle();
        assert_eq!(lifecycle.current(), 0);

        let first = lifecycle.track();
        assert_eq!(lifecycle.current(), 1);
        let second = lifecycle.track();
        assert_eq!(lifecycle.current(), 2);
        drop(first);
        assert_eq!(lifecycle.current(), 1);
        drop(second);
        assert_eq!(lifecycle.current(), 0);
    }

    #[tokio::test]
    async fn wine_socket_disconnect_releases_the_subscriber() {
        let relay = InviteActivationRelay::new();
        let lifecycle = relay.subscriber_lifecycle();
        let (service_socket, game_socket) = UnixStream::pair().unwrap();
        let cancellation = CancellationToken::new();
        let task = tokio::spawn(serve_subscriber(service_socket, relay, cancellation));

        lifecycle.wait_for_count(1).await.unwrap();
        drop(game_socket);
        task.await.unwrap().unwrap();
        lifecycle.wait_for_count(0).await.unwrap();
    }
}
