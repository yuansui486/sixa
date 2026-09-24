; Included by Tauri's NSIS template for both the installer and uninstaller.
!ifndef SIXA_INSTALLER_HOOKS
!define SIXA_INSTALLER_HOOKS
!include FileFunc.nsh
!define SIXA_MCP_STOP_SCRIPT "${__FILEDIR__}\stop-mcp.ps1"

!macro SixaDefineMcpCleanup PREFIX
Function ${PREFIX}SixaStopMcp
  Push $0
  Push $1
  InitPluginsDir
  File /oname=$PLUGINSDIR\sixa-stop-mcp.ps1 "${SIXA_MCP_STOP_SCRIPT}"
  DetailPrint "正在关闭私匣 MCP 连接…"
  sixa_mcp_retry:
    ; Do not interpolate the directory into PowerShell source. The trailing dot
    ; also prevents a trailing backslash from escaping the argument's closing quote.
    nsExec::ExecToStack /TIMEOUT=15000 '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PLUGINSDIR\sixa-stop-mcp.ps1" -InstallDir "$INSTDIR\."'
    Pop $0
    Pop $1
    ${If} $0 == 0
      DetailPrint "私匣 MCP 文件已释放。升级后可在 AI 客户端中重新连接。"
      Goto sixa_mcp_done
    ${EndIf}
    DetailPrint "私匣 MCP 文件仍被占用或无法访问（检查结果：$0）。"
    IfSilent sixa_mcp_abort 0
    ; Tauri includes this file before declaring its PassiveMode variable.
    ; Read /P directly so this function works in both generated contexts.
    ClearErrors
    ${GetOptions} $CMDLINE "/P" $1
    ${IfNot} ${Errors}
      Goto sixa_mcp_abort
    ${EndIf}
    MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "无法释放私匣 MCP 程序文件。请在 AI 客户端中暂时停用私匣 MCP 连接后点击“重试”。若仍然失败，请检查安装目录访问权限或安全软件拦截。$\r$\n$\r$\n本次操作未完成；不会跳过该文件。" IDRETRY sixa_mcp_retry
  sixa_mcp_abort:
    SetErrorLevel 10
    Abort "私匣 MCP 程序文件未释放，安装或卸载已停止。"
  sixa_mcp_done:
    ClearErrors
    Pop $1
    Pop $0
FunctionEnd
!macroend

!insertmacro SixaDefineMcpCleanup ""
!insertmacro SixaDefineMcpCleanup "un."

!macro NSIS_HOOK_PREINSTALL
  ; Stop the desktop first so its connection self-test cannot start another MCP.
  ; Tauri repeats this check afterwards; no process remains in the normal case.
  !insertmacro CheckIfAppIsRunning "${MAINBINARYNAME}.exe" "${PRODUCTNAME}"
  Call SixaStopMcp
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro CheckIfAppIsRunning "${MAINBINARYNAME}.exe" "${PRODUCTNAME}"
  Call un.SixaStopMcp
!macroend
!endif
