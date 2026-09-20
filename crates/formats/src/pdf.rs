use ab_glyph::FontArc;
use domain::{Error, PdfMode, Point, RegionDto, Result};
use image::{Rgba, RgbaImage, imageops};
use mupdf::pdf::{PdfDocument, PdfWriteOptions};
use mupdf::{Colorspace, InsertImageOptions, Matrix, PageImageSource, Pixmap, Rect, Size};

const MAX_PAGES: i32 = 200;
const TARGET_SCALE: f32 = 2.0;
const MAX_PAGE_PIXELS: f32 = 12_000_000.0;

fn pdf_error(error: impl std::fmt::Display) -> Error {
    Error::Invalid(format!("PDF 处理失败：{error}"))
}

pub fn validate(bytes: &[u8]) -> Result<()> {
    page_count(bytes)?;
    Ok(())
}

pub fn page_count(bytes: &[u8]) -> Result<usize> {
    let document = PdfDocument::from_copied_bytes(bytes).map_err(pdf_error)?;
    let pages = document.page_count().map_err(pdf_error)?;
    if pages <= 0 || pages > MAX_PAGES {
        return Err(Error::Invalid(format!(
            "PDF 页数必须在 1 到 {MAX_PAGES} 之间"
        )));
    }
    Ok(pages as usize)
}

pub fn render_pages(bytes: &[u8]) -> Result<Vec<RgbaImage>> {
    render_pages_interruptible(bytes, &mut || Ok(()))
}

