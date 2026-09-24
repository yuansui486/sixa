use crate::{
    AppState, batch_jobs, job, poisoned, register_run, runtime, scheduled_job, unregister_run, work,
};
use domain::{Error, Result, TaskOptions, TaskState};
use integration_protocol::{
    CancelJobResult, CreateJobResult, DesensitizeBatchParams, DesensitizeFileParams, ErrorCode,
    IntegrationError, JobParams, JobResult, JobState, MAX_FRAME_BYTES, Method, PIPE_PREFIX,
    PROTOCOL_VERSION, Request, Response, StatusResult, WaitJobParams,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    ffi::c_void,
    fs::OpenOptions,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tauri::{AppHandle, Manager, State};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use uuid::Uuid;

const SUPPORTED_FORMATS: &[&str] = &[
    "txt", "md", "docx", "xlsx", "xlsm", "pdf", "png", "jpg", "jpeg", "bmp", "tif", "tiff",
];
const MAX_TERMINAL_JOBS: usize = 256;

#[derive(Clone, Default)]
pub struct JobRegistry {
    inner: Arc<Mutex<RegistryInner>>,
}

#[derive(Default)]
struct RegistryInner {
    jobs: HashMap<Uuid, Arc<JobEntry>>,
    terminal_order: VecDeque<Uuid>,
}

struct JobEntry {
    snapshot: Mutex<IntegrationJob>,
    changed: tokio::sync::Notify,
    cancellation: recognition::ocr::OcrRun,
}

#[derive(Clone)]
struct IntegrationJob {
    entity_counts: BTreeMap<String, usize>,
    result: JobResult,
}

impl JobRegistry {
    fn insert(
        &self,
        id: Uuid,
        _kind: &'static str,
        cancellation: recognition::ocr::OcrRun,
    ) -> Result<()> {
        let snapshot = IntegrationJob {
            entity_counts: BTreeMap::new(),
            result: JobResult {
                job_id: id.to_string(),
                state: JobState::Queued,
                progress_percent: 0,
                output_paths: Vec::new(),
                report_path: None,
                entity_count: None,
                error: None,
            },
        };
        self.inner.lock().map_err(poisoned)?.jobs.insert(
            id,
            Arc::new(JobEntry {
                snapshot: Mutex::new(snapshot),
                changed: tokio::sync::Notify::new(),
                cancellation,
            }),
        );
        Ok(())
    }

    fn entry(&self, id: Uuid) -> std::result::Result<Arc<JobEntry>, IntegrationError> {
        self.inner
            .lock()
            .map_err(|_| IntegrationError::new(ErrorCode::TaskFailed, "任务状态不可用"))?
            .jobs
            .get(&id)
            .cloned()
            .ok_or_else(|| IntegrationError::new(ErrorCode::JobNotFound, "任务不存在"))
    }

    fn update(&self, id: Uuid, update: impl FnOnce(&mut IntegrationJob)) {
        let entry = self
            .inner
            .lock()
            .ok()
            .and_then(|jobs| jobs.jobs.get(&id).cloned());
        if let Some(entry) = entry {
            let mut became_terminal = false;
            if let Ok(mut snapshot) = entry.snapshot.lock() {
                let was_terminal = is_terminal(snapshot.result.state);
                update(&mut snapshot);
                became_terminal = !was_terminal && is_terminal(snapshot.result.state);
            }
            entry.changed.notify_waiters();
            if became_terminal {
                self.record_terminal(id);
            }
        }
    }

    fn record_terminal(&self, id: Uuid) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        inner.terminal_order.push_back(id);
        while inner.terminal_order.len() > MAX_TERMINAL_JOBS {
            if let Some(expired) = inner.terminal_order.pop_front() {
                inner.jobs.remove(&expired);
            }
        }
    }

    fn snapshot(&self, id: Uuid) -> std::result::Result<JobResult, IntegrationError> {
        let entry = self.entry(id)?;
        let snapshot = entry
            .snapshot
            .lock()
            .map_err(|_| IntegrationError::new(ErrorCode::TaskFailed, "任务状态不可用"))?;
        Ok(snapshot.result.clone())
    }
}

#[derive(Serialize)]
pub struct IntegrationInfo {
    enabled: bool,
    mcp_available: bool,
    authenticated: bool,
    models_ready: bool,
    executable_path: String,
    protocol_version: String,
    supported_formats: Vec<&'static str>,
}

#[derive(Serialize)]
pub struct IntegrationCheck {
    ok: bool,
    message: String,
}

fn mcp_executable_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("sixa-mcp.exe")))
        .unwrap_or_else(|| PathBuf::from("sixa-mcp.exe"))
}

