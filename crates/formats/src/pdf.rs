use ab_glyph::FontArc;
use domain::{Error, PdfMode, Point, RegionDto, Result};
use image::{Rgba, RgbaImage, imageops};
use mupdf::pdf::{PdfDocument, PdfWriteOptions};
use mupdf::{Colorspace, InsertImageOptions, Matrix, PageImageSource, Pixmap, Rect, Size};
use std::io::Write;

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
    static CONFIGURE: std::sync::Once = std::sync::Once::new();
    CONFIGURE.call_once(|| {
        // The native resource cache is shared across renderer threads. PDF
        // objects and active page pixels remain outside this cache budget.
        let _ = mupdf::set_store_max_size(64 * 1024 * 1024);
    });
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
    page_count(bytes)?;
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
        result.push(render_document_page(&document, index, None)?);
        check()?;
    }
    Ok(result)
}

fn render_document_page(
    document: &PdfDocument,
    index: i32,
    max_dimension: Option<u32>,
) -> Result<(RgbaImage, Rect)> {
    let page = document.load_page(index).map_err(pdf_error)?;
    let bounds = page.bounds().map_err(pdf_error)?;
    let area = (bounds.width() * bounds.height()).max(1.0);
    let mut scale = TARGET_SCALE.min((MAX_PAGE_PIXELS / area).sqrt());
    if let Some(limit) = max_dimension {
        scale = scale.min(limit as f32 / bounds.width().max(bounds.height()).max(1.0));
    }
    let pixmap = page
        .to_pixmap(
            &Matrix::new_scale(scale, scale),
            &Colorspace::device_rgb(),
            false,
            false,
        )
        .map_err(pdf_error)?;
    Ok((pixmap_to_image(&pixmap)?, bounds))
}

/// Metadata only: no page pixels are allocated.
pub fn page_dimensions(bytes: &[u8]) -> Result<Vec<(u32, u32)>> {
    let count = page_count(bytes)?;
    let document = PdfDocument::from_copied_bytes(bytes).map_err(pdf_error)?;
    (0..count)
        .map(|index| {
            let bounds = document
                .load_page(index as i32)
                .map_err(pdf_error)?
                .bounds()
                .map_err(pdf_error)?;
            let scale = TARGET_SCALE
                .min((MAX_PAGE_PIXELS / (bounds.width() * bounds.height()).max(1.0)).sqrt());
            Ok((
                (bounds.width() * scale).ceil().max(1.0) as u32,
                (bounds.height() * scale).ceil().max(1.0) as u32,
            ))
        })
        .collect()
}

pub fn render_page(bytes: &[u8], index: u32, max_dimension: Option<u32>) -> Result<RgbaImage> {
    if index as usize >= page_count(bytes)? {
        return Err(Error::Invalid("页面不存在".into()));
    }
    let document = PdfDocument::from_copied_bytes(bytes).map_err(pdf_error)?;
    Ok(render_document_page(&document, index as i32, max_dimension)?.0)
}

pub fn visit_pages(
    bytes: &[u8],
    visit: &mut dyn FnMut(u32, RgbaImage) -> Result<()>,
) -> Result<()> {
    let count = page_count(bytes)?;
    let document = PdfDocument::from_copied_bytes(bytes).map_err(pdf_error)?;
    for index in 0..count {
        visit(
            index as u32,
            render_document_page(&document, index as i32, None)?.0,
        )?;
    }
    Ok(())
}

