use std::process::ExitCode;

fn main() -> ExitCode {
    let Ok(mode) = xodus_overlay::parse_ui_mode(std::env::args_os().skip(1)) else {
        eprintln!("usage: xodus-overlay [--mode full|invite-only]");
        return ExitCode::from(2);
    };
    ExitCode::from(xodus_overlay::ui::run(mode) as u8)
}
