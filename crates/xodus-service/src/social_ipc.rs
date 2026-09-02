use std::collections::HashMap;
use std::io;
use std::sync::Arc;

use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use xodus::forza::ipc::{encode_frame, read_frame_after_magic};
use xodus::forza::{
    FORZA_CLIENT_ID, FORZA_TITLE_ID, ForzaSession, FriendHandle, InviteActivationUri, Presence,
    SocialFriend, SocialPresence, SocialRequest, SocialResponse, SocialSnapshot,
    XBOX_RELYING_PARTY, XboxFriend, Xuid,
};
use xodus::tokens::TokenManager;

use crate::invite_activation::InviteActivationRelay;

const SOCIAL_MAGIC: [u8; 4] = *b"XDSO";
const STALE_FRIEND_MESSAGE: &str = "Selected friend is no longer available";

#[derive(Debug)]
pub(crate) struct BackendFailure;

pub(crate) trait SocialBackend: Send {
    async fn refresh_friends(&mut self) -> Result<Vec<XboxFriend>, BackendFailure>;
    async fn invite_friend(&mut self, friend: &Xuid) -> Result<(), BackendFailure>;
    async fn join_friend(&mut self, friend: &Xuid) -> Result<InviteActivationUri, BackendFailure>;
}

pub(crate) struct ForzaBackend {
    client: reqwest::Client,
    tokens: Arc<TokenManager>,
    session: Option<ForzaSession>,
}

impl ForzaBackend {
    pub(crate) fn new(tokens: Arc<TokenManager>) -> Self {
        Self {
            client: reqwest::Client::new(),
            tokens,
            session: None,
        }
    }

    async fn session(&mut self) -> Result<&mut ForzaSession, BackendFailure> {
        if self.session.is_none() {
            let authorization = xodus::tokens::mint_title_xsts(
                &self.client,
                &self.tokens,
                FORZA_CLIENT_ID,
                i64::from(FORZA_TITLE_ID),
                XBOX_RELYING_PARTY,
            )
            .await
            .map_err(|_| BackendFailure)?;
            self.session =
                Some(ForzaSession::authenticate(authorization).map_err(|_| BackendFailure)?);
        }
        self.session.as_mut().ok_or(BackendFailure)
    }
}

impl SocialBackend for ForzaBackend {
    async fn refresh_friends(&mut self) -> Result<Vec<XboxFriend>, BackendFailure> {
        self.session()
            .await?
            .friends()
            .await
            .map_err(|_| BackendFailure)
    }

    async fn invite_friend(&mut self, friend: &Xuid) -> Result<(), BackendFailure> {
        self.session()
            .await?
            .invite(friend)
            .await
            .map_err(|_| BackendFailure)
    }

    async fn join_friend(&mut self, friend: &Xuid) -> Result<InviteActivationUri, BackendFailure> {
        self.session()
            .await?
            .join_uri(friend)
            .await
            .map_err(|_| BackendFailure)
    }
}

pub(crate) struct SocialService<B> {
    backend: B,
    snapshot: SocialSnapshot,
    transports: HashMap<FriendHandle, Xuid>,
}

