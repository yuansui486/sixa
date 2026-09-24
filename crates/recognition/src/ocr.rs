use domain::{Error, Point, Result};
use image::{RgbaImage, imageops};
use ort::{
    session::{RunOptions, Session},
    value::Tensor,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    path::Path,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

fn model_error(error: impl std::fmt::Display) -> Error {
    Error::ModelsNotReady(format!("OCR 模型错误：{error}"))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OcrConfig {
    #[serde(default = "default_det_limit")]
    pub det_limit_side: u32,
    #[serde(default = "default_det_threshold")]
    pub det_threshold: f32,
    #[serde(default = "default_box_threshold")]
    pub box_threshold: f32,
    #[serde(default = "default_unclip")]
    pub unclip_ratio: f32,
    #[serde(default = "default_rec_width")]
    pub rec_max_width: u32,
    #[serde(default = "default_cls_threshold")]
    pub cls_threshold: f32,
    #[serde(default = "default_rec_threshold")]
    pub rec_threshold: f32,
}

fn default_det_limit() -> u32 {
    960
}
fn default_det_threshold() -> f32 {
    0.3
}
fn default_box_threshold() -> f32 {
    0.6
}
fn default_unclip() -> f32 {
    1.5
}
fn default_rec_width() -> u32 {
    640
}
fn default_cls_threshold() -> f32 {
    0.9
}
fn default_rec_threshold() -> f32 {
    0.5
}

impl Default for OcrConfig {
    fn default() -> Self {
        Self {
            det_limit_side: default_det_limit(),
            det_threshold: default_det_threshold(),
            box_threshold: default_box_threshold(),
            unclip_ratio: default_unclip(),
            rec_max_width: default_rec_width(),
            cls_threshold: default_cls_threshold(),
            rec_threshold: default_rec_threshold(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OcrLine {
    pub text: String,
    pub score: f32,
    pub polygon: Vec<Point>,
    pub rotation: f32,
}

pub struct OcrRun {
    cancelled: Arc<AtomicBool>,
    active: Arc<Mutex<Vec<Weak<RunOptions>>>>,
}

impl OcrRun {
    pub fn new() -> Result<Self> {
        Ok(Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            active: Arc::new(Mutex::new(Vec::new())),
        })
    }
    pub fn cancel(&self) -> Result<()> {
        self.cancelled.store(true, Ordering::SeqCst);
        let active = self
            .active
            .lock()
            .map_err(|_| model_error("OCR 取消状态异常"))?
            .clone();
        for options in active.into_iter().filter_map(|entry| entry.upgrade()) {
            options.terminate().map_err(model_error)?;
        }
        Ok(())
    }
    pub fn reset(&self) -> Result<()> {
        self.cancelled.store(false, Ordering::SeqCst);
        Ok(())
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
    pub(crate) fn begin(&self) -> Result<Arc<RunOptions>> {
        if self.is_cancelled() {
            return Err(Error::State("任务已取消".into()));
        }
        let options = Arc::new(RunOptions::new().map_err(model_error)?);
        self.active
            .lock()
            .map_err(|_| model_error("OCR 取消状态异常"))?
            .push(Arc::downgrade(&options));
        if self.is_cancelled() {
            options.terminate().map_err(model_error)?;
            return Err(Error::State("任务已取消".into()));
        }
        Ok(options)
    }
    pub(crate) fn finish(&self, options: &Arc<RunOptions>) -> Result<()> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| model_error("OCR 取消状态异常"))?;
        active.retain(|entry| {
            entry
                .upgrade()
                .is_some_and(|current| !Arc::ptr_eq(&current, options))
        });
        Ok(())
    }
}
impl Clone for OcrRun {
    fn clone(&self) -> Self {
        Self {
            cancelled: self.cancelled.clone(),
            active: self.active.clone(),
        }
    }
}

pub trait Ocr: Send {
    fn recognize(&mut self, image: &RgbaImage, run: &OcrRun) -> Result<Vec<OcrLine>>;
}

pub struct PpOcr {
    det: Session,
    cls: Session,
    rec: Session,
    characters: Vec<String>,
    config: OcrConfig,
}

impl PpOcr {
    pub fn load(dir: &Path) -> Result<Self> {
        let verified =
            crate::models::verify_model_with_required(dir, crate::models::OCR_MODEL_FILES)?;
        Self::load_verified(&verified)
    }
    pub fn load_verified(verified: &crate::models::VerifiedModel) -> Result<Self> {
        Self::load_with_progress(verified, &mut |_| {})
    }
    pub fn load_with_progress(
        verified: &crate::models::VerifiedModel,
        progress: &mut dyn FnMut(&'static str),
    ) -> Result<Self> {
        verified.require(crate::models::OCR_MODEL_FILES)?;
        let dir = verified.directory();
        progress("runtime");
        ort::init_from(crate::models::runtime_library_path(dir))
            .map_err(model_error)?
            .commit();
        let mut load = |name: &'static str| {
            progress(name);
            Session::builder()
                .map_err(model_error)?
                .with_intra_threads(1)
                .map_err(model_error)?
                .commit_from_file(dir.join(name))
                .map_err(model_error)
        };
        let characters = std::fs::read_to_string(dir.join("dict.txt"))?
            .lines()
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if characters.is_empty() || characters.len() > 100_000 {
            return Err(model_error("OCR 字符表为空或过大"));
        }
        let config: OcrConfig =
            serde_json::from_slice(&std::fs::read(dir.join("ocr-config.json"))?)
                .map_err(model_error)?;
        validate_config(&config)?;
        Ok(Self {
            det: load("det.onnx")?,
            cls: load("cls.onnx")?,
            rec: load("rec.onnx")?,
            characters,
            config,
        })
    }

    /// Runs every OCR graph once so a model is only reported ready after the
    /// runtime, model inputs, and model outputs have all been exercised.
    pub fn warm_up(&mut self) -> Result<()> {
        self.warm_up_with_run(&OcrRun::new()?)
    }

    pub fn warm_up_with_run(&mut self, run: &OcrRun) -> Result<()> {
        let image = RgbaImage::from_pixel(64, 64, image::Rgba([255, 255, 255, 255]));
        self.detect(&image, run)?;
        self.classify_batch(std::slice::from_ref(&image), run)?;
        self.recognize_batch(std::slice::from_ref(&image), run)?;
        Ok(())
    }

    fn detect(&mut self, image: &RgbaImage, run: &OcrRun) -> Result<Vec<Vec<Point>>> {
        let (input, width, height) = det_input(image, self.config.det_limit_side);
        let options = run.begin()?;
        let output = self
            .det
            .run_with_options(
                ort::inputs![
                    Tensor::from_array(([1, 3, height as usize, width as usize], input))
                        .map_err(model_error)?
                ],
                &options,
            )
            .map_err(model_error);
        run.finish(&options)?;
        let output = output?;
        let (shape, probabilities) = output[0].try_extract_tensor::<f32>().map_err(model_error)?;
        if shape.len() != 4 || shape[0] != 1 || shape[1] != 1 {
            return Err(model_error("检测模型输出维度应为 [1,1,H,W]"));
        }
        let output_height = shape[2] as usize;
        let output_width = shape[3] as usize;
        db_polygons(
            probabilities,
            output_width,
            output_height,
            self.config.det_threshold,
            self.config.box_threshold,
            self.config.unclip_ratio,
        )
    }

    fn classify_batch(&mut self, crops: &[RgbaImage], run: &OcrRun) -> Result<Vec<bool>> {
        if crops.is_empty() {
            return Ok(Vec::new());
        }
        let input = crops
            .iter()
            .flat_map(|crop| normalized_padded(crop, 48, 192))
            .collect::<Vec<_>>();
        let options = run.begin()?;
        let output = self
            .cls
            .run_with_options(
                ort::inputs![
                    Tensor::from_array(([crops.len(), 3, 48usize, 192usize], input))
                        .map_err(model_error)?
                ],
                &options,
            )
            .map_err(model_error);
        run.finish(&options)?;
        let output = output?;
        let (shape, logits) = output[0].try_extract_tensor::<f32>().map_err(model_error)?;
        if shape.len() != 2 || shape[0] != crops.len() as i64 || shape[1] < 2 {
            return Err(model_error("方向模型输出维度无效"));
        }
        let classes = shape[1] as usize;
        Ok(logits
            .chunks_exact(classes)
            .map(|row| {
                let (class, score) = argmax(row);
                class == 1 && score >= self.config.cls_threshold
            })
            .collect())
    }

    fn recognize_batch(&mut self, crops: &[RgbaImage], run: &OcrRun) -> Result<Vec<(String, f32)>> {
        if crops.is_empty() {
            return Ok(Vec::new());
        }
        let width = self.recognition_width(&crops[0]);
        let input = crops
            .iter()
            .flat_map(|crop| normalized_padded(crop, 48, width))
            .collect::<Vec<_>>();
        let options = run.begin()?;
        let output = self
            .rec
            .run_with_options(
                ort::inputs![
                    Tensor::from_array(([crops.len(), 3, 48usize, width as usize], input))
                        .map_err(model_error)?
                ],
                &options,
            )
            .map_err(model_error);
        run.finish(&options)?;
        let output = output?;
        let (shape, logits) = output[0].try_extract_tensor::<f32>().map_err(model_error)?;
        if shape.len() != 3 || shape[0] != crops.len() as i64 {
            return Err(model_error("识别模型输出维度应为 [1,T,C]"));
        }
        let time_steps = shape[1] as usize;
        let classes = shape[2] as usize;
        logits
            .chunks_exact(time_steps * classes)
            .map(|batch| ctc_decode(batch, time_steps, classes, &self.characters))
            .collect()
    }

    fn recognition_width(&self, crop: &RgbaImage) -> u32 {
        let ratio = crop.width() as f32 / crop.height().max(1) as f32;
        ((ratio * 48.0).ceil() as u32)
            .clamp(48, self.config.rec_max_width)
            .next_multiple_of(8)
            .min(self.config.rec_max_width)
    }
}

impl Ocr for PpOcr {
    fn recognize(&mut self, image: &RgbaImage, run: &OcrRun) -> Result<Vec<OcrLine>> {
        const BATCH_SIZE: usize = 32;
        let mut lines = Vec::new();
        let polygons = self.detect(image, run)?;
        for polygon_batch in polygons.chunks(BATCH_SIZE) {
            if run.is_cancelled() {
                return Err(Error::State("任务已取消".into()));
            }
            let mut crops = polygon_batch
                .iter()
                .map(|polygon| perspective_crop(image, polygon, 48))
                .collect::<Result<Vec<_>>>()?;
            let rotations = self.classify_batch(&crops, run)?;
            for (crop, rotated) in crops.iter_mut().zip(&rotations) {
                if *rotated {
                    imageops::rotate180_in_place(crop);
                }
            }
            let mut grouped = std::collections::BTreeMap::<u32, Vec<(usize, RgbaImage)>>::new();
            for (index, crop) in crops.into_iter().enumerate() {
                grouped
                    .entry(self.recognition_width(&crop))
                    .or_default()
                    .push((index, crop));
            }
            let mut recognized = vec![None; polygon_batch.len()];
            for entries in grouped.into_values() {
                let indexes = entries.iter().map(|(index, _)| *index).collect::<Vec<_>>();
                let batch = entries
                    .into_iter()
                    .map(|(_, crop)| crop)
                    .collect::<Vec<_>>();
                for (index, result) in indexes.into_iter().zip(self.recognize_batch(&batch, run)?) {
                    recognized[index] = Some(result);
                }
            }
            for ((polygon, rotated), (text, score)) in polygon_batch.iter().zip(rotations).zip(
                recognized
                    .into_iter()
                    .map(|result| result.expect("OCR batch result")),
            ) {
                if !text.trim().is_empty() && score >= self.config.rec_threshold {
                    lines.push(OcrLine {
                        text,
                        score,
                        polygon: polygon.clone(),
                        rotation: if rotated { 180.0 } else { 0.0 },
                    });
                }
            }
        }
        sort_reading_order(&mut lines);
        Ok(lines)
    }
}

fn validate_config(config: &OcrConfig) -> Result<()> {
    if config.det_limit_side < 32
        || config.det_limit_side > 4096
        || config.rec_max_width < 48
        || config.rec_max_width > 4096
        || ![
            config.det_threshold,
            config.box_threshold,
            config.cls_threshold,
            config.rec_threshold,
        ]
        .into_iter()
        .all(|v| v.is_finite() && (0.0..=1.0).contains(&v))
        || !config.unclip_ratio.is_finite()
        || !(1.0..=4.0).contains(&config.unclip_ratio)
    {
        return Err(model_error("OCR 配置数值无效"));
    }
    Ok(())
}

fn det_input(image: &RgbaImage, limit: u32) -> (Vec<f32>, u32, u32) {
    let scale = (limit as f32 / image.width().max(image.height()) as f32).min(1.0);
    let width = ((image.width() as f32 * scale).round() as u32)
        .max(32)
        .next_multiple_of(32);
    let height = ((image.height() as f32 * scale).round() as u32)
        .max(32)
        .next_multiple_of(32);
    let resized = imageops::resize(image, width, height, imageops::FilterType::Triangle);
    (normalize_det_chw(&resized, width, height), width, height)
}

fn normalized_padded(image: &RgbaImage, height: u32, width: u32) -> Vec<f32> {
    let ratio = image.width() as f32 / image.height().max(1) as f32;
    let resized_width = (height as f32 * ratio).ceil().clamp(1.0, width as f32) as u32;
    let resized = imageops::resize(image, resized_width, height, imageops::FilterType::Triangle);
    let mut canvas = RgbaImage::from_pixel(width, height, image::Rgba([255, 255, 255, 255]));
    imageops::overlay(&mut canvas, &resized, 0, 0);
    normalize_chw(&canvas, width, height)
}

fn normalize_det_chw(image: &RgbaImage, width: u32, height: u32) -> Vec<f32> {
    const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
    const STD: [f32; 3] = [0.229, 0.224, 0.225];
    let plane = width as usize * height as usize;
    let mut result = vec![0.0; plane * 3];
    for (x, y, pixel) in image.enumerate_pixels() {
        let index = y as usize * width as usize + x as usize;
        for channel in 0..3 {
            result[channel * plane + index] =
                (f32::from(pixel[channel]) / 255.0 - MEAN[channel]) / STD[channel];
        }
    }
    result
}

fn normalize_chw(image: &RgbaImage, width: u32, height: u32) -> Vec<f32> {
    let plane = width as usize * height as usize;
    let mut result = vec![0.0; plane * 3];
    for (x, y, pixel) in image.enumerate_pixels() {
        let index = y as usize * width as usize + x as usize;
        for channel in 0..3 {
            result[channel * plane + index] = (f32::from(pixel[channel]) / 255.0 - 0.5) / 0.5;
        }
    }
    result
}

pub fn ctc_decode(
    logits: &[f32],
    time_steps: usize,
    classes: usize,
    characters: &[String],
) -> Result<(String, f32)> {
    if time_steps == 0
        || classes < 2
        || logits.len() != time_steps * classes
        || !(characters.len() + 1..=characters.len() + 2).contains(&classes)
    {
        return Err(model_error("CTC 输出或字符表维度无效"));
    }
    let mut previous = usize::MAX;
    let mut text = String::new();
    let mut scores = Vec::new();
    for row in logits.chunks_exact(classes) {
        let (class, score) = argmax(row);
        if class != 0 && class != previous {
            if let Some(character) = characters.get(class - 1) {
                text.push_str(character);
            } else if classes == characters.len() + 2 && class == characters.len() + 1 {
                text.push(' ');
            } else {
                return Err(model_error("CTC 类别超出字符表"));
            }
            scores.push(score);
        }
        previous = class;
    }
    let score = if scores.is_empty() {
        0.0
    } else {
        scores.iter().sum::<f32>() / scores.len() as f32
    };
    Ok((text, score))
}

fn argmax(values: &[f32]) -> (usize, f32) {
    let mut best = 0usize;
    for index in 1..values.len() {
        if values[index] > values[best] {
            best = index;
        }
    }
    (best, values.get(best).copied().unwrap_or(0.0))
}

pub fn db_polygons(
    probabilities: &[f32],
    width: usize,
    height: usize,
    threshold: f32,
    box_threshold: f32,
    unclip_ratio: f32,
) -> Result<Vec<Vec<Point>>> {
    if width == 0 || height == 0 || probabilities.len() != width * height {
        return Err(model_error("DB 输出尺寸无效"));
    }
    let mut visited = vec![false; probabilities.len()];
    let mut polygons = Vec::new();
    for start in 0..probabilities.len() {
        if visited[start] || probabilities[start] < threshold {
            continue;
        }
        let mut queue = VecDeque::from([start]);
        visited[start] = true;
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (width, height, 0usize, 0usize);
        let mut score = 0.0;
        let mut count = 0usize;
        while let Some(index) = queue.pop_front() {
            let x = index % width;
            let y = index / width;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
            score += probabilities[index];
            count += 1;
            for (nx, ny) in [
                (x.wrapping_sub(1), y),
                (x + 1, y),
                (x, y.wrapping_sub(1)),
                (x, y + 1),
            ] {
                if nx < width && ny < height {
                    let next = ny * width + nx;
                    if !visited[next] && probabilities[next] >= threshold {
                        visited[next] = true;
                        queue.push_back(next);
                    }
                }
            }
        }
        if count < 3 || score / (count as f32) < box_threshold {
            continue;
        }
        let box_width = max_x.saturating_sub(min_x) + 1;
        let box_height = max_y.saturating_sub(min_y) + 1;
        if box_width < 3 || box_height < 3 {
            continue;
        }
        let area = (box_width * box_height) as f32;
        let perimeter = 2.0 * (box_width + box_height) as f32;
        let expand_x = ((area * unclip_ratio / perimeter).ceil() as usize).max(1);
        let expand_y = expand_x;
        let x0 = min_x.saturating_sub(expand_x) as f32 / width as f32;
        let y0 = min_y.saturating_sub(expand_y) as f32 / height as f32;
        let x1 = (max_x + 1 + expand_x).min(width) as f32 / width as f32;
        let y1 = (max_y + 1 + expand_y).min(height) as f32 / height as f32;
        polygons.push(vec![
            Point { x: x0, y: y0 },
            Point { x: x1, y: y0 },
            Point { x: x1, y: y1 },
            Point { x: x0, y: y1 },
        ]);
        if polygons.len() >= 1000 {
            break;
        }
    }
    polygons.sort_by(|a, b| {
        a[0].y
            .total_cmp(&b[0].y)
            .then_with(|| a[0].x.total_cmp(&b[0].x))
    });
    Ok(polygons)
}

fn perspective_crop(image: &RgbaImage, polygon: &[Point], target_height: u32) -> Result<RgbaImage> {
    if polygon.len() != 4 {
        return Err(Error::Invalid("OCR 区域必须为四边形".into()));
    }
    let points: Vec<(f32, f32)> = polygon
        .iter()
        .map(|p| (p.x * image.width() as f32, p.y * image.height() as f32))
        .collect();
    let distance =
        |a: (f32, f32), b: (f32, f32)| ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt();
    let source_width = distance(points[0], points[1]).max(distance(points[3], points[2]));
    let source_height = distance(points[0], points[3])
        .max(distance(points[1], points[2]))
        .max(1.0);
    let target_width = ((source_width / source_height) * target_height as f32)
        .round()
        .clamp(8.0, 4096.0) as u32;
    let mut output = RgbaImage::new(target_width, target_height);
    for y in 0..target_height {
        let v = (y as f32 + 0.5) / target_height as f32;
        for x in 0..target_width {
            let u = (x as f32 + 0.5) / target_width as f32;
            let sx = (1.0 - u) * (1.0 - v) * points[0].0
                + u * (1.0 - v) * points[1].0
                + u * v * points[2].0
                + (1.0 - u) * v * points[3].0;
            let sy = (1.0 - u) * (1.0 - v) * points[0].1
                + u * (1.0 - v) * points[1].1
                + u * v * points[2].1
                + (1.0 - u) * v * points[3].1;
            let px = sx
                .round()
                .clamp(0.0, image.width().saturating_sub(1) as f32) as u32;
            let py = sy
                .round()
                .clamp(0.0, image.height().saturating_sub(1) as f32) as u32;
            output.put_pixel(x, y, *image.get_pixel(px, py));
        }
    }
    Ok(output)
}

fn sort_reading_order(lines: &mut [OcrLine]) {
    lines.sort_by(|a, b| {
        let ay = a.polygon.iter().map(|p| p.y).sum::<f32>() / a.polygon.len() as f32;
        let by = b.polygon.iter().map(|p| p.y).sum::<f32>() / b.polygon.len() as f32;
        let ax = a.polygon.iter().map(|p| p.x).sum::<f32>() / a.polygon.len() as f32;
        let bx = b.polygon.iter().map(|p| p.x).sum::<f32>() / b.polygon.len() as f32;
        let a_height = a
            .polygon
            .iter()
            .map(|p| p.y)
            .fold((1.0f32, 0.0f32), |(min, max), y| (min.min(y), max.max(y)));
        let b_height = b
            .polygon
            .iter()
            .map(|p| p.y)
            .fold((1.0f32, 0.0f32), |(min, max), y| (min.min(y), max.max(y)));
        let same_row =
            (ay - by).abs() <= (a_height.1 - a_height.0).min(b_height.1 - b_height.0) * 0.5;
        if same_row {
            ax.total_cmp(&bx)
        } else {
            ay.total_cmp(&by)
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctc_removes_blank_and_duplicates() {
        let logits = [
            0.9, 0.1, 0.0, 0.1, 0.8, 0.1, 0.1, 0.7, 0.2, 0.8, 0.1, 0.1, 0.1, 0.1, 0.9,
        ];
        let (text, score) = ctc_decode(&logits, 5, 3, &["你".into(), "好".into()]).unwrap();
        assert_eq!(text, "你好");
        assert!((score - 0.85).abs() < 1e-6);
    }

    #[test]
    fn ctc_maps_paddleocr_trailing_space_class() {
        let logits = vec![0.0, 0.1, 0.2, 0.9];
        let (text, score) = ctc_decode(&logits, 1, 4, &["你".into(), "好".into()]).unwrap();
        assert_eq!(text, " ");
        assert!((score - 0.9).abs() < 1e-6);
    }

    #[test]
    fn ctc_preserves_whitespace_dictionary_characters() {
        let characters = "你\n\u{3000}\n好\n"
            .lines()
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let logits = vec![0.0, 0.1, 0.9, 0.2, 0.0];
        let (text, score) = ctc_decode(&logits, 1, 5, &characters).unwrap();
        assert_eq!(text, "\u{3000}");
        assert!((score - 0.9).abs() < 1e-6);
    }

    #[test]
    fn db_groups_pixels_and_normalizes_coordinates() {
        let mut map = vec![0.0; 10 * 8];
        for y in 2..6 {
            for x in 3..8 {
                map[y * 10 + x] = 0.95;
            }
        }
        let polygons = db_polygons(&map, 10, 8, 0.3, 0.6, 1.5).unwrap();
        assert_eq!(polygons.len(), 1);
        assert!(
            polygons[0]
                .iter()
                .all(|p| (0.0..=1.0).contains(&p.x) && (0.0..=1.0).contains(&p.y))
        );
    }
}
