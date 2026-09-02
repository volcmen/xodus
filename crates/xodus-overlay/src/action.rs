const STICK_DEAD_ZONE: i16 = 16_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiAction {
    MoveUp,
    MoveDown,
    Activate,
    Back,
    Invite,
    Refresh,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyInput {
    Up,
    Down,
    W,
    S,
    Enter,
    Space,
    Escape,
    Backspace,
    I,
    X,
    R,
    F5,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GamepadButton {
    DpadUp,
    DpadDown,
    South,
    East,
    West,
    North,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GamepadInput {
    ButtonPressed(GamepadButton),
    LeftY(i16),
    DeviceRemoved,
}

#[derive(Default)]
pub struct InputMapper {
    stick_direction: i8,
}

impl InputMapper {
    pub fn key(input: KeyInput) -> Option<UiAction> {
        match input {
            KeyInput::Up | KeyInput::W => Some(UiAction::MoveUp),
            KeyInput::Down | KeyInput::S => Some(UiAction::MoveDown),
            KeyInput::Enter | KeyInput::Space => Some(UiAction::Activate),
            KeyInput::Escape | KeyInput::Backspace => Some(UiAction::Back),
            KeyInput::I | KeyInput::X => Some(UiAction::Invite),
            KeyInput::R | KeyInput::F5 => Some(UiAction::Refresh),
            KeyInput::Other => None,
        }
    }

    pub fn gamepad(&mut self, input: GamepadInput) -> Option<UiAction> {
        match input {
            GamepadInput::ButtonPressed(button) => match button {
                GamepadButton::DpadUp => Some(UiAction::MoveUp),
                GamepadButton::DpadDown => Some(UiAction::MoveDown),
                GamepadButton::South => Some(UiAction::Activate),
                GamepadButton::East => Some(UiAction::Back),
                GamepadButton::West => Some(UiAction::Invite),
                GamepadButton::North => Some(UiAction::Refresh),
                GamepadButton::Other => None,
            },
            GamepadInput::LeftY(value) => {
                let direction = if value <= -STICK_DEAD_ZONE {
                    -1
                } else if value >= STICK_DEAD_ZONE {
                    1
                } else {
                    0
                };
                let changed = direction != self.stick_direction;
                self.stick_direction = direction;
                if !changed {
                    None
                } else {
                    match direction {
                        -1 => Some(UiAction::MoveUp),
                        1 => Some(UiAction::MoveDown),
                        _ => None,
                    }
                }
            }
            GamepadInput::DeviceRemoved => {
                self.stick_direction = 0;
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{GamepadButton, GamepadInput, InputMapper, KeyInput, UiAction};

    #[test]
    fn keyboard_bindings_cover_navigation_actions_and_fallbacks() {
        let cases = [
            (KeyInput::Up, Some(UiAction::MoveUp)),
            (KeyInput::W, Some(UiAction::MoveUp)),
            (KeyInput::Down, Some(UiAction::MoveDown)),
            (KeyInput::S, Some(UiAction::MoveDown)),
            (KeyInput::Enter, Some(UiAction::Activate)),
            (KeyInput::Space, Some(UiAction::Activate)),
            (KeyInput::Escape, Some(UiAction::Back)),
            (KeyInput::Backspace, Some(UiAction::Back)),
            (KeyInput::I, Some(UiAction::Invite)),
            (KeyInput::X, Some(UiAction::Invite)),
            (KeyInput::R, Some(UiAction::Refresh)),
            (KeyInput::F5, Some(UiAction::Refresh)),
            (KeyInput::Other, None),
        ];
        for (input, expected) in cases {
            assert_eq!(InputMapper::key(input), expected, "input {input:?}");
        }
    }

    #[test]
    fn controller_buttons_map_to_the_same_semantic_actions() {
        let mut mapper = InputMapper::default();
        let cases = [
            (GamepadButton::DpadUp, UiAction::MoveUp),
            (GamepadButton::DpadDown, UiAction::MoveDown),
            (GamepadButton::South, UiAction::Activate),
            (GamepadButton::East, UiAction::Back),
            (GamepadButton::West, UiAction::Invite),
            (GamepadButton::North, UiAction::Refresh),
        ];
        for (button, expected) in cases {
            assert_eq!(
                mapper.gamepad(GamepadInput::ButtonPressed(button)),
                Some(expected)
            );
        }
    }

    #[test]
    fn stick_uses_dead_zone_and_emits_only_on_direction_edges() {
        let mut mapper = InputMapper::default();
        assert_eq!(mapper.gamepad(GamepadInput::LeftY(8_000)), None);
        assert_eq!(
            mapper.gamepad(GamepadInput::LeftY(-20_000)),
            Some(UiAction::MoveUp)
        );
        assert_eq!(mapper.gamepad(GamepadInput::LeftY(-25_000)), None);
        assert_eq!(mapper.gamepad(GamepadInput::LeftY(0)), None);
        assert_eq!(
            mapper.gamepad(GamepadInput::LeftY(20_000)),
            Some(UiAction::MoveDown)
        );
        assert_eq!(mapper.gamepad(GamepadInput::DeviceRemoved), None);
        assert_eq!(
            mapper.gamepad(GamepadInput::LeftY(-20_000)),
            Some(UiAction::MoveUp)
        );
    }
}
