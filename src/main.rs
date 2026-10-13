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
    #[error("Not a valid LIVE container: expected 'LIVE' magic, got {0:08X}")]
    InvalidMagic(u32),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// STFS content types (BE u32 at 0x0344).
pub const CONTENT_TITLE_UPDATE: u32 = 0x000B0000;
pub const CONTENT_MARKETPLACE: u32 = 0x00000002;
pub const CONTENT_XBLA: u32 = 0x000D0000;
pub const CONTENT_AVATAR: u32 = 0x00009000;
pub const CONTENT_THEME: u32 = 0x00030000;

/// Human-readable content-type name.
pub fn content_type_name(content_type: u32) -> &'static str {
    match content_type {
        CONTENT_TITLE_UPDATE => "Title Update",
        CONTENT_MARKETPLACE => "Marketplace Content (DLC)",
        CONTENT_XBLA => "Xbox Live Arcade (XBLA)",
        CONTENT_AVATAR => "Avatar Item",
        CONTENT_THEME => "Theme",
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
    pub fn read(reader: &mut (impl Read + Seek), file_size: u64) -> Result<Self> {
        reader.seek(SeekFrom::Start(0))?;
        let magic = reader.read_u32::<LittleEndian>()?;
        if magic != 0x4556494C {
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

        // Locate embedded XEX2. Only TU *patch* XEXs carry the region
        // mask at xex_offset + 0x1A0 — full game XEXs (XBLA etc.) have a
        // real header there instead, so only read the mask for title updates.
        // Full game XEXs do carry ExecutionId (field 0x00040006) with
        // media/version/title IDs (iso2god-rs src/executable/xex.rs).
        reader.seek(SeekFrom::Start(0))?;
        let mut buf = vec![0u8; file_size as usize];
        reader.read_exact(&mut buf)?;
        let xex_offset = buf.windows(4).position(|w| w == b"XEX2").map(|p| p as u64);
        let is_tu = content_type == CONTENT_TITLE_UPDATE;
        let region_mask = if is_tu {
            xex_offset.and_then(|x| {
                let roff = x as usize + 0x1A0;
                if roff + 4 <= buf.len() {
                    Some(u32::from_be_bytes([
                        buf[roff],
                        buf[roff + 1],
                        buf[roff + 2],
                        buf[roff + 3],
                    ]))
                } else {
                    None
                }
            })
        } else {
            None
        };
        let xex_exec = xex_offset.and_then(|x| parse_xex_exec_info(&buf, x as usize));

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
            is_live: true,
            is_title_update,
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
        self.reader.seek(SeekFrom::Start(0))?;
        let mut buffer = vec![0u8; self.file_size as usize];
        self.reader.read_exact(&mut buffer)?;

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
    about = "Xbox 360 Title Update file parser"
)]
struct Args {
    #[arg(required_unless_present = "dir")]
    files: Vec<PathBuf>,

    #[arg(long, conflicts_with = "files")]
    dir: Option<PathBuf>,

    #[arg(long, requires = "dir")]
    recursive: bool,

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

    let file_paths = collect_files(&args.files, args.dir.as_ref(), args.recursive)?;

    if file_paths.is_empty() {
        eprintln!("No files to process");
        return Ok(());
    }

    let mut all_results = Vec::new();
    for file_path in &file_paths {
        if args.verbose {
            eprintln!("Parsing: {}", file_path.display());
        }
        let mut parser = TuParser::open(file_path)?;
        let mut info = parser.parse()?;
        info.file_path = file_path.display().to_string();
        if args.verbose {
            eprintln!("  File size: {} bytes", info.file_size);
            eprintln!("  Kind: {}", info.content_kind);
            eprintln!("  Media ID: {}", info.media_id);
            eprintln!("  Base title ID: {}", info.base_title_id);
            eprintln!("  Title: {}", info.display_title);
            eprintln!(
                "  Region mask: {}",
                info.region_mask.as_deref().unwrap_or("n/a")
            );
            eprintln!(
                "  Regions: {}",
                if info.regions.is_empty() {
                    "n/a".to_string()
                } else {
                    info.regions.join(" ")
                }
            );
        }
        all_results.push(info);
    }

    let output = match args.format {
        OutputFormat::Json => serde_json::to_string_pretty(&all_results)?,
        OutputFormat::Text => {
            let mut out = String::new();
            for info in &all_results {
                out.push_str(&format!("=== {} ===\n", info.file_path));
                out.push_str(&format!("File size: {}\n", format_size(info.file_size)));
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
                    out.push_str(&format!(
                        "XEX: media {} version {} (base {}) title {} platform {} type {} disc {}/{}\n",
                        xex.media_id,
                        xex.version,
                        xex.base_version,
                        xex.title_id,
                        xex.platform,
                        xex.executable_type,
                        xex.disc_number,
                        xex.disc_count
                    ));
                }
                if args.show_entries || info.directory_entries.len() <= 50 {
                    out.push_str(&format!(
                        "\nDirectory entries ({}):\n",
                        info.directory_entries.len()
                    ));
                    for (i, entry) in info.directory_entries.iter().enumerate() {
                        out.push_str(&format!(
                            "  [{}] {} | Offset: 0x{:X} | Size: {}{}\n",
                            i,
                            entry.filename,
                            entry.offset,
                            format_size(entry.size),
                            if entry.is_dir { " (dir)" } else { "" }
                        ));
                    }
                } else {
                    out.push_str(&format!(
                        "\nDirectory entries ({}): [use --show-entries to list all]\n",
                        info.directory_entries.len()
                    ));
                    for (i, entry) in info.directory_entries.iter().take(10).enumerate() {
                        out.push_str(&format!(
                            "  [{}] {} | Offset: 0x{:X} | Size: {}{}\n",
                            i,
                            entry.filename,
                            entry.offset,
                            format_size(entry.size),
                            if entry.is_dir { " (dir)" } else { "" }
                        ));
                    }
                    out.push_str(&format!(
                        "  ... and {} more\n",
                        info.directory_entries.len() - 10
                    ));
                }
                if args.show_strings {
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
            out
        }
    };

    if let Some(output_path) = args.output {
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
}
