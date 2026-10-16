//! Streaming TU/DLC parser (`TuParser`) and file-table helpers.

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::constants::{content_type_name, magic_name, CONTENT_TITLE_UPDATE, MAGIC_LIVE};
use crate::regions::{collapse_regions, decode_regions};
use crate::report::{DirectoryEntry, TuInfo};
use crate::stfs::{entry_full_path, StfsFileEntry, StfsLayout};
use crate::tu_header::TuHeader;
use crate::xex::read_at;

pub struct TuParser {
    reader: BufReader<File>,
    file_size: u64,
}

impl TuParser {
    pub fn open(path: &PathBuf) -> Result<Self> {
        let file = File::open(path).context("Failed to open file")?;
        let file_size = file.metadata()?.len();
        let reader = BufReader::new(file);
        Ok(TuParser { reader, file_size })
    }

    pub fn parse(&mut self) -> Result<TuInfo> {
        let header = TuHeader::read(&mut self.reader, self.file_size)?;

        let is_title_update =
            header.content_type == CONTENT_TITLE_UPDATE && header.tu_signature == Some(0x44505554);
        let (detailed, collapsed) = match header.region_mask {
            Some(mask) => {
                let detailed = decode_regions(mask);
                let collapsed = collapse_regions(&detailed);
                (detailed, collapsed)
            }
            None => (Vec::new(), Vec::new()),
        };

        let mut raw_entries = Vec::new();
        let mut raw_strings = Vec::new();
        self.extract_strings_and_entries(&mut raw_entries, &mut raw_strings)?;
        let entries = raw_entries
            .iter()
            .map(|e| DirectoryEntry {
                filename: e.name.clone(),
                path: entry_full_path(&raw_entries, e.index),
                offset: e.table_offset,
                size: e.size,
                is_dir: e.is_dir,
                start_block: e.start_block,
            })
            .collect();

        Ok(TuInfo {
            file_path: String::new(),
            file_size: self.file_size,
            is_live: header.magic == MAGIC_LIVE,
            is_title_update,
            container_magic: magic_name(header.magic).to_string(),
            content_kind: content_type_name(header.content_type).to_string(),
            media_id: format!("{:08X}", header.media_id),
            base_title_id: format!("{:08X}", header.base_title_id),
            content_type: format!("0x{:08X}", header.content_type),
            bootable: crate::constants::content_type_is_bootable(header.content_type),
            content_size: header.content_size,
            version: format!("{:08X}", header.version),
            base_version: format!("{:08X}", header.base_version),
            save_game_id: format!("{:08X}", header.save_game_id),
            console_id: header.console_id,
            profile_id: header.profile_id,
            device_id: header.device_id,
            data_file_count: header.data_file_count,
            data_file_combined_size: header.data_file_combined_size,
            descriptor_type: match header.descriptor_type {
                0 => "STFS".to_string(),
                1 => "SVOD".to_string(),
                t => format!("Unknown({t})"),
            },
            metadata_version: header.metadata_version,
            transfer_flags: if header.transfer_flag_names.is_empty() {
                format!("{:02X}", header.transfer_flags)
            } else {
                format!(
                    "{:02X} ({})",
                    header.transfer_flags,
                    header.transfer_flag_names.join(", ")
                )
            },
            licenses: header
                .licenses
                .iter()
                .map(|l| format!("{} bits={:08X} flags={:08X}", l.id, l.bits, l.flags))
                .collect(),
            locale_titles: header.locale_titles,
            thumbnails: header.thumbnails,
            display_title: header.display_title,
            region_mask: header.region_mask.map(|m| format!("{:08X}", m)),
            regions_detailed: detailed,
            regions: collapsed,
            xex_exec: header.xex_exec,
            title_class: crate::title_id::classify(header.base_title_id).to_string(),
            directory_entries: entries,
            raw_strings,
            verify: None,
        })
    }

