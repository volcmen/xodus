use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use sdl3::event::Event;
use sdl3::gamepad::{Axis, Button, Gamepad};

use crate::action::{GamepadButton, GamepadInput, InputMapper, UiAction};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControllerDeviceEvent {
    Added(u32),
    Removed(u32),
}

#[derive(Default)]
pub struct ControllerState {
    devices: HashSet<u32>,
    active: Option<u32>,
}

impl ControllerState {
    pub fn apply(&mut self, event: ControllerDeviceEvent) {
        match event {
            ControllerDeviceEvent::Added(id) => {
                self.devices.insert(id);
                if self.active.is_none() {
                    self.active = Some(id);
                }
            }
            ControllerDeviceEvent::Removed(id) => {
                self.devices.remove(&id);
                if self.active == Some(id) {
                    self.active = self.devices.iter().copied().min();
                }
            }
        }
    }

    #[must_use]
    pub fn active(&self) -> Option<u32> {
        self.active
    }

    #[must_use]
    pub fn devices(&self) -> HashSet<u32> {
        self.devices.clone()
    }
}

pub struct ControllerListener {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ControllerListener {
    #[must_use]
    pub fn start(actions: mpsc::Sender<UiAction>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = thread::spawn(move || run_listener(actions, &thread_stop));
        Self {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for ControllerListener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_listener(actions: mpsc::Sender<UiAction>, stop: &AtomicBool) {
    let Ok(sdl) = sdl3::init() else {
        return;
    };
    let Ok(gamepads) = sdl.gamepad() else {
        return;
    };
    let Ok(mut events) = sdl.event_pump() else {
        return;
    };

    let mut devices = ControllerState::default();
    let mut opened = HashMap::<u32, Gamepad>::new();
    for id in gamepads.gamepads().unwrap_or_default() {
        if let Ok(gamepad) = gamepads.open(id) {
            let raw = id.0;
            opened.insert(raw, gamepad);
            devices.apply(ControllerDeviceEvent::Added(raw));
        }
    }
    let mut mapper = InputMapper::default();

    while !stop.load(Ordering::Acquire) {
        for event in events.poll_iter() {
            match event {
                Event::ControllerDeviceAdded { which, .. } => {
                    let id = sdl3::sys::joystick::SDL_JoystickID(which);
                    if let Ok(gamepad) = gamepads.open(id) {
                        opened.insert(which, gamepad);
                        devices.apply(ControllerDeviceEvent::Added(which));
                    }
                }
                Event::ControllerDeviceRemoved { which, .. } => {
                    opened.remove(&which);
                    devices.apply(ControllerDeviceEvent::Removed(which));
                }
                Event::ControllerButtonDown { which, button, .. }
                    if devices.active() == Some(which) =>
                {
                    if let Some(action) =
                        mapper.gamepad(GamepadInput::ButtonPressed(map_sdl_button(button)))
                        && actions.send(action).is_err()
                    {
                        return;
                    }
                }
                Event::ControllerAxisMotion {
                    which,
                    axis: Axis::LeftY,
                    value,
                    ..
                } if devices.active() == Some(which) => {
                    if let Some(action) = mapper.gamepad(GamepadInput::LeftY(value))
                        && actions.send(action).is_err()
                    {
                        return;
                    }
                }
                _ => {}
            }
        }
        thread::sleep(Duration::from_millis(16));
    }
}

fn map_sdl_button(button: Button) -> GamepadButton {
    match button {
        Button::DPadUp => GamepadButton::DPadUp,
        Button::DPadDown => GamepadButton::DPadDown,
        Button::South => GamepadButton::South,
        Button::East => GamepadButton::East,
        Button::West => GamepadButton::West,
        Button::North => GamepadButton::North,
        Button::Start => GamepadButton::Start,
        _ => GamepadButton::Other,
    }
}
