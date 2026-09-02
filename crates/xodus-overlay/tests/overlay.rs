use std::cell::RefCell;
use std::collections::HashSet;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use tokio::io::{AsyncWriteExt, split};
use xodus_overlay::action::{GamepadButton, GamepadInput, InputMapper, KeyInput, UiAction};
use xodus_overlay::client::{ClientError, ClientWorker, ClientWorkerRequestError, SocialClient};
use xodus_overlay::controller::{ControllerDeviceEvent, ControllerInputState, ControllerState};
use xodus_overlay::model::{
    ModelEffect, OverlayModel, UiOutcome, ViewKind, with_action_effect, with_detached_view,
    with_response_effect,
};
use xodus_social_protocol::{
    FriendHandle, SocialFriend, SocialPresence, SocialRequest, SocialResponse, SocialSnapshot,
    decode_frame, encode_frame, read_frame,
};

fn friend(label: &str, presence: SocialPresence) -> SocialFriend {
    SocialFriend {
        handle: FriendHandle::new(),
        display_label: label.to_owned(),
        presence,
    }
}

fn snapshot(friends: Vec<SocialFriend>) -> SocialResponse {
    SocialResponse::Snapshot(SocialSnapshot {
        friends,
        status: "synthetic service status".to_owned(),
    })
}

fn read_sync_request(stream: &mut StdUnixStream) -> SocialRequest {
    let mut header = [0_u8; 10];
    stream.read_exact(&mut header).unwrap();
    assert_eq!(&header[..4], b"XDSO");
    let length = u32::from_le_bytes(header[6..10].try_into().unwrap()) as usize;
    let mut frame = Vec::from(header);
    frame.resize(10 + length, 0);
    stream.read_exact(&mut frame[10..]).unwrap();
    decode_frame(&frame).unwrap()
}

#[test]
fn every_keyboard_and_xbox_button_maps_to_the_required_semantic_action() {
    let keyboard = [
        (KeyInput::Up, UiAction::MoveUp),
        (KeyInput::W, UiAction::MoveUp),
        (KeyInput::Down, UiAction::MoveDown),
        (KeyInput::S, UiAction::MoveDown),
        (KeyInput::Enter, UiAction::Activate),
        (KeyInput::Space, UiAction::Activate),
        (KeyInput::Escape, UiAction::Back),
        (KeyInput::Backspace, UiAction::Back),
        (KeyInput::I, UiAction::Invite),
        (KeyInput::J, UiAction::Join),
        (KeyInput::R, UiAction::Refresh),
        (KeyInput::F5, UiAction::Refresh),
    ];
    for (input, expected) in keyboard {
        assert_eq!(InputMapper::key(input), Some(expected));
    }
    assert_eq!(InputMapper::key(KeyInput::Other), None);

    let controller = [
        (GamepadButton::DPadUp, UiAction::MoveUp),
        (GamepadButton::DPadDown, UiAction::MoveDown),
        (GamepadButton::South, UiAction::Activate),
        (GamepadButton::East, UiAction::Back),
        (GamepadButton::West, UiAction::Invite),
        (GamepadButton::North, UiAction::Join),
        (GamepadButton::Start, UiAction::Refresh),
    ];
    let mut mapper = InputMapper::default();
    for (button, expected) in controller {
        assert_eq!(
            mapper.gamepad(GamepadInput::ButtonPressed(button)),
            Some(expected)
        );
    }
    assert_eq!(
        mapper.gamepad(GamepadInput::ButtonPressed(GamepadButton::Other)),
        None
    );

    assert_eq!(
        InputMapper::key(KeyInput::Enter),
        InputMapper::default().gamepad(GamepadInput::ButtonPressed(GamepadButton::South))
    );
    assert_eq!(
        InputMapper::key(KeyInput::Escape),
        InputMapper::default().gamepad(GamepadInput::ButtonPressed(GamepadButton::East))
    );
}

#[test]
fn stick_crossing_is_suppressed_until_neutral_and_direction_changes_emit_once() {
    let mut mapper = InputMapper::default();
    assert_eq!(
        mapper.gamepad(GamepadInput::LeftY(-20_000)),
        Some(UiAction::MoveUp)
    );
    assert_eq!(mapper.gamepad(GamepadInput::LeftY(-25_000)), None);
    assert_eq!(mapper.gamepad(GamepadInput::LeftY(-10_000)), None);
    assert_eq!(mapper.gamepad(GamepadInput::LeftY(0)), None);
    assert_eq!(
        mapper.gamepad(GamepadInput::LeftY(-20_000)),
        Some(UiAction::MoveUp)
    );
    assert_eq!(
        mapper.gamepad(GamepadInput::LeftY(20_000)),
        Some(UiAction::MoveDown)
    );
    assert_eq!(mapper.gamepad(GamepadInput::LeftY(25_000)), None);
}

