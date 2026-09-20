pub mod batch;
use domain::{
    Entity, EntityDto, Error, ErrorInfo, OcrProfile, PageDto, Point, Policy, PreviewDto, RegionDto,
    RegionSource, Result, Rule, Selection, TaskMeta, TaskOptions, TaskState,
};
use formats::Document;
use recognition::{
    Ner,
    ocr::{Ocr, OcrRun},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use storage::Store;
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
struct Payload {
    document: Document,
    #[serde(default)]
    analysis_text: String,
    entities: Vec<Entity>,
    policies: Vec<Policy>,
    #[serde(default)]
    regions: Vec<RegionDto>,
    #[serde(default)]
    options: TaskOptions,
    #[serde(default)]
    warnings: Vec<String>,
    #[serde(default)]
    revision: u64,
    #[serde(default)]
    page_count: u32,
    #[serde(default)]
    output: Option<Vec<u8>>,
}
#[derive(Clone, Serialize, Deserialize)]
struct ReviewPayload {
    entities: Vec<Entity>,
    regions: Vec<RegionDto>,
    revision: u64,
    #[serde(default)]
    page_count: u32,
}
#[derive(Clone, Serialize)]
pub struct RegionMutationAck {
    pub revision: u64,
    pub region_id: Uuid,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct TaskView {
    pub meta: TaskMeta,
    pub text: String,
    pub entities: Vec<EntityDto>,
    pub preview: Option<String>,
    pub extension: String,
    pub regions: Vec<RegionDto>,
    pub options: TaskOptions,
    pub warnings: Vec<String>,
    pub revision: u64,
}
pub struct Engine {
    pub store: Store,
    pub ner: Option<Box<dyn Ner>>,
    pub ocr_mobile: Option<Box<dyn Ocr>>,
    pub ocr_accurate: Option<Box<dyn Ocr>>,
    pub active_ocr: Arc<Mutex<HashMap<Uuid, OcrRun>>>,
}
struct AnalysisSource {
    display_name: String,
    file_size: u64,
    parent_batch_id: Option<Uuid>,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
impl Engine {
    fn meta(&self, id: Uuid) -> Result<TaskMeta> {
        self.store
            .task(id)?
            .ok_or_else(|| Error::Invalid("任务不存在".into()))
    }
    fn move_to(&self, meta: &mut TaskMeta, state: TaskState) -> Result<()> {
        meta.state = meta.state.transition(state)?;
        meta.updated_at = now();
        if let Some(stored) = self.store.task(meta.id)? {
            meta.storage_bytes = stored.storage_bytes;
            meta.reviewed_revision = meta.reviewed_revision.max(stored.reviewed_revision);
        }
        if !matches!(state, TaskState::Failed | TaskState::Cancelled) {
            meta.error = None;
            meta.error_info = None;
        }
        self.store.save_meta(meta)
    }
    pub fn analyze_text(&mut self, text: String) -> Result<TaskView> {
        let file_size = text.len() as u64;
        self.analyze(
            Document::load("txt", text.into_bytes())?,
            TaskOptions::default(),
            None,
            None,
            AnalysisSource {
                display_name: "粘贴文本".into(),
                file_size,
                parent_batch_id: None,
            },
        )
    }
    pub fn analyze_text_as(&mut self, text: String, id: Uuid, run: OcrRun) -> Result<TaskView> {
        let file_size = text.len() as u64;
        self.analyze(
            Document::load("txt", text.into_bytes())?,
            TaskOptions::default(),
            Some(id),
            Some(run),
            AnalysisSource {
                display_name: "粘贴文本".into(),
                file_size,
                parent_batch_id: None,
            },
        )
    }
    pub fn analyze_file(&mut self, path: &Path) -> Result<TaskView> {
        self.analyze_file_with_options(path, TaskOptions::default())
    }
    pub fn analyze_file_with_options(
        &mut self,
        path: &Path,
        options: TaskOptions,
    ) -> Result<TaskView> {
        let size = path.metadata()?.len();
        if size > 50 * 1024 * 1024 {
            return Err(Error::Invalid("文件超过 50 MB".into()));
        }
        let extension = path
            .extension()
            .and_then(|s| s.to_str())
            .ok_or_else(|| Error::Invalid("文件缺少扩展名".into()))?;
        self.analyze(
            Document::load(extension, std::fs::read(path)?)?,
            options,
            None,
            None,
            AnalysisSource {
                display_name: safe_display_name(path),
                file_size: size,
                parent_batch_id: None,
            },
        )
    }
    pub fn analyze_file_as(
        &mut self,
        path: &Path,
        options: TaskOptions,
        id: Uuid,
        run: OcrRun,
    ) -> Result<TaskView> {
        let size = path.metadata()?.len();
        if size > 50 * 1024 * 1024 {
            return Err(Error::Invalid("文件超过 50 MB".into()));
        }
        let extension = path
            .extension()
            .and_then(|s| s.to_str())
            .ok_or_else(|| Error::Invalid("文件缺少扩展名".into()))?;
        self.analyze(
            Document::load(extension, std::fs::read(path)?)?,
            options,
            Some(id),
            Some(run),
            AnalysisSource {
                display_name: safe_display_name(path),
                file_size: size,
                parent_batch_id: None,
            },
        )
    }
    pub(crate) fn analyze_file_for_batch(
        &mut self,
        path: &Path,
        options: TaskOptions,
        id: Uuid,
        run: OcrRun,
        parent_batch_id: Uuid,
    ) -> Result<TaskView> {
        let size = path.metadata()?.len();
        if size > 50 * 1024 * 1024 {
            return Err(Error::Invalid("文件超过 50 MB".into()));
        }
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .ok_or_else(|| Error::Invalid("文件缺少扩展名".into()))?;
        self.analyze(
            Document::load(extension, std::fs::read(path)?)?,
            options,
            Some(id),
            Some(run),
            AnalysisSource {
                display_name: safe_display_name(path),
                file_size: size,
                parent_batch_id: Some(parent_batch_id),
            },
        )
    }
    fn analyze(
        &mut self,
        document: Document,
        options: TaskOptions,
        id: Option<Uuid>,
        requested_run: Option<OcrRun>,
        source: AnalysisSource,
    ) -> Result<TaskView> {
        if self.ner.is_none() {
            return Err(Error::ModelsNotReady(
                "请在模型管理中安装并加载 RaNER 模型".into(),
            ));
        }
        let mut meta = TaskMeta {
            id: id.unwrap_or_else(Uuid::new_v4),
            kind: document.extension.clone(),
            state: TaskState::Queued,
            created_at: now(),
            updated_at: now(),
            error: None,
            error_info: None,
            display_name: source.display_name,
            file_size: source.file_size,
            parent_batch_id: source.parent_batch_id,
            reviewed_revision: 0,
            storage_bytes: 0,
        };
        self.store.save_meta(&meta)?;
        let run = requested_run.unwrap_or(OcrRun::new()?);
        self.active_ocr
            .lock()
            .map_err(|_| Error::State("OCR 取消状态异常".into()))?
            .insert(meta.id, run.clone());
        self.move_to(&mut meta, TaskState::Analyzing)?;
        let result = (|| {
            if run.is_cancelled() {
                return Err(Error::State("任务已取消".into()));
            }
            let rules = self.store.rules()?;
            let mut analysis_text = document.text.clone();
            let mut regions = Vec::new();
            let pages = document.visual_pages_interruptible(&mut || {
                if run.is_cancelled() {
                    Err(Error::State("任务已取消".into()))
                } else {
                    Ok(())
                }
            })?;
            let mut line_maps = Vec::new();
            if !pages.is_empty() {
                let ocr = match options.ocr_profile {
                    OcrProfile::Mobile => self.ocr_mobile.as_deref_mut(),
                    OcrProfile::Accurate => self.ocr_accurate.as_deref_mut(),
                }
                .ok_or_else(|| Error::ModelsNotReady("请安装并加载所选 PP-OCRv4 模型".into()))?;
                for (page, image) in pages.iter().enumerate() {
                    if run.is_cancelled() {
                        return Err(Error::State("任务已取消".into()));
                    }
                    for line in ocr.recognize(image, &run)? {
                        if !analysis_text.is_empty() && !analysis_text.ends_with('\n') {
                            analysis_text.push('\n');
                        }
                        let start = analysis_text.len();
                        analysis_text.push_str(&line.text);
                        let end = analysis_text.len();
                        regions.push(RegionDto {
                            id: Uuid::new_v4(),
                            page: page as u32,
                            polygon: line.polygon.clone(),
                            entity_id: None,
                            selected: false,
                            source: RegionSource::Ocr,
                            text: line.text.clone(),
                            score: Some(line.score),
                            rotation: line.rotation,
                            replacement: None,
                        });
                        line_maps.push((start, end, page as u32, line));
                    }
                }
            }
            if analysis_text.is_empty() {
                return Err(Error::Invalid("文件中没有可识别文字".into()));
            }
            if run.is_cancelled() {
                return Err(Error::State("任务已取消".into()));
            }
            let entities = recognition::analyze(
                &analysis_text,
                &rules,
                self.ner
                    .as_deref_mut()
                    .ok_or_else(|| Error::ModelsNotReady("RaNER 未加载".into()))?,
            )?;
            if run.is_cancelled() {
                return Err(Error::State("任务已取消".into()));
            }
            let policies = self.store.policies()?;
            for entity in &entities {
                if let Some((start, end, page, line)) =
                    line_maps.iter().find(|(start, end, _, _)| {
                        entity.span.start >= *start && entity.span.end <= *end
                    })
                {
                    let polygon =
                        entity_polygon(&analysis_text[*start..*end], *start, entity, &line.polygon);
                    regions.push(RegionDto {
                        id: Uuid::new_v4(),
                        page: *page,
                        polygon,
                        entity_id: Some(entity.id),
                        selected: entity.selected,
                        source: RegionSource::Entity,
                        text: analysis_text[entity.span.start..entity.span.end].into(),
                        score: Some(entity.score),
                        rotation: line.rotation,
                        replacement: Some(domain::replacement(
                            entity,
                            &analysis_text[entity.span.start..entity.span.end],
                            &policies,
                        )),
                    });
                }
            }
            let mut warnings = Vec::new();
            if document.has_removed_office_signatures() {
                warnings.push("输出时将移除失效的 Office 数字签名".into());
            }
            let payload = Payload {
                document,
                analysis_text,
                entities,
                policies,
                regions,
                options,
                warnings,
                revision: 1,
                page_count: pages.len() as u32,
                output: None,
            };
            self.store.save_payload(meta.id, &payload)?;
            self.store.save_review(
                meta.id,
                &ReviewPayload {
                    entities: payload.entities.clone(),
                    regions: payload.regions.clone(),
                    revision: payload.revision,
                    page_count: payload.page_count,
                },
            )?;
            Ok(())
        })();
        let cancelled = run.is_cancelled();
        if let Err(e) = result {
            let error_info = if cancelled {
                ErrorInfo::cancelled("分析已停止，未生成待复核结果")
            } else {
                ErrorInfo::for_error(&e, "分析失败")
            };
            meta.error = Some(error_info.title.clone());
            meta.error_info = Some(error_info);
            self.move_to(
                &mut meta,
                if cancelled {
                    TaskState::Cancelled
                } else {
                    TaskState::Failed
                },
            )?;
            self.active_ocr
                .lock()
                .map_err(|_| Error::State("OCR 取消状态异常".into()))?
                .remove(&meta.id);
            return Err(e);
        }
        if run.is_cancelled() {
            meta.error = Some("任务已取消".into());
            meta.error_info = Some(ErrorInfo::cancelled("分析完成前收到取消请求"));
            self.move_to(&mut meta, TaskState::Cancelled)?;
            self.active_ocr
                .lock()
                .map_err(|_| Error::State("OCR 取消状态异常".into()))?
                .remove(&meta.id);
            return Err(Error::State("任务已取消".into()));
        }
        self.move_to(&mut meta, TaskState::AwaitingReview)?;
        self.active_ocr
            .lock()
            .map_err(|_| Error::State("OCR 取消状态异常".into()))?
            .remove(&meta.id);
        self.view(meta.id)
    }
    pub fn view(&self, id: Uuid) -> Result<TaskView> {
        let meta = self.meta(id)?;
        let p = self.task_payload(id)?;
        let entities = p
            .entities
            .iter()
            .map(|e| e.dto(payload_text(&p)))
            .collect::<Result<Vec<_>>>()?;
        let preview = if self.task_output(id, &p)?.is_some() {
            Some(domain::redact(payload_text(&p), &p.entities, &p.policies)?)
        } else {
            None
        };
        Ok(TaskView {
            meta,
            text: payload_text(&p).to_owned(),
            entities,
            preview,
            extension: p.document.extension,
            regions: p.regions,
            options: p.options,
            warnings: p.warnings,
            revision: p.revision,
        })
    }
    pub fn select(&self, id: Uuid, selections: Vec<Selection>) -> Result<TaskView> {
        if self.meta(id)?.state != TaskState::AwaitingReview {
            return Err(Error::State("只能复核等待处理的任务".into()));
        }
        let mut payload = self.task_payload(id)?;
        let mut seen = std::collections::HashSet::new();
        for selection in selections {
            if !seen.insert(selection.id) {
                return Err(Error::Invalid("重复的实体 ID".into()));
            }
            if selection
                .replacement
                .as_ref()
                .is_some_and(|s| s.len() > 4096)
            {
                return Err(Error::Invalid("替换内容过长".into()));
            }
            let (entity_id, selected, replacement) = {
                let entity = payload
                    .entities
                    .iter_mut()
                    .find(|e| e.id == selection.id)
                    .ok_or_else(|| Error::Invalid("实体不属于当前任务".into()))?;
                entity.selected = selection.selected;
                entity.replacement = selection.replacement;
                let replacement = domain::replacement(
                    entity,
                    &payload.analysis_text[entity.span.start..entity.span.end],
                    &payload.policies,
                );
                (entity.id, entity.selected, replacement)
            };
            for region in payload
                .regions
                .iter_mut()
                .filter(|r| r.entity_id == Some(entity_id))
            {
                region.selected = selected;
                region.replacement = Some(replacement.clone());
            }
        }
        payload.revision = payload.revision.saturating_add(1);
        self.save_review(id, &payload)?;
        self.mark_reviewed(id, payload.revision)?;
        self.view(id)
    }
    pub fn preview(&self, id: Uuid) -> Result<String> {
        if !matches!(
            self.meta(id)?.state,
            TaskState::AwaitingReview | TaskState::Completed
        ) {
            return Err(Error::State("当前任务不能预览".into()));
        }
        let p = self.task_payload(id)?;
        domain::redact(payload_text(&p), &p.entities, &p.policies)
    }
    pub fn document_preview(&self, id: Uuid) -> Result<PreviewDto> {
        let payload = self.task_payload(id)?;
        self.preview_document(&payload.document, payload.revision, payload.warnings, None)
    }

    pub fn document_result_preview(&self, id: Uuid) -> Result<PreviewDto> {
        if !matches!(
            self.meta(id)?.state,
            TaskState::AwaitingReview | TaskState::Completed
        ) {
            return Err(Error::State("当前任务不能预览结果".into()));
        }
        let payload = self.task_payload(id)?;
        let text_entities = payload
            .entities
            .iter()
            .filter(|entity| entity.span.end <= payload.document.text.len())
            .cloned()
            .collect::<Vec<_>>();
        let output = match self.task_output(id, &payload)? {
            Some(output) => output,
            None => payload.document.render_with_regions(
                &text_entities,
                &payload.policies,
                &payload.regions,
                std::fs::read(r"C:\Windows\Fonts\msyh.ttc").ok(),
                payload.options.pdf_mode,
            )?,
        };
        let result = Document::load(&payload.document.extension, output)?;
        let text = domain::redact(payload_text(&payload), &payload.entities, &payload.policies)?;
        self.preview_document(&result, payload.revision, payload.warnings, Some(text))
    }

    fn preview_document(
        &self,
        document: &Document,
        revision: u64,
        warnings: Vec<String>,
        text: Option<String>,
    ) -> Result<PreviewDto> {
        let visual_pages = document.visual_pages()?;
        let mut pages = Vec::new();
        for (index, page) in visual_pages.iter().enumerate() {
            let scale = (1400.0 / page.width().max(page.height()) as f32).min(1.0);
            let image = if scale < 1.0 {
                image::imageops::resize(
                    page,
                    (page.width() as f32 * scale).round().max(1.0) as u32,
                    (page.height() as f32 * scale).round().max(1.0) as u32,
                    image::imageops::FilterType::Triangle,
                )
            } else {
                page.clone()
            };
            let bytes = formats::raster::encode_pages(
                std::slice::from_ref(&image),
                formats::raster::RasterFormat::Png,
            )?;
            use base64::Engine as _;
            pages.push(PageDto {
                index: index as u32,
                width: image.width(),
                height: image.height(),
                preview_uri: format!(
                    "data:image/png;base64,{}",
                    base64::engine::general_purpose::STANDARD.encode(bytes)
                ),
            });
        }
        Ok(PreviewDto {
            revision,
            pages,
            text: if visual_pages.is_empty() {
                text.or_else(|| Some(document.text.clone()))
            } else {
                None
            },
            warnings,
        })
    }
    pub fn execute(&self, id: Uuid) -> Result<TaskView> {
        self.execute_with_run(id, None)
    }
    pub fn execute_as(&self, id: Uuid, run: OcrRun) -> Result<TaskView> {
        self.execute_with_run(id, Some(run))
    }
    fn execute_with_run(&self, id: Uuid, requested_run: Option<OcrRun>) -> Result<TaskView> {
        let mut meta = self.meta(id)?;
        self.move_to(&mut meta, TaskState::Processing)?;
        let run = requested_run.unwrap_or(OcrRun::new()?);
        self.active_ocr
            .lock()
            .map_err(|_| Error::State("任务取消状态异常".into()))?
            .insert(id, run.clone());
        let result = (|| {
            if run.is_cancelled() {
                return Err(Error::State("任务已取消".into()));
            }
            let p = self.task_payload(id)?;
            let text_entities = p
                .entities
                .iter()
                .filter(|e| e.span.end <= p.document.text.len())
                .cloned()
                .collect::<Vec<_>>();
            let font = std::fs::read(r"C:\Windows\Fonts\msyh.ttc").ok();
            let output = p.document.render_with_regions_interruptible(
                &text_entities,
                &p.policies,
                &p.regions,
                font,
                p.options.pdf_mode,
                &mut || {
                    if run.is_cancelled() {
                        Err(Error::State("任务已取消".into()))
                    } else {
                        Ok(())
                    }
                },
            )?;
            if run.is_cancelled() {
                return Err(Error::State("任务已取消".into()));
            }
            self.store.save_output(id, &output)
        })();
        let cancelled = run.is_cancelled();
        if let Err(e) = result {
            let error_info = if cancelled {
                ErrorInfo::cancelled("生成已停止，未写入可导出结果")
            } else {
                ErrorInfo::for_error(&e, "生成失败")
            };
            meta.error = Some(error_info.title.clone());
            meta.error_info = Some(error_info);
            self.move_to(
                &mut meta,
                if cancelled {
                    TaskState::Cancelled
                } else {
                    TaskState::Failed
                },
            )?;
            self.active_ocr
                .lock()
                .map_err(|_| Error::State("任务取消状态异常".into()))?
                .remove(&id);
            return Err(e);
        }
        if run.is_cancelled() {
            meta.error = Some("任务已取消".into());
            meta.error_info = Some(ErrorInfo::cancelled("生成完成前收到取消请求"));
            self.move_to(&mut meta, TaskState::Cancelled)?;
            self.active_ocr
                .lock()
                .map_err(|_| Error::State("任务取消状态异常".into()))?
                .remove(&id);
            return Err(Error::State("任务已取消".into()));
        }
        self.move_to(&mut meta, TaskState::Completed)?;
        self.active_ocr
            .lock()
            .map_err(|_| Error::State("任务取消状态异常".into()))?
            .remove(&id);
        self.view(id)
    }
    pub fn export(&self, id: Uuid, path: &Path) -> Result<()> {
        if self.meta(id)?.state != TaskState::Completed {
            return Err(Error::State("只能导出已完成任务".into()));
        }
        let p = self.task_payload(id)?;
        let data = self
            .task_output(id, &p)?
            .ok_or_else(|| Error::State("任务没有输出".into()))?;
        storage::atomic_write(path, &data)
    }
    pub fn export_recovery(&self, id: Uuid, password: &str, path: &Path) -> Result<()> {
        if self.meta(id)?.state != TaskState::Completed {
            return Err(Error::State("请先完成脱敏".into()));
        }
        let p = self.task_payload(id)?;
        let encrypted = storage::vault::recovery_encrypt(password, &p.document.original)?;
        let verified = storage::vault::recovery_decrypt(password, &encrypted)?;
        if verified.as_slice() != p.document.original.as_slice() {
            return Err(Error::State("恢复包写入前自检失败".into()));
        }
        storage::atomic_write(path, &encrypted)?;
        let written = std::fs::read(path)?;
        let verified = storage::vault::recovery_decrypt(password, &written)?;
        if verified.as_slice() != p.document.original.as_slice() {
            let _ = std::fs::remove_file(path);
            return Err(Error::State("恢复包写入后自检失败，已删除无效文件".into()));
        }
        Ok(())
    }
    pub fn restore(&self, source: &Path, password: &str, destination: &Path) -> Result<()> {
        if source.metadata()?.len() > 513 * 1024 * 1024 {
            return Err(Error::Invalid("恢复包过大".into()));
        }
        let data = std::fs::read(source)?;
        let plain = storage::vault::recovery_decrypt(password, &data)?;
        storage::atomic_write(destination, &plain)
    }
    pub fn cancel(&self, id: Uuid) -> Result<()> {
        if let Some(run) = self
            .active_ocr
            .lock()
            .map_err(|_| Error::State("OCR 取消状态异常".into()))?
            .get(&id)
        {
            run.cancel()?;
            return Ok(());
        }
        let mut meta = self.meta(id)?;
        meta.error = Some("任务已取消".into());
        meta.error_info = Some(ErrorInfo::cancelled("任务在等待复核时被取消"));
        self.move_to(&mut meta, TaskState::Cancelled)
    }

    pub fn upsert_region(
        &self,
        id: Uuid,
        mut region: RegionDto,
        expected_revision: u64,
    ) -> Result<RegionMutationAck> {
        if self.meta(id)?.state != TaskState::AwaitingReview {
            return Err(Error::State("只能编辑等待复核任务的区域".into()));
        }
        let mut review = self.mutable_review(id, true)?;
        self.ensure_revision(review.revision, expected_revision)?;
        region.validate(review.page_count)?;
        region.source = RegionSource::Manual;
        let region_id = region.id;
        if let Some(existing) = review.regions.iter_mut().find(|item| item.id == region.id) {
            *existing = region;
        } else {
            review.regions.push(region);
        }
        review.revision = review.revision.saturating_add(1);
        self.save_review_payload(id, &review)?;
        self.mark_reviewed(id, review.revision)?;
        Ok(RegionMutationAck {
            revision: review.revision,
            region_id,
        })
    }

    pub fn remove_region(
        &self,
        id: Uuid,
        region_id: Uuid,
        expected_revision: u64,
    ) -> Result<RegionMutationAck> {
        if self.meta(id)?.state != TaskState::AwaitingReview {
            return Err(Error::State("只能编辑等待复核任务的区域".into()));
        }
        let mut review = self.mutable_review(id, false)?;
        self.ensure_revision(review.revision, expected_revision)?;
        let old_len = review.regions.len();
        review
            .regions
            .retain(|region| region.id != region_id || region.source != RegionSource::Manual);
        if old_len == review.regions.len() {
            return Err(Error::Invalid("手工区域不存在".into()));
        }
        review.revision = review.revision.saturating_add(1);
        self.save_review_payload(id, &review)?;
        self.mark_reviewed(id, review.revision)?;
        Ok(RegionMutationAck {
            revision: review.revision,
            region_id,
        })
    }
    pub fn save_rule(&mut self, rule: &Rule) -> Result<()> {
        recognition::compile_rule(rule)?;
        self.store.save_rule(rule)
    }

    fn mark_reviewed(&self, id: Uuid, revision: u64) -> Result<()> {
        let mut meta = self.meta(id)?;
        meta.reviewed_revision = revision;
        meta.updated_at = now();
        self.store.save_meta(&meta)
    }

    fn task_payload(&self, id: Uuid) -> Result<Payload> {
        let mut payload: Payload = self.store.payload(id)?;
        if let Some(review) = self.store.review::<ReviewPayload>(id)? {
            payload.entities = review.entities;
            payload.regions = review.regions;
            payload.revision = review.revision;
            if review.page_count > 0 {
                payload.page_count = review.page_count;
            }
            payload.output = None;
        }
        Ok(payload)
    }

    fn task_output(&self, id: Uuid, payload: &Payload) -> Result<Option<Vec<u8>>> {
        if let Some(output) = self.store.output(id)? {
            return Ok(Some(output));
        }
        if self.store.review::<ReviewPayload>(id)?.is_none() {
            return Ok(payload.output.clone());
        }
        Ok(None)
    }

    fn save_review(&self, id: Uuid, payload: &Payload) -> Result<()> {
        self.store.save_review(
            id,
            &ReviewPayload {
                entities: payload.entities.clone(),
                regions: payload.regions.clone(),
                revision: payload.revision,
                page_count: payload.page_count,
            },
        )?;
        self.store.remove_output(id)
    }

    fn save_review_payload(&self, id: Uuid, review: &ReviewPayload) -> Result<()> {
        self.store.save_review(id, review)?;
        self.store.remove_output(id)
    }

    fn mutable_review(&self, id: Uuid, require_page_count: bool) -> Result<ReviewPayload> {
        let existing = self.store.review::<ReviewPayload>(id)?;
        if let Some(review) = existing.as_ref()
            && (!require_page_count || review.page_count > 0)
        {
            return Ok(review.clone());
        }

        let payload: Payload = self.store.payload(id)?;
        let mut review = existing.unwrap_or_else(|| ReviewPayload {
            entities: payload.entities.clone(),
            regions: payload.regions.clone(),
            revision: payload.revision,
            page_count: 0,
        });
        if require_page_count && review.page_count == 0 {
            review.page_count = if payload.page_count > 0 {
                payload.page_count
            } else {
                let inferred = review
                    .regions
                    .iter()
                    .map(|item| item.page.saturating_add(1))
                    .max()
                    .unwrap_or(0);
                if inferred > 0 {
                    inferred
                } else {
                    payload.document.visual_page_count()?
                }
            };
        }
        Ok(review)
    }

    fn ensure_revision(&self, actual: u64, expected: u64) -> Result<()> {
        if actual != expected {
            return Err(Error::Conflict(format!(
                "预期版本 {expected}，当前版本 {actual}"
            )));
        }
        Ok(())
    }
}

pub(crate) fn safe_display_name(path: &Path) -> String {
    let raw = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("未命名文件");
    let mut name = raw
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                )
            {
                '_'
            } else {
                character
            }
        })
        .take(180)
        .collect::<String>();
    name = name.trim().trim_end_matches(['.', ' ']).to_owned();
    let stem = Path::new(&name)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_uppercase();
    let reserved = matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    );
    if name.is_empty() || name == "." || name == ".." {
        "未命名文件".into()
    } else if reserved {
        format!("_{name}")
    } else {
        name
    }
}

