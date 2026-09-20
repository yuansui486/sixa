//! OOXML text surgery. Unchanged ZIP entries retain their compressed bytes.
//! Supported media is redacted in place, invalid signatures are removed, and
//! embedded documents or ActiveX are rejected because they cannot be audited.
use domain::{Entity, Error, Policy, Result, Span};
use quick_xml::{Reader, events::Event};
use serde::{Deserialize, Serialize};
use std::io::{Cursor, Read, Write};
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

const MAX_EXPANDED: u64 = 200 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Piece {
    part: String,
    xml_start: usize,
    xml_end: usize,
    span: Span,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Office {
    pub text: String,
    pieces: Vec<Piece>,
    media_entries: Vec<String>,
    signature_entries: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct MediaPart {
    pub name: String,
    pub extension: String,
    pub bytes: Vec<u8>,
}
fn invalid(e: impl std::fmt::Display) -> Error {
    Error::Invalid(format!("Office 文件无效：{e}"))
}
fn relevant(name: &[u8]) -> bool {
    matches!(
        name,
        b"t" | b"v"
            | b"delText"
            | b"instrText"
            | b"f"
            | b"definedName"
            | b"oddHeader"
            | b"evenHeader"
            | b"firstHeader"
            | b"oddFooter"
            | b"evenFooter"
            | b"firstFooter"
            | b"creator"
            | b"lastModifiedBy"
            | b"title"
            | b"subject"
            | b"description"
            | b"keywords"
            | b"company"
            | b"Company"
    )
}
impl Office {
    pub fn read(bytes: &[u8], extension: &str) -> Result<Self> {
        let mut archive = ZipArchive::new(Cursor::new(bytes)).map_err(invalid)?;
        if archive.len() > 10000 {
            return Err(invalid("压缩包条目过多"));
        }
        let mut total = 0u64;
        let mut names = std::collections::HashSet::new();
        for i in 0..archive.len() {
            let f = archive.by_index(i).map_err(invalid)?;
            total = total
                .checked_add(f.size())
                .ok_or_else(|| invalid("大小溢出"))?;
            if total > MAX_EXPANDED
                || f.enclosed_name().is_none()
                || !names.insert(f.name().to_string())
            {
                return Err(invalid("压缩包过大、路径无效或存在重复条目"));
            }
            if f.name().contains("/embeddings/") || f.name().contains("/activeX/") {
                return Err(Error::Unsupported(
                    "Office 包含 OLE 嵌入文档或 ActiveX，无法完整审计，已阻止导出".into(),
                ));
            }
        }
        let required = if extension == "docx" {
            "word/document.xml"
        } else {
            "xl/workbook.xml"
        };
        if !names.contains(required) || !names.contains("[Content_Types].xml") {
            return Err(invalid("文件扩展名与 OOXML 内容不符"));
        }
        let mut sorted: Vec<_> = names.into_iter().collect();
        sorted.sort();
        let mut office = Self {
            text: String::new(),
            pieces: Vec::new(),
            media_entries: sorted
                .iter()
                .filter(|name| name.contains("/media/"))
                .cloned()
                .collect(),
            signature_entries: sorted
                .iter()
                .filter(|name| {
                    name.starts_with("_xmlsignatures/") || name.ends_with("vbaProjectSignature.bin")
                })
                .cloned()
                .collect(),
        };
        for name in sorted {
            if !name.ends_with(".xml") && !name.ends_with(".rels") {
                continue;
            }
            let mut file = archive.by_name(&name).map_err(invalid)?;
            let mut xml = String::new();
            file.read_to_string(&mut xml).map_err(invalid)?;
            office.parse(&name, &xml)?;
            office.text.push('\n');
        }
        Ok(office)
    }
    pub fn media(&self, original: &[u8]) -> Result<Vec<MediaPart>> {
        self.media_interruptible(original, &mut || Ok(()))
    }

    pub fn media_interruptible(
        &self,
        original: &[u8],
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Vec<MediaPart>> {
        check()?;
        let mut archive = ZipArchive::new(Cursor::new(original)).map_err(invalid)?;
        let mut media = Vec::new();
        for name in &self.media_entries {
            check()?;
            let extension = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
            if !matches!(
                extension.as_str(),
                "png" | "jpg" | "jpeg" | "bmp" | "tif" | "tiff"
            ) {
                return Err(Error::Unsupported(format!(
                    "Office 内嵌图片格式不支持：{name}"
                )));
            }
            let mut bytes = Vec::new();
            archive
                .by_name(name)
                .map_err(invalid)?
                .read_to_end(&mut bytes)?;
            check()?;
            media.push(MediaPart {
                name: name.clone(),
                extension,
                bytes,
            });
        }
        Ok(media)
    }

    pub fn has_signatures(&self) -> bool {
        !self.signature_entries.is_empty()
    }
    fn add(&mut self, part: &str, start: usize, end: usize, text: &str) {
        if text.is_empty() {
            return;
        }
        let span = Span {
            start: self.text.len(),
            end: self.text.len() + text.len(),
        };
        self.text.push_str(text);
        self.pieces.push(Piece {
            part: part.into(),
            xml_start: start,
            xml_end: end,
            span,
        });
    }
    fn parse(&mut self, part: &str, xml: &str) -> Result<()> {
        let mut reader = Reader::from_str(xml);
        loop {
            let before = reader.buffer_position() as usize;
            match reader.read_event().map_err(invalid)? {
                Event::Start(e) if relevant(e.local_name().as_ref()) => {
                    let start = reader.buffer_position() as usize;
                    let raw = reader.read_text(e.name()).map_err(invalid)?;
                    if raw.contains('<') {
                        return Err(invalid("文字节点包含嵌套标记"));
                    }
                    let end = start + raw.len();
                    let value = quick_xml::escape::unescape(&raw).map_err(invalid)?;
                    self.add(part, start, end, &value);
                }
                Event::Start(e) | Event::Empty(e) => {
                    // XML attribute positions are retained from the original start tag.
                    let after = reader.buffer_position() as usize;
                    let external = e.attributes().with_checks(true).any(|a| {
                        a.is_ok_and(|a| {
                            a.key.as_ref() == b"TargetMode" && a.value.as_ref() == b"External"
                        })
                    });
                    let mut search = before;
                    for attribute in e.attributes().with_checks(true) {
                        let a = attribute.map_err(invalid)?;
                        let key = std::str::from_utf8(a.key.as_ref()).map_err(invalid)?;
                        if !(external && key == "Target"
                            || key == "tooltip"
                            || key == "display"
                            || key == "name" && e.local_name().as_ref() == b"sheet")
                        {
                            continue;
                        }
                        // Find the exact attribute using XML token delimiters, never a substring inside another value.
                        let raw = &xml[before..after];
                        let relative =
                            find_attribute(raw, key).ok_or_else(|| invalid("无法定位属性"))?;
                        let start = before + relative.0;
                        let end = before + relative.1;
                        if start < search {
                            return Err(invalid("属性顺序无效"));
                        }
                        search = end;
                        let decoded = a
                            .decode_and_unescape_value(reader.decoder())
                            .map_err(invalid)?;
                        self.text.push('\n');
                        self.add(part, start, end, &decoded);
                        self.text.push('\n');
                    }
                }
                Event::End(e) => {
                    if matches!(
                        e.local_name().as_ref(),
                        b"p" | b"row" | b"si" | b"c" | b"comment" | b"definedName"
                    ) {
                        self.text.push('\n');
                    }
                }
                Event::DocType(_) => return Err(invalid("不允许 DTD")),
                Event::Eof => break,
                _ => {}
            }
        }
        Ok(())
    }
    pub fn render(
        &self,
        original: &[u8],
        entities: &[Entity],
        policies: &[Policy],
    ) -> Result<Vec<u8>> {
        self.render_with_media(
            original,
            entities,
            policies,
            &std::collections::HashMap::new(),
        )
    }

    pub fn render_with_media(
        &self,
        original: &[u8],
        entities: &[Entity],
        policies: &[Policy],
        media: &std::collections::HashMap<String, Vec<u8>>,
    ) -> Result<Vec<u8>> {
        self.render_with_media_interruptible(original, entities, policies, media, &mut || Ok(()))
    }

    pub fn render_with_media_interruptible(
        &self,
        original: &[u8],
        entities: &[Entity],
        policies: &[Policy],
        media: &std::collections::HashMap<String, Vec<u8>>,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Vec<u8>> {
        check()?;
        // Validate all trusted internal spans before any output can be written.
        domain::redact(&self.text, entities, policies)?;
        let mut modifications: std::collections::HashMap<&str, Vec<(usize, usize, String)>> =
            std::collections::HashMap::new();
        for piece in &self.pieces {
            check()?;
            let mut affected: Vec<_> = entities
                .iter()
                .filter(|e| e.selected && e.span.overlaps(piece.span))
                .collect();
            if affected.is_empty() {
                continue;
            }
            affected.sort_by_key(|e| e.span.start);
            let source = &self.text[piece.span.start..piece.span.end];
            let mut changed = String::new();
            let mut cursor = 0;
            for entity in affected {
                check()?;
                let start = entity.span.start.max(piece.span.start) - piece.span.start;
                let end = entity.span.end.min(piece.span.end) - piece.span.start;
                changed.push_str(&source[cursor..start]);
                if entity.span.start >= piece.span.start {
                    changed.push_str(&domain::replacement(
                        entity,
                        &self.text[entity.span.start..entity.span.end],
                        policies,
                    ));
                }
                cursor = end;
            }
            changed.push_str(&source[cursor..]);
            modifications.entry(&piece.part).or_default().push((
                piece.xml_start,
                piece.xml_end,
                quick_xml::escape::escape(&changed).into_owned(),
            ));
        }
        let mut archive = ZipArchive::new(Cursor::new(original)).map_err(invalid)?;
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        for i in 0..archive.len() {
            check()?;
            let mut entry = archive.by_index(i).map_err(invalid)?;
            if self
                .signature_entries
                .iter()
                .any(|name| name == entry.name())
            {
                continue;
            }
            if let Some(bytes) = media.get(entry.name()) {
                writer
                    .start_file(
                        entry.name(),
                        SimpleFileOptions::default().compression_method(entry.compression()),
                    )
                    .map_err(invalid)?;
                writer.write_all(bytes)?;
                check()?;
                continue;
            }
            if let Some(changes) = modifications.get_mut(entry.name()) {
                let mut xml = String::new();
                entry.read_to_string(&mut xml)?;
                changes.sort_by_key(|c| std::cmp::Reverse(c.0));
                for (start, end, value) in changes {
                    xml.replace_range(*start..*end, value);
                }
                if !self.signature_entries.is_empty() {
                    xml = strip_signature_references(&xml);
                }
                writer
                    .start_file(
                        entry.name(),
                        SimpleFileOptions::default().compression_method(entry.compression()),
                    )
                    .map_err(invalid)?;
                writer.write_all(xml.as_bytes())?;
            } else if !self.signature_entries.is_empty()
                && (entry.name().ends_with(".rels") || entry.name() == "[Content_Types].xml")
            {
                let mut xml = String::new();
                entry.read_to_string(&mut xml)?;
                let xml = strip_signature_references(&xml);
                writer
                    .start_file(
                        entry.name(),
                        SimpleFileOptions::default().compression_method(entry.compression()),
                    )
                    .map_err(invalid)?;
                writer.write_all(xml.as_bytes())?;
            } else {
                writer.raw_copy_file(entry).map_err(invalid)?;
            }
            check()?;
        }
        let output = writer.finish().map_err(invalid)?.into_inner();
        check()?;
        Ok(output)
    }
}

fn strip_signature_references(xml: &str) -> String {
    let mut output = xml.to_owned();
    for marker in [
        "_xmlsignatures",
        "vbaProjectSignature.bin",
        "digital-signature",
    ] {
        while let Some(position) = output.find(marker) {
            let start = output[..position].rfind('<').unwrap_or(position);
            let end = output[position..]
                .find('>')
                .map(|offset| position + offset + 1)
                .unwrap_or(position + marker.len());
            output.replace_range(start..end, "");
        }
    }
    output
}
fn find_attribute(raw: &str, wanted: &str) -> Option<(usize, usize)> {
    let b = raw.as_bytes();
    let mut i = 1;
    while i < b.len() && !b[i].is_ascii_whitespace() {
        i += 1;
    }
    while i < b.len() {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'=' {
            i += 1;
        }
        let key = raw.get(start..i)?;
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if b.get(i) != Some(&b'=') {
            break;
        }
        i += 1;
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let quote = *b.get(i)?;
        if quote != b'\'' && quote != b'"' {
            return None;
        }
        i += 1;
        let value_start = i;
        while i < b.len() && b[i] != quote {
            i += 1;
        }
        if key == wanted {
            return Some((value_start, i));
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    fn package(main: &str, xml: &str) -> Vec<u8> {
        let mut w = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, content) in [
            ("[Content_Types].xml", b"<Types/>".as_slice()),
            (main, xml.as_bytes()),
            ("xl/vbaProject.bin", b"opaque VBA binary"),
        ] {
            w.start_file(name, SimpleFileOptions::default()).unwrap();
            w.write_all(content).unwrap();
        }
        w.finish().unwrap().into_inner()
    }
    #[test]
    fn cross_run_redaction_and_opaque_entry_preservation() {
        let original = package(
            "word/document.xml",
            r#"<w:document xmlns:w="urn:word"><w:p><w:r><w:t>张</w:t></w:r><w:r><w:t>三&amp;同事</w:t></w:r></w:p></w:document>"#,
        );
        let office = Office::read(&original, "docx").unwrap();
        let start = office.text.find("张三").unwrap();
        let entity = Entity {
            id: uuid::Uuid::new_v4(),
            entity_type: "PERSON".into(),
            score: 1.,
            source: "test".into(),
            selected: true,
            span: Span {
                start,
                end: start + 6,
            },
            replacement: None,
        };
        let output = office.render(&original, &[entity], &[]).unwrap();
        let read = Office::read(&output, "docx").unwrap();
        assert!(read.text.contains("某人&同事"));
        assert!(!read.text.contains("张三"));
        let mut zip = ZipArchive::new(Cursor::new(output)).unwrap();
        let mut v = Vec::new();
        zip.by_name("xl/vbaProject.bin")
            .unwrap()
            .read_to_end(&mut v)
            .unwrap();
        assert_eq!(v, b"opaque VBA binary");
    }
    #[test]
    fn relationships_are_extracted_and_escaped() {
        let mut office = Office {
            text: String::new(),
            pieces: vec![],
            media_entries: vec![],
            signature_entries: vec![],
        };
        let xml = r#"<Relationships><Relationship TargetMode="External" Target="mailto:zhang@example.com?a=1&amp;b=2"/></Relationships>"#;
        office.parse("word/_rels/document.xml.rels", xml).unwrap();
        assert!(office.text.contains("mailto:zhang@example.com?a=1&b=2"));
    }
    #[test]
    fn refuses_dtd() {
        let original = package(
            "word/document.xml",
            "<!DOCTYPE x [<!ENTITY x SYSTEM 'file:///x'>]><x/>",
        );
        assert!(Office::read(&original, "docx").is_err());
    }
}
