use adw::prelude::*;
use gtk::{gdk, glib};
use std::{cell::RefCell, rc::Rc, sync::Arc, time::Duration};
use tokio::{runtime::Runtime, sync::mpsc};
use xodus::forza::{FriendAction, SocialRequest, SocialResponse, SocialSnapshot, SocialStatus};
use xodus_overlay::{
    action::{InputMapper, KeyInput, UiAction},
    client::SocialClient,
    controller::ControllerListener,
    hyprland,
    model::OverlayModel,
};

const APP_ID: &str = "io.xodus.ForzaSocial";

enum NetworkEvent {
    Response(SocialResponse),
    Failure(String),
}

struct OverlayView {
    window: adw::ApplicationWindow,
    toast_overlay: adw::ToastOverlay,
    status: gtk::Label,
    list: gtk::ListBox,
    model: RefCell<OverlayModel>,
    client: SocialClient,
    runtime: Arc<Runtime>,
    network_sender: mpsc::UnboundedSender<NetworkEvent>,
}

impl OverlayView {
    fn request(&self, request: SocialRequest) {
        let client = self.client.clone();
        let sender = self.network_sender.clone();
        self.runtime.spawn(async move {
            let event = match client.request(request).await {
                Ok(response) => NetworkEvent::Response(response),
                Err(error) => NetworkEvent::Failure(error.to_string()),
            };
            let _ = sender.send(event);
        });
    }

    fn toast(&self, message: impl AsRef<str>) {
        self.toast_overlay
            .add_toast(adw::Toast::new(message.as_ref()));
    }

    fn render(&self) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }

        let (snapshot, selected, row_count) = {
            let model = self.model.borrow();
            (
                model.snapshot().clone(),
                model.selected(),
                model.row_count(),
            )
        };
        self.status.set_label(match snapshot.status {
            SocialStatus::WaitingForGame => "Waiting for Forza",
            SocialStatus::Connecting => "Connecting to Xbox",
            SocialStatus::Connected => "Xbox connected",
            SocialStatus::Reconnecting => "Reconnecting to Xbox",
            SocialStatus::Error => "Xbox connection needs attention",
        });

        for invite in &snapshot.invitations {
            self.list.append(&social_row(
                &format!("Invite from {}", invite.sender_gamertag),
                &invite.title_name,
                "A  Join",
                true,
            ));
        }
        for friend in &snapshot.friends {
            let action = match friend.action {
                FriendAction::Join => "A  Join   X  Invite",
                FriendAction::Invite => "A / X  Invite",
                FriendAction::Unavailable => "Unavailable",
            };
            self.list.append(&social_row(
                &friend.gamertag,
                &friend.presence,
                action,
                false,
            ));
        }
        if row_count == 0 {
            self.list.append(&social_row(
                "No friends or invitations yet",
                "Press R or Y to refresh Xbox social",
                "",
                false,
            ));
            self.list.unselect_all();
        } else if let Some(row) = self.list.row_at_index(selected as i32) {
            self.list.select_row(Some(&row));
            row.grab_focus();
        }
    }

    fn sync_selection(&self) {
        let selected = self.model.borrow().selected() as i32;
        if let Some(row) = self.list.row_at_index(selected) {
            self.list.select_row(Some(&row));
            row.grab_focus();
        }
    }

    fn action(&self, action: UiAction) {
        match action {
            UiAction::MoveUp => {
                self.model.borrow_mut().move_up();
                self.sync_selection();
            }
            UiAction::MoveDown => {
                self.model.borrow_mut().move_down();
                self.sync_selection();
            }
            UiAction::Activate => {
                if let Some(request) = self.model.borrow().primary_request() {
                    self.request(request);
                } else {
                    self.toast("Nothing available for this selection");
                }
            }
            UiAction::Invite => {
                if let Some(request) = self.model.borrow().invite_request() {
                    self.request(request);
                } else {
                    self.toast("Select a friend to send an invite");
                }
            }
            UiAction::Refresh => self.request(SocialRequest::RefreshFriends),
            UiAction::Back => self.window.close(),
        }
    }

    fn network_event(&self, event: NetworkEvent) {
        match event {
            NetworkEvent::Response(SocialResponse::Snapshot(snapshot)) => {
                self.model.borrow_mut().replace(snapshot);
                self.render();
            }
            NetworkEvent::Response(SocialResponse::Success { message }) => {
                self.toast(message);
                self.request(SocialRequest::Snapshot);
            }
            NetworkEvent::Response(SocialResponse::Error { message }) => self.toast(message),
            NetworkEvent::Failure(message) => {
                self.status.set_label("Xodus service unavailable");
                self.toast(message);
            }
        }
    }
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("xodus-overlay-net")
            .build()?,
    );
    let application = adw::Application::builder().application_id(APP_ID).build();
    application.connect_startup(|_| install_css());
    application.connect_activate(move |app| {
        if let Some(window) = app.active_window() {
            window.present();
            return;
        }
        build_window(app, runtime.clone());
    });
    application.run();
    Ok(())
}

