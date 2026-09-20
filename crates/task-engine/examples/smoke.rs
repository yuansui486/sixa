//! Real-model text workflow, encryption and recovery smoke test in a temporary directory.
use recognition::ner::Raner;
use std::time::Instant;
use task_engine::Engine;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let path = std::path::Path::new(args.get(1).ok_or("model directory required")?);
    let start = Instant::now();
    let ner = Raner::load(path)?;
    let load_ms = start.elapsed().as_millis();
    let dir = tempfile::tempdir()?;
    let mut engine = Engine {
        store: storage::Store::open(dir.path(), zeroize::Zeroizing::new([17; 32]))?,
        ner: Some(Box::new(ner)),
        ocr_mobile: None,
        ocr_accurate: None,
        active_ocr: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
    };
    let source = "😀张三在北京大学工作，电话13812345678。\r\n";
    let start = Instant::now();
    let task = engine.analyze_text(source.into())?;
    let analyze_ms = start.elapsed().as_millis();
    let preview = engine.preview(task.meta.id)?;
    if preview.contains("张三") || preview.contains("北京大学") || preview.contains("13812345678")
    {
        return Err("sensitive content remained in preview".into());
    }
    engine.execute(task.meta.id)?;
    let destination = dir.path().join("中文路径脱敏.txt");
    engine.export(task.meta.id, &destination)?;
    assert_eq!(std::fs::read_to_string(destination)?, preview);
    let recovery = dir.path().join("恢复.ldsrec");
    engine.export_recovery(task.meta.id, "synthetic-test-password", &recovery)?;
    let restored = dir.path().join("恢复后.txt");
    engine.restore(&recovery, "synthetic-test-password", &restored)?;
    assert_eq!(std::fs::read_to_string(restored)?, source);
    println!(
        "{}",
        serde_json::json!({"passed":true,"entities":task.entities.len(),"model_load_ms":load_ms,"analyze_ms":analyze_ms,"preview":preview})
    );
    Ok(())
}
