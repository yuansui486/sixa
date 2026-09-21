# 桌面迁移状态

日期：2026-09-18。版本：1.0.0。本文记录七项 Rust/Tauri 交付及当时的本机验收结果。全新 Windows 虚拟机和 LibreOffice 等外部环境门禁仍未执行，因此当前状态为发布候选。2026-09-20 按用户决定移除了旧 Python、uv 和 Nuitka 源文件；项目 `.venv` 及旧缓存的递归删除受到执行环境阻止，尚未完成。以下历史验证结果不代表迁移后重新执行了 Python 差分。

2026-09-19 易用性与可靠性复核后又完成以下加固：本地模型优先加载并支持离线重启；默认仅安装 RaNER 与轻量 OCR；前端增加首次准备页、分能力状态、全局进度、自动保存、文件名明确的历史/批量视图和恢复口令确认；图片、PDF 与 Office 的结果预览改用正式生成链路；任务元数据增加父批次、复核版本、存储占用和结构化错误；SQLite 增加版本化迁移与坏记录隔离；PDF 保真模式不再删除同页全部同名文字；图片/TIFF 增加解码前尺寸和总像素限制；恢复包写盘后执行解密自检。

2026-09-20 加入可展开的系统内置脱敏方式目录及真实替换示例。按用户确定的提速取舍，启动只检查模型清单和必需文件，第一套模型加载后立即进入工作台，后续 worker 在后台预热并在就绪后加入调度；安装阶段及模型页手动完整校验仍执行 SHA-256。模型页提供确认后移除指定模型目录并重新下载的“重建模型”；模型加载失败时对应分析不可用。旧 Python/FastAPI 源码、差分脚本、uv 清单及 Nuitka 构建脚本已清理；项目 `.venv` 与旧构建缓存仍需删除。`models/`、`data/`、Rust 黄金样本与共享环境均保留。

2026-09-20 增加通用 MCP stdio 接入。安装包内置独立 sidecar，通过当前 Windows 用户 SID 命名且显式限制 ACL 的本机命名管道连接桌面进程，不监听 HTTP，也不修改 `PATH`。提供状态、单文件、批量、查询、长轮询和取消六项工具；调用使用桌面现有规则与策略。输出采用文件系统原子占名或 `create_new` 方式避重，并发任务不会覆盖源文件或已有结果；结束任务状态只在内存保留最近 256 项。

