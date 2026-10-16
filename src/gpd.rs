//! GPD (game profile data) record decoders on top of [`crate::xdbf`].
//!
//! Layouts mirror the Free60 GPD doc
//! (`../../wiki/docs/System-Software/Formats/GPD.md`): achievement,
//! title, setting, string, image and sync records. Namespaces follow the
//! doc table (`1=Achievement, 2=Image, 3=Setting, 4=Title, 5=String,
//! 6=AvatarAward`). Only meaningful for the GPD XDBF variant.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Seek;
use std::path::Path;

use crate::xdbf::XdbfInfo;

/// Decoded GPD records (only the entry kinds with documented layouts).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GpdRecord {
    Achievement(GpdAchievement),
    Title(GpdTitle),
    Setting(GpdSetting),
    SyncList(Vec<GpdSyncItem>),
    SyncData { next: u64, last: u64, time: String },
}

/// One achievement entry (namespace 1): `u32 size(0x1C)` + fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpdAchievement {
    pub achievement_id: u32,
    pub image_id: u32,
    pub gamerscore: i32,
    pub kind: String,
    pub earned: bool,
    pub secret: bool,
    pub online: bool,
    pub unlocked: Option<String>,
    pub name: String,
    pub unlocked_desc: String,
    pub locked_desc: String,
}

/// Title entry (namespace 4, only in `FFFE07D1.gpd`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpdTitle {
    pub title_id: String,
    pub achievements: u32,
    pub unlocked: u32,
    pub gamerscore_total: u32,
    pub gamerscore_unlocked: u32,
    pub unlocked_online: bool,
    pub last_played: Option<String>,
    pub title_name: String,
}

/// Setting entry (namespace 3): typed payload dispatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpdSetting {
    pub setting_id: String,
    pub setting_name: String,
    pub data_type: String,
    pub value: String,
}

/// Human-readable name for a known GPD setting ID
/// (Free60 `Formats/GPD.md` entry-ID table). Unknown IDs fall back to hex.
pub fn setting_name(id: u32) -> &'static str {
    match id {
        0x8000 => "Title Information",
        0x8007 => "Avatar Image",
        0x10040002 => "Gamer Yaxis Inversion",
        0x10040003 => "Option Controller Vibration",
        0x10040004 => "Gamercard Zone",
        0x10040005 => "Gamercard Region",
        0x10040006 => "Gamercard Cred",
        0x10040012 => "Option Voice Muted",
        0x10040013 => "Option Voice Thru Speakers",
        0x10040014 => "Option Voice Volume",
        0x10040018 => "Gamercard Titles Played",
        0x10040019 => "Gamercard Achievements Earned",
        0x10040021 => "Gamer Difficulty",
        0x10040024 => "Gamer Control Sensitivity",
        0x10040029 => "Gamer Preferred Color First",
        0x10040030 => "Gamer Preferred Color Second",
        0x10040034 => "Gamer Action Auto Aim",
        0x10040035 => "Gamer Action Auto Center",
        0x10040036 => "Gamer Action Movement Control",
        0x10040038 => "Gamer Race Transmission",
        0x10040039 => "Gamer Race Camera Location",
        0x10040040 => "Gamer Race Brake Control",
        0x10040041 => "Gamer Race Accelerator Control",
        0x10040056 => "Gamercard Title Cred Earned",
        0x10040057 => "Gamercard Title Achievements Earned",
        0x10040063 => "Option Voice Thru Speakers Raw",
        0x402C0011 => "Gamercard Motto",
        0x4064000F => "Gamercard Picture Key",
        0x41040040 => "Gamer Name",
        0x40520041 => "Gamer Location",
        0x50040011 => "Gamercard Rep",
        0x63E80044 => "Avatar Information",
        0x63E80068 => "Avatar Metadata",
        0x63E83FFD => "Title Specific 3",
        0x63E83FFE => "Title Specific 2",
        0x63E83FFF => "Title Specific 1",
        _ => "",
    }
}

/// One sync-list item: entry ID + sync ID.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpdSyncItem {
    pub entry_id: String,
    pub sync_id: String,
}

