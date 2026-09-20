use integration_protocol::{
    CancelJobResult, CreateJobResult, DesensitizeBatchParams, DesensitizeFileParams, EmptyParams,
    ErrorCode, IntegrationError, JobParams, JobResult, JobState, MAX_FRAME_BYTES, Method,
    ProtocolError, Request, Response, StatusResult, WaitJobParams, decode_frame, encode_frame,
    pipe_name_for_sid,
};
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig},
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_RETRY_DELAY: Duration = Duration::from_millis(25);
const CONNECT_RETRY_WINDOW: Duration = Duration::from_millis(750);
const SERVER_INSTRUCTIONS: &str = "私匣用于在当前 Windows 电脑上脱敏用户明确指定或授权的本地文件。仅在用户要求处理具体文件时调用，不要自行扫描目录。首次调用先使用 desensitization_status；authenticated、authorization_valid、models_ready 和 ready 均为 true 后再创建任务。desensitize_file 和 desensitize_batch 只表示任务已进入队列，必须保存返回的 job_id，并按照 next_action 持续调用 wait_desensitization_job；不要因等待超时或任务仍在运行而重复创建任务。queued、analyzing、generating、exporting 是非终态，completed、partial、failed、cancelled 是终态。partial 表示只有部分结果可用，必须如实告知并提供 report_path。取消请求发出后仍应等待终态。工具不会向 AI 返回文件正文或识别出的实体值；完成后只报告状态、计数、output_paths 和 report_path，不要声称已经阅读或核验脱敏后的正文。";

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct FileInput {
    #[schemars(
        description = "用户明确指定或授权的本地源文件绝对路径。文件必须存在、可访问，扩展名应来自状态工具返回的 supported_formats"
    )]
    source_path: String,
    #[schemars(
        description = "可选的输出目录绝对路径。目录必须已经存在且可写；省略时写入源文件目录。私匣不会覆盖源文件或已有输出"
    )]
    output_dir: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct BatchInput {
    #[schemars(
        description = "用户明确指定或授权的 1 至 20 个本地源文件绝对路径。文件必须存在、可访问且格式受支持",
        length(min = 1, max = 20)
    )]
    source_paths: Vec<String>,
    #[schemars(
        description = "可选的输出目录绝对路径。目录必须已经存在且可写；省略时使用第一个源文件目录。私匣不会覆盖已有输出"
    )]
    output_dir: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct JobInput {
    #[schemars(description = "创建脱敏任务时返回的 job_id；等待期间必须重复使用同一个值")]
    job_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct WaitJobInput {
    #[schemars(description = "创建脱敏任务时返回的 job_id；不要为了继续等待而重新创建任务")]
    job_id: String,
    #[schemars(
        description = "本次长轮询等待秒数，范围 1 至 60，默认 60。返回非终态时应继续使用同一 job_id 等待",
        range(min = 1, max = 60)
    )]
    wait_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
