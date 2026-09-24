//! A presentation-only package. Source offsets always refer to the unmodified
//! Office extraction; generated bookmarks never enter exported documents.
use super::*;
use domain::{OfficeAnchor, OfficeImage, OfficePreview};
use std::collections::{HashMap, HashSet};

const MAX_LAYOUT_BYTES: u64 = 50 * 1024 * 1024;
const MAX_LAYOUT_NODES: usize = 10_000;
pub struct Presentation {
    pub metadata: OfficePreview,
    pub docx: Option<Vec<u8>>,
}
struct Node {
    name: String,
    attributes: Vec<(String, String)>,
    start: usize,
    open_end: usize,
    close_start: usize,
    end: usize,
    empty: bool,
    parent: Option<usize>,
    children: Vec<usize>,
}
impl Node {
    fn local(&self) -> &str {
        self.name.rsplit(':').next().unwrap_or(&self.name)
    }
    fn attr(&self, local: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| key.rsplit(':').next() == Some(local))
            .map(|(_, value)| value.as_str())
    }
    fn opening(&self, empty: bool) -> String {
        let mut result = format!("<{}", self.name);
        for (key, value) in &self.attributes {
            // No preview resource may navigate to or retrieve an external URL.
            if !key.starts_with("xmlns")
                && matches!(
                    key.rsplit(':').next().unwrap_or(key),
                    "src" | "href" | "link"
                )
            {
                continue;
            }
            result.push_str(&format!(" {key}=\"{}\"", quick_xml::escape::escape(value)));
        }
        result.push_str(if empty { "/>" } else { ">" });
        result
    }
}
struct Xml {
    source: String,
    nodes: Vec<Node>,
    roots: Vec<usize>,
    word_prefix: String,
}
impl Xml {
    fn parse(source: String, count: &mut usize) -> Result<Self> {
        let mut reader = Reader::from_str(&source);
        let mut nodes: Vec<Node> = Vec::new();
        let mut roots = Vec::new();
        let mut stack: Vec<usize> = Vec::new();
        let mut word_prefix = "w".to_owned();
        loop {
            let before = reader.buffer_position() as usize;
            let event = reader.read_event().map_err(invalid)?;
            match event {
                Event::Start(ref tag) | Event::Empty(ref tag) => {
                    *count += 1;
                    if *count > MAX_LAYOUT_NODES || stack.len() > 128 {
                        return Err(Error::Unsupported(
                            "文档布局节点超过 10000 或嵌套过深，已改为内容预览".into(),
                        ));
                    }
                    let empty = matches!(event, Event::Empty(_));
                    let name = String::from_utf8(tag.name().as_ref().to_vec()).map_err(invalid)?;
                    let attributes = tag
                        .attributes()
                        .with_checks(true)
                        .map(|a| {
                            let a = a.map_err(invalid)?;
                            let key =
                                String::from_utf8(a.key.as_ref().to_vec()).map_err(invalid)?;
                            let value = a
                                .decode_and_unescape_value(reader.decoder())
                                .map_err(invalid)?
                                .into_owned();
                            if key.starts_with("xmlns:")
                                && (value.ends_with("wordprocessingml/2006/main")
                                    || value.ends_with("wordprocessingml/main"))
                            {
                                word_prefix = key[6..].to_owned();
                            }
                            Ok((key, value))
                        })
                        .collect::<Result<Vec<_>>>()?;
                    let id = nodes.len();
                    nodes.push(Node {
                        name,
                        attributes,
                        start: before,
                        open_end: reader.buffer_position() as usize,
                        close_start: reader.buffer_position() as usize,
                        end: reader.buffer_position() as usize,
                        empty,
                        parent: stack.last().copied(),
                        children: Vec::new(),
                    });
                    if let Some(parent) = stack.last() {
                        nodes[*parent].children.push(id);
                    } else {
                        roots.push(id);
                    }
                    if !empty {
                        stack.push(id);
                    }
                }
                Event::End(_) => {
                    let index = stack.pop().ok_or_else(|| invalid("XML 结束节点无效"))?;
                    nodes[index].close_start = before;
                    nodes[index].end = reader.buffer_position() as usize;
                }
                Event::Text(_) | Event::CData(_) => {
                    *count += 1;
                    if *count > MAX_LAYOUT_NODES {
                        return Err(Error::Unsupported(
                            "文档布局节点超过 10000，已改为内容预览".into(),
                        ));
                    }
                }
                Event::DocType(_) => return Err(invalid("不允许 DTD")),
                Event::Eof => break,
                _ => {}
            }
        }
        if !stack.is_empty() {
            return Err(invalid("XML 节点未关闭"));
        }
        Ok(Self {
            source,
            nodes,
            roots,
            word_prefix,
        })
    }
}

