fn main() {
    // Tauri embeds the Common Controls v6 manifest for the desktop binary only.
    // The native updater example also links dialog code, but is never packaged.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-arg-examples=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-examples=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
        );
    }
    // MuPDF debug builds link the debug C++ runtime. Tauri's release CRT
    // override conflicts with it when linking the desktop test executable.
    let mut attributes = tauri_build::Attributes::new();
    if std::env::var("PROFILE").as_deref() == Ok("debug") {
        attributes = attributes
            .windows_attributes(tauri_build::WindowsAttributes::new().static_vc_runtime(false));
    }
    tauri_build::try_build(attributes).expect("无法生成桌面构建配置");
}
