use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use adw::prelude::*;
use gtk::gdk;
use xodus::forza::{SocialPresence, SocialRequest, SocialResponse};

use crate::action::{InputMapper, KeyInput, UiAction};
use crate::client::{ClientError, SocialClient};
use crate::controller::ControllerListener;
use crate::model::{ModelEffect, OverlayModel, UiOutcome};

const APPLICATION_ID: &str = "com.xodusgaming.XodusOverlay";

#[derive(Clone)]
struct Widgets {
    list: gtk::ListBox,
    status: gtk::Label,
    invite: gtk::Button,
    join: gtk::Button,
}

#[must_use]
pub fn run() -> i32 {
    let application = adw::Application::builder()
        .application_id(APPLICATION_ID)
        .build();
    let exit_code = Rc::new(Cell::new(UiOutcome::Failed.exit_code()));
    let activation_exit = Rc::clone(&exit_code);
    application.connect_activate(move |application| {
        build_window(application, Rc::clone(&activation_exit));
    });
    application.run();
    exit_code.get()
}

fn build_window(application: &adw::Application, exit_code: Rc<Cell<i32>>) {
    install_css();

    let header = adw::HeaderBar::builder()
        .title_widget(&adw::WindowTitle::new("Forza Friends", "Xodus"))
        .build();
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .css_classes(["boxed-list"])
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&list)
        .build();
    let status = gtk::Label::builder()
        .label("Loading friends")
        .wrap(true)
        .xalign(0.0)
        .css_classes(["status"])
        .build();
    let invite = gtk::Button::with_label("Invite");
    let join = gtk::Button::with_label("Join");
    let refresh = gtk::Button::with_label("Refresh");
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.append(&invite);
    buttons.append(&join);
    buttons.append(&refresh);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.add_css_class("overlay-content");
    content.append(&header);
    content.append(&scroller);
    content.append(&status);
    content.append(&buttons);

    let window = adw::ApplicationWindow::builder()
        .application(application)
        .title("Xodus Forza Friends")
        .default_width(520)
        .default_height(620)
        .content(&content)
        .build();
    let widgets = Widgets {
        list,
        status,
        invite,
        join,
    };
    let model = Rc::new(RefCell::new(OverlayModel::new()));
    render(&model.borrow(), &widgets);

    let (requests, responses) = spawn_client_worker();
    let alive = Rc::new(Cell::new(true));
    let window_weak = window.downgrade();

    connect_button(
        &widgets.invite,
        UiAction::Invite,
        &model,
        &widgets,
        &requests,
        &window_weak,
        &exit_code,
    );
    connect_button(
        &widgets.join,
        UiAction::Join,
        &model,
        &widgets,
        &requests,
        &window_weak,
        &exit_code,
    );
    connect_button(
        &refresh,
        UiAction::Refresh,
        &model,
        &widgets,
        &requests,
        &window_weak,
        &exit_code,
    );

    {
        let model = Rc::clone(&model);
        widgets.list.connect_row_selected(move |_, row| {
            model
                .borrow_mut()
                .select(row.map(|row| row.index() as usize));
        });
    }
    {
        let model = Rc::clone(&model);
        let widgets = widgets.clone();
        let requests = requests.clone();
        let window = window_weak.clone();
        let exit_code = Rc::clone(&exit_code);
        widgets.list.clone().connect_row_activated(move |_, _| {
            dispatch(
                UiAction::Activate,
                &model,
                &widgets,
                &requests,
                &window,
                &exit_code,
            );
        });
    }

    let keys = gtk::EventControllerKey::new();
    {
        let model = Rc::clone(&model);
        let widgets = widgets.clone();
        let requests = requests.clone();
        let window = window_weak.clone();
        let exit_code = Rc::clone(&exit_code);
        keys.connect_key_pressed(move |_, key, _, _| {
            let Some(action) = InputMapper::key(map_key(key)) else {
                return gtk::glib::Propagation::Proceed;
            };
            dispatch(action, &model, &widgets, &requests, &window, &exit_code);
            gtk::glib::Propagation::Stop
        });
    }
    window.add_controller(keys);

    {
        let model = Rc::clone(&model);
        let widgets = widgets.clone();
        let alive = Rc::clone(&alive);
        let window = window_weak.clone();
        let exit_code = Rc::clone(&exit_code);
        gtk::glib::timeout_add_local(Duration::from_millis(16), move || {
            if !alive.get() {
                return gtk::glib::ControlFlow::Break;
            }
            let mut changed = false;
            let mut channel_closed = false;
            let mut close = None;
            loop {
                match responses.try_recv() {
                    Ok(Ok(response)) => {
                        if let ModelEffect::Close(outcome) =
                            model.borrow_mut().apply_response(response)
                        {
                            close = Some(outcome);
                        }
                        changed = true;
                    }
                    Ok(Err(_)) => {
                        model.borrow_mut().service_failed();
                        changed = true;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        model.borrow_mut().service_failed();
                        changed = true;
                        channel_closed = true;
                        break;
                    }
                }
            }
            if changed {
                render(&model.borrow(), &widgets);
            }
            if let Some(outcome) = close {
                exit_code.set(outcome.exit_code());
                if let Some(window) = window.upgrade() {
                    window.close();
                }
                return gtk::glib::ControlFlow::Break;
            }
            if channel_closed {
                gtk::glib::ControlFlow::Break
            } else {
                gtk::glib::ControlFlow::Continue
            }
        });
    }

    let (controller_actions, actions) = mpsc::channel();
    let controller = Rc::new(ControllerListener::start(controller_actions));
    {
        let model = Rc::clone(&model);
        let widgets = widgets.clone();
        let requests = requests.clone();
        let window = window_weak.clone();
        let exit_code = Rc::clone(&exit_code);
        let alive = Rc::clone(&alive);
        gtk::glib::timeout_add_local(Duration::from_millis(16), move || {
            if !alive.get() {
                return gtk::glib::ControlFlow::Break;
            }
            while let Ok(action) = actions.try_recv() {
                dispatch(action, &model, &widgets, &requests, &window, &exit_code);
            }
            gtk::glib::ControlFlow::Continue
        });
    }

    {
        let model = Rc::clone(&model);
        let alive = Rc::clone(&alive);
        let controller = Rc::clone(&controller);
        window.connect_close_request(move |_| {
            alive.set(false);
            let outcome = match model.borrow().outcome() {
                UiOutcome::Pending => UiOutcome::Cancelled,
                outcome => outcome,
            };
            exit_code.set(outcome.exit_code());
            drop(Rc::clone(&controller));
            gtk::glib::Propagation::Proceed
        });
    }

    if requests.send(OverlayModel::initial_request()).is_err() {
        model.borrow_mut().service_failed();
        render(&model.borrow(), &widgets);
    }
    window.present();
}

