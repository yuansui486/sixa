//! Offline native runtime smoke: uses a generated ONNX graph, never downloads model weights.
use recognition::{
    Ner, models,
    ner::Raner,
    ocr::{Ocr, OcrRun, PpOcr},
};
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

fn tensor_info(name: &str, dimensions: &[u64]) -> Vec<u8> {
    let shape: Vec<u8> = dimensions
        .iter()
        .flat_map(|&n| bytes(1, integer(1, n)))
        .collect();
    [
        bytes(1, name),
        bytes(2, bytes(1, [integer(1, 1), bytes(2, shape)].concat())),
    ]
    .concat()
}

// Tiny constant graphs exercise the production OCR loader and all three run paths.
fn constant_model(input: &[u64], output: &[u64], values: &[f32]) -> Vec<u8> {
    let mut tensor: Vec<u8> = output.iter().flat_map(|&n| integer(1, n)).collect();
    tensor.extend(integer(2, 1));
    tensor.extend(bytes(
        9,
        values
            .iter()
            .flat_map(|n| n.to_le_bytes())
            .collect::<Vec<_>>(),
    ));
    let attribute = [bytes(1, "value"), bytes(5, tensor), integer(20, 4)].concat();
    let node = [
        bytes(2, "output"),
        bytes(4, "Constant"),
        bytes(5, attribute),
    ]
    .concat();
    let graph = [
        bytes(1, node),
        bytes(2, "offline-ocr"),
        bytes(11, tensor_info("input", input)),
        bytes(12, tensor_info("output", output)),
    ]
    .concat();
    [integer(1, 8), bytes(7, graph), bytes(8, integer(2, 11))].concat()
}

fn probe_ocr() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    std::fs::write(
        root.join("det.onnx"),
        constant_model(&[1, 3, 64, 64], &[1, 1, 2, 2], &[0.; 4]),
    )?;
    std::fs::write(
        root.join("cls.onnx"),
        constant_model(&[1, 3, 48, 192], &[1, 2], &[1., 0.]),
    )?;
    std::fs::write(
        root.join("rec.onnx"),
        constant_model(&[1, 3, 48, 48], &[1, 1, 3], &[1., 0., 0.]),
    )?;
    std::fs::write(root.join("dict.txt"), "字\n")?;
    std::fs::write(root.join("ocr-config.json"), "{}")?;
    let files = models::OCR_MODEL_FILES.iter().map(|name| {
        let path = root.join(name);
        if !path.exists() { std::fs::write(&path, b"runtime loaded explicitly").unwrap(); }
        json!({"name":name,"size":std::fs::metadata(path).unwrap().len(),"sha256":"0".repeat(64)})
    }).collect::<Vec<_>>();
    std::fs::write(
        root.join("manifest.json"),
        serde_json::to_vec(
            &json!({"schema":1,"version":"synthetic-ocr","source":"generated offline","license":"AGPL-3.0-or-later","files":files}),
        )?,
    )?;
    let verified = models::inspect_model_with_required(root, models::OCR_MODEL_FILES)?;
    let mut ocr = PpOcr::load_with_progress(&verified, &mut |phase| {
        eprintln!("runtime_probe OCR: {phase}")
    })?;
    ocr.warm_up()?;
    let run = OcrRun::new()?;
    run.cancel()?;
    assert!(ocr.warm_up_with_run(&run).is_err());
    ocr.warm_up()?;
    assert!(
        ocr.recognize(
            &image::RgbaImage::from_pixel(64, 64, image::Rgba([255; 4])),
            &OcrRun::new()?
        )?
        .is_empty()
    );
    std::fs::write(root.join("det.onnx"), b"invalid graph")?;
    let corrupt = models::inspect_model_with_required(root, models::OCR_MODEL_FILES)?;
    assert!(PpOcr::load_verified(&corrupt).is_err());
    std::fs::remove_file(root.join("det.onnx"))?;
    assert!(models::inspect_model_with_required(root, models::OCR_MODEL_FILES).is_err());
    Ok(())
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
    probe_ocr()?;
    println!(
        "{}",
        json!({"passed":true,"arch":std::env::consts::ARCH,"elapsed_ms":start.elapsed().as_millis(),"fixture":"synthetic ONNX; not production NER quality validation"})
    );
    Ok(())
}
