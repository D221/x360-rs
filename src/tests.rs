use std::fs::File;
use std::io::{BufReader, Seek, SeekFrom};

use crate::constants::{content_type_name, magic_name, TuError, MAGIC_CON, MAGIC_LIVE, MAGIC_PIRS};
use crate::parse::{parse_file, parse_game_dir, parse_iso};
use crate::regions::{collapse_regions, decode_regions};
use crate::report::Report;
use crate::stfs::{entry_full_path, read_stfs_file, verify_stfs, StfsLayout};
use crate::tu_parser::{decode_slot, format_size, TuParser};
use crate::xex::parse_xex_exec_info;

#[test]
fn region_free_mask() {
    assert_eq!(decode_regions(0xFFFFFFFF), vec!["Region Free".to_string()]);
}

#[test]
fn region_bits_match_checker() {
    // Masks taken from Title_Update_List.txt samples.
    assert_eq!(
        decode_regions(0x00FF00FF),
        vec![
            "PAL(A)".to_string(),
            "PAL(E)".to_string(),
            "NTSC/U".to_string()
        ]
    );
    assert_eq!(
        decode_regions(0x0000FD00),
        vec!["NTSC/J(J)".to_string(), "NTSC/J(0)".to_string()]
    );
    assert_eq!(decode_regions(0x000000FF), vec!["NTSC/U".to_string()]);
    assert_eq!(
        decode_regions(0x00FF0000),
        vec!["PAL(A)".to_string(), "PAL(E)".to_string()]
    );
}

#[test]
fn regions_collapse_pal() {
    let detailed = decode_regions(0x00FF00FF);
    assert_eq!(
        collapse_regions(&detailed),
        vec!["PAL".to_string(), "NTSC/U".to_string()]
    );
}

#[test]
fn sizes_format() {
    assert_eq!(format_size(0), "0 B");
    assert_eq!(format_size(312), "312 B");
    assert_eq!(format_size(10712), "10712 (10.5 KB)");
    assert_eq!(format_size(4128768), "4128768 (3.9 MB)");
}

#[test]
fn entry_size_and_dir_flag() {
    // Synthetic 64-byte STFS entry: name + flags + BE size at +0x34.
    let mut entry = vec![0u8; 64];
    entry[..12].copy_from_slice(b"default.xexp");
    entry[0x28] = 0x4C;
    entry[0x34..0x38].copy_from_slice(&1134080u32.to_be_bytes());
    let parsed = decode_slot(&entry, 0, 0xC000).expect("entry parses");
    assert_eq!(parsed.name, "default.xexp");
    assert_eq!(parsed.size, 1134080);
    assert!(!parsed.is_dir);

    entry[0x28] = 0x80 | 12;
    entry[0x34..0x38].copy_from_slice(&0u32.to_be_bytes());
    let parsed = decode_slot(&entry, 1, 0xC080).expect("dir parses");
    assert!(parsed.is_dir);
    assert_eq!(parsed.size, 0);
}

#[test]
fn container_magics() {
    assert_eq!(magic_name(MAGIC_LIVE), "LIVE");
    assert_eq!(magic_name(MAGIC_CON), "CON");
    assert_eq!(magic_name(MAGIC_PIRS), "PIRS");
    assert_eq!(magic_name(0xDEADBEEF), "Unknown");
    assert_eq!(MAGIC_LIVE, 0x4556494C);
    assert_eq!(MAGIC_CON, u32::from_le_bytes(*b"CON "));
    assert_eq!(MAGIC_PIRS, u32::from_le_bytes(*b"PIRS"));
}

#[test]
fn content_kinds() {
    assert_eq!(content_type_name(0x000B0000), "Title Update");
    assert_eq!(content_type_name(0x00000002), "Marketplace Content (DLC)");
    assert_eq!(content_type_name(0x000D0000), "Xbox Live Arcade (XBLA)");
    assert_eq!(content_type_name(0x00009000), "Avatar Item");
    assert_eq!(content_type_name(0x00030000), "Theme");
    assert_eq!(content_type_name(0xDEADBEEF), "Unknown");
}

fn write_synthetic_dlc(path: &std::path::Path) {
    // Minimal DLC-like LIVE container: header fields + 1 file-table
    // entry, no TUPD signature area, no XEX.
    let mut buf = vec![0u8; 0xC0C0];
    buf[0..4].copy_from_slice(b"LIVE");
    buf[0x340..0x344].copy_from_slice(&0xAD0Eu32.to_be_bytes());
    buf[0x344..0x348].copy_from_slice(&0x00000002u32.to_be_bytes());
    buf[0x354..0x358].copy_from_slice(&0u32.to_be_bytes());
    buf[0x360..0x364].copy_from_slice(&0x434307D2u32.to_be_bytes());
    buf[0x379] = 0x24;
    buf[0x37C..0x37E].copy_from_slice(&1u16.to_le_bytes());
    buf[0x395..0x399].copy_from_slice(&1u32.to_be_bytes());
    let title: Vec<u16> = "Miami Nights Key".encode_utf16().collect();
    for (i, c) in title.iter().enumerate() {
        buf[0x411 + i * 2..0x411 + i * 2 + 2].copy_from_slice(&c.to_be_bytes());
    }
    buf[0xC000..0xC00B].copy_from_slice(b"DSDLKey.bin");
    buf[0xC000 + 0x28] = 0x40 | 11; // consecutive + namelen
    buf[0xC000 + 0x34..0xC000 + 0x38].copy_from_slice(&21u32.to_be_bytes());
    std::fs::write(path, &buf).unwrap();
}

#[test]
fn xex_exec_info_parses() {
    // Synthetic XEX2 header with one ExecutionId field (0x00040006).
    let mut buf = vec![0u8; 0x100];
    buf[0..4].copy_from_slice(b"XEX2");
    buf[20..24].copy_from_slice(&1u32.to_be_bytes());
    buf[24..28].copy_from_slice(&0x00040006u32.to_be_bytes());
    buf[28..32].copy_from_slice(&0x40u32.to_be_bytes());
    buf[0x40..0x44].copy_from_slice(&0x36D691EEu32.to_be_bytes());
    buf[0x44..0x48].copy_from_slice(&0x00000103u32.to_be_bytes());
    buf[0x48..0x4C].copy_from_slice(&0x00000103u32.to_be_bytes());
    buf[0x4C..0x50].copy_from_slice(&0x58410886u32.to_be_bytes());
    let info = parse_xex_exec_info(&buf, 0).expect("exec parses");
    assert_eq!(info.media_id, "36D691EE");
    assert_eq!(info.version, "00000103");
    assert_eq!(info.title_id, "58410886");

    // TU patch XEX shape (no ExecutionId) yields None.
    let mut buf = vec![0u8; 0x100];
    buf[0..4].copy_from_slice(b"XEX2");
    buf[20..24].copy_from_slice(&2u32.to_be_bytes());
    assert!(parse_xex_exec_info(&buf, 0).is_none());
}

