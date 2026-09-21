#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod auth;
#[cfg(windows)]
mod integration;
#[cfg(not(windows))]
#[path = "integration_non_windows.rs"]
mod integration;

use domain::{
    AppSettings, Error, Policy, PreviewDto, RegionDto, Result, Rule, Selection, TaskMeta,
    TaskOptions,
};
use model_manager::{Cancellation, Catalog};
use serde::Serialize;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
    sync::{Arc, Mutex},
};
use task_engine::batch::BatchView;
use task_engine::{Engine, RegionMutationAck, TaskView};
use tauri::{Emitter, Manager, State};
use uuid::Uuid;
use zeroize::Zeroizing;

struct AppState {
    auth: auth::AuthManager,
    engines: Arc<Vec<Arc<Worker>>>,
    next_engine: AtomicUsize,
    worker_limit: AtomicUsize,
    desired_workers: AtomicUsize,
    model: Arc<Mutex<RuntimeModelStatus>>,
    active_ocr: Arc<Mutex<HashMap<Uuid, recognition::ocr::OcrRun>>>,
    catalog: Mutex<Option<Catalog>>,
    model_operations: tokio::sync::Mutex<()>,
    installs: Mutex<HashMap<String, Cancellation>>,
    permits: Mutex<Arc<tokio::sync::Semaphore>>,
    client: reqwest::Client,
    integration_jobs: integration::JobRegistry,
}
struct Worker {
    engine: Mutex<Engine>,
    busy: std::sync::atomic::AtomicBool,
}
struct WorkerLease(Arc<Worker>);
impl Drop for WorkerLease {
    fn drop(&mut self) {
        self.0.busy.store(false, Ordering::Release);
    }
}
type Shared<'a> = State<'a, AppState>;
fn poisoned<T>(_: std::sync::PoisonError<T>) -> Error {
    Error::State("后台任务异常，请重启应用".into())
}
async fn work<T: Send + 'static>(
    state: Shared<'_>,
    f: impl FnOnce(&mut Engine) -> Result<T> + Send + 'static,
) -> Result<T> {
    state.auth.require_authenticated()?;
    let engine = primary(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut guard = engine.engine.lock().map_err(poisoned)?;
        f(&mut guard)
    })
    .await
    .map_err(|e| Error::Io(e.to_string()))?
}
async fn job<T: Send + 'static>(
    state: Shared<'_>,
    f: impl FnOnce(&mut Engine) -> Result<T> + Send + 'static,
) -> Result<T> {
    state.auth.require_authenticated()?;
    let permits = state.permits.lock().map_err(poisoned)?.clone();
    let _permit = permits
        .acquire_owned()
        .await
        .map_err(|_| Error::State("任务调度器已关闭".into()))?;
    loop {
        let count = state
            .worker_limit
            .load(Ordering::Acquire)
            .clamp(1, state.engines.len());
        if count == 0 {
            return Err(Error::State("后台任务 worker 未初始化".into()));
        }
        let start = state.next_engine.fetch_add(1, Ordering::Relaxed) % count;
        for offset in 0..count {
            let worker = state.engines[(start + offset) % count].clone();
            if worker
                .busy
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                return tauri::async_runtime::spawn_blocking(move || {
                    let _lease = WorkerLease(worker.clone());
                    let mut guard = worker.engine.lock().map_err(poisoned)?;
                    f(&mut guard)
                })
                .await
                .map_err(|error| Error::Io(error.to_string()))?;
            }
        }
        tokio::task::yield_now().await;
    }
}

fn primary(state: &AppState) -> Result<Arc<Worker>> {
    state
        .engines
        .first()
        .cloned()
        .ok_or_else(|| Error::State("后台任务 worker 未初始化".into()))
}
fn register_run(state: &AppState, id: Uuid) -> Result<recognition::ocr::OcrRun> {
    let run = recognition::ocr::OcrRun::new()?;
    state
        .active_ocr
        .lock()
        .map_err(poisoned)?
        .insert(id, run.clone());
    Ok(run)
}
fn unregister_run(state: &AppState, id: Uuid) -> Result<()> {
    state.active_ocr.lock().map_err(poisoned)?.remove(&id);
    Ok(())
}

fn finish_task(app: &tauri::AppHandle, id: Uuid, result: Result<TaskView>) -> Result<TaskView> {
    match result {
        Ok(view) => {
            let _ = app.emit(
                "task-progress",
                serde_json::json!({"id":id,"stage":&view.meta.state,"terminal":true}),
            );
            let _ = app.emit("task-updated", &view.meta);
            Ok(view)
        }
        Err(error) => {
            emit_terminal_error(app, "task-progress", id, &error);
            Err(error)
        }
    }
}

