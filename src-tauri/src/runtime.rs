use crate::{CapabilityStatus, RuntimeModelStatus, Worker, poisoned, verify_installed};
use domain::{Error, OcrProfile, Result, TaskOptions};
use recognition::{Ner, ocr::OcrRun};
use std::{
    cell::Cell,
    io::Write,
    path::Path,
    sync::{Arc, Mutex, atomic::Ordering},
    time::Instant,
};
use task_engine::Engine;
use tauri::Emitter;
use uuid::Uuid;

pub const IDS: [&str; 3] = ["raner-v1", "ppocrv4-mobile-v1", "ppocrv4-accurate-v1"];
const LABELS: [&str; 3] = ["中文实体识别", "轻量 OCR", "高精度 OCR"];

fn load_label(stage: &str) -> &str {
    match stage {
        "inspect" => "检查本机模型文件",
        "runtime" => "加载 ONNX 推理引擎",
        "session" => "读取实体模型并初始化计算图",
        "tokenizer" => "加载中文分词器",
        "crf" => "加载实体解码参数",
        "det.onnx" => "初始化文字检测模型",
        "cls.onnx" => "初始化文字方向模型",
        "rec.onnx" => "初始化文字识别模型",
        "warmup" => "执行首次推理验证",
        _ => stage,
    }
}

fn log_load(root: &Path, operation: Uuid, id: &str, start: Instant, stage: &str) {
    if let Ok(mut log) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("logs/models.log"))
    {
        let _ = writeln!(
            log,
            "{} {} {} {}ms {} arch={}",
            chrono::Utc::now().to_rfc3339(),
            operation,
            id,
            start.elapsed().as_millis(),
            stage,
            std::env::consts::ARCH
        );
    }
}

fn guarded_load<T>(load: impl FnOnce() -> Result<T>) -> Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(load)).unwrap_or_else(|_| {
        Err(Error::ModelsNotReady("推理引擎发生内部异常，请退出私匣后重新打开；如仍失败，请提供 logs/models.log 中的加载阶段记录".into()))
    })
}
#[derive(Clone, Copy)]
pub struct Requirements {
    mask: u8,
}
impl Requirements {
    pub fn none() -> Self {
        Self { mask: 0 }
    }
    pub fn text() -> Self {
        Self { mask: 1 }
    }
    /// Generation uses the raster budget without loading any inference model.
    pub fn render() -> Self {
        Self { mask: 8 }
    }
    pub fn uses_raster_budget(self) -> bool {
        self.mask & 8 != 0
    }
    pub fn package(id: &str) -> Result<Self> {
        let index = IDS
            .iter()
            .position(|value| *value == id)
            .ok_or_else(|| Error::Invalid("未知的模型 ID".into()))?;
        Ok(Self { mask: 1 << index })
    }
    pub fn for_file(path: &Path, options: &TaskOptions) -> Self {
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let visual = matches!(
            extension.as_str(),
            "pdf" | "png" | "jpg" | "jpeg" | "bmp" | "tif" | "tiff" | "docx" | "xlsx" | "xlsm"
        );
        Self {
            mask: 1 | if visual {
                8 | match options.ocr_profile {
                    OcrProfile::Mobile => 2,
                    OcrProfile::Accurate => 4,
                }
            } else {
                0
            },
        }
    }
}

pub fn initial(root: &Path) -> RuntimeModelStatus {
    let capabilities = IDS
        .iter()
        .enumerate()
        .map(|(index, id)| {
            let location = root.join("models").join(id);
            let installed = verify_installed(root, id, false).is_ok();
            CapabilityStatus {
                id: (*id).into(),
                label: LABELS[index].into(),
                installed,
                ready: false,
                version: None,
                bytes: 0,
                location: location.display().to_string(),
                error: None,
                state: if installed { "installed" } else { "missing" }.into(),
                ready_workers: 0,
                operation_id: None,
            }
        })
        .collect();
    RuntimeModelStatus {
        ready: false,
        version: None,
        location: root.join("models/raner-v1").display().to_string(),
        bytes: 0,
        error: None,
        capabilities,
    }
}

fn publish(status: &Arc<Mutex<RuntimeModelStatus>>, app: Option<&tauri::AppHandle>) -> Result<()> {
    let mut status = status.lock().map_err(poisoned)?;
    status.ready = status.capabilities[0].ready;
    status.version = status.capabilities[0].version.clone();
    status.error = status.capabilities[0].error.clone();
    status.bytes = status.capabilities.iter().map(|item| item.bytes).sum();
    if let Some(app) = app {
        let _ = app.emit("model-progress", &*status);
    }
    Ok(())
}

