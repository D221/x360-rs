//! Top-level parse entry points (files, ISOs, game directories).

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::constants::{TuError, MAGIC_CON, MAGIC_LIVE, MAGIC_PIRS};
use crate::iso::{
    detect_iso_type, parse_xbe_info, read_u32_le, read_u64_le, walk_gdf_table, IsoEntry, IsoInfo,
    SECTOR_SIZE,
};
use crate::report::{GameDirInfo, Report, XbeFileInfo, XexFileInfo};
use crate::tu_parser::TuParser;
use crate::xex::{parse_xex_exec_info, parse_xex_extended_info, read_at};

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
            extended: parse_xex_extended_info(&win, 0),
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
    // XDBF has no fixed magic: structural probe last (strict validation).
    drop(reader);
    if let Ok(xdbf) = crate::xdbf::parse_xdbf(path, file_size) {
        return Ok(Report::Xdbf(crate::xdbf::report_info(&xdbf)));
    }
    Err(TuError::UnknownFormat.into())
}

/// Read up to `len` bytes at `offset` (clamped to EOF; short final reads OK).
pub(crate) fn read_window(
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
