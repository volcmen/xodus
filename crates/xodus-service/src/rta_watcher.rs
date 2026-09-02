use std::{
    error::Error,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use tokio::{
    sync::{Mutex, broadcast, watch},
    time,
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        client::IntoClientRequest,
        http::{HeaderValue, header},
        protocol::Message,
    },
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use xodus::{
    forza::{
        ForzaSession, RTA_SIGNING_URI, RTA_SUBPROTOCOL, RTA_URI, RtaEvent, RtaMachine,
        XboxServiceError, notification_resource_uri, parse_multiplayer_invite,
    },
    tokens::TokenManager,
    xal::extensions::SigningReqwestBuilder,
};

use crate::{invite_activation::SubscriberLifecycle, invite_store::InviteStore};

pub type SharedInviteStore = Arc<Mutex<Option<InviteStore>>>;
type WatcherResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

pub fn install_tls_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[derive(Clone, Copy, Debug)]
pub struct WatcherPolicy {
    maximum_backoff: Duration,
    renewal: Duration,
}

impl Default for WatcherPolicy {
    fn default() -> Self {
        Self {
            maximum_backoff: Duration::from_secs(60),
            renewal: Duration::from_secs(90 * 60),
        }
    }
}

impl WatcherPolicy {
    pub fn reconnect_delay(&self, attempt: u32, jitter: Duration) -> Duration {
        let quadratic = u64::from(attempt).saturating_pow(2);
        Duration::from_secs(quadratic).min(self.maximum_backoff) + jitter
    }

    pub fn renew_after(&self) -> Duration {
        self.renewal
    }
}

pub async fn run_rta_watcher(
    client: Client,
    tokens: Arc<TokenManager>,
    lifecycle: SubscriberLifecycle,
    store: SharedInviteStore,
    invite_events: broadcast::Sender<Uuid>,
    cancellation: CancellationToken,
) {
    let policy = WatcherPolicy::default();
    let mut subscribers = lifecycle.changed();

    loop {
        if wait_until_active(&mut subscribers, &cancellation)
            .await
            .is_err()
        {
            return;
        }

        let mut attempt = 0;
        while *subscribers.borrow() > 0 && !cancellation.is_cancelled() {
            match run_connection(
                &client,
                &tokens,
                &mut subscribers,
                &store,
                &invite_events,
                &cancellation,
                policy,
            )
            .await
            {
                Ok(()) if *subscribers.borrow() == 0 || cancellation.is_cancelled() => break,
                Ok(()) => log::info!("Xbox RTA requested a clean resync"),
                Err(error) if is_permanent_registration_error(error.as_ref()) => {
                    log::error!(
                        "Xbox rejected the notification endpoint; incoming invite notifications are disabled for this service run: {error}"
                    );
                    return;
                }
                Err(error) => log::warn!("Xbox RTA connection ended: {error}"),
            }

            if *subscribers.borrow() == 0 || cancellation.is_cancelled() {
                break;
            }
            let delay = policy.reconnect_delay(attempt, reconnect_jitter());
            attempt = attempt.saturating_add(1);
            tokio::select! {
                _ = cancellation.cancelled() => return,
                changed = subscribers.changed() => {
                    if changed.is_err() || *subscribers.borrow() == 0 {
                        break;
                    }
                }
                _ = time::sleep(delay) => {}
            }
        }
    }
}

fn is_permanent_registration_error(error: &(dyn Error + 'static)) -> bool {
    matches!(
        error.downcast_ref::<XboxServiceError>(),
        Some(XboxServiceError::Http { service, status })
            if service == "Xbox Notification Service" && *status == reqwest::StatusCode::BAD_REQUEST
    )
}

async fn wait_until_active(
    subscribers: &mut watch::Receiver<usize>,
    cancellation: &CancellationToken,
) -> Result<(), ()> {
    loop {
        if *subscribers.borrow_and_update() > 0 {
            return Ok(());
        }
        tokio::select! {
            _ = cancellation.cancelled() => return Err(()),
            changed = subscribers.changed() => changed.map_err(|_| ())?,
        }
    }
}

async fn run_connection(
    client: &Client,
    tokens: &TokenManager,
    subscribers: &mut watch::Receiver<usize>,
    store: &SharedInviteStore,
    invite_events: &broadcast::Sender<Uuid>,
    cancellation: &CancellationToken,
    policy: WatcherPolicy,
) -> WatcherResult<()> {
    let mut session = ForzaSession::authenticate(client, tokens).await?;
    let local_xuid = session.local_xuid().to_owned();
    {
        let mut stored = store.lock().await;
        if stored
            .as_ref()
            .is_none_or(|current| !current.belongs_to(&local_xuid))
        {
            *stored = Some(InviteStore::new(&local_xuid));
        }
    }

    let signed = client
        .get(RTA_SIGNING_URI)
        .header("Authorization", session.authorization())
        .sign(session.signer_mut(), None)
        .await?
        .build()?;
    let signature = signed
        .headers()
        .get("Signature")
        .ok_or("signed Xbox RTA request has no Signature header")?
        .to_str()?
        .to_owned();

    let mut request = RTA_URI.into_client_request()?;
    request.headers_mut().insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(session.authorization())?,
    );
    request
        .headers_mut()
        .insert("Signature", HeaderValue::from_str(&signature)?);
    request.headers_mut().insert(
        header::SEC_WEBSOCKET_PROTOCOL,
        HeaderValue::from_static(RTA_SUBPROTOCOL),
    );
    request
        .headers_mut()
        .insert(header::ACCEPT_LANGUAGE, HeaderValue::from_static("en-US"));
    request.headers_mut().insert(
        header::USER_AGENT,
        HeaderValue::from_static("xodus-forza/0.1"),
    );

    let (mut socket, _) = connect_async(request).await?;
    let resource = notification_resource_uri(&local_xuid);
    // The RTA resource has no data until Notification Service has registered
    // this endpoint. Waiting for a successful subscription before registering
    // creates a code-3 (NoResourceData) deadlock.
    let system_id = Uuid::new_v4().to_string();
    let endpoint_id = session
        .register_notification_endpoint(&system_id, &resource)
        .await?;
    let mut machine = RtaMachine::new(1, &resource);
    if let Err(error) = socket
        .send(Message::Text(machine.subscribe_frame().into()))
        .await
    {
        let _ = session.unregister_notification_endpoint(&endpoint_id).await;
        return Err(error.into());
    }

    let renewal = time::sleep(policy.renew_after());
    tokio::pin!(renewal);
    let result = loop {
        tokio::select! {
            _ = cancellation.cancelled() => break Ok(()),
            changed = subscribers.changed() => {
                if changed.is_err() || *subscribers.borrow() == 0 {
                    break Ok(());
                }
            }
            _ = &mut renewal => break Ok(()),
            message = socket.next() => {
                let Some(message) = message else {
                    break Err("Xbox RTA WebSocket closed".into());
                };
                match message? {
                    Message::Text(text) => match machine.handle_text(&text)? {
                        RtaEvent::Subscribed { .. } => log::info!(
                            "Xbox Multiplayer Activity invite notifications are active"
                        ),
                        RtaEvent::Payload(payload) => {
                            match parse_multiplayer_invite(&payload, &local_xuid) {
                                Ok(invite) => {
                                    let sender = invite.sender_gamertag.clone();
                                    let mut guard = store.lock().await;
                                    if let Some(store) = guard.as_mut() {
                                        match store.insert_with_status(invite, Utc::now()) {
                                            Ok(outcome) => {
                                                if outcome.is_new {
                                                    let _ = invite_events.send(outcome.id);
                                                    log::info!("Received a Forza invite from {sender}");
                                                }
                                            }
                                            Err(error) => log::debug!("Ignored Xbox invite: {error}"),
                                        }
                                    }
                                }
                                Err(error) => log::debug!("Ignored Xbox RTA payload: {error}"),
                            }
                        }
                        RtaEvent::Resync => break Ok(()),
                        RtaEvent::Ignored => {}
                    },
                    Message::Close(_) => break Err("Xbox RTA WebSocket closed".into()),
                    _ => {}
                }
            }
        }
    };

    if let Err(error) = session.unregister_notification_endpoint(&endpoint_id).await {
        log::debug!("Xbox notification endpoint cleanup failed: {error}");
    }
    let _ = socket.close(None).await;
    result
}

fn reconnect_jitter() -> Duration {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_millis();
    Duration::from_millis(u64::from(millis % 5_000))
}

#[cfg(test)]
mod tests {
    use super::{WatcherPolicy, install_tls_provider, is_permanent_registration_error};
    use reqwest::StatusCode;
    use std::time::Duration;
    use xodus::forza::XboxServiceError;

    #[test]
    fn installs_a_process_wide_tls_crypto_provider() {
        install_tls_provider();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }

    #[test]
    fn treats_rejected_notification_registration_as_permanent() {
        let rejected = XboxServiceError::Http {
            service: "Xbox Notification Service".into(),
            status: StatusCode::BAD_REQUEST,
        };
        let throttled = XboxServiceError::Http {
            service: "Xbox Notification Service".into(),
            status: StatusCode::TOO_MANY_REQUESTS,
        };
        assert!(is_permanent_registration_error(&rejected));
        assert!(!is_permanent_registration_error(&throttled));
    }

    #[test]
    fn reconnect_backoff_is_quadratic_and_capped() {
        let policy = WatcherPolicy::default();
        assert_eq!(policy.reconnect_delay(0, Duration::ZERO), Duration::ZERO);
        assert_eq!(
            policy.reconnect_delay(1, Duration::from_millis(20)),
            Duration::from_millis(1_020)
        );
        assert_eq!(
            policy.reconnect_delay(4, Duration::ZERO),
            Duration::from_secs(16)
        );
        assert_eq!(
            policy.reconnect_delay(100, Duration::from_secs(2)),
            Duration::from_secs(62)
        );
    }

    #[test]
    fn connection_renews_before_microsoft_two_hour_limit() {
        assert_eq!(
            WatcherPolicy::default().renew_after(),
            Duration::from_secs(90 * 60)
        );
    }
}
