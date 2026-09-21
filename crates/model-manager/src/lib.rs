use domain::{Error, ProgressEvent, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::io::AsyncWriteExt;

const MAX_PACKAGE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const DISK_RESERVE_BYTES: u64 = 128 * 1024 * 1024;
const CHUNK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);
const TRUSTED_HOST: &str = "www.modelscope.cn";
pub const CATALOG_URL: &str = "https://www.modelscope.cn/models/yuansui486/data_desensitization_0918/resolve/desktop-models-v1.0.0/desktop/catalog.json";
pub const CATALOG_SHA256: &str = "1b91ec357c0908192a94a9c21f145fd1fb48f686c442ca7d2db3223e611819a0";
const TRUSTED_CATALOG: &str = r#"{
  "schema": 1,
  "revision": "desktop-models-v1.0.0",
  "packages": [
    {"id":"raner-v1","version":"raner-v1.0.0","profile":"text","url":"https://www.modelscope.cn/models/yuansui486/data_desensitization_0918/resolve/desktop-models-v1.0.0/desktop/raner-v1.zip","size":383443560,"unpacked_size":423875153,"sha256":"4386188a3453e20f703feb009f3c731a1adea654b87747bfd292cca05ce3e6b1"},
    {"id":"ppocrv4-mobile-v1","version":"ppocrv4-mobile-v1.0.0","profile":"mobile","url":"https://www.modelscope.cn/models/yuansui486/data_desensitization_0918/resolve/desktop-models-v1.0.0/desktop/ppocrv4-mobile-v1.zip","size":20617575,"unpacked_size":32590797,"sha256":"b260c430ed85d3ebe0bbf37705cdbcf33a11be41be691ded77df470b4e83a832"},
    {"id":"ppocrv4-accurate-v1","version":"ppocrv4-accurate-v1.0.0","profile":"accurate","url":"https://www.modelscope.cn/models/yuansui486/data_desensitization_0918/resolve/desktop-models-v1.0.0/desktop/ppocrv4-accurate-v1.zip","size":185636512,"unpacked_size":220901772,"sha256":"6e9ded592fa160877168b5d1c51e802210473f645d2800c5c150548f922cde07"}
  ]
}"#;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Catalog {
    pub schema: u32,
    pub revision: String,
    pub packages: Vec<ModelPackage>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelPackage {
    pub id: String,
    pub version: String,
    pub profile: String,
    pub url: String,
    pub size: u64,
    pub unpacked_size: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PartialState {
    url: String,
    etag: Option<String>,
    expected_size: u64,
}

#[derive(Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);
impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

pub fn verify_catalog(bytes: &[u8], expected_sha256: &str) -> Result<Catalog> {
    if format!("{:x}", Sha256::digest(bytes)) != expected_sha256.to_ascii_lowercase() {
        return Err(Error::ModelsNotReady("模型目录 SHA-256 校验失败".into()));
    }
    let catalog: Catalog = serde_json::from_slice(bytes)
        .map_err(|e| Error::ModelsNotReady(format!("模型目录无效：{e}")))?;
    validate_catalog(catalog)
}

/// Returns the catalog pinned into this application build. Reading model status and
/// loading already-installed models must never depend on network availability.
pub fn trusted_catalog() -> Result<Catalog> {
    validate_catalog(serde_json::from_str(TRUSTED_CATALOG).map_err(json_error)?)
}

fn validate_catalog(catalog: Catalog) -> Result<Catalog> {
    if catalog.schema != 1 || catalog.revision.is_empty() || catalog.packages.is_empty() {
        return Err(Error::ModelsNotReady("模型目录版本无效".into()));
    }
    let mut ids = std::collections::HashSet::new();
    for package in &catalog.packages {
        let url = reqwest::Url::parse(&package.url)
            .map_err(|_| Error::ModelsNotReady("模型下载地址无效".into()))?;
        if url.scheme() != "https"
            || url.host_str() != Some(TRUSTED_HOST)
            || package.id.is_empty()
            || !package
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || !ids.insert(package.id.to_ascii_lowercase())
            || package.size == 0
            || package.size > MAX_PACKAGE_BYTES
            || package.unpacked_size < package.size
            || package.unpacked_size > MAX_PACKAGE_BYTES
            || package.sha256.len() != 64
            || !package.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(Error::ModelsNotReady("模型包条目无效".into()));
        }
    }
    Ok(catalog)
}

pub async fn fetch_catalog(client: &reqwest::Client) -> Result<Catalog> {
    let response = client
        .get(CATALOG_URL)
        .send()
        .await
        .map_err(download_error)?;
    if !response.status().is_success() {
        return Err(download_error(format!("HTTP {}", response.status())));
    }
    let bytes = response.bytes().await.map_err(download_error)?;
    if bytes.len() > 1024 * 1024 {
        return Err(Error::ModelsNotReady("模型目录文件过大".into()));
    }
    verify_catalog(&bytes, CATALOG_SHA256)
}