fn build_window(application: &adw::Application, runtime: Arc<Runtime>) {
    let window = adw::ApplicationWindow::builder()
        .application(application)
        .title("Forza Social")
        .default_width(900)
        .default_height(620)
        .build();
    window.add_css_class("forza-social-window");

    let header = adw::HeaderBar::new();
    let title = adw::WindowTitle::new("Forza Social", "Xbox friends and invitations");
    header.set_title_widget(Some(&title));

    let status = gtk::Label::new(Some("Connecting to Xodus"));
    status.add_css_class("status-pill");
    header.pack_end(&status);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.set_activate_on_single_click(false);
    list.add_css_class("social-list");

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&list)
        .build();
    let help = gtk::Label::new(Some(
        "Keyboard  ↑↓ / W S · Enter Join · I Invite · R Refresh · Esc Close     Controller  D-pad / Stick · A Join · X Invite · Y Refresh · B Close",
    ));
    help.set_wrap(true);
    help.add_css_class("controls-hint");

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&header);
    content.append(&scroller);
    content.append(&help);

    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&content));
    window.set_content(Some(&toast_overlay));

    let client = SocialClient::from_runtime_dir()
        .unwrap_or_else(|_| SocialClient::new("/run/user/0/xodus-unavailable.sock"));
    let (network_sender, mut network_receiver) = mpsc::unbounded_channel();
    let (action_sender, mut action_receiver) = mpsc::unbounded_channel();
    let initial = SocialSnapshot {
        status: SocialStatus::Connecting,
        invitations: Vec::new(),
        friends: Vec::new(),
    };
    let view = Rc::new(OverlayView {
        window: window.clone(),
        toast_overlay,
        status,
        list: list.clone(),
        model: RefCell::new(OverlayModel::new(initial)),
        client,
        runtime,
        network_sender,
    });
    view.render();

    let controller = Rc::new(RefCell::new(Some(ControllerListener::start(action_sender))));
    let controller_on_close = controller.clone();
    window.connect_close_request(move |_| {
        controller_on_close.borrow_mut().take();
        glib::Propagation::Proceed
    });

    let key_view = view.clone();
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(move |_, key, _, _| {
        if let Some(action) = map_gdk_key(key) {
            key_view.action(action);
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    window.add_controller(keys);

    let row_view = view.clone();
    list.connect_row_activated(move |_, row| {
        if row_view.model.borrow_mut().select(row.index() as usize) {
            row_view.action(UiAction::Activate);
        }
    });
    let selection_view = view.clone();
    list.connect_row_selected(move |_, row| {
        if let Some(row) = row {
            selection_view
                .model
                .borrow_mut()
                .select(row.index() as usize);
        }
    });

    let event_view = view.clone();
    glib::timeout_add_local(Duration::from_millis(16), move || {
        while let Ok(action) = action_receiver.try_recv() {
            event_view.action(action);
        }
        while let Ok(event) = network_receiver.try_recv() {
            event_view.network_event(event);
        }
        glib::ControlFlow::Continue
    });

    window.present();
    let mapped_window = window.clone();
    glib::idle_add_local_once(move || {
        mapped_window.present();
        hyprland::apply_runtime_window_rules();
    });
    view.request(SocialRequest::Snapshot);
}

fn social_row(title: &str, subtitle: &str, action: &str, invitation: bool) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_activatable(true);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 18);
    content.set_margin_top(16);
    content.set_margin_bottom(16);
    content.set_margin_start(20);
    content.set_margin_end(20);

    let labels = gtk::Box::new(gtk::Orientation::Vertical, 4);
    labels.set_hexpand(true);
    let title_label = gtk::Label::new(Some(title));
    title_label.set_xalign(0.0);
    title_label.add_css_class("social-title");
    let subtitle_label = gtk::Label::new(Some(subtitle));
    subtitle_label.set_xalign(0.0);
    subtitle_label.add_css_class("social-subtitle");
    labels.append(&title_label);
    labels.append(&subtitle_label);

    let action_label = gtk::Label::new(Some(action));
    action_label.add_css_class("row-action");
    content.append(&labels);
    content.append(&action_label);
    row.set_child(Some(&content));
    if invitation {
        row.add_css_class("invitation");
    }
    row
}

fn map_gdk_key(key: gdk::Key) -> Option<UiAction> {
    let name = key.name()?;
    let input = match name.as_str() {
        "Up" | "KP_Up" => KeyInput::Up,
        "Down" | "KP_Down" => KeyInput::Down,
        "w" | "W" => KeyInput::W,
        "s" | "S" => KeyInput::S,
        "Return" | "KP_Enter" => KeyInput::Enter,
        "space" => KeyInput::Space,
        "Escape" => KeyInput::Escape,
        "BackSpace" => KeyInput::Backspace,
        "i" | "I" => KeyInput::I,
        "x" | "X" => KeyInput::X,
        "r" | "R" => KeyInput::R,
        "F5" => KeyInput::F5,
        _ => KeyInput::Other,
    };
    InputMapper::key(input)
}

fn install_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(include_str!("style.css"));
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}
