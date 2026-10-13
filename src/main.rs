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
    #[error("Not a Title Update: expected 'TUPD' signature at 0x971A, got {0:08X}")]
    InvalidSignature(u32),
    #[error("XEX2 header not found in file")]
    XexNotFound,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
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
    pub magic: u32,            // 0x0000: "LIVE" (LE u32 0x4556494C)
    pub tu_signature: u32,     // 0x971A: "TUPD" (LE u32 0x44505554)
    pub content_type: u32,     // 0x0344: BE u32, 0x000B0000 for TU (per iso2god con_header.rs)
    pub media_id: u32, // 0x0354: BE u32, TU media ID (matches [ID] in Title_Update_List.txt)
    pub base_title_id: u32, // 0x0360: BE u32, base game title ID (per iso2god with_execution_info)
    pub display_title: String, // 0x0411: UTF-16BE string (per iso2god with_game_title)
    pub xex_offset: u64, // file offset of embedded "XEX2"
    pub region_mask: u32, // BE u32 at xex_offset + 0x1A0
}

impl TuHeader {
    pub fn read(reader: &mut (impl Read + Seek), file_size: u64) -> Result<Self> {
        reader.seek(SeekFrom::Start(0))?;
        let magic = reader.read_u32::<LittleEndian>()?;
        if magic != 0x4556494C {
            return Err(TuError::InvalidMagic(magic).into());
        }

        reader.seek(SeekFrom::Start(0x971A))?;
        let tu_signature = reader.read_u32::<LittleEndian>()?;
        if tu_signature != 0x44505554 {
            return Err(TuError::InvalidSignature(tu_signature).into());
        }

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

        // Locate embedded XEX2 and read region mask at xex_offset + 0x1A0
        reader.seek(SeekFrom::Start(0))?;
        let mut buf = vec![0u8; file_size as usize];
        reader.read_exact(&mut buf)?;
        let xex_offset = buf
            .windows(4)
            .position(|w| w == b"XEX2")
            .ok_or(TuError::XexNotFound)? as u64;
        let roff = xex_offset as usize + 0x1A0;
        if roff + 4 > buf.len() {
            anyhow::bail!("region mask offset out of range");
        }
        let region_mask =
            u32::from_be_bytes([buf[roff], buf[roff + 1], buf[roff + 2], buf[roff + 3]]);

        Ok(TuHeader {
            magic,
            tu_signature,
            content_type,
            media_id,
            base_title_id,
            display_title,
            xex_offset,
            region_mask,
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
    pub media_id: String,
    pub base_title_id: String,
    pub content_type: String,
    pub display_title: String,
    pub region_mask: String,
    pub regions_detailed: Vec<String>,
    pub regions: Vec<String>,
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

        let detailed = decode_regions(header.region_mask);
        let collapsed = collapse_regions(&detailed);

        let mut entries = Vec::new();
        let mut raw_strings = Vec::new();
        self.extract_strings_and_entries(header.xex_offset, &mut entries, &mut raw_strings)?;

        Ok(TuInfo {
            file_path: String::new(),
            file_size: self.file_size,
            is_live: true,
            is_title_update: true,
            media_id: format!("{:08X}", header.media_id),
            base_title_id: format!("{:08X}", header.base_title_id),
            content_type: format!("0x{:08X}", header.content_type),
            display_title: header.display_title,
            region_mask: format!("{:08X}", header.region_mask),
            regions_detailed: detailed,
            regions: collapsed,
            directory_entries: entries,
            raw_strings,
        })
    }

    fn extract_strings_and_entries(
        &mut self,
        xex_offset: u64,
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

        // STFS file table: 64-byte entries starting at 0xC000, bounded by
        // the embedded XEX data (slot at xex_offset is the "XEX2" magic,
        // not an entry; bytes past it are file data, not names).
        let dir_start = 0xC000usize;
        let dir_end = (xex_offset as usize).min(buffer.len());
        let entry_size = 64;
        for offset in (dir_start..dir_end).step_by(entry_size) {
            if offset + entry_size > buffer.len() {
                break;
            }
            let entry_data = &buffer[offset..offset + entry_size];
            if let Some(entry) = parse_directory_entry(entry_data, offset as u64) {
                entries.push(entry);
            }
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
            eprintln!("  Media ID: {}", info.media_id);
            eprintln!("  Base title ID: {}", info.base_title_id);
            eprintln!("  Title: {}", info.display_title);
            eprintln!("  Region mask: {}", info.region_mask);
            eprintln!("  Regions: {}", info.regions.join(" "));
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
                out.push_str(&format!("Media ID: {}\n", info.media_id));
                out.push_str(&format!("Base title ID: {}\n", info.base_title_id));
                out.push_str(&format!("Content type: {}\n", info.content_type));
                out.push_str(&format!("Title: {}\n", info.display_title));
                out.push_str(&format!("Region mask: {}\n", info.region_mask));
                out.push_str(&format!("Regions: {}\n", info.regions.join(" ")));
                out.push_str(&format!(
                    "Regions (detailed): {}\n",
                    info.regions_detailed.join(" ")
                ));
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
        assert_eq!(collapse_regions(&detailed), vec!["PAL".to_string(), "NTSC/U".to_string()]);
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
}
