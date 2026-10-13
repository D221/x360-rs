//! Raw XISO/GDF structures and directory-table walking.

use std::io::{Read, Seek, SeekFrom};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::xex::XexExecInfo;

/// Raw Xbox 360 ISO (GDF/GDFX) support, mirroring iso2god-rs `src/iso/`.
/// Everything is seek-based so multi-GB images never get loaded.
pub const SECTOR_SIZE: u64 = 0x800;

/// ISO data placement, in iso2god's detection order (XSF first, XGD3 last
/// as fallback). See iso2god-rs `src/iso/iso_type.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IsoType {
    Xsf,
    Xgd2,
    Xgd1,
    Xgd3,
}

impl IsoType {
    pub fn name(self) -> &'static str {
        match self {
            IsoType::Xsf => "XSF",
            IsoType::Xgd2 => "XGD2",
            IsoType::Xgd1 => "XGD1",
            IsoType::Xgd3 => "XGD3",
        }
    }

    pub fn root_offset(self) -> u64 {
        match self {
            IsoType::Xsf => 0,
            IsoType::Xgd2 => 0xFD90000,
            IsoType::Xgd1 => 0x18300000,
            IsoType::Xgd3 => 0x2080000,
        }
    }
}

/// GDF directory entry attribute flags (iso2god-rs `src/iso/directory_table.rs`).
pub const GDF_ATTR_DIRECTORY: u8 = 0x10;