struct NextAction {
    #[schemars(description = "call_tool 表示继续调用工具，user_action 表示需要用户在桌面端操作")]
    action_type: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    arguments: Option<Value>,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
struct StatusOutput {
    authenticated: bool,
    authorization_valid: bool,
    models_ready: bool,
    ready: bool,
    supported_formats: Vec<String>,
    protocol_version: u32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_action: Option<NextAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum McpJobState {
    Queued,
    Analyzing,
    Generating,
    Exporting,
    Completed,
    Partial,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
struct CreateJobOutput {
    job_id: String,
    state: McpJobState,
    message: String,
    next_action: NextAction,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
struct ErrorOutput {
    code: String,
    message: String,
    #[schemars(description = "完成 recovery_action 后是否适合重试原调用")]
    retryable: bool,
    #[schemars(description = "请求结果是否不确定；为 true 时不得自动重试创建任务")]
    outcome_unknown: bool,
    recovery_action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    suggested_tool: Option<NextAction>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(untagged)]
enum ToolOutput<T> {
    Success(T),
    Error(ErrorOutput),
}

#[derive(Clone, Copy)]
enum CallContext {
    Read,
    Create,
    Cancel,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
struct JobOutput {
    job_id: String,
    state: McpJobState,
    #[schemars(range(min = 0, max = 100))]
    progress_percent: u8,
    terminal: bool,
    output_paths: Vec<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    report_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    entity_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorOutput>,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_action: Option<NextAction>,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
struct CancelJobOutput {
    job_id: String,
    #[schemars(description = "是否已成功发出取消请求；仍需等待任务进入 cancelled 终态")]
    cancel_requested: bool,
    #[schemars(description = "兼容旧客户端；语义与 cancel_requested 相同")]
    cancelled: bool,
    message: String,
    next_action: NextAction,
}

#[derive(Clone)]
struct PipeClient {
    pipe_name: String,
}

impl PipeClient {
    fn for_current_user() -> Result<Self, IntegrationError> {
        let sid = current_user_sid().map_err(|error| {
            IntegrationError::new(
                ErrorCode::TaskFailed,
                format!("无法确定当前 Windows 用户: {error}"),
            )
        })?;
        let pipe_name = pipe_name_for_sid(&sid).map_err(protocol_error)?;
        Ok(Self { pipe_name })
    }

    async fn request<P, R>(
        &self,
        method: Method,
        params: &P,
        timeout_duration: Duration,
    ) -> Result<R, IntegrationError>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        let request = Request::new(method, params).map_err(protocol_error)?;
        let request_id = request.id.clone();
        let operation = async {
            let mut pipe = connect_pipe(&self.pipe_name).await?;
            let frame = encode_frame(&request).map_err(protocol_error)?;
            pipe.write_all(&frame).await.map_err(transport_error)?;
            pipe.flush().await.map_err(transport_error)?;

            let mut prefix = [0_u8; 4];
            pipe.read_exact(&mut prefix)
                .await
                .map_err(transport_error)?;
            let length = u32::from_le_bytes(prefix) as usize;
            if length > MAX_FRAME_BYTES {
                return Err(IntegrationError::new(
                    ErrorCode::ProtocolMismatch,
                    format!("桌面应用返回的数据帧超过 {MAX_FRAME_BYTES} 字节"),
                ));
            }
            let mut frame = Vec::with_capacity(length + 4);
            frame.extend_from_slice(&prefix);
            frame.resize(length + 4, 0);
            pipe.read_exact(&mut frame[4..])
                .await
                .map_err(transport_error)?;
            let response: Response = decode_frame(&frame).map_err(protocol_error)?;
            response.decode_result(&request_id).map_err(protocol_error)
        };

        tokio::time::timeout(timeout_duration, operation)
            .await
            .map_err(|_| IntegrationError::new(ErrorCode::TaskFailed, "等待桌面应用响应超时"))?
    }
}

#[derive(Clone)]
struct DesensitizationMcp {
    client: PipeClient,
}

impl DesensitizationMcp {
    fn new(client: PipeClient) -> Self {
        Self { client }
    }

    fn success<T: Serialize>(value: &T, summary: impl Into<String>) -> CallToolResult {
        match serde_json::to_value(value) {
            Ok(value) => {
                let summary = summary.into();
                let serialized = value.to_string();
                let mut result = CallToolResult::structured(value);
                result.content = vec![ContentBlock::text(format!(
                    "{summary}\n\n结构化结果：{serialized}"
                ))];
                result
            }
            Err(error) => Self::failure(IntegrationError::new(
                ErrorCode::ProtocolMismatch,
                error.to_string(),
            )),
        }
    }

    fn failure(error: IntegrationError) -> CallToolResult {
        Self::failure_for(error, CallContext::Read)
    }

    fn failure_for(error: IntegrationError, context: CallContext) -> CallToolResult {
        let output = error_output(error, context);
        let value = serde_json::to_value(&output).unwrap_or_else(|_| {
            json!({
                "code": "TASK_FAILED",
                "message": "无法序列化错误响应",
                "retryable": false,
                "outcome_unknown": false,
                "recovery_action": "请在私匣桌面应用中查看任务详情"
            })
        });
        let mut result = CallToolResult::structured_error(value.clone());
        result.content = vec![ContentBlock::text(format!(
            "调用失败：{}\n错误码：{}\n恢复方式：{}\n\n结构化错误：{}",
            output.message, output.code, output.recovery_action, value
        ))];
        result
    }
}

#[tool_router]
impl DesensitizationMcp {
    #[tool(
        title = "检查私匣状态",
        description = "检查私匣桌面应用、租户登录授权和本机模型是否可用。创建任何任务前先调用；仅当 ready 为 true 时继续。",
        output_schema = rmcp::handler::server::common::schema_for_output::<ToolOutput<StatusOutput>>(),
        annotations(title = "检查私匣状态", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn desensitization_status(&self) -> CallToolResult {
        match self
            .client
            .request::<_, StatusResult>(
                Method::Status,
                &EmptyParams::default(),
                Duration::from_secs(5),
            )
            .await
        {
            Ok(status) => {
                let output = status_output(status);
                let message = output.message.clone();
                Self::success(&output, message)
            }
            Err(error) => Self::failure(error),
        }
    }

    #[tool(
        title = "单文件脱敏",
        description = "为用户明确指定的一个本地文件创建自动脱敏任务。返回 job_id 只表示任务已排队，不表示处理完成；随后必须按 next_action 使用同一 job_id 持续等待。",
        output_schema = rmcp::handler::server::common::schema_for_output::<ToolOutput<CreateJobOutput>>(),
        annotations(title = "单文件脱敏", read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = false)
    )]
    async fn desensitize_file(&self, Parameters(input): Parameters<FileInput>) -> CallToolResult {
        let source_path = match absolute_path(&input.source_path, "source_path") {
            Ok(path) => path,
            Err(error) => return Self::failure(error),
        };
        let output_dir = match optional_absolute_path(input.output_dir.as_deref(), "output_dir") {
            Ok(path) => path,
            Err(error) => return Self::failure(error),
        };
        match self
            .client
            .request::<_, CreateJobResult>(
                Method::DesensitizeFile,
                &DesensitizeFileParams {
                    source_path,
                    output_dir,
                },
                DEFAULT_REQUEST_TIMEOUT,
            )
            .await
        {
            Ok(result) => {
                let output = create_job_output(result);
                let message = output.message.clone();
                Self::success(&output, message)
            }
            Err(error) => Self::failure_for(error, CallContext::Create),
        }
    }

    #[tool(
        title = "批量文件脱敏",
        description = "为用户明确指定的 1 至 20 个本地文件创建批量脱敏任务。返回 job_id 只表示任务已排队；随后必须按 next_action 使用同一 job_id 持续等待。",
        output_schema = rmcp::handler::server::common::schema_for_output::<ToolOutput<CreateJobOutput>>(),
        annotations(title = "批量文件脱敏", read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = false)
    )]
    async fn desensitize_batch(&self, Parameters(input): Parameters<BatchInput>) -> CallToolResult {
        if input.source_paths.is_empty() {
            return Self::failure(invalid_path("source_paths 不能为空"));
        }
        if input.source_paths.len() > 20 {
            return Self::failure(invalid_path("source_paths 最多包含 20 个文件"));
        }
        let mut source_paths = Vec::with_capacity(input.source_paths.len());
        for source in &input.source_paths {
            match absolute_path(source, "source_paths") {
                Ok(path) => source_paths.push(path),
                Err(error) => return Self::failure(error),
            }
        }
        let output_dir = match optional_absolute_path(input.output_dir.as_deref(), "output_dir") {
            Ok(path) => path,
            Err(error) => return Self::failure(error),
        };
        match self
            .client
            .request::<_, CreateJobResult>(
                Method::DesensitizeBatch,
                &DesensitizeBatchParams {
                    source_paths,
                    output_dir,
                },
                DEFAULT_REQUEST_TIMEOUT,
            )
            .await
        {
            Ok(result) => {
                let output = create_job_output(result);
                let message = output.message.clone();
                Self::success(&output, message)
            }
            Err(error) => Self::failure_for(error, CallContext::Create),
        }
    }

    #[tool(
        title = "查询脱敏任务",
        description = "立即查询同一 job_id 的当前状态、进度和输出路径。非终态时按照 next_action 继续等待；不要重新创建任务。",
        output_schema = rmcp::handler::server::common::schema_for_output::<ToolOutput<JobOutput>>(),
        annotations(title = "查询脱敏任务", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn get_desensitization_job(
        &self,
        Parameters(input): Parameters<JobInput>,
    ) -> CallToolResult {
        if input.job_id.trim().is_empty() {
            return Self::failure(invalid_job_id());
        }
        match self
            .client
            .request::<_, JobResult>(
                Method::GetJob,
                &JobParams {
                    job_id: input.job_id,
                },
                DEFAULT_REQUEST_TIMEOUT,
            )
            .await
        {
            Ok(result) => {
                let output = job_output(result);
                let message = output.message.clone();
                Self::success(&output, message)
            }
            Err(error) => Self::failure(error),
        }
    }

    #[tool(
        title = "等待脱敏任务",
        description = "使用原 job_id 长轮询等待最多 60 秒。返回 queued、analyzing、generating 或 exporting 时继续按 next_action 等待；进入终态后停止轮询并向用户报告结果。",
        output_schema = rmcp::handler::server::common::schema_for_output::<ToolOutput<JobOutput>>(),
        annotations(title = "等待脱敏任务", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn wait_desensitization_job(
        &self,
        Parameters(input): Parameters<WaitJobInput>,
    ) -> CallToolResult {
        if input.job_id.trim().is_empty() {
            return Self::failure(invalid_job_id());
        }
        let wait_seconds = input.wait_seconds.unwrap_or(60);
        if !(1..=60).contains(&wait_seconds) {
            return Self::failure(IntegrationError::new(
                ErrorCode::TaskFailed,
                "wait_seconds 必须在 1 到 60 之间",
            ));
        }
        match self
            .client
            .request::<_, JobResult>(
                Method::WaitJob,
                &WaitJobParams {
                    job_id: input.job_id,
                    wait_seconds,
                },
                Duration::from_secs(wait_seconds + 5),
            )
            .await
        {
            Ok(result) => {
                let output = job_output(result);
                let message = output.message.clone();
                Self::success(&output, message)
            }
            Err(error) => Self::failure(error),
        }
    }

    #[tool(
        title = "取消脱敏任务",
        description = "请求取消排队中或运行中的任务。cancel_requested 为 true 只表示请求已发出；随后必须按 next_action 等待并确认任务进入 cancelled 或其他终态。",
        output_schema = rmcp::handler::server::common::schema_for_output::<ToolOutput<CancelJobOutput>>(),
        annotations(title = "取消脱敏任务", read_only_hint = false, destructive_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    async fn cancel_desensitization_job(
        &self,
        Parameters(input): Parameters<JobInput>,
    ) -> CallToolResult {
        if input.job_id.trim().is_empty() {
            return Self::failure(invalid_job_id());
        }
        match self
            .client
            .request::<_, CancelJobResult>(
                Method::CancelJob,
                &JobParams {
                    job_id: input.job_id,
                },
                DEFAULT_REQUEST_TIMEOUT,
            )
            .await
        {
            Ok(result) => {
                let output = cancel_job_output(result);
                let message = output.message.clone();
                Self::success(&output, message)
            }
            Err(error) => Self::failure_for(error, CallContext::Cancel),
        }
    }
}

#[tool_handler]
impl ServerHandler for DesensitizationMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("sixa", env!("CARGO_PKG_VERSION"))
                    .with_title("私匣 · 本机文件脱敏")
                    .with_description("在 Windows 本机对 PDF、Office、图片和文本文件进行脱敏。文件正文和识别出的敏感值不会返回给 AI。"),
            )
            .with_instructions(SERVER_INSTRUCTIONS)
    }
}

fn call_tool_action(tool: &str, arguments: Value, message: impl Into<String>) -> NextAction {
    NextAction {
        action_type: "call_tool".into(),
        message: message.into(),
        tool: Some(tool.into()),
        arguments: Some(arguments),
    }
}

fn user_action(message: impl Into<String>) -> NextAction {
    NextAction {
        action_type: "user_action".into(),
        message: message.into(),
        tool: None,
        arguments: None,
    }
}

fn status_output(status: StatusResult) -> StatusOutput {
    let ready = status.authenticated && status.authorization_valid && status.models_ready;
    let (message, next_action) = if !status.authenticated {
        (
            "私匣尚未登录，暂时不能创建脱敏任务".into(),
            Some(user_action(
                "请用户打开私匣并完成登录，然后重新调用检查私匣状态",
            )),
        )
    } else if !status.authorization_valid {
        (
            "当前租户未获得私匣使用授权".into(),
            Some(user_action(
                "请用户联系管理员开通本地数据脱敏功能，然后重新调用检查私匣状态",
            )),
        )
    } else if !status.models_ready {
        (
            "私匣本机模型尚未就绪，暂时不能创建脱敏任务".into(),
            Some(user_action(
                "请用户在私匣的模型管理页完成模型准备，然后重新调用检查私匣状态",
            )),
        )
    } else {
        (
            "私匣已就绪，可以为用户明确指定的文件创建脱敏任务".into(),
            None,
        )
    };
    StatusOutput {
        authenticated: status.authenticated,
        authorization_valid: status.authorization_valid,
        models_ready: status.models_ready,
        ready,
        supported_formats: status.supported_formats,
        protocol_version: status.protocol_version,
        message,
        next_action,
    }
}

fn wait_action(job_id: &str, message: impl Into<String>) -> NextAction {
    call_tool_action(
        "wait_desensitization_job",
        json!({"job_id": job_id, "wait_seconds": 60}),
        message,
    )
}

fn create_job_output(result: CreateJobResult) -> CreateJobOutput {
    let next_action = wait_action(
        &result.job_id,
        "使用同一 job_id 等待任务进入终态；任务仍在运行时不要重复创建",
    );
    CreateJobOutput {
        message: format!("脱敏任务已创建，任务编号为 {}，当前尚未完成", result.job_id),
        job_id: result.job_id,
        state: McpJobState::Queued,
        next_action,
    }
}

fn mcp_job_state(state: JobState) -> McpJobState {
    match state {
        JobState::Queued => McpJobState::Queued,
        JobState::Analyzing => McpJobState::Analyzing,
        JobState::Generating => McpJobState::Generating,
        JobState::Exporting => McpJobState::Exporting,
        JobState::Completed => McpJobState::Completed,
        JobState::Partial => McpJobState::Partial,
        JobState::Failed => McpJobState::Failed,
        JobState::Cancelled => McpJobState::Cancelled,
    }
}

fn terminal_state(state: JobState) -> bool {
    matches!(
        state,
        JobState::Completed | JobState::Partial | JobState::Failed | JobState::Cancelled
    )
}

fn job_output(result: JobResult) -> JobOutput {
    let terminal = terminal_state(result.state);
    let state = mcp_job_state(result.state);
    let error = result
        .error
        .map(|error| error_output(error, CallContext::Read));
    let (message, next_action) = match result.state {
        JobState::Queued => (
            format!("任务正在排队，当前进度 {}%", result.progress_percent),
            Some(wait_action(
                &result.job_id,
                "继续使用同一 job_id 等待任务状态变化",
            )),
        ),
        JobState::Analyzing => (
            format!(
                "任务正在识别敏感内容，当前进度 {}%",
                result.progress_percent
            ),
            Some(wait_action(
                &result.job_id,
                "继续使用同一 job_id 等待识别和生成完成",
            )),
        ),
        JobState::Generating => (
            format!(
                "任务正在生成脱敏结果，当前进度 {}%",
                result.progress_percent
            ),
            Some(wait_action(
                &result.job_id,
                "继续使用同一 job_id 等待结果生成完成",
            )),
        ),
        JobState::Exporting => (
            format!("任务正在导出文件，当前进度 {}%", result.progress_percent),
            Some(wait_action(
                &result.job_id,
                "继续使用同一 job_id 等待导出完成",
            )),
        ),
        JobState::Completed => (
            format!(
                "脱敏任务已完成，共生成 {} 个输出文件",
                result.output_paths.len()
            ),
            None,
        ),
        JobState::Partial => (
            format!(
                "脱敏任务仅部分完成，已有 {} 个输出文件；必须向用户说明部分成功并提供报告路径",
                result.output_paths.len()
            ),
            Some(user_action(
                "请查看 report_path 中的失败项，并向用户如实说明部分成功",
            )),
        ),
        JobState::Failed => {
            let recovery = error
                .as_ref()
                .map(|value| value.recovery_action.as_str())
                .unwrap_or("请在私匣桌面应用的任务历史中查看失败详情");
            (
                format!("脱敏任务失败：{recovery}"),
                Some(user_action(recovery)),
            )
        }
        JobState::Cancelled => ("脱敏任务已取消，没有需要继续等待的任务".into(), None),
    };
    JobOutput {
        job_id: result.job_id,
        state,
        progress_percent: result.progress_percent,
        terminal,
        output_paths: result.output_paths,
        report_path: result.report_path,
        entity_count: result.entity_count,
        error,
        message,
        next_action,
    }
}

fn cancel_job_output(result: CancelJobResult) -> CancelJobOutput {
    let message = if result.cancelled {
        "取消请求已发出，但任务尚未确认进入 cancelled 终态".into()
    } else {
        "任务已经处于终态，未重复发出取消请求".into()
    };
    let next_action = if result.cancelled {
        wait_action(
            &result.job_id,
            "继续等待并确认任务进入 cancelled 或其他终态",
        )
    } else {
        call_tool_action(
            "get_desensitization_job",
            json!({"job_id": result.job_id}),
            "查询任务的最终状态和已有输出",
        )
    };
    CancelJobOutput {
        job_id: result.job_id,
        cancel_requested: result.cancelled,
        cancelled: result.cancelled,
        message,
        next_action,
    }
}

fn error_code_name(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::AppNotRunning => "APP_NOT_RUNNING",
        ErrorCode::AuthRequired => "AUTH_REQUIRED",
        ErrorCode::AuthExpired => "AUTH_EXPIRED",
        ErrorCode::ModelsNotReady => "MODELS_NOT_READY",
        ErrorCode::InvalidPath => "INVALID_PATH",
        ErrorCode::UnsupportedFormat => "UNSUPPORTED_FORMAT",
        ErrorCode::ProtocolMismatch => "PROTOCOL_MISMATCH",
        ErrorCode::JobNotFound => "JOB_NOT_FOUND",
        ErrorCode::TaskFailed => "TASK_FAILED",
        ErrorCode::Cancelled => "CANCELLED",
    }
}

fn retry_status_action(message: &str) -> NextAction {
    call_tool_action("desensitization_status", json!({}), message)
}

fn error_output(error: IntegrationError, context: CallContext) -> ErrorOutput {
    let (retryable, outcome_unknown, recovery_action, suggested_tool) = match error.code {
        ErrorCode::AppNotRunning => (
            true,
            false,
            "请用户启动私匣桌面应用并保持运行，然后重新检查状态",
            Some(retry_status_action("启动私匣后重新检查桌面应用状态")),
        ),
        ErrorCode::AuthRequired => (
            true,
            false,
            "请用户在私匣桌面应用中完成登录，然后重新检查状态",
            Some(retry_status_action("用户登录后重新检查登录和授权状态")),
        ),
        ErrorCode::AuthExpired => (
            true,
            false,
            "请用户联网并在私匣中刷新或重新登录，然后重新检查状态",
            Some(retry_status_action("登录授权恢复后重新检查状态")),
        ),
        ErrorCode::ModelsNotReady => (
            true,
            false,
            "请用户在私匣的模型管理页完成模型准备，然后重新检查状态",
            Some(retry_status_action("模型就绪后重新检查状态")),
        ),
        ErrorCode::InvalidPath => (
            true,
            false,
            "请核对源文件和输出目录均为当前 Windows 用户可访问的绝对路径；输出目录必须已经存在",
            None,
        ),
        ErrorCode::UnsupportedFormat => (
            false,
            false,
            "请调用检查私匣状态读取 supported_formats，并改用受支持的文件格式",
            Some(retry_status_action("读取当前版本支持的文件格式")),
        ),
        ErrorCode::ProtocolMismatch => (
            true,
            false,
            "请重新安装同一版本的私匣桌面应用和随附 MCP 程序，然后重新检查状态",
            Some(retry_status_action("更新完成后重新检查协议状态")),
        ),
        ErrorCode::JobNotFound => (
            false,
            false,
            "请核对 job_id 和已有输出；不要盲目重复创建任务，确认任务已过期后再征求用户是否重建",
            None,
        ),
        ErrorCode::TaskFailed => match context {
            CallContext::Create => (
                false,
                true,
                "桌面应用可能已经创建任务。不要自动重试；请用户先检查私匣任务历史和输出目录，确认没有对应任务后再决定是否重建",
                None,
            ),
            CallContext::Cancel => (
                true,
                false,
                "取消结果未确认。请先查询原 job_id 的状态；若仍在运行，可再次发送取消请求",
                None,
            ),
            CallContext::Read => (
                true,
                false,
                "请根据错误信息检查私匣任务历史；排除原因后重试当前查询或等待调用，不要重新创建任务",
                None,
            ),
        },
        ErrorCode::Cancelled => (
            false,
            false,
            "任务已取消；除非用户明确要求，否则不要重新创建任务",
            None,
        ),
    };
    ErrorOutput {
        code: error_code_name(error.code).into(),
        message: error.message,
        retryable,
        outcome_unknown,
        recovery_action: recovery_action.into(),
        suggested_tool,
    }
}

fn absolute_path(value: &str, field: &str) -> Result<PathBuf, IntegrationError> {
    if value.trim().is_empty() {
        return Err(invalid_path(format!("{field} 不能为空")));
    }
    let path = Path::new(value);
    if !path.is_absolute() {
        return Err(invalid_path(format!("{field} 必须是绝对路径")));
    }
    Ok(path.to_path_buf())
}

fn optional_absolute_path(
    value: Option<&str>,
    field: &str,
) -> Result<Option<PathBuf>, IntegrationError> {
    value.map(|value| absolute_path(value, field)).transpose()
}

fn invalid_path(message: impl Into<String>) -> IntegrationError {
    IntegrationError::new(ErrorCode::InvalidPath, message)
}

fn invalid_job_id() -> IntegrationError {
    IntegrationError::new(ErrorCode::JobNotFound, "job_id 不能为空")
}

fn protocol_error(error: ProtocolError) -> IntegrationError {
    match error {
        ProtocolError::Remote(error) => error,
        other => IntegrationError::new(ErrorCode::ProtocolMismatch, other.to_string()),
    }
}

fn transport_error(error: std::io::Error) -> IntegrationError {
    IntegrationError::new(
        ErrorCode::TaskFailed,
        format!("与桌面应用通信失败: {error}"),
    )
}

#[cfg(windows)]
async fn connect_pipe(
    pipe_name: &str,
) -> Result<tokio::net::windows::named_pipe::NamedPipeClient, IntegrationError> {
    use tokio::net::windows::named_pipe::ClientOptions;

    let deadline = tokio::time::Instant::now() + CONNECT_RETRY_WINDOW;
    loop {
        match ClientOptions::new().open(pipe_name) {
            Ok(client) => return Ok(client),
            Err(error)
                if error.raw_os_error() == Some(231) && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(CONNECT_RETRY_DELAY).await;
            }
            Err(error) if matches!(error.raw_os_error(), Some(2 | 3)) => {
                return Err(IntegrationError::new(
                    ErrorCode::AppNotRunning,
                    "私匣桌面应用未运行",
                ));
            }
            Err(error) => return Err(transport_error(error)),
        }
    }
}

