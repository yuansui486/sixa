use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{io, path::PathBuf};
use thiserror::Error;
use uuid::Uuid;

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub const PIPE_PREFIX: &str = r"\\.\pipe\cn.shierkeji.sixa.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    Status,
    DesensitizeFile,
    DesensitizeBatch,
    GetJob,
    WaitJob,
    CancelJob,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub version: u32,
    pub id: String,
    pub method: Method,
    pub params: Value,
}

impl Request {
    pub fn new<T: Serialize>(method: Method, params: &T) -> Result<Self, ProtocolError> {
        Ok(Self {
            version: PROTOCOL_VERSION,
            id: Uuid::new_v4().to_string(),
            method,
            params: serde_json::to_value(params)?,
        })
    }

    pub fn parse_params<T: DeserializeOwned>(&self) -> Result<T, ProtocolError> {
        Ok(serde_json::from_value(self.params.clone())?)
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.version != PROTOCOL_VERSION {
            return Err(ProtocolError::Version {
                expected: PROTOCOL_VERSION,
                actual: self.version,
            });
        }
        if self.id.is_empty() {
            return Err(ProtocolError::InvalidEnvelope("request id is empty"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub version: u32,
    pub id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<IntegrationError>,
}

impl Response {
    pub fn success<T: Serialize>(id: impl Into<String>, result: &T) -> Result<Self, ProtocolError> {
        Ok(Self {
            version: PROTOCOL_VERSION,
            id: id.into(),
            ok: true,
            result: Some(serde_json::to_value(result)?),
            error: None,
        })
    }

    pub fn failure(id: impl Into<String>, error: IntegrationError) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            id: id.into(),
            ok: false,
            result: None,
            error: Some(error),
        }
    }

    pub fn decode_result<T: DeserializeOwned>(&self, request_id: &str) -> Result<T, ProtocolError> {
        self.validate(request_id)?;
        if let Some(error) = &self.error {
            return Err(ProtocolError::Remote(error.clone()));
        }
        let value = self.result.clone().ok_or(ProtocolError::InvalidEnvelope(
            "successful response has no result",
        ))?;
        Ok(serde_json::from_value(value)?)
    }

    pub fn validate(&self, request_id: &str) -> Result<(), ProtocolError> {
        if self.version != PROTOCOL_VERSION {
            return Err(ProtocolError::Version {
                expected: PROTOCOL_VERSION,
                actual: self.version,
            });
        }
        if self.id != request_id {
            return Err(ProtocolError::MismatchedResponseId);
        }
        if self.ok != self.error.is_none() || self.ok != self.result.is_some() {
            return Err(ProtocolError::InvalidEnvelope(
                "response result/error fields are inconsistent",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    AppNotRunning,
    AuthRequired,
    AuthExpired,
    ModelsNotReady,
    InvalidPath,
    UnsupportedFormat,
    ProtocolMismatch,
    JobNotFound,
    TaskFailed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
#[error("{code:?}: {message}")]
pub struct IntegrationError {
    pub code: ErrorCode,
    pub message: String,
}

impl IntegrationError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmptyParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesensitizeFileParams {
    pub source_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesensitizeBatchParams {
    pub source_paths: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobParams {
    pub job_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitJobParams {
    pub job_id: String,
    pub wait_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusResult {
    pub authenticated: bool,
    pub authorization_valid: bool,
    pub models_ready: bool,
    pub supported_formats: Vec<String>,
    pub protocol_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateJobResult {
    pub job_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Analyzing,
    Generating,
    Exporting,
    Completed,
    Partial,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobResult {
    pub job_id: String,
    pub state: JobState,
    pub progress_percent: u8,
    #[serde(default)]
    pub output_paths: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<IntegrationError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelJobResult {
    pub job_id: String,
    pub cancelled: bool,
}

pub fn pipe_name_for_sid(sid: &str) -> Result<String, ProtocolError> {
    let valid = !sid.is_empty()
        && sid.len() <= 184
        && sid.starts_with("S-")
        && sid.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !valid {
        return Err(ProtocolError::InvalidSid);
    }
    Ok(format!("{PIPE_PREFIX}{sid}"))
}

pub fn encode_frame<T: Serialize>(value: &T) -> Result<Vec<u8>, ProtocolError> {
    let body = serde_json::to_vec(value)?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge(body.len()));
    }
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(&body);
    Ok(frame)
}

pub fn decode_frame<T: DeserializeOwned>(frame: &[u8]) -> Result<T, ProtocolError> {
    if frame.len() < 4 {
        return Err(ProtocolError::TruncatedFrame);
    }
    let length = u32::from_le_bytes(frame[..4].try_into().expect("four-byte prefix")) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge(length));
    }
    if frame.len() != length + 4 {
        return Err(ProtocolError::TruncatedFrame);
    }
    Ok(serde_json::from_slice(&frame[4..])?)
}

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("protocol version mismatch (expected {expected}, received {actual})")]
    Version { expected: u32, actual: u32 },
    #[error("frame is too large: {0} bytes")]
    FrameTooLarge(usize),
    #[error("frame is truncated or has trailing data")]
    TruncatedFrame,
    #[error("response id does not match the request")]
    MismatchedResponseId,
    #[error("invalid protocol envelope: {0}")]
    InvalidEnvelope(&'static str),
    #[error("invalid Windows user SID")]
    InvalidSid,
    #[error("JSON protocol error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("desktop application error: {0:?}")]
    Remote(IntegrationError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_frame_round_trip_preserves_contract() {
        let request = Request::new(
            Method::GetJob,
            &JobParams {
                job_id: "job-1".into(),
            },
        )
        .unwrap();
        let decoded: Request = decode_frame(&encode_frame(&request).unwrap()).unwrap();
        assert_eq!(decoded, request);
        assert_eq!(serde_json::to_value(decoded.method).unwrap(), "get_job");
    }

    #[test]
    fn response_rejects_mismatched_request() {
        let response = Response::success(
            "one",
            &CreateJobResult {
                job_id: "job".into(),
            },
        )
        .unwrap();
        assert!(matches!(
            response.decode_result::<CreateJobResult>("two"),
            Err(ProtocolError::MismatchedResponseId)
        ));
    }

    #[test]
    fn error_codes_are_stable() {
        assert_eq!(
            serde_json::to_string(&ErrorCode::AppNotRunning).unwrap(),
            "\"APP_NOT_RUNNING\""
        );
        assert_eq!(
            serde_json::to_string(&ErrorCode::ProtocolMismatch).unwrap(),
            "\"PROTOCOL_MISMATCH\""
        );
    }

    #[test]
    fn pipe_name_only_accepts_sid_shape() {
        assert_eq!(
            pipe_name_for_sid("S-1-5-21-42").unwrap(),
            r"\\.\pipe\cn.shierkeji.sixa.S-1-5-21-42"
        );
        assert!(pipe_name_for_sid("user\\name").is_err());
    }
}