pub fn render_pages_interruptible(
    bytes: &[u8],
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<Vec<RgbaImage>> {
    Ok(render_pages_with_bounds(bytes, check)?
        .into_iter()
        .map(|(image, _)| image)
        .collect())
}

pub fn redact(
    bytes: &[u8],
    regions: &[RegionDto],
    font: Option<&FontArc>,
    mode: PdfMode,
) -> Result<Vec<u8>> {
    redact_interruptible(bytes, regions, font, mode, &mut || Ok(()))
}

pub fn redact_interruptible(
    bytes: &[u8],
    regions: &[RegionDto],
    font: Option<&FontArc>,
    mode: PdfMode,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<Vec<u8>> {
    check()?;
    validate_regions(bytes, regions)?;
    match mode {
        PdfMode::SafeRebuild => safe_rebuild(bytes, regions, font, check),
        PdfMode::Fidelity => fidelity_redact(bytes, regions, font, check),
    }
}

fn render_pages_with_bounds(
    bytes: &[u8],
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<Vec<(RgbaImage, Rect)>> {
    let document = PdfDocument::from_copied_bytes(bytes).map_err(pdf_error)?;
    let count = document.page_count().map_err(pdf_error)?;
    if count <= 0 || count > MAX_PAGES {
        return Err(Error::Invalid(format!(
            "PDF 页数必须在 1 到 {MAX_PAGES} 之间"
        )));
    }
    let mut result = Vec::with_capacity(count as usize);
    for index in 0..count {
        check()?;
        let page = document.load_page(index).map_err(pdf_error)?;
        let bounds = page.bounds().map_err(pdf_error)?;
        let area = (bounds.width() * bounds.height()).max(1.0);
        let scale = TARGET_SCALE.min((MAX_PAGE_PIXELS / area).sqrt());
        let pixmap = page
            .to_pixmap(
                &Matrix::new_scale(scale, scale),
                &Colorspace::device_rgb(),
                false,
                false,
            )
            .map_err(pdf_error)?;
        result.push((pixmap_to_image(&pixmap)?, bounds));
        check()?;
    }
    Ok(result)
}

fn validate_regions(bytes: &[u8], regions: &[RegionDto]) -> Result<()> {
    let document = PdfDocument::from_copied_bytes(bytes).map_err(pdf_error)?;
    let pages = document.page_count().map_err(pdf_error)? as u32;
    for region in regions {
        region.validate(pages)?;
    }
    Ok(())
}

fn safe_rebuild(
    bytes: &[u8],
    regions: &[RegionDto],
    font: Option<&FontArc>,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<Vec<u8>> {
    let rendered = render_pages_with_bounds(bytes, check)?;
    let mut images = rendered
        .iter()
        .map(|(image, _)| image.clone())
        .collect::<Vec<_>>();
    crate::raster::redact_pages_interruptible(&mut images, regions, font, check)?;

    let mut output = PdfDocument::new();
    for (image, (_, bounds)) in images.iter().zip(rendered.iter()) {
        check()?;
        let mut page = output
            .new_page(Size::new(bounds.width(), bounds.height()))
            .map_err(pdf_error)?;
        let pixmap = image_to_pixmap(image)?;
        page.insert_image(
            &mut output,
            Rect::new(0.0, 0.0, bounds.width(), bounds.height()),
            PageImageSource::Pixmap(&pixmap),
            InsertImageOptions::default(),
        )
        .map_err(pdf_error)?;
        check()?;
    }
    write_and_verify(output, rendered.len(), &[], check)
}

fn fidelity_redact(
    bytes: &[u8],
    regions: &[RegionDto],
    font: Option<&FontArc>,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<Vec<u8>> {
    let rendered = render_pages_with_bounds(bytes, check)?;
    let mut redacted_images = rendered
        .iter()
        .map(|(image, _)| image.clone())
        .collect::<Vec<_>>();
    crate::raster::redact_pages_interruptible(&mut redacted_images, regions, font, check)?;

    let mut document = PdfDocument::from_copied_bytes(bytes).map_err(pdf_error)?;
    let embedded = document.embedded_files().map_err(pdf_error)?;
    for file in embedded {
        document
            .delete_embedded_file(&file.name)
            .map_err(pdf_error)?;
    }

    for (page_index, (image, (_, bounds))) in
        redacted_images.iter().zip(rendered.iter()).enumerate()
    {
        check()?;
        let page_regions = regions
            .iter()
            .filter(|region| region.selected && region.page == page_index as u32)
            .collect::<Vec<_>>();
        if page_regions.is_empty() {
            continue;
        }
        let mut page = document
            .load_pdf_page(page_index as i32)
            .map_err(pdf_error)?;
        let mut redaction_rects = Vec::new();
        for region in &page_regions {
            check()?;
            let fallback = region_rect(region, bounds);
            if region.text.trim().is_empty() {
                redaction_rects.push(fallback);
                continue;
            }
            let hits = page.search(region.text.trim(), 128).map_err(pdf_error)?;
            if hits.is_empty() {
                redaction_rects.push(fallback);
            } else {
                // Text can occur more than once on a page. Redacting every search
                // hit would remove content the user did not select, so choose the
                // hit which overlaps the reviewed region most closely.
                let selected = hits
                    .iter()
                    .map(quad_rect)
                    .max_by(|left, right| {
                        overlap_ratio(left, &fallback).total_cmp(&overlap_ratio(right, &fallback))
                    })
                    .filter(|rect| overlap_ratio(rect, &fallback) > 0.0)
                    .unwrap_or(fallback);
                redaction_rects.push(selected);
            }
        }
        for rect in redaction_rects {
            check()?;
            page.add_redact_annotation(rect).map_err(pdf_error)?;
        }
        page.apply_redactions().map_err(pdf_error)?;

        for region in page_regions {
            check()?;
            let (x, y, width, height) = pixel_bounds(region, image.width(), image.height());
            if width == 0 || height == 0 {
                continue;
            }
            let patch = imageops::crop_imm(image, x, y, width, height).to_image();
            let pixmap = image_to_pixmap(&patch)?;
            page.insert_image(
                &mut document,
                region_rect(region, bounds),
                PageImageSource::Pixmap(&pixmap),
                InsertImageOptions::default(),
            )
            .map_err(pdf_error)?;
        }
        check()?;
    }

    let sensitive = regions
        .iter()
        .filter(|region| region.selected && !region.text.trim().is_empty())
        .map(|region| region.text.as_str())
        .collect::<Vec<_>>();
    write_and_verify(document, rendered.len(), &sensitive, check)
}

fn write_and_verify(
    document: PdfDocument,
    expected_pages: usize,
    sensitive: &[&str],
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<Vec<u8>> {
    check()?;
    let mut options = PdfWriteOptions::default();
    options
        .set_compress(true)
        .set_compress_images(true)
        .set_compress_fonts(true)
        .set_clean(true)
        .set_sanitize(true)
        .set_garbage_level(4);
    let mut bytes = Vec::new();
    document
        .write_to_with_options(&mut bytes, options)
        .map_err(pdf_error)?;
    check()?;

    let verified = PdfDocument::from_copied_bytes(&bytes).map_err(pdf_error)?;
    if verified.page_count().map_err(pdf_error)? as usize != expected_pages
        || !verified.embedded_files().map_err(pdf_error)?.is_empty()
    {
        return Err(Error::State("PDF 二次安全检查未通过，禁止导出".into()));
    }
    for index in 0..expected_pages {
        check()?;
        let page = verified.load_page(index as i32).map_err(pdf_error)?;
        for value in sensitive {
            check()?;
            if !value.trim().is_empty() && !page.search(value, 1).map_err(pdf_error)?.is_empty() {
                return Err(Error::State(format!(
                    "PDF 二次提取仍发现待脱敏内容（第 {} 页），禁止导出",
                    index + 1
                )));
            }
        }
    }
    Ok(bytes)
}

fn region_rect(region: &RegionDto, bounds: &Rect) -> Rect {
    let (min_x, min_y, max_x, max_y) = normalized_bounds(&region.polygon);
    Rect::new(
        bounds.x0 + min_x * bounds.width(),
        bounds.y0 + min_y * bounds.height(),
        bounds.x0 + max_x * bounds.width(),
        bounds.y0 + max_y * bounds.height(),
    )
}

fn pixel_bounds(region: &RegionDto, width: u32, height: u32) -> (u32, u32, u32, u32) {
    let (min_x, min_y, max_x, max_y) = normalized_bounds(&region.polygon);
    let x0 = (min_x * width as f32).floor().clamp(0.0, width as f32) as u32;
    let y0 = (min_y * height as f32).floor().clamp(0.0, height as f32) as u32;
    let x1 = (max_x * width as f32).ceil().clamp(0.0, width as f32) as u32;
    let y1 = (max_y * height as f32).ceil().clamp(0.0, height as f32) as u32;
    (x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0))
}

fn normalized_bounds(points: &[Point]) -> (f32, f32, f32, f32) {
    points.iter().fold(
        (1.0, 1.0, 0.0, 0.0),
        |(min_x, min_y, max_x, max_y), point| {
            (
                min_x.min(point.x),
                min_y.min(point.y),
                max_x.max(point.x),
                max_y.max(point.y),
            )
        },
    )
}

fn quad_rect(quad: &mupdf::Quad) -> Rect {
    let min_x = quad.ul.x.min(quad.ur.x).min(quad.ll.x).min(quad.lr.x);
    let max_x = quad.ul.x.max(quad.ur.x).max(quad.ll.x).max(quad.lr.x);
    let min_y = quad.ul.y.min(quad.ur.y).min(quad.ll.y).min(quad.lr.y);
    let max_y = quad.ul.y.max(quad.ur.y).max(quad.ll.y).max(quad.lr.y);
    Rect::new(min_x, min_y, max_x, max_y)
}

fn overlap_ratio(left: &Rect, right: &Rect) -> f32 {
    let width = (left.x1.min(right.x1) - left.x0.max(right.x0)).max(0.0);
    let height = (left.y1.min(right.y1) - left.y0.max(right.y0)).max(0.0);
    let intersection = width * height;
    let union = left.width() * left.height() + right.width() * right.height() - intersection;
    if union <= 0.0 {
        0.0
    } else {
        intersection / union
    }
}

fn pixmap_to_image(pixmap: &Pixmap) -> Result<RgbaImage> {
    let channels = pixmap.n() as usize;
    if channels < 3 {
        return Err(Error::Invalid("PDF 页面颜色空间不支持".into()));
    }
    let mut image = RgbaImage::new(pixmap.width(), pixmap.height());
    let stride = pixmap.stride().unsigned_abs();
    for y in 0..pixmap.height() as usize {
        let row = &pixmap.samples()[y * stride..y * stride + pixmap.width() as usize * channels];
        for x in 0..pixmap.width() as usize {
            let offset = x * channels;
            image.put_pixel(
                x as u32,
                y as u32,
                Rgba([row[offset], row[offset + 1], row[offset + 2], 255]),
            );
        }
    }
    Ok(image)
}

fn image_to_pixmap(image: &RgbaImage) -> Result<Pixmap> {
    let mut pixmap = Pixmap::new_with_w_h(
        &Colorspace::device_rgb(),
        image.width() as i32,
        image.height() as i32,
        false,
    )
    .map_err(pdf_error)?;
    let stride = pixmap.stride().unsigned_abs();
    for y in 0..image.height() as usize {
        let row = &mut pixmap.samples_mut()[y * stride..y * stride + image.width() as usize * 3];
        for x in 0..image.width() as usize {
            let pixel = image.get_pixel(x as u32, y as u32);
            row[x * 3..x * 3 + 3].copy_from_slice(&pixel.0[..3]);
        }
    }
    Ok(pixmap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::RegionSource;
    use mupdf::pdf::EmbeddedFileOptions;
    use mupdf::shape::{Shape, TextOptions};
    use uuid::Uuid;

    fn fixture() -> Vec<u8> {
        let mut document = PdfDocument::new();
        let mut page = document.new_page(Size::new(300.0, 200.0)).unwrap();
        let font = std::fs::read(r"C:\Windows\Fonts\arial.ttf").unwrap();
        let text_options = TextOptions {
            fontname: "fixture-font".into(),
            fontfile: Some(&font),
            ..TextOptions::default()
        };
        {
            let mut shape = Shape::new(&mut page).unwrap();
            shape
                .insert_text(
                    mupdf::Point::new(30.0, 55.0),
                    "SECRET-13812345678",
                    &text_options,
                )
                .unwrap()
                .insert_text(mupdf::Point::new(190.0, 165.0), "PUBLIC", &text_options)
                .unwrap()
                .commit(&mut document, true)
                .unwrap();
        }
        document
            .add_embedded_file(
                "source",
                b"private attachment",
                EmbeddedFileOptions::new("source.txt"),
            )
            .unwrap();
        let mut bytes = Vec::new();
        document.write_to(&mut bytes).unwrap();
        bytes
    }

    fn sensitive_region() -> RegionDto {
        RegionDto {
            id: Uuid::new_v4(),
            page: 0,
            polygon: vec![
                Point { x: 0.05, y: 0.12 },
                Point { x: 0.58, y: 0.12 },
                Point { x: 0.58, y: 0.34 },
                Point { x: 0.05, y: 0.34 },
            ],
            entity_id: None,
            selected: true,
            source: RegionSource::Manual,
            text: "SECRET-13812345678".into(),
            score: None,
            rotation: 0.0,
            replacement: None,
        }
    }

    fn search(bytes: &[u8], value: &str) -> bool {
        let document = PdfDocument::from_copied_bytes(bytes).unwrap();
        let page = document.load_page(0).unwrap();
        !page.search(value, 8).unwrap().is_empty()
    }

    #[test]
    fn safe_rebuild_removes_text_layer_and_attachments() {
        let output = redact(
            &fixture(),
            &[sensitive_region()],
            None,
            PdfMode::SafeRebuild,
        )
        .unwrap();
        assert!(!search(&output, "SECRET-13812345678"));
        assert!(!search(&output, "PUBLIC"));
        assert!(
            PdfDocument::from_copied_bytes(&output)
                .unwrap()
                .embedded_files()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn fidelity_removes_sensitive_text_and_attachment_but_keeps_public_text() {
        let source = fixture();
        let before = render_pages(&source).unwrap();
        let output = redact(&source, &[sensitive_region()], None, PdfMode::Fidelity).unwrap();
        assert!(!search(&output, "SECRET-13812345678"));
        assert!(search(&output, "PUBLIC"));
        assert!(
            PdfDocument::from_copied_bytes(&output)
                .unwrap()
                .embedded_files()
                .unwrap()
                .is_empty()
        );

        let after = render_pages(&output).unwrap();
        for (x, y) in [(10, 10), (500, 300), (580, 380)] {
            assert_eq!(before[0].get_pixel(x, y), after[0].get_pixel(x, y));
        }
    }

    #[test]
    fn overlap_ratio_prefers_the_reviewed_location() {
        let reviewed = Rect::new(10.0, 10.0, 40.0, 30.0);
        let matching = Rect::new(12.0, 11.0, 39.0, 29.0);
        let duplicate = Rect::new(150.0, 120.0, 190.0, 140.0);
        assert!(overlap_ratio(&matching, &reviewed) > 0.7);
        assert_eq!(overlap_ratio(&duplicate, &reviewed), 0.0);
    }
}
