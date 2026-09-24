; A registry-free install/uninstall harness for the production hooks.
; Tauri's existing main-process guard is outside this harness's scope.
Unicode true
RequestExecutionLevel user
!include LogicLib.nsh
Name "Sixa installer regression"
OutFile "${OUTPUT}"
!define MAINBINARYNAME "sixa-installer-regression"
!define PRODUCTNAME "Sixa installer regression"
!define INSTALLMODE "currentUser"
!ifdef TAURI_NSIS_DIR
  ; Exercise the exact main-process guard and plugin used by the built installer.
  LoadLanguageFile "${NSISDIR}\Contrib\Language files\SimpChinese.nlf"
  !include "${TAURI_NSIS_DIR}\SimpChinese.nsh"
  !include "${TAURI_NSIS_DIR}\utils.nsh"
  !addplugindir "${TAURI_PLUGIN_DIR}"
!else
  !macro CheckIfAppIsRunning NAME PRODUCT
    DetailPrint "Main-process guard is provided by Tauri in production."
  !macroend
!endif
!include "..\hooks.nsh"
; Mirror Tauri's ordering: template variables are declared AFTER installerHooks.
Var PassiveMode
Function .onInit
  StrCpy $PassiveMode 0
FunctionEnd
Function un.onInit
  StrCpy $PassiveMode 0
FunctionEnd
Section Install
  !insertmacro NSIS_HOOK_PREINSTALL
  SetOutPath "$INSTDIR"
  File /oname=sixa-mcp.exe "${MCP_BINARY}"
  File /oname=${MAINBINARYNAME}.exe "${MCP_BINARY}"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  FileOpen $0 "$INSTDIR\installed.txt" w
  FileWrite $0 "installed"
  FileClose $0
SectionEnd
Section Uninstall
  !insertmacro NSIS_HOOK_PREUNINSTALL
  Delete "$INSTDIR\sixa-mcp.exe"
  Delete "$INSTDIR\${MAINBINARYNAME}.exe"
  Delete "$INSTDIR\installed.txt"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
SectionEnd
