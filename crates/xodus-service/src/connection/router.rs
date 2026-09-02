use std::sync::Arc;

use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;
use xodus::{forza::SOCIAL_API_MAGIC, models::secrets::LegacyToken, tokens::TokenManager};

use crate::{
    invite_activation::{InviteActivationRelay, PUBLISH_MAGIC, SUBSCRIBE_MAGIC},
    simple_context::SimpleContext,
    social_ui::SOCIAL_UI_MAGIC,
};

pub async fn route(
    mut socket: tokio::net::UnixStream,
    token: CancellationToken,
    device_token: LegacyToken,
    tokens: Arc<TokenManager>,
    invite_relay: InviteActivationRelay,
    social_service: crate::social_ipc::SocialService,
) {
    let cred = socket.peer_cred().ok();
    log::debug!(
        "Connection from pid {:?}",
        cred.as_ref().and_then(|cred| cred.pid())
    );

    let mut context = SimpleContext::new(device_token, tokens);
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

        let magic = u32::from_le_bytes(read_magic);
        let res = match magic {
            crate::XML_MAGIC => super::xml::handle(&mut socket, &mut context).await,
            crate::PROTO_MAGIC => super::proto::handle(&mut socket, &mut context).await,
            SUBSCRIBE_MAGIC => {
                if let Err(error) =
                    crate::invite_activation::serve_subscriber(socket, invite_relay, token).await
                {
                    log::debug!("Invite subscriber disconnected: {error}");
                }
                return;
            }
            PUBLISH_MAGIC => {
                if let Err(error) =
                    crate::invite_activation::serve_publisher(socket, invite_relay).await
                {
                    log::warn!("Invite activation was not relayed: {error}");
                }
                return;
            }
            SOCIAL_UI_MAGIC => {
                if let Err(error) = crate::social_ui::serve(socket).await {
                    log::warn!("Forza social UI failed: {error}");
                }
                return;
            }
            SOCIAL_API_MAGIC => {
                let peer_uid = cred.as_ref().map(|cred| cred.uid());
                if peer_uid != Some(current_uid()) {
                    log::warn!("Rejected Forza social IPC from uid {peer_uid:?}");
                    return;
                }
                if let Err(error) = crate::social_ipc::serve(socket, social_service).await {
                    log::warn!("Forza social IPC failed: {error}");
                }
                return;
            }
            _ => {
                log::error!("Unknown magic");
                return;
            }
        };

        if let Err(err) = res {
            log::error!("There was an error handling the message: {err}");
        }
    }
}

fn current_uid() -> u32 {
    use std::os::unix::fs::MetadataExt;

    std::fs::metadata("/proc/self")
        .expect("/proc/self metadata")
        .uid()
}
