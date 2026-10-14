#![allow(dead_code)]
#![allow(clippy::needless_borrow)]
//! XISO image writer orchestrator.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::iso::IsoInfo;
use crate::write::dirtab::{
    build_dir_tree_from_entries, calculate_sizes, calculate_sizes_recursive, DirTreeNode,
};
use crate::write::format::{iso_type_offset, sector_offset, OutputFormat};
use crate::write::sector::{required_sectors, SectorAllocator, SECTOR_SIZE};
use crate::write::volume::VolumeDescriptor;

struct WriteFrame {
    node: *mut DirTreeNode,
    table_sector: u32,
    entry_index: usize,
    entry_sectors: Vec<u32>,
    awaiting_child: bool,
}

fn iso_type_root_offset(iso_type: &str) -> u64 {
    iso_type_offset(iso_type)
}

pub(crate) fn check_duplicates(node: &DirTreeNode) -> Result<()> {
    use std::collections::BTreeSet;
    let mut seen = BTreeSet::new();
    for entry in &node.entries {
        if !seen.insert(entry.name.clone()) {
            anyhow::bail!("duplicate entry: {}", entry.name);
        }
    }
    for child in &node.children {
        check_duplicates(child)?;
    }
    Ok(())
}

fn copy_file_data(
    source: &mut File,
    source_offset: u64,
    output: &mut File,
    output_offset: u64,
    size: u64,
) -> Result<()> {
    const BUF_SIZE: usize = 1024 * 1024;

    output
        .seek(SeekFrom::Start(output_offset))
        .context("seek output for file data")?;
    source
        .seek(SeekFrom::Start(source_offset))
        .context("seek source file data")?;

    if size == 0 {
        let fill = vec![0xFFu8; SECTOR_SIZE as usize];
        output.write_all(&fill).context("write empty file sector")?;
        return Ok(());
    }

    let mut remaining = size;
    let mut buffer = vec![0u8; BUF_SIZE];
    while remaining > 0 {
        let to_read = (remaining as usize).min(buffer.len());
        source.read_exact(&mut buffer[..to_read])?;
        output.write_all(&buffer[..to_read])?;
        remaining -= to_read as u64;
    }
    Ok(())
}

fn bytestore_u32_le(out: &mut [u8], value: u32) {
    out[0] = (value & 0xFF) as u8;
    out[1] = ((value >> 8) & 0xFF) as u8;
    out[2] = ((value >> 16) & 0xFF) as u8;
    out[3] = ((value >> 24) & 0xFF) as u8;
}

