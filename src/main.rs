use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use byteorder::{BigEndian, LittleEndian, ReadBytesExt};
use clap::Parser;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum TuError {
    #[error("Not a valid STFS container: expected 'LIVE', 'CON ' or 'PIRS' magic, got {0:08X}")]
    InvalidMagic(u32),
    #[error("Unrecognized file format (not STFS, XEX, XBE or XISO)")]
    UnknownFormat,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Accepted STFS container magics (LE u32): "LIVE", "CON ", "PIRS".
/// Same header/table layout regardless of magic.
pub const MAGIC_LIVE: u32 = 0x4556494C;
pub const MAGIC_CON: u32 = 0x204E4F43;
pub const MAGIC_PIRS: u32 = 0x53524950;

/// Display name for a container magic.
pub fn magic_name(magic: u32) -> &'static str {
    match magic {
        MAGIC_LIVE => "LIVE",
        MAGIC_CON => "CON",
        MAGIC_PIRS => "PIRS",
        _ => "Unknown",
    }
}

/// STFS content types (BE u32 at 0x0344).
pub const CONTENT_TITLE_UPDATE: u32 = 0x000B0000;
pub const CONTENT_MARKETPLACE: u32 = 0x00000002;
pub const CONTENT_XBLA: u32 = 0x000D0000;
pub const CONTENT_AVATAR: u32 = 0x00009000;
pub const CONTENT_THEME: u32 = 0x00030000;
pub const CONTENT_GOD: u32 = 0x00007000;

/// Human-readable content-type name.
pub fn content_type_name(content_type: u32) -> &'static str {
    match content_type {
        CONTENT_TITLE_UPDATE => "Title Update",
        CONTENT_MARKETPLACE => "Marketplace Content (DLC)",
        CONTENT_XBLA => "Xbox Live Arcade (XBLA)",
        CONTENT_AVATAR => "Avatar Item",
        CONTENT_THEME => "Theme",
        CONTENT_GOD => "Games on Demand (GoD)",
        _ => "Unknown",
    }
}

/// Execution info from a full game XEX header (ExecutionId field
/// 0x00040006), per iso2god-rs `src/executable/xex.rs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XexExecInfo {
    pub media_id: String,
    pub version: String,
    pub base_version: String,
    pub title_id: String,
    pub platform: u8,
    pub executable_type: u8,
    pub disc_number: u8,
    pub disc_count: u8,
}

/// Parse the XEX header at `xex_offset` in `buf`, returning the ExecutionId
/// info when the header carries field 0x00040006. TU patch XEXs only have
/// 2 fields and no ExecutionId; full game XEXs (XBLA etc.) do.
pub fn parse_xex_exec_info(buf: &[u8], xex_offset: usize) -> Option<XexExecInfo> {
    let need = xex_offset.checked_add(24)?;
    if need > buf.len() || &buf[xex_offset..xex_offset + 4] != b"XEX2" {
        return None;
    }
    let field_count = u32::from_be_bytes([
        buf[xex_offset + 20],
        buf[xex_offset + 21],
        buf[xex_offset + 22],
        buf[xex_offset + 23],
    ]) as usize;
    if field_count > 256 {
        return None;
    }
    for i in 0..field_count {
        let base = xex_offset.checked_add(24 + i * 8)?;
        if base + 8 > buf.len() {
            return None;
        }
        let key = u32::from_be_bytes([buf[base], buf[base + 1], buf[base + 2], buf[base + 3]]);
        let val = u32::from_be_bytes([buf[base + 4], buf[base + 5], buf[base + 6], buf[base + 7]]);
        if key == 0x00040006 {
            let e = xex_offset.checked_add(val as usize)?;
            if e + 20 > buf.len() {
                return None;
            }
            let be = |o: usize| {
                u32::from_be_bytes([buf[e + o], buf[e + o + 1], buf[e + o + 2], buf[e + o + 3]])
            };
            return Some(XexExecInfo {
                media_id: format!("{:08X}", be(0)),
                version: format!("{:08X}", be(4)),
                base_version: format!("{:08X}", be(8)),
                title_id: format!("{:08X}", be(12)),
                platform: buf[e + 16],
                executable_type: buf[e + 17],
                disc_number: buf[e + 18],
                disc_count: buf[e + 19],
            });
        }
    }
    None
}

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

fn read_u16_le(reader: &mut (impl Read + Seek)) -> Result<u16> {
    let mut b = [0u8; 2];
    reader.read_exact(&mut b)?;
    Ok(u16::from_le_bytes(b))
}

