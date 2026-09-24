# 关闭窗口与托盘

默认点击关闭按钮会询问“最小化到托盘 / 直接退出 / 取消退出”。勾选“不再提醒”后，成功执行的选择保存在本机 SQLite 的独立桌面设置中；可在“应用设置 → 关闭窗口”更改。取消退出不会保存本次选择。

最小化保留当前 WebView、复核队列和后台任务。Windows 托盘左键恢复窗口；右键菜单提供打开私匣、应用设置和退出私匣。再次启动会恢复并聚焦现有窗口。托盘退出始终执行退出流程，不受最小化偏好影响。

退出会先保存复核修改，再确认是否停止任务和下载。保存失败可重试或取消；保存、停止超过 5 秒可继续等待或明确强制退出。强制退出前仍会确认运行中的操作，可能丢失未保存修改或中断输出文件。取消停止后允许新任务进入，已取消的旧队列不会重新执行。

关闭事件由 Rust 接管，登录页同样可用。前端必须确认接收关闭请求；未响应时 3 秒后使用原生对话框。已显示提示后前端失去响应，再点关闭会重新检测。未创建成功的托盘不会提供隐藏窗口选项。

任务、模型操作和原生阻塞工作使用活动 guard；退出先关闭新工作入口，取消任务和下载，并等待收尾。复核保存与任务收尾仍能写入，活动数归零后不会再接受新的写操作。外部 AI 启动的 MCP 进程不由桌面退出流程终止，安装器仍负责升级时清理同安装目录的 MCP。

## 验证

- Rust：`cargo test -p sixa -p storage -p domain --locked`。
- 静态检查：`cargo clippy -p sixa -p storage -p domain --all-targets --locked -- -D warnings`。
- UI：在 `ui` 运行 `npm test`、`npm run test:e2e`、`npm run build`。浏览器测试模拟 Tauri IPC。
- 原生 Windows：先构建 UI，再运行 `cargo build -p sixa --features custom-protocol --locked`，使用 PowerShell 7 运行 `src-tauri/tests/desktop-smoke.ps1`。需要可交互的桌面和通知区域；测试通过 Win32 关闭消息、实际托盘通知和菜单命令、UI Automation 验证未登录关闭、取消、最小化、恢复、单实例、退出、重启后记忆，以及没有前端时的原生回退。脚本会按需展开通知区域隐藏图标。
- `SIXA_DESKTOP_TEST_ROOT` 与 `SIXA_DESKTOP_TEST_NO_UI` 仅调试构建读取；测试使用独立实例标识、数据目录、测试密钥，跳过生产 MCP 管道。正式构建不读取这些变量，也不开放调试端口。测试不会登录、下载模型或覆盖现有安装。
- 安装器：`src-tauri/installer/tests/run.ps1`；仍须覆盖 MCP 占用、被重启、静默安装等回归。

2026-09-23：Windows 原生验收通过；Rust 44 项、Vitest 26 项、Playwright 两档桌面尺寸 50 项、安装器 10 组回归通过，严格 Clippy 通过。已重新构建 `target/release/bundle/nsis/私匣_1.0.8_x64-setup.exe`。macOS 的原生托盘与 Dock 恢复尚未在本机实测。
