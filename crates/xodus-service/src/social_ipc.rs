use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
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
const SOCIAL_TIMEOUT_MESSAGE: &str = "Social request timed out";
/// Bound the complete XDSO operation, including mutex acquisition and backend I/O.
const SOCIAL_OPERATION_TIMEOUT: Duration = Duration::from_secs(30);
/// Renew title authorization one minute early to avoid expiring during a request.
const SESSION_REFRESH_MARGIN_SECONDS: i64 = 60;

#[derive(Debug)]
pub(crate) struct BackendFailure;

pub(crate) trait SocialBackend: Send {
    async fn refresh_friends(&mut self) -> Result<Vec<XboxFriend>, BackendFailure>;
    async fn invite_friend(&mut self, friend: &Xuid) -> Result<(), BackendFailure>;
    async fn join_friend(&mut self, friend: &Xuid) -> Result<InviteActivationUri, BackendFailure>;
}

trait BackendSession: Send {
    fn expires_at_or_before(&self, unix_timestamp: i64) -> bool;
    async fn refresh_friends(&mut self) -> Result<Vec<XboxFriend>, BackendFailure>;
    async fn invite_friend(&mut self, friend: &Xuid) -> Result<(), BackendFailure>;
    async fn join_friend(&mut self, friend: &Xuid) -> Result<InviteActivationUri, BackendFailure>;
}

impl BackendSession for ForzaSession {
    fn expires_at_or_before(&self, unix_timestamp: i64) -> bool {
        ForzaSession::expires_at_or_before(self, unix_timestamp)
    }

    async fn refresh_friends(&mut self) -> Result<Vec<XboxFriend>, BackendFailure> {
        self.friends().await.map_err(|_| BackendFailure)
    }

    async fn invite_friend(&mut self, friend: &Xuid) -> Result<(), BackendFailure> {
        self.invite(friend).await.map_err(|_| BackendFailure)
    }

    async fn join_friend(&mut self, friend: &Xuid) -> Result<InviteActivationUri, BackendFailure> {
        self.join_uri(friend).await.map_err(|_| BackendFailure)
    }
}

trait SessionMinter: Send {
    type Session: BackendSession;

    async fn mint(&mut self) -> Result<Self::Session, BackendFailure>;
}

struct ProductionSessionMinter {
    client: reqwest::Client,
    tokens: Arc<TokenManager>,
}

impl SessionMinter for ProductionSessionMinter {
    type Session = ForzaSession;

    async fn mint(&mut self) -> Result<Self::Session, BackendFailure> {
        let authorization = xodus::tokens::mint_title_xsts(
            &self.client,
            &self.tokens,
            FORZA_CLIENT_ID,
            i64::from(FORZA_TITLE_ID),
            XBOX_RELYING_PARTY,
        )
        .await
        .map_err(|_| BackendFailure)?;
        ForzaSession::authenticate(authorization).map_err(|_| BackendFailure)
    }
}

struct SessionBackend<M>
where
    M: SessionMinter,
{
    minter: M,
    session: Option<M::Session>,
}

impl<M> SessionBackend<M>
where
    M: SessionMinter,
{
    fn new(minter: M) -> Self {
        Self {
            minter,
            session: None,
        }
    }

    async fn take_session_at(&mut self, now: i64) -> Result<M::Session, BackendFailure> {
        let refresh_deadline = now.saturating_add(SESSION_REFRESH_MARGIN_SECONDS);
        if let Some(session) = self.session.take()
            && !session.expires_at_or_before(refresh_deadline)
        {
            return Ok(session);
        }
        self.minter.mint().await
    }

    async fn refresh_friends_at(&mut self, now: i64) -> Result<Vec<XboxFriend>, BackendFailure> {
        let mut session = self.take_session_at(now).await?;
        let result = session.refresh_friends().await;
        if result.is_ok() {
            self.session = Some(session);
        }
        result
    }

    async fn invite_friend_at(&mut self, friend: &Xuid, now: i64) -> Result<(), BackendFailure> {
        let mut session = self.take_session_at(now).await?;
        let result = session.invite_friend(friend).await;
        if result.is_ok() {
            self.session = Some(session);
        }
        result
    }

    async fn join_friend_at(
        &mut self,
        friend: &Xuid,
        now: i64,
    ) -> Result<InviteActivationUri, BackendFailure> {
        let mut session = self.take_session_at(now).await?;
        let result = session.join_friend(friend).await;
        if result.is_ok() {
            self.session = Some(session);
        }
        result
    }
}