fn finish_batch(app: &tauri::AppHandle, id: Uuid, result: Result<BatchView>) -> Result<BatchView> {
    match result {
        Ok(view) => {
            let _ = app.emit(
                "batch-progress",
                serde_json::json!({
                    "id":id,
                    "stage":&view.meta.state,
                    "terminal":true
                }),
            );
            let _ = app.emit("task-updated", &view.meta);
            Ok(view)
        }
        Err(error) => {
            emit_terminal_error(app, "batch-progress", id, &error);
            Err(error)
        }
    }
}

fn emit_terminal_error(app: &tauri::AppHandle, event: &str, id: Uuid, error: &Error) {
    let message = error.to_string();
    let stage = if message.contains("取消") {
        "cancelled"
    } else {
        "failed"
    };
    let _ = app.emit(
        event,
        serde_json::json!({
            "id":id,
            "stage":stage,
            "terminal":true,
            "message":message
        }),
    );
}

#[derive(Clone, Serialize)]
struct PackageStatus {
    id: String,
    version: String,
    profile: String,
    size: u64,
    location: String,
    installed: bool,
    ready: bool,
    error: Option<String>,
}

#[derive(Clone, Serialize)]
struct CapabilityStatus {
    id: String,
    label: String,
    installed: bool,
    ready: bool,
    version: Option<String>,
    bytes: u64,
    location: String,
    error: Option<String>,
}

#[derive(Clone, Serialize)]
struct RuntimeModelStatus {
    // Kept for existing clients. `ready` means the core text model can serve work.
    ready: bool,
    version: Option<String>,
    location: String,
    bytes: u64,
    error: Option<String>,
    capabilities: Vec<CapabilityStatus>,
}

async fn catalog(state: &AppState) -> Result<Catalog> {
    if let Some(catalog) = state.catalog.lock().map_err(poisoned)?.clone() {
        return Ok(catalog);
    }
    let catalog = model_manager::trusted_catalog()?;
    *state.catalog.lock().map_err(poisoned)? = Some(catalog.clone());
    Ok(catalog)
}

#[derive(Serialize)]
struct Bootstrap {
    tasks: Vec<TaskMeta>,
    model: RuntimeModelStatus,
    data_dir: String,
}

#[tauri::command]
async fn auth_status(state: Shared<'_>) -> Result<auth::AuthStatus> {
    state.auth.status().await
}

#[tauri::command]
async fn auth_login(
    state: Shared<'_>,
    app: tauri::AppHandle,
    tenant_code: String,
    username: String,
    password: String,
) -> Result<auth::AuthStatus> {
    let status = state.auth.login(tenant_code, username, password).await?;
    let _ = app.emit("auth-state-changed", &status);
    Ok(status)
}

#[tauri::command]
async fn auth_logout(state: Shared<'_>, app: tauri::AppHandle) -> Result<()> {
    state.auth.logout().await?;
    let status = auth::AuthStatus {
        authenticated: false,
        offline: false,
        offline_until: None,
        subject: None,
        policy: None,
        reason: None,
    };
    let _ = app.emit("auth-state-changed", status);
    Ok(())
}