#[tauri::command]
pub fn integration_info(state: State<'_, AppState>) -> IntegrationInfo {
    let executable = mcp_executable_path();
    IntegrationInfo {
        enabled: true,
        mcp_available: executable.is_file(),
        authenticated: state.auth.require_authenticated().is_ok(),
        models_ready: state
            .model
            .lock()
            .map(|model| {
                model
                    .capabilities
                    .iter()
                    .filter(|item| item.id != "ppocrv4-accurate-v1")
                    .all(|item| item.installed)
            })
            .unwrap_or(false),
        executable_path: executable.display().to_string(),
        protocol_version: PROTOCOL_VERSION.to_string(),
        supported_formats: SUPPORTED_FORMATS.to_vec(),
    }
}

#[tauri::command]
pub async fn integration_check() -> IntegrationCheck {
    let executable = mcp_executable_path();
    if !executable.is_file() {
        return IntegrationCheck {
            ok: false,
            message: format!("未找到 MCP 程序：{}", executable.display()),
        };
    }
    match tokio::time::timeout(Duration::from_secs(8), mcp_stdio_check(&executable)).await {
        Ok(Ok(())) => IntegrationCheck {
            ok: true,
            message: "MCP 初始化、工具清单和桌面状态调用均正常，AI 工具接入已就绪".into(),
        },
        Ok(Err(message)) => IntegrationCheck { ok: false, message },
        Err(_) => IntegrationCheck {
            ok: false,
            message: "MCP 完整链路自检超时，请重启私匣后重试".into(),
        },
    }
}

async fn mcp_stdio_check(executable: &Path) -> std::result::Result<(), String> {
    let mut child = tokio::process::Command::new(executable)
        .arg("serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("无法启动 MCP 程序：{error}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "无法连接 MCP 标准输入".to_owned())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "无法连接 MCP 标准输出".to_owned())?;
    let mut lines = BufReader::new(stdout).lines();
    let result = async {
        write_mcp_message(
            &mut stdin,
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "sixa-desktop-self-test", "version": env!("CARGO_PKG_VERSION")}
                }
            }),
        )
        .await?;
        let initialize = read_mcp_response(&mut lines, 1).await?;
        let server = &initialize["result"]["serverInfo"];
        if initialize["result"]["protocolVersion"] != "2025-06-18"
            || server["name"] != "sixa"
            || server["title"] != "私匣 · 本机文件脱敏"
            || server["version"] != env!("CARGO_PKG_VERSION")
        {
            return Err("MCP 服务名称或中文标题不匹配，请重新安装私匣".into());
        }

        write_mcp_message(
            &mut stdin,
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        )
        .await?;
        write_mcp_message(
            &mut stdin,
            &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
        )
        .await?;
        let tools_response = read_mcp_response(&mut lines, 2).await?;
        let tools = tools_response["result"]["tools"]
            .as_array()
            .ok_or_else(|| "MCP 工具清单格式无效".to_owned())?;
        let required = [
            "desensitization_status",
            "desensitize_file",
            "desensitize_batch",
            "get_desensitization_job",
            "wait_desensitization_job",
            "cancel_desensitization_job",
        ];
        for name in required {
            let tool = tools
                .iter()
                .find(|tool| tool["name"] == name)
                .ok_or_else(|| format!("MCP 缺少工具：{name}"))?;
            if tool["title"].as_str().is_none()
                || tool["description"].as_str().is_none()
                || !tool["outputSchema"].is_object()
                || !tool["annotations"].is_object()
            {
                return Err(format!("MCP 工具元数据不完整：{name}"));
            }
        }

        write_mcp_message(
            &mut stdin,
            &json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {"name": "desensitization_status", "arguments": {}}
            }),
        )
        .await?;
        let status_response = read_mcp_response(&mut lines, 3).await?;
        let tool_result = &status_response["result"];
        let status = &tool_result["structuredContent"];
        if tool_result["isError"] == true {
            let message = status["recovery_action"]
                .as_str()
                .or_else(|| status["message"].as_str())
                .unwrap_or("状态调用失败");
            return Err(format!("MCP 状态调用失败：{message}"));
        }
        if status["authenticated"] != true || status["authorization_valid"] != true {
            return Err("请先在私匣中登录并确认租户已开通本地数据脱敏功能".into());
        }
        if status["models_ready"] != true || status["ready"] != true {
            return Err("本机模型尚未就绪，请先在模型管理页完成准备".into());
        }
        if status["protocol_version"] != PROTOCOL_VERSION || !status["supported_formats"].is_array()
        {
            return Err("MCP 与桌面应用的协议或支持格式信息不完整，请重新安装私匣".into());
        }
        Ok(())
    }
    .await;

    drop(stdin);
    let _ = child.kill().await;
    let _ = child.wait().await;
    result
}

async fn write_mcp_message<W>(writer: &mut W, message: &Value) -> std::result::Result<(), String>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    writer
        .write_all(format!("{message}\n").as_bytes())
        .await
        .map_err(|error| format!("写入 MCP 请求失败：{error}"))?;
    writer
        .flush()
        .await
        .map_err(|error| format!("提交 MCP 请求失败：{error}"))
}

