//! OSS metadata, cancellable downloads and the user-confirmed installation boundary.
use crate::{AppState, lifecycle, poisoned};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use domain::{Error, Result, UpdatePreferences};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};
use tauri::{Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::io::AsyncWriteExt;

const MAX_PACKAGE: u64 = 1024 * 1024 * 1024;
const DAY: i64 = 24 * 60 * 60;
const HOST: &str = "tct12.oss-cn-beijing.aliyuncs.com";
const PREFIX: &str = "/12box/sixa/updates/releases/";

#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub revision: u64,
    pub current_version: String,
    pub version: Option<String>,
    pub notes: Option<String>,
    pub phase: String,
    pub automatic: bool,
    pub last_check: i64,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub bytes_per_second: u64,
    pub eta_seconds: Option<u64>,
    pub error: Option<String>,
}
struct Inner {
    status: Status,
    update: Option<Update>,
    package: Option<PathBuf>,
    cancel: Option<tokio::sync::watch::Sender<bool>>,
}
pub struct Updates {
    inner: Mutex<Inner>,
    gate: tokio::sync::Mutex<()>,
    store: Mutex<storage::Store>,
    cache: PathBuf,
}
#[derive(Serialize, Deserialize)]
struct CachedUpdate {
    version: String,
    signature: String,
    target: String,
}
impl Updates {
    pub fn new(store: storage::Store, root: &Path) -> Result<Self> {
        let prefs = store.update_preferences()?;
        let cache = root.join("cache/app-updates");
        if let Ok(entries) = std::fs::read_dir(&cache) {
            for entry in entries.flatten() {
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("sixa-update-")
                    && entry.file_type().is_ok_and(|t| t.is_file())
                    && entry
                        .metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|time| time.elapsed().ok())
                        .is_some_and(|age| age.as_secs() > DAY as u64)
                {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        Ok(Self {
            inner: Mutex::new(Inner {
                status: Status {
                    revision: 0,
                    current_version: env!("CARGO_PKG_VERSION").into(),
                    version: None,
                    notes: None,
                    phase: "idle".into(),
                    automatic: prefs.automatic,
                    last_check: prefs.last_check,
                    downloaded: 0,
                    total: None,
                    bytes_per_second: 0,
                    eta_seconds: None,
                    error: None,
                },
                update: None,
                package: None,
                cancel: None,
            }),
            gate: tokio::sync::Mutex::new(()),
            store: Mutex::new(store),
            cache,
        })
    }
    fn change(&self, app: &tauri::AppHandle, f: impl FnOnce(&mut Status)) -> Result<Status> {
        let mut inner = self.inner.lock().map_err(poisoned)?;
        f(&mut inner.status);
        inner.status.revision += 1;
        let status = inner.status.clone();
        drop(inner);
        let _ = app.emit("app-update-progress", &status);
        Ok(status)
    }
    pub fn cancel(&self) {
        if let Ok(inner) = self.inner.lock()
            && let Some(cancel) = &inner.cancel
        {
            let _ = cancel.send(true);
        }
    }
    pub fn downloading(&self) -> bool {
        self.inner.lock().is_ok_and(|inner| inner.cancel.is_some())
    }
}
fn error(value: impl std::fmt::Display) -> Error {
    let raw = value.to_string();
    let lower = raw.to_lowercase();
    let contains = |words: &[&str]| words.iter().any(|word| lower.contains(word));
    let advice = if contains(&["signature", "minisign", "pubkey", "public key"]) {
        "更新包签名校验失败，请重新下载；若仍失败，请联系维护人员"
    } else if contains(&[
        "permission",
        "denied",
        "access is",
        "os error 5",
        "os error 13",
    ]) {
        "无法写入安装目录，请检查目录权限或安全软件拦截后重试"
    } else if contains(&["no space", "disk full", "os error 112", "os error 28"]) {
        "磁盘空间不足，请释放空间后重试"
    } else if contains(&["target", "platform"]) {
        "没有适用于当前系统或芯片的更新包，请联系维护人员"
    } else if contains(&[
        "network",
        "request",
        "status",
        "timeout",
        "timed out",
        "connection",
        "dns",
        "tls",
        "redirect",
    ]) {
        "暂时无法获取更新，请检查网络后重试"
    } else {
        "应用更新未完成，请重试；若仍失败，请联系维护人员"
    };
    // Keep bounded diagnostics instead of masking every failure as a network error.
    let detail: String = raw.chars().take(360).collect();
    Error::Io(format!("{advice}。详细信息：{detail}"))
}
fn snapshot(state: &Updates) -> Result<Status> {
    Ok(state.inner.lock().map_err(poisoned)?.status.clone())
}
fn fail(state: &Updates, app: &tauri::AppHandle, e: Error) -> Error {
    let _ = state.change(app, |s| {
        s.phase = "failed".into();
        s.error = Some(e.to_string());
        s.bytes_per_second = 0;
        s.eta_seconds = None;
    });
    e
}

#[tauri::command]
pub fn get_app_update_status(state: tauri::State<'_, Updates>) -> Result<Status> {
    snapshot(&state)
}

#[tauri::command]
pub fn set_app_update_preferences(app: tauri::AppHandle, automatic: bool) -> Result<Status> {
    let state = app.state::<Updates>();
    let mut inner = state.inner.lock().map_err(poisoned)?;
    state
        .store
        .lock()
        .map_err(poisoned)?
        .save_update_preferences(&UpdatePreferences {
            automatic,
            last_check: inner.status.last_check,
        })?;
    inner.status.automatic = automatic;
    inner.status.revision += 1;
    let status = inner.status.clone();
    drop(inner);
    let _ = app.emit("app-update-progress", &status);
    Ok(status)
}

pub fn start(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        loop {
            let status = snapshot(&app.state::<Updates>());
            if status.is_ok_and(|s| {
                s.automatic
                    && (automatic_due(s.last_check, chrono::Utc::now().timestamp())
                        || app.state::<Updates>().cache.join("ready.json").exists())
            }) {
                let _ = check_app_update(app.clone()).await;
            }
            // A sleeping timer, not UI polling. Failed checks are throttled as well.
            tokio::time::sleep(Duration::from_secs(DAY as u64)).await;
        }
    });
}
fn automatic_due(last: i64, now: i64) -> bool {
    last == 0 || now - last >= DAY || last > now + DAY
}
fn validate_url(url: &reqwest::Url, version: &str) -> Result<()> {
    let expected = format!("{PREFIX}{version}/");
    if url.scheme() != "https"
        || url.host_str() != Some(HOST)
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.path().starts_with(&expected)
        || url.path().contains('%')
        || url.path().ends_with('/')
    {
        return Err(Error::Invalid(
            "更新包地址不属于受信任的 OSS 版本目录".into(),
        ));
    }
    Ok(())
}

