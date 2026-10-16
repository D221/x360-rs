//! Report data types produced by the parsers.

use serde::{Deserialize, Serialize};

use crate::iso::{IsoInfo, XbeInfo};
use crate::stfs::VerifyReport;
use crate::xex::{XexExecInfo, XexExtendedInfo};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryEntry {
    pub filename: String,
    pub path: String,
    pub offset: u64,
    pub size: u64,
    pub is_dir: bool,
    pub start_block: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuInfo {
    pub file_path: String,
    pub file_size: u64,
    pub is_live: bool,
    pub is_title_update: bool,
    pub container_magic: String,
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
    pub verify: Option<VerifyReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XexFileInfo {
    pub file_path: String,
    pub file_size: u64,
    pub exec: XexExecInfo,
    pub extended: Option<XexExtendedInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XbeFileInfo {
    pub file_path: String,
    pub file_size: u64,
    pub info: XbeInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XdbfFileInfo {
    pub file_path: String,
    pub file_size: u64,
    pub magic: String,
    pub version: u32,
    pub variant: String,
    pub num_entries: u32,
    pub title_name: Option<String>,
    pub entries: Vec<XdbfEntryInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XdbfEntryInfo {
    pub identifier: String,
    pub namespace: Option<String>,
    pub resource: String,
    pub offset: u64,
    pub size: u64,
    pub entry_type: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameDirInfo {
    pub dir_path: String,
    pub files: u64,
    pub dirs: u64,
    pub total_bytes: u64,
    pub executable: Option<String>,
    pub xex_exec: Option<XexExecInfo>,
    pub xbe_info: Option<XbeInfo>,
}

/// Top-level report. Internally tagged, so JSON gains a `"type"` field:
/// `stfs`, `xiso`, `xex`, `xbe`, `xdbf` or `gamedir`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Report {
    Stfs(TuInfo),
    Xiso(IsoInfo),
    Xex(XexFileInfo),
    Xbe(XbeFileInfo),
    Xdbf(XdbfFileInfo),
    GameDir(GameDirInfo),
}
