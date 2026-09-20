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
                raster::decode_pages(&bytes, format)?;
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
            DocumentContent::Raster(format) => raster::decode_pages(&self.original, *format)?.len(),
            DocumentContent::Office(office) => office.media(&self.original)?.len(),
        };
        u32::try_from(count).map_err(|_| Error::Invalid("页面数量超出支持范围".into()))
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
        let font = font_bytes
            .map(ab_glyph::FontArc::try_from_vec)
            .transpose()
            .map_err(|_| Error::Invalid("替换文字字体无效".into()))?;
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