/// GDF directory entry attributes (from xdvdfs/xdvdfs-core/src/layout/attributes.rs).
/// Bits: 0=read_only, 1=hidden, 2=system, 4=directory, 5=archive, 7=normal
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IsoEntryAttrs {
    pub read_only: bool,
    pub hidden: bool,
    pub system: bool,
    pub directory: bool,
    pub archive: bool,
    pub normal: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IsoEntry {
    pub path: String,
    pub sector: u32,
    pub size: u64,
    pub is_dir: bool,
    pub attrs: IsoEntryAttrs,
    pub left_child_offset: Option<u16>,
    pub right_child_offset: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IsoInfo {
    pub file_path: String,
    pub file_size: u64,
    pub iso_type: String,
    pub root_sector: u32,
    pub root_size: u32,
    pub volume_sectors: u64,
    /// Volume creation timestamp from the volume descriptor (u64, Windows FILETIME format)
    pub volume_timestamp: Option<u64>,
    pub xex_exec: Option<XexExecInfo>,
    pub xbe_info: Option<XbeInfo>,
    pub executable: Option<String>,
    pub entries: Vec<IsoEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XbeInfo {
    pub title_id: String,
    pub version: String,
}

/// Parse an original-Xbox XBE header: magic "XBEH", title ID LE u32 at +8,
/// version LE u32 at +176 (iso2god-rs `TitleExecutionInfo::from_xbe`).
pub fn parse_xbe_info(buf: &[u8]) -> Option<XbeInfo> {
    if buf.len() < 180 || &buf[0..4] != b"XBEH" {
        return None;
    }
    let le = |o: usize| u32::from_le_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]);
    Some(XbeInfo {
        title_id: format!("{:08X}", le(8)),
        version: format!("{:08X}", le(176)),
    })
}

/// Detect the ISO type by probing for `MICROSOFT*XBOX*MEDIA` at sector 0x20
/// plus each type's root offset. Returns the type and its root offset.
pub fn detect_iso_type(
    reader: &mut (impl Read + Seek),
    file_size: u64,
) -> Result<Option<(IsoType, u64)>> {
    for iso_type in [IsoType::Xsf, IsoType::Xgd2, IsoType::Xgd1, IsoType::Xgd3] {
        let at = 0x20 * SECTOR_SIZE + iso_type.root_offset();
        if at + 20 > file_size {
            continue;
        }
        reader.seek(SeekFrom::Start(at))?;
        let mut magic = [0u8; 20];
        reader.read_exact(&mut magic)?;
        if &magic == b"MICROSOFT*XBOX*MEDIA" {
            return Ok(Some((iso_type, iso_type.root_offset())));
        }
    }
    Ok(None)
}

pub(crate) fn read_u16_le(reader: &mut (impl Read + Seek)) -> Result<u16> {
    let mut b = [0u8; 2];
    reader.read_exact(&mut b)?;
    Ok(u16::from_le_bytes(b))
}

pub(crate) fn read_u32_le(reader: &mut (impl Read + Seek)) -> Result<u32> {
    let mut b = [0u8; 4];
    reader.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

pub(crate) fn read_u64_le(reader: &mut (impl Read + Seek)) -> Result<u64> {
    let mut b = [0u8; 8];
    reader.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

/// Walk one GDF directory table, flattening entries to `prefix`-joined paths.
/// Mirrors iso2god-rs `DirectoryTable::read` / `DirectoryEntry::read`.
/// Also captures attributes and tree structure (left/right child offsets).
pub(crate) fn walk_gdf_table(
    reader: &mut (impl Read + Seek),
    root_offset: u64,
    prefix: &str,
    sector: u32,
    size: u32,
    depth: usize,
    out: &mut Vec<IsoEntry>,
) -> Result<()> {
    if depth > 32 || out.len() > 200_000 {
        return Ok(());
    }
    let sector_count = size.div_ceil(SECTOR_SIZE as u32);
    for sector_index in 0..sector_count {
        let sector_pos = (sector + sector_index) as u64 * SECTOR_SIZE + root_offset;
        reader.seek(SeekFrom::Start(sector_pos))?;
        loop {
            let left = read_u16_le(reader)?;
            let right = read_u16_le(reader)?;
            if left == 0xFFFF || right == 0xFFFF {
                break;
            }
            let ent_sector = read_u32_le(reader)?;
            let ent_size = read_u32_le(reader)?;
            if ent_size == 0 {
                break;
            }
            let mut ab = [0u8; 1];
            reader.read_exact(&mut ab)?;
            let attrs_byte = ab[0];
            reader.read_exact(&mut ab)?;
            let name_len = ab[0] as usize;
            let mut name = vec![0u8; name_len];
            reader.read_exact(&mut name)?;
            let name = String::from_utf8_lossy(&name).into_owned();
            let pos = reader.stream_position()?;
            let pad = ((4 - pos % 4) % 4) as i64;
            reader.seek(SeekFrom::Current(pad))?;

            let is_dir = attrs_byte & GDF_ATTR_DIRECTORY != 0;
            let attrs = IsoEntryAttrs {
                read_only: attrs_byte & 0x01 != 0,
                hidden: attrs_byte & 0x02 != 0,
                system: attrs_byte & 0x04 != 0,
                directory: is_dir,
                archive: attrs_byte & 0x20 != 0,
                normal: attrs_byte & 0x80 != 0,
            };
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}\\{name}")
            };
            if is_dir {
                out.push(IsoEntry {
                    path: path.clone(),
                    sector: ent_sector,
                    size: ent_size as u64,
                    is_dir: true,
                    attrs,
                    left_child_offset: if left != 0xFFFF { Some(left) } else { None },
                    right_child_offset: if right != 0xFFFF { Some(right) } else { None },
                });
                let resume = reader.stream_position()?;
                walk_gdf_table(
                    reader,
                    root_offset,
                    &path,
                    ent_sector,
                    ent_size,
                    depth + 1,
                    out,
                )?;
                reader.seek(SeekFrom::Start(resume))?;
            } else {
                out.push(IsoEntry {
                    path,
                    sector: ent_sector,
                    size: ent_size as u64,
                    is_dir: false,
                    attrs,
                    left_child_offset: if left != 0xFFFF { Some(left) } else { None },
                    right_child_offset: if right != 0xFFFF { Some(right) } else { None },
                });
            }
        }
    }
    Ok(())
}
