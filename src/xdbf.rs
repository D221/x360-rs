//! Xbox DataBase File (XDBF) parsing: title metadata containers.
//!
//! Layout mirrors `Chilano.Xbox360.Xdbf` (Iso2God): big-endian header,
//! entry table at offset 30, then zero padding up to the data region.
//! An entry's bytes live at `data_start + offset`, length `size`.
//! There is no validated magic, so [`parse_xdbf`] only accepts structurally
//! consistent files (matching entry counts, in-bounds tables and data).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Trait combining Read + Seek for dyn dispatch
pub trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

/// One XDBF table entry. XEX-embedded form uses 32-bit `identifier` plus
/// `entry_type`; GPD form uses a 64-bit ID with `namespace` set
/// (`1=Achievement, 2=Image, 3=Setting, 4=Title, 5=String,
/// 6=AvatarAward`). Data lives at `data_start + offset`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XdbfEntry {
    pub identifier: u64,
    pub namespace: Option<u16>,
    pub offset: u32,
    pub size: u32,
    pub entry_type: u16,
}

/// Parsed XDBF file. Two on-disk variants exist (see the Free60 XDBF doc
/// in `../../wiki/docs/System-Software/Formats/XDBF.md`): the XEX-embedded
/// form (table at offset 30) and the GPD/SPA form (capacity-sized tables,
/// version `0x10000`, data after both tables).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XdbfInfo {
    pub file_path: String,
    pub file_size: u64,
    pub magic: [u8; 4],
    pub version: u32,
    pub variant: String,
    pub num_entries: u32,
    pub data_start: u64,
    pub entries: Vec<XdbfEntry>,
    /// Title from resource (1,3), fallback (1,0): length-prefixed UTF-8 at +17.
    /// XEX-embedded variant only; always `None` for GPD files.
    pub title_name: Option<String>,
}

fn read_u16_be(r: &mut dyn ReadSeek) -> Result<u16> {
    let mut b = [0u8; 2];
    r.read_exact(&mut b)?;
    Ok(u16::from_be_bytes(b))
}

fn read_u32_be(r: &mut dyn ReadSeek) -> Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_be_bytes(b))
}

fn read_u64_be(r: &mut dyn ReadSeek) -> Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_be_bytes(b))
}

/// GPD namespace names (Free60 GPD doc).
pub fn namespace_name(namespace: u16) -> &'static str {
    match namespace {
        1 => "Achievement",
        2 => "Image",
        3 => "Setting",
        4 => "Title",
        5 => "String",
        6 => "AvatarAward",
        _ => "Unknown",
    }
}

/// Human-readable resource name. XEX-embedded form: `FOURCC/Type` plus
/// `Thumb+` for `0x8000`. GPD form: `Namespace/ID` with a 16-digit ID.
pub fn resource_name(identifier: u64, entry_type: u16, namespace: Option<u16>) -> String {
    if let Some(ns) = namespace {
        return format!("{}/{:016X}", namespace_name(ns), identifier);
    }
    let id32 = identifier as u32;
    let base = id32 & !0x8000;
    let bytes = base.to_be_bytes();
    let fourcc = if bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        String::from_utf8_lossy(&bytes).trim().to_string()
    } else {
        format!("{base:08X}")
    };
    let thumb = if id32 & 0x8000 != 0 { "Thumb+" } else { "" };
    let ty = match entry_type {
        1 => "Standard".to_string(),
        2 => "Achievement".to_string(),
        3 => "TitleInfo".to_string(),
        t => format!("type{t}"),
    };
    format!("{thumb}{fourcc}/{ty}")
}

/// Validate entry data bounds against the file size.
fn check_entry_bounds(entries: &[XdbfEntry], data_start: u64, file_size: u64) -> Result<()> {
    for e in entries {
        let end = data_start
            .checked_add(e.offset as u64)
            .and_then(|o| o.checked_add(e.size as u64))
            .ok_or_else(|| anyhow::anyhow!("XDBF entry address overflow"))?;
        if end > file_size {
            anyhow::bail!("XDBF entry data overruns file");
        }
    }
    Ok(())
}

/// Parse an XDBF file with strict structural validation.
///
/// Dispatch: a u32 version of `0x10000` is ambiguous — it means GPD/SPA, but
/// also reads that way for XEX-embedded files with version 1 / reserved 0.
/// GPD capacities are 512-entry multiples while XEX-embedded counts are
/// small, so a small equal count pair (`A == B < 512`) tries the XEX form
/// first and falls back to GPD; anything else goes straight to GPD.
pub fn parse_xdbf(path: &PathBuf, file_size: u64) -> Result<XdbfInfo> {
    let file = File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let mut reader = std::io::BufReader::new(file);
    if file_size < 30 {
        anyhow::bail!("too small for XDBF ({} bytes)", file_size);
    }
    let mut magic = [0u8; 4];
    reader.read_exact(&mut magic)?;
    let version = read_u32_be(&mut reader)?;
    if version != 0x10000 {
        return parse_xex_embedded(&mut reader, path, file_size, magic);
    }
    let (a, b) = peek_counts(&mut reader)?;
    if a == b && a > 0 && a < 512 {
        if let Ok(info) = parse_xex_embedded(&mut reader, path, file_size, magic) {
            return Ok(info);
        }
    }
    parse_gpd(&mut reader, path, file_size, magic)
}

