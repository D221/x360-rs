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

/// Human-readable content-type name.
pub fn content_type_name(content_type: u32) -> &'static str {
    match content_type {
        CONTENT_TITLE_UPDATE => "Title Update",
        CONTENT_MARKETPLACE => "Marketplace Content (DLC)",
        CONTENT_XBLA => "Xbox Live Arcade (XBLA)",
        CONTENT_AVATAR => "Avatar Item",
        CONTENT_THEME => "Theme",
        CONTENT_GOD => "Games on Demand (GoD)",
        _ => "Unknown",
    }
}
