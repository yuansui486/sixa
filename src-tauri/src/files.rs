use crate::*;
use storage::files::{OutputPage, OutputQuery, OutputRecord};

fn default_directory(app: &tauri::AppHandle) -> Result<PathBuf> {
    app.path()
        .document_dir()
        .map(|path| path.join("私匣").join("处理结果"))
        .map_err(|e| Error::Io(format!("无法找到文稿目录，请在文件管理中指定保存位置：{e}")))
}

pub async fn automatic(state: Shared<'_>, app: &tauri::AppHandle, id: Uuid) -> Result<()> {
    let default = default_directory(app);
    work(state, move |engine| {
        let directory = match engine.store.output_directory()? {
            Some(path) => path,
            None => default?,
        };
        engine.save_automatically(id, &directory)?;
        Ok(())
    })
    .await?;
    let _ = app.emit("files-updated", ());
    Ok(())
}

#[tauri::command]
pub async fn get_file_directory(state: Shared<'_>, app: tauri::AppHandle) -> Result<PathBuf> {
    let default = default_directory(&app);
    work(state, move |e| match e.store.output_directory()? {
        Some(path) => Ok(path),
        None => default,
    })
    .await
}

#[tauri::command]
pub async fn set_file_directory(state: Shared<'_>, path: PathBuf) -> Result<()> {
    work(state, move |e| e.store.save_output_directory(&path)).await
}

#[tauri::command]
pub async fn query_output_files(state: Shared<'_>, query: OutputQuery) -> Result<OutputPage> {
    work(state, move |e| e.store.output_page(query)).await
}

#[tauri::command]
pub async fn retry_output_file(
    state: Shared<'_>,
    app: tauri::AppHandle,
    id: Uuid,
) -> Result<OutputRecord> {
    let default = default_directory(&app);
    let result = work(state, move |e| {
        let directory = match e.store.output_directory()? {
            Some(path) => path,
            None => default?,
        };
        e.retry_output(id, &directory)
    })
    .await;
    let _ = app.emit("files-updated", ());
    result
}

#[tauri::command]
pub async fn open_output_file(state: Shared<'_>, id: Uuid) -> Result<()> {
    work(state, move |e| {
        let record = e.store.output_record(id)?;
        if record.error.is_some() || !std::path::Path::new(&record.path).is_file() {
            return Err(Error::Invalid(
                "文件尚未保存或已被移动、删除，请刷新文件列表".into(),
            ));
        }
        #[cfg(windows)]
        {
            // Explorer opens only a recorded result; never interpolate a shell command.
            std::process::Command::new("explorer.exe")
                .arg(&record.path)
                .spawn()?;
        }
        #[cfg(target_os = "macos")]
        {
            std::process::Command::new("/usr/bin/open")
                .arg(&record.path)
                .spawn()?;
        }
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn output_task_view(state: Shared<'_>, id: Uuid) -> Result<TaskView> {
    work(state, move |e| {
        let record = e.store.output_record(id)?;
        let view = e.view(record.task_id)?;
        if record.generation != view.revision.to_string() {
            return Err(Error::State(
                "任务已重新编辑，无法预览旧版本；请点击打开文件查看已保存结果".into(),
            ));
        }
        Ok(view)
    })
    .await
}

#[tauri::command]
pub async fn copy_output_file(state: Shared<'_>, id: Uuid, path: PathBuf) -> Result<()> {
    work(state, move |e| {
        let mut record = e.store.output_record(id)?;
        if record.error.is_some() || !path.is_absolute() {
            return Err(Error::Invalid(
                "请先成功保存文件，并选择有效的另存位置".into(),
            ));
        }
        let source = PathBuf::from(&record.path).canonicalize()?;
        if path.canonicalize().ok().as_ref() == Some(&source) {
            return Err(Error::Invalid("请选择不同的文件名或目录".into()));
        }
        let parent = path
            .parent()
            .ok_or_else(|| Error::Invalid("保存目录无效".into()))?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        let mut input = std::fs::File::open(&source)?;
        std::io::copy(&mut input, &mut temporary)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist_noclobber(&path)
            .map_err(|e| Error::Io(format!("不会覆盖已有文件，请更换文件名：{}", e.error)))?;
        record.id = Uuid::new_v4();
        record.path = path.to_string_lossy().into_owned();
        record.name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        record.created_at = chrono::Utc::now().timestamp().max(0) as u64;
        record.size = std::fs::metadata(&path)?.len();
        record.source = "manual".into();
        e.store.put_output(&record, None)
    })
    .await
}