#[test]
fn xex_extended_info_parses() {
    use crate::xex::{module_flag_names, parse_xex_extended_info};
    // Synthetic XEX2 header: flags + ratings + LAN key + security region.
    let mut buf = vec![0u8; 0x500];
    buf[0..4].copy_from_slice(b"XEX2");
    buf[4..8].copy_from_slice(&0x81u32.to_be_bytes()); // TitleModule + UserMode
    buf[0x10..0x14].copy_from_slice(&0x300u32.to_be_bytes()); // secinfo at 0x300
    buf[20..24].copy_from_slice(&2u32.to_be_bytes());
    buf[24..28].copy_from_slice(&0x00040310u32.to_be_bytes());
    buf[28..32].copy_from_slice(&0x100u32.to_be_bytes());
    buf[32..36].copy_from_slice(&0x00040404u32.to_be_bytes());
    buf[36..40].copy_from_slice(&0x120u32.to_be_bytes());
    // Full rating set: T / 18+ / 18+ / 18+ / 18+ / Z / 18+ / MA15+ / MA15+ /
    // 18+ / 18+ / 18+.
    for (i, b) in [0x06u8, 14, 14, 14, 14, 8, 8, 6, 6, 6, 8, 14]
        .iter()
        .enumerate()
    {
        buf[0x100 + i] = *b;
    }
    for i in 0..16 {
        buf[0x120 + i] = 0xA0 + i as u8;
    }
    buf[0x300 + 0x178..0x300 + 0x17C].copy_from_slice(&0x00FF00FFu32.to_be_bytes());
    let info = parse_xex_extended_info(&buf, 0).expect("extended parses");
    assert_eq!(info.module_flags, 0x81);
    assert_eq!(module_flag_names(0x81), vec!["TitleModule", "UserMode"]);
    assert_eq!(info.ratings.as_deref(), Some("060E0E0E0E0808060606080E"));
    assert_eq!(
        info.rating_labels,
        [
            "ESRB=T",
            "PEGI=18+",
            "PEGI-FI=18+",
            "PEGI-PT=18+",
            "BBFC=18+",
            "CERO=Z",
            "USK=18+",
            "OFLC-AU=MA15+",
            "OFLC-NZ=MA15+",
            "KMRB=18+",
            "BRASIL=18+",
            "FPB=18+",
        ]
    );
    assert_eq!(info.lan_key.unwrap().len(), 32);
    assert_eq!(info.security_region, Some(0x00FF00FF));
    assert_eq!(info.region_names, ["NTSC/U", "PAL", "PAL(AU/NZ)"]);
    assert_eq!(
        crate::xex::security_region_names(0x0000_FF00),
        ["NTSC/J", "NTSC/J(J)", "NTSC/J(C)"]
    );
    assert_eq!(
        crate::xex::security_region_names(0xFFFF_FFFF),
        vec!["Region Free"]
    );
    // Missing entries yield None fields, not failure.
    let mut bare = vec![0u8; 0x100];
    bare[0..4].copy_from_slice(b"XEX2");
    bare[20..24].copy_from_slice(&0u32.to_be_bytes());
    let bare_info = parse_xex_extended_info(&bare, 0).expect("bare parses");
    assert!(bare_info.ratings.is_none());
    assert!(bare_info.lan_key.is_none());
    assert!(bare_info.security_region.is_none());
    // Not an XEX at all.
    assert!(parse_xex_extended_info(&[0u8; 8], 0).is_none());
}

#[test]
fn dlc_without_xex_parses() {
    let path = std::env::temp_dir().join(format!("tu_parser_dlc_{}.bin", std::process::id()));
    write_synthetic_dlc(&path);
    let mut parser = TuParser::open(&path).expect("synthetic dlc opens");
    let info = parser.parse().expect("synthetic dlc parses");
    std::fs::remove_file(&path).ok();

    assert!(!info.is_title_update);
    assert_eq!(info.content_kind, "Marketplace Content (DLC)");
    assert_eq!(info.media_id, "00000000");
    assert_eq!(info.base_title_id, "434307D2");
    assert_eq!(info.display_title, "Miami Nights Key");
    assert_eq!(info.region_mask, None);
    assert!(info.regions.is_empty());
    assert_eq!(info.directory_entries.len(), 1);
    assert_eq!(info.directory_entries[0].filename, "DSDLKey.bin");
    assert_eq!(info.directory_entries[0].size, 21);
}

/// Build a minimal XSF image: volume descriptor at sector 0x20, one
/// file entry in the root table (mirrors iso2god's unit-test layout).
fn write_synthetic_xsf(path: &std::path::Path) {
    let mut img = vec![0u8; 64 * 0x800];
    let desc = 0x20 * 0x800;
    img[desc..desc + 20].copy_from_slice(b"MICROSOFT*XBOX*MEDIA");
    img[desc + 20..desc + 24].copy_from_slice(&40u32.to_le_bytes());
    img[desc + 24..desc + 28].copy_from_slice(&0x800u32.to_le_bytes());
    let table = 40 * 0x800;
    img[table..table + 2].copy_from_slice(&0u16.to_le_bytes());
    img[table + 2..table + 4].copy_from_slice(&0u16.to_le_bytes());
    img[table + 4..table + 8].copy_from_slice(&50u32.to_le_bytes());
    img[table + 8..table + 12].copy_from_slice(&100u32.to_le_bytes());
    img[table + 12] = 0x20;
    img[table + 13] = 1;
    img[table + 14] = b'a';
    std::fs::write(path, &img).unwrap();
}

#[test]
fn xiso_synthetic_parses() {
    let path = std::env::temp_dir().join(format!("tu_parser_xsf_{}.iso", std::process::id()));
    write_synthetic_xsf(&path);
    let size = std::fs::metadata(&path).unwrap().len();
    let report = parse_file(&path).expect("synthetic xsf parses");
    std::fs::remove_file(&path).ok();
    match report {
        Report::Xiso(info) => {
            assert_eq!(info.iso_type, "XSF");
            assert_eq!(info.file_size, size);
            assert_eq!(info.entries.len(), 1);
            assert_eq!(info.entries[0].path, "a");
            assert_eq!(info.entries[0].sector, 50);
            assert_eq!(info.entries[0].size, 100);
            assert!(!info.entries[0].is_dir);
            assert_eq!(info.executable, None);
        }
        _ => panic!("expected xiso"),
    }
}

