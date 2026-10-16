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
pub struct XexPageDescriptor {
    pub info: u32,
    pub page_count: u32,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XexExtendedInfo {
    pub module_flags: u32,
    pub module_flag_names: Vec<String>,
    pub ratings: Option<String>,
    pub rating_labels: Vec<String>,
    pub lan_key: Option<String>,
    pub security_region: Option<u32>,
    pub region_names: Vec<String>,
    /// Every optional-header entry as `(ID, data)` hex, including ones with
    /// no dedicated decoder (Xenia `UserModule::Dump` default arm).
    pub headers: Vec<XexHeaderEntry>,
    pub entry_point: Option<u32>,
    pub image_base: Option<u32>,
    pub original_base: Option<u32>,
    pub stack_size: Option<u32>,
    pub heap_size: Option<u32>,
    pub fs_cache_size: Option<u32>,
    pub system_flags: Option<u32>,
    pub system_flag_names: Vec<String>,
    pub system_flags2: Option<u32>, // IMAGEKEY_SYSTEM_FLAGS2 (0x00030100)
    pub system_flag2_names: Vec<String>,
    pub tls: Option<XexTlsInfo>,
    pub static_libs: Vec<XexStaticLib>,
    pub pe_name: Option<String>,
    pub image_flags: Option<u32>,
    pub image_flag_names: Vec<String>,
    pub allowed_media: Option<u32>,
    pub allowed_media_names: Vec<String>,
    pub xgd2_media_id: Option<String>,
    pub load_address: Option<u32>,
    pub page_descriptors: Vec<XexPageDescriptor>,
    /// Additional optional-header / security-info fields (XexTool
    /// `src/XexImageEntryTypes.h`, verified layouts).
    pub savegame_id: Option<String>, // ExecutionId.saveGameId
    pub bounding_path: Option<String>,         // 0x000080FF
    pub bounding_device_id: Option<String>,    // 0x00008105 (20 bytes)
    pub restrict_console_ids: Vec<String>,     // 0x000042FF (5-byte IDs)
    pub restrict_kv_privs: Option<(u64, u64)>, // 0x00004004 (mask, val)
    pub restrict_dates: Option<(u64, u64)>,    // 0x00004104 (start, end)
    pub disc_profile_id: Option<String>,       // 0x00004304 (16 bytes)
    pub alt_title_ids: Vec<u32>,               // 0x000407FF
    pub multidisc_media_ids: Vec<String>,      // 0x000406FF (16-byte MediaIds)
    pub image_checksum: Option<u32>,           // 0x00018002
    pub image_filetime: Option<u32>,           // 0x00018002
    pub page_heap: Option<(u32, u32)>,         // 0x00028002 (size, flags)
    pub logo_data: Option<String>,             // 0x000405FF
}

/// Image flag names (Xenia `xex2_image_flags`).
pub fn image_flag_names(flags: u32) -> Vec<String> {
    const NAMES: [(u32, &str); 18] = [
        (0x00000002, "ManufacturingUtility"),
        (0x00000004, "ManufacturingSupportTools"),
        (0x00000008, "Xgd2MediaOnly"),
        (0x00000100, "CardeaKey"),
        (0x00000200, "XeikaKey"),
        (0x00000400, "UsermodeTitle"),
        (0x00000800, "UsermodeSystem"),
        (0x00001000, "Orange0"),
        (0x00002000, "Orange1"),
        (0x00004000, "Orange2"),
        (0x00010000, "IptvSignup"),
        (0x00020000, "IptvTitle"),
        (0x04000000, "KeyvaultPrivs"),
        (0x08000000, "OnlineActivation"),
        (0x10000000, "Page4K"),
        (0x20000000, "RegionFree"),
        (0x40000000, "RevocationOptional"),
        (0x80000000, "RevocationRequired"),
    ];
    NAMES
        .iter()
        .filter(|(b, _)| flags & b != 0)
        .map(|(_, n)| n.to_string())
        .collect()
}

/// Allowed-media names (Xenia `xex2_media_flags`).
pub fn allowed_media_names(media: u32) -> Vec<String> {
    const NAMES: [(u32, &str); 17] = [
        (0x00000001, "HardDisk"),
        (0x00000002, "DvdX2"),
        (0x00000004, "DvdCd"),
        (0x00000008, "Dvd5"),
        (0x00000010, "Dvd9"),
        (0x00000020, "SystemFlash"),
        (0x00000080, "MemoryUnit"),
        (0x00000100, "UsbMassStorage"),
        (0x00000200, "Network"),
        (0x00000400, "DirectFromMemory"),
        (0x00000800, "RamDrive"),
        (0x00001000, "Svod"),
        (0x01000000, "InsecurePackage"),
        (0x02000000, "SavegamePackage"),
        (0x04000000, "LocallySignedPackage"),
        (0x08000000, "LiveSignedPackage"),
        (0x10000000, "XboxPackage"),
    ];
    NAMES
        .iter()
        .filter(|(b, _)| media & b != 0)
        .map(|(_, n)| n.to_string())
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XexTlsInfo {
    pub slots: u32,
    pub data_address: u32,
    pub data_size: u32,
    pub raw_size: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XexStaticLib {
    pub name: String,
    pub version: String,
    pub approval: String,
}

/// System flag names (Xenia `xex2_system_flags`).
pub fn system_flag_names(flags: u32) -> Vec<String> {
    const NAMES: [(u32, &str); 32] = [
        (0x00000001, "NoForcedReboot"),
        (0x00000002, "ForegroundTasks"),
        (0x00000004, "NoOddMapping"),
        (0x00000008, "HandleMceInput"),
        (0x00000010, "RestrictedHud"),
        (0x00000020, "HandleGamepadDisconnect"),
        (0x00000040, "InsecureSockets"),
        (0x00000080, "Xbox1Interop"),
        (0x00000100, "DashContext"),
        (0x00000200, "GameVoiceChannel"),
        (0x00000400, "Pal50Incompatible"),
        (0x00000800, "InsecureUtilityDrive"),
        (0x00001000, "XamHooks"),
        (0x00002000, "AccessPii"),
        (0x00004000, "CrossPlatformLink"),
        (0x00008000, "MultidiscSwap"),
        (0x00010000, "MultidiscInsecureMedia"),
        (0x00020000, "Ap25Media"),
        (0x00040000, "NoConfirmExit"),
        (0x00080000, "BackgroundDownload"),
        (0x00100000, "PersistableRamdrive"),
        (0x00200000, "InheritRamdrive"),
        (0x00400000, "HudVibration"),
        (0x00800000, "UtilityPartitions"),
        (0x01000000, "IptvInput"),
        (0x02000000, "BigButtonInput"),
        (0x04000000, "ExtendedReservation"),
        (0x08000000, "MultidiscCrossTitle"),
        (0x10000000, "InstallIncompatible"),
        (0x20000000, "AvatarMetadataByXuid"),
        (0x40000000, "ControllerSwapping"),
        (0x80000000, "DashExtensibility"),
    ];
    NAMES
        .iter()
        .filter(|(b, _)| flags & b != 0)
        .map(|(_, n)| n.to_string())
        .collect()
}

/// System flags 2 names (XexTool `XexDefines.h`, `SYSFLAG2_*`).
pub fn system_flag2_names(flags: u32) -> Vec<String> {
    const NAMES: [(u32, &str); 10] = [
        (0x00000001, "AllowNetworkReadCancel"),
        (0x00000002, "UninterruptableReads"),
        (0x00000004, "RequiresNXE"),
        (0x00000008, "GamevoiceRequiredUI"),
        (0x00000010, "TitleSetsPresenceString"),
        (0x00000020, "NatalTiltControl"),
        (0x00000040, "SupportsSkeletalTracking"),
        (0x00000080, "RequiresSkeletalTracking"),
        (0x00000100, "UseLargeHdsFileCache"),
        (0x00000200, "TitleSupportsDeepLink"),
    ];
    NAMES
        .iter()
        .filter(|(b, _)| flags & b != 0)
        .map(|(_, n)| n.to_string())
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XexHeaderEntry {
    pub id: String,
    pub data: String,
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
    let mut headers = Vec::new();
    // Scalar inline values keyed by header ID (verified on real files:
    // e.g. entry point reads as an 0x82xxxxxx address, stack as 0x40000).
    let mut entry_point = None;
    let mut image_base = None;
    let mut original_base = None;
    let mut stack_size = None;
    let mut heap_size = None;
    let mut fs_cache_size = None;
    let mut system_flags = None;
    let mut system_flags2 = None;
    let mut tls = None;
    let mut static_libs = Vec::new();
    let mut pe_name = None;
    let mut savegame_id = None;
    let mut bounding_path = None;
    let mut bounding_device_id = None;
    let mut restrict_console_ids = Vec::new();
    let mut restrict_kv_privs = None;
    let mut restrict_dates = None;
    let mut disc_profile_id = None;
    let mut alt_title_ids = Vec::new();
    let mut multidisc_media_ids = Vec::new();
    let mut image_checksum = None;
    let mut image_filetime = None;
    let mut page_heap = None;
    let mut logo_data = None;
    for i in 0..field_count {
        let base = xex_offset.checked_add(24 + i * 8)?;
        if base + 8 > buf.len() {
            return None;
        }
        let key = u32::from_be_bytes([buf[base], buf[base + 1], buf[base + 2], buf[base + 3]]);
        let val = u32::from_be_bytes([buf[base + 4], buf[base + 5], buf[base + 6], buf[base + 7]]);
        headers.push(XexHeaderEntry {
            id: format!("{key:08X}"),
            data: format!("{val:08X}"),
        });
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
            // ExecutionId savegame ID: u32 at ExecutionId data + 16.
            0x00040006 => {
                if let Some(e) = data_at(20) {
                    savegame_id = Some(format!(
                        "{:08X}",
                        u32::from_be_bytes([buf[e + 16], buf[e + 17], buf[e + 18], buf[e + 19]])
                    ));
                }
            }
            // Inline u32 scalars (verified on real files).
            0x00010001 => original_base = Some(val),
            0x00010100 => entry_point = Some(val),
            0x00010201 => image_base = Some(val),
            0x00020200 => stack_size = Some(val),
            0x00020301 => fs_cache_size = Some(val),
            0x00020401 => heap_size = Some(val),
            0x00030000 => system_flags = Some(val),
            0x00030100 => system_flags2 = Some(val),
            // TLS info: 16 bytes at entry offset.
            0x00020104 => {
                if let Some(e) = data_at(16) {
                    let u = |o: usize| {
                        u32::from_be_bytes([
                            buf[e + o],
                            buf[e + o + 1],
                            buf[e + o + 2],
                            buf[e + o + 3],
                        ])
                    };
                    tls = Some(XexTlsInfo {
                        slots: u(0),
                        data_address: u(4),
                        data_size: u(8),
                        raw_size: u(12),
                    });
                }
            }
            // Static libraries: size-prefixed, 0x10-byte records.
            0x000200FF => {
                if let Some(e) = data_at(4) {
                    let total =
                        u32::from_be_bytes([buf[e], buf[e + 1], buf[e + 2], buf[e + 3]]) as usize;
                    let mut off = e + 4;
                    let end = e + total.min(buf.len().saturating_sub(e));
                    while off + 0x10 <= end {
                        let name = String::from_utf8_lossy(&buf[off..off + 8])
                            .trim_end_matches('\0')
                            .to_string();
                        let approval = match buf[off + 0xE] {
                            0 => "Unapproved",
                            1 => "Possible",
                            2 => "Approved",
                            3 => "Expired",
                            _ => "?",
                        }
                        .to_string();
                        static_libs.push(XexStaticLib {
                            name,
                            version: format!(
                                "{}.{}.{}.{}",
                                u16::from_be_bytes([buf[off + 8], buf[off + 9]]),
                                u16::from_be_bytes([buf[off + 10], buf[off + 11]]),
                                u16::from_be_bytes([buf[off + 12], buf[off + 13]]),
                                buf[off + 0xF],
                            ),
                            approval,
                        });
                        off += 0x10;
                    }
                }
            }
            // Original PE name: size-prefixed NUL-terminated string.
            0x000183FF => {
                if let Some(e) = data_at(4) {
                    let total =
                        u32::from_be_bytes([buf[e], buf[e + 1], buf[e + 2], buf[e + 3]]) as usize;
                    let str_end = (e + total).min(buf.len());
                    let mut s_end = e + 4;
                    while s_end < str_end && buf[s_end] != 0 {
                        s_end += 1;
                    }
                    pe_name = Some(String::from_utf8_lossy(&buf[e + 4..s_end]).to_string());
                }
            }
            // Image checksum + filetime: u32 checksum + u32 filetime at offset.
            0x00018002 => {
                if let Some(e) = data_at(8) {
                    image_checksum = Some(u32::from_be_bytes([
                        buf[e],
                        buf[e + 1],
                        buf[e + 2],
                        buf[e + 3],
                    ]));
                    image_filetime = Some(u32::from_be_bytes([
                        buf[e + 4],
                        buf[e + 5],
                        buf[e + 6],
                        buf[e + 7],
                    ]));
                }
            }
            // Page heap: u32 size + u32 flags at offset.
            0x00028002 => {
                if let Some(e) = data_at(8) {
                    page_heap = Some((
                        u32::from_be_bytes([buf[e], buf[e + 1], buf[e + 2], buf[e + 3]]),
                        u32::from_be_bytes([buf[e + 4], buf[e + 5], buf[e + 6], buf[e + 7]]),
                    ));
                }
            }
            // Bounding pathname: size-prefixed NUL-terminated string at offset.
            0x000080FF => {
                if let Some(e) = data_at(4) {
                    let total =
                        u32::from_be_bytes([buf[e], buf[e + 1], buf[e + 2], buf[e + 3]]) as usize;
                    let str_end = (e + 4 + total).min(buf.len());
                    let mut s_end = e + 4;
                    while s_end < str_end && buf[s_end] != 0 {
                        s_end += 1;
                    }
                    bounding_path = Some(String::from_utf8_lossy(&buf[e + 4..s_end]).to_string());
                }
            }
            // Bounding device ID: 20 bytes at offset.
            0x00008105 => {
                if let Some(e) = data_at(20) {
                    bounding_device_id = Some(
                        buf[e..e + 20]
                            .iter()
                            .map(|b| format!("{b:02X}"))
                            .collect::<Vec<_>>()
                            .join(""),
                    );
                }
            }
            // Restrict console IDs: infoSize + one or more 5-byte IDs at offset.
            0x000042FF => {
                if let Some(e) = data_at(4) {
                    let total =
                        u32::from_be_bytes([buf[e], buf[e + 1], buf[e + 2], buf[e + 3]]) as usize;
                    let mut off = e + 4;
                    let end = (e + total).min(buf.len());
                    while off + 5 <= end {
                        restrict_console_ids.push(
                            buf[off..off + 5]
                                .iter()
                                .map(|b| format!("{b:02X}"))
                                .collect::<Vec<_>>()
                                .join(""),
                        );
                        off += 5;
                    }
                }
            }
            // Restrict KV privs: u64 mask + u64 val at offset.
            0x00004004 => {
                if let Some(e) = data_at(16) {
                    restrict_kv_privs = Some((
                        u64::from_be_bytes([
                            buf[e],
                            buf[e + 1],
                            buf[e + 2],
                            buf[e + 3],
                            buf[e + 4],
                            buf[e + 5],
                            buf[e + 6],
                            buf[e + 7],
                        ]),
                        u64::from_be_bytes([
                            buf[e + 8],
                            buf[e + 9],
                            buf[e + 10],
                            buf[e + 11],
                            buf[e + 12],
                            buf[e + 13],
                            buf[e + 14],
                            buf[e + 15],
                        ]),
                    ));
                }
            }
            // Restrict dates: u64 start + u64 end at offset.
            0x00004104 => {
                if let Some(e) = data_at(16) {
                    restrict_dates = Some((
                        u64::from_be_bytes([
                            buf[e],
                            buf[e + 1],
                            buf[e + 2],
                            buf[e + 3],
                            buf[e + 4],
                            buf[e + 5],
                            buf[e + 6],
                            buf[e + 7],
                        ]),
                        u64::from_be_bytes([
                            buf[e + 8],
                            buf[e + 9],
                            buf[e + 10],
                            buf[e + 11],
                            buf[e + 12],
                            buf[e + 13],
                            buf[e + 14],
                            buf[e + 15],
                        ]),
                    ));
                }
            }
            // Disc profile ID: 16 bytes at offset.
            0x00004304 => {
                if let Some(e) = data_at(16) {
                    disc_profile_id = Some(
                        buf[e..e + 16]
                            .iter()
                            .map(|b| format!("{b:02X}"))
                            .collect::<Vec<_>>()
                            .join(""),
                    );
                }
            }
            // Alt title IDs: infoSize + one or more u32 IDs at offset.
            0x000407FF => {
                if let Some(e) = data_at(4) {
                    let total =
                        u32::from_be_bytes([buf[e], buf[e + 1], buf[e + 2], buf[e + 3]]) as usize;
                    let mut off = e + 4;
                    let end = (e + total).min(buf.len());
                    while off + 4 <= end {
                        alt_title_ids.push(u32::from_be_bytes([
                            buf[off],
                            buf[off + 1],
                            buf[off + 2],
                            buf[off + 3],
                        ]));
                        off += 4;
                    }
                }
            }
            // Multidisc media IDs: infoSize + one or more 16-byte IDs at offset.
            0x000406FF => {
                if let Some(e) = data_at(4) {
                    let total =
                        u32::from_be_bytes([buf[e], buf[e + 1], buf[e + 2], buf[e + 3]]) as usize;
                    let mut off = e + 4;
                    let end = (e + total).min(buf.len());
                    while off + 16 <= end {
                        multidisc_media_ids.push(
                            buf[off..off + 16]
                                .iter()
                                .map(|b| format!("{b:02X}"))
                                .collect::<Vec<_>>()
                                .join(""),
                        );
                        off += 16;
                    }
                }
            }
            // Logo data: size-prefixed blob at offset.
            0x000405FF => {
                if let Some(e) = data_at(4) {
                    let total =
                        u32::from_be_bytes([buf[e], buf[e + 1], buf[e + 2], buf[e + 3]]) as usize;
                    let data_end = (e + 4 + total).min(buf.len());
                    logo_data = Some(
                        buf[e + 4..data_end]
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
    // Companion fields share the same base: image flags +0x10C, load
    // address +0x110, XGD2 media ID +0x140, allowed media +0x17C,
    // page-descriptor count +0x180 with 24-byte records at +0x184
    // (u32 info with low 4 bits section type + 28-bit page count,
    // then the 20-byte data digest).
    let sec = |off: usize, len: usize| -> Option<usize> {
        let base = u32_at(0x10)? as usize;
        let at = xex_offset.checked_add(base)?.checked_add(off)?;
        if at + len > buf.len() {
            return None;
        }
        Some(at)
    };
    let security_region = sec(0x178, 4)
        .map(|at| u32::from_be_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]));
    let image_flags = sec(0x10C, 4)
        .map(|at| u32::from_be_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]));
    let load_address = sec(0x110, 4)
        .map(|at| u32::from_be_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]));
    let xgd2_media_id = sec(0x140, 16).map(|at| {
        buf[at..at + 16]
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join("")
    });
    let allowed_media = sec(0x17C, 4)
        .map(|at| u32::from_be_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]));
    let mut page_descriptors = Vec::new();
    if let Some(at) = sec(0x180, 4) {
        let count = u32::from_be_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]) as usize;
        for i in 0..count.min(4096) {
            let base = at + 4 + i * 24;
            if base + 24 > buf.len() {
                break;
            }
            let info = u32::from_be_bytes([buf[base], buf[base + 1], buf[base + 2], buf[base + 3]]);
            page_descriptors.push(XexPageDescriptor {
                info,
                page_count: info >> 4,
                digest: buf[base + 4..base + 24]
                    .iter()
                    .map(|b| format!("{b:02X}"))
                    .collect::<Vec<_>>()
                    .join(""),
            });
        }
    }
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
        headers,
        entry_point,
        image_base,
        original_base,
        stack_size,
        heap_size,
        fs_cache_size,
        system_flags,
        system_flag_names: system_flags.map(system_flag_names).unwrap_or_default(),
        system_flags2,
        system_flag2_names: system_flags2.map(system_flag2_names).unwrap_or_default(),
        tls,
        static_libs,
        pe_name,
        image_flags,
        image_flag_names: image_flags.map(image_flag_names).unwrap_or_default(),
        allowed_media,
        allowed_media_names: allowed_media.map(allowed_media_names).unwrap_or_default(),
        xgd2_media_id,
        load_address,
        page_descriptors,
        savegame_id,
        bounding_path,
        bounding_device_id,
        restrict_console_ids,
        restrict_kv_privs,
        restrict_dates,
        disc_profile_id,
        alt_title_ids,
        multidisc_media_ids,
        image_checksum,
        image_filetime,
        page_heap,
        logo_data,
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