#[tauri::command]
async fn initialize(state: Shared<'_>) -> Result<Bootstrap> {
    let model = state.model.lock().map_err(poisoned)?.clone();
    work(state, move |e| {
        Ok(Bootstrap {
            tasks: e.store.tasks()?,
            model,
            data_dir: e.store.root.display().to_string(),
        })
    })
    .await
}
#[tauri::command]
async fn analyze_file(
    state: Shared<'_>,
    app: tauri::AppHandle,
    path: PathBuf,
    options: Option<TaskOptions>,
    request_id: Option<Uuid>,
) -> Result<TaskView> {
    state.auth.require_authenticated()?;
    let id = request_id.unwrap_or_else(Uuid::new_v4);
    let run = register_run(&state, id)?;
    let _ = app.emit(
        "task-progress",
        serde_json::json!({"id":id,"stage":"queued"}),
    );
    let result = job(state.clone(), move |e| {
        e.analyze_file_as(&path, options.unwrap_or_default(), id, run)
    })
    .await;
    unregister_run(&state, id)?;
    finish_task(&app, id, result)
}
#[tauri::command]
async fn task_view(state: Shared<'_>, id: Uuid) -> Result<TaskView> {
    work(state, move |e| e.view(id)).await
}
#[tauri::command]
async fn select_entities(
    state: Shared<'_>,
    id: Uuid,
    selections: Vec<Selection>,
) -> Result<TaskView> {
    work(state, move |e| e.select(id, selections)).await
}
#[tauri::command]
async fn preview(state: Shared<'_>, id: Uuid) -> Result<String> {
    work(state, move |e| e.preview(id)).await
}
#[tauri::command]
async fn document_preview(state: Shared<'_>, id: Uuid) -> Result<PreviewDto> {
    work(state, move |e| e.document_preview(id)).await
}
#[tauri::command]
async fn document_result_preview(state: Shared<'_>, id: Uuid) -> Result<PreviewDto> {
    work(state, move |e| e.document_result_preview(id)).await
}
#[tauri::command]
async fn upsert_region(
    state: Shared<'_>,
    id: Uuid,
    region: RegionDto,
    expected_revision: u64,
) -> Result<RegionMutationAck> {
    work(state, move |e| {
        e.upsert_region(id, region, expected_revision)
    })
    .await
}
#[tauri::command]
async fn remove_region(
    state: Shared<'_>,
    id: Uuid,
    region_id: Uuid,
    expected_revision: u64,
) -> Result<RegionMutationAck> {
    work(state, move |e| {
        e.remove_region(id, region_id, expected_revision)
    })
    .await
}
#[tauri::command]
async fn execute(state: Shared<'_>, app: tauri::AppHandle, id: Uuid) -> Result<TaskView> {
    state.auth.require_authenticated()?;
    let run = register_run(&state, id)?;
    let _ = app.emit(
        "task-progress",
        serde_json::json!({"id":id,"stage":"processing"}),
    );
    let result = job(state.clone(), move |e| e.execute_as(id, run)).await;
    unregister_run(&state, id)?;
    finish_task(&app, id, result)
}
#[tauri::command]
async fn export_task(state: Shared<'_>, id: Uuid, path: PathBuf) -> Result<()> {
    work(state, move |e| e.export(id, &path)).await
}
#[tauri::command]
async fn export_recovery(
    state: Shared<'_>,
    id: Uuid,
    password: String,
    path: PathBuf,
) -> Result<()> {
    let password = Zeroizing::new(password);
    work(state, move |e| e.export_recovery(id, &password, &path)).await
}
#[tauri::command]
async fn restore(
    state: Shared<'_>,
    source: PathBuf,
    password: String,
    destination: PathBuf,
) -> Result<()> {
    let password = Zeroizing::new(password);
    work(state, move |e| e.restore(&source, &password, &destination)).await
}
#[tauri::command]
async fn list_tasks(state: Shared<'_>) -> Result<Vec<TaskMeta>> {
    work(state, |e| e.store.tasks()).await
}
#[tauri::command]
async fn delete_task(state: Shared<'_>, id: Uuid) -> Result<()> {
    state.auth.require_authenticated()?;
    if state.active_ocr.lock().map_err(poisoned)?.contains_key(&id) {
        return Err(Error::State("任务正在运行，请先取消并等待任务停止".into()));
    }
    work(state, move |e| e.store.delete(id)).await
}
#[tauri::command]
async fn cancel_task(state: Shared<'_>, id: Uuid) -> Result<()> {
    state.auth.require_authenticated()?;
    if let Some(run) = state.active_ocr.lock().map_err(poisoned)?.get(&id) {
        return run.cancel();
    }
    work(state, move |e| e.cancel(id)).await
}
#[tauri::command]
async fn create_batch(
    state: Shared<'_>,
    app: tauri::AppHandle,
    paths: Vec<PathBuf>,
    request_id: Option<Uuid>,
) -> Result<BatchView> {
    state.auth.require_authenticated()?;
    let id = request_id.unwrap_or_else(Uuid::new_v4);
    let run = register_run(&state, id)?;
    let _ = app.emit(
        "batch-progress",
        serde_json::json!({"id":id,"done":0,"total":paths.len(),"stage":"queued"}),
    );
    let callback_app = app.clone();
    let result = job(state.clone(), move |e| {
        e.create_batch_as(paths, id, run, |done, total| {
            let _ = callback_app.emit(
                "batch-progress",
                serde_json::json!({"id":id,"done":done,"total":total,"stage":"analyzing"}),
            );
        })
    })
    .await;
    unregister_run(&state, id)?;
    finish_batch(&app, id, result)
}
#[tauri::command]
async fn batch_view(state: Shared<'_>, id: Uuid) -> Result<BatchView> {
    work(state, move |e| e.batch_view(id)).await
}
#[tauri::command]
async fn execute_batch(state: Shared<'_>, app: tauri::AppHandle, id: Uuid) -> Result<BatchView> {
    state.auth.require_authenticated()?;
    let run = register_run(&state, id)?;
    let _ = app.emit(
        "batch-progress",
        serde_json::json!({"id":id,"stage":"queued"}),
    );
    let callback_app = app.clone();
    let result = job(state.clone(), move |e| {
        e.execute_batch_as(id, run, |done, total| {
            let _ = callback_app.emit(
                "batch-progress",
                serde_json::json!({"id":id,"done":done,"total":total,"stage":"processing"}),
            );
        })
    })
    .await;
    unregister_run(&state, id)?;
    finish_batch(&app, id, result)
}
#[tauri::command]
async fn export_batch(state: Shared<'_>, id: Uuid, path: PathBuf) -> Result<()> {
    work(state, move |e| e.export_batch(id, &path)).await
}
#[tauri::command]
async fn list_rules(state: Shared<'_>) -> Result<Vec<Rule>> {
    work(state, |e| e.store.rules()).await
}
#[tauri::command]
async fn save_rule(state: Shared<'_>, rule: Rule) -> Result<()> {
    work(state, move |e| e.save_rule(&rule)).await
}
#[tauri::command]
async fn delete_rule(state: Shared<'_>, id: Uuid) -> Result<()> {
    work(state, move |e| e.store.delete_rule(id)).await
}
#[tauri::command]
async fn list_policies(state: Shared<'_>) -> Result<Vec<Policy>> {
    work(state, |e| e.store.policies()).await
}
#[tauri::command]
fn builtin_policies(state: Shared<'_>) -> Result<Vec<domain::BuiltinPolicy>> {
    state.auth.require_authenticated()?;
    Ok(domain::builtin_policies())
}
#[tauri::command]
async fn save_policy(state: Shared<'_>, policy: Policy) -> Result<()> {
    if policy.entity_type.trim().is_empty() || policy.replacement.len() > 4096 {
        return Err(Error::Invalid("类型为空或替换内容过长".into()));
    }
    work(state, move |e| e.store.save_policy(&policy)).await
}
#[tauri::command]
async fn get_settings(state: Shared<'_>) -> Result<AppSettings> {
    work(state, |e| e.store.settings()).await
}
#[tauri::command]
async fn save_settings(
    state: Shared<'_>,
    app: tauri::AppHandle,
    settings: AppSettings,
) -> Result<AppSettings> {
    let settings = settings.validate()?;
    let concurrency = settings.concurrency;
    let saved = settings.clone();
    work(state.clone(), move |e| e.store.save_settings(saved)).await?;
    let _guard = state.model_operations.lock().await;
    state
        .desired_workers
        .store(concurrency as usize, Ordering::Release);
    let status = reload_models(
        &state,
        app,
        HashMap::new(),
        true,
        false,
        concurrency as usize,
    )
    .await?;
    if status.ready {
        activate_workers(&state, concurrency as usize)?;
    }
    Ok(settings)
}

