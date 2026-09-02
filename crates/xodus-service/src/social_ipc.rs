use std::{io::Error, sync::Arc};

use chrono::{DateTime, Utc};
use reqwest::Client;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    sync::Mutex,
};
use uuid::Uuid;
use xodus::{
    forza::{
        ForzaSession, FriendAction, FriendView, InviteView, SocialRequest, SocialResponse,
        SocialSnapshot, SocialStatus, XboxFriend, decode_frame, encode_frame,
    },
    tokens::TokenManager,
};

use crate::{
    desktop_notifications::{NotificationAction, apply_action},
    invite_activation::{InviteActivationRelay, SubscriberLifecycle},
    invite_store::InviteStore,
    rta_watcher::SharedInviteStore,
};

#[derive(Clone)]
pub struct SocialService {
    client: Client,
    tokens: Arc<TokenManager>,
    store: SharedInviteStore,
    relay: InviteActivationRelay,
    lifecycle: SubscriberLifecycle,
    friends: Arc<Mutex<Vec<FriendEntry>>>,
    session: Arc<Mutex<Option<ForzaSession>>>,
}

#[derive(Clone)]
struct FriendEntry {
    id: String,
    friend: XboxFriend,
}

impl SocialService {
    pub fn new(
        client: Client,
        tokens: Arc<TokenManager>,
        store: SharedInviteStore,
        relay: InviteActivationRelay,
        lifecycle: SubscriberLifecycle,
    ) -> Self {
        Self {
            client,
            tokens,
            store,
            relay,
            lifecycle,
            friends: Arc::new(Mutex::new(Vec::new())),
            session: Arc::new(Mutex::new(None)),
        }
    }

    async fn handle(&self, request: SocialRequest) -> SocialResponse {
        match request {
            SocialRequest::Snapshot => {
                let refresh_failed = if self.friends.lock().await.is_empty() {
                    self.refresh_friends().await.err()
                } else {
                    None
                };
                self.snapshot(refresh_failed.is_some()).await
            }
            SocialRequest::RefreshFriends => match self.refresh_friends().await {
                Ok(()) => self.snapshot(false).await,
                Err(error) => SocialResponse::Error {
                    message: format!("Could not refresh Xbox friends: {error}"),
                },
            },
            SocialRequest::JoinInvite { invite_id } => {
                let id = match Uuid::parse_str(&invite_id) {
                    Ok(id) => id,
                    Err(_) => return safe_error("This invitation no longer exists"),
                };
                let result = {
                    let mut guard = self.store.lock().await;
                    match guard.as_mut() {
                        Some(store) => apply_action(
                            NotificationAction::Join(id),
                            store,
                            &self.relay,
                            Utc::now(),
                        ),
                        None => Err(crate::desktop_notifications::ActionError::ForzaUnavailable),
                    }
                };
                result_to_response(result)
            }
            SocialRequest::DismissInvite { invite_id } => {
                let id = match Uuid::parse_str(&invite_id) {
                    Ok(id) => id,
                    Err(_) => return safe_error("This invitation no longer exists"),
                };
                let result = {
                    let mut guard = self.store.lock().await;
                    match guard.as_mut() {
                        Some(store) => apply_action(
                            NotificationAction::Dismiss(id),
                            store,
                            &self.relay,
                            Utc::now(),
                        ),
                        None => Err(crate::desktop_notifications::ActionError::ForzaUnavailable),
                    }
                };
                result_to_response(result)
            }
            SocialRequest::InviteFriend { friend_id } => {
                match self.friend_by_id(&friend_id).await {
                    Some(friend) => match self.invite_friend(&friend).await {
                        Ok(()) => SocialResponse::Success {
                            message: format!("Xbox invite sent to {}", friend.gamertag),
                        },
                        Err(error) => safe_error(&format!("Forza invite failed: {error}")),
                    },
                    None => safe_error("Refresh the Xbox friends list and try again"),
                }
            }
            SocialRequest::JoinFriend { friend_id } => match self.friend_by_id(&friend_id).await {
                Some(friend) => match self.join_friend(&friend).await {
                    Ok(()) => SocialResponse::Success {
                        message: format!("Joining {} in Forza Motorsport", friend.gamertag),
                    },
                    Err(error) => safe_error(&format!("Forza join failed: {error}")),
                },
                None => safe_error("Refresh the Xbox friends list and try again"),
            },
        }
    }

    async fn refresh_friends(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut session = self.session.lock().await;
        if session.is_none() {
            *session = Some(ForzaSession::authenticate(&self.client, &self.tokens).await?);
        }
        let friends = session
            .as_mut()
            .expect("session initialized")
            .friends()
            .await?;
        let entries = friends
            .into_iter()
            .map(|friend| FriendEntry {
                id: Uuid::new_v4().to_string(),
                friend,
            })
            .collect();
        *self.friends.lock().await = entries;
        Ok(())
    }

    async fn invite_friend(
        &self,
        friend: &XboxFriend,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut session = self.session.lock().await;
        if session.is_none() {
            *session = Some(ForzaSession::authenticate(&self.client, &self.tokens).await?);
        }
        session
            .as_mut()
            .expect("session initialized")
            .invite(&friend.xuid)
            .await?;
        Ok(())
    }

    async fn join_friend(
        &self,
        friend: &XboxFriend,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut session = self.session.lock().await;
        if session.is_none() {
            *session = Some(ForzaSession::authenticate(&self.client, &self.tokens).await?);
        }
        let uri = session
            .as_mut()
            .expect("session initialized")
            .join_uri(&friend.xuid)
            .await?;
        self.relay.publish(&uri)?;
        Ok(())
    }

