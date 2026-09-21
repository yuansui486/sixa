use model_manager::{Cancellation, download_and_install, fetch_catalog};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = reqwest::Client::builder()
        .https_only(true)
        .user_agent("Sixa/1.0.4")
        .build()?;
    let catalog = fetch_catalog(&client)
        .await
        .map_err(|error| format!("fetch catalog: {error}"))?;
    let package = catalog
        .packages
        .iter()
        .find(|package| package.id == "ppocrv4-mobile-v1")
        .ok_or("catalog is missing ppocrv4-mobile-v1")?;
    let root = tempfile::tempdir().map_err(|error| format!("create temp root: {error}"))?;

    let first_cancel = Cancellation::default();
    let callback_cancel = first_cancel.clone();
    let interrupted = download_and_install(
        &client,
        package,
        root.path(),
        &first_cancel,
        move |progress| {
            if progress.stage == "downloading" && progress.current >= 1024 * 1024 {
                callback_cancel.cancel();
            }
        },
    )
    .await;
    let interrupted_error = interrupted
        .err()
        .ok_or("the interrupted download unexpectedly completed")?;
    println!("initial download stopped: {interrupted_error}");
    let part = root.path().join("cache/ppocrv4-mobile-v1.zip.part");
    let partial_bytes = std::fs::metadata(&part)
        .map_err(|error| format!("inspect partial file {}: {error}", part.display()))?
        .len();
    if partial_bytes == 0 || partial_bytes >= package.size {
        return Err(format!("invalid partial download size: {partial_bytes}").into());
    }

    let resumed_from = std::sync::Mutex::new(None::<u64>);
    let installed = download_and_install(
        &client,
        package,
        root.path(),
        &Cancellation::default(),
        |progress| {
            if progress.stage == "downloading" {
                let mut value = resumed_from.lock().unwrap();
                if value.is_none() {
                    *value = Some(progress.current);
                }
            }
        },
    )
    .await
    .map_err(|error| format!("resume and install: {error}"))?;
    let first_progress = resumed_from
        .into_inner()?
        .ok_or("missing resume progress")?;
    if first_progress <= partial_bytes {
        return Err("download did not advance from the partial offset".into());
    }
    recognition::models::verify_with_required(
        &installed,
        &[
            "det.onnx",
            "cls.onnx",
            "rec.onnx",
            "dict.txt",
            "ocr-config.json",
            "onnxruntime.dll",
        ],
    )
    .map_err(|error| format!("verify installed model: {error}"))?;
    println!(
        "{}",
        serde_json::json!({
            "catalog_revision": catalog.revision,
            "package": package.id,
            "partial_bytes": partial_bytes,
            "first_resumed_progress": first_progress,
            "installed": true
        })
    );
    Ok(())
}
