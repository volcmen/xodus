use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::process::Child;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub struct SocialUiLauncher {
    path: Option<PathBuf>,
    gate: Arc<Mutex<()>>,
}

impl SocialUiLauncher {
    pub fn discover() -> Self {
        let path = resolve_overlay_path(std::env::var_os("XODUS_SOCIAL_UI"), || {
            std::env::current_exe()
        })
        .ok();
        Self {
            path,
            gate: Arc::new(Mutex::new(())),
        }
    }

    #[cfg(test)]
    pub fn from_path(path: PathBuf) -> Self {
        Self {
            path: Some(path),
            gate: Arc::new(Mutex::new(())),
        }
    }

    pub async fn wait_idle(&self) {
        let _guard = self.gate.lock().await;
    }

    pub async fn serve(
        &self,
        socket: &mut UnixStream,
        cancellation: &CancellationToken,
    ) -> std::io::Result<()> {
        let Ok(_guard) = Arc::clone(&self.gate).try_lock_owned() else {
            return write_status(socket, 2).await;
        };
        if cancellation.is_cancelled() {
            return write_status(socket, 2).await;
        }
        let Some(path) = &self.path else {
            log::warn!("Social UI path resolution failed");
            return write_status(socket, 2).await;
        };
        let mut child = match tokio::process::Command::from(overlay_command(path)).spawn() {
            Ok(child) => child,
            Err(_) => {
                log::warn!("Social UI process spawn failed");
                return write_status(socket, 2).await;
            }
        };

        let mut peer_byte = [0_u8; 1];
        let status = tokio::select! {
            biased;
            _ = cancellation.cancelled() => {
                terminate_and_reap(&mut child).await;
                log::debug!("Social UI process stopped on service cancellation");
                2
            }
            read = socket.read(&mut peer_byte) => {
                terminate_and_reap(&mut child).await;
                if matches!(read, Ok(0) | Err(_)) {
                    log::debug!("Social UI process stopped on peer disconnect");
                } else {
                    log::warn!("Social UI protocol input was unexpected");
                }
                2
            }
            exit = child.wait() => map_exit(exit),
        };
        write_status(socket, status).await
    }
}

fn map_exit(exit: io::Result<std::process::ExitStatus>) -> u8 {
    match exit.ok().and_then(|exit| exit.code()) {
        Some(code @ 0..=2) => code as u8,
        _ => {
            log::warn!("Social UI process exited unexpectedly");
            2
        }
    }
}

async fn terminate_and_reap(child: &mut Child) {
    let _ = child.start_kill();
    let _ = child.wait().await;
}

async fn write_status(socket: &mut UnixStream, status: u8) -> io::Result<()> {
    socket.write_all(&[status]).await?;
    socket.shutdown().await
}

pub(crate) fn sibling_overlay_path(current_exe: &Path) -> io::Result<PathBuf> {
    current_exe
        .parent()
        .map(|parent| parent.join("xodus-overlay"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "service executable has no parent"))
}

pub(crate) fn resolve_overlay_path<F>(
    explicit: Option<OsString>,
    current_exe: F,
) -> io::Result<PathBuf>
where
    F: FnOnce() -> io::Result<PathBuf>,
{
    match explicit {
        Some(path) if !path.is_empty() => Ok(PathBuf::from(path)),
        _ => sibling_overlay_path(&current_exe()?),
    }
}

