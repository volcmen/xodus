use crate::action::{GamepadButton, GamepadInput, InputMapper, UiAction};
use sdl3::{
    event::Event,
    gamepad::{Axis, Button, Gamepad},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use tokio::sync::mpsc::UnboundedSender;

pub fn map_button(button: Button) -> GamepadButton {
    match button {
        Button::DPadUp => GamepadButton::DpadUp,
        Button::DPadDown => GamepadButton::DpadDown,
        Button::South => GamepadButton::South,
        Button::East => GamepadButton::East,
        Button::West => GamepadButton::West,
        Button::North => GamepadButton::North,
        _ => GamepadButton::Other,
    }
}

pub struct ControllerListener {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl ControllerListener {
    pub fn start(sender: UnboundedSender<UiAction>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker = thread::Builder::new()
            .name("xodus-overlay-sdl".into())
            .spawn(move || run_event_loop(sender, worker_stop))
            .ok();
        Self { stop, worker }
    }
}

impl Drop for ControllerListener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run_event_loop(sender: UnboundedSender<UiAction>, stop: Arc<AtomicBool>) {
    let Ok(sdl) = sdl3::init() else { return };
    let Ok(subsystem) = sdl.gamepad() else { return };
    let Ok(mut events) = sdl.event_pump() else {
        return;
    };
    let mut open_gamepads: Vec<Gamepad> = subsystem
        .gamepads()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|id| subsystem.open(id).ok())
        .collect();
    let mut mapper = InputMapper::default();

    while !stop.load(Ordering::Acquire) {
        let Some(event) = events.wait_event_timeout(Duration::from_millis(50)) else {
            continue;
        };
        let mapped = match event {
            Event::ControllerButtonDown { button, .. } => {
                mapper.gamepad(GamepadInput::ButtonPressed(map_button(button)))
            }
            Event::ControllerAxisMotion {
                axis: Axis::LeftY,
                value,
                ..
            } => mapper.gamepad(GamepadInput::LeftY(value)),
            Event::ControllerDeviceAdded { which, .. } => {
                let id = sdl3::sys::joystick::SDL_JoystickID(which);
                if let Ok(gamepad) = subsystem.open(id) {
                    open_gamepads.push(gamepad);
                }
                None
            }
            Event::ControllerDeviceRemoved { which, .. } => {
                let id = sdl3::sys::joystick::SDL_JoystickID(which);
                open_gamepads.retain(|gamepad| gamepad.id().ok() != Some(id));
                mapper.gamepad(GamepadInput::DeviceRemoved)
            }
            _ => None,
        };
        if let Some(action) = mapped
            && sender.send(action).is_err()
        {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::map_button;
    use crate::action::GamepadButton;
    use sdl3::gamepad::Button;

    #[test]
    fn maps_xbox_face_buttons_and_vertical_dpad() {
        assert_eq!(map_button(Button::DPadUp), GamepadButton::DpadUp);
        assert_eq!(map_button(Button::DPadDown), GamepadButton::DpadDown);
        assert_eq!(map_button(Button::South), GamepadButton::South);
        assert_eq!(map_button(Button::East), GamepadButton::East);
        assert_eq!(map_button(Button::West), GamepadButton::West);
        assert_eq!(map_button(Button::North), GamepadButton::North);
        assert_eq!(map_button(Button::Start), GamepadButton::Other);
    }
}