fn write_directory(
    node: &mut DirTreeNode,
    allocator: &mut SectorAllocator,
    output: &mut File,
    source: &mut Option<File>,
    source_base: u64,
    output_base: u64,
    log: &dyn Fn(String),
) -> Result<u32> {
    let root_ptr = node as *mut DirTreeNode;
    let root_table_bytes =
        (unsafe { (*root_ptr).table_size_sectors } as usize) * (SECTOR_SIZE as usize);
    if root_table_bytes == 0 {
        anyhow::bail!("directory table has zero size");
    }
    let root_table_sector = allocator.allocate_contiguous(root_table_bytes as u64);
    unsafe {
        (*root_ptr).allocated_sector = root_table_sector;
    }

    let mut frames = vec![WriteFrame {
        node: root_ptr,
        table_sector: root_table_sector,
        entry_index: 0,
        entry_sectors: Vec::new(),
        awaiting_child: false,
    }];

    let mut root_result = 0u32;

    while !frames.is_empty() {
        let last_idx = frames.len() - 1;
        let node_ptr = frames[last_idx].node;

        let entry_count = unsafe { (*node_ptr).entries.len() };
        if frames[last_idx].entry_index >= entry_count {
            let frame = frames.pop().unwrap();
            let table_bytes_size =
                (unsafe { (*frame.node).table_size_sectors } as usize) * (SECTOR_SIZE as usize);
            let mut buffer = vec![0xFFu8; table_bytes_size];
            let mut pos = 0usize;
            let count = unsafe { (*frame.node).entries.len() };
            for i in 0..count {
                let sector = frame.entry_sectors[i];
                let name = unsafe { (&(*frame.node).entries)[i].name.clone() };
                let attrs = unsafe { (&(*frame.node).entries)[i].attributes };
                let size = unsafe { (&(*frame.node).entries)[i].size };
                if size > u32::MAX as u64 {
                    anyhow::bail!("entry too large for GDF");
                }
                let name_bytes = name.as_bytes();
                let entry_size = (14 + name_bytes.len() + 3) & !3;
                if pos + entry_size > buffer.len() {
                    anyhow::bail!("directory table overflow");
                }
                buffer[pos] = 0;
                buffer[pos + 1] = 0;
                buffer[pos + 2] = 0;
                buffer[pos + 3] = 0;
                bytestore_u32_le(&mut buffer[pos + 4..pos + 8], sector);
                bytestore_u32_le(&mut buffer[pos + 8..pos + 12], size as u32);
                buffer[pos + 12] = attrs;
                buffer[pos + 13] = name_bytes.len() as u8;
                buffer[pos + 14..pos + 14 + name_bytes.len()].copy_from_slice(name_bytes);
                pos += entry_size;
            }

            output
                .seek(SeekFrom::Start(
                    output_base + frame.table_sector as u64 * SECTOR_SIZE,
                ))
                .context("seek output directory table")?;
            output.write_all(&buffer).context("write directory table")?;

            let path_display = unsafe {
                if (*frame.node).path.as_os_str().is_empty() {
                    "/".to_string()
                } else {
                    format!("/{}", (*frame.node).path.display())
                }
            };
            log(format!(
                "DirAdded({}, {})",
                path_display, frame.table_sector
            ));

            let result = frame.table_sector;
            if !frames.is_empty() {
                let parent = frames.last_mut().unwrap();
                parent.entry_sectors.push(result);
                parent.awaiting_child = false;
                parent.entry_index += 1;
            } else {
                root_result = result;
            }
            continue;
        }

        let i = frames[last_idx].entry_index;
        let is_dir = unsafe { (&(*node_ptr).entries)[i].is_dir };
        let size = unsafe { (&(*node_ptr).entries)[i].size };

        if is_dir {
            if !frames[last_idx].awaiting_child {
                let child_index = unsafe {
                    (&(*node_ptr).entries)[i]
                        .child_index
                        .ok_or_else(|| anyhow::anyhow!("missing child index"))?
                };
                if child_index >= unsafe { (*node_ptr).children.len() } {
                    anyhow::bail!("child index out of range");
                }
                let child_ptr =
                    unsafe { &mut (&mut (*node_ptr).children)[child_index] as *mut DirTreeNode };
                let child_table_bytes =
                    (unsafe { (*child_ptr).table_size_sectors } as usize) * (SECTOR_SIZE as usize);
                if child_table_bytes == 0 {
                    anyhow::bail!("child directory table has zero size");
                }
                let child_table_sector = allocator.allocate_contiguous(child_table_bytes as u64);
                unsafe {
                    (*child_ptr).allocated_sector = child_table_sector;
                }
                if unsafe { (&(*node_ptr).entries)[i].size } == 0 {
                    unsafe {
                        (&mut (*node_ptr).entries)[i].size =
                            ((*child_ptr).table_size_sectors) as u64 * SECTOR_SIZE
                    };
                }

                frames[last_idx].awaiting_child = true;
                frames.push(WriteFrame {
                    node: child_ptr,
                    table_sector: child_table_sector,
                    entry_index: 0,
                    entry_sectors: Vec::new(),
                    awaiting_child: false,
                });
            }
            continue;
        } else {
            if size > u32::MAX as u64 {
                anyhow::bail!("file too large for GDF");
            }
            let source_sector = unsafe { (&(*node_ptr).entries)[i].source_sector };
            let on_disk = unsafe { (&(*node_ptr).entries)[i].on_disk.clone() };
            let file_sector = allocator.allocate_contiguous(size);
            if let Some(path) = on_disk {
                let mut src_file = File::open(&path)
                    .with_context(|| format!("open on-disk source {}", path.display()))?;
                copy_file_data(
                    &mut src_file,
                    0,
                    output,
                    output_base + file_sector as u64 * SECTOR_SIZE,
                    size,
                )?;
            } else {
                let src = source
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("no source ISO for file entry"))?;
                copy_file_data(
                    src,
                    source_base + source_sector as u64 * SECTOR_SIZE,
                    output,
                    output_base + file_sector as u64 * SECTOR_SIZE,
                    size,
                )?;
            }
            let name = unsafe { (&(*node_ptr).entries)[i].name.clone() };
            let file_path = unsafe {
                if (*node_ptr).path.as_os_str().is_empty() {
                    name.clone()
                } else {
                    format!("{}/{}", (*node_ptr).path.display(), name)
                }
            };
            log(format!("FileAdded({}, {})", file_path, file_sector));

            frames[last_idx].entry_sectors.push(file_sector);
            frames[last_idx].entry_index += 1;
        }
    }

    Ok(root_result)
}

