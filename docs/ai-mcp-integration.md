# AI 工具接入

私匣是本机文件脱敏工具，支持 PDF、Office、图片和文本文件。安装包会同时安装桌面应用和 `sixa-mcp.exe`。MCP 程序通过标准输入输出与 AI 工具通信，不监听 HTTP 端口，也不会修改系统 `PATH`。AI 只会收到任务状态、统计信息和结果路径，不会收到文件正文或识别出的实体值。

在桌面应用的“AI 工具接入”页可以复制当前安装路径和通用配置：

```json
{
  "mcpServers": {
    "sixa": {
      "command": "C:\\完整安装路径\\sixa-mcp.exe",
      "args": ["serve"]
    }
  }
}
```

`sixa` 是稳定的配置键，避免部分客户端把中文键拼入工具命名空间时出现兼容问题。支持 MCP 中文元数据的客户端会显示服务标题“私匣 · 本机文件脱敏”和各工具的中文名称。程序文件和工具机器名继续使用英文。调用前需要保持私匣运行、登录已授权的租户账号，并完成所需模型加载。MCP 进程只负责协议转换；任务实际由桌面进程执行，使用桌面中当前启用的识别规则和脱敏方式。

## AI 调用流程

AI 客户端应按以下顺序调用，确保耗时较长的 OCR、PDF 和批量任务能够完整执行：

1. 调用 `desensitization_status`。只有桌面会话、租户授权和所需模型全部就绪后才能创建任务。
2. 调用 `desensitize_file` 或 `desensitize_batch`，保存响应中的 `job_id`。创建成功只表示任务已进入队列。
3. 使用同一个 `job_id` 反复调用 `wait_desensitization_job`，建议每次等待 60 秒。等待超时不代表任务失败，应继续调用等待工具，不要重复创建任务。
4. 遇到 `queued`、`analyzing`、`generating` 或 `exporting` 时继续等待；遇到 `completed`、`partial`、`failed` 或 `cancelled` 时停止等待。
5. `completed` 时向用户提供输出文件和报告路径；`partial` 时明确说明只有部分文件成功，并同时提供输出和报告路径；`failed` 时说明错误和建议的恢复操作。
6. 用户要求取消时调用 `cancel_desensitization_job`，随后继续等待，直到任务进入 `cancelled` 或其他结束状态。

AI 不应自行扫描目录、猜测文件路径或处理用户没有明确指定的文件，也不应声称已经读取、检查或验证了脱敏后的正文。

## 工具

| 工具 | 用途 |
| --- | --- |
| `desensitization_status` | 检查桌面会话、租户授权、模型、协议版本和支持格式 |
| `desensitize_file` | 创建单文件自动脱敏任务，立即返回 `job_id` |
| `desensitize_batch` | 创建 1-20 个文件的批量任务，立即返回 `job_id` |
| `get_desensitization_job` | 查询任务状态、进度、实体总数和输出路径 |
| `wait_desensitization_job` | 等待任务变化，单次最长 60 秒 |
| `cancel_desensitization_job` | 取消排队中或执行中的任务 |

`desensitize_file` 接收 `source_path` 和可选的 `output_dir`；`desensitize_batch` 接收 `source_paths` 和可选的 `output_dir`。所有路径必须是当前 Windows 用户可访问的绝对路径，指定的输出目录必须已经存在。未指定输出目录时使用源文件目录。

单文件默认命名为 `原文件名_已脱敏.扩展名`，批量结果默认命名为 `批量_已脱敏.zip`。源文件和已有结果不会被覆盖；名称冲突时自动追加 `(2)`、`(3)`。同目录还会生成只含任务状态、实体类型计数和输出路径的 JSON 报告，不包含正文或识别出的实体值。

任务状态包括 `queued`、`analyzing`、`generating`、`exporting`、`completed`、`partial`、`failed` 和 `cancelled`。桌面只保留最近 256 个已结束的 MCP 任务状态；输出文件和桌面任务历史不受此内存上限影响。

## 错误码

| 错误码 | 处理方式 |
| --- | --- |
| `APP_NOT_RUNNING` | 启动私匣，等待桌面应用就绪后重新检查状态 |
| `AUTH_REQUIRED` | 在私匣中登录，再重新检查状态 |
| `AUTH_EXPIRED` | 联网刷新登录和租户授权，再重新检查状态 |
| `MODELS_NOT_READY` | 在“模型管理”中准备对应模型，再重新检查状态 |
| `INVALID_PATH` | 请用户提供存在且可访问的绝对路径；不要自行扫描目录 |
| `UNSUPPORTED_FORMAT` | 根据状态工具返回的支持格式，请用户更换文件 |
| `PROTOCOL_MISMATCH` | 安装同一版本的私匣桌面应用和 MCP 程序 |
| `JOB_NOT_FOUND` | 核对保存的 `job_id` 和已有输出；不要直接重复创建任务 |
| `TASK_FAILED` | 告知用户错误信息，并建议在桌面任务历史中检查详情 |
| `CANCELLED` | 告知用户任务已停止，不会生成新的完整结果 |

错误结果中的 `retryable` 表示完成 `recovery_action` 后是否适合重试原调用。创建任务时若 `outcome_unknown` 为 `true`，桌面可能已经成功入队；AI 必须先请用户检查任务历史和输出目录，不得自动重复调用 `desensitize_file` 或 `desensitize_batch`。取消调用通信失败时先查询原 `job_id`，仍在运行才再次取消。

## 本机安全边界

桌面端按当前 Windows 用户 SID 创建命名管道，ACL 只允许当前用户和 `SYSTEM`，并拒绝远程管道客户端。协议帧上限为 1 MiB。MCP 不读取数据库、模型目录、Windows Credential Manager 或登录令牌，也不向 AI 客户端返回源文件内容、识别文本或实体值。AI 客户端能够要求桌面处理当前用户有权访问的任意绝对路径，因此只应在可信的本机 AI 工具中启用这项配置。
