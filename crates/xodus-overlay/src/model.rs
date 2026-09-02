use xodus::forza::{SocialFriend, SocialPresence, SocialRequest, SocialResponse};

use crate::action::UiAction;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewKind {
    Loading,
    Empty,
    Friends,
    Error,
    Success,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiOutcome {
    Pending,
    Completed,
    Cancelled,
    Failed,
}

impl UiOutcome {
    #[must_use]
    pub fn exit_code(self) -> i32 {
        match self {
            Self::Completed => 0,
            Self::Cancelled => 1,
            Self::Pending | Self::Failed => 2,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderedFriend {
    pub display_label: String,
    pub presence: SocialPresence,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderedView {
    pub kind: ViewKind,
    pub friends: Vec<RenderedFriend>,
    pub selected: Option<usize>,
    pub status: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelEffect {
    None,
    Request(SocialRequest),
    Close(UiOutcome),
}

pub struct OverlayModel {
    friends: Vec<SocialFriend>,
    selected: Option<usize>,
    kind: ViewKind,
    status: String,
    outcome: UiOutcome,
}

impl Default for OverlayModel {
    fn default() -> Self {
        Self::new()
    }
}

impl OverlayModel {
    #[must_use]
    pub fn new() -> Self {
        Self {
            friends: Vec::new(),
            selected: None,
            kind: ViewKind::Loading,
            status: "Loading friends".to_owned(),
            outcome: UiOutcome::Pending,
        }
    }

    #[must_use]
    pub fn initial_request() -> SocialRequest {
        SocialRequest::RefreshFriends
    }

    #[must_use]
    pub fn outcome(&self) -> UiOutcome {
        self.outcome
    }

    #[must_use]
    pub fn view(&self) -> RenderedView {
        RenderedView {
            kind: self.kind,
            friends: self
                .friends
                .iter()
                .map(|friend| RenderedFriend {
                    display_label: friend.display_label.clone(),
                    presence: friend.presence.clone(),
                })
                .collect(),
            selected: self.selected,
            status: self.status.clone(),
        }
    }

    pub fn select(&mut self, index: Option<usize>) {
        self.selected = index.filter(|index| *index < self.friends.len());
    }

    pub fn apply_response(&mut self, response: SocialResponse) -> ModelEffect {
        match response {
            SocialResponse::Snapshot(snapshot) => {
                self.friends = snapshot.friends;
                if self.friends.is_empty() {
                    self.selected = None;
                    self.kind = ViewKind::Empty;
                    self.status = "No friends available".to_owned();
                } else {
                    self.selected = Some(
                        self.selected
                            .unwrap_or(0)
                            .min(self.friends.len().saturating_sub(1)),
                    );
                    self.kind = ViewKind::Friends;
                    self.status = format!("{} friends available", self.friends.len());
                }
                ModelEffect::None
            }
            SocialResponse::Success { .. } => {
                self.kind = ViewKind::Success;
                self.status = "Action completed".to_owned();
                self.outcome = UiOutcome::Completed;
                ModelEffect::Close(UiOutcome::Completed)
            }
            SocialResponse::Error { .. } => {
                self.kind = ViewKind::Error;
                self.status = "Request failed".to_owned();
                ModelEffect::None
            }
        }
    }

    pub fn service_failed(&mut self) {
        self.kind = ViewKind::Error;
        self.status = "Xodus social service is unavailable".to_owned();
        self.outcome = UiOutcome::Failed;
    }

    pub fn action(&mut self, action: UiAction) -> ModelEffect {
        match action {
            UiAction::MoveUp => {
                self.move_selection(-1);
                ModelEffect::None
            }
            UiAction::MoveDown => {
                self.move_selection(1);
                ModelEffect::None
            }
            UiAction::Back => {
                if self.outcome == UiOutcome::Failed {
                    return ModelEffect::Close(UiOutcome::Failed);
                }
                self.kind = ViewKind::Cancelled;
                self.status = "Cancelled".to_owned();
                self.outcome = UiOutcome::Cancelled;
                ModelEffect::Close(UiOutcome::Cancelled)
            }
            UiAction::Refresh => {
                self.kind = ViewKind::Loading;
                self.status = "Refreshing friends".to_owned();
                ModelEffect::Request(SocialRequest::RefreshFriends)
            }
            UiAction::Invite => self.selected_request(false),
            UiAction::Join => self.selected_request(true),
            UiAction::Activate => {
                let Some(friend) = self.selected_friend() else {
                    return self.no_selection();
                };
                self.selected_request(matches!(friend.presence, SocialPresence::Online))
            }
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let Some(selected) = self.selected else {
            return;
        };
        let maximum = self.friends.len().saturating_sub(1);
        self.selected = Some(if delta < 0 {
            selected.saturating_sub(1)
        } else {
            selected.saturating_add(1).min(maximum)
        });
    }

    fn selected_friend(&self) -> Option<&SocialFriend> {
        self.selected.and_then(|index| self.friends.get(index))
    }

    fn selected_request(&mut self, join: bool) -> ModelEffect {
        let Some(friend) = self.selected_friend() else {
            return self.no_selection();
        };
        let request = if join {
            SocialRequest::JoinFriend {
                friend: friend.handle,
            }
        } else {
            SocialRequest::InviteFriend {
                friend: friend.handle,
            }
        };
        ModelEffect::Request(request)
    }

    fn no_selection(&mut self) -> ModelEffect {
        self.kind = ViewKind::Error;
        self.status = "Select a friend first".to_owned();
        ModelEffect::None
    }
}
