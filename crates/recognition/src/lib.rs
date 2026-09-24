pub mod models;
pub mod ner;
pub mod ocr;
use domain::{Entity, Error, Result, Rule, RuleKind, Span};
use presidio_analyzer::RecognizerResult;
use regex::{Regex, RegexBuilder};
use uuid::Uuid;

pub trait Ner: Send {
    fn analyze(&mut self, text: &str) -> Result<Vec<Entity>>;
    fn analyze_with_run(&mut self, text: &str, run: &ocr::OcrRun) -> Result<Vec<Entity>> {
        if run.is_cancelled() {
            return Err(Error::State("任务已取消".into()));
        }
        self.analyze(text)
    }
}
pub fn entity(result: RecognizerResult, source: &str) -> Entity {
    Entity {
        id: Uuid::new_v4(),
        entity_type: result.entity_type,
        score: result.score as f32,
        source: source.into(),
        selected: result.score >= 0.3,
        span: Span {
            start: result.start,
            end: result.end,
        },
        replacement: None,
    }
}
pub fn compile_rule(rule: &Rule) -> Result<Vec<Regex>> {
    if rule.name.trim().is_empty()
        || rule.entity_type.trim().is_empty()
        || rule.pattern.is_empty()
        || rule.pattern.len() > 64 * 1024
    {
        return Err(Error::Invalid(
            "规则名称、类型和内容不能为空，内容上限 64 KB".into(),
        ));
    }
    let patterns = match rule.kind {
        RuleKind::Literal => vec![regex::escape(&rule.pattern)],
        RuleKind::Regex => vec![rule.pattern.clone()],
        RuleKind::Dictionary => rule
            .pattern
            .lines()
            .filter(|s| !s.trim().is_empty())
            .map(|s| regex::escape(s.trim()))
            .collect(),
    };
    if patterns.is_empty() || patterns.len() > 2000 {
        return Err(Error::Invalid("词典需要 1–2000 个词条".into()));
    }
    patterns
        .into_iter()
        .map(|p| {
            let regex = RegexBuilder::new(&p)
                .size_limit(2 * 1024 * 1024)
                .dfa_size_limit(2 * 1024 * 1024)
                .build()
                .map_err(|e| Error::Invalid(format!("正则无效：{e}")))?;
            if regex.is_match("") {
                return Err(Error::Invalid("规则不得匹配空字符串".into()));
            }
            Ok(regex)
        })
        .collect()
}
pub fn analyze(text: &str, rules: &[Rule], ner: &mut dyn Ner) -> Result<Vec<Entity>> {
    analyze_with_run(text, rules, ner, &ocr::OcrRun::new()?)
}
pub fn analyze_with_run(
    text: &str,
    rules: &[Rule],
    ner: &mut dyn Ner,
    run: &ocr::OcrRun,
) -> Result<Vec<Entity>> {
    if text.is_empty() || text.len() > 8 * 1024 * 1024 {
        return Err(Error::Invalid("文本不能为空或超过 8 MB".into()));
    }
    // Required inference happens before any rule work; failures never become regex-only success.
    let mut results = ner.analyze_with_run(text, run)?;
    for (typ, pattern, score) in [
        ("PHONE", r"1[3-9][0-9]{9}", 0.95),
        (
            "EMAIL",
            r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}",
            0.95,
        ),
        (
            "ID_CARD",
            r"[1-9][0-9]{5}(?:19|20)[0-9]{2}(?:0[1-9]|1[0-2])(?:0[1-9]|[12][0-9]|3[01])[0-9]{3}[0-9Xx]",
            0.9,
        ),
    ] {
        let regex = Regex::new(pattern).map_err(|e| Error::Invalid(e.to_string()))?;
        for m in regex.find_iter(text) {
            if matches!(typ, "PHONE" | "ID_CARD")
                && (text[..m.start()]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_ascii_digit())
                    || text[m.end()..]
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_digit()))
            {
                continue;
            }
            results.push(entity(
                RecognizerResult::new(typ, m.start(), m.end(), score),
                "内置规则",
            ));
        }
    }
    for rule in rules.iter().filter(|r| r.enabled) {
        if run.is_cancelled() {
            return Err(Error::State("任务已取消".into()));
        }
        for regex in compile_rule(rule)? {
            for m in regex.find_iter(text) {
                if results.len() >= 100_000 {
                    return Err(Error::Invalid(
                        "实体数量超过 100000，请缩小输入或规则范围".into(),
                    ));
                }
                if !m.is_empty() {
                    results.push(entity(
                        RecognizerResult::new(&rule.entity_type, m.start(), m.end(), 1.0),
                        "自定义规则",
                    ));
                }
            }
        }
    }
    for e in &results {
        e.span.validate(text)?;
    }
    Ok(domain::resolve_conflicts(results))
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Failed;
    impl Ner for Failed {
        fn analyze(&mut self, _: &str) -> Result<Vec<Entity>> {
            Err(Error::ModelsNotReady("测试失败".into()))
        }
    }
    #[test]
    fn cannot_fall_back_to_rules() {
        assert!(analyze("13812345678", &[], &mut Failed).is_err());
    }
    #[test]
    fn cancellation_prevents_inference_and_rule_only_results() {
        struct Never;
        impl Ner for Never {
            fn analyze(&mut self, _: &str) -> Result<Vec<Entity>> {
                panic!("cancelled work must not run inference")
            }
        }
        let run = ocr::OcrRun::new().unwrap();
        let child = run.clone();
        run.cancel().unwrap();
        assert!(child.is_cancelled());
        assert!(matches!(
            analyze_with_run("电话13812345678", &[], &mut Never, &child),
            Err(Error::State(_))
        ));
    }
    #[test]
    fn unsafe_regex_rejected() {
        let rule = Rule {
            id: Uuid::new_v4(),
            name: "测试".into(),
            entity_type: "CUSTOM".into(),
            kind: RuleKind::Regex,
            pattern: "(?<=秘密).*".into(),
            enabled: true,
        };
        assert!(compile_rule(&rule).is_err());
    }
    #[test]
    fn phone_boundaries_allow_adjacent_chinese_but_not_longer_numbers() {
        struct Empty;
        impl Ner for Empty {
            fn analyze(&mut self, _: &str) -> Result<Vec<Entity>> {
                Ok(vec![])
            }
        }
        let text = "电话13812345678号码，错误9138123456789";
        let entities = analyze(text, &[], &mut Empty).unwrap();
        assert_eq!(entities.len(), 1);
        assert_eq!(
            &text[entities[0].span.start..entities[0].span.end],
            "13812345678"
        );
    }
}
