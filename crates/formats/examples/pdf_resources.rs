//! Reproducible PDF memory/first-preview probe. No network, models or private documents.
//! cargo run --release -p formats --example pdf_resources -- 100 preview
//! Modes: preview (new page API), legacy (all-page preview), rebuild, fidelity.
use domain::PdfMode;
use mupdf::{Size, pdf::PdfDocument};
use std::{hint::black_box, time::Instant};

#[cfg(windows)]
fn peak_memory() -> usize {
    #[repr(C)]
    #[derive(Default)]
    struct Counters {
        size: u32,
        faults: u32,
        peak_working_set: usize,
        working_set: usize,
        peak_paged_pool: usize,
        paged_pool: usize,
        peak_nonpaged_pool: usize,
        nonpaged_pool: usize,
        pagefile: usize,
        peak_pagefile: usize,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
    }
    #[link(name = "psapi")]
    unsafe extern "system" {
        fn GetProcessMemoryInfo(
            process: *mut std::ffi::c_void,
            counters: *mut Counters,
            size: u32,
        ) -> i32;
    }
    let mut counters = Counters {
        size: std::mem::size_of::<Counters>() as u32,
        ..Default::default()
    };
    // The structure has the documented PROCESS_MEMORY_COUNTERS layout, including on x64.
    unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters,
            std::mem::size_of::<Counters>() as u32,
        );
    }
    counters.peak_working_set
}
#[cfg(not(windows))]
fn peak_memory() -> usize {
    0
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().collect::<Vec<_>>();
    let pages: usize = args.get(1).ok_or("page count required")?.parse()?;
    if !(1..=200).contains(&pages) {
        return Err("page count must be 1–200".into());
    }
    let mode = args.get(2).map(String::as_str).unwrap_or("preview");
    if !["preview", "legacy", "rebuild", "fidelity"].contains(&mode) {
        return Err("unknown mode".into());
    }
    let mut document = PdfDocument::new();
    for _ in 0..pages {
        document.new_page(Size::new(595.0, 842.0))?;
    }
    let mut source = Vec::new();
    document.write_to(&mut source)?;
    drop(document);
    let started = Instant::now();
    let mut retained_pixels = 0usize;
    let output_bytes = match mode {
        "preview" => {
            let dimensions = formats::pdf::page_dimensions(&source)?;
            assert_eq!(dimensions.len(), pages);
            let first = formats::pdf::render_page(&source, 0, Some(1400))?;
            retained_pixels = first.as_raw().len();
            black_box(first);
            0
        }
        "legacy" => {
            let all = formats::pdf::render_pages(&source)?;
            retained_pixels = all.iter().map(|page| page.as_raw().len()).sum();
            black_box(all);
            0
        }
        "rebuild" | "fidelity" => {
            let output = formats::pdf::redact(
                &source,
                &[],
                None,
                if mode == "rebuild" {
                    PdfMode::SafeRebuild
                } else {
                    PdfMode::Fidelity
                },
            )?;
            assert_eq!(formats::pdf::page_count(&output)?, pages);
            black_box(output).len()
        }
        _ => unreachable!(),
    };
    println!(
        "{{\"pages\":{pages},\"mode\":\"{mode}\",\"elapsed_ms\":{},\"peak_working_set_bytes\":{},\"retained_raster_bytes\":{retained_pixels},\"output_bytes\":{output_bytes}}}",
        started.elapsed().as_millis(),
        peak_memory()
    );
    Ok(())
}