fn read_u32_le(reader: &mut (impl Read + Seek)) -> Result<u32> {
    let mut b = [0u8; 4];
    reader.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn read_u64_le(reader: &mut (impl Read + Seek)) -> Result<u64> {
    let mut b = [0u8; 8];
    reader.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

/// Walk one GDF directory table, flattening entries to `prefix`-joined paths.
/// Mirrors iso2god-rs `DirectoryTable::read` / `DirectoryEntry::read`.
/// Also captures attributes and tree structure (left/right child offsets).
fn walk_gdf_table(
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

// Region bitmask mapping, confirmed via REA/Ghidra against
// Title Update Checker_unpacked.exe (evidence ev_15f0a71c..., ev_a5b10f23...,
// read_bytes at 0x4032e0-0x4036c0):
//   0x4032FE: AND ECX,0x00010000 / CMP -> "PAL(A) "
//   0x403383: AND ECX,0x00FE0000 / CMP -> "PAL(E) "
//   0x403408: AND ECX,0x0000FF00 / CMP -> "NTSC/J "
//   0x403492: AND ECX,0x00000100 / CMP -> "NTSC/J(J) "
//   0x403517: AND ECX,0x00000200 / CMP -> "NTSC/J(C) "
//   0x40359C: AND ECX,0x0000FC00 / CMP -> "NTSC/J(0) "
//   0x403621: AND ECX,0x000000FF / CMP -> "NTSC/U "
//   0x4036A6: AND ECX,0xFFFFFFFF / CMP -> "Region Free" (imm8 0xFF sign-extended)
const REGION_CHECKS: &[(u32, &str)] = &[
    (0x00010000, "PAL(A)"),
    (0x00FE0000, "PAL(E)"),
    (0x0000FF00, "NTSC/J"),
    (0x00000100, "NTSC/J(J)"),
    (0x00000200, "NTSC/J(C)"),
    (0x0000FC00, "NTSC/J(0)"),
    (0x000000FF, "NTSC/U"),
];

/// Decode a region mask the same way the checker does:
/// every (mask & check) == check appends its label;
/// a full 0xFFFFFFFF mask yields just "Region Free".
pub fn decode_regions(mask: u32) -> Vec<String> {
    if mask == 0xFFFFFFFF {
        return vec!["Region Free".to_string()];
    }
    let mut out = Vec::new();
    for (bits, name) in REGION_CHECKS {
        if (mask & bits) == *bits {
            out.push(name.to_string());
        }
    }
    out
}

/// Collapse detailed PAL sub-regions to "PAL" to match Title_Update_List.txt style.
pub fn collapse_regions(detailed: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut pal = false;
    for r in detailed {
        if r == "PAL(A)" || r == "PAL(E)" {
            if !pal {
                out.push("PAL".to_string());
                pal = true;
            }
        } else {
            out.push(r.clone());
        }
    }
    out
}

/// Read exactly `len` bytes at `offset`. Short reads (EOF) are an error.
fn read_at(reader: &mut (impl Read + Seek), offset: u64, len: usize) -> Result<Vec<u8>> {
    reader.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    Ok(buf)
}

/// Locate the embedded `XEX2` magic with a chunked streaming search so
/// multi-hundred-MB containers never get loaded into memory.
fn find_xex_offset(reader: &mut (impl Read + Seek), file_size: u64) -> Result<Option<u64>> {
    const CHUNK: usize = 1024 * 1024;
    const OVERLAP: usize = 3; // len("XEX2") - 1, catches split magics
    let mut buf = vec![0u8; CHUNK + OVERLAP];
    let mut base: u64 = 0; // file offset of buf[0]
    let mut valid: usize = 0; // carry-over bytes at buf start
    loop {
        let pos = base + valid as u64;
        if pos >= file_size {
            break;
        }
        let want = ((file_size - pos).min(CHUNK as u64)) as usize;
        reader.seek(SeekFrom::Start(pos))?;
        reader.read_exact(&mut buf[valid..valid + want])?;
        let total = valid + want;
        if let Some(hit) = buf[..total].windows(4).position(|w| w == b"XEX2") {
            return Ok(Some(base + hit as u64));
        }
        if want < CHUNK {
            break;
        }
        buf.copy_within(total - OVERLAP..total, 0);
        base += (total - OVERLAP) as u64;
        valid = OVERLAP;
    }
    Ok(None)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuHeader {
    pub magic: u32,                    // 0x0000: "LIVE" (LE u32 0x4556494C)
    pub tu_signature: Option<u32>, // 0x971A: "TUPD" (LE u32 0x44505554), present in title updates
    pub content_type: u32,         // 0x0344: BE u32 (per iso2god con_header.rs)
    pub media_id: u32,             // 0x0354: BE u32, media ID (0 for DLC)
    pub base_title_id: u32, // 0x0360: BE u32, base game title ID (per iso2god with_execution_info)
    pub display_title: String, // 0x0411: UTF-16BE string (per iso2god with_game_title)
    pub xex_offset: Option<u64>, // file offset of embedded "XEX2", if any
    pub region_mask: Option<u32>, // BE u32 at xex_offset + 0x1A0 (TU patch XEX only)
    pub xex_exec: Option<XexExecInfo>, // ExecutionId from full game XEX (XBLA etc.)
}

impl TuHeader {
    pub fn read(mut reader: &mut (impl Read + Seek), file_size: u64) -> Result<Self> {
        reader.seek(SeekFrom::Start(0))?;
        let magic = reader.read_u32::<LittleEndian>()?;
        if magic != MAGIC_LIVE && magic != MAGIC_CON && magic != MAGIC_PIRS {
            return Err(TuError::InvalidMagic(magic).into());
        }

        // TUPD signature only exists in title updates (and only if the
        // file is large enough to contain offset 0x971A).
        let tu_signature = if file_size >= 0x971E {
            reader.seek(SeekFrom::Start(0x971A))?;
            let sig = reader.read_u32::<LittleEndian>()?;
            if sig == 0x44505554 {
                Some(sig)
            } else {
                None
            }
        } else {
            None
        };

        reader.seek(SeekFrom::Start(0x0344))?;
        let content_type = reader.read_u32::<BigEndian>()?;

        reader.seek(SeekFrom::Start(0x0354))?;
        let media_id = reader.read_u32::<BigEndian>()?;

        reader.seek(SeekFrom::Start(0x0360))?;
        let base_title_id = reader.read_u32::<BigEndian>()?;

        // Display title: UTF-16BE at 0x0411 (iso2god con_header.rs with_game_title)
        reader.seek(SeekFrom::Start(0x0411))?;
        let mut title_units = Vec::new();
        for _ in 0..128 {
            let c = reader.read_u16::<BigEndian>()?;
            if c == 0 {
                break;
            }
            title_units.push(c);
        }
        let display_title = String::from_utf16_lossy(&title_units);

        // Locate embedded XEX2 with a chunked streaming search (never load
        // the whole file: TUs can be hundreds of MB). Only TU *patch* XEXs
        // carry the region mask at xex_offset + 0x1A0 — full game XEXs
        // (XBLA etc.) have a real header there instead, so only read the
        // mask for title updates. Full game XEXs do carry ExecutionId
        // (field 0x00040006) with media/version/title IDs
        // (iso2god-rs src/executable/xex.rs).
        let xex_offset = find_xex_offset(&mut reader, file_size)?;
        let is_tu = content_type == CONTENT_TITLE_UPDATE;
        let region_mask = if is_tu {
            xex_offset
                .and_then(|x| read_at(&mut reader, x + 0x1A0, 4).ok())
                .and_then(|b| {
                    <[u8; 4]>::try_from(b.as_slice())
                        .map(u32::from_be_bytes)
                        .ok()
                })
        } else {
            None
        };
        // The ExecutionId struct always sits near the header start; a 64 KiB
        // window is plenty (observed < 10 KiB) and keeps big files fast.
        let xex_exec = xex_offset.and_then(|x| {
            read_at(&mut reader, x, 0x10000)
                .ok()
                .and_then(|win| parse_xex_exec_info(&win, 0))
        });

        Ok(TuHeader {
            magic,
            tu_signature,
            content_type,
            media_id,
            base_title_id,
            display_title,
            xex_offset,
            region_mask,
            xex_exec,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryEntry {
    pub filename: String,
    pub path: String,
    pub offset: u64,
    pub size: u64,
    pub is_dir: bool,
    pub start_block: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuInfo {
    pub file_path: String,
    pub file_size: u64,
    pub is_live: bool,
    pub is_title_update: bool,
    pub container_magic: String,
    pub content_kind: String,
    pub media_id: String,
    pub base_title_id: String,
    pub content_type: String,
    pub display_title: String,
    pub region_mask: Option<String>,
    pub regions_detailed: Vec<String>,
    pub regions: Vec<String>,
    pub xex_exec: Option<XexExecInfo>,
    pub directory_entries: Vec<DirectoryEntry>,
    pub raw_strings: Vec<(u64, String)>,
    pub verify: Option<VerifyReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XexFileInfo {
    pub file_path: String,
    pub file_size: u64,
    pub exec: XexExecInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XbeFileInfo {
    pub file_path: String,
    pub file_size: u64,
    pub info: XbeInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameDirInfo {
    pub dir_path: String,
    pub files: u64,
    pub dirs: u64,
    pub total_bytes: u64,
    pub executable: Option<String>,
    pub xex_exec: Option<XexExecInfo>,
    pub xbe_info: Option<XbeInfo>,
}

/// Top-level report. Internally tagged, so JSON gains a `"type"` field:
/// `stfs`, `xiso`, `xex`, `xbe` or `gamedir`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Report {
    Stfs(TuInfo),
    Xiso(IsoInfo),
    Xex(XexFileInfo),
    Xbe(XbeFileInfo),
    GameDir(GameDirInfo),
}

/// Dispatch on magic bytes: STFS (`LIVE`/`CON `/`PIRS`), standalone
/// `XEX2`/`XBEH` executables, else an XISO probe (GDF magic at the
/// per-type data offset).
pub fn parse_file(path: &PathBuf) -> Result<Report> {
    let file = File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let file_size = file.metadata()?.len();
    let mut reader = BufReader::new(file);
    let mut magic = [0u8; 4];
    reader.read_exact(&mut magic).context("File too small")?;
    let magic_le = u32::from_le_bytes(magic);

    if magic_le == MAGIC_LIVE || magic_le == MAGIC_CON || magic_le == MAGIC_PIRS {
        drop(reader);
        let mut parser = TuParser::open(path)?;
        let mut info = parser.parse()?;
        info.file_path = path.display().to_string();
        return Ok(Report::Stfs(info));
    }
    if &magic == b"XEX2" {
        drop(reader);
        let mut file = File::open(path)?;
        let mut reader = BufReader::new(&mut file);
        let win = read_window(&mut reader, 0, 0x40000, file_size)?;
        let exec = parse_xex_exec_info(&win, 0).context("XEX without ExecutionId")?;
        return Ok(Report::Xex(XexFileInfo {
            file_path: path.display().to_string(),
            file_size,
            exec,
        }));
    }
    if &magic == b"XBEH" {
        drop(reader);
        let mut file = File::open(path)?;
        let mut reader = BufReader::new(&mut file);
        let win = read_window(&mut reader, 0, 0x400, file_size)?;
        let info = parse_xbe_info(&win).context("Unrecognized XBE header")?;
        return Ok(Report::Xbe(XbeFileInfo {
            file_path: path.display().to_string(),
            file_size,
            info,
        }));
    }
    if detect_iso_type(&mut reader, file_size)?.is_some() {
        drop(reader);
        return Ok(Report::Xiso(parse_iso(path, file_size)?));
    }
    Err(TuError::UnknownFormat.into())
}

/// Read up to `len` bytes at `offset` (clamped to EOF; short final reads OK).
fn read_window(
    reader: &mut (impl Read + Seek),
    offset: u64,
    len: usize,
    file_size: u64,
) -> Result<Vec<u8>> {
    if offset >= file_size {
        return Ok(Vec::new());
    }
    let len = len.min((file_size - offset) as usize);
    read_at(reader, offset, len)
}

/// Parse a raw XISO/GDF image: detect type, read the volume descriptor,
/// walk the directory tables, then resolve `default.xex`/`default.xbe`.
pub fn parse_iso(path: &PathBuf, file_size: u64) -> Result<IsoInfo> {
    let file = File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let (iso_type, root_offset) =
        detect_iso_type(&mut reader, file_size)?.ok_or(TuError::UnknownFormat)?;

    let desc_at = 0x20 * SECTOR_SIZE + root_offset;
    reader.seek(SeekFrom::Start(desc_at))?;
    let mut ident = [0u8; 20];
    reader.read_exact(&mut ident)?;
    let root_sector = read_u32_le(&mut reader)?;
    let root_size = read_u32_le(&mut reader)?;

    // Volume timestamp is at offset 0x1C from descriptor start (after magic0 + root_table)
    // Volume descriptor layout: magic0(0x14) + root_table(8) + filetime(8) + ...
    reader.seek(SeekFrom::Start(desc_at + 0x1C))?;
    let volume_timestamp = read_u64_le(&mut reader).ok();

    let volume_sectors = file_size.saturating_sub(root_offset) / SECTOR_SIZE;

    let mut entries = Vec::new();
    walk_gdf_table(
        &mut reader,
        root_offset,
        "",
        root_sector,
        root_size,
        0,
        &mut entries,
    )?;

    // Prefer a top-level executable, fall back to any nested match.
    let pick = |names: &[&str]| -> Option<IsoEntry> {
        entries
            .iter()
            .find(|e| {
                !e.is_dir
                    && !e.path.contains('\\')
                    && names.iter().any(|n| e.path.eq_ignore_ascii_case(n))
            })
            .or_else(|| {
                entries.iter().find(|e| {
                    !e.is_dir
                        && names.iter().any(|n| {
                            e.path
                                .rsplit('\\')
                                .next()
                                .is_some_and(|f| f.eq_ignore_ascii_case(n))
                        })
                })
            })
            .cloned()
    };
    let mut executable = None;
    let mut xex_exec = None;
    let mut xbe_info = None;
    if let Some(e) = pick(&["default.xex"]) {
        executable = Some(e.path.clone());
        let at = root_offset + e.sector as u64 * SECTOR_SIZE;
        if let Ok(win) = read_window(&mut reader, at, 0x40000, file_size) {
            xex_exec = parse_xex_exec_info(&win, 0);
        }
    } else if let Some(e) = pick(&["default.xbe"]) {
        executable = Some(e.path.clone());
        let at = root_offset + e.sector as u64 * SECTOR_SIZE;
        if let Ok(win) = read_window(&mut reader, at, 0x400, file_size) {
            xbe_info = parse_xbe_info(&win);
        }
    }

    Ok(IsoInfo {
        file_path: path.display().to_string(),
        file_size,
        iso_type: iso_type.name().to_string(),
        root_sector,
        root_size,
        volume_sectors,
        volume_timestamp,
        xex_exec,
        xbe_info,
        executable,
        entries,
    })
}

// ---------------------------------------------------------------------------
// XISO Extraction — multiple strategies for benchmarking
// ---------------------------------------------------------------------------

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;

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

/// Method 1: Sequential per-file reads (baseline)
/// Opens file, seeks to each file's sector, reads its sectors
fn extract_sequential(
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
fn extract_bulk_read(
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

/// Method 3: Memory map - mmap the entire ISO, slice directly
/// Zero-copy for reads, OS handles paging
#[cfg(unix)]
fn extract_mmap(
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
fn extract_mmap(
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
fn extract_mmap_with_progress<F>(
    iso_path: &std::path::Path,
    root_offset: u64,
    files: &[ExtractFile],
    out_dir: &std::path::Path,
    progress: F,
) -> Result<ExtractionResult>
where
    F: Fn(String) + Send + Sync + 'static,
{
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
fn extract_mmap_with_progress<F>(
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

/// Method 4: Parallel extraction - split files across threads
/// Uses a thread pool, each thread has its own file handle
fn extract_parallel(
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

/// Progress-enabled parallel extraction
fn extract_parallel_with_progress<F>(
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

/// Method 5: Large buffered sequential - bigger buffer, fewer syscalls
fn extract_large_buffer(
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

/// Run all extraction methods on an ISO and return the fastest
pub fn benchmark_extract_iso(
    iso_path: &std::path::Path,
    out_base: &std::path::Path,
) -> Result<Vec<ExtractionResult>> {
    // First parse to get file list
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

    // Create output directories for each method
    let methods = vec![
        ("sequential", 1),
        ("bulk_read", 1),
        ("mmap", 1),
        ("large_buffer", 1),
        ("parallel_2", 2),
        ("parallel_4", 4),
        ("parallel_8", 8),
    ];

    let mut results = Vec::new();
    for (method, threads) in methods {
        let out_dir = out_base.join(method);
        std::fs::remove_dir_all(&out_dir).ok();
        std::fs::create_dir_all(&out_dir)?;

        let result = match method {
            "sequential" => extract_sequential(iso_path, root_offset, &extract_files, &out_dir)?,
            "bulk_read" => extract_bulk_read(iso_path, root_offset, &extract_files, &out_dir)?,
            "mmap" => extract_mmap(iso_path, root_offset, &extract_files, &out_dir)?,
            "large_buffer" => {
                extract_large_buffer(iso_path, root_offset, &extract_files, &out_dir)?
            }
            _ if method.starts_with("parallel_") => {
                extract_parallel(iso_path, root_offset, &extract_files, &out_dir, threads)?
            }
            _ => continue,
        };
        println!(
            "  {}: {:.1} MB/s ({}ms, {} files, {} MB)",
            result.method,
            result.throughput_mb_s,
            result.duration_ms,
            result.files_extracted,
            result.bytes_extracted / 1_048_576
        );
        results.push(result);
    }

    // Sort by throughput
    results.sort_by(|a, b| b.throughput_mb_s.partial_cmp(&a.throughput_mb_s).unwrap());

    println!(
        "\n  Fastest: {} at {:.1} MB/s",
        results[0].method, results[0].throughput_mb_s
    );

    Ok(results)
}

/// Sanitize a string for use as a directory name.
fn sanitize_dir_name(name: &str) -> String {
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
pub fn extract_iso_fast(
    iso_path: &std::path::Path,
    out_dir: &std::path::Path,
) -> Result<ExtractionResult> {
    extract_iso_fast_with_progress(iso_path, out_dir, |msg| eprintln!("{}", msg), None)
}

/// Parse an extracted game directory: tree stats plus `default.xex` /
/// `default.xbe` execution info (top level preferred).
pub fn parse_game_dir(dir: &PathBuf) -> Result<GameDirInfo> {
    let mut files: u64 = 0;
    let mut dirs: u64 = 0;
    let mut total_bytes: u64 = 0;
    let mut top_exe: Option<PathBuf> = None;
    let mut any_exe: Option<PathBuf> = None;
    let mut stack = vec![dir.clone()];
    let mut visited: u64 = 0;
    while let Some(d) = stack.pop() {
        visited += 1;
        if visited > 2_000_000 {
            break;
        }
        let rd =
            std::fs::read_dir(&d).with_context(|| format!("Failed to list {}", d.display()))?;
        for entry in rd {
            let entry = entry?;
            let ft = entry.file_type()?;
            if ft.is_symlink() {
                continue;
            }
            let p = entry.path();
            if ft.is_dir() {
                dirs += 1;
                stack.push(p);
            } else if ft.is_file() {
                files += 1;
                total_bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
                let is_exe = p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                    n.eq_ignore_ascii_case("default.xex") || n.eq_ignore_ascii_case("default.xbe")
                });
                if is_exe {
                    if p.parent() == Some(dir.as_path()) {
                        top_exe = Some(p);
                    } else if any_exe.is_none() {
                        any_exe = Some(p);
                    }
                }
            }
        }
    }
    let exe_path = top_exe.or(any_exe);
    let executable = exe_path
        .as_ref()
        .and_then(|p| p.strip_prefix(dir).ok().map(|r| r.display().to_string()));
    let mut xex_exec = None;
    let mut xbe_info = None;
    if let Some(p) = &exe_path {
        let file = File::open(p)?;
        let size = file.metadata()?.len();
        let mut reader = BufReader::new(file);
        let mut magic = [0u8; 4];
        if reader.read_exact(&mut magic).is_ok() {
            if &magic == b"XEX2" {
                if let Ok(win) = read_window(&mut reader, 0, 0x40000, size) {
                    xex_exec = parse_xex_exec_info(&win, 0);
                }
            } else if &magic == b"XBEH" {
                if let Ok(win) = read_window(&mut reader, 0, 0x400, size) {
                    xbe_info = parse_xbe_info(&win);
                }
            }
        }
    }
    Ok(GameDirInfo {
        dir_path: dir.display().to_string(),
        files,
        dirs,
        total_bytes,
        executable,
        xex_exec,
        xbe_info,
    })
}

// ---------------------------------------------------------------------------
// STFS block layout, hashing and extraction.
//
// Verified empirically against 23 TU/DLC plus XBLA/avatar/theme samples
// (free60 STFS wiki for field semantics):
// - block size 0x1000; block-space base = ((HeaderSize@0x340 + 0xFFF) & 0xF000)
// - physical block 0 = level-0 hash table T0; data block N lives at
//   physical block N + 1 + (N / 170) + (1 if N >= 170 else 0), i.e. one
//   hash-table block every 170 data blocks plus the single level-1 table
//   at physical block 171 (hashes of the L0 tables; header top hash @0x381
//   is its SHA1).
// - file table (LE24 block number @0x37E, LE16 count @0x37C) holds 64-byte
//   entries: name[0x28], flags+namelen @0x28 (0x80 = dir, 0x40 =
//   consecutive, low 6 bits = name length), allocated LE24 @0x29,
//   start block LE24 @0x2F, path indicator BE u16 @0x32 (0xFFFF = root),
//   size BE u32 @0x34.
// - hash record (24 bytes): SHA1(20) + status(1, 0x80 = used) + next BE24
//   (0xFFFFFF = end of chain).
// Files whose tables exceed one level-1 table (> 28900 data blocks) use an
// unmapped multi-level layout and are refused with a clear error.

pub const STFS_BLOCK: u64 = 0x1000;
pub const STFS_HASH_SPAN: u64 = 170;
pub const STFS_MAX_SINGLE_L1_BLOCKS: u64 = 170 * 170; // 28900

#[derive(Debug, Clone)]
pub struct StfsLayout {
    pub base: u64,
    pub table_off: u64,
    pub table_blocks: u64,
    pub alloc_blocks: u64,
    pub top_hash: [u8; 20],
    pub header_sha1: [u8; 20],
    pub file_size: u64,
}

#[derive(Error, Debug)]
pub enum StfsError {
    #[error("unsupported STFS layout (base 0x{0:X}, expected 0xB000)")]
    UnsupportedBase(u64),
    #[error("SVOD data layout not supported (only STFS containers can be verified/extracted)")]
    SvodLayout,
    #[error(
        "multi-level hash tables beyond one level-1 table are not supported ({0} data blocks)"
    )]
    MultiLevel(u64),
    #[error("table region out of range")]
    TableRange,
    #[error("block {0} out of range")]
    BlockRange(u64),
    #[error("broken hash chain at block {0}")]
    BrokenChain(u64),
    #[error("unexpected record status 0x{0:02X} at data block {1}")]
    BadStatus(u8, u64),
}

fn be_u32_at(buf: &[u8], off: usize) -> Result<u32> {
    buf.get(off..off + 4)
        .and_then(|b| <[u8; 4]>::try_from(b).ok())
        .map(u32::from_be_bytes)
        .context("short read")
}

impl StfsLayout {
    pub fn read(reader: &mut (impl Read + Seek), file_size: u64) -> Result<Self> {
        let hdr = read_at(reader, 0x340, 0x70)?;
        let header_size = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as u64;
        let base = (header_size + 0xFFF) & 0xF000;
        if base != 0xB000 {
            return Err(StfsError::UnsupportedBase(base).into());
        }
        // Volume descriptor @0x379: block sep @+2, table count LE16 @+3,
        // table block LE24 @+5, top hash @+8, allocated BE32 @+0x1C.
        let desc = read_at(reader, 0x379, 0x24)?;
        let table_blocks = u16::from_le_bytes([desc[3], desc[4]]) as u64;
        let table_blk = desc[5] as u64 | ((desc[6] as u64) << 8) | ((desc[7] as u64) << 16);
        let mut top_hash = [0u8; 20];
        top_hash.copy_from_slice(&desc[8..28]);
        let alloc_blocks =
            u32::from_be_bytes([desc[0x1C], desc[0x1D], desc[0x1E], desc[0x1F]]) as u64;
        let table_off = base + (table_blk + 1) * STFS_BLOCK;
        if table_off + table_blocks * STFS_BLOCK > file_size {
            return Err(StfsError::TableRange.into());
        }
        let desc_type = be_u32_at(&read_at(reader, 0x3A9, 4)?, 0)?;
        if desc_type != 0 {
            return Err(StfsError::SvodLayout.into());
        }
        let header_sha1 = read_at(reader, 0x32C, 20)?;
        let mut header_sha1_arr = [0u8; 20];
        header_sha1_arr.copy_from_slice(&header_sha1);
        Ok(StfsLayout {
            base,
            table_off,
            table_blocks,
            alloc_blocks,
            top_hash,
            header_sha1: header_sha1_arr,
            file_size,
        })
    }

    /// Number of level-0 hash tables needed; errors when more than one
    /// level-1 table would be required.
    pub fn num_l0_tables(&self) -> Result<u64> {
        let n = self.alloc_blocks.div_ceil(STFS_HASH_SPAN);
        if n > STFS_HASH_SPAN {
            return Err(StfsError::MultiLevel(self.alloc_blocks).into());
        }
        Ok(n.max(1))
    }

    /// Physical block number holding level-0 table `i`.
    pub fn l0_table_phys(i: u64) -> u64 {
        if i == 0 {
            0
        } else {
            172 + (i - 1) * 171
        }
    }

    /// Physical block number holding data block `b`.
    pub fn data_phys_block(b: u64) -> u64 {
        b + 1 + b / STFS_HASH_SPAN + u64::from(b >= STFS_HASH_SPAN)
    }

    pub fn phys_offset(&self, phys: u64) -> Result<u64> {
        let off = self
            .base
            .checked_add(phys.checked_mul(STFS_BLOCK).context("overflow")?)
            .context("overflow")?;
        if off + STFS_BLOCK > self.file_size {
            return Err(StfsError::BlockRange(phys).into());
        }
        Ok(off)
    }

    pub fn data_offset(&self, b: u64) -> Result<u64> {
        if b >= self.alloc_blocks {
            return Err(StfsError::BlockRange(b).into());
        }
        self.phys_offset(Self::data_phys_block(b))
    }

    pub fn read_block(&self, reader: &mut (impl Read + Seek), phys: u64) -> Result<Vec<u8>> {
        read_at(reader, self.phys_offset(phys)?, STFS_BLOCK as usize)
    }

    pub fn read_l0_record(
        &self,
        reader: &mut (impl Read + Seek),
        table: u64,
        slot: u64,
    ) -> Result<([u8; 20], u8, u64)> {
        let off = self.phys_offset(Self::l0_table_phys(table))? + slot * 24;
        let rec = read_at(reader, off, 24)?;
        let mut hash = [0u8; 20];
        hash.copy_from_slice(&rec[..20]);
        let next = ((rec[21] as u64) << 16) | ((rec[22] as u64) << 8) | (rec[23] as u64);
        Ok((hash, rec[20], next))
    }
}

fn sha1_of(data: &[u8]) -> [u8; 20] {
    use sha1::{Digest, Sha1};
    let mut h = Sha1::new();
    h.update(data);
    h.finalize().into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyReport {
    pub ok: bool,
    pub header_sha1_ok: bool,
    pub top_hash_ok: bool,
    pub tables_checked: u64,
    pub blocks_checked: u64,
    pub blocks_skipped: u64,
    pub errors: Vec<String>,
}

/// Verify an STFS container: header SHA1, top hash, every level-0 table
/// hash, and every used data block hash.
pub fn verify_stfs(reader: &mut (impl Read + Seek), file_size: u64) -> Result<VerifyReport> {
    let layout = StfsLayout::read(reader, file_size)?;
    let mut errors = Vec::new();
    let mut tables_checked = 0u64;
    let mut blocks_checked = 0u64;
    let mut blocks_skipped = 0u64;

    // 1. Header SHA1 over 0x344..base (first hash table).
    let hdr = read_at(reader, 0x344, (layout.base - 0x344) as usize)?;
    let header_sha1_ok = sha1_of(&hdr) == layout.header_sha1;
    if !header_sha1_ok {
        errors.push("header SHA1 mismatch".to_string());
    }

    // 2. Top hash: SHA1 of the level-1 table, or of T0 itself when a
    // single level-0 table covers everything (tiny files have no L1).
    let num_l0 = layout.num_l0_tables()?;
    let top_hash_ok = if num_l0 == 1 {
        let t0 = layout.read_block(reader, 0)?;
        tables_checked += 1;
        let ok = sha1_of(&t0) == layout.top_hash;
        if !ok {
            errors.push("top hash mismatch (level-0 table)".to_string());
        }
        ok
    } else {
        let l1 = layout.read_block(reader, 171)?;
        tables_checked += 1;
        let ok = sha1_of(&l1) == layout.top_hash;
        if !ok {
            errors.push("top hash mismatch (level-1 table)".to_string());
        }

        // 3. Each level-0 table hash must match the level-1 record.
        if ok {
            for i in 0..num_l0 {
                let t = layout.read_block(reader, StfsLayout::l0_table_phys(i))?;
                tables_checked += 1;
                if sha1_of(&t) != l1[i as usize * 24..i as usize * 24 + 20] {
                    errors.push(format!("level-0 table {i} hash mismatch"));
                }
            }
        }
        ok
    };

    // 4. Every used data block hash.
    for b in 0..layout.alloc_blocks {
        let table = b / STFS_HASH_SPAN;
        let slot = b % STFS_HASH_SPAN;
        let (hash, status, _) = layout.read_l0_record(reader, table, slot)?;
        if status != 0x80 {
            blocks_skipped += 1;
            continue;
        }
        let off = layout.data_offset(b)?;
        let blk = read_at(reader, off, STFS_BLOCK as usize)?;
        blocks_checked += 1;
        if sha1_of(&blk) != hash {
            errors.push(format!("data block {b} hash mismatch"));
            if errors.len() > 20 {
                errors.push("...".to_string());
                break;
            }
        }
    }

    Ok(VerifyReport {
        ok: errors.is_empty(),
        header_sha1_ok,
        top_hash_ok,
        tables_checked,
        blocks_checked,
        blocks_skipped,
        errors,
    })
}

/// Full decoded STFS file-table entry.
#[derive(Debug, Clone)]
pub struct StfsFileEntry {
    pub index: usize,
    pub name: String,
    pub flags: u8,
    pub start_block: u64,
    pub size: u64,
    pub parent: u64, // path indicator; 0xFFFF = root
    pub is_dir: bool,
    pub table_offset: u64,
}

/// Resolve an entry's full path from parent indicators (0xFFFF = root).
pub fn entry_full_path(entries: &[StfsFileEntry], index: usize) -> String {
    let mut parts = Vec::new();
    let mut cur = index;
    let mut guard = 0usize;
    loop {
        guard += 1;
        let Some(e) = entries.iter().find(|e| e.index == cur) else {
            break;
        };
        parts.push(e.name.clone());
        if e.parent == 0xFFFF || guard > entries.len() + 1 {
            break;
        }
        let parent = e.parent as usize;
        if parent == cur {
            break;
        }
        cur = parent;
    }
    parts.reverse();
    parts.join("/")
}

/// Read one file's bytes by walking its hash chain, verifying every block.
pub fn read_stfs_file(
    reader: &mut (impl Read + Seek),
    layout: &StfsLayout,
    start_block: u64,
    size: u64,
) -> Result<Vec<u8>> {
    if size == 0 {
        return Ok(Vec::new());
    }
    let need = size.div_ceil(STFS_BLOCK);
    let mut out = Vec::with_capacity(size.min(1 << 30) as usize);
    let mut seen = std::collections::HashSet::new();
    let mut blk = start_block;
    let mut blocks_read = 0u64;
    for _ in 0..need {
        if blk >= layout.alloc_blocks || !seen.insert(blk) {
            return Err(StfsError::BrokenChain(blk).into());
        }
        let table = blk / STFS_HASH_SPAN;
        let slot = blk % STFS_HASH_SPAN;
        let (hash, status, next) = layout.read_l0_record(reader, table, slot)?;
        if status != 0x80 {
            return Err(StfsError::BadStatus(status, blk).into());
        }
        let off = layout.data_offset(blk)?;
        let data = read_at(reader, off, STFS_BLOCK as usize)?;
        if sha1_of(&data) != hash {
            return Err(anyhow::anyhow!("block {blk} hash mismatch"));
        }
        out.extend_from_slice(&data);
        blocks_read += 1;
        if next == 0xFFFFFF {
            break;
        }
        blk = next;
    }
    if blocks_read != need {
        return Err(StfsError::BrokenChain(start_block).into());
    }
    out.truncate(size as usize);
    Ok(out)
}

/// Discover the file-table entries: read the table region (location from
/// the volume descriptor, 0xC000 in all observed files), stop at the first
/// all-zero slot. Returns the layout, the accepted entries (slot indices
/// count every slot, since path indicators refer to them) and the raw
/// bytes of [0, table_end) for string scanning. Falls back to legacy
/// bounds when the layout header itself is unreadable.
pub fn discover_entries(
    reader: &mut (impl Read + Seek),
    file_size: u64,
) -> Result<(StfsLayout, Vec<StfsFileEntry>, Vec<u8>)> {
    const MAX_SLOTS: usize = 4096;
    let layout = StfsLayout::read(reader, file_size).unwrap_or(StfsLayout {
        base: 0xB000,
        table_off: 0xC000,
        table_blocks: file_size.saturating_sub(0xC000) / STFS_BLOCK,
        alloc_blocks: u64::MAX,
        top_hash: [0u8; 20],
        header_sha1: [0u8; 20],
        file_size,
    });
    let table_end = (layout.table_off + (MAX_SLOTS as u64) * 64).min(file_size);
    let buffer = if table_end > 0 {
        read_at(reader, 0, table_end as usize)?
    } else {
        Vec::new()
    };
    let mut entries = Vec::new();
    let mut offset = layout.table_off;
    let mut index = 0usize;
    while offset + 64 <= buffer.len() as u64 {
        let entry_data = &buffer[offset as usize..offset as usize + 64];
        if entry_data.iter().all(|&b| b == 0) {
            break;
        }
        if let Some(entry) = decode_slot(entry_data, index, offset) {
            entries.push(entry);
        }
        index += 1;
        offset += 64;
    }
    Ok((layout, entries, buffer))
}

/// Extract all files of an STFS container into `out_dir`, recreating the
/// directory tree. Every block hash is verified during the walk.
pub fn extract_stfs(
    reader: &mut (impl Read + Seek),
    file_size: u64,
    out_dir: &std::path::Path,
) -> Result<Vec<(String, u64)>> {
    let (layout, entries, _) = discover_entries(reader, file_size)?;
    layout.num_l0_tables()?;
    let mut done = Vec::new();
    for e in &entries {
        let rel = entry_full_path(&entries, e.index);
        let dest = out_dir.join(&rel);
        if e.is_dir {
            std::fs::create_dir_all(&dest)?;
            continue;
        }
        if let Some(parent) = dest.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let data = match read_stfs_file(reader, &layout, e.start_block, e.size) {
            Ok(d) => d,
            Err(err) => {
                let _ = std::fs::remove_file(&dest);
                return Err(err);
            }
        };
        std::fs::write(&dest, &data)?;
        done.push((rel, data.len() as u64));
    }
    Ok(done)
}

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
            display_title: header.display_title,
            region_mask: header.region_mask.map(|m| format!("{:08X}", m)),
            regions_detailed: detailed,
            regions: collapsed,
            xex_exec: header.xex_exec,
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

fn extract_cstring(data: &[u8], start: usize) -> Option<String> {
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

fn looks_like_filename(filename: &str) -> bool {
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
fn decode_slot(data: &[u8], index: usize, offset: u64) -> Option<StfsFileEntry> {
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

#[derive(Parser, Debug)]
#[command(
    name = "tu_parser",
    version,
    about = "Xbox 360 container and image parser (STFS, XISO, XEX, GoD)"
)]
struct Args {
    #[arg()]
    files: Vec<PathBuf>,

    #[arg(long, conflicts_with = "files")]
    dir: Option<PathBuf>,

    #[arg(long, requires = "dir")]
    recursive: bool,

    #[arg(long, conflicts_with_all = ["files", "dir", "extract", "bench_extract_iso"])]
    game_dir: Option<PathBuf>,

    #[arg(short, long, default_value = "text")]
    format: OutputFormat,

    #[arg(short, long)]
    output: Option<PathBuf>,

    #[arg(long)]
    show_strings: bool,

    #[arg(long)]
    show_entries: bool,

    /// Verify STFS hashes (header, tables, data blocks).
    #[arg(long)]
    verify: bool,

    /// Extract STFS contents into OUTDIR (hash-verified during extraction).
    #[arg(long, value_name = "OUTDIR")]
    extract: Option<PathBuf>,

    /// Extract XISO contents into OUTDIR (fast, multi-method).
    #[arg(long, value_name = "OUTDIR", conflicts_with = "extract")]
    extract_iso: Option<PathBuf>,

    /// Benchmark all XISO extraction methods and show fastest.
    #[arg(long, conflicts_with = "extract")]
    bench_extract_iso: bool,

    #[arg(short, long)]
    verbose: bool,
}

#[derive(Debug, Clone, clap::ValueEnum, PartialEq)]
enum OutputFormat {
    Text,
    Json,
}

fn collect_files(
    files: &[PathBuf],
    dir: Option<&PathBuf>,
    recursive: bool,
) -> Result<Vec<PathBuf>> {
    let mut result = Vec::new();

    // Add explicit files
    result.extend(files.iter().cloned());

    // Add files from directory
    if let Some(dir) = dir {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                result.push(path);
            } else if path.is_dir() && recursive {
                // Recursively collect files from subdirectory
                let mut sub_files = collect_files(&[], Some(&path), true)?;
                result.append(&mut sub_files);
            }
        }
    }

    Ok(result)
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Extracted game directory mode.
    if let Some(game_dir) = &args.game_dir {
        let info = parse_game_dir(game_dir)?;
        return emit(
            &args,
            &[Report::GameDir(info)],
            args.show_entries,
            args.show_strings,
        );
    }

    if args.files.is_empty() && args.dir.is_none() {
        anyhow::bail!("no input: pass FILES, --dir <DIR> or --game-dir <DIR>");
    }

    let file_paths = collect_files(&args.files, args.dir.as_ref(), args.recursive)?;

    if file_paths.is_empty() {
        eprintln!("No files to process");
        return Ok(());
    }

    // Sweep tolerance: unrecognized files warn and continue instead of
    // aborting the run (e.g. GOD DataNNNN parts in a directory walk).
    let mut all_results = Vec::new();
    let mut failures: u64 = 0;
    let mut verify_failed = false;
    let json_mode = matches!(args.format, OutputFormat::Json);
    // Human log lines go to stdout in text mode, stderr in JSON mode
    // (stdout must stay pure JSON).
    let log = |s: String| {
        if json_mode {
            eprintln!("{s}");
        } else {
            println!("{s}");
        }
    };
    for file_path in &file_paths {
        if args.verbose {
            eprintln!("Parsing: {}", file_path.display());
        }
        match parse_file(file_path) {
            Ok(Report::Stfs(mut info)) => {
                if args.verbose {
                    eprintln!("  -> {}", report_summary(&Report::Stfs(info.clone())));
                }
                if args.verify || args.extract.is_some() {
                    match open_stfs_reader(file_path) {
                        Ok((mut reader, file_size)) => {
                            if args.verify {
                                match verify_stfs(&mut reader, file_size) {
                                    Ok(rep) => {
                                        if !rep.ok {
                                            verify_failed = true;
                                        }
                                        info.verify = Some(rep);
                                    }
                                    Err(e) => {
                                        verify_failed = true;
                                        eprintln!(
                                            "warning: verify {}: {:#}",
                                            file_path.display(),
                                            e
                                        );
                                    }
                                }
                            }
                            if let Some(outdir) = &args.extract {
                                match extract_report(
                                    &mut reader,
                                    file_size,
                                    &info,
                                    outdir,
                                    &log,
                                    Some(&info.base_title_id),
                                ) {
                                    Ok(n) => {
                                        if args.verbose {
                                            eprintln!("  extracted {n} files");
                                        }
                                    }
                                    Err(e) => {
                                        failures += 1;
                                        eprintln!(
                                            "warning: extract {}: {:#}",
                                            file_path.display(),
                                            e
                                        );
                                        continue;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            failures += 1;
                            eprintln!("warning: skipping {}: {:#}", file_path.display(), e);
                            continue;
                        }
                    }
                }
                all_results.push(Report::Stfs(info));
            }
            Ok(other) => {
                if args.verbose {
                    eprintln!("  -> {}", report_summary(&other));
                }
                if args.verify || args.extract.is_some() {
                    let is_xiso = matches!(&other, Report::Xiso(_));
                    if is_xiso && args.extract.is_some() {
                        eprintln!(
                            "warning: {}: --extract only works for STFS containers; use --extract-iso for XISO/GDF",
                            file_path.display()
                        );
                    } else if !is_xiso {
                        eprintln!(
                            "warning: {}: verify/extract only support STFS containers, skipping",
                            file_path.display()
                        );
                    }
                }
                // XISO extraction
                if let Report::Xiso(ref info) = &other {
                    if let Some(outdir) = &args.extract_iso {
                        let title_id = info.xex_exec.as_ref().map(|x| x.title_id.as_str());
                        match extract_iso_fast_with_progress(
                            file_path,
                            outdir,
                            |msg| eprintln!("{}", msg),
                            title_id,
                        ) {
                            Ok(res) => {
                                eprintln!(
                                    "  extracted {} files ({:.1} MB/s) via {}",
                                    res.files_extracted, res.throughput_mb_s, res.method
                                );
                            }
                            Err(e) => {
                                failures += 1;
                                eprintln!("warning: extract iso {}: {:#}", file_path.display(), e);
                                continue;
                            }
                        }
                    }
                    if args.bench_extract_iso {
                        let bench_dir = std::env::temp_dir()
                            .join(format!("tu_parser_bench_{}", std::process::id()));
                        match benchmark_extract_iso(file_path, &bench_dir) {
                            Ok(results) => {
                                if args.format == OutputFormat::Json {
                                    let json = serde_json::to_string_pretty(&results)?;
                                    println!("{}", json);
                                }
                            }
                            Err(e) => {
                                eprintln!(
                                    "warning: bench extract {}: {:#}",
                                    file_path.display(),
                                    e
                                );
                            }
                        }
                        std::fs::remove_dir_all(&bench_dir).ok();
                    }
                }
                all_results.push(other);
            }
            Err(e) => {
                failures += 1;
                eprintln!("warning: skipping {}: {:#}", file_path.display(), e);
            }
        }
    }
    if all_results.is_empty() {
        anyhow::bail!("no files parsed successfully ({failures} failed)");
    }
    if failures > 0 {
        eprintln!("{failures} file(s) skipped");
    }

    emit(&args, &all_results, args.show_entries, args.show_strings)?;
    if verify_failed {
        std::process::exit(1);
    }
    Ok(())
}

/// Open a fresh streaming reader for an STFS container.
fn open_stfs_reader(path: &PathBuf) -> Result<(BufReader<File>, u64)> {
    let file = File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let file_size = file.metadata()?.len();
    Ok((BufReader::new(file), file_size))
}

/// Extract one parsed STFS report into `out_dir`, recreating the tree.
/// Every block hash is verified during the walk; any failure aborts and
/// removes the partial file.
fn extract_report(
    reader: &mut (impl Read + Seek),
    file_size: u64,
    info: &TuInfo,
    out_dir: &Path,
    log: &dyn Fn(String),
    title_id: Option<&str>,
) -> Result<u64> {
    let layout = StfsLayout::read(reader, file_size)?;
    // Refuse multi-level layouts before writing anything.
    layout.num_l0_tables()?;

    // Create subfolder with title_id if provided
    let final_out_dir = if let Some(tid) = title_id {
        let safe_name = sanitize_dir_name(tid);
        out_dir.join(safe_name)
    } else {
        out_dir.to_path_buf()
    };
    std::fs::create_dir_all(&final_out_dir)?;
    let mut count = 0u64;
    for e in &info.directory_entries {
        let dest = final_out_dir.join(&e.path);
        if e.is_dir {
            std::fs::create_dir_all(&dest)?;
            continue;
        }
        if let Some(parent) = dest.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let data = match read_stfs_file(reader, &layout, e.start_block, e.size) {
            Ok(d) => d,
            Err(err) => {
                let _ = std::fs::remove_file(&dest);
                return Err(err);
            }
        };
        std::fs::write(&dest, &data)?;
        count += 1;
        log(format!(
            "extracted {} ({})",
            dest.display(),
            format_size(e.size)
        ));
    }
    log(format!(
        "{} file(s) extracted to {}",
        count,
        final_out_dir.display()
    ));
    Ok(count)
}

fn report_summary(report: &Report) -> String {
    match report {
        Report::Stfs(i) => format!("{} {}", i.content_kind, i.display_title),
        Report::Xiso(i) => format!("XISO {} ({} entries)", i.iso_type, i.entries.len()),
        Report::Xex(i) => format!("XEX {}", i.exec.title_id),
        Report::Xbe(i) => format!("XBE {}", i.info.title_id),
        Report::GameDir(i) => format!("game dir ({} files)", i.files),
    }
}

fn xex_line(xex: &XexExecInfo) -> String {
    format!(
        "XEX Execution Info:\n  Media ID: {}\n  Version: {}\n  Base Version: {}\n  Title ID: {}\n  Platform: {}\n  Executable Type: {}\n  Disc: {}/{}\n",
        xex.media_id,
        xex.version,
        xex.base_version,
        xex.title_id,
        xex.platform,
        xex.executable_type,
        xex.disc_number,
        xex.disc_count
    )
}

fn render_report(report: &Report, show_entries: bool, show_strings: bool) -> String {
    let mut out = String::new();
    match report {
        Report::Stfs(info) => {
            out.push_str(&format!("=== {} ===\n", info.file_path));
            out.push_str(&format!("File size: {}\n", format_size(info.file_size)));
            out.push_str(&format!("Container: {}\n", info.container_magic));
            out.push_str(&format!(
                "LIVE container: {}\n",
                if info.is_live { "YES" } else { "NO" }
            ));
            out.push_str(&format!(
                "Title Update: {}\n",
                if info.is_title_update { "YES" } else { "NO" }
            ));
            out.push_str(&format!("Kind: {}\n", info.content_kind));
            out.push_str(&format!("Media ID: {}\n", info.media_id));
            out.push_str(&format!("Base title ID: {}\n", info.base_title_id));
            out.push_str(&format!("Content type: {}\n", info.content_type));
            out.push_str(&format!("Title: {}\n", info.display_title));
            out.push_str(&format!(
                "Region mask: {}\n",
                info.region_mask.as_deref().unwrap_or("n/a")
            ));
            out.push_str(&format!(
                "Regions: {}\n",
                if info.regions.is_empty() {
                    "n/a".to_string()
                } else {
                    info.regions.join(" ")
                }
            ));
            if !info.regions_detailed.is_empty() {
                out.push_str(&format!(
                    "Regions (detailed): {}\n",
                    info.regions_detailed.join(" ")
                ));
            }
            if let Some(xex) = &info.xex_exec {
                out.push_str(&xex_line(xex));
            }
            if let Some(v) = &info.verify {
                if v.ok {
                    out.push_str(&format!(
                        "Verify: OK ({} blocks, {} tables)\n",
                        v.blocks_checked, v.tables_checked
                    ));
                } else {
                    out.push_str(&format!(
                        "Verify: FAIL ({} blocks, {} tables)\n",
                        v.blocks_checked, v.tables_checked
                    ));
                    for e in v.errors.iter().take(10) {
                        out.push_str(&format!("  ! {}\n", e));
                    }
                }
            }
            render_entries(
                &mut out,
                &info
                    .directory_entries
                    .iter()
                    .map(|e| (e.path.as_str(), e.offset, e.size, e.is_dir))
                    .collect::<Vec<_>>(),
                show_entries,
            );
            if show_strings {
                out.push_str(&format!(
                    "\nRaw strings found ({}):\n",
                    info.raw_strings.len()
                ));
                for (offset, s) in &info.raw_strings {
                    out.push_str(&format!("  0x{:08X}: {}\n", offset, s));
                }
            }
            out.push('\n');
        }
        Report::Xiso(info) => {
            out.push_str(&format!("=== {} ===\n", info.file_path));
            out.push_str(&format!("File size: {}\n", format_size(info.file_size)));
            out.push_str(&format!("Format: XISO ({})\n", info.iso_type));
            out.push_str(&format!(
                "Root table: sector {} ({} bytes)\n",
                info.root_sector, info.root_size
            ));
            out.push_str(&format!("Volume sectors: {}\n", info.volume_sectors));
            if let Some(ts) = info.volume_timestamp {
                // Convert Windows FILETIME (100ns intervals since 1601-01-01) to readable format
                // For now just show the raw value; could use chrono for conversion
                out.push_str(&format!("Volume timestamp: {} (FILETIME)\n", ts));
            }
            match &info.executable {
                Some(e) => out.push_str(&format!("Executable: {}\n", e)),
                None => out.push_str("Executable: not found\n"),
            }
            if let Some(xex) = &info.xex_exec {
                out.push_str(&xex_line(xex));
            }
            if let Some(xbe) = &info.xbe_info {
                out.push_str(&format!(
                    "XBE: title {} version {}\n",
                    xbe.title_id, xbe.version
                ));
            }
            render_entries(
                &mut out,
                &info
                    .entries
                    .iter()
                    .map(|e| (e.path.as_str(), e.sector as u64, e.size, e.is_dir))
                    .collect::<Vec<_>>(),
                show_entries,
            );
            // Extended attributes display for XISO (when --show-entries)
            if show_entries && !info.entries.is_empty() {
                out.push_str("\nDirectory entry details:\n");
                for e in &info.entries {
                    let mut attr_strs = Vec::new();
                    if e.attrs.read_only {
                        attr_strs.push("R");
                    }
                    if e.attrs.hidden {
                        attr_strs.push("H");
                    }
                    if e.attrs.system {
                        attr_strs.push("S");
                    }
                    if e.attrs.directory {
                        attr_strs.push("D");
                    }
                    if e.attrs.archive {
                        attr_strs.push("A");
                    }
                    if e.attrs.normal {
                        attr_strs.push("N");
                    }
                    let attrs = if attr_strs.is_empty() {
                        "—".to_string()
                    } else {
                        attr_strs.join("")
                    };
                    let l = e
                        .left_child_offset
                        .map(|v| format!("0x{:X}", v))
                        .unwrap_or("—".to_string());
                    let r = e
                        .right_child_offset
                        .map(|v| format!("0x{:X}", v))
                        .unwrap_or("—".to_string());
                    out.push_str(&format!(
                        "  {} | Sector: 0x{:X} | Size: {} | Attrs: {} | L:{} R:{}{}\n",
                        e.path,
                        e.sector,
                        format_size(e.size),
                        attrs,
                        l,
                        r,
                        if e.is_dir { " (dir)" } else { "" }
                    ));
                }
            }
            out.push('\n');
        }
        Report::Xex(info) => {
            out.push_str(&format!("=== {} ===\n", info.file_path));
            out.push_str(&format!("File size: {}\n", format_size(info.file_size)));
            out.push_str("Format: XEX executable\n");
            out.push_str(&xex_line(&info.exec));
            out.push('\n');
        }
        Report::Xbe(info) => {
            out.push_str(&format!("=== {} ===\n", info.file_path));
            out.push_str(&format!("File size: {}\n", format_size(info.file_size)));
            out.push_str("Format: XBE executable (original Xbox)\n");
            out.push_str(&format!(
                "XBE: title {} version {}\n",
                info.info.title_id, info.info.version
            ));
            out.push('\n');
        }
        Report::GameDir(info) => {
            out.push_str(&format!("=== {} ===\n", info.dir_path));
            out.push_str("Format: extracted game directory\n");
            out.push_str(&format!(
                "Files: {}  Dirs: {}  Total: {}\n",
                info.files,
                info.dirs,
                format_size(info.total_bytes)
            ));
            match &info.executable {
                Some(e) => out.push_str(&format!("Executable: {}\n", e)),
                None => out.push_str("Executable: default.xex/.xbe not found\n"),
            }
            if let Some(xex) = &info.xex_exec {
                out.push_str(&xex_line(xex));
            }
            if let Some(xbe) = &info.xbe_info {
                out.push_str(&format!(
                    "XBE: title {} version {}\n",
                    xbe.title_id, xbe.version
                ));
            }
            out.push('\n');
        }
    }
    out
}

fn render_entries(out: &mut String, entries: &[(&str, u64, u64, bool)], show_all: bool) {
    // entries: (name, offset-or-sector, size, is_dir). STFS passes table
    // offsets, XISO passes sector numbers; both print as hex.
    if show_all || entries.len() <= 50 {
        out.push_str(&format!("\nDirectory entries ({}):\n", entries.len()));
        for (i, (name, at, size, is_dir)) in entries.iter().enumerate() {
            out.push_str(&format!(
                "  [{}] {} | Offset: 0x{:X} | Size: {}{}\n",
                i,
                name,
                at,
                format_size(*size),
                if *is_dir { " (dir)" } else { "" }
            ));
        }
    } else {
        out.push_str(&format!(
            "\nDirectory entries ({}): [use --show-entries to list all]\n",
            entries.len()
        ));
        for (i, (name, at, size, is_dir)) in entries.iter().take(10).enumerate() {
            out.push_str(&format!(
                "  [{}] {} | Offset: 0x{:X} | Size: {}{}\n",
                i,
                name,
                at,
                format_size(*size),
                if *is_dir { " (dir)" } else { "" }
            ));
        }
        out.push_str(&format!("  ... and {} more\n", entries.len() - 10));
    }
}

fn emit(args: &Args, reports: &[Report], show_entries: bool, show_strings: bool) -> Result<()> {
    let output = match args.format {
        OutputFormat::Json => serde_json::to_string_pretty(&reports)?,
        OutputFormat::Text => {
            let mut out = String::new();
            for report in reports {
                out.push_str(&render_report(report, show_entries, show_strings));
            }
            out
        }
    };
    if let Some(output_path) = &args.output {
        std::fs::write(output_path, output)?;
    } else {
        print!("{}", output);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_free_mask() {
        assert_eq!(decode_regions(0xFFFFFFFF), vec!["Region Free".to_string()]);
    }

    #[test]
    fn region_bits_match_checker() {
        // Masks taken from Title_Update_List.txt samples.
        assert_eq!(
            decode_regions(0x00FF00FF),
            vec![
                "PAL(A)".to_string(),
                "PAL(E)".to_string(),
                "NTSC/U".to_string()
            ]
        );
        assert_eq!(
            decode_regions(0x0000FD00),
            vec!["NTSC/J(J)".to_string(), "NTSC/J(0)".to_string()]
        );
        assert_eq!(decode_regions(0x000000FF), vec!["NTSC/U".to_string()]);
        assert_eq!(
            decode_regions(0x00FF0000),
            vec!["PAL(A)".to_string(), "PAL(E)".to_string()]
        );
    }

    #[test]
    fn regions_collapse_pal() {
        let detailed = decode_regions(0x00FF00FF);
        assert_eq!(
            collapse_regions(&detailed),
            vec!["PAL".to_string(), "NTSC/U".to_string()]
        );
    }

    #[test]
    fn sizes_format() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(312), "312 B");
        assert_eq!(format_size(10712), "10712 (10.5 KB)");
        assert_eq!(format_size(4128768), "4128768 (3.9 MB)");
    }

    #[test]
    fn entry_size_and_dir_flag() {
        // Synthetic 64-byte STFS entry: name + flags + BE size at +0x34.
        let mut entry = vec![0u8; 64];
        entry[..12].copy_from_slice(b"default.xexp");
        entry[0x28] = 0x4C;
        entry[0x34..0x38].copy_from_slice(&1134080u32.to_be_bytes());
        let parsed = decode_slot(&entry, 0, 0xC000).expect("entry parses");
        assert_eq!(parsed.name, "default.xexp");
        assert_eq!(parsed.size, 1134080);
        assert!(!parsed.is_dir);

        entry[0x28] = 0x80 | 12;
        entry[0x34..0x38].copy_from_slice(&0u32.to_be_bytes());
        let parsed = decode_slot(&entry, 1, 0xC080).expect("dir parses");
        assert!(parsed.is_dir);
        assert_eq!(parsed.size, 0);
    }

    #[test]
    fn container_magics() {
        assert_eq!(magic_name(MAGIC_LIVE), "LIVE");
        assert_eq!(magic_name(MAGIC_CON), "CON");
        assert_eq!(magic_name(MAGIC_PIRS), "PIRS");
        assert_eq!(magic_name(0xDEADBEEF), "Unknown");
        assert_eq!(MAGIC_LIVE, 0x4556494C);
        assert_eq!(MAGIC_CON, u32::from_le_bytes(*b"CON "));
        assert_eq!(MAGIC_PIRS, u32::from_le_bytes(*b"PIRS"));
    }

    #[test]
    fn content_kinds() {
        assert_eq!(content_type_name(0x000B0000), "Title Update");
        assert_eq!(content_type_name(0x00000002), "Marketplace Content (DLC)");
        assert_eq!(content_type_name(0x000D0000), "Xbox Live Arcade (XBLA)");
        assert_eq!(content_type_name(0x00009000), "Avatar Item");
        assert_eq!(content_type_name(0x00030000), "Theme");
        assert_eq!(content_type_name(0xDEADBEEF), "Unknown");
    }

    fn write_synthetic_dlc(path: &std::path::Path) {
        // Minimal DLC-like LIVE container: header fields + 1 file-table
        // entry, no TUPD signature area, no XEX.
        let mut buf = vec![0u8; 0xC0C0];
        buf[0..4].copy_from_slice(b"LIVE");
        buf[0x340..0x344].copy_from_slice(&0xAD0Eu32.to_be_bytes());
        buf[0x344..0x348].copy_from_slice(&0x00000002u32.to_be_bytes());
        buf[0x354..0x358].copy_from_slice(&0u32.to_be_bytes());
        buf[0x360..0x364].copy_from_slice(&0x434307D2u32.to_be_bytes());
        buf[0x379] = 0x24;
        buf[0x37C..0x37E].copy_from_slice(&1u16.to_le_bytes());
        buf[0x395..0x399].copy_from_slice(&1u32.to_be_bytes());
        let title: Vec<u16> = "Miami Nights Key".encode_utf16().collect();
        for (i, c) in title.iter().enumerate() {
            buf[0x411 + i * 2..0x411 + i * 2 + 2].copy_from_slice(&c.to_be_bytes());
        }
        buf[0xC000..0xC00B].copy_from_slice(b"DSDLKey.bin");
        buf[0xC000 + 0x28] = 0x40 | 11; // consecutive + namelen
        buf[0xC000 + 0x34..0xC000 + 0x38].copy_from_slice(&21u32.to_be_bytes());
        std::fs::write(path, &buf).unwrap();
    }

    #[test]
    fn xex_exec_info_parses() {
        // Synthetic XEX2 header with one ExecutionId field (0x00040006).
        let mut buf = vec![0u8; 0x100];
        buf[0..4].copy_from_slice(b"XEX2");
        buf[20..24].copy_from_slice(&1u32.to_be_bytes());
        buf[24..28].copy_from_slice(&0x00040006u32.to_be_bytes());
        buf[28..32].copy_from_slice(&0x40u32.to_be_bytes());
        buf[0x40..0x44].copy_from_slice(&0x36D691EEu32.to_be_bytes());
        buf[0x44..0x48].copy_from_slice(&0x00000103u32.to_be_bytes());
        buf[0x48..0x4C].copy_from_slice(&0x00000103u32.to_be_bytes());
        buf[0x4C..0x50].copy_from_slice(&0x58410886u32.to_be_bytes());
        let info = parse_xex_exec_info(&buf, 0).expect("exec parses");
        assert_eq!(info.media_id, "36D691EE");
        assert_eq!(info.version, "00000103");
        assert_eq!(info.title_id, "58410886");

        // TU patch XEX shape (no ExecutionId) yields None.
        let mut buf = vec![0u8; 0x100];
        buf[0..4].copy_from_slice(b"XEX2");
        buf[20..24].copy_from_slice(&2u32.to_be_bytes());
        assert!(parse_xex_exec_info(&buf, 0).is_none());
    }

    #[test]
    fn dlc_without_xex_parses() {
        let path = std::env::temp_dir().join(format!("tu_parser_dlc_{}.bin", std::process::id()));
        write_synthetic_dlc(&path);
        let mut parser = TuParser::open(&path).expect("synthetic dlc opens");
        let info = parser.parse().expect("synthetic dlc parses");
        std::fs::remove_file(&path).ok();

        assert!(!info.is_title_update);
        assert_eq!(info.content_kind, "Marketplace Content (DLC)");
        assert_eq!(info.media_id, "00000000");
        assert_eq!(info.base_title_id, "434307D2");
        assert_eq!(info.display_title, "Miami Nights Key");
        assert_eq!(info.region_mask, None);
        assert!(info.regions.is_empty());
        assert_eq!(info.directory_entries.len(), 1);
        assert_eq!(info.directory_entries[0].filename, "DSDLKey.bin");
        assert_eq!(info.directory_entries[0].size, 21);
    }

    /// Build a minimal XSF image: volume descriptor at sector 0x20, one
    /// file entry in the root table (mirrors iso2god's unit-test layout).
    fn write_synthetic_xsf(path: &std::path::Path) {
        let mut img = vec![0u8; 64 * 0x800];
        let desc = 0x20 * 0x800;
        img[desc..desc + 20].copy_from_slice(b"MICROSOFT*XBOX*MEDIA");
        img[desc + 20..desc + 24].copy_from_slice(&40u32.to_le_bytes());
        img[desc + 24..desc + 28].copy_from_slice(&0x800u32.to_le_bytes());
        let table = 40 * 0x800;
        img[table..table + 2].copy_from_slice(&0u16.to_le_bytes());
        img[table + 2..table + 4].copy_from_slice(&0u16.to_le_bytes());
        img[table + 4..table + 8].copy_from_slice(&50u32.to_le_bytes());
        img[table + 8..table + 12].copy_from_slice(&100u32.to_le_bytes());
        img[table + 12] = 0x20;
        img[table + 13] = 1;
        img[table + 14] = b'a';
        std::fs::write(path, &img).unwrap();
    }

    #[test]
    fn xiso_synthetic_parses() {
        let path = std::env::temp_dir().join(format!("tu_parser_xsf_{}.iso", std::process::id()));
        write_synthetic_xsf(&path);
        let size = std::fs::metadata(&path).unwrap().len();
        let report = parse_file(&path).expect("synthetic xsf parses");
        std::fs::remove_file(&path).ok();
        match report {
            Report::Xiso(info) => {
                assert_eq!(info.iso_type, "XSF");
                assert_eq!(info.file_size, size);
                assert_eq!(info.entries.len(), 1);
                assert_eq!(info.entries[0].path, "a");
                assert_eq!(info.entries[0].sector, 50);
                assert_eq!(info.entries[0].size, 100);
                assert!(!info.entries[0].is_dir);
                assert_eq!(info.executable, None);
            }
            _ => panic!("expected xiso"),
        }
    }

    #[test]
    fn xbe_synthetic_parses() {
        let path = std::env::temp_dir().join(format!("tu_parser_xbe_{}.xbe", std::process::id()));
        let mut buf = vec![0u8; 0x400];
        buf[0..4].copy_from_slice(b"XBEH");
        buf[8..12].copy_from_slice(&0x4D53084Du32.to_le_bytes());
        buf[176..180].copy_from_slice(&0x00000002u32.to_le_bytes());
        std::fs::write(&path, &buf).unwrap();
        let report = parse_file(&path).expect("synthetic xbe parses");
        std::fs::remove_file(&path).ok();
        match report {
            Report::Xbe(info) => {
                assert_eq!(info.info.title_id, "4D53084D");
                assert_eq!(info.info.version, "00000002");
            }
            _ => panic!("expected xbe"),
        }
    }

    #[test]
    fn unknown_file_rejected() {
        let path = std::env::temp_dir().join(format!("tu_parser_unk_{}.bin", std::process::id()));
        std::fs::write(&path, b"definitely not an xbox file................").unwrap();
        let err = parse_file(&path).expect_err("unknown file rejected");
        std::fs::remove_file(&path).ok();
        assert!(matches!(
            err.downcast_ref::<TuError>(),
            Some(TuError::UnknownFormat)
        ));
    }

    #[test]
    fn game_dir_parses_default_xex() {
        let dir = std::env::temp_dir().join(format!("tu_parser_gdir_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Minimal XEX2 with ExecutionId.
        let mut xex = vec![0u8; 0x100];
        xex[0..4].copy_from_slice(b"XEX2");
        xex[20..24].copy_from_slice(&1u32.to_be_bytes());
        xex[24..28].copy_from_slice(&0x00040006u32.to_be_bytes());
        xex[28..32].copy_from_slice(&0x40u32.to_be_bytes());
        xex[0x40..0x44].copy_from_slice(&0x36D691EEu32.to_be_bytes());
        xex[0x44..0x48].copy_from_slice(&0x00000103u32.to_be_bytes());
        xex[0x48..0x4C].copy_from_slice(&0x00000103u32.to_be_bytes());
        xex[0x4C..0x50].copy_from_slice(&0x58410886u32.to_be_bytes());
        std::fs::write(dir.join("default.xex"), &xex).unwrap();
        std::fs::write(dir.join("readme.txt"), b"hello").unwrap();
        let info = parse_game_dir(&dir).expect("game dir parses");
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(info.files, 2);
        assert_eq!(info.total_bytes, 0x100 + 5);
        assert_eq!(info.executable.as_deref(), Some("default.xex"));
        let exec = info.xex_exec.expect("exec parsed");
        assert_eq!(exec.title_id, "58410886");
        assert!(info.xbe_info.is_none());
    }

    /// Build a minimal synthetic STFS image with real hashes: header,
    /// one L0 table, a 2-block consecutive file plus a subdir with a
    /// chained (non-consecutive) 1-block file.
    fn write_synthetic_stfs(path: &std::path::Path) -> Vec<u8> {
        use sha1::{Digest, Sha1};
        let mut img = vec![0u8; 0x10000];
        img[0..4].copy_from_slice(b"LIVE");
        img[0x340..0x344].copy_from_slice(&0xAD0Eu32.to_be_bytes());
        img[0x344..0x348].copy_from_slice(&0x000B0000u32.to_be_bytes());
        img[0x379] = 0x24;
        img[0x37C..0x37E].copy_from_slice(&1u16.to_le_bytes());
        img[0x395..0x399].copy_from_slice(&4u32.to_be_bytes()); // alloc: table + 3 data
                                                                // file data blocks 1,2 (file) and 3 (chained file)
        let d1 = vec![0xAAu8; 0x1000];
        let mut d2 = vec![0u8; 0x1000];
        d2[..5].copy_from_slice(b"hello");
        let d3 = vec![0x55u8; 0x1000];
        img[0xD000..0xE000].copy_from_slice(&d1);
        img[0xE000..0xF000].copy_from_slice(&d2);
        img[0xF000..0x10000].copy_from_slice(&d3);
        // file table at 0xC000: dir "sub", file "big.bin" (start 1, 5000 B),
        // file "one.bin" in sub (start 3, chained single block)
        let mut e = |slot: usize, name: &[u8], flags: u8, start: u32, size: u32, parent: u16| {
            let o = 0xC000 + slot * 64;
            img[o..o + name.len()].copy_from_slice(name);
            img[o + 0x28] = flags | (name.len() as u8);
            img[o + 0x29..o + 0x2C].copy_from_slice(&1u32.to_le_bytes()[..3]);
            img[o + 0x2F..o + 0x32].copy_from_slice(&start.to_le_bytes()[..3]);
            img[o + 0x32..o + 0x34].copy_from_slice(&parent.to_be_bytes());
            img[o + 0x34..o + 0x38].copy_from_slice(&size.to_be_bytes());
        };
        e(0, b"sub", 0x80, 0, 0, 0xFFFF);
        e(1, b"big.bin", 0x40, 1, 5000, 0xFFFF);
        e(2, b"one.bin", 0x00, 3, 100, 0);
        // L0 table at 0xB000: rec0 = table block, rec1/2/3 = data
        let h = |img: &[u8], r: std::ops::Range<usize>| Sha1::digest(&img[r]).to_vec();
        let h0 = h(&img, 0xC000..0xD000);
        let h1 = h(&img, 0xD000..0xE000);
        let h2 = h(&img, 0xE000..0xF000);
        let h3 = h(&img, 0xF000..0x10000);
        let mut t = |slot: usize, hash: &[u8], next: u32| {
            let o = 0xB000 + slot * 24;
            img[o..o + 20].copy_from_slice(hash);
            img[o + 20] = 0x80;
            img[o + 21..o + 24].copy_from_slice(&next.to_be_bytes()[1..]);
        };
        t(0, &h0, 0xFFFFFF);
        t(1, &h1, 2);
        t(2, &h2, 0xFFFFFF);
        t(3, &h3, 0xFFFFFF);
        // top hash first (it lives inside the header-sha1 range), then header
        let top = Sha1::digest(&img[0xB000..0xC000]).to_vec();
        img[0x381..0x395].copy_from_slice(&top);
        let hdr = Sha1::digest(&img[0x344..0xB000]).to_vec();
        img[0x32C..0x340].copy_from_slice(&hdr);
        std::fs::write(path, &img).unwrap();
        img
    }

    #[test]
    fn stfs_verify_ok_and_extract() {
        let path = std::env::temp_dir().join(format!("tu_parser_stfs_{}.bin", std::process::id()));
        write_synthetic_stfs(&path);
        let file = File::open(&path).unwrap();
        let size = file.metadata().unwrap().len();
        let mut reader = BufReader::new(file);
        let rep = verify_stfs(&mut reader, size).expect("verify runs");
        assert!(rep.ok, "errors: {:?}", rep.errors);
        assert!(rep.header_sha1_ok && rep.top_hash_ok);
        assert_eq!(rep.blocks_checked, 4);

        // extract
        let out = std::env::temp_dir().join(format!("tu_parser_stfs_out_{}", std::process::id()));
        let file = File::open(&path).unwrap();
        let mut reader = BufReader::new(file);
        let layout = StfsLayout::read(&mut reader, size).unwrap();
        layout.num_l0_tables().unwrap();
        let data = read_stfs_file(&mut reader, &layout, 1, 5000).unwrap();
        assert_eq!(data.len(), 5000);
        assert!(data.starts_with(&[0xAAu8; 16]));
        assert_eq!(&data[4096..4101], b"hello");
        let one = read_stfs_file(&mut reader, &layout, 3, 100).unwrap();
        assert_eq!(one.len(), 100);
        assert!(one.iter().all(|&b| b == 0x55));
        drop(reader);
        std::fs::remove_file(&path).ok();
        std::fs::remove_dir_all(&out).ok();
    }

    #[test]
    fn stfs_verify_detects_tamper() {
        let path =
            std::env::temp_dir().join(format!("tu_parser_tamper_{}.bin", std::process::id()));
        let mut img = write_synthetic_stfs(&path);
        img[0xD000] ^= 0xFF;
        std::fs::write(&path, &img).unwrap();
        let file = File::open(&path).unwrap();
        let size = file.metadata().unwrap().len();
        let mut reader = BufReader::new(file);
        let rep = verify_stfs(&mut reader, size).expect("verify runs");
        std::fs::remove_file(&path).ok();
        assert!(!rep.ok);
        assert!(!rep.header_sha1_ok || rep.errors.iter().any(|e| e.contains("block 1")));
    }

    #[test]
    fn stfs_multilevel_refused() {
        // alloc needs 177 L0 tables -> multi level-1, unsupported.
        let mut img = vec![0u8; 0xC000];
        img[0..4].copy_from_slice(b"LIVE");
        img[0x340..0x344].copy_from_slice(&0xAD0Eu32.to_be_bytes());
        img[0x379] = 0x24;
        img[0x395..0x399].copy_from_slice(&30000u32.to_be_bytes());
        let path = std::env::temp_dir().join(format!("tu_parser_big_{}.bin", std::process::id()));
        std::fs::write(&path, &img).unwrap();
        let file = File::open(&path).unwrap();
        let size = file.metadata().unwrap().len();
        let mut reader = BufReader::new(file);
        let layout = StfsLayout::read(&mut reader, size).expect("layout reads");
        assert!(layout.num_l0_tables().is_err());
        assert!(verify_stfs(&mut reader, size).is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn stfs_paths_resolve() {
        let path = std::env::temp_dir().join(format!("tu_parser_paths_{}.bin", std::process::id()));
        write_synthetic_stfs(&path);
        let file = File::open(&path).unwrap();
        let size = file.metadata().unwrap().len();
        let mut reader = BufReader::new(file);
        let layout = StfsLayout::read(&mut reader, size).unwrap();
        // reimplement tiny discovery: read table region, decode slots
        let mut raw = Vec::new();
        let mut off = layout.table_off;
        let mut idx = 0usize;
        while off + 64 <= size {
            let mut slot = vec![0u8; 64];
            use std::io::Read as _;
            reader.seek(SeekFrom::Start(off)).unwrap();
            reader.read_exact(&mut slot).unwrap();
            if slot.iter().all(|&b| b == 0) {
                break;
            }
            if let Some(e) = decode_slot(&slot, idx, off) {
                raw.push(e);
            }
            idx += 1;
            off += 64;
        }
        assert_eq!(entry_full_path(&raw, 2), "sub/one.bin");
        assert_eq!(entry_full_path(&raw, 1), "big.bin");
        std::fs::remove_file(&path).ok();
    }
}
