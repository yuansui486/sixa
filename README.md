# 私匣 Sixa

私匣是一款 Windows 10/11 x64 本机数据脱敏桌面应用，英文开发名为 Sixa。应用基于 Tauri v2、Rust 和 React 18 / TypeScript，按 AGPL-3.0-or-later 发布。客户安装包不依赖 Python，不开放本机 HTTP 端口；仅前端开发服务器监听 `127.0.0.1:1420`。

支持 TXT/Markdown、PNG/JPEG/BMP/多页 TIFF、DOCX、XLSX/XLSM 和 PDF。工作台提供实体与区域复核、手工框选、结果预览、脱敏导出和加密恢复；还支持批量、历史、识别规则、脱敏方式和模型管理。PDF 提供安全重建与 MuPDF 保真模式；Office 保留 XLSM 的 VBA 原字节并报告移除的失效签名。

私匣使用 Smart Ops 租户用户登录，租户需单独开通“本地数据脱敏”模块。设备会话每 10 分钟在线校验一次，网络中断后可继续使用最近一次签发的 24 小时离线租约。账号、租户和设备授权通过 HTTPS 校验；待处理文件、识别内容、任务历史和规则不会发送到认证服务器。会话令牌保存在 Windows Credential Manager，不进入 WebView 或浏览器存储。

安装包附带通用 MCP stdio 服务，供本机 AI 工具异步创建、查询、等待和取消脱敏任务。桌面应用必须保持运行并已登录；MCP 通过当前 Windows 用户专属的命名管道通信，不开放 HTTP 端口，也不修改 `PATH`。外部调用使用桌面中现有规则和脱敏方式，只返回任务状态、计数及输出路径，不返回正文或实体值。配置和工具说明见 [AI 工具接入](docs/ai-mcp-integration.md)。

首次启动自动安装 RaNER 与轻量 PP-OCRv4；高精度 OCR 可在模型管理页按需安装。模型从固定版本的 [ModelScope 仓库](https://www.modelscope.cn/models/yuansui486/data_desensitization_0918/tags/desktop-models-v1.0.0) 下载，支持中断续传。安装包及解压结果执行 SHA-256 校验；后续启动只检查清单和必需文件。第一套模型加载后工作台即可使用，其他 worker 在后台预热，预热完成前只调度已就绪的 worker。模型页提供手动完整校验和确认后清空单个模型目录并重新下载的“重建模型”。文件内容损坏可能在加载时才报错；对应分析会被阻止。桌面数据继续保存在兼容目录 `%LOCALAPPDATA%\LocalDesensitization\`，升级到私匣后不会重复下载模型或丢失现有任务。

## 构建和验证

需要 Rust 1.88+、Node.js 和 Windows Tauri v2 所需的编译工具链。依次执行：

```powershell
npm.cmd --prefix ui ci
./tools/verify-desktop.ps1
./tools/build-desktop.ps1
```

NSIS 安装包输出到 `target/release/bundle/nsis/`，并包含 `sixa-mcp.exe`。开发时可分别运行 `npm.cmd --prefix ui run dev` 与 `cargo run -p sixa`。模型的本地离线安装入口为 `tools/install-local-model.ps1`，默认读取已转换的 ONNX 模型目录。品牌与开发命名规范见 [私匣品牌说明](docs/brand.md)。

实现与本机验收记录见 [桌面版状态](docs/migration/desktop-status.md)。发布前仍需全新 Windows 虚拟机、LibreOffice、原生 WebView2 自动化和长期性能/格式模糊测试。旧 Python/FastAPI 与 Nuitka 的源文件已移除；本项目 `.venv` 和旧缓存目录的递归删除被当前执行环境拦截，仍待清理。已有 Rust 黄金样本继续用于识别回归。
