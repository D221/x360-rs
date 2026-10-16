//! Container magics, content types, and the top-level error type.

use thiserror::Error;

#[derive(Error, Debug)]
pub enum TuError {
    #[error("Not a valid STFS container: expected 'LIVE', 'CON ' or 'PIRS' magic, got {0:08X}")]
    InvalidMagic(u32),
    #[error("Unrecognized file format (not STFS, XEX, XBE or XISO)")]
    UnknownFormat,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Accepted STFS container magics (LE u32): "LIVE", "CON ", "PIRS".
/// Same header/table layout regardless of magic.
pub const MAGIC_LIVE: u32 = 0x4556494C;
pub const MAGIC_CON: u32 = 0x204E4F43;
pub const MAGIC_PIRS: u32 = 0x53524950;

/// Display name for a container magic.
pub fn magic_name(magic: u32) -> &'static str {
    match magic {
        MAGIC_LIVE => "LIVE",
        MAGIC_CON => "CON",
        MAGIC_PIRS => "PIRS",
        _ => "Unknown",
    }
}

/// STFS content types (BE u32 at 0x0344).
pub const CONTENT_TITLE_UPDATE: u32 = 0x000B0000;
pub const CONTENT_MARKETPLACE: u32 = 0x00000002;
pub const CONTENT_XBLA: u32 = 0x000D0000;
pub const CONTENT_AVATAR: u32 = 0x00009000;
pub const CONTENT_THEME: u32 = 0x00030000;
pub const CONTENT_GOD: u32 = 0x00007000;

/// Human-readable content-type name. The original six names are kept
/// verbatim; the rest follow Xenia's `XContentTypeMap`
/// (`xenia-canary/src/xenia/xbox.h`).
pub fn content_type_name(content_type: u32) -> &'static str {
    match content_type {
        CONTENT_TITLE_UPDATE => "Title Update",
        CONTENT_MARKETPLACE => "Marketplace Content (DLC)",
        CONTENT_XBLA => "Xbox Live Arcade (XBLA)",
        CONTENT_AVATAR => "Avatar Item",
        CONTENT_THEME => "Theme",
        CONTENT_GOD => "Games on Demand (GoD)",
        0x00000001 => "Saved Game",
        0x00000003 => "Publisher",
        0x00001000 => "IPTV DVR",
        0x00002000 => "IPTV Pause Buffer",
        0x00003000 => "XNA Community",
        0x00004000 => "Installed Game",
        0x00005000 => "Xbox Title",
        0x00006000 => "Social Title",
        0x00008000 => "SU Storage Pack",
        0x00010000 => "Profile",
        0x00020000 => "Gamer Picture",
        0x00040000 => "Cache File",
        0x00050000 => "Storage Download",
        0x00060000 => "Xbox Saved Game",
        0x00070000 => "Xbox Download",
        0x00080000 => "Game Demo",
        0x00090000 => "Video",
        0x000A0000 => "Game Title",
        0x000C0000 => "Game Trailer",
        0x000E0000 => "XNA",
        0x000F0000 => "License Store",
        0x00100000 => "Movie",
        0x00200000 => "TV",
        0x00300000 => "Music Video",
        0x00400000 => "Game Video",
        0x00500000 => "Podcast Video",
        0x00600000 => "Viral Video",
        0x02000000 => "Community Game",
        0xFFFFFFFF => "All",
        _ => "Unknown",
    }
}

/// Content types that can boot as a game (Xenia `isBootableType`,
/// `xenia-canary/src/xenia/app/content_list_dialog.cc`).
pub fn content_type_is_bootable(content_type: u32) -> bool {
    matches!(
        content_type,
        0x000D0000 // Arcade Title
            | 0x00080000 // Game Demo
            | 0x00004000 // Installed Game
            | 0x00007000 // Xbox 360 Title
    )
}
