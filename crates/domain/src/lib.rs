use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "code", content = "message", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Error {
    #[error("输入无效：{0}")]
    Invalid(String),
    #[error("模型未就绪：{0}")]
    ModelsNotReady(String),
    #[error("状态不允许此操作：{0}")]
    State(String),
    #[error("复核内容已被其他操作更新：{0}")]
    Conflict(String),
    #[error("文件或数据库操作失败：{0}")]
    Io(String),
    #[error("口令错误或文件已损坏")]
    Authentication,
    #[error("该格式的安全处理尚未可用：{0}")]
    Unsupported(String),
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Queued,
    Analyzing,
    AwaitingReview,
    Processing,
    Completed,
    Partial,
    Failed,
    Cancelled,
}
impl TaskState {
    pub fn transition(self, next: Self) -> Result<Self> {
        use TaskState::*;
        if matches!(
            (self, next),
            (Queued, Analyzing | Cancelled | Failed)
                | (Analyzing, AwaitingReview | Failed | Cancelled)
                | (AwaitingReview, Processing | Cancelled | Failed)
                | (Processing, Completed | Partial | Failed | Cancelled)
        ) {
            Ok(next)
        } else {
            Err(Error::State(format!("{self:?} → {next:?}")))
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}
impl Span {
    pub fn validate(self, text: &str) -> Result<Self> {
        if self.start >= self.end || text.get(self.start..self.end).is_none() {
            return Err(Error::Invalid("实体区间不在 UTF-8 字符边界上".into()));
        }
        Ok(self)
    }
    pub fn utf16(self, text: &str) -> Result<Self> {
        self.validate(text)?;
        Ok(Self {
            start: text[..self.start].encode_utf16().count(),
            end: text[..self.end].encode_utf16().count(),
        })
    }
    pub fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub id: Uuid,
    pub entity_type: String,
    pub score: f32,
    pub source: String,
    pub selected: bool,
    pub span: Span,
    pub replacement: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityDto {
    pub id: Uuid,
    pub entity_type: String,
    pub type_label: String,
    pub score: f32,
    pub source: String,
    pub selected: bool,
    pub display: Span,
    pub text: String,
    pub replacement: Option<String>,
}
impl Entity {
    pub fn dto(&self, text: &str) -> Result<EntityDto> {
        let display = self.span.utf16(text)?;
        Ok(EntityDto {
            id: self.id,
            entity_type: self.entity_type.clone(),
            type_label: type_label(&self.entity_type).into(),
            score: self.score,
            source: self.source.clone(),
            selected: self.selected,
            display,
            text: text[self.span.start..self.span.end].into(),
            replacement: self.replacement.clone(),
        })
    }
}
pub fn type_label(t: &str) -> &str {
    match t {
        "PERSON" => "姓名",
        "ORGANIZATION" => "机构",
        "LOCATION" | "GPE" => "地点",
        "PHONE" => "手机号",
        "EMAIL" => "邮箱",
        "ID_CARD" => "身份证",
        "BANK_CARD" => "银行卡",
        "ADDRESS" => "地址",
        _ => t,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub id: Uuid,
    pub selected: bool,
    pub replacement: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegionDto {
    pub id: Uuid,
    pub page: u32,
    pub polygon: Vec<Point>,
    pub entity_id: Option<Uuid>,
    pub selected: bool,
    #[serde(default)]
    pub source: RegionSource,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub score: Option<f32>,
    #[serde(default)]
    pub rotation: f32,
    #[serde(default)]
    pub replacement: Option<String>,
}
impl RegionDto {
    pub fn validate(&self, pages: u32) -> Result<()> {
        if self.page >= pages
            || self.polygon.len() < 3
            || self.polygon.len() > 32
            || self.polygon.iter().any(|p| {
                !p.x.is_finite()
                    || !p.y.is_finite()
                    || !(0.0..=1.0).contains(&p.x)
                    || !(0.0..=1.0).contains(&p.y)
            })
        {
            return Err(Error::Invalid("页面或归一化区域坐标无效".into()));
        }
        let area: f32 = self
            .polygon
            .iter()
            .zip(self.polygon.iter().cycle().skip(1))
            .map(|(a, b)| a.x * b.y - b.x * a.y)
            .sum();
        if area.abs() < 1e-8 {
            return Err(Error::Invalid("区域面积必须大于零".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RegionSource {
    Ocr,
    Entity,
    #[default]
    Manual,
    EmbeddedImage,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OcrProfile {
    #[default]
    Mobile,
    Accurate,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PdfMode {
    #[default]
    SafeRebuild,
    Fidelity,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskOptions {
    #[serde(default)]
    pub ocr_profile: OcrProfile,
    #[serde(default)]
    pub pdf_mode: PdfMode,
}
impl Default for TaskOptions {
    fn default() -> Self {
        Self {
            ocr_profile: OcrProfile::Mobile,
            pdf_mode: PdfMode::SafeRebuild,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageDto {
    pub index: u32,
    pub width: u32,
    pub height: u32,
    pub preview_uri: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreviewDto {
    pub revision: u64,
    pub pages: Vec<PageDto>,
    pub text: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppSettings {
    pub concurrency: u8,
    pub ocr_profile: OcrProfile,
    pub pdf_mode: PdfMode,
}
impl Default for AppSettings {
    fn default() -> Self {
        Self {
            concurrency: 2,
            ocr_profile: OcrProfile::Mobile,
            pdf_mode: PdfMode::SafeRebuild,
        }
    }
}
impl AppSettings {
    pub fn validate(self) -> Result<Self> {
        if !(1..=4).contains(&self.concurrency) {
            return Err(Error::Invalid("并发任务数必须在 1 到 4 之间".into()));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressEvent {
    pub id: String,
    pub stage: String,
    pub current: u64,
    pub total: u64,
    pub percent: f32,
    #[serde(default)]
    pub bytes_per_second: u64,
    #[serde(default)]
    pub eta_seconds: Option<u64>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub source_label: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind {
    Literal,
    Regex,
    Dictionary,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub id: Uuid,
    pub name: String,
    pub entity_type: String,
    pub kind: RuleKind,
    pub pattern: String,
    pub enabled: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    pub entity_type: String,
    pub replacement: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BuiltinPolicy {
    pub entity_type: &'static str,
    pub type_label: &'static str,
    pub behavior: &'static str,
    pub examples: Vec<PolicyExample>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PolicyExample {
    pub original: &'static str,
    pub replacement: String,
}

pub fn builtin_policies() -> Vec<BuiltinPolicy> {
    [
        ("PERSON", "替换为“某人”", &["张三"][..]),
        (
            "ORGANIZATION",
            "学校替换为“某学校”，其余替换为“某公司”",
            &["北京大学", "某科技有限公司"],
        ),
        ("LOCATION", "替换为“某地点”", &["北京市"]),
        ("GPE", "替换为“某地点”", &["北京市"]),
        ("ADDRESS", "替换为“某地址”", &["北京市朝阳区某街道"]),
        (
            "PHONE",
            "11 位号码保留前三后四，其余替换为“已脱敏”",
            &["13812345678", "01088888888"],
        ),
        ("EMAIL", "替换为“***@***”", &["name@example.com"]),
        ("ID_CARD", "替换为“已脱敏”", &["110101199001011234"]),
        ("BANK_CARD", "替换为“已脱敏”", &["6222021234567890123"]),
    ]
    .into_iter()
    .map(|(entity_type, behavior, originals)| BuiltinPolicy {
        entity_type,
        type_label: type_label(entity_type),
        behavior,
        examples: originals
            .iter()
            .map(|original| PolicyExample {
                original,
                replacement: default_replacement(entity_type, original),
            })
            .collect(),
    })
    .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorInfo {
    pub code: String,
    pub title: String,
    pub detail: String,
    pub recovery_action: String,
    pub retryable: bool,
    pub diagnostic_id: Uuid,
}
impl ErrorInfo {
    pub fn new(
        code: impl Into<String>,
        title: impl Into<String>,
        detail: impl Into<String>,
        recovery_action: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            code: code.into(),
            title: title.into(),
            detail: detail.into(),
            recovery_action: recovery_action.into(),
            retryable,
            diagnostic_id: Uuid::new_v4(),
        }
    }

    pub fn for_error(error: &Error, title: impl Into<String>) -> Self {
        let (code, recovery_action, retryable) = match error {
            Error::Invalid(_) => ("INVALID_INPUT", "检查文件格式、大小或输入内容后重试", true),
            Error::ModelsNotReady(_) => ("MODELS_NOT_READY", "前往模型管理完成安装和校验", true),
            Error::State(_) => ("INVALID_TASK_STATE", "刷新任务状态后重试", false),
            Error::Conflict(_) => ("REVISION_CONFLICT", "刷新任务内容后重试", true),
            Error::Io(_) => ("IO_ERROR", "检查磁盘空间和文件访问权限后重试", true),
            Error::Authentication => ("AUTHENTICATION_FAILED", "确认口令和恢复包后重试", true),
            Error::Unsupported(_) => (
                "UNSUPPORTED_FORMAT",
                "改用受支持的文件格式或处理模式",
                false,
            ),
        };
        Self::new(code, title, error.to_string(), recovery_action, retryable)
    }

    pub fn cancelled(detail: impl Into<String>) -> Self {
        Self::new(
            "TASK_CANCELLED",
            "任务已取消",
            detail,
            "可以从原文件重新创建任务",
            true,
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskMeta {
    pub id: Uuid,
    pub kind: String,
    pub state: TaskState,
    pub created_at: u64,
    pub updated_at: u64,
    pub error: Option<String>,
    #[serde(default)]
    pub error_info: Option<ErrorInfo>,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub file_size: u64,
    #[serde(default)]
    pub parent_batch_id: Option<Uuid>,
    #[serde(default)]
    pub reviewed_revision: u64,
    #[serde(default)]
    pub storage_bytes: u64,
}

pub fn resolve_conflicts(mut entities: Vec<Entity>) -> Vec<Entity> {
    entities.retain(|e| {
        e.score.is_finite() && (0.0..=1.0).contains(&e.score) && e.span.start < e.span.end
    });
    entities.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| (b.span.end - b.span.start).cmp(&(a.span.end - a.span.start)))
            .then(a.span.start.cmp(&b.span.start))
            .then(a.entity_type.cmp(&b.entity_type))
    });
    let mut chosen: std::collections::BTreeMap<usize, Entity> = std::collections::BTreeMap::new();
    for e in entities {
        let previous_overlaps = chosen
            .range(..=e.span.start)
            .next_back()
            .is_some_and(|(_, c)| c.span.overlaps(e.span));
        let next_overlaps = chosen
            .range(e.span.start..)
            .next()
            .is_some_and(|(_, c)| c.span.overlaps(e.span));
        if !previous_overlaps && !next_overlaps {
            chosen.insert(e.span.start, e);
        }
    }
    chosen.into_values().collect()
}
pub fn replacement(entity: &Entity, value: &str, policies: &[Policy]) -> String {
    if let Some(s) = &entity.replacement {
        return s.clone();
    }
    if let Some(p) = policies
        .iter()
        .find(|p| p.entity_type == entity.entity_type)
    {
        return p.replacement.clone();
    }
    default_replacement(&entity.entity_type, value)
}
fn default_replacement(entity_type: &str, value: &str) -> String {
    match entity_type {
        "PERSON" => "某人".into(),
        "ORGANIZATION" => if ["大学", "学院", "学校", "中学", "小学"]
            .iter()
            .any(|s| value.contains(s))
        {
            "某学校"
        } else {
            "某公司"
        }
        .into(),
        "LOCATION" | "GPE" => "某地点".into(),
        "ADDRESS" => "某地址".into(),
        "PHONE" if value.chars().count() == 11 => format!(
            "{}****{}",
            value.chars().take(3).collect::<String>(),
            value.chars().skip(7).collect::<String>()
        ),
        "EMAIL" => "***@***".into(),
        _ => "已脱敏".into(),
    }
}
pub fn redact(text: &str, entities: &[Entity], policies: &[Policy]) -> Result<String> {
    let mut selected: Vec<_> = entities.iter().filter(|e| e.selected).collect();
    selected.sort_by_key(|e| e.span.start);
    let mut output = String::new();
    let mut cursor = 0;
    for e in selected {
        e.span.validate(text)?;
        if e.span.start < cursor {
            return Err(Error::Invalid("选中实体存在重叠".into()));
        }
        output.push_str(&text[cursor..e.span.start]);
        output.push_str(&replacement(e, &text[e.span.start..e.span.end], policies));
        cursor = e.span.end;
    }
    output.push_str(&text[cursor..]);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    #[test]
    fn builtin_examples_match_default_replacement_and_overrides_win() {
        let builtins = builtin_policies();
        assert_eq!(builtins.len(), 9);
        for policy in &builtins {
            for example in &policy.examples {
                let entity = Entity {
                    id: Uuid::new_v4(),
                    entity_type: policy.entity_type.into(),
                    score: 1.0,
                    source: "test".into(),
                    selected: true,
                    span: Span {
                        start: 0,
                        end: example.original.len(),
                    },
                    replacement: None,
                };
                assert_eq!(
                    replacement(&entity, example.original, &[]),
                    example.replacement
                );
                let custom = Policy {
                    entity_type: policy.entity_type.into(),
                    replacement: "用户覆盖".into(),
                };
                assert_eq!(
                    replacement(&entity, example.original, &[custom]),
                    "用户覆盖"
                );
            }
        }
        let phone = builtins
            .iter()
            .find(|policy| policy.entity_type == "PHONE")
            .unwrap();
        assert_eq!(phone.examples[0].replacement, "138****5678");
        assert_eq!(phone.examples[1].replacement, "010****8888");
    }
    #[test]
    fn conflicts_prioritize_score_then_length_then_position() {
        let make = |start, end, score| Entity {
            id: Uuid::new_v4(),
            entity_type: "PERSON".into(),
            score,
            source: "test".into(),
            selected: true,
            span: Span { start, end },
            replacement: None,
        };
        let chosen = resolve_conflicts(vec![
            make(1, 8, 0.9),
            make(0, 4, 1.0),
            make(4, 9, 0.9),
            make(10, 12, 0.8),
            make(9, 13, 0.8),
        ]);
        assert_eq!(
            chosen.iter().map(|e| e.span).collect::<Vec<_>>(),
            vec![
                Span { start: 0, end: 4 },
                Span { start: 4, end: 9 },
                Span { start: 9, end: 13 }
            ]
        );
    }
    #[test]
    fn unicode_boundaries() {
        let t = "😀张三";
        assert_eq!(
            Span { start: 4, end: 10 }.utf16(t).unwrap(),
            Span { start: 2, end: 4 }
        );
        assert!(Span { start: 1, end: 4 }.validate(t).is_err());
    }
    #[test]
    fn terminal_states_are_terminal() {
        for s in [
            TaskState::Completed,
            TaskState::Partial,
            TaskState::Failed,
            TaskState::Cancelled,
        ] {
            assert!(s.transition(TaskState::Processing).is_err());
        }
    }
    proptest! { #[test] fn full_unicode_span(s in ".{1,200}") { let span=Span{start:0,end:s.len()}; prop_assert_eq!(span.utf16(&s).unwrap().end,s.encode_utf16().count()); } }
}
