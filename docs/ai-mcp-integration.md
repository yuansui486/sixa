# AI 工具接入

私匣安装包会同时安装桌面应用和 `sixa-mcp.exe`。MCP 程序是标准输入输出服务器，不监听 HTTP 端口，也不会修改系统 `PATH`。在桌面应用的“AI 工具接入”页可以复制当前安装路径和通用配置：

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

调用前需要保持私匣运行、登录已授权的租户账号，并完成所需模型加载。MCP 进程只负责协议转换；任务实际由桌面进程执行，使用桌面中当前启用的识别规则和脱敏方式。

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

任务状态包括 `queued`、`analyzing`、`generating`、`exporting`、`completed`、`partial`、`failed` 和 `cancelled`。调用方应保存创建接口返回的 `job_id`，使用 `wait_desensitization_job` 长轮询，直到进入结束状态。桌面只保留最近 256 个已结束的 MCP 任务状态；输出文件和桌面任务历史不受此内存上限影响。

## 错误码

| 错误码 | 处理方式 |
| --- | --- |
| `APP_NOT_RUNNING` | 启动私匣 |
| `AUTH_REQUIRED` | 在私匣中登录 |
| `AUTH_EXPIRED` | 联网刷新登录和租户授权 |
| `MODELS_NOT_READY` | 在模型管理中安装并加载对应模型 |
| `INVALID_PATH` | 使用存在且可访问的绝对路径 |
| `UNSUPPORTED_FORMAT` | 改用状态接口返回的支持格式 |
| `PROTOCOL_MISMATCH` | 更新桌面应用及随附的 MCP 程序 |
| `JOB_NOT_FOUND` | 检查 `job_id`，或重新创建已过期任务 |
| `TASK_FAILED` | 查看错误信息并在桌面任务历史中检查详情 |
| `CANCELLED` | 任务已按请求停止 |

## 本机安全边界

桌面端按当前 Windows 用户 SID 创建命名管道，ACL 只允许当前用户和 `SYSTEM`，并拒绝远程管道客户端。协议帧上限为 1 MiB。MCP 不读取数据库、模型目录、Windows Credential Manager 或登录令牌，也不向 AI 客户端返回源文件内容、识别文本或实体值。AI 客户端能够要求桌面处理当前用户有权访问的任意绝对路径，因此只应在可信的本机 AI 工具中启用这项配置。