pub struct LoadContext<'a> {
    pub root: &'a Path,
    pub engines: &'a [Arc<Worker>],
    pub status: &'a Arc<Mutex<RuntimeModelStatus>>,
    pub app: Option<&'a tauri::AppHandle>,
}

pub fn ensure(
    context: LoadContext<'_>,
    engine: &mut Engine,
    worker: &Worker,
    requirements: Requirements,
    run: Option<&OcrRun>,
    full_verify: bool,
) -> Result<()> {
    let LoadContext {
        root,
        engines,
        status,
        app,
    } = context;
    for (index, id) in IDS.iter().enumerate() {
        let bit = 1 << index;
        if requirements.mask & bit == 0 {
            continue;
        }
        if run.is_some_and(OcrRun::is_cancelled) {
            return Err(Error::State("任务已取消".into()));
        }
        if worker.loaded.load(Ordering::Acquire) & bit != 0 && !full_verify {
            continue;
        }
        let operation_id = Uuid::new_v4();
        {
            let mut status = status.lock().map_err(poisoned)?;
            let capability = &mut status.capabilities[index];
            capability.state = if capability.ready { "ready" } else { "loading" }.into();
            capability.operation_id = Some(operation_id);
            capability.error = None;
        }
        if let Some(app) = app {
            let _ = app.emit("model-progress", serde_json::json!({"id":id,"stage":"loading","operation_id":operation_id,"message":format!("正在加载{}", LABELS[index])}));
        }
        publish(status, app)?;
        let start = Instant::now();
        let local_run = OcrRun::new()?;
        let run = run.unwrap_or(&local_run);
        let stage = Cell::new("inspect");
        let mut progress = |value: &'static str| {
            stage.set(value);
            log_load(root, operation_id, id, start, value);
            if let Some(app) = app {
                let _ = app.emit(
                    "model-progress",
                    serde_json::json!({
                        "id":id, "stage":"loading", "operation_id":operation_id,
                        "elapsed_ms":start.elapsed().as_millis(), "message":load_label(value)
                    }),
                );
            }
        };
        let result: Result<_> = guarded_load(|| {
            progress("inspect");
            let verified = verify_installed(root, id, full_verify)?;
            if worker.loaded.load(Ordering::Acquire) & bit == 0 {
                match index {
                    0 => {
                        let mut model =
                            recognition::ner::Raner::load_with_progress(&verified, &mut progress)?;
                        progress("warmup");
                        model.analyze_with_run("张三在北京工作", run)?;
                        engine.ner = Some(Box::new(model));
                    }
                    1 | 2 => {
                        let mut model =
                            recognition::ocr::PpOcr::load_with_progress(&verified, &mut progress)?;
                        progress("warmup");
                        model.warm_up_with_run(run)?;
                        if index == 1 {
                            engine.ocr_mobile = Some(Box::new(model));
                        } else {
                            engine.ocr_accurate = Some(Box::new(model));
                        }
                    }
                    _ => unreachable!(),
                }
            }
            worker.loaded.fetch_or(bit, Ordering::Release);
            Ok(verified)
        })
        .map_err(|error| Error::ModelsNotReady(format!("{}：{error}", load_label(stage.get()))));
        {
            let mut status = status.lock().map_err(poisoned)?;
            let capability = &mut status.capabilities[index];
            capability.ready_workers = engines
                .iter()
                .filter(|worker| worker.loaded.load(Ordering::Acquire) & bit != 0)
                .count();
            capability.ready = capability.ready_workers > 0;
            capability.operation_id = Some(operation_id);
            match &result {
                Ok(verified) => {
                    let manifest = verified.manifest();
                    capability.installed = true;
                    capability.version = Some(manifest.version.clone());
                    capability.bytes = manifest.files.iter().map(|file| file.size).sum();
                    capability.error = None;
                    capability.state = "ready".into();
                }
                Err(error) => {
                    capability.installed = verify_installed(root, id, false).is_ok();
                    capability.error = if run.is_cancelled() {
                        None
                    } else {
                        Some(error.to_string())
                    };
                    capability.state = if capability.ready {
                        "ready"
                    } else if run.is_cancelled() && capability.installed {
                        "installed"
                    } else if !capability.installed {
                        "missing"
                    } else {
                        "failed"
                    }
                    .into();
                }
            }
        }
        if let Some(app) = app {
            let _ = app.emit("model-progress", serde_json::json!({"id":id,"stage":if result.is_ok() {"ready"} else if run.is_cancelled() {"cancelled"} else {"failed"},"operation_id":operation_id,"elapsed_ms":start.elapsed().as_millis(),"message":result.as_ref().err().map(ToString::to_string)}));
        }
        // Metadata only: never write source paths or document text to diagnostics.
        log_load(
            root,
            operation_id,
            id,
            start,
            if result.is_ok() { "ready" } else { "failed" },
        );
        publish(status, app)?;
        if run.is_cancelled() {
            return Err(Error::State("任务已取消".into()));
        }
        result?;
    }
    Ok(())
}

