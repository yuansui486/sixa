use recognition::ocr::{Ocr, OcrRun, PpOcr};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Serialize)]
struct Output {
    lines: Vec<recognition::ocr::OcrLine>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let model = PathBuf::from(arguments.next().ok_or("missing model directory")?);
    let image = PathBuf::from(arguments.next().ok_or("missing image path")?);
    let output = arguments.next().map(PathBuf::from);
    if arguments.next().is_some() {
        return Err("usage: ocr_smoke MODEL_DIR IMAGE [OUTPUT_JSON]".into());
    }
    let image = image::open(image)?.to_rgba8();
    let mut ocr = PpOcr::load(&model)?;
    let lines = ocr.recognize(&image, &OcrRun::new()?)?;
    let json = serde_json::to_string_pretty(&Output { lines })?;
    if let Some(path) = output {
        std::fs::write(path, json + "\n")?;
    } else {
        println!("{json}");
    }
    Ok(())
}
