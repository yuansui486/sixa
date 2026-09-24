#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod activity;
mod auth;
mod batch_jobs;
#[cfg(windows)]
mod integration;
#[cfg(not(windows))]
#[path = "integration_non_windows.rs"]
mod integration;
mod lifecycle;
mod preview_jobs;
mod runtime;
mod scheduler;

use domain::{
    AppSettings, Error, Policy, PreviewDto, RegionDto, Result, Rule, Selection, TaskMeta,
    TaskOptions,
};
use model_manager::{Cancellation, Catalog};
use serde::Serialize;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::atomic::AtomicU8,
    sync::{Arc, Mutex},
};
use task_engine::batch::BatchView;
use task_engine::{Engine, RegionMutationAck, TaskView};
use tauri::{Emitter, Manager, State};
use uuid::Uuid;
use zeroize::Zeroizing;

struct AppState {
    desktop: Arc<activity::Activity>,
    previews: Arc<preview_jobs::PreviewJobs>,
    auth: auth::AuthManager,
    root: PathBuf,
    control: Arc<Mutex<Engine>>,
    preview_engine: Arc<Mutex<Engine>>,
    engines: Arc<Vec<Arc<Worker>>>,
    scheduler: Arc<scheduler::Scheduler>,
    model: Arc<Mutex<RuntimeModelStatus>>,
    active_ocr: Arc<Mutex<HashMap<Uuid, recognition::ocr::OcrRun>>>,
    catalog: Mutex<Option<Catalog>>,
    model_operations: tokio::sync::Mutex<()>,
    installs: Mutex<HashMap<String, Cancellation>>,
    client: reqwest::Client,
    integration_jobs: integration::JobRegistry,
}
struct Worker {
    engine: Mutex<Engine>,
    loaded: AtomicU8,
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
    let activity = state.desktop.track()?;
    let engine = state.control.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _activity = activity;
        let mut guard = engine.lock().map_err(poisoned)?;
        f(&mut guard)
    })
    .await
    .map_err(|e| Error::Io(e.to_string()))?
}
async fn job<T: Send + 'static>(
    state: Shared<'_>,
    f: impl FnOnce(&mut Engine) -> Result<T> + Send + 'static,
) -> Result<T> {
    scheduled_job(state, None, runtime::Requirements::none(), None, f).await
}
async fn preview_work<T: Send + 'static>(
    state: Shared<'_>,
    f: impl FnOnce(&Engine) -> Result<T> + Send + 'static,
) -> Result<T> {
    state.auth.require_authenticated()?;
    let activity = state.desktop.admit()?;
    let engine = state.preview_engine.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _activity = activity;
        let guard = engine.lock().map_err(poisoned)?;
        f(&guard)
    })
    .await
    .map_err(|error| Error::Io(error.to_string()))?
}
async fn scheduled_job<T: Send + 'static>(
    state: Shared<'_>,
    app: Option<tauri::AppHandle>,
    requirements: runtime::Requirements,
    run: Option<recognition::ocr::OcrRun>,
    f: impl FnOnce(&mut Engine) -> Result<T> + Send + 'static,
) -> Result<T> {
    state.auth.require_authenticated()?;
    let activity = state.desktop.admit()?;
    let lease = state
        .scheduler
        .acquire_with_budget(run.as_ref(), requirements.uses_raster_budget())
        .await?;
    // Authentication may expire while a job is queued.
    state.auth.require_authenticated()?;
    let worker = state.engines[lease.index].clone();
    let engines = state.engines.clone();
    let model = state.model.clone();
    let root = state.root.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _activity = activity;
        let _lease = lease;
        if run
            .as_ref()
            .is_some_and(recognition::ocr::OcrRun::is_cancelled)
        {
            return Err(Error::State("任务已取消".into()));
        }
        let mut guard = worker.engine.lock().map_err(poisoned)?;
        runtime::ensure(
            runtime::LoadContext {
                root: &root,
                engines: &engines,
                status: &model,
                app: app.as_ref(),
            },
            &mut guard,
            &worker,
            requirements,
            run.as_ref(),
            false,
        )?;
        f(&mut guard)
    })
    .await
    .map_err(|error| Error::Io(error.to_string()))?
}
fn register_run(state: &AppState, id: Uuid) -> Result<recognition::ocr::OcrRun> {
    let run = recognition::ocr::OcrRun::new()?;
    let mut active = state.active_ocr.lock().map_err(poisoned)?;
    if state.desktop.is_stopping() {
        return Err(Error::State("应用正在退出，不能启动新的任务".into()));
    }
    if active.contains_key(&id) {
        return Err(Error::Conflict("该任务已在运行".into()));
    }
    active.insert(id, run.clone());
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
    state: String,
    ready_workers: usize,
    operation_id: Option<Uuid>,
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
    state: String,
    ready_workers: usize,
    operation_id: Option<Uuid>,
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
            tasks: e.store.task_page(storage::TaskQuery::default())?.items,
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
    let _activity = state.desktop.admit()?;
    let id = request_id.unwrap_or_else(Uuid::new_v4);
    let run = register_run(&state, id)?;
    let _ = app.emit(
        "task-progress",
        serde_json::json!({"id":id,"stage":"queued"}),
    );
    let options = match options {
        Some(options) => options,
        None => {
            work(state.clone(), |engine| {
                let settings = engine.store.settings()?;
                Ok(TaskOptions {
                    ocr_profile: settings.ocr_profile,
                    pdf_mode: settings.pdf_mode,
                })
            })
            .await?
        }
    };
    let requirements = runtime::Requirements::for_file(&path, &options);
    let result = scheduled_job(
        state.clone(),
        Some(app.clone()),
        requirements,
        Some(run.clone()),
        move |e| e.analyze_file_as(&path, options, id, run),
    )
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
    preview_work(state, move |e| e.document_preview(id)).await
}
#[tauri::command]
async fn document_result_preview(state: Shared<'_>, id: Uuid) -> Result<PreviewDto> {
    preview_work(state, move |e| e.document_result_preview(id)).await
}
#[tauri::command]
async fn document_manifest(state: Shared<'_>, id: Uuid, result: bool) -> Result<PreviewDto> {
    preview_work(state, move |engine| engine.document_manifest(id, result)).await
}
#[tauri::command]
async fn document_page(
    state: Shared<'_>,
    id: Uuid,
    result: bool,
    page: u32,
    max_dimension: u32,
    expected_revision: u64,
) -> Result<tauri::ipc::Response> {
    scheduled_job(
        state,
        None,
        runtime::Requirements::render(),
        None,
        move |engine| {
            engine
                .document_page(id, result, page, max_dimension, expected_revision)
                .map(tauri::ipc::Response::new)
        },
    )
    .await
}
#[tauri::command]
async fn document_draft_page(
    state: Shared<'_>,
    id: Uuid,
    page: u32,
    max_dimension: u32,
    expected_revision: u64,
    request_id: Uuid,
) -> Result<tauri::ipc::Response> {
    state.auth.require_authenticated()?;
    let ticket = state.previews.start(request_id)?;
    let run = ticket.run();
    scheduled_job(
        state,
        None,
        runtime::Requirements::render(),
        Some(run),
        move |engine| {
            ticket.check()?;
            let bytes =
                engine.draft_page(id, page, max_dimension, expected_revision, &mut || {
                    ticket.check()
                })?;
            ticket.check()?;
            Ok(tauri::ipc::Response::new(bytes))
        },
    )
    .await
}
#[tauri::command]
fn cancel_document_preview(state: Shared<'_>, request_id: Uuid) -> Result<()> {
    state.previews.cancel(request_id)
}
#[tauri::command]
async fn office_preview(
    state: Shared<'_>,
    id: Uuid,
    result: bool,
    expected_revision: u64,
) -> Result<domain::OfficePreview> {
    preview_work(state, move |engine| {
        engine.office_preview(id, result, expected_revision)
    })
    .await
}
#[tauri::command]
async fn office_preview_docx(
    state: Shared<'_>,
    id: Uuid,
    result: bool,
    expected_revision: u64,
) -> Result<tauri::ipc::Response> {
    preview_work(state, move |engine| {
        engine
            .office_preview_docx(id, result, expected_revision)
            .map(tauri::ipc::Response::new)
    })
    .await
}
#[tauri::command]
async fn review_patch(
    state: Shared<'_>,
    id: Uuid,
    selections: Vec<Selection>,
    expected_revision: u64,
    mutation_id: Uuid,
) -> Result<TaskView> {
    work(state, move |engine| {
        engine.review_patch(id, selections, expected_revision, mutation_id)
    })
    .await
}
#[tauri::command]
async fn confirm_review(state: Shared<'_>, id: Uuid, expected_revision: u64) -> Result<TaskView> {
    work(state, move |engine| {
        engine.confirm_review(id, expected_revision)
    })
    .await
}
#[tauri::command]
async fn clone_for_review(state: Shared<'_>, id: Uuid) -> Result<TaskView> {
    work(state, move |engine| engine.clone_for_review(id)).await
}
#[tauri::command]
async fn retry_task(state: Shared<'_>, app: tauri::AppHandle, id: Uuid) -> Result<TaskView> {
    let _activity = state.desktop.admit()?;
    let source = work(state.clone(), move |engine| engine.view(id)).await?;
    let new_id = Uuid::new_v4();
    let run = register_run(&state, new_id)?;
    let requirements = runtime::Requirements::for_file(
        &PathBuf::from(format!("source.{}", source.extension)),
        &source.options,
    );
    let result = scheduled_job(
        state.clone(),
        Some(app.clone()),
        requirements,
        Some(run.clone()),
        move |engine| engine.retry_saved_as(id, new_id, run),
    )
    .await;
    unregister_run(&state, new_id)?;
    finish_task(&app, new_id, result)
}
#[tauri::command]
async fn upsert_region(
    state: Shared<'_>,
    id: Uuid,
    region: RegionDto,
    expected_revision: u64,
    mutation_id: Option<Uuid>,
) -> Result<RegionMutationAck> {
    work(state, move |e| {
        e.upsert_region_with_mutation(id, region, expected_revision, mutation_id)
    })
    .await
}
#[tauri::command]
async fn remove_region(
    state: Shared<'_>,
    id: Uuid,
    region_id: Uuid,
    expected_revision: u64,
    mutation_id: Option<Uuid>,
) -> Result<RegionMutationAck> {
    work(state, move |e| {
        e.remove_region_with_mutation(id, region_id, expected_revision, mutation_id)
    })
    .await
}
#[tauri::command]
async fn execute(state: Shared<'_>, app: tauri::AppHandle, id: Uuid) -> Result<TaskView> {
    state.auth.require_authenticated()?;
    let _activity = state.desktop.admit()?;
    let run = register_run(&state, id)?;
    let _ = app.emit(
        "task-progress",
        serde_json::json!({"id":id,"stage":"processing"}),
    );
    let result = scheduled_job(
        state.clone(),
        Some(app.clone()),
        runtime::Requirements::render(),
        Some(run.clone()),
        move |e| e.execute_as(id, run),
    )
    .await;
    unregister_run(&state, id)?;
    finish_task(&app, id, result)
}
#[tauri::command]
async fn export_task(state: Shared<'_>, id: Uuid, path: PathBuf) -> Result<()> {
    job(state, move |e| e.export(id, &path)).await
}
#[tauri::command]
async fn export_recovery(
    state: Shared<'_>,
    id: Uuid,
    password: String,
    path: PathBuf,
) -> Result<()> {
    let password = Zeroizing::new(password);
    job(state, move |e| e.export_recovery(id, &password, &path)).await
}
#[tauri::command]
async fn restore(
    state: Shared<'_>,
    source: PathBuf,
    password: String,
    destination: PathBuf,
) -> Result<()> {
    let password = Zeroizing::new(password);
    job(state, move |e| e.restore(&source, &password, &destination)).await
}
#[tauri::command]
async fn list_tasks(state: Shared<'_>) -> Result<Vec<TaskMeta>> {
    work(state, |e| e.store.tasks()).await
}
#[derive(Serialize)]
struct TaskPage {
    items: Vec<TaskMeta>,
    total: u64,
    offset: u32,
    limit: u32,
}
#[tauri::command]
async fn query_tasks(state: Shared<'_>, query: storage::TaskQuery) -> Result<TaskPage> {
    let offset = query.offset;
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    work(state, move |engine| {
        let page = engine.store.task_page(query)?;
        Ok(TaskPage {
            items: page.items,
            total: page.total,
            offset,
            limit,
        })
    })
    .await
}
#[tauri::command]
async fn reveal_file(state: Shared<'_>, path: PathBuf) -> Result<()> {
    state.auth.require_authenticated()?;
    tauri::async_runtime::spawn_blocking(move || {
        let path = path.canonicalize()?;
        #[cfg(windows)]
        {
            let mut arg = std::ffi::OsString::from("/select,");
            arg.push(&path);
            std::process::Command::new("explorer.exe")
                .arg(arg)
                .spawn()?;
        }
        #[cfg(target_os = "macos")]
        {
            std::process::Command::new("open")
                .arg("-R")
                .arg(&path)
                .spawn()?;
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            std::process::Command::new("xdg-open")
                .arg(path.parent().unwrap_or(&path))
                .spawn()?;
        }
        Ok(())
    })
    .await
    .map_err(|error| Error::Io(error.to_string()))?
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
    let _activity = state.desktop.admit()?;
    let id = request_id.unwrap_or_else(Uuid::new_v4);
    let run = register_run(&state, id)?;
    let _ = app.emit(
        "batch-progress",
        serde_json::json!({"id":id,"done":0,"total":paths.len(),"stage":"queued"}),
    );
    let prepared = work(state.clone(), move |engine| engine.begin_batch(paths, id)).await;
    let result = match prepared {
        Ok(_) => batch_jobs::analyze(state.clone(), app.clone(), id, run).await,
        Err(error) => Err(error),
    };
    unregister_run(&state, id)?;
    finish_batch(&app, id, result)
}
#[tauri::command]
async fn batch_view(state: Shared<'_>, id: Uuid) -> Result<BatchView> {
    work(state, move |e| e.batch_view(id)).await
}
#[tauri::command]
async fn execute_batch(state: Shared<'_>, app: tauri::AppHandle, id: Uuid) -> Result<BatchView> {
    let _activity = state.desktop.admit()?;
    state.auth.require_authenticated()?;
    let run = register_run(&state, id)?;
    let _ = app.emit(
        "batch-progress",
        serde_json::json!({"id":id,"stage":"queued"}),
    );
    let result = batch_jobs::execute(state.clone(), app.clone(), id, run).await;
    unregister_run(&state, id)?;
    finish_batch(&app, id, result)
}
#[tauri::command]
async fn retry_batch(
    state: Shared<'_>,
    app: tauri::AppHandle,
    id: Uuid,
    failed_only: bool,
    request_id: Option<Uuid>,
) -> Result<BatchView> {
    state.auth.require_authenticated()?;
    let _activity = state.desktop.admit()?;
    let new_id = request_id.unwrap_or_else(Uuid::new_v4);
    let run = register_run(&state, new_id)?;
    let _ = app.emit(
        "batch-progress",
        serde_json::json!({"id":new_id,"source_id":id,"stage":"queued"}),
    );
    let prepared = work(state.clone(), move |engine| {
        engine.prepare_retry_batch_as(id, failed_only, new_id)
    })
    .await;
    let result = match prepared {
        Ok(_) => batch_jobs::analyze(state.clone(), app.clone(), new_id, run).await,
        Err(error) => Err(error),
    };
    unregister_run(&state, new_id)?;
    finish_batch(&app, new_id, result)
}
#[tauri::command]
async fn export_batch(state: Shared<'_>, id: Uuid, path: PathBuf) -> Result<()> {
    job(state, move |e| e.export_batch(id, &path)).await
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
async fn test_rule(state: Shared<'_>, rule: Rule, text: String) -> Result<Vec<domain::EntityDto>> {
    state.auth.require_authenticated()?;
    if text.len() > 64 * 1024 {
        return Err(Error::Invalid("测试文本不得超过 64 KB".into()));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let mut matches = Vec::new();
        for regex in recognition::compile_rule(&rule)? {
            for found in regex.find_iter(&text) {
                if matches.len() >= 1000 {
                    return Err(Error::Invalid("匹配超过 1000 项，请缩小测试文本".into()));
                }
                matches.push(
                    domain::Entity {
                        id: Uuid::new_v4(),
                        entity_type: rule.entity_type.clone(),
                        score: 1.0,
                        source: "规则测试".into(),
                        selected: true,
                        span: domain::Span {
                            start: found.start(),
                            end: found.end(),
                        },
                        replacement: None,
                    }
                    .dto(&text)?,
                );
            }
        }
        Ok(matches)
    })
    .await
    .map_err(|error| Error::Io(error.to_string()))?
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
async fn save_settings(state: Shared<'_>, settings: AppSettings) -> Result<AppSettings> {
    let settings = settings.validate()?;
    let saved = settings.clone();
    work(state.clone(), move |engine| {
        engine.store.save_settings(saved)
    })
    .await?;
    state.scheduler.set_limit(settings.concurrency as usize)?;
    Ok(settings)
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
    } else if full {
        recognition::models::verify_model_with_required(&dir, recognition::models::OCR_MODEL_FILES)
    } else {
        recognition::models::inspect_model_with_required(&dir, recognition::models::OCR_MODEL_FILES)
    }
}
/// Maintenance caller holds model_operations and pauses the scheduler first.
async fn prepare_model(
    state: &AppState,
    app: tauri::AppHandle,
    requirements: runtime::Requirements,
    full_verify: bool,
) -> Result<RuntimeModelStatus> {
    let activity = state.desktop.admit()?;
    let engines = state.engines.clone();
    let root = state.root.clone();
    let model = state.model.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _activity = activity;
        let worker = &engines[0];
        let mut engine = worker.engine.lock().map_err(poisoned)?;
        runtime::ensure(
            runtime::LoadContext {
                root: &root,
                engines: &engines,
                status: &model,
                app: Some(&app),
            },
            &mut engine,
            worker,
            requirements,
            None,
            full_verify,
        )?;
        Ok(model.lock().map_err(poisoned)?.clone())
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
        if state.desktop.is_stopping() {
            return Err(Error::State("应用正在退出，不能启动模型下载".into()));
        }
        if installs.contains_key(package_id) {
            return Err(Error::State("该模型正在下载".into()));
        }
        installs.insert(package_id.to_owned(), cancellation.clone());
    }
    let root = state.root.clone();
    let app_handle = app.clone();
    let operation_id = Uuid::new_v4();
    {
        let mut model = state.model.lock().map_err(poisoned)?;
        if let Some(capability) = model
            .capabilities
            .iter_mut()
            .find(|item| item.id == package_id)
        {
            capability.state = "downloading".into();
            capability.operation_id = Some(operation_id);
            capability.error = None;
        }
        let _ = app.emit("model-progress", &*model);
    }
    let result = model_manager::download_and_install(
        &state.client,
        &package,
        &root,
        &cancellation,
        move |progress| {
            if let Ok(mut payload) = serde_json::to_value(progress) {
                payload["operation_id"] = serde_json::json!(operation_id);
                let _ = app_handle.emit("model-progress", payload);
            }
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
                "operation_id": operation_id,
                "stage": stage,
                "current": 0,
                "total": package.size,
                "percent": 0.0,
                "message": error.to_string()
            }),
        );
    }
    {
        let mut model = state.model.lock().map_err(poisoned)?;
        if let Some(capability) = model
            .capabilities
            .iter_mut()
            .find(|item| item.id == package_id)
        {
            capability.installed = verify_installed(&root, package_id, false).is_ok();
            capability.state = if capability.ready {
                "ready"
            } else if capability.installed {
                "installed"
            } else if cancellation.is_cancelled() {
                "missing"
            } else {
                "failed"
            }
            .into();
            capability.error = result.as_ref().err().map(ToString::to_string);
        }
        let _ = app.emit("model-progress", &*model);
    }
    result.map(|_| ())
}

