use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use xodus::models::secrets::LegacyToken;
use xodus::tokens::TokenManager;

use crate::invite_activation::{InviteActivationRelay, handle_publisher, handle_subscriber};
use crate::simple_context::SimpleContext;
use crate::social_ipc::{SharedSocialService, handle_socket};
use crate::social_ui::{SocialUiLauncher, SocialUiMode};

const SOCIAL_UI_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub struct RouteServices {
    service_uid: u32,
    social: SharedSocialService,
    activations: InviteActivationRelay,
    social_ui: Arc<SocialUiLauncher>,
}

impl RouteServices {
    pub fn new(
        service_uid: u32,
        social: SharedSocialService,
        activations: InviteActivationRelay,
        social_ui: Arc<SocialUiLauncher>,
    ) -> Self {
        Self {
            service_uid,
            social,
            activations,
            social_ui,
        }
    }
}

pub async fn route(
    mut socket: tokio::net::UnixStream,
    token: CancellationToken,
    device_token: LegacyToken,
    tokens: Arc<TokenManager>,
    services: RouteServices,
) {
    let credentials = socket.peer_cred().ok();
    let peer_pid = credentials.as_ref().and_then(tokio::net::unix::UCred::pid);
    let peer_uid = credentials.as_ref().map(tokio::net::unix::UCred::uid);
    log::debug!("Connection from pid {peer_pid:?} uid {peer_uid:?}");

    let Some(mut context) = authorize_same_uid_then(peer_uid, services.service_uid, || {
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
            handle_socket(&mut socket, &services.social, &services.activations).await
        } else if read_magic == *b"XDSI" {
            let result = handle_subscriber(&mut socket, &services.activations, &token).await;
            if result.is_err() {
                log::warn!("Activation subscriber disconnected with an error");
            }
            return;
        } else if read_magic == *b"XDAP" {
            if handle_publisher(&mut socket, &services.activations)
                .await
                .is_err()
            {
                log::warn!("Activation publisher disconnected with an error");
            }
            return;
        } else if read_magic == *b"XDUI" {
            if handle_social_ui(&mut socket, &services.social_ui, &token)
                .await
                .is_err()
            {
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

pub(crate) async fn handle_social_ui(
    socket: &mut tokio::net::UnixStream,
    launcher: &SocialUiLauncher,
    cancellation: &CancellationToken,
) -> std::io::Result<()> {
    handle_social_ui_with_timeout(socket, launcher, cancellation, SOCIAL_UI_REQUEST_TIMEOUT).await
}

pub(crate) async fn handle_social_ui_with_timeout(
    socket: &mut tokio::net::UnixStream,
    launcher: &SocialUiLauncher,
    cancellation: &CancellationToken,
    request_timeout: Duration,
) -> std::io::Result<()> {
    let mut request = [0_u8; 2];
    let mode = tokio::select! {
        biased;
        _ = cancellation.cancelled() => None,
        read = timeout(request_timeout, socket.read_exact(&mut request)) => {
            match read {
                Ok(Ok(_)) => SocialUiMode::from_request(request),
                Ok(Err(error)) => {
                    log::debug!("Social UI request disconnected before its payload: {error}");
                    None
                }
                Err(_) => {
                    log::warn!("Social UI request payload timed out");
                    None
                }
            }
        }
    };
    let Some(mode) = mode else {
        socket.write_all(&[2]).await?;
        return socket.shutdown().await;
    };
    launcher.serve(socket, cancellation, mode).await
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::time::Duration;

    use tempfile::TempDir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;
    use tokio_util::sync::CancellationToken;

    use crate::social_ui::{SOCIAL_UI_ACCEPTED, SocialUiLauncher};

    use super::{authorize_same_uid_then, handle_social_ui, handle_social_ui_with_timeout};

    fn recording_overlay() -> (TempDir, PathBuf, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("overlay-helper");
        let launched = directory.path().join("launched");
        fs::write(
            &path,
            "#!/bin/sh\n: > \"$(dirname \"$0\")/launched\"\nexit 0\n",
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        (directory, path, launched)
    }

    #[test]
    fn social_peer_uid_gate_accepts_same_uid_and_rejects_before_backend_invocation() {
        let launcher_calls = Cell::new(0);
        let accepted = authorize_same_uid_then(Some(1000), 1000, || {
            launcher_calls.set(launcher_calls.get() + 1);
            "accepted"
        });
        assert_eq!(accepted, Some("accepted"));
        assert_eq!(launcher_calls.get(), 1);

        let rejected = authorize_same_uid_then(Some(2000), 1000, || {
            launcher_calls.set(launcher_calls.get() + 1);
            "must not run"
        });
        assert_eq!(rejected, None);
        assert_eq!(launcher_calls.get(), 1);
    }

    #[tokio::test]
    async fn social_xdui_handler_returns_the_launcher_status_and_eof() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let launcher = SocialUiLauncher::from_path(PathBuf::from("/usr/bin/true"));
        let cancellation = CancellationToken::new();
        let task =
            tokio::spawn(
                async move { handle_social_ui(&mut server, &launcher, &cancellation).await },
            );

        client.write_all(&[1, 1]).await.unwrap();
        assert_eq!(client.read_u8().await.unwrap(), SOCIAL_UI_ACCEPTED);
        assert_eq!(client.read_u8().await.unwrap(), 0);
        let mut trailing = Vec::new();
        client.read_to_end(&mut trailing).await.unwrap();
        assert!(trailing.is_empty());
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn social_xdui_handler_rejects_unknown_version_and_mode_without_launching() {
        for request in [[2, 1], [1, 2]] {
            let (_directory, overlay, launched) = recording_overlay();
            let (mut client, mut server) = UnixStream::pair().unwrap();
            let launcher = SocialUiLauncher::from_path(overlay);
            let cancellation = CancellationToken::new();
            let task = tokio::spawn(async move {
                handle_social_ui(&mut server, &launcher, &cancellation).await
            });

            client.write_all(&request).await.unwrap();
            let mut response = Vec::new();
            client.read_to_end(&mut response).await.unwrap();
            assert_eq!(response, [2]);
            task.await.unwrap().unwrap();
            assert!(!launched.exists());
        }
    }

    #[tokio::test]
    async fn payload_free_legacy_request_times_out_with_one_rejection_byte() {
        let (_directory, overlay, launched) = recording_overlay();
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let launcher = SocialUiLauncher::from_path(overlay);
        let cancellation = CancellationToken::new();
        let task = tokio::spawn(async move {
            handle_social_ui_with_timeout(
                &mut server,
                &launcher,
                &cancellation,
                Duration::from_millis(10),
            )
            .await
        });

        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        assert_eq!(response, [2]);
        task.await.unwrap().unwrap();
        assert!(!launched.exists());
    }

    #[tokio::test]
    async fn service_cancellation_interrupts_the_pre_ack_request_wait() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let launcher = SocialUiLauncher::from_path(PathBuf::from("/usr/bin/true"));
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let mut task = tokio::spawn(async move {
            handle_social_ui_with_timeout(
                &mut server,
                &launcher,
                &task_cancellation,
                Duration::from_secs(1),
            )
            .await
        });

        cancellation.cancel();
        if tokio::time::timeout(Duration::from_millis(50), &mut task)
            .await
            .is_err()
        {
            task.abort();
            panic!("service cancellation must interrupt the request payload wait");
        }
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        assert_eq!(response, [2]);
    }
}