async fn read_mcp_response<R>(
    lines: &mut tokio::io::Lines<BufReader<R>>,
    expected_id: u64,
) -> std::result::Result<Value, String>
where
    R: tokio::io::AsyncRead + Unpin,
{
    loop {
        let line = lines
            .next_line()
            .await
            .map_err(|error| format!("读取 MCP 响应失败：{error}"))?
            .ok_or_else(|| "MCP 程序提前退出".to_owned())?;
        let response: Value = serde_json::from_str(&line)
            .map_err(|error| format!("MCP 响应不是有效 JSON：{error}"))?;
        let Some(id) = response.get("id") else {
            continue;
        };
        if id != expected_id {
            return Err(format!("MCP 响应标识不匹配，期望 {expected_id}，实际 {id}"));
        }
        if let Some(error) = response.get("error") {
            return Err(format!("MCP 协议调用失败：{error}"));
        }
        return Ok(response);
    }
}

pub fn start(app: AppHandle) -> Result<()> {
    let pipe_name = pipe_name().map_err(Error::Io)?;
    tauri::async_runtime::spawn(async move {
        if let Err(error) = serve(app, pipe_name).await {
            eprintln!("命名管道服务已停止：{error}");
        }
    });
    Ok(())
}

async fn serve(app: AppHandle, pipe_name: String) -> std::io::Result<()> {
    use tokio::net::windows::named_pipe::ServerOptions;
    let mut first = true;
    loop {
        let server = {
            let security = PipeSecurity::for_current_user()?;
            let mut options = ServerOptions::new();
            options
                .first_pipe_instance(first)
                .max_instances(16)
                .reject_remote_clients(true)
                .in_buffer_size(MAX_FRAME_BYTES as u32)
                .out_buffer_size(MAX_FRAME_BYTES as u32);
            unsafe {
                options.create_with_security_attributes_raw(
                    &pipe_name,
                    security.attributes() as *const _ as *mut c_void,
                )?
            }
        };
        first = false;
        server.connect().await?;
        let client_app = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(error) = serve_client(client_app, server).await
                && !matches!(
                    error.kind(),
                    std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::BrokenPipe
                )
            {
                eprintln!("命名管道客户端异常：{error}");
            }
        });
    }
}

async fn serve_client(
    app: AppHandle,
    mut stream: tokio::net::windows::named_pipe::NamedPipeServer,
) -> std::io::Result<()> {
    loop {
        let size = stream.read_u32_le().await? as usize;
        if size == 0 || size > MAX_FRAME_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "消息长度无效",
            ));
        }
        let mut bytes = vec![0u8; size];
        stream.read_exact(&mut bytes).await?;
        let response = match serde_json::from_slice::<Request>(&bytes) {
            Ok(request) => dispatch(app.clone(), request).await,
            Err(_) => Response::failure(
                String::new(),
                IntegrationError::new(ErrorCode::ProtocolMismatch, "请求格式无效"),
            ),
        };
        let frame = integration_protocol::encode_frame(&response)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        stream.write_all(&frame).await?;
        stream.flush().await?;
    }
}

async fn dispatch(app: AppHandle, request: Request) -> Response {
    let id = request.id.clone();
    if request.validate().is_err() {
        return Response::failure(
            id,
            IntegrationError::new(ErrorCode::ProtocolMismatch, "协议版本或请求标识不兼容"),
        );
    }
    let result = dispatch_method(&app, request.method, request.params).await;
    match result {
        Ok(value) => Response::success(id.clone(), &value).unwrap_or_else(|error| {
            Response::failure(
                id,
                IntegrationError::new(ErrorCode::TaskFailed, error.to_string()),
            )
        }),
        Err(error) => Response::failure(id, error),
    }
}

