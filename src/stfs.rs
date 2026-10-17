//! STFS block layout, hash verification, and extraction.

use std::fs::File;
use std::io::{BufReader, Read, Seek};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::extract::sanitize_dir_name;
use crate::report::TuInfo;
use crate::tu_parser::{decode_slot, format_size};
use crate::xex::read_at;

// ---------------------------------------------------------------------------
// STFS block layout, hashing and extraction.
//
// Verified empirically against 23 TU/DLC plus XBLA/avatar/theme samples
// (free60 STFS wiki for field semantics):
// - block size 0x1000; block-space base = ((HeaderSize@0x340 + 0xFFF) & 0xF000)
// - hash hierarchy (Xenia `BlockToOffset`/`BlockToHashBlockNumber` with one
//   hash block per table, matching all verified samples):
//   Level 0: table of the next 170 data blocks (span 170)
//   Level 1: table of the next 170 level-0 tables (span 28900)
//   Level 2: table of level-1 tables (single block at physical 29071)
//   Physical block = data block + one hash block per spanned level, i.e.
//   data block 171 sits where block 172 would (block 172 holds a table).
// - file table (LE24 block number @0x37E, LE16 count @0x37C) holds 64-byte
//   entries: name[0x28], flags+namelen @0x28 (0x80 = dir, 0x40 =
//   consecutive, low 6 bits = name length), allocated LE24 @0x29,
//   start block LE24 @0x2F, path indicator BE u16 @0x32 (0xFFFF = root),
//   size BE u32 @0x34.
// - hash record (24 bytes): SHA1(20) + status(1, 0x80 = used) + next BE24
//   (0xFFFFFF = end of chain).
// - header top hash covers T0 alone (single table), the L1 table at
//   physical 171 (up to 170 L0 tables), or the L2 table at physical 29071.
// - packages beyond 170 level-1 tables (> 4913000 data blocks, ~20 GiB)
//   are refused with a clear error.

pub const STFS_BLOCK: u64 = 0x1000;
pub const STFS_HASH_SPAN: u64 = 170;
pub const STFS_L1_SPAN: u64 = 170 * 170; // 28900 data blocks per L1 table
pub const STFS_L2_SPAN: u64 = 170 * 170 * 170; // 4913000 data blocks
/// Physical block of the first level-1 table.
pub const STFS_L1_PHYS: u64 = 171;
/// Physical block of the level-2 (top) table.
pub const STFS_L2_PHYS: u64 = 29071;

#[derive(Debug, Clone)]
pub struct StfsLayout {
    pub base: u64,
    pub table_off: u64,
    #[allow(dead_code)] // decoded from the descriptor; kept for debugging/inspection
    pub table_blocks: u64,
    pub alloc_blocks: u64,
    pub top_hash: [u8; 20],
    pub header_sha1: [u8; 20],
    pub file_size: u64,
}

#[derive(Error, Debug)]
pub enum StfsError {
    #[error("unsupported STFS layout (base 0x{0:X}, expected 0xB000)")]
    UnsupportedBase(u64),
    #[error("SVOD data layout not supported (only STFS containers can be verified/extracted)")]
    SvodLayout,
    #[error(
        "multi-level hash tables beyond 170 level-1 tables are not supported ({0} data blocks)"
    )]
    MultiLevel(u64),
    #[error("table region out of range")]
    TableRange,
    #[error("block {0} out of range")]
    BlockRange(u64),
    #[error("broken hash chain at block {0}")]
    BrokenChain(u64),
    #[error("unexpected record status 0x{0:02X} at data block {1}")]
    BadStatus(u8, u64),
}

pub(crate) fn be_u32_at(buf: &[u8], off: usize) -> Result<u32> {
    buf.get(off..off + 4)
        .and_then(|b| <[u8; 4]>::try_from(b).ok())
        .map(u32::from_be_bytes)
        .context("short read")
}

