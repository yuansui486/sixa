use domain::{OcrProfile, PdfMode, TaskOptions};
use recognition::{ner::Raner, ocr::PpOcr};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Instant,
};
use task_engine::Engine;

#[derive(Clone, Serialize, Deserialize)]
struct ResultRow {
    source_index: usize,
    source_sha256: String,
    mode: PdfMode,
    pages: usize,
    entities: BTreeMap<String, usize>,
    output_sha256: String,
    output_bytes: u64,
    elapsed_ms: u128,
}

#[derive(Serialize, Deserialize)]
struct Report {
    schema: u32,
    private_sources_committed: bool,
    cases: Vec<ResultRow>,
}

fn digest(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    Ok(format!("{:x}", Sha256::digest(std::fs::read(path)?)))
}

fn save_report(path: &Path, cases: &[ResultRow]) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let report = Report {
        schema: 2,
        private_sources_committed: false,
        cases: cases.to_vec(),
    };
    std::fs::write(path, serde_json::to_vec_pretty(&report)?)?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    if arguments.len() != 4 {
        return Err("usage: resume_acceptance RANER_DIR OCR_DIR PDF_DIR REPORT_JSON".into());
    }
    let mut sources = std::fs::read_dir(&arguments[2])?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
        })
        .collect::<Vec<_>>();
    sources.sort();
    sources.truncate(6);
    if sources.len() != 6 {
        return Err(format!("expected six private PDFs, found {}", sources.len()).into());
    }

    let work = tempfile::tempdir()?;
    let store = storage::Store::open(work.path(), zeroize::Zeroizing::new([29; 32]))?;
    let mut engine = Engine {
        store,
        ner: Some(Box::new(Raner::load(&arguments[0])?)),
        ocr_mobile: None,
        ocr_accurate: Some(Box::new(PpOcr::load(&arguments[1])?)),
        active_ocr: Arc::new(Mutex::new(std::collections::HashMap::new())),
    };
    let mut cases = std::fs::read(&arguments[3])
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Report>(&bytes).ok())
        .filter(|report| report.schema == 2 && !report.private_sources_committed)
        .map(|report| report.cases)
        .unwrap_or_default();
    for (source_index, source) in sources.into_iter().enumerate() {
        if cases
            .iter()
            .filter(|case| case.source_index == source_index)
            .count()
            == 2
        {
            println!("skipping completed private case {}/6", source_index + 1);
            continue;
        }
        let source_sha256 = digest(&source)?;
        println!("analyzing private case {}/6", source_index + 1);
        let analyze_started = Instant::now();
        let task = engine.analyze_file_with_options(
            &source,
            TaskOptions {
                ocr_profile: OcrProfile::Accurate,
                pdf_mode: PdfMode::SafeRebuild,
            },
        )?;
        let analysis_ms = analyze_started.elapsed().as_millis();
        let source_bytes = std::fs::read(&source)?;
        let pages = formats::pdf::page_count(&source_bytes)?;
        let mut entities = BTreeMap::new();
        for entity in &task.entities {
            *entities.entry(entity.entity_type.clone()).or_insert(0) += 1;
        }

        let safe_started = Instant::now();
        engine.execute(task.meta.id)?;
        let safe_output = work.path().join(format!("{}-safe.pdf", task.meta.id));
        engine.export(task.meta.id, &safe_output)?;
        let safe_bytes = std::fs::read(&safe_output)?;
        formats::pdf::validate(&safe_bytes)?;
        cases.push(ResultRow {
            source_index,
            source_sha256: source_sha256.clone(),
            mode: PdfMode::SafeRebuild,
            pages,
            entities: entities.clone(),
            output_sha256: format!("{:x}", Sha256::digest(&safe_bytes)),
            output_bytes: safe_bytes.len() as u64,
            elapsed_ms: analysis_ms + safe_started.elapsed().as_millis(),
        });
        println!("completed private case {}/6 safe rebuild", source_index + 1);

        let fidelity_started = Instant::now();
        let font = std::fs::read(r"C:\Windows\Fonts\msyh.ttc")
            .ok()
            .and_then(|bytes| ab_glyph::FontArc::try_from_vec(bytes).ok());
        let fidelity_bytes = formats::pdf::redact(
            &source_bytes,
            &task.regions,
            font.as_ref(),
            PdfMode::Fidelity,
        )?;
        formats::pdf::validate(&fidelity_bytes)?;
        cases.push(ResultRow {
            source_index,
            source_sha256,
            mode: PdfMode::Fidelity,
            pages,
            entities,
            output_sha256: format!("{:x}", Sha256::digest(&fidelity_bytes)),
            output_bytes: fidelity_bytes.len() as u64,
            elapsed_ms: analysis_ms + fidelity_started.elapsed().as_millis(),
        });
        println!("completed private case {}/6 fidelity", source_index + 1);
        engine.store.delete(task.meta.id)?;
        save_report(&arguments[3], &cases)?;
    }
    save_report(&arguments[3], &cases)?;
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "cases": cases.len(),
            "report": arguments[3],
        }))?
    );
    Ok(())
}