async fn dispatch_method(
    app: &AppHandle,
    method: Method,
    params: Value,
) -> std::result::Result<Value, IntegrationError> {
    match method {
        Method::Status => {
            let state = app.state::<AppState>();
            let authenticated = state.auth.require_authenticated().is_ok();
            let models_ready = state
                .model
                .lock()
                .map(|model| {
                    model
                        .capabilities
                        .iter()
                        .filter(|item| item.id != "ppocrv4-accurate-v1")
                        .all(|item| item.installed)
                })
                .unwrap_or(false);
            to_value(StatusResult {
                authenticated,
                authorization_valid: authenticated,
                models_ready,
                protocol_version: PROTOCOL_VERSION,
                supported_formats: SUPPORTED_FORMATS
                    .iter()
                    .map(|value| (*value).to_owned())
                    .collect(),
            })
        }
        Method::DesensitizeFile => {
            let params: DesensitizeFileParams = parse_params(params)?;
            let id = enqueue_file(app.clone(), params)?;
            to_value(CreateJobResult {
                job_id: id.to_string(),
            })
        }
        Method::DesensitizeBatch => {
            let params: DesensitizeBatchParams = parse_params(params)?;
            let id = enqueue_batch(app.clone(), params)?;
            to_value(CreateJobResult {
                job_id: id.to_string(),
            })
        }
        Method::GetJob => {
            let params: JobParams = parse_params(params)?;
            let id = parse_job_id(&params.job_id)?;
            let state = app.state::<AppState>();
            to_value(state.integration_jobs.snapshot(id)?)
        }
        Method::WaitJob => {
            let params: WaitJobParams = parse_params(params)?;
            let id = parse_job_id(&params.job_id)?;
            let timeout = params.wait_seconds.clamp(1, 60);
            let registry = app.state::<AppState>().integration_jobs.clone();
            let entry = registry.entry(id)?;
            let notified = entry.changed.notified();
            let current = registry.snapshot(id)?;
            if !is_terminal(current.state) {
                let _ = tokio::time::timeout(Duration::from_secs(timeout), notified).await;
            }
            to_value(registry.snapshot(id)?)
        }
        Method::CancelJob => {
            let params: JobParams = parse_params(params)?;
            let id = parse_job_id(&params.job_id)?;
            let state = app.state::<AppState>();
            let entry = state.integration_jobs.entry(id)?;
            let snapshot = state.integration_jobs.snapshot(id)?;
            let cancelled = !is_terminal(snapshot.state);
            if cancelled {
                entry.cancellation.cancel().map_err(map_error)?;
            }
            to_value(CancelJobResult {
                job_id: id.to_string(),
                cancelled,
            })
        }
    }
}

fn enqueue_file(
    app: AppHandle,
    params: DesensitizeFileParams,
) -> std::result::Result<Uuid, IntegrationError> {
    let source = validate_source(&params.source_path)?;
    let output_dir = validate_output_dir(params.output_dir.as_deref(), &source)?;
    ensure_ready(&app)?;
    let id = Uuid::new_v4();
    let state = app.state::<AppState>();
    let activity = state.desktop.admit().map_err(map_error)?;
    let run = register_run(&state, id).map_err(map_error)?;
    state
        .integration_jobs
        .insert(id, "file", run.clone())
        .map_err(map_error)?;
    tauri::async_runtime::spawn(async move {
        let _activity = activity;
        run_file(app, id, source, output_dir, run).await;
    });
    Ok(id)
}

async fn run_file(
    app: AppHandle,
    id: Uuid,
    source: PathBuf,
    output_dir: PathBuf,
    run: recognition::ocr::OcrRun,
) {
    let result = run_file_inner(&app, id, &source, &output_dir, run).await;
    let state = app.state::<AppState>();
    let _ = unregister_run(&state, id);
    if let Err(error) = result {
        let cancelled = error.code == ErrorCode::Cancelled;
        state.integration_jobs.update(id, |snapshot| {
            snapshot.result.state = if cancelled {
                JobState::Cancelled
            } else {
                JobState::Failed
            };
            snapshot.result.error = Some(error);
        });
    }
}

async fn run_file_inner(
    app: &AppHandle,
    id: Uuid,
    source: &Path,
    output_dir: &Path,
    run: recognition::ocr::OcrRun,
) -> std::result::Result<(), IntegrationError> {
    let state = app.state::<AppState>();
    state.integration_jobs.update(id, |snapshot| {
        snapshot.result.state = JobState::Analyzing;
        snapshot.result.progress_percent = 10;
    });
    let source_owned = source.to_path_buf();
    let analyze_run = run.clone();
    let options = work(state.clone(), |engine| {
        let settings = engine.store.settings()?;
        Ok(TaskOptions {
            ocr_profile: settings.ocr_profile,
            pdf_mode: settings.pdf_mode,
        })
    })
    .await
    .map_err(map_error)?;
    let requirements = runtime::Requirements::for_file(&source_owned, &options);
    let analyzed = scheduled_job(
        state.clone(),
        Some(app.clone()),
        requirements,
        Some(run.clone()),
        move |engine| engine.analyze_file_as(&source_owned, options, id, analyze_run),
    )
    .await
    .map_err(map_error)?;
    state.integration_jobs.update(id, |snapshot| {
        snapshot.entity_counts = entity_counts(&analyzed);
        snapshot.result.entity_count = Some(snapshot.entity_counts.values().sum::<usize>() as u64);
        snapshot.result.state = JobState::Generating;
        snapshot.result.progress_percent = 55;
    });
    let execute_run = run.clone();
    let view = scheduled_job(
        state.clone(),
        Some(app.clone()),
        runtime::Requirements::render(),
        Some(run.clone()),
        move |engine| engine.execute_as(id, execute_run),
    )
    .await
    .map_err(map_error)?;
    state.integration_jobs.update(id, |snapshot| {
        snapshot.entity_counts = entity_counts(&view);
        snapshot.result.entity_count = Some(snapshot.entity_counts.values().sum::<usize>() as u64);
        snapshot.result.state = JobState::Exporting;
        snapshot.result.progress_percent = 90;
    });
    ensure_not_cancelled(&run)?;
    let temporary = TemporaryOutput::new(output_dir, id, "result")?;
    let export_path = temporary.path.clone();
    job(state.clone(), move |engine| engine.export(id, &export_path))
        .await
        .map_err(map_error)?;
    ensure_not_cancelled(&run)?;
    let output_path = publish_file_unique(
        &temporary.path,
        output_dir,
        source
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("文件"),
        "_已脱敏",
        &view.extension,
        Some(&run),
    )?;
    state.integration_jobs.update(id, |snapshot| {
        snapshot.result.output_paths = vec![output_path.clone()];
    });
    let report_path = write_report(
        output_dir,
        source
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("文件"),
        id,
        "file",
        &view.meta.state,
        &entity_counts(&view),
        Some(&output_path),
    )?;
    state.integration_jobs.update(id, |snapshot| {
        snapshot.result.state = JobState::Completed;
        snapshot.result.progress_percent = 100;
        snapshot.result.output_paths = vec![output_path];
        snapshot.result.report_path = Some(report_path);
    });
    Ok(())
}