#[tauri::command]
pub async fn check_app_update(app: tauri::AppHandle) -> Result<Status> {
    let state = app.state::<Updates>();
    let Ok(_gate) = state.gate.try_lock() else {
        return snapshot(&state);
    };
    let now = chrono::Utc::now().timestamp();
    {
        let mut inner = state.inner.lock().map_err(poisoned)?;
        state
            .store
            .lock()
            .map_err(poisoned)?
            .save_update_preferences(&UpdatePreferences {
                automatic: inner.status.automatic,
                last_check: now,
            })?;
        inner.status.last_check = now;
        inner.update = None;
    }
    state.change(&app, |s| {
        s.phase = "checking".into();
        s.error = None;
        s.version = None;
        s.notes = None;
    })?;
    let result = async {
        // Preserve the live UI if Windows cannot start the installer. The
        // installation boundary already flushed writes and froze admissions.
        let update = app
            .updater_builder()
            .on_before_exit(|| {})
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(error)?
            .check()
            .await
            .map_err(error)?;
        if let Some(update) = &update {
            let version = semver::Version::parse(&update.version).map_err(error)?;
            if !version.pre.is_empty()
                || version <= semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap()
            {
                return Err(Error::Invalid("更新清单不是较新的稳定版本".into()));
            }
            validate_url(&update.download_url, &update.version)?;
        }
        let version = update.as_ref().map(|u| u.version.clone());
        let notes = update.as_ref().and_then(|u| u.body.clone());
        let cached = if let Some(update) = &update {
            let cache = state.cache.clone();
            let receipt = CachedUpdate {
                version: update.version.clone(),
                signature: update.signature.clone(),
                target: update.target.clone(),
            };
            tauri::async_runtime::spawn_blocking(move || cached_package(&cache, &receipt))
                .await
                .map_err(error)?
        } else {
            discard_cache(&state.cache);
            None
        };
        let ready = cached.is_some();
        {
            let mut inner = state.inner.lock().map_err(poisoned)?;
            inner.update = update;
            inner.package = cached;
        }
        state.change(&app, |s| {
            s.phase = if ready {
                "ready"
            } else if version.is_some() {
                "available"
            } else {
                "current"
            }
            .into();
            s.version = version;
            s.notes = notes;
        })
    }
    .await;
    result.map_err(|e| fail(&state, &app, e))
}

