# 第三方组件

本项目源码采用 `AGPL-3.0-or-later`，完整文本见 `LICENSE`。应用仍为发布候选；正式分发前会依据锁文件生成完整的软件物料清单并完成许可证复核。

- Tauri、React、TypeScript、Vite、Tokio、Serde、Rusqlite 等依赖保持各自上游许可证。
- docx-preview 0.4.1：Apache-2.0，用于本机 Word 轻量排版；JSZip：MIT 或 GPL-3.0 双许可证，本项目选择 MIT。两者上游许可证随安装包保存在 `licenses/`。
- ONNX Runtime：MIT；Windows 模型包包含对应运行库、`LICENSE` 和 `ThirdPartyNotices.txt`，macOS 安装包固定捆绑官方 1.23.2 universal2 运行库。
- RaNER：来源 `iic/nlp_raner_named-entity-recognition_chinese-base-generic`；本地模型 README 的 metadata 声明 Apache License 2.0。转换权重不随基础安装包发布；公开模型分发前需一并保留上游通知和来源。
- MuPDF：AGPL。Rust 桌面版使用 MuPDF 渲染、重建并校验 PDF；本项目整体以 `AGPL-3.0-or-later` 分发。
- Noto Sans SC：SIL Open Font License 1.1。字体内置于应用，用于跨平台中文替换文字；安装包附带 `licenses/NotoSansSC-OFL.txt`，来源和固定 SHA-256 见 `crates/formats/assets/fonts/README.md`。
- 其他 Rust 和 npm 依赖的具体版本及许可证以 `Cargo.lock`、各 crate 元数据和 `ui/package-lock.json` 为准。

模型、用户文件和真实简历不提交到源码仓库。客户运行环境不需要 Python、PaddlePaddle 或 PyTorch。
