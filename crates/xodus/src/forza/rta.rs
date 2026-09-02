use serde_json::{Value, json};
use thiserror::Error;

pub const RTA_URI: &str = "wss://rta.xboxlive.com/connect";
// xal-rs discovers signing policy over HTTP(S). Xbox signatures cover the
// method, path/query, authorization and body, so the equivalent HTTPS URI
// produces the signature used by the WSS upgrade.
pub const RTA_SIGNING_URI: &str = "https://rta.xboxlive.com/connect";
pub const RTA_SUBPROTOCOL: &str = "rta.xboxlive.com.V2";

const SUBSCRIBE: u64 = 1;
const EVENT: u64 = 3;
const RESYNC: u64 = 4;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RtaEvent {
    Subscribed { service_id: u32, initial: Value },
    Payload(Value),
    Resync,
    Ignored,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum RtaProtocolError {
    #[error("RTA message is not valid JSON")]
    InvalidJson,
    #[error("RTA message is not an array")]
    NotArray,
    #[error("RTA message is missing required fields")]
    MissingFields,
    #[error("RTA subscription failed with code {0}")]
    Subscription(u64),
}

pub struct RtaMachine {
    client_id: u32,
    resource: String,
    service_id: Option<u32>,
}

impl RtaMachine {
    pub fn new(client_id: u32, resource: impl Into<String>) -> Self {
        Self {
            client_id,
            resource: resource.into(),
            service_id: None,
        }
    }

    pub fn subscribe_frame(&self) -> String {
        json!([SUBSCRIBE, self.client_id, self.resource]).to_string()
    }

    pub fn handle_text(&mut self, text: &str) -> Result<RtaEvent, RtaProtocolError> {
        let message: Value =
            serde_json::from_str(text).map_err(|_| RtaProtocolError::InvalidJson)?;
        let fields = message.as_array().ok_or(RtaProtocolError::NotArray)?;
        let message_type = fields
            .first()
            .and_then(Value::as_u64)
            .ok_or(RtaProtocolError::MissingFields)?;

        match message_type {
            SUBSCRIBE => self.handle_subscribe(fields),
            EVENT => self.handle_event(fields),
            RESYNC => Ok(RtaEvent::Resync),
            _ => Ok(RtaEvent::Ignored),
        }
    }

    fn handle_subscribe(&mut self, fields: &[Value]) -> Result<RtaEvent, RtaProtocolError> {
        let client_id = fields
            .get(1)
            .and_then(Value::as_u64)
            .ok_or(RtaProtocolError::MissingFields)?;
        if client_id != u64::from(self.client_id) {
            return Ok(RtaEvent::Ignored);
        }
        let code = fields
            .get(2)
            .and_then(Value::as_u64)
            .ok_or(RtaProtocolError::MissingFields)?;
        if code != 0 {
            return Err(RtaProtocolError::Subscription(code));
        }
        let service_id = fields
            .get(3)
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or(RtaProtocolError::MissingFields)?;
        let initial = fields
            .get(4)
            .cloned()
            .ok_or(RtaProtocolError::MissingFields)?;
        self.service_id = Some(service_id);
        Ok(RtaEvent::Subscribed {
            service_id,
            initial,
        })
    }

    fn handle_event(&self, fields: &[Value]) -> Result<RtaEvent, RtaProtocolError> {
        let service_id = fields
            .get(1)
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or(RtaProtocolError::MissingFields)?;
        let payload = fields
            .get(2)
            .cloned()
            .ok_or(RtaProtocolError::MissingFields)?;
        if self.service_id == Some(service_id) {
            Ok(RtaEvent::Payload(payload))
        } else {
            Ok(RtaEvent::Ignored)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{RTA_SIGNING_URI, RTA_URI, RtaEvent, RtaMachine};
    use serde_json::json;

    const RESOURCE: &str =
        "https://notify.xboxlive.com/users/xuid(111)/deviceId/current/titleId/1842668909";

    #[test]
    fn signs_the_websocket_path_through_the_https_policy_endpoint() {
        assert_eq!(RTA_URI, "wss://rta.xboxlive.com/connect");
        assert_eq!(RTA_SIGNING_URI, "https://rta.xboxlive.com/connect");
    }

    #[test]
    fn builds_microsoft_v2_subscribe_frame() {
        let machine = RtaMachine::new(7, RESOURCE);
        assert_eq!(machine.subscribe_frame(), format!(r#"[1,7,"{RESOURCE}"]"#));
    }

    #[test]
    fn accepts_subscription_and_routes_matching_events() {
        let mut machine = RtaMachine::new(7, RESOURCE);

        assert_eq!(
            machine.handle_text(r#"[1,7,0,42,{"ready":true}]"#).unwrap(),
            RtaEvent::Subscribed {
                service_id: 42,
                initial: json!({ "ready": true }),
            }
        );
        assert_eq!(
            machine
                .handle_text(r#"[3,42,{"notificationData":{"titleName":"Forza Motorsport"}}]"#)
                .unwrap(),
            RtaEvent::Payload(json!({
                "notificationData": { "titleName": "Forza Motorsport" }
            }))
        );
    }

    #[test]
    fn ignores_event_for_unknown_subscription() {
        let mut machine = RtaMachine::new(7, RESOURCE);
        machine.handle_text(r#"[1,7,0,42,{"ready":true}]"#).unwrap();

        assert_eq!(
            machine
                .handle_text(r#"[3,99,{"secret":"ignored"}]"#)
                .unwrap(),
            RtaEvent::Ignored
        );
    }

    #[test]
    fn reports_resync_and_ignores_future_message_types() {
        let mut machine = RtaMachine::new(7, RESOURCE);
        assert_eq!(machine.handle_text("[4]").unwrap(), RtaEvent::Resync);
        assert_eq!(
            machine.handle_text(r#"[99,{"future":true}]"#).unwrap(),
            RtaEvent::Ignored
        );
    }

    #[test]
    fn rejects_failed_or_malformed_subscribe_frames() {
        let mut machine = RtaMachine::new(7, RESOURCE);
        assert!(machine.handle_text(r#"[1,7,3,0,null]"#).is_err());
        assert!(machine.handle_text("not-json").is_err());
        assert!(machine.handle_text("{}").is_err());
    }
}