fn discard_cache(cache: &Path) {
    let _ = std::fs::remove_file(cache.join("ready.json"));
    let _ = std::fs::remove_file(cache.join("ready.package"));
}
fn cached_package(cache: &Path, expected: &CachedUpdate) -> Option<PathBuf> {
    let result = (|| {
        let receipt_path = cache.join("ready.json");
        if std::fs::metadata(&receipt_path).ok()?.len() > 8192 {
            return None;
        }
        let cached: CachedUpdate =
            serde_json::from_slice(&std::fs::read(receipt_path).ok()?).ok()?;
        if cached.version != expected.version
            || cached.signature != expected.signature
            || cached.target != expected.target
        {
            return None;
        }
        let path = cache.join("ready.package");
        if std::fs::metadata(&path).ok()?.len() > MAX_PACKAGE {
            return None;
        }
        verify(
            &std::fs::read(&path).ok()?,
            &cached.signature,
            &public_key(),
        )
        .ok()?;
        Some(path)
    })();
    if result.is_none() {
        discard_cache(cache);
    }
    result
}
fn cache_package(cache: &Path, path: tempfile::TempPath, update: &Update) -> Result<PathBuf> {
    let target = cache.join("ready.package");
    path.persist(&target).map_err(error)?;
    let receipt = CachedUpdate {
        version: update.version.clone(),
        signature: update.signature.clone(),
        target: update.target.clone(),
    };
    let mut file = tempfile::Builder::new()
        .prefix("sixa-update-")
        .tempfile_in(cache)
        .map_err(error)?;
    serde_json::to_writer(&mut file, &receipt).map_err(error)?;
    file.as_file().sync_all().map_err(error)?;
    file.persist(cache.join("ready.json")).map_err(error)?;
    Ok(target)
}

fn verify(bytes: &[u8], signature: &str, pubkey: &str) -> Result<()> {
    let key = STANDARD.decode(pubkey.trim()).map_err(error)?;
    let sig = STANDARD.decode(signature.trim()).map_err(error)?;
    let key = minisign_verify::PublicKey::decode(std::str::from_utf8(&key).map_err(error)?)
        .map_err(error)?;
    let sig = minisign_verify::Signature::decode(std::str::from_utf8(&sig).map_err(error)?)
        .map_err(error)?;
    key.verify(bytes, &sig, true)
        .map_err(|_| Error::Invalid("更新包签名校验失败，文件可能不完整，请重新下载".into()))
}
fn public_key() -> String {
    let config: serde_json::Value =
        serde_json::from_str(include_str!("../tauri.conf.json")).expect("bundled config");
    config["plugins"]["updater"]["pubkey"]
        .as_str()
        .expect("updater public key")
        .into()
}

