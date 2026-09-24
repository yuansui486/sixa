//! Offline native runtime smoke: uses a generated ONNX graph, never downloads model weights.
use recognition::{Ner, models, ner::Raner, ocr::OcrRun};
use serde_json::json;
use std::{path::PathBuf, time::Instant};
fn varint(mut value: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    while value >= 128 {
        bytes.push((value as u8 & 127) | 128);
        value >>= 7;
    }
    bytes.push(value as u8);
    bytes
}
fn integer(field: u64, value: u64) -> Vec<u8> {
    [varint(field << 3), varint(value)].concat()
}
fn bytes(field: u64, value: impl AsRef<[u8]>) -> Vec<u8> {
    let value = value.as_ref();
    [
        varint((field << 3) | 2),
        varint(value.len() as u64),
        value.to_vec(),
    ]
    .concat()
}
fn value(name: &str, kind: u64, output: bool) -> Vec<u8> {
    let mut shape = [bytes(1, integer(1, 1)), bytes(1, bytes(2, "sequence"))].concat();
    if output {
        shape.extend(bytes(1, integer(1, 1)));
    }
    let tensor = [integer(1, kind), bytes(2, shape)].concat();
    [bytes(1, name), bytes(2, bytes(1, tensor))].concat()
}
fn model() -> Vec<u8> {
    let cast = [
        bytes(1, "input_ids"),
        bytes(2, "float_ids"),
        bytes(4, "Cast"),
        bytes(5, [bytes(1, "to"), integer(3, 1), integer(20, 2)].concat()),
    ]
    .concat();
    let unsqueeze = [
        bytes(1, "float_ids"),
        bytes(2, "emissions"),
        bytes(4, "Unsqueeze"),
        bytes(
            5,
            [bytes(1, "axes"), integer(8, 2), integer(20, 7)].concat(),
        ),
    ]
    .concat();
    let mut graph = [
        bytes(1, cast),
        bytes(1, unsqueeze),
        bytes(2, "offline-runtime-probe"),
    ]
    .concat();
    for name in ["input_ids", "attention_mask", "token_type_ids"] {
        graph.extend(bytes(11, value(name, 7, false)));
    }
    graph.extend(bytes(12, value("emissions", 1, true)));
    [
        integer(1, 8),
        bytes(2, "sixa-runtime-probe"),
        bytes(7, graph),
        bytes(8, integer(2, 11)),
    ]
    .concat()
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let library = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("runtime library path required")?,
    )
    .canonicalize()?;
    let start = Instant::now();
    eprintln!(
        "runtime_probe: loading runtime arch={}",
        std::env::consts::ARCH
    );
    ort::init_from(&library)?.commit();
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    std::fs::write(root.join("emissions.onnx"), model())?;
    std::fs::write(
        root.join("tokenizer.json"),
        serde_json::to_vec(
            &json!({"version":"1.0", "truncation":null,"padding":null,"added_tokens":[],"normalizer":null,"pre_tokenizer":null,"post_processor":null,"decoder":null,"model":{"type":"WordPiece","unk_token":"[UNK]","continuing_subword_prefix":"##","max_input_chars_per_word":100,"vocab":{"[UNK]":100}}}),
        )?,
    )?;
    std::fs::write(
        root.join("crf.json"),
        serde_json::to_vec(
            &json!({"labels":["O"],"start":[0.0],"end":[0.0],"transitions":[[0.0]]}),
        )?,
    )?;
    // The runtime has already been initialized explicitly above. The model-file
    // loader must keep using it when the following fallback path does not exist.
    let files = models::RANER_MODEL_FILES.iter().map(|name| {
        let path = root.join(name);
        if !path.exists() { std::fs::write(&path, b"runtime loaded explicitly").unwrap(); }
        json!({"name":name,"size":std::fs::metadata(path).unwrap().len(),"sha256":"0".repeat(64)})
    }).collect::<Vec<_>>();
    std::fs::write(
        root.join("manifest.json"),
        serde_json::to_vec(
            &json!({"schema":1,"version":"synthetic-test","source":"generated offline","license":"AGPL-3.0-or-later","files":files}),
        )?,
    )?;
    let verified = models::inspect_model(root)?;
    let mut ner =
        Raner::load_with_progress(&verified, &mut |phase| eprintln!("runtime_probe: {phase}"))?;
    let run = OcrRun::new()?;
    assert!(ner.analyze_with_run("张三在北京工作", &run)?.is_empty());
    run.cancel()?;
    assert!(ner.analyze_with_run("张三", &run).is_err());
    let fresh = OcrRun::new()?;
    assert!(ner.analyze_with_run("再次推理", &fresh)?.is_empty());
    println!(
        "{}",
        json!({"passed":true,"arch":std::env::consts::ARCH,"elapsed_ms":start.elapsed().as_millis(),"fixture":"synthetic ONNX; not production NER quality validation"})
    );
    Ok(())
}
