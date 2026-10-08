# 桌面应用接入 OSS 自动更新

面向接入私匣、灵雀及后续桌面产品的开发团队。目标是用户确认后，由应用完成下载、验签、安装、启动新版，并保留用户数据。下载完成或 CI 编译成功都不等于升级验收通过。

本文以 Tauri v2 为可运行的参考方案；Electron 等框架可沿用目录隔离、权限和发布顺序，但须使用自己更新器要求的清单、签名与安装包，不能直接读取本文的 Tauri 清单。更新服务只需要 OSS 静态文件，不要求新增登录接口或在本机开放生产 HTTP 服务。

私匣日常发版操作见 [应用更新与发布](automatic-updates.md)。本文记录的灵雀代码基线：`E:/MyProject/nuphus`，`edition/lingque` 提交 `15c04a1`；参考上游 `upstream/main` 提交 `8bb370f`。灵雀接入步骤是待实施方案，不代表它已经开启自动更新。

## 1. 先确定产品身份与更新范围

每个产品独立使用产品代号、OSS 前缀、更新签名密钥、版本号和发布流程。私匣为 `sixa`，灵雀为 `lingque`。同一应用后续发版必须沿用原签名密钥；新产品不能复用私匣或上游 Nuphus 的私钥。

升级沿用原来的应用标识、主程序名称、安装目录和数据目录。界面品牌改名不意味着可以同时修改这些标识。登录、任务、模型和项目数据库应在用户数据目录，不能放在随安装覆盖的目录。涉及数据结构升级时使用独立、可恢复的数据迁移，先验证旧数据可读。

| 项目 | 私匣 | 灵雀接入时 |
| --- | --- | --- |
| 产品代号 | `sixa` | `lingque` |
| 更新前缀 | `12box/sixa/updates/` | `12box/lingque/updates/` |
| 平台 | Windows x64、Mac ARM64、Mac x64 | 先沿用现有 Windows x64、Mac ARM64；其他平台验收后再加入 |
| 有效版本来源 | Tauri、Cargo workspace、前端版本保持一致 | 使用合并后的 Workbench Tauri 配置版本，不使用上游版本代替 |
| 自动检查 | 启动延迟 5 秒，每天一次，可关闭 | 推荐相同默认值 |
| 下载/安装 | 用户分别确认，安装前保存并检查工作状态 | 同样处理画布、工作流、定时调度和外部调用 |

## 2. 文件、清单与签名

使用北京 OSS 桶 `tct12`，对象公共读取、禁止公共写入。客户端不保存 AccessKey，也不需要用户配置 OSS。Tauri 原生网络请求不依赖浏览器 CORS；不要为了更新放宽整个 WebView 的 CSP。

```text
12box/<产品代号>/updates/
  stable/latest.json
  releases/<版本>/
    latest.json
    SHA256SUMS.txt
    <产品>_<版本>_windows_x64_setup.exe
    <产品>_<版本>_windows_x64_setup.exe.sig
    <产品>_<版本>_macos_arm64.app.tar.gz
    <产品>_<版本>_macos_arm64.app.tar.gz.sig
    <产品>_<版本>_macos_arm64.dmg
    # Mac x64 仅在支持并通过验证时发布对应文件
```

固定地址为 `https://tct12.oss-cn-beijing.aliyuncs.com/12box/<产品代号>/updates/stable/latest.json`。模型放在各产品的 `models/`，与应用更新目录分开。

Tauri 清单示例，版本、域名、文件及签名由构建生成，下面的签名占位符不能直接用于生产：

```json
{
  "version": "0.1.1",
  "notes": "改进工作流编辑与运行体验。",
  "pub_date": "2026-10-08T03:00:00Z",
  "platforms": {
    "windows-x86_64": {
      "url": "https://tct12.oss-cn-beijing.aliyuncs.com/12box/lingque/updates/releases/0.1.1/Lingque_0.1.1_windows_x64_setup.exe",
      "signature": "此处为该 exe.sig 的完整文本内容"
    },
    "darwin-aarch64": {
      "url": "https://tct12.oss-cn-beijing.aliyuncs.com/12box/lingque/updates/releases/0.1.1/Lingque_0.1.1_macos_arm64.app.tar.gz",
      "signature": "此处为该 app.tar.gz.sig 的完整文本内容"
    }
  }
}
```

