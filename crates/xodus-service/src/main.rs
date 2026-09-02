use std::{fs::Permissions, os::unix::fs::PermissionsExt, sync::Arc};

use tokio::{
    net::UnixListener,
    sync::{Mutex, broadcast},
};
use tokio_util::sync::CancellationToken;
use xodus::tokens::TokenManager;

mod connection;
mod desktop_notifications;
mod invite_activation;
mod invite_store;
mod rta_watcher;
mod simple_context;
mod social_ipc;
mod social_ui;
mod utils;

const XML_MAGIC: u32 = 0x58445358;
const PROTO_MAGIC: u32 = 0x58445350;

#[tokio::main]
async fn main() {
    rta_watcher::install_tls_provider();
    xodus::secrets::init_secrets().expect("Failed to init keychain");
    let tokens = Arc::new(TokenManager::with_keychain_and_memory());
    let client = reqwest::Client::new();
    xodus::tokens::device::ensure_device_credentials(&client, &tokens).await;
    let xodus::models::secrets::Token::Legacy(device_token) =
        tokens.get_device_sts_token().unwrap()
    else {
        panic!("Device token isnt legacy")
    };

    env_logger::init_from_env("XODUS_LOG");
    let runtime_dir = utils::get_runtime_dir();
    let cancellation = CancellationToken::new();
    let invite_relay = invite_activation::InviteActivationRelay::new();
    let subscriber_lifecycle = invite_relay.subscriber_lifecycle();
    let invite_store = Arc::new(Mutex::new(None));
    let (invite_events, _) = broadcast::channel(32);
    let social_service = social_ipc::SocialService::new(
        client.clone(),
        tokens.clone(),
        invite_store.clone(),
        invite_relay.clone(),
        subscriber_lifecycle.clone(),
    );
    let rta_task = tokio::spawn(rta_watcher::run_rta_watcher(
        client,
        tokens.clone(),
        subscriber_lifecycle,
        invite_store.clone(),
        invite_events.clone(),
        cancellation.clone(),
    ));
    let notification_task = tokio::spawn(desktop_notifications::run(
        invite_events.subscribe(),
        invite_store.clone(),
        invite_relay.clone(),
        cancellation.clone(),
    ));
    let socket_path = format!("{runtime_dir}/xodus.sock");
    let trigger = cancellation.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c()
            .await
            .expect("Failure to handle ctrl_c");
        trigger.cancel();
    });
    {
        let listener = UnixListener::bind(&socket_path).expect("Unable to bind to socket");
        let mode = 0o600;
        let perms = Permissions::from_mode(mode);
        _ = tokio::fs::set_permissions(&socket_path, perms).await;
        loop {
            let accept = tokio::select! {
                r = listener.accept() => r,
                _ = cancellation.cancelled() => break,
            }
            .expect("Failed to accept");

            let token = cancellation.clone();
            let device_token = device_token.clone();
            let tokens = tokens.clone();
            let invite_relay = invite_relay.clone();
            let social_service = social_service.clone();
            tokio::spawn(async move {
                connection::router::route(
                    accept.0,
                    token,
                    device_token,
                    tokens,
                    invite_relay,
                    social_service,
                )
                .await
            });
        }
    }

    _ = tokio::fs::remove_file(socket_path).await;
    cancellation.cancel();
    _ = rta_task.await;
    _ = notification_task.await;
}
