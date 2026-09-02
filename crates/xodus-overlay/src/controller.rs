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

#[derive(Default)]
pub struct ControllerInputState {
    devices: ControllerState,
    mapper: InputMapper,
}

impl ControllerInputState {
    pub fn apply_device(&mut self, event: ControllerDeviceEvent) {
        let active = self.devices.active();
        self.devices.apply(event);
        if self.devices.active() != active {
            self.mapper = InputMapper::default();
        }
    }

    #[must_use]
    pub fn active(&self) -> Option<u32> {
        self.devices.active()
    }

    pub fn gamepad(&mut self, device: u32, input: GamepadInput) -> Option<UiAction> {
        (self.active() == Some(device))
            .then(|| self.mapper.gamepad(input))
            .flatten()
    }
}

pub struct ControllerListener {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ControllerListener {
    #[must_use]
    pub fn start(actions: mpsc::Sender<UiAction>) -> Self {
        Self::spawn(move |stop| run_listener(actions, &stop))
    }

    fn spawn(run: impl FnOnce(Arc<AtomicBool>) + Send + 'static) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = thread::spawn(move || run(thread_stop));
        Self {
            stop,
            thread: Some(thread),
        }
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for ControllerListener {
    fn drop(&mut self) {
        self.stop();
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

    let mut input = ControllerInputState::default();
    let mut opened = HashMap::<u32, Gamepad>::new();
    for id in gamepads.gamepads().unwrap_or_default() {
        if let Ok(gamepad) = gamepads.open(id) {
            let raw = id.0;
            opened.insert(raw, gamepad);
            input.apply_device(ControllerDeviceEvent::Added(raw));
        }
    }

    while !stop.load(Ordering::Acquire) {
        for event in events.poll_iter() {
            match event {
                Event::ControllerDeviceAdded { which, .. } => {
                    let id = sdl3::sys::joystick::SDL_JoystickID(which);
                    if let Ok(gamepad) = gamepads.open(id) {
                        opened.insert(which, gamepad);
                        input.apply_device(ControllerDeviceEvent::Added(which));
                    }
                }
                Event::ControllerDeviceRemoved { which, .. } => {
                    opened.remove(&which);
                    input.apply_device(ControllerDeviceEvent::Removed(which));
                }
                Event::ControllerButtonDown { which, button, .. } => {
                    if let Some(action) =
                        input.gamepad(which, GamepadInput::ButtonPressed(map_sdl_button(button)))
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
                } => {
                    if let Some(action) = input.gamepad(which, GamepadInput::LeftY(value))
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;

    use super::ControllerListener;

    #[test]
    fn controller_listener_stop_is_idempotent_and_joins_its_thread() {
        let exited = Arc::new(AtomicBool::new(false));
        let thread_exited = Arc::clone(&exited);
        let mut listener = ControllerListener::spawn(move |stop| {
            while !stop.load(Ordering::Acquire) {
                thread::yield_now();
            }
            thread_exited.store(true, Ordering::Release);
        });

        listener.stop();
        listener.stop();
        assert!(exited.load(Ordering::Acquire));
    }
}