Windows 更新器下载 NSIS `.exe`，Mac 下载 `.app.tar.gz`。DMG 供首次安装，不能把 DMG 的 URL 配给 `.app.tar.gz` 的签名。用户不会手动解压更新包。`signature` 不是 SHA-256，也不是 `.sig` 的网址；SHA-256 清单用于产物核对，更新签名用于确认来源。

稳定清单设置 `Cache-Control: no-cache, max-age=0, must-revalidate`；版本目录设置一年 immutable。版本目录内的文件不覆盖，更新失败重试应复用同一次构建产物；重新编译导致字节不同，应发布更高版本号。

## 3. 私匣代码如何工作

仓库路径：`E:/Python/shierkeji/data-desensitization`。

| 职责 | 代码位置 | 接入其他产品时的处理 |
| --- | --- | --- |
| 更新状态、下载、验签、缓存、安装 | `src-tauri/src/updates.rs` | 适配产品版本、数据目录、签名公钥和受信任 URL 前缀 |
| 命令注册与后台检查 | `src-tauri/src/main.rs` | 更新管理在登录和模型门禁外初始化 |
| 安装前保存、关闭与任务协调 | `src-tauri/src/lifecycle.rs` | 对接产品自身的任务准入、保存及退出流程 |
| 设置保存 | `crates/domain/src/lib.rs`、`crates/storage/src/lib.rs` | 保存自动检查开关及上次检查时间，不保存密钥 |
| UI 与调用类型 | `ui/src/Updates.tsx`、`ui/src/api.ts` | 可沿用交互和 DTO 思路，接入自己的组件体系 |
| 保存复核修改 | `ui/src/review.ts` | 替换为自身草稿/画布的保存队列 |
| 安装中 MCP 协调 | `crates/integration-protocol/src/updating.rs`、`crates/mcp-server/src/` | 让外部客户端等待或重连，避免重新启动旧桌面程序 |
| Windows 安装钩子 | `src-tauri/installer/hooks.nsh`、`stop-mcp.ps1` | 只结束当前用户、当前安装目录下的相关进程 |
| 配置及平台覆盖 | `src-tauri/tauri.conf.json`、`tauri.windows.conf.json`、`tauri.macos.conf.json` | 核对配置合并后真正生效的值 |
| 版本、签名与 OSS 发布 | `tools/release/`、`.github/workflows/release.yml` | 替换产品、平台、路径及版本来源；不是零配置通用 SDK |

### IPC 契约

私匣的这些命令已经实现，返回应用更新状态；接入方可以沿用命名和字段，但不需要暴露安装文件路径或文件内容给前端。

| 命令 | 输入 | 作用 |
| --- | --- | --- |
| `get_app_update_status` | 无 | 读取当前状态 |
| `set_app_update_preferences` | `automatic: boolean` | 保存自动检查开关 |
| `check_app_update` | 无 | 获取清单、检查平台与版本、识别可复用缓存 |
| `download_app_update` | 无 | 下载已检查的更新，完成签名校验 |
| `cancel_app_update` | 无 | 请求取消下载 |
| `install_app_update` | 无 | 检查空闲状态、再次验签、协调进程并安装 |

状态通过 `app-update-progress` 推送，包含 `revision`、`current_version`、`version`、`notes`、`phase`、`automatic`、`last_check`、`downloaded`、`total`、`bytes_per_second`、`eta_seconds` 和 `error`。前端按 `revision` 忽略迟到状态，不用高频轮询。阶段包括 `idle/checking/current/available/downloading/verifying/ready/installing/failed`。