#[test]
fn xbe_synthetic_parses() {
    let path = std::env::temp_dir().join(format!("tu_parser_xbe_{}.xbe", std::process::id()));
    let mut buf = vec![0u8; 0x400];
    buf[0..4].copy_from_slice(b"XBEH");
    buf[8..12].copy_from_slice(&0x4D53084Du32.to_le_bytes());
    buf[176..180].copy_from_slice(&0x00000002u32.to_le_bytes());
    std::fs::write(&path, &buf).unwrap();
    let report = parse_file(&path).expect("synthetic xbe parses");
    std::fs::remove_file(&path).ok();
    match report {
        Report::Xbe(info) => {
            assert_eq!(info.info.title_id, "4D53084D");
            assert_eq!(info.info.version, "00000002");
        }
        _ => panic!("expected xbe"),
    }
}

#[test]
fn unknown_file_rejected() {
    let path = std::env::temp_dir().join(format!("tu_parser_unk_{}.bin", std::process::id()));
    std::fs::write(&path, b"definitely not an xbox file................").unwrap();
    let err = parse_file(&path).expect_err("unknown file rejected");
    std::fs::remove_file(&path).ok();
    assert!(matches!(
        err.downcast_ref::<TuError>(),
        Some(TuError::UnknownFormat)
    ));
}