/// Render just one draft page through the same redaction helpers as export.
pub fn draft_page(
    bytes: &[u8],
    index: u32,
    max_dimension: u32,
    regions: &[RegionDto],
    mode: PdfMode,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<RgbaImage> {
    check()?;
    if index as usize >= page_count(bytes)? {
        return Err(Error::Invalid("页面不存在".into()));
    }
    validate_regions(bytes, regions)?;
    let font = crate::fonts::replacement_font()?;
    let mut document = PdfDocument::from_copied_bytes(bytes).map_err(pdf_error)?;
    let image = match mode {
        PdfMode::SafeRebuild => {
            let mut output = PdfDocument::new();
            append_safe_page(
                &document,
                &mut output,
                index as usize,
                regions,
                Some(&font),
                check,
            )?;
            render_document_page(&output, 0, Some(max_dimension))?.0
        }
        PdfMode::Fidelity => {
            apply_fidelity_page(&mut document, index as usize, regions, Some(&font), check)?;
            render_document_page(&document, index as i32, Some(max_dimension))?.0
        }
    };
    check()?;
    Ok(image)
}

fn local_regions(regions: &[RegionDto], index: u32) -> Vec<RegionDto> {
    regions
        .iter()
        .filter(|region| region.page == index)
        .cloned()
        .map(|mut region| {
            region.page = 0;
            region
        })
        .collect()
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
    let count = page_count(bytes)?;
    let source = PdfDocument::from_copied_bytes(bytes).map_err(pdf_error)?;
    let mut output = PdfDocument::new();
    for index in 0..count {
        append_safe_page(&source, &mut output, index, regions, font, check)?;
    }
    write_and_verify(output, count, &[], check)
}

fn append_safe_page(
    source: &PdfDocument,
    output: &mut PdfDocument,
    index: usize,
    regions: &[RegionDto],
    font: Option<&FontArc>,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<()> {
    check()?;
    let (mut image, bounds) = render_document_page(source, index as i32, None)?;
    crate::raster::redact_pages_interruptible(
        std::slice::from_mut(&mut image),
        &local_regions(regions, index as u32),
        font,
        check,
    )?;
    let mut page = output
        .new_page(Size::new(bounds.width(), bounds.height()))
        .map_err(pdf_error)?;
    let image_xref = add_compressed_image(output, &image)?;
    page.insert_image(
        output,
        Rect::new(0.0, 0.0, bounds.width(), bounds.height()),
        PageImageSource::ExistingXref(image_xref),
        InsertImageOptions::default(),
    )
    .map_err(pdf_error)?;
    check()?;
    Ok(())
}

fn fidelity_redact(
    bytes: &[u8],
    regions: &[RegionDto],
    font: Option<&FontArc>,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<Vec<u8>> {
    let count = page_count(bytes)?;
    let mut document = PdfDocument::from_copied_bytes(bytes).map_err(pdf_error)?;
    let embedded = document.embedded_files().map_err(pdf_error)?;
    for file in embedded {
        document
            .delete_embedded_file(&file.name)
            .map_err(pdf_error)?;
    }

    for page_index in 0..count {
        apply_fidelity_page(&mut document, page_index, regions, font, check)?;
    }

    let sensitive = regions
        .iter()
        .filter(|region| region.selected && !region.text.trim().is_empty())
        .map(|region| region.text.as_str())
        .collect::<Vec<_>>();
    write_and_verify(document, count, &sensitive, check)
}

fn apply_fidelity_page(
    document: &mut PdfDocument,
    page_index: usize,
    regions: &[RegionDto],
    font: Option<&FontArc>,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<()> {
    check()?;
    let page_regions = regions
        .iter()
        .filter(|region| region.selected && region.page == page_index as u32)
        .collect::<Vec<_>>();
    if page_regions.is_empty() {
        return Ok(());
    }
    let (mut image, bounds) = render_document_page(document, page_index as i32, None)?;
    crate::raster::redact_pages_interruptible(
        std::slice::from_mut(&mut image),
        &local_regions(regions, page_index as u32),
        font,
        check,
    )?;
    let mut page = document
        .load_pdf_page(page_index as i32)
        .map_err(pdf_error)?;
    let mut redaction_rects = Vec::new();
    for region in &page_regions {
        check()?;
        let fallback = region_rect(region, &bounds);
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
        let patch = imageops::crop_imm(&image, x, y, width, height).to_image();
        let image_xref = add_compressed_image(document, &patch)?;
        page.insert_image(
            document,
            region_rect(region, &bounds),
            PageImageSource::ExistingXref(image_xref),
            InsertImageOptions::default(),
        )
        .map_err(pdf_error)?;
    }
    check()?;
    Ok(())
}

/// Insert a lossless compressed stream immediately. MuPDF's generic PNG image
/// insertion decodes into an uncompressed PDF stream until final serialization;
/// that would accumulate full RGB pages despite releasing our Rust images.
fn add_compressed_image(document: &mut PdfDocument, image: &RgbaImage) -> Result<i32> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut row = Vec::with_capacity(image.width() as usize * 3);
    for pixels in image.rows() {
        row.clear();
        for pixel in pixels {
            row.extend_from_slice(&pixel.0[..3]);
        }
        encoder.write_all(&row)?;
    }
    let compressed = encoder.finish()?;
    let dict = document.new_object_from_str(&format!(
        "<< /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode >>",
        image.width(), image.height()
    )).map_err(pdf_error)?;
    let buffer = mupdf::Buffer::from_copied_bytes(&compressed).map_err(pdf_error)?;
    document
        .add_stream(&buffer, Some(&dict), true)
        .map_err(pdf_error)?
        .as_indirect()
        .map_err(pdf_error)
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
        let font = crate::fonts::CJK_FONT_BYTES;
        let text_options = TextOptions {
            fontname: "fixture-font".into(),
            fontfile: Some(font),
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

    #[test]
    fn single_page_draft_matches_export_in_both_pdf_modes() {
        let source = fixture();
        let font = crate::fonts::replacement_font().unwrap();
        let mut region = sensitive_region();
        region.replacement = Some("某人".into());
        for mode in [PdfMode::SafeRebuild, PdfMode::Fidelity] {
            let draft = draft_page(
                &source,
                0,
                420,
                std::slice::from_ref(&region),
                mode,
                &mut || Ok(()),
            )
            .unwrap();
            let exported =
                redact(&source, std::slice::from_ref(&region), Some(&font), mode).unwrap();
            let actual = render_page(&exported, 0, Some(420)).unwrap();
            assert_eq!(
                draft, actual,
                "draft and export must share pixel processing for {mode:?}"
            );
        }
        assert!(draft_page(&source, 1, 420, &[], PdfMode::SafeRebuild, &mut || Ok(())).is_err());
        let mut checks = 0;
        assert!(
            draft_page(&source, 0, 420, &[region], PdfMode::Fidelity, &mut || {
                checks += 1;
                if checks > 2 {
                    Err(Error::State("cancelled".into()))
                } else {
                    Ok(())
                }
            })
            .is_err()
        );
        assert!(checks > 2);
    }

    #[test]
    fn rotated_middle_page_draft_matches_export_and_preserves_other_pages() {
        let mut document = PdfDocument::new();
        for index in 0..3 {
            let image = RgbaImage::from_fn(240, 160, |x, y| {
                Rgba([(x % 230) as u8, (y % 230) as u8, (index * 60) as u8, 255])
            });
            let mut page = document.new_page(Size::new(120.0, 80.0)).unwrap();
            let xref = add_compressed_image(&mut document, &image).unwrap();
            page.insert_image(
                &mut document,
                Rect::new(0.0, 0.0, 120.0, 80.0),
                PageImageSource::ExistingXref(xref),
                InsertImageOptions::default(),
            )
            .unwrap();
            if index == 1 {
                page.set_rotation(90).unwrap();
            }
        }
        let mut source = Vec::new();
        document.write_to(&mut source).unwrap();
        let mut region = sensitive_region();
        region.page = 1;
        region.text.clear();
        region.rotation = 180.0;
        region.replacement = Some("某公司".into());
        region.polygon = vec![
            Point { x: 0.2, y: 0.2 },
            Point { x: 0.7, y: 0.2 },
            Point { x: 0.7, y: 0.65 },
            Point { x: 0.2, y: 0.65 },
        ];
        let font = crate::fonts::replacement_font().unwrap();
        for mode in [PdfMode::SafeRebuild, PdfMode::Fidelity] {
            let draft = draft_page(
                &source,
                1,
                200,
                std::slice::from_ref(&region),
                mode,
                &mut || Ok(()),
            )
            .unwrap();
            let output = redact(&source, std::slice::from_ref(&region), Some(&font), mode).unwrap();
            assert_eq!(
                draft,
                render_page(&output, 1, Some(200)).unwrap(),
                "rotated draft mismatch for {mode:?}"
            );
            assert_eq!(page_count(&output).unwrap(), 3);
            if mode == PdfMode::Fidelity {
                for page in [0, 2] {
                    assert_eq!(
                        render_page(&source, page, Some(200)).unwrap(),
                        render_page(&output, page, Some(200)).unwrap()
                    );
                }
                let before = render_page(&source, 1, Some(200)).unwrap();
                assert_ne!(
                    before, draft,
                    "manual regions with empty OCR text must still redact"
                );
                for (x, y) in [(3, 3), (120, 3), (3, 190), (120, 190)] {
                    assert_eq!(before.get_pixel(x, y), draft.get_pixel(x, y));
                }
            }
        }
    }

    #[test]
    fn manifest_single_page_and_streaming_preserve_page_geometry() {
        let mut document = PdfDocument::new();
        for index in 0..20 {
            document
                .new_page(Size::new(60.0 + index as f32, 80.0))
                .unwrap();
        }
        let mut bytes = Vec::new();
        document.write_to(&mut bytes).unwrap();
        let dimensions = page_dimensions(&bytes).unwrap();
        assert_eq!(dimensions.len(), 20);
        assert_eq!(dimensions[19], (158, 160));
        let preview = render_page(&bytes, 19, Some(80)).unwrap();
        assert_eq!((preview.width(), preview.height()), (79, 80));
        assert!(render_page(&bytes, 20, Some(80)).is_err());
        let mut visited = Vec::new();
        visit_pages(&bytes, &mut |index, image| {
            visited.push(index);
            assert_eq!(image.dimensions(), dimensions[index as usize]);
            Ok(())
        })
        .unwrap();
        assert_eq!(visited, (0..20).collect::<Vec<_>>());
        let output = redact(&bytes, &[], None, PdfMode::SafeRebuild).unwrap();
        assert_eq!(page_dimensions(&output).unwrap(), dimensions);
        let mut visited_before_cancel = 0;
        assert!(
            visit_pages(&bytes, &mut |_, _| {
                visited_before_cancel += 1;
                Err(Error::State("cancelled".into()))
            })
            .is_err()
        );
        assert_eq!(visited_before_cancel, 1);
    }

    #[test]
    fn output_page_pixels_are_compressed_before_final_serialization() {
        let mut document = PdfDocument::new();
        let image = RgbaImage::from_pixel(1200, 1600, Rgba([240, 245, 250, 255]));
        let xref = add_compressed_image(&mut document, &image).unwrap();
        let retained = document.xref_raw_stream(xref).unwrap();
        assert!(
            retained.len() < 100_000,
            "a flat page must not retain its 5.76 MB RGB raster"
        );
        let decoded = document.xref_stream(xref).unwrap();
        assert_eq!(decoded.len(), 1200 * 1600 * 3);
        assert!(
            decoded
                .as_chunks::<3>()
                .0
                .iter()
                .all(|pixel| *pixel == [240, 245, 250])
        );
    }
}
