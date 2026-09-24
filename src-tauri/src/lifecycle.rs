//! Native window ownership: a close request never destroys the WebView implicitly.
use crate::{AppState, poisoned};
use domain::{CloseBehavior, DesktopPreferences, Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};
use tauri::{
    Emitter, Manager,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogResult};
use uuid::Uuid;

pub struct Lifecycle {
    inner: Mutex<Inner>,
    store: Mutex<storage::Store>,
}
struct Inner {
    preferences: DesktopPreferences,
    tray_available: bool,
    pending: Option<Pending>,
    exiting: bool,
}
struct Pending {
    request: CloseRequest,
    remember: bool,
    acknowledged: bool,
    native: bool,
    changed: Instant,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Choice,
    Saving,
    Confirm,
    Stopping,
    Slow,
}
#[derive(Clone, Serialize)]
pub struct CloseRequest {
    id: Uuid,
    phase: Phase,
    tray_available: bool,
    active_tasks: usize,
    active_downloads: usize,
}
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Tray,
    Exit,
    Saved,
    Stop,
    Cancel,
    Force,
    Wait,
}
#[derive(Serialize)]
pub struct PreferencesDto {
    close_behavior: CloseBehavior,
    tray_available: bool,
}
impl Lifecycle {
    pub fn new(store: storage::Store) -> Result<Self> {
        let preferences = store.desktop_preferences()?;
        Ok(Self {
            inner: Mutex::new(Inner {
                preferences,
                tray_available: false,
                pending: None,
                exiting: false,
            }),
            store: Mutex::new(store),
        })
    }
    pub fn exiting(&self) -> bool {
        self.inner.lock().is_ok_and(|inner| inner.exiting)
    }
}

pub fn restore(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
pub fn install_tray(app: &tauri::AppHandle) {
    let result = (|| -> tauri::Result<()> {
        let open = MenuItem::with_id(app, "open", "打开私匣", true, None::<&str>)?;
        let settings = MenuItem::with_id(app, "settings", "应用设置", true, None::<&str>)?;
        let separator = PredefinedMenuItem::separator(app)?;
        let exit = MenuItem::with_id(app, "exit", "退出私匣", true, None::<&str>)?;
        let menu = Menu::with_items(app, &[&open, &settings, &separator, &exit])?;
        let mut builder = TrayIconBuilder::with_id("sixa")
            .tooltip("私匣 · 本机数据脱敏")
            .menu(&menu)
            .show_menu_on_left_click(cfg!(target_os = "macos"))
            .on_menu_event(|app, event| match event.id.as_ref() {
                "open" => restore(app),
                "settings" => {
                    restore(app);
                    let _ = app.emit("app-open-settings", ());
                }
                "exit" => request_close(app, true),
                _ => {}
            });
        if let Some(icon) = app.default_window_icon() {
            builder = builder.icon(icon.clone());
        }
        #[cfg(windows)]
        {
            builder = builder.on_tray_icon_event(|tray, event| {
                if matches!(
                    event,
                    tauri::tray::TrayIconEvent::Click {
                        button: tauri::tray::MouseButton::Left,
                        button_state: tauri::tray::MouseButtonState::Up,
                        ..
                    }
                ) {
                    restore(tray.app_handle());
                }
            });
        }
        builder.build(app)?;
        Ok(())
    })();
    if let Ok(mut inner) = app.state::<Lifecycle>().inner.lock() {
        inner.tray_available = result.is_ok();
    }
}

#[tauri::command]
pub fn get_desktop_preferences(state: tauri::State<'_, Lifecycle>) -> Result<PreferencesDto> {
    let inner = state.inner.lock().map_err(poisoned)?;
    Ok(PreferencesDto {
        close_behavior: inner.preferences.close_behavior,
        tray_available: inner.tray_available,
    })
}
#[tauri::command]
pub async fn set_desktop_preferences(
    app: tauri::AppHandle,
    close_behavior: CloseBehavior,
) -> Result<PreferencesDto> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<Lifecycle>();
        let mut inner = state.inner.lock().map_err(poisoned)?;
        if close_behavior == CloseBehavior::Tray && !inner.tray_available {
            return Err(Error::State("托盘不可用，请选择每次询问或直接退出".into()));
        }
        let preferences = DesktopPreferences { close_behavior };
        state
            .store
            .lock()
            .map_err(poisoned)?
            .save_desktop_preferences(&preferences)?;
        inner.preferences = preferences;
        Ok(PreferencesDto {
            close_behavior,
            tray_available: inner.tray_available,
        })
    })
    .await
    .map_err(|e| Error::Io(e.to_string()))?
}
#[tauri::command]
pub fn get_close_request(state: tauri::State<'_, Lifecycle>) -> Result<Option<CloseRequest>> {
    let mut inner = state.inner.lock().map_err(poisoned)?;
    Ok(inner.pending.as_mut().map(|pending| {
        pending.acknowledged = true;
        pending.request.clone()
    }))
}
#[tauri::command]
pub fn acknowledge_app_close(state: tauri::State<'_, Lifecycle>, request_id: Uuid) -> Result<()> {
    let mut inner = state.inner.lock().map_err(poisoned)?;
    if let Some(pending) = inner
        .pending
        .as_mut()
        .filter(|p| p.request.id == request_id)
    {
        pending.acknowledged = true;
    }
    Ok(())
}

