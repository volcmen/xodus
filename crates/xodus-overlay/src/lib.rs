use std::ffi::{OsStr, OsString};

pub mod action;
pub mod client;
pub mod controller;
pub mod model;
pub mod ui;

use model::UiMode;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UiModeParseError;

impl std::fmt::Display for UiModeParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("invalid overlay mode arguments")
    }
}

impl std::error::Error for UiModeParseError {}

pub fn parse_ui_mode<I, S>(arguments: I) -> Result<UiMode, UiModeParseError>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let arguments = arguments.into_iter().map(Into::into).collect::<Vec<_>>();
    match arguments.as_slice() {
        [] => Ok(UiMode::Full),
        [flag, value] if flag == OsStr::new("--mode") && value == OsStr::new("full") => {
            Ok(UiMode::Full)
        }
        [flag, value] if flag == OsStr::new("--mode") && value == OsStr::new("invite-only") => {
            Ok(UiMode::InviteOnly)
        }
        _ => Err(UiModeParseError),
    }
}
