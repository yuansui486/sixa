# AI 工具接入

私匣是本机文件脱敏工具，支持 PDF、Office、图片和文本文件。Windows 安装包包含 `sixa-mcp.exe`；macOS Apple Silicon 和 Intel 安装包包含原生 `sixa-mcp`，位于应用包内。MCP 程序通过标准输入输出与 AI 工具通信，不监听 HTTP 端口，也不会修改系统 `PATH`。AI 只会收到任务状态、统计信息和结果路径，不会收到文件正文或识别出的实体值。

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

`sixa` 是稳定的配置键，避免部分客户端把中文键拼入工具命名空间时出现兼容问题。支持 MCP 中文元数据的客户端会显示服务标题“私匣 · 本机文件脱敏”和各工具的中文名称。程序文件和工具机器名继续使用英文。Windows 调用前需保持私匣运行，Mac 调用时会尝试自动打开私匣。两端都需要登录已授权的租户账号，并完成所需模型安装。MCP 进程只负责协议转换；任务实际由桌面进程执行，使用桌面中当前启用的识别规则和脱敏方式，不另开一份模型。

## Mac 安装与配置

将 DMG 中的私匣拖入“应用程序”，首次打开并登录，再从“AI 工具接入”复制配置。例如：

```json
{
  "mcpServers": {
    "sixa": {
      "command": "/Applications/私匣.app/Contents/MacOS/sixa-mcp",
      "args": ["serve"]
    }
  }
}
```

以界面复制的实际路径为准；支持用户自己的 Applications 目录、中文及空格路径。不要单独复制应用包内的 MCP 可执行文件，也不要使用 DMG 挂载卷里的临时路径。移动应用或更新后路径改变时，重新复制配置并重连 AI 客户端。

MCP 初始化和工具发现不启动桌面；第一次实际调用时，若未找到私匣通信服务，会通过 macOS 打开同一应用包。启动最多等待 15 秒，并发调用合并启动，失败后 30 秒内不反复打开窗口。启动等待不占用状态查询的 5 秒响应预算。未登录时会提示登录，不代填账号或绕过授权。Windows 继续沿用手动启动方式。

“本机连接自检”验证程序启动、MCP 初始化、工具清单和桌面往返通信。连接成功不代表已登录或模型已安装，请同时查看页面上的桌面会话和本机模型状态。`models_ready` 表示所需模型已安装、可以尝试提交任务，不保证模型已经加载到内存；首次任务可能需要加载，请按任务进度继续等待，加载失败会明确返回任务错误。

若 macOS 拒绝访问桌面、文稿、下载或外接磁盘中的文件，请检查文件读写权限和“系统设置 → 隐私与安全性 → 文件与文件夹”中的私匣权限，或由用户将文件放到可访问的目录。无需默认开启“完全磁盘访问权限”。若自动启动失败，先手动打开私匣查看系统提示，再重新检查状态。

退出私匣会中断任务通信。重新打开后可以重连，但旧 MCP 任务 ID 不会跨桌面重启保存；请检查桌面任务历史和输出后再决定是否重建，不能因断线自动重复提交。

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

`desensitize_file` 接收 `source_path` 和可选的 `output_dir`；`desensitize_batch` 接收 `source_paths` 和可选的 `output_dir`。所有路径必须是当前系统用户可访问的绝对路径，指定的输出目录必须已经存在。未指定输出目录时使用源文件目录。

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

macOS 使用 `/private/tmp/cn.shierkeji.sixa.<uid>/mcp.sock`，目录权限为 `0700`，socket 为 `0600`，双方检查对端 UID。服务锁防止重复监听，启动时只清理当前用户拥有的失效 socket，不删除普通文件或符号链接。最多同时接受 16 个连接，限制异常帧和空闲连接。MCP 不读取钥匙串；登录与模型仍由桌面管理。

## 开发验证（不下载生产模型）

- `cargo test -p integration-protocol -p sixa-mcp`：协议、提示、路径和 Mac socket 权限/生命周期测试。
- `node scripts/mcp-smoke.mjs --metadata <sixa-mcp 路径>`：从真实安装包启动程序，检查握手、六个工具及中文元数据，确认关闭标准输入后退出。
- Mac 上 `node scripts/mcp-smoke.mjs --desktop target/debug/sixa target/debug/sixa-mcp`：隔离数据目录，实际调用未登录桌面，验证权限门禁、六个工具、异常帧和崩溃重连。只在调试构建支持测试目录，不注入登录或模型旁路。
- Mac 上 `node scripts/mcp-smoke.mjs --launch target/debug/sixa-mcp`：使用离线测试应用包，实际调用 LaunchServices，检查中文空格路径、并发自动启动、启动超时及冷却；不调用真实模型或鉴权服务。
- CI 不下载生产模型、不访问 OSS。已登录并安装模型的真实 Mac 仍需验收 PDF、Word、图片和批量的完整输出、报告、取消与部分失败；通信检查不能替代模型推理验收。

## Windows 升级与卸载

安装器会先处理正在运行的私匣主程序，再自动停止当前用户、当前安装目录中的全部 `sixa-mcp.exe` 实例，等待程序文件释放后继续。AI 客户端可以保持打开；升级后请启动私匣，并在 AI 客户端中重新连接私匣 MCP。升级期间已有 MCP 连接会断开，正在运行的任务应在升级前完成。

如果客户端持续自动重启 MCP，或者文件因权限、安全软件而无法访问，安装器会停止操作并提供“重试／取消”。请暂时停用客户端中的私匣 MCP 连接，再点击“重试”。静默安装遇到 MCP 清理失败时返回退出码 `10`，不会跳过该文件并报告成功。
