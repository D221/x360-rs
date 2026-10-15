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

/// Result of a copy-out run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopyOutResult {
    pub files_copied: u64,
    pub bytes_copied: u64,
}

/// Split an image-internal path into lowercase components, accepting both
/// separators (GDF lookup is case-insensitive).
fn split_image_path(path: &str) -> Vec<String> {
    path.replace('\\', "/")
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .collect()
}

/// Split preserving original case (for output file names).
fn split_image_path_raw(path: &str) -> Vec<String> {
    path.replace('\\', "/")
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Copy a single file or directory out of an already-parsed XISO image
/// (same as xdvdfs `copy-out`). For a file source `dest` is the output
/// file (or an existing directory to place it in); for a directory source
/// `dest` is the directory the subtree is recreated under.
pub fn copy_out_iso(
    info: &crate::iso::IsoInfo,
    src: &str,
    dest: &std::path::Path,
    skip_systemupdate: bool,
) -> Result<CopyOutResult> {
    use anyhow::Context as _;
    let want = split_image_path(src);
    if want.is_empty() {
        anyhow::bail!(
            "copy-out needs a file or directory path inside the image (use --extract-iso for everything)"
        );
    }
    let base = crate::write::format::iso_type_offset(&info.iso_type);
    let entries: Vec<&crate::iso::IsoEntry> = info
        .entries
        .iter()
        .filter(|e| !skip_systemupdate || !crate::iso::is_systemupdate_path(&e.path))
        .collect();
    let target = entries
        .iter()
        .find(|e| split_image_path(&e.path) == want)
        .ok_or_else(|| anyhow::anyhow!("path not found in image: {src}"))?;
    let mut source = File::open(&info.file_path)
        .with_context(|| format!("open {} for copy-out", info.file_path))?;
    let mut result = CopyOutResult {
        files_copied: 0,
        bytes_copied: 0,
    };
    if !target.is_dir {
        let out_file = if dest.is_dir() {
            let raw = split_image_path_raw(&target.path);
            let name = raw.last().map(String::as_str).unwrap_or(&target.path);
            dest.join(name)
        } else {
            dest.to_path_buf()
        };
        if let Some(parent) = out_file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        stream_entry_bytes(&mut source, base, target.sector, target.size, &out_file)?;
        result.files_copied = 1;
        result.bytes_copied = target.size;
        return Ok(result);
    }
    // Directory source: recreate the subtree under dest.
    std::fs::create_dir_all(dest)?;
    for e in &entries {
        let norm = split_image_path(&e.path);
        if norm.len() < want.len() || norm[..want.len()] != want[..] {
            continue;
        }
        let raw = split_image_path_raw(&e.path);
        if raw.len() == want.len() {
            continue; // the directory entry itself
        }
        let out_path = dest.join(raw[want.len()..].join("/"));
        if e.is_dir {
            std::fs::create_dir_all(&out_path)?;
        } else {
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            stream_entry_bytes(&mut source, base, e.sector, e.size, &out_path)?;
            result.files_copied += 1;
            result.bytes_copied += e.size;
        }
    }
    Ok(result)
}

/// Stream one entry's data bytes from the image into `out_file`.
fn stream_entry_bytes(
    source: &mut File,
    base: u64,
    sector: u32,
    size: u64,
    out_file: &std::path::Path,
) -> Result<()> {
    use anyhow::Context as _;
    use std::io::Write as _;
    const BUF: usize = 1024 * 1024;
    source
        .seek(SeekFrom::Start(base + sector as u64 * SECTOR_SIZE))
        .context("seek entry data")?;
    let mut out =
        File::create(out_file).with_context(|| format!("create {}", out_file.display()))?;
    let mut remaining = size;
    let mut buf = vec![0u8; BUF];
    while remaining > 0 {
        let n = (remaining as usize).min(BUF);
        source
            .read_exact(&mut buf[..n])
            .context("read entry data")?;
        out.write_all(&buf[..n]).context("write copy-out data")?;
        remaining -= n as u64;
    }
    Ok(())
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
///
/// When `skip_systemupdate` is set, entries under `$SystemUpdate` are
/// skipped (same as extract-xiso `-s`).
pub fn extract_iso_fast_with_progress<F>(
    iso_path: &std::path::Path,
    out_dir: &std::path::Path,
    progress: F,
    title_id: Option<&str>,
    skip_systemupdate: bool,
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
    if skip_systemupdate {
        files.retain(|e| !crate::iso::is_systemupdate_path(&e.path));
    }

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
    extract_iso_fast_with_progress(iso_path, out_dir, |msg| eprintln!("{}", msg), None, false)
}
