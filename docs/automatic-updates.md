# 私匣应用更新与发布

从 1.0.9 开始使用 Tauri v2 更新器。Windows x64 使用 NSIS；Mac Intel、Apple Silicon 使用各自的 `.app.tar.gz` 更新包。DMG 仍供首次安装使用，应用内更新不需要用户解压或拖拽。

## 用户操作

启动后延迟 5 秒检查，默认每天自动检查一次；应用设置可关闭自动检查。登录页面和设置中均可手动检查，不依赖授权或模型准备成功。自动检查只读取 JSON，不下载更新包。

发现新版本后点击“下载更新”；下载显示大小、速度和剩余时间，可取消或放到后台。完整包缓存到用户数据目录的 `cache/app-updates/`；重启应用后重新核对线上版本和签名，可复用完整缓存。未完成的下载取消后重新下载，不与模型下载的断点文件混用。网络不可用时保留缓存，不自动安装。

点击“重启并安装”后先保存复核修改；仍有处理、导出或模型准备时不会安装。请等待结束后再次点击。安装过程中暂时阻止新的 MCP 调用，完成后可在 AI 客户端中重新连接。MCP 配置路径不变。Windows 安装器只清理当前用户、当前安装目录的 MCP 进程。

应用更新不清除模型、历史、规则、设置或登录数据。更新包签名校验是安装门禁，与模型启动时仅检查文件存在的策略不同。

## OSS 对象

桶：`tct12`；地域：`oss-cn-beijing`。模型继续使用 `12box/sixa/models/`，应用更新使用独立目录：

```text
12box/sixa/updates/
  stable/latest.json
  releases/1.0.9/
    latest.json
    SHA256SUMS.txt
    Sixa_1.0.9_windows_x64_setup.exe
    Sixa_1.0.9_windows_x64_setup.exe.sig
    Sixa_1.0.9_macos_arm64.app.tar.gz
    Sixa_1.0.9_macos_arm64.app.tar.gz.sig
    Sixa_1.0.9_macos_x64.app.tar.gz
    Sixa_1.0.9_macos_x64.app.tar.gz.sig
    Sixa_1.0.9_macos_arm64.dmg
    Sixa_1.0.9_macos_x64.dmg
```

固定检查地址：<https://tct12.oss-cn-beijing.aliyuncs.com/12box/sixa/updates/stable/latest.json>。

`latest.json` 由脚本生成，包含 `version`、`notes`、RFC 3339 格式的 `pub_date` 和三个平台的 `url`、`signature`。平台键为 `windows-x86_64`、`darwin-aarch64`、`darwin-x86_64`；`signature` 是 `.sig` 的完整内容，不能填文件路径或 SHA-256。客户端仅接受指定 OSS 版本目录的 HTTPS 下载地址，不跟随下载重定向。

版本目录不可覆盖，缓存策略为一年 immutable；稳定清单使用 `no-cache, max-age=0, must-revalidate`。OSS 保持公共读取，禁止公共写入。应用不需要 AccessKey 或 OSS CORS 配置，网络请求由 Rust 发起。

## 一次性配置

GitHub 仓库：<https://github.com/yuansui486/sixa/settings/secrets/actions>。

| Secret | 内容 |
| --- | --- |
| `TAURI_SIGNING_PRIVATE_KEY` | Tauri 更新签名私钥的文件内容；本次已配置 |
| `OSS_ACCESS_KEY_ID` | 专用 RAM 发布账号的 AccessKey ID |
| `OSS_ACCESS_KEY_SECRET` | 对应 Secret；不要提交到代码或聊天 |

本次生成的签名密钥保存在发布机器 `%LOCALAPPDATA%\SixaRelease\updater.key`，目录访问权限已限制到当前账号和 SYSTEM。请通过安全方式备份私钥；不要重新生成并替换它，否则旧客户端无法验证之后的更新。公钥已写入 `src-tauri/tauri.conf.json`，可以公开。当前私钥未设密码，CI 明确传入空密码，安全性依赖本机 ACL 和 GitHub Secrets。如果以后给同一密钥增加密码，应同时配置构建步骤的密码 Secret。

RAM 最小权限示例，不包含模型目录和删除权限：

```json
{
  "Version": "1",
  "Statement": [{
    "Effect": "Allow",
    "Action": ["oss:GetObject", "oss:PutObject"],
    "Resource": ["acs:oss:*:*:tct12/12box/sixa/updates/*"]
  }]
}
```

Tauri 更新签名证明更新包来源；它不能替代 Apple Developer ID、公证或 Windows Authenticode。本项目仍沿用现有 Mac 完整性签名。

## 发布新版本

1. 同步修改 Cargo workspace、Tauri 配置、前端 package 和 lock 的版本号；执行 `cargo check` 更新 workspace 包的 Cargo.lock 版本。
2. 新建 `docs/releases/<版本>.md`，用中文写明用户可见改动。使用稳定三段版本号，例如 `1.0.10`。
3. 提交经过测试的代码，并在该提交上创建、推送 `v<版本>` 标签。手动运行工作流时，勾选发布并填写已有标签；工作流会检出该标签，不使用随意选择的分支代替。
4. Windows、Mac Intel、Mac ARM 构建、签名、验收均成功后，生成固定文件名及更新清单，发布 GitHub Release，上传 OSS 版本目录，最后切换稳定清单。源码、标签和清单版本不一致时拒绝发布。

发布脚本位于 `tools/release/`：`check-version.mjs` 核对版本，`manifest.mjs` 验证 Tauri 签名并整理产物，`publish.mjs` 上传 OSS。依赖通过该目录的独立 lockfile 安装，不进入客户安装包。

GitHub Actions 的 OSS 发布阶段跨标签串行执行。已上传文件通过 SHA-256 元信息和大小核对，同一版本不同内容禁止覆盖；因此重试上传应复用同一次构建产物。若重新编译导致产物变化，请增加版本号。不要删除旧发布来绕过检查。

仅构建时无需 OSS 凭据，也不会发布 Release 或更新 OSS；签名构建仍需仓库的签名 Secret。正式发布缺少 OSS 凭据会提前失败。CI 使用离线签名样本和本地测试，不下载生产模型；上传后仅核对对象元信息和公开的小型 JSON，不从 OSS 下载整包作测试。

## 异常处理与首次上线

- 1.0.8 及以前没有更新器，需要手动安装一次 1.0.9。之后才能应用内升级。
- 网络、校验或缓存异常时，界面提供重试；不会安装未经验证的文件。完整缓存每次重新使用及安装前均重新验签。
- Mac 从 DMG 或 App Translocation 位置运行时，先退出并将应用拖入“应用程序”；正常安装目录需要管理员权限时由系统请求授权。
- 安装时的 MCP 阻止标记保存在数据目录 `cache/app-update-lease.json`。新版正常启动后解除；安装失败由原进程解除，进程意外结束时最多 10 分钟自动过期，避免永久阻断。不要在安装进行中删除此文件。
- 若某版本发现问题，撤回稳定清单到上一份已验证清单可停止向旧客户端推荐问题版本，但已经升级的客户端不会自动降级。为这些用户发布更高版本号的修复版本。
- 首次发布前固定检查地址可能返回 404，应用不会因此阻止登录或文件操作。只有 CI 发布成功且公开清单校验通过后，线上更新才正式可用。
