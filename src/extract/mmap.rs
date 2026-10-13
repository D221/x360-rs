//! Memory-mapped extraction strategies (Unix and Windows).

use std::fs::File;
use std::sync::Arc;

use anyhow::Result;

use crate::extract::{ExtractFile, ExtractionResult};
use crate::iso::SECTOR_SIZE;

/// Method 3: Memory map - mmap the entire ISO, slice directly
/// Zero-copy for reads, OS handles paging
#[cfg(unix)]
pub(crate) fn extract_mmap(
    iso_path: &std::path::Path,
    root_offset: u64,
    files: &[ExtractFile],
    out_dir: &std::path::Path,
) -> Result<ExtractionResult> {
    use std::os::unix::io::AsRawFd;
    use std::ptr;

    let start = std::time::Instant::now();
    let file = File::open(iso_path)?;
    let file_size = file.metadata()?.len();

    // mmap the file
    let mmap = unsafe {
        libc::mmap(
            ptr::null_mut(),
            file_size as usize,
            libc::PROT_READ,
            libc::MAP_PRIVATE,
            file.as_raw_fd(),
            0,
        )
    };
    if mmap == libc::MAP_FAILED {
        return Err(std::io::Error::last_os_error().into());
    }
    let data = unsafe { std::slice::from_raw_parts(mmap as *const u8, file_size as usize) };

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
        let data_offset = (root_offset + f.sector as u64 * SECTOR_SIZE) as usize;
        let slice = &data[data_offset..data_offset + f.size as usize];
        std::fs::write(out_dir.join(&f.path), slice)?;
        files_extracted += 1;
        bytes_extracted += f.size;
    }

    unsafe { libc::munmap(mmap, file_size as usize) };

    let duration = start.elapsed();
    Ok(ExtractionResult {
        method: "mmap".to_string(),
        duration_ms: duration.as_millis() as u64,
        files_extracted,
        bytes_extracted,
        throughput_mb_s: bytes_extracted as f64 / duration.as_secs_f64() / 1_048_576.0,
    })
}

/// Method 3b: Windows memory map equivalent
#[cfg(windows)]
pub(crate) fn extract_mmap(
    iso_path: &std::path::Path,
    root_offset: u64,
    files: &[ExtractFile],
    out_dir: &std::path::Path,
) -> Result<ExtractionResult> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Memory::{
        CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, FILE_MAP_READ,
        MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READONLY,
    };

    let start = std::time::Instant::now();
    let file = File::open(iso_path)?;
    let file_size = file.metadata()?.len();

    let handle = file.as_raw_handle();
    let mapping = unsafe {
        CreateFileMappingW(
            HANDLE(handle),
            None,
            PAGE_READONLY,
            (file_size >> 32) as u32,
            (file_size & 0xFFFFFFFF) as u32,
            None,
        )
    };
    let mapping = match mapping {
        Ok(h) => h,
        Err(_) => return Err(std::io::Error::last_os_error().into()),
    };
    if mapping.is_invalid() {
        return Err(std::io::Error::last_os_error().into());
    }
    let view: MEMORY_MAPPED_VIEW_ADDRESS =
        unsafe { MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0) };
    if view.Value.is_null() {
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(mapping) };
        return Err(std::io::Error::last_os_error().into());
    }
    let data = unsafe { std::slice::from_raw_parts(view.Value as *const u8, file_size as usize) };

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
        let data_offset = (root_offset + f.sector as u64 * SECTOR_SIZE) as usize;
        let slice = &data[data_offset..data_offset + f.size as usize];
        std::fs::write(out_dir.join(&f.path), slice)?;
        files_extracted += 1;
        bytes_extracted += f.size;
    }

    unsafe {
        let _ = UnmapViewOfFile(view);
        let _ = windows::Win32::Foundation::CloseHandle(mapping);
    }
    let duration = start.elapsed();
    Ok(ExtractionResult {
        method: "mmap".to_string(),
        duration_ms: duration.as_millis() as u64,
        files_extracted,
        bytes_extracted,
        throughput_mb_s: bytes_extracted as f64 / duration.as_secs_f64() / 1_048_576.0,
    })
}

