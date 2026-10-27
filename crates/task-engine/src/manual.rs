use super::*;
use domain::Span;
use sha2::{Digest, Sha256};

#[derive(Serialize)]
pub struct TextMatch {
    pub id: String,
    pub display: Span,
    pub context: String,
    pub conflicts: usize,
    pub locatable: bool,
    pub page: Option<u32>,
}
#[derive(Serialize)]
pub struct TextMatches {
    pub revision: u64,
    pub items: Vec<TextMatch>,
}
#[derive(Serialize, Deserialize)]
pub struct ApplyText {
    pub query: String,
    pub replacement: String,
    pub match_ids: Vec<String>,
    pub expected_revision: u64,
    pub mutation_id: Uuid,
    #[serde(default)]
    pub allow_conflicts: bool,
}
#[derive(Clone, Serialize, Deserialize)]
struct UndoEntry {
    id: Uuid,
    fingerprint: String,
    undo_id: Uuid,
    before: ReviewPayload,
    #[serde(default)]
    applied: bool,
    #[serde(default)]
    undone: bool,
}
fn match_id(span: Span, text: &str) -> String {
    format!(
        "{:x}",
        Sha256::digest(format!("{}:{}:{text}", span.start, span.end))
    )
}
// Reconstruct the original OCR line offsets in extraction order, never from entity offsets.
fn line_maps(p: &Payload) -> Result<Vec<(Span, &RegionDto)>> {
    let text = payload_text(p);
    let mut cursor = p.document.text.len();
    let mut maps = Vec::new();
    for region in p
        .regions
        .iter()
        .filter(|r| matches!(r.source, RegionSource::Ocr))
    {
        if region.text.is_empty() {
            continue;
        }
        if text[cursor..].starts_with('\n') {
            cursor += 1;
        }
        if !text[cursor..].starts_with(&region.text) {
            return Err(Error::State(
                "旧任务的 OCR 位置无法确认，请重新分析文件或使用框选".into(),
            ));
        }
        let end = cursor + region.text.len();
        maps.push((Span { start: cursor, end }, region));
        cursor = end;
    }
    Ok(maps)
}
fn spans(p: &Payload, query: &str) -> Result<Vec<Span>> {
    if query.trim().is_empty() || query.len() > 4096 {
        return Err(Error::Invalid("请输入不超过 4096 字节的查找文字".into()));
    }
    let text = payload_text(p);
    let values: Vec<_> = text
        .match_indices(query)
        .map(|(start, value)| Span {
            start,
            end: start + value.len(),
        })
        .filter(|span| span.start >= p.document.text.len() || p.document.valid_text_match(*span))
        .take(100_001)
        .collect();
    if values.len() > 100_000 {
        return Err(Error::Invalid("匹配过多，请缩小查找范围".into()));
    }
    Ok(values)
}
impl Engine {
    pub fn find_text(&self, id: Uuid, query: &str) -> Result<TextMatches> {
        let p = self.task_payload(id)?;
        let text = payload_text(&p);
        let maps = line_maps(&p)?;
        let items = spans(&p, query)?
            .into_iter()
            .map(|span| {
                let context = format!(
                    "{}{}{}",
                    text[..span.start]
                        .chars()
                        .rev()
                        .take(16)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect::<String>(),
                    &text[span.start..span.end],
                    text[span.end..].chars().take(22).collect::<String>()
                );
                Ok(TextMatch {
                    id: match_id(span, query),
                    display: span.utf16(text)?,
                    context,
                    conflicts: p
                        .entities
                        .iter()
                        .filter(|e| e.selected && e.span.overlaps(span) && e.span != span)
                        .count(),
                    locatable: span.end <= p.document.text.len()
                        || maps
                            .iter()
                            .any(|(line, _)| span.start >= line.start && span.end <= line.end),
                    page: maps
                        .iter()
                        .find(|(line, _)| span.start >= line.start && span.end <= line.end)
                        .map(|(_, region)| region.page),
                })
            })
            .collect::<Result<_>>()?;
        Ok(TextMatches {
            revision: p.revision,
            items,
        })
    }
    pub fn apply_text(&self, id: Uuid, args: ApplyText) -> Result<TaskView> {
        if self.meta(id)?.state != TaskState::AwaitingReview {
            return Err(Error::State("请先点击继续调整".into()));
        }
        let mut history: Vec<UndoEntry> = self.store.manual_history(id)?.unwrap_or_default();
        let fingerprint = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&args).map_err(|e| Error::Io(e.to_string()))?)
        );
        if history
            .iter()
            .any(|h| h.id == args.mutation_id && h.fingerprint != fingerprint)
        {
            return Err(Error::Conflict(
                "重复请求的替换内容不一致，请重新查找".into(),
            ));
        }
        if history
            .iter()
            .any(|h| h.id == args.mutation_id && h.applied)
        {
            return self.view(id);
        }
        let mut review = self.mutable_review(id, false)?;
        if review.last_mutation == Some(args.mutation_id) {
            if let Some(entry) = history.iter_mut().find(|h| h.id == args.mutation_id) {
                entry.applied = true;
                self.store.save_manual_history(id, &history)?;
            }
            return self.view(id);
        }
        self.ensure_revision(review.revision, args.expected_revision)?;
        if args.replacement.len() > 4096 {
            return Err(Error::Invalid("替换内容过长".into()));
        }
        let p = self.task_payload(id)?;
        let text = payload_text(&p);
        let maps = line_maps(&p)?;
        let found = spans(&p, &args.query)?;
        let wanted: std::collections::HashSet<_> = args.match_ids.iter().collect();
        let chosen: Vec<_> = found
            .into_iter()
            .filter(|s| wanted.contains(&match_id(*s, &args.query)))
            .collect();
        if chosen.is_empty() || chosen.len() != wanted.len() {
            return Err(Error::Conflict("查找结果已变化，请重新查找".into()));
        }
        if !args.allow_conflicts
            && review
                .entities
                .iter()
                .any(|e| e.selected && chosen.iter().any(|s| s.overlaps(e.span) && *s != e.span))
        {
            return Err(Error::Conflict(
                "替换范围与已有实体重叠，请确认后应用".into(),
            ));
        }
        let before = review.clone();
        for span in chosen {
            let previous = review
                .entities
                .iter()
                .find(|e| e.span == span)
                .map(|e| e.id);
            let removed: Vec<_> = review
                .entities
                .iter()
                .filter(|e| e.span.overlaps(span))
                .map(|e| e.id)
                .collect();
            review.entities.retain(|e| !removed.contains(&e.id));
            review
                .regions
                .retain(|r| !r.entity_id.is_some_and(|id| removed.contains(&id)));
            let entity = Entity {
                id: previous.unwrap_or_else(Uuid::new_v4),
                entity_type: "手动文字".into(),
                score: 1.0,
                source: "手动添加".into(),
                selected: true,
                span,
                replacement: Some(args.replacement.clone()),
            };
            if span.end > p.document.text.len() {
                let (line_span, line) = maps
                    .iter()
                    .find(|(s, _)| span.start >= s.start && span.end <= s.end)
                    .ok_or_else(|| {
                        Error::Invalid("文字跨越 OCR 行或无法定位，请分行替换或使用框选".into())
                    })?;
                review.regions.push(RegionDto {
                    id: Uuid::new_v4(),
                    page: line.page,
                    polygon: entity_polygon(
                        &text[line_span.start..line_span.end],
                        line_span.start,
                        &entity,
                        &line.polygon,
                    ),
                    entity_id: Some(entity.id),
                    selected: true,
                    source: RegionSource::Entity,
                    text: args.query.clone(),
                    score: Some(1.0),
                    rotation: line.rotation,
                    replacement: Some(args.replacement.clone()),
                });
            }
            review.entities.push(entity);
        }
        if review.entities.len() > 100_000 {
            return Err(Error::Invalid("实体数量超过限制".into()));
        }
        review.entities.sort_by_key(|e| e.span.start);
        review.revision += 1;
        review.last_mutation = Some(args.mutation_id);
        review.last_region_mutation = None;
        history.retain(|h| h.id != args.mutation_id);
        if history.len() >= 20 {
            history.remove(0);
        }
        history.push(UndoEntry {
            id: args.mutation_id,
            fingerprint,
            undo_id: Uuid::new_v4(),
            before,
            applied: false,
            undone: false,
        });
        self.store.save_manual_history(id, &history)?;
        self.save_review_payload(id, &review)?;
        history.last_mut().unwrap().applied = true;
        self.store.save_manual_history(id, &history)?;
        self.mark_reviewed(id, 0)?;
        self.view(id)
    }
    pub fn undo_text(
        &self,
        id: Uuid,
        mutation_id: Uuid,
        expected_revision: u64,
    ) -> Result<TaskView> {
        if self.meta(id)?.state != TaskState::AwaitingReview {
            return Err(Error::State("只能撤销复核中的任务".into()));
        }
        let mut history: Vec<UndoEntry> = self.store.manual_history(id)?.unwrap_or_default();
        let entry = history
            .iter_mut()
            .find(|h| h.id == mutation_id)
            .ok_or_else(|| Error::Invalid("撤销记录已过期".into()))?;
        if entry.undone {
            return self.view(id);
        }
        let current = self.mutable_review(id, false)?;
        if current.last_mutation == Some(entry.undo_id) {
            entry.undone = true;
            self.store.save_manual_history(id, &history)?;
            self.mark_reviewed(id, 0)?;
            return self.view(id);
        }
        self.ensure_revision(current.revision, expected_revision)?;
        let mut review = entry.before.clone();
        review.revision = current.revision + 1;
        review.last_mutation = Some(entry.undo_id);
        review.last_region_mutation = None;
        self.save_review_payload(id, &review)?;
        entry.undone = true;
        self.store.save_manual_history(id, &history)?;
        self.mark_reviewed(id, 0)?;
        self.view(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    struct NoEntities;
    impl Ner for NoEntities {
        fn analyze(&mut self, _: &str) -> Result<Vec<Entity>> {
            Ok(vec![])
        }
    }
    fn engine(root: &Path) -> Engine {
        Engine {
            store: Store::open(root, zeroize::Zeroizing::new([1; 32])).unwrap(),
            ner: Some(Box::new(NoEntities)),
            ocr_mobile: None,
            ocr_accurate: None,
            active_ocr: Arc::new(Mutex::new(HashMap::new())),
        }
    }
    fn request(e: &Engine, id: Uuid, query: &str, replacement: &str) -> ApplyText {
        let found = e.find_text(id, query).unwrap();
        ApplyText {
            query: query.into(),
            replacement: replacement.into(),
            match_ids: found.items.into_iter().map(|m| m.id).collect(),
            expected_revision: found.revision,
            mutation_id: Uuid::new_v4(),
            allow_conflicts: false,
        }
    }
    #[test]
    fn manual_text_changes_actual_output_and_undo_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let mut e = engine(dir.path());
        let task = e.analyze_text("😀青岛市 青岛市 公开文字".into()).unwrap();
        let id = task.meta.id;
        let matches = e.find_text(id, "青岛市").unwrap();
        assert_eq!(matches.items.len(), 2);
        assert_eq!(matches.items[0].display.start, 2);
        let args = request(&e, id, "青岛市", "某地");
        let mutation = args.mutation_id;
        let view = e.apply_text(id, args).unwrap();
        assert_eq!(view.entities.len(), 2);
        assert_eq!(e.preview(id).unwrap(), "😀某地 某地 公开文字");
        drop(e);
        let e = engine(dir.path());
        let view = e.undo_text(id, mutation, view.revision).unwrap();
        assert!(view.entities.is_empty());
        assert_eq!(e.preview(id).unwrap(), "😀青岛市 青岛市 公开文字");
        e.apply_text(id, request(&e, id, "青岛市", "替换")).unwrap();
        e.execute(id).unwrap();
        let output = dir.path().join("实际.txt");
        e.export(id, &output).unwrap();
        assert_eq!(
            std::fs::read_to_string(output).unwrap(),
            "😀替换 替换 公开文字"
        );
    }
    #[test]
    fn docx_cross_run_replacement_preserves_other_content_and_anchor_identity() {
        let dir = tempfile::tempdir().unwrap();
        let mut e = engine(dir.path());
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, xml) in [
            ("[Content_Types].xml", "<Types/>"),
            (
                "word/document.xml",
                "<w:document xmlns:w=\"urn:w\"><w:body><w:p><w:r><w:t>青岛</w:t></w:r><w:r><w:t>市</w:t></w:r><w:r><w:t>公开文字</w:t></w:r></w:p></w:body></w:document>",
            ),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(xml.as_bytes()).unwrap();
        }
        let input = dir.path().join("图文.docx");
        std::fs::write(&input, zip.finish().unwrap().into_inner()).unwrap();
        let task = e.analyze_file(&input).unwrap();
        let id = task.meta.id;
        let before = e.office_preview(id, false, task.revision).unwrap();
        e.apply_text(id, request(&e, id, "青岛市", "某地")).unwrap();
        let task = e.execute(id).unwrap();
        let output = dir.path().join("实际.docx");
        e.export(id, &output).unwrap();
        let document = Document::load("docx", std::fs::read(output).unwrap()).unwrap();
        assert!(document.text.contains("某地公开文字"));
        assert!(!document.text.contains("青岛"));
        let after = e.office_preview(id, true, task.revision).unwrap();
        let a = before
            .anchors
            .iter()
            .find(|a| a.text == "公开文字")
            .unwrap();
        let b = after.anchors.iter().find(|a| a.text == "公开文字").unwrap();
        assert_eq!(a.id, b.id);
    }
    #[test]
    fn stale_and_forged_matches_do_not_change_review() {
        let dir = tempfile::tempdir().unwrap();
        let mut e = engine(dir.path());
        let task = e.analyze_text("青岛市 青岛市".into()).unwrap();
        let id = task.meta.id;
        let mut forged = request(&e, id, "青岛市", "某地");
        forged.match_ids.push("untrusted-offset".into());
        assert!(e.apply_text(id, forged).is_err());
        let stale = request(&e, id, "青岛市", "某地");
        e.review_patch(id, vec![], task.revision, Uuid::new_v4())
            .unwrap();
        assert!(e.apply_text(id, stale).is_err());
        assert!(e.view(id).unwrap().entities.is_empty());
    }
    #[test]
    fn retries_validate_request_and_recover_interrupted_undo() {
        let dir = tempfile::tempdir().unwrap();
        let mut e = engine(dir.path());
        let id = e.analyze_text("重复 重复".into()).unwrap().meta.id;
        let args = request(&e, id, "重复", "替换");
        let serialized = serde_json::to_vec(&args).unwrap();
        let mutation = args.mutation_id;
        let applied = e.apply_text(id, args).unwrap();
        let retry = e
            .apply_text(id, serde_json::from_slice(&serialized).unwrap())
            .unwrap();
        assert_eq!(applied.revision, retry.revision);
        let mut changed: ApplyText = serde_json::from_slice(&serialized).unwrap();
        changed.replacement = "另一内容".into();
        assert!(e.apply_text(id, changed).is_err());
        let history: Vec<UndoEntry> = e.store.manual_history(id).unwrap().unwrap();
        let undone = e.undo_text(id, mutation, applied.revision).unwrap();
        // Simulate process exit after the review write but before the undo receipt.
        e.store.save_manual_history(id, &history).unwrap();
        let recovered = e.undo_text(id, mutation, applied.revision).unwrap();
        assert_eq!(undone.revision, recovered.revision);
        assert_eq!(e.preview(id).unwrap(), "重复 重复");
    }
    #[test]
    fn manual_ocr_match_generates_real_redaction_region() {
        let dir = tempfile::tempdir().unwrap();
        let mut e = engine(dir.path());
        let task = e.analyze_text("占位".into()).unwrap();
        let id = task.meta.id;
        let mut image = Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(120, 80, image::Rgba([240, 240, 240, 255]))
            .write_to(&mut image, image::ImageFormat::Png)
            .unwrap();
        let mut p: Payload = e.store.payload(id).unwrap();
        p.document = Document::load("png", image.into_inner()).unwrap();
        p.analysis_text = "青岛市青岛市".into();
        p.page_count = 1;
        p.regions = vec![RegionDto {
            id: Uuid::new_v4(),
            page: 0,
            polygon: vec![
                Point { x: 0.1, y: 0.1 },
                Point { x: 0.9, y: 0.1 },
                Point { x: 0.9, y: 0.6 },
                Point { x: 0.1, y: 0.6 },
            ],
            entity_id: None,
            selected: false,
            source: RegionSource::Ocr,
            text: p.analysis_text.clone(),
            score: Some(1.0),
            rotation: 0.0,
            replacement: None,
        }];
        e.store.save_payload(id, &p).unwrap();
        e.save_review(id, &p).unwrap();
        let updated = e.apply_text(id, request(&e, id, "青岛市", "某地")).unwrap();
        assert_eq!(updated.regions.iter().filter(|r| r.selected).count(), 2);
        let source = p.document.original.clone();
        e.execute(id).unwrap();
        let output = dir.path().join("结果.png");
        e.export(id, &output).unwrap();
        let original = image::load_from_memory(&source).unwrap().into_rgba8();
        let result = image::open(output).unwrap().into_rgba8();
        assert_eq!(original.get_pixel(119, 79), result.get_pixel(119, 79));
        assert_ne!(original, result);
    }
}
