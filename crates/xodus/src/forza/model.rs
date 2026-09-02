use std::fmt;

use serde_json::Value;
use thiserror::Error;
use url::Url;

pub const FORZA_CLIENT_ID: &str = "000000004CC9D265";
pub const FORZA_TITLE_ID: i64 = 1842668909;
pub const XBOX_RELYING_PARTY: &str = "http://xboxlive.com";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XboxFriend {
    pub gamertag: String,
    pub xuid: String,
    pub presence_state: Option<String>,
    pub presence_text: Option<String>,
}

impl fmt::Display for XboxFriend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.gamertag)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForzaActivity {
    pub connection_string: String,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ModelError {
    #[error("Xbox PeopleHub response has no people array")]
    MissingPeople,
    #[error("Xbox friend has no gamertag")]
    MissingGamertag,
    #[error("Xbox friend {0} has no XUID")]
    MissingXuid(String),
    #[error("invite URI field {0} is empty")]
    EmptyInviteField(&'static str),
    #[error("invalid invite URI: {0}")]
    InvalidInviteUri(String),
}

pub fn parse_friends(body: &Value) -> Result<Vec<XboxFriend>, ModelError> {
    let people = body
        .get("people")
        .and_then(Value::as_array)
        .ok_or(ModelError::MissingPeople)?;

    people
        .iter()
        .map(|person| {
            let gamertag = person
                .get("gamertag")
                .and_then(Value::as_str)
                .ok_or(ModelError::MissingGamertag)?;
            let xuid = person
                .get("xuid")
                .and_then(Value::as_str)
                .ok_or_else(|| ModelError::MissingXuid(gamertag.to_owned()))?;
            Ok(XboxFriend {
                gamertag: gamertag.to_owned(),
                xuid: xuid.to_owned(),
                presence_state: person
                    .get("presenceState")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                presence_text: person
                    .get("presenceText")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            })
        })
        .collect()
}

pub fn activity_connection_string(body: &Value, xuid: &str) -> Option<String> {
    body.get("userActivities")?
        .as_array()?
        .iter()
        .filter(|user| user.get("userId").and_then(Value::as_str) == Some(xuid))
        .flat_map(|user| {
            user.get("activities")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .find(|activity| {
            activity.get("titleId").is_some_and(|title_id| {
                title_id.as_i64() == Some(FORZA_TITLE_ID)
                    || title_id.as_str() == Some(&FORZA_TITLE_ID.to_string())
            })
        })?
        .get("connectionString")?
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

pub fn build_invite_accept_uri(
    invited_user: &str,
    sender: &str,
    connection_string: &str,
) -> Result<String, ModelError> {
    for (name, value) in [
        ("invitedUser", invited_user),
        ("sender", sender),
        ("connectionString", connection_string),
    ] {
        if value.is_empty() {
            return Err(ModelError::EmptyInviteField(name));
        }
    }

    let mut uri = Url::parse("ms-xbl-multiplayer://inviteAccept")
        .map_err(|error| ModelError::InvalidInviteUri(error.to_string()))?;
    uri.query_pairs_mut()
        .append_pair("invitedUser", invited_user)
        .append_pair("sender", sender)
        .append_pair("connectionString", connection_string);
    Ok(uri.into())
}

#[cfg(test)]
mod tests {
    use super::{
        FORZA_TITLE_ID, activity_connection_string, build_invite_accept_uri, parse_friends,
    };
    use serde_json::json;

    #[test]
    fn parses_complete_peoplehub_friends() {
        let body = json!({
            "people": [
                {
                    "gamertag": "SyntheticFriend",
                    "xuid": "222",
                    "presenceState": "Online",
                    "presenceText": "Forza Motorsport"
                },
                {
                    "gamertag": "Driver Two",
                    "xuid": "333",
                    "presenceState": "Offline"
                }
            ]
        });

        let friends = parse_friends(&body).unwrap();
        assert_eq!(friends.len(), 2);
        assert_eq!(friends[0].gamertag, "SyntheticFriend");
        assert_eq!(friends[0].xuid, "222");
        assert_eq!(friends[0].presence_state.as_deref(), Some("Online"));
        assert_eq!(
            friends[0].presence_text.as_deref(),
            Some("Forza Motorsport")
        );
        assert_eq!(friends[1].presence_text, None);
    }

    #[test]
    fn rejects_friend_without_identity() {
        assert!(parse_friends(&json!({ "people": [{ "gamertag": "Broken" }] })).is_err());
        assert!(parse_friends(&json!({ "people": [{ "xuid": "222" }] })).is_err());
    }

    #[test]
    fn extracts_only_the_requested_users_forza_activity() {
        let body = json!({
            "userActivities": [
                {
                    "userId": "222",
                    "activities": [{
                        "titleId": FORZA_TITLE_ID,
                        "connectionString": "numeric-title"
                    }]
                },
                {
                    "userId": "333",
                    "activities": [{
                        "titleId": FORZA_TITLE_ID.to_string(),
                        "connectionString": "string-title"
                    }]
                },
                {
                    "userId": "444",
                    "activities": [{
                        "titleId": 123,
                        "connectionString": "wrong-title"
                    }]
                }
            ]
        });

        assert_eq!(
            activity_connection_string(&body, "222").unwrap(),
            "numeric-title"
        );
        assert_eq!(
            activity_connection_string(&body, "333").unwrap(),
            "string-title"
        );
        assert_eq!(activity_connection_string(&body, "444"), None);
    }

    #[test]
    fn ignores_empty_connection_strings() {
        let body = json!({
            "userActivities": [{
                "userId": "222",
                "activities": [{
                    "titleId": FORZA_TITLE_ID,
                    "connectionString": ""
                }]
            }]
        });

        assert_eq!(activity_connection_string(&body, "222"), None);
    }

    #[test]
    fn builds_encoded_invite_accept_uri() {
        assert_eq!(
            build_invite_accept_uri("111", "222", "abc/def=ghi+jkl").unwrap(),
            "ms-xbl-multiplayer://inviteAccept?invitedUser=111&sender=222&connectionString=abc%2Fdef%3Dghi%2Bjkl"
        );
    }

    #[test]
    fn rejects_empty_invite_uri_fields() {
        assert!(build_invite_accept_uri("", "222", "session").is_err());
        assert!(build_invite_accept_uri("111", "", "session").is_err());
        assert!(build_invite_accept_uri("111", "222", "").is_err());
    }
}
