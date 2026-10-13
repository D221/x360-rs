use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::PathBuf;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IsoEntry {
    pub path: String,
    pub sector: u32,
    pub size: u64,
    pub is_dir: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IsoInfo {
    pub file_path: String,
    pub file_size: u64,
    pub iso_type: String,
    pub root_sector: u32,
    pub root_size: u32,
    pub volume_sectors: u64,
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

/// Walk one GDF directory table, flattening entries to `prefix`-joined paths.
/// Mirrors iso2god-rs `DirectoryTable::read` / `DirectoryEntry::read`.
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
            let attrs = ab[0];
            reader.read_exact(&mut ab)?;
            let name_len = ab[0] as usize;
            let mut name = vec![0u8; name_len];
            reader.read_exact(&mut name)?;
            let name = String::from_utf8_lossy(&name).into_owned();
            let pos = reader.stream_position()?;
            let pad = ((4 - pos % 4) % 4) as i64;
            reader.seek(SeekFrom::Current(pad))?;

            let is_dir = attrs & GDF_ATTR_DIRECTORY != 0;
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
    pub offset: u64,
    pub size: u64,
    pub is_dir: bool,
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
        xex_exec,
        xbe_info,
        executable,
        entries,
    })
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

        let mut entries = Vec::new();
        let mut raw_strings = Vec::new();
        self.extract_strings_and_entries(&mut entries, &mut raw_strings)?;

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
        })
    }

    fn extract_strings_and_entries(
        &mut self,
        entries: &mut Vec<DirectoryEntry>,
        raw_strings: &mut Vec<(u64, String)>,
    ) -> Result<()> {
        // Only the header + file-table region is ever needed here (table
        // slots, fixed strings). Never load the whole container: TUs/XBLA
        // images can be hundreds of MB while this region tops out at a few
        // hundred KB. Cap the table scan at 4096 slots as a sanity bound.
        const TABLE_START: usize = 0xC000;
        const MAX_SLOTS: usize = 4096;
        let scan_end = (TABLE_START + MAX_SLOTS * 64).min(self.file_size as usize);
        let buffer = if scan_end > 0 {
            read_at(&mut self.reader, 0, scan_end)?
        } else {
            Vec::new()
        };

        const DEFAULT_XEX_OFFSET: usize = 0xC000;
        const DEFAULT_DIR_OFFSET: usize = 0xC040;
        if let Some(s) = extract_cstring(&buffer, DEFAULT_XEX_OFFSET) {
            raw_strings.push((DEFAULT_XEX_OFFSET as u64, s));
        }
        if let Some(s) = extract_cstring(&buffer, DEFAULT_DIR_OFFSET) {
            raw_strings.push((DEFAULT_DIR_OFFSET as u64, s));
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

        // STFS file table: 64-byte entries starting at 0xC000. The table
        // is densely packed: stop at the first all-zero slot. (The slot
        // holding the embedded "XEX2" magic and bytes past the table are
        // file data, never reached thanks to the zero terminator.)
        let dir_start = 0xC000usize;
        let entry_size = 64;
        let mut offset = dir_start;
        while offset + entry_size <= buffer.len() {
            let entry_data = &buffer[offset..offset + entry_size];
            if entry_data.iter().all(|&b| b == 0) {
                break;
            }
            if let Some(entry) = parse_directory_entry(entry_data, offset as u64) {
                entries.push(entry);
            }
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

fn parse_directory_entry(data: &[u8], offset: u64) -> Option<DirectoryEntry> {
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

    let looks_like_filename = filename.contains('.')
        || filename.contains('\\')
        || filename.contains('/')
        || filename.ends_with(".xexp")
        || filename.ends_with(".wav")
        || filename.ends_with(".dds")
        || filename.starts_with("dlc")
        || filename.starts_with("gd_")
        || filename.starts_with("prop_")
        || filename.chars().filter(|c| c.is_alphanumeric()).count() > 3;

    if !looks_like_filename {
        return None;
    }

    // STFS file-table entry: file size is a big-endian u32 at entry+0x34.
    // Directories (flags 0x80 at entry+0x28) have size 0.
    let size = u32::from_be_bytes([data[0x34], data[0x35], data[0x36], data[0x37]]) as u64;
    let is_dir = data[0x28] & 0x80 != 0;

    Some(DirectoryEntry {
        filename,
        offset,
        size,
        is_dir,
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

    #[arg(long, conflicts_with_all = ["files", "dir"])]
    game_dir: Option<PathBuf>,

    #[arg(short, long, default_value = "text")]
    format: OutputFormat,

    #[arg(short, long)]
    output: Option<PathBuf>,

    #[arg(long)]
    show_strings: bool,

    #[arg(long)]
    show_entries: bool,

    #[arg(short, long)]
    verbose: bool,
}

#[derive(Debug, Clone, clap::ValueEnum)]
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
    for file_path in &file_paths {
        if args.verbose {
            eprintln!("Parsing: {}", file_path.display());
        }
        match parse_file(file_path) {
            Ok(report) => {
                if args.verbose {
                    eprintln!("  -> {}", report_summary(&report));
                }
                all_results.push(report);
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

    emit(&args, &all_results, args.show_entries, args.show_strings)
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
        "XEX: media {} version {} (base {}) title {} platform {} type {} disc {}/{}\n",
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
            render_entries(
                &mut out,
                &info
                    .directory_entries
                    .iter()
                    .map(|e| (e.filename.as_str(), e.offset, e.size, e.is_dir))
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
        let parsed = parse_directory_entry(&entry, 0xC000).expect("entry parses");
        assert_eq!(parsed.filename, "default.xexp");
        assert_eq!(parsed.size, 1134080);
        assert!(!parsed.is_dir);

        entry[0x28] = 0xCA;
        entry[0x34..0x38].copy_from_slice(&0u32.to_be_bytes());
        let parsed = parse_directory_entry(&entry, 0xC080).expect("dir parses");
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
        buf[0x344..0x348].copy_from_slice(&0x00000002u32.to_be_bytes());
        buf[0x354..0x358].copy_from_slice(&0u32.to_be_bytes());
        buf[0x360..0x364].copy_from_slice(&0x434307D2u32.to_be_bytes());
        let title: Vec<u16> = "Miami Nights Key".encode_utf16().collect();
        for (i, c) in title.iter().enumerate() {
            buf[0x411 + i * 2..0x411 + i * 2 + 2].copy_from_slice(&c.to_be_bytes());
        }
        buf[0xC000..0xC00B].copy_from_slice(b"DSDLKey.bin");
        buf[0xC000 + 0x28] = 0x49;
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
}
