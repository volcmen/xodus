use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;
use xodus::models::secrets::LegacyToken;
use xodus::tokens::TokenManager;

use crate::invite_activation::{InviteActivationRelay, handle_publisher, handle_subscriber};
use crate::simple_context::SimpleContext;
use crate::social_ipc::{SharedSocialService, handle_socket};

pub async fn route(
    mut socket: tokio::net::UnixStream,
    token: CancellationToken,
    device_token: LegacyToken,
    tokens: Arc<TokenManager>,
    service_uid: u32,
    social: SharedSocialService,
    activations: InviteActivationRelay,
) {
    let credentials = socket.peer_cred().ok();
    let peer_pid = credentials.as_ref().and_then(tokio::net::unix::UCred::pid);
    let peer_uid = credentials.as_ref().map(tokio::net::unix::UCred::uid);
    log::debug!("Connection from pid {peer_pid:?} uid {peer_uid:?}");

    let Some(mut context) = authorize_same_uid_then(peer_uid, service_uid, || {
        SimpleContext::new(device_token, tokens)
    }) else {
        log::warn!("Rejected connection from uid {peer_uid:?}");
        return;
    };
    loop {
        let mut read_magic = [0; 4];
        if token.is_cancelled() {
            return;
        }
        let read = socket.read_exact(&mut read_magic).await;
        if let Err(err) = read {
            log::error!("Failed to read magic: {err:?}");
            return;
        }

        let res = if read_magic == crate::XML_MAGIC.to_le_bytes() {
            super::xml::handle(&mut socket, &mut context).await
        } else if read_magic == crate::PROTO_MAGIC.to_le_bytes() {
            super::proto::handle(&mut socket, &mut context).await
        } else if read_magic == *b"XDSO" {
            handle_socket(&mut socket, &social, &activations).await
        } else if read_magic == *b"XDSI" {
            let result = handle_subscriber(&mut socket, &activations, &token).await;
            if result.is_err() {
                log::warn!("Activation subscriber disconnected with an error");
            }
            return;
        } else if read_magic == *b"XDAP" {
            if handle_publisher(&mut socket, &activations).await.is_err() {
                log::warn!("Activation publisher disconnected with an error");
            }
            return;
        } else if read_magic == *b"XDUI" {
            if handle_deferred_social_ui(&mut socket).await.is_err() {
                log::warn!("Social UI requester disconnected with an error");
            }
            return;
        } else {
            log::error!("Unknown magic");
            return;
        };

        if let Err(err) = res {
            log::error!("There was an error handling the message: {err}");
        }
    }
}

pub(crate) fn authorize_same_uid_then<T>(
    peer_uid: Option<u32>,
    service_uid: u32,
    operation: impl FnOnce() -> T,
) -> Option<T> {
    (peer_uid == Some(service_uid)).then(operation)
}

pub(crate) async fn handle_deferred_social_ui(
    socket: &mut tokio::net::UnixStream,
) -> std::io::Result<()> {
    socket.write_u8(2).await
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use tokio::io::AsyncReadExt;
    use tokio::net::UnixStream;

    use super::{authorize_same_uid_then, handle_deferred_social_ui};

    #[test]
    fn social_peer_uid_gate_accepts_same_uid_and_rejects_before_backend_invocation() {
        let backend_calls = Cell::new(0);
        let accepted = authorize_same_uid_then(Some(1000), 1000, || {
            backend_calls.set(backend_calls.get() + 1);
            "accepted"
        });
        assert_eq!(accepted, Some("accepted"));
        assert_eq!(backend_calls.get(), 1);

        let rejected = authorize_same_uid_then(Some(2000), 1000, || {
            backend_calls.set(backend_calls.get() + 1);
            "must not run"
        });
        assert_eq!(rejected, None);
        assert_eq!(backend_calls.get(), 1);
    }

    #[tokio::test]
    async fn social_xdui_route_returns_deferred_failure_status() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let task = tokio::spawn(async move { handle_deferred_social_ui(&mut server).await });

        assert_eq!(client.read_u8().await.unwrap(), 2);
        task.await.unwrap().unwrap();
    }
}
