use ab_glyph::{Font, FontArc, PxScale, ScaleFont, point};
use domain::{Error, Point, RegionDto, Result};
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage, imageops};
use std::io::Cursor;

const MAX_PAGE_PIXELS: u64 = 40_000_000;
const MAX_TOTAL_PIXELS: u64 = 80_000_000;
const MAX_PAGES: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RasterFormat {
    Png,
    Jpeg,
    Bmp,
    Tiff,
}

impl RasterFormat {
    pub fn from_extension(extension: &str) -> Result<Self> {
        match extension.to_ascii_lowercase().as_str() {
            "png" => Ok(Self::Png),
            "jpg" | "jpeg" => Ok(Self::Jpeg),
            "bmp" => Ok(Self::Bmp),
            "tif" | "tiff" => Ok(Self::Tiff),
            other => Err(Error::Unsupported(other.into())),
        }
    }
}

pub fn decode_pages(bytes: &[u8], format: RasterFormat) -> Result<Vec<RgbaImage>> {
    decode_pages_interruptible(bytes, format, &mut || Ok(()))
}

pub fn page_dimensions(bytes: &[u8], format: RasterFormat) -> Result<Vec<(u32, u32)>> {
    if format != RasterFormat::Tiff {
        let dimensions = image::ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|e| Error::Invalid(e.to_string()))?
            .into_dimensions()
            .map_err(|e| Error::Invalid(e.to_string()))?;
        validate_dimensions(dimensions.0, dimensions.1)?;
        return Ok(vec![dimensions]);
    }
    let mut decoder = tiff::decoder::Decoder::new(Cursor::new(bytes))
        .map_err(|e| Error::Invalid(e.to_string()))?;
    let mut dimensions = Vec::new();
    let mut pixels = 0u64;
    loop {
        let (width, height) = decoder
            .dimensions()
            .map_err(|e| Error::Invalid(e.to_string()))?;
        validate_dimensions(width, height)?;
        pixels += u64::from(width) * u64::from(height);
        if dimensions.len() >= MAX_PAGES || pixels > MAX_TOTAL_PIXELS {
            return Err(Error::Invalid("TIFF 超过页面或像素限制".into()));
        }
        dimensions.push((width, height));
        if !decoder.more_images() {
            break;
        }
        decoder
            .next_image()
            .map_err(|e| Error::Invalid(e.to_string()))?;
    }
    Ok(dimensions)
}

pub fn decode_page(bytes: &[u8], format: RasterFormat, index: u32) -> Result<RgbaImage> {
    let dimensions = page_dimensions(bytes, format)?;
    let &(width, height) = dimensions
        .get(index as usize)
        .ok_or_else(|| Error::Invalid("页面不存在".into()))?;
    if format != RasterFormat::Tiff {
        return Ok(decode_pages(bytes, format)?.remove(0));
    }
    let mut decoder = tiff::decoder::Decoder::new(Cursor::new(bytes))
        .map_err(|e| Error::Invalid(e.to_string()))?;
    for _ in 0..index {
        decoder
            .next_image()
            .map_err(|e| Error::Invalid(e.to_string()))?;
    }
    let color = decoder
        .colortype()
        .map_err(|e| Error::Invalid(e.to_string()))?;
    let decoded = decoder
        .read_image()
        .map_err(|e| Error::Invalid(e.to_string()))?;
    tiff_to_rgba(width, height, color, decoded)
}

