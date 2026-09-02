use reqwest::{Client, Response, StatusCode};
use serde_json::{Value, json};
use thiserror::Error;
use xal::{RequestSigner, extensions::SigningReqwestBuilder};

use crate::{auth::do_sisu, tokens::TokenManager};

use super::{
    FORZA_CLIENT_ID, FORZA_TITLE_ID, ForzaActivity, ModelError, XBOX_RELYING_PARTY, XboxFriend,
    activity_connection_string, build_invite_accept_uri, parse_friends, registration_payload,
};

const PEOPLEHUB_SERVICE: &str = "Xbox PeopleHub";
const ACTIVITY_SERVICE: &str = "Xbox Multiplayer Activity";

#[derive(Debug, Error, Eq, PartialEq)]
pub enum XboxServiceError {
    #[error("Forza Xbox authentication failed")]
    Authentication,
    #[error("Xbox token has no XUID")]
    MissingXuid,
    #[error("{service} request failed: {message}")]
    Transport { service: String, message: String },
    #[error("{service} returned HTTP {status}")]
    Http { service: String, status: StatusCode },
    #[error("{service} returned invalid JSON")]
    InvalidJson { service: String },
    #[error(transparent)]
    Model(#[from] ModelError),
    #[error("Forza has no published Xbox activity")]
    MissingLocalActivity,
    #[error("the selected friend has no joinable Forza activity")]
    MissingFriendActivity,
}

pub struct ForzaSession {
    client: Client,
    local_xuid: String,
    authorization: String,
    signer: RequestSigner,
}

impl ForzaSession {
    pub async fn authenticate(
        client: &Client,
        tokens: &TokenManager,
    ) -> Result<Self, XboxServiceError> {
        let (mut auth, sisu, device) = do_sisu(client, tokens, FORZA_CLIENT_ID, FORZA_TITLE_ID)
            .await
            .map_err(|_| XboxServiceError::Authentication)?;
        let xsts = auth
            .get_xsts_token(
                Some(&device),
                Some(&sisu.title_token),
                Some(&sisu.user_token),
                XBOX_RELYING_PARTY,
            )
            .await
            .map_err(|_| XboxServiceError::Authentication)?;
        let local_xuid = xsts
            .display_claims
            .as_ref()
            .and_then(|claims| claims.xui.first())
            .and_then(|claims| claims.get("xid"))
            .cloned()
            .ok_or(XboxServiceError::MissingXuid)?;

        Ok(Self {
            client: client.clone(),
            local_xuid,
            authorization: xsts.authorization_header_value(),
            signer: auth.request_signer(),
        })
    }

    pub fn local_xuid(&self) -> &str {
        &self.local_xuid
    }

    pub fn authorization(&self) -> &str {
        &self.authorization
    }

    pub fn signer_mut(&mut self) -> &mut RequestSigner {
        &mut self.signer
    }

    pub async fn friends(&mut self) -> Result<Vec<XboxFriend>, XboxServiceError> {
        let url = format!(
            "https://peoplehub.xboxlive.com/users/xuid({})/people/friends/decoration/presenceDetail",
            self.local_xuid
        );
        let response = self
            .client
            .get(url)
            .header("Authorization", &self.authorization)
            .header("x-xbl-contract-version", "7")
            .header("Accept", "application/json")
            .header("Accept-Language", "en-US")
            .sign(&mut self.signer, None)
            .await
            .map_err(|error| transport_error(PEOPLEHUB_SERVICE, error))?
            .send()
            .await
            .map_err(|error| transport_error(PEOPLEHUB_SERVICE, error))?;
        parse_friends(&checked_json(response, PEOPLEHUB_SERVICE).await?).map_err(Into::into)
    }