/// Called only while scheduler maintenance has reserved every worker.
pub fn unload(
    engines: &[Arc<Worker>],
    status: &Arc<Mutex<RuntimeModelStatus>>,
    app: &tauri::AppHandle,
    package_id: Option<&str>,
) -> Result<()> {
    for worker in engines {
        let mut engine = worker.engine.lock().map_err(poisoned)?;
        for (index, id) in IDS.iter().enumerate() {
            if package_id.is_some_and(|selected| selected != *id) {
                continue;
            }
            match index {
                0 => engine.ner = None,
                1 => engine.ocr_mobile = None,
                _ => engine.ocr_accurate = None,
            }
            worker.loaded.fetch_and(!(1 << index), Ordering::Release);
        }
    }
    {
        let mut status = status.lock().map_err(poisoned)?;
        for capability in &mut status.capabilities {
            if package_id.is_some_and(|selected| selected != capability.id) {
                continue;
            }
            capability.ready = false;
            capability.ready_workers = 0;
            capability.state = if capability.installed {
                "installed"
            } else {
                "missing"
            }
            .into();
            capability.operation_id = None;
            capability.error = None;
        }
    }
    publish(status, Some(app))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn runtime_panic_becomes_an_error_without_poisoning_worker_lock() {
        let worker = Mutex::new(());
        let guard = worker.lock().unwrap();
        let result: Result<()> =
            guarded_load(|| panic!("synthetic runtime initialization failure"));
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("推理引擎发生内部异常")
        );
        drop(guard);
        assert!(worker.lock().is_ok());
        assert_eq!(guarded_load(|| Ok(42)).unwrap(), 42);
    }
    #[test]
    fn loading_phase_is_recorded_before_completion() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("logs")).unwrap();
        let operation = Uuid::new_v4();
        let start = Instant::now();
        log_load(root.path(), operation, IDS[0], start, "session");
        let log = std::fs::read_to_string(root.path().join("logs/models.log")).unwrap();
        assert!(log.contains("session arch="));
        assert!(!log.contains("ready"));
    }
    #[test]
    fn failed_preparation_never_leaves_a_loading_capability() {
        let root = tempfile::tempdir().unwrap();
        let worker = Arc::new(Worker {
            engine: Mutex::new(Engine {
                store: storage::Store::open(root.path(), zeroize::Zeroizing::new([31; 32]))
                    .unwrap(),
                ner: None,
                ocr_mobile: None,
                ocr_accurate: None,
                active_ocr: Arc::new(Mutex::new(std::collections::HashMap::new())),
            }),
            loaded: std::sync::atomic::AtomicU8::new(0),
        });
        let status = Arc::new(Mutex::new(initial(root.path())));
        let workers = [worker.clone()];
        let result = ensure(
            LoadContext {
                root: root.path(),
                engines: &workers,
                status: &status,
                app: None,
            },
            &mut worker.engine.lock().unwrap(),
            &worker,
            Requirements::text(),
            None,
            false,
        );
        assert!(result.is_err());
        let snapshot = status.lock().unwrap();
        assert_ne!(snapshot.capabilities[0].state, "loading");
        assert!(!snapshot.ready);
        assert!(
            snapshot
                .error
                .as_ref()
                .unwrap()
                .contains("检查本机模型文件")
        );
        assert_eq!(worker.loaded.load(Ordering::Acquire), 0);
    }
    #[test]
    fn text_and_visual_formats_request_only_the_needed_models() {
        let mut options = TaskOptions::default();
        assert_eq!(
            Requirements::for_file(Path::new("中文简历.docx"), &options).mask,
            11
        );
        assert_eq!(
            Requirements::for_file(Path::new("中文简历.PDF"), &options).mask,
            11
        );
        options.ocr_profile = OcrProfile::Accurate;
        assert_eq!(
            Requirements::for_file(Path::new("扫描件.tiff"), &options).mask,
            13
        );
        assert_eq!(
            Requirements::for_file(Path::new("文本.txt"), &options).mask,
            1
        );
    }
    #[test]
    fn missing_optional_models_are_not_initial_load_failures() {
        let root = tempfile::tempdir().unwrap();
        let status = initial(root.path());
        assert!(!status.ready);
        assert!(status.error.is_none());
        assert!(
            status
                .capabilities
                .iter()
                .all(|item| item.state == "missing"
                    && item.error.is_none()
                    && item.ready_workers == 0)
        );
    }
}