/// Peek the two u32 fields at offsets 8 and 12 without consuming input.
fn peek_counts(reader: &mut (impl Read + Seek)) -> Result<(u32, u32)> {
    reader.seek(SeekFrom::Start(8))?;
    let a = read_u32_be(reader)?;
    let b = read_u32_be(reader)?;
    Ok((a, b))
}

/// XEX-embedded form: 16-bit version/reserved, matching entry counts,
/// table at offset 30, 18-byte entries.
fn parse_xex_embedded(
    reader: &mut dyn ReadSeek,
    path: &Path,
    file_size: u64,
    magic: [u8; 4],
) -> Result<XdbfInfo> {
    reader.seek(SeekFrom::Start(4))?;
    let version = read_u16_be(&mut *reader)?;
    let _reserved = read_u16_be(&mut *reader)?;
    let num_entries = read_u32_be(&mut *reader)?;
    let num_copy = read_u32_be(&mut *reader)?;
    let _unknown_a = read_u32_be(&mut *reader)?;
    let _unknown_b = read_u32_be(&mut *reader)?;
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
            identifier: read_u32_be(&mut *reader)? as u64,
            namespace: None,
            offset: read_u32_be(&mut *reader)?,
            size: read_u32_be(&mut *reader)?,
            entry_type: read_u16_be(&mut *reader)?,
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
    check_entry_bounds(&entries, data_start, file_size)?;
    // Title name: resource (1,3), fallback (1,0); length-prefixed UTF-8 at +17.
    let mut title_name = None;
    for (id, ty) in [(1u64, 3u16), (1, 0)] {
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
        version: version as u32,
        variant: "xex".to_string(),
        num_entries,
        data_start,
        entries,
        title_name,
    })
}

/// GPD/SPA form: capacity-sized entry table plus free-space table, data
/// after both. Only the first `entry_count` entries are used.
fn parse_gpd(
    mut reader: &mut dyn ReadSeek,
    path: &Path,
    file_size: u64,
    magic: [u8; 4],
) -> Result<XdbfInfo> {
    reader.seek(SeekFrom::Start(8))?;
    let table_len = read_u32_be(&mut reader)?;
    let entry_count = read_u32_be(&mut reader)?;
    let free_len = read_u32_be(&mut reader)?;
    let _free_count = read_u32_be(&mut reader)?;
    if entry_count == 0 || entry_count > 100_000 {
        anyhow::bail!("implausible GPD entry count {entry_count}");
    }
    let table_bytes = table_len as u64 * 18;
    let free_bytes = free_len as u64 * 8;
    let data_start = 24u64
        .checked_add(table_bytes)
        .and_then(|o| o.checked_add(free_bytes))
        .ok_or_else(|| anyhow::anyhow!("GPD table address overflow"))?;
    if data_start > file_size {
        anyhow::bail!("GPD tables overrun file");
    }
    if entry_count as u64 * 18 > table_bytes {
        anyhow::bail!("GPD entry count exceeds table length");
    }
    let mut entries = Vec::with_capacity(entry_count as usize);
    reader.seek(SeekFrom::Start(24))?;
    for _ in 0..entry_count {
        let namespace = read_u16_be(&mut *reader)?;
        let identifier = read_u64_be(&mut *reader)?;
        entries.push(XdbfEntry {
            identifier,
            namespace: Some(namespace),
            offset: read_u32_be(&mut *reader)?,
            size: read_u32_be(&mut *reader)?,
            entry_type: 0,
        });
    }
    check_entry_bounds(&entries, data_start, file_size)?;
    Ok(XdbfInfo {
        file_path: path.display().to_string(),
        file_size,
        magic,
        version: 0x10000,
        variant: "gpd".to_string(),
        num_entries: entry_count,
        data_start,
        entries,
        title_name: None,
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
        variant: info.variant.clone(),
        num_entries: info.num_entries,
        title_name: info.title_name.clone(),
        records: Vec::new(),
        entries: info
            .entries
            .iter()
            .map(|e| crate::report::XdbfEntryInfo {
                identifier: format!("{:08X}", e.identifier),
                namespace: e.namespace.map(namespace_name).map(str::to_string),
                resource: resource_name(e.identifier, e.entry_type, e.namespace),
                offset: info.data_start + e.offset as u64,
                size: e.size as u64,
                entry_type: e.entry_type,
            })
            .collect(),
    }
}