fn enqueue_batch(
    app: AppHandle,
    params: DesensitizeBatchParams,
) -> std::result::Result<Uuid, IntegrationError> {
    if params.source_paths.is_empty() || params.source_paths.len() > 20 {
        return Err(IntegrationError::new(
            ErrorCode::InvalidPath,
            "每批需要 1-20 个文件",
        ));
    }
    let sources = params
        .source_paths
        .iter()
        .map(|path| validate_source(path))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let output_dir = validate_output_dir(params.output_dir.as_deref(), &sources[0])?;
    ensure_ready(&app)?;
    let id = Uuid::new_v4();
    let state = app.state::<AppState>();
    let activity = state.desktop.admit().map_err(map_error)?;
    let run = register_run(&state, id).map_err(map_error)?;
    state
        .integration_jobs
        .insert(id, "batch", run.clone())
        .map_err(map_error)?;
    tauri::async_runtime::spawn(async move {
        let _activity = activity;
        run_batch(app, id, sources, output_dir, run).await;
    });
    Ok(id)
}

async fn run_batch(
    app: AppHandle,
    id: Uuid,
    sources: Vec<PathBuf>,
    output_dir: PathBuf,
    run: recognition::ocr::OcrRun,
) {
    let result = run_batch_inner(&app, id, sources, &output_dir, run).await;
    let state = app.state::<AppState>();
    let _ = unregister_run(&state, id);
    if let Err(error) = result {
        let cancelled = error.code == ErrorCode::Cancelled;
        state.integration_jobs.update(id, |snapshot| {
            snapshot.result.state = if cancelled {
                JobState::Cancelled
            } else {
                JobState::Failed
            };
            snapshot.result.error = Some(error);
        });
    }
}

async fn run_batch_inner(
    app: &AppHandle,
    id: Uuid,
    sources: Vec<PathBuf>,
    output_dir: &Path,
    run: recognition::ocr::OcrRun,
) -> std::result::Result<(), IntegrationError> {
    let state = app.state::<AppState>();
    state.integration_jobs.update(id, |snapshot| {
        snapshot.result.state = JobState::Analyzing;
        snapshot.result.progress_percent = 10;
    });
    let analyze_run = run.clone();
    work(state.clone(), move |engine| engine.begin_batch(sources, id))
        .await
        .map_err(map_error)?;
    let _batch = batch_jobs::analyze(state.clone(), app.clone(), id, analyze_run)
        .await
        .map_err(map_error)?;
    ensure_not_cancelled(&run)?;
    state.integration_jobs.update(id, |snapshot| {
        snapshot.result.state = JobState::Generating;
        snapshot.result.progress_percent = 55;
    });
    let execute_run = run.clone();
    let batch = batch_jobs::execute(state.clone(), app.clone(), id, execute_run)
        .await
        .map_err(map_error)?;
    ensure_not_cancelled(&run)?;
    let batch_state = batch.meta.state;
    let counts = job(state.clone(), move |engine| {
        let mut counts = BTreeMap::new();
        for item in batch
            .items
            .iter()
            .filter(|item| item.state == TaskState::Completed)
        {
            if let Some(task_id) = item.task_id {
                for (kind, count) in entity_counts(&engine.view(task_id)?) {
                    *counts.entry(kind).or_insert(0) += count;
                }
            }
        }
        Ok(counts)
    })
    .await
    .map_err(map_error)?;
    state.integration_jobs.update(id, |snapshot| {
        snapshot.entity_counts = counts.clone();
        snapshot.result.entity_count = Some(counts.values().sum::<usize>() as u64);
        snapshot.result.state = JobState::Exporting;
        snapshot.result.progress_percent = 90;
    });
    ensure_not_cancelled(&run)?;
    let temporary = TemporaryOutput::new(output_dir, id, "batch")?;
    let export_path = temporary.path.clone();
    job(state.clone(), move |engine| {
        engine.export_batch(id, &export_path)
    })
    .await
    .map_err(map_error)?;
    ensure_not_cancelled(&run)?;
    let output_path = publish_file_unique(
        &temporary.path,
        output_dir,
        "批量",
        "_已脱敏",
        "zip",
        Some(&run),
    )?;
    state.integration_jobs.update(id, |snapshot| {
        snapshot.result.output_paths = vec![output_path.clone()];
    });
    let report_path = write_report(
        output_dir,
        "批量",
        id,
        "batch",
        &batch_state,
        &counts,
        Some(&output_path),
    )?;
    state.integration_jobs.update(id, |snapshot| {
        snapshot.result.state = if batch_state == TaskState::Partial {
            JobState::Partial
        } else {
            JobState::Completed
        };
        snapshot.result.progress_percent = 100;
        snapshot.result.output_paths = vec![output_path];
        snapshot.result.report_path = Some(report_path);
        snapshot.entity_counts = counts;
    });
    Ok(())
}

