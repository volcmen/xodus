mod ui;

fn main() {
    if let Err(error) = ui::run() {
        eprintln!("xodus-overlay: {error}");
        std::process::exit(1);
    }
}