#[test]
fn controller_state_survives_zero_hotplug_and_active_removal() {
    let mut state = ControllerState::default();
    assert_eq!(state.active(), None);

    state.apply(ControllerDeviceEvent::Added(7));
    state.apply(ControllerDeviceEvent::Added(3));
    assert_eq!(state.active(), Some(7));
    assert_eq!(state.devices(), HashSet::from([3, 7]));

    state.apply(ControllerDeviceEvent::Removed(7));
    assert_eq!(state.active(), Some(3));
    assert_eq!(state.devices(), HashSet::from([3]));

    state.apply(ControllerDeviceEvent::Removed(3));
    state.apply(ControllerDeviceEvent::Removed(999));
    assert_eq!(state.active(), None);
    assert!(state.devices().is_empty());
}

#[test]
fn active_controller_change_rearms_stick_hysteresis() {
    let mut input = ControllerInputState::default();
    input.apply_device(ControllerDeviceEvent::Added(7));
    input.apply_device(ControllerDeviceEvent::Added(3));
    assert_eq!(
        input.gamepad(7, GamepadInput::LeftY(-20_000)),
        Some(UiAction::MoveUp)
    );
    assert_eq!(input.gamepad(7, GamepadInput::LeftY(-25_000)), None);

    input.apply_device(ControllerDeviceEvent::Removed(7));
    assert_eq!(input.active(), Some(3));
    assert_eq!(
        input.gamepad(3, GamepadInput::LeftY(-20_000)),
        Some(UiAction::MoveUp)
    );
}

#[test]
fn model_covers_loading_empty_friends_error_success_and_cancellation() {
    let mut model = OverlayModel::new();
    assert_eq!(model.view().kind, ViewKind::Loading);
    assert!(model.view().busy);
    assert_eq!(
        OverlayModel::initial_request(),
        SocialRequest::RefreshFriends
    );

    model.apply_response(snapshot(Vec::new()));
    assert_eq!(model.view().kind, ViewKind::Empty);
    assert_eq!(model.view().selected, None);
    assert!(!model.view().busy);

    model.apply_response(snapshot(vec![friend("Driver One", SocialPresence::Online)]));
    assert_eq!(model.view().kind, ViewKind::Friends);
    assert_eq!(model.view().selected, Some(0));

    model.apply_response(SocialResponse::Error {
        message: "Selected friend is no longer available".to_owned(),
    });
    assert_eq!(model.view().kind, ViewKind::Error);
    assert_eq!(model.view().friends.len(), 1);

    assert_eq!(
        model.apply_response(SocialResponse::Success {
            message: "Invite sent".to_owned(),
        }),
        ModelEffect::Close(UiOutcome::Completed)
    );
    assert_eq!(model.view().kind, ViewKind::Success);
    assert_eq!(model.outcome(), UiOutcome::Completed);

    assert_eq!(
        model.action(UiAction::Back),
        ModelEffect::Close(UiOutcome::Cancelled)
    );
    assert_eq!(model.view().kind, ViewKind::Cancelled);
    assert_eq!(UiOutcome::Completed.exit_code(), 0);
    assert_eq!(UiOutcome::Cancelled.exit_code(), 1);
    assert_eq!(UiOutcome::Failed.exit_code(), 2);
}

#[test]
fn service_failure_remains_a_failure_when_the_error_view_is_closed() {
    let mut model = OverlayModel::new();
    model.service_failed();

    assert!(!model.view().busy);
    assert_eq!(
        model.action(UiAction::Back),
        ModelEffect::Close(UiOutcome::Failed)
    );
    assert_eq!(model.outcome(), UiOutcome::Failed);
}

#[test]
fn model_keeps_selection_safe_and_clamps_it_after_refresh() {
    let mut model = OverlayModel::new();
    assert_eq!(model.action(UiAction::Invite), ModelEffect::None);
    assert_eq!(model.action(UiAction::Join), ModelEffect::None);
    assert_eq!(model.action(UiAction::Activate), ModelEffect::None);

    model.apply_response(snapshot(vec![
        friend("One", SocialPresence::Online),
        friend("Two", SocialPresence::Offline),
        friend("Three", SocialPresence::Unknown),
    ]));
    model.action(UiAction::MoveDown);
    model.action(UiAction::MoveDown);
    assert_eq!(model.view().selected, Some(2));

    model.apply_response(snapshot(vec![friend("Only", SocialPresence::Online)]));
    assert_eq!(model.view().selected, Some(0));

    model.apply_response(snapshot(Vec::new()));
    assert_eq!(model.view().selected, None);
}

