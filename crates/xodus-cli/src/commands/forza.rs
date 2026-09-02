use std::{
    fmt,
    io::Write,
    process::{Command, ExitCode, Stdio},
};

use inquire::Select;
use reqwest::Client;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};
use xodus::{
    forza::{ForzaSession, XboxFriend},
    tokens::TokenManager,
};

const INVITE_PUBLISH_MAGIC: u32 = u32::from_le_bytes(*b"XDAP");

fn io_error(message: impl Into<String>) -> std::io::Error {
    std::io::Error::other(message.into())
}

async fn relay_invite_activation(uri: &str) -> Result<(), std::io::Error> {
    let runtime_dir =
        std::env::var("XDG_RUNTIME_DIR").map_err(|_| io_error("XDG_RUNTIME_DIR is not set"))?;
    let socket_path = format!("{runtime_dir}/xodus.sock");
    let mut socket = UnixStream::connect(&socket_path).await.map_err(|error| {
        io_error(format!(
            "Xodus is not running at {socket_path}: {error}. Start Forza first."
        ))
    })?;
    socket.write_u32_le(INVITE_PUBLISH_MAGIC).await?;
    socket.write_u32_le(uri.len() as u32).await?;
    socket.write_all(uri.as_bytes()).await?;
    match socket.read_u8().await? {
        0 => Ok(()),
        1 => Err(io_error("Xodus rejected the Xbox invite URI")),
        2 => Err(io_error(
            "Forza is running but has not registered its Xbox invite handler yet. Enter the main menu and retry.",
        )),
        status => Err(io_error(format!("Xodus returned unknown status {status}"))),
    }
}

fn select_friend(
    friends: Vec<XboxFriend>,
    requested: Option<&str>,
    use_fuzzel: bool,
) -> Result<XboxFriend, std::io::Error> {
    if friends.is_empty() {
        return Err(io_error("Your Xbox friends list is empty"));
    }

    if let Some(requested) = requested {
        return friends
            .into_iter()
            .find(|friend| {
                friend.gamertag.eq_ignore_ascii_case(requested) || friend.xuid == requested
            })
            .ok_or_else(|| io_error(format!("Xbox friend not found: {requested}")));
    }

    if use_fuzzel {
        let mut child = Command::new("fuzzel")
            .args([
                "--dmenu",
                "--prompt",
                "Xbox friend: ",
                "--lines",
                &friends.len().to_string(),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| io_error(format!("Failed to start fuzzel: {error}")))?;
        let input = friends
            .iter()
            .map(|friend| friend.gamertag.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        child
            .stdin
            .take()
            .ok_or_else(|| io_error("Failed to open fuzzel input"))?
            .write_all(input.as_bytes())?;
        let output = child.wait_with_output()?;
        if !output.status.success() {
            return Err(io_error("Friend selection cancelled"));
        }
        let selected = String::from_utf8(output.stdout)
            .map_err(|error| io_error(format!("Invalid fuzzel output: {error}")))?;
        let selected = selected.trim();
        return friends
            .into_iter()
            .find(|friend| friend.gamertag == selected)
            .ok_or_else(|| io_error(format!("Unknown Xbox friend selected: {selected}")));
    }

    Select::new("Xbox friend to invite:", friends)
        .prompt()
        .map_err(|error| io_error(format!("Friend selection cancelled: {error}")))
}

pub async fn run(
    client: &Client,
    tokens: &TokenManager,
    requested_friend: Option<&str>,
    use_fuzzel: bool,
    dry_run: bool,
) -> ExitCode {
    match run_inner(client, tokens, requested_friend, use_fuzzel, dry_run).await {
        Ok(friend) if dry_run => {
            let message = format!(
                "Ready to invite {}. Forza has published a joinable Xbox activity.",
                friend.gamertag
            );
            println!("{message}");
            notify_hyprland(&message, true, use_fuzzel);
            ExitCode::SUCCESS
        }
        Ok(friend) => {
            let message = format!("Xbox invite sent to {}.", friend.gamertag);
            println!("{message}");
            notify_hyprland(&message, true, use_fuzzel);
            ExitCode::SUCCESS
        }
        Err(error) => {
            let message = format!("Forza invite failed: {error}");
            eprintln!("{message}");
            notify_hyprland(&message, false, use_fuzzel);
            ExitCode::FAILURE
        }
    }
}

pub async fn run_join(
    client: &Client,
    tokens: &TokenManager,
    requested_friend: Option<&str>,
    use_fuzzel: bool,
    dry_run: bool,
) -> ExitCode {
    match run_join_inner(client, tokens, requested_friend, use_fuzzel, dry_run).await {
        Ok(friend) if dry_run => {
            let message = format!(
                "Ready to join {}. A joinable Forza activity is available.",
                friend.gamertag
            );
            println!("{message}");
            notify_hyprland(&message, true, use_fuzzel);
            ExitCode::SUCCESS
        }
        Ok(friend) => {
            let message = format!("Join request delivered to Forza for {}.", friend.gamertag);
            println!("{message}");
            notify_hyprland(&message, true, use_fuzzel);
            ExitCode::SUCCESS
        }
        Err(error) => {
            let message = format!("Forza join failed: {error}");
            eprintln!("{message}");
            notify_hyprland(&message, false, use_fuzzel);
            ExitCode::FAILURE
        }
    }
}

#[derive(Clone, Copy)]
enum SocialAction {
    Invite,
    Join,
}

impl fmt::Display for SocialAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Invite => "Invite a friend to my Forza session",
            Self::Join => "Join a friend's Forza session",
        })
    }
}