/// Decode UTF-16BE NUL-terminated strings from a blob.
fn utf16_strings(blob: &[u8]) -> Vec<String> {
    let (chunks, _) = blob.as_chunks::<2>();
    let units: Vec<u16> = chunks
        .iter()
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    units
        .split(|&c| c == 0)
        .filter(|s| !s.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

/// Windows FILETIME to `YYYY-MM-DD HH:MM:SS` (days-from-civil, no deps).
pub(crate) fn filetime_string(ft: u64) -> Option<String> {
    if ft == 0 || ft == u64::MAX {
        return None;
    }
    let unix = ft.checked_sub(116444736000000000)? / 10_000_000;
    let days = (unix / 86400) as i64;
    let secs = (unix % 86400) as i64;
    // Howard Hinnant days-from-civil inverse.
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    Some(format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        y + i64::from(m <= 2),
        m,
        d,
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    ))
}

fn achievement_kind(flags: u32) -> &'static str {
    match flags & 7 {
        1 => "Completion",
        2 => "Leveling",
        3 => "Unlock",
        4 => "Event",
        5 => "Tournament",
        6 => "Checkpoint",
        7 => "Other",
        _ => "?",
    }
}

fn be_u32(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(o..o + 4)?.try_into().ok()?))
}
fn be_i32(b: &[u8], o: usize) -> Option<i32> {
    Some(i32::from_be_bytes(b.get(o..o + 4)?.try_into().ok()?))
}
fn be_u64(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

fn decode_achievement(data: &[u8]) -> Option<GpdAchievement> {
    if data.len() < 0x1C || be_u32(data, 0)? != 0x1C {
        return None;
    }
    let flags = be_u32(data, 0x10)?;
    let strings = utf16_strings(data.get(0x1C..)?);
    Some(GpdAchievement {
        achievement_id: be_u32(data, 0x4)?,
        image_id: be_u32(data, 0x8)?,
        gamerscore: be_i32(data, 0xC)?,
        kind: achievement_kind(flags).to_string(),
        earned: flags & 0x20000 != 0,
        secret: flags & 0x8 == 0,
        online: flags & 0x10000 != 0,
        unlocked: filetime_string(be_u64(data, 0x14)?),
        name: strings.first().cloned().unwrap_or_default(),
        unlocked_desc: strings.get(1).cloned().unwrap_or_default(),
        locked_desc: strings.get(2).cloned().unwrap_or_default(),
    })
}

fn decode_title(data: &[u8]) -> Option<GpdTitle> {
    if data.len() < 0x28 {
        return None;
    }
    let name_blob = data.get(0x28..)?;
    let name = utf16_strings(name_blob)
        .first()
        .cloned()
        .unwrap_or_default();
    Some(GpdTitle {
        title_id: format!("{:08X}", be_u32(data, 0)?),
        achievements: be_u32(data, 0x4)?,
        unlocked: be_u32(data, 0x8)?,
        gamerscore_total: be_u32(data, 0xC)?,
        gamerscore_unlocked: be_u32(data, 0x10)?,
        unlocked_online: data.get(0x15).is_some_and(|&b| b != 0),
        last_played: filetime_string(be_u64(data, 0x20)?),
        title_name: name,
    })
}

fn decode_setting(setting_id: u32, data: &[u8]) -> Option<GpdSetting> {
    if data.len() < 16 {
        return None;
    }
    let ty = data[8];
    let payload = data.get(16..)?;
    let (data_type, value) = match ty {
        0 => ("Context".to_string(), format!("{}", be_u32(payload, 0)?)),
        1 => ("Int32".to_string(), format!("{}", be_i32(payload, 0)?)),
        2 => (
            "Int64".to_string(),
            format!("{}", i64::from_be_bytes(payload.get(..8)?.try_into().ok()?)),
        ),
        3 => (
            "Double".to_string(),
            format!("{}", f64::from_be_bytes(payload.get(..8)?.try_into().ok()?)),
        ),
        4 => {
            let len = be_i32(payload, 0).unwrap_or(0).max(0) as usize;
            let end = (4 + len).min(payload.len());
            let (chunks, _) = payload[4..end].as_chunks::<2>();
            (
                "String".to_string(),
                String::from_utf16_lossy(
                    &chunks
                        .iter()
                        .map(|c| u16::from_be_bytes([c[0], c[1]]))
                        .collect::<Vec<_>>(),
                )
                .trim_end_matches('\0')
                .to_string(),
            )
        }
        5 => (
            "Float".to_string(),
            format!("{}", f32::from_be_bytes(payload.get(..4)?.try_into().ok()?)),
        ),
        6 => (
            "Binary".to_string(),
            format!("{} bytes", payload.len().saturating_sub(4)),
        ),
        7 => (
            "DateTime".to_string(),
            filetime_string(be_u64(payload, 0)?).unwrap_or_default(),
        ),
        0xFF => ("Null".to_string(), String::new()),
        t => (format!("Unknown({t})"), String::new()),
    };
    let name = setting_name(setting_id);
    Some(GpdSetting {
        setting_id: format!("{setting_id:08X}"),
        setting_name: if name.is_empty() {
            format!("{setting_id:08X}")
        } else {
            name.to_string()
        },
        data_type,
        value,
    })
}

/// Decode all documented records of a GPD-variant XDBF. Returns empty for
/// the XEX-embedded variant (different entry vocabulary).
pub fn decode_gpd_records(info: &XdbfInfo) -> Result<Vec<GpdRecord>> {
    if info.variant != "gpd" {
        return Ok(Vec::new());
    }
    let file = File::open(Path::new(&info.file_path))
        .with_context(|| format!("open {} for GPD decode", info.file_path))?;
    let mut reader = std::io::BufReader::new(file);
    use std::io::SeekFrom;
    let mut out = Vec::new();
    for e in &info.entries {
        let ns = e.namespace.unwrap_or(0);
        reader.seek(SeekFrom::Start(info.data_start + e.offset as u64))?;
        let mut data = vec![0u8; e.size as usize];
        use std::io::Read as _;
        reader.read_exact(&mut data)?;
        match ns {
            1 => {
                if let Some(a) = decode_achievement(&data) {
                    out.push(GpdRecord::Achievement(a));
                }
            }
            3 => {
                // Setting ID rides in the entry identifier.
                if let Some(s) = decode_setting(e.identifier as u32, &data) {
                    out.push(GpdRecord::Setting(s));
                }
            }
            4 => {
                if let Some(t) = decode_title(&data) {
                    out.push(GpdRecord::Title(t));
                }
            }
            _ => {}
        }
        // Sync list / sync data have reserved identifiers.
        if e.identifier == 0x100000000 {
            let n = data.len() / 16;
            if n >= 1 {
                let items = (0..n - 1)
                    .map(|i| GpdSyncItem {
                        entry_id: format!("{:016X}", be_u64(&data, i * 16).unwrap_or(0)),
                        sync_id: format!("{:016X}", be_u64(&data, i * 16 + 8).unwrap_or(0)),
                    })
                    .collect();
                out.push(GpdRecord::SyncList(items));
            }
        }
        if e.identifier == 0x200000000 && data.len() >= 24 {
            out.push(GpdRecord::SyncData {
                next: be_u64(&data, 0).unwrap_or(0),
                last: be_u64(&data, 8).unwrap_or(0),
                time: filetime_string(be_u64(&data, 16).unwrap_or(0)).unwrap_or_default(),
            });
        }
    }
    Ok(out)
}

/// Decoded SPA (Statistics, Presence, Achievement) records.
/// SPA files are XDBF variant "xex" with entries identified by 4-char
/// magic codes (XTHD, XACH, XITB, XSTR, etc.) instead of namespaces.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SpaRecord {
    Xthd(SpaXthd),
    Xach(SpaXach),
    Xitb(SpaXitb),
    Xstr(SpaXstr),
}