#[test]
fn detached_view_allows_synchronous_selection_callback_mutation() {
    let model = RefCell::new(OverlayModel::new());
    model
        .borrow_mut()
        .apply_response(snapshot(vec![friend("Driver One", SocialPresence::Online)]));

    with_detached_view(&model, |view| {
        assert_eq!(view.selected, Some(0));
        model.borrow_mut().select(None);
    });
    assert_eq!(model.borrow().view().selected, None);
}

#[test]
fn action_request_failure_callback_can_reborrow_the_model() {
    let model = RefCell::new(OverlayModel::new());
    model
        .borrow_mut()
        .apply_response(snapshot(vec![friend("Driver One", SocialPresence::Online)]));

    with_action_effect(&model, UiAction::Invite, |effect| {
        assert!(matches!(effect, ModelEffect::Request(_)));
        model.borrow_mut().service_failed();
    });
    assert_eq!(model.borrow().outcome(), UiOutcome::Failed);
}

#[test]
fn synchronous_action_close_callback_can_reborrow_the_model() {
    let model = RefCell::new(OverlayModel::new());

    with_action_effect(&model, UiAction::Back, |effect| {
        assert_eq!(effect, ModelEffect::Close(UiOutcome::Cancelled));
        assert_eq!(model.borrow().outcome(), UiOutcome::Cancelled);
    });
}

#[test]
fn synchronous_response_close_callback_can_reborrow_the_model() {
    let model = RefCell::new(OverlayModel::new());

    with_response_effect(
        &model,
        SocialResponse::Success {
            message: "Invite sent".to_owned(),
        },
        |effect| {
            assert_eq!(effect, ModelEffect::Close(UiOutcome::Completed));
            assert_eq!(model.borrow().outcome(), UiOutcome::Completed);
        },
    );
}

#[test]
fn semantic_invite_join_and_activate_requests_carry_only_selected_handles() {
    let online = friend("Online", SocialPresence::Online);
    let online_handle = online.handle;
    let offline = friend("Offline", SocialPresence::Offline);
    let offline_handle = offline.handle;
    let mut model = OverlayModel::new();
    model.apply_response(snapshot(vec![online, offline]));

    assert_eq!(
        model.action(UiAction::Invite),
        ModelEffect::Request(SocialRequest::InviteFriend {
            friend: online_handle
        })
    );
    model.apply_response(SocialResponse::Error {
        message: "synthetic invite failure".to_owned(),
    });
    assert_eq!(
        model.action(UiAction::Join),
        ModelEffect::Request(SocialRequest::JoinFriend {
            friend: online_handle
        })
    );
    model.apply_response(SocialResponse::Error {
        message: "synthetic join failure".to_owned(),
    });
    assert_eq!(
        model.action(UiAction::Activate),
        ModelEffect::Request(SocialRequest::JoinFriend {
            friend: online_handle
        })
    );
    model.apply_response(SocialResponse::Error {
        message: "synthetic activation failure".to_owned(),
    });

    model.action(UiAction::MoveDown);
    assert_eq!(
        model.action(UiAction::Activate),
        ModelEffect::Request(SocialRequest::InviteFriend {
            friend: offline_handle
        })
    );
}

#[test]
fn repeated_row_and_button_activation_emit_only_one_request() {
    let selected = friend("Online", SocialPresence::Online);
    let selected_handle = selected.handle;
    let mut model = OverlayModel::new();
    model.apply_response(snapshot(vec![selected]));

    assert_eq!(
        model.action(UiAction::Activate),
        ModelEffect::Request(SocialRequest::JoinFriend {
            friend: selected_handle
        })
    );
    assert!(model.view().busy);
    assert_eq!(model.action(UiAction::Activate), ModelEffect::None);
    assert_eq!(model.action(UiAction::Invite), ModelEffect::None);
    assert_eq!(model.action(UiAction::Join), ModelEffect::None);
    assert_eq!(model.action(UiAction::Refresh), ModelEffect::None);
}

