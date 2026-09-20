use super::*;
use std::{
    collections::HashSet,
    io::{Cursor, Write},
    path::{Path, PathBuf},
};
use zip::{ZipWriter, write::SimpleFileOptions};

#[derive(Clone, Serialize, Deserialize)]
pub struct BatchItem {
    pub index: usize,
    pub task_id: Option<Uuid>,
    pub state: TaskState,
    pub error: Option<String>,
    #[serde(default)]
    pub error_info: Option<ErrorInfo>,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub extension: String,
    #[serde(default)]
    pub file_size: u64,
    #[serde(default)]
    pub reviewed_revision: u64,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct BatchView {
    pub meta: TaskMeta,
    pub items: Vec<BatchItem>,
}
#[derive(Serialize, Deserialize)]
struct BatchPayload {
    items: Vec<BatchItem>,
}
impl Engine {
    pub fn create_batch(
        &mut self,
        paths: Vec<PathBuf>,
        progress: impl FnMut(usize, usize),
    ) -> Result<BatchView> {
        self.create_batch_as(paths, Uuid::new_v4(), OcrRun::new()?, progress)
    }

    pub fn create_batch_as(
        &mut self,
        paths: Vec<PathBuf>,
        id: Uuid,
        run: OcrRun,
        mut progress: impl FnMut(usize, usize),
    ) -> Result<BatchView> {
        if self.ner.is_none() {
            return Err(Error::ModelsNotReady("请先加载 RaNER 模型".into()));
        }
        if paths.is_empty() || paths.len() > 20 {
            return Err(Error::Invalid("每批需要 1–20 个文件".into()));
        }
        let mut total = 0u64;
        for path in &paths {
            if let Ok(meta) = path.metadata() {
                total = total
                    .checked_add(meta.len())
                    .ok_or_else(|| Error::Invalid("批量大小溢出".into()))?;
            }
        }
        if total > 500 * 1024 * 1024 {
            return Err(Error::Invalid("每批总大小不得超过 500 MB".into()));
        }
        let mut meta = TaskMeta {
            id,
            kind: "batch".into(),
            state: TaskState::Queued,
            created_at: now(),
            updated_at: now(),
            error: None,
            error_info: None,
            display_name: "批量任务".into(),
            file_size: total,
            parent_batch_id: None,
            reviewed_revision: 0,
            storage_bytes: 0,
        };
        self.store.save_meta(&meta)?;
        self.move_to(&mut meta, TaskState::Analyzing)?;
        let mut payload = BatchPayload { items: vec![] };
        self.store.save_payload(meta.id, &payload)?;
        self.active_ocr
            .lock()
            .map_err(|_| Error::State("批量任务取消状态异常".into()))?
            .insert(meta.id, run.clone());
        for (index, path) in paths.iter().enumerate() {
            if run.is_cancelled() {
                self.store.save_payload(meta.id, &payload)?;
                meta.error = Some("批量分析已取消".into());
                meta.error_info = Some(ErrorInfo::cancelled("批量分析已停止"));
                self.move_to(&mut meta, TaskState::Cancelled)?;
                self.active_ocr
                    .lock()
                    .map_err(|_| Error::State("批量任务取消状态异常".into()))?
                    .remove(&meta.id);
                return Err(Error::State("任务已取消".into()));
            }
            let display_name = safe_display_name(path);
            let extension = safe_extension(path);
            let file_size = path.metadata().map(|metadata| metadata.len()).unwrap_or(0);
            let task_id = Uuid::new_v4();
            let item = match self.analyze_file_for_batch(
                path,
                TaskOptions::default(),
                task_id,
                run.clone(),
                meta.id,
            ) {
                Ok(task) => BatchItem {
                    index,
                    task_id: Some(task.meta.id),
                    state: task.meta.state,
                    error: None,
                    error_info: None,
                    display_name,
                    extension,
                    file_size,
                    reviewed_revision: task.meta.reviewed_revision,
                },
                Err(error) => {
                    let failed_meta = self.store.task(task_id)?;
                    BatchItem {
                        index,
                        task_id: failed_meta.as_ref().map(|task| task.id),
                        state: TaskState::Failed,
                        error: failed_meta
                            .as_ref()
                            .and_then(|task| task.error.clone())
                            .or_else(|| Some("分析失败：请检查文件格式、大小及模型状态".into())),
                        error_info: failed_meta
                            .and_then(|task| task.error_info)
                            .or_else(|| Some(ErrorInfo::for_error(&error, "文件分析失败"))),
                        display_name,
                        extension,
                        file_size,
                        reviewed_revision: 0,
                    }
                }
            };
            payload.items.push(item);
            self.store.save_payload(meta.id, &payload)?;
            progress(index + 1, paths.len());
            if run.is_cancelled() {
                meta.error = Some("批量分析已取消".into());
                meta.error_info = Some(ErrorInfo::cancelled("批量分析已停止"));
                self.move_to(&mut meta, TaskState::Cancelled)?;
                self.active_ocr
                    .lock()
                    .map_err(|_| Error::State("批量任务取消状态异常".into()))?
                    .remove(&meta.id);
                return Err(Error::State("任务已取消".into()));
            }
        }
        let next_state = if payload
            .items
            .iter()
            .any(|item| item.state == TaskState::AwaitingReview)
        {
            TaskState::AwaitingReview
        } else {
            meta.error = Some("批量分析失败".into());
            meta.error_info = Some(ErrorInfo::new(
                "BATCH_ANALYSIS_FAILED",
                "批量分析失败",
                "批次中没有文件成功进入复核",
                "检查失败项提示后重新创建批量任务",
                true,
            ));
            TaskState::Failed
        };
        self.move_to(&mut meta, next_state)?;
        self.active_ocr
            .lock()
            .map_err(|_| Error::State("批量任务取消状态异常".into()))?
            .remove(&meta.id);
        self.batch_view(meta.id)
    }
    pub fn batch_view(&self, id: Uuid) -> Result<BatchView> {
        let meta = self.meta(id)?;
        if meta.kind != "batch" {
            return Err(Error::Invalid("不是批量任务".into()));
        }
        let mut payload: BatchPayload = self.store.payload(id)?;
        for item in &mut payload.items {
            if let Some(id) = item.task_id {
                match self.meta(id) {
                    Ok(task) => {
                        item.state = task.state;
                        item.reviewed_revision = task.reviewed_revision;
                        item.error = task.error;
                        item.error_info = task.error_info;
                    }
                    Err(_) => {
                        item.state = TaskState::Failed;
                        item.error = Some("任务已删除".into());
                        item.error_info = Some(ErrorInfo::new(
                            "TASK_MISSING",
                            "任务已删除",
                            "批次引用的子任务已经不存在",
                            "从原文件重新创建批量任务",
                            false,
                        ));
                    }
                }
            }
        }
        Ok(BatchView {
            meta,
            items: payload.items,
        })
    }
    pub fn execute_batch(&self, id: Uuid, progress: impl FnMut(usize, usize)) -> Result<BatchView> {
        self.execute_batch_as(id, OcrRun::new()?, progress)
    }