下载进度限频；完整包存磁盘，前端只收计数。文件长度未知时显示不定进度，不显示虚假的 0% 或 100%。可取消或后台下载，完整缓存重启后重新核对版本与签名再复用。私匣应用更新尚不支持未完成文件断点续传，不能把模型下载的断点能力当成应用更新能力。

### 安装与退出边界

1. 前端进入安装状态并禁止继续编辑，等待草稿保存完成；保存失败不调用安装。
2. 后端原子检查并阻止新任务进入。有运行中的处理、导出、模型准备或外部调用时拒绝安装，而不是先退出再发现未保存。
3. 重新验证下载缓存；建立短期安装标记，暂停 MCP 自动启动。Windows 预先释放 MCP 文件，NSIS 中再次检查，覆盖安装器启动前的竞争。
4. 使用官方 `tauri-plugin-updater` 安装。Windows 官方更新器启动 NSIS 后退出旧进程；被动模式通过 `/P /UPDATE /R` 请求安装及重新打开。Mac 替换成功后调用应用重启。
5. 新版启动解除安装标记。安装前失败恢复任务准入，明确显示错误，保留仍然有效的完整缓存供重试。

Nuphus 上游前端使用 `update.download()`、`update.install({ restartAfterInstall: true })` 和 `relaunch()`。私匣使用同一官方原生更新器，由 Rust 管理下载和安装状态。两种接法任选一种作为统一安装入口，不要同时由 JS、Rust 各自安装或重复重启。私匣覆盖 Windows `on_before_exit` 回调，是为了安装器启动失败时保留可显示错误的界面；此前已经完成保存及任务冻结，不能只复制空回调而省去这些步骤。

Mac 必须从已安装的 `.app` 运行；从 DMG 或 App Translocation 运行时提示先拖入“应用程序”。权限不足需要由原生安装流程处理或明确提示。Tauri 更新签名不等于 Apple Developer ID 公证，也不等于 Windows Authenticode；不要承诺它能消除所有系统安全提示。

## 4. 给新项目配置构建和发布

1. 建立独立的 Tauri 签名密钥，把公钥写入应用配置，把私钥保存在安全备份及对应仓库 Secrets。已有上线应用必须沿用旧私钥。测试使用另一套临时密钥。
2. 开启 `bundle.createUpdaterArtifacts: true`，配置本产品 OSS endpoint 和公钥；Windows 使用 NSIS/passive。Mac 显式构建 `app,dmg`，避免只生成 DMG 而没有更新归档。明确前端构建钩子的工作目录；Tauri Rust 与前端 API 的主次版本必须一致。
3. 先完成应用、MCP、动态库的最终系统签名，再生成更新归档并签名。生成归档后再次修改或深签 `.app` 会使 DMG 和更新包出现不同内容，应重新生成归档及其签名。
4. CI 配置 `TAURI_SIGNING_PRIVATE_KEY`、`OSS_ACCESS_KEY_ID`、`OSS_ACCESS_KEY_SECRET`。私钥设密码时，另配密码 Secret 并传给 Tauri；密码为空时也应明确设置空字符串。不要把任何发布凭据编译到客户应用。
5. 发版绑定具体提交及标签，校验产品有效版本与标签。所有支持平台通过构建、签名和安装验收后，才生成清单。不同架构下载到独立目录，避免同名 `.app.tar.gz` 相互覆盖。
6. 先验签本地产物，再上传版本目录；用 HEAD 核对大小与 SHA-256 元信息。所有对象成功后才切换稳定清单，并以公开读取方式核对 JSON。GitHub Release 可以作为备用分发，但客户端仍使用 OSS。

RAM 权限示例，将 `lingque` 换成目标产品；无需模型目录权限、删桶权限或公共写权限：

```json
{
  "Version": "1",
  "Statement": [{
    "Effect": "Allow",
    "Action": ["oss:GetObject", "oss:PutObject"],
    "Resource": ["acs:oss:*:*:tct12/12box/lingque/updates/*"]
  }]
}
```