impl<B> SocialService<B>
where
    B: SocialBackend,
{
    pub(crate) fn new(backend: B) -> Self {
        Self {
            backend,
            snapshot: SocialSnapshot {
                friends: Vec::new(),
                status: "Refresh to load friends".to_owned(),
            },
            transports: HashMap::new(),
        }
    }

    pub(crate) async fn handle(
        &mut self,
        request: SocialRequest,
        activations: &InviteActivationRelay,
    ) -> SocialResponse {
        match request {
            SocialRequest::Snapshot => SocialResponse::Snapshot(self.snapshot.clone()),
            SocialRequest::RefreshFriends => self.refresh().await,
            SocialRequest::InviteFriend { friend } => {
                let Some(transport) = self.transports.get(&friend).cloned() else {
                    return stale_friend();
                };
                match self.backend.invite_friend(&transport).await {
                    Ok(()) => SocialResponse::Success {
                        message: "Invite sent".to_owned(),
                    },
                    Err(_) => {
                        log::warn!("Forza invite request failed");
                        SocialResponse::Error {
                            message: "Invite could not be sent".to_owned(),
                        }
                    }
                }
            }
            SocialRequest::JoinFriend { friend } => {
                let Some(transport) = self.transports.get(&friend).cloned() else {
                    return stale_friend();
                };
                match self.backend.join_friend(&transport).await {
                    Ok(uri) => {
                        if activations.publish(uri).is_ok() {
                            SocialResponse::Success {
                                message: "Joining friend".to_owned(),
                            }
                        } else {
                            SocialResponse::Error {
                                message: "Game invite channel is unavailable".to_owned(),
                            }
                        }
                    }
                    Err(_) => {
                        log::warn!("Forza join request failed");
                        SocialResponse::Error {
                            message: "Friend could not be joined".to_owned(),
                        }
                    }
                }
            }
        }
    }

    async fn refresh(&mut self) -> SocialResponse {
        let friends = match self.backend.refresh_friends().await {
            Ok(friends) => friends,
            Err(_) => {
                log::warn!("Forza friend refresh failed");
                return SocialResponse::Error {
                    message: "Friends are unavailable".to_owned(),
                };
            }
        };
        let mut transports = HashMap::with_capacity(friends.len());
        let mut redacted = Vec::with_capacity(friends.len());
        for friend in friends {
            let handle = FriendHandle::new();
            transports.insert(handle, friend.xuid);
            redacted.push(SocialFriend {
                handle,
                display_label: friend.display_name,
                presence: match friend.presence {
                    Presence::Online { .. } => SocialPresence::Online,
                    Presence::Offline => SocialPresence::Offline,
                    Presence::Unknown => SocialPresence::Unknown,
                },
            });
        }
        self.transports = transports;
        self.snapshot = SocialSnapshot {
            friends: redacted,
            status: "Friends refreshed".to_owned(),
        };
        SocialResponse::Snapshot(self.snapshot.clone())
    }
}

fn stale_friend() -> SocialResponse {
    SocialResponse::Error {
        message: STALE_FRIEND_MESSAGE.to_owned(),
    }
}

pub(crate) type SharedSocialService = Arc<Mutex<SocialService<ForzaBackend>>>;