    pub async fn activity(
        &mut self,
        xuid: &str,
    ) -> Result<Option<ForzaActivity>, XboxServiceError> {
        let url = format!(
            "https://multiplayeractivity.xboxlive.com/titles/{FORZA_TITLE_ID}/activities/query"
        );
        let response = self
            .client
            .post(url)
            .header("Authorization", &self.authorization)
            .header("x-xbl-contract-version", "1")
            .header("Accept", "application/json")
            .header("Accept-Language", "en-US")
            .json(&json!({ "users": [xuid] }))
            .sign(&mut self.signer, None)
            .await
            .map_err(|error| transport_error(ACTIVITY_SERVICE, error))?
            .send()
            .await
            .map_err(|error| transport_error(ACTIVITY_SERVICE, error))?;
        let body = checked_json(response, ACTIVITY_SERVICE).await?;
        Ok(activity_connection_string(&body, xuid)
            .map(|connection_string| ForzaActivity { connection_string }))
    }

    pub async fn invite(&mut self, friend_xuid: &str) -> Result<(), XboxServiceError> {
        let local_xuid = self.local_xuid.clone();
        let activity = self
            .activity(&local_xuid)
            .await?
            .ok_or(XboxServiceError::MissingLocalActivity)?;
        let url =
            format!("https://multiplayeractivity.xboxlive.com/titles/{FORZA_TITLE_ID}/invites");
        let response = self
            .client
            .post(url)
            .header("Authorization", &self.authorization)
            .header("x-xbl-contract-version", "1")
            .header("Accept", "application/json")
            .header("Accept-Language", "en-US")
            .json(&json!({
                "invitedUsers": [friend_xuid],
                "connectionString": activity.connection_string,
            }))
            .sign(&mut self.signer, None)
            .await
            .map_err(|error| transport_error(ACTIVITY_SERVICE, error))?
            .send()
            .await
            .map_err(|error| transport_error(ACTIVITY_SERVICE, error))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(XboxServiceError::Http {
                service: ACTIVITY_SERVICE.into(),
                status: response.status(),
            })
        }
    }

    pub async fn join_uri(&mut self, friend_xuid: &str) -> Result<String, XboxServiceError> {
        let activity = self
            .activity(friend_xuid)
            .await?
            .ok_or(XboxServiceError::MissingFriendActivity)?;
        build_invite_accept_uri(&self.local_xuid, friend_xuid, &activity.connection_string)
            .map_err(Into::into)
    }

    pub async fn register_notification_endpoint(
        &mut self,
        instance_id: &str,
        resource_uri: &str,
    ) -> Result<String, XboxServiceError> {
        const SERVICE: &str = "Xbox Notification Service";
        let response = self
            .client
            .post("https://notify.xboxlive.com/system/notifications/endpoints")
            .header("Authorization", &self.authorization)
            .header("x-xbl-contract-version", "1")
            .header("Accept", "application/json")
            .header("Accept-Language", "en-US")
            .header("Content-Type", "application/json; charset=utf-8")
            .header("User-Agent", "XboxServicesAPI/2025.10.20251000.0")
            .json(&registration_payload(instance_id, resource_uri))
            .sign(&mut self.signer, None)
            .await
            .map_err(|error| transport_error(SERVICE, error))?
            .send()
            .await
            .map_err(|error| transport_error(SERVICE, error))?;
        let body = checked_json(response, SERVICE).await?;
        notification_endpoint_id(&body)
    }

    pub async fn unregister_notification_endpoint(
        &mut self,
        endpoint_id: &str,
    ) -> Result<(), XboxServiceError> {
        const SERVICE: &str = "Xbox Notification Service";
        let url =
            format!("https://notify.xboxlive.com/system/notifications/endpoints/{endpoint_id}");
        let response = self
            .client
            .delete(url)
            .header("Authorization", &self.authorization)
            .header("x-xbl-contract-version", "1")
            .header("Accept", "application/json")
            .header("Accept-Language", "en-US")
            .sign(&mut self.signer, None)
            .await
            .map_err(|error| transport_error(SERVICE, error))?
            .send()
            .await
            .map_err(|error| transport_error(SERVICE, error))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(XboxServiceError::Http {
                service: SERVICE.into(),
                status: response.status(),
            })
        }
    }
}

