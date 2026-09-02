use std::fmt;
use std::future::Future;

use thiserror::Error;
use xal::response::{DeviceToken, TitleToken, UserToken, XSTSToken};
use xal::{RequestSigner, XalAuthenticator};

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
    MintFuture: Future<Output = Result<(XalAuthenticator, XSTSToken), TitleAuthorizationError>>,
{
    let (authenticator, xsts) = mint(
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
        signer: authenticator.request_signer(),
        token,
        expires_at,
    })
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
        |mut authenticator, device, title, user, relying_party| async move {
            let xsts = authenticator
                .get_xsts_token(Some(&device), Some(&title), Some(&user), &relying_party)
                .await
                .map_err(|_| TitleAuthorizationError::Exchange)?;
            Ok((authenticator, xsts))
        },
    )
    .await
}

#[cfg(test)]
mod title_authorization_tests {
    use chrono::{TimeZone, Utc};
    use xal::XalAuthenticator;
    use xal::response::{
        DeviceToken, TitleToken, UserToken, XADDisplayClaims, XATDisplayClaims, XAUDisplayClaims,
        XSTSDisplayClaims, XSTSToken, XTokenResponse,
    };

    use super::{TitleTokenBundle, mint_title_xsts_bundle_with};

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
                Ok::<_, super::TitleAuthorizationError>((authenticator, xsts))
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
    async fn title_xsts_bundle_rejects_missing_per_token_identity() {
        let result = mint_title_xsts_bundle_with(
            fixture_bundle(),
            "rp://forza",
            |authenticator, _, _, _, _| async move {
                let xsts: XSTSToken = token(
                    "synthetic-xsts",
                    XSTSDisplayClaims {
                        xui: [Default::default()].into(),
                    },
                );
                Ok::<_, super::TitleAuthorizationError>((authenticator, xsts))
            },
        )
        .await;

        assert!(result.is_err());
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