    pub fn execute_batch_as(
        &self,
        id: Uuid,
        run: OcrRun,
        mut progress: impl FnMut(usize, usize),
    ) -> Result<BatchView> {
        let mut batch = self.batch_view(id)?;
        self.move_to(&mut batch.meta, TaskState::Processing)?;
        self.active_ocr
            .lock()
            .map_err(|_| Error::State("批量任务取消状态异常".into()))?
            .insert(id, run.clone());
        let count = batch.items.len();
        for i in 0..count {
            if run.is_cancelled() {
                self.store.save_payload(
                    id,
                    &BatchPayload {
                        items: batch.items.clone(),
                    },
                )?;
                batch.meta.error = Some("批量生成已取消".into());
                batch.meta.error_info = Some(ErrorInfo::cancelled("批量生成已停止"));
                self.move_to(&mut batch.meta, TaskState::Cancelled)?;
                self.active_ocr
                    .lock()
                    .map_err(|_| Error::State("批量任务取消状态异常".into()))?
                    .remove(&id);
                return Err(Error::State("任务已取消".into()));
            }
            let item = &mut batch.items[i];
            if let Some(task_id) = item.task_id
                && item.state == TaskState::AwaitingReview
            {
                match self.execute_as(task_id, run.clone()) {
                    Ok(task) => item.state = task.meta.state,
                    Err(error) => {
                        item.state = TaskState::Failed;
                        item.error = Some("生成失败".into());
                        item.error_info = Some(ErrorInfo::for_error(&error, "文件生成失败"));
                    }
                }
            }
            progress(i + 1, count);
            if run.is_cancelled() {
                self.store.save_payload(
                    id,
                    &BatchPayload {
                        items: batch.items.clone(),
                    },
                )?;
                batch.meta.error = Some("批量生成已取消".into());
                batch.meta.error_info = Some(ErrorInfo::cancelled("批量生成已停止"));
                self.move_to(&mut batch.meta, TaskState::Cancelled)?;
                self.active_ocr
                    .lock()
                    .map_err(|_| Error::State("批量任务取消状态异常".into()))?
                    .remove(&id);
                return Err(Error::State("任务已取消".into()));
            }
        }
        let completed = batch
            .items
            .iter()
            .filter(|i| i.state == TaskState::Completed)
            .count();
        self.store
            .save_payload(id, &BatchPayload { items: batch.items })?;
        let next_state = if completed == count {
            TaskState::Completed
        } else if completed > 0 {
            TaskState::Partial
        } else {
            batch.meta.error = Some("批量生成失败".into());
            batch.meta.error_info = Some(ErrorInfo::new(
                "BATCH_PROCESSING_FAILED",
                "批量生成失败",
                "批次中没有文件成功生成",
                "查看失败项提示后重新创建任务",
                true,
            ));
            TaskState::Failed
        };
        self.move_to(&mut batch.meta, next_state)?;
        self.active_ocr
            .lock()
            .map_err(|_| Error::State("批量任务取消状态异常".into()))?
            .remove(&id);
        self.batch_view(id)
    }
    pub fn export_batch(&self, id: Uuid, path: &Path) -> Result<()> {
        let mut batch = self.batch_view(id)?;
        if !matches!(batch.meta.state, TaskState::Completed | TaskState::Partial) {
            return Err(Error::State("批量任务尚未完成".into()));
        }
        let completed = batch
            .items
            .iter()
            .filter(|item| item.state == TaskState::Completed)
            .count();
        if completed == 0 {
            return Err(Error::State("没有可导出的已完成项目".into()));
        }
        if completed < batch.items.len() {
            batch.meta.state = TaskState::Partial;
        }
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        let zip_err = |e: zip::result::ZipError| Error::Io(e.to_string());
        let mut used_names = HashSet::new();
        for item in &batch.items {
            if item.state != TaskState::Completed {
                continue;
            }
            if let Some(task_id) = item.task_id {
                let payload = self.task_payload(task_id)?;
                let output = self
                    .task_output(task_id, &payload)?
                    .ok_or_else(|| Error::State("已完成的任务缺少输出，禁止导出".into()))?;
                let entry_name =
                    unique_export_name(item, &payload.document.extension, &mut used_names);
                zip.start_file(entry_name, SimpleFileOptions::default())
                    .map_err(zip_err)?;
                zip.write_all(&output)?;
            }
        }
        zip.start_file("report.json", SimpleFileOptions::default())
            .map_err(zip_err)?;
        zip.write_all(&serde_json::to_vec_pretty(&batch).map_err(|e| Error::Io(e.to_string()))?)?;
        storage::atomic_write(path, &zip.finish().map_err(zip_err)?.into_inner())
    }
}

fn unique_export_name(
    item: &BatchItem,
    output_extension: &str,
    used_names: &mut HashSet<String>,
) -> String {
    let safe_name = if item.display_name.is_empty() {
        format!("第{}个文件", item.index + 1)
    } else {
        item.display_name.clone()
    };
    let stem = Path::new(&safe_name)
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("未命名文件");
    let extension = output_extension
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .take(16)
        .collect::<String>()
        .to_ascii_lowercase();
    let extension = if extension.is_empty() {
        "bin".to_owned()
    } else {
        extension
    };
    for suffix in 1usize.. {
        let candidate = if suffix == 1 {
            format!("{stem}-脱敏结果.{extension}")
        } else {
            format!("{stem}-脱敏结果 ({suffix}).{extension}")
        };
        if used_names.insert(candidate.to_lowercase()) {
            return candidate;
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Empty;
    impl Ner for Empty {
        fn analyze(&mut self, _: &str) -> Result<Vec<Entity>> {
            Ok(vec![])
        }
    }
    #[test]
    fn old_batch_item_json_uses_compatible_defaults() {
        let item: BatchItem =
            serde_json::from_str(r#"{"index":0,"task_id":null,"state":"failed","error":"旧错误"}"#)
                .unwrap();
        assert_eq!(item.display_name, "");
        assert_eq!(item.extension, "");
        assert_eq!(item.file_size, 0);
        assert_eq!(item.reviewed_revision, 0);
        assert!(item.error_info.is_none());
    }

    #[test]
    fn batch_survives_restart_and_reports_failed_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("输入.txt");
        std::fs::write(&input, "电话13812345678").unwrap();
        let mut engine = Engine {
            store: Store::open(dir.path(), zeroize::Zeroizing::new([5; 32])).unwrap(),
            ner: Some(Box::new(Empty)),
            ocr_mobile: None,
            ocr_accurate: None,
            active_ocr: std::sync::Arc::new(
                std::sync::Mutex::new(std::collections::HashMap::new()),
            ),
        };
        let batch = engine
            .create_batch(vec![input, dir.path().join("missing.pdf")], |_, _| {})
            .unwrap();
        let id = batch.meta.id;
        assert_eq!(batch.items.len(), 2);
        assert_eq!(batch.items[0].display_name, "输入.txt");
        assert_eq!(batch.items[0].extension, "txt");
        let child = engine.meta(batch.items[0].task_id.unwrap()).unwrap();
        assert_eq!(child.parent_batch_id, Some(id));
        assert_eq!(child.display_name, "输入.txt");
        drop(engine);
        let engine = Engine {
            store: Store::open(dir.path(), zeroize::Zeroizing::new([5; 32])).unwrap(),
            ner: None,
            ocr_mobile: None,
            ocr_accurate: None,
            active_ocr: std::sync::Arc::new(
                std::sync::Mutex::new(std::collections::HashMap::new()),
            ),
        };
        assert_eq!(engine.batch_view(id).unwrap().items.len(), 2);
        let result = engine.execute_batch(id, |_, _| {}).unwrap();
        assert_eq!(result.meta.state, TaskState::Partial);
        let output = dir.path().join("结果.zip");
        engine.export_batch(id, &output).unwrap();
        let mut zip = zip::ZipArchive::new(std::fs::File::open(output).unwrap()).unwrap();
        use std::io::Read;
        let mut text = String::new();
        zip.by_name("输入-脱敏结果.txt")
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, "电话138****5678");
        assert!(zip.by_name("report.json").is_ok());
    }

    #[test]
    fn batch_export_preserves_names_and_resolves_case_insensitive_collisions() {
        let dir = tempfile::tempdir().unwrap();
        let first_dir = dir.path().join("first");
        let second_dir = dir.path().join("second");
        std::fs::create_dir_all(&first_dir).unwrap();
        std::fs::create_dir_all(&second_dir).unwrap();
        let first = first_dir.join("同名.txt");
        let second = second_dir.join("同名.TXT");
        std::fs::write(&first, "电话13812345678").unwrap();
        std::fs::write(&second, "电话13912345678").unwrap();
        let mut engine = Engine {
            store: Store::open(dir.path(), zeroize::Zeroizing::new([7; 32])).unwrap(),
            ner: Some(Box::new(Empty)),
            ocr_mobile: None,
            ocr_accurate: None,
            active_ocr: std::sync::Arc::new(
                std::sync::Mutex::new(std::collections::HashMap::new()),
            ),
        };
        let batch = engine.create_batch(vec![first, second], |_, _| {}).unwrap();
        engine.execute_batch(batch.meta.id, |_, _| {}).unwrap();
        let output = dir.path().join("结果.zip");
        engine.export_batch(batch.meta.id, &output).unwrap();
        let mut zip = zip::ZipArchive::new(std::fs::File::open(output).unwrap()).unwrap();
        assert!(zip.by_name("同名-脱敏结果.txt").is_ok());
        assert!(zip.by_name("同名-脱敏结果 (2).txt").is_ok());
    }

    #[test]
    fn all_failed_batch_has_actionable_error_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = Engine {
            store: Store::open(dir.path(), zeroize::Zeroizing::new([8; 32])).unwrap(),
            ner: Some(Box::new(Empty)),
            ocr_mobile: None,
            ocr_accurate: None,
            active_ocr: std::sync::Arc::new(
                std::sync::Mutex::new(std::collections::HashMap::new()),
            ),
        };
        let batch = engine
            .create_batch(vec![dir.path().join("缺失.pdf")], |_, _| {})
            .unwrap();
        assert_eq!(batch.meta.state, TaskState::Failed);
        assert_eq!(batch.meta.error_info.unwrap().code, "BATCH_ANALYSIS_FAILED");
        assert_eq!(batch.items[0].display_name, "缺失.pdf");
        assert!(batch.items[0].error_info.is_some());
    }

    #[test]
    fn batch_analysis_and_generation_can_be_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("输入.txt");
        std::fs::write(&input, "电话13812345678").unwrap();
        let mut engine = Engine {
            store: Store::open(dir.path(), zeroize::Zeroizing::new([6; 32])).unwrap(),
            ner: Some(Box::new(Empty)),
            ocr_mobile: None,
            ocr_accurate: None,
            active_ocr: std::sync::Arc::new(
                std::sync::Mutex::new(std::collections::HashMap::new()),
            ),
        };

        let analysis_id = Uuid::new_v4();
        let analysis_run = OcrRun::new().unwrap();
        analysis_run.cancel().unwrap();
        assert!(
            engine
                .create_batch_as(vec![input.clone()], analysis_id, analysis_run, |_, _| {})
                .is_err()
        );
        assert_eq!(
            engine.meta(analysis_id).unwrap().state,
            TaskState::Cancelled
        );
        assert_eq!(
            engine.meta(analysis_id).unwrap().error_info.unwrap().code,
            "TASK_CANCELLED"
        );

        let batch = engine.create_batch(vec![input], |_, _| {}).unwrap();
        let generation_run = OcrRun::new().unwrap();
        generation_run.cancel().unwrap();
        assert!(
            engine
                .execute_batch_as(batch.meta.id, generation_run, |_, _| {})
                .is_err()
        );
        assert_eq!(
            engine.meta(batch.meta.id).unwrap().state,
            TaskState::Cancelled
        );
    }
}