#[cfg(not(windows))]
async fn connect_pipe(_pipe_name: &str) -> Result<(), IntegrationError> {
    Err(IntegrationError::new(
        ErrorCode::AppNotRunning,
        "私匣仅支持 Windows",
    ))
}

#[cfg(windows)]
fn current_user_sid() -> std::io::Result<String> {
    use std::ptr::null_mut;
    use windows_sys::{
        Win32::{
            Foundation::{CloseHandle, HANDLE, LocalFree},
            Security::{
                Authorization::ConvertSidToStringSidW, GetTokenInformation, TOKEN_QUERY,
                TOKEN_USER, TokenUser,
            },
            System::Threading::{GetCurrentProcess, OpenProcessToken},
        },
        core::PWSTR,
    };

    unsafe {
        let mut token: HANDLE = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(std::io::Error::last_os_error());
        }

        let result = (|| {
            let mut required = 0_u32;
            GetTokenInformation(token, TokenUser, null_mut(), 0, &mut required);
            if required == 0 {
                return Err(std::io::Error::last_os_error());
            }
            let word_size = std::mem::size_of::<usize>();
            let mut buffer = vec![0_usize; (required as usize).div_ceil(word_size)];
            if GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                required,
                &mut required,
            ) == 0
            {
                return Err(std::io::Error::last_os_error());
            }
            let user = &*(buffer.as_ptr().cast::<TOKEN_USER>());
            let mut sid_text: PWSTR = null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut sid_text) == 0 {
                return Err(std::io::Error::last_os_error());
            }
            if sid_text.is_null() {
                return Err(std::io::Error::other(
                    "ConvertSidToStringSidW returned null",
                ));
            }
            let mut length = 0;
            while *sid_text.add(length) != 0 {
                length += 1;
            }
            let sid = String::from_utf16(std::slice::from_raw_parts(sid_text, length))
                .map_err(|_| std::io::Error::other("Windows returned an invalid SID"));
            LocalFree(sid_text.cast());
            sid
        })();

        CloseHandle(token);
        result
    }
}

