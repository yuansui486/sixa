# 本地数据脱敏系统

FastAPI + Presidio 的本地测试版，默认只监听 `127.0.0.1:8765`。文本规则识别、实体复核、稳定占位符脱敏和任务内恢复已经可用；OCR/NER 模型按需初始化，模型目录固定为 `models/`。

## 启动

```powershell
uv sync --extra ai --extra ocr --extra dev
.\run.ps1
```

打开 http://127.0.0.1:8765 。接口文档位于 `/docs`。

## 中文模型与 OCR

RaNER 模型使用 ModelScope 标识 `iic/nlp_raner_named-entity-recognition_chinese-base-generic`，后续接入自定义 Presidio recognizer；图片适配器预留 PaddleOCR 3.x，建议在具备对应 PaddlePaddle wheel 的环境中额外安装：

```powershell
uv pip install paddleocr paddlepaddle
```

当前版本即使未安装或未下载 OCR/NER 模型，规则引擎仍可独立运行。首次模型初始化会下载约 409 MB 的 RaNER 权重；PaddleOCR 会在本机模型目录中准备中文 OCR 模型。

## 最终版文件与批量能力

- 支持 DOCX、XLSX/XLSM、PDF、PNG/JPEG/BMP/TIFF；输出保留原扩展名的脱敏文件。
- Excel 扫描可见/隐藏工作表、单元格、公式、批注和超链接；XLSM 保留宏包但不会执行宏。
- PDF 自动区分文字页和扫描页，扫描页使用 PaddleOCR 后进行页面遮盖。
- 图片支持 OCR 框复核、手工框和按实体类型选择模糊、像素化或纯色遮盖。
- 批量接口默认每批 20 个文件、总计 500 MB，失败项跳过并生成 ZIP 与 `report.json`。
- 可逆模式默认关闭；开启后使用用户口令通过 scrypt + AES-GCM 加密本地映射和原始文件。

环境变量：`TASK_TTL_HOURS`（默认 24）、`MAX_BATCH_FILES`（默认 20）、`MAX_BATCH_BYTES`（默认 524288000）。`.doc` 和 `.xls` 建议先转换为现代 Office 格式。

## 功能入口

- 文本：分析实体、人工复核、占位符脱敏和任务恢复。
- 图片：OCR 框识别以及 Canvas 框选后的高斯模糊。
- 文档：TXT、MD、DOCX、PDF、TIFF 的文本/标准化结果处理。
- 规则：添加词条或正则规则，按实体类型参与分析。
- 任务：查看本地任务和下载 `result.md`、`content.json`、`entities.json`、PNG/ZIP 等产物。

所有用户文件都保存在项目 `data/`，默认服务地址为 `http://127.0.0.1:8765`，不会上传到远程服务。
