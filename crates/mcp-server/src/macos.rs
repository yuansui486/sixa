use crate::{ErrorCode, IntegrationError, transport_error};
use integration_protocol::macos::{connect, try_lock};
use std::{
    io,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{net::UnixStream, sync::Mutex, time::Instant};

const START_TIMEOUT: Duration = Duration::from_secs(15);
const RETRY_COOLDOWN: Duration = Duration::from_secs(30);
static LAST_LAUNCH: Mutex<Option<Instant>> = Mutex::const_new(None);

fn unavailable(message: impl Into<String>) -> IntegrationError {
    IntegrationError::new(ErrorCode::AppNotRunning, message)
}

fn absent(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}

fn app_bundle(executable: &Path) -> Option<PathBuf> {
    let macos = executable.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension()? == "app"
        && contents.join("Info.plist").is_file())
    .then(|| bundle.to_owned())
}

pub async fn connect_or_launch(path: &Path) -> Result<UnixStream, IntegrationError> {
    tokio::time::timeout(START_TIMEOUT, async {
        match connect(path).await {
            Ok(stream) => return Ok(stream),
            Err(error) if absent(&error) => (),
            Err(error) => return Err(transport_error(error)),
        }
        let mut last = LAST_LAUNCH.lock().await;
        let mut launch_lock = None;
        loop {
            match connect(path).await {
                Ok(stream) => return Ok(stream),
                Err(error) if absent(&error) => (),
                Err(error) => return Err(transport_error(error)),
            }
            if launch_lock.is_none() {
                match try_lock(path, "launch.lock") {
                    Ok(mut lock) => {
                        if last.is_some_and(|time| time.elapsed() < RETRY_COOLDOWN)
                            || !lock.begin_launch(RETRY_COOLDOWN).map_err(transport_error)? {
                            return Err(unavailable("私匣尚未完成启动。请检查桌面窗口并完成登录，30 秒后重新检查状态"));
                        }
                        let executable = std::env::current_exe().map_err(transport_error)?;
                        let app = app_bundle(&executable).ok_or_else(|| unavailable(
                            "MCP 程序不在私匣应用包内，无法自动启动。请将私匣拖入应用程序目录，打开后重新复制 MCP 配置"
                        ))?;
                        *last = Some(Instant::now());
                        launch_lock = Some(lock);
                        let status = tokio::process::Command::new("/usr/bin/open")
                            .arg("-a").arg(app)
                            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
                            .kill_on_drop(true).status().await
                            .map_err(|error| unavailable(format!("无法自动打开私匣：{error}。请手动打开应用后重试")))?;
                        if !status.success() {
                            return Err(unavailable("macOS 未能打开私匣。请手动打开应用，检查系统提示后重新检查状态"));
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => (),
                    Err(error) => return Err(transport_error(error)),
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }).await.map_err(|_| unavailable(
        "已等待私匣启动 15 秒，但本机通信尚未就绪。请查看私匣窗口和系统提示，完成登录后重新检查状态"
    ))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_or_unbundled_executable_never_launches_an_unrelated_app() {
        assert!(app_bundle(Path::new("/usr/local/bin/sixa-mcp")).is_none());
        assert!(
            app_bundle(Path::new(
                "/Applications/Missing.app/Contents/MacOS/sixa-mcp"
            ))
            .is_none()
        );
    }

    #[test]
    fn only_missing_endpoints_trigger_launch() {
        assert!(absent(&io::ErrorKind::NotFound.into()));
        assert!(absent(&io::ErrorKind::ConnectionRefused.into()));
        assert!(!absent(&io::ErrorKind::PermissionDenied.into()));
        assert!(!absent(&io::ErrorKind::InvalidData.into()));
    }
}