#[tauri::command]
pub async fn download_app_update(app: tauri::AppHandle) -> Result<Status> {
    let state = app.state::<Updates>();
    let _gate = state
        .gate
        .try_lock()
        .map_err(|_| Error::State("正在执行更新操作，请稍候".into()))?;
    let activity = app.state::<AppState>().desktop.admit()?;
    let update = state
        .inner
        .lock()
        .map_err(poisoned)?
        .update
        .clone()
        .ok_or_else(|| Error::State("请先检查更新".into()))?;
    if state.inner.lock().map_err(poisoned)?.package.is_some() {
        return snapshot(&state);
    }
    let (sender, mut cancel) = tokio::sync::watch::channel(false);
    state.inner.lock().map_err(poisoned)?.cancel = Some(sender);
    state.change(&app, |s| {
        s.phase = "downloading".into();
        s.error = None;
        s.downloaded = 0;
        s.total = None;
        s.bytes_per_second = 0;
        s.eta_seconds = None;
    })?;
    let result = tokio::select! {
        result = download(&app, &state, &update) => result.map(Some),
        _ = cancel.changed() => Ok(None),
    };
    state.inner.lock().map_err(poisoned)?.cancel = None;
    let result = match result {
        Ok(Some(path)) => {
            let path =
                cache_package(&state.cache, path, &update).map_err(|e| fail(&state, &app, e))?;
            state.inner.lock().map_err(poisoned)?.package = Some(path);
            state.change(&app, |s| {
                s.phase = "ready".into();
                s.bytes_per_second = 0;
                s.eta_seconds = None;
            })
        }
        Ok(None) => state.change(&app, |s| {
            s.phase = "available".into();
            s.bytes_per_second = 0;
            s.eta_seconds = None;
        }),
        Err(e) => Err(fail(&state, &app, e)),
    };
    drop(activity);
    result
}
async fn download(
    app: &tauri::AppHandle,
    state: &Updates,
    update: &Update,
) -> Result<tempfile::TempPath> {
    validate_url(&update.download_url, &update.version)?;
    let client = reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30 * 60))
        .build()
        .map_err(error)?;
    receive_package(
        &client,
        update.download_url.clone(),
        &state.cache,
        update.signature.clone(),
        public_key(),
        |downloaded, total, speed, verifying| {
            state
                .change(app, |s| {
                    s.downloaded = downloaded;
                    s.total = total;
                    s.bytes_per_second = speed;
                    s.eta_seconds = total
                        .and_then(|n| (speed > 0).then(|| n.saturating_sub(downloaded) / speed));
                    if verifying {
                        s.phase = "verifying".into();
                    }
                })
                .map(|_| ())
        },
    )
    .await
}

async fn receive_package(
    client: &reqwest::Client,
    url: reqwest::Url,
    cache: &Path,
    signature: String,
    key: String,
    mut progress: impl FnMut(u64, Option<u64>, u64, bool) -> Result<()>,
) -> Result<tempfile::TempPath> {
    tokio::fs::create_dir_all(cache).await.map_err(error)?;
    let (file, path) = tempfile::Builder::new()
        .prefix("sixa-update-")
        .tempfile_in(cache)
        .map_err(error)?
        .into_parts();
    let mut file = tokio::fs::File::from_std(file);
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(error)?
        .error_for_status()
        .map_err(error)?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(Error::Invalid("更新包下载响应异常，请重试".into()));
    }
    let total = response.content_length();
    if total.is_some_and(|n| n == 0 || n > MAX_PACKAGE) {
        return Err(Error::Invalid("更新包大小异常".into()));
    }
    let mut downloaded = 0_u64;
    let mut tick = Instant::now();
    let mut previous = 0;
    progress(0, total, 0, false)?;
    loop {
        let chunk = tokio::time::timeout(Duration::from_secs(30), response.chunk())
            .await
            .map_err(|_| Error::Io("下载超过 30 秒没有响应，请检查网络后重试".into()))?
            .map_err(error)?;
        let Some(chunk) = chunk else { break };
        downloaded += chunk.len() as u64;
        if downloaded > MAX_PACKAGE {
            return Err(Error::Invalid("更新包超过大小限制".into()));
        }
        file.write_all(&chunk).await.map_err(error)?;
        if tick.elapsed() >= Duration::from_millis(250) {
            let speed = ((downloaded - previous) as f64 / tick.elapsed().as_secs_f64()) as u64;
            progress(downloaded, total, speed, false)?;
            previous = downloaded;
            tick = Instant::now();
        }
    }
    file.flush().await.map_err(error)?;
    file.sync_all().await.map_err(error)?;
    drop(file);
    if downloaded == 0 || total.is_some_and(|n| n != downloaded) {
        return Err(Error::Invalid("更新包下载不完整，请重试".into()));
    }
    progress(downloaded, total, 0, true)?;
    // The worker owns the tempfile until verification ends, even if cancellation wins.
    tauri::async_runtime::spawn_blocking(move || {
        let bytes = std::fs::read(&path).map_err(error)?;
        verify(&bytes, &signature, &key)?;
        Ok(path)
    })
    .await
    .map_err(error)?
}

