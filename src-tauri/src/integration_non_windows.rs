use crate::AppState;
use domain::Result;
use serde::Serialize;
use tauri::{AppHandle, State};

const SUPPORTED_FORMATS: &[&str] = &[
    "txt", "md", "docx", "xlsx", "xlsm", "pdf", "png", "jpg", "jpeg", "bmp", "tif", "tiff",
];

#[derive(Clone, Default)]
pub struct JobRegistry;

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

#[tauri::command]
pub fn integration_info(state: State<'_, AppState>) -> IntegrationInfo {
    IntegrationInfo {
        enabled: false,
        mcp_available: false,
        authenticated: state.auth.require_authenticated().is_ok(),
        models_ready: state.model.lock().map(|model| model.ready).unwrap_or(false),
        executable_path: String::new(),
        protocol_version: "Windows 专属".into(),
        supported_formats: SUPPORTED_FORMATS.to_vec(),
    }
}

#[tauri::command]
pub async fn integration_check() -> IntegrationCheck {
    IntegrationCheck {
        ok: false,
        message: "当前版本的 MCP 本机调用仅支持 Windows；macOS 桌面脱敏功能不受影响".into(),
    }
}

pub fn start(_app: AppHandle) -> Result<()> {
    Ok(())
}
