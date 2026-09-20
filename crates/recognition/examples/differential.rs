use recognition::{Ner, ner::Raner};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let mut ner = Raner::load(std::path::Path::new(
        args.get(1).ok_or("model directory required")?,
    ))?;
    let cases: serde_json::Value =
        serde_json::from_slice(&std::fs::read(args.get(2).ok_or("golden file required")?)?)?;
    let mut count = 0;
    for (index, case) in cases.as_array().ok_or("array required")?.iter().enumerate() {
        let text = case["text"].as_str().ok_or("text required")?;
        let actual = ner.analyze(text)?;
        let expected = case["entities"].as_array().ok_or("entities required")?;
        if actual.len() != expected.len() {
            return Err(format!(
                "case {index}: entity count {} != {}",
                actual.len(),
                expected.len()
            )
            .into());
        }
        for (a, b) in actual.iter().zip(expected) {
            if a.span.start != b["start"].as_u64().unwrap() as usize
                || a.span.end != b["end"].as_u64().unwrap() as usize
                || a.entity_type != b["type"].as_str().unwrap()
                || (a.score as f64 - b["score"].as_f64().unwrap()).abs() > 1e-4
            {
                return Err(format!("case {index}: mismatch {a:?} != {b}").into());
            }
            count += 1;
        }
    }
    println!(
        "RaNER differential passed: {} cases, {count} entities, score tolerance 1e-4",
        cases.as_array().unwrap().len()
    );
    Ok(())
}
