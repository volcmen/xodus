use std::collections::VecDeque;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;
use xodus::forza::MultiplayerInvite;

const DEFAULT_CAPACITY: usize = 32;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SafePendingInvite {
    pub id: Uuid,
    pub sender_gamertag: String,
    pub title_name: String,
    pub expires_at_unix: i64,
}

struct StoredInvite {
    id: Uuid,
    invite: MultiplayerInvite,
}

pub struct InviteStore {
    local_xuid: String,
    capacity: usize,
    entries: VecDeque<StoredInvite>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InsertOutcome {
    pub id: Uuid,
    pub is_new: bool,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum StoreError {
    #[error("invite targets another Xbox user")]
    WrongUser,
    #[error("invite has expired")]
    Expired,
    #[error("invite no longer exists")]
    NotFound,
}

impl InviteStore {
    pub fn new(local_xuid: impl Into<String>) -> Self {
        Self::with_capacity(local_xuid, DEFAULT_CAPACITY)
    }

    pub fn with_capacity(local_xuid: impl Into<String>, capacity: usize) -> Self {
        Self {
            local_xuid: local_xuid.into(),
            capacity: capacity.max(1),
            entries: VecDeque::new(),
        }
    }

    pub fn belongs_to(&self, local_xuid: &str) -> bool {
        self.local_xuid == local_xuid
    }

    #[cfg(test)]
    pub fn insert(
        &mut self,
        invite: MultiplayerInvite,
        now: DateTime<Utc>,
    ) -> Result<Uuid, StoreError> {
        Ok(self.insert_with_status(invite, now)?.id)
    }

    pub fn insert_with_status(
        &mut self,
        invite: MultiplayerInvite,
        now: DateTime<Utc>,
    ) -> Result<InsertOutcome, StoreError> {
        self.prune(now);
        if invite.invited_xuid != self.local_xuid {
            return Err(StoreError::WrongUser);
        }
        if invite.expires_at <= now {
            return Err(StoreError::Expired);
        }
        if let Some(existing) = self.entries.iter().find(|entry| {
            entry.invite.sender_xuid == invite.sender_xuid
                && entry.invite.connection_string == invite.connection_string
                && entry.invite.expires_at == invite.expires_at
        }) {
            return Ok(InsertOutcome {
                id: existing.id,
                is_new: false,
            });
        }

        let id = Uuid::new_v4();
        self.entries.push_front(StoredInvite { id, invite });
        while self.entries.len() > self.capacity {
            self.entries.pop_back();
        }
        Ok(InsertOutcome { id, is_new: true })
    }

    pub fn safe_list(&mut self, now: DateTime<Utc>) -> Vec<SafePendingInvite> {
        self.prune(now);
        self.entries
            .iter()
            .map(|entry| SafePendingInvite {
                id: entry.id,
                sender_gamertag: entry.invite.sender_gamertag.clone(),
                title_name: entry.invite.title_name.clone(),
                expires_at_unix: entry.invite.expires_at.timestamp(),
            })
            .collect()
    }

    #[cfg(test)]
    pub fn take_joinable(
        &mut self,
        id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<MultiplayerInvite, StoreError> {
        self.prune(now);
        let index = self
            .entries
            .iter()
            .position(|entry| entry.id == id)
            .ok_or(StoreError::NotFound)?;
        Ok(self
            .entries
            .remove(index)
            .expect("known invite index")
            .invite)
    }

    pub fn joinable(
        &mut self,
        id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<MultiplayerInvite, StoreError> {
        self.prune(now);
        self.entries
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.invite.clone())
            .ok_or(StoreError::NotFound)
    }

    pub fn safe(&mut self, id: Uuid, now: DateTime<Utc>) -> Result<SafePendingInvite, StoreError> {
        self.prune(now);
        self.entries
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| SafePendingInvite {
                id: entry.id,
                sender_gamertag: entry.invite.sender_gamertag.clone(),
                title_name: entry.invite.title_name.clone(),
                expires_at_unix: entry.invite.expires_at.timestamp(),
            })
            .ok_or(StoreError::NotFound)
    }

    pub fn dismiss(&mut self, id: Uuid) -> Result<(), StoreError> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.id == id)
            .ok_or(StoreError::NotFound)?;
        self.entries.remove(index);
        Ok(())
    }

