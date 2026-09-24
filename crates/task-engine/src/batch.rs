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
    #[serde(default)]
    pub review_confirmed: bool,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct BatchView {
    pub meta: TaskMeta,
    pub items: Vec<BatchItem>,
    pub source_batch_id: Option<Uuid>,
}
#[derive(Serialize, Deserialize)]
struct BatchPayload {
    items: Vec<BatchItem>,
    #[serde(default)]
    paths: Vec<PathBuf>,
    #[serde(default)]
    source_batch_id: Option<Uuid>,
}
impl Engine {
    pub fn begin_batch(&self, paths: Vec<PathBuf>, id: Uuid) -> Result<BatchView> {
        if paths.is_empty() || paths.len() > 20 {
            return Err(Error::Invalid("每批需要 1–20 个文件".into()));
        }
        let mut total = 0u64;
        let mut items = Vec::with_capacity(paths.len());
        for (index, path) in paths.iter().enumerate() {
            let size = path.metadata().map(|m| m.len()).unwrap_or(0);
            total = total
                .checked_add(size)
                .ok_or_else(|| Error::Invalid("批量大小溢出".into()))?;
            items.push(BatchItem {
                index,
                task_id: Some(Uuid::new_v4()),
                state: TaskState::Queued,
                error: None,
                error_info: None,
                display_name: safe_display_name(path),
                extension: safe_extension(path),
                file_size: size,
                reviewed_revision: 0,
                review_confirmed: false,
            });
        }
        if total > 500 * 1024 * 1024 {
            return Err(Error::Invalid("每批总大小不得超过 500 MB".into()));
        }
        if self.store.task(id)?.is_some() {
            return Err(Error::Conflict("批次 ID 已存在".into()));
        }
        let meta = TaskMeta {
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
        self.store.save_payload(
            id,
            &BatchPayload {
                items,
                paths,
                source_batch_id: None,
            },
        )?;
        let mut meta = meta;
        self.move_to(&mut meta, TaskState::Analyzing)?;
        self.batch_view(id)
    }

    pub fn batch_source_path(&self, id: Uuid, index: usize) -> Result<PathBuf> {
        let payload: BatchPayload = self.store.payload(id)?;
        payload
            .paths
            .get(index)
            .filter(|p| !p.as_os_str().is_empty())
            .cloned()
            .ok_or_else(|| Error::Invalid("原文件位置未保留，请重新选择文件".into()))
    }

    /// Called by the control service after one worker completes; stores the full input list.
    pub fn record_batch_item(
        &self,
        id: Uuid,
        index: usize,
        error: Option<ErrorInfo>,
    ) -> Result<BatchView> {
        let mut payload: BatchPayload = self.store.payload(id)?;
        let item = payload
            .items
            .get_mut(index)
            .ok_or_else(|| Error::Invalid("批次项目不存在".into()))?;
        let meta = item
            .task_id
            .map(|id| self.store.task(id))
            .transpose()?
            .flatten();
        if let Some(mut meta) = meta {
            if let Some(error) = error.as_ref()
                && matches!(
                    meta.state,
                    TaskState::Queued | TaskState::Analyzing | TaskState::Processing
                )
            {
                meta.state = if error.code == "TASK_CANCELLED" {
                    TaskState::Cancelled
                } else {
                    TaskState::Failed
                };
                meta.error = Some(error.title.clone());
                meta.error_info = Some(error.clone());
                meta.updated_at = now();
                self.store.save_meta(&meta)?;
            }
            item.state = meta.state;
            item.error = meta.error;
            item.error_info = meta.error_info;
            item.reviewed_revision = meta.reviewed_revision;
            item.review_confirmed = meta.reviewed_revision > 0;
        } else if let Some(error) = error {
            item.state = if error.code == "TASK_CANCELLED" {
                TaskState::Cancelled
            } else {
                TaskState::Failed
            };
            item.error = Some(error.title.clone());
            item.error_info = Some(error);
        }
        self.store.save_payload(id, &payload)?;
        self.batch_view(id)
    }

    pub fn finish_batch_analysis(&self, id: Uuid, cancelled: bool) -> Result<BatchView> {
        let mut payload: BatchPayload = self.store.payload(id)?;
        let mut meta = self.meta(id)?;
        if cancelled {
            for item in &mut payload.items {
                if item.state == TaskState::Queued {
                    item.state = TaskState::Cancelled;
                    item.error = Some("尚未开始，已取消".into());
                    item.error_info =
                        Some(ErrorInfo::cancelled("原文件清单已保留，可继续未完成项目"));
                }
            }
            self.store.save_payload(id, &payload)?;
            meta.error = Some("批量分析已取消".into());
            meta.error_info = Some(ErrorInfo::cancelled(
                "已识别文件仍可复核，其余文件可继续处理",
            ));
            self.move_to(&mut meta, TaskState::Cancelled)?;
        } else if payload
            .items
            .iter()
            .any(|i| matches!(i.state, TaskState::AwaitingReview | TaskState::Completed))
        {
            self.move_to(&mut meta, TaskState::AwaitingReview)?;
        } else {
            meta.error = Some("批量分析失败".into());
            meta.error_info = Some(ErrorInfo::new(
                "BATCH_ANALYSIS_FAILED",
                "批量分析失败",
                "没有文件成功进入复核",
                "检查失败项后重试",
                true,
            ));
            self.move_to(&mut meta, TaskState::Failed)?;
        }
        self.batch_view(id)
    }

    pub fn begin_batch_execution(&self, id: Uuid) -> Result<BatchView> {
        let mut view = self.batch_view(id)?;
        self.move_to(&mut view.meta, TaskState::Processing)?;
        self.batch_view(id)
    }

    pub fn finish_batch_execution(&self, id: Uuid, cancelled: bool) -> Result<BatchView> {
        let mut view = self.batch_view(id)?;
        let mut payload: BatchPayload = self.store.payload(id)?;
        payload.items = view.items.clone();
        self.store.save_payload(id, &payload)?;
        let completed = view
            .items
            .iter()
            .filter(|i| i.state == TaskState::Completed)
            .count();
        let next = if cancelled {
            view.meta.error = Some("批量生成已取消".into());
            view.meta.error_info = Some(ErrorInfo::cancelled(
                "已完成文件可以导出，其余文件可继续处理",
            ));
            TaskState::Cancelled
        } else if completed == view.items.len() {
            TaskState::Completed
        } else if completed > 0 {
            TaskState::Partial
        } else {
            view.meta.error = Some("批量生成失败".into());
            view.meta.error_info = Some(ErrorInfo::new(
                "BATCH_PROCESSING_FAILED",
                "批量生成失败",
                "没有文件成功生成",
                "查看失败项提示后重试",
                true,
            ));
            TaskState::Failed
        };
        self.move_to(&mut view.meta, next)?;
        self.batch_view(id)
    }

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
        let view = self.begin_batch(paths.clone(), id)?;
        self.active_ocr
            .lock()
            .map_err(|_| Error::State("批次取消状态异常".into()))?
            .insert(id, run.clone());
        let result = (|| {
            let settings = self.store.settings()?;
            let options = TaskOptions {
                ocr_profile: settings.ocr_profile,
                pdf_mode: settings.pdf_mode,
            };
            for (index, path) in paths.iter().enumerate() {
                if run.is_cancelled() {
                    break;
                }
                let task_id = view.items[index].task_id.unwrap();
                let outcome =
                    self.analyze_file_for_batch(path, options.clone(), task_id, run.clone(), id);
                self.record_batch_item(
                    id,
                    index,
                    outcome
                        .err()
                        .map(|e| ErrorInfo::for_error(&e, "文件分析失败")),
                )?;
                progress(index + 1, paths.len());
            }
            let view = self.finish_batch_analysis(id, run.is_cancelled())?;
            if run.is_cancelled() {
                Err(Error::State("任务已取消".into()))
            } else {
                Ok(view)
            }
        })();
        self.active_ocr
            .lock()
            .map_err(|_| Error::State("批次取消状态异常".into()))?
            .remove(&id);
        result
    }

