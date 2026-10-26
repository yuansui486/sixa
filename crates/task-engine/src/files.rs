use super::*;
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf};
use storage::files::OutputRecord;

impl Engine {
    pub fn export_recorded(&self, id: Uuid, path: &Path) -> Result<()> {
        if !path.is_absolute() {
            return Err(Error::Invalid("请选择绝对保存路径".into()));
        }
        let parent = path
            .parent()
            .ok_or_else(|| Error::Invalid("保存目录无效".into()))?;
        let temporary = tempfile::NamedTempFile::new_in(parent)?.into_temp_path();
        if self.meta(id)?.kind == "batch" {
            self.export_batch(id, &temporary)?;
        } else {
            self.export(id, &temporary)?;
        }
        temporary.persist_noclobber(path).map_err(|e| {
            Error::Io(format!(
                "无法保存（不会覆盖已有文件），请更换文件名：{}",
                e.error
            ))
        })?;
        self.record_export(id, path, "manual")?;
        Ok(())
    }
    fn output_generation(&self, id: Uuid) -> Result<(String, String, bool, usize)> {
        let meta = self.meta(id)?;
        if meta.kind == "batch" {
            let batch = self.batch_view(id)?;
            let revisions: Vec<_> = batch
                .items
                .iter()
                .map(|item| {
                    let revision = if item.state == TaskState::Completed {
                        item.task_id
                            .map(|id| self.view(id).map(|v| v.revision))
                            .transpose()?
                    } else {
                        None
                    };
                    Ok((item.task_id, item.state, revision))
                })
                .collect::<Result<_>>()?;
            let bytes = serde_json::to_vec(&revisions).map_err(|e| Error::Io(e.to_string()))?;
            Ok((
                format!("{:x}", Sha256::digest(bytes)),
                "zip".into(),
                meta.state == TaskState::Partial,
                batch
                    .items
                    .iter()
                    .filter(|i| i.state != TaskState::Completed)
                    .count(),
            ))
        } else {
            let view = self.view(id)?;
            Ok((view.revision.to_string(), view.extension, false, 0))
        }
    }

    pub fn record_export(&self, id: Uuid, path: &Path, source: &str) -> Result<OutputRecord> {
        let (generation, kind, partial, failed_count) = self.output_generation(id)?;
        let record = OutputRecord {
            id: Uuid::new_v4(),
            task_id: id,
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            kind,
            path: path.to_string_lossy().into_owned(),
            created_at: now(),
            size: fs::metadata(path)?.len(),
            error: None,
            generation,
            partial,
            failed_count,
            source: source.into(),
            exists: true,
            task_exists: true,
        };
        self.store.put_output(&record, None)?;
        Ok(record)
    }

    pub fn save_automatically(&self, id: Uuid, directory: &Path) -> Result<Option<OutputRecord>> {
        let meta = self.meta(id)?;
        if meta.parent_batch_id.is_some()
            || !matches!(meta.state, TaskState::Completed | TaskState::Partial)
        {
            return Ok(None);
        }
        let (generation, kind, partial, failed_count) = self.output_generation(id)?;
        let key = format!("{id}:{generation}");
        if let Some(existing) = self.store.automatic_output(&key)? {
            return Ok(Some(existing));
        }
        let stem = if meta.kind == "batch" {
            "批量"
        } else {
            Path::new(&meta.display_name)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("文件")
        };
        let name = format!("{}_脱敏.{kind}", safe_stem(stem));
        let mut record = OutputRecord {
            id: Uuid::new_v4(),
            task_id: id,
            name,
            kind,
            path: directory.to_string_lossy().into_owned(),
            created_at: now(),
            size: 0,
            error: Some("保存尚未完成；如果上次运行中断，请重试保存".into()),
            generation,
            partial,
            failed_count,
            source: "automatic".into(),
            exists: false,
            task_exists: true,
        };
        // Claim the generation before writing, including across separate worker connections.
        if let Err(error) = self.store.put_output(&record, Some(&key)) {
            if let Some(existing) = self.store.automatic_output(&key)? {
                return Ok(Some(existing));
            }
            return Err(error);
        }
        self.write_output(&mut record, directory);
        self.store.put_output(&record, None)?;
        Ok(Some(record))
    }

