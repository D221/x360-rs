//! Multi-threaded extraction strategies.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;

use anyhow::Result;

use crate::extract::{ExtractFile, ExtractionResult};
use crate::iso::SECTOR_SIZE;

/// Method 4: Parallel extraction - split files across threads
/// Uses a thread pool, each thread has its own file handle
pub(crate) fn extract_parallel(
    iso_path: &std::path::Path,
    root_offset: u64,
    files: &[ExtractFile],
    out_dir: &std::path::Path,
    num_threads: usize,
) -> Result<ExtractionResult> {
    let start = std::time::Instant::now();

    let files: Vec<_> = files.iter().filter(|f| !f.is_dir).cloned().collect();
    if files.is_empty() {
        return Ok(ExtractionResult {
            method: format!("parallel_{}", num_threads),
            duration_ms: 0,
            files_extracted: 0,
            bytes_extracted: 0,
            throughput_mb_s: 0.0,
        });
    }

    let bytes_extracted = Arc::new(AtomicU64::new(0));
    let files_extracted = Arc::new(AtomicU64::new(0));
    let iso_path = iso_path.to_path_buf();
    let out_dir = out_dir.to_path_buf();

    let chunk_size = files.len().div_ceil(num_threads);
    let mut handles = Vec::new();

    for chunk in files.chunks(chunk_size) {
        let chunk = chunk.to_vec();
        let iso_path = iso_path.clone();
        let out_dir = out_dir.clone();
        let bytes_extracted = bytes_extracted.clone();
        let files_extracted = files_extracted.clone();

        let handle = thread::spawn(move || -> Result<()> {
            let file = File::open(&iso_path)?;
            let mut reader = BufReader::new(file);
            for f in chunk {
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
                    bytes_extracted.fetch_add(to_read as u64, Ordering::Relaxed);
                }
                files_extracted.fetch_add(1, Ordering::Relaxed);
            }
            Ok(())
        });
        handles.push(handle);
    }

    for h in handles {
        h.join().unwrap()?;
    }

    let duration = start.elapsed();
    Ok(ExtractionResult {
        method: format!("parallel_{}", num_threads),
        duration_ms: duration.as_millis() as u64,
        files_extracted: files_extracted.load(Ordering::Relaxed),
        bytes_extracted: bytes_extracted.load(Ordering::Relaxed),
        throughput_mb_s: bytes_extracted.load(Ordering::Relaxed) as f64
            / duration.as_secs_f64()
            / 1_048_576.0,
    })
}

/// Progress-enabled parallel extraction (Windows only)
#[cfg(windows)]
pub(crate) fn extract_parallel_with_progress<F>(
    iso_path: &std::path::Path,
    root_offset: u64,
    files: &[ExtractFile],
    out_dir: &std::path::Path,
    num_threads: usize,
    progress: F,
) -> Result<ExtractionResult>
where
    F: Fn(String) + Send + Sync + 'static,
{
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    let start = Instant::now();

    let files: Vec<_> = files.iter().filter(|f| !f.is_dir).cloned().collect();
    if files.is_empty() {
        return Ok(ExtractionResult {
            method: format!("parallel_{}", num_threads),
            duration_ms: 0,
            files_extracted: 0,
            bytes_extracted: 0,
            throughput_mb_s: 0.0,
        });
    }

    let total_files = files.len() as u64;
    let _total_bytes = files.iter().map(|f| f.size).sum::<u64>();
    let bytes_extracted = Arc::new(AtomicU64::new(0));
    let files_extracted = Arc::new(AtomicU64::new(0));
    let iso_path = iso_path.to_path_buf();
    let out_dir = out_dir.to_path_buf();
    let progress = Arc::new(progress);

    // Progress reporter thread
    let progress_files = files_extracted.clone();
    let progress_bytes = bytes_extracted.clone();
    let progress_total = total_files;
    let progress_progress = progress.clone();
    let reporter = thread::spawn(move || {
        loop {
            thread::sleep(std::time::Duration::from_millis(200));
            let f = progress_files.load(Ordering::Relaxed);
            let b = progress_bytes.load(Ordering::Relaxed);
            if f >= progress_total {
                break;
            }
            if f.is_multiple_of(10) || f > 0 {
                let mb = b as f64 / 1_048_576.0;
                progress_progress(format!("  [{}/{}] {:.1} MB", f, progress_total, mb));
            }
        }
        // Final report
        let f = progress_files.load(Ordering::Relaxed);
        let b = progress_bytes.load(Ordering::Relaxed);
        let mb = b as f64 / 1_048_576.0;
        progress_progress(format!("  [{}/{}] {:.1} MB done", f, progress_total, mb));
    });

    let chunk_size = files.len().div_ceil(num_threads);
    let mut handles = Vec::new();

    for chunk in files.chunks(chunk_size) {
        let chunk = chunk.to_vec();
        let iso_path = iso_path.clone();
        let out_dir = out_dir.clone();
        let bytes_extracted = bytes_extracted.clone();
        let files_extracted = files_extracted.clone();

        let handle = thread::spawn(move || -> Result<()> {
            let file = File::open(&iso_path)?;
            let mut reader = BufReader::new(file);
            for f in chunk {
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
                    bytes_extracted.fetch_add(to_read as u64, Ordering::Relaxed);
                }
                files_extracted.fetch_add(1, Ordering::Relaxed);
            }
            Ok(())
        });
        handles.push(handle);
    }

    for h in handles {
        h.join().unwrap()?;
    }
    reporter.join().ok();

    let duration = start.elapsed();
    Ok(ExtractionResult {
        method: format!("parallel_{}", num_threads),
        duration_ms: duration.as_millis() as u64,
        files_extracted: files_extracted.load(Ordering::Relaxed),
        bytes_extracted: bytes_extracted.load(Ordering::Relaxed),
        throughput_mb_s: bytes_extracted.load(Ordering::Relaxed) as f64
            / duration.as_secs_f64()
            / 1_048_576.0,
    })
}
