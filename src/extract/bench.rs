//! Extraction benchmark harness.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};

use anyhow::Result;

use crate::constants::TuError;
use crate::extract::basic::{extract_bulk_read, extract_large_buffer, extract_sequential};
use crate::extract::mmap::extract_mmap;
use crate::extract::parallel::extract_parallel;
use crate::extract::{ExtractFile, ExtractionResult};
use crate::iso::{detect_iso_type, read_u32_le, walk_gdf_table, SECTOR_SIZE};

/// Run all extraction methods on an ISO and return the fastest
pub fn benchmark_extract_iso(
    iso_path: &std::path::Path,
    out_base: &std::path::Path,
) -> Result<Vec<ExtractionResult>> {
    // First parse to get file list
    let file = File::open(iso_path)?;
    let file_size = file.metadata()?.len();
    let mut reader = BufReader::new(file);
    let (_iso_type, root_offset) =
        detect_iso_type(&mut reader, file_size)?.ok_or(TuError::UnknownFormat)?;

    let desc_at = 0x20 * SECTOR_SIZE + root_offset;
    reader.seek(SeekFrom::Start(desc_at))?;
    let mut ident = [0u8; 20];
    reader.read_exact(&mut ident)?;
    let root_sector = read_u32_le(&mut reader)?;
    let root_size = read_u32_le(&mut reader)?;

    let mut files = Vec::new();
    walk_gdf_table(
        &mut reader,
        root_offset,
        "",
        root_sector,
        root_size,
        0,
        &mut files,
    )?;

    let extract_files: Vec<ExtractFile> = files
        .iter()
        .map(|e| ExtractFile {
            path: e.path.clone(),
            sector: e.sector,
            size: e.size,
            is_dir: e.is_dir,
        })
        .collect();

    // Create output directories for each method
    let methods = vec![
        ("sequential", 1),
        ("bulk_read", 1),
        ("mmap", 1),
        ("large_buffer", 1),
        ("parallel_2", 2),
        ("parallel_4", 4),
        ("parallel_8", 8),
    ];

    let mut results = Vec::new();
    for (method, threads) in methods {
        let out_dir = out_base.join(method);
        std::fs::remove_dir_all(&out_dir).ok();
        std::fs::create_dir_all(&out_dir)?;

        let result = match method {
            "sequential" => extract_sequential(iso_path, root_offset, &extract_files, &out_dir)?,
            "bulk_read" => extract_bulk_read(iso_path, root_offset, &extract_files, &out_dir)?,
            "mmap" => extract_mmap(iso_path, root_offset, &extract_files, &out_dir)?,
            "large_buffer" => {
                extract_large_buffer(iso_path, root_offset, &extract_files, &out_dir)?
            }
            _ if method.starts_with("parallel_") => {
                extract_parallel(iso_path, root_offset, &extract_files, &out_dir, threads)?
            }
            _ => continue,
        };
        println!(
            "  {}: {:.1} MB/s ({}ms, {} files, {} MB)",
            result.method,
            result.throughput_mb_s,
            result.duration_ms,
            result.files_extracted,
            result.bytes_extracted / 1_048_576
        );
        results.push(result);
    }

    // Sort by throughput
    results.sort_by(|a, b| b.throughput_mb_s.partial_cmp(&a.throughput_mb_s).unwrap());

    println!(
        "\n  Fastest: {} at {:.1} MB/s",
        results[0].method, results[0].throughput_mb_s
    );

    Ok(results)
}
