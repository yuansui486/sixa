# 本地数据脱敏系统

一个只在本机处理文件的中文数据脱敏平台。后端使用 FastAPI，`uv` 管理环境；Presidio 的 `RecognizerResult` 作为统一实体协议，并由 `AnonymizerEngine` 执行文本实体替换；规则识别和中文 RaNER NER 共同提供文本实体，PaddleOCR 负责图片及扫描 PDF 的文字框识别。默认只监听 `127.0.0.1:8765`，上传内容不会发送到远程服务。

## 启动

```powershell
uv sync --extra ai --extra ocr --extra dev
.\run.ps1
```

打开 http://127.0.0.1:8765 。接口文档位于 `/docs`。

## 中文模型与 OCR

RaNER 模型使用 ModelScope 标识 `iic/nlp_raner_named-entity-recognition_chinese-base-generic`。图片和扫描 PDF 使用 PaddleOCR **2.10.0**（配套 PaddlePaddle 3.0.0 CPU wheel），模型文件保存在项目 `models/` 或 Paddle 的本机缓存目录。首次点击“初始化 RaNER / OCR”会下载模型，耗时和磁盘占用取决于网络速度。

未安装或未初始化模型时，内置正则和自定义规则仍可处理文本、Office 和可搜索 PDF。扫描 PDF 必须先初始化 OCR，并在模型就绪后重新上传分析；如果分析结果包含 OCR 失败、页数超限或 OCR 未初始化警告，服务会拒绝生成脱敏文件，避免返回仍含原图文字的“假成功”结果。

Windows CPU 环境的 OCR 推理固定在单线程 worker，并显式关闭 oneDNN/MKL-DNN。若本机已有冲突的 Paddle wheel，请按项目锁定版本重新同步：

```powershell
uv sync --extra ai --extra ocr --extra dev
```

## 最终版文件与批量能力

- 支持 TXT/Markdown、DOCX、XLSX/XLSM、PDF、PNG/JPEG/BMP/TIFF；输出保留原扩展名的脱敏文件。
- Excel 扫描可见/隐藏工作表、单元格、公式、批注、超链接和定义名称；XLSM 保留宏包但不会执行宏。
- PDF 自动区分文字页和扫描页，扫描页使用 PaddleOCR 后进行页面遮盖。
- 图片支持 OCR 框复核、手工框和按实体类型选择模糊、像素化或纯色遮盖。
- 批量接口默认每批 20 个文件、总计 500 MB，失败项跳过并生成 ZIP 与 `report.json`；待复核批次在刷新页面后可以从批量页或任务历史继续。
- 可逆模式默认关闭；开启后使用用户口令通过 scrypt + AES-GCM 加密本地映射和原始文件。

当前不直接解析旧式二进制 `.doc` / `.xls`。请先用 Microsoft Office 或 LibreOffice 转换为 DOCX/XLSX；上传时服务会按扩展名和文件签名校验，损坏或加密的 Office 压缩包以及密码保护的 PDF 会被明确拒绝。

环境变量：`LOCAL_DESENSITIZATION_DATA_DIR`（默认 `data/`）、`LOCAL_DESENSITIZATION_MODEL_DIR`（默认 `models/`）、`TASK_TTL_HOURS`（默认 24）、`CLEANUP_INTERVAL_SECONDS`（默认 300）、`MAX_BATCH_FILES`（默认 20）、`MAX_BATCH_BYTES`（默认 524288000）、`MAX_IMAGE_PIXELS`（默认 40000000）、`MAX_PDF_OCR_PAGES`（默认 200）和 `MAX_PDF_OCR_PIXELS`（默认 12000000）。单文件上传上限为 50 MB。

## 功能入口

- 文本：分析实体、人工复核、策略化脱敏和任务内恢复。
- 图片：OCR 框识别、Canvas 手工框选、模糊/像素化/纯色/文字遮盖。
- 文档：TXT、MD、DOCX、XLSX/XLSM、PDF 的文本/页面区域复核与标准化输出。
- 规则：添加词条或正则规则，按实体类型参与分析。
- 策略：按实体类型配置文本动作和图片动作，并持久化到本地 SQLite。
- 批量：上传多个文件，统一复核实体/页面区域后生成 ZIP 和脱敏报告。
- 任务：查看本地任务、下载脱敏文件/报告/恢复文件并删除任务。

所有用户文件都保存在项目 `data/`，默认任务保留 24 小时后自动清理（可通过 `TASK_TTL_HOURS` 调整）。默认服务地址为 `http://127.0.0.1:8765`，不会上传到远程服务。`data/`、`models/` 和运行时临时目录已加入 `.gitignore`。

## 接口与测试

主要接口为 `/api/text/*`、`/api/files/analyze`、`/api/files/mask`、`/api/image/*`、`/api/document/*`、`/api/batches/*`、`/api/policies`、`/api/rules` 和 `/api/tasks`；完整请求模型可在 `/docs` 查看。

运行回归测试（会生成真实 DOCX、XLSX、XLSM、PDF 和图片到 pytest 临时目录）：

```powershell
uv run pytest -q
uv run python -m compileall -q app
```

本机已下载 RaNER 与 PaddleOCR 模型后，可执行真实模型验收：

```powershell
$env:RUN_REAL_MODELS='1'
uv run pytest -q tests/test_real_models.py
```
