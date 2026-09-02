use std::fs::Permissions;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use tokio::net::UnixListener;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use xodus::tokens::TokenManager;

mod connection;
mod invite_activation;
mod simple_context;
mod social_ipc;
mod utils;

const XML_MAGIC: u32 = 0x58445358;
const PROTO_MAGIC: u32 = 0x58445350;

#[tokio::main]
async fn main() {
    xodus::secrets::init_secrets().expect("Failed to init keychain");
    let tokens = Arc::new(TokenManager::with_keychain_and_memory());
    xodus::tokens::device::ensure_device_credentials(&reqwest::Client::new(), &tokens).await;
    let xodus::models::secrets::Token::Legacy(device_token) =
        tokens.get_device_sts_token().unwrap()
    else {
        panic!("Device token isnt legacy")
    };

    env_logger::init_from_env("XODUS_LOG");
    let runtime_dir = utils::get_runtime_dir();
    let cancellation = CancellationToken::new();
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
        let service_uid = std::fs::metadata(&socket_path)
            .expect("Unable to inspect socket owner")
            .uid();
        let mode = 0o600;
        let perms = Permissions::from_mode(mode);
        _ = tokio::fs::set_permissions(&socket_path, perms).await;
        let activations = invite_activation::InviteActivationRelay::default();
        let social = Arc::new(Mutex::new(social_ipc::SocialService::new(
            social_ipc::ForzaBackend::new(Arc::clone(&tokens)),
        )));
        loop {
            let accept = tokio::select! {
                r = listener.accept() => r,
                _ = cancellation.cancelled() => break,
            }
            .expect("Failed to accept");

            let token = cancellation.clone();
            let device_token = device_token.clone();
            let tokens = tokens.clone();
            let activations = activations.clone();
            let social = Arc::clone(&social);
            tokio::spawn(async move {
                connection::router::route(
                    accept.0,
                    token,
                    device_token,
                    tokens,
                    service_uid,
                    social,
                    activations,
                )
                .await
            });
        }
    }

    _ = tokio::fs::remove_file(socket_path).await;
}