fn activate_workers(state: &AppState, count: usize) -> Result<()> {
    *state.permits.lock().map_err(poisoned)? = Arc::new(tokio::sync::Semaphore::new(count));
    state.worker_limit.store(count, Ordering::Release);
    Ok(())
}
#[tauri::command]
async fn model_status(state: Shared<'_>) -> Result<RuntimeModelStatus> {
    state.auth.require_authenticated()?;
    Ok(state.model.lock().map_err(poisoned)?.clone())
}

fn verify_installed(
    root: &std::path::Path,
    id: &str,
    full: bool,
) -> Result<recognition::models::VerifiedModel> {
    let dir = root.join("models").join(id);
    if id == "raner-v1" {
        if full {
            recognition::models::verify_model(&dir)
        } else {
            recognition::models::inspect_model(&dir)
        }
    } else {
        if full {
            recognition::models::verify_model_with_required(
                &dir,
                recognition::models::OCR_MODEL_FILES,
            )
        } else {
            recognition::models::inspect_model_with_required(
                &dir,
                recognition::models::OCR_MODEL_FILES,
            )
        }
    }
}

async fn reload_models(
    state: &AppState,
    app: tauri::AppHandle,
    verified: HashMap<String, recognition::models::VerifiedModel>,
    reuse_loaded: bool,
    full_verify: bool,
    requested_limit: usize,
) -> Result<RuntimeModelStatus> {
    let _ = app.emit(
        "model-progress",
        serde_json::json!({
            "stage": "loading",
            "percent": 100.0,
            "message": "模型文件已就绪，正在加载到本机内存，首次加载可能需要几十秒"
        }),
    );
    let engines = state.engines.clone();
    let status = state.model.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let root = engines[0]
            .engine
            .lock()
            .map_err(poisoned)?
            .store
            .root
            .clone();
        let packages = ["raner-v1", "ppocrv4-mobile-v1", "ppocrv4-accurate-v1"]
            .into_iter()
            .map(|id| {
                let result = verified
                    .get(id)
                    .cloned()
                    .map(Ok)
                    .unwrap_or_else(|| verify_installed(&root, id, full_verify))
                    .map_err(|error| error.to_string());
                (id, result)
            })
            .collect::<HashMap<_, _>>();
        let mut ready_workers = [0usize; 3];
        let mut errors = [Vec::<String>::new(), Vec::new(), Vec::new()];
        let worker_limit = requested_limit.clamp(1, engines.len());
        for (index, id) in ["raner-v1", "ppocrv4-mobile-v1", "ppocrv4-accurate-v1"]
            .into_iter()
            .enumerate()
        {
            if let Err(error) = &packages[id] {
                errors[index].push(error.clone());
                let _ = app.emit(
                    "model-progress",
                    serde_json::json!({"id":id,"stage":"failed","message":error}),
                );
            }
        }
        for (worker_index, engine) in engines.iter().enumerate() {
            let mut e = engine.engine.lock().map_err(poisoned)?;
            if worker_index >= worker_limit {
                continue;
            }
            for (index, id) in ["raner-v1", "ppocrv4-mobile-v1", "ppocrv4-accurate-v1"]
                .into_iter()
                .enumerate()
            {
                let slot_ready = match index {
                    0 => e.ner.is_some(),
                    1 => e.ocr_mobile.is_some(),
                    _ => e.ocr_accurate.is_some(),
                };
                let Ok(package) = &packages[id] else {
                    match index {
                        0 => e.ner = None,
                        1 => e.ocr_mobile = None,
                        _ => e.ocr_accurate = None,
                    }
                    continue;
                };
                if reuse_loaded && slot_ready {
                    ready_workers[index] += 1;
                    continue;
                }
                let loaded = match index {
                    0 => recognition::ner::Raner::load_verified(package).map(|model| {
                        e.ner = Some(Box::new(model));
                    }),
                    1 => recognition::ocr::PpOcr::load_verified(package).map(|model| {
                        e.ocr_mobile = Some(Box::new(model));
                    }),
                    _ => recognition::ocr::PpOcr::load_verified(package).map(|model| {
                        e.ocr_accurate = Some(Box::new(model));
                    }),
                };
                match loaded {
                    Ok(()) => ready_workers[index] += 1,
                    Err(error) => {
                        match index {
                            0 => e.ner = None,
                            1 => e.ocr_mobile = None,
                            _ => e.ocr_accurate = None,
                        }
                        errors[index].push(format!("worker {worker_index}: {error}"));
                    }
                }
            }
        }
        let capability = |index: usize, id: &str, label: &str| {
            let location = root.join("models").join(id);
            let manifest = packages[id].as_ref().ok().map(|package| package.manifest());
            CapabilityStatus {
                id: id.into(),
                label: label.into(),
                installed: location.join("manifest.json").is_file(),
                ready: ready_workers[index] == worker_limit,
                version: manifest.map(|manifest| manifest.version.clone()),
                bytes: manifest.map_or(0, |manifest| {
                    manifest.files.iter().map(|file| file.size).sum()
                }),
                location: location.display().to_string(),
                error: (!errors[index].is_empty()).then(|| errors[index].join("；")),
            }
        };
        let capabilities = vec![
            capability(0, "raner-v1", "中文实体识别"),
            capability(1, "ppocrv4-mobile-v1", "轻量 OCR"),
            capability(2, "ppocrv4-accurate-v1", "高精度 OCR"),
        ];
        let mut model = status.lock().map_err(poisoned)?;
        model.ready = capabilities[0].ready;
        model.version = capabilities[0].version.clone();
        model.bytes = capabilities.iter().map(|item| item.bytes).sum();
        model.error = capabilities[0].error.clone();
        model.capabilities = capabilities;
        let _ = app.emit("model-progress", &*model);
        Ok(model.clone())
    })
    .await
    .map_err(|error| Error::Io(error.to_string()))?
}

