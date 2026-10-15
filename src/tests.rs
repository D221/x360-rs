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
    assert!(crate::verify_optimize_fidelity(&info, &tampered).is_err());
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
    assert!(crate::verify_optimize_fidelity(&info, &changed).is_err());
    let mut changed_ts = info.clone();
    changed_ts.volume_timestamp = Some(info.volume_timestamp.unwrap_or(0).wrapping_add(1));
    assert!(crate::verify_optimize_fidelity(&info, &changed_ts).is_err());
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