#[test]
fn keyboard_and_controller_repeat_share_the_semantic_request_gate() {
    let selected = friend("Online", SocialPresence::Online);
    let selected_handle = selected.handle;
    let mut model = OverlayModel::new();
    model.apply_response(snapshot(vec![selected]));

    let keyboard_action = InputMapper::key(KeyInput::I).unwrap();
    assert_eq!(
        model.action(keyboard_action),
        ModelEffect::Request(SocialRequest::InviteFriend {
            friend: selected_handle
        })
    );
    assert_eq!(model.action(keyboard_action), ModelEffect::None);

    model.apply_response(SocialResponse::Error {
        message: "synthetic keyboard failure".to_owned(),
    });
    let mut mapper = InputMapper::default();
    let controller_action = mapper
        .gamepad(GamepadInput::ButtonPressed(GamepadButton::West))
        .unwrap();
    assert_eq!(
        model.action(controller_action),
        ModelEffect::Request(SocialRequest::InviteFriend {
            friend: selected_handle
        })
    );
    assert_eq!(model.action(controller_action), ModelEffect::None);
}

#[test]
fn response_and_request_failures_clear_busy_state_for_retry() {
    let selected = friend("Online", SocialPresence::Online);
    let mut model = OverlayModel::new();
    model.apply_response(snapshot(vec![selected]));

    assert!(matches!(
        model.action(UiAction::Invite),
        ModelEffect::Request(_)
    ));
    model.apply_response(SocialResponse::Error {
        message: "synthetic safe error".to_owned(),
    });
    assert!(!model.view().busy);
    assert!(matches!(
        model.action(UiAction::Refresh),
        ModelEffect::Request(SocialRequest::RefreshFriends)
    ));

    model.request_failed();
    assert!(!model.view().busy);
    assert!(matches!(
        model.action(UiAction::Refresh),
        ModelEffect::Request(SocialRequest::RefreshFriends)
    ));
}

#[test]
fn navigation_and_cancel_remain_available_while_a_request_is_busy() {
    let mut model = OverlayModel::new();
    model.apply_response(snapshot(vec![
        friend("One", SocialPresence::Online),
        friend("Two", SocialPresence::Offline),
    ]));
    assert!(matches!(
        model.action(UiAction::Invite),
        ModelEffect::Request(_)
    ));

    assert_eq!(model.action(UiAction::MoveDown), ModelEffect::None);
    assert_eq!(model.view().selected, Some(1));
    assert_eq!(
        model.action(UiAction::Back),
        ModelEffect::Close(UiOutcome::Cancelled)
    );
}

#[test]
fn successful_action_clears_busy_state_and_closes() {
    let mut model = OverlayModel::new();
    model.apply_response(snapshot(vec![friend("Online", SocialPresence::Online)]));
    assert!(matches!(
        model.action(UiAction::Join),
        ModelEffect::Request(_)
    ));

    assert_eq!(
        model.apply_response(SocialResponse::Success {
            message: "synthetic success".to_owned(),
        }),
        ModelEffect::Close(UiOutcome::Completed)
    );
    assert!(!model.view().busy);
    assert_eq!(model.outcome(), UiOutcome::Completed);
}

#[test]
fn rendered_model_ignores_protocol_status_and_action_message_secrets() {
    let mut model = OverlayModel::new();
    model.apply_response(SocialResponse::Snapshot(SocialSnapshot {
        friends: vec![friend("Safe Driver", SocialPresence::Unknown)],
        status: "2810999999999999 synthetic-token ms-xbl-multiplayer://secret".to_owned(),
    }));
    model.apply_response(SocialResponse::Error {
        message: "synthetic-connection-string synthetic-authorization".to_owned(),
    });

    let rendered = format!("{:?}", model.view());
    for sentinel in [
        "2810999999999999",
        "synthetic-token",
        "ms-xbl-multiplayer",
        "synthetic-connection-string",
        "synthetic-authorization",
    ] {
        assert!(!rendered.contains(sentinel), "rendered {sentinel}");
    }
    assert!(rendered.contains("Safe Driver"));
}

#[tokio::test]
async fn client_reads_one_byte_fragmented_xdso_response() {
    let (client_socket, server_socket) = tokio::net::UnixStream::pair().unwrap();
    let client = SocialClient::from_stream(client_socket);
    let (mut server_reader, mut server_writer) = split(server_socket);
    let server = tokio::spawn(async move {
        assert_eq!(
            read_frame::<_, SocialRequest>(&mut server_reader)
                .await
                .unwrap(),
            SocialRequest::RefreshFriends
        );
        let response = snapshot(Vec::new());
        for byte in encode_frame(&response).unwrap() {
            server_writer.write_all(&[byte]).await.unwrap();
            tokio::task::yield_now().await;
        }
    });

    assert!(matches!(
        client.request(SocialRequest::RefreshFriends).await.unwrap(),
        SocialResponse::Snapshot(_)
    ));
    server.await.unwrap();
}