    pub fn batch_view(&self, id: Uuid) -> Result<BatchView> {
        let meta = self.meta(id)?;
        if meta.kind != "batch" {
            return Err(Error::Invalid("不是批量任务".into()));
        }
        let mut payload: BatchPayload = self.store.payload(id)?;
        for item in &mut payload.items {
            if let Some(task_id) = item.task_id {
                match self.store.task(task_id)? {
                    Some(task) => {
                        item.state = task.state;
                        item.reviewed_revision = task.reviewed_revision;
                        item.review_confirmed = task.reviewed_revision > 0;
                        item.error = task.error;
                        item.error_info = task.error_info;
                    }
                    None if matches!(
                        item.state,
                        TaskState::Queued | TaskState::Cancelled | TaskState::Failed
                    ) => {}
                    None => {
                        item.state = TaskState::Failed;
                        item.review_confirmed = false;
                        item.error = Some("任务已删除".into());
                        item.error_info = Some(ErrorInfo::new(
                            "TASK_MISSING",
                            "任务已删除",
                            "批次引用的子任务已经不存在",
                            "请重新选择源文件",
                            true,
                        ));
                    }
                }
            }
        }
        Ok(BatchView {
            meta,
            items: payload.items,
            source_batch_id: payload.source_batch_id,
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
        let view = self.begin_batch_execution(id)?;
        self.active_ocr
            .lock()
            .map_err(|_| Error::State("批次取消状态异常".into()))?
            .insert(id, run.clone());
        let result = (|| {
            for item in &view.items {
                if run.is_cancelled() {
                    break;
                }
                if let Some(task_id) = item.task_id
                    && item.state == TaskState::AwaitingReview
                {
                    let outcome = self.execute_as(task_id, run.clone());
                    self.record_batch_item(
                        id,
                        item.index,
                        outcome.err().map(|e| ErrorInfo::for_error(&e, "生成失败")),
                    )?;
                }
                progress(item.index + 1, view.items.len());
            }
            let result = self.finish_batch_execution(id, run.is_cancelled())?;
            if run.is_cancelled() {
                Err(Error::State("任务已取消".into()))
            } else {
                Ok(result)
            }
        })();
        self.active_ocr
            .lock()
            .map_err(|_| Error::State("批次取消状态异常".into()))?
            .remove(&id);
        result
    }

    /// Creates a new attempt, copying successes and review edits with fresh encrypted task IDs.
    pub fn prepare_retry_batch(&self, id: Uuid, failed_only: bool) -> Result<BatchView> {
        self.prepare_retry_batch_as(id, failed_only, Uuid::new_v4())
    }
    pub fn prepare_retry_batch_as(
        &self,
        id: Uuid,
        failed_only: bool,
        new_id: Uuid,
    ) -> Result<BatchView> {
        if self.store.task(new_id)?.is_some() {
            return Err(Error::Conflict("重试任务已存在，请打开该任务查看".into()));
        }
        let source = self.batch_view(id)?;
        if matches!(
            source.meta.state,
            TaskState::Queued | TaskState::Analyzing | TaskState::Processing
        ) {
            return Err(Error::State("请先等待批次停止再重试".into()));
        }
        let old: BatchPayload = self.store.payload(id)?;
        let mut payload = BatchPayload {
            items: Vec::new(),
            paths: old.paths,
            source_batch_id: Some(id),
        };
        for mut item in source.items {
            let retry = if failed_only {
                item.state == TaskState::Failed
            } else {
                item.state != TaskState::Completed
            };
            if let Some(old_id) = item.task_id {
                if let Ok(p) = self.task_payload(old_id) {
                    if p.revision > 0 {
                        let cloned = self.clone_for_review(old_id)?;
                        let mut meta = cloned.meta;
                        meta.parent_batch_id = Some(new_id);
                        if item.state == TaskState::Completed {
                            let output = self
                                .task_output(old_id, &p)?
                                .ok_or_else(|| Error::State("已完成任务缺少输出".into()))?;
                            self.store.save_output(meta.id, &output)?;
                            meta.state = TaskState::Completed;
                        } else if !retry
                            && matches!(item.state, TaskState::Failed | TaskState::Cancelled)
                        {
                            meta.state = item.state;
                        }
                        meta.reviewed_revision = if item.review_confirmed { 1 } else { 0 };
                        meta.error = None;
                        meta.error_info = None;
                        meta.storage_bytes = self
                            .store
                            .task(meta.id)?
                            .map(|m| m.storage_bytes)
                            .unwrap_or(0);
                        self.store.save_meta(&meta)?;
                        item.task_id = Some(meta.id);
                        item.state = meta.state;
                        item.reviewed_revision = meta.reviewed_revision;
                        item.error = None;
                        item.error_info = None;
                    } else if retry {
                        let new_task = Uuid::new_v4();
                        let mut meta = self.meta(old_id)?;
                        meta.id = new_task;
                        meta.state = TaskState::Queued;
                        meta.parent_batch_id = Some(new_id);
                        meta.created_at = now();
                        meta.updated_at = now();
                        meta.error = None;
                        meta.error_info = None;
                        meta.reviewed_revision = 0;
                        meta.storage_bytes = 0;
                        self.store.save_meta(&meta)?;
                        self.store.save_payload(new_task, &p)?;
                        item.task_id = Some(new_task);
                        item.state = TaskState::Queued;
                        item.error = None;
                        item.error_info = None;
                    }
                } else if retry {
                    item.task_id = Some(Uuid::new_v4());
                    item.state = TaskState::Queued;
                    item.error = None;
                    item.error_info = None;
                }
            } else if retry {
                item.task_id = Some(Uuid::new_v4());
                item.state = TaskState::Queued;
                item.error = None;
                item.error_info = None;
            }
            payload.items.push(item);
        }
        let mut meta = source.meta;
        meta.id = new_id;
        meta.state = TaskState::Analyzing;
        meta.created_at = now();
        meta.updated_at = now();
        meta.error = None;
        meta.error_info = None;
        meta.storage_bytes = 0;
        meta.display_name = "批量任务 · 继续处理".into();
        self.store.save_meta(&meta)?;
        self.store.save_payload(new_id, &payload)?;
        self.batch_view(new_id)
    }

    pub fn analyze_batch_item(&mut self, id: Uuid, index: usize, run: OcrRun) -> Result<TaskView> {
        let payload: BatchPayload = self.store.payload(id)?;
        let item = payload
            .items
            .get(index)
            .ok_or_else(|| Error::Invalid("批次项目不存在".into()))?;
        let task_id = item
            .task_id
            .ok_or_else(|| Error::State("批次项目缺少任务 ID".into()))?;
        if let Ok(saved) = self.task_payload(task_id) {
            return self.analyze(
                saved.document,
                saved.options,
                Some(task_id),
                Some(run),
                AnalysisSource {
                    display_name: item.display_name.clone(),
                    file_size: item.file_size,
                    parent_batch_id: Some(id),
                },
            );
        }
        let path = payload
            .paths
            .get(index)
            .ok_or_else(|| Error::Invalid("请重新选择原文件".into()))?;
        let settings = self.store.settings()?;
        self.analyze_file_for_batch(
            path,
            TaskOptions {
                ocr_profile: settings.ocr_profile,
                pdf_mode: settings.pdf_mode,
            },
            task_id,
            run,
            id,
        )
    }
    pub fn export_batch(&self, id: Uuid, path: &Path) -> Result<()> {
        let batch = self.batch_view(id)?;
        if matches!(
            batch.meta.state,
            TaskState::Analyzing | TaskState::Queued | TaskState::Processing
        ) {
            return Err(Error::State("请先等待批次停止，再导出已完成文件".into()));
        }
        let completed = batch
            .items
            .iter()
            .filter(|item| item.state == TaskState::Completed)
            .count();
        if completed == 0 {
            return Err(Error::State("没有可导出的已完成项目".into()));
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
    fn test_engine(root: &Path) -> Engine {
        Engine {
            store: Store::open(root, zeroize::Zeroizing::new([31; 32])).unwrap(),
            ner: Some(Box::new(Empty)),
            ocr_mobile: None,
            ocr_accurate: None,
            active_ocr: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    #[test]
    fn cancelled_generation_exports_successes_and_retry_preserves_them() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = test_engine(dir.path());
        let paths = (0..3)
            .map(|i| {
                let path = dir.path().join(format!("文件{i}.txt"));
                std::fs::write(&path, "电话13812345678").unwrap();
                path
            })
            .collect::<Vec<_>>();
        let batch = engine.create_batch(paths, |_, _| {}).unwrap();
        let run = OcrRun::new().unwrap();
        let cancel = run.clone();
        assert!(
            engine
                .execute_batch_as(batch.meta.id, run, move |done, _| {
                    if done == 1 {
                        cancel.cancel().unwrap();
                    }
                })
                .is_err()
        );
        let stopped = engine.batch_view(batch.meta.id).unwrap();
        assert_eq!(stopped.meta.state, TaskState::Cancelled);
        assert_eq!(
            stopped
                .items
                .iter()
                .filter(|i| i.state == TaskState::Completed)
                .count(),
            1
        );
        let zip_path = dir.path().join("已完成.zip");
        engine.export_batch(stopped.meta.id, &zip_path).unwrap();
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&zip_path).unwrap()).unwrap();
        assert_eq!(zip.len(), 2);
        use std::io::Read;
        let mut report = String::new();
        zip.by_name("report.json")
            .unwrap()
            .read_to_string(&mut report)
            .unwrap();
        assert!(report.contains("cancelled"));
        let request_id = Uuid::new_v4();
        let retry = engine
            .prepare_retry_batch_as(stopped.meta.id, false, request_id)
            .unwrap();
        assert_eq!(retry.meta.id, request_id);
        assert!(
            engine
                .prepare_retry_batch_as(stopped.meta.id, false, request_id)
                .is_err()
        );
        assert_eq!(retry.source_batch_id, Some(stopped.meta.id));
        assert_eq!(retry.items[0].state, TaskState::Completed);
        assert_ne!(retry.items[0].task_id, stopped.items[0].task_id);
        engine.finish_batch_analysis(retry.meta.id, false).unwrap();
        let result = engine.execute_batch(retry.meta.id, |_, _| {}).unwrap();
        assert_eq!(result.meta.state, TaskState::Completed);
        assert_eq!(
            engine.batch_view(stopped.meta.id).unwrap().meta.state,
            TaskState::Cancelled
        );
        // Each attempt has independent encrypted output; deleting the old child is safe.
        engine
            .store
            .delete(stopped.items[0].task_id.unwrap())
            .unwrap();
        engine
            .export_batch(result.meta.id, &dir.path().join("续处理.zip"))
            .unwrap();
    }

    #[test]
    fn cancelled_analysis_keeps_every_input_and_confirmation_is_explicit() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = test_engine(dir.path());
        let paths = (0..3)
            .map(|i| {
                let path = dir.path().join(format!("文件{i}.txt"));
                std::fs::write(&path, "电话13812345678").unwrap();
                path
            })
            .collect::<Vec<_>>();
        let id = Uuid::new_v4();
        let run = OcrRun::new().unwrap();
        let cancel = run.clone();
        assert!(
            engine
                .create_batch_as(paths.clone(), id, run, move |done, _| {
                    if done == 1 {
                        cancel.cancel().unwrap();
                    }
                })
                .is_err()
        );
        let stopped = engine.batch_view(id).unwrap();
        assert_eq!(stopped.items.len(), 3);
        assert_eq!(stopped.items[0].state, TaskState::AwaitingReview);
        assert_eq!(stopped.items[2].state, TaskState::Cancelled);
        assert!(!stopped.items[0].review_confirmed);
        let first = engine.view(stopped.items[0].task_id.unwrap()).unwrap();
        engine
            .confirm_review(first.meta.id, first.revision)
            .unwrap();
        assert!(engine.batch_view(id).unwrap().items[0].review_confirmed);
        let retry = engine.prepare_retry_batch(id, false).unwrap();
        assert!(retry.items[0].review_confirmed);
        assert_eq!(
            engine.batch_source_path(retry.meta.id, 2).unwrap(),
            paths[2]
        );
        for item in retry
            .items
            .iter()
            .filter(|item| item.state == TaskState::Queued)
        {
            engine
                .analyze_batch_item(retry.meta.id, item.index, OcrRun::new().unwrap())
                .unwrap();
            engine
                .record_batch_item(retry.meta.id, item.index, None)
                .unwrap();
        }
        let ready = engine.finish_batch_analysis(retry.meta.id, false).unwrap();
        assert!(
            ready
                .items
                .iter()
                .all(|item| item.state == TaskState::AwaitingReview)
        );
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