async fn install_package(state: &AppState, app: &tauri::AppHandle, package_id: &str) -> Result<()> {
    let catalog = catalog(state).await?;
    let package = catalog
        .packages
        .iter()
        .find(|package| package.id == package_id)
        .cloned()
        .ok_or_else(|| Error::Invalid("模型包不存在".into()))?;
    let cancellation = Cancellation::default();
    {
        let mut installs = state.installs.lock().map_err(poisoned)?;
        if installs.contains_key(package_id) {
            return Err(Error::State("该模型正在下载".into()));
        }
        installs.insert(package_id.to_owned(), cancellation.clone());
    }
    let root = primary(state)?
        .engine
        .lock()
        .map_err(poisoned)?
        .store
        .root
        .clone();
    let app_handle = app.clone();
    let result = model_manager::download_and_install(
        &state.client,
        &package,
        &root,
        &cancellation,
        move |progress| {
            let _ = app_handle.emit("model-progress", progress);
        },
    )
    .await;
    state.installs.lock().map_err(poisoned)?.remove(package_id);
    if let Err(error) = &result {
        let stage = if cancellation.is_cancelled() {
            "cancelled"
        } else {
            "failed"
        };
        let _ = app.emit(
            "model-progress",
            serde_json::json!({
                "id": package_id,
                "stage": stage,
                "current": 0,
                "total": package.size,
                "percent": 0.0,
                "message": error.to_string()
            }),
        );
    }
    result.map(|_| ())
}