pub(crate) fn overlay_command(path: &Path) -> Command {
    Command::new(path)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::io::Write;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::{Mutex as StdMutex, MutexGuard};
    use std::time::Duration;

    use tempfile::TempDir;
    use tokio::io::AsyncReadExt;
    use tokio::net::UnixStream;
    use tokio::time::{sleep, timeout};
    use tokio_util::sync::CancellationToken;

    use super::{SocialUiLauncher, overlay_command, resolve_overlay_path, sibling_overlay_path};

    static HELPER_LOCK: StdMutex<()> = StdMutex::new(());

    struct Helper {
        _directory: TempDir,
        _execution_guard: MutexGuard<'static, ()>,
        path: PathBuf,
    }

    fn write_helper_file(path: &Path, body: &str, mode: u32) {
        let mut file = fs::File::create(path).unwrap();
        file.write_all(format!("#!/bin/sh\n{body}\n").as_bytes())
            .unwrap();
        file.sync_all().unwrap();
        drop(file);
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    fn helper_directory() -> TempDir {
        tempfile::tempdir_in(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target")).unwrap()
    }

    fn helper_with_mode(body: &str, mode: u32) -> Helper {
        let execution_guard = HELPER_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let directory = helper_directory();
        let path = directory.path().join("overlay-helper");
        write_helper_file(&path, body, mode);
        Helper {
            _directory: directory,
            _execution_guard: execution_guard,
            path,
        }
    }

    fn helper(body: &str) -> Helper {
        helper_with_mode(body, 0o700)
    }

    fn blocking_helper(seconds: u8) -> (Helper, PathBuf, PathBuf) {
        let execution_guard = HELPER_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let directory = helper_directory();
        let path = directory.path().join("overlay-helper");
        let starts = directory.path().join("starts");
        let pid = directory.path().join("pid");
        write_helper_file(
            &path,
            format!(
                "printf 'x\\n' >> '{}'\nprintf '%s' \"$$\" > '{}'\nexec sleep {seconds}",
                starts.display(),
                pid.display()
            )
            .as_str(),
            0o700,
        );
        (
            Helper {
                _directory: directory,
                _execution_guard: execution_guard,
                path,
            },
            starts,
            pid,
        )
    }

    async fn wait_for_file(path: &Path) {
        timeout(Duration::from_secs(1), async {
            while !path.exists() {
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }

    fn recorded_pid(path: &Path) -> u32 {
        fs::read_to_string(path).unwrap().parse().unwrap()
    }

    fn process_exists(pid: u32) -> bool {
        PathBuf::from(format!("/proc/{pid}")).exists()
    }

    async fn served_response(launcher: &SocialUiLauncher) -> Vec<u8> {
        let cancellation = CancellationToken::new();
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let serve = launcher.serve(&mut server, &cancellation);
        let read = async {
            let mut response = Vec::new();
            client.read_to_end(&mut response).await.unwrap();
            response
        };
        let (result, response) = tokio::join!(serve, read);
        result.unwrap();
        response
    }

    #[tokio::test]
    async fn successful_overlay_writes_exactly_completed_status() {
        let helper = helper("exit 0");
        assert_eq!(
            served_response(&SocialUiLauncher::from_path(helper.path.clone())).await,
            [0]
        );
    }

    #[tokio::test]
    async fn overlay_exit_codes_and_signal_map_to_exact_protocol_statuses() {
        for (body, expected) in [
            ("exit 0", 0),
            ("exit 1", 1),
            ("exit 2", 2),
            ("exit 17", 2),
            ("kill -TERM $$", 2),
        ] {
            let helper = helper(body);
            assert_eq!(
                served_response(&SocialUiLauncher::from_path(helper.path.clone())).await,
                [expected],
                "helper body: {body}"
            );
        }
    }

    #[tokio::test]
    async fn missing_or_non_executable_overlay_maps_to_failure_without_panicking() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(
            served_response(&SocialUiLauncher::from_path(
                directory.path().join("missing-overlay")
            ))
            .await,
            [2]
        );

        let helper = helper_with_mode("exit 0", 0o600);
        assert_eq!(
            served_response(&SocialUiLauncher::from_path(helper.path.clone())).await,
            [2]
        );
    }

    #[test]
    fn sibling_discovery_uses_the_service_executable_directory() {
        assert_eq!(
            sibling_overlay_path(Path::new("/opt/xodus/bin/xodus-service")).unwrap(),
            Path::new("/opt/xodus/bin/xodus-overlay")
        );
    }

    #[test]
    fn explicit_non_utf8_override_wins_and_empty_override_falls_back() {
        let override_path = OsString::from_vec(b"/tmp/overlay-\xff".to_vec());
        assert_eq!(
            resolve_overlay_path(Some(override_path.clone()), || panic!(
                "explicit override must not inspect the current executable"
            ))
            .unwrap(),
            PathBuf::from(override_path)
        );
        assert_eq!(
            resolve_overlay_path(Some(OsString::new()), || Ok(PathBuf::from(
                "/opt/xodus/xodus-service"
            )))
            .unwrap(),
            Path::new("/opt/xodus/xodus-overlay")
        );
    }

    #[test]
    fn overlay_command_has_zero_arguments_and_no_explicit_environment() {
        let command = overlay_command(Path::new("/opt/xodus/xodus-overlay"));
        assert_eq!(command.get_program(), "/opt/xodus/xodus-overlay");
        assert_eq!(command.get_args().count(), 0);
        assert_eq!(command.get_envs().count(), 0);
    }

    #[tokio::test]
    async fn concurrent_request_is_immediately_busy_and_starts_only_one_child() {
        let (helper, starts, pid) = blocking_helper(1);
        let launcher = Arc::new(SocialUiLauncher::from_path(helper.path.clone()));
        let first_cancellation = CancellationToken::new();
        let first_task_cancellation = first_cancellation.clone();
        let first_launcher = Arc::clone(&launcher);
        let (mut first_client, mut first_server) = UnixStream::pair().unwrap();
        let first = tokio::spawn(async move {
            first_launcher
                .serve(&mut first_server, &first_task_cancellation)
                .await
        });
        wait_for_file(&pid).await;

        let second_cancellation = CancellationToken::new();
        let (mut second_client, mut second_server) = UnixStream::pair().unwrap();
        let second_launcher = Arc::clone(&launcher);
        let second = tokio::spawn(async move {
            second_launcher
                .serve(&mut second_server, &second_cancellation)
                .await
        });
        let mut second_response = Vec::new();
        let second_result = timeout(
            Duration::from_millis(250),
            second_client.read_to_end(&mut second_response),
        )
        .await;

        first_cancellation.cancel();
        let _ = first.await.unwrap();
        let second_serve = second.await.unwrap();
        let mut ignored = Vec::new();
        first_client.read_to_end(&mut ignored).await.unwrap();
        second_result
            .expect("busy response must not wait for the child")
            .unwrap();
        second_serve.unwrap();
        assert_eq!(second_response, [2]);
        assert_eq!(fs::read_to_string(starts).unwrap(), "x\n");
    }

    #[tokio::test]
    async fn cancellation_terminates_reaps_and_releases_the_single_instance_gate() {
        let (helper, _starts, pid_path) = blocking_helper(1);
        let helper_path = helper.path.clone();
        let launcher = Arc::new(SocialUiLauncher::from_path(helper_path.clone()));
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let task_launcher = Arc::clone(&launcher);
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let mut task =
            tokio::spawn(async move { task_launcher.serve(&mut server, &task_cancellation).await });
        wait_for_file(&pid_path).await;
        let pid = recorded_pid(&pid_path);

        cancellation.cancel();
        timeout(Duration::from_millis(250), launcher.wait_idle())
            .await
            .expect("service shutdown must wait for overlay cleanup");
        let prompt = timeout(Duration::from_millis(250), &mut task).await;
        if prompt.is_err() {
            let _ = task.await;
            panic!("cancellation must promptly join the child");
        }
        prompt.unwrap().unwrap().unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        assert_eq!(response, [2]);
        assert!(!process_exists(pid), "cancelled child must be reaped");

        write_helper_file(&helper_path, "exit 0", 0o700);
        assert_eq!(
            served_response(&launcher).await,
            [0],
            "gate must be released after cancellation cleanup"
        );
    }

    #[tokio::test]
    async fn peer_disconnect_terminates_and_reaps_the_child() {
        let (helper, _starts, pid_path) = blocking_helper(1);
        let launcher = SocialUiLauncher::from_path(helper.path.clone());
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let (client, mut server) = UnixStream::pair().unwrap();
        let mut task =
            tokio::spawn(async move { launcher.serve(&mut server, &task_cancellation).await });
        wait_for_file(&pid_path).await;
        let pid = recorded_pid(&pid_path);

        drop(client);
        let prompt = timeout(Duration::from_millis(250), &mut task).await;
        if prompt.is_err() {
            let _ = task.await;
            panic!("peer disconnect must promptly join the child");
        }
        prompt
            .unwrap()
            .unwrap()
            .expect_err("status write to a disconnected peer must fail");
        assert!(!process_exists(pid), "disconnected child must be reaped");
    }
}
