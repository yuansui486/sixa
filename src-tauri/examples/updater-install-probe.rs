//! Test-only native updater launcher. Never bundled with the desktop application.
#[cfg(windows)]
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::{path::PathBuf, time::Duration};
    use tauri_plugin_updater::UpdaterExt;
    let root = PathBuf::from(std::env::var("SIXA_UPDATER_PROBE_ROOT")?);
    let canonical_root = root.canonicalize()?;
    let temp = std::env::temp_dir().canonicalize()?;
    if !canonical_root.starts_with(temp)
        || !root
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("sixa-installer-test-")
    {
        return Err("Probe requires an isolated installer test directory".into());
    }
    let install = root.join("native update 中文");
    let url: reqwest::Url = std::env::var("SIXA_UPDATER_PROBE_URL")?.parse()?;
    if url.scheme() != "http" || url.host_str() != Some("127.0.0.1") {
        return Err("Probe requires a loopback fixture server".into());
    }
    let key = std::fs::read_to_string(root.join("updater-test-key.pub"))?;
    let mut context = tauri::test::mock_context(tauri::test::noop_assets());
    context.config_mut().plugins.0.insert(
        "updater".into(),
        serde_json::json!({"pubkey":key.trim(), "windows":{"installMode":"passive"}}),
    );
    let app = tauri::test::mock_builder()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .build(context)?;
    let update = app
        .updater_builder()
        .endpoints(vec![url])?
        .executable_path(install.join("sixa-installer-regression.exe"))
        .installer_arg(format!("/D={}", install.display()))
        .on_before_exit(|| {})
        .restart_after_install(true)
        .timeout(Duration::from_secs(15))
        .build()?
        .check()
        .await?
        .ok_or("Expected fixture update")?;
    let bytes = update.download(|_, _| {}, || {}).await?;
    // Official Windows updater starts NSIS and exits this helper. The parent
    // waits for a marker written by the newly installed executable after /R.
    update.install(bytes)?;
    Err("Windows updater unexpectedly returned without exiting".into())
}

#[cfg(not(windows))]
fn main() {}