pub async fn download_and_install(
    client: &reqwest::Client,
    package: &ModelPackage,
    root: &Path,
    cancellation: &Cancellation,
    mut progress: impl FnMut(ProgressEvent),
) -> Result<PathBuf> {
    std::fs::create_dir_all(root.join("cache"))?;
    std::fs::create_dir_all(root.join("models"))?;
    let part = root.join("cache").join(format!("{}.zip.part", package.id));
    let state_path = root
        .join("cache")
        .join(format!("{}.partial.json", package.id));
    let mut verified = false;
    for attempt in 0..2 {
        if cancellation.is_cancelled() {
            return Err(Error::State("模型下载已取消".into()));
        }
        if attempt > 0 {
            let _ = std::fs::remove_file(&part);
            let _ = std::fs::remove_file(&state_path);
            progress(ProgressEvent {
                id: package.id.clone(),
                stage: "retrying".into(),
                current: 0,
                total: package.size,
                percent: 0.0,
                bytes_per_second: 0,
                eta_seconds: None,
                message: format!("{} 校验失败，正在重新下载", package_label(package)),
            });
        }
        let mut old_state = read_state(&state_path);
        let reusable = old_state
            .as_ref()
            .is_some_and(|old| old.url == package.url && old.expected_size == package.size);
        if !reusable {
            let _ = std::fs::remove_file(&part);
            let _ = std::fs::remove_file(&state_path);
            old_state = None;
        }
        let mut offset = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        if offset > package.size
            || (offset == package.size
                && file_sha256(&part)? != package.sha256.to_ascii_lowercase())
        {
            let _ = std::fs::remove_file(&part);
            let _ = std::fs::remove_file(&state_path);
            old_state = None;
            offset = 0;
        }
        ensure_disk_space(root, package, offset)?;
        let mut downloaded = offset;
        if offset < package.size {
            progress(ProgressEvent {
                id: package.id.clone(),
                stage: "connecting".into(),
                current: offset,
                total: package.size,
                percent: offset as f32 * 100.0 / package.size as f32,
                bytes_per_second: 0,
                eta_seconds: None,
                message: if offset > 0 {
                    format!(
                        "正在连接 ModelScope 中国站，准备继续下载 {}",
                        package_label(package)
                    )
                } else {
                    format!(
                        "正在连接 ModelScope 中国站，准备下载 {}",
                        package_label(package)
                    )
                },
            });
            let mut request = client.get(&package.url);
            if offset > 0 {
                request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
                if let Some(value) = old_state.as_ref().and_then(|state| state.etag.as_ref()) {
                    request = request.header(reqwest::header::IF_RANGE, value);
                }
            }
            let mut response = request.send().await.map_err(download_error)?;
            if offset > 0 && response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
                if !response.status().is_success() {
                    return Err(download_error(format!("HTTP {}", response.status())));
                }
                offset = 0;
                downloaded = 0;
                let _ = std::fs::remove_file(&part);
            } else if offset > 0 {
                let expected_prefix = format!("bytes {offset}-");
                let content_range = response
                    .headers()
                    .get(reqwest::header::CONTENT_RANGE)
                    .and_then(|value| value.to_str().ok());
                if !content_range.is_some_and(|value| value.starts_with(&expected_prefix)) {
                    return Err(download_error("续传响应的 Content-Range 无效"));
                }
            } else if !response.status().is_success() {
                return Err(download_error(format!("HTTP {}", response.status())));
            }
            let etag = response
                .headers()
                .get(reqwest::header::ETAG)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
                .or_else(|| old_state.and_then(|state| state.etag));
            let state = PartialState {
                url: package.url.clone(),
                etag,
                expected_size: package.size,
            };
            std::fs::write(&state_path, serde_json::to_vec(&state).map_err(json_error)?)?;
            let mut output = tokio::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .append(offset > 0)
                .truncate(offset == 0)
                .open(&part)
                .await?;
            let session_offset = downloaded;
            let session_started = Instant::now();
            let mut last_progress = Instant::now() - PROGRESS_INTERVAL;
            loop {
                let chunk = tokio::time::timeout(CHUNK_TIMEOUT, response.chunk())
                    .await
                    .map_err(|_| download_error("连接长时间没有收到数据，请检查网络后重试"))?
                    .map_err(download_error)?;
                let Some(chunk) = chunk else { break };
                if cancellation.is_cancelled() {
                    return Err(Error::State("模型下载已取消".into()));
                }
                downloaded = downloaded
                    .checked_add(chunk.len() as u64)
                    .ok_or_else(|| Error::ModelsNotReady("模型包大小溢出".into()))?;
                if downloaded > package.size {
                    return Err(download_error("服务端返回的数据超过清单大小"));
                }
                output.write_all(&chunk).await?;
                if last_progress.elapsed() >= PROGRESS_INTERVAL || downloaded == package.size {
                    let elapsed = session_started.elapsed().as_secs_f64();
                    let bytes_per_second = if elapsed > 0.0 {
                        ((downloaded - session_offset) as f64 / elapsed).round() as u64
                    } else {
                        0
                    };
                    let eta_seconds = (bytes_per_second > 0).then(|| {
                        package
                            .size
                            .saturating_sub(downloaded)
                            .div_ceil(bytes_per_second)
                    });
                    progress(ProgressEvent {
                        id: package.id.clone(),
                        stage: "downloading".into(),
                        current: downloaded,
                        total: package.size,
                        percent: downloaded as f32 * 100.0 / package.size as f32,
                        bytes_per_second,
                        eta_seconds,
                        message: format!("正在下载 {}", package_label(package)),
                    });
                    last_progress = Instant::now();
                }
            }
            output.flush().await?;
        }
        if cancellation.is_cancelled() {
            return Err(Error::State("模型下载已取消".into()));
        }
        progress(ProgressEvent {
            id: package.id.clone(),
            stage: "verifying".into(),
            current: downloaded,
            total: package.size,
            percent: 100.0,
            bytes_per_second: 0,
            eta_seconds: None,
            message: format!("正在校验 {}", package_label(package)),
        });
        verified = downloaded == package.size
            && file_sha256(&part)? == package.sha256.to_ascii_lowercase();
        if verified {
            break;
        }
    }
    if !verified {
        let _ = std::fs::remove_file(&part);
        let _ = std::fs::remove_file(&state_path);
        return Err(Error::ModelsNotReady(
            "模型包大小或 SHA-256 校验失败，已清除损坏的下载文件".into(),
        ));
    }
    let destination = root.join("models").join(&package.id);
    progress(ProgressEvent {
        id: package.id.clone(),
        stage: "installing".into(),
        current: package.size,
        total: package.size,
        percent: 100.0,
        bytes_per_second: 0,
        eta_seconds: None,
        message: format!("正在安装 {}", package_label(package)),
    });
    install_archive(&part, &destination, package.unpacked_size)?;
    let _ = std::fs::remove_file(part);
    let _ = std::fs::remove_file(state_path);
    progress(ProgressEvent {
        id: package.id.clone(),
        stage: "installed".into(),
        current: package.size,
        total: package.size,
        percent: 100.0,
        bytes_per_second: 0,
        eta_seconds: Some(0),
        message: format!("{} 已安装", package_label(package)),
    });
    Ok(destination)
}