pub fn create_xiso_from_parsed(
    info: &IsoInfo,
    output: &Path,
    fmt: OutputFormat,
    log: &dyn Fn(String),
) -> Result<()> {
    if info.file_path.is_empty() {
        anyhow::bail!("IsoInfo has no source file path");
    }

    let mut root = build_dir_tree_from_entries(&info.entries);
    calculate_sizes(&mut root);
    calculate_sizes_recursive(&mut root);
    check_duplicates(&root)?;

    create_xiso_from_tree(
        root,
        Some(PathBuf::from(&info.file_path)),
        iso_type_root_offset(&info.iso_type),
        output,
        fmt,
        info.volume_timestamp.unwrap_or(0),
        log,
    )
}

pub fn create_xiso_from_tree(
    mut root: DirTreeNode,
    source_iso: Option<PathBuf>,
    source_base: u64,
    output: &Path,
    fmt: OutputFormat,
    volume_timestamp: u64,
    log: &dyn Fn(String),
) -> Result<()> {
    let output_base = sector_offset(fmt);

    let mut source = match source_iso {
        Some(path) => Some(
            File::open(&path)
                .with_context(|| format!("failed to open source ISO {}", path.display()))?,
        ),
        None => None,
    };
    let mut output_file = File::create(output)
        .with_context(|| format!("failed to create output {}", output.display()))?;

    let mut allocator = SectorAllocator::default();

    let root_sector = write_directory(
        &mut root,
        &mut allocator,
        &mut output_file,
        &mut source,
        source_base,
        output_base,
        log,
    )?;

    let volume_descriptor = VolumeDescriptor::new(
        root_sector,
        root.table_size_sectors * SECTOR_SIZE as u32,
        volume_timestamp,
    );
    let descriptor_offset = output_base + 32 * SECTOR_SIZE;
    output_file
        .seek(SeekFrom::Start(descriptor_offset))
        .context("seek volume descriptor")?;
    output_file
        .write_all(&volume_descriptor.serialize())
        .context("write volume descriptor")?;

    let rel_final = allocator.total_sectors() as u64 * SECTOR_SIZE;
    let rel_padded = if rel_final.is_multiple_of(32 * SECTOR_SIZE) {
        rel_final
    } else {
        rel_final + (32 * SECTOR_SIZE - rel_final % (32 * SECTOR_SIZE))
    };

    output_file
        .set_len(output_base + rel_padded)
        .context("pad output image")?;

    log("FinishedCopyingImageData".to_string());
    log("FinishedPacking".to_string());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iso::IsoEntry;
    use std::fs;

    #[test]
    fn test_helper_boundaries() {
        assert_eq!(required_sectors(0), 1);
    }

    #[test]
    fn test_check_duplicates_ok() {
        let root = build_dir_tree_from_entries(&[IsoEntry {
            path: "a.txt".into(),
            sector: 0,
            size: 3,
            is_dir: false,
            attrs: crate::iso::IsoEntryAttrs::default(),
            left_child_offset: None,
            right_child_offset: None,
        }]);
        assert!(check_duplicates(&root).is_ok());
    }

    #[test]
    fn test_create_xiso_empty_image() {
        let dir = std::env::temp_dir().join(format!("tu_img_empty_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let out = dir.join("empty.iso");

        let mut img = vec![0u8; 64 * 2048];
        let desc = 0x20 * 2048;
        img[desc..desc + 20].copy_from_slice(b"MICROSOFT*XBOX*MEDIA");
        let src = dir.join("src.iso");
        fs::write(&src, &img).unwrap();

        let info = crate::iso::IsoInfo {
            file_path: src.display().to_string(),
            file_size: img.len() as u64,
            iso_type: "XSF".to_string(),
            root_sector: 33,
            root_size: 2048,
            volume_sectors: 64,
            volume_timestamp: Some(0),
            xex_exec: None,
            xbe_info: None,
            executable: None,
            entries: vec![],
        };

        create_xiso_from_parsed(&info, &out, OutputFormat::Xsf, &(|_| ())).unwrap();
        assert!(out.exists());
        assert!(fs::metadata(&out).unwrap().len() > 0);

        let _ = fs::remove_dir_all(&dir);
    }
}
