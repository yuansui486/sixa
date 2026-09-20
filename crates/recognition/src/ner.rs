use crate::{Ner, entity};
use domain::{Entity, Error, Result, Span};
use ort::{session::Session, value::Tensor};
use presidio_analyzer::RecognizerResult;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokenizers::Tokenizer;

#[derive(Debug, Serialize, Deserialize)]
pub struct Crf {
    pub labels: Vec<String>,
    pub start: Vec<f32>,
    pub end: Vec<f32>,
    pub transitions: Vec<Vec<f32>>,
}
impl Crf {
    pub fn validate(&self) -> Result<()> {
        let n = self.labels.len();
        if n == 0
            || n > 256
            || self.start.len() != n
            || self.end.len() != n
            || self.transitions.len() != n
            || self.transitions.iter().any(|r| r.len() != n)
            || self.labels.iter().any(|label| {
                label != "O"
                    && !(label.len() > 2
                        && label.is_ascii()
                        && matches!(label.as_bytes()[0], b'B' | b'I' | b'E' | b'S')
                        && label.as_bytes()[1] == b'-')
            })
            || self
                .start
                .iter()
                .chain(&self.end)
                .chain(self.transitions.iter().flatten())
                .any(|v| !v.is_finite())
        {
            return Err(Error::ModelsNotReady("CRF 参数无效".into()));
        }
        Ok(())
    }
    pub fn decode(&self, emissions: &[Vec<f32>]) -> Result<Vec<usize>> {
        self.validate()?;
        let n = self.labels.len();
        if emissions.is_empty() {
            return Ok(Vec::new());
        }
        if emissions
            .iter()
            .any(|r| r.len() != n || r.iter().any(|v| !v.is_finite()))
        {
            return Err(Error::ModelsNotReady("NER 输出维度或数值无效".into()));
        }
        let mut score: Vec<f32> = (0..n).map(|i| self.start[i] + emissions[0][i]).collect();
        let mut history = Vec::new();
        for emission in &emissions[1..] {
            let mut next = vec![f32::NEG_INFINITY; n];
            let mut back = vec![0; n];
            for j in 0..n {
                for (i, s) in score.iter().enumerate() {
                    let candidate = s + self.transitions[i][j] + emission[j];
                    if candidate > next[j] {
                        next[j] = candidate;
                        back[j] = i;
                    }
                }
            }
            score = next;
            history.push(back);
        }
        let mut best = 0;
        for i in 1..n {
            if score[i] + self.end[i] > score[best] + self.end[best] {
                best = i;
            }
        }
        let mut path = vec![best];
        for back in history.iter().rev() {
            best = back[best];
            path.push(best);
        }
        path.reverse();
        Ok(path)
    }
}
fn err(e: impl std::fmt::Display) -> Error {
    Error::ModelsNotReady(e.to_string())
}
pub struct Raner {
    session: Session,
    tokenizer: Tokenizer,
    crf: Crf,
}
impl Raner {
    pub fn load(dir: &Path) -> Result<Self> {
        let verified = crate::models::verify_model(dir)?;
        Self::load_verified(&verified)
    }
    pub fn load_verified(verified: &crate::models::VerifiedModel) -> Result<Self> {
        verified.require(&[
            "emissions.onnx",
            "tokenizer.json",
            "crf.json",
            "onnxruntime.dll",
        ])?;
        let dir = verified.directory();
        ort::init_from(dir.join("onnxruntime.dll"))
            .map_err(err)?
            .commit();
        let session = Session::builder()
            .map_err(err)?
            .with_intra_threads(1)
            .map_err(err)?
            .commit_from_file(dir.join("emissions.onnx"))
            .map_err(err)?;
        let tokenizer = Tokenizer::from_file(dir.join("tokenizer.json")).map_err(err)?;
        let crf: Crf =
            serde_json::from_slice(&std::fs::read(dir.join("crf.json"))?).map_err(err)?;
        crf.validate()?;
        Ok(Self {
            session,
            tokenizer,
            crf,
        })
    }
    fn window(&mut self, text: &str, base: usize) -> Result<Vec<Entity>> {
        // RaNER's tokenizer_config.json specifies is_split_into_words=true.
        // ModelScope tokenizes each Unicode scalar independently, keeping the
        // first subtoken for CRF and substituting UNK for whitespace.
        let mut ids = vec![101i64];
        let mut kept = Vec::new();
        let mut offsets = Vec::new();
        for (start, ch) in text.char_indices() {
            let encoded = self.tokenizer.encode(ch.to_string(), false).map_err(err)?;
            kept.push(ids.len());
            offsets.push(Span {
                start,
                end: start + ch.len_utf8(),
            });
            if encoded.get_ids().is_empty() {
                ids.push(100);
            } else {
                ids.extend(encoded.get_ids().iter().map(|id| *id as i64));
            }
        }
        ids.push(102);
        let len = ids.len();
        if len > 512 {
            return Err(err("分词长度超过模型上限"));
        }
        let attention = vec![1i64; len];
        let types = vec![0i64; len];
        let outputs=self.session.run(ort::inputs!["input_ids"=>Tensor::from_array(([1,len],ids)).map_err(err)?,"attention_mask"=>Tensor::from_array(([1,len],attention)).map_err(err)?,"token_type_ids"=>Tensor::from_array(([1,len],types)).map_err(err)?]).map_err(err)?;
        let (shape, logits) = outputs["emissions"]
            .try_extract_tensor::<f32>()
            .map_err(err)?;
        let n = self.crf.labels.len();
        if shape.as_ref() != [1, len as i64, n as i64] {
            return Err(err("emissions 维度不符"));
        }
        let emissions: Vec<Vec<f32>> = kept
            .into_iter()
            .map(|i| logits[i * n..(i + 1) * n].to_vec())
            .collect();
        let predictions = self.crf.decode(&emissions)?;
        let mut result = Vec::new();
        let mut pending: Option<Entity> = None;
        for (i, &prediction) in predictions.iter().enumerate() {
            let label = &self.crf.labels[prediction];
            let prefix = label.as_bytes()[0];
            // ModelScope keeps the probability at the opening tag, including orphan I/E tags.
            if matches!(prefix, b'B' | b'S')
                && let Some(e) = pending.take()
            {
                result.push(e);
            }
            if matches!(prefix, b'B' | b'S' | b'I' | b'E') && pending.is_none() {
                let max = emissions[i]
                    .iter()
                    .copied()
                    .fold(f32::NEG_INFINITY, f32::max);
                let score = (emissions[i][prediction] - max).exp()
                    / emissions[i].iter().map(|v| (v - max).exp()).sum::<f32>();
                let typ = match &label[2..] {
                    "PER" => "PERSON",
                    "ORG" => "ORGANIZATION",
                    "LOC" => "LOCATION",
                    t => t,
                };
                pending = Some(entity(
                    RecognizerResult::new(
                        typ,
                        base + offsets[i].start,
                        base + offsets[i].end,
                        score as f64,
                    ),
                    "RaNER",
                ));
            }
            if matches!(prefix, b'I' | b'E' | b'S')
                && let Some(e) = pending.as_mut()
            {
                e.span.end = base + offsets[i].end;
            }
            if matches!(prefix, b'E' | b'S')
                && let Some(e) = pending.take()
            {
                result.push(e);
            }
        }
        if let Some(e) = pending {
            result.push(e);
        }
        Ok(result)
    }
}
impl Ner for Raner {
    fn analyze(&mut self, text: &str) -> Result<Vec<Entity>> {
        let mut boundaries: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
        boundaries.push(text.len());
        let mut results = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for start in (0..boundaries.len() - 1).step_by(400) {
            let end = (start + 450).min(boundaries.len() - 1);
            let base = boundaries[start];
            for e in self.window(&text[base..boundaries[end]], base)? {
                if seen.insert((e.entity_type.clone(), e.span.start, e.span.end)) {
                    results.push(e);
                }
            }
        }
        results.sort_by_key(|e| (e.span.start, std::cmp::Reverse(e.span.end)));
        Ok(results)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn viterbi_uses_transitions_and_end_scores() {
        let crf = Crf {
            labels: vec!["O".into(), "S-PER".into()],
            start: vec![0., 0.],
            end: vec![0., 3.],
            transitions: vec![vec![0., -10.], vec![-10., 0.]],
        };
        assert_eq!(
            crf.decode(&[vec![2., 1.], vec![2., 1.]]).unwrap(),
            vec![1, 1]
        );
        assert!(crf.decode(&[vec![f32::NAN, 1.]]).is_err());
    }
}
