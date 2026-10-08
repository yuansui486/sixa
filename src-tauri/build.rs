fn main() {
    // MuPDF debug builds link the debug C++ runtime. Tauri's release CRT
    // override conflicts with it when linking the desktop test executable.
    let mut attributes = tauri_build::Attributes::new();
    if std::env::var("PROFILE").as_deref() == Ok("debug") {
        attributes = attributes
            .windows_attributes(tauri_build::WindowsAttributes::new().static_vc_runtime(false));
    }
    tauri_build::try_build(attributes).expect("无法生成桌面构建配置");
}