fn label(part: &str) -> String {
    if part == "word/document.xml" {
        "正文".into()
    } else if part.contains("header") {
        "页眉".into()
    } else if part.contains("footer") {
        "页脚".into()
    } else if part.contains("footnotes") {
        "脚注".into()
    } else if part.contains("endnotes") {
        "尾注".into()
    } else if part.contains("comments") {
        "批注".into()
    } else if part.starts_with("docProps/") {
        "文档属性".into()
    } else if part.ends_with(".rels") {
        "链接地址".into()
    } else if part.starts_with("xl/") {
        format!("表格内容 · {part}")
    } else {
        format!("其他内容 · {part}")
    }
}
fn safe_entry(name: &str) -> bool {
    name == "[Content_Types].xml"
        || name == "_rels/.rels"
        || name.starts_with("word/")
            && (name.ends_with(".xml")
                || name.ends_with(".rels")
                || name.starts_with("word/media/")
                    && matches!(
                        name.rsplit('.')
                            .next()
                            .unwrap_or("")
                            .to_ascii_lowercase()
                            .as_str(),
                        "png" | "jpg" | "jpeg" | "bmp" | "tif" | "tiff"
                    ))
}
fn relationship_owner(name: &str) -> Option<String> {
    if name == "_rels/.rels" {
        return Some(String::new());
    }
    let (folder, file) = name.rsplit_once("/_rels/")?;
    Some(format!("{folder}/{}", file.strip_suffix(".rels")?))
}
fn internal_target(owner: &str, target: &str) -> Option<String> {
    if target.contains([':', '\\', '#', '?']) || target.starts_with("//") {
        return None;
    }
    let mut components: Vec<&str> = if target.starts_with('/') {
        Vec::new()
    } else {
        owner
            .rsplit_once('/')
            .map(|(dir, _)| dir.split('/').collect())
            .unwrap_or_default()
    };
    for part in target.trim_start_matches('/').split('/') {
        match part {
            "" | "." => {}
            ".." => {
                components.pop()?;
            }
            _ => components.push(part),
        }
    }
    Some(components.join("/"))
}
fn relationship_target(owner: &str, node: &Node, included: &HashSet<String>) -> Option<String> {
    if node
        .attr("TargetMode")
        .is_some_and(|value| value.eq_ignore_ascii_case("external"))
    {
        return None;
    }
    let kind = node.attr("Type")?.rsplit('/').next()?;
    if !matches!(
        kind,
        "officeDocument"
            | "styles"
            | "stylesWithEffects"
            | "numbering"
            | "settings"
            | "theme"
            | "fontTable"
            | "header"
            | "footer"
            | "footnotes"
            | "endnotes"
            | "comments"
            | "commentsExtended"
            | "image"
    ) {
        return None;
    }
    let target = internal_target(owner, node.attr("Target")?)?;
    included.contains(&target).then_some(target)
}