pub(crate) async fn handle_socket(
    socket: &mut tokio::net::UnixStream,
    service: &SharedSocialService,
    activations: &InviteActivationRelay,
) -> io::Result<()> {
    let request = read_frame_after_magic(socket, SOCIAL_MAGIC)
        .await
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let response = service.lock().await.handle(request, activations).await;
    let frame = encode_frame(&response)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    socket.write_all(&frame).await
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use xodus::forza::{
        FriendHandle, InviteActivationUri, Presence, SocialRequest, SocialResponse, XboxFriend,
        Xuid,
    };

    use super::{BackendFailure, SocialBackend, SocialService};
    use crate::invite_activation::InviteActivationRelay;

    const FRIEND_XUID: &str = "2810999999999999";
    const ACTIVATION_URI: &str = "ms-xbl-multiplayer://inviteAccept?invitedUser=2810000000000000&sender=2810999999999999&connectionString=synthetic-connection-secret";

    #[derive(Default)]
    struct Calls {
        invites: Vec<Xuid>,
        joins: Vec<Xuid>,
    }

    struct SyntheticBackend {
        calls: Arc<Mutex<Calls>>,
        friends: Vec<XboxFriend>,
        join_uri: InviteActivationUri,
    }

    impl SocialBackend for SyntheticBackend {
        async fn refresh_friends(&mut self) -> Result<Vec<XboxFriend>, BackendFailure> {
            Ok(self.friends.clone())
        }

        async fn invite_friend(&mut self, friend: &Xuid) -> Result<(), BackendFailure> {
            self.calls.lock().unwrap().invites.push(friend.clone());
            Ok(())
        }

        async fn join_friend(
            &mut self,
            friend: &Xuid,
        ) -> Result<InviteActivationUri, BackendFailure> {
            self.calls.lock().unwrap().joins.push(friend.clone());
            Ok(self.join_uri.clone())
        }
    }

    fn service() -> (SocialService<SyntheticBackend>, Arc<Mutex<Calls>>) {
        let calls = Arc::new(Mutex::new(Calls::default()));
        let backend = SyntheticBackend {
            calls: Arc::clone(&calls),
            friends: vec![XboxFriend {
                xuid: Xuid::new(FRIEND_XUID).unwrap(),
                display_name: "Synthetic Driver".to_owned(),
                presence: Presence::Online {
                    text: Some(format!(
                        "{FRIEND_XUID} synthetic-connection-secret synthetic-authorization-secret ms-xbl-multiplayer"
                    )),
                },
            }],
            join_uri: InviteActivationUri::parse(ACTIVATION_URI).unwrap(),
        };
        (SocialService::new(backend), calls)
    }

    fn snapshot_handle(response: SocialResponse) -> FriendHandle {
        let SocialResponse::Snapshot(snapshot) = response else {
            panic!("expected snapshot response");
        };
        snapshot.friends[0].handle
    }

    #[tokio::test]
    async fn social_snapshot_serialization_contains_no_transport_or_authorization_secrets() {
        let (mut service, _) = service();
        let relay = InviteActivationRelay::default();
        let response = service.handle(SocialRequest::RefreshFriends, &relay).await;
        let json = serde_json::to_string(&response).unwrap();

        for secret in [
            FRIEND_XUID,
            "synthetic-connection-secret",
            "synthetic-authorization-secret",
            "ms-xbl-multiplayer",
        ] {
            assert!(!json.contains(secret), "snapshot leaked {secret}");
        }
        assert!(json.contains("Synthetic Driver"));
    }

    #[tokio::test]
    async fn social_stale_handle_returns_safe_error_without_backend_action() {
        let (mut service, calls) = service();
        let relay = InviteActivationRelay::default();
        let stale = snapshot_handle(service.handle(SocialRequest::RefreshFriends, &relay).await);
        let current = snapshot_handle(service.handle(SocialRequest::RefreshFriends, &relay).await);
        assert_ne!(stale, current);

        let response = service
            .handle(SocialRequest::InviteFriend { friend: stale }, &relay)
            .await;
        assert_eq!(
            response,
            SocialResponse::Error {
                message: "Selected friend is no longer available".to_owned(),
            }
        );
        let calls = calls.lock().unwrap();
        assert!(calls.invites.is_empty());
        assert!(calls.joins.is_empty());
    }

    #[tokio::test]
    async fn social_invite_and_join_dispatch_only_through_synthetic_backend() {
        let (mut service, calls) = service();
        let relay = InviteActivationRelay::default();
        let (_subscription, mut activations) = relay.register().unwrap();
        let handle = snapshot_handle(service.handle(SocialRequest::RefreshFriends, &relay).await);

        assert!(matches!(
            service
                .handle(SocialRequest::InviteFriend { friend: handle }, &relay)
                .await,
            SocialResponse::Success { .. }
        ));
        assert!(matches!(
            service
                .handle(SocialRequest::JoinFriend { friend: handle }, &relay)
                .await,
            SocialResponse::Success { .. }
        ));

        let calls = calls.lock().unwrap();
        assert_eq!(calls.invites, [Xuid::new(FRIEND_XUID).unwrap()]);
        assert_eq!(calls.joins, [Xuid::new(FRIEND_XUID).unwrap()]);
        drop(calls);
        assert_eq!(
            activations.try_recv().unwrap().expose_for_activation(),
            ACTIVATION_URI
        );
    }
}
