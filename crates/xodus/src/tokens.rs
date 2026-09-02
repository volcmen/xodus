use std::fmt;
use std::future::Future;

use thiserror::Error;
use xal::extensions::{CorrelationVectorReqwestBuilder, SigningReqwestBuilder};
use xal::request::{XSTSProperties, XTokenRequest};
use xal::response::{DeviceToken, TitleToken, UserToken, XSTSToken};
use xal::{Constants, RequestSigner, XalAuthenticator};

pub mod backend;
pub mod device;
pub mod manager;
pub mod store;

pub use manager::{PASSPORT_STS, TokenManager};

/// A title-claimed Xbox authorization and the proof-key signer that owns it.
///
/// The signer comes from the same authenticator that minted the device, title,
/// user, and relying-party XSTS tokens. Keeping it in this value prevents a
/// caller from accidentally signing with a different proof key.
pub struct TitleAuthorization {
    pub authorization: String,
    pub user_hash: String,
    pub xuid: String,
    pub signer: RequestSigner,
    token: String,
    expires_at: i64,
}

impl TitleAuthorization {
    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }

    #[must_use]
    pub fn expires_at(&self) -> i64 {
        self.expires_at
    }
}

impl fmt::Debug for TitleAuthorization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TitleAuthorization")
            .field("authorization", &"[REDACTED]")
            .field("user_hash", &"[REDACTED]")
            .field("xuid", &"[REDACTED]")
            .field("signer", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum TitleAuthorizationError {
    #[error("title authentication failed")]
    Authentication,
    #[error("title-scoped Xbox token exchange failed")]
    Exchange,
    #[error("title-scoped Xbox token has no user hash")]
    MissingUserHash,
    #[error("title-scoped Xbox token has no XUID")]
    MissingXuid,
}

struct TitleTokenBundle {
    authenticator: XalAuthenticator,
    device: DeviceToken,
    title: TitleToken,
    user: UserToken,
}

impl TitleTokenBundle {
    fn new(
        authenticator: XalAuthenticator,
        device: DeviceToken,
        title: TitleToken,
        user: UserToken,
    ) -> Self {
        Self {
            authenticator,
            device,
            title,
            user,
        }
    }

    #[cfg(test)]
    fn signer(&self) -> RequestSigner {
        self.authenticator.request_signer()
    }
}

async fn mint_title_xsts_bundle_with<Mint, MintFuture>(
    bundle: TitleTokenBundle,
    relying_party: &str,
    mint: Mint,
) -> Result<TitleAuthorization, TitleAuthorizationError>
where
    Mint: FnOnce(XalAuthenticator, DeviceToken, TitleToken, UserToken, String) -> MintFuture,
    MintFuture: Future<Output = Result<(RequestSigner, XSTSToken), TitleAuthorizationError>>,
{
    let (signer, xsts) = mint(
        bundle.authenticator,
        bundle.device,
        bundle.title,
        bundle.user,
        relying_party.to_owned(),
    )
    .await?;

    let claims = xsts
        .display_claims
        .as_ref()
        .and_then(|claims| claims.xui.first());
    let user_hash = claims
        .and_then(|claims| claims.get("uhs"))
        .filter(|value| !value.is_empty())
        .cloned()
        .ok_or(TitleAuthorizationError::MissingUserHash)?;
    let xuid = claims
        .and_then(|claims| claims.get("xid"))
        .filter(|value| !value.is_empty())
        .cloned()
        .ok_or(TitleAuthorizationError::MissingXuid)?;

    let token = xsts.token;
    let expires_at = xsts.not_after.timestamp();
    Ok(TitleAuthorization {
        authorization: format!("XBL3.0 x={user_hash};{token}"),
        user_hash,
        xuid,
        signer,
        token,
        expires_at,
    })
}

async fn safe_title_xsts_exchange(
    mut signer: RequestSigner,
    device: &DeviceToken,
    title: &TitleToken,
    user: &UserToken,
    relying_party: &str,
    sandbox_id: &str,
    endpoint: &str,
) -> Result<(RequestSigner, XSTSToken), TitleAuthorizationError> {
    let body = XTokenRequest::<XSTSProperties> {
        relying_party,
        token_type: "JWT",
        properties: XSTSProperties {
            sandbox_id,
            device_token: Some(&device.token),
            title_token: Some(&title.token),
            user_tokens: vec![&user.token],
        },
    };
    let mut correlation_vector = xal::cvlib::CorrelationVector::new();
    let client = reqwest_xal::Client::new();

    log::debug!("Sending Xbox authorization request: method=POST service=title-xsts");
    let response = client
        .post(endpoint)
        .header("x-xbl-contract-version", "1")
        .add_cv(&mut correlation_vector)
        .map_err(|_| TitleAuthorizationError::Exchange)?
        .json(&body)
        .sign(&mut signer, None)
        .await
        .map_err(|_| TitleAuthorizationError::Exchange)?
        .send()
        .await
        .map_err(|_| TitleAuthorizationError::Exchange)?;
    let status = response.status();
    log::debug!(
        "Received Xbox authorization response: service=title-xsts status={}",
        status.as_u16()
    );
    let response = response
        .error_for_status()
        .map_err(|_| TitleAuthorizationError::Exchange)?;
    let xsts = response
        .json::<XSTSToken>()
        .await
        .map_err(|_| TitleAuthorizationError::Exchange)?;

    Ok((signer, xsts))
}

/// Mint a relying-party XSTS token from the one SISU device identity and retain
/// the matching request signer.
pub async fn mint_title_xsts(
    client: &reqwest::Client,
    tokens: &TokenManager,
    client_id: &str,
    title_id: i64,
    relying_party: &str,
) -> Result<TitleAuthorization, TitleAuthorizationError> {
    let (authenticator, sisu, device) = crate::auth::do_sisu(client, tokens, client_id, title_id)
        .await
        .map_err(|_| TitleAuthorizationError::Authentication)?;
    let bundle = TitleTokenBundle::new(authenticator, device, sisu.title_token, sisu.user_token);
    mint_title_xsts_bundle_with(
        bundle,
        relying_party,
        |authenticator, device, title, user, relying_party| async move {
            let signer = authenticator.request_signer();
            safe_title_xsts_exchange(
                signer,
                &device,
                &title,
                &user,
                &relying_party,
                &authenticator.sandbox_id(),
                Constants::XBOX_XSTS_AUTH_URL,
            )
            .await
        },
    )
    .await
}

#[cfg(test)]
mod title_authorization_tests {
    use std::sync::{Mutex, Once};

    use chrono::{TimeZone, Utc};
    use log::{Level, LevelFilter, Log, Metadata, Record};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use xal::XalAuthenticator;
    use xal::response::{
        DeviceToken, TitleEndpoint, TitleEndpointsResponse, TitleToken, UserToken,
        XADDisplayClaims, XATDisplayClaims, XAUDisplayClaims, XSTSDisplayClaims, XSTSToken,
        XTokenResponse,
    };
    use xal::{RequestSigner, SignaturePolicyCache, SigningAlgorithm, SigningPolicy};

    use super::{
        TitleAuthorizationError, TitleTokenBundle, mint_title_xsts_bundle_with,
        safe_title_xsts_exchange,
    };

    static TEST_LOGGER: CapturingLogger = CapturingLogger {
        entries: Mutex::new(Vec::new()),
    };
    static INSTALL_LOGGER: Once = Once::new();

    struct CapturingLogger {
        entries: Mutex<Vec<String>>,
    }

    impl Log for CapturingLogger {
        fn enabled(&self, metadata: &Metadata<'_>) -> bool {
            metadata.level() <= Level::Trace
        }

        fn log(&self, record: &Record<'_>) {
            if self.enabled(record.metadata()) {
                self.entries
                    .lock()
                    .expect("capture test log")
                    .push(record.args().to_string());
            }
        }

        fn flush(&self) {}
    }

    fn start_log_capture() {
        INSTALL_LOGGER.call_once(|| {
            log::set_logger(&TEST_LOGGER).expect("install test logger");
            log::set_max_level(LevelFilter::Trace);
        });
        TEST_LOGGER.entries.lock().expect("clear test log").clear();
    }

    fn captured_logs() -> String {
        TEST_LOGGER
            .entries
            .lock()
            .expect("read test log")
            .join("\n")
    }

    fn token<T>(value: &str, display_claims: T) -> XTokenResponse<T> {
        XTokenResponse {
            issue_instant: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            not_after: Utc.with_ymd_and_hms(2100, 1, 1, 0, 0, 0).unwrap(),
            token: value.to_owned(),
            display_claims: Some(display_claims),
        }
    }

    fn fixture_bundle() -> TitleTokenBundle {
        let authenticator = XalAuthenticator::default();
        let device: DeviceToken = token(
            "device-one",
            XADDisplayClaims {
                xdi: [("did".to_owned(), "device-one".to_owned())].into(),
            },
        );
        let title: TitleToken = token(
            "title-on-device-one",
            XATDisplayClaims {
                xti: [("tid".to_owned(), "1842668909".to_owned())].into(),
            },
        );
        let user: UserToken = token(
            "user-on-device-one",
            XAUDisplayClaims {
                xui: [[("uhs".to_owned(), "sisu-user".to_owned())].into()].into(),
            },
        );

        TitleTokenBundle::new(authenticator, device, title, user)
    }

    #[tokio::test]
    async fn title_xsts_bundle_keeps_one_device_identity() {
        let bundle = fixture_bundle();
        let expected_proof_key =
            serde_json::to_value(bundle.signer().get_proof_key()).expect("serialize proof key");

        let result = mint_title_xsts_bundle_with(
            bundle,
            "rp://forza",
            |authenticator, device, title, user, relying_party| async move {
                assert_eq!(device.token, "device-one");
                assert_eq!(title.token, "title-on-device-one");
                assert_eq!(user.token, "user-on-device-one");
                assert_eq!(relying_party, "rp://forza");

                let xsts: XSTSToken = token(
                    "synthetic-xsts",
                    XSTSDisplayClaims {
                        xui: [[
                            ("uhs".to_owned(), "synthetic-uhs".to_owned()),
                            ("xid".to_owned(), "2810000000000000".to_owned()),
                        ]
                        .into()]
                        .into(),
                    },
                );
                Ok::<_, super::TitleAuthorizationError>((authenticator.request_signer(), xsts))
            },
        )
        .await
        .expect("synthetic title authorization");

        assert_eq!(result.user_hash, "synthetic-uhs");
        assert_eq!(result.xuid, "2810000000000000");
        assert_eq!(
            result.authorization,
            "XBL3.0 x=synthetic-uhs;synthetic-xsts"
        );
        assert_eq!(result.token(), "synthetic-xsts");
        assert_eq!(result.expires_at(), 4_102_444_800);
        assert_eq!(
            serde_json::to_value(result.signer.get_proof_key()).expect("serialize proof key"),
            expected_proof_key
        );
    }

    #[tokio::test]
    async fn title_xsts_bundle_reports_missing_user_hash() {
        let result = mint_title_xsts_bundle_with(
            fixture_bundle(),
            "rp://forza",
            |authenticator, _, _, _, _| async move {
                let xsts: XSTSToken = token(
                    "synthetic-xsts",
                    XSTSDisplayClaims {
                        xui: [[("xid".to_owned(), "2810000000000000".to_owned())].into()].into(),
                    },
                );
                Ok::<_, super::TitleAuthorizationError>((authenticator.request_signer(), xsts))
            },
        )
        .await;

        assert!(matches!(
            result,
            Err(TitleAuthorizationError::MissingUserHash)
        ));
    }

    #[tokio::test]
    async fn title_xsts_bundle_reports_missing_xuid() {
        let result = mint_title_xsts_bundle_with(
            fixture_bundle(),
            "rp://forza",
            |authenticator, _, _, _, _| async move {
                let xsts: XSTSToken = token(
                    "synthetic-xsts",
                    XSTSDisplayClaims {
                        xui: [[("uhs".to_owned(), "synthetic-uhs".to_owned())].into()].into(),
                    },
                );
                Ok::<_, super::TitleAuthorizationError>((authenticator.request_signer(), xsts))
            },
        )
        .await;

        assert!(matches!(result, Err(TitleAuthorizationError::MissingXuid)));
    }

    fn local_signer(endpoint: &str) -> RequestSigner {
        let url = reqwest::Url::parse(endpoint).expect("parse mock URL");
        let mut signer = RequestSigner::new();
        signer.signature_policy_cache = SignaturePolicyCache::new(TitleEndpointsResponse {
            end_points: vec![TitleEndpoint {
                protocol: url.scheme().to_owned(),
                host: url.host_str().expect("mock host").to_owned(),
                host_type: "fqdn".to_owned(),
                path: Some(url.path().to_owned()),
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

    async fn mock_xsts_server(response_body: String) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local mock");
        let endpoint = format!("http://{}/xsts/authorize", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept mock request");
            let mut request = Vec::new();
            let mut body_end = None;
            let mut content_length = None;
            loop {
                let mut chunk = [0_u8; 4096];
                let count = socket.read(&mut chunk).await.expect("read mock request");
                assert_ne!(count, 0, "request ended before the body was complete");
                request.extend_from_slice(&chunk[..count]);

                if body_end.is_none()
                    && let Some(index) = request.windows(4).position(|part| part == b"\r\n\r\n")
                {
                    let end = index + 4;
                    let headers = String::from_utf8_lossy(&request[..end]);
                    content_length = headers.lines().find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().expect("content length"))
                    });
                    body_end = Some(end);
                }

                if let (Some(end), Some(length)) = (body_end, content_length)
                    && request.len() >= end + length
                {
                    break;
                }
            }

            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("write mock response");
            String::from_utf8(request).expect("UTF-8 mock request")
        });
        (endpoint, task)
    }

    #[tokio::test]
    async fn safe_title_xsts_exchange_never_logs_credentials_and_preserves_identity() {
        const DEVICE: &str = "sentinel-device-secret";
        const TITLE: &str = "sentinel-title-secret";
        const USER: &str = "sentinel-user-secret";
        const XSTS: &str = "sentinel-xsts-secret";
        const UHS: &str = "sentinel-uhs-secret";
        const XUID: &str = "sentinel-xuid-secret";

        start_log_capture();
        let response = format!(
            r#"{{"IssueInstant":"2026-01-01T00:00:00Z","NotAfter":"2100-01-01T00:00:00Z","Token":"{XSTS}","DisplayClaims":{{"xui":[{{"uhs":"{UHS}","xid":"{XUID}"}}]}}}}"#
        );
        let (endpoint, request_task) = mock_xsts_server(response).await;
        let signer = local_signer(&endpoint);
        let expected_proof_key =
            serde_json::to_value(signer.get_proof_key()).expect("serialize proof key");
        let device: DeviceToken = token(
            DEVICE,
            XADDisplayClaims {
                xdi: Default::default(),
            },
        );
        let title: TitleToken = token(
            TITLE,
            XATDisplayClaims {
                xti: Default::default(),
            },
        );
        let user: UserToken = token(
            USER,
            XAUDisplayClaims {
                xui: Default::default(),
            },
        );

        let result = safe_title_xsts_exchange(
            signer,
            &device,
            &title,
            &user,
            "rp://forza",
            "RETAIL",
            &endpoint,
        )
        .await;
        let (signer, xsts) = result.unwrap_or_else(|error| {
            panic!(
                "synthetic safe XSTS exchange failed: {error}; {}",
                captured_logs()
            )
        });
        let request = request_task.await.expect("join mock server");

        assert!(request.contains(DEVICE));
        assert!(request.contains(TITLE));
        assert!(request.contains(USER));
        assert_eq!(xsts.token, XSTS);
        assert_eq!(
            serde_json::to_value(signer.get_proof_key()).expect("serialize proof key"),
            expected_proof_key
        );

        let logs = captured_logs();
        for sentinel in [DEVICE, TITLE, USER, XSTS, UHS, XUID] {
            assert!(
                !logs.contains(sentinel),
                "credential leaked to logs: {sentinel}"
            );
        }
    }

    #[test]
    fn title_authorization_debug_redacts_identity_and_token() {
        let mut signer = XalAuthenticator::default().request_signer();
        let authorization = super::TitleAuthorization {
            authorization: "XBL3.0 x=private-uhs;private-token".to_owned(),
            user_hash: "private-uhs".to_owned(),
            xuid: "2810000000000000".to_owned(),
            signer: signer.clone(),
            token: "private-token".to_owned(),
            expires_at: 4_102_444_800,
        };

        let debug = format!("{authorization:?}");
        assert!(!debug.contains("private-token"));
        assert!(!debug.contains("private-uhs"));
        assert!(!debug.contains("2810000000000000"));

        // Keep the signer usable after formatting; Debug must not consume it.
        let _ = &mut signer;
    }
}