pub(crate) fn safe_extension(path: &Path) -> String {
    path.extension()
        .and_then(|value| value.to_str())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 16
                && value
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric())
        })
        .map(str::to_ascii_lowercase)
        .unwrap_or_default()
}

fn payload_text(payload: &Payload) -> &str {
    if payload.analysis_text.is_empty() {
        &payload.document.text
    } else {
        &payload.analysis_text
    }
}

fn entity_polygon(
    line_text: &str,
    line_start: usize,
    entity: &Entity,
    polygon: &[Point],
) -> Vec<Point> {
    if polygon.len() != 4 || line_text.is_empty() {
        return polygon.to_vec();
    }
    let local_start = entity
        .span
        .start
        .saturating_sub(line_start)
        .min(line_text.len());
    let local_end = entity
        .span
        .end
        .saturating_sub(line_start)
        .min(line_text.len());
    let total = line_text.chars().count().max(1) as f32;
    let start = line_text[..local_start].chars().count() as f32 / total;
    let end = line_text[..local_end].chars().count() as f32 / total;
    let lerp = |a: &Point, b: &Point, t: f32| Point {
        x: a.x + (b.x - a.x) * t,
        y: a.y + (b.y - a.y) * t,
    };
    vec![
        lerp(&polygon[0], &polygon[1], start),
        lerp(&polygon[0], &polygon[1], end),
        lerp(&polygon[3], &polygon[2], end),
        lerp(&polygon[3], &polygon[2], start),
    ]
}
#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};
    struct TestNer;
    impl Ner for TestNer {
        fn analyze(&mut self, text: &str) -> Result<Vec<Entity>> {
            Ok(text
                .find("张三")
                .map(|start| recognition::entity(presidio_result(start), "测试"))
                .into_iter()
                .collect())
        }
    }
    fn presidio_result(start: usize) -> recognition_result::RecognizerResult {
        recognition_result::RecognizerResult::new("PERSON", start, start + 6, 0.99)
    }
    #[test]
    fn missing_model_blocks_all_analysis() {
        let d = tempfile::tempdir().unwrap();
        let mut engine = Engine {
            store: Store::open(d.path(), zeroize::Zeroizing::new([1; 32])).unwrap(),
            ner: None,
            ocr_mobile: None,
            ocr_accurate: None,
            active_ocr: Arc::new(Mutex::new(HashMap::new())),
        };
        assert!(engine.analyze_text("13812345678".into()).is_err());
        assert!(engine.store.tasks().unwrap().is_empty());
    }
    #[test]
    fn review_preview_export_and_restart() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), zeroize::Zeroizing::new([1; 32])).unwrap();
        let mut engine = Engine {
            store,
            ner: Some(Box::new(TestNer)),
            ocr_mobile: None,
            ocr_accurate: None,
            active_ocr: Arc::new(Mutex::new(HashMap::new())),
        };
        let task = engine
            .analyze_text("😀张三，13812345678\r\n".into())
            .unwrap();
        let id = task.meta.id;
        let entity_id = task.entities[0].id;
        let analysis_path = engine.store.task_dir(id).join("analysis.enc");
        let analysis_before_review = std::fs::read(&analysis_path).unwrap();
        assert_eq!(task.meta.display_name, "粘贴文本");
        assert_eq!(task.meta.file_size, 26);
        assert!(task.meta.storage_bytes > 0);
        assert_eq!(task.entities[0].display, domain::Span { start: 2, end: 4 });
        assert_eq!(engine.preview(id).unwrap(), "😀某人，138****5678\r\n");
        assert_eq!(
            engine.document_result_preview(id).unwrap().text.as_deref(),
            Some("😀某人，138****5678\r\n")
        );
        assert!(
            engine
                .select(
                    id,
                    vec![Selection {
                        id: Uuid::new_v4(),
                        selected: true,
                        replacement: None
                    }]
                )
                .is_err()
        );
        engine
            .select(
                id,
                vec![Selection {
                    id: entity_id,
                    selected: true,
                    replacement: None,
                }],
            )
            .unwrap();
        assert_eq!(
            std::fs::read(&analysis_path).unwrap(),
            analysis_before_review
        );
        engine.execute(id).unwrap();
        assert!(engine.store.task_dir(id).join("output.enc").is_file());
        assert_eq!(
            std::fs::read(&analysis_path).unwrap(),
            analysis_before_review
        );
        let output = dir.path().join("输出.txt");
        engine.export(id, &output).unwrap();
        assert_eq!(
            std::fs::read_to_string(output).unwrap(),
            "😀某人，138****5678\r\n"
        );
        let recovery = dir.path().join("恢复包.ldsrec");
        engine
            .export_recovery(id, "test-password", &recovery)
            .unwrap();
        let restored = dir.path().join("恢复.txt");
        engine
            .restore(&recovery, "test-password", &restored)
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(restored).unwrap(),
            "😀张三，13812345678\r\n"
        );
        drop(engine);
        let engine = Engine {
            store: Store::open(dir.path(), zeroize::Zeroizing::new([1; 32])).unwrap(),
            ner: None,
            ocr_mobile: None,
            ocr_accurate: None,
            active_ocr: Arc::new(Mutex::new(HashMap::new())),
        };
        assert_eq!(engine.view(id).unwrap().meta.state, TaskState::Completed);
        assert!(engine.execute(id).is_err());
    }

    #[test]
    fn queued_analysis_and_processing_can_be_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = Engine {
            store: Store::open(dir.path(), zeroize::Zeroizing::new([9; 32])).unwrap(),
            ner: Some(Box::new(TestNer)),
            ocr_mobile: None,
            ocr_accurate: None,
            active_ocr: Arc::new(Mutex::new(HashMap::new())),
        };
        let queued_id = Uuid::new_v4();
        let queued_run = OcrRun::new().unwrap();
        queued_run.cancel().unwrap();
        assert!(
            engine
                .analyze_text_as("张三".into(), queued_id, queued_run)
                .is_err()
        );
        assert_eq!(engine.meta(queued_id).unwrap().state, TaskState::Cancelled);
        assert_eq!(
            engine.meta(queued_id).unwrap().error_info.unwrap().code,
            "TASK_CANCELLED"
        );

        let task = engine.analyze_text("张三".into()).unwrap();
        let processing_run = OcrRun::new().unwrap();
        processing_run.cancel().unwrap();
        assert!(engine.execute_as(task.meta.id, processing_run).is_err());
        assert_eq!(
            engine.meta(task.meta.id).unwrap().state,
            TaskState::Cancelled
        );
    }

    #[test]
    fn display_names_are_safe_for_history_and_archives() {
        assert_eq!(safe_display_name(Path::new("CON.txt")), "_CON.txt");
        assert_eq!(safe_display_name(Path::new("folder/输入.txt")), "输入.txt");
        assert_eq!(safe_extension(Path::new("输入.XLSM")), "xlsm");
        assert_eq!(safe_extension(Path::new("输入.bad-extension")), "");
    }

    #[test]
    fn region_edits_only_update_review_and_survive_restart() {
        let dir = tempfile::tempdir().unwrap();
        let key = zeroize::Zeroizing::new([13; 32]);
        let store = Store::open(dir.path(), key).unwrap();
        let id = Uuid::new_v4();
        let meta = TaskMeta {
            id,
            kind: "png".into(),
            state: TaskState::AwaitingReview,
            created_at: 1,
            updated_at: 1,
            error: None,
            error_info: None,
            display_name: "测试.png".into(),
            file_size: 4,
            parent_batch_id: None,
            reviewed_revision: 0,
            storage_bytes: 0,
        };
        store.save_meta(&meta).unwrap();
        let page = RgbaImage::from_pixel(16, 16, Rgba([255, 255, 255, 255]));
        let png = formats::raster::encode_pages(
            std::slice::from_ref(&page),
            formats::raster::RasterFormat::Png,
        )
        .unwrap();
        let mut document = Document::load("png", png).unwrap();
        // Region validation must use persisted metadata, not decode/render the original again.
        document.original = b"not-an-image-anymore".to_vec();
        store
            .save_payload(
                id,
                &Payload {
                    document,
                    analysis_text: "测试".into(),
                    entities: vec![],
                    policies: vec![],
                    regions: vec![],
                    options: TaskOptions::default(),
                    warnings: vec![],
                    revision: 1,
                    page_count: 1,
                    output: None,
                },
            )
            .unwrap();
        let analysis_path = store.task_dir(id).join("analysis.enc");
        let analysis_before = std::fs::read(&analysis_path).unwrap();
        let engine = Engine {
            store,
            ner: None,
            ocr_mobile: None,
            ocr_accurate: None,
            active_ocr: Arc::new(Mutex::new(HashMap::new())),
        };
        let region_id = Uuid::new_v4();
        let region = RegionDto {
            id: region_id,
            page: 0,
            polygon: vec![
                Point { x: 0.1, y: 0.1 },
                Point { x: 0.4, y: 0.1 },
                Point { x: 0.4, y: 0.3 },
                Point { x: 0.1, y: 0.3 },
            ],
            entity_id: None,
            selected: true,
            source: RegionSource::Manual,
            text: "".into(),
            score: None,
            rotation: 0.0,
            replacement: Some("已脱敏".into()),
        };
        let ack = engine.upsert_region(id, region, 1).unwrap();
        assert_eq!(ack.revision, 2);
        assert_eq!(ack.region_id, region_id);
        assert_eq!(std::fs::read(&analysis_path).unwrap(), analysis_before);
        assert!(engine.store.task_dir(id).join("review.enc").is_file());
        assert!(matches!(
            engine.remove_region(id, region_id, 1),
            Err(Error::Conflict(_))
        ));
        std::fs::write(&analysis_path, b"intentionally unreadable analysis").unwrap();
        drop(engine);

        let engine = Engine {
            store: Store::open(dir.path(), zeroize::Zeroizing::new([13; 32])).unwrap(),
            ner: None,
            ocr_mobile: None,
            ocr_accurate: None,
            active_ocr: Arc::new(Mutex::new(HashMap::new())),
        };
        let ack = engine.remove_region(id, region_id, 2).unwrap();
        assert_eq!(ack.revision, 3);
        let replacement_region_id = Uuid::new_v4();
        let replacement_region = RegionDto {
            id: replacement_region_id,
            page: 0,
            polygon: vec![
                Point { x: 0.2, y: 0.2 },
                Point { x: 0.5, y: 0.2 },
                Point { x: 0.5, y: 0.4 },
                Point { x: 0.2, y: 0.4 },
            ],
            entity_id: None,
            selected: true,
            source: RegionSource::Manual,
            text: "".into(),
            score: None,
            rotation: 0.0,
            replacement: Some("已脱敏".into()),
        };
        let ack = engine
            .upsert_region(id, replacement_region, ack.revision)
            .unwrap();
        assert_eq!(ack.revision, 4);
        assert_eq!(ack.region_id, replacement_region_id);
        std::fs::write(&analysis_path, &analysis_before).unwrap();
        let view = engine.view(id).unwrap();
        assert_eq!(view.revision, 4);
        assert_eq!(view.regions.len(), 1);
        assert_eq!(std::fs::read(&analysis_path).unwrap(), analysis_before);
    }
}
