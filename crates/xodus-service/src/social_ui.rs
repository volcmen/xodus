use std::{io::Error, path::Path};

use tokio::{io::AsyncWriteExt, net::UnixStream, process::Command};

pub const SOCIAL_UI_MAGIC: u32 = u32::from_le_bytes(*b"XDUI");

fn sibling_binary(name: &str, override_variable: &str) -> Result<std::path::PathBuf, Error> {
    if let Some(path) = std::env::var_os(override_variable) {
        return Ok(path.into());
    }
    let service = std::env::current_exe()?;
    let directory = service
        .parent()
        .ok_or_else(|| Error::other("xodus-service has no parent directory"))?;
    Ok(directory.join(name))
}

async fn run_cli(cli: &Path) -> Result<(), Error> {
    let status = Command::new(cli)
        .args(["forza-social", "--fuzzel"])
        .status()
        .await?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::other(format!(
            "Forza social UI exited with {status}"
        )))
    }
}

async fn run_social_ui(overlay: &Path, cli: &Path) -> Result<(), Error> {
    if Command::new(overlay)
        .status()
        .await
        .is_ok_and(|status| status.success())
    {
        return Ok(());
    }
    run_cli(cli).await
}

pub async fn serve(mut socket: UnixStream) -> Result<(), Error> {
    let overlay = sibling_binary("xodus-overlay", "XODUS_FORZA_OVERLAY")?;
    let cli = sibling_binary("xodus-cli", "XODUS_FORZA_CLI")?;
    let status = if run_social_ui(&overlay, &cli).await.is_ok() {
        0
    } else {
        1
    };
    socket.write_u8(status).await?;
    if status == 0 {
        Ok(())
    } else {
        Err(Error::other("Forza social UI failed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn preferred_overlay_completes_social_ui() {
        assert!(
            run_social_ui(Path::new("/usr/bin/true"), Path::new("/usr/bin/false"))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn falls_back_to_cli_when_overlay_fails() {
        assert!(
            run_social_ui(Path::new("/usr/bin/false"), Path::new("/usr/bin/true"))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn rejects_social_ui_only_when_overlay_and_fallback_fail() {
        assert!(
            run_social_ui(Path::new("/usr/bin/false"), Path::new("/usr/bin/false"))
                .await
                .is_err()
        );
    }
}