struct Renderer<'a> {
    office: &'a Office,
    metadata: &'a mut OfficePreview,
    relations: &'a HashMap<(String, String), String>,
    included: &'a HashSet<String>,
    part: &'a str,
    xml: &'a Xml,
    pieces: HashMap<(usize, usize), usize>,
}
impl Renderer<'_> {
    fn document(&mut self) -> Result<String> {
        self.range(0, self.xml.source.len(), &self.xml.roots)
    }
    fn range(&mut self, start: usize, end: usize, children: &[usize]) -> Result<String> {
        let mut result = String::new();
        let mut cursor = start;
        for &child in children {
            let node = &self.xml.nodes[child];
            result.push_str(&self.xml.source[cursor..node.start]);
            result.push_str(&self.node(child)?);
            cursor = node.end;
        }
        result.push_str(&self.xml.source[cursor..end]);
        Ok(result)
    }
    fn node(&mut self, index: usize) -> Result<String> {
        let node = &self.xml.nodes[index];
        let local = node.local();
        if matches!(
            local,
            "altChunk" | "object" | "control" | "bookmarkStart" | "bookmarkEnd"
        ) {
            return Ok(String::new());
        }
        if local == "Relationship"
            && relationship_target(
                &relationship_owner(self.part).unwrap_or_default(),
                node,
                self.included,
            )
            .is_none()
        {
            return Ok(String::new());
        }
        if local == "Override"
            && node
                .attr("PartName")
                .is_some_and(|name| !self.included.contains(name.trim_start_matches('/')))
        {
            return Ok(String::new());
        }
        if local == "Default"
            && node.attr("Extension").is_some_and(|ext| {
                !matches!(
                    ext.to_ascii_lowercase().as_str(),
                    "xml" | "rels" | "png" | "jpg" | "jpeg" | "bmp" | "tif" | "tiff"
                )
            })
        {
            return Ok(String::new());
        }
        let is_word = node.name.starts_with(&format!("{}:", self.xml.word_prefix));
        if is_word && matches!(local, "hyperlink" | "smartTag") {
            return self.range(node.open_end, node.close_start, &node.children);
        }
        if is_word && local == "r" {
            return self.run(index);
        }
        if node.empty {
            return Ok(node.opening(true));
        }
        let mut output = node.opening(false);
        output.push_str(&self.range(node.open_end, node.close_start, &node.children)?);
        output.push_str(&format!("</{}>", node.name));
        Ok(output)
    }
    fn run(&mut self, index: usize) -> Result<String> {
        let node = &self.xml.nodes[index];
        let mut ancestor = node.parent;
        let mut supported_layout = true;
        while let Some(parent) = ancestor {
            let parent = &self.xml.nodes[parent];
            if matches!(parent.local(), "txbxContent" | "AlternateContent" | "del") {
                supported_layout = false;
            }
            ancestor = parent.parent;
        }
        let prefix = &self.xml.word_prefix;
        let mut properties = String::new();
        let mut hidden = false;
        for &child in &node.children {
            if self.xml.nodes[child].local() == "rPr" {
                properties.push_str(&self.node(child)?);
                hidden |= self.xml.source[self.xml.nodes[child].start..self.xml.nodes[child].end]
                    .contains(":vanish");
            }
        }
        let mut output = String::new();
        for &child in &node.children {
            let element = &self.xml.nodes[child];
            if element.local() == "rPr" {
                continue;
            }
            let rendered = self.node(child)?;
            if rendered.is_empty() {
                continue;
            }
            let mut bookmarks = Vec::new();
            if !hidden
                && element.local() == "t"
                && let Some(&piece_index) =
                    self.pieces.get(&(element.open_end, element.close_start))
            {
                let anchor = &mut self.metadata.anchors[piece_index];
                anchor.available_in_layout = supported_layout
                    && !self.part.contains("comments")
                    && !self.part.contains("footnotes")
                    && !self.part.contains("endnotes");
                bookmarks.push((anchor.id.clone(), piece_index + 100_000));
            }
            if supported_layout && matches!(element.local(), "drawing" | "pict") {
                let mut pending = vec![child];
                while let Some(image_node) = pending.pop() {
                    let image = &self.xml.nodes[image_node];
                    let reference = if image.local() == "blip" {
                        image.attr("embed")
                    } else if image.local() == "imagedata" {
                        image.attr("id")
                    } else {
                        None
                    };
                    if let Some(reference) = reference
                        && let Some(target) = self
                            .relations
                            .get(&(self.part.to_owned(), reference.to_owned()))
                        && let Some(media_index) = self
                            .office
                            .media_entries
                            .iter()
                            .position(|name| name == target)
                    {
                        let media = &mut self.metadata.images[media_index];
                        let id = format!("sixa_image_{}_{}", media_index, media.occurrences.len());
                        let number = 1_000_000 + media_index * 10_001 + media.occurrences.len();
                        media.occurrences.push(id.clone());
                        bookmarks.push((id, number));
                    }
                    pending.extend(image.children.iter().rev().copied());
                }
            }
            for (name, number) in &bookmarks {
                output.push_str(&format!(
                    "<{prefix}:bookmarkStart {prefix}:id=\"{number}\" {prefix}:name=\"{name}\"/>"
                ));
            }
            output.push_str(&node.opening(false));
            output.push_str(&properties);
            output.push_str(&rendered);
            output.push_str(&format!("</{}>", node.name));
            for (_, number) in bookmarks.iter().rev() {
                output.push_str(&format!("<{prefix}:bookmarkEnd {prefix}:id=\"{number}\"/>"));
            }
        }
        Ok(output)
    }
}

