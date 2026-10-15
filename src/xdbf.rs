//! Xbox DataBase File (XDBF) parsing: title metadata containers.
//!
//! Layout mirrors `Chilano.Xbox360.Xdbf` (Iso2God): big-endian header,
//! entry table at offset 30, then zero padding up to the data region.
//! An entry's bytes live at `data_start + offset`, length `size`.
//! There is no validated magic, so [`parse_xdbf`] only accepts structurally
//! consistent files (matching entry counts, in-bounds tables and data).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// One XDBF table entry: `identifier` (often a big-endian fourcc, with
/// `0x8000` marking thumbnails), data at `data_start + offset`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XdbfEntry {
    pub identifier: u32,
    pub offset: u32,
    pub size: u32,
    pub entry_type: u16,
}

/// Parsed XDBF file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XdbfInfo {
    pub file_path: String,
    pub file_size: u64,
    pub magic: [u8; 4],
    pub version: u16,
    pub num_entries: u32,
    pub data_start: u64,
    pub entries: Vec<XdbfEntry>,
    /// Title from resource (1,3), fallback (1,0): length-prefixed UTF-8 at +17.
    pub title_name: Option<String>,
}

fn read_u16_be(r: &mut (impl Read + Seek)) -> Result<u16> {
    let mut b = [0u8; 2];
    r.read_exact(&mut b)?;
    Ok(u16::from_be_bytes(b))
}

fn read_u32_be(r: &mut (impl Read + Seek)) -> Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_be_bytes(b))
}

/// Human-readable resource name: `FOURCC/Type` plus `Thumb+` for `0x8000`.
pub fn resource_name(identifier: u32, entry_type: u16) -> String {
    let base = identifier & !0x8000;
    let bytes = base.to_be_bytes();
    let fourcc = if bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        String::from_utf8_lossy(&bytes).trim().to_string()
    } else {
        format!("{base:08X}")
    };
    let thumb = if identifier & 0x8000 != 0 {
        "Thumb+"
    } else {
        ""
    };
    let ty = match entry_type {
        1 => "Standard".to_string(),
        2 => "Achievement".to_string(),
        3 => "TitleInfo".to_string(),
        t => format!("type{t}"),
    };
    format!("{thumb}{fourcc}/{ty}")
}

/// Parse an XDBF file with strict structural validation.
pub fn parse_xdbf(path: &PathBuf, file_size: u64) -> Result<XdbfInfo> {
    let file = File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let mut reader = std::io::BufReader::new(file);
    if file_size < 30 {
        anyhow::bail!("too small for XDBF ({} bytes)", file_size);
    }
    let mut magic = [0u8; 4];
    reader.read_exact(&mut magic)?;
    let version = read_u16_be(&mut reader)?;
    let _reserved = read_u16_be(&mut reader)?;
    let num_entries = read_u32_be(&mut reader)?;
    let num_copy = read_u32_be(&mut reader)?;
    let _unknown_a = read_u32_be(&mut reader)?;
    let _unknown_b = read_u32_be(&mut reader)?;
    if num_entries == 0 || num_entries != num_copy {
        anyhow::bail!("not an XDBF table (entries {num_entries}/{num_copy})");
    }
    if num_entries > 100_000 {
        anyhow::bail!("implausible XDBF entry count {num_entries}");
    }
    let table_end = 30u64 + num_entries as u64 * 18;
    if table_end > file_size {
        anyhow::bail!("XDBF table overruns file");
    }
    // Entries first (table at offset 30), then skip zero padding to data.
    let mut entries = Vec::with_capacity(num_entries as usize);
    reader.seek(SeekFrom::Start(30))?;
    for _ in 0..num_entries {
        entries.push(XdbfEntry {
            identifier: read_u32_be(&mut reader)?,
            offset: read_u32_be(&mut reader)?,
            size: read_u32_be(&mut reader)?,
            entry_type: read_u16_be(&mut reader)?,
        });
        let mut pad = [0u8; 4];
        reader.read_exact(&mut pad)?;
    }
    let mut data_start = table_end;
    loop {
        if data_start >= file_size {
            break;
        }
        reader.seek(SeekFrom::Start(data_start))?;
        let mut b = [0u8; 1];
        reader.read_exact(&mut b)?;
        if b[0] != 0 {
            break;
        }
        data_start += 1;
    }
    for e in &entries {
        let end = data_start
            .checked_add(e.offset as u64)
            .and_then(|o| o.checked_add(e.size as u64))
            .ok_or_else(|| anyhow::anyhow!("XDBF entry address overflow"))?;
        if end > file_size {
            anyhow::bail!("XDBF entry data overruns file");
        }
    }
    // Title name: resource (1,3), fallback (1,0); length-prefixed UTF-8 at +17.
    let mut title_name = None;
    for (id, ty) in [(1u32, 3u16), (1, 0)] {
        if let Some(e) = entries
            .iter()
            .find(|e| e.identifier == id && e.entry_type == ty)
        {
            if e.size as u64 > 18 {
                reader.seek(SeekFrom::Start(data_start + e.offset as u64 + 17))?;
                let mut len = [0u8; 1];
                reader.read_exact(&mut len)?;
                let n = (len[0] as u64).min(e.size as u64 - 18);
                let mut buf = vec![0u8; n as usize];
                reader.read_exact(&mut buf)?;
                title_name = Some(String::from_utf8_lossy(&buf).to_string());
                break;
            }
        }
    }
    Ok(XdbfInfo {
        file_path: path.display().to_string(),
        file_size,
        magic,
        version,
        num_entries,
        data_start,
        entries,
        title_name,
    })
}

/// Convert a parsed XDBF into its report form.
pub fn report_info(info: &XdbfInfo) -> crate::report::XdbfFileInfo {
    crate::report::XdbfFileInfo {
        file_path: info.file_path.clone(),
        file_size: info.file_size,
        magic: format!(
            "{:02X}{:02X}{:02X}{:02X}",
            info.magic[0], info.magic[1], info.magic[2], info.magic[3]
        ),
        version: info.version,
        num_entries: info.num_entries,
        title_name: info.title_name.clone(),
        entries: info
            .entries
            .iter()
            .map(|e| crate::report::XdbfEntryInfo {
                identifier: format!("{:08X}", e.identifier),
                resource: resource_name(e.identifier, e.entry_type),
                offset: info.data_start + e.offset as u64,
                size: e.size as u64,
                entry_type: e.entry_type,
            })
            .collect(),
    }
}
