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
    pub lan_key: Option<String>,
    pub security_region: Option<u32>,
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
    let mut ratings = None;
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
                    ratings = Some(
                        buf[e..e + 12]
                            .iter()
                            .map(|b| format!("{b:02X}"))
                            .collect::<Vec<_>>()
                            .join(""),
                    );
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
    Some(XexExtendedInfo {
        module_flags,
        module_flag_names: module_flag_names(module_flags),
        ratings,
        lan_key,
        security_region,
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
