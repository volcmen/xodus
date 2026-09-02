use std::{env, process::Command};

pub fn runtime_commands(is_hyprland: bool) -> Vec<Vec<&'static str>> {
    if !is_hyprland {
        return Vec::new();
    }

    vec![
        vec![
            "dispatch",
            "hl.dsp.window.float({ window = \"class:^io[.]xodus[.]ForzaSocial$\", action = \"on\" })",
        ],
        vec![
            "dispatch",
            "hl.dsp.window.resize({ window = \"class:^io[.]xodus[.]ForzaSocial$\", x = 900, y = 620, relative = false })",
        ],
        vec![
            "dispatch",
            "hl.dsp.focus({ window = \"class:^io[.]xodus[.]ForzaSocial$\" })",
        ],
        vec![
            "dispatch",
            "hl.dsp.window.pin({ window = \"class:^io[.]xodus[.]ForzaSocial$\", action = \"on\" })",
        ],
        vec![
            "dispatch",
            "hl.dsp.window.center({ window = \"class:^io[.]xodus[.]ForzaSocial$\" })",
        ],
    ]
}

/// Apply window rules to the running overlay without modifying Hyprland's
/// persisted configuration. Failures are intentionally non-fatal: GTK still
/// presents a regular application window on other compositors.
pub fn apply_runtime_window_rules() {
    let is_hyprland = env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some();
    for arguments in runtime_commands(is_hyprland) {
        let _ = Command::new("hyprctl").args(arguments).status();
    }
}

#[cfg(test)]
mod tests {
    use super::runtime_commands;

    #[test]
    fn targets_only_the_forza_social_application_class() {
        let commands = runtime_commands(true);
        assert_eq!(
            commands,
            vec![
                vec![
                    "dispatch",
                    "hl.dsp.window.float({ window = \"class:^io[.]xodus[.]ForzaSocial$\", action = \"on\" })"
                ],
                vec![
                    "dispatch",
                    "hl.dsp.window.resize({ window = \"class:^io[.]xodus[.]ForzaSocial$\", x = 900, y = 620, relative = false })"
                ],
                vec![
                    "dispatch",
                    "hl.dsp.focus({ window = \"class:^io[.]xodus[.]ForzaSocial$\" })"
                ],
                vec![
                    "dispatch",
                    "hl.dsp.window.pin({ window = \"class:^io[.]xodus[.]ForzaSocial$\", action = \"on\" })"
                ],
                vec![
                    "dispatch",
                    "hl.dsp.window.center({ window = \"class:^io[.]xodus[.]ForzaSocial$\" })"
                ],
            ]
        );
    }

    #[test]
    fn does_nothing_outside_hyprland() {
        assert!(runtime_commands(false).is_empty());
    }
}
