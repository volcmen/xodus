use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use thiserror::Error;
use tokio::sync::{Mutex, broadcast};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use xodus::forza::{ModelError, build_invite_accept_uri};
use zbus::{Connection, Proxy, zvariant::Value};

use crate::{
    invite_activation::InviteActivationRelay,
    invite_store::{InviteStore, StoreError},
    rta_watcher::SharedInviteStore,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotificationAction {
    Join(Uuid),
    Dismiss(Uuid),
}

#[derive(Debug, Error)]
pub enum ActionError {
    #[error("notification action is invalid")]
    InvalidAction,
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Model(#[from] ModelError),
    #[error("Forza is not ready to accept the invite")]
    ForzaUnavailable,
}

pub fn action_key(action: NotificationAction) -> String {
    match action {
        NotificationAction::Join(id) => format!("join:{id}"),
        NotificationAction::Dismiss(id) => format!("dismiss:{id}"),
    }
}

pub fn parse_action_key(key: &str) -> Result<NotificationAction, ActionError> {
    let (action, id) = key.split_once(':').ok_or(ActionError::InvalidAction)?;
    let id = Uuid::parse_str(id).map_err(|_| ActionError::InvalidAction)?;
    match action {
        "join" => Ok(NotificationAction::Join(id)),
        "dismiss" => Ok(NotificationAction::Dismiss(id)),
        _ => Err(ActionError::InvalidAction),
    }
}

pub fn apply_action(
    action: NotificationAction,
    store: &mut InviteStore,
    relay: &InviteActivationRelay,
    now: DateTime<Utc>,
) -> Result<String, ActionError> {
    match action {
        NotificationAction::Join(id) => {
            let invite = store.joinable(id, now)?;
            let uri = build_invite_accept_uri(
                &invite.invited_xuid,
                &invite.sender_xuid,
                &invite.connection_string,
            )?;
            relay
                .publish(&uri)
                .map_err(|_| ActionError::ForzaUnavailable)?;
            store.dismiss(id)?;
            Ok(format!(
                "Joining {} in Forza Motorsport",
                invite.sender_gamertag
            ))
        }
        NotificationAction::Dismiss(id) => {
            store.dismiss(id)?;
            Ok("Forza invitation dismissed".into())
        }
    }
}

pub async fn run(
    mut invites: broadcast::Receiver<Uuid>,
    store: SharedInviteStore,
    relay: InviteActivationRelay,
    cancellation: CancellationToken,
) {
    if let Err(error) = run_inner(&mut invites, store, relay, cancellation).await {
        log::warn!("Desktop invite notifications are unavailable: {error}");
    }
}

async fn run_inner(
    invites: &mut broadcast::Receiver<Uuid>,
    store: Arc<Mutex<Option<InviteStore>>>,
    relay: InviteActivationRelay,
    cancellation: CancellationToken,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let connection = Connection::session().await?;
    let proxy = Proxy::new(
        &connection,
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
    )
    .await?;
    let mut action_signals = proxy.receive_signal("ActionInvoked").await?;

    loop {
        tokio::select! {
            _ = cancellation.cancelled() => return Ok(()),
            event = invites.recv() => match event {
                Ok(id) => {
                    let safe = {
                        let mut guard = store.lock().await;
                        guard.as_mut().and_then(|store| store.safe(id, Utc::now()).ok())
                    };
                    if let Some(safe) = safe {
                        show_invite(&proxy, id, &safe.sender_gamertag).await?;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    log::warn!("Desktop notification stream skipped {skipped} old invite events");
                }
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
            signal = action_signals.next() => {
                let Some(signal) = signal else { return Ok(()); };
                let body = signal.body();
                let (_notification_id, key): (u32, String) = body.deserialize()?;
                let Ok(action) = parse_action_key(&key) else { continue; };
                let result = {
                    let mut guard = store.lock().await;
                    match guard.as_mut() {
                        Some(store) => apply_action(action, store, &relay, Utc::now()),
                        None => Err(ActionError::ForzaUnavailable),
                    }
                };
                match result {
                    Ok(message) => show_status(&proxy, "Forza Motorsport", &message).await?,
                    Err(error) => show_status(&proxy, "Forza invitation", &error.to_string()).await?,
                };
            }
        }
    }
}

async fn show_invite(proxy: &Proxy<'_>, id: Uuid, sender_gamertag: &str) -> zbus::Result<u32> {
    let actions = vec![
        action_key(NotificationAction::Join(id)),
        "Join".into(),
        action_key(NotificationAction::Dismiss(id)),
        "Dismiss".into(),
    ];
    let hints: HashMap<String, Value<'_>> = HashMap::new();
    let body = format!("{sender_gamertag} invited you to Forza Motorsport");
    proxy
        .call(
            "Notify",
            &(
                "Xodus Forza",
                0u32,
                "",
                "Forza invitation",
                body,
                actions,
                hints,
                15_000i32,
            ),
        )
        .await
}

async fn show_status(proxy: &Proxy<'_>, summary: &str, body: &str) -> zbus::Result<u32> {
    let actions: Vec<String> = Vec::new();
    let hints: HashMap<String, Value<'_>> = HashMap::new();
    proxy
        .call(
            "Notify",
            &(
                "Xodus Forza",
                0u32,
                "",
                summary,
                body,
                actions,
                hints,
                7_000i32,
            ),
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::{NotificationAction, action_key, apply_action, parse_action_key};
    use crate::{
        invite_activation::InviteActivationRelay,
        invite_store::{InviteStore, StoreError},
    };
    use chrono::{Duration, TimeZone, Utc};
    use uuid::Uuid;
    use xodus::forza::MultiplayerInvite;

    fn now() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 30, 20, 0, 0).unwrap()
    }

    fn invite() -> MultiplayerInvite {
        MultiplayerInvite {
            invited_xuid: "111".into(),
            sender_xuid: "222".into(),
            sender_gamertag: "SyntheticFriend".into(),
            title_name: "Forza Motorsport".into(),
            connection_string: "secret/session".into(),
            expires_at: now() + Duration::minutes(10),
        }
    }

    #[test]
    fn action_keys_contain_only_operation_and_opaque_id() {
        let id = Uuid::parse_str("018f0c24-7a9b-7b36-a2ce-4c35be1384d0").unwrap();
        let join = NotificationAction::Join(id);
        let dismiss = NotificationAction::Dismiss(id);

        assert_eq!(
            action_key(join),
            "join:018f0c24-7a9b-7b36-a2ce-4c35be1384d0"
        );
        assert_eq!(parse_action_key(&action_key(join)).unwrap(), join);
        assert_eq!(parse_action_key(&action_key(dismiss)).unwrap(), dismiss);
        assert!(parse_action_key("join:secret/session").is_err());
    }

    #[test]
    fn failed_join_keeps_invite_for_a_later_retry() {
        let relay = InviteActivationRelay::new();
        let mut store = InviteStore::new("111");
        let id = store.insert(invite(), now()).unwrap();

        assert!(apply_action(NotificationAction::Join(id), &mut store, &relay, now()).is_err());
        assert_eq!(
            store.joinable(id, now()).unwrap().connection_string,
            "secret/session"
        );
    }

    #[tokio::test]
    async fn successful_join_relays_once_and_consumes_invite() {
        let relay = InviteActivationRelay::new();
        let mut game = relay.subscribe();
        let mut store = InviteStore::new("111");
        let id = store.insert(invite(), now()).unwrap();

        apply_action(NotificationAction::Join(id), &mut store, &relay, now()).unwrap();
        let uri = game.recv().await.unwrap();
        assert_eq!(
            uri,
            "ms-xbl-multiplayer://inviteAccept?invitedUser=111&sender=222&connectionString=secret%2Fsession"
        );
        assert_eq!(store.joinable(id, now()).unwrap_err(), StoreError::NotFound);
    }

    #[test]
    fn dismiss_removes_invite_without_relaying() {
        let relay = InviteActivationRelay::new();
        let mut store = InviteStore::new("111");
        let id = store.insert(invite(), now()).unwrap();

        apply_action(NotificationAction::Dismiss(id), &mut store, &relay, now()).unwrap();
        assert_eq!(store.joinable(id, now()).unwrap_err(), StoreError::NotFound);
    }
}