    pub fn retry_output(&self, record_id: Uuid, directory: &Path) -> Result<OutputRecord> {
        let mut record = self.store.output_record(record_id)?;
        if record.error.is_none() {
            return Err(Error::State("该结果已经保存，请使用另存为".into()));
        }
        if self.output_generation(record.task_id)?.0 != record.generation {
            return Err(Error::State(
                "任务已重新编辑，请从任务页面保存最新结果".into(),
            ));
        }
        self.write_output(&mut record, directory);
        self.store.put_output(&record, None)?;
        Ok(record)
    }

    fn write_output(&self, record: &mut OutputRecord, directory: &Path) {
        let result = (|| -> Result<PathBuf> {
            if !directory.is_absolute() {
                return Err(Error::Invalid(
                    "保存位置必须是绝对目录，请在文件管理中重新选择".into(),
                ));
            }
            fs::create_dir_all(directory)?;
            let mut staging = tempfile::NamedTempFile::new_in(directory)?.into_temp_path();
            if self.meta(record.task_id)?.kind == "batch" {
                self.export_batch(record.task_id, &staging)?;
            } else {
                self.export(record.task_id, &staging)?;
            }
            let stem = Path::new(&record.name)
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy();
            for index in 1..=10000 {
                let name = if index == 1 {
                    record.name.clone()
                } else {
                    format!("{stem} ({index}).{}", record.kind)
                };
                let destination = directory.join(name);
                match staging.persist_noclobber(&destination) {
                    Ok(()) => return Ok(destination),
                    Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
                        staging = e.path;
                    }
                    Err(e) => {
                        return Err(Error::Io(format!(
                            "无法保存到此目录：{}。请更换保存位置",
                            e.error
                        )));
                    }
                }
            }
            Err(Error::Io("同名文件过多，请更换保存目录".into()))
        })();
        match result {
            Ok(path) => {
                record.size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                record.name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                record.path = path.to_string_lossy().into_owned();
                record.error = None;
                record.exists = true;
            }
            Err(error) => {
                record.path = directory.to_string_lossy().into_owned();
                record.error = Some(format!("处理已完成，保存失败：{error}"));
            }
        }
    }
}