#[tauri::command]
async fn model_packages(state: Shared<'_>) -> Result<Vec<PackageStatus>> {
    state.auth.require_authenticated()?;
    let catalog = catalog(&state).await?;
    let root = primary(&state)?
        .engine
        .lock()
        .map_err(poisoned)?
        .store
        .root
        .clone();
    let runtime = state.model.lock().map_err(poisoned)?.clone();
    Ok(catalog
        .packages
        .iter()
        .map(|package| {
            let capability = runtime
                .capabilities
                .iter()
                .find(|capability| capability.id == package.id);
            PackageStatus {
                id: package.id.clone(),
                version: package.version.clone(),
                profile: package.profile.clone(),
                size: package.size,
                location: root.join("models").join(&package.id).display().to_string(),
                installed: root
                    .join("models")
                    .join(&package.id)
                    .join("manifest.json")
                    .is_file(),
                ready: capability.is_some_and(|capability| capability.ready),
                error: capability.and_then(|capability| capability.error.clone()),
            }
        })
        .collect())
}

#[tauri::command]
async fn ensure_default_models(
    state: Shared<'_>,
    app: tauri::AppHandle,
) -> Result<RuntimeModelStatus> {
    state.auth.require_authenticated()?;
    let _guard = state.model_operations.lock().await;
    let current = state.model.lock().map_err(poisoned)?.clone();
    if current.ready
        && current
            .capabilities
            .iter()
            .any(|capability| capability.id == "ppocrv4-mobile-v1" && capability.ready)
    {
        return Ok(current);
    }
    let root = primary(&state)?
        .engine
        .lock()
        .map_err(poisoned)?
        .store
        .root
        .clone();
    let mut verified = HashMap::new();
    for id in ["raner-v1", "ppocrv4-mobile-v1"] {
        let model = match verify_installed(&root, id, false) {
            Ok(model) => model,
            Err(_) => {
                install_package(&state, &app, id).await?;
                verify_installed(&root, id, false)?
            }
        };
        verified.insert(id.to_owned(), model);
    }
    let result = reload_models(&state, app.clone(), verified.clone(), true, false, 1).await?;
    if result.ready && state.desired_workers.load(Ordering::Acquire) > 1 {
        tauri::async_runtime::spawn(async move {
            let state = app.state::<AppState>();
            let _guard = state.model_operations.lock().await;
            let desired = state.desired_workers.load(Ordering::Acquire);
            if desired <= state.worker_limit.load(Ordering::Acquire) {
                return;
            }
            let warmed =
                reload_models(&state, app.clone(), verified.clone(), true, false, desired).await;
            if warmed.as_ref().is_ok_and(|status| {
                status.ready
                    && status
                        .capabilities
                        .iter()
                        .any(|item| item.id == "ppocrv4-mobile-v1" && item.ready)
            }) {
                let _ = activate_workers(&state, desired);
            } else {
                let _ = reload_models(&state, app.clone(), verified, true, false, 1).await;
            }
        });
    }
    Ok(result)
}

#[tauri::command]
async fn install_model(
    state: Shared<'_>,
    app: tauri::AppHandle,
    package_id: String,
) -> Result<RuntimeModelStatus> {
    state.auth.require_authenticated()?;
    let _guard = state.model_operations.lock().await;
    install_package(&state, &app, &package_id).await?;
    for worker in state.engines.iter() {
        let mut engine = worker.engine.lock().map_err(poisoned)?;
        match package_id.as_str() {
            "raner-v1" => engine.ner = None,
            "ppocrv4-mobile-v1" => engine.ocr_mobile = None,
            "ppocrv4-accurate-v1" => engine.ocr_accurate = None,
            _ => {}
        }
    }
    reload_models(
        &state,
        app,
        HashMap::new(),
        true,
        false,
        state.worker_limit.load(Ordering::Acquire),
    )
    .await
}