fn select_social_action(use_fuzzel: bool) -> Result<SocialAction, std::io::Error> {
    let actions = vec![SocialAction::Invite, SocialAction::Join];
    if !use_fuzzel {
        return Select::new("Forza Xbox social action:", actions)
            .prompt()
            .map_err(|error| io_error(format!("Action selection cancelled: {error}")));
    }

    let mut child = Command::new("fuzzel")
        .args(["--dmenu", "--prompt", "Forza Xbox: ", "--lines", "2"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| io_error(format!("Failed to start fuzzel: {error}")))?;
    child
        .stdin
        .take()
        .ok_or_else(|| io_error("Failed to open fuzzel input"))?
        .write_all(b"Invite a friend to my Forza session\nJoin a friend's Forza session")?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(io_error("Action selection cancelled"));
    }
    match String::from_utf8(output.stdout)
        .map_err(|error| io_error(format!("Invalid fuzzel output: {error}")))?
        .trim()
    {
        "Invite a friend to my Forza session" => Ok(SocialAction::Invite),
        "Join a friend's Forza session" => Ok(SocialAction::Join),
        selected => Err(io_error(format!("Unknown action selected: {selected}"))),
    }
}

pub async fn run_social(client: &Client, tokens: &TokenManager, use_fuzzel: bool) -> ExitCode {
    match select_social_action(use_fuzzel) {
        Ok(SocialAction::Invite) => run(client, tokens, None, use_fuzzel, false).await,
        Ok(SocialAction::Join) => run_join(client, tokens, None, use_fuzzel, false).await,
        Err(error) => {
            eprintln!("Forza social action cancelled: {error}");
            ExitCode::FAILURE
        }
    }
}

fn notify_hyprland(message: &str, success: bool, enabled: bool) {
    if !enabled {
        return;
    }
    let (icon, color) = if success {
        ("1", "rgb(55aa55)")
    } else {
        ("3", "rgb(cc3333)")
    };
    let _ = Command::new("hyprctl")
        .args(["notify", icon, "7000", color, message])
        .status();
}

async fn run_inner(
    client: &Client,
    tokens: &TokenManager,
    requested_friend: Option<&str>,
    use_fuzzel: bool,
    dry_run: bool,
) -> Result<XboxFriend, Box<dyn std::error::Error>> {
    let mut session = ForzaSession::authenticate(client, tokens).await?;
    let friend = select_friend(session.friends().await?, requested_friend, use_fuzzel)?;

    if dry_run {
        let local_xuid = session.local_xuid().to_owned();
        session.activity(&local_xuid).await?.ok_or_else(|| {
            io_error(
                "Forza has no published Xbox activity. Start the game, enter an online multiplayer lobby, and retry.",
            )
        })?;
        return Ok(friend);
    }

    session.invite(&friend.xuid).await?;
    Ok(friend)
}

async fn run_join_inner(
    client: &Client,
    tokens: &TokenManager,
    requested_friend: Option<&str>,
    use_fuzzel: bool,
    dry_run: bool,
) -> Result<XboxFriend, Box<dyn std::error::Error>> {
    let mut session = ForzaSession::authenticate(client, tokens).await?;
    let friend = select_friend(session.friends().await?, requested_friend, use_fuzzel)?;
    let uri = session.join_uri(&friend.xuid).await.map_err(|error| {
        if matches!(error, xodus::forza::XboxServiceError::MissingFriendActivity) {
            io_error(format!(
                "{} has no joinable Forza activity. They must be online in a joinable multiplayer session.",
                friend.gamertag
            ))
        } else {
            io_error(error.to_string())
        }
    })?;

    if !dry_run {
        relay_invite_activation(&uri).await?;
    }

    Ok(friend)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_friend_by_gamertag_or_xuid() {
        let friends = vec![XboxFriend {
            gamertag: "Driver One".into(),
            xuid: "123".into(),
            presence_state: None,
            presence_text: None,
        }];

        assert_eq!(
            select_friend(friends.clone(), Some("driver one"), false)
                .unwrap()
                .xuid,
            "123"
        );
        assert_eq!(
            select_friend(friends, Some("123"), false).unwrap().gamertag,
            "Driver One"
        );
    }
}