同一产品的清单发布需要串行执行，避免旧构建覆盖新版本。私匣脚本会拒绝降级清单及覆盖同版本不同产物。上传失败后，使用原来的产物重试发布；不要重跑全部构建后强制覆盖同版本文件。撤回清单只能停止推荐问题版本，已升级用户需更高版本号的修复包，不能依赖自动降级。

私匣发布工作流的 `reuse_build_run` 支持复用已经全部通过的构建：必须与发布标签指向同一提交、属于同一发布工作流，且产物仍在保留期内。这样验收与实际发布使用同一批文件。其他产品实现类似功能时也必须做这些校验，不能仅按产物文件名下载“最近一次”构建。

## 5. 灵雀接入清单

只在 `edition/lingque` 的独立产品链路中开发，保留 `upstream/main` 的原始更新源和发布行为。

| 位置（相对 `E:/MyProject/nuphus`） | 当前状态 | 开发动作 |
| --- | --- | --- |
| `src-tauri/tauri.workbench.conf.json` | endpoint 为空、公钥为空、`createUpdaterArtifacts: false` | 配置灵雀 OSS 地址、独立公钥和更新产物，使用 Workbench 的有效版本 |
| `frontend/src/workbench/WorkbenchSettings.tsx`、`WorkbenchApp.tsx` | 独立工作台，没有复用上游完整更新页面 | 在设置内提供版本与更新入口；全局状态不依赖画布是否打开或登录成功 |
| `frontend/src/main-window/pages/UpdatePage.tsx` | 原版页面有下载和安装逻辑 | 参考官方插件调用、更新说明及错误分类；仅改这个页面不会自动接到 Workbench |
| `src-tauri/src/main.rs` | 已注册 updater 插件 | 增加灵雀独立状态、检查和安装入口，保留上游行为 |
| `src-tauri/src/workbench.rs`、`crates/nuphus-workbench/src/service.rs`、`schedules.rs` | 管理独立工作流服务与定时执行 | 保存画布后，原子阻止新工作流/定时触发/外部调用进入；已有运行或暂停中执行未结束时拒绝安装，失败后恢复调度 |
| `src-tauri/installer/workbench-hooks.nsh`、`workbench-stop-mcp.ps1` | 已处理 Workbench MCP 占用和升级标记 | 验证官方被动更新参数；失败路径必须退出安装器，不能停在需要点击的错误页 |
| `.github/workflows/workbench-release.yml` | 构建 Windows 和 Mac ARM 的安装产物，尚未生成独立更新清单 | 固定产品提交/标签、注入独立签名密钥、生成平台更新包，增加 OSS 上传与清单发布 |

灵雀 endpoint：`https://tct12.oss-cn-beijing.aliyuncs.com/12box/lingque/updates/stable/latest.json`，在首次正式发布前它可以尚不存在。不要回退到上游 Nuphus 或私匣清单。

具体注意事项：

- 当前 Workbench 配置版本为 `0.1.0`，原版配置为 `0.2.23`，前端 package 另有版本；更新判断必须取灵雀构建后实际版本，不能直接照搬私匣的 `env!("CARGO_PKG_VERSION")`。同步灵雀发布标签、有效 Tauri 配置、安装器与清单，并用 `getVersion()` 核对。
- 建议灵雀标签使用 `lingque-v<版本>`，避免触发原版 `.github/workflows/release.yml` 的 `v*` 标签规则。独立发布工作流检出标签对应提交，不在发布中临时取变化中的分支 HEAD。
- 沿用既有 `io.github.yuansui486.nuphusworkbench`、`nuphus-workbench` 和数据目录完成原地升级；`Lingque_...` 只是分发文件命名，不代表要迁移安装身份。
- Workbench 的更新开关必须在独立配置中打开；只改基础 `tauri.conf.json` 会被覆盖。插件已注册也不代表它有可用的公钥和 endpoint。
- 原工作流在构建 `.app` 后深签再制作 DMG。启用更新归档后，务必调整签名顺序或重新生成并签署归档，验收两份产物中的二进制一致。
- 现有 NSIS 钩子部分错误分支使用 `Abort`；被动模式可能停留在错误页，应参考私匣“非零退出码 + 退出安装器”的处理，并补回归验证。
- 更新安装不注销租户会话，不修改登录设备配额，不清理运行历史、项目数据库、计划任务和用户模型。

