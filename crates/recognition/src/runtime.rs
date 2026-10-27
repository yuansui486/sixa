//! Load the native API without constructing ort::Error before the API exists.
//! ort rc.12's dynamic-loader error path re-enters its own OnceLock and deadlocks.
use domain::{Error, Result};
use libloading::Library;
use std::{ffi::CStr, path::Path, sync::Mutex};

// The API contains function pointers into the library: retain it for process lifetime.
// Serialize initialization across NER/OCR workers; a failed load remains retryable.
static RUNTIME: Mutex<Option<Library>> = Mutex::new(None);

pub fn initialize(path: &Path) -> Result<()> {
    let mut runtime = RUNTIME
        .lock()
        .map_err(|_| failure("推理引擎初始化状态异常"))?;
    if runtime.is_some() {
        return Ok(());
    }
    let path = path
        .canonicalize()
        .map_err(|e| failure(format!("无法访问推理库 {}：{e}", path.display())))?;
    // Only the platform library path selected by the application is accepted.
    // SAFETY: the shipped native library implements the ONNX Runtime C ABI.
    let library = unsafe { Library::new(&path) }.map_err(|e| {
        failure(format!(
            "系统无法加载推理库 {}：{e}。请重新安装当前版本的私匣；无需重新下载模型",
            path.display()
        ))
    })?;
    // SAFETY: OrtGetApiBase is the version-independent ONNX Runtime ABI entrypoint.
    let get_base = unsafe {
        library
            .get::<unsafe extern "system" fn() -> *const ort::sys::OrtApiBase>(b"OrtGetApiBase\0")
    }
    .map_err(|_| failure("推理库缺少 OrtGetApiBase，请重新安装私匣"))?;
    let base = unsafe { get_base().as_ref() }.ok_or_else(|| failure("推理库返回空 API"))?;
    let version = unsafe { (base.GetVersionString)() };
    if version.is_null() {
        return Err(failure("推理库未提供版本信息"));
    }
    let version = unsafe { CStr::from_ptr(version) }.to_string_lossy();
    let minor = version
        .split('.')
        .nth(1)
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0);
    if minor < ort::MINOR_VERSION {
        return Err(failure(format!(
            "推理库版本 {version} 过旧，需要 1.{} 或更高版本",
            ort::MINOR_VERSION
        )));
    }
    // SAFETY: query exactly the API version matching the Rust bindings. A null
    // result signals incompatibility; no ort error constructor is called here.
    let api = unsafe { (base.GetApi)(ort::sys::ORT_API_VERSION) };
    if api.is_null() {
        return Err(failure("推理库不支持所需 API，请重新安装私匣"));
    }
    if !ort::set_api(unsafe { api.read() }) {
        return Err(failure("推理 API 已由其他路径初始化，请重启私匣"));
    }
    *runtime = Some(library);
    ort::init().commit();
    Ok(())
}

fn failure(message: impl Into<String>) -> Error {
    Error::ModelsNotReady(message.into())
}