/// XTHD — Title header (SPA title type/version).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaXthd {
    pub magic: String,
    pub version: u32,
    pub structure_size: u32,
    pub title_id: String,
    pub title_type: u32,
    pub title_type_name: String,
    pub version_major: u16,
    pub version_minor: u16,
    pub version_build: u16,
    pub version_revision: u16,
}

/// XACH — Achievement catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaXach {
    pub magic: String,
    pub version: u32,
    pub structure_size: u32,
    pub achievement_count: u16,
}

/// XITB — Image inventory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaXitb {
    pub magic: String,
    pub version: u32,
    pub structure_size: u32,
    pub image_count: u32,
}

/// XSTR — String table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaXstr {
    pub magic: String,
    pub version: u32,
    pub structure_size: u32,
    pub string_count: u16,
}

/// Decode SPA records from an XDBF variant "xex" file.
/// Entries are identified by 4-char magic codes (XTHD, XACH, XITB, XSTR).
pub fn decode_spa_records(info: &XdbfInfo) -> Vec<SpaRecord> {
    if info.variant != "xex" {
        return Vec::new();
    }
    let file = match File::open(Path::new(&info.file_path)) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let mut reader = std::io::BufReader::new(file);
    use std::io::{Read as _, Seek as _};
    let mut out = Vec::new();
    for e in &info.entries {
        if reader
            .seek(std::io::SeekFrom::Start(info.data_start + e.offset as u64))
            .is_err()
        {
            continue;
        }
        let mut data = vec![0u8; e.size as usize];
        if reader.read_exact(&mut data).is_err() {
            continue;
        }
        if data.len() < 4 {
            continue;
        }
        let magic = &data[0..4];
        match magic {
            b"XTHD" => {
                if let Some(x) = decode_xthd(&data) {
                    out.push(SpaRecord::Xthd(x));
                }
            }
            b"XACH" => {
                if let Some(x) = decode_xach(&data) {
                    out.push(SpaRecord::Xach(x));
                }
            }
            b"XITB" => {
                if let Some(x) = decode_xitb(&data) {
                    out.push(SpaRecord::Xitb(x));
                }
            }
            b"XSTR" => {
                if let Some(x) = decode_xstr(&data) {
                    out.push(SpaRecord::Xstr(x));
                }
            }
            _ => {}
        }
    }
    out
}

