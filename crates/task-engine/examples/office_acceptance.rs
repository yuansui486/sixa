use domain::Selection;
use recognition::ner::Raner;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use task_engine::Engine;
use zip::ZipArchive;

#[derive(Serialize)]
struct Case {
    extension: String,
    entities: BTreeMap<String, usize>,
    output_bytes: usize,
    vba_hash_unchanged: Option<bool>,
    embedded_media_changed: bool,
}

fn vba_hash(path: &Path) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let file = std::fs::File::open(path)?;
    let mut archive = ZipArchive::new(file)?;
    let Ok(mut entry) = archive.by_name("xl/vbaProject.bin") else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut entry, &mut bytes)?;
    Ok(Some(format!("{:x}", Sha256::digest(bytes))))
}

fn media_hashes(path: &Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let file = std::fs::File::open(path)?;
    let mut archive = ZipArchive::new(file)?;
    let mut hashes = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        if entry.name().contains("/media/") {
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut bytes)?;
            hashes.push(format!("{:x}", Sha256::digest(bytes)));
        }
    }
    hashes.sort();
    Ok(hashes)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1).map(PathBuf::from);
    let model = args.next().ok_or("RaNER model directory required")?;
    let ocr_model = args.next().ok_or("OCR model directory required")?;
    let fixture_dir = args.next().ok_or("Office fixture directory required")?;
    let output_dir = args.next().ok_or("output directory required")?;
    if args.next().is_some() {
        return Err("usage: office_acceptance RANER_DIR OCR_DIR FIXTURE_DIR OUTPUT_DIR".into());
    }
    std::fs::create_dir_all(&output_dir)?;
    let work = tempfile::tempdir()?;
    let mut engine = Engine {
        store: storage::Store::open(work.path(), zeroize::Zeroizing::new([41; 32]))?,
        ner: Some(Box::new(Raner::load(&model)?)),
        ocr_mobile: Some(Box::new(recognition::ocr::PpOcr::load(&ocr_model)?)),
        ocr_accurate: None,
        active_ocr: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
    };
    let mut cases = Vec::new();
    for extension in ["docx", "xlsx", "xlsm"] {
        let source = fixture_dir.join(format!("fixture.{extension}"));
        let before_vba = vba_hash(&source)?;
        let before_media = media_hashes(&source)?;
        let task = engine.analyze_file(&source)?;
        eprintln!(
            "{extension} entities: {:?}",
            task.entities
                .iter()
                .map(|entity| (&entity.entity_type, entity.selected, entity.display))
                .collect::<Vec<_>>()
        );
        let mut entities = BTreeMap::new();
        for entity in &task.entities {
            *entities.entry(entity.entity_type.clone()).or_insert(0) += 1;
        }
        let task = engine.select(
            task.meta.id,
            task.entities
                .iter()
                .map(|entity| Selection {
                    id: entity.id,
                    selected: true,
                    replacement: entity.replacement.clone(),
                })
                .collect(),
        )?;
        engine.execute(task.meta.id)?;
        let destination = output_dir.join(format!("output.{extension}"));
        engine.export(task.meta.id, &destination)?;
        let output = std::fs::read(&destination)?;
        let read_back = formats::Document::load(extension, output.clone())?;
        eprintln!(
            "{extension} sensitive flags: {:?}",
            ["张三", "13812345678", "北京大学"]
                .into_iter()
                .map(|sensitive| read_back.text.contains(sensitive))
                .collect::<Vec<_>>()
        );
        for sensitive in ["张三", "13812345678", "北京大学"] {
            if read_back.text.contains(sensitive) {
                return Err(format!("{extension} output still contains sensitive text").into());
            }
        }
        let after_vba = vba_hash(&destination)?;
        let after_media = media_hashes(&destination)?;
        cases.push(Case {
            extension: extension.into(),
            entities,
            output_bytes: output.len(),
            vba_hash_unchanged: before_vba.map(|hash| after_vba.as_ref() == Some(&hash)),
            embedded_media_changed: !before_media.is_empty() && before_media != after_media,
        });
        engine.store.delete(task.meta.id)?;
    }
    println!("{}", serde_json::to_string_pretty(&cases)?);
    Ok(())
}