    async fn friend_by_id(&self, id: &str) -> Option<XboxFriend> {
        self.friends
            .lock()
            .await
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.friend.clone())
    }

    async fn snapshot(&self, refresh_failed: bool) -> SocialResponse {
        let status = if self.lifecycle.current() == 0 {
            SocialStatus::WaitingForGame
        } else if refresh_failed {
            SocialStatus::Error
        } else if self.store.lock().await.is_none() {
            SocialStatus::Connecting
        } else {
            SocialStatus::Connected
        };
        let friends = self.friends.lock().await.clone();
        let mut store = self.store.lock().await;
        SocialResponse::Snapshot(build_snapshot(store.as_mut(), &friends, status, Utc::now()))
    }
}

fn result_to_response(result: Result<String, impl std::fmt::Display>) -> SocialResponse {
    match result {
        Ok(message) => SocialResponse::Success { message },
        Err(error) => safe_error(&error.to_string()),
    }
}

fn safe_error(message: &str) -> SocialResponse {
    SocialResponse::Error {
        message: message.to_owned(),
    }
}

fn friend_action(_friend: &XboxFriend) -> FriendAction {
    FriendAction::Join
}

fn build_snapshot(
    store: Option<&mut InviteStore>,
    friends: &[FriendEntry],
    status: SocialStatus,
    now: DateTime<Utc>,
) -> SocialSnapshot {
    let invitations = store
        .map(|store| {
            store
                .safe_list(now)
                .into_iter()
                .map(|invite| InviteView {
                    id: invite.id.to_string(),
                    sender_gamertag: invite.sender_gamertag,
                    title_name: invite.title_name,
                    expires_at_unix: invite.expires_at_unix,
                })
                .collect()
        })
        .unwrap_or_default();
    let friends = friends
        .iter()
        .map(|entry| FriendView {
            id: entry.id.clone(),
            gamertag: entry.friend.gamertag.clone(),
            presence: entry
                .friend
                .presence_text
                .clone()
                .or_else(|| entry.friend.presence_state.clone())
                .unwrap_or_else(|| "Xbox status unavailable".into()),
            action: friend_action(&entry.friend),
        })
        .collect();
    SocialSnapshot {
        status,
        invitations,
        friends,
    }
}

pub async fn serve(mut socket: UnixStream, service: SocialService) -> Result<(), Error> {
    let length = socket.read_u32_le().await? as usize;
    if length > xodus::forza::MAX_SOCIAL_FRAME_BYTES {
        return Err(Error::other("social IPC frame is too large"));
    }
    let mut payload = vec![0u8; length];
    socket.read_exact(&mut payload).await?;
    let mut frame = (length as u32).to_le_bytes().to_vec();
    frame.extend_from_slice(&payload);
    let request = decode_frame::<SocialRequest>(&frame).map_err(Error::other)?;
    let response = service.handle(request).await;
    let frame = encode_frame(&response).map_err(Error::other)?;
    socket.write_all(&frame).await
}

#[cfg(test)]
mod tests {
    use super::{FriendEntry, build_snapshot, friend_action};
    use crate::invite_store::InviteStore;
    use chrono::{Duration, TimeZone, Utc};
    use xodus::forza::{
        FriendAction, MultiplayerInvite, SocialResponse, SocialStatus, XboxFriend, encode_frame,
    };

    fn now() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 30, 20, 0, 0).unwrap()
    }

    #[test]
    fn every_friend_exposes_a_revalidated_join_action() {
        assert_eq!(
            friend_action(&XboxFriend {
                gamertag: "In Forza".into(),
                xuid: "222".into(),
                presence_state: Some("Online".into()),
                presence_text: Some("Forza Motorsport".into()),
            }),
            FriendAction::Join
        );
        assert_eq!(
            friend_action(&XboxFriend {
                gamertag: "Elsewhere".into(),
                xuid: "333".into(),
                presence_state: Some("Offline".into()),
                presence_text: None,
            }),
            FriendAction::Join
        );
    }

    #[test]
    fn snapshot_projects_only_safe_invite_and_friend_fields() {
        let mut store = InviteStore::new("111");
        store
            .insert(
                MultiplayerInvite {
                    invited_xuid: "111".into(),
                    sender_xuid: "222".into(),
                    sender_gamertag: "SyntheticFriend".into(),
                    title_name: "Forza Motorsport".into(),
                    connection_string: "secret/session".into(),
                    expires_at: now() + Duration::minutes(10),
                },
                now(),
            )
            .unwrap();
        let friends = vec![FriendEntry {
            id: "opaque-friend".into(),
            friend: XboxFriend {
                gamertag: "SyntheticFriend".into(),
                xuid: "222".into(),
                presence_state: Some("Online".into()),
                presence_text: Some("Forza Motorsport".into()),
            },
        }];

        let response = SocialResponse::Snapshot(build_snapshot(
            Some(&mut store),
            &friends,
            SocialStatus::Connected,
            now(),
        ));
        let json = String::from_utf8(encode_frame(&response).unwrap()[4..].to_vec()).unwrap();
        assert!(json.contains("SyntheticFriend"));
        assert!(json.contains("opaque-friend"));
        assert!(!json.contains("secret/session"));
        assert!(!json.contains("\"222\""));
    }
}
