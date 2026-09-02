use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct MSATokenRequest {
    pub client_id: String,
    #[serde(default)]
    pub allow_ui: bool,
    #[serde(default, alias = "MSAFullTrust")]
    pub msa_full_trust: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct MSATokenResponse {
    pub token: String,
    pub expiry: i64,
    pub device_rps: String,
    pub device_expiry: i64,
}

/// Request an XSTS token carrying a TITLE claim for one relying party.
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct XstsTokenRequest {
    pub relying_party: String,
    pub client_id: String,
    /// Decimal title ID. MicrosoftGame.config stores it as hexadecimal, so the
    /// Windows-side caller performs that conversion before sending XML.
    pub title_id: String,
}

impl fmt::Debug for XstsTokenRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("XstsTokenRequest")
            .field("relying_party", &"[REDACTED]")
            .field("client_id", &"[REDACTED]")
            .field("title_id", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum XstsTokenRequestError {
    #[error("invalid relying party")]
    InvalidRelyingParty,
    #[error("invalid Xbox client ID")]
    InvalidClientId,
    #[error("invalid Xbox title ID")]
    InvalidTitleId,
}

pub struct ValidatedXstsTokenRequest {
    pub relying_party: String,
    pub client_id: String,
    pub title_id: i64,
}

impl XstsTokenRequest {
    pub fn validate(self) -> Result<ValidatedXstsTokenRequest, XstsTokenRequestError> {
        if self.relying_party.is_empty()
            || self.relying_party.len() > 256
            || !self.relying_party.is_ascii()
        {
            return Err(XstsTokenRequestError::InvalidRelyingParty);
        }
        let relying_party = url::Url::parse(&self.relying_party)
            .map_err(|_| XstsTokenRequestError::InvalidRelyingParty)?;
        if !matches!(relying_party.scheme(), "http" | "https" | "rp")
            || relying_party.host_str().is_none()
            || !relying_party.username().is_empty()
            || relying_party.password().is_some()
        {
            return Err(XstsTokenRequestError::InvalidRelyingParty);
        }

        if self.client_id.len() != 16
            || !self.client_id.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(XstsTokenRequestError::InvalidClientId);
        }

        let title_id = self
            .title_id
            .parse::<u32>()
            .ok()
            .filter(|title_id| *title_id != 0)
            .ok_or(XstsTokenRequestError::InvalidTitleId)?;

        Ok(ValidatedXstsTokenRequest {
            relying_party: self.relying_party,
            client_id: self.client_id,
            title_id: i64::from(title_id),
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct XstsTokenResponse {
    pub token: String,
    pub user_hash: String,
    pub xuid: String,
    pub expiry: i64,
}

impl fmt::Debug for XstsTokenResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("XstsTokenResponse")
            .field("token", &"[REDACTED]")
            .field("user_hash", &"[REDACTED]")
            .field("xuid", &"[REDACTED]")
            .field("expiry", &self.expiry)
            .finish()
    }
}

impl From<crate::tokens::TitleAuthorization> for XstsTokenResponse {
    fn from(authorization: crate::tokens::TitleAuthorization) -> Self {
        let token = authorization.token().to_owned();
        let expiry = authorization.expires_at();
        Self {
            token,
            user_hash: authorization.user_hash,
            xuid: authorization.xuid,
            expiry,
        }
    }
}

#[cfg(test)]
mod title_xsts_tests {
    use super::{XstsTokenRequest, XstsTokenResponse};

    #[test]
    fn title_xsts_request_accepts_bounded_identity_fields() {
        let request: XstsTokenRequest = quick_xml::de::from_str(
            "<XstsTokenRequest><RelyingParty>rp://forza</RelyingParty><ClientId>000000004CC9D265</ClientId><TitleId>1842668909</TitleId></XstsTokenRequest>",
        )
        .expect("deserialize request");

        let request = request.validate().expect("validate request");
        assert_eq!(request.relying_party, "rp://forza");
        assert_eq!(request.client_id, "000000004CC9D265");
        assert_eq!(request.title_id, 1_842_668_909);
    }

    #[test]
    fn title_xsts_request_rejects_unbounded_or_malformed_fields() {
        for request in [
            XstsTokenRequest {
                relying_party: "r".repeat(257),
                client_id: "000000004CC9D265".to_owned(),
                title_id: "1842668909".to_owned(),
            },
            XstsTokenRequest {
                relying_party: "not a relying party".to_owned(),
                client_id: "000000004CC9D265".to_owned(),
                title_id: "1842668909".to_owned(),
            },
            XstsTokenRequest {
                relying_party: "rp://forza".to_owned(),
                client_id: "not-an-app-id".to_owned(),
                title_id: "1842668909".to_owned(),
            },
            XstsTokenRequest {
                relying_party: "rp://forza".to_owned(),
                client_id: "000000004CC9D265".to_owned(),
                title_id: "0".to_owned(),
            },
            XstsTokenRequest {
                relying_party: "rp://forza".to_owned(),
                client_id: "000000004CC9D265".to_owned(),
                title_id: "4294967296".to_owned(),
            },
        ] {
            assert!(request.validate().is_err());
        }
    }

    #[test]
    fn title_xsts_response_serializes_fields_and_redacts_debug() {
        let response = XstsTokenResponse {
            token: "private-token".to_owned(),
            user_hash: "private-uhs".to_owned(),
            xuid: "2810000000000000".to_owned(),
            expiry: 4_102_444_800,
        };

        let xml = quick_xml::se::to_string(&response).expect("serialize response");
        assert!(xml.contains("<Token>private-token</Token>"));
        assert!(xml.contains("<UserHash>private-uhs</UserHash>"));
        assert!(xml.contains("<Xuid>2810000000000000</Xuid>"));
        assert!(xml.contains("<Expiry>4102444800</Expiry>"));

        let debug = format!("{response:?}");
        assert!(!debug.contains("private-token"));
        assert!(!debug.contains("private-uhs"));
        assert!(!debug.contains("2810000000000000"));
    }
}
