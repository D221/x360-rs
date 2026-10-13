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
