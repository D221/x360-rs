//! Title-ID classification rules, mirroring Xenia's
//! `xenia-canary/src/xenia/kernel/title_id_utils.h` (which carries
//! `static_assert`s for each rule — repeated as unit tests in `tests.rs`).

/// Dashboard title ID.
pub const DASHBOARD_ID: u32 = 0xFFFE07D1;
/// Avatar editor title ID.
pub const AVATAR_EDITOR_ID: u32 = 0x584D07D1;

/// Publisher letters are the top two bytes (e.g. `MS`, `XA`).
pub fn publisher(title_id: u32) -> (char, char) {
    (
        (title_id >> 24) as u8 as char,
        ((title_id >> 16) & 0xFF) as u8 as char,
    )
}

/// Short human-readable class for a title ID.
pub fn classify(title_id: u32) -> &'static str {
    if title_id == DASHBOARD_ID {
        return "Dashboard";
    }
    if title_id == AVATAR_EDITOR_ID {
        return "Avatar Editor";
    }
    if is_gamer_picture(title_id) {
        return "Gamer Picture";
    }
    if is_system_title(title_id) {
        return "System Title";
    }
    if is_xbla_title(title_id) {
        return "XBLA Game";
    }
    if is_app_title(title_id) {
        return "App";
    }
    if is_original_xbox_title(title_id) {
        return "Original Xbox Game";
    }
    "Xbox 360 Game"
}

pub fn is_xbox_title(title_id: u32) -> bool {
    publisher(title_id).0 == 'X'
}

pub fn is_xbla_title(title_id: u32) -> bool {
    let (a, b) = publisher(title_id);
    a == 'X' && b == 'A'
}

pub fn is_app_title(title_id: u32) -> bool {
    let (a, b) = publisher(title_id);
    (a == 'X' && b == 'H') || (a == 'X' && b == 'J')
}

fn is_valid_game_id(title_id: u32) -> bool {
    title_id >> 16 != 0xFFFE
}

pub fn is_system_title(title_id: u32) -> bool {
    if title_id == 0 {
        return true;
    }
    if !is_xbox_title(title_id) {
        return !is_valid_game_id(title_id);
    }
    if is_xbla_title(title_id) {
        return !is_valid_game_id(title_id);
    }
    true
}

pub fn is_original_xbox_title(title_id: u32) -> bool {
    if !is_valid_game_id(title_id) {
        return true;
    }
    if title_id >> 24 == 0xFF {
        return false;
    }
    (title_id & 0x7FFF) < 0x7D0
}

fn is_gamer_picture(title_id: u32) -> bool {
    title_id == 0xFFFE0854
        || title_id & 0xFFFF0000 == 0x20000
        || title_id & 0xFFFF0000 == 0x10000
        || title_id == 0xFFFE0700
        || title_id & 0xFFFF0000 == 0
}