2026-09-20 商业品牌确定为“私匣”，英文开发名为 `Sixa`。桌面程序、MCP sidecar、包名、命名管道和应用标识统一使用 `sixa`；旧 `%LOCALAPPDATA%\LocalDesensitization\` 数据目录和 Credential Manager 兼容标识继续保留。新 Logo 使用文件进入半封闭匣盒的图形，不再使用通用盾牌图标。

2026-09-20 完成 MCP 中文元数据与 AI 调用闭环。支持中文元数据的客户端显示“私匣 · 本机文件脱敏”，通用配置使用稳定键 `sixa`；服务和六项工具提供中文标题、简介、输入输出 Schema 及只读/幂等注解。创建、等待、取消和错误结果均返回可执行的 `next_action` 或恢复操作；创建请求通信失败时明确标记结果未知并禁止自动重试，避免生成重复任务。桌面“本机连接自检”按 MCP 初始化顺序实际启动 sidecar，并依次检查初始化、工具清单和状态调用。

## 交付结果

| 范围 | 当前实现 |
| --- | --- |
| PP-OCRv4 | mobile/accurate det、cls、rec ONNX；DB 后处理、透视裁切、方向分类、CTC 解码、0.5 丢弃阈值、原图归一化坐标回映射；cls 最多 32 张批处理，rec 按推理宽度分组批处理；保留 Paddle 字典中的全角空格并支持末尾空格类别 |
| 图片 | PNG、JPEG、BMP、多页 TIFF 的解码、OCR、区域复核、手工区域、预览和原格式输出；强模糊叠加局部背景色，按区域适配字号和前景色，180 度 OCR 区域按原方向绘制自然中文替换文字 |
| PDF | MuPDF 安全重建会逐页栅格化后创建新 PDF，移除旧文字层、附件和旧对象；保真模式应用真实 redaction，再插入脱敏图块；导出前检查页数、附件和敏感文字二次提取，检查失败即禁止导出 |
| Office | DOCX、XLSX、XLSM 已接入任务引擎、工作台和批量流程；处理跨 run 文本、表格、页眉页脚、批注、公式及缓存、链接、隐藏表、定义名称、文本框相关 XML 和支持的内嵌图片；未修改 ZIP entry raw-copy，XLSM VBA 原字节保留，失效数字签名移除并报告；OLE/ActiveX 因无法完整审计而拒绝 |
| 模型下载 | 阿里云 OSS 北京公网地址为主源，ModelScope `desktop-models-v1.0.0` 为备用源；跨源保留断点并严格检查 `206 Content-Range`，最终执行固定 SHA-256、原子安装和运行清单校验；UI 显示当前来源及自动切换状态 |
| 取消和并发 | 分析前生成任务 ID；排队、ONNX 推理、逐页图片/PDF、逐个 Office 媒体、逐个 ZIP entry 和批量任务均检查取消；ORT 活跃推理可 terminate；1-4 个独立 Engine worker、信号量限流、原子 worker lease 和 SQLite WAL/busy timeout 支持有界并发 |
| 中文 UI | 固定流程“选择文件 → 自动识别 → 复核实体/区域 → 预览 → 生成并导出”；工作台、批量、历史、识别规则、脱敏方式、模型管理和恢复页面均为中文；批量选择器覆盖全部正式格式并提供批次取消 |

Rust workspace 为 `domain`、`recognition`、`formats`、`storage`、`model-manager`、`task-engine`、`integration-protocol`、`mcp-server` 和 `src-tauri`。React 18 / TypeScript 前端位于 `ui/`。客户运行时不包含 Python，也不开放本机 HTTP 端口。

## 模型发布

主下载目录：[阿里云 OSS desktop-models-v1.0.0](https://tct12.oss-cn-beijing.aliyuncs.com/12box/sixa/models/desktop-models-v1.0.0/desktop/catalog.json)

备用版本：[ModelScope desktop-models-v1.0.0](https://www.modelscope.cn/models/yuansui486/data_desensitization_0918/tags/desktop-models-v1.0.0)

- 双源目录 SHA-256：`94ee999d45dae02e38d54b509ece4788d082f6ea58e2930403f67c24f7a3f21d`
- RaNER 包：`4386188a3453e20f703feb009f3c731a1adea654b87747bfd292cca05ce3e6b1`
- PP-OCRv4 mobile 包：`b260c430ed85d3ebe0bbf37705cdbcf33a11be41be691ded77df470b4e83a832`
- PP-OCRv4 accurate 包：`6e9ded592fa160877168b5d1c51e802210473f645d2800c5c150548f922cde07`
- 三个包下载总量 589,697,647 字节（562.38 MiB），解压总量 677,367,722 字节（645.99 MiB）

真实远端验收在约 1 MiB 时取消 mobile 包下载，确认 `.part` 保留；第二次请求通过 Range 从原偏移继续，完成包 SHA-256、运行清单和原子安装验证。

## 已执行验证

- Rust workspace 与 Tauri：73 项测试通过；覆盖状态机、Unicode/proptest、安全正则、NER/CTC/Viterbi、图片及多页 TIFF、方向绘制、PDF 两种模式、OOXML、任务加密、恢复包、数据库迁移、坏记录隔离、并发 SQLite 写入、批量、取消、MCP 中文元数据、AI 调用闭环、错误恢复、创建结果未知保护、并发输出避重、路径校验和任务状态清理。`cargo clippy --workspace --all-targets -- -D warnings` 通过。
- Python 差分基线（清理前历史记录）：112 passed，2 skipped。旧实现已按用户决定移除；现有 Rust 黄金样本保留。
- RaNER 黄金差分：7 个样本、15 个实体，span/type 一致，FP32 分数误差不超过 `1e-4`。
- PP-OCRv4 黄金差分：mobile 与 accurate 均为 4/4 文本和阅读顺序一致；最小 polygon IoU 分别为 `0.8576887519`、`0.9103483793`。
- 图片：PNG/JPEG/BMP 解码和回写、多页 TIFF 页数保持、仅选中区域发生像素变化、取消在页边界生效等测试通过。
- PDF 合成验收：安全重建移除旧文字层和附件；保真模式移除敏感文字和附件、保留公开文字及区域外像素。
- 私有简历验收：6 份、11 页，安全重建和保真模式共 12 个案例全部通过。六份均识别姓名、机构和地点；所有源文档中实际存在手机号的样本均识别出电话。报告不含文件名、路径或正文，只保存在忽略目录 `models/acceptance/`。
- Office 验收：DOCX/XLSX/XLSM 敏感文字移除，三者内嵌媒体均变化；真实 Excel 生成且含 VBA 项目的 XLSM 保持 `vbaProject.bin` SHA-256 不变；三个输出通过 `officecli validate`，并由 Microsoft Word/Excel 原生打开和回读。LibreOffice 未安装。
- ModelScope：真实网络取消、续传、校验和安装通过。
- 前端：Vitest 4 项、TypeScript/Vite production build、Playwright Chromium 两种桌面尺寸 20 项通过；覆盖中文复核流程、模型错误阻断、高级设置、恢复口令确认、AI 接入配置和紧凑桌面布局。
- MCP 真实链路：release sidecar 的 stdio 初始化、中文服务信息、六项工具元数据和命名管道状态调用通过；此前 TXT 创建/等待/导出和运行中取消验收继续有效。源文件及预先存在的同名输出保持不变，手机号和邮箱均被脱敏，报告不含正文或实体值。测试结束后已退出租户会话。
- Windows 包：x64 NSIS 构建通过；本机静默安装后同时存在主程序与 MCP sidecar；从安装目录启动、sidecar 状态调用、无 TCP 监听、静默卸载及目录清理通过。

## 构建和测量

- NSIS：`target/release/bundle/nsis/私匣_1.0.0_x64-setup.exe`
- 安装包：9,577,104 字节，SHA-256 `33f5b3073785a206b52e97799d6a6f500faa1a496dcbc9a54c0b7c3e7a1ca32c`
- 主程序 `sixa.exe`：29,328,896 字节，SHA-256 `6d2f779eeea29c5a08e429dc13eb58c40eae209b6983335db73bf6ba159a9272`
- MCP sidecar `sixa-mcp.exe`：3,426,816 字节，SHA-256 `092dd7357f5a2d7c58178d25621c0f3f0cae066547b920d889d33f2bdb8412d6`
- 上一构建本机静默安装文件总量：32,580,756 字节，不含按需下载模型和 `%LOCALAPPDATA%` 任务数据；本次重建保留现有 `D:\私匣` 安装，未覆盖测量
- 安装版启动后 8 秒采样：主进程工作集约 26.7 MiB、峰值约 26.7 MiB；不含 WebView2 子进程及后续模型会话
- 六份私有简历 accurate 验收：单案例 568,981-1,285,052 ms；验收进程采样峰值工作集 3,403 MiB。该数据受页面内容和本机构建并发影响，不作为正式客户性能承诺

机器可读数据见 [build-report.json](build-report.json)。

## 仍需外部环境验收

1. 在全新 Windows 10/11 x64 虚拟机中验证无 Python、中文路径、非管理员用户、断网续传、离线重启、安装和卸载。
2. 在 LibreOffice 中原生打开 DOCX/XLSX/XLSM；本机只完成 Microsoft Word/Excel 和 officecli 验收。
3. 使用原生 WebView2 自动化补充端到端测试；当前 Playwright 使用模拟 Tauri IPC，本机仅做原生启动存活检查。
4. 扩大真实 OCR/PDF/Office 差分集，执行长期并发、格式 fuzz 和正式性能基准，完成第三方许可证及发布签名审计。

旧 Python/uv/Nuitka 源文件已根据用户的明确决定清理；目录级环境和缓存清理仍未完成。剩余门禁仍须在正式发布前完成，不应将历史验收结果当作新环境中的验证。

## 可重复命令

```powershell
$env:LIBCLANG_PATH='C:\Program Files\LLVM\bin'
$env:CARGO_BUILD_JOBS='1'
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
npm.cmd --prefix ui test -- --run
npm.cmd --prefix ui run build
npm.cmd --prefix ui run test:e2e
./tools/build-desktop.ps1
./tools/smoke-native.ps1
```
