use model_manager::{Cancellation, download_and_install, fetch_catalog};
use recognition::ocr::{Ocr, OcrRun, PpOcr};
use recognition::{Ner, ner::Raner};
use std::path::Path;

#[cfg(target_os = "macos")]
fn install_test_runtime(model_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let source = std::env::var_os("SIXA_TEST_ONNX_RUNTIME")
        .ok_or("SIXA_TEST_ONNX_RUNTIME is required on macOS")?;
    std::fs::copy(source, model_dir.join("libonnxruntime.dylib"))?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn install_test_runtime(_model_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = reqwest::Client::builder()
        .https_only(true)
        .user_agent("Sixa/1.0.8")
        .build()?;
    let catalog = fetch_catalog(&client)
        .await
        .map_err(|error| format!("fetch catalog: {error}"))?;
    for candidate in &catalog.packages {
        let source = candidate
            .sources
            .first()
            .ok_or_else(|| format!("{} has no download source", candidate.id))?;
        let end = candidate.size.saturating_sub(1).min(1024 * 1024 - 1);
        let response = client
            .get(&source.url)
            .header(reqwest::header::RANGE, format!("bytes=0-{end}"))
            .send()
            .await
            .map_err(|error| format!("range request {}: {error}", candidate.id))?;
        if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            return Err(format!(
                "{} range request returned HTTP {}",
                candidate.id,
                response.status()
            )
            .into());
        }
        let expected_range = format!("bytes 0-{end}/{}", candidate.size);
        let actual_range = response
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if actual_range != expected_range {
            return Err(format!(
                "{} returned invalid Content-Range: {actual_range}",
                candidate.id
            )
            .into());
        }
        let body = response
            .bytes()
            .await
            .map_err(|error| format!("read range {}: {error}", candidate.id))?;
        if body.len() as u64 != end + 1 {
            return Err(format!("{} returned an incomplete range", candidate.id).into());
        }
        println!(
            "range ok: {} via {} ({} bytes)",
            candidate.id,
            source.label,
            body.len()
        );
    }
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
    recognition::models::verify_with_required(&installed, recognition::models::OCR_MODEL_FILES)
        .map_err(|error| format!("verify installed model: {error}"))?;
    eprintln!("inference checkpoint: package verified");
    install_test_runtime(&installed)?;
    eprintln!("inference checkpoint: runtime installed");
    let mut ocr = PpOcr::load(&installed).map_err(|error| format!("load OCR model: {error}"))?;
    eprintln!("inference checkpoint: sessions loaded");
    let image = image::RgbaImage::from_pixel(64, 64, image::Rgba([255, 255, 255, 255]));
    let lines = ocr
        .recognize(&image, &OcrRun::new()?)
        .map_err(|error| format!("run OCR inference: {error}"))?;
    eprintln!("inference checkpoint: inference completed");

    let ner_package = catalog
        .packages
        .iter()
        .find(|package| package.id == "raner-v1")
        .ok_or("catalog is missing raner-v1")?;
    let ner_root = tempfile::tempdir().map_err(|error| format!("create NER temp root: {error}"))?;
    let ner_installed = download_and_install(
        &client,
        ner_package,
        ner_root.path(),
        &Cancellation::default(),
        |_| {},
    )
    .await
    .map_err(|error| format!("download and install RaNER: {error}"))?;
    recognition::models::verify_model(&ner_installed)
        .map_err(|error| format!("verify RaNER model: {error}"))?;
    install_test_runtime(&ner_installed)?;
    let mut ner =
        Raner::load(&ner_installed).map_err(|error| format!("load RaNER model: {error}"))?;
    let ner_entities = ner
        .analyze("张三在北京工作，联系电话是13812345678。")
        .map_err(|error| format!("run RaNER inference: {error}"))?;
    for expected in ["PERSON", "LOCATION"] {
        if !ner_entities
            .iter()
            .any(|entity| entity.entity_type == expected)
        {
            return Err(format!("RaNER inference did not detect {expected}").into());
        }
    }
    eprintln!("inference checkpoint: RaNER inference completed");
    println!(
        "{}",
        serde_json::json!({
            "catalog_revision": catalog.revision,
            "package": package.id,
            "partial_bytes": partial_bytes,
            "first_resumed_progress": first_progress,
            "installed": true,
            "inference": true,
            "detected_lines": lines.len(),
            "ner_inference": true,
            "ner_entities": ner_entities.len()
        })
    );
    Ok(())
}