fn connect_button(
    button: &gtk::Button,
    action: UiAction,
    model: &Rc<RefCell<OverlayModel>>,
    widgets: &Widgets,
    requests: &mpsc::Sender<SocialRequest>,
    window: &gtk::glib::WeakRef<adw::ApplicationWindow>,
    exit_code: &Rc<Cell<i32>>,
) {
    let model = Rc::clone(model);
    let widgets = widgets.clone();
    let requests = requests.clone();
    let window = window.clone();
    let exit_code = Rc::clone(exit_code);
    button.connect_clicked(move |_| {
        dispatch(action, &model, &widgets, &requests, &window, &exit_code);
    });
}

fn dispatch(
    action: UiAction,
    model: &Rc<RefCell<OverlayModel>>,
    widgets: &Widgets,
    requests: &mpsc::Sender<SocialRequest>,
    window: &gtk::glib::WeakRef<adw::ApplicationWindow>,
    exit_code: &Rc<Cell<i32>>,
) {
    match model.borrow_mut().action(action) {
        ModelEffect::None => {}
        ModelEffect::Request(request) => {
            if requests.send(request).is_err() {
                model.borrow_mut().service_failed();
            }
        }
        ModelEffect::Close(outcome) => {
            exit_code.set(outcome.exit_code());
            if let Some(window) = window.upgrade() {
                window.close();
            }
        }
    }
    render(&model.borrow(), widgets);
}

fn render(model: &OverlayModel, widgets: &Widgets) {
    let view = model.view();
    while let Some(child) = widgets.list.first_child() {
        widgets.list.remove(&child);
    }
    for friend in &view.friends {
        let presence = match friend.presence {
            SocialPresence::Online => "Online",
            SocialPresence::Offline => "Offline",
            SocialPresence::Unknown => "Unknown",
        };
        let row = adw::ActionRow::builder()
            .title(&friend.display_label)
            .subtitle(presence)
            .activatable(true)
            .build();
        widgets.list.append(&row);
    }
    if let Some(index) = view.selected
        && let Some(row) = widgets.list.row_at_index(index as i32)
    {
        widgets.list.select_row(Some(&row));
    }
    widgets.status.set_label(&view.status);
    let has_selection = view.selected.is_some();
    widgets.invite.set_sensitive(has_selection);
    widgets.join.set_sensitive(has_selection);
}

fn spawn_client_worker() -> (
    mpsc::Sender<SocialRequest>,
    mpsc::Receiver<Result<SocialResponse, ClientError>>,
) {
    let (requests, incoming) = mpsc::channel();
    let (outgoing, responses) = mpsc::channel();
    thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
        {
            Ok(runtime) => runtime,
            Err(_) => {
                let _ = outgoing.send(Err(ClientError::Unavailable));
                return;
            }
        };
        let client = match runtime.block_on(SocialClient::connect_default()) {
            Ok(client) => client,
            Err(error) => {
                let _ = outgoing.send(Err(error));
                return;
            }
        };
        while let Ok(request) = incoming.recv() {
            let response = runtime.block_on(client.request(request));
            let disconnected = response.is_err();
            if outgoing.send(response).is_err() || disconnected {
                return;
            }
        }
    });
    (requests, responses)
}

fn install_css() {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    provider.load_from_data(include_str!("style.css"));
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

fn map_key(key: gdk::Key) -> KeyInput {
    match key {
        gdk::Key::Up => KeyInput::Up,
        gdk::Key::w | gdk::Key::W => KeyInput::W,
        gdk::Key::Down => KeyInput::Down,
        gdk::Key::s | gdk::Key::S => KeyInput::S,
        gdk::Key::Return | gdk::Key::KP_Enter => KeyInput::Enter,
        gdk::Key::space => KeyInput::Space,
        gdk::Key::Escape => KeyInput::Escape,
        gdk::Key::BackSpace => KeyInput::Backspace,
        gdk::Key::i | gdk::Key::I => KeyInput::I,
        gdk::Key::j | gdk::Key::J => KeyInput::J,
        gdk::Key::r | gdk::Key::R => KeyInput::R,
        gdk::Key::F5 => KeyInput::F5,
        _ => KeyInput::Other,
    }
}
