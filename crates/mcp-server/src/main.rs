use integration_protocol::{
    CancelJobResult, CreateJobResult, DesensitizeBatchParams, DesensitizeFileParams, EmptyParams,
    ErrorCode, IntegrationError, JobParams, JobResult, MAX_FRAME_BYTES, Method, ProtocolError,
    Request, Response, StatusResult, WaitJobParams, decode_frame, encode_frame, pipe_name_for_sid,
};
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, Implementation, ServerCapabilities, ServerConfig},
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_RETRY_DELAY: Duration = Duration::from_millis(25);
const CONNECT_RETRY_WINDOW: Duration = Duration::from_millis(750);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct FileInput {
    #[schemars(description = "Absolute path to a supported local source file")]
    source_path: String,
    #[schemars(
        description = "Optional absolute output directory; the source file is never overwritten"
    )]
    output_dir: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct BatchInput {
    #[schemars(description = "Non-empty list of absolute paths to supported local source files")]
    source_paths: Vec<String>,
    #[schemars(description = "Optional absolute output directory")]
    output_dir: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct JobInput {
    #[schemars(description = "Job identifier returned when a desensitization job was created")]
    job_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct WaitJobInput {
    #[schemars(description = "Job identifier returned when a desensitization job was created")]
    job_id: String,
    #[schemars(
        description = "Long-poll duration in seconds, from 1 through 60; defaults to 60",
        range(min = 1, max = 60)
    )]
    wait_seconds: Option<u64>,
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

    fn success<T: Serialize>(value: &T) -> CallToolResult {
        match serde_json::to_value(value) {
            Ok(value) => CallToolResult::structured(value),
            Err(error) => Self::failure(IntegrationError::new(
                ErrorCode::ProtocolMismatch,
                error.to_string(),
            )),
        }
    }

    fn failure(error: IntegrationError) -> CallToolResult {
        let value = serde_json::to_value(&error).unwrap_or_else(
            |_| serde_json::json!({"code": "TASK_FAILED", "message": "无法序列化错误响应"}),
        );
        CallToolResult::structured_error(value)
    }

    async fn call<P, R>(&self, method: Method, params: &P, timeout: Duration) -> CallToolResult
    where
        P: Serialize,
        R: DeserializeOwned + Serialize,
    {
        match self.client.request::<P, R>(method, params, timeout).await {
            Ok(value) => Self::success(&value),
            Err(error) => Self::failure(error),
        }
    }
}

#[tool_router]
impl DesensitizationMcp {
    #[tool(
        description = "Check whether the local data desensitization desktop application is authenticated, authorized, and ready"
    )]
    async fn desensitization_status(&self) -> CallToolResult {
        self.call::<_, StatusResult>(
            Method::Status,
            &EmptyParams::default(),
            Duration::from_secs(5),
        )
        .await
    }

    #[tool(
        description = "Start automatic desensitization of one local file; returns a job ID immediately"
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
        self.call::<_, CreateJobResult>(
            Method::DesensitizeFile,
            &DesensitizeFileParams {
                source_path,
                output_dir,
            },
            DEFAULT_REQUEST_TIMEOUT,
        )
        .await
    }

    #[tool(
        description = "Start automatic desensitization of multiple local files; returns a job ID immediately"
    )]
    async fn desensitize_batch(&self, Parameters(input): Parameters<BatchInput>) -> CallToolResult {
        if input.source_paths.is_empty() {
            return Self::failure(invalid_path("source_paths 不能为空"));
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
        self.call::<_, CreateJobResult>(
            Method::DesensitizeBatch,
            &DesensitizeBatchParams {
                source_paths,
                output_dir,
            },
            DEFAULT_REQUEST_TIMEOUT,
        )
        .await
    }

    #[tool(description = "Get current progress and output paths for a desensitization job")]
    async fn get_desensitization_job(
        &self,
        Parameters(input): Parameters<JobInput>,
    ) -> CallToolResult {
        if input.job_id.trim().is_empty() {
            return Self::failure(invalid_job_id());
        }
        self.call::<_, JobResult>(
            Method::GetJob,
            &JobParams {
                job_id: input.job_id,
            },
            DEFAULT_REQUEST_TIMEOUT,
        )
        .await
    }

    #[tool(
        description = "Wait up to 60 seconds for a desensitization job to change or finish, then return its current state"
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
        self.call::<_, JobResult>(
            Method::WaitJob,
            &WaitJobParams {
                job_id: input.job_id,
                wait_seconds,
            },
            Duration::from_secs(wait_seconds + 5),
        )
        .await
    }

    #[tool(description = "Cancel a queued or running desensitization job")]
    async fn cancel_desensitization_job(
        &self,
        Parameters(input): Parameters<JobInput>,
    ) -> CallToolResult {
        if input.job_id.trim().is_empty() {
            return Self::failure(invalid_job_id());
        }
        self.call::<_, CancelJobResult>(
            Method::CancelJob,
            &JobParams {
                job_id: input.job_id,
            },
            DEFAULT_REQUEST_TIMEOUT,
        )
        .await
    }
}

#[tool_handler]
impl ServerHandler for DesensitizationMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("sixa", env!("CARGO_PKG_VERSION")))
            .with_instructions("Use these tools to desensitize files through the locally running desktop application. File contents and detected values are never returned to the MCP client.")
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
    }
}