fn package_label(package: &ModelPackage) -> &'static str {
    match package.profile.as_str() {
        "text" => "中文实体识别模型",
        "mobile" => "轻量 OCR 模型",
        "accurate" => "高精度 OCR 模型",
        _ => "模型",
    }
}

fn ensure_disk_space(root: &Path, package: &ModelPackage, downloaded: u64) -> Result<()> {
    let needed = package
        .size
        .saturating_sub(downloaded)
        .saturating_add(package.unpacked_size)
        .saturating_add(DISK_RESERVE_BYTES);
    let available = fs2::available_space(root)?;
    if available < needed {
        return Err(Error::ModelsNotReady(format!(
            "磁盘空间不足：至少还需要 {:.1} GB，可用 {:.1} GB",
            needed as f64 / 1_073_741_824.0,
            available as f64 / 1_073_741_824.0
        )));
    }
    Ok(())
}

fn install_archive(archive: &Path, destination: &Path, max_unpacked: u64) -> Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| Error::Invalid("模型目录无父目录".into()))?;
    let stage = tempfile::Builder::new()
        .prefix("model-stage-")
        .tempdir_in(parent)?;
    let mut zip = zip::ZipArchive::new(std::fs::File::open(archive)?)
        .map_err(|e| Error::ModelsNotReady(format!("模型压缩包无效：{e}")))?;
    let mut total = 0u64;
    let mut names = std::collections::HashSet::new();
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|e| Error::ModelsNotReady(e.to_string()))?;
        let relative = entry
            .enclosed_name()
            .ok_or_else(|| Error::ModelsNotReady("模型压缩包路径无效".into()))?;
        let key = relative.to_string_lossy().to_ascii_lowercase();
        if !names.insert(key) {
            return Err(Error::ModelsNotReady("模型压缩包包含重复路径".into()));
        }
        total = total
            .checked_add(entry.size())
            .ok_or_else(|| Error::ModelsNotReady("模型解压大小溢出".into()))?;
        if total > max_unpacked {
            return Err(Error::ModelsNotReady("模型解压大小超过清单".into()));
        }
        let target = stage.path().join(relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::File::create(target)?;
        std::io::copy(&mut entry, &mut file)?;
        file.flush()?;
    }
    let manifest: recognition::models::Manifest =
        serde_json::from_slice(&std::fs::read(stage.path().join("manifest.json"))?)
            .map_err(|e| Error::ModelsNotReady(e.to_string()))?;
    let required = manifest
        .files
        .iter()
        .map(|f| f.name.as_str())
        .collect::<Vec<_>>();
    recognition::models::verify_with_required(stage.path(), &required)?;
    let backup = destination.with_extension("old");
    let _ = std::fs::remove_dir_all(&backup);
    if destination.exists() {
        std::fs::rename(destination, &backup)?;
    }
    if let Err(error) = std::fs::rename(stage.path(), destination) {
        if backup.exists() {
            let _ = std::fs::rename(&backup, destination);
        }
        return Err(Error::Io(error.to_string()));
    }
    let _ = std::fs::remove_dir_all(backup);
    Ok(())
}

