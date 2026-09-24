pub mod fonts;
pub mod ooxml;
pub mod pdf;
pub mod raster;
use domain::{Entity, Error, PdfMode, Policy, RegionDto, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Encoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    Gbk,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TextDocument {
    pub text: String,
    pub encoding: Encoding,
}
impl TextDocument {
    pub fn decode(data: &[u8]) -> Result<Self> {
        if data.len() > 50 * 1024 * 1024 {
            return Err(Error::Invalid("文件超过 50 MB".into()));
        }
        let (encoding, codec, bytes) = if data.starts_with(&[0xff, 0xfe]) {
            (Encoding::Utf16Le, encoding_rs::UTF_16LE, &data[2..])
        } else if data.starts_with(&[0xfe, 0xff]) {
            (Encoding::Utf16Be, encoding_rs::UTF_16BE, &data[2..])
        } else if data.starts_with(&[0xef, 0xbb, 0xbf]) {
            (Encoding::Utf8Bom, encoding_rs::UTF_8, &data[3..])
        } else if std::str::from_utf8(data).is_ok() {
            (Encoding::Utf8, encoding_rs::UTF_8, data)
        } else {
            (Encoding::Gbk, encoding_rs::GBK, data)
        };
        let (text, errors) = codec.decode_without_bom_handling(bytes);
        if errors || text.contains('\0') {
            return Err(Error::Invalid(
                "无法无损解码文件，请另存为 UTF-8 或带 BOM 的 UTF-16".into(),
            ));
        }
        Ok(Self {
            text: text.into_owned(),
            encoding,
        })
    }
    pub fn encode(&self, text: &str) -> Result<Vec<u8>> {
        Ok(match self.encoding {
            Encoding::Utf8 => text.as_bytes().to_vec(),
            Encoding::Utf8Bom => [&[0xef, 0xbb, 0xbf], text.as_bytes()].concat(),
            Encoding::Utf16Le => {
                let mut v = vec![0xff, 0xfe];
                for c in text.encode_utf16() {
                    v.extend(c.to_le_bytes());
                }
                v
            }
            Encoding::Utf16Be => {
                let mut v = vec![0xfe, 0xff];
                for c in text.encode_utf16() {
                    v.extend(c.to_be_bytes());
                }
                v
            }
            Encoding::Gbk => {
                let (v, _, errors) = encoding_rs::GBK.encode(text);
                if errors {
                    return Err(Error::Invalid("替换文字无法用原 GBK 编码表示".into()));
                }
                v.into_owned()
            }
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Document {
    pub extension: String,
    pub original: Vec<u8>,
    pub text: String,
    pub encoding: Option<Encoding>,
    #[serde(default)]
    content: DocumentContent,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
enum DocumentContent {
    #[default]
    Text,
    Office(ooxml::Office),
    Raster(raster::RasterFormat),
    Pdf,
}
impl Document {
    pub fn office_presentation(
        &self,
        revision: u64,
        include_docx: bool,
    ) -> Result<ooxml::Presentation> {
        let DocumentContent::Office(office) = &self.content else {
            return Err(Error::Invalid("当前文件不是 Office 文档".into()));
        };
        office.presentation(&self.original, &self.extension, revision, include_docx)
    }

    pub fn preview_kind(&self) -> domain::PreviewKind {
        match &self.content {
            DocumentContent::Pdf | DocumentContent::Raster(_) => domain::PreviewKind::Pages,
            DocumentContent::Office(_) if self.extension == "docx" => domain::PreviewKind::Docx,
            DocumentContent::Office(_) => domain::PreviewKind::OfficeContent,
            DocumentContent::Text => domain::PreviewKind::Text,
        }
    }
    pub fn load(extension: &str, bytes: Vec<u8>) -> Result<Self> {
        let extension = extension.to_lowercase();
        match extension.as_str() {
            "txt" | "md" => {
                let doc = TextDocument::decode(&bytes)?;
                Ok(Self {
                    extension,
                    original: bytes,
                    text: doc.text,
                    encoding: Some(doc.encoding),
                    content: DocumentContent::Text,
                })
            }
            "docx" | "xlsx" | "xlsm" => {
                let office = ooxml::Office::read(&bytes, &extension)?;
                Ok(Self {
                    extension,
                    original: bytes,
                    text: office.text.clone(),
                    encoding: None,
                    content: DocumentContent::Office(office),
                })
            }
            "png" | "jpg" | "jpeg" | "bmp" | "tif" | "tiff" => {
                let format = raster::RasterFormat::from_extension(&extension)?;
                raster::page_dimensions(&bytes, format)?;
                Ok(Self {
                    extension,
                    original: bytes,
                    text: String::new(),
                    encoding: None,
                    content: DocumentContent::Raster(format),
                })
            }
            "pdf" => {
                pdf::validate(&bytes)?;
                Ok(Self {
                    extension,
                    original: bytes,
                    text: String::new(),
                    encoding: None,
                    content: DocumentContent::Pdf,
                })
            }
            _ => Err(Error::Unsupported(extension)),
        }
    }
    pub fn render(&self, entities: &[Entity], policies: &[Policy]) -> Result<Vec<u8>> {
        match &self.content {
            DocumentContent::Text => {
                let text = domain::redact(&self.text, entities, policies)?;
                TextDocument {
                    text: self.text.clone(),
                    encoding: self
                        .encoding
                        .clone()
                        .ok_or_else(|| Error::Unsupported(self.extension.clone()))?,
                }
                .encode(&text)
            }
            DocumentContent::Office(office) => office.render(&self.original, entities, policies),
            DocumentContent::Raster(_) => Err(Error::State("图片输出需要经过区域复核流程".into())),
            DocumentContent::Pdf => Err(Error::State("PDF 输出需要经过区域复核流程".into())),
        }
    }

    pub fn visual_pages(&self) -> Result<Vec<image::RgbaImage>> {
        self.visual_pages_interruptible(&mut || Ok(()))
    }

    pub fn visual_page_count(&self) -> Result<u32> {
        let count = match &self.content {
            DocumentContent::Text => 0,
            DocumentContent::Pdf => pdf::page_count(&self.original)?,
            DocumentContent::Raster(format) => {
                raster::page_dimensions(&self.original, *format)?.len()
            }
            DocumentContent::Office(office) => office.media(&self.original)?.len(),
        };
        u32::try_from(count).map_err(|_| Error::Invalid("页面数量超出支持范围".into()))
    }

    pub fn visual_dimensions(&self) -> Result<Vec<(u32, u32)>> {
        match &self.content {
            DocumentContent::Pdf => pdf::page_dimensions(&self.original),
            DocumentContent::Raster(format) => raster::page_dimensions(&self.original, *format),
            DocumentContent::Text => Ok(Vec::new()),
            DocumentContent::Office(office) => office
                .media(&self.original)?
                .into_iter()
                .map(|part| {
                    let dimensions = raster::page_dimensions(
                        &part.bytes,
                        raster::RasterFormat::from_extension(&part.extension)?,
                    )?;
                    if dimensions.len() != 1 {
                        return Err(Error::Unsupported(
                            "Office 内嵌多页 TIFF 需拆分后才能安全写回".into(),
                        ));
                    }
                    Ok(dimensions[0])
                })
                .collect(),
        }
    }

    pub fn visual_page(&self, index: u32, max_dimension: Option<u32>) -> Result<image::RgbaImage> {
        if let DocumentContent::Pdf = self.content {
            return pdf::render_page(&self.original, index, max_dimension);
        }
        let page = match &self.content {
            DocumentContent::Raster(format) => raster::decode_page(&self.original, *format, index)?,
            DocumentContent::Office(office) => {
                let media = office.media(&self.original)?;
                let part = media
                    .get(index as usize)
                    .ok_or_else(|| Error::Invalid("页面不存在".into()))?;
                raster::decode_page(
                    &part.bytes,
                    raster::RasterFormat::from_extension(&part.extension)?,
                    0,
                )?
            }
            _ => return Err(Error::Invalid("文档没有图像页面".into())),
        };
        let scale = max_dimension
            .map(|limit| limit as f32 / page.width().max(page.height()) as f32)
            .unwrap_or(1.0)
            .min(1.0);
        if scale < 1.0 {
            Ok(image::imageops::resize(
                &page,
                (page.width() as f32 * scale).round().max(1.0) as u32,
                (page.height() as f32 * scale).round().max(1.0) as u32,
                image::imageops::FilterType::Triangle,
            ))
        } else {
            Ok(page)
        }
    }

    pub fn draft_page(
        &self,
        index: u32,
        max_dimension: u32,
        regions: &[RegionDto],
        mode: PdfMode,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<image::RgbaImage> {
        check()?;
        if let DocumentContent::Pdf = self.content {
            return pdf::draft_page(&self.original, index, max_dimension, regions, mode, check);
        }
        let mut page = self.visual_page(index, None)?;
        let local = regions
            .iter()
            .filter(|region| region.page == index)
            .cloned()
            .map(|mut region| {
                region.page = 0;
                region
            })
            .collect::<Vec<_>>();
        let font = fonts::replacement_font()?;
        raster::redact_pages_interruptible(
            std::slice::from_mut(&mut page),
            &local,
            Some(&font),
            check,
        )?;
        let format = match &self.content {
            DocumentContent::Raster(format) => *format,
            DocumentContent::Office(office) => {
                let media = office.media(&self.original)?;
                raster::RasterFormat::from_extension(
                    &media
                        .get(index as usize)
                        .ok_or_else(|| Error::Invalid("页面不存在".into()))?
                        .extension,
                )?
            }
            _ => return Err(Error::Invalid("文档没有图像页面".into())),
        };
        // JPEG exports recompress the edited page. Include that codec step in
        // the draft so its pixels match the eventual exported file as well.
        if format == raster::RasterFormat::Jpeg {
            let encoded =
                raster::encode_pages_interruptible(std::slice::from_ref(&page), format, check)?;
            page = raster::decode_page(&encoded, format, 0)?;
        }
        let scale = (max_dimension as f32 / page.width().max(page.height()) as f32).min(1.0);
        let output = if scale < 1.0 {
            image::imageops::resize(
                &page,
                (page.width() as f32 * scale).round().max(1.0) as u32,
                (page.height() as f32 * scale).round().max(1.0) as u32,
                image::imageops::FilterType::Triangle,
            )
        } else {
            page
        };
        check()?;
        Ok(output)
    }

    /// The visitor owns one page at a time; callers must not retain page pixels.
    pub fn visit_visual_pages(
        &self,
        visit: &mut dyn FnMut(u32, image::RgbaImage) -> Result<()>,
    ) -> Result<()> {
        if let DocumentContent::Pdf = self.content {
            return pdf::visit_pages(&self.original, visit);
        }
        for index in 0..self.visual_page_count()? {
            visit(index, self.visual_page(index, None)?)?;
        }
        Ok(())
    }

    pub fn visual_pages_interruptible(
        &self,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Vec<image::RgbaImage>> {
        check()?;
        match &self.content {
            DocumentContent::Raster(format) => {
                raster::decode_pages_interruptible(&self.original, *format, check)
            }
            DocumentContent::Office(office) => office
                .media_interruptible(&self.original, check)?
                .into_iter()
                .map(|part| {
                    check()?;
                    let format = raster::RasterFormat::from_extension(&part.extension)?;
                    let mut pages = raster::decode_pages_interruptible(&part.bytes, format, check)?;
                    if pages.len() != 1 {
                        return Err(Error::Unsupported(
                            "Office 内嵌多页 TIFF 需拆分后才能安全写回".into(),
                        ));
                    }
                    Ok(pages.remove(0))
                })
                .collect(),
            DocumentContent::Text => Ok(Vec::new()),
            DocumentContent::Pdf => pdf::render_pages_interruptible(&self.original, check),
        }
    }

    pub fn render_with_regions(
        &self,
        entities: &[Entity],
        policies: &[Policy],
        regions: &[RegionDto],
        font_bytes: Option<Vec<u8>>,
        pdf_mode: PdfMode,
    ) -> Result<Vec<u8>> {
        self.render_with_regions_interruptible(
            entities,
            policies,
            regions,
            font_bytes,
            pdf_mode,
            &mut || Ok(()),
        )
    }

    pub fn render_with_regions_interruptible(
        &self,
        entities: &[Entity],
        policies: &[Policy],
        regions: &[RegionDto],
        font_bytes: Option<Vec<u8>>,
        pdf_mode: PdfMode,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Vec<u8>> {
        check()?;
        let font = Some(match font_bytes {
            Some(bytes) => ab_glyph::FontArc::try_from_vec(bytes)
                .map_err(|_| Error::Invalid("替换文字字体无效".into()))?,
            None => fonts::replacement_font()?,
        });
        match &self.content {
            DocumentContent::Raster(format) => {
                let mut pages = raster::decode_pages_interruptible(&self.original, *format, check)?;
                raster::redact_pages_interruptible(&mut pages, regions, font.as_ref(), check)?;
                raster::encode_pages_interruptible(&pages, *format, check)
            }
            DocumentContent::Office(office) => {
                let media = office.media_interruptible(&self.original, check)?;
                let mut replacements = std::collections::HashMap::new();
                for (index, part) in media.into_iter().enumerate() {
                    check()?;
                    let format = raster::RasterFormat::from_extension(&part.extension)?;
                    let mut pages = raster::decode_pages_interruptible(&part.bytes, format, check)?;
                    let mut local = regions
                        .iter()
                        .filter(|region| region.page == index as u32)
                        .cloned()
                        .collect::<Vec<_>>();
                    for region in &mut local {
                        region.page = 0;
                    }
                    raster::redact_pages_interruptible(&mut pages, &local, font.as_ref(), check)?;
                    replacements.insert(
                        part.name,
                        raster::encode_pages_interruptible(&pages, format, check)?,
                    );
                }
                check()?;
                office.render_with_media_interruptible(
                    &self.original,
                    entities,
                    policies,
                    &replacements,
                    check,
                )
            }
            DocumentContent::Text => {
                let output = self.render(entities, policies)?;
                check()?;
                Ok(output)
            }
            DocumentContent::Pdf => {
                pdf::redact_interruptible(&self.original, regions, font.as_ref(), pdf_mode, check)
            }
        }
    }

    pub fn has_removed_office_signatures(&self) -> bool {
        matches!(&self.content, DocumentContent::Office(office) if office.has_signatures())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn single_page_image_draft_matches_real_codec_output_for_every_format() {
        let image = image::RgbaImage::from_fn(160, 96, |x, y| {
            image::Rgba([
                (x % 255) as u8,
                (y * 2 % 255) as u8,
                ((x + y) % 255) as u8,
                255,
            ])
        });
        let region = RegionDto {
            id: uuid::Uuid::new_v4(),
            page: 0,
            polygon: vec![
                domain::Point { x: 0.1, y: 0.1 },
                domain::Point { x: 0.8, y: 0.1 },
                domain::Point { x: 0.8, y: 0.8 },
                domain::Point { x: 0.1, y: 0.8 },
            ],
            entity_id: None,
            selected: true,
            source: domain::RegionSource::Manual,
            text: String::new(),
            score: None,
            rotation: 0.0,
            replacement: Some("某人".into()),
        };
        for extension in ["png", "jpg", "bmp", "tiff"] {
            let format = raster::RasterFormat::from_extension(extension).unwrap();
            let source = raster::encode_pages(std::slice::from_ref(&image), format).unwrap();
            let document = Document::load(extension, source).unwrap();
            let draft = document
                .draft_page(
                    0,
                    512,
                    std::slice::from_ref(&region),
                    PdfMode::SafeRebuild,
                    &mut || Ok(()),
                )
                .unwrap();
            let bytes = document
                .render_with_regions(
                    &[],
                    &[],
                    std::slice::from_ref(&region),
                    None,
                    PdfMode::SafeRebuild,
                )
                .unwrap();
            let result = Document::load(extension, bytes)
                .unwrap()
                .visual_page(0, Some(512))
                .unwrap();
            assert_eq!(
                draft, result,
                "draft must include codec effects: {extension}"
            );
        }
    }
    #[test]
    fn preserves_encodings_and_crlf() {
        for encoding in [
            Encoding::Utf8,
            Encoding::Utf8Bom,
            Encoding::Utf16Le,
            Encoding::Utf16Be,
            Encoding::Gbk,
        ] {
            let doc = TextDocument {
                text: "姓名：张三\r\n电话\r\n".into(),
                encoding,
            };
            let bytes = doc.encode(&doc.text).unwrap();
            let read = TextDocument::decode(&bytes).unwrap();
            assert_eq!(read.text, doc.text);
            assert_eq!(read.encode(&read.text).unwrap(), bytes);
        }
    }
    #[test]
    fn rejects_binary_and_lossy_output() {
        assert!(TextDocument::decode(b"a\0b").is_err());
        let d = TextDocument {
            text: String::new(),
            encoding: Encoding::Gbk,
        };
        assert!(d.encode("😀").is_err());
        assert!(Document::load("pdf", b"%PDF".to_vec()).is_err());
    }
}