impl StfsLayout {
    pub fn read(reader: &mut (impl Read + Seek), file_size: u64) -> Result<Self> {
        let hdr = read_at(reader, 0x340, 0x70)?;
        let header_size = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as u64;
        let base = (header_size + 0xFFF) & 0xF000;
        if base != 0xB000 {
            return Err(StfsError::UnsupportedBase(base).into());
        }
        // Volume descriptor @0x379: block sep @+2, table count LE16 @+3,
        // table block LE24 @+5, top hash @+8, allocated BE32 @+0x1C.
        let desc = read_at(reader, 0x379, 0x24)?;
        let table_blocks = u16::from_le_bytes([desc[3], desc[4]]) as u64;
        let table_blk = desc[5] as u64 | ((desc[6] as u64) << 8) | ((desc[7] as u64) << 16);
        let mut top_hash = [0u8; 20];
        top_hash.copy_from_slice(&desc[8..28]);
        let alloc_blocks =
            u32::from_be_bytes([desc[0x1C], desc[0x1D], desc[0x1E], desc[0x1F]]) as u64;
        let table_off = base + (table_blk + 1) * STFS_BLOCK;
        if table_off + table_blocks * STFS_BLOCK > file_size {
            return Err(StfsError::TableRange.into());
        }
        let desc_type = be_u32_at(&read_at(reader, 0x3A9, 4)?, 0)?;
        if desc_type != 0 {
            return Err(StfsError::SvodLayout.into());
        }
        let header_sha1 = read_at(reader, 0x32C, 20)?;
        let mut header_sha1_arr = [0u8; 20];
        header_sha1_arr.copy_from_slice(&header_sha1);
        Ok(StfsLayout {
            base,
            table_off,
            table_blocks,
            alloc_blocks,
            top_hash,
            header_sha1: header_sha1_arr,
            file_size,
        })
    }

    /// Number of level-0 hash tables needed; errors past 170 level-1
    /// tables (> 4913000 data blocks, ~20 GiB).
    pub fn num_l0_tables(&self) -> Result<u64> {
        let n = self.alloc_blocks.div_ceil(STFS_HASH_SPAN);
        if n > STFS_HASH_SPAN * STFS_HASH_SPAN {
            return Err(StfsError::MultiLevel(self.alloc_blocks).into());
        }
        Ok(n.max(1))
    }

    /// Number of level-1 hash tables needed (0 or 1 when a single L0/L1
    /// covers everything).
    pub fn num_l1_tables(&self, num_l0: u64) -> u64 {
        num_l0.div_ceil(STFS_HASH_SPAN)
    }

    /// Physical block number holding level-0 table `i` (covers data blocks
    /// `170*i .. 170*i+170`). General form of Xenia's
    /// `BlockToHashBlockNumber(block, 0)` with one hash block per table.
    pub fn l0_table_phys(i: u64) -> u64 {
        let b = i * STFS_HASH_SPAN;
        if b < STFS_HASH_SPAN {
            return 0;
        }
        let mut block = (b / STFS_HASH_SPAN) * (STFS_HASH_SPAN + 1);
        block += (b / STFS_L1_SPAN) + 1;
        if b < STFS_L1_SPAN {
            return block;
        }
        block + 1
    }

    /// Physical block number holding level-1 table `j` (covers L0 tables
    /// `170*j .. 170*j+170`). General form of Xenia's
    /// `BlockToHashBlockNumber(block, 1)` with one hash block per table.
    pub fn l1_table_phys(j: u64) -> u64 {
        if j == 0 {
            return STFS_L1_PHYS;
        }
        j * (STFS_L1_SPAN + STFS_HASH_SPAN + 1) + 1
    }

    /// Physical block number holding data block `b`. General form of
    /// Xenia's `BlockToOffset`: one hash-table block per spanned level.
    pub fn data_phys_block(b: u64) -> u64 {
        let mut block = b;
        for &span in &[STFS_HASH_SPAN, STFS_L1_SPAN, STFS_L2_SPAN] {
            block += (b + span) / span;
            if b < span {
                break;
            }
        }
        block
    }

    pub fn phys_offset(&self, phys: u64) -> Result<u64> {
        let off = self
            .base
            .checked_add(phys.checked_mul(STFS_BLOCK).context("overflow")?)
            .context("overflow")?;
        if off + STFS_BLOCK > self.file_size {
            return Err(StfsError::BlockRange(phys).into());
        }
        Ok(off)
    }

    pub fn data_offset(&self, b: u64) -> Result<u64> {
        if b >= self.alloc_blocks {
            return Err(StfsError::BlockRange(b).into());
        }
        self.phys_offset(Self::data_phys_block(b))
    }

    pub fn read_block(&self, reader: &mut (impl Read + Seek), phys: u64) -> Result<Vec<u8>> {
        read_at(reader, self.phys_offset(phys)?, STFS_BLOCK as usize)
    }

    pub fn read_l0_record(
        &self,
        reader: &mut (impl Read + Seek),
        table: u64,
        slot: u64,
    ) -> Result<([u8; 20], u8, u64)> {
        let off = self.phys_offset(Self::l0_table_phys(table))? + slot * 24;
        let rec = read_at(reader, off, 24)?;
        let mut hash = [0u8; 20];
        hash.copy_from_slice(&rec[..20]);
        let next = ((rec[21] as u64) << 16) | ((rec[22] as u64) << 8) | (rec[23] as u64);
        Ok((hash, rec[20], next))
    }
}

