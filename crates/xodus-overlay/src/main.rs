use std::process::ExitCode;

fn main() -> ExitCode {
    ExitCode::from(xodus_overlay::ui::run() as u8)
}