/// Progress-enabled mmap extraction (Unix)
#[cfg(unix)]
pub(crate) fn extract_mmap_with_progress<F>(
    iso_path: &std::path::Path,
    root_offset: u64,
    files: &[ExtractFile],
    out_dir: &std::path::Path,
    progress: F,
) -> Result<ExtractionResult>
where
    F: Fn(String) + Send + Sync + 'static,
{
    use std::os::unix::io::AsRawFd;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    let start = Instant::now();
    let file = File::open(iso_path)?;
    let file_size = file.metadata()?.len();

    let mmap = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            file_size as usize,
            libc::PROT_READ,
            libc::MAP_PRIVATE,
            file.as_raw_fd(),
            0,
        )
    };
    if mmap == libc::MAP_FAILED {
        return Err(std::io::Error::last_os_error().into());
    }
    let data = unsafe { std::slice::from_raw_parts(mmap as *const u8, file_size as usize) };

    let total_files = files.iter().filter(|f| !f.is_dir).count() as u64;
    let files_extracted = Arc::new(AtomicU64::new(0));
    let progress = Arc::new(progress);

    for f in files {
        if f.is_dir {
            std::fs::create_dir_all(out_dir.join(&f.path))?;
            continue;
        }
        if let Some(parent) = std::path::Path::new(&f.path).parent() {
            std::fs::create_dir_all(out_dir.join(parent))?;
        }
        let data_offset = (root_offset + f.sector as u64 * SECTOR_SIZE) as usize;
        let slice = &data[data_offset..data_offset + f.size as usize];
        std::fs::write(out_dir.join(&f.path), slice)?;

        let count = files_extracted.fetch_add(1, Ordering::Relaxed) + 1;
        if count.is_multiple_of(50) || count == total_files {
            let p = progress.clone();
            p(format!("  [{}/{}] {}", count, total_files, f.path));
        }
    }

    unsafe { libc::munmap(mmap, file_size as usize) };

    let duration = start.elapsed();
    Ok(ExtractionResult {
        method: "mmap".to_string(),
        duration_ms: duration.as_millis() as u64,
        files_extracted: files_extracted.load(Ordering::Relaxed),
        bytes_extracted: files.iter().map(|f| f.size).sum(),
        throughput_mb_s: files.iter().map(|f| f.size).sum::<u64>() as f64
            / duration.as_secs_f64()
            / 1_048_576.0,
    })
}

/// Progress-enabled mmap extraction (Windows)
#[cfg(windows)]
pub(crate) fn extract_mmap_with_progress<F>(
    iso_path: &std::path::Path,
    root_offset: u64,
    files: &[ExtractFile],
    out_dir: &std::path::Path,
    progress: F,
) -> Result<ExtractionResult>
where
    F: Fn(String) + Send + Sync + 'static,
{
    use std::os::windows::io::AsRawHandle;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Memory::{
        CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, FILE_MAP_READ,
        MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READONLY,
    };

    let start = Instant::now();
    let file = File::open(iso_path)?;
    let file_size = file.metadata()?.len();

    let handle = file.as_raw_handle();
    let mapping = unsafe {
        CreateFileMappingW(
            HANDLE(handle),
            None,
            PAGE_READONLY,
            (file_size >> 32) as u32,
            (file_size & 0xFFFFFFFF) as u32,
            None,
        )
    };
    let mapping = match mapping {
        Ok(h) => h,
        Err(_) => return Err(std::io::Error::last_os_error().into()),
    };
    if mapping.is_invalid() {
        return Err(std::io::Error::last_os_error().into());
    }
    let view: MEMORY_MAPPED_VIEW_ADDRESS =
        unsafe { MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0) };
    if view.Value.is_null() {
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(mapping) };
        return Err(std::io::Error::last_os_error().into());
    }
    let data = unsafe { std::slice::from_raw_parts(view.Value as *const u8, file_size as usize) };

    let total_files = files.iter().filter(|f| !f.is_dir).count() as u64;
    let files_extracted = Arc::new(AtomicU64::new(0));
    let progress = Arc::new(progress);

    for f in files {
        if f.is_dir {
            std::fs::create_dir_all(out_dir.join(&f.path))?;
            continue;
        }
        if let Some(parent) = std::path::Path::new(&f.path).parent() {
            std::fs::create_dir_all(out_dir.join(parent))?;
        }
        let data_offset = (root_offset + f.sector as u64 * SECTOR_SIZE) as usize;
        let slice = &data[data_offset..data_offset + f.size as usize];
        std::fs::write(out_dir.join(&f.path), slice)?;

        let count = files_extracted.fetch_add(1, Ordering::Relaxed) + 1;
        if count.is_multiple_of(50) || count == total_files {
            let p = progress.clone();
            p(format!("  [{}/{}] {}", count, total_files, f.path));
        }
    }

    unsafe {
        let _ = UnmapViewOfFile(view);
        let _ = windows::Win32::Foundation::CloseHandle(mapping);
    }
    let duration = start.elapsed();
    Ok(ExtractionResult {
        method: "mmap".to_string(),
        duration_ms: duration.as_millis() as u64,
        files_extracted: files_extracted.load(Ordering::Relaxed),
        bytes_extracted: files.iter().map(|f| f.size).sum(),
        throughput_mb_s: files.iter().map(|f| f.size).sum::<u64>() as f64
            / duration.as_secs_f64()
            / 1_048_576.0,
    })
}