pub(crate) fn sha1_of(data: &[u8]) -> [u8; 20] {
    use sha1::{Digest, Sha1};
    let mut h = Sha1::new();
    h.update(data);
    h.finalize().into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyReport {
    pub ok: bool,
    pub header_sha1_ok: bool,
    pub top_hash_ok: bool,
    pub tables_checked: u64,
    pub blocks_checked: u64,
    pub blocks_skipped: u64,
    pub errors: Vec<String>,
}

/// Verify an STFS container: header SHA1, top hash, every level-0 table
/// hash, and every used data block hash.
pub fn verify_stfs(reader: &mut (impl Read + Seek), file_size: u64) -> Result<VerifyReport> {
    let layout = StfsLayout::read(reader, file_size)?;
    let mut errors = Vec::new();
    let mut tables_checked = 0u64;
    let mut blocks_checked = 0u64;
    let mut blocks_skipped = 0u64;

    // 1. Header SHA1 over 0x344..base (first hash table).
    let hdr = read_at(reader, 0x344, (layout.base - 0x344) as usize)?;
    let header_sha1_ok = sha1_of(&hdr) == layout.header_sha1;
    if !header_sha1_ok {
        errors.push("header SHA1 mismatch".to_string());
    }

    // 2. Top hash: SHA1 of T0 alone (single table), of the level-1
    // table (up to 170 L0 tables), or of the level-2 table (beyond).
    let num_l0 = layout.num_l0_tables()?;
    let num_l1 = layout.num_l1_tables(num_l0);
    let top_hash_ok = if num_l0 == 1 {
        let t0 = layout.read_block(reader, 0)?;
        tables_checked += 1;
        let ok = sha1_of(&t0) == layout.top_hash;
        if !ok {
            errors.push("top hash mismatch (level-0 table)".to_string());
        }
        ok
    } else if num_l1 == 1 {
        let l1 = layout.read_block(reader, StfsLayout::l1_table_phys(0))?;
        tables_checked += 1;
        let ok = sha1_of(&l1) == layout.top_hash;
        if !ok {
            errors.push("top hash mismatch (level-1 table)".to_string());
        }

        // 3. Each level-0 table hash must match the level-1 record.
        if ok {
            for i in 0..num_l0 {
                let t = layout.read_block(reader, StfsLayout::l0_table_phys(i))?;
                tables_checked += 1;
                if sha1_of(&t) != l1[i as usize * 24..i as usize * 24 + 20] {
                    errors.push(format!("level-0 table {i} hash mismatch"));
                }
            }
        }
        ok
    } else {
        let l2 = layout.read_block(reader, STFS_L2_PHYS)?;
        tables_checked += 1;
        let mut ok = sha1_of(&l2) == layout.top_hash;
        if !ok {
            errors.push("top hash mismatch (level-2 table)".to_string());
        }

        // 3. Each level-1 table hash must match the level-2 record, and
        // each level-0 table hash its level-1 record.
        if ok {
            for j in 0..num_l1 {
                let l1 = layout.read_block(reader, StfsLayout::l1_table_phys(j))?;
                tables_checked += 1;
                if sha1_of(&l1) != l2[j as usize * 24..j as usize * 24 + 20] {
                    errors.push(format!("level-1 table {j} hash mismatch"));
                    ok = false;
                    continue;
                }
                for k in 0..STFS_HASH_SPAN {
                    let i = j * STFS_HASH_SPAN + k;
                    if i >= num_l0 {
                        break;
                    }
                    let t = layout.read_block(reader, StfsLayout::l0_table_phys(i))?;
                    tables_checked += 1;
                    // Slot within this L1 table: L0 tables are densely
                    // packed, so global index maps directly.
                    let slot = (i % STFS_HASH_SPAN) as usize * 24;
                    if sha1_of(&t) != l1[slot..slot + 20] {
                        errors.push(format!("level-0 table {i} hash mismatch"));
                    }
                }
            }
        }
        ok
    };

    // 4. Every used data block hash.
    for b in 0..layout.alloc_blocks {
        let table = b / STFS_HASH_SPAN;
        let slot = b % STFS_HASH_SPAN;
        let (hash, status, _) = layout.read_l0_record(reader, table, slot)?;
        if status != 0x80 {
            blocks_skipped += 1;
            continue;
        }
        let off = layout.data_offset(b)?;
        let blk = read_at(reader, off, STFS_BLOCK as usize)?;
        blocks_checked += 1;
        if sha1_of(&blk) != hash {
            errors.push(format!("data block {b} hash mismatch"));
            if errors.len() > 20 {
                errors.push("...".to_string());
                break;
            }
        }
    }

    Ok(VerifyReport {
        ok: errors.is_empty(),
        header_sha1_ok,
        top_hash_ok,
        tables_checked,
        blocks_checked,
        blocks_skipped,
        errors,
    })
}

/// Full decoded STFS file-table entry.
#[derive(Debug, Clone)]
pub struct StfsFileEntry {
    pub index: usize,
    pub name: String,
    #[allow(dead_code)] // raw flags byte; `is_dir` is the decoded form
    pub flags: u8,
    pub start_block: u64,
    pub size: u64,
    pub parent: u64, // path indicator; 0xFFFF = root
    pub is_dir: bool,
    pub table_offset: u64,
}

/// Resolve an entry's full path from parent indicators (0xFFFF = root).
pub fn entry_full_path(entries: &[StfsFileEntry], index: usize) -> String {
    let mut parts = Vec::new();
    let mut cur = index;
    let mut guard = 0usize;
    loop {
        guard += 1;
        let Some(e) = entries.iter().find(|e| e.index == cur) else {
            break;
        };
        parts.push(e.name.clone());
        if e.parent == 0xFFFF || guard > entries.len() + 1 {
            break;
        }
        let parent = e.parent as usize;
        if parent == cur {
            break;
        }
        cur = parent;
    }
    parts.reverse();
    parts.join("/")
}

/// Read one file's bytes by walking its hash chain, verifying every block.
pub fn read_stfs_file(
    reader: &mut (impl Read + Seek),
    layout: &StfsLayout,
    start_block: u64,
    size: u64,
) -> Result<Vec<u8>> {
    if size == 0 {
        return Ok(Vec::new());
    }
    let need = size.div_ceil(STFS_BLOCK);
    let mut out = Vec::with_capacity(size.min(1 << 30) as usize);
    let mut seen = std::collections::HashSet::new();
    let mut blk = start_block;
    let mut blocks_read = 0u64;
    for _ in 0..need {
        if blk >= layout.alloc_blocks || !seen.insert(blk) {
            return Err(StfsError::BrokenChain(blk).into());
        }
        let table = blk / STFS_HASH_SPAN;
        let slot = blk % STFS_HASH_SPAN;
        let (hash, status, next) = layout.read_l0_record(reader, table, slot)?;
        if status != 0x80 {
            return Err(StfsError::BadStatus(status, blk).into());
        }
        let off = layout.data_offset(blk)?;
        let data = read_at(reader, off, STFS_BLOCK as usize)?;
        if sha1_of(&data) != hash {
            return Err(anyhow::anyhow!("block {blk} hash mismatch"));
        }
        out.extend_from_slice(&data);
        blocks_read += 1;
        if next == 0xFFFFFF {
            break;
        }
        blk = next;
    }
    if blocks_read != need {
        return Err(StfsError::BrokenChain(start_block).into());
    }
    out.truncate(size as usize);
    Ok(out)
}

/// Discover the file-table entries: read the table region (location from
/// the volume descriptor, 0xC000 in all observed files), stop at the first
/// all-zero slot. Returns the layout, the accepted entries (slot indices
/// count every slot, since path indicators refer to them) and the raw
/// bytes of [0, table_end) for string scanning. Falls back to legacy
/// bounds when the layout header itself is unreadable.
#[allow(dead_code)]
pub fn discover_entries(
    reader: &mut (impl Read + Seek),
    file_size: u64,
) -> Result<(StfsLayout, Vec<StfsFileEntry>, Vec<u8>)> {
    const MAX_SLOTS: usize = 4096;
    let layout = StfsLayout::read(reader, file_size).unwrap_or(StfsLayout {
        base: 0xB000,
        table_off: 0xC000,
        table_blocks: file_size.saturating_sub(0xC000) / STFS_BLOCK,
        alloc_blocks: u64::MAX,
        top_hash: [0u8; 20],
        header_sha1: [0u8; 20],
        file_size,
    });
    let table_end = (layout.table_off + (MAX_SLOTS as u64) * 64).min(file_size);
    let buffer = if table_end > 0 {
        read_at(reader, 0, table_end as usize)?
    } else {
        Vec::new()
    };
    let mut entries = Vec::new();
    let mut offset = layout.table_off;
    let mut index = 0usize;
    while offset + 64 <= buffer.len() as u64 {
        let entry_data = &buffer[offset as usize..offset as usize + 64];
        if entry_data.iter().all(|&b| b == 0) {
            break;
        }
        if let Some(entry) = decode_slot(entry_data, index, offset) {
            entries.push(entry);
        }
        index += 1;
        offset += 64;
    }
    Ok((layout, entries, buffer))
}

/// Extract all files of an STFS container into `out_dir`, recreating the
/// directory tree. Every block hash is verified during the walk.
#[allow(dead_code)]
pub fn extract_stfs(
    reader: &mut (impl Read + Seek),
    file_size: u64,
    out_dir: &std::path::Path,
) -> Result<Vec<(String, u64)>> {
    let (layout, entries, _) = discover_entries(reader, file_size)?;
    layout.num_l0_tables()?;
    let mut done = Vec::new();
    for e in &entries {
        let rel = entry_full_path(&entries, e.index);
        let dest = out_dir.join(&rel);
        if e.is_dir {
            std::fs::create_dir_all(&dest)?;
            continue;
        }
        if let Some(parent) = dest.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let data = match read_stfs_file(reader, &layout, e.start_block, e.size) {
            Ok(d) => d,
            Err(err) => {
                let _ = std::fs::remove_file(&dest);
                return Err(err);
            }
        };
        std::fs::write(&dest, &data)?;
        done.push((rel, data.len() as u64));
    }
    Ok(done)
}

/// Open a fresh streaming reader for an STFS container.
pub(crate) fn open_stfs_reader(path: &PathBuf) -> Result<(BufReader<File>, u64)> {
    let file = File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let file_size = file.metadata()?.len();
    Ok((BufReader::new(file), file_size))
}

/// Extract dashboard thumbnails of a parsed STFS report into `out_dir`,
/// named `<base-title-id>_<kind>.<format>`. Returns files written.
pub(crate) fn extract_thumbnails(
    reader: &mut (impl Read + Seek),
    info: &TuInfo,
    out_dir: &Path,
) -> Result<u64> {
    use crate::tu_header::sniff_image_format;

    std::fs::create_dir_all(out_dir)?;
    let mut count = 0u64;
    for t in &info.thumbnails {
        let data = read_at(reader, t.offset, t.size as usize)?;
        let sniffed = sniff_image_format(&data);
        let (bytes, ext) = if sniffed == "xpr" {
            match crate::xpr::decode_xpr_to_png(&data) {
                Ok(png_bytes) => (png_bytes, "png"),
                Err(e) => {
                    eprintln!(
                        "warning: {} thumbnail XPR decode failed ({e}); writing raw blob",
                        t.kind
                    );
                    (data, "xpr")
                }
            }
        } else {
            let ext = match sniffed {
                "png" => "png",
                "dds" => "dds",
                "bmp" => "bmp",
                "jpg" => "jpg",
                _ => "bin",
            };
            (data, ext)
        };
        let out_path = out_dir.join(format!(
            "{}_{}.{}",
            sanitize_dir_name(&info.base_title_id),
            t.kind,
            ext
        ));
        std::fs::write(&out_path, &bytes)?;
        count += 1;
    }
    Ok(count)
}
/// Extract one parsed STFS report into `out_dir`, recreating the tree.
/// Every block hash is verified during the walk; any failure aborts and
/// removes the partial file.
pub(crate) fn extract_report(
    reader: &mut (impl Read + Seek),
    file_size: u64,
    info: &TuInfo,
    out_dir: &Path,
    log: &dyn Fn(String),
    title_id: Option<&str>,
) -> Result<u64> {
    let layout = StfsLayout::read(reader, file_size)?;
    // Refuse absurd sizes (> 170 level-1 tables) before writing anything.
    layout.num_l0_tables()?;

    // Create subfolder with title_id if provided
    let final_out_dir = if let Some(tid) = title_id {
        let safe_name = sanitize_dir_name(tid);
        out_dir.join(safe_name)
    } else {
        out_dir.to_path_buf()
    };
    std::fs::create_dir_all(&final_out_dir)?;
    let mut count = 0u64;
    for e in &info.directory_entries {
        let dest = final_out_dir.join(&e.path);
        if e.is_dir {
            std::fs::create_dir_all(&dest)?;
            continue;
        }
        if let Some(parent) = dest.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let data = match read_stfs_file(reader, &layout, e.start_block, e.size) {
            Ok(d) => d,
            Err(err) => {
                let _ = std::fs::remove_file(&dest);
                return Err(err);
            }
        };
        std::fs::write(&dest, &data)?;
        count += 1;
        log(format!(
            "extracted {} ({})",
            dest.display(),
            format_size(e.size)
        ));
    }
    log(format!(
        "{} file(s) extracted to {}",
        count,
        final_out_dir.display()
    ));
    Ok(count)
}