fn notification_endpoint_id(body: &Value) -> Result<String, XboxServiceError> {
    body.get("endpointId")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| XboxServiceError::InvalidJson {
            service: "Xbox Notification Service".into(),
        })
}

fn transport_error(service: &str, error: impl std::fmt::Display) -> XboxServiceError {
    XboxServiceError::Transport {
        service: service.into(),
        message: error.to_string(),
    }
}

async fn checked_json(response: Response, service: &str) -> Result<Value, XboxServiceError> {
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| transport_error(service, error))?;
    if !status.is_success()
        && let Some(detail) = safe_error_detail(&bytes)
    {
        log::debug!("{service} error detail: {detail}");
    }
    checked_json_body(status, &bytes, service)
}

fn safe_error_detail(bytes: &[u8]) -> Option<String> {
    let body: Value = serde_json::from_slice(bytes).ok()?;
    let object = body.as_object()?;
    let fields = ["code", "errorCode", "message", "description", "reason"];
    let details: Vec<String> = fields
        .into_iter()
        .filter_map(|field| {
            let value = object.get(field)?;
            let mut value = match value {
                Value::String(value) => value.clone(),
                Value::Number(value) => value.to_string(),
                Value::Bool(value) => value.to_string(),
                _ => return None,
            };
            value.truncate(256);
            Some(format!("{field}={value}"))
        })
        .collect();
    (!details.is_empty()).then(|| details.join(", "))
}

fn checked_json_body(
    status: StatusCode,
    bytes: &[u8],
    service: &str,
) -> Result<Value, XboxServiceError> {
    if !status.is_success() {
        return Err(XboxServiceError::Http {
            service: service.into(),
            status,
        });
    }
    serde_json::from_slice(bytes).map_err(|_| XboxServiceError::InvalidJson {
        service: service.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::{XboxServiceError, checked_json_body, notification_endpoint_id, safe_error_detail};
    use reqwest::StatusCode;
    use serde_json::json;

    #[test]
    fn exposes_only_allowlisted_xbox_error_fields() {
        assert_eq!(
            safe_error_detail(
                br#"{"code":"BadRequest","message":"Invalid field","private":"never-log"}"#
            ),
            Some("code=BadRequest, message=Invalid field".into())
        );
        assert_eq!(safe_error_detail(br#"{"private":"never-log"}"#), None);
    }

    #[test]
    fn parses_successful_json_response() {
        assert_eq!(
            checked_json_body(StatusCode::OK, br#"{"people":[]}"#, "Xbox PeopleHub").unwrap(),
            json!({ "people": [] })
        );
    }

    #[test]
    fn preserves_service_and_status_without_response_secrets() {
        assert_eq!(
            checked_json_body(
                StatusCode::TOO_MANY_REQUESTS,
                br#"{"private":"do-not-copy"}"#,
                "Xbox PeopleHub",
            )
            .unwrap_err(),
            XboxServiceError::Http {
                service: "Xbox PeopleHub".into(),
                status: StatusCode::TOO_MANY_REQUESTS,
            }
        );
    }

    #[test]
    fn reports_invalid_json_for_success_status() {
        assert_eq!(
            checked_json_body(StatusCode::OK, b"not-json", "Xbox PeopleHub").unwrap_err(),
            XboxServiceError::InvalidJson {
                service: "Xbox PeopleHub".into(),
            }
        );
    }

    #[test]
    fn parses_notification_endpoint_id_without_retaining_response_body() {
        assert_eq!(
            notification_endpoint_id(&json!({ "endpointId": "endpoint-123" })).unwrap(),
            "endpoint-123"
        );
        assert!(notification_endpoint_id(&json!({ "endpointId": "" })).is_err());
        assert!(notification_endpoint_id(&json!({})).is_err());
    }
}