#[test]
fn game_dir_parses_default_xex() {
    let dir = std::env::temp_dir().join(format!("tu_parser_gdir_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // Minimal XEX2 with ExecutionId.
    let mut xex = vec![0u8; 0x100];
    xex[0..4].copy_from_slice(b"XEX2");
    xex[20..24].copy_from_slice(&1u32.to_be_bytes());
    xex[24..28].copy_from_slice(&0x00040006u32.to_be_bytes());
    xex[28..32].copy_from_slice(&0x40u32.to_be_bytes());
    xex[0x40..0x44].copy_from_slice(&0x36D691EEu32.to_be_bytes());
    xex[0x44..0x48].copy_from_slice(&0x00000103u32.to_be_bytes());
    xex[0x48..0x4C].copy_from_slice(&0x00000103u32.to_be_bytes());
    xex[0x4C..0x50].copy_from_slice(&0x58410886u32.to_be_bytes());
    std::fs::write(dir.join("default.xex"), &xex).unwrap();
    std::fs::write(dir.join("readme.txt"), b"hello").unwrap();
    let info = parse_game_dir(&dir).expect("game dir parses");
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(info.files, 2);
    assert_eq!(info.total_bytes, 0x100 + 5);
    assert_eq!(info.executable.as_deref(), Some("default.xex"));
    let exec = info.xex_exec.expect("exec parsed");
    assert_eq!(exec.title_id, "58410886");
    assert!(info.xbe_info.is_none());
}

/// Build a minimal synthetic STFS image with real hashes: header,
/// one L0 table, a 2-block consecutive file plus a subdir with a
/// chained (non-consecutive) 1-block file.
fn write_synthetic_stfs(path: &std::path::Path) -> Vec<u8> {
    use sha1::{Digest, Sha1};
    let mut img = vec![0u8; 0x10000];
    img[0..4].copy_from_slice(b"LIVE");
    img[0x340..0x344].copy_from_slice(&0xAD0Eu32.to_be_bytes());
    img[0x344..0x348].copy_from_slice(&0x000B0000u32.to_be_bytes());
    img[0x379] = 0x24;
    img[0x37C..0x37E].copy_from_slice(&1u16.to_le_bytes());
    img[0x395..0x399].copy_from_slice(&4u32.to_be_bytes()); // alloc: table + 3 data
                                                            // file data blocks 1,2 (file) and 3 (chained file)
    let d1 = vec![0xAAu8; 0x1000];
    let mut d2 = vec![0u8; 0x1000];
    d2[..5].copy_from_slice(b"hello");
    let d3 = vec![0x55u8; 0x1000];
    img[0xD000..0xE000].copy_from_slice(&d1);
    img[0xE000..0xF000].copy_from_slice(&d2);
    img[0xF000..0x10000].copy_from_slice(&d3);
    // file table at 0xC000: dir "sub", file "big.bin" (start 1, 5000 B),
    // file "one.bin" in sub (start 3, chained single block)
    let mut e = |slot: usize, name: &[u8], flags: u8, start: u32, size: u32, parent: u16| {
        let o = 0xC000 + slot * 64;
        img[o..o + name.len()].copy_from_slice(name);
        img[o + 0x28] = flags | (name.len() as u8);
        img[o + 0x29..o + 0x2C].copy_from_slice(&1u32.to_le_bytes()[..3]);
        img[o + 0x2F..o + 0x32].copy_from_slice(&start.to_le_bytes()[..3]);
        img[o + 0x32..o + 0x34].copy_from_slice(&parent.to_be_bytes());
        img[o + 0x34..o + 0x38].copy_from_slice(&size.to_be_bytes());
    };
    e(0, b"sub", 0x80, 0, 0, 0xFFFF);
    e(1, b"big.bin", 0x40, 1, 5000, 0xFFFF);
    e(2, b"one.bin", 0x00, 3, 100, 0);
    // L0 table at 0xB000: rec0 = table block, rec1/2/3 = data
    let h = |img: &[u8], r: std::ops::Range<usize>| Sha1::digest(&img[r]).to_vec();
    let h0 = h(&img, 0xC000..0xD000);
    let h1 = h(&img, 0xD000..0xE000);
    let h2 = h(&img, 0xE000..0xF000);
    let h3 = h(&img, 0xF000..0x10000);
    let mut t = |slot: usize, hash: &[u8], next: u32| {
        let o = 0xB000 + slot * 24;
        img[o..o + 20].copy_from_slice(hash);
        img[o + 20] = 0x80;
        img[o + 21..o + 24].copy_from_slice(&next.to_be_bytes()[1..]);
    };
    t(0, &h0, 0xFFFFFF);
    t(1, &h1, 2);
    t(2, &h2, 0xFFFFFF);
    t(3, &h3, 0xFFFFFF);
    // top hash first (it lives inside the header-sha1 range), then header
    let top = Sha1::digest(&img[0xB000..0xC000]).to_vec();
    img[0x381..0x395].copy_from_slice(&top);
    let hdr = Sha1::digest(&img[0x344..0xB000]).to_vec();
    img[0x32C..0x340].copy_from_slice(&hdr);
    std::fs::write(path, &img).unwrap();
    img
}

#[test]
fn stfs_verify_ok_and_extract() {
    let path = std::env::temp_dir().join(format!("tu_parser_stfs_{}.bin", std::process::id()));
    write_synthetic_stfs(&path);
    let file = File::open(&path).unwrap();
    let size = file.metadata().unwrap().len();
    let mut reader = BufReader::new(file);
    let rep = verify_stfs(&mut reader, size).expect("verify runs");
    assert!(rep.ok, "errors: {:?}", rep.errors);
    assert!(rep.header_sha1_ok && rep.top_hash_ok);
    assert_eq!(rep.blocks_checked, 4);

    // extract
    let out = std::env::temp_dir().join(format!("tu_parser_stfs_out_{}", std::process::id()));
    let file = File::open(&path).unwrap();
    let mut reader = BufReader::new(file);
    let layout = StfsLayout::read(&mut reader, size).unwrap();
    layout.num_l0_tables().unwrap();
    let data = read_stfs_file(&mut reader, &layout, 1, 5000).unwrap();
    assert_eq!(data.len(), 5000);
    assert!(data.starts_with(&[0xAAu8; 16]));
    assert_eq!(&data[4096..4101], b"hello");
    let one = read_stfs_file(&mut reader, &layout, 3, 100).unwrap();
    assert_eq!(one.len(), 100);
    assert!(one.iter().all(|&b| b == 0x55));
    drop(reader);
    std::fs::remove_file(&path).ok();
    std::fs::remove_dir_all(&out).ok();
}

#[test]
fn stfs_verify_detects_tamper() {
    let path = std::env::temp_dir().join(format!("tu_parser_tamper_{}.bin", std::process::id()));
    let mut img = write_synthetic_stfs(&path);
    img[0xD000] ^= 0xFF;
    std::fs::write(&path, &img).unwrap();
    let file = File::open(&path).unwrap();
    let size = file.metadata().unwrap().len();
    let mut reader = BufReader::new(file);
    let rep = verify_stfs(&mut reader, size).expect("verify runs");
    std::fs::remove_file(&path).ok();
    assert!(!rep.ok);
    assert!(!rep.header_sha1_ok || rep.errors.iter().any(|e| e.contains("block 1")));
}

#[test]
fn stfs_multilevel_refused() {
    // alloc needs 177 L0 tables -> multi level-1, unsupported.
    let mut img = vec![0u8; 0xC000];
    img[0..4].copy_from_slice(b"LIVE");
    img[0x340..0x344].copy_from_slice(&0xAD0Eu32.to_be_bytes());
    img[0x379] = 0x24;
    img[0x395..0x399].copy_from_slice(&30000u32.to_be_bytes());
    let path = std::env::temp_dir().join(format!("tu_parser_big_{}.bin", std::process::id()));
    std::fs::write(&path, &img).unwrap();
    let file = File::open(&path).unwrap();
    let size = file.metadata().unwrap().len();
    let mut reader = BufReader::new(file);
    let layout = StfsLayout::read(&mut reader, size).expect("layout reads");
    assert!(layout.num_l0_tables().is_err());
    assert!(verify_stfs(&mut reader, size).is_err());
    std::fs::remove_file(&path).ok();
}

#[test]
fn stfs_paths_resolve() {
    let path = std::env::temp_dir().join(format!("tu_parser_paths_{}.bin", std::process::id()));
    write_synthetic_stfs(&path);
    let file = File::open(&path).unwrap();
    let size = file.metadata().unwrap().len();
    let mut reader = BufReader::new(file);
    let layout = StfsLayout::read(&mut reader, size).unwrap();
    // reimplement tiny discovery: read table region, decode slots
    let mut raw = Vec::new();
    let mut off = layout.table_off;
    let mut idx = 0usize;
    while off + 64 <= size {
        let mut slot = vec![0u8; 64];
        use std::io::Read as _;
        reader.seek(SeekFrom::Start(off)).unwrap();
        reader.read_exact(&mut slot).unwrap();
        if slot.iter().all(|&b| b == 0) {
            break;
        }
        if let Some(e) = decode_slot(&slot, idx, off) {
            raw.push(e);
        }
        idx += 1;
        off += 64;
    }
    assert_eq!(entry_full_path(&raw, 2), "sub/one.bin");
    assert_eq!(entry_full_path(&raw, 1), "big.bin");
    std::fs::remove_file(&path).ok();
}

#[test]
fn test_optimize_roundtrip() {
    use crate::write::format::OutputFormat as XisoFormat;
    let path = std::env::temp_dir().join(format!("tu_opt_src_{}.iso", std::process::id()));
    let out = std::env::temp_dir().join(format!("tu_opt_out_{}.iso", std::process::id()));
    write_synthetic_xsf(&path);
    let size = std::fs::metadata(&path).unwrap().len();
    let info = parse_iso(&path, size).expect("synthetic parses");
    crate::write::create_xiso_from_parsed(&info, &out, XisoFormat::Xsf, &(|_| ()))
        .expect("optimize writes");
    let out_size = std::fs::metadata(&out).unwrap().len();
    let info2 = parse_iso(&out, out_size).expect("optimized re-parses");
    assert_eq!(info2.entries.len(), info.entries.len());
    assert_eq!(info2.entries[0].path, info.entries[0].path);
    assert_eq!(info2.entries[0].size, info.entries[0].size);
    assert_eq!(info2.entries[0].is_dir, info.entries[0].is_dir);
    std::fs::remove_file(&path).ok();
    std::fs::remove_file(&out).ok();
}

#[test]
fn test_optimize_removes_padding() {
    use crate::write::format::OutputFormat as XisoFormat;
    let path = std::env::temp_dir().join(format!("tu_opt_pad_{}.iso", std::process::id()));
    let out = std::env::temp_dir().join(format!("tu_opt_pad_out_{}.iso", std::process::id()));
    write_synthetic_xsf(&path);
    // Source places the single file at sector 50 (sparse layout).
    let size = std::fs::metadata(&path).unwrap().len();
    let info = parse_iso(&path, size).expect("synthetic parses");
    assert_eq!(info.entries[0].sector, 50);
    crate::write::create_xiso_from_parsed(&info, &out, XisoFormat::Xsf, &(|_| ()))
        .expect("optimize writes");
    let out_size = std::fs::metadata(&out).unwrap().len();
    let info2 = parse_iso(&out, out_size).expect("optimized re-parses");
    // Root table at 33, single file packed contiguously at 34.
    assert_eq!(info2.root_sector, 33);
    assert_eq!(info2.entries[0].sector, 34);
    std::fs::remove_file(&path).ok();
    std::fs::remove_file(&out).ok();
}

#[test]
fn test_create_xgd3_roundtrip() {
    use crate::write::format::OutputFormat as XisoFormat;
    let dir = std::env::temp_dir().join(format!("tu_create_xgd3_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("hello.txt"), b"hello world").unwrap();
    let out = std::env::temp_dir().join(format!("tu_create_xgd3_{}.iso", std::process::id()));
    crate::write::create_xiso_from_dir(&dir, &out, XisoFormat::Xgd3, &(|_| ()))
        .expect("xgd3 create writes");
    let out_size = std::fs::metadata(&out).unwrap().len();
    let info = parse_iso(&out, out_size).expect("xgd3 re-parses");
    assert_eq!(info.iso_type, "XGD3");
    assert_eq!(info.entries.len(), 1);
    assert_eq!(info.entries[0].path, "hello.txt");
    std::fs::remove_file(&out).ok();
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_multisector_root_size_in_bytes() {
    use crate::write::format::OutputFormat as XisoFormat;
    // Enough entries to force a multi-sector root directory table.
    let dir = std::env::temp_dir().join(format!("tu_root_bytes_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..150 {
        std::fs::write(dir.join(format!("file_{i:03}.txt")), b"x").unwrap();
    }
    let out = std::env::temp_dir().join(format!("tu_root_bytes_{}.iso", std::process::id()));
    crate::write::create_xiso_from_dir(&dir, &out, XisoFormat::Xsf, &(|_| ()))
        .expect("create writes");
    let out_size = std::fs::metadata(&out).unwrap().len();
    let info = parse_iso(&out, out_size).expect("output re-parses");
    assert!(info.root_size > 2048, "root table should span sectors");
    assert_eq!(
        info.root_size % 2048,
        0,
        "root_size must be a byte count, got {}",
        info.root_size
    );
    // Root table occupies root_size bytes starting at root_sector.
    let sectors = info.root_size / 2048;
    assert_eq!(info.root_size, sectors * 2048);
    std::fs::remove_file(&out).ok();
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_restore_backup_replaces_partial() {
    let dir = std::env::temp_dir().join(format!("tu_restore_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("game.iso");
    let backup = dir.join("game.bak.iso");
    std::fs::write(&backup, b"original").unwrap();
    std::fs::write(&input, b"partial-output").unwrap();
    crate::restore_backup(&backup, &input);
    assert_eq!(std::fs::read(&input).unwrap(), b"original");
    assert!(!backup.exists());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_verify_fidelity_catches_content_tamper() {
    use crate::write::format::OutputFormat as XisoFormat;
    let path = std::env::temp_dir().join(format!("tu_tamper_src_{}.iso", std::process::id()));
    let out = std::env::temp_dir().join(format!("tu_tamper_out_{}.iso", std::process::id()));
    write_synthetic_xsf(&path);
    let size = std::fs::metadata(&path).unwrap().len();
    let info = parse_iso(&path, size).expect("synthetic parses");
    crate::write::create_xiso_from_parsed(&info, &out, XisoFormat::Xsf, &(|_| ()))
        .expect("optimize writes");
    // Flip a byte inside the copied file data (file lands at sector 34).
    {
        use std::io::{Seek, SeekFrom, Write};
        let mut f = std::fs::OpenOptions::new().write(true).open(&out).unwrap();
        f.seek(SeekFrom::Start(34 * 2048)).unwrap();
        f.write_all(&[0xFF]).unwrap();
    }
    let out_size = std::fs::metadata(&out).unwrap().len();
    let tampered = parse_iso(&out, out_size).expect("tampered re-parses");
    assert!(crate::verify_optimize_fidelity(&info, &tampered, false).is_err());
    std::fs::remove_file(&path).ok();
    std::fs::remove_file(&out).ok();
}

#[test]
fn test_verify_fidelity_catches_attr_and_timestamp_diff() {
    let path = std::env::temp_dir().join(format!("tu_attr_src_{}.iso", std::process::id()));
    write_synthetic_xsf(&path);
    let size = std::fs::metadata(&path).unwrap().len();
    let info = parse_iso(&path, size).expect("synthetic parses");
    let mut changed = info.clone();
    changed.entries[0].attrs.archive = !changed.entries[0].attrs.archive;
    assert!(crate::verify_optimize_fidelity(&info, &changed, false).is_err());
    let mut changed_ts = info.clone();
    changed_ts.volume_timestamp = Some(info.volume_timestamp.unwrap_or(0).wrapping_add(1));
    assert!(crate::verify_optimize_fidelity(&info, &changed_ts, false).is_err());
    std::fs::remove_file(&path).ok();
}

#[test]
fn test_zero_byte_file_does_not_truncate_table() {
    use crate::write::format::OutputFormat as XisoFormat;
    let dir = std::env::temp_dir().join(format!("tu_zero_byte_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("empty.txt"), b"").unwrap();
    std::fs::write(dir.join("after.txt"), b"data").unwrap();
    let out = std::env::temp_dir().join(format!("tu_zero_byte_{}.iso", std::process::id()));
    crate::write::create_xiso_from_dir(&dir, &out, XisoFormat::Xsf, &(|_| ()))
        .expect("create writes");
    let out_size = std::fs::metadata(&out).unwrap().len();
    let info = parse_iso(&out, out_size).expect("output re-parses");
    assert_eq!(info.entries.len(), 2);
    assert!(info.entries.iter().any(|e| e.path == "after.txt"));
    std::fs::remove_file(&out).ok();
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_md5_and_checksum_match_known_values() {
    use crate::extract::{checksum_iso, md5_iso};
    use crate::write::format::OutputFormat as XisoFormat;
    let dir = std::env::temp_dir().join(format!("tu_hash_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("a.txt"), b"aaa").unwrap();
    std::fs::write(dir.join("sub").join("b.txt"), b"bbbb").unwrap();
    let out = std::env::temp_dir().join(format!("tu_hash_{}.iso", std::process::id()));
    crate::write::create_xiso_from_dir(&dir, &out, XisoFormat::Xsf, &(|_| ()))
        .expect("create writes");
    let out_size = std::fs::metadata(&out).unwrap().len();
    let mut info = parse_iso(&out, out_size).expect("output re-parses");
    info.file_path = out.display().to_string();

    // md5("aaa") cross-checked against xdvdfs md5 on the same bytes.
    // Directories hash their raw table bytes, like xdvdfs md5.
    let sums = md5_iso(&info).expect("md5 runs");
    assert_eq!(sums.len(), 3);
    assert_eq!(sums[0].0, "a.txt");
    assert_eq!(sums[0].1, "47bce5c74f589f4867dbd57e9ca9f808");
    assert_eq!(sums[1].0, "sub");
    assert_eq!(sums[2].0, "sub/b.txt");

    // Checksum mirrors xdvdfs checksum (SHA3-256 over sorted `/path`
    // keys plus file bytes); value cross-checked against xdvdfs below.
    let sum = checksum_iso(&info).expect("checksum runs");
    assert_eq!(sum.len(), 64);

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_copy_out_single_file_and_subtree() {
    use crate::extract::copy_out_iso;
    use crate::write::format::OutputFormat as XisoFormat;
    let dir = std::env::temp_dir().join(format!("tu_copyout_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("a.txt"), b"aaa").unwrap();
    std::fs::write(dir.join("sub").join("b.txt"), b"bbbb").unwrap();
    let out = std::env::temp_dir().join(format!("tu_copyout_{}.iso", std::process::id()));
    crate::write::create_xiso_from_dir(&dir, &out, XisoFormat::Xsf, &(|_| ()))
        .expect("create writes");
    let out_size = std::fs::metadata(&out).unwrap().len();
    let info = parse_iso(&out, out_size).expect("output re-parses");
    // info needs the real source path for copy-out reads.
    let mut info = info;
    info.file_path = out.display().to_string();

    // Single file, exact bytes back (case-insensitive match).
    let dest = std::env::temp_dir().join(format!("tu_copyout_{}_a.txt", std::process::id()));
    let res = copy_out_iso(&info, "SUB\\B.TXT", &dest, false).expect("copy file");
    assert_eq!(res.files_copied, 1);
    assert_eq!(res.bytes_copied, 4);
    assert_eq!(std::fs::read(&dest).unwrap(), b"bbbb");

    // Directory subtree.
    let dest_dir = std::env::temp_dir().join(format!("tu_copyout_{}_sub", std::process::id()));
    let res = copy_out_iso(&info, "sub", &dest_dir, false).expect("copy dir");
    assert_eq!(res.files_copied, 1);
    assert_eq!(std::fs::read(dest_dir.join("b.txt")).unwrap(), b"bbbb");

    // Missing path errors.
    assert!(copy_out_iso(&info, "nope.txt", &dest, false).is_err());

    let _ = std::fs::remove_file(&dest);
    let _ = std::fs::remove_dir_all(&dest_dir);
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_map_rule_parsing_rejects_bad_rules() {
    use crate::write::remap::parse_map_rule;
    assert!(parse_map_rule("docs:/manual").is_ok());
    assert!(parse_map_rule("!secret").is_ok());
    assert!(parse_map_rule("bareword").is_err());
    assert!(parse_map_rule(":empty").is_err());
}

#[test]
fn test_mapped_create_relocates_prefix_and_excludes() {
    use crate::write::create::create_xiso_from_mapped_dir_with_progress;
    use crate::write::format::OutputFormat as XisoFormat;
    use crate::write::remap::parse_map_rule;
    let dir = std::env::temp_dir().join(format!("tu_map_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("docs")).unwrap();
    std::fs::create_dir_all(dir.join("secret")).unwrap();
    std::fs::write(dir.join("docs").join("a.txt"), b"aaa").unwrap();
    std::fs::write(dir.join("secret").join("s.txt"), b"sss").unwrap();
    std::fs::write(dir.join("top.txt"), b"top").unwrap();
    let rules = [
        parse_map_rule("docs:/manual").unwrap(),
        parse_map_rule("!secret").unwrap(),
        parse_map_rule("top.txt:/top.txt").unwrap(),
    ];
    let out = std::env::temp_dir().join(format!("tu_map_{}.iso", std::process::id()));
    create_xiso_from_mapped_dir_with_progress(
        &dir,
        &out,
        XisoFormat::Xsf,
        &(|_| ()),
        &(|_| ()),
        false,
        &rules,
        false,
        true,
    )
    .expect("mapped create writes");
    let out_size = std::fs::metadata(&out).unwrap().len();
    let info = parse_iso(&out, out_size).expect("output re-parses");
    let mut paths: Vec<String> = info
        .entries
        .iter()
        .map(|e| e.path.replace('\\', "/"))
        .collect();
    paths.sort();
    // Prefix rule pulls the subtree; negation drops secret; unmatched top
    // rule keeps top.txt.
    assert!(paths.iter().any(|p| p.ends_with("manual/a.txt")));
    assert!(!paths.iter().any(|p| p.contains("secret")));
    assert!(paths.iter().any(|p| p.ends_with("top.txt")));
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_mapped_dry_run_writes_nothing() {
    use crate::write::create::create_xiso_from_mapped_dir_with_progress;
    use crate::write::format::OutputFormat as XisoFormat;
    use crate::write::remap::parse_map_rule;
    let dir = std::env::temp_dir().join(format!("tu_mapdry_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.txt"), b"aaa").unwrap();
    let rules = [parse_map_rule("a.txt:/a.txt").unwrap()];
    let out = std::env::temp_dir().join(format!("tu_mapdry_{}.iso", std::process::id()));
    create_xiso_from_mapped_dir_with_progress(
        &dir,
        &out,
        XisoFormat::Xsf,
        &(|_| ()),
        &(|_| ()),
        false,
        &rules,
        true,
        true,
    )
    .expect("dry run ok");
    assert!(!out.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_xdbf_gpd_variant_parses() {
    use crate::xdbf::parse_xdbf;
    let dir = std::env::temp_dir().join(format!("tu_gpd_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // GPD form: 24B header, capacity 512-entry table, 1-entry free table.
    let mut blob = Vec::new();
    blob.extend_from_slice(b"XDBF");
    blob.extend_from_slice(&0x10000u32.to_be_bytes());
    blob.extend_from_slice(&512u32.to_be_bytes());
    blob.extend_from_slice(&2u32.to_be_bytes());
    blob.extend_from_slice(&1u32.to_be_bytes());
    blob.extend_from_slice(&0u32.to_be_bytes());
    // Entry 0: namespace 3 (Setting), id 0x10040003, 4 payload bytes.
    blob.extend_from_slice(&3u16.to_be_bytes());
    blob.extend_from_slice(&0x10040003u64.to_be_bytes());
    blob.extend_from_slice(&0u32.to_be_bytes());
    blob.extend_from_slice(&4u32.to_be_bytes());
    // Entry 1: namespace 5 (String), id 7, "hi!" + NUL.
    blob.extend_from_slice(&5u16.to_be_bytes());
    blob.extend_from_slice(&7u64.to_be_bytes());
    blob.extend_from_slice(&4u32.to_be_bytes());
    blob.extend_from_slice(&4u32.to_be_bytes());
    // Pad the rest of the 512-entry table + 8B free table with zeros.
    while blob.len() < 24 + 512 * 18 + 8 {
        blob.push(0);
    }
    blob.extend_from_slice(&[1, 2, 3, 4]);
    blob.extend_from_slice(b"hi!\0");
    let path = dir.join("test.gpd");
    std::fs::write(&path, &blob).unwrap();
    let info = parse_xdbf(&path, blob.len() as u64).expect("parses");
    assert_eq!(info.variant, "gpd");
    assert_eq!(info.num_entries, 2);
    assert_eq!(info.entries[0].namespace, Some(3));
    assert_eq!(info.entries[0].identifier, 0x10040003);
    assert_eq!(info.entries[1].namespace, Some(5));
    let rep = crate::xdbf::report_info(&info);
    assert_eq!(rep.entries[0].resource, "Setting/0000000010040003");
    assert_eq!(rep.entries[0].namespace.as_deref(), Some("Setting"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_xdbf_parses_entries_and_title() {
    use crate::xdbf::{parse_xdbf, resource_name};
    let dir = std::env::temp_dir().join(format!("tu_xdbf_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Header (24B) + 6B gap + 2x18B entries + data region.
    let mut blob = Vec::new();
    blob.extend_from_slice(b"XDBF");
    blob.extend_from_slice(&1u16.to_be_bytes());
    blob.extend_from_slice(&0u16.to_be_bytes());
    blob.extend_from_slice(&2u32.to_be_bytes());
    blob.extend_from_slice(&2u32.to_be_bytes());
    blob.extend_from_slice(&0u32.to_be_bytes());
    blob.extend_from_slice(&0u32.to_be_bytes());
    blob.extend_from_slice(&[0u8; 6]);
    // Entry 0: identifier 1, type 3 (title), data "xx"+len+name.
    // First byte nonzero so the zero-padding skip stops at the data start.
    let mut title_data = vec![0xAAu8];
    title_data.extend_from_slice(&[0u8; 16]);
    title_data.push(9);
    title_data.extend_from_slice(b"Test Game");
    // Entry 1: XACH achievement blob.
    let ach = b"achievement-bytes";
    let off0 = 0u32;
    let off1 = title_data.len() as u32;
    for (id, ty, off, size) in [
        (1u32, 3u16, off0, title_data.len() as u32),
        (0x58414348, 2, off1, ach.len() as u32),
    ] {
        blob.extend_from_slice(&id.to_be_bytes());
        blob.extend_from_slice(&off.to_be_bytes());
        blob.extend_from_slice(&size.to_be_bytes());
        blob.extend_from_slice(&ty.to_be_bytes());
        blob.extend_from_slice(&0u32.to_be_bytes());
    }
    blob.extend_from_slice(&title_data);
    blob.extend_from_slice(ach);
    let path = dir.join("test.xdbf");
    std::fs::write(&path, &blob).unwrap();
    let info = parse_xdbf(&path, blob.len() as u64).expect("parses");
    assert_eq!(info.num_entries, 2);
    assert_eq!(info.title_name.as_deref(), Some("Test Game"));
    assert_eq!(resource_name(0x58414348, 2, None), "XACH/Achievement");
    assert_eq!(resource_name(1, 3, None), "00000001/TitleInfo");
    assert_eq!(
        resource_name(0x10040003, 0, Some(3)),
        "Setting/0000000010040003"
    );
    // Report conversion carries file offsets.
    let rep = crate::xdbf::report_info(&info);
    assert_eq!(rep.entries.len(), 2);
    assert!(rep.entries[0].offset >= 30 + 2 * 18);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_xdbf_rejects_garbage() {
    use crate::xdbf::parse_xdbf;
    let dir = std::env::temp_dir().join(format!("tu_xdbfbad_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Mismatched entry counts.
    let mut bad = vec![0u8; 64];
    bad[0..4].copy_from_slice(b"XDBF");
    bad[8..12].copy_from_slice(&2u32.to_be_bytes());
    bad[12..16].copy_from_slice(&3u32.to_be_bytes());
    let path = dir.join("bad.xdbf");
    std::fs::write(&path, &bad).unwrap();
    assert!(parse_xdbf(&path, 64).is_err());
    // Random small file.
    let tiny = dir.join("tiny.bin");
    std::fs::write(&tiny, b"hello world, this is not xdbf data!!").unwrap();
    assert!(parse_xdbf(&tiny, 36).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_truncate_drops_only_trailing_padding() {
    use crate::write::format::OutputFormat as XisoFormat;
    use crate::write::truncate::{last_used_sector, truncate_iso_to_last_used};
    let dir = std::env::temp_dir().join(format!("tu_trunc_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.txt"), b"aaa").unwrap();
    let base = std::env::temp_dir().join(format!("tu_trunc_{}.iso", std::process::id()));
    crate::write::create_xiso_from_dir(&dir, &base, XisoFormat::Xsf, &(|_| ()))
        .expect("create writes");
    // Append 1 MiB of trailing garbage like an unoptimized image would have.
    let padded = std::env::temp_dir().join(format!("tu_trunc_{}_pad.iso", std::process::id()));
    let mut data = std::fs::read(&base).unwrap();
    let clean_len = data.len();
    data.extend_from_slice(&vec![0u8; 1024 * 1024]);
    std::fs::write(&padded, &data).unwrap();
    let info = parse_iso(&padded, data.len() as u64).expect("parses");
    let last = last_used_sector(&info);
    assert!((last as u64 * 2048) <= clean_len as u64);
    let out = std::env::temp_dir().join(format!("tu_trunc_{}_out.iso", std::process::id()));
    let out_size = truncate_iso_to_last_used(&padded, &out, &info).expect("truncates");
    assert!(out_size < data.len() as u64);
    // Layout preserved: same entries, same sectors, same bytes.
    let out_info = parse_iso(&out, out_size).expect("re-parses");
    assert_eq!(out_info.entries.len(), info.entries.len());
    for (a, b) in info.entries.iter().zip(out_info.entries.iter()) {
        assert_eq!(a.path, b.path);
        assert_eq!(a.sector, b.sector);
        assert_eq!(a.size, b.size);
    }
    assert!(crate::verify_optimize_fidelity(&info, &out_info, false).is_ok());
    for f in [&base, &padded, &out] {
        let _ = std::fs::remove_file(f);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_media_patch_rewrites_pattern_bytes() {
    use crate::write::img::{is_xbe_file, patch_media_enable};
    assert!(is_xbe_file("default.xbe"));
    assert!(is_xbe_file("DEFAULT.XBE"));
    assert!(!is_xbe_file("default.xex"));
    assert!(!is_xbe_file("xbe"));
    // Pattern at buffer start, mid-buffer, and straddling position.
    let mut buf = vec![0u8];
    buf.extend_from_slice(&[0xe8, 0xca, 0xfd, 0xff, 0xff, 0x85, 0xc0, 0x7d]);
    buf.extend_from_slice(&[0x11, 0x22]);
    buf.extend_from_slice(&[0xe8, 0xca, 0xfd, 0xff, 0xff, 0x85, 0xc0, 0x7d]);
    patch_media_enable(&mut buf);
    assert_eq!(buf[8], 0xeb);
    assert_eq!(buf[buf.len() - 1], 0xeb);
    assert_eq!(buf[9], 0x11);
    // Already-patched input is untouched (idempotent).
    let before = buf.clone();
    patch_media_enable(&mut buf);
    assert_eq!(buf, before);
    // Short buffers never match.
    let mut tiny = vec![0xe8, 0xca];
    patch_media_enable(&mut tiny);
    assert_eq!(tiny, vec![0xe8, 0xca]);
}

#[test]
fn test_create_patches_xbe_by_default() {
    use crate::write::format::OutputFormat as XisoFormat;
    let dir = std::env::temp_dir().join(format!("tu_xbe_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Fake .xbe with two pattern occurrences plus tail bytes.
    let mut xbe = vec![0x90u8; 100];
    xbe.extend_from_slice(&[0xe8, 0xca, 0xfd, 0xff, 0xff, 0x85, 0xc0, 0x7d]);
    xbe.extend_from_slice(&[0x90u8; 3000]);
    xbe.extend_from_slice(&[0xe8, 0xca, 0xfd, 0xff, 0xff, 0x85, 0xc0, 0x7d]);
    xbe.extend_from_slice(&[0x90u8; 50]);
    std::fs::write(dir.join("default.xbe"), &xbe).unwrap();
    std::fs::write(dir.join("readme.txt"), b"plain").unwrap();
    let out = std::env::temp_dir().join(format!("tu_xbe_{}.iso", std::process::id()));
    crate::write::create_xiso_from_dir_with_progress(
        &dir,
        &out,
        XisoFormat::Xsf,
        &(|_| ()),
        &(|_| ()),
        false,
        true,
    )
    .expect("create writes");
    let out_size = std::fs::metadata(&out).unwrap().len();
    let mut info = parse_iso(&out, out_size).expect("output re-parses");
    info.file_path = out.display().to_string();
    let entry = info
        .entries
        .iter()
        .find(|e| e.path.eq_ignore_ascii_case("default.xbe"))
        .expect("xbe entry present")
        .clone();
    // Read back the stored bytes via copy-out and check both hits patched.
    let dest = std::env::temp_dir().join(format!("tu_xbe_{}_out.xbe", std::process::id()));
    let res = crate::extract::copy_out_iso(&info, &entry.path, &dest, false).expect("copy out");
    assert_eq!(res.bytes_copied as usize, xbe.len());
    let stored = std::fs::read(&dest).unwrap();
    assert_eq!(stored.len(), xbe.len());
    assert_eq!(stored[107], 0xeb);
    assert_eq!(stored[3115], 0xeb);
    assert_eq!(stored[stored.len() - 1], 0x90);
    // Non-xbe file untouched.
    let txt = info
        .entries
        .iter()
        .find(|e| e.path.eq_ignore_ascii_case("readme.txt"))
        .expect("txt entry present")
        .clone();
    let dest_txt = std::env::temp_dir().join(format!("tu_xbe_{}_out.txt", std::process::id()));
    crate::extract::copy_out_iso(&info, &txt.path, &dest_txt, false).expect("copy out");
    assert_eq!(std::fs::read(&dest_txt).unwrap(), b"plain");
    let _ = std::fs::remove_file(&dest);
    let _ = std::fs::remove_file(&dest_txt);
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_is_systemupdate_path_matches() {
    use crate::iso::is_systemupdate_path;
    assert!(is_systemupdate_path("$SystemUpdate"));
    assert!(is_systemupdate_path("$SystemUpdate/su20076000_00000000"));
    assert!(is_systemupdate_path("$systemupdate\\system.manifest"));
    assert!(!is_systemupdate_path("disk/bb.cpk"));
    assert!(!is_systemupdate_path("default.xex"));
    assert!(!is_systemupdate_path(""));
}

#[test]
fn test_create_skips_systemupdate_when_flagged() {
    use crate::write::dirtab::build_tree_from_dir_arena;
    let dir = std::env::temp_dir().join(format!("tu_skipsu_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("$SystemUpdate")).unwrap();
    std::fs::write(dir.join("$SystemUpdate").join("a.bin"), b"sys").unwrap();
    std::fs::write(dir.join("default.xex"), b"game").unwrap();
    let out = dir.join("skip.iso");
    let full = build_tree_from_dir_arena(&dir, &out, false).unwrap();
    let full_files: usize = full
        .iter()
        .flat_map(|n| n.entries.iter())
        .filter(|e| !e.is_dir)
        .count();
    assert_eq!(full_files, 2);
    let skipped = build_tree_from_dir_arena(&dir, &out, true).unwrap();
    let kept: Vec<String> = skipped
        .iter()
        .flat_map(|n| n.entries.iter())
        .filter(|e| !e.is_dir)
        .map(|e| e.name.clone())
        .collect();
    assert_eq!(kept, vec!["default.xex".to_string()]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_written_tables_chain_entries_right() {
    use crate::write::format::OutputFormat as XisoFormat;
    // Tree-walking readers (extract-xiso, xdvdfs) follow right-child
    // offsets, so entries must form a right-linked chain; a table of
    // all-zero offsets reads as a single entry to them.
    let dir = std::env::temp_dir().join(format!("tu_chain_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.txt"), b"a").unwrap();
    std::fs::write(dir.join("b.txt"), b"bb").unwrap();
    std::fs::write(dir.join("c.txt"), b"ccc").unwrap();
    let out = std::env::temp_dir().join(format!("tu_chain_{}.iso", std::process::id()));
    crate::write::create_xiso_from_dir(&dir, &out, XisoFormat::Xsf, &(|_| ()))
        .expect("create writes");
    let out_size = std::fs::metadata(&out).unwrap().len();
    let info = parse_iso(&out, out_size).expect("output re-parses");
    assert_eq!(info.entries.len(), 3);
    // First entry must point at the second; last entry terminates the chain.
    assert_ne!(info.entries[0].right_child_offset, Some(0));
    assert_eq!(info.entries[2].right_child_offset, Some(0));
    std::fs::remove_file(&out).ok();
    std::fs::remove_dir_all(&dir).ok();
}
