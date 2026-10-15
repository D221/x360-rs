//! Truncate an XISO to its last used sector (Iso2God "Partial" equivalent).
//!
//! Unlike `--optimize` (full rebuild with contiguous sectors), truncation
//! preserves the original layout byte-for-byte and only drops trailing
//! sectors past the last referenced one (volume descriptor, root table,
//! directory tables, file data). Output keeps the input format.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use anyhow::{Context, Result};

use crate::iso::IsoInfo;
use crate::write::format::iso_type_offset;
use crate::write::sector::{required_sectors, SECTOR_SIZE};

/// Last referenced sector (volume-relative), covering the volume descriptor
/// area, the root table and every entry (files and directory tables).
pub fn last_used_sector(info: &IsoInfo) -> u32 {
    let mut last = 33u32; // volume descriptor lives at sector 32
    last = last.max(info.root_sector + required_sectors(info.root_size as u64));
    for e in &info.entries {
        last = last.max(e.sector + required_sectors(e.size));
    }
    last
}

/// Copy `input[0..cut)` to `output`, padded to a 32-sector boundary like
/// the image writer. Returns the output size in bytes.
pub fn truncate_iso_to_last_used(input: &Path, output: &Path, info: &IsoInfo) -> Result<u64> {
    const BUF: usize = 1024 * 1024;
    // Truncation copies input bytes verbatim, so the output keeps the
    // input format (and its base offset) automatically.
    let base = iso_type_offset(&info.iso_type);
    let end_rel = last_used_sector(info) as u64 * SECTOR_SIZE;
    let end_padded = if end_rel.is_multiple_of(32 * SECTOR_SIZE) {
        end_rel
    } else {
        end_rel + (32 * SECTOR_SIZE - end_rel % (32 * SECTOR_SIZE))
    };
    let cut = base + end_rel;

    let mut src = File::open(input).with_context(|| format!("open {}", input.display()))?;
    let mut out = File::create(output).with_context(|| format!("create {}", output.display()))?;
    src.seek(SeekFrom::Start(0))?;
    let mut remaining = cut;
    let mut buf = vec![0u8; BUF];
    while remaining > 0 {
        let n = (remaining as usize).min(BUF);
        src.read_exact(&mut buf[..n]).context("read input")?;
        out.write_all(&buf[..n]).context("write output")?;
        remaining -= n as u64;
    }
    out.set_len(base + end_padded).context("pad output")?;
    Ok(base + end_padded)
}
