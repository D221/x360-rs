//! XEX2 header parsing and XEX location inside containers.

use std::io::{Read, Seek, SeekFrom};

use anyhow::Result;
use serde::{Deserialize, Serialize};

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

/// Extra header fields from the same optional-header walk
/// (Free60 XEX doc): module flags, game ratings, LAN key,
/// security-info region. All optional — absent entries yield `None`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XexExtendedInfo {
    pub module_flags: u32,
    pub module_flag_names: Vec<String>,
    pub ratings: Option<String>,
    pub rating_labels: Vec<String>,
    pub lan_key: Option<String>,
    pub security_region: Option<u32>,
    pub region_names: Vec<String>,
}

/// Named module flags (XEX header offset 4).
pub fn module_flag_names(flags: u32) -> Vec<String> {
    const NAMES: [(u32, &str); 8] = [
        (1 << 0, "TitleModule"),
        (1 << 1, "ExportsToTitle"),
        (1 << 2, "SystemDebugger"),
        (1 << 3, "DllModule"),
        (1 << 4, "ModulePatch"),
        (1 << 5, "PatchFull"),
        (1 << 6, "PatchDelta"),
        (1 << 7, "UserMode"),
    ];
    NAMES
        .iter()
        .filter(|(b, _)| flags & b != 0)
        .map(|(_, n)| n.to_string())
        .collect()
}

/// Rating board order in the `0x40310` entry (Free60 XEX doc), each with its
/// value table from Xenia's `xex2_info.h`.
const RATING_BOARDS: [&str; 12] = [
    "ESRB", "PEGI", "PEGI-FI", "PEGI-PT", "BBFC", "CERO", "USK", "OFLC-AU", "OFLC-NZ", "KMRB",
    "BRASIL", "FPB",
];

/// Decode one rating byte into a readable label. Unknown values keep hex
/// rather than guessing.
pub fn rating_label(board: usize, value: u8) -> String {
    let name: Option<&str> = match board {
        0 => match value {
            0x00 => Some("eC"),
            0x02 => Some("E"),
            0x04 => Some("E10"),
            0x06 => Some("T"),
            0x08 => Some("M"),
            0x0E => Some("AO"),
            0xFF => Some("Unrated"),
            _ => None,
        },
        1 => match value {
            0 => Some("3+"),
            4 => Some("7+"),
            9 => Some("12+"),
            13 => Some("16+"),
            14 => Some("18+"),
            0xFF => Some("Unrated"),
            _ => None,
        },
        2 => match value {
            0 => Some("3+"),
            4 => Some("7+"),
            8 => Some("11+"),
            12 => Some("15+"),
            14 => Some("18+"),
            0xFF => Some("Unrated"),
            _ => None,
        },
        3 => match value {
            1 => Some("4+"),
            3 => Some("6+"),
            9 => Some("12+"),
            13 => Some("16+"),
            14 => Some("18+"),
            0xFF => Some("Unrated"),
            _ => None,
        },
        4 => match value {
            0 => Some("3+"),
            1 => Some("U"),
            5 => Some("PG"),
            4 => Some("7+"),
            9 => Some("12+"),
            12 => Some("15+"),
            13 => Some("16+"),
            14 => Some("18+"),
            0xFF => Some("Unrated"),
            _ => None,
        },
        5 => match value {
            0 => Some("A"),
            2 => Some("B"),
            4 => Some("C"),
            6 => Some("D"),
            8 => Some("Z"),
            0xFF => Some("Unrated"),
            _ => None,
        },
        6 => match value {
            0 => Some("All"),
            2 => Some("6+"),
            4 => Some("12+"),
            6 => Some("16+"),
            8 => Some("18+"),
            0xFF => Some("Unrated"),
            _ => None,
        },
        7 | 8 => match value {
            0 => Some("G"),
            2 => Some("PG"),
            4 => Some("M"),
            6 => Some("MA15+"),
            0xFF => Some("Unrated"),
            _ => None,
        },
        9 => match value {
            0 => Some("All"),
            2 => Some("12+"),
            4 => Some("15+"),
            6 => Some("18+"),
            0xFF => Some("Unrated"),
            _ => None,
        },
        10 => match value {
            0 => Some("All"),
            2 => Some("12+"),
            4 => Some("14+"),
            5 => Some("16+"),
            8 => Some("18+"),
            0xFF => Some("Unrated"),
            _ => None,
        },
        11 => match value {
            0 => Some("All"),
            6 => Some("PG"),
            7 => Some("10+"),
            10 => Some("13+"),
            13 => Some("16+"),
            14 => Some("18+"),
            0xFF => Some("Unrated"),
            _ => None,
        },
        _ => None,
    };
    match name {
        Some(n) => format!("{}={n}", RATING_BOARDS.get(board).unwrap_or(&"?")),
        None => format!("{}={value:02X}?", RATING_BOARDS.get(board).unwrap_or(&"?")),
    }
}

/// Decode the 12 rating bytes into `BOARD=label` strings.
pub fn rating_labels(bytes: &[u8; 12]) -> Vec<String> {
    bytes
        .iter()
        .enumerate()
        .map(|(i, &b)| rating_label(i, b))
        .collect()
}

