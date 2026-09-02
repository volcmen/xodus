use std::fs::Permissions;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use tokio::net::UnixListener;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use xodus::tokens::TokenManager;

mod connection;
mod invite_activation;
mod simple_context;
mod social_ipc;
mod social_ui;
mod utils;

const XML_MAGIC: u32 = 0x58445358;
const PROTO_MAGIC: u32 = 0x58445350;

fn restrict_socket_permissions(path: &Path) -> io::Result<()> {
    std::fs::set_permissions(path, Permissions::from_mode(0o600))
}

fn bind_private_socket(path: &Path) -> io::Result<UnixListener> {
    let listener = UnixListener::bind(path)?;
    if let Err(permission_error) = restrict_socket_permissions(path) {
        drop(listener);
        std::fs::remove_file(path).map_err(|cleanup_error| {
            io::Error::new(
                cleanup_error.kind(),
                format!(
                    "failed to restrict socket permissions ({permission_error}); cleanup failed ({cleanup_error})"
                ),
            )
        })?;
        return Err(permission_error);
    }
    Ok(listener)
}

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
        let listener = bind_private_socket(Path::new(&socket_path))
            .expect("Unable to bind a private service socket");
        let service_uid = std::fs::metadata(&socket_path)
            .expect("Unable to inspect socket owner")
            .uid();
        let activations = invite_activation::InviteActivationRelay::default();
        let social = Arc::new(Mutex::new(social_ipc::SocialService::new(
            social_ipc::ForzaBackend::new(Arc::clone(&tokens)),
        )));
        let social_ui = Arc::new(social_ui::SocialUiLauncher::discover());
        let route_services = connection::router::RouteServices::new(
            service_uid,
            social,
            activations,
            Arc::clone(&social_ui),
        );
        loop {
            let accept = tokio::select! {
                r = listener.accept() => r,
                _ = cancellation.cancelled() => break,
            }
            .expect("Failed to accept");

            let token = cancellation.clone();
            let device_token = device_token.clone();
            let tokens = tokens.clone();
            let route_services = route_services.clone();
            tokio::spawn(async move {
                connection::router::route(accept.0, token, device_token, tokens, route_services)
                    .await
            });
        }
        social_ui.wait_idle().await;
    }

    _ = tokio::fs::remove_file(socket_path).await;
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use tempfile::tempdir;

    use super::restrict_socket_permissions;

    #[test]
    fn invite_activation_socket_permissions_are_exactly_0600() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("synthetic.sock");
        fs::write(&path, b"synthetic").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o777)).unwrap();

        restrict_socket_permissions(&path).unwrap();

        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