fn counts(app: &tauri::AppHandle) -> (usize, usize) {
    let state = app.state::<AppState>();
    // Activity counts include queued work, exports, model loading and finalization.
    let downloads = state.installs.lock().map(|i| i.len()).unwrap_or(0);
    let runs = state.active_ocr.lock().map(|runs| runs.len()).unwrap_or(0);
    let other_work = usize::from(downloads == 0 && state.desktop.active_count() > 0);
    (runs.max(other_work), downloads)
}
pub fn request_close(app: &tauri::AppHandle, explicit_exit: bool) {
    let state = app.state::<Lifecycle>();
    let Ok(mut inner) = state.inner.lock() else {
        return;
    };
    if inner.exiting {
        return;
    }
    if let Some(pending) = &mut inner.pending {
        // Another native close also probes a WebView that stopped responding
        // after acknowledging the original request.
        let native = pending.native;
        if !native {
            pending.acknowledged = false;
            pending.changed = Instant::now();
        }
        drop(inner);
        restore(app);
        if !native {
            publish(app);
        }
        return;
    }
    let behavior = if explicit_exit {
        CloseBehavior::Exit
    } else {
        inner.preferences.close_behavior
    };
    if behavior == CloseBehavior::Tray && inner.tray_available {
        if let Some(window) = app.get_webview_window("main")
            && window.hide().is_ok()
        {
            return;
        }
        inner.tray_available = false;
    }
    let (active_tasks, active_downloads) = counts(app);
    inner.pending = Some(Pending {
        request: CloseRequest {
            id: Uuid::new_v4(),
            phase: if behavior == CloseBehavior::Exit {
                Phase::Saving
            } else {
                Phase::Choice
            },
            tray_available: inner.tray_available,
            active_tasks,
            active_downloads,
        },
        remember: false,
        acknowledged: false,
        native: false,
        changed: Instant::now(),
    });
    drop(inner);
    restore(app);
    publish(app);
}
fn publish(app: &tauri::AppHandle) {
    let state = app.state::<Lifecycle>();
    let Ok(inner) = state.inner.lock() else {
        return;
    };
    let Some(pending) = &inner.pending else {
        return;
    };
    let request = pending.request.clone();
    let changed = pending.changed;
    let native = pending.native;
    drop(inner);
    let _ = app.emit("app-close-request", &request);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if !native {
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
        let fallback = {
            let state = app.state::<Lifecycle>();
            let Ok(mut inner) = state.inner.lock() else {
                return;
            };
            inner.pending.as_mut().is_some_and(|p| {
                if p.request.id == request.id && p.changed == changed && (!p.acknowledged || native)
                {
                    p.native = true;
                    true
                } else {
                    false
                }
            })
        };
        if fallback {
            native_prompt(&app, &request);
        }
    });
}
fn native_prompt(app: &tauri::AppHandle, request: &CloseRequest) {
    let (text, first, second) = match request.phase {
        Phase::Choice if request.tray_available => (
            "界面暂未响应。可以保留后台运行，或退出私匣。",
            "最小化到托盘",
            "直接退出",
        ),
        Phase::Choice => ("界面暂未响应，是否退出私匣？", "直接退出", "取消"),
        Phase::Confirm => (
            "仍有任务或模型下载。退出将停止这些操作，已生成的文件会保留。",
            "停止并退出",
            "取消",
        ),
        _ => (
            "界面暂未响应，无法确认复核修改是否已保存。强制退出可能丢失修改或中断文件写入。",
            "继续等待",
            "强制退出",
        ),
    };
    let id = request.id;
    let phase = request.phase;
    let tray_available = request.tray_available;
    let app_handle = app.clone();
    let mut dialog = app.dialog().message(text).title("关闭私匣").buttons(
        MessageDialogButtons::YesNoCancelCustom(first.into(), second.into(), "取消退出".into()),
    );
    if let Some(window) = app.get_webview_window("main") {
        dialog = dialog.parent(&window);
    }
    dialog.show_with_result(move |result| {
        let first_chosen = result == MessageDialogResult::Yes
            || result == MessageDialogResult::Custom(first.into());
        let second_chosen = result == MessageDialogResult::No
            || result == MessageDialogResult::Custom(second.into());
        let action = if first_chosen {
            match phase {
                Phase::Choice if tray_available => Action::Tray,
                Phase::Choice => Action::Exit,
                Phase::Confirm => Action::Stop,
                _ => Action::Wait,
            }
        } else if second_chosen {
            match phase {
                Phase::Choice if tray_available => Action::Exit,
                Phase::Choice | Phase::Confirm => Action::Cancel,
                _ => Action::Force,
            }
        } else {
            Action::Cancel
        };
        tauri::async_runtime::spawn(async move {
            let _ = respond_app_close(app_handle, id, action, None).await;
        });
    });
}