fn safe_stem(stem: &str) -> String {
    let value: String = stem
        .chars()
        .take(100)
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    if value.trim().is_empty() {
        "文件".into()
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct EmptyNer;
    impl Ner for EmptyNer {
        fn analyze(&mut self, _: &str) -> Result<Vec<Entity>> {
            Ok(Vec::new())
        }
    }
    fn engine(root: &Path) -> Engine {
        Engine {
            store: Store::open(root, zeroize::Zeroizing::new([8; 32])).unwrap(),
            ner: Some(Box::new(EmptyNer)),
            ocr_mobile: None,
            ocr_accurate: None,
            active_ocr: Arc::new(Mutex::new(HashMap::new())),
        }
    }
    fn complete(engine: &mut Engine) -> Uuid {
        let view = engine.analyze_text("13812345678".into()).unwrap();
        engine.execute(view.meta.id).unwrap();
        view.meta.id
    }
    #[test]
    fn unique_output_is_durable_idempotent_and_survives_task_deletion() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("中文 结果");
        let mut e = engine(root.path());
        let id = complete(&mut e);
        let saved = e.save_automatically(id, &output).unwrap().unwrap();
        assert!(saved.error.is_none(), "{:?}", saved.error);
        assert_eq!(fs::read_to_string(&saved.path).unwrap(), "138****5678");
        assert_eq!(
            e.save_automatically(id, &output).unwrap().unwrap().id,
            saved.id
        );
        let second = complete(&mut e);
        let other = e.save_automatically(second, &output).unwrap().unwrap();
        assert_ne!(saved.path, other.path);
        assert!(other.name.contains("(2)"));
        e.store.save_output_directory(&output).unwrap();
        e.store.delete(id).unwrap();
        drop(e);
        let e = engine(root.path());
        assert_eq!(e.store.output_directory().unwrap(), Some(output));
        let page = e.store.output_page(Default::default()).unwrap();
        assert_eq!(page.total, 2);
        let record = page.items.iter().find(|r| r.id == saved.id).unwrap();
        assert!(record.exists);
        assert!(!record.task_exists);
        fs::remove_file(&saved.path).unwrap();
        assert!(
            !e.store
                .output_page(Default::default())
                .unwrap()
                .items
                .iter()
                .find(|r| r.id == saved.id)
                .unwrap()
                .exists
        );
    }
    #[test]
    fn failed_save_can_retry_without_reprocessing_and_old_task_is_not_exported_on_open() {
        let root = tempfile::tempdir().unwrap();
        let mut e = engine(root.path());
        let id = complete(&mut e);
        assert_eq!(e.store.output_page(Default::default()).unwrap().total, 0);
        let blocked = root.path().join("file-not-directory");
        fs::write(&blocked, "original").unwrap();
        let failed = e.save_automatically(id, &blocked).unwrap().unwrap();
        assert!(failed.error.is_some());
        assert_eq!(fs::read_to_string(&blocked).unwrap(), "original");
        assert_eq!(e.meta(id).unwrap().state, TaskState::Completed);
        let saved = e
            .retry_output(failed.id, &root.path().join("重试"))
            .unwrap();
        assert!(saved.error.is_none());
        assert_eq!(saved.id, failed.id);
        assert!(e.retry_output(saved.id, root.path()).is_err());
    }
    #[test]
    fn batch_children_do_not_auto_export_and_records_filter_literally() {
        let root = tempfile::tempdir().unwrap();
        let mut e = engine(root.path());
        let id = complete(&mut e);
        let mut meta = e.meta(id).unwrap();
        meta.parent_batch_id = Some(Uuid::new_v4());
        e.store.save_meta(&meta).unwrap();
        assert!(e.save_automatically(id, root.path()).unwrap().is_none());
        let path = root.path().join("100%_中文.txt");
        e.export(id, &path).unwrap();
        e.record_export(id, &path, "mcp").unwrap();
        let page = e
            .store
            .output_page(storage::files::OutputQuery {
                search: "%_".into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].source, "mcp");
    }
    #[test]
    fn simultaneous_workers_never_overwrite_or_duplicate_generation() {
        let root = tempfile::tempdir().unwrap();
        let mut e = engine(root.path());
        let first = complete(&mut e);
        let second = complete(&mut e);
        let output = root.path().join("并发");
        std::thread::scope(|scope| {
            let handles: Vec<_> = [first, first, second]
                .into_iter()
                .map(|id| {
                    let root = root.path();
                    let output = &output;
                    scope.spawn(move || {
                        let e = engine(root);
                        e.save_automatically(id, output).unwrap()
                    })
                })
                .collect();
            for handle in handles {
                handle.join().unwrap();
            }
        });
        let page = e.store.output_page(Default::default()).unwrap();
        assert_eq!(page.total, 2);
        assert!(page.items.iter().all(|r| r.error.is_none() && r.exists));
        assert_eq!(fs::read_dir(output).unwrap().count(), 2);
    }
    #[test]
    fn partial_batch_saves_only_completed_outputs_and_manual_save_never_overwrites() {
        let root = tempfile::tempdir().unwrap();
        let mut e = engine(root.path());
        let good = root.path().join("成功.txt");
        fs::write(&good, "13812345678").unwrap();
        let batch = e
            .create_batch(
                vec![good.clone(), root.path().join("不存在.txt")],
                |_, _| {},
            )
            .unwrap();
        let result = e.execute_batch(batch.meta.id, |_, _| {}).unwrap();
        assert_eq!(result.meta.state, TaskState::Partial);
        let record = e
            .save_automatically(batch.meta.id, &root.path().join("输出"))
            .unwrap()
            .unwrap();
        assert!(record.error.is_none(), "{:?}", record.error);
        assert!(record.partial);
        assert_eq!(record.failed_count, 1);
        let archive = zip::ZipArchive::new(fs::File::open(record.path).unwrap()).unwrap();
        assert_eq!(archive.len(), 2); // one result and report
        let child = result
            .items
            .iter()
            .find(|i| i.state == TaskState::Completed)
            .unwrap()
            .task_id
            .unwrap();
        assert!(e.export_recorded(child, &good).is_err());
        assert_eq!(fs::read_to_string(good).unwrap(), "13812345678");
    }
}
