use std::fmt;

use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;
use url::Url;

#[derive(Clone, Eq, Hash, PartialEq)]
pub struct Xuid(String);

impl Xuid {
    pub fn new(value: impl Into<String>) -> Result<Self, ModelError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 20
            || !value.bytes().all(|byte| byte.is_ascii_digit())
            || value.parse::<u64>().is_err()
        {
            return Err(ModelError::InvalidXuid);
        }
        Ok(Self(value))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Xuid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Xuid([REDACTED])")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Presence {
    Online { text: Option<String> },
    Offline,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XboxFriend {
    pub xuid: Xuid,
    pub display_name: String,
    pub presence: Presence,
}

#[derive(Clone, Eq, PartialEq)]
pub struct ForzaActivity {
    connection_string: String,
}

impl fmt::Debug for ForzaActivity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ForzaActivity([REDACTED])")
    }
}

impl ForzaActivity {
    pub(crate) fn new(connection_string: String) -> Result<Self, ModelError> {
        if connection_string.is_empty() {
            return Err(ModelError::MissingConnectionString);
        }
        Ok(Self { connection_string })
    }

    pub(crate) fn connection_string(&self) -> &str {
        &self.connection_string
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct InviteActivationUri(Url);

impl InviteActivationUri {
    pub fn parse(value: &str) -> Result<Self, ModelError> {
        let uri = Url::parse(value).map_err(|_| ModelError::InvalidInviteUri)?;
        validate_invite_uri(&uri)?;
        Ok(Self(uri))
    }

    #[must_use]
    pub fn expose_for_activation(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for InviteActivationUri {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("InviteActivationUri([REDACTED])")
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ModelError {
    #[error("invalid XUID")]
    InvalidXuid,
    #[error("Xbox PeopleHub response is invalid")]
    InvalidFriendsResponse,
    #[error("Xbox friend has no display name")]
    MissingDisplayName,
    #[error("Forza activity has no connection string")]
    MissingConnectionString,
    #[error("Xbox Multiplayer Activity response is invalid")]
    InvalidActivityResponse,
    #[error("invalid Xbox invite activation URI")]
    InvalidInviteUri,
}

#[derive(Deserialize)]
struct FriendsResponse {
    people: Vec<TransportFriend>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TransportFriend {
    xuid: String,
    display_name: Option<String>,
    gamertag: Option<String>,
    presence_state: Option<String>,
    presence_text: Option<String>,
}

pub fn parse_friends(body: &str) -> Result<Vec<XboxFriend>, ModelError> {
    let response: FriendsResponse =
        serde_json::from_str(body).map_err(|_| ModelError::InvalidFriendsResponse)?;
    convert_friends(response)
}

pub(crate) fn parse_friends_value(body: Value) -> Result<Vec<XboxFriend>, ModelError> {
    let response: FriendsResponse =
        serde_json::from_value(body).map_err(|_| ModelError::InvalidFriendsResponse)?;
    convert_friends(response)
}

fn convert_friends(response: FriendsResponse) -> Result<Vec<XboxFriend>, ModelError> {
    response
        .people
        .into_iter()
        .map(|friend| {
            let display_name = friend
                .display_name
                .filter(|value| !value.is_empty())
                .or_else(|| friend.gamertag.filter(|value| !value.is_empty()))
                .ok_or(ModelError::MissingDisplayName)?;
            let presence = match friend.presence_state.as_deref() {
                Some(state) if state.eq_ignore_ascii_case("online") => Presence::Online {
                    text: friend.presence_text,
                },
                Some(state) if state.eq_ignore_ascii_case("offline") => Presence::Offline,
                _ => Presence::Unknown,
            };
            Ok(XboxFriend {
                xuid: Xuid::new(friend.xuid)?,
                display_name,
                presence,
            })
        })
        .collect()
}

pub(crate) fn parse_activity(
    body: &Value,
    requested_xuid: &Xuid,
) -> Result<Option<ForzaActivity>, ModelError> {
    let activities = body
        .get("userActivities")
        .and_then(Value::as_array)
        .ok_or(ModelError::InvalidActivityResponse)?;

    for user in activities {
        let user_id = user
            .get("userId")
            .and_then(Value::as_str)
            .ok_or(ModelError::InvalidActivityResponse)?;
        let user_xuid = Xuid::new(user_id)?;
        let user_activities = user
            .get("activities")
            .and_then(Value::as_array)
            .ok_or(ModelError::InvalidActivityResponse)?;
        if &user_xuid != requested_xuid {
            continue;
        }
        for activity in user_activities {
            let has_forza_title = activity.get("titleId").is_some_and(|title_id| {
                title_id.as_u64() == Some(u64::from(super::FORZA_TITLE_ID))
                    || title_id.as_str() == Some(&super::FORZA_TITLE_ID.to_string())
            });
            if !has_forza_title {
                continue;
            }
            let Some(connection_string) = activity
                .get("connectionString")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            return ForzaActivity::new(connection_string.to_owned()).map(Some);
        }
    }
    Ok(None)
}

pub fn build_invite_accept_uri(
    invited_user: &Xuid,
    sender: &Xuid,
    connection_string: &str,
) -> Result<InviteActivationUri, ModelError> {
    if connection_string.is_empty() {
        return Err(ModelError::MissingConnectionString);
    }
    let mut uri = Url::parse("ms-xbl-multiplayer://inviteAccept")
        .map_err(|_| ModelError::InvalidInviteUri)?;
    uri.query_pairs_mut()
        .append_pair("invitedUser", invited_user.as_str())
        .append_pair("sender", sender.as_str())
        .append_pair("connectionString", connection_string);
    validate_invite_uri(&uri)?;
    Ok(InviteActivationUri(uri))
}

fn validate_invite_uri(uri: &Url) -> Result<(), ModelError> {
    if uri.scheme() != "ms-xbl-multiplayer"
        || !uri
            .host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case("inviteAccept"))
        || uri.path() != ""
        || uri.fragment().is_some()
    {
        return Err(ModelError::InvalidInviteUri);
    }

    let mut invited_user = None;
    let mut sender = None;
    let mut connection_string = None;
    for (name, value) in uri.query_pairs() {
        let slot = match name.as_ref() {
            "invitedUser" => &mut invited_user,
            "sender" => &mut sender,
            "connectionString" => &mut connection_string,
            _ => return Err(ModelError::InvalidInviteUri),
        };
        if slot.replace(value.into_owned()).is_some() {
            return Err(ModelError::InvalidInviteUri);
        }
    }

    Xuid::new(invited_user.ok_or(ModelError::InvalidInviteUri)?)?;
    Xuid::new(sender.ok_or(ModelError::InvalidInviteUri)?)?;
    if connection_string
        .as_deref()
        .is_none_or(|connection_string| connection_string.is_empty())
    {
        return Err(ModelError::InvalidInviteUri);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ModelError, Xuid, parse_activity, parse_friends};

    #[test]
    fn peoplehub_prefers_nonempty_display_name_when_gamertag_is_also_present() {
        let friends = parse_friends(
            r#"{"people":[{"xuid":"2810000000000001","displayName":"Driver Display","gamertag":"DriverTag"}]}"#,
        )
        .unwrap();

        assert_eq!(friends[0].display_name, "Driver Display");
    }

    #[test]
    fn peoplehub_uses_nonempty_gamertag_when_display_name_is_unusable() {
        for body in [
            r#"{"people":[{"xuid":"2810000000000001","gamertag":"DriverTag"}]}"#,
            r#"{"people":[{"xuid":"2810000000000001","displayName":"","gamertag":"DriverTag"}]}"#,
        ] {
            let friends = parse_friends(body).unwrap();
            assert_eq!(friends[0].display_name, "DriverTag");
        }
    }

    #[test]
    fn peoplehub_reports_missing_display_name_when_neither_name_is_usable() {
        for body in [
            r#"{"people":[{"xuid":"2810000000000001"}]}"#,
            r#"{"people":[{"xuid":"2810000000000001","displayName":"","gamertag":""}]}"#,
        ] {
            assert_eq!(parse_friends(body), Err(ModelError::MissingDisplayName));
        }
    }

    #[test]
    fn activity_skips_non_joinable_candidates_before_a_valid_forza_session() {
        let requested = Xuid::new("2810000000000001").unwrap();
        let body = json!({
            "userActivities": [{
                "userId": "2810000000000001",
                "activities": [
                    { "titleId": 1842668909 },
                    { "titleId": "1842668909", "connectionString": "" },
                    {
                        "titleId": 1842668909,
                        "connectionString": "synthetic-valid-session"
                    }
                ]
            }]
        });

        let activity = parse_activity(&body, &requested).unwrap().unwrap();
        assert_eq!(activity.connection_string(), "synthetic-valid-session");
    }
}