impl Office {
    pub fn presentation(
        &self,
        original: &[u8],
        extension: &str,
        revision: u64,
        include_docx: bool,
    ) -> Result<Presentation> {
        let mut utf16 = 0usize;
        let mut previous = 0usize;
        let anchors = self
            .pieces
            .iter()
            .enumerate()
            .map(|(index, piece)| {
                utf16 += self.text[previous..piece.span.start].encode_utf16().count();
                let text = &self.text[piece.span.start..piece.span.end];
                let start = utf16;
                utf16 += text.encode_utf16().count();
                previous = piece.span.end;
                OfficeAnchor {
                    id: format!("sixa_text_{index}"),
                    text: text.into(),
                    display: Span { start, end: utf16 },
                    label: label(&piece.part),
                    available_in_layout: false,
                }
            })
            .collect();
        let mut metadata = OfficePreview {
            revision,
            layout_available: false,
            reason: None,
            anchors,
            images: self
                .media_entries
                .iter()
                .enumerate()
                .map(|(index, name)| OfficeImage {
                    index: index as u32,
                    name: name.clone(),
                    occurrences: Vec::new(),
                })
                .collect(),
            warnings: Vec::new(),
        };
        if extension != "docx" {
            metadata.reason = Some("该 Office 格式提供内容预览，导出保留原文件格式".into());
            return Ok(Presentation {
                metadata,
                docx: None,
            });
        }
        let mut archive = ZipArchive::new(Cursor::new(original)).map_err(invalid)?;
        let expanded = (0..archive.len()).try_fold(0u64, |sum, index| {
            Ok::<_, Error>(sum.saturating_add(archive.by_index(index).map_err(invalid)?.size()))
        })?;
        if expanded > MAX_LAYOUT_BYTES {
            metadata.reason = Some("文档解压后超过 50 MiB，已改为内容预览以降低内存占用".into());
            return Ok(Presentation {
                metadata,
                docx: None,
            });
        }
        let mut included = HashSet::new();
        for index in 0..archive.len() {
            let entry = archive.by_index(index).map_err(invalid)?;
            if safe_entry(entry.name()) && !entry.is_dir() {
                included.insert(entry.name().to_owned());
            }
        }
        let mut names = included.iter().cloned().collect::<Vec<_>>();
        names.sort();
        let mut xmls = HashMap::new();
        let mut count = 0;
        for name in &names {
            if !name.ends_with(".xml") && !name.ends_with(".rels") {
                continue;
            }
            let mut xml = String::new();
            archive
                .by_name(name)
                .map_err(invalid)?
                .read_to_string(&mut xml)?;
            match Xml::parse(xml, &mut count) {
                Ok(xml) => {
                    xmls.insert(name.clone(), xml);
                }
                Err(Error::Unsupported(reason)) => {
                    metadata.reason = Some(reason);
                    return Ok(Presentation {
                        metadata,
                        docx: None,
                    });
                }
                Err(error) => return Err(error),
            }
        }
        let mut relations = HashMap::new();
        for (part, xml) in &xmls {
            if let Some(owner) = relationship_owner(part) {
                for node in &xml.nodes {
                    if node.local() == "Relationship"
                        && let Some(target) = relationship_target(&owner, node, &included)
                        && let Some(id) = node.attr("Id")
                    {
                        relations.insert((owner.clone(), id.into()), target);
                    }
                }
            }
        }
        let mut writer = include_docx.then(|| ZipWriter::new(Cursor::new(Vec::new())));
        for name in &names {
            if let Some(xml) = xmls.get(name) {
                let pieces = self
                    .pieces
                    .iter()
                    .enumerate()
                    .filter(|(_, piece)| piece.part == *name)
                    .map(|(index, piece)| ((piece.xml_start, piece.xml_end), index))
                    .collect();
                let transformed = Renderer {
                    office: self,
                    metadata: &mut metadata,
                    relations: &relations,
                    included: &included,
                    part: name,
                    xml,
                    pieces,
                }
                .document()?;
                if let Some(writer) = &mut writer {
                    writer
                        .start_file(
                            name,
                            SimpleFileOptions::default()
                                .compression_method(zip::CompressionMethod::Deflated),
                        )
                        .map_err(invalid)?;
                    writer.write_all(transformed.as_bytes())?;
                }
            } else if let Some(writer) = &mut writer {
                writer
                    .raw_copy_file(archive.by_name(name).map_err(invalid)?)
                    .map_err(invalid)?;
            }
        }
        metadata.layout_available = true;
        if metadata
            .anchors
            .iter()
            .any(|anchor| !anchor.available_in_layout)
        {
            metadata
                .warnings
                .push("文档属性、批注、脚注、域代码及未显示的文字可在内容列表中复核".into());
        }
        metadata.warnings.push(
            "Word 布局为本机近似预览，分页和字体可能与 Word 不同；外部链接及嵌入网页不在预览中加载"
                .into(),
        );
        let docx = writer
            .map(|writer| {
                writer
                    .finish()
                    .map(|cursor| cursor.into_inner())
                    .map_err(invalid)
            })
            .transpose()?;
        Ok(Presentation { metadata, docx })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn package(extra: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in extra {
            zip.start_file(
                *name,
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
            )
            .unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }
    fn mixed() -> Vec<u8> {
        let drawing = r#"<w:drawing><wp:inline><wp:extent cx="914400" cy="914400"/><wp:docPr id="1" name="photo"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic><pic:nvPicPr><pic:cNvPr id="1" name="photo.png"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="image"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="914400"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing>"#;
        let second_drawing = drawing.replace("id=\"1\"", "id=\"2\"");
        let document = format!(
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><w:body><w:p><w:r><w:rPr><w:b/></w:rPr><w:t>😀张</w:t><w:t>三</w:t></w:r><w:hyperlink r:id="external"><w:r><w:t>同一个张三</w:t></w:r></w:hyperlink><w:r>{drawing}</w:r></w:p><w:tbl><w:tblPr><w:tblW w:w="5000" w:type="dxa"/></w:tblPr><w:tblGrid><w:gridCol w:w="5000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="5000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>同一个张三</w:t>{second_drawing}</w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:instrText>域代码张三</w:instrText></w:r></w:p><w:altChunk r:id="html"/><w:sectPr><w:headerReference w:type="default" r:id="header"/><w:footerReference w:type="default" r:id="footer"/><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:bottom="1440" w:left="1440" w:right="1440" w:header="720" w:footer="720"/></w:sectPr></w:body></w:document>"#
        );
        let relationships = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="image" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/photo.png"/><Relationship Id="header" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/><Relationship Id="footer" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/><Relationship Id="comments" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="comments.xml"/><Relationship Id="external" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.invalid/secret" TargetMode="External"/><Relationship Id="html" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/aFChunk" Target="chunk.html"/></Relationships>"#;
        let png = crate::raster::encode_pages(
            &[image::RgbaImage::from_pixel(
                8,
                8,
                image::Rgba([30, 120, 70, 255]),
            )],
            crate::raster::RasterFormat::Png,
        )
        .unwrap();
        package(&[
            ("[Content_Types].xml", br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/><Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/><Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/><Override PartName="/word/chunk.html" ContentType="text/html"/></Types>"#.to_vec()),
            ("_rels/.rels", br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="document" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#.to_vec()),
            ("word/document.xml", document.as_bytes().to_vec()),
            ("word/_rels/document.xml.rels", relationships.as_bytes().to_vec()),
            ("word/header1.xml", br#"<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:r><w:t>Header</w:t></w:r></w:p></w:hdr>"#.to_vec()),
            ("word/footer1.xml", br#"<w:ftr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:r><w:t>Footer</w:t></w:r></w:p></w:ftr>"#.to_vec()),
            ("word/comments.xml", "<w:comments xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:comment w:id=\"0\" w:author=\"Fixture\"><w:p><w:r><w:t>批注作者张三</w:t></w:r></w:p></w:comment></w:comments>".as_bytes().to_vec()),
            ("docProps/core.xml", "<properties><creator>文档作者张三</creator></properties>".as_bytes().to_vec()),
            ("word/chunk.html", b"<script>fetch('https://example.invalid')</script>".to_vec()),
            ("word/media/photo.png", png),
        ])
    }
    fn entry(bytes: &[u8], name: &str) -> String {
        let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut text = String::new();
        archive
            .by_name(name)
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        text
    }
    #[test]
    fn mixed_docx_maps_repeated_text_and_image_occurrences_without_external_content() {
        let bytes = mixed();
        let office = Office::read(&bytes, "docx").unwrap();
        let before = office.text.clone();
        let prepared = office.presentation(&bytes, "docx", 7, true).unwrap();
        if let Some(folder) = std::env::var_os("SIXA_PREVIEW_FIXTURE_DIR") {
            let folder = std::path::PathBuf::from(folder);
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(folder.join("mixed-source.docx"), &bytes).unwrap();
            std::fs::write(
                folder.join("mixed-presentation.docx"),
                prepared.docx.as_ref().unwrap(),
            )
            .unwrap();
            std::fs::write(
                folder.join("mixed-metadata.json"),
                serde_json::to_vec_pretty(&prepared.metadata).unwrap(),
            )
            .unwrap();
        }
        assert!(prepared.metadata.layout_available);
        assert_eq!(prepared.metadata.revision, 7);
        assert_eq!(prepared.metadata.anchors.len(), office.pieces.len());
        let utf16 = office.text.encode_utf16().collect::<Vec<_>>();
        for anchor in &prepared.metadata.anchors {
            assert_eq!(
                String::from_utf16(&utf16[anchor.display.start..anchor.display.end]).unwrap(),
                anchor.text
            );
        }
        let repeated = prepared
            .metadata
            .anchors
            .iter()
            .filter(|a| a.text == "同一个张三")
            .collect::<Vec<_>>();
        assert_eq!(repeated.len(), 2);
        assert_ne!(repeated[0].id, repeated[1].id);
        assert!(repeated.iter().all(|a| a.available_in_layout));
        assert!(
            prepared
                .metadata
                .anchors
                .iter()
                .any(|a| a.text == "文档作者张三"
                    && !a.available_in_layout
                    && a.label == "文档属性")
        );
        assert!(
            prepared
                .metadata
                .anchors
                .iter()
                .any(|a| a.text == "域代码张三" && !a.available_in_layout)
        );
        assert_eq!(prepared.metadata.images[0].occurrences.len(), 2);
        let preview = prepared.docx.unwrap();
        let xml = entry(&preview, "word/document.xml");
        for anchor in repeated {
            assert!(xml.contains(&format!("w:name=\"{}\"", anchor.id)));
        }
        for occurrence in &prepared.metadata.images[0].occurrences {
            assert!(xml.contains(occurrence));
        }
        assert!(!xml.contains("altChunk"));
        assert!(!xml.contains("hyperlink"));
        assert!(!entry(&preview, "word/_rels/document.xml.rels").contains("example.invalid"));
        assert!(
            ZipArchive::new(Cursor::new(&preview))
                .unwrap()
                .by_name("word/chunk.html")
                .is_err()
        );
        assert_eq!(office.text, before);
        assert!(!entry(&bytes, "word/document.xml").contains("sixa_text_"));
        let metadata_only = office.presentation(&bytes, "docx", 7, false).unwrap();
        assert!(metadata_only.docx.is_none());
        assert_eq!(
            serde_json_equivalent(&metadata_only.metadata),
            serde_json_equivalent(&prepared.metadata)
        );
    }
    fn serde_json_equivalent(metadata: &OfficePreview) -> Vec<(String, usize, usize, bool)> {
        metadata
            .anchors
            .iter()
            .map(|a| {
                (
                    a.id.clone(),
                    a.display.start,
                    a.display.end,
                    a.available_in_layout,
                )
            })
            .collect()
    }
    #[test]
    fn output_anchors_are_remapped_to_actual_output_and_never_enter_export() {
        let bytes = mixed();
        let office = Office::read(&bytes, "docx").unwrap();
        let start = office.text.find("😀张三").unwrap() + "😀".len();
        let entity = Entity {
            id: uuid::Uuid::new_v4(),
            entity_type: "PERSON".into(),
            score: 1.0,
            source: "test".into(),
            selected: true,
            span: Span {
                start,
                end: start + "张三".len(),
            },
            replacement: Some("一位访客".into()),
        };
        let output = office.render(&bytes, &[entity], &[]).unwrap();
        assert!(!entry(&output, "word/document.xml").contains("sixa_text_"));
        assert!(
            entry(&output, "word/_rels/document.xml.rels").contains("example.invalid"),
            "presentation sanitizing must never alter exports"
        );
        let result = Office::read(&output, "docx").unwrap();
        let metadata = result
            .presentation(&output, "docx", 8, false)
            .unwrap()
            .metadata;
        let utf16 = result.text.encode_utf16().collect::<Vec<_>>();
        for anchor in metadata.anchors {
            assert_eq!(
                String::from_utf16(&utf16[anchor.display.start..anchor.display.end]).unwrap(),
                anchor.text
            );
        }
        assert!(result.text.contains("😀一位访客"));
    }
    #[test]
    fn layout_limits_fall_back_without_losing_extracted_content() {
        let many = format!(
            "<w:document xmlns:w=\"urn:word\"><w:p>{}</w:p></w:document>",
            "<w:r><w:t>A</w:t></w:r>".repeat(3400)
        );
        let bytes = package(&[
            ("[Content_Types].xml", b"<Types/>".to_vec()),
            ("word/document.xml", many.into_bytes()),
        ]);
        let office = Office::read(&bytes, "docx").unwrap();
        let preview = office.presentation(&bytes, "docx", 1, true).unwrap();
        assert!(!preview.metadata.layout_available);
        assert!(preview.docx.is_none());
        assert!(preview.metadata.reason.unwrap().contains("10000"));
        assert_eq!(preview.metadata.anchors.len(), 3400);
        let mut oversized = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, size) in [
            ("[Content_Types].xml", 0),
            ("word/document.xml", 0),
            ("padding.bin", 51 * 1024),
        ] {
            oversized
                .start_file(
                    name,
                    SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Deflated),
                )
                .unwrap();
            if size == 0 {
                oversized
                    .write_all(if name == "word/document.xml" {
                        b"<document><t>retained</t></document>"
                    } else {
                        b"<Types/>"
                    })
                    .unwrap();
            } else {
                for _ in 0..size {
                    oversized.write_all(&[0; 1024]).unwrap();
                }
            }
        }
        let bytes = oversized.finish().unwrap().into_inner();
        let office = Office::read(&bytes, "docx").unwrap();
        let preview = office.presentation(&bytes, "docx", 1, true).unwrap();
        assert!(!preview.metadata.layout_available);
        assert!(preview.metadata.reason.unwrap().contains("50 MiB"));
        assert_eq!(preview.metadata.anchors[0].text, "retained");
    }
}
