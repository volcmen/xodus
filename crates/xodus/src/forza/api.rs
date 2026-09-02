use reqwest_xal::{Client, Response};
use serde_json::{Value, json};
use thiserror::Error;
use url::Url;
use xal::RequestSigner;
use xal::extensions::SigningReqwestBuilder;

use crate::tokens::TitleAuthorization;

use super::model::{parse_activity, parse_friends_value};
use super::{
    FORZA_TITLE_ID, ForzaActivity, InviteActivationUri, ModelError, XboxFriend, Xuid,
    build_invite_accept_uri,
};

const PEOPLEHUB_SERVICE: &str = "Xbox PeopleHub";
const ACTIVITY_SERVICE: &str = "Xbox Multiplayer Activity";
const PEOPLEHUB_BASE: &str = "https://peoplehub.xboxlive.com/";
const ACTIVITY_BASE: &str = "https://multiplayeractivity.xboxlive.com/";

#[derive(Debug, Error, Eq, PartialEq)]
pub enum XboxServiceError {
    #[error("Forza title authorization contains an invalid XUID")]
    InvalidLocalXuid,
    #[error("{service} request signing failed")]
    Signing { service: &'static str },
    #[error("{service} transport failed")]
    Transport { service: &'static str },
    #[error("{service} returned HTTP {status}")]
    Http { service: &'static str, status: u16 },
    #[error("{service} returned invalid JSON")]
    InvalidJson { service: &'static str },
    #[error(transparent)]
    Model(#[from] ModelError),
    #[error("Forza has no published Xbox activity")]
    MissingLocalActivity,
    #[error("the selected friend has no joinable Forza activity")]
    MissingFriendActivity,
}

struct ServiceEndpoints {
    peoplehub: Url,
    activity: Url,
}

impl ServiceEndpoints {
    fn production() -> Self {
        Self {
            peoplehub: Url::parse(PEOPLEHUB_BASE).expect("PeopleHub endpoint must be valid"),
            activity: Url::parse(ACTIVITY_BASE).expect("Activity endpoint must be valid"),
        }
    }
}

pub struct ForzaSession {
    client: Client,
    authorization: String,
    signer: RequestSigner,
    local_xuid: Xuid,
    expires_at: i64,
    endpoints: ServiceEndpoints,
}

impl ForzaSession {
    pub fn authenticate(authorization: TitleAuthorization) -> Result<Self, XboxServiceError> {
        let expires_at = authorization.expires_at();
        let local_xuid =
            Xuid::new(authorization.xuid).map_err(|_| XboxServiceError::InvalidLocalXuid)?;
        Ok(Self {
            client: Client::new(),
            authorization: authorization.authorization,
            signer: authorization.signer,
            local_xuid,
            expires_at,
            endpoints: ServiceEndpoints::production(),
        })
    }

    #[must_use]
    pub fn expires_at_or_before(&self, unix_timestamp: i64) -> bool {
        self.expires_at <= unix_timestamp
    }

    #[cfg(test)]
    fn for_test(
        client: Client,
        authorization: String,
        signer: RequestSigner,
        local_xuid: Xuid,
        base: Url,
        expires_at: i64,
    ) -> Self {
        Self {
            client,
            authorization,
            signer,
            local_xuid,
            expires_at,
            endpoints: ServiceEndpoints {
                peoplehub: base.clone(),
                activity: base,
            },
        }
    }

    pub async fn friends(&mut self) -> Result<Vec<XboxFriend>, XboxServiceError> {
        let url = self
            .endpoints
            .peoplehub
            .join(&format!(
                "users/xuid({})/people/friends/decoration/presenceDetail",
                self.local_xuid.as_str()
            ))
            .expect("validated XUID must produce a valid PeopleHub URL");
        let response = self
            .signed(
                self.client
                    .get(url)
                    .header("Authorization", &self.authorization)
                    .header("x-xbl-contract-version", "7")
                    .header("Accept", "application/json")
                    .header("Accept-Language", "en-US"),
                PEOPLEHUB_SERVICE,
            )
            .await?;
        parse_friends_value(checked_json(response, PEOPLEHUB_SERVICE).await?).map_err(Into::into)
    }

    pub async fn activity(
        &mut self,
        xuid: &Xuid,
    ) -> Result<Option<ForzaActivity>, XboxServiceError> {
        let url = self
            .endpoints
            .activity
            .join(&format!("titles/{FORZA_TITLE_ID}/activities/query"))
            .expect("fixed Activity endpoint must be valid");
        let response = self
            .signed(
                self.client
                    .post(url)
                    .header("Authorization", &self.authorization)
                    .header("x-xbl-contract-version", "1")
                    .header("Accept", "application/json")
                    .header("Accept-Language", "en-US")
                    .json(&json!({ "users": [xuid.as_str()] })),
                ACTIVITY_SERVICE,
            )
            .await?;
        let body = checked_json(response, ACTIVITY_SERVICE).await?;
        parse_activity(&body, xuid).map_err(Into::into)
    }

    pub async fn invite(&mut self, friend: &Xuid) -> Result<(), XboxServiceError> {
        let local_xuid = self.local_xuid.clone();
        let activity = self
            .activity(&local_xuid)
            .await?
            .ok_or(XboxServiceError::MissingLocalActivity)?;
        let url = self
            .endpoints
            .activity
            .join(&format!("titles/{FORZA_TITLE_ID}/invites"))
            .expect("fixed Activity endpoint must be valid");
        let response = self
            .signed(
                self.client
                    .post(url)
                    .header("Authorization", &self.authorization)
                    .header("x-xbl-contract-version", "1")
                    .header("Accept", "application/json")
                    .header("Accept-Language", "en-US")
                    .json(&json!({
                        "invitedUsers": [friend.as_str()],
                        "connectionString": activity.connection_string(),
                    })),
                ACTIVITY_SERVICE,
            )
            .await?;
        ensure_success(&response, ACTIVITY_SERVICE)
    }

    pub async fn join_uri(
        &mut self,
        friend: &Xuid,
    ) -> Result<InviteActivationUri, XboxServiceError> {
        let activity = self
            .activity(friend)
            .await?
            .ok_or(XboxServiceError::MissingFriendActivity)?;
        build_invite_accept_uri(&self.local_xuid, friend, activity.connection_string())
            .map_err(Into::into)
    }

    async fn signed(
        &mut self,
        request: reqwest_xal::RequestBuilder,
        service: &'static str,
    ) -> Result<Response, XboxServiceError> {
        request
            .sign(&mut self.signer, None)
            .await
            .map_err(|_| XboxServiceError::Signing { service })?
            .send()
            .await
            .map_err(|_| XboxServiceError::Transport { service })
    }
}

fn ensure_success(response: &Response, service: &'static str) -> Result<(), XboxServiceError> {
    let status = response.status();
    if status.is_success() {
        Ok(())
    } else {
        Err(XboxServiceError::Http {
            service,
            status: status.as_u16(),
        })
    }
}

async fn checked_json(
    response: Response,
    service: &'static str,
) -> Result<Value, XboxServiceError> {
    ensure_success(&response, service)?;
    let body = response
        .bytes()
        .await
        .map_err(|_| XboxServiceError::Transport { service })?;
    serde_json::from_slice(&body).map_err(|_| XboxServiceError::InvalidJson { service })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use url::Url;
    use xal::response::{TitleEndpoint, TitleEndpointsResponse};
    use xal::{RequestSigner, SignaturePolicyCache, SigningAlgorithm, SigningPolicy};

    use super::{ForzaSession, XboxServiceError};
    use crate::forza::{InviteActivationUri, Presence, Xuid};

    const LOCAL_XUID: &str = "2810000000000000";
    const FRIEND_XUID: &str = "2810000000000001";
    const AUTHORIZATION: &str = "XBL3.0 x=synthetic-uhs;synthetic-token";

    struct MockResponse {
        status: u16,
        reason: &'static str,
        content_type: &'static str,
        body: String,
    }

    impl MockResponse {
        fn json(body: impl Into<String>) -> Self {
            Self {
                status: 200,
                reason: "OK",
                content_type: "application/json",
                body: body.into(),
            }
        }

        fn no_content() -> Self {
            Self {
                status: 204,
                reason: "No Content",
                content_type: "application/json",
                body: String::new(),
            }
        }

        fn unauthorized(body: impl Into<String>) -> Self {
            Self {
                status: 401,
                reason: "Unauthorized",
                content_type: "application/json",
                body: body.into(),
            }
        }
    }

    async fn mock_server(responses: Vec<MockResponse>) -> (Url, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        tokio::spawn(async move {
            for response in responses {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let header_end = loop {
                    let mut chunk = [0_u8; 4096];
                    let count = stream.read(&mut chunk).await.unwrap();
                    assert_ne!(count, 0, "client closed before sending request headers");
                    request.extend_from_slice(&chunk[..count]);
                    if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n")
                    {
                        break index + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                while request.len() < header_end + content_length {
                    let mut chunk = [0_u8; 4096];
                    let count = stream.read(&mut chunk).await.unwrap();
                    assert_ne!(count, 0, "client closed before sending request body");
                    request.extend_from_slice(&chunk[..count]);
                }
                captured
                    .lock()
                    .unwrap()
                    .push(String::from_utf8(request).unwrap());

                let response_head = format!(
                    "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response.status,
                    response.reason,
                    response.content_type,
                    response.body.len()
                );
                stream.write_all(response_head.as_bytes()).await.unwrap();
                stream.write_all(response.body.as_bytes()).await.unwrap();
            }
        });
        (Url::parse(&format!("http://{address}/")).unwrap(), requests)
    }

    fn session(base: Url) -> ForzaSession {
        let signer = local_signer(&base);
        ForzaSession::for_test(
            reqwest_xal::Client::new(),
            AUTHORIZATION.to_owned(),
            signer,
            Xuid::new(LOCAL_XUID).unwrap(),
            base,
            1_000,
        )
    }

    fn local_signer(base: &Url) -> RequestSigner {
        let mut signer = RequestSigner::new();
        signer.signature_policy_cache = SignaturePolicyCache::new(TitleEndpointsResponse {
            end_points: vec![TitleEndpoint {
                protocol: base.scheme().to_owned(),
                host: base.host_str().unwrap().to_owned(),
                host_type: "fqdn".to_owned(),
                path: None,
                relying_party: None,
                sub_relying_party: None,
                token_type: None,
                signature_policy_index: Some(0),
                server_cert_index: None,
            }],
            signature_policies: vec![SigningPolicy {
                version: 1,
                supported_algorithms: vec![SigningAlgorithm::ES256],
                max_body_bytes: 8192,
            }],
            certs: Vec::new(),
            root_certs: Vec::new(),
        });
        signer
    }

    fn activity_body(xuid: &str, connection_string: &str) -> String {
        format!(
            r#"{{"userActivities":[{{"userId":"{xuid}","activities":[{{"titleId":1842668909,"connectionString":"{connection_string}"}}]}}]}}"#
        )
    }

    #[tokio::test]
    async fn forza_session_expiry_predicate_has_a_deterministic_boundary() {
        let (base, _) = mock_server(Vec::new()).await;
        let session = session(base);

        assert!(!session.expires_at_or_before(999));
        assert!(session.expires_at_or_before(1_000));
        assert!(session.expires_at_or_before(1_001));
    }

    #[tokio::test]
    async fn empty_friends_response() {
        let (base, requests) = mock_server(vec![MockResponse::json(r#"{"people":[]}"#)]).await;
        assert!(session(base).friends().await.unwrap().is_empty());

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert!(request.starts_with(
            "GET /users/xuid(2810000000000000)/people/friends/decoration/presenceDetail HTTP/1.1\r\n"
        ));
        let request = request.to_ascii_lowercase();
        assert!(request.contains("\r\nauthorization: xbl3.0 x=synthetic-uhs;synthetic-token\r\n"));
        assert!(request.contains("\r\nx-xbl-contract-version: 7\r\n"));
        let signature = request
            .lines()
            .find_map(|line| line.strip_prefix("signature:"))
            .expect("signed PeopleHub request");
        assert!(!signature.trim().is_empty());
    }

    #[tokio::test]
    async fn friend_presence_absent() {
        let (base, _) = mock_server(vec![MockResponse::json(
            r#"{"people":[{"xuid":"2810000000000001","displayName":"Driver One"}]}"#,
        )])
        .await;
        let friends = session(base).friends().await.unwrap();
        assert_eq!(friends[0].presence, Presence::Unknown);
    }

    #[tokio::test]
    async fn local_activity_missing_blocks_invite() {
        let (base, requests) =
            mock_server(vec![MockResponse::json(r#"{"userActivities":[]}"#)]).await;
        let error = session(base)
            .invite(&Xuid::new(FRIEND_XUID).unwrap())
            .await
            .unwrap_err();
        assert_eq!(error, XboxServiceError::MissingLocalActivity);
        assert_eq!(requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn friend_activity_missing_blocks_join_uri() {
        let (base, _) = mock_server(vec![MockResponse::json(r#"{"userActivities":[]}"#)]).await;
        let error = session(base)
            .join_uri(&Xuid::new(FRIEND_XUID).unwrap())
            .await
            .unwrap_err();
        assert_eq!(error, XboxServiceError::MissingFriendActivity);
    }

    #[tokio::test]
    async fn invite_http_204_succeeds_with_signed_requests() {
        let local_activity = activity_body(LOCAL_XUID, "synthetic-connection");
        let (base, requests) = mock_server(vec![
            MockResponse::json(local_activity),
            MockResponse::no_content(),
        ])
        .await;
        session(base)
            .invite(&Xuid::new(FRIEND_XUID).unwrap())
            .await
            .unwrap();

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].starts_with("POST /titles/1842668909/activities/query HTTP/1.1\r\n"));
        assert!(requests[1].starts_with("POST /titles/1842668909/invites HTTP/1.1\r\n"));
        for request in requests.iter() {
            assert!(request.to_ascii_lowercase().contains("signature:"));
            assert!(request.contains("x-xbl-contract-version: 1"));
        }
        assert!(requests[1].contains(FRIEND_XUID));
        assert!(requests[1].contains("synthetic-connection"));
    }

    #[tokio::test]
    async fn invite_http_401_error_is_redacted() {
        let local_activity = activity_body(LOCAL_XUID, "private-connection");
        let (base, _) = mock_server(vec![
            MockResponse::json(local_activity),
            MockResponse::unauthorized(r#"{"private":"response-secret"}"#),
        ])
        .await;
        let error = session(base)
            .invite(&Xuid::new(FRIEND_XUID).unwrap())
            .await
            .unwrap_err();
        assert_eq!(
            error,
            XboxServiceError::Http {
                service: "Xbox Multiplayer Activity",
                status: 401,
            }
        );
        let rendered = format!("{error:?} {error}");
        for secret in [
            "response-secret",
            "synthetic-token",
            FRIEND_XUID,
            "private-connection",
            "ms-xbl-multiplayer",
        ] {
            assert!(!rendered.contains(secret));
        }
    }

    #[tokio::test]
    async fn malformed_json_error_is_redacted() {
        let (base, _) = mock_server(vec![MockResponse::json("private-malformed-body")]).await;
        let error = session(base).friends().await.unwrap_err();
        assert_eq!(
            error,
            XboxServiceError::InvalidJson {
                service: "Xbox PeopleHub",
            }
        );
        let rendered = format!("{error:?} {error}");
        assert!(!rendered.contains("private-malformed-body"));
        assert!(!rendered.contains(AUTHORIZATION));
        assert!(!rendered.contains(LOCAL_XUID));
    }

    #[test]
    fn invalid_activation_scheme_is_rejected() {
        assert!(
            InviteActivationUri::parse(
                "https://inviteAccept?invitedUser=2810000000000000&sender=2810000000000001&connectionString=synthetic"
            )
            .is_err()
        );
    }
}
