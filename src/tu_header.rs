//! STFS (TU/DLC) header parsing.

use std::io::{Read, Seek, SeekFrom};

use anyhow::Result;
use byteorder::{BigEndian, LittleEndian, ReadBytesExt};
use serde::{Deserialize, Serialize};

use crate::constants::{TuError, CONTENT_TITLE_UPDATE, MAGIC_CON, MAGIC_LIVE, MAGIC_PIRS};
use crate::xex::{find_xex_offset, parse_xex_exec_info, read_at, XexExecInfo};

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
