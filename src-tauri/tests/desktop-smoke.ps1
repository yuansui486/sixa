# Native Windows smoke test. Requires a debug build with embedded UI:
# cargo build -p sixa --features custom-protocol
# No login, model downloads, production profile or installed application changes.
param([string]$Executable = "$PSScriptRoot\..\..\target\debug\sixa.exe")
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public class SixaDesktopTest {
  public delegate bool Callback(IntPtr h, IntPtr p);
  [DllImport("user32.dll")] public static extern bool EnumWindows(Callback c, IntPtr p);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint p);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder c, int n);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowEx(IntPtr p, IntPtr after, string cls, string title);
  [StructLayout(LayoutKind.Sequential)] public struct IconId { public uint cb; public IntPtr hwnd; public uint id; public Guid guid; }
  [StructLayout(LayoutKind.Sequential)] public struct Rect { public int l,t,r,b; }
  [DllImport("shell32.dll")] public static extern int Shell_NotifyIconGetRect(ref IconId id, out Rect rect);
  public static bool TrayVisible(IntPtr hwnd) { for (uint i=0;i<8;i++) { var id=new IconId { cb=(uint)Marshal.SizeOf(typeof(IconId)),hwnd=hwnd,id=i }; Rect r; if(Shell_NotifyIconGetRect(ref id,out r)==0) return true; } return false; }
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetMenuString(IntPtr menu, uint item, StringBuilder text, int size, uint flags);
  [DllImport("user32.dll")] public static extern uint GetMenuItemID(IntPtr menu, int position);
  public static uint MenuCommand(IntPtr window, int position) { return GetMenuItemID(SendMessage(window, 0x1e1, IntPtr.Zero, IntPtr.Zero), position); }
  public static string MenuText(IntPtr window, uint position) {
    var menu = SendMessage(window, 0x1e1, IntPtr.Zero, IntPtr.Zero);
    var text = new StringBuilder(256); GetMenuString(menu, position, text, 256, 0x400); return text.ToString();
  }
  public static IntPtr Find(uint process, string name) {
    IntPtr result = IntPtr.Zero;
    EnumWindows((h,p) => { uint id; GetWindowThreadProcessId(h, out id); var c = new StringBuilder(256); GetClassName(h,c,256);
      if (id == process && c.ToString() == name) { result=h; return false; } return true; }, IntPtr.Zero);
    return result;
  }
}
'@
function Wait-For([scriptblock]$Condition, [string]$Failure) {
    $until = [DateTime]::UtcNow.AddSeconds(12)
    do { if (& $Condition) { return }; Start-Sleep -Milliseconds 100 } while ([DateTime]::UtcNow -lt $until)
    throw $Failure
}
function Find-Element([string]$Name) {
    $element = [System.Windows.Automation.AutomationElement]::FromHandle($script:window)
    $condition = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NameProperty, $Name)
    $element.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
}
function Click([string]$Name) {
    Wait-For { $null -ne (Find-Element $Name) } "Missing control: $Name"
    $container = [System.Windows.Automation.AutomationElement]::FromHandle($script:window)
    $condition = New-Object System.Windows.Automation.AndCondition(
        (New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NameProperty, $Name)),
        (New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Button)))
    $element = $container.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
    if (-not $element) {
        # Windows TaskDialog command links appear as Pane controls to UIA.
        $nativeControl = (Find-Element $Name).Current.NativeWindowHandle
        if ($nativeControl -eq 0) { throw "No native button for: $Name" }
        [void][SixaDesktopTest]::PostMessage([IntPtr]$nativeControl, 245, [IntPtr]::Zero, [IntPtr]::Zero)
        return
    }
    $pattern = $element.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern)
    $pattern.Invoke()
}
function Close-Window {
    [void][SixaDesktopTest]::PostMessage($script:window, 16, [IntPtr]::Zero, [IntPtr]::Zero)
}
function Tray-Left {
    Start-Sleep -Milliseconds 200
    if (-not [SixaDesktopTest]::TrayVisible($script:tray)) {
        # A synthetic click has no shell rectangle while the icon is in the
        # collapsed overflow. Open it as a user would before clicking the icon.
        $taskbar = [SixaDesktopTest]::FindWindowEx([IntPtr]::Zero,[IntPtr]::Zero,'Shell_TrayWnd',$null)
        $area = [SixaDesktopTest]::FindWindowEx($taskbar,[IntPtr]::Zero,'TrayNotifyWnd',$null)
        $chevron = [SixaDesktopTest]::FindWindowEx($area,[IntPtr]::Zero,'Button',$null)
        [void][SixaDesktopTest]::PostMessage($chevron,245,[IntPtr]::Zero,[IntPtr]::Zero)
        Wait-For { [SixaDesktopTest]::TrayVisible($script:tray) } 'Tray icon unavailable in notification area'
    }
    [void][SixaDesktopTest]::PostMessage($script:tray, 6002, [IntPtr]::Zero, [IntPtr]514)
    Wait-For { [SixaDesktopTest]::IsWindowVisible($script:window) } 'Tray click did not restore the window'
}
$Executable = (Resolve-Path -LiteralPath $Executable).Path
$root = Join-Path ([IO.Path]::GetTempPath()) ('sixa-desktop-test-' + [Guid]::NewGuid().ToString('N'))
$originalRoot = $env:SIXA_DESKTOP_TEST_ROOT
$originalNoUi = $env:SIXA_DESKTOP_TEST_NO_UI
$process = $null
try {
    New-Item -ItemType Directory -Path $root | Out-Null
    $env:SIXA_DESKTOP_TEST_ROOT = $root
    $process = Start-Process -FilePath $Executable -PassThru -WindowStyle Hidden -RedirectStandardError (Join-Path $root 'native.log')
    Wait-For { $process.Refresh(); $process.MainWindowHandle -ne [IntPtr]::Zero } 'No main window'
    $script:window = $process.MainWindowHandle
    [void][SixaDesktopTest]::ShowWindow($script:window, 5)
    Wait-For { $script:tray = [SixaDesktopTest]::Find($process.Id, 'tray_icon_app'); $script:tray -ne [IntPtr]::Zero } 'Native tray window missing'
    Close-Window
    Wait-For { $null -ne (Find-Element '直接退出') } 'Title-bar close did not open a choice before login'
    # Repeated native closes must coalesce.
    Close-Window
    Start-Sleep -Milliseconds 200
    Click '取消退出'
    Wait-For { $null -eq (Find-Element '直接退出') } 'Cancel left a stale close dialog'
    Write-Host 'PASS native close prompt and cancellation before login'
    Close-Window
    Wait-For { $null -ne (Find-Element '不再提醒，可在应用设置中修改') } 'Remember checkbox missing'
    $checkbox = Find-Element '不再提醒，可在应用设置中修改'
    $checkbox.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern).Toggle()
    Click '最小化到托盘'
    Wait-For { -not [SixaDesktopTest]::IsWindowVisible($script:window) } 'Tray choice did not hide window'
    if ($process.HasExited) { throw 'Minimize unexpectedly terminated process' }
    Tray-Left
    Close-Window
    Wait-For { -not [SixaDesktopTest]::IsWindowVisible($script:window) } 'Remembered tray preference was not applied'
    Write-Host 'PASS minimize, tray left-click restore and remembered choice'
    $second = Start-Process -FilePath $Executable -PassThru -WindowStyle Hidden
    if (-not $second.WaitForExit(10000)) { throw 'Second instance did not exit' }
    Wait-For { [SixaDesktopTest]::IsWindowVisible($script:window) } 'Second launch did not restore window'
    Write-Host 'PASS single-instance restore'
    [void][SixaDesktopTest]::PostMessage($script:tray, 6002, [IntPtr]::Zero, [IntPtr]517)
    $menu = $null
    Wait-For { $script:menu = [SixaDesktopTest]::Find($process.Id, '#32768'); $script:menu -ne [IntPtr]::Zero } 'Right click did not show native menu'
    $names = @(0..3 | ForEach-Object { [SixaDesktopTest]::MenuText($script:menu, $_) })
    Write-Host ('Native menu entries: ' + ($names -join ' | '))
    foreach ($name in @('打开私匣','应用设置','退出私匣')) { if ($names -notcontains $name) { throw "Missing native tray menu item: $name" } }
    # Route the real menu command through the native owner window.
    $exitCommand = [SixaDesktopTest]::MenuCommand($script:menu, 3)
    [void][SixaDesktopTest]::PostMessage($script:tray, 31, [IntPtr]::Zero, [IntPtr]::Zero)
    [void][SixaDesktopTest]::PostMessage($script:tray, 273, [IntPtr]$exitCommand, [IntPtr]::Zero)
    if (-not $process.WaitForExit(15000)) {
        $debugWindow = [System.Windows.Automation.AutomationElement]::FromHandle($script:window)
        $debugWindow.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition) | ForEach-Object { Write-Host $_.Current.Name }
        throw 'Tray exit did not terminate process despite saved tray preference'
    }
    Write-Host 'PASS native tray menu and explicit process exit'
    # Same SQLite profile must retain the choice across application restarts.
    $process = Start-Process -FilePath $Executable -PassThru -WindowStyle Hidden
    Wait-For { $process.Refresh(); $process.MainWindowHandle -ne [IntPtr]::Zero } 'No restarted window'
    $script:window = $process.MainWindowHandle
    [void][SixaDesktopTest]::ShowWindow($script:window, 5)
    Close-Window
    Wait-For { -not [SixaDesktopTest]::IsWindowVisible($script:window) } 'Close preference did not survive restart'
    Write-Host 'PASS preference persistence across restart'
    $process.Kill(); $process.WaitForExit()
    $env:SIXA_DESKTOP_TEST_ROOT = Join-Path $root 'no-ui'
    $env:SIXA_DESKTOP_TEST_NO_UI = '1'
    $process = Start-Process -FilePath $Executable -PassThru -WindowStyle Hidden
    Wait-For { $process.Refresh(); $process.MainWindowHandle -ne [IntPtr]::Zero } 'No fallback test window'
    $script:window = $process.MainWindowHandle
    [void][SixaDesktopTest]::ShowWindow($script:window, 5)
    Close-Window
    Wait-For { $script:nativeDialog = [SixaDesktopTest]::Find($process.Id, '#32770'); $script:nativeDialog -ne [IntPtr]::Zero } 'Unresponsive UI did not show native fallback'
    $script:window = $script:nativeDialog
    Click '直接退出'
    Wait-For { $script:window = [SixaDesktopTest]::Find($process.Id, '#32770'); $script:window -ne [IntPtr]::Zero -and $null -ne (Find-Element '强制退出') } 'Fallback did not warn about unsaved modifications'
    Click '强制退出'
    if (-not $process.WaitForExit(10000)) { throw 'Native fallback did not exit' }
    Write-Host 'PASS native fallback without any frontend and explicit force exit'
} finally {
    if ($process -and -not $process.HasExited) { $process.Kill(); $process.WaitForExit() }
    Get-Content (Join-Path $root 'native.log')
    $env:SIXA_DESKTOP_TEST_ROOT = $originalRoot
    $env:SIXA_DESKTOP_TEST_NO_UI = $originalNoUi
    Write-Host "Isolated test data: $root"
}