fn ensure_ready(app: &AppHandle) -> std::result::Result<(), IntegrationError> {
    let state = app.state::<AppState>();
    state.auth.require_authenticated().map_err(map_error)?;
    if !state
        .model
        .lock()
        .map_err(|_| IntegrationError::new(ErrorCode::TaskFailed, "模型状态不可用"))?
        .capabilities
        .iter()
        .find(|item| item.id == "raner-v1")
        .is_some_and(|item| item.installed)
    {
        return Err(IntegrationError::new(
            ErrorCode::ModelsNotReady,
            "模型尚未就绪",
        ));
    }
    Ok(())
}

fn validate_source(path: &Path) -> std::result::Result<PathBuf, IntegrationError> {
    if !path.is_absolute() {
        return Err(IntegrationError::new(
            ErrorCode::InvalidPath,
            "源文件必须使用绝对路径",
        ));
    }
    let path = path
        .canonicalize()
        .map_err(|_| IntegrationError::new(ErrorCode::InvalidPath, "源文件不存在或不可访问"))?;
    if !path.is_file() {
        return Err(IntegrationError::new(
            ErrorCode::InvalidPath,
            "源路径不是文件",
        ));
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !SUPPORTED_FORMATS.contains(&extension.as_str()) {
        return Err(IntegrationError::new(
            ErrorCode::UnsupportedFormat,
            format!("不支持的文件格式：{extension}"),
        ));
    }
    Ok(path)
}

fn validate_output_dir(
    requested: Option<&Path>,
    source: &Path,
) -> std::result::Result<PathBuf, IntegrationError> {
    let path = requested.unwrap_or_else(|| source.parent().unwrap_or(Path::new("")));
    if !path.is_absolute() {
        return Err(IntegrationError::new(
            ErrorCode::InvalidPath,
            "输出目录必须使用绝对路径",
        ));
    }
    let path = path
        .canonicalize()
        .map_err(|_| IntegrationError::new(ErrorCode::InvalidPath, "输出目录不存在或不可访问"))?;
    if !path.is_dir() {
        return Err(IntegrationError::new(
            ErrorCode::InvalidPath,
            "输出路径不是目录",
        ));
    }
    Ok(path)
}

fn ensure_not_cancelled(
    run: &recognition::ocr::OcrRun,
) -> std::result::Result<(), IntegrationError> {
    if run.is_cancelled() {
        Err(IntegrationError::new(ErrorCode::Cancelled, "任务已取消"))
    } else {
        Ok(())
    }
}

struct TemporaryOutput {
    path: PathBuf,
}

impl TemporaryOutput {
    fn new(
        directory: &Path,
        job_id: Uuid,
        label: &str,
    ) -> std::result::Result<Self, IntegrationError> {
        for index in 0u32..1_000 {
            let path = directory.join(format!(".sixa-{job_id}-{label}-{index}.tmp"));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(_) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(map_io(error)),
            }
        }
        Err(IntegrationError::new(
            ErrorCode::TaskFailed,
            "无法创建临时输出文件",
        ))
    }
}

