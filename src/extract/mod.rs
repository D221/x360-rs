//! XISO extraction: shared types and the auto-selected fast path.

mod basic;
mod bench;
mod mmap;
mod parallel;

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::constants::TuError;
use crate::extract::mmap::extract_mmap_with_progress;
#[cfg(windows)]
use crate::extract::parallel::extract_parallel_with_progress;
use crate::iso::{detect_iso_type, read_u32_le, walk_gdf_table, SECTOR_SIZE};

pub use bench::benchmark_extract_iso;

/// Result of an extraction method run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionResult {
    pub method: String,
    pub duration_ms: u64,
    pub files_extracted: u64,
    pub bytes_extracted: u64,
    pub throughput_mb_s: f64,
}

/// Common file info for extraction
#[derive(Debug, Clone)]
pub struct ExtractFile {
    pub path: String,
    pub sector: u32,
    pub size: u64,
    pub is_dir: bool,
}

/// Sanitize a string for use as a directory name.
pub(crate) fn sanitize_dir_name(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect()
}

/// Extract with a custom progress callback.
pub fn extract_iso_fast_with_progress<F>(
    iso_path: &std::path::Path,
    out_dir: &std::path::Path,
    progress: F,
    title_id: Option<&str>,
) -> Result<ExtractionResult>
where
    F: Fn(String) + Send + Sync + 'static,
{
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

    // Create subfolder with title_id if provided
    let final_out_dir = if let Some(tid) = title_id {
        let safe_name = sanitize_dir_name(tid);
        out_dir.join(safe_name)
    } else {
        out_dir.to_path_buf()
    };
    std::fs::create_dir_all(&final_out_dir)?;

    let total_files = extract_files.iter().filter(|f| !f.is_dir).count() as u64;
    let total_bytes = extract_files.iter().map(|f| f.size).sum::<u64>();

    progress(format!(
        "Extracting {} files ({:.1} MB) to {}...",
        total_files,
        total_bytes as f64 / 1_048_576.0,
        final_out_dir.display()
    ));

    // Auto-select best method based on platform and ISO size
    #[cfg(unix)]
    {
        // mmap is usually fastest on Unix
        extract_mmap_with_progress(
            iso_path,
            root_offset,
            &extract_files,
            &final_out_dir,
            progress,
        )
    }
    #[cfg(windows)]
    {
        // On Windows, parallel extraction with 4 threads is optimal for large ISOs
        // (benchmarking shows 4 threads outperforms 8 due to contention)
        if file_size > 2 * 1024 * 1024 * 1024 {
            extract_parallel_with_progress(
                iso_path,
                root_offset,
                &extract_files,
                &final_out_dir,
                4,
                progress,
            )
        } else {
            extract_mmap_with_progress(
                iso_path,
                root_offset,
                &extract_files,
                &final_out_dir,
                progress,
            )
        }
    }
}

/// Extract using the best known method (mmap on Unix, parallel on Windows for large ISOs)
/// Shows progress to stderr by default.
#[allow(dead_code)]
pub fn extract_iso_fast(
    iso_path: &std::path::Path,
    out_dir: &std::path::Path,
) -> Result<ExtractionResult> {
    extract_iso_fast_with_progress(iso_path, out_dir, |msg| eprintln!("{}", msg), None)
}