#[tokio::test]
async fn client_turns_service_disconnect_into_a_fixed_safe_error() {
    let (client_socket, server_socket) = tokio::net::UnixStream::pair().unwrap();
    let client = SocialClient::from_stream(client_socket);
    let server = tokio::spawn(async move {
        let (mut reader, _) = split(server_socket);
        read_frame::<_, SocialRequest>(&mut reader).await.unwrap();
    });

    assert_eq!(
        client.request(SocialRequest::Snapshot).await.unwrap_err(),
        ClientError::Disconnected
    );
    server.await.unwrap();
}

#[tokio::test]
async fn concurrent_client_requests_are_serialized_as_complete_frames() {
    let (client_socket, server_socket) = tokio::net::UnixStream::pair().unwrap();
    let client = SocialClient::from_stream(client_socket);
    let server = tokio::spawn(async move {
        let (mut reader, mut writer) = split(server_socket);
        let mut seen = Vec::new();
        for _ in 0..2 {
            let request = read_frame::<_, SocialRequest>(&mut reader).await.unwrap();
            let message = match request {
                SocialRequest::Snapshot => "snapshot",
                SocialRequest::RefreshFriends => "refresh",
                _ => panic!("unexpected action request"),
            };
            seen.push(message);
            let response = SocialResponse::Success {
                message: message.to_owned(),
            };
            writer
                .write_all(&encode_frame(&response).unwrap())
                .await
                .unwrap();
        }
        seen
    });

    let first = {
        let client = client.clone();
        tokio::spawn(async move {
            (
                "snapshot",
                client.request(SocialRequest::Snapshot).await.unwrap(),
            )
        })
    };
    let second = {
        let client = client.clone();
        tokio::spawn(async move {
            (
                "refresh",
                client.request(SocialRequest::RefreshFriends).await.unwrap(),
            )
        })
    };

    for (expected, response) in [first.await.unwrap(), second.await.unwrap()] {
        assert_eq!(
            response,
            SocialResponse::Success {
                message: expected.to_owned()
            }
        );
    }
    assert_eq!(server.await.unwrap().len(), 2);
}

#[test]
fn client_worker_stop_interrupts_in_flight_request_and_is_idempotent() {
    let (client_socket, mut server_socket) = StdUnixStream::pair().unwrap();
    server_socket
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let (mut worker, _responses) = ClientWorker::from_stream(client_socket);
    worker.request(SocialRequest::Snapshot).unwrap();

    let mut header = [0_u8; 10];
    server_socket.read_exact(&mut header).unwrap();
    assert_eq!(&header[..4], b"XDSO");
    let length = u32::from_le_bytes(header[6..10].try_into().unwrap()) as usize;
    let mut payload = vec![0_u8; length];
    server_socket.read_exact(&mut payload).unwrap();

    let (stopped, confirmation) = mpsc::channel();
    thread::spawn(move || {
        worker.stop();
        worker.stop();
        stopped.send(()).unwrap();
    });
    confirmation
        .recv_timeout(Duration::from_secs(1))
        .expect("worker shutdown must interrupt a blocked response read");
}

#[test]
fn client_worker_queue_is_bounded_nonblocking_and_recovers_after_full() {
    let (client_socket, mut server_socket) = StdUnixStream::pair().unwrap();
    server_socket
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let (mut worker, responses) = ClientWorker::from_stream(client_socket);

    worker.request(SocialRequest::Snapshot).unwrap();
    assert_eq!(
        read_sync_request(&mut server_socket),
        SocialRequest::Snapshot
    );

    worker.request(SocialRequest::RefreshFriends).unwrap();
    assert_eq!(
        worker.request(SocialRequest::Snapshot),
        Err(ClientWorkerRequestError::Full)
    );

    server_socket
        .write_all(
            &encode_frame(&SocialResponse::Success {
                message: "synthetic first response".to_owned(),
            })
            .unwrap(),
        )
        .unwrap();
    assert!(matches!(
        responses.recv_timeout(Duration::from_secs(1)).unwrap(),
        Ok(SocialResponse::Success { .. })
    ));
    assert_eq!(
        read_sync_request(&mut server_socket),
        SocialRequest::RefreshFriends
    );
    server_socket
        .write_all(
            &encode_frame(&SocialResponse::Success {
                message: "synthetic second response".to_owned(),
            })
            .unwrap(),
        )
        .unwrap();
    assert!(matches!(
        responses.recv_timeout(Duration::from_secs(1)).unwrap(),
        Ok(SocialResponse::Success { .. })
    ));

    worker.request(SocialRequest::Snapshot).unwrap();
    worker.stop();
}