impl Drop for TemporaryOutput {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn candidate_path(
    directory: &Path,
    stem: &str,
    suffix: &str,
    extension: &str,
    index: u32,
) -> PathBuf {
    let serial = if index == 0 {
        String::new()
    } else {
        format!(" ({})", index + 1)
    };
    directory.join(format!("{stem}{suffix}{serial}.{extension}"))
}

fn publish_file_unique(
    source: &Path,
    directory: &Path,
    stem: &str,
    suffix: &str,
    extension: &str,
    cancellation: Option<&recognition::ocr::OcrRun>,
) -> std::result::Result<PathBuf, IntegrationError> {
    for index in 0u32.. {
        let candidate = candidate_path(directory, stem, suffix, extension, index);
        match std::fs::hard_link(source, &candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => {}
        }
        let mut destination = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(map_io(error)),
        };
        let copied = (|| -> std::result::Result<(), IntegrationError> {
            let mut input = std::fs::File::open(source).map_err(map_io)?;
            let mut buffer = [0_u8; 1024 * 1024];
            loop {
                if let Some(run) = cancellation {
                    ensure_not_cancelled(run)?;
                }
                let read = input.read(&mut buffer).map_err(map_io)?;
                if read == 0 {
                    break;
                }
                destination.write_all(&buffer[..read]).map_err(map_io)?;
            }
            destination.sync_all().map_err(map_io)
        })();
        if let Err(error) = copied {
            drop(destination);
            let _ = std::fs::remove_file(&candidate);
            return Err(error);
        }
        return Ok(candidate);
    }
    unreachable!()
}

fn write_bytes_unique(
    directory: &Path,
    stem: &str,
    suffix: &str,
    extension: &str,
    bytes: &[u8],
) -> std::result::Result<PathBuf, IntegrationError> {
    for index in 0u32.. {
        let candidate = candidate_path(directory, stem, suffix, extension, index);
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(map_io(error)),
        };
        if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
            drop(file);
            let _ = std::fs::remove_file(&candidate);
            return Err(map_io(error));
        }
        return Ok(candidate);
    }
    unreachable!()
}

fn entity_counts(view: &task_engine::TaskView) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for entity in view.entities.iter().filter(|entity| entity.selected) {
        *counts.entry(entity.entity_type.clone()).or_insert(0) += 1;
    }
    counts
}

fn write_report(
    directory: &Path,
    stem: &str,
    job_id: Uuid,
    kind: &str,
    state: &TaskState,
    counts: &BTreeMap<String, usize>,
    output: Option<&Path>,
) -> std::result::Result<PathBuf, IntegrationError> {
    let report = json!({
        "version": 1,
        "job_id": job_id,
        "kind": kind,
        "state": state,
        "entity_counts": counts,
        "output_path": output,
    });
    let bytes = serde_json::to_vec_pretty(&report)
        .map_err(|error| IntegrationError::new(ErrorCode::TaskFailed, error.to_string()))?;
    write_bytes_unique(directory, stem, "_脱敏报告", "json", &bytes)
}

fn map_io(error: std::io::Error) -> IntegrationError {
    IntegrationError::new(ErrorCode::TaskFailed, format!("写入输出文件失败：{error}"))
}

fn parse_params<T: for<'de> Deserialize<'de>>(
    params: Value,
) -> std::result::Result<T, IntegrationError> {
    serde_json::from_value(params)
        .map_err(|_| IntegrationError::new(ErrorCode::ProtocolMismatch, "方法参数无效"))
}

fn parse_job_id(value: &str) -> std::result::Result<Uuid, IntegrationError> {
    Uuid::parse_str(value)
        .map_err(|_| IntegrationError::new(ErrorCode::ProtocolMismatch, "job_id 无效"))
}

fn to_value(value: impl Serialize) -> std::result::Result<Value, IntegrationError> {
    serde_json::to_value(value)
        .map_err(|error| IntegrationError::new(ErrorCode::TaskFailed, error.to_string()))
}

fn map_error(error: Error) -> IntegrationError {
    match error {
        Error::ModelsNotReady(message) => IntegrationError::new(ErrorCode::ModelsNotReady, message),
        Error::Unsupported(message) => IntegrationError::new(ErrorCode::UnsupportedFormat, message),
        Error::Invalid(message) => IntegrationError::new(ErrorCode::InvalidPath, message),
        Error::State(message) if message.contains("取消") => {
            IntegrationError::new(ErrorCode::Cancelled, message)
        }
        Error::State(message) if message.contains("期限") || message.contains("时间") => {
            IntegrationError::new(ErrorCode::AuthExpired, message)
        }
        Error::State(message) if message.contains("登录") => {
            IntegrationError::new(ErrorCode::AuthRequired, message)
        }
        other => IntegrationError::new(ErrorCode::TaskFailed, other.to_string()),
    }
}

fn is_terminal(state: JobState) -> bool {
    matches!(
        state,
        JobState::Completed | JobState::Partial | JobState::Failed | JobState::Cancelled
    )
}

fn pipe_name() -> std::result::Result<String, String> {
    Ok(format!("{PIPE_PREFIX}{}", current_user_sid()?))
}

struct PipeSecurity {
    descriptor: windows_sys::Win32::Security::PSECURITY_DESCRIPTOR,
    attributes: windows_sys::Win32::Security::SECURITY_ATTRIBUTES,
}