    fn prune(&mut self, now: DateTime<Utc>) {
        self.entries.retain(|entry| entry.invite.expires_at > now);
    }
}

#[cfg(test)]
mod tests {
    use super::{InviteStore, StoreError};
    use chrono::{Duration, TimeZone, Utc};
    use xodus::forza::MultiplayerInvite;

    fn now() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 30, 20, 0, 0).unwrap()
    }

    fn invite(sender: &str, connection: &str, expires_in_minutes: i64) -> MultiplayerInvite {
        MultiplayerInvite {
            invited_xuid: "111".into(),
            sender_xuid: sender.into(),
            sender_gamertag: format!("Driver {sender}"),
            title_name: "Forza Motorsport".into(),
            connection_string: connection.into(),
            expires_at: now() + Duration::minutes(expires_in_minutes),
        }
    }

    #[test]
    fn inserts_opaque_safe_view_without_connection_material() {
        let mut store = InviteStore::new("111");
        let id = store
            .insert(invite("222", "secret/session", 15), now())
            .unwrap();

        assert!(!id.is_nil());
        let safe = store.safe_list(now());
        assert_eq!(safe.len(), 1);
        assert_eq!(safe[0].id, id);
        assert_eq!(safe[0].sender_gamertag, "Driver 222");
        let json = serde_json::to_string(&safe).unwrap();
        assert!(!json.contains("secret/session"));
        assert!(!json.contains("connectionString"));
    }

    #[test]
    fn deduplicates_same_sender_connection_and_expiry() {
        let mut store = InviteStore::new("111");
        let first = store.insert(invite("222", "same", 15), now()).unwrap();
        let second = store.insert(invite("222", "same", 15), now()).unwrap();

        assert_eq!(first, second);
        assert_eq!(store.safe_list(now()).len(), 1);
    }

    #[test]
    fn bounds_entries_and_lists_newest_first() {
        let mut store = InviteStore::with_capacity("111", 2);
        store.insert(invite("222", "one", 10), now()).unwrap();
        store.insert(invite("333", "two", 11), now()).unwrap();
        store.insert(invite("444", "three", 12), now()).unwrap();

        let safe = store.safe_list(now());
        assert_eq!(safe.len(), 2);
        assert_eq!(safe[0].sender_gamertag, "Driver 444");
        assert_eq!(safe[1].sender_gamertag, "Driver 333");
    }

    #[test]
    fn prunes_expired_and_consumes_valid_invite_once() {
        let mut store = InviteStore::new("111");
        let expired = store.insert(invite("222", "old", -1), now()).unwrap_err();
        assert_eq!(expired, StoreError::Expired);

        let id = store.insert(invite("333", "live", 10), now()).unwrap();
        assert_eq!(
            store.take_joinable(id, now()).unwrap().connection_string,
            "live"
        );
        assert_eq!(
            store.take_joinable(id, now()).unwrap_err(),
            StoreError::NotFound
        );
    }

    #[test]
    fn valid_invite_can_be_checked_without_consuming_it() {
        let mut store = InviteStore::new("111");
        let id = store.insert(invite("333", "live", 10), now()).unwrap();

        assert_eq!(store.joinable(id, now()).unwrap().connection_string, "live");
        assert_eq!(store.joinable(id, now()).unwrap().connection_string, "live");
        store.dismiss(id).unwrap();
        assert_eq!(store.joinable(id, now()).unwrap_err(), StoreError::NotFound);
    }

    #[test]
    fn rejects_invite_for_another_signed_in_user() {
        let mut store = InviteStore::new("111");
        let mut wrong = invite("222", "session", 10);
        wrong.invited_xuid = "999".into();

        assert_eq!(
            store.insert(wrong, now()).unwrap_err(),
            StoreError::WrongUser
        );
    }
}
