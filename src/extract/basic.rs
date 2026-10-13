//! Buffered/sequential extraction strategies.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};

use anyhow::Result;

use crate::extract::{ExtractFile, ExtractionResult};
use crate::iso::SECTOR_SIZE;

/// Method 1: Sequential per-file reads (baseline)
/// Opens file, seeks to each file's sector, reads its sectors
pub(crate) fn extract_sequential(
    iso_path: &std::path::Path,
    root_offset: u64,
    files: &[ExtractFile],
    out_dir: &std::path::Path,
) -> Result<ExtractionResult> {
    let start = std::time::Instant::now();
    let file = File::open(iso_path)?;
    let mut reader = BufReader::new(file);
    let mut files_extracted = 0u64;
    let mut bytes_extracted = 0u64;

    for f in files {
        if f.is_dir {
            std::fs::create_dir_all(out_dir.join(&f.path))?;
            continue;
        }
        if let Some(parent) = std::path::Path::new(&f.path).parent() {
            std::fs::create_dir_all(out_dir.join(parent))?;
        }
        let data_offset = root_offset + f.sector as u64 * SECTOR_SIZE;
        reader.seek(SeekFrom::Start(data_offset))?;
        let mut remaining = f.size;
        let mut out = File::create(out_dir.join(&f.path))?;
        let mut buf = vec![0u8; 64 * 1024];
        while remaining > 0 {
            let to_read = remaining.min(buf.len() as u64) as usize;
            reader.read_exact(&mut buf[..to_read])?;
            out.write_all(&buf[..to_read])?;
            remaining -= to_read as u64;
            bytes_extracted += to_read as u64;
        }
        files_extracted += 1;
    }
    let duration = start.elapsed();
    Ok(ExtractionResult {
        method: "sequential".to_string(),
        duration_ms: duration.as_millis() as u64,
        files_extracted,
        bytes_extracted,
        throughput_mb_s: bytes_extracted as f64 / duration.as_secs_f64() / 1_048_576.0,
    })
}

/// Method 2: Bulk read - read entire data region in one go, then slice
/// Computes min/max sector range covering all files, reads once
pub(crate) fn extract_bulk_read(
    iso_path: &std::path::Path,
    root_offset: u64,
    files: &[ExtractFile],
    out_dir: &std::path::Path,
) -> Result<ExtractionResult> {
    let start = std::time::Instant::now();
    let file = File::open(iso_path)?;
    let mut reader = BufReader::new(file);

    // Find the contiguous data range
    let mut min_sector = u32::MAX;
    let mut max_end_sector = 0u32;
    for f in files {
        if f.is_dir {
            continue;
        }
        let end = f.sector + f.size.div_ceil(SECTOR_SIZE) as u32;
        min_sector = min_sector.min(f.sector);
        max_end_sector = max_end_sector.max(end);
    }
    if min_sector == u32::MAX {
        return Ok(ExtractionResult {
            method: "bulk_read".to_string(),
            duration_ms: 0,
            files_extracted: 0,
            bytes_extracted: 0,
            throughput_mb_s: 0.0,
        });
    }

    let data_start = root_offset + min_sector as u64 * SECTOR_SIZE;
    let data_len = (max_end_sector - min_sector) as u64 * SECTOR_SIZE;

    // Read entire data region
    reader.seek(SeekFrom::Start(data_start))?;
    let mut data = vec![0u8; data_len as usize];
    reader.read_exact(&mut data)?;

    // Extract files by slicing
    let mut files_extracted = 0u64;
    let mut bytes_extracted = 0u64;
    for f in files {
        if f.is_dir {
            std::fs::create_dir_all(out_dir.join(&f.path))?;
            continue;
        }
        if let Some(parent) = std::path::Path::new(&f.path).parent() {
            std::fs::create_dir_all(out_dir.join(parent))?;
        }
        let rel_start = (f.sector - min_sector) as u64 * SECTOR_SIZE;
        let rel_end = rel_start + f.size;
        let slice = &data[rel_start as usize..rel_end as usize];
        std::fs::write(out_dir.join(&f.path), slice)?;
        files_extracted += 1;
        bytes_extracted += f.size;
    }

    let duration = start.elapsed();
    Ok(ExtractionResult {
        method: "bulk_read".to_string(),
        duration_ms: duration.as_millis() as u64,
        files_extracted,
        bytes_extracted,
        throughput_mb_s: bytes_extracted as f64 / duration.as_secs_f64() / 1_048_576.0,
    })
}

/// Method 5: Large buffered sequential - bigger buffer, fewer syscalls
pub(crate) fn extract_large_buffer(
    iso_path: &std::path::Path,
    root_offset: u64,
    files: &[ExtractFile],
    out_dir: &std::path::Path,
) -> Result<ExtractionResult> {
    let start = std::time::Instant::now();
    let file = File::open(iso_path)?;
    let mut reader = BufReader::with_capacity(4 * 1024 * 1024, file); // 4MB buffer
    let mut files_extracted = 0u64;
    let mut bytes_extracted = 0u64;

    for f in files {
        if f.is_dir {
            std::fs::create_dir_all(out_dir.join(&f.path))?;
            continue;
        }
        if let Some(parent) = std::path::Path::new(&f.path).parent() {
            std::fs::create_dir_all(out_dir.join(parent))?;
        }
        let data_offset = root_offset + f.sector as u64 * SECTOR_SIZE;
        reader.seek(SeekFrom::Start(data_offset))?;
        let mut remaining = f.size;
        let mut out = File::create(out_dir.join(&f.path))?;
        let mut buf = vec![0u8; 4 * 1024 * 1024]; // 4MB write buffer
        while remaining > 0 {
            let to_read = remaining.min(buf.len() as u64) as usize;
            reader.read_exact(&mut buf[..to_read])?;
            out.write_all(&buf[..to_read])?;
            remaining -= to_read as u64;
            bytes_extracted += to_read as u64;
        }
        files_extracted += 1;
    }
    let duration = start.elapsed();
    Ok(ExtractionResult {
        method: "large_buffer".to_string(),
        duration_ms: duration.as_millis() as u64,
        files_extracted,
        bytes_extracted,
        throughput_mb_s: bytes_extracted as f64 / duration.as_secs_f64() / 1_048_576.0,
    })
}