impl PipeSecurity {
    fn for_current_user() -> std::io::Result<Self> {
        use windows_sys::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
        };
        let sid = current_user_sid().map_err(std::io::Error::other)?;
        let sddl = format!("D:P(A;;GA;;;SY)(A;;GA;;;{sid})");
        let wide = sddl
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let mut descriptor = std::ptr::null_mut();
        let succeeded = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        };
        if succeeded == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let attributes = windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<windows_sys::Win32::Security::SECURITY_ATTRIBUTES>()
                as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        Ok(Self {
            descriptor,
            attributes,
        })
    }

    fn attributes(&self) -> &windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
        &self.attributes
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::LocalFree(self.descriptor) };
    }
}

fn current_user_sid() -> std::result::Result<String, String> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE, LocalFree},
        Security::Authorization::ConvertSidToStringSidW,
        Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser},
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };
    let mut token: HANDLE = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let result = (|| {
        let mut needed = 0u32;
        unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed) };
        if needed == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let mut buffer = vec![0u8; needed as usize];
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let user = unsafe { &*(buffer.as_ptr().cast::<TOKEN_USER>()) };
        let mut sid_string = std::ptr::null_mut();
        if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut sid_string) } == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let len = (0..)
            .take_while(|&index| unsafe { *sid_string.add(index) } != 0)
            .count();
        let sid = String::from_utf16(unsafe { std::slice::from_raw_parts(sid_string, len) })
            .map_err(|error| error.to_string());
        unsafe { LocalFree(sid_string.cast()) };
        sid
    })();
    unsafe { CloseHandle(token) };
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_names_do_not_replace_existing_files() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("合同_已脱敏.pdf"), b"existing").unwrap();
        let source = directory.path().join("temporary");
        std::fs::write(&source, b"new").unwrap();
        assert_eq!(
            publish_file_unique(&source, directory.path(), "合同", "_已脱敏", "pdf", None).unwrap(),
            directory.path().join("合同_已脱敏 (2).pdf")
        );
        assert_eq!(
            std::fs::read(directory.path().join("合同_已脱敏.pdf")).unwrap(),
            b"existing"
        );
    }

    #[test]
    fn concurrent_outputs_get_distinct_names() {
        let directory = Arc::new(tempfile::tempdir().unwrap());
        let source = directory.path().join("temporary");
        std::fs::write(&source, b"result").unwrap();
        let threads = (0..16)
            .map(|_| {
                let directory = directory.clone();
                let source = source.clone();
                std::thread::spawn(move || {
                    publish_file_unique(&source, directory.path(), "合同", "_已脱敏", "pdf", None)
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        let paths = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(paths.len(), 16);
        assert!(
            paths
                .iter()
                .all(|path| std::fs::read(path).unwrap() == b"result")
        );
    }

    #[test]
    fn registry_keeps_only_recent_terminal_jobs() {
        let registry = JobRegistry::default();
        let mut ids = Vec::new();
        for _ in 0..(MAX_TERMINAL_JOBS + 4) {
            let id = Uuid::new_v4();
            ids.push(id);
            registry
                .insert(id, "file", recognition::ocr::OcrRun::new().unwrap())
                .unwrap();
            registry.update(id, |job| job.result.state = JobState::Completed);
        }
        assert!(registry.entry(ids[0]).is_err());
        assert!(registry.entry(*ids.last().unwrap()).is_ok());
        assert_eq!(registry.inner.lock().unwrap().jobs.len(), MAX_TERMINAL_JOBS);
    }

    #[test]
    fn source_validation_rejects_relative_missing_and_unsupported_paths() {
        assert_eq!(
            validate_source(Path::new("relative.pdf")).unwrap_err().code,
            ErrorCode::InvalidPath
        );
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(
            validate_source(&directory.path().join("missing.pdf"))
                .unwrap_err()
                .code,
            ErrorCode::InvalidPath
        );
        let source = directory.path().join("source.exe");
        std::fs::write(&source, b"x").unwrap();
        assert_eq!(
            validate_source(&source).unwrap_err().code,
            ErrorCode::UnsupportedFormat
        );
    }

    #[test]
    fn wire_errors_do_not_expose_sensitive_values() {
        let response = Response::failure(
            "request",
            IntegrationError::new(ErrorCode::AuthRequired, "请先登录"),
        );
        let value = serde_json::to_value(response).unwrap();
        assert_eq!(value["error"]["code"], "AUTH_REQUIRED");
        assert!(value.get("result").is_none());
    }

    #[test]
    fn pipe_is_scoped_to_the_current_windows_user() {
        let sid = current_user_sid().unwrap();
        assert!(sid.starts_with("S-1-"));
        assert_eq!(pipe_name().unwrap(), format!("{PIPE_PREFIX}{sid}"));
        PipeSecurity::for_current_user().unwrap();
    }
}