/// Decode the security-info region mask (Xenia `xex2_region_flags`).
/// This is a different field from the TU region mask in `regions.rs`.
pub fn security_region_names(region: u32) -> Vec<String> {
    if region == 0xFFFF_FFFF {
        return vec!["Region Free".to_string()];
    }
    let mut out = Vec::new();
    if region & 0x0000_00FF != 0 {
        out.push("NTSC/U".to_string());
    }
    if region & 0x0000_FF00 != 0 {
        out.push("NTSC/J".to_string());
        if region & 0x0000_0100 != 0 {
            out.push("NTSC/J(J)".to_string());
        }
        if region & 0x0000_0200 != 0 {
            out.push("NTSC/J(C)".to_string());
        }
    }
    if region & 0x00FF_0000 != 0 {
        out.push("PAL".to_string());
        if region & 0x0001_0000 != 0 {
            out.push("PAL(AU/NZ)".to_string());
        }
    }
    if region & 0xFF00_0000 != 0 {
        out.push("Other".to_string());
    }
    out
}

/// Parse extra XEX header fields at `xex_offset` in `buf`: module flags
/// (u32 at +4), game ratings `0x40310` (12 bytes at entry offset),
/// LAN key `0x40404` (16 bytes at entry offset), security-info region
/// (u32 at security-info offset + `0x178`). Returns `None` when the buffer
/// is not an XEX2 header; individual fields are `None` when absent.
pub fn parse_xex_extended_info(buf: &[u8], xex_offset: usize) -> Option<XexExtendedInfo> {
    let need = xex_offset.checked_add(24)?;
    if need > buf.len() || &buf[xex_offset..xex_offset + 4] != b"XEX2" {
        return None;
    }
    let u32_at = |o: usize| -> Option<u32> {
        let b = buf.get(xex_offset.checked_add(o)?..xex_offset.checked_add(o)?.checked_add(4)?)?;
        Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    };
    let module_flags = u32_at(4)?;
    let field_count = u32_at(20)? as usize;
    if field_count > 256 {
        return None;
    }
    let mut rating_bytes: Option<[u8; 12]> = None;
    let mut lan_key = None;
    for i in 0..field_count {
        let base = xex_offset.checked_add(24 + i * 8)?;
        if base + 8 > buf.len() {
            return None;
        }
        let key = u32::from_be_bytes([buf[base], buf[base + 1], buf[base + 2], buf[base + 3]]);
        let val = u32::from_be_bytes([buf[base + 4], buf[base + 5], buf[base + 6], buf[base + 7]]);
        // Entry data lives at `val` as a file offset for these IDs
        // (their low bytes are not 0x01, which would mean inline data).
        let data_at = |len: usize| -> Option<usize> {
            let e = xex_offset.checked_add(val as usize)?;
            if e + len > buf.len() {
                return None;
            }
            Some(e)
        };
        match key {
            // 12 rating bytes: ESRB, PEGI, PEGI-FI, PEGI-PT, BBFC, CERO,
            // USK, OFLC-AU, OFLC-NZ, KMRB, BRASIL, FPB.
            0x00040310 => {
                if let Some(e) = data_at(12) {
                    let mut raw = [0u8; 12];
                    raw.copy_from_slice(&buf[e..e + 12]);
                    rating_bytes = Some(raw);
                }
            }
            // 16-byte LAN key.
            0x00040404 => {
                if let Some(e) = data_at(16) {
                    lan_key = Some(
                        buf[e..e + 16]
                            .iter()
                            .map(|b| format!("{b:02X}"))
                            .collect::<Vec<_>>()
                            .join(""),
                    );
                }
            }
            _ => {}
        }
    }
    // Security-info region: u32 at security-info offset + 0x178.
    let security_region = u32_at(0x10).and_then(|off| {
        let at = xex_offset.checked_add(off as usize)?.checked_add(0x178)?;
        let b = buf.get(at..at.checked_add(4)?)?;
        Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    });
    let region_names = security_region
        .map(security_region_names)
        .unwrap_or_default();
    let ratings = rating_bytes.map(|raw| {
        raw.iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join("")
    });
    let rating_labels = rating_bytes
        .map(|raw| self::rating_labels(&raw))
        .unwrap_or_default();
    Some(XexExtendedInfo {
        module_flags,
        module_flag_names: module_flag_names(module_flags),
        ratings,
        rating_labels,
        lan_key,
        security_region,
        region_names,
    })
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

/// Read exactly `len` bytes at `offset`. Short reads (EOF) are an error.
pub(crate) fn read_at(reader: &mut (impl Read + Seek), offset: u64, len: usize) -> Result<Vec<u8>> {
    reader.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    Ok(buf)
}

/// Locate the embedded `XEX2` magic with a chunked streaming search so
/// multi-hundred-MB containers never get loaded into memory.
pub(crate) fn find_xex_offset(
    reader: &mut (impl Read + Seek),
    file_size: u64,
) -> Result<Option<u64>> {
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