pub(crate) struct ForzaBackend {
    inner: SessionBackend<ProductionSessionMinter>,
}

impl ForzaBackend {
    pub(crate) fn new(tokens: Arc<TokenManager>) -> Self {
        Self {
            inner: SessionBackend::new(ProductionSessionMinter {
                client: reqwest::Client::new(),
                tokens,
            }),
        }
    }
}

impl SocialBackend for ForzaBackend {
    async fn refresh_friends(&mut self) -> Result<Vec<XboxFriend>, BackendFailure> {
        self.inner.refresh_friends_at(Utc::now().timestamp()).await
    }

    async fn invite_friend(&mut self, friend: &Xuid) -> Result<(), BackendFailure> {
        self.inner
            .invite_friend_at(friend, Utc::now().timestamp())
            .await
    }

    async fn join_friend(&mut self, friend: &Xuid) -> Result<InviteActivationUri, BackendFailure> {
        self.inner
            .join_friend_at(friend, Utc::now().timestamp())
            .await
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

async fn run_social_operation<B>(
    service: &Arc<Mutex<SocialService<B>>>,
    request: SocialRequest,
    activations: &InviteActivationRelay,
    operation_timeout: Duration,
) -> SocialResponse
where
    B: SocialBackend,
{
    match tokio::time::timeout(operation_timeout, async {
        service.lock().await.handle(request, activations).await
    })
    .await
    {
        Ok(response) => response,
        Err(_) => {
            log::warn!("Forza social operation timed out");
            SocialResponse::Error {
                message: SOCIAL_TIMEOUT_MESSAGE.to_owned(),
            }
        }
    }
}

pub(crate) async fn handle_socket(
    socket: &mut tokio::net::UnixStream,
    service: &SharedSocialService,
    activations: &InviteActivationRelay,
) -> io::Result<()> {
    let request = read_frame_after_magic(socket, SOCIAL_MAGIC)
        .await
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let response =
        run_social_operation(service, request, activations, SOCIAL_OPERATION_TIMEOUT).await;
    let frame = encode_frame(&response)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    socket.write_all(&frame).await
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::future::pending;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use xodus::forza::{
        FriendHandle, InviteActivationUri, Presence, SocialRequest, SocialResponse, XboxFriend,
        Xuid,
    };

    use super::{
        BackendFailure, BackendSession, SessionBackend, SessionMinter, SocialBackend,
        SocialService, run_social_operation,
    };
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

    struct DelayedOnceBackend {
        refreshes: usize,
    }

    impl SocialBackend for DelayedOnceBackend {
        async fn refresh_friends(&mut self) -> Result<Vec<XboxFriend>, BackendFailure> {
            self.refreshes += 1;
            if self.refreshes == 1 {
                pending::<()>().await;
            }
            Ok(vec![synthetic_friend()])
        }

        async fn invite_friend(&mut self, _friend: &Xuid) -> Result<(), BackendFailure> {
            Err(BackendFailure)
        }

        async fn join_friend(
            &mut self,
            _friend: &Xuid,
        ) -> Result<InviteActivationUri, BackendFailure> {
            Err(BackendFailure)
        }
    }

    #[derive(Clone, Copy, Eq, PartialEq)]
    enum FailingOperation {
        None,
        Refresh,
        PendingRefresh,
        Invite,
        Join,
    }

    #[derive(Default)]
    struct SessionCalls {
        mints: usize,
        refreshes: usize,
        invites: usize,
        joins: usize,
    }

    struct SyntheticSession {
        calls: Arc<Mutex<SessionCalls>>,
        expires_at: i64,
        failing_operation: FailingOperation,
    }

    impl BackendSession for SyntheticSession {
        fn expires_at_or_before(&self, unix_timestamp: i64) -> bool {
            self.expires_at <= unix_timestamp
        }

        async fn refresh_friends(&mut self) -> Result<Vec<XboxFriend>, BackendFailure> {
            self.calls.lock().unwrap().refreshes += 1;
            if self.failing_operation == FailingOperation::PendingRefresh {
                pending::<()>().await;
            }
            if self.failing_operation == FailingOperation::Refresh {
                return Err(BackendFailure);
            }
            Ok(vec![synthetic_friend()])
        }

        async fn invite_friend(&mut self, _friend: &Xuid) -> Result<(), BackendFailure> {
            self.calls.lock().unwrap().invites += 1;
            if self.failing_operation == FailingOperation::Invite {
                return Err(BackendFailure);
            }
            Ok(())
        }

        async fn join_friend(
            &mut self,
            _friend: &Xuid,
        ) -> Result<InviteActivationUri, BackendFailure> {
            self.calls.lock().unwrap().joins += 1;
            if self.failing_operation == FailingOperation::Join {
                return Err(BackendFailure);
            }
            Ok(InviteActivationUri::parse(ACTIVATION_URI).unwrap())
        }
    }

    struct SyntheticMinter {
        calls: Arc<Mutex<SessionCalls>>,
        sessions: VecDeque<SyntheticSession>,
    }

    impl SessionMinter for SyntheticMinter {
        type Session = SyntheticSession;

        async fn mint(&mut self) -> Result<Self::Session, BackendFailure> {
            self.calls.lock().unwrap().mints += 1;
            self.sessions.pop_front().ok_or(BackendFailure)
        }
    }

    fn synthetic_friend() -> XboxFriend {
        XboxFriend {
            xuid: Xuid::new(FRIEND_XUID).unwrap(),
            display_name: "Synthetic Driver".to_owned(),
            presence: Presence::Online { text: None },
        }
    }

    fn session_backend(
        sessions: impl IntoIterator<Item = (i64, FailingOperation)>,
    ) -> (SessionBackend<SyntheticMinter>, Arc<Mutex<SessionCalls>>) {
        let calls = Arc::new(Mutex::new(SessionCalls::default()));
        let sessions = sessions
            .into_iter()
            .map(|(expires_at, failing_operation)| SyntheticSession {
                calls: Arc::clone(&calls),
                expires_at,
                failing_operation,
            })
            .collect();
        (
            SessionBackend::new(SyntheticMinter {
                calls: Arc::clone(&calls),
                sessions,
            }),
            calls,
        )
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

    fn timeout_error() -> SocialResponse {
        SocialResponse::Error {
            message: "Social request timed out".to_owned(),
        }
    }

    #[tokio::test]
    async fn social_operation_timeout_bounds_mutex_wait_and_recovers() {
        let (service, _) = service();
        let service = Arc::new(tokio::sync::Mutex::new(service));
        let relay = InviteActivationRelay::default();
        let gate = service.lock().await;

        let response = run_social_operation(
            &service,
            SocialRequest::RefreshFriends,
            &relay,
            Duration::from_millis(10),
        )
        .await;
        assert_eq!(response, timeout_error());

        drop(gate);
        let response = run_social_operation(
            &service,
            SocialRequest::RefreshFriends,
            &relay,
            Duration::from_millis(100),
        )
        .await;
        assert!(matches!(response, SocialResponse::Snapshot(_)));
    }

    #[tokio::test]
    async fn social_operation_timeout_drops_backend_future_and_recovers_gate() {
        let service = Arc::new(tokio::sync::Mutex::new(SocialService::new(
            DelayedOnceBackend { refreshes: 0 },
        )));
        let relay = InviteActivationRelay::default();

        let response = run_social_operation(
            &service,
            SocialRequest::RefreshFriends,
            &relay,
            Duration::from_millis(10),
        )
        .await;
        assert_eq!(response, timeout_error());

        let response = run_social_operation(
            &service,
            SocialRequest::RefreshFriends,
            &relay,
            Duration::from_millis(100),
        )
        .await;
        assert!(matches!(response, SocialResponse::Snapshot(_)));
    }

    #[tokio::test]
    async fn social_backend_remints_at_the_expiry_safety_margin_before_reuse() {
        let (mut backend, calls) = session_backend([
            (160, FailingOperation::None),
            (1_000, FailingOperation::None),
        ]);

        backend.refresh_friends_at(0).await.unwrap();
        backend.refresh_friends_at(99).await.unwrap();
        assert_eq!(calls.lock().unwrap().mints, 1);

        backend.refresh_friends_at(100).await.unwrap();
        let calls = calls.lock().unwrap();
        assert_eq!(calls.mints, 2);
        assert_eq!(calls.refreshes, 3);
    }

    #[tokio::test]
    async fn social_backend_cancelled_operation_remints_on_the_next_call() {
        let (mut backend, calls) = session_backend([
            (1_000, FailingOperation::PendingRefresh),
            (1_000, FailingOperation::None),
        ]);

        assert!(
            tokio::time::timeout(Duration::from_millis(10), backend.refresh_friends_at(0))
                .await
                .is_err()
        );
        {
            let calls = calls.lock().unwrap();
            assert_eq!(calls.mints, 1);
            assert_eq!(calls.refreshes, 1);
        }

        let second =
            tokio::time::timeout(Duration::from_millis(100), backend.refresh_friends_at(0)).await;
        assert!(matches!(second, Ok(Ok(_))));
        let calls = calls.lock().unwrap();
        assert_eq!(calls.mints, 2);
        assert_eq!(calls.refreshes, 2);
    }

    #[tokio::test]
    async fn social_backend_successful_operations_reuse_one_session() {
        let (mut backend, calls) = session_backend([(1_000, FailingOperation::None)]);
        let friend = Xuid::new(FRIEND_XUID).unwrap();

        backend.refresh_friends_at(0).await.unwrap();
        backend.invite_friend_at(&friend, 0).await.unwrap();
        backend.join_friend_at(&friend, 0).await.unwrap();

        let calls = calls.lock().unwrap();
        assert_eq!(calls.mints, 1);
        assert_eq!(calls.refreshes, 1);
        assert_eq!(calls.invites, 1);
        assert_eq!(calls.joins, 1);
    }

    #[tokio::test]
    async fn social_backend_peoplehub_failure_discards_session_without_retry() {
        let (mut backend, calls) = session_backend([
            (1_000, FailingOperation::Refresh),
            (1_000, FailingOperation::None),
        ]);

        assert!(backend.refresh_friends_at(0).await.is_err());
        {
            let calls = calls.lock().unwrap();
            assert_eq!(calls.mints, 1);
            assert_eq!(calls.refreshes, 1);
        }
        assert!(backend.refresh_friends_at(0).await.is_ok());
        let calls = calls.lock().unwrap();
        assert_eq!(calls.mints, 2);
        assert_eq!(calls.refreshes, 2);
    }

    #[tokio::test]
    async fn social_backend_invite_failure_discards_session_without_retry() {
        let (mut backend, calls) = session_backend([
            (1_000, FailingOperation::Invite),
            (1_000, FailingOperation::None),
        ]);
        let friend = Xuid::new(FRIEND_XUID).unwrap();

        assert!(backend.invite_friend_at(&friend, 0).await.is_err());
        {
            let calls = calls.lock().unwrap();
            assert_eq!(calls.mints, 1);
            assert_eq!(calls.invites, 1);
        }
        assert!(backend.invite_friend_at(&friend, 0).await.is_ok());
        let calls = calls.lock().unwrap();
        assert_eq!(calls.mints, 2);
        assert_eq!(calls.invites, 2);
    }

    #[tokio::test]
    async fn social_backend_join_failure_discards_session_without_retry() {
        let (mut backend, calls) = session_backend([
            (1_000, FailingOperation::Join),
            (1_000, FailingOperation::None),
        ]);
        let friend = Xuid::new(FRIEND_XUID).unwrap();

        assert!(backend.join_friend_at(&friend, 0).await.is_err());
        {
            let calls = calls.lock().unwrap();
            assert_eq!(calls.mints, 1);
            assert_eq!(calls.joins, 1);
        }
        assert!(backend.join_friend_at(&friend, 0).await.is_ok());
        let calls = calls.lock().unwrap();
        assert_eq!(calls.mints, 2);
        assert_eq!(calls.joins, 2);
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
