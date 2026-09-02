use xodus::forza::{FriendAction, SocialRequest, SocialSnapshot};

pub struct OverlayModel {
    snapshot: SocialSnapshot,
    selected: usize,
}

impl OverlayModel {
    pub fn new(snapshot: SocialSnapshot) -> Self {
        Self {
            snapshot,
            selected: 0,
        }
    }

    pub fn snapshot(&self) -> &SocialSnapshot {
        &self.snapshot
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn select(&mut self, index: usize) -> bool {
        if index >= self.row_count() {
            return false;
        }
        self.selected = index;
        true
    }

    pub fn replace(&mut self, snapshot: SocialSnapshot) {
        self.snapshot = snapshot;
        self.selected = self.selected.min(self.row_count().saturating_sub(1));
    }

    pub fn move_up(&mut self) {
        let count = self.row_count();
        if count > 0 {
            self.selected = if self.selected == 0 {
                count - 1
            } else {
                self.selected - 1
            };
        }
    }

    pub fn move_down(&mut self) {
        let count = self.row_count();
        if count > 0 {
            self.selected = (self.selected + 1) % count;
        }
    }

    pub fn primary_request(&self) -> Option<SocialRequest> {
        if let Some(invite) = self.snapshot.invitations.get(self.selected) {
            return Some(SocialRequest::JoinInvite {
                invite_id: invite.id.clone(),
            });
        }
        let friend_index = self.selected.checked_sub(self.snapshot.invitations.len())?;
        let friend = self.snapshot.friends.get(friend_index)?;
        match friend.action {
            FriendAction::Join => Some(SocialRequest::JoinFriend {
                friend_id: friend.id.clone(),
            }),
            FriendAction::Invite => Some(SocialRequest::InviteFriend {
                friend_id: friend.id.clone(),
            }),
            FriendAction::Unavailable => None,
        }
    }

    pub fn invite_request(&self) -> Option<SocialRequest> {
        let friend_index = self.selected.checked_sub(self.snapshot.invitations.len())?;
        let friend = self.snapshot.friends.get(friend_index)?;
        Some(SocialRequest::InviteFriend {
            friend_id: friend.id.clone(),
        })
    }

    pub fn row_count(&self) -> usize {
        self.snapshot.invitations.len() + self.snapshot.friends.len()
    }
}

#[cfg(test)]
mod tests {
    use super::OverlayModel;
    use xodus::forza::{
        FriendAction, FriendView, InviteView, SocialRequest, SocialSnapshot, SocialStatus,
    };

    fn snapshot(invites: usize, friends: usize) -> SocialSnapshot {
        SocialSnapshot {
            status: SocialStatus::Connected,
            invitations: (0..invites)
                .map(|index| InviteView {
                    id: format!("invite-{index}"),
                    sender_gamertag: format!("Inviter {index}"),
                    title_name: "Forza Motorsport".into(),
                    expires_at_unix: 2_000_000_000,
                })
                .collect(),
            friends: (0..friends)
                .map(|index| FriendView {
                    id: format!("friend-{index}"),
                    gamertag: format!("Friend {index}"),
                    presence: "Online".into(),
                    action: if index == 0 {
                        FriendAction::Join
                    } else {
                        FriendAction::Invite
                    },
                })
                .collect(),
        }
    }

    #[test]
    fn selection_wraps_across_invites_and_friends() {
        let mut model = OverlayModel::new(snapshot(1, 2));
        assert_eq!(model.selected(), 0);
        model.move_up();
        assert_eq!(model.selected(), 2);
        model.move_down();
        assert_eq!(model.selected(), 0);
        model.move_down();
        assert_eq!(model.selected(), 1);
    }

    #[test]
    fn refresh_clamps_selection_when_rows_disappear() {
        let mut model = OverlayModel::new(snapshot(1, 3));
        model.move_up();
        assert_eq!(model.selected(), 3);
        model.replace(snapshot(0, 1));
        assert_eq!(model.selected(), 0);
    }

    #[test]
    fn direct_selection_accepts_only_visible_rows() {
        let mut model = OverlayModel::new(snapshot(1, 2));
        assert!(model.select(2));
        assert_eq!(model.selected(), 2);
        assert!(!model.select(3));
        assert_eq!(model.selected(), 2);
    }

    #[test]
    fn primary_action_uses_invite_or_contextual_friend_action() {
        let mut model = OverlayModel::new(snapshot(1, 2));
        assert_eq!(
            model.primary_request(),
            Some(SocialRequest::JoinInvite {
                invite_id: "invite-0".into()
            })
        );
        model.move_down();
        assert_eq!(
            model.primary_request(),
            Some(SocialRequest::JoinFriend {
                friend_id: "friend-0".into()
            })
        );
        model.move_down();
        assert_eq!(
            model.primary_request(),
            Some(SocialRequest::InviteFriend {
                friend_id: "friend-1".into()
            })
        );
    }

    #[test]
    fn explicit_invite_only_targets_a_friend_row() {
        let mut model = OverlayModel::new(snapshot(1, 1));
        assert_eq!(model.invite_request(), None);
        model.move_down();
        assert_eq!(
            model.invite_request(),
            Some(SocialRequest::InviteFriend {
                friend_id: "friend-0".into()
            })
        );
    }
}
