# Mac 推理库加载排查与发布验证

## 1.0.12 修复的问题

故障发生在模型文件读取前。系统拒绝 `dlopen(libonnxruntime.dylib)`，提示调用程序与动态库没有相同 Team ID。主程序启用了 hardened runtime、使用 ad-hoc 签名，不能依赖相同开发者证书满足 library validation。

`src-tauri/entitlements.macos.plist` 声明 `com.apple.security.cs.disable-library-validation`，由 `tauri.macos.conf.json` 配置到签名流程。保留其他 hardened runtime 限制；不要求用户关闭 Gatekeeper、SIP 或执行全局安全设置命令。此权限放宽进程对动态库签名团队的限制，因此只应加载由应用选择的随包库路径，不接受前端或 MCP 提供的库路径。

ort 2.0.0-rc.12 在加载动态库失败时构造 `ort::Error`；这个错误构造又读取 ORT API，从而重入动态库初始化锁。线程采样表现为 `load_dylib_from_path → Error::new_internal → setup_api → load_dylib_from_path`。

`crates/recognition/src/runtime.rs` 先通过系统加载器打开库，验证 `OrtGetApiBase`、版本及目标 API，再调用 `ort::set_api`。失败时使用领域错误，不触发尚未初始化的 ORT API；成功后持有库至进程结束。NER 和 OCR 共用串行初始化，失败不缓存为成功，也不污染后续重试。

## 安装包验收

最终 `.app/Contents/MacOS/sixa --check-inference-runtime` 在创建窗口、读取用户数据和登录之前，只加载自身随包推理库，成功打印 `SIXA_RUNTIME_READY`。此命令不接受自定义库路径、不读取模型、不绕过登录执行任务。

`scripts/macos-runtime-smoke.mjs` 设置 30 秒进程超时：

1. 在最终签名的 DMG 主程序中检查推理库加载。
2. 给独立推理探针设置 hardened runtime，但不授予 library validation 例外；若系统实施拒绝，确认及时返回错误。部分托管 CI 放宽 AMFI，允许此加载时明确记录无法覆盖该反向检查，不误报已验证拒绝路径；用户故障机已实际复现系统拒绝。
3. 使用正式应用的 entitlements 重签探针，验证库缺失和损坏后可以重试，并通过真实生产加载器运行离线 NER/OCR 微型图。
4. 继续验证更新归档与 DMG 的可执行文件、MCP 和动态库完全相同。

仅用 unsigned `cargo run` 或 Python `ctypes.CDLL` 成功加载，不能证明正式应用的签名权限正确。这也是旧 CI 漏掉该问题的原因。

实体模型实机验收记录：2026-10-27，在故障 Mac（arm64、8 GB 内存、macOS 26.5.1）上，应用运行 1.0.11 时确认上述死锁。保留原应用备份，仅补齐签名权限后重启，现有实体模型在 396 ms 内完成 warmup 并报告 ready。人工 TXT 经应用本机 IPC 完成完整生成与保存，识别 4 项实体；未读取用户私人文档。