fn read_state(path: &Path) -> Option<PartialState> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}
fn json_error(error: serde_json::Error) -> Error {
    Error::Invalid(error.to_string())
}
fn download_error(error: impl std::fmt::Display) -> Error {
    Error::Io(format!("模型下载失败：{error}"))
}
fn file_sha256(path: &Path) -> Result<String> {
    let mut source = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_requires_pinned_modelscope_urls_and_hash() {
        let body = serde_json::to_vec(&Catalog {
            schema: 1,
            revision: "desktop-models-v1.0.0".into(),
            packages: vec![ModelPackage {
                id: "ppocrv4-mobile-v1".into(),
                version: "1".into(),
                profile: "mobile".into(),
                url: "https://www.modelscope.cn/api/v1/models/example/file".into(),
                size: 10,
                unpacked_size: 20,
                sha256: "0".repeat(64),
            }],
        })
        .unwrap();
        let hash = format!("{:x}", Sha256::digest(&body));
        assert!(verify_catalog(&body, &hash).is_ok());
        assert!(verify_catalog(&body, &"1".repeat(64)).is_err());
    }

    #[test]
    fn bundled_catalog_is_valid_and_contains_each_runtime_capability() {
        let catalog = trusted_catalog().unwrap();
        let ids = catalog
            .packages
            .iter()
            .map(|package| package.id.as_str())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(catalog.revision, "desktop-models-v1.0.0");
        assert!(ids.contains("raner-v1"));
        assert!(ids.contains("ppocrv4-mobile-v1"));
        assert!(ids.contains("ppocrv4-accurate-v1"));
    }

    #[tokio::test]
    async fn corrupt_download_is_cleared_and_retried_once() {
        let content = b"synthetic model";
        let manifest = recognition::models::Manifest {
            schema: 1,
            version: "test-v1".into(),
            source: "unit-test".into(),
            license: "AGPL-3.0-or-later".into(),
            files: vec![recognition::models::ModelFile {
                name: "model.bin".into(),
                size: content.len() as u64,
                sha256: format!("{:x}", Sha256::digest(content)),
            }],
        };
        let mut archive = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut archive);
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("manifest.json", options).unwrap();
            zip.write_all(&serde_json::to_vec(&manifest).unwrap())
                .unwrap();
            zip.start_file("model.bin", options).unwrap();
            zip.write_all(content).unwrap();
            zip.finish().unwrap();
        }
        let valid = archive.into_inner();
        let mut corrupt = valid.clone();
        let index = corrupt.len() / 2;
        corrupt[index] ^= 0xff;
        let valid_size = valid.len() as u64;
        let valid_sha256 = format!("{:x}", Sha256::digest(&valid));

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for body in [corrupt, valid] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 2048];
                let _ = stream.read(&mut request).unwrap();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
                stream.flush().unwrap();
            }
        });
        let root = tempfile::tempdir().unwrap();
        let package = ModelPackage {
            id: "test-model".into(),
            version: "test-v1".into(),
            profile: "test".into(),
            url: format!("http://{address}/model.zip"),
            size: valid_size,
            unpacked_size: 1024 * 1024,
            sha256: valid_sha256,
        };
        let mut stages = Vec::new();
        let destination = download_and_install(
            &reqwest::Client::new(),
            &package,
            root.path(),
            &Cancellation::default(),
            |event| stages.push(event.stage),
        )
        .await
        .unwrap();
        server.join().unwrap();
        assert_eq!(
            std::fs::read(destination.join("model.bin")).unwrap(),
            content
        );
        assert!(stages.iter().any(|stage| stage == "retrying"));
        assert_eq!(stages.last().map(String::as_str), Some("installed"));
    }
}