## 6. 验收：实际升级成功才算完成

在 CI 或隔离测试目录中准备可运行的旧版和签名新版，使用测试密钥与本机回环清单。测试端点只存在于测试程序，不给生产应用添加任意更新 URL 或跳过签名的入口。临时目录结束后清理，不覆盖开发者自己的安装。

| 场景 | 必须观察到的结果 |
| --- | --- |
| 官方更新器完整流程 | 获取正确平台包、通过签名、执行安装器/替换应用、新版实际运行并报告新版本 |
| Windows 主程序和 MCP 运行中 | 仅当前安装实例被释放，`/P /UPDATE /R` 完成，新程序自动启动，MCP 能再次握手 |
| 文件持续被占用 | 明确失败且有界退出，不能跳过关键文件或永久停在错误页；释放后能重试 |
| Mac ARM/x64 | 对应架构可运行；归档解压后签名有效、执行权限保留，与 DMG 内主程序/MCP/运行库一致 |
| 保存失败或任务运行 | 不安装、不重启；原有工作仍能继续 |
| 断网、404、超时、无匹配平台 | 不显示“已是最新版”，提示原因，可重试 |
| 错误签名、篡改缓存 | 不调用安装器，要求重新下载；不能关闭签名校验绕过 |
| 取消、后台下载、重开应用 | 状态正常恢复，完整缓存验签后可复用，没有重复下载/安装并发 |
| 中文和空格路径、普通用户 | 可安装到预期目录，不依赖管理员开发环境 |
| 升级后数据 | 旧数据标记、真实设置/历史样本保留，新版本可读取，不再次初始化覆盖 |

私匣已有对应参考：`src-tauri/src/updates.rs` 内原生 Mac 替换测试；`src-tauri/installer/tests/run.ps1` 的 NSIS 占用与安装测试；`scripts/updater-windows-smoke.mjs` 和 `src-tauri/examples/updater-install-probe.rs` 用官方更新器启动真实 NSIS 并验证新版进程自动运行。测试夹具不等同于目标用户全部机器的验收，发版还应检查最终安装包的签名、架构、资源与启动情况。

更新 UI 的浏览器模拟测试只能证明交互，不能代替原生安装测试。CI 不下载生产模型、不从 OSS 下载完整安装包；使用已构建产物和离线小模型。发布后只请求小型清单及 HEAD 元信息。

## 7. 常见问题

| 现象 | 检查方向 |
| --- | --- |
| 发现新版但下载 404 | 清单是否提前发布，URL 文件名/版本/平台是否与实际对象一致 |
| 下载完成后验签失败 | 公钥与签名私钥是否配套，签名是否对应同一文件，签名后是否重打包 |
| Mac 旧文件仍存在或应用不能启动 | 是否使用 `.app.tar.gz`，归档结构/权限/动态库是否正确，最终包系统签名是否有效 |
| Windows 安装卡住 | MCP 自动重启竞争、文件权限、被动模式错误页、旧卸载器处理；保留具体退出码 |
| 安装后仍是旧版 | 实际安装目录、启动快捷方式和当前进程路径是否一致；新版版本号是否正确嵌入 |
| 灵雀仍访问 Nuphus | Workbench 配置是否生效、构建 feature/参数是否正确、是否误用原版发布工作流 |
| 更新后设置消失 | 是否修改应用标识/数据目录、执行清数据卸载或错误的数据库初始化 |

对用户显示简明中文建议，日志保留受限长度的原始诊断。不要记录 AccessKey、签名私钥、登录令牌或用户文档。首次启用更新器的旧版本一般需要一次手动安装；此后才具备应用内升级入口。
