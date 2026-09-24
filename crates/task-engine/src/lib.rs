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
    #[serde(default)]
    last_mutation: Option<Uuid>,
    #[serde(default)]
    last_region_mutation: Option<RegionMutationReceipt>,
}
#[derive(Clone, Serialize, Deserialize)]
struct RegionMutationReceipt {
    mutation_id: Uuid,
    expected_revision: u64,
    fingerprint: String,
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
    #[serde(default)]
    pub review_confirmed: bool,
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
            meta.reviewed_revision = stored.reviewed_revision;
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
        // Keep the encrypted source even when analysis is cancelled or fails.
        self.store.save_payload(
            meta.id,
            &Payload {
                document: document.clone(),
                analysis_text: document.text.clone(),
                entities: Vec::new(),
                policies: Vec::new(),
                regions: Vec::new(),
                options: options.clone(),
                warnings: Vec::new(),
                revision: 1,
                page_count: 0,
                output: None,
            },
        )?;
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
            let page_count = document.visual_page_count()?;
            let mut line_maps = Vec::new();
            if page_count > 0 {
                let ocr = match options.ocr_profile {
                    OcrProfile::Mobile => self.ocr_mobile.as_deref_mut(),
                    OcrProfile::Accurate => self.ocr_accurate.as_deref_mut(),
                }
                .ok_or_else(|| Error::ModelsNotReady("请安装并加载所选 PP-OCRv4 模型".into()))?;
                document.visit_visual_pages(&mut |page, image| {
                    if run.is_cancelled() {
                        return Err(Error::State("任务已取消".into()));
                    }
                    for line in ocr.recognize(&image, &run)? {
                        if !analysis_text.is_empty() && !analysis_text.ends_with('\n') {
                            analysis_text.push('\n');
                        }
                        let start = analysis_text.len();
                        analysis_text.push_str(&line.text);
                        let end = analysis_text.len();
                        regions.push(RegionDto {
                            id: Uuid::new_v4(),
                            page,
                            polygon: line.polygon.clone(),
                            entity_id: None,
                            selected: false,
                            source: RegionSource::Ocr,
                            text: line.text.clone(),
                            score: Some(line.score),
                            rotation: line.rotation,
                            replacement: None,
                        });
                        line_maps.push((start, end, page, line));
                    }
                    Ok(())
                })?;
            }
            if analysis_text.is_empty() && page_count == 0 {
                return Err(Error::Invalid("文件中没有可识别文字".into()));
            }
            if run.is_cancelled() {
                return Err(Error::State("任务已取消".into()));
            }
            let entities = if analysis_text.is_empty() {
                Vec::new()
            } else {
                recognition::analyze_with_run(
                    &analysis_text,
                    &rules,
                    self.ner
                        .as_deref_mut()
                        .ok_or_else(|| Error::ModelsNotReady("RaNER 未加载".into()))?,
                    &run,
                )?
            };
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
            if analysis_text.is_empty() && page_count > 0 {
                warnings.push("未识别到文字，可在预览中手工框选需要脱敏的区域".into());
            }
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
                page_count,
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
                    last_mutation: None,
                    last_region_mutation: None,
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
            .map(|e| {
                let mut dto = e.dto(payload_text(&p))?;
                dto.effective_replacement = domain::replacement(e, &dto.text, &p.policies);
                Ok(dto)
            })
            .collect::<Result<Vec<_>>>()?;
        // Reading a task must not decrypt the entire generated PDF just to
        // decide whether its compact text preview is available.
        let preview = if meta.state == TaskState::Completed {
            Some(domain::redact(payload_text(&p), &p.entities, &p.policies)?)
        } else {
            None
        };
        Ok(TaskView {
            review_confirmed: meta.reviewed_revision > 0 && meta.reviewed_revision == p.revision,
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
        let revision = self.task_payload(id)?.revision;
        self.review_patch(id, selections, revision, Uuid::new_v4())
    }

    /// Caller serializes all mutations for this task, including region edits.
    pub fn review_patch(
        &self,
        id: Uuid,
        selections: Vec<Selection>,
        expected_revision: u64,
        mutation_id: Uuid,
    ) -> Result<TaskView> {
        if self.meta(id)?.state != TaskState::AwaitingReview {
            return Err(Error::State("只能复核等待处理的任务".into()));
        }
        if self
            .store
            .review::<ReviewPayload>(id)?
            .is_some_and(|review| review.last_mutation == Some(mutation_id))
        {
            return self.view(id);
        }
        let mut payload = self.task_payload(id)?;
        self.ensure_revision(payload.revision, expected_revision)?;
        let analysis_text = payload_text(&payload).to_owned();
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
                    &analysis_text[entity.span.start..entity.span.end],
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
        self.save_review_payload(
            id,
            &ReviewPayload {
                entities: payload.entities,
                regions: payload.regions,
                revision: payload.revision,
                page_count: payload.page_count,
                last_mutation: Some(mutation_id),
                last_region_mutation: None,
            },
        )?;
        self.mark_reviewed(id, 0)?;
        self.view(id)
    }

    pub fn confirm_review(&self, id: Uuid, expected_revision: u64) -> Result<TaskView> {
        if self.meta(id)?.state != TaskState::AwaitingReview {
            return Err(Error::State("只能确认等待处理的任务".into()));
        }
        self.ensure_revision(self.task_payload(id)?.revision, expected_revision)?;
        self.mark_reviewed(id, expected_revision)?;
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
        let (document, revision, warnings) = self.preview_source(id, true)?;
        self.preview_document(&document, revision, warnings, None)
    }

    fn preview_source(&self, id: Uuid, result: bool) -> Result<(Document, u64, Vec<String>)> {
        let payload = self.task_payload(id)?;
        if result {
            if self.meta(id)?.state != TaskState::Completed {
                return Err(Error::State("请先生成脱敏文件".into()));
            }
            let output = self
                .task_output(id, &payload)?
                .ok_or_else(|| Error::State("任务没有输出".into()))?;
            Ok((
                Document::load(&payload.document.extension, output)?,
                payload.revision,
                payload.warnings,
            ))
        } else {
            Ok((payload.document, payload.revision, payload.warnings))
        }
    }

    pub fn document_manifest(&self, id: Uuid, result: bool) -> Result<PreviewDto> {
        let (document, revision, warnings) = self.preview_source(id, result)?;
        let kind = document.preview_kind();
        let pages = document
            .visual_dimensions()?
            .into_iter()
            .enumerate()
            .map(|(index, (width, height))| PageDto {
                index: index as u32,
                width,
                height,
                preview_uri: String::new(),
            })
            .collect::<Vec<_>>();
        let text = if pages.is_empty()
            || matches!(document.extension.as_str(), "docx" | "xlsx" | "xlsm")
        {
            Some(document.text)
        } else {
            None
        };
        Ok(PreviewDto {
            kind,
            revision,
            pages,
            text,
            warnings,
        })
    }

    pub fn document_page(
        &self,
        id: Uuid,
        result: bool,
        page: u32,
        max_dimension: u32,
        expected_revision: u64,
    ) -> Result<Vec<u8>> {
        if !(256..=2400).contains(&max_dimension) {
            return Err(Error::Invalid("预览尺寸必须在 256 到 2400 之间".into()));
        }
        let (document, revision, _) = self.preview_source(id, result)?;
        // Source images are immutable; only result pages depend on review revision.
        if result {
            self.ensure_revision(revision, expected_revision)?;
        }
        let image = document.visual_page(page, Some(max_dimension))?;
        formats::raster::encode_pages(
            std::slice::from_ref(&image),
            formats::raster::RasterFormat::Png,
        )
    }

    pub fn office_preview(
        &self,
        id: Uuid,
        result: bool,
        expected_revision: u64,
    ) -> Result<domain::OfficePreview> {
        let (document, revision, warnings) = self.preview_source(id, result)?;
        self.ensure_revision(revision, expected_revision)?;
        let mut metadata = document.office_presentation(revision, false)?.metadata;
        metadata.warnings.extend(warnings);
        self.ensure_revision(self.mutable_review(id, false)?.revision, expected_revision)?;
        Ok(metadata)
    }

    pub fn office_preview_docx(
        &self,
        id: Uuid,
        result: bool,
        expected_revision: u64,
    ) -> Result<Vec<u8>> {
        let (document, revision, _) = self.preview_source(id, result)?;
        self.ensure_revision(revision, expected_revision)?;
        let presentation = document.office_presentation(revision, true)?;
        self.ensure_revision(self.mutable_review(id, false)?.revision, expected_revision)?;
        presentation.docx.ok_or_else(|| {
            Error::Unsupported(
                presentation
                    .metadata
                    .reason
                    .unwrap_or_else(|| "该文档仅支持内容预览".into()),
            )
        })
    }

    pub fn draft_page(
        &self,
        id: Uuid,
        page: u32,
        max_dimension: u32,
        expected_revision: u64,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Vec<u8>> {
        check()?;
        if self.meta(id)?.state != TaskState::AwaitingReview {
            return Err(Error::State("只能预览等待复核任务的修改".into()));
        }
        if !(256..=2400).contains(&max_dimension) {
            return Err(Error::Invalid("预览尺寸必须在 256 到 2400 之间".into()));
        }
        let payload = self.task_payload(id)?;
        self.ensure_revision(payload.revision, expected_revision)?;
        let image = payload.document.draft_page(
            page,
            max_dimension,
            &payload.regions,
            payload.options.pdf_mode,
            check,
        )?;
        check()?;
        self.ensure_revision(self.mutable_review(id, false)?.revision, expected_revision)?;
        let output = formats::raster::encode_pages(
            std::slice::from_ref(&image),
            formats::raster::RasterFormat::Png,
        )?;
        check()?;
        self.ensure_revision(self.mutable_review(id, false)?.revision, expected_revision)?;
        Ok(output)
    }

    pub fn clone_for_review(&self, id: Uuid) -> Result<TaskView> {
        let source = self.meta(id)?;
        if matches!(
            source.state,
            TaskState::Queued | TaskState::Analyzing | TaskState::Processing
        ) {
            return Err(Error::State("任务运行时不能创建复核副本".into()));
        }
        let mut payload = self.task_payload(id)?;
        payload.output = None;
        payload.revision = 1;
        let mut meta = source;
        meta.id = Uuid::new_v4();
        meta.state = TaskState::AwaitingReview;
        meta.created_at = now();
        meta.updated_at = meta.created_at;
        meta.reviewed_revision = 0;
        meta.storage_bytes = 0;
        meta.parent_batch_id = None;
        meta.error = None;
        meta.error_info = None;
        self.store.save_meta(&meta)?;
        self.store.save_payload(meta.id, &payload)?;
        self.save_review(meta.id, &payload)?;
        self.view(meta.id)
    }

    pub fn retry_saved_as(&mut self, id: Uuid, new_id: Uuid, run: OcrRun) -> Result<TaskView> {
        let source = self.meta(id)?;
        let payload = self.task_payload(id)?;
        self.analyze(
            payload.document,
            payload.options,
            Some(new_id),
            Some(run),
            AnalysisSource {
                display_name: source.display_name,
                file_size: source.file_size,
                parent_batch_id: source.parent_batch_id,
            },
        )
    }

    fn preview_document(
        &self,
        document: &Document,
        revision: u64,
        warnings: Vec<String>,
        text: Option<String>,
    ) -> Result<PreviewDto> {
        let mut pages = Vec::new();
        let page_count = document.visual_page_count()?;
        for index in 0..page_count {
            let image = document.visual_page(index, Some(1400))?;
            let bytes = formats::raster::encode_pages(
                std::slice::from_ref(&image),
                formats::raster::RasterFormat::Png,
            )?;
            use base64::Engine as _;
            pages.push(PageDto {
                index,
                width: image.width(),
                height: image.height(),
                preview_uri: format!(
                    "data:image/png;base64,{}",
                    base64::engine::general_purpose::STANDARD.encode(bytes)
                ),
            });
        }
        Ok(PreviewDto {
            kind: document.preview_kind(),
            revision,
            pages,
            text: if page_count == 0 {
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
            let output = p.document.render_with_regions_interruptible(
                &text_entities,
                &p.policies,
                &p.regions,
                None,
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
        region: RegionDto,
        expected_revision: u64,
    ) -> Result<RegionMutationAck> {
        self.upsert_region_with_mutation(id, region, expected_revision, None)
    }

    pub fn upsert_region_with_mutation(
        &self,
        id: Uuid,
        mut region: RegionDto,
        expected_revision: u64,
        mutation_id: Option<Uuid>,
    ) -> Result<RegionMutationAck> {
        if self.meta(id)?.state != TaskState::AwaitingReview {
            return Err(Error::State("只能编辑等待复核任务的区域".into()));
        }
        let mut review = self.mutable_review(id, true)?;
        region.source = RegionSource::Manual;
        let region_id = region.id;
        let fingerprint = region_fingerprint("upsert", &region)?;
        if let Some(ack) = self.region_retry(
            &review,
            mutation_id,
            expected_revision,
            &fingerprint,
            region_id,
        )? {
            return Ok(ack);
        }
        self.ensure_revision(review.revision, expected_revision)?;
        region.validate(review.page_count)?;
        if let Some(existing) = review.regions.iter_mut().find(|item| item.id == region.id) {
            *existing = region;
        } else {
            review.regions.push(region);
        }
        review.revision = review.revision.saturating_add(1);
        review.last_mutation = None;
        review.last_region_mutation = mutation_id.map(|mutation_id| RegionMutationReceipt {
            mutation_id,
            expected_revision,
            fingerprint,
        });
        self.save_review_payload(id, &review)?;
        self.mark_reviewed(id, 0)?;
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
        self.remove_region_with_mutation(id, region_id, expected_revision, None)
    }

    pub fn remove_region_with_mutation(
        &self,
        id: Uuid,
        region_id: Uuid,
        expected_revision: u64,
        mutation_id: Option<Uuid>,
    ) -> Result<RegionMutationAck> {
        if self.meta(id)?.state != TaskState::AwaitingReview {
            return Err(Error::State("只能编辑等待复核任务的区域".into()));
        }
        let mut review = self.mutable_review(id, false)?;
        let fingerprint = region_fingerprint("remove", &region_id)?;
        if let Some(ack) = self.region_retry(
            &review,
            mutation_id,
            expected_revision,
            &fingerprint,
            region_id,
        )? {
            return Ok(ack);
        }
        self.ensure_revision(review.revision, expected_revision)?;
        let old_len = review.regions.len();
        review
            .regions
            .retain(|region| region.id != region_id || region.source != RegionSource::Manual);
        if old_len == review.regions.len() {
            return Err(Error::Invalid("手工区域不存在".into()));
        }
        review.revision = review.revision.saturating_add(1);
        review.last_mutation = None;
        review.last_region_mutation = mutation_id.map(|mutation_id| RegionMutationReceipt {
            mutation_id,
            expected_revision,
            fingerprint,
        });
        self.save_review_payload(id, &review)?;
        self.mark_reviewed(id, 0)?;
        Ok(RegionMutationAck {
            revision: review.revision,
            region_id,
        })
    }
    pub fn save_rule(&mut self, rule: &Rule) -> Result<()> {
        recognition::compile_rule(rule)?;
        self.store.save_rule(rule)
    }

    fn region_retry(
        &self,
        review: &ReviewPayload,
        mutation_id: Option<Uuid>,
        expected_revision: u64,
        fingerprint: &str,
        region_id: Uuid,
    ) -> Result<Option<RegionMutationAck>> {
        if let Some(receipt) = &review.last_region_mutation
            && Some(receipt.mutation_id) == mutation_id
        {
            if receipt.expected_revision != expected_revision || receipt.fingerprint != fingerprint
            {
                return Err(Error::Conflict(
                    "操作标识已用于不同的区域修改，请重新读取任务".into(),
                ));
            }
            return Ok(Some(RegionMutationAck {
                revision: review.revision,
                region_id,
            }));
        }
        Ok(None)
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
                last_mutation: None,
                last_region_mutation: None,
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
            last_mutation: None,
            last_region_mutation: None,
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

fn region_fingerprint(operation: &str, request: &impl Serialize) -> Result<String> {
    use sha2::{Digest, Sha256};
    let bytes =
        serde_json::to_vec(&(operation, request)).map_err(|e| Error::Invalid(e.to_string()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
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
        assert!(engine.document_result_preview(id).is_err());
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
        assert_eq!(
            engine.document_result_preview(id).unwrap().text.as_deref(),
            Some("😀某人，138****5678\r\n")
        );
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
    fn review_confirmation_is_explicit_and_stale_patches_cannot_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = Engine {
            store: Store::open(dir.path(), zeroize::Zeroizing::new([17; 32])).unwrap(),
            ner: Some(Box::new(TestNer)),
            ocr_mobile: None,
            ocr_accurate: None,
            active_ocr: Arc::new(Mutex::new(HashMap::new())),
        };
        let task = engine.analyze_text("张三".into()).unwrap();
        assert!(!task.review_confirmed);
        assert_eq!(task.entities[0].effective_replacement, "某人");
        assert!(
            engine
                .confirm_review(task.meta.id, task.revision)
                .unwrap()
                .review_confirmed
        );
        let selections = vec![Selection {
            id: task.entities[0].id,
            selected: true,
            replacement: Some("访客".into()),
        }];
        let mutation_id = Uuid::new_v4();
        let updated = engine
            .review_patch(task.meta.id, selections.clone(), task.revision, mutation_id)
            .unwrap();
        assert_eq!(
            engine
                .review_patch(task.meta.id, selections.clone(), task.revision, mutation_id)
                .unwrap()
                .revision,
            updated.revision,
            "a lost acknowledgement can be retried safely"
        );
        assert!(!updated.review_confirmed);
        assert_eq!(updated.entities[0].effective_replacement, "访客");
        assert!(matches!(
            engine.review_patch(task.meta.id, selections, task.revision, Uuid::new_v4()),
            Err(Error::Conflict(_))
        ));
        assert!(matches!(
            engine.confirm_review(task.meta.id, task.revision),
            Err(Error::Conflict(_))
        ));
        assert_eq!(
            engine.view(task.meta.id).unwrap().revision,
            updated.revision
        );
        engine.execute(task.meta.id).unwrap();
        let copy = engine.clone_for_review(task.meta.id).unwrap();
        assert_ne!(copy.meta.id, task.meta.id);
        assert_eq!(copy.entities[0].effective_replacement, "访客");
        assert!(!copy.review_confirmed);
        assert_eq!(
            engine.view(task.meta.id).unwrap().meta.state,
            TaskState::Completed
        );
        assert!(copy.preview.is_none());
    }

    #[test]
    fn cancelled_analysis_can_retry_from_encrypted_source() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = Engine {
            store: Store::open(dir.path(), zeroize::Zeroizing::new([18; 32])).unwrap(),
            ner: Some(Box::new(TestNer)),
            ocr_mobile: None,
            ocr_accurate: None,
            active_ocr: Arc::new(Mutex::new(HashMap::new())),
        };
        let id = Uuid::new_v4();
        let run = OcrRun::new().unwrap();
        run.cancel().unwrap();
        assert!(engine.analyze_text_as("张三".into(), id, run).is_err());
        let retried = engine
            .retry_saved_as(id, Uuid::new_v4(), OcrRun::new().unwrap())
            .unwrap();
        assert_eq!(retried.text, "张三");
        assert_eq!(engine.meta(id).unwrap().state, TaskState::Cancelled);
        assert_eq!(retried.meta.state, TaskState::AwaitingReview);
    }

    #[test]
    fn text_manifest_uses_generated_output_and_revision() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = Engine {
            store: Store::open(dir.path(), zeroize::Zeroizing::new([19; 32])).unwrap(),
            ner: Some(Box::new(TestNer)),
            ocr_mobile: None,
            ocr_accurate: None,
            active_ocr: Arc::new(Mutex::new(HashMap::new())),
        };
        let task = engine.analyze_text("张三".into()).unwrap();
        assert_eq!(
            engine
                .document_manifest(task.meta.id, false)
                .unwrap()
                .text
                .as_deref(),
            Some("张三")
        );
        assert!(engine.document_manifest(task.meta.id, true).is_err());
        engine.execute(task.meta.id).unwrap();
        assert_eq!(
            engine
                .document_manifest(task.meta.id, true)
                .unwrap()
                .text
                .as_deref(),
            Some("某人")
        );
    }

    #[test]
    fn image_manifest_and_page_preview_do_not_expose_source_on_result_request() {
        struct TestOcr;
        impl Ocr for TestOcr {
            fn recognize(
                &mut self,
                _: &RgbaImage,
                _: &OcrRun,
            ) -> Result<Vec<recognition::ocr::OcrLine>> {
                Ok(vec![recognition::ocr::OcrLine {
                    text: "张三".into(),
                    score: 0.99,
                    polygon: vec![
                        Point { x: 0.1, y: 0.1 },
                        Point { x: 0.8, y: 0.1 },
                        Point { x: 0.8, y: 0.6 },
                        Point { x: 0.1, y: 0.6 },
                    ],
                    rotation: 0.0,
                }])
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let mut engine = Engine {
            store: Store::open(dir.path(), zeroize::Zeroizing::new([20; 32])).unwrap(),
            ner: Some(Box::new(TestNer)),
            ocr_mobile: Some(Box::new(TestOcr)),
            ocr_accurate: None,
            active_ocr: Arc::new(Mutex::new(HashMap::new())),
        };
        let source = dir.path().join("图片.png");
        let image = RgbaImage::from_pixel(800, 400, Rgba([255, 255, 255, 255]));
        std::fs::write(
            &source,
            formats::raster::encode_pages(&[image], formats::raster::RasterFormat::Png).unwrap(),
        )
        .unwrap();
        let task = engine.analyze_file(&source).unwrap();
        let manifest = engine.document_manifest(task.meta.id, false).unwrap();
        assert_eq!(manifest.pages.len(), 1);
        assert_eq!(
            (manifest.pages[0].width, manifest.pages[0].height),
            (800, 400)
        );
        assert!(manifest.pages[0].preview_uri.is_empty());
        assert!(manifest.text.is_none());
        assert!(
            engine
                .document_page(task.meta.id, true, 0, 512, task.revision)
                .is_err()
        );
        let source_preview = engine
            .document_page(task.meta.id, false, 0, 512, task.revision)
            .unwrap();
        assert_eq!(
            image::load_from_memory(&source_preview).unwrap().width(),
            512
        );
        let analysis_before =
            std::fs::read(engine.store.task_dir(task.meta.id).join("analysis.enc")).unwrap();
        let concurrent = engine.clone_for_review(task.meta.id).unwrap();
        let mut checkpoints = 0;
        let outdated =
            engine.draft_page(concurrent.meta.id, 0, 512, concurrent.revision, &mut || {
                checkpoints += 1;
                if checkpoints == 2 {
                    engine.select(
                        concurrent.meta.id,
                        vec![Selection {
                            id: concurrent.entities[0].id,
                            selected: false,
                            replacement: None,
                        }],
                    )?;
                }
                Ok(())
            });
        assert!(
            matches!(outdated, Err(Error::Conflict(_))),
            "an edit during rendering invalidates the whole draft response"
        );
        assert!(matches!(
            engine.draft_page(task.meta.id, 0, 512, task.revision + 1, &mut || Ok(())),
            Err(Error::Conflict(_))
        ));
        assert!(
            engine
                .draft_page(task.meta.id, 0, 512, task.revision, &mut || Err(
                    Error::State("cancelled".into())
                ))
                .is_err()
        );
        let draft = engine
            .draft_page(task.meta.id, 0, 512, task.revision, &mut || Ok(()))
            .unwrap();
        assert_eq!(
            engine.view(task.meta.id).unwrap().meta.state,
            TaskState::AwaitingReview
        );
        assert!(
            !engine
                .store
                .task_dir(task.meta.id)
                .join("output.enc")
                .exists()
        );
        assert_eq!(
            std::fs::read(engine.store.task_dir(task.meta.id).join("analysis.enc")).unwrap(),
            analysis_before
        );
        engine.execute(task.meta.id).unwrap();
        assert!(matches!(
            engine.document_page(task.meta.id, true, 0, 512, task.revision + 1),
            Err(Error::Conflict(_))
        ));
        let result = engine
            .document_page(task.meta.id, true, 0, 512, task.revision)
            .unwrap();
        assert_eq!(draft, result);
        assert_ne!(
            result, source_preview,
            "Chinese replacement must render even without installed fonts"
        );
        assert!(
            engine
                .document_page(task.meta.id, false, 1, 512, task.revision)
                .is_err()
        );
    }

    #[test]
    fn office_preview_separates_source_draft_and_actual_output_offsets() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let mut engine = Engine {
            store: Store::open(dir.path(), zeroize::Zeroizing::new([25; 32])).unwrap(),
            ner: Some(Box::new(TestNer)),
            ocr_mobile: None,
            ocr_accurate: None,
            active_ocr: Arc::new(Mutex::new(HashMap::new())),
        };
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, xml) in [
            ("[Content_Types].xml", "<Types/>"),
            (
                "word/document.xml",
                "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body><w:p><w:r><w:t>😀张</w:t></w:r><w:r><w:t>三</w:t></w:r></w:p></w:body></w:document>",
            ),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(xml.as_bytes()).unwrap();
        }
        let source = dir.path().join("简历.docx");
        std::fs::write(&source, zip.finish().unwrap().into_inner()).unwrap();
        let task = engine.analyze_file(&source).unwrap();
        assert_eq!(
            engine.document_manifest(task.meta.id, false).unwrap().kind,
            domain::PreviewKind::Docx
        );
        let first = engine
            .office_preview(task.meta.id, false, task.revision)
            .unwrap();
        assert!(first.layout_available);
        let original_docx = engine
            .office_preview_docx(task.meta.id, false, task.revision)
            .unwrap();
        assert!(
            engine
                .office_preview(task.meta.id, true, task.revision)
                .is_err()
        );
        assert!(matches!(
            engine.office_preview(task.meta.id, false, task.revision + 1),
            Err(Error::Conflict(_))
        ));
        let updated = engine
            .select(
                task.meta.id,
                vec![Selection {
                    id: task.entities[0].id,
                    selected: true,
                    replacement: Some("一位访客".into()),
                }],
            )
            .unwrap();
        let source_metadata = engine
            .office_preview(task.meta.id, false, updated.revision)
            .unwrap();
        assert_eq!(first.anchors[0].display, source_metadata.anchors[0].display);
        assert_eq!(
            original_docx,
            engine
                .office_preview_docx(task.meta.id, false, updated.revision)
                .unwrap()
        );
        engine.execute(task.meta.id).unwrap();
        let result_metadata = engine
            .office_preview(task.meta.id, true, updated.revision)
            .unwrap();
        assert!(
            result_metadata
                .anchors
                .iter()
                .any(|a| a.text.contains("一位访客"))
        );
        assert!(result_metadata.anchors[0].display.end > first.anchors[0].display.end);
        assert!(
            engine
                .draft_page(task.meta.id, 0, 512, updated.revision, &mut || Ok(()))
                .is_err()
        );
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
        let upsert_mutation = Uuid::new_v4();
        let ack = engine
            .upsert_region_with_mutation(id, region.clone(), 1, Some(upsert_mutation))
            .unwrap();
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
        // Simulate a saved operation whose IPC acknowledgement was lost, then
        // retry after restarting. No source decode or duplicate write is needed.
        let retried = engine
            .upsert_region_with_mutation(id, region.clone(), 1, Some(upsert_mutation))
            .unwrap();
        assert_eq!(retried.revision, 2);
        assert_eq!(retried.region_id, region_id);
        let mut changed = region.clone();
        changed.replacement = Some("different".into());
        assert!(matches!(
            engine.upsert_region_with_mutation(id, changed, 1, Some(upsert_mutation)),
            Err(Error::Conflict(_))
        ));
        assert!(matches!(
            engine.upsert_region_with_mutation(id, region.clone(), 2, Some(upsert_mutation)),
            Err(Error::Conflict(_))
        ));
        let delete_mutation = Uuid::new_v4();
        let ack = engine
            .remove_region_with_mutation(id, region_id, 2, Some(delete_mutation))
            .unwrap();
        assert_eq!(ack.revision, 3);
        let retried = engine
            .remove_region_with_mutation(id, region_id, 2, Some(delete_mutation))
            .unwrap();
        assert_eq!(retried.revision, 3);
        assert_eq!(retried.region_id, region_id);
        assert!(matches!(
            engine.remove_region_with_mutation(id, Uuid::new_v4(), 2, Some(delete_mutation)),
            Err(Error::Conflict(_))
        ));
        assert!(matches!(
            engine.upsert_region_with_mutation(id, region, 1, Some(upsert_mutation)),
            Err(Error::Conflict(_))
        ));
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
        // An old delete must never erase newer edits, even with its old token.
        assert!(matches!(
            engine.remove_region_with_mutation(id, region_id, 2, Some(delete_mutation)),
            Err(Error::Conflict(_))
        ));
        std::fs::write(&analysis_path, &analysis_before).unwrap();
        let view = engine.view(id).unwrap();
        assert_eq!(view.revision, 4);
        assert_eq!(view.regions.len(), 1);
        assert_eq!(std::fs::read(&analysis_path).unwrap(), analysis_before);
    }
}