fn clear_installed_model(root: &std::path::Path, id: &str) -> Result<()> {
    if !matches!(id, "raner-v1" | "ppocrv4-mobile-v1" | "ppocrv4-accurate-v1") {
        return Err(Error::Invalid("未知的模型 ID".into()));
    }
    let models = root.join("models");
    if !models.exists() {
        return Ok(());
    }
    if std::fs::symlink_metadata(&models)?.file_type().is_symlink() {
        return Err(Error::Invalid("模型根目录不能是链接".into()));
    }
    let target = models.join(id);
    match std::fs::symlink_metadata(&target) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(Error::Invalid("模型目录无效，无法重建".into()));
            }
            std::fs::remove_dir_all(target)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(Error::Io(error.to_string())),
    }
    Ok(())
}

#[tauri::command]
async fn rebuild_model(
    state: Shared<'_>,
    app: tauri::AppHandle,
    package_id: String,
) -> Result<RuntimeModelStatus> {
    state.auth.require_authenticated()?;
    let _guard = state.model_operations.lock().await;
    let catalog = catalog(&state).await?;
    if !catalog
        .packages
        .iter()
        .any(|package| package.id == package_id)
    {
        return Err(Error::Invalid("未知的模型 ID".into()));
    }
    let permits = state.permits.lock().map_err(poisoned)?.clone();
    let _idle = permits
        .acquire_many_owned(state.worker_limit.load(Ordering::Acquire) as u32)
        .await
        .map_err(|_| Error::State("任务调度器已关闭".into()))?;
    let root = primary(&state)?
        .engine
        .lock()
        .map_err(poisoned)?
        .store
        .root
        .clone();
    for worker in state.engines.iter() {
        let mut engine = worker.engine.lock().map_err(poisoned)?;
        match package_id.as_str() {
            "raner-v1" => engine.ner = None,
            "ppocrv4-mobile-v1" => engine.ocr_mobile = None,
            "ppocrv4-accurate-v1" => engine.ocr_accurate = None,
            _ => unreachable!(),
        }
    }
    if let Err(error) = clear_installed_model(&root, &package_id) {
        let _ = reload_models(
            &state,
            app,
            HashMap::new(),
            true,
            false,
            state.worker_limit.load(Ordering::Acquire),
        )
        .await;
        return Err(error);
    }
    {
        let mut status = state.model.lock().map_err(poisoned)?;
        if let Some(item) = status
            .capabilities
            .iter_mut()
            .find(|item| item.id == package_id)
        {
            item.ready = false;
            item.installed = false;
            item.error = Some("正在重新下载模型".into());
        }
        if package_id == "raner-v1" {
            status.ready = false;
            status.error = Some("正在重新下载模型".into());
        }
        let _ = app.emit("model-progress", &*status);
    }
    let installed = install_package(&state, &app, &package_id).await;
    let refreshed = reload_models(
        &state,
        app,
        HashMap::new(),
        true,
        false,
        state.worker_limit.load(Ordering::Acquire),
    )
    .await?;
    installed?;
    Ok(refreshed)
}

#[tauri::command]
async fn cancel_model_install(state: Shared<'_>, package_id: String) -> Result<()> {
    state.auth.require_authenticated()?;
    let cancellation = state
        .installs
        .lock()
        .map_err(poisoned)?
        .get(&package_id)
        .cloned()
        .ok_or_else(|| Error::State("该模型没有正在进行的下载".into()))?;
    cancellation.cancel();
    Ok(())
}