#[tauri::command]
pub async fn respond_app_close(
    app: tauri::AppHandle,
    request_id: Uuid,
    action: Action,
    remember: Option<bool>,
) -> Result<Option<CloseRequest>> {
    let app_clone = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        respond(&app_clone, request_id, action, remember.unwrap_or(false))
    })
    .await
    .map_err(|e| Error::Io(e.to_string()))?
}
fn respond(
    app: &tauri::AppHandle,
    id: Uuid,
    action: Action,
    remember: bool,
) -> Result<Option<CloseRequest>> {
    let state = app.state::<Lifecycle>();
    let mut inner = state.inner.lock().map_err(poisoned)?;
    let pending = inner
        .pending
        .as_mut()
        .filter(|p| p.request.id == id)
        .ok_or_else(|| Error::State("关闭请求已结束".into()))?;
    let phase = pending.request.phase;
    match action {
        Action::Cancel => {
            inner.pending = None;
            app.state::<AppState>().scheduler.resume();
            app.state::<AppState>().desktop.resume();
            let _ = app.emit("app-close-request", Option::<CloseRequest>::None);
            return Ok(None);
        }
        Action::Tray if phase == Phase::Choice => {
            if !pending.request.tray_available {
                return Err(Error::State("托盘不可用".into()));
            }
            // Window calls can wait for the main thread. Never hold the lifecycle
            // mutex while that thread may be handling another close request.
            drop(inner);
            let window = app
                .get_webview_window("main")
                .ok_or_else(|| Error::State("窗口不存在".into()))?;
            window
                .hide()
                .map_err(|error| Error::Io(error.to_string()))?;
            let mut inner = state.inner.lock().map_err(poisoned)?;
            if !inner.pending.as_ref().is_some_and(|p| p.request.id == id) {
                drop(inner);
                restore(app);
                return Ok(None);
            }
            if remember {
                if let Err(error) = state
                    .store
                    .lock()
                    .map_err(poisoned)?
                    .save_desktop_preferences(&DesktopPreferences {
                        close_behavior: CloseBehavior::Tray,
                    })
                {
                    drop(inner);
                    restore(app);
                    return Err(error);
                }
                inner.preferences.close_behavior = CloseBehavior::Tray;
            }
            inner.pending = None;
            let _ = app.emit("app-close-request", Option::<CloseRequest>::None);
            return Ok(None);
        }
        Action::Exit if phase == Phase::Choice => {
            pending.remember = remember;
            pending.request.phase = Phase::Saving;
        }
        Action::Saved if phase == Phase::Saving => {
            let (tasks, downloads) = counts(app);
            pending.request.active_tasks = tasks;
            pending.request.active_downloads = downloads;
            if tasks + downloads > 0 {
                pending.request.phase = Phase::Confirm;
            } else {
                // Freeze admissions before checking idle again: a concurrent MCP job
                // must either be counted and confirmed, or rejected.
                let work = app.state::<AppState>();
                if !work.desktop.freeze_if_idle() {
                    let (tasks, downloads) = counts(app);
                    pending.request.active_tasks = tasks;
                    pending.request.active_downloads = downloads;
                    pending.request.phase = Phase::Confirm;
                } else {
                    finish_exit(app, &state, &mut inner, true)?;
                    return Ok(None);
                }
            }
        }
        Action::Stop if phase == Phase::Confirm => {
            app.state::<AppState>().desktop.begin_shutdown();
            app.state::<AppState>().scheduler.shutdown()?;
            app.state::<AppState>().previews.cancel_all();
            pending.request.phase = Phase::Stopping;
        }
        Action::Wait if matches!(phase, Phase::Saving | Phase::Slow) => {
            if phase == Phase::Slow {
                pending.request.phase = Phase::Stopping;
            }
        }
        Action::Force
            if phase == Phase::Slow
                || (phase == Phase::Saving
                    && (pending.native || pending.changed.elapsed() >= Duration::from_secs(5))) =>
        {
            if phase == Phase::Saving && !app.state::<AppState>().desktop.freeze_if_idle() {
                let (tasks, downloads) = counts(app);
                pending.request.active_tasks = tasks;
                pending.request.active_downloads = downloads;
                pending.request.phase = Phase::Confirm;
            } else {
                finish_exit(app, &state, &mut inner, false)?;
                return Ok(None);
            }
        }
        _ => return Err(Error::State("关闭步骤已改变，请重试".into())),
    }
    let pending = inner.pending.as_mut().unwrap();
    pending.changed = Instant::now();
    pending.acknowledged = false;
    let request = pending.request.clone();
    drop(inner);
    publish(app);
    if request.phase == Phase::Stopping {
        stop_and_wait(app.clone(), id);
    }
    Ok(Some(request))
}
fn finish_exit(
    app: &tauri::AppHandle,
    state: &Lifecycle,
    inner: &mut Inner,
    persist: bool,
) -> Result<()> {
    if persist && inner.pending.as_ref().is_some_and(|p| p.remember) {
        if let Err(error) = state
            .store
            .lock()
            .map_err(poisoned)?
            .save_desktop_preferences(&DesktopPreferences {
                close_behavior: CloseBehavior::Exit,
            })
        {
            app.state::<AppState>().scheduler.resume();
            app.state::<AppState>().desktop.resume();
            return Err(error);
        }
        inner.preferences.close_behavior = CloseBehavior::Exit;
    }
    inner.exiting = true;
    app.exit(0);
    Ok(())
}
fn stop_and_wait(app: tauri::AppHandle, id: Uuid) {
    tauri::async_runtime::spawn(async move {
        let start = Instant::now();
        loop {
            let idle = {
                let lifecycle = app.state::<Lifecycle>();
                let Ok(mut inner) = lifecycle.inner.lock() else {
                    return;
                };
                if !inner
                    .pending
                    .as_ref()
                    .is_some_and(|p| p.request.id == id && p.request.phase == Phase::Stopping)
                {
                    return;
                }
                let state = app.state::<AppState>();
                if let Ok(runs) = state.active_ocr.lock() {
                    for run in runs.values() {
                        let _ = run.cancel();
                    }
                }
                if let Ok(installs) = state.installs.lock() {
                    for cancellation in installs.values() {
                        cancellation.cancel();
                    }
                }
                let idle = state.desktop.active_count() == 0;
                if !idle && start.elapsed() >= Duration::from_secs(5) {
                    let pending = inner.pending.as_mut().unwrap();
                    pending.request.phase = Phase::Slow;
                    pending.changed = Instant::now();
                    pending.acknowledged = false;
                    drop(inner);
                    publish(&app);
                    return;
                }
                idle
            };
            if idle {
                let finish_app = app.clone();
                let _ =
                    tauri::async_runtime::spawn_blocking(move || {
                        let state = finish_app.state::<Lifecycle>();
                        let mut inner = state.inner.lock().map_err(poisoned)?;
                        if inner.pending.as_ref().is_some_and(|p| {
                            p.request.id == id && p.request.phase == Phase::Stopping
                        }) && finish_exit(&finish_app, &state, &mut inner, true).is_err()
                        {
                            if let Some(p) = inner.pending.as_mut() {
                                p.request.phase = Phase::Slow;
                            }
                            drop(inner);
                            publish(&finish_app);
                        }
                        Ok::<_, Error>(())
                    })
                    .await;
                return;
            }
            // Only runs during an explicitly requested shutdown, never at idle.
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });
}