pub fn decode_pages_interruptible(
    bytes: &[u8],
    format: RasterFormat,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<Vec<RgbaImage>> {
    check()?;
    if format != RasterFormat::Tiff {
        let image_format = match format {
            RasterFormat::Png => ImageFormat::Png,
            RasterFormat::Jpeg => ImageFormat::Jpeg,
            RasterFormat::Bmp => ImageFormat::Bmp,
            RasterFormat::Tiff => unreachable!(),
        };
        let dimensions = image::ImageReader::with_format(Cursor::new(bytes), image_format)
            .into_dimensions()
            .map_err(|e| Error::Invalid(format!("图片尺寸无法读取：{e}")))?;
        validate_dimensions(dimensions.0, dimensions.1)?;
        let image = image::load_from_memory_with_format(bytes, image_format)
            .map_err(|e| Error::Invalid(format!("图片无法解码：{e}")))?
            .to_rgba8();
        validate_page(&image)?;
        check()?;
        return Ok(vec![image]);
    }

    let mut decoder = tiff::decoder::Decoder::new(Cursor::new(bytes))
        .map_err(|e| Error::Invalid(format!("TIFF 无法解码：{e}")))?;
    let mut pages = Vec::new();
    let mut total_pixels = 0u64;
    loop {
        check()?;
        let (width, height) = decoder
            .dimensions()
            .map_err(|e| Error::Invalid(format!("TIFF 页面无效：{e}")))?;
        validate_dimensions(width, height)?;
        total_pixels = total_pixels
            .checked_add(u64::from(width) * u64::from(height))
            .ok_or_else(|| Error::Invalid("TIFF 总像素数量溢出".into()))?;
        if pages.len() >= MAX_PAGES || total_pixels > MAX_TOTAL_PIXELS {
            return Err(Error::Invalid(format!(
                "TIFF 最多支持 {MAX_PAGES} 页且总计不超过 {MAX_TOTAL_PIXELS} 像素"
            )));
        }
        let color = decoder
            .colortype()
            .map_err(|e| Error::Invalid(format!("TIFF 颜色类型无效：{e}")))?;
        let decoded = decoder
            .read_image()
            .map_err(|e| Error::Invalid(format!("TIFF 页面无法读取：{e}")))?;
        let rgba = tiff_to_rgba(width, height, color, decoded)?;
        validate_page(&rgba)?;
        pages.push(rgba);
        check()?;
        if !decoder.more_images() {
            break;
        }
        decoder
            .next_image()
            .map_err(|e| Error::Invalid(format!("TIFF 下一页无效：{e}")))?;
    }
    if pages.is_empty() {
        return Err(Error::Invalid("TIFF 不包含页面".into()));
    }
    Ok(pages)
}

fn tiff_to_rgba(
    width: u32,
    height: u32,
    color: tiff::ColorType,
    decoded: tiff::decoder::DecodingResult,
) -> Result<RgbaImage> {
    use tiff::{ColorType, decoder::DecodingResult};
    let samples = match decoded {
        DecodingResult::U8(v) => v,
        DecodingResult::U16(v) => v.into_iter().map(|x| (x >> 8) as u8).collect(),
        _ => return Err(Error::Unsupported("TIFF 仅支持 8/16 位整数像素".into())),
    };
    let mut output = Vec::with_capacity(width as usize * height as usize * 4);
    match color {
        ColorType::Gray(_) => {
            for value in samples {
                output.extend_from_slice(&[value, value, value, 255]);
            }
        }
        ColorType::GrayA(_) => {
            for value in samples.as_chunks::<2>().0 {
                output.extend_from_slice(&[value[0], value[0], value[0], value[1]]);
            }
        }
        ColorType::RGB(_) => {
            for value in samples.as_chunks::<3>().0 {
                output.extend_from_slice(&[value[0], value[1], value[2], 255]);
            }
        }
        ColorType::RGBA(_) => output = samples,
        _ => return Err(Error::Unsupported("TIFF 颜色空间暂不支持".into())),
    }
    RgbaImage::from_raw(width, height, output)
        .ok_or_else(|| Error::Invalid("TIFF 像素数量与页面尺寸不一致".into()))
}

fn validate_page(image: &RgbaImage) -> Result<()> {
    validate_dimensions(image.width(), image.height())
}

fn validate_dimensions(width: u32, height: u32) -> Result<()> {
    let pixels = u64::from(width) * u64::from(height);
    if width == 0 || height == 0 || pixels > MAX_PAGE_PIXELS {
        return Err(Error::Invalid("图片尺寸无效或单页超过 4000 万像素".into()));
    }
    Ok(())
}

pub fn encode_pages(pages: &[RgbaImage], format: RasterFormat) -> Result<Vec<u8>> {
    encode_pages_interruptible(pages, format, &mut || Ok(()))
}

pub fn encode_pages_interruptible(
    pages: &[RgbaImage],
    format: RasterFormat,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<Vec<u8>> {
    check()?;
    let first = pages
        .first()
        .ok_or_else(|| Error::Invalid("图片不包含页面".into()))?;
    if format == RasterFormat::Tiff {
        let mut output = Cursor::new(Vec::new());
        {
            let mut encoder = tiff::encoder::TiffEncoder::new(&mut output)
                .map_err(|e| Error::Io(e.to_string()))?;
            for page in pages {
                check()?;
                encoder
                    .write_image::<tiff::encoder::colortype::RGBA8>(
                        page.width(),
                        page.height(),
                        page.as_raw(),
                    )
                    .map_err(|e| Error::Io(e.to_string()))?;
                check()?;
            }
        }
        return Ok(output.into_inner());
    }
    let mut output = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(first.clone())
        .write_to(
            &mut output,
            match format {
                RasterFormat::Png => ImageFormat::Png,
                RasterFormat::Jpeg => ImageFormat::Jpeg,
                RasterFormat::Bmp => ImageFormat::Bmp,
                RasterFormat::Tiff => unreachable!(),
            },
        )
        .map_err(|e| Error::Io(e.to_string()))?;
    check()?;
    Ok(output.into_inner())
}

pub fn redact_pages(
    pages: &mut [RgbaImage],
    regions: &[RegionDto],
    font: Option<&FontArc>,
) -> Result<()> {
    redact_pages_interruptible(pages, regions, font, &mut || Ok(()))
}

pub fn redact_pages_interruptible(
    pages: &mut [RgbaImage],
    regions: &[RegionDto],
    font: Option<&FontArc>,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<()> {
    for region in regions.iter().filter(|r| r.selected) {
        check()?;
        region.validate(pages.len() as u32)?;
        let page = &mut pages[region.page as usize];
        let (x0, y0, x1, y1) = pixel_bounds(page, &region.polygon);
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        let width = x1 - x0;
        let height = y1 - y0;
        let crop = imageops::crop_imm(page, x0, y0, width, height).to_image();
        let sigma = ((width.min(height) as f32) / 3.0).clamp(8.0, 32.0);
        let blurred = imageops::blur(&crop, sigma);
        let background = surrounding_color(page, x0, y0, x1, y1);
        for (x, y, pixel) in blurred.enumerate_pixels() {
            let mixed = Rgba([
                ((u16::from(pixel[0]) + 4 * u16::from(background[0])) / 5) as u8,
                ((u16::from(pixel[1]) + 4 * u16::from(background[1])) / 5) as u8,
                ((u16::from(pixel[2]) + 4 * u16::from(background[2])) / 5) as u8,
                255,
            ]);
            page.put_pixel(x0 + x, y0 + y, mixed);
        }
        if let (Some(font), Some(value)) = (font, region.replacement.as_deref())
            && !value.is_empty()
        {
            draw_replacement(
                page,
                (x0, y0, x1, y1),
                value,
                font,
                background,
                region.rotation,
            );
        }
        check()?;
    }
    Ok(())
}

fn pixel_bounds(image: &RgbaImage, polygon: &[Point]) -> (u32, u32, u32, u32) {
    let width = image.width() as f32;
    let height = image.height() as f32;
    let min_x = polygon.iter().map(|p| p.x).fold(1.0, f32::min);
    let min_y = polygon.iter().map(|p| p.y).fold(1.0, f32::min);
    let max_x = polygon.iter().map(|p| p.x).fold(0.0, f32::max);
    let max_y = polygon.iter().map(|p| p.y).fold(0.0, f32::max);
    (
        (min_x * width).floor().clamp(0.0, width) as u32,
        (min_y * height).floor().clamp(0.0, height) as u32,
        (max_x * width).ceil().clamp(0.0, width) as u32,
        (max_y * height).ceil().clamp(0.0, height) as u32,
    )
}

fn surrounding_color(image: &RgbaImage, x0: u32, y0: u32, x1: u32, y1: u32) -> Rgba<u8> {
    let left = x0.saturating_sub(3);
    let top = y0.saturating_sub(3);
    let right = (x1 + 3).min(image.width());
    let bottom = (y1 + 3).min(image.height());
    let mut channels = [Vec::new(), Vec::new(), Vec::new()];
    for y in top..bottom {
        for x in left..right {
            if x >= x0 && x < x1 && y >= y0 && y < y1 {
                continue;
            }
            let p = image.get_pixel(x, y);
            for channel in 0..3 {
                channels[channel].push(p[channel]);
            }
        }
    }
    let mut result = [238u8; 4];
    result[3] = 255;
    for channel in 0..3 {
        if !channels[channel].is_empty() {
            channels[channel].sort_unstable();
            result[channel] = channels[channel][channels[channel].len() / 2];
        }
    }
    Rgba(result)
}

fn draw_replacement(
    image: &mut RgbaImage,
    bounds: (u32, u32, u32, u32),
    text: &str,
    font: &FontArc,
    background: Rgba<u8>,
    rotation: f32,
) {
    let (x0, y0, x1, y1) = bounds;
    let available_width = (x1 - x0).max(1) as f32;
    let available_height = (y1 - y0).max(1) as f32;
    let chars = text.chars().count().max(1) as f32;
    let size = available_height
        .min(available_width / chars)
        .clamp(8.0, 72.0)
        * 0.78;
    let luminance = 0.2126 * f32::from(background[0])
        + 0.7152 * f32::from(background[1])
        + 0.0722 * f32::from(background[2]);
    let color: Rgba<u8> = if luminance > 145.0 {
        Rgba([25, 25, 25, 255])
    } else {
        Rgba([245, 245, 245, 255])
    };
    let scaled = font.as_scaled(PxScale::from(size));
    let mut caret = point(
        x0 as f32 + 2.0,
        y0 as f32 + ((available_height - size) / 2.0).max(0.0) + scaled.ascent(),
    );
    for character in text.chars() {
        let glyph_id = scaled.glyph_id(character);
        let glyph = glyph_id.with_scale_and_position(size, caret);
        if let Some(outline) = font.outline_glyph(glyph) {
            let bounds = outline.px_bounds();
            outline.draw(|x, y, coverage| {
                let source_x = bounds.min.x as i32 + x as i32;
                let source_y = bounds.min.y as i32 + y as i32;
                if source_x < x0 as i32
                    || source_y < y0 as i32
                    || source_x >= x1 as i32
                    || source_y >= y1 as i32
                {
                    return;
                }
                let upside_down = (rotation.rem_euclid(360.0) - 180.0).abs() < 45.0;
                let (px, py) = if upside_down {
                    (
                        x0 as i32 + x1 as i32 - 1 - source_x,
                        y0 as i32 + y1 as i32 - 1 - source_y,
                    )
                } else {
                    (source_x, source_y)
                };
                let target = image.get_pixel_mut(px as u32, py as u32);
                let alpha = coverage.clamp(0.0, 1.0);
                for channel in 0..3 {
                    target[channel] = (f32::from(target[channel]) * (1.0 - alpha)
                        + f32::from(color[channel]) * alpha)
                        .round() as u8;
                }
                target[3] = 255;
            });
        }
        caret.x += scaled.h_advance(glyph_id);
        if caret.x >= x1 as f32 - 2.0 {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_dimensions_before_large_pixel_allocation() {
        assert!(validate_dimensions(20_000, 20_000).is_err());
        assert!(validate_dimensions(0, 10).is_err());
        assert!(validate_dimensions(4_000, 4_000).is_ok());
    }
    use domain::RegionSource;

    fn region(page: u32) -> RegionDto {
        RegionDto {
            id: uuid::Uuid::new_v4(),
            page,
            polygon: vec![
                Point { x: 0.25, y: 0.25 },
                Point { x: 0.75, y: 0.25 },
                Point { x: 0.75, y: 0.75 },
                Point { x: 0.25, y: 0.75 },
            ],
            entity_id: None,
            selected: true,
            source: RegionSource::Manual,
            text: String::new(),
            score: None,
            rotation: 0.0,
            replacement: None,
        }
    }

    #[test]
    fn preserves_raster_format_and_tiff_pages() {
        let page = RgbaImage::from_pixel(32, 24, Rgba([255, 255, 255, 255]));
        for format in [RasterFormat::Png, RasterFormat::Jpeg, RasterFormat::Bmp] {
            let bytes = encode_pages(std::slice::from_ref(&page), format).unwrap();
            let read = decode_pages(&bytes, format).unwrap();
            assert_eq!((read[0].width(), read[0].height()), (32, 24));
        }
        let bytes = encode_pages(&[page.clone(), page], RasterFormat::Tiff).unwrap();
        assert_eq!(decode_pages(&bytes, RasterFormat::Tiff).unwrap().len(), 2);
    }

    #[test]
    fn changes_only_selected_region() {
        let mut page = RgbaImage::from_pixel(40, 20, Rgba([255, 255, 255, 255]));
        for y in 5..15 {
            for x in 10..30 {
                page.put_pixel(x, y, Rgba([0, 0, 0, 255]));
            }
        }
        let before = page.clone();
        redact_pages(std::slice::from_mut(&mut page), &[region(0)], None).unwrap();
        assert_eq!(before.get_pixel(0, 0), page.get_pixel(0, 0));
        assert_ne!(before.get_pixel(20, 10), page.get_pixel(20, 10));
    }

    #[test]
    fn multipage_work_stops_when_cancelled() {
        let page = RgbaImage::from_pixel(32, 24, Rgba([255, 255, 255, 255]));
        let bytes = encode_pages(&[page.clone(), page], RasterFormat::Tiff).unwrap();
        let mut checks = 0;
        let result = decode_pages_interruptible(&bytes, RasterFormat::Tiff, &mut || {
            checks += 1;
            if checks >= 3 {
                Err(Error::State("任务已取消".into()))
            } else {
                Ok(())
            }
        });
        assert!(result.is_err());
        assert_eq!(checks, 3);
    }

    #[test]
    fn replacement_text_respects_ocr_upside_down_rotation() {
        let font = crate::fonts::replacement_font().unwrap();
        let background = Rgba([255, 255, 255, 255]);
        let mut normal = RgbaImage::from_pixel(64, 32, background);
        let mut rotated = normal.clone();
        let bounds = (5, 5, 55, 27);
        draw_replacement(&mut normal, bounds, "TEST", &font, background, 0.0);
        draw_replacement(&mut rotated, bounds, "TEST", &font, background, 180.0);
        for y in bounds.1..bounds.3 {
            for x in bounds.0..bounds.2 {
                assert_eq!(
                    normal.get_pixel(x, y),
                    rotated.get_pixel(bounds.0 + bounds.2 - 1 - x, bounds.1 + bounds.3 - 1 - y)
                );
            }
        }
    }
}
