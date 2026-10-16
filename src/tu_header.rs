//! STFS (TU/DLC) header parsing.

use std::io::{Read, Seek, SeekFrom};

use anyhow::Result;
use byteorder::{BigEndian, LittleEndian, ReadBytesExt};
use serde::{Deserialize, Serialize};

use crate::constants::{TuError, CONTENT_TITLE_UPDATE, MAGIC_CON, MAGIC_LIVE, MAGIC_PIRS};
use crate::xex::{find_xex_offset, parse_xex_exec_info, read_at, XexExecInfo};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StfsLicense {
    pub id: String,
    pub bits: u32,
    pub flags: u32,
}

/// Dashboard thumbnail image (PNG/DDS/BMP as stored).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StfsThumbnail {
    pub kind: String,
    pub offset: u64,
    pub size: u64,
    pub format: String,
}

/// Sniff a dashboard image blob (PNG, DDS, BMP, JPEG).
pub fn sniff_image_format(blob: &[u8]) -> &'static str {
    if blob.starts_with(b"\x89PNG\r\n\x1a\n") {
        "png"
    } else if blob.starts_with(b"DDS ") {
        "dds"
    } else if blob.starts_with(b"BM") {
        "bmp"
    } else if blob.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "jpg"
    } else {
        "bin"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuHeader {
    pub magic: u32,                   // 0x0000: "LIVE" (LE u32 0x4556494C)
    pub tu_signature: Option<u32>, // 0x971A: "TUPD" (LE u32 0x44505554), present in title updates
    pub content_type: u32,         // 0x0344: BE u32 (per iso2god con_header.rs)
    pub media_id: u32,             // 0x0354: BE u32, media ID (0 for DLC)
    pub base_title_id: u32, // 0x0360: BE u32, base game title ID (per iso2god with_execution_info)
    pub display_title: String, // 0x0411: UTF-16BE string (per iso2god with_game_title)
    pub content_size: u64,  // 0x034C: BE u64
    pub version: u32,       // 0x0358: BE u32
    pub base_version: u32,  // 0x035C: BE u32
    pub save_game_id: u32,  // 0x0368: BE u32
    pub console_id: String, // 0x036C: 5 bytes hex
    pub profile_id: String, // 0x0371: 8 bytes hex
    pub device_id: String,  // 0x03FD: 0x14 bytes hex
    pub data_file_count: u32, // 0x039D: BE u32
    pub data_file_combined_size: u64, // 0x03A1: BE u64
    pub descriptor_type: u32, // 0x03A9: 0 = STFS, 1 = SVOD
    pub metadata_version: u32, // 0x0348: BE u32 (v1/v2 thumbnail layout)
    pub transfer_flags: u8, // 0x1711
    pub transfer_flag_names: Vec<String>,
    pub licenses: Vec<StfsLicense>, // 16x16B at 0x022C, non-zero only
    pub thumbnails: Vec<StfsThumbnail>, // dashboard images at 0x171A/0x571A
    pub xex_offset: Option<u64>,    // file offset of embedded "XEX2", if any
    pub region_mask: Option<u32>,   // BE u32 at xex_offset + 0x1A0 (TU patch XEX only)
    pub xex_exec: Option<XexExecInfo>, // ExecutionId from full game XEX (XBLA etc.)
}

/// Transfer flag names (Free60 STFS doc, byte at 0x1711).
pub fn transfer_flag_names(flags: u8) -> Vec<String> {
    const NAMES: [(u8, &str); 6] = [
        (1 << 2, "DeepLink"),
        (1 << 3, "NoNetworkStorage"),
        (1 << 4, "Kinect"),
        (1 << 5, "MoveOnly"),
        (1 << 6, "DeviceIdTransfer"),
        (1 << 7, "ProfileIdTransfer"),
    ];
    NAMES
        .iter()
        .filter(|(b, _)| flags & b != 0)
        .map(|(_, n)| n.to_string())
        .collect()
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
        reader.seek(SeekFrom::Start(0x0348))?;
        let metadata_version = reader.read_u32::<BigEndian>()?;

        reader.seek(SeekFrom::Start(0x0354))?;
        let media_id = reader.read_u32::<BigEndian>()?;

        reader.seek(SeekFrom::Start(0x0360))?;
        let base_title_id = reader.read_u32::<BigEndian>()?;

        reader.seek(SeekFrom::Start(0x034C))?;
        let content_size = reader.read_u64::<BigEndian>()?;

        reader.seek(SeekFrom::Start(0x0358))?;
        let version = reader.read_u32::<BigEndian>()?;
        let base_version = reader.read_u32::<BigEndian>()?;

        reader.seek(SeekFrom::Start(0x0368))?;
        let save_game_id = reader.read_u32::<BigEndian>()?;

        let mut hex_at = |off: u64, len: usize| -> Result<String> {
            reader.seek(SeekFrom::Start(off))?;
            let mut b = vec![0u8; len];
            reader.read_exact(&mut b)?;
            Ok(b.iter().map(|x| format!("{x:02X}")).collect())
        };
        let console_id = hex_at(0x036C, 5)?;
        let profile_id = hex_at(0x0371, 8)?;
        let device_id = hex_at(0x03FD, 0x14)?;

        reader.seek(SeekFrom::Start(0x039D))?;
        let data_file_count = reader.read_u32::<BigEndian>()?;
        let data_file_combined_size = reader.read_u64::<BigEndian>()?;
        reader.seek(SeekFrom::Start(0x03A9))?;
        let descriptor_type = reader.read_u32::<BigEndian>()?;

        reader.seek(SeekFrom::Start(0x1711))?;
        let mut flag = [0u8; 1];
        reader.read_exact(&mut flag)?;
        let transfer_flags = flag[0];

        // 16 license slots at 0x022C, each id u64 + bits u32 + flags u32.
        let mut licenses = Vec::new();
        reader.seek(SeekFrom::Start(0x022C))?;
        for _ in 0..16 {
            let id = reader.read_u64::<BigEndian>()?;
            let bits = reader.read_u32::<BigEndian>()?;
            let flags = reader.read_u32::<BigEndian>()?;
            if id != 0 || bits != 0 || flags != 0 {
                licenses.push(StfsLicense {
                    id: format!("{id:016X}"),
                    bits,
                    flags,
                });
            }
        }

        // Dashboard thumbnails: sizes at 0x1712/0x1716, blobs at
        // 0x171A/0x571A (both metadata versions; v2 raises the caps).
        let mut thumbnails = Vec::new();
        for (kind, size_off, data_off) in [
            ("thumbnail", 0x1712u64, 0x171Au64),
            ("title", 0x1716, 0x571A),
        ] {
            reader.seek(SeekFrom::Start(size_off))?;
            let size = reader.read_u32::<BigEndian>()? as u64;
            if size > 0 && data_off + size <= file_size {
                reader.seek(SeekFrom::Start(data_off))?;
                let mut head = vec![0u8; size.min(16) as usize];
                reader.read_exact(&mut head)?;
                thumbnails.push(StfsThumbnail {
                    kind: kind.to_string(),
                    offset: data_off,
                    size,
                    format: sniff_image_format(&head).to_string(),
                });
            }
        }

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
            content_size,
            version,
            base_version,
            save_game_id,
            console_id,
            profile_id,
            device_id,
            data_file_count,
            data_file_combined_size,
            descriptor_type,
            metadata_version,
            transfer_flags,
            transfer_flag_names: transfer_flag_names(transfer_flags),
            licenses,
            thumbnails,
            xex_offset,
            region_mask,
            xex_exec,
        })
    }
}