#[cfg(not(windows))]
fn current_user_sid() -> std::io::Result<String> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Windows SID is unavailable",
    ))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = DesensitizationMcp::new(PipeClient::for_current_user()?);
    server.serve(stdio()).await?.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_server() -> DesensitizationMcp {
        DesensitizationMcp::new(PipeClient {
            pipe_name: "unused-in-metadata-tests".into(),
        })
    }

    fn tool(name: &str) -> rmcp::model::Tool {
        DesensitizationMcp::tool_router()
            .list_all()
            .into_iter()
            .find(|tool| tool.name == name)
            .unwrap_or_else(|| panic!("missing MCP tool: {name}"))
    }

    #[test]
    fn file_inputs_require_absolute_paths() {
        assert_eq!(
            absolute_path("relative.txt", "source_path")
                .unwrap_err()
                .code,
            ErrorCode::InvalidPath
        );
        assert!(absolute_path(r"C:\\data\\source.txt", "source_path").is_ok());
        assert!(absolute_path(r"\\server\share\source.pdf", "source_path").is_ok());
    }

    #[test]
    fn remote_errors_keep_the_stable_error_code() {
        let error = IntegrationError::new(ErrorCode::AuthRequired, "请登录");
        let mapped = protocol_error(ProtocolError::Remote(error.clone()));
        assert_eq!(mapped, error);
    }

    #[test]
    fn mcp_error_does_not_add_sensitive_content() {
        let result = DesensitizationMcp::failure(IntegrationError::new(
            ErrorCode::AppNotRunning,
            "应用未运行",
        ));
        assert!(result.is_error.unwrap_or(false));
        assert!(result.structured_content.is_some());
        let serialized = serde_json::to_string(&result).unwrap();
        assert!(!serialized.contains("source_path"));
        assert!(!serialized.contains("entity_value"));
        assert!(!serialized.contains("access_token"));
    }

    #[test]
    fn initialize_metadata_explains_the_complete_ai_workflow_in_chinese() {
        let info = test_server().get_info();
        assert_eq!(info.server_info.name, "sixa");
        assert_eq!(
            info.server_info.title.as_deref(),
            Some("私匣 · 本机文件脱敏")
        );
        assert!(
            info.server_info
                .description
                .as_deref()
                .is_some_and(|description| description.contains("文件正文")
                    && description.contains("不会返回给 AI"))
        );

        let instructions = info.instructions.expect("server instructions");
        for expected in [
            "用户明确指定或授权",
            "不要自行扫描目录",
            "desensitization_status",
            "必须保存返回的 job_id",
            "wait_desensitization_job",
            "completed、partial、failed、cancelled 是终态",
            "取消请求发出后仍应等待终态",
            "不要声称已经阅读或核验脱敏后的正文",
        ] {
            assert!(
                instructions.contains(expected),
                "server instructions missing: {expected}"
            );
        }
    }

    #[test]
    fn all_tools_publish_chinese_metadata_annotations_and_output_schemas() {
        let expectations = [
            ("desensitization_status", "检查私匣状态", true, false, true),
            ("desensitize_file", "单文件脱敏", false, false, false),
            ("desensitize_batch", "批量文件脱敏", false, false, false),
            ("get_desensitization_job", "查询脱敏任务", true, false, true),
            (
                "wait_desensitization_job",
                "等待脱敏任务",
                true,
                false,
                true,
            ),
            (
                "cancel_desensitization_job",
                "取消脱敏任务",
                false,
                true,
                true,
            ),
        ];
        assert_eq!(DesensitizationMcp::tool_router().list_all().len(), 6);

        for (name, title, read_only, destructive, idempotent) in expectations {
            let tool = tool(name);
            assert_eq!(tool.title.as_deref(), Some(title));
            assert_eq!(
                tool.annotations
                    .as_ref()
                    .and_then(|value| value.title.as_deref()),
                Some(title)
            );
            let description = tool.description.as_deref().expect("tool description");
            assert!(!description.is_ascii());
            assert!(tool.output_schema.is_some(), "{name} needs outputSchema");

            let annotations = tool.annotations.expect("tool annotations");
            assert_eq!(annotations.read_only_hint, Some(read_only));
            assert_eq!(annotations.destructive_hint, Some(destructive));
            assert_eq!(annotations.idempotent_hint, Some(idempotent));
            assert_eq!(annotations.open_world_hint, Some(false));
        }
    }

    #[test]
    fn tool_schemas_expose_limits_and_next_step_fields() {
        let batch_schema = serde_json::to_value(&*tool("desensitize_batch").input_schema).unwrap();
        let source_paths = &batch_schema["properties"]["source_paths"];
        assert_eq!(source_paths["minItems"], 1);
        assert_eq!(source_paths["maxItems"], 20);

        let wait_schema =
            serde_json::to_value(&*tool("wait_desensitization_job").input_schema).unwrap();
        let wait_seconds = &wait_schema["properties"]["wait_seconds"];
        assert_eq!(wait_seconds["minimum"], 1);
        assert_eq!(wait_seconds["maximum"], 60);

        let status_schema = serde_json::to_value(
            &*tool("desensitization_status")
                .output_schema
                .expect("status output schema"),
        )
        .unwrap();
        for property in ["ready", "message", "next_action", "supported_formats"] {
            assert!(schema_has_property(&status_schema, property));
        }
        assert_error_schema(&status_schema);

        let create_schema = serde_json::to_value(
            &*tool("desensitize_file")
                .output_schema
                .expect("create output schema"),
        )
        .unwrap();
        for property in ["job_id", "state", "message", "next_action"] {
            assert!(schema_has_property(&create_schema, property));
        }
        assert_error_schema(&create_schema);

        let job_schema = serde_json::to_value(
            &*tool("wait_desensitization_job")
                .output_schema
                .expect("job output schema"),
        )
        .unwrap();
        for property in [
            "state",
            "terminal",
            "output_paths",
            "report_path",
            "error",
            "next_action",
        ] {
            assert!(schema_has_property(&job_schema, property));
        }
        assert_error_schema(&job_schema);
        let serialized_job_schema = job_schema.to_string();
        for state in [
            "queued",
            "analyzing",
            "generating",
            "exporting",
            "completed",
            "partial",
            "failed",
            "cancelled",
        ] {
            assert!(serialized_job_schema.contains(&format!("\"{state}\"")));
        }
        assert!(serialized_job_schema.contains("\"maximum\":100"));

        let cancel_schema = serde_json::to_value(
            &*tool("cancel_desensitization_job")
                .output_schema
                .expect("cancel output schema"),
        )
        .unwrap();
        assert!(schema_has_property(&cancel_schema, "cancel_requested"));
        assert!(schema_has_property(&cancel_schema, "next_action"));
        assert_error_schema(&cancel_schema);
    }

    #[test]
    fn structured_results_drive_ai_until_a_terminal_state() {
        let status = status_output(StatusResult {
            authenticated: true,
            authorization_valid: true,
            models_ready: true,
            supported_formats: vec!["pdf".into()],
            protocol_version: 1,
        });
        assert!(status.ready);
        assert!(status.next_action.is_none());

        let created = create_job_output(CreateJobResult {
            job_id: "job-1".into(),
        });
        assert_eq!(created.state, McpJobState::Queued);
        assert_eq!(
            created.next_action.tool.as_deref(),
            Some("wait_desensitization_job")
        );
        assert_eq!(
            created.next_action.arguments.as_ref().unwrap()["job_id"],
            "job-1"
        );

        let running = job_output(JobResult {
            job_id: "job-1".into(),
            state: JobState::Analyzing,
            progress_percent: 35,
            output_paths: Vec::new(),
            report_path: None,
            entity_count: None,
            error: None,
        });
        assert!(!running.terminal);
        assert_eq!(
            running
                .next_action
                .as_ref()
                .and_then(|action| action.tool.as_deref()),
            Some("wait_desensitization_job")
        );

        let completed = job_output(JobResult {
            job_id: "job-1".into(),
            state: JobState::Completed,
            progress_percent: 100,
            output_paths: vec![PathBuf::from(r"C:\\out\\result.pdf")],
            report_path: None,
            entity_count: Some(8),
            error: None,
        });
        assert!(completed.terminal);
        assert!(completed.next_action.is_none());

        let partial = job_output(JobResult {
            job_id: "job-2".into(),
            state: JobState::Partial,
            progress_percent: 100,
            output_paths: vec![PathBuf::from(r"C:\\out\\one.pdf")],
            report_path: Some(PathBuf::from(r"C:\\out\\report.json")),
            entity_count: Some(3),
            error: None,
        });
        assert!(partial.terminal);
        assert!(partial.message.contains("部分完成"));
        assert_eq!(
            partial
                .next_action
                .as_ref()
                .map(|action| action.action_type.as_str()),
            Some("user_action")
        );
    }

    #[test]
    fn cancellation_always_tells_ai_how_to_confirm_the_final_state() {
        let requested = cancel_job_output(CancelJobResult {
            job_id: "job-1".into(),
            cancelled: true,
        });
        assert!(requested.cancel_requested);
        assert_eq!(
            requested.next_action.tool.as_deref(),
            Some("wait_desensitization_job")
        );

        let already_terminal = cancel_job_output(CancelJobResult {
            job_id: "job-2".into(),
            cancelled: false,
        });
        assert!(!already_terminal.cancel_requested);
        assert_eq!(
            already_terminal.next_action.tool.as_deref(),
            Some("get_desensitization_job")
        );
    }

    #[test]
    fn every_error_code_has_a_safe_recovery_contract() {
        let expectations = [
            (ErrorCode::AppNotRunning, true, true),
            (ErrorCode::AuthRequired, true, true),
            (ErrorCode::AuthExpired, true, true),
            (ErrorCode::ModelsNotReady, true, true),
            (ErrorCode::InvalidPath, true, false),
            (ErrorCode::UnsupportedFormat, false, true),
            (ErrorCode::ProtocolMismatch, true, true),
            (ErrorCode::JobNotFound, false, false),
            (ErrorCode::TaskFailed, true, false),
            (ErrorCode::Cancelled, false, false),
        ];

        for (code, retryable, has_suggested_tool) in expectations {
            let output = error_output(
                IntegrationError::new(code, "可公开错误信息"),
                CallContext::Read,
            );
            assert!(!output.code.is_empty());
            assert_eq!(
                output.retryable, retryable,
                "unexpected retryability for {code:?}"
            );
            assert!(!output.recovery_action.is_empty());
            assert_eq!(output.suggested_tool.is_some(), has_suggested_tool);
            assert!(!output.outcome_unknown);
            let serialized = serde_json::to_string(&output).unwrap();
            assert!(!serialized.contains("source_path"));
            assert!(!serialized.contains("entity_value"));
            assert!(!serialized.contains("access_token"));
        }
    }

    #[test]
    fn create_transport_failure_never_invites_an_automatic_retry() {
        let output = error_output(
            IntegrationError::new(ErrorCode::TaskFailed, "等待桌面应用响应超时"),
            CallContext::Create,
        );
        assert!(!output.retryable);
        assert!(output.outcome_unknown);
        assert!(output.recovery_action.contains("不要自动重试"));
        assert!(output.recovery_action.contains("任务历史"));
    }

    #[test]
    fn task_failure_recovery_matches_the_operation_context() {
        let read = error_output(
            IntegrationError::new(ErrorCode::TaskFailed, "读取超时"),
            CallContext::Read,
        );
        assert!(read.retryable);
        assert!(!read.outcome_unknown);
        assert!(read.recovery_action.contains("不要重新创建任务"));

        let cancel = error_output(
            IntegrationError::new(ErrorCode::TaskFailed, "取消超时"),
            CallContext::Cancel,
        );
        assert!(cancel.retryable);
        assert!(!cancel.outcome_unknown);
        assert!(cancel.recovery_action.contains("先查询原 job_id"));
    }

    fn schema_has_property(schema: &Value, property: &str) -> bool {
        match schema {
            Value::Object(object) => {
                object
                    .get("properties")
                    .and_then(Value::as_object)
                    .is_some_and(|properties| properties.contains_key(property))
                    || object
                        .values()
                        .any(|value| schema_has_property(value, property))
            }
            Value::Array(values) => values
                .iter()
                .any(|value| schema_has_property(value, property)),
            _ => false,
        }
    }

    fn assert_error_schema(schema: &Value) {
        for property in [
            "code",
            "message",
            "retryable",
            "outcome_unknown",
            "recovery_action",
            "suggested_tool",
        ] {
            assert!(
                schema_has_property(schema, property),
                "error output schema missing {property}"
            );
        }
    }

    #[tokio::test]
    async fn runtime_validation_rejects_invalid_limits_before_connecting() {
        let server = test_server();
        let batch = server
            .desensitize_batch(Parameters(BatchInput {
                source_paths: (0..21)
                    .map(|index| format!(r"C:\\input\\{index}.pdf"))
                    .collect(),
                output_dir: None,
            }))
            .await;
        assert_eq!(batch.is_error, Some(true));
        assert!(batch.structured_content.as_ref().is_some_and(|value| {
            value["message"]
                .as_str()
                .is_some_and(|message| message.contains("20"))
        }));

        let wait = server
            .wait_desensitization_job(Parameters(WaitJobInput {
                job_id: "job-1".into(),
                wait_seconds: Some(0),
            }))
            .await;
        assert_eq!(wait.is_error, Some(true));
        assert!(wait.structured_content.as_ref().is_some_and(|value| {
            value["message"]
                .as_str()
                .is_some_and(|message| message.contains("1 到 60"))
        }));
    }
}
