use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use super::FORZA_TITLE_ID;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiplayerInvite {
    pub invited_xuid: String,
    pub sender_xuid: String,
    pub sender_gamertag: String,
    pub title_name: String,
    pub connection_string: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SafeInvite {
    pub sender_gamertag: String,
    pub title_name: String,
    pub expires_at: DateTime<Utc>,
}

impl MultiplayerInvite {
    pub fn safe(&self) -> SafeInvite {
        SafeInvite {
            sender_gamertag: self.sender_gamertag.clone(),
            title_name: self.title_name.clone(),
            expires_at: self.expires_at,
        }
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum NotificationError {
    #[error("Xbox notification has no Multiplayer Activity payload")]
    MissingPayload,
    #[error("Xbox notification is missing {0}")]
    MissingField(&'static str),
    #[error("Xbox notification targets another user")]
    WrongUser,
    #[error("Xbox notification expiry is invalid")]
    InvalidExpiry,
}

pub fn notification_resource_uri(local_xuid: &str) -> String {
    format!(
        "https://notify.xboxlive.com/users/xuid({local_xuid})/deviceId/current/titleId/{FORZA_TITLE_ID}"
    )
}

pub fn registration_payload(system_id: &str, endpoint_uri: &str) -> Value {
    json!({
        "systemId": system_id,
        "endpointUri": endpoint_uri,
        "platform": "Win32",
        "transport": "RTA",
        "locale": "en-US",
        "titleId": FORZA_TITLE_ID.to_string(),
        "filters": [
            { "action": "Include", "source": 6, "type": 1 },
            { "action": "Include", "source": 6, "type": 8 },
            { "action": "Include", "source": 8, "type": 1 }
        ]
    })
}

pub fn parse_multiplayer_invite(
    event: &Value,
    expected_xuid: &str,
) -> Result<MultiplayerInvite, NotificationError> {
    let data = event
        .get("notificationData")
        .and_then(Value::as_object)
        .ok_or(NotificationError::MissingPayload)?;
    let invited_xuid = xuid_field(data.get("invitedUser"))
        .ok_or(NotificationError::MissingField("invitedUser"))?;
    if invited_xuid != expected_xuid {
        return Err(NotificationError::WrongUser);
    }
    let sender = data
        .get("sender")
        .and_then(Value::as_object)
        .ok_or(NotificationError::MissingField("sender"))?;
    let sender_xuid =
        xuid_field(sender.get("xuid")).ok_or(NotificationError::MissingField("sender.xuid"))?;
    let sender_gamertag = required_string(sender.get("gamertag"), "sender.gamertag")?;
    let title_name = required_string(data.get("titleName"), "titleName")?;
    let connection_string = required_string(data.get("connectionString"), "connectionString")?;
    let expiry = required_string(data.get("expirationTime"), "expirationTime")?;
    let expires_at = DateTime::parse_from_rfc3339(&expiry)
        .map_err(|_| NotificationError::InvalidExpiry)?
        .with_timezone(&Utc);

    Ok(MultiplayerInvite {
        invited_xuid,
        sender_xuid,
        sender_gamertag,
        title_name,
        connection_string,
        expires_at,
    })
}

fn required_string(
    value: Option<&Value>,
    field: &'static str,
) -> Result<String, NotificationError> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or(NotificationError::MissingField(field))
}

fn xuid_field(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(value) if !value.is_empty() => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{notification_resource_uri, parse_multiplayer_invite, registration_payload};
    use serde_json::json;

    fn invite_event() -> serde_json::Value {
        json!({
            "notificationData": {
                "sender": {
                    "xuid": "222",
                    "gamertag": "SyntheticFriend",
                    "modernGamertag": "SyntheticFriend",
                    "modernGamertagSuffix": "",
                    "uniqueModernGamertag": "SyntheticFriend",
                    "imageUrl": "https://images.example/avatar.png"
                },
                "invitedUser": "111",
                "titleName": "Forza Motorsport",
                "titleImageUrl": "https://images.example/forza.png",
                "connectionString": "opaque/session+secret",
                "expirationTime": "2026-08-30T20:15:30Z"
            }
        })
    }

    #[test]
    fn parses_real_multiplayer_activity_notification_shape() {
        let invite = parse_multiplayer_invite(&invite_event(), "111").unwrap();
        assert_eq!(invite.sender_xuid, "222");
        assert_eq!(invite.sender_gamertag, "SyntheticFriend");
        assert_eq!(invite.invited_xuid, "111");
        assert_eq!(invite.title_name, "Forza Motorsport");
        assert_eq!(invite.connection_string, "opaque/session+secret");
        assert_eq!(invite.expires_at.timestamp(), 1_788_120_930);
    }

    #[test]
    fn safe_projection_excludes_connection_material() {
        let invite = parse_multiplayer_invite(&invite_event(), "111").unwrap();
        let serialized = serde_json::to_string(&invite.safe()).unwrap();
        assert!(serialized.contains("SyntheticFriend"));
        assert!(!serialized.contains("opaque/session+secret"));
        assert!(!serialized.contains("connectionString"));
    }

    #[test]
    fn rejects_wrong_user_missing_fields_and_invalid_expiry() {
        assert!(parse_multiplayer_invite(&invite_event(), "999").is_err());

        let mut missing = invite_event();
        missing["notificationData"]
            .as_object_mut()
            .unwrap()
            .remove("connectionString");
        assert!(parse_multiplayer_invite(&missing, "111").is_err());

        let mut invalid_expiry = invite_event();
        invalid_expiry["notificationData"]["expirationTime"] = json!("not-a-time");
        assert!(parse_multiplayer_invite(&invalid_expiry, "111").is_err());
    }

    #[test]
    fn builds_official_notification_resource_and_registration() {
        let resource = notification_resource_uri("111");
        assert_eq!(
            resource,
            "https://notify.xboxlive.com/users/xuid(111)/deviceId/current/titleId/1842668909"
        );

        assert_eq!(
            registration_payload("instance-id", &resource),
            json!({
                "systemId": "instance-id",
                "endpointUri": resource,
                "platform": "Win32",
                "transport": "RTA",
                "locale": "en-US",
                "titleId": "1842668909",
                "filters": [
                    { "action": "Include", "source": 6, "type": 1 },
                    { "action": "Include", "source": 6, "type": 8 },
                    { "action": "Include", "source": 8, "type": 1 }
                ]
            })
        );
    }
}