#[tauri::command]
pub fn cancel_app_update(state: tauri::State<'_, Updates>) {
    state.cancel();
}

#[tauri::command]
pub async fn install_app_update(app: tauri::AppHandle) -> Result<Status> {
    let state = app.state::<Updates>();
    let _gate = state
        .gate
        .try_lock()
        .map_err(|_| Error::State("正在执行更新操作，请稍候".into()))?;
    let (update, path) = {
        let inner = state.inner.lock().map_err(poisoned)?;
        (
            inner
                .update
                .clone()
                .ok_or_else(|| Error::State("请先检查更新".into()))?,
            inner
                .package
                .as_ref()
                .map(|p| p.to_path_buf())
                .ok_or_else(|| Error::State("请先下载更新包".into()))?,
        )
    };
    #[cfg(target_os = "macos")]
    {
        let executable = std::env::current_exe().map_err(error)?;
        if executable.starts_with("/Volumes")
            || executable.to_string_lossy().contains("/AppTranslocation/")
        {
            return Err(Error::State(
                "请先退出私匣，将应用拖入“应用程序”文件夹并从那里打开，再安装更新".into(),
            ));
        }
    }
    // The WebView flushes its review queue before invoking this command. Native
    // admissions close atomically, so MCP cannot race the last idle check.
    lifecycle::begin_update(&app)?;
    state.change(&app, |s| {
        s.phase = "installing".into();
        s.error = None;
    })?;
    let root = app.state::<AppState>().root.clone();
    let target = update.version.clone();
    let result = async {
        let signature = update.signature.clone();
        let bytes = tauri::async_runtime::spawn_blocking(move || {
            if std::fs::metadata(&path).map_err(error)?.len() > MAX_PACKAGE {
                return Err(Error::Invalid("更新缓存大小异常，请重新下载".into()));
            }
            let bytes = std::fs::read(path).map_err(error)?;
            verify(&bytes, &signature, &public_key())?;
            Ok::<_, Error>(bytes)
        })
        .await
        .map_err(error)??;
        integration_protocol::updating::begin(&root, &target).map_err(error)?;
        #[cfg(windows)]
        release_windows_mcp().await?;
        tauri::async_runtime::spawn_blocking(move || update.install(&bytes).map_err(error))
            .await
            .map_err(error)?
    }
    .await;
    match result {
        Ok(()) => {
            lifecycle::finish_update(&app);
            app.restart();
        }
        Err(e) => {
            integration_protocol::updating::clear(&app.state::<AppState>().root);
            lifecycle::abort_update(&app);
            // Keep a verified download after a transient permission/MCP failure.
            // Corrupt or deleted packages must be downloaded again.
            if matches!(e, Error::Invalid(_)) || !state.cache.join("ready.package").is_file() {
                state.inner.lock().map_err(poisoned)?.package = None;
                discard_cache(&state.cache);
                Err(fail(&state, &app, e))
            } else {
                let _ = state.change(&app, |s| {
                    s.phase = "ready".into();
                    s.error = Some(e.to_string());
                });
                Err(e)
            }
        }
    }
}