    fn extract_strings_and_entries(
        &mut self,
        entries: &mut Vec<StfsFileEntry>,
        raw_strings: &mut Vec<(u64, String)>,
    ) -> Result<()> {
        // Only the header + file-table region is ever needed here (table
        // slots, fixed strings). Never load the whole container: TUs/XBLA
        // images can be hundreds of MB while this region tops out at a few
        // hundred KB. Cap the table scan at 4096 slots as a sanity bound.
        // Table location comes from the volume descriptor (0xC000 observed).
        const MAX_SLOTS: usize = 4096;
        let table_start = StfsLayout::read(&mut self.reader, self.file_size)
            .map(|l| l.table_off as usize)
            .unwrap_or(0xC000);
        let scan_end = (table_start + MAX_SLOTS * 64).min(self.file_size as usize);
        let buffer = if scan_end > 0 {
            read_at(&mut self.reader, 0, scan_end)?
        } else {
            Vec::new()
        };

        if let Some(s) = extract_cstring(&buffer, table_start) {
            raw_strings.push((table_start as u64, s));
        }
        if let Some(s) = extract_cstring(&buffer, table_start + 0x40) {
            raw_strings.push((table_start as u64 + 0x40, s));
        }

        let region_strings = [
            "PAL(A) ",
            "PAL(E) ",
            "NTSC/J ",
            "NTSC/J(J) ",
            "NTSC/J(C) ",
            "NTSC/J(0) ",
            "NTSC/U ",
            "Region Free",
        ];
        for (i, _) in buffer.iter().enumerate() {
            for region in &region_strings {
                if i + region.len() <= buffer.len()
                    && &buffer[i..i + region.len()] == region.as_bytes()
                {
                    raw_strings.push((i as u64, region.to_string()));
                }
            }
        }

        let label_strings = ["Filename", "Patch Name", "MediaID"];
        for (i, _) in buffer.iter().enumerate() {
            for label in &label_strings {
                if i + label.len() <= buffer.len()
                    && &buffer[i..i + label.len()] == label.as_bytes()
                {
                    raw_strings.push((i as u64, label.to_string()));
                }
            }
        }

        // STFS file table: 64-byte entries starting at the table offset
        // from the volume descriptor (0xC000 in all observed files). The
        // table is densely packed: stop at the first all-zero slot. Slot
        // indices count every slot (path indicators refer to them).
        let dir_start = table_start;
        let entry_size = 64;
        let mut offset = dir_start;
        let mut index = 0usize;
        while offset + entry_size <= buffer.len() {
            let entry_data = &buffer[offset..offset + entry_size];
            if entry_data.iter().all(|&b| b == 0) {
                break;
            }
            if let Some(entry) = decode_slot(entry_data, index, offset as u64) {
                entries.push(entry);
            }
            index += 1;
            offset += entry_size;
        }
        Ok(())
    }
}

pub(crate) fn extract_cstring(data: &[u8], start: usize) -> Option<String> {
    if start >= data.len() {
        return None;
    }
    let slice = &data[start..];
    let end = slice.iter().position(|&b| b == 0).unwrap_or(slice.len());
    if end > 0 && end < 256 {
        String::from_utf8(slice[..end].to_vec()).ok()
    } else {
        None
    }
}

/// Format a byte count as "12345 (1.2 MB)".
pub fn format_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    if bytes < 1024 {
        return format!("{} B", bytes);
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{} ({:.1} {})", bytes, value, UNITS[unit])
}

pub(crate) fn looks_like_filename(filename: &str) -> bool {
    filename.contains('.')
        || filename.contains('\\')
        || filename.contains('/')
        || filename.ends_with(".xexp")
        || filename.ends_with(".wav")
        || filename.ends_with(".dds")
        || filename.starts_with("dlc")
        || filename.starts_with("gd_")
        || filename.starts_with("prop_")
        || filename.chars().filter(|c| c.is_alphanumeric()).count() > 3
}

/// Decode one 64-byte file-table slot. Accepts entries whose flag
/// name-length matches and that look like files, or are directories
/// (short dir names like `dcl` have no dots). Returns the raw record
/// with its table index (path indicators count every slot from 0).
pub(crate) fn decode_slot(data: &[u8], index: usize, offset: u64) -> Option<StfsFileEntry> {
    if data.len() < 64 {
        return None;
    }
    let filename = extract_cstring(data, 0)?;

    if filename.is_empty() || filename.len() > 128 {
        return None;
    }

    if !filename.chars().any(|c| c.is_alphanumeric()) {
        return None;
    }

    let flags = data[0x28];
    let is_dir = flags & 0x80 != 0;
    // Flag low 6 bits are the name length (verified on all samples).
    if (flags & 63) as usize != filename.len() {
        return None;
    }

    if !looks_like_filename(&filename) && !is_dir {
        return None;
    }

    let start = data[0x2F] as u64 | ((data[0x30] as u64) << 8) | ((data[0x31] as u64) << 16);
    // STFS file-table entry: file size is a big-endian u32 at entry+0x34.
    // Directories have size 0.
    let size = u32::from_be_bytes([data[0x34], data[0x35], data[0x36], data[0x37]]) as u64;
    let parent = u16::from_be_bytes([data[0x32], data[0x33]]) as u64;

    Some(StfsFileEntry {
        index,
        name: filename,
        flags,
        start_block: start,
        size,
        parent,
        is_dir,
        table_offset: offset,
    })
}
