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

## 功能入口

- 文本：分析实体、人工复核、占位符脱敏和任务恢复。
- 图片：OCR 框识别以及 Canvas 框选后的高斯模糊。
- 文档：TXT、MD、DOCX、PDF、TIFF 的文本/标准化结果处理。
- 规则：添加词条或正则规则，按实体类型参与分析。
- 任务：查看本地任务和下载 `result.md`、`content.json`、`entities.json`、PNG/ZIP 等产物。

所有用户文件都保存在项目 `data/`，默认服务地址为 `http://127.0.0.1:8765`，不会上传到远程服务。