#[tauri::command]
async fn model_packages(state: Shared<'_>) -> Result<Vec<PackageStatus>> {
    state.auth.require_authenticated()?;
    let catalog = catalog(&state).await?;
    let root = state.root.clone();
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
                installed: capability.is_some_and(|capability| capability.installed),
                ready: capability.is_some_and(|capability| capability.ready),
                error: capability.and_then(|capability| capability.error.clone()),
                state: capability.map_or("missing".into(), |capability| capability.state.clone()),
                ready_workers: capability.map_or(0, |capability| capability.ready_workers),
                operation_id: capability.and_then(|capability| capability.operation_id),
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
    let _activity = state.desktop.admit()?;
    let _guard = state.model_operations.lock().await;
    _activity.ensure_current()?;
    for id in ["raner-v1", "ppocrv4-mobile-v1"] {
        _activity.ensure_current()?;
        if verify_installed(&state.root, id, false).is_err() {
            install_package(&state, &app, id).await?;
        }
    }
    // Startup warms one text session. OCR and additional sessions are created on demand.
    if state.model.lock().map_err(poisoned)?.ready {
        return Ok(state.model.lock().map_err(poisoned)?.clone());
    }
    let _idle = state.scheduler.pause().await?;
    _activity.ensure_current()?;
    prepare_model(&state, app, runtime::Requirements::text(), false).await
}
#[tauri::command]
async fn load_model(
    state: Shared<'_>,
    app: tauri::AppHandle,
    package_id: String,
) -> Result<RuntimeModelStatus> {
    state.auth.require_authenticated()?;
    let _activity = state.desktop.admit()?;
    let requirements = runtime::Requirements::package(&package_id)?;
    let _guard = state.model_operations.lock().await;
    _activity.ensure_current()?;
    let _idle = state.scheduler.pause().await?;
    prepare_model(&state, app, requirements, false).await
}
#[tauri::command]
async fn retry_model_load(
    state: Shared<'_>,
    app: tauri::AppHandle,
    package_id: String,
) -> Result<RuntimeModelStatus> {
    load_model(state, app, package_id).await
}
#[tauri::command]
async fn install_model(
    state: Shared<'_>,
    app: tauri::AppHandle,
    package_id: String,
) -> Result<RuntimeModelStatus> {
    state.auth.require_authenticated()?;
    let _activity = state.desktop.admit()?;
    let requirements = runtime::Requirements::package(&package_id)?;
    let _guard = state.model_operations.lock().await;
    _activity.ensure_current()?;
    let _idle = state.scheduler.pause().await?;
    unload_model(&state, app.clone(), Some(package_id.clone())).await?;
    _activity.ensure_current()?;
    install_package(&state, &app, &package_id).await?;
    _activity.ensure_current()?;
    prepare_model(&state, app, requirements, false).await
}
async fn unload_model(
    state: &AppState,
    app: tauri::AppHandle,
    package_id: Option<String>,
) -> Result<()> {
    let activity = state.desktop.track()?;
    let engines = state.engines.clone();
    let model = state.model.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _activity = activity;
        runtime::unload(&engines, &model, &app, package_id.as_deref())
    })
    .await
    .map_err(|error| Error::Io(error.to_string()))?
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
    let _activity = state.desktop.admit()?;
    let requirements = runtime::Requirements::package(&package_id)?;
    let _guard = state.model_operations.lock().await;
    _activity.ensure_current()?;
    let _idle = state.scheduler.pause().await?;
    unload_model(&state, app.clone(), Some(package_id.clone())).await?;
    _activity.ensure_current()?;
    let root = state.root.clone();
    let clear_id = package_id.clone();
    let activity = state.desktop.admit()?;
    tauri::async_runtime::spawn_blocking(move || {
        let _activity = activity;
        clear_installed_model(&root, &clear_id)
    })
    .await
    .map_err(|error| Error::Io(error.to_string()))??;
    _activity.ensure_current()?;
    install_package(&state, &app, &package_id).await?;
    _activity.ensure_current()?;
    prepare_model(&state, app, requirements, false).await
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
    let _activity = state.desktop.admit()?;
    let _guard = state.model_operations.lock().await;
    _activity.ensure_current()?;
    let _idle = state.scheduler.pause().await?;
    for id in runtime::IDS {
        _activity.ensure_current()?;
        if id == "raner-v1"
            || state
                .root
                .join("models")
                .join(id)
                .join("manifest.json")
                .is_file()
        {
            prepare_model(
                &state,
                app.clone(),
                runtime::Requirements::package(id)?,
                true,
            )
            .await?;
        }
    }
    Ok(state.model.lock().map_err(poisoned)?.clone())
}
fn main() {
    let mut context = tauri::generate_context!();
    // Debug-only native UI tests use a separate instance, profile and data root.
    // Release builds never read this environment variable.
    #[cfg(debug_assertions)]
    let desktop_test_root = std::env::var_os("SIXA_DESKTOP_TEST_ROOT").map(PathBuf::from);
    #[cfg(not(debug_assertions))]
    let desktop_test_root: Option<PathBuf> = None;
    if desktop_test_root.is_some() {
        context.config_mut().identifier = "cn.shierkeji.sixa.desktop-test".into();
        #[cfg(debug_assertions)]
        if std::env::var_os("SIXA_DESKTOP_TEST_NO_UI").is_some() {
            for window in &mut context.config_mut().app.windows {
                window.url =
                    tauri::WebviewUrl::External("about:blank".parse().expect("static test URL"));
            }
        }
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            lifecycle::restore(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            let test_mode = desktop_test_root.is_some();
            let root = desktop_test_root
                .clone()
                .unwrap_or(app.path().local_data_dir()?.join("LocalDesensitization"));
            let key = if test_mode {
                Zeroizing::new([197; 32])
            } else {
                storage::credential_key()?
            };
            let store = storage::Store::open(&root, key.clone())?;
            let concurrency = store.settings()?.concurrency;
            app.manage(lifecycle::Lifecycle::new(storage::Store::connect(
                &root,
                key.clone(),
            )?)?);
            let active_ocr = Arc::new(Mutex::new(HashMap::new()));
            let engines = (0..4)
                .map(|_| {
                    storage::Store::connect(&root, key.clone()).map(|store| {
                        Arc::new(Worker {
                            engine: Mutex::new(Engine {
                                store,
                                ner: None,
                                ocr_mobile: None,
                                ocr_accurate: None,
                                active_ocr: active_ocr.clone(),
                            }),
                            loaded: AtomicU8::new(0),
                        })
                    })
                })
                .collect::<std::result::Result<Vec<_>, _>>()?;
            app.manage(AppState {
                desktop: Arc::new(activity::Activity::default()),
                previews: Arc::new(preview_jobs::PreviewJobs::default()),
                auth: auth::AuthManager::new(&root, key.clone())?,
                root: root.clone(),
                control: Arc::new(Mutex::new(Engine {
                    store,
                    ner: None,
                    ocr_mobile: None,
                    ocr_accurate: None,
                    active_ocr: active_ocr.clone(),
                })),
                preview_engine: Arc::new(Mutex::new(Engine {
                    store: storage::Store::connect(&root, key.clone())?,
                    ner: None,
                    ocr_mobile: None,
                    ocr_accurate: None,
                    active_ocr: active_ocr.clone(),
                })),
                engines: Arc::new(engines),
                scheduler: scheduler::Scheduler::new(4, concurrency as usize),
                model: Arc::new(Mutex::new(runtime::initial(&root))),
                active_ocr,
                catalog: Mutex::new(None),
                model_operations: tokio::sync::Mutex::new(()),
                installs: Mutex::new(HashMap::new()),
                client: reqwest::Client::builder()
                    .https_only(true)
                    .connect_timeout(std::time::Duration::from_secs(15))
                    .timeout(std::time::Duration::from_secs(30 * 60))
                    .user_agent("Sixa/1.0.8")
                    .build()
                    .map_err(|error| error.to_string())?,
                integration_jobs: integration::JobRegistry::default(),
            });
            lifecycle::install_tray(app.handle());
            if !test_mode {
                integration::start(app.handle().clone())?;
            }
            let idle_app = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    let state = idle_app.state::<AppState>();
                    if !state
                        .scheduler
                        .idle_for(std::time::Duration::from_secs(15 * 60))
                    {
                        continue;
                    }
                    let Ok(_guard) = state.model_operations.try_lock() else {
                        continue;
                    };
                    let Ok(Some(_idle)) = state
                        .scheduler
                        .pause_if_idle(std::time::Duration::from_secs(15 * 60))
                    else {
                        continue;
                    };
                    let _ = unload_model(&state, idle_app.clone(), None).await;
                }
            });
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
            lifecycle::get_desktop_preferences,
            lifecycle::set_desktop_preferences,
            lifecycle::get_close_request,
            lifecycle::acknowledge_app_close,
            lifecycle::respond_app_close,
            auth_status,
            auth_login,
            auth_logout,
            initialize,
            analyze_file,
            task_view,
            select_entities,
            review_patch,
            confirm_review,
            clone_for_review,
            retry_task,
            preview,
            document_preview,
            document_result_preview,
            document_manifest,
            document_page,
            document_draft_page,
            cancel_document_preview,
            office_preview,
            office_preview_docx,
            upsert_region,
            remove_region,
            execute,
            export_task,
            export_recovery,
            restore,
            list_tasks,
            query_tasks,
            reveal_file,
            delete_task,
            cancel_task,
            create_batch,
            batch_view,
            execute_batch,
            retry_batch,
            export_batch,
            list_rules,
            save_rule,
            test_rule,
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
            load_model,
            retry_model_load,
            integration::integration_info,
            integration::integration_check
        ])
        .build(context)
        .expect("无法启动私匣")
        .run(|app, event| match event {
            tauri::RunEvent::WindowEvent {
                label,
                event: tauri::WindowEvent::CloseRequested { api, .. },
                ..
            } if label == "main" => {
                if !app.state::<lifecycle::Lifecycle>().exiting() {
                    api.prevent_close();
                    lifecycle::request_close(app, false);
                }
            }
            tauri::RunEvent::ExitRequested { api, .. }
                if !app.state::<lifecycle::Lifecycle>().exiting() =>
            {
                api.prevent_exit();
                lifecycle::request_close(app, true);
            }
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { .. } => lifecycle::restore(app),
            _ => {}
        });
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
