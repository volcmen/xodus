#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiAction {
    MoveUp,
    MoveDown,
    Activate,
    Back,
    Invite,
    Join,
    Refresh,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyInput {
    Up,
    W,
    Down,
    S,
    Enter,
    Space,
    Escape,
    Backspace,
    I,
    J,
    R,
    F5,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GamepadButton {
    DPadUp,
    DPadDown,
    South,
    East,
    West,
    North,
    Start,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GamepadInput {
    ButtonPressed(GamepadButton),
    LeftY(i16),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StickDirection {
    Up,
    Down,
}

#[derive(Default)]
pub struct InputMapper {
    stick_direction: Option<StickDirection>,
}

impl InputMapper {
    const STICK_ENGAGE: i16 = 16_000;
    const STICK_NEUTRAL: i16 = 8_000;

    #[must_use]
    pub fn key(input: KeyInput) -> Option<UiAction> {
        match input {
            KeyInput::Up | KeyInput::W => Some(UiAction::MoveUp),
            KeyInput::Down | KeyInput::S => Some(UiAction::MoveDown),
            KeyInput::Enter | KeyInput::Space => Some(UiAction::Activate),
            KeyInput::Escape | KeyInput::Backspace => Some(UiAction::Back),
            KeyInput::I => Some(UiAction::Invite),
            KeyInput::J => Some(UiAction::Join),
            KeyInput::R | KeyInput::F5 => Some(UiAction::Refresh),
            KeyInput::Other => None,
        }
    }

    pub fn gamepad(&mut self, input: GamepadInput) -> Option<UiAction> {
        match input {
            GamepadInput::ButtonPressed(button) => match button {
                GamepadButton::DPadUp => Some(UiAction::MoveUp),
                GamepadButton::DPadDown => Some(UiAction::MoveDown),
                GamepadButton::South => Some(UiAction::Activate),
                GamepadButton::East => Some(UiAction::Back),
                GamepadButton::West => Some(UiAction::Invite),
                GamepadButton::North => Some(UiAction::Join),
                GamepadButton::Start => Some(UiAction::Refresh),
                GamepadButton::Other => None,
            },
            GamepadInput::LeftY(value) => self.left_stick(value),
        }
    }

    fn left_stick(&mut self, value: i16) -> Option<UiAction> {
        if value.unsigned_abs() <= Self::STICK_NEUTRAL as u16 {
            self.stick_direction = None;
            return None;
        }

        let next = if value <= -Self::STICK_ENGAGE {
            Some((StickDirection::Up, UiAction::MoveUp))
        } else if value >= Self::STICK_ENGAGE {
            Some((StickDirection::Down, UiAction::MoveDown))
        } else {
            None
        };
        let (direction, action) = next?;
        if self.stick_direction == Some(direction) {
            None
        } else {
            self.stick_direction = Some(direction);
            Some(action)
        }
    }
}