#[cfg(windows)]
async fn release_windows_mcp() -> Result<()> {
    use std::io::Write;
    let executable = std::env::current_exe().map_err(error)?;
    let directory = executable
        .parent()
        .ok_or_else(|| Error::State("无法确定安装目录".into()))?;
    let mut script = tempfile::Builder::new()
        .prefix("sixa-update-mcp-")
        .suffix(".ps1")
        .tempfile()
        .map_err(error)?;
    script
        .write_all(include_bytes!("../installer/stop-mcp.ps1"))
        .map_err(error)?;
    script.flush().map_err(error)?;
    let powershell = std::env::var_os("SYSTEMROOT")
        .map(PathBuf::from)
        .ok_or_else(|| Error::State("无法找到 Windows 系统目录".into()))?
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let output = tokio::time::timeout(
        Duration::from_secs(25),
        tokio::process::Command::new(powershell)
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(script.path())
            .arg("-InstallDir")
            .arg(directory)
            .creation_flags(0x08000000)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| Error::State("释放 MCP 连接超时。请暂时停用 AI 客户端中的私匣连接后重试".into()))?
    .map_err(error)?;
    if !output.status.success() {
        return Err(Error::State("MCP 程序仍被占用或安装目录不可写。请暂时停用 AI 客户端中的私匣连接并检查目录权限，再重试更新".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_untrusted_sources_and_wrong_version_paths() {
        let good = format!("https://{HOST}{PREFIX}1.0.9/Sixa.exe");
        assert!(validate_url(&good.parse().unwrap(), "1.0.9").is_ok());
        for bad in [
            good.replace("https:", "http:"),
            good.replace(HOST, "evil.invalid"),
            good.replace("1.0.9", "1.0.8"),
            format!("{good}?token=x"),
            good.replace("Sixa.exe", "%2e%2e/evil.exe"),
        ] {
            assert!(
                validate_url(&bad.parse().unwrap(), "1.0.9").is_err(),
                "{bad}"
            );
        }
    }
    #[test]
    fn checks_are_throttled_and_clock_changes_recover() {
        assert!(automatic_due(0, DAY));
        assert!(!automatic_due(100, 101));
        assert!(automatic_due(100, 100 + DAY));
        assert!(automatic_due(3 * DAY, DAY));
    }
    #[test]
    fn malformed_signature_never_reaches_installer() {
        assert!(verify(b"package", "invalid", &public_key()).is_err());
    }
    #[test]
    fn update_errors_keep_actionable_advice_and_bounded_diagnostics() {
        for (raw, advice) in [
            ("Minisign signature mismatch", "签名校验失败"),
            ("HTTP status 404", "检查网络"),
            ("TargetNotFound: darwin-aarch64", "系统或芯片"),
            ("Access is denied (os error 5)", "安装目录"),
            ("No space left on device", "磁盘空间不足"),
        ] {
            let message = error(raw).to_string();
            assert!(message.contains(advice));
            assert!(message.contains(raw));
        }
        assert!(error("长".repeat(1000)).to_string().chars().count() < 450);
    }
    #[test]
    fn signed_package_and_cached_file_tampering_are_verified() {
        let bytes = include_bytes!("fixtures/updater/package.txt");
        let signature = include_str!("fixtures/updater/package.txt.sig");
        let key = include_str!("fixtures/updater/public.key");
        assert!(verify(bytes, signature, key).is_ok());
        assert!(verify(b"modified cached package", signature, key).is_err());
        assert!(verify(bytes, signature, &public_key()).is_err());
    }

    async fn server(response: Vec<u8>, stall: bool) -> (reqwest::Url, tokio::task::JoinHandle<()>) {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/fixture", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let worker = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let read = stream.read(&mut request).await.unwrap();
            assert!(read > 0);
            stream.write_all(&response).await.unwrap();
            if stall {
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        });
        (url, worker)
    }
    #[tokio::test]
    async fn download_verifies_payload_and_unknown_lengths_without_network_services() {
        let body = include_bytes!("fixtures/updater/package.txt");
        for known in [true, false] {
            let header = if known {
                format!("Content-Length: {}\r\n", body.len())
            } else {
                String::new()
            };
            let mut response =
                format!("HTTP/1.1 200 OK\r\n{header}Connection: close\r\n\r\n").into_bytes();
            response.extend_from_slice(body);
            let (url, worker) = server(response, false).await;
            let temp = tempfile::tempdir().unwrap();
            let mut verified = false;
            let path = receive_package(
                &reqwest::Client::new(),
                url,
                temp.path(),
                include_str!("fixtures/updater/package.txt.sig").into(),
                include_str!("fixtures/updater/public.key").into(),
                |count, total, _, verifying| {
                    assert_eq!(total, known.then_some(body.len() as u64));
                    if verifying {
                        assert_eq!(count, body.len() as u64);
                        verified = true;
                    }
                    Ok(())
                },
            )
            .await
            .unwrap();
            assert!(verified);
            assert_eq!(std::fs::read(&path).unwrap(), body);
            drop(path);
            assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
            worker.await.unwrap();
        }
    }
    #[tokio::test]
    async fn broken_and_cancelled_downloads_do_not_leave_installable_packages() {
        for response in [
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nbroken".to_vec(),
        ] {
            let (url, worker) = server(response, false).await;
            let temp = tempfile::tempdir().unwrap();
            assert!(
                receive_package(
                    &reqwest::Client::new(),
                    url,
                    temp.path(),
                    String::new(),
                    public_key(),
                    |_, _, _, _| Ok(())
                )
                .await
                .is_err()
            );
            assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
            worker.await.unwrap();
        }
        let (url, worker) = server(
            b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n".to_vec(),
            true,
        )
        .await;
        let temp = tempfile::tempdir().unwrap();
        let client = reqwest::Client::new();
        let download = receive_package(
            &client,
            url,
            temp.path(),
            String::new(),
            public_key(),
            |_, _, _, _| Ok(()),
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(100), download)
                .await
                .is_err()
        );
        worker.abort();
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn macos_native_updater_replaces_bundle_and_new_executable_runs() {
        use std::os::unix::fs::PermissionsExt;
        let archive = include_bytes!("fixtures/updater/mac-fixture.app.tar.gz");
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            archive.len()
        )
        .into_bytes();
        response.extend_from_slice(archive);
        let (download_url, download_server) = server(response, false).await;
        let metadata = serde_json::to_vec(&serde_json::json!({
            "version": "9.0.0", "url": download_url.as_str(),
            "signature": include_str!("fixtures/updater/mac-fixture.app.tar.gz.sig").trim(),
            "notes": "offline install acceptance"
        }))
        .unwrap();
        let mut response = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n", metadata.len()).into_bytes();
        response.extend(metadata);
        let (endpoint, metadata_server) = server(response, false).await;
        let key = include_str!("fixtures/updater/mac-fixture.pub").trim();
        let mut context = tauri::test::mock_context(tauri::test::noop_assets());
        context
            .config_mut()
            .plugins
            .0
            .insert("updater".into(), serde_json::json!({ "pubkey": key }));
        let app = tauri::test::mock_builder()
            .plugin(tauri_plugin_updater::Builder::new().build())
            .build(context)
            .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("中文 Applications/SixaFixture.app");
        let executable = bundle.join("Contents/MacOS/sixa-fixture");
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::write(&executable, "old executable").unwrap();
        std::fs::write(bundle.join("obsolete-resource"), "must disappear").unwrap();
        let update = app
            .updater_builder()
            .endpoints(vec![endpoint])
            .unwrap()
            .executable_path(&executable)
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
            .check()
            .await
            .unwrap()
            .unwrap();
        let bytes = update.download(|_, _| {}, || {}).await.unwrap();
        update.install(&bytes).unwrap();
        assert!(!bundle.join("obsolete-resource").exists());
        assert_eq!(
            std::fs::metadata(&executable).unwrap().permissions().mode() & 0o111,
            0o111
        );
        let output = std::process::Command::new(&executable).output().unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim(),
            "SIXA_UPDATED_9.0.0"
        );
        assert_eq!(
            std::fs::read_to_string(bundle.join("Contents/Resources/version.txt")).unwrap(),
            "9.0.0"
        );
        metadata_server.await.unwrap();
        download_server.await.unwrap();
    }
}