fn decode_xthd(data: &[u8]) -> Option<SpaXthd> {
    if data.len() < 28 {
        return None;
    }
    let title_type = be_u32(data, 0x10)?;
    let title_type_name = match title_type {
        0 => "System",
        1 => "Full",
        2 => "Demo",
        3 => "Download",
        _ => "Unknown",
    }
    .to_string();
    Some(SpaXthd {
        magic: "XTHD".to_string(),
        version: be_u32(data, 0x04)?,
        structure_size: be_u32(data, 0x08)?,
        title_id: format!("{:08X}", be_u32(data, 0x0C)?),
        title_type,
        title_type_name,
        version_major: be_u16(data, 0x14)?,
        version_minor: be_u16(data, 0x16)?,
        version_build: be_u16(data, 0x18)?,
        version_revision: be_u16(data, 0x1A)?,
    })
}

fn decode_xach(data: &[u8]) -> Option<SpaXach> {
    if data.len() < 14 {
        return None;
    }
    Some(SpaXach {
        magic: "XACH".to_string(),
        version: be_u32(data, 0x04)?,
        structure_size: be_u32(data, 0x08)?,
        achievement_count: be_u16(data, 0x0C)?,
    })
}

fn decode_xitb(data: &[u8]) -> Option<SpaXitb> {
    if data.len() < 16 {
        return None;
    }
    Some(SpaXitb {
        magic: "XITB".to_string(),
        version: be_u32(data, 0x04)?,
        structure_size: be_u32(data, 0x08)?,
        image_count: be_u32(data, 0x0C)?,
    })
}

fn decode_xstr(data: &[u8]) -> Option<SpaXstr> {
    if data.len() < 14 {
        return None;
    }
    Some(SpaXstr {
        magic: "XSTR".to_string(),
        version: be_u32(data, 0x04)?,
        structure_size: be_u32(data, 0x08)?,
        string_count: be_u16(data, 0x0C)?,
    })
}

fn be_u16(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(o..o + 2)?.try_into().ok()?))
}