#[tauri::command]
async fn load_models(state: Shared<'_>, app: tauri::AppHandle) -> Result<RuntimeModelStatus> {
    state.auth.require_authenticated()?;
    let _guard = state.model_operations.lock().await;
    let desired = state.desired_workers.load(Ordering::Acquire);
    let result = reload_models(&state, app, HashMap::new(), false, true, desired).await?;
    if result.ready {
        activate_workers(&state, desired)?;
    }
    Ok(result)
}
fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let root = app.path().local_data_dir()?.join("LocalDesensitization");
            let key = storage::credential_key()?;
            let store = storage::Store::open(&root, key.clone())?;
            let concurrency = store.settings()?.concurrency;
            let dir = root.join("models/raner-v1");
            let active_ocr = Arc::new(Mutex::new(HashMap::new()));
            let engines = (0..4)
                .map(|_| {
                    storage::Store::open(&root, key.clone()).map(|store| {
                        Arc::new(Worker {
                            engine: Mutex::new(Engine {
                                store,
                                ner: None,
                                ocr_mobile: None,
                                ocr_accurate: None,
                                active_ocr: active_ocr.clone(),
                            }),
                            busy: std::sync::atomic::AtomicBool::new(false),
                        })
                    })
                })
                .collect::<std::result::Result<Vec<_>, _>>()?;
            app.manage(AppState {
                auth: auth::AuthManager::new(&root, key.clone())?,
                engines: Arc::new(engines),
                next_engine: AtomicUsize::new(0),
                worker_limit: AtomicUsize::new(1),
                desired_workers: AtomicUsize::new(concurrency as usize),
                model: Arc::new(Mutex::new(RuntimeModelStatus {
                    ready: false,
                    version: None,
                    location: dir.display().to_string(),
                    bytes: 0,
                    error: Some("模型尚未加载".into()),
                    capabilities: vec![
                        CapabilityStatus {
                            id: "raner-v1".into(),
                            label: "中文实体识别".into(),
                            installed: dir.join("manifest.json").is_file(),
                            ready: false,
                            version: None,
                            bytes: 0,
                            location: dir.display().to_string(),
                            error: None,
                        },
                        CapabilityStatus {
                            id: "ppocrv4-mobile-v1".into(),
                            label: "轻量 OCR".into(),
                            installed: root
                                .join("models/ppocrv4-mobile-v1/manifest.json")
                                .is_file(),
                            ready: false,
                            version: None,
                            bytes: 0,
                            location: root.join("models/ppocrv4-mobile-v1").display().to_string(),
                            error: None,
                        },
                        CapabilityStatus {
                            id: "ppocrv4-accurate-v1".into(),
                            label: "高精度 OCR".into(),
                            installed: root
                                .join("models/ppocrv4-accurate-v1/manifest.json")
                                .is_file(),
                            ready: false,
                            version: None,
                            bytes: 0,
                            location: root
                                .join("models/ppocrv4-accurate-v1")
                                .display()
                                .to_string(),
                            error: None,
                        },
                    ],
                })),
                active_ocr,
                catalog: Mutex::new(None),
                model_operations: tokio::sync::Mutex::new(()),
                installs: Mutex::new(HashMap::new()),
                permits: Mutex::new(Arc::new(tokio::sync::Semaphore::new(1))),
                client: reqwest::Client::builder()
                    .https_only(true)
                    .connect_timeout(std::time::Duration::from_secs(15))
                    .timeout(std::time::Duration::from_secs(30 * 60))
                    .user_agent("Sixa/1.0.7")
                    .build()
                    .map_err(|error| error.to_string())?,
                integration_jobs: integration::JobRegistry::default(),
            });
            integration::start(app.handle().clone())?;
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(600));
                interval.tick().await;
                loop {
                    interval.tick().await;
                    let status = {
                        let state = app_handle.state::<AppState>();
                        state.auth.heartbeat().await
                    };
                    match status {
                        Ok(status) => {
                            let _ = app_handle.emit("auth-state-changed", status);
                        }
                        Err(error) => {
                            let _ = app_handle.emit(
                                "auth-state-changed",
                                serde_json::json!({
                                    "authenticated": false,
                                    "offline": false,
                                    "reason": error.to_string()
                                }),
                            );
                        }
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            auth_status,
            auth_login,
            auth_logout,
            initialize,
            analyze_file,
            task_view,
            select_entities,
            preview,
            document_preview,
            document_result_preview,
            upsert_region,
            remove_region,
            execute,
            export_task,
            export_recovery,
            restore,
            list_tasks,
            delete_task,
            cancel_task,
            create_batch,
            batch_view,
            execute_batch,
            export_batch,
            list_rules,
            save_rule,
            delete_rule,
            list_policies,
            builtin_policies,
            save_policy,
            get_settings,
            save_settings,
            model_status,
            model_packages,
            ensure_default_models,
            install_model,
            rebuild_model,
            cancel_model_install,
            load_models,
            integration::integration_info,
            integration::integration_check
        ])
        .run(tauri::generate_context!())
        .expect("无法启动私匣");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebuild_removes_only_the_selected_known_model() {
        let root = tempfile::tempdir().unwrap();
        let models = root.path().join("models");
        std::fs::create_dir_all(models.join("raner-v1")).unwrap();
        std::fs::create_dir_all(models.join("ppocrv4-mobile-v1")).unwrap();
        std::fs::write(models.join("raner-v1/manifest.json"), b"old").unwrap();
        std::fs::write(models.join("ppocrv4-mobile-v1/manifest.json"), b"keep").unwrap();
        assert!(clear_installed_model(root.path(), "../../outside").is_err());
        clear_installed_model(root.path(), "raner-v1").unwrap();
        assert!(!models.join("raner-v1").exists());
        assert_eq!(
            std::fs::read(models.join("ppocrv4-mobile-v1/manifest.json")).unwrap(),
            b"keep"
        );
    }
}
