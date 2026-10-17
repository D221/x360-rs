//! Xbox 360 container and image parser (STFS, XISO, XEX, GoD).

mod account;
mod cli;
mod constants;
mod extract;
mod gpd;
mod iso;
mod parse;
mod regions;
mod render;
mod report;
mod stfs;
#[cfg(test)]
mod tests;
mod title_id;
mod tu_header;
mod tu_parser;
mod write;
mod xdbf;
mod xex;
mod xpr;
use anyhow::{Context, Result};
use clap::Parser;

use crate::cli::{collect_files, Args, OutputFormat};
use crate::extract::{
    benchmark_extract_iso, checksum_iso, copy_out_iso, extract_iso_fast_with_progress, md5_iso,
};
use crate::parse::{parse_file, parse_game_dir, parse_iso};
use crate::render::{emit, report_summary};
use crate::report::Report;
use crate::stfs::{extract_report, open_stfs_reader, verify_stfs};
use crate::tu_parser::format_size;
use crate::write::{create_xiso_from_dir_with_progress, create_xiso_from_mapped_dir_with_progress};
use crate::write::{create_xiso_from_parsed_with_progress, remap::parse_map_rule};

fn detect_output_format(iso_type: &str) -> crate::write::format::OutputFormat {
    match iso_type {
        "XGD2" => crate::write::format::OutputFormat::Xgd2,
        "XGD3" => crate::write::format::OutputFormat::Xgd3,
        "XGD1" => crate::write::format::OutputFormat::Xgd1,
        _ => crate::write::format::OutputFormat::Xsf,
    }
}

/// Restore `input` from `backup` after a failed in-place optimize.
///
/// The failed write leaves a partial file at `input`; on Windows `rename`
/// fails when the target exists, so the partial file must be removed first.
/// Both operations are best-effort: there is nothing further to recover if
/// the restore itself fails.
pub(crate) fn restore_backup(backup: &std::path::Path, input: &std::path::Path) {
    std::fs::remove_file(input).ok();
    std::fs::rename(backup, input).ok();
}

fn optimize_one(
    src: &std::path::Path,
    out: &std::path::Path,
    info: &crate::iso::IsoInfo,
    fmt: crate::write::format::OutputFormat,
    args: &Args,
) -> Result<()> {
    let log = |m: String| {
        if args.verbose {
            eprintln!("{m}");
        }
    };
    create_xiso_from_parsed_with_progress(
        info,
        out,
        fmt,
        &log,
        &|m| {
            if !args.quiet {
                eprintln!("{m}");
            }
        },
        !args.no_media_patch,
    )?;
    if args.verbose {
        eprintln!("optimized {} -> {}", src.display(), out.display());
    }
    if args.verify_output {
        let out_meta =
            std::fs::metadata(out).with_context(|| format!("cannot stat {}", out.display()))?;
        let out_info = parse_iso(&out.to_path_buf(), out_meta.len())?;
        verify_optimize_fidelity(info, &out_info, !args.no_media_patch).with_context(|| {
            format!("optimized output verification failed for {}", out.display())
        })?;
        if args.verbose {
            eprintln!("verified {}", out.display());
        }
    }
    Ok(())
}

fn truncate_one(
    src: &std::path::Path,
    out: &std::path::Path,
    info: &crate::iso::IsoInfo,
    args: &Args,
) -> Result<()> {
    let out_size = crate::write::truncate::truncate_iso_to_last_used(src, out, info)?;
    if args.verbose {
        eprintln!(
            "truncated {} -> {} ({} bytes)",
            src.display(),
            out.display(),
            out_size
        );
    }
    if args.verify_output {
        let out_info = parse_iso(
            &out.to_path_buf(),
            std::fs::metadata(out)
                .with_context(|| format!("cannot stat {}", out.display()))?
                .len(),
        )?;
        verify_optimize_fidelity(info, &out_info, false).with_context(|| {
            format!("truncated output verification failed for {}", out.display())
        })?;
        if args.verbose {
            eprintln!("verified {}", out.display());
        }
    }
    Ok(())
}

fn verify_optimize_fidelity(
    before: &crate::iso::IsoInfo,
    after: &crate::iso::IsoInfo,
    media_patch: bool,
) -> Result<()> {
    use std::collections::BTreeMap;
    if before.entries.len() != after.entries.len() {
        anyhow::bail!(
            "entry count mismatch: {} vs {}",
            before.entries.len(),
            after.entries.len()
        );
    }
    if before.volume_timestamp != after.volume_timestamp {
        anyhow::bail!("volume timestamp changed");
    }
    let mut a: BTreeMap<(&str, u64), &crate::iso::IsoEntry> = BTreeMap::new();
    for e in &before.entries {
        if a.insert((e.path.as_str(), e.size), e).is_some() {
            anyhow::bail!("duplicate entry in source: {}", e.path);
        }
    }
    for e in &after.entries {
        match a.remove(&(e.path.as_str(), e.size)) {
            Some(src) => {
                if src.is_dir != e.is_dir {
                    anyhow::bail!("type changed for {}", e.path);
                }
                if src.attrs != e.attrs {
                    anyhow::bail!("attributes changed for {}", e.path);
                }
                if !e.is_dir && e.size > 0 {
                    compare_iso_file_bytes(before, src, after, e, media_patch)?;
                }
            }
            None => anyhow::bail!("unexpected entry in output: {}", e.path),
        }
    }
    if !a.is_empty() {
        anyhow::bail!("{} entries missing from output", a.len());
    }
    Ok(())
}

/// Stream-compare the data bytes of one file entry between two images.
/// When `media_patch` is set and the entry is a `.xbe` file, the source
/// bytes are patched before comparing (the output was patched on write).
fn compare_iso_file_bytes(
    before: &crate::iso::IsoInfo,
    b_entry: &crate::iso::IsoEntry,
    after: &crate::iso::IsoInfo,
    a_entry: &crate::iso::IsoEntry,
    media_patch: bool,
) -> Result<()> {
    use crate::write::img::{is_xbe_file, patch_media_enable};
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom};
    const BUF: usize = 1024 * 1024;
    let base_b = crate::write::iso_type_offset(&before.iso_type);
    let base_a = crate::write::iso_type_offset(&after.iso_type);
    let sector = crate::write::SECTOR_SIZE;
    let mut fb = File::open(&before.file_path)
        .with_context(|| format!("open {} for verify", before.file_path))?;
    let mut fa = File::open(&after.file_path)
        .with_context(|| format!("open {} for verify", after.file_path))?;
    fb.seek(SeekFrom::Start(base_b + b_entry.sector as u64 * sector))
        .context("seek source file data for verify")?;
    fa.seek(SeekFrom::Start(base_a + a_entry.sector as u64 * sector))
        .context("seek output file data for verify")?;
    let patch = media_patch && is_xbe_file(&a_entry.path);
    let mut remaining = b_entry.size;
    let mut bb = vec![0u8; BUF];
    let mut ba = vec![0u8; BUF];
    // Same 7-byte hold-back as the writer so chunk boundaries patch alike.
    let mut carry: Vec<u8> = Vec::new();
    while remaining > 0 {
        let n = (remaining as usize).min(BUF);
        fb.read_exact(&mut bb[..n]).context("read source bytes")?;
        remaining -= n as u64;
        if !patch {
            fa.read_exact(&mut ba[..n]).context("read output bytes")?;
            if bb[..n] != ba[..n] {
                anyhow::bail!("content differs for {}", a_entry.path);
            }
            continue;
        }
        let mut window = std::mem::take(&mut carry);
        window.extend_from_slice(&bb[..n]);
        patch_media_enable(&mut window);
        let keep = if remaining == 0 {
            0
        } else {
            7.min(window.len())
        };
        let (emit, rest) = window.split_at(window.len() - keep);
        fa.read_exact(&mut ba[..emit.len()])
            .context("read output bytes")?;
        if emit != &ba[..emit.len()] {
            anyhow::bail!("content differs for {}", a_entry.path);
        }
        carry = rest.to_vec();
    }
    if !carry.is_empty() {
        fa.read_exact(&mut ba[..carry.len()])
            .context("read output bytes")?;
        if carry[..] != ba[..carry.len()] {
            anyhow::bail!("content differs for {}", a_entry.path);
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();

    if args.verify_output && !(args.optimize || args.truncate) {
        anyhow::bail!("--verify-output requires --optimize or --truncate");
    }
    if args.in_place && !(args.optimize || args.truncate) {
        anyhow::bail!("--in-place requires --optimize or --truncate");
    }

    // Copy-out mode: copy a single file or directory out of one XISO image
    // (same as xdvdfs copy-out). Returns before the parse sweep so the
    // report emitter cannot overwrite the destination file.
    if let Some(src_path) = &args.copy_out {
        if args.files.len() != 1 {
            anyhow::bail!("--copy-out requires exactly one input file");
        }
        let input = &args.files[0];
        let dest = args
            .output
            .clone()
            .ok_or_else(|| anyhow::anyhow!("--copy-out requires -o/--output destination"))?;
        let meta =
            std::fs::metadata(input).with_context(|| format!("cannot stat {}", input.display()))?;
        let info = parse_iso(input, meta.len())?;
        match copy_out_iso(&info, src_path, &dest, args.skip_systemupdate) {
            Ok(res) => {
                if !args.quiet {
                    eprintln!(
                        "copied {} files ({} bytes) from {}",
                        res.files_copied, res.bytes_copied, src_path
                    );
                }
                return Ok(());
            }
            Err(e) => {
                anyhow::bail!("copy-out {}: {:#}", input.display(), e);
            }
        }
    }

    // Account mode: decrypt and dump an Xbox 360 Profile Account file.
    if let Some(account_path) = &args.dump_account {
        let meta = std::fs::metadata(account_path)
            .with_context(|| format!("cannot stat {}", account_path.display()))?;
        let mut data = vec![0u8; meta.len() as usize];
        let mut f = std::fs::File::open(account_path)
            .with_context(|| format!("cannot open {}", account_path.display()))?;
        use std::io::Read as _;
        f.read_exact(&mut data)?;
        let decrypted = crate::account::decrypt_account(&data)?;
        let info = crate::account::parse_account(&decrypted)?;
        let redacted = crate::account::redact_account(&info);
        if let Some(dest) = &args.output {
            let json = serde_json::to_string_pretty(&redacted)?;
            std::fs::write(dest, &json)?;
            if !args.quiet {
                eprintln!("dumped account to {}", dest.display());
            }
        } else {
            let json = serde_json::to_string_pretty(&redacted)?;
            println!("{json}");
        }
        return Ok(());
    }

    // Create mode: build a new XISO from an extracted game directory.
    if let Some(dir) = &args.create {
        let out_path = args
            .output
            .clone()
            .unwrap_or_else(|| dir.with_extension("iso"));
        if out_path == *dir {
            anyhow::bail!("source and destination paths are the same");
        }
        if out_path.starts_with(dir) {
            anyhow::bail!("destination path is contained by source path");
        }
        let fmt = args.create_format;
        let log = |m: String| {
            if args.verbose {
                eprintln!("{m}");
            }
        };
        let quiet = args.quiet;
        let progress = move |m: String| {
            if !quiet {
                eprintln!("{m}");
            }
        };
        if args.map.is_empty() && !args.dry_run {
            create_xiso_from_dir_with_progress(
                dir,
                &out_path,
                fmt,
                &log,
                &progress,
                args.skip_systemupdate,
                !args.no_media_patch,
            )?;
        } else {
            let mut rules = Vec::with_capacity(args.map.len());
            for rule in &args.map {
                rules.push(parse_map_rule(rule)?);
            }
            create_xiso_from_mapped_dir_with_progress(
                dir,
                &out_path,
                fmt,
                &log,
                &progress,
                args.skip_systemupdate,
                &rules,
                args.dry_run,
                !args.no_media_patch,
            )?;
        }
        if !args.quiet && !args.dry_run {
            eprintln!("created {} from {}", out_path.display(), dir.display());
        }
        return Ok(());
    }

    // Optimize mode: rebuild existing XISO(s) with contiguous sectors.
    if args.optimize {
        if args.files.is_empty() {
            anyhow::bail!("no input: pass FILES with --optimize");
        }
        if args.output.is_some() && args.files.len() > 1 && !args.in_place {
            anyhow::bail!("--output with multiple inputs requires --in-place");
        }
        let mut failures: u64 = 0;
        const XISO_OPTIMIZED_TAG_OFFSET: u64 = 31337;
        const XISO_OPTIMIZED_TAG_MIN: &str = "in!xiso!";
        for input in &args.files {
            let result = (|| -> Result<()> {
                let meta = std::fs::metadata(input)
                    .with_context(|| format!("cannot stat {}", input.display()))?;
                let info = parse_iso(input, meta.len())?;
                let fmt = if args.preserve_format {
                    detect_output_format(&info.iso_type)
                } else {
                    crate::write::format::OutputFormat::Xsf
                };
                // Same as extract-xiso -s: drop $SystemUpdate before rebuilding.
                let info = if args.skip_systemupdate {
                    let mut filtered = info.clone();
                    filtered
                        .entries
                        .retain(|e| !crate::iso::is_systemupdate_path(&e.path));
                    filtered
                } else {
                    info
                };
                // Check for extract-xiso "already optimized" tag at offset 31337.
                let mut optimized_tag_found = false;
                if meta.len() > XISO_OPTIMIZED_TAG_OFFSET + XISO_OPTIMIZED_TAG_MIN.len() as u64 {
                    let mut file = std::fs::File::open(input).with_context(|| {
                        format!("open {} for optimized tag check", input.display())
                    })?;
                    use std::io::{Read, Seek, SeekFrom};
                    file.seek(SeekFrom::Start(XISO_OPTIMIZED_TAG_OFFSET))
                        .context("seek tag offset")?;
                    let mut buf = [0u8; 8];
                    if file.read_exact(&mut buf).is_ok()
                        && std::str::from_utf8(&buf)
                            .is_ok_and(|s| s.starts_with(XISO_OPTIMIZED_TAG_MIN))
                    {
                        optimized_tag_found = true;
                    }
                }
                if optimized_tag_found {
                    eprintln!("{} is already optimized, skipping...", input.display());
                    return Ok(());
                }
                if args.in_place {
                    let backup = input.with_extension("bak.iso");
                    std::fs::rename(input, &backup).with_context(|| {
                        format!("cannot back up {} to {}", input.display(), backup.display())
                    })?;
                    match optimize_one(&backup, input, &info, fmt, &args) {
                        Ok(()) => {
                            std::fs::remove_file(&backup).ok();
                            Ok(())
                        }
                        Err(e) => {
                            // Restore original on failure.
                            restore_backup(&backup, input);
                            Err(e)
                        }
                    }
                } else {
                    let out = args
                        .output
                        .clone()
                        .unwrap_or_else(|| input.with_extension("opt.iso"));
                    if out == *input {
                        anyhow::bail!(
                            "source and destination are the same (use --in-place to overwrite)"
                        );
                    }
                    optimize_one(input, &out, &info, fmt, &args)
                }?;
                Ok(())
            })();
            if let Err(e) = result {
                failures += 1;
                eprintln!("warning: optimize {}: {:#}", input.display(), e);
            }
        }
        if failures > 0 {
            anyhow::bail!("{failures} file(s) failed to optimize");
        }
        return Ok(());
    }

    // Truncate mode: drop trailing sectors past the last used one,
    // preserving layout and format (Iso2God "Partial" equivalent).
    if args.truncate {
        if args.files.is_empty() {
            anyhow::bail!("no input: pass FILES with --truncate");
        }
        if args.output.is_some() && args.files.len() > 1 && !args.in_place {
            anyhow::bail!("--output with multiple inputs requires --in-place");
        }
        let mut failures: u64 = 0;
        for input in &args.files {
            let result = (|| -> Result<()> {
                let meta = std::fs::metadata(input)
                    .with_context(|| format!("cannot stat {}", input.display()))?;
                let info = parse_iso(input, meta.len())?;
                if args.in_place {
                    let backup = input.with_extension("bak.iso");
                    std::fs::rename(input, &backup).with_context(|| {
                        format!("cannot back up {} to {}", input.display(), backup.display())
                    })?;
                    match truncate_one(&backup, input, &info, &args) {
                        Ok(()) => {
                            std::fs::remove_file(&backup).ok();
                            Ok(())
                        }
                        Err(e) => {
                            // Restore original on failure.
                            restore_backup(&backup, input);
                            Err(e)
                        }
                    }
                } else {
                    let out = args
                        .output
                        .clone()
                        .unwrap_or_else(|| input.with_extension("trunc.iso"));
                    if out == *input {
                        anyhow::bail!(
                            "source and destination are the same (use --in-place to overwrite)"
                        );
                    }
                    truncate_one(input, &out, &info, &args)
                }?;
                Ok(())
            })();
            if let Err(e) = result {
                failures += 1;
                eprintln!("warning: truncate {}: {:#}", input.display(), e);
            }
        }
        if failures > 0 {
            anyhow::bail!("{failures} file(s) failed to truncate");
        }
        return Ok(());
    }

    // Extracted game directory mode.
    if let Some(game_dir) = &args.game_dir {
        let info = parse_game_dir(game_dir)?;
        return emit(
            &args,
            &[Report::GameDir(info)],
            args.show_entries,
            args.show_strings,
        );
    }

    if args.files.is_empty() && args.dir.is_none() {
        anyhow::bail!("no input: pass FILES, --dir <DIR> or --game-dir <DIR>");
    }

    let file_paths = collect_files(&args.files, args.dir.as_ref(), args.recursive)?;

    if file_paths.is_empty() {
        eprintln!("No files to process");
        return Ok(());
    }

    if file_paths.is_empty() {
        eprintln!("No files to process");
        return Ok(());
    }

    // Sweep tolerance: unrecognized files warn and continue instead of
    // aborting the run (e.g. GOD DataNNNN parts in a directory walk).
    let mut all_results = Vec::new();
    let mut failures: u64 = 0;
    let mut verify_failed = false;
    let json_mode = matches!(args.format, OutputFormat::Json);
    // Human log lines go to stdout in text mode, stderr in JSON mode
    // (stdout must stay pure JSON).
    let log = |s: String| {
        if json_mode {
            eprintln!("{s}");
        } else {
            println!("{s}");
        }
    };
    for file_path in &file_paths {
        if args.verbose {
            eprintln!("Parsing: {}", file_path.display());
        }
        match parse_file(file_path) {
            Ok(Report::Stfs(mut info)) => {
                if args.verbose {
                    eprintln!("  -> {}", report_summary(&Report::Stfs(info.clone())));
                }
                if args.verify || args.extract.is_some() || args.extract_thumbnail.is_some() {
                    match open_stfs_reader(file_path) {
                        Ok((mut reader, file_size)) => {
                            if args.verify {
                                match verify_stfs(&mut reader, file_size) {
                                    Ok(rep) => {
                                        if !rep.ok {
                                            verify_failed = true;
                                        }
                                        info.verify = Some(rep);
                                    }
                                    Err(e) => {
                                        verify_failed = true;
                                        eprintln!(
                                            "warning: verify {}: {:#}",
                                            file_path.display(),
                                            e
                                        );
                                    }
                                }
                            }
                            if let Some(outdir) = &args.extract {
                                match extract_report(
                                    &mut reader,
                                    file_size,
                                    &info,
                                    outdir,
                                    &log,
                                    Some(&info.base_title_id),
                                ) {
                                    Ok(n) => {
                                        if args.verbose {
                                            eprintln!("  extracted {n} files");
                                        }
                                    }
                                    Err(e) => {
                                        failures += 1;
                                        eprintln!(
                                            "warning: extract {}: {:#}",
                                            file_path.display(),
                                            e
                                        );
                                        continue;
                                    }
                                }
                            }
                            if let Some(thumbdir) = &args.extract_thumbnail {
                                match crate::stfs::extract_thumbnails(&mut reader, &info, thumbdir)
                                {
                                    Ok(n) => {
                                        if args.verbose {
                                            eprintln!("  extracted {n} thumbnails");
                                        }
                                    }
                                    Err(e) => {
                                        failures += 1;
                                        eprintln!(
                                            "warning: thumbnails {}: {:#}",
                                            file_path.display(),
                                            e
                                        );
                                        continue;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            failures += 1;
                            eprintln!("warning: skipping {}: {:#}", file_path.display(), e);
                            continue;
                        }
                    }
                }
                all_results.push(Report::Stfs(info));
            }
            Ok(other) => {
                if args.verbose {
                    eprintln!("  -> {}", report_summary(&other));
                }
                // XEX basefile dump: extract the raw basefile (PE executable)
                // from the XEX container to a file.
                if let Some(dest) = &args.dump_basefile {
                    if let Report::Xex(info) = &other {
                        let Some(off) = info.extended.as_ref().and_then(|e| e.basefile_offset)
                        else {
                            eprintln!(
                                "warning: {}: no basefile offset in XEX, skipping dump",
                                file_path.display()
                            );
                            failures += 1;
                            continue;
                        };
                        let Some(size) = info.extended.as_ref().and_then(|e| e.basefile_size)
                        else {
                            eprintln!(
                                "warning: {}: no basefile size in XEX, skipping dump",
                                file_path.display()
                            );
                            failures += 1;
                            continue;
                        };
                        if !args.quiet {
                            eprintln!(
                                "  dumping basefile ({} bytes) from {} to {}",
                                format_size(size),
                                file_path.display(),
                                dest.display()
                            );
                        }
                        match (|| -> Result<()> {
                            let mut f = std::fs::File::open(file_path)
                                .with_context(|| format!("cannot open {}", file_path.display()))?;
                            use std::io::{Read, Seek, SeekFrom};
                            let meta = std::fs::metadata(file_path)
                                .with_context(|| format!("cannot stat {}", file_path.display()))?;
                            let file_size = meta.len();
                            let read_size = size.min(file_size.saturating_sub(off));
                            f.seek(SeekFrom::Start(off))?;
                            let mut data = vec![0u8; read_size as usize];
                            f.read_exact(&mut data)?;
                            std::fs::write(dest, &data)?;
                            Ok(())
                        })() {
                            Ok(()) => {
                                if !args.quiet {
                                    eprintln!("  dumped basefile to {}", dest.display());
                                }
                            }
                            Err(e) => {
                                failures += 1;
                                eprintln!(
                                    "warning: dump-basefile {}: {:#}",
                                    file_path.display(),
                                    e
                                );
                                continue;
                            }
                        }
                    } else {
                        eprintln!(
                            "warning: {}: --dump-basefile only works for XEX files, skipping",
                            file_path.display()
                        );
                        failures += 1;
                        continue;
                    }
                }
                // XEX resource dumping: extract XEX resources (SPA blobs,
                // game icons) to files using flat virtual-address mapping.
                if let Some(res_dir) = &args.dump_resources {
                    if let Report::Xex(info) = &other {
                        let Some(ext) = &info.extended else {
                            eprintln!(
                                "warning: {}: no extended XEX info, skipping resource dump",
                                file_path.display()
                            );
                            failures += 1;
                            continue;
                        };
                        // Find resource directory offset from image entry 0x000002FF.
                        let Some(res_off_hex) = ext
                            .headers
                            .iter()
                            .find(|h| h.id == "000002FF")
                            .map(|h| h.data.trim_start_matches("0x").trim_start_matches('0'))
                        else {
                            eprintln!(
                                "warning: {}: no resource section in XEX, skipping dump",
                                file_path.display()
                            );
                            failures += 1;
                            continue;
                        };
                        let Ok(res_off) = u64::from_str_radix(res_off_hex, 16) else {
                            eprintln!(
                                "warning: {}: invalid resource section offset, skipping dump",
                                file_path.display()
                            );
                            failures += 1;
                            continue;
                        };
                        if !args.quiet {
                            eprintln!(
                                "  dumping resources from {} to {}",
                                file_path.display(),
                                res_dir.display()
                            );
                        }
                        match (|| -> Result<()> {
                            use std::io::{Read, Seek, SeekFrom};
                            let meta = std::fs::metadata(file_path)
                                .with_context(|| format!("cannot stat {}", file_path.display()))?;
                            let mut f = std::fs::File::open(file_path)
                                .with_context(|| format!("cannot open {}", file_path.display()))?;
                            std::fs::create_dir_all(res_dir)?;
                            // Read resource directory: s32 infoSize + entries.
                            // infoSize includes itself; entries are 16 bytes each
                            // (8 char name + u32 BE addr + u32 BE size), all big-endian.
                            f.seek(SeekFrom::Start(res_off))?;
                            let mut info_size_buf = [0u8; 4];
                            f.read_exact(&mut info_size_buf)?;
                            let info_size = i32::from_be_bytes(info_size_buf) as u64;
                            if info_size < 20 {
                                anyhow::bail!(
                                    "resource directory too small (infoSize={info_size})"
                                );
                            }
                            let entry_count = ((info_size - 4) / 16) as usize;
                            if entry_count == 0 {
                                eprintln!("warning: {}: no resources found", file_path.display());
                                return Ok(());
                            }
                            let load_addr = ext.load_address.ok_or_else(|| {
                                anyhow::anyhow!("no load address for resource address mapping")
                            })?;
                            let base_off = ext.basefile_offset.ok_or_else(|| {
                                anyhow::anyhow!("no basefile offset for resource address mapping")
                            })?;
                            // Read each resource entry. Flat virtual-address ->
                            // file offset mapping: offset = basefile_offset + (addr - load_address).
                            // Exact for raw/unencrypted basefiles; encrypted basefiles
                            // yield encrypted data (no decryption keys available).
                            for i in 0..entry_count {
                                let entry_off = res_off
                                    .checked_add(4)
                                    .and_then(|o| o.checked_add(i as u64 * 16))
                                    .ok_or_else(|| {
                                        anyhow::anyhow!("resource entry offset overflow")
                                    })?;
                                f.seek(SeekFrom::Start(entry_off))?;
                                let mut entry = [0u8; 16];
                                f.read_exact(&mut entry)?;
                                let name = std::str::from_utf8(&entry[0..8])
                                    .unwrap_or("")
                                    .trim_end_matches('\0')
                                    .trim_end_matches(' ')
                                    .to_string();
                                if name.is_empty() {
                                    continue;
                                }
                                let rva =
                                    u32::from_be_bytes([entry[8], entry[9], entry[10], entry[11]]);
                                let size = u32::from_be_bytes([
                                    entry[12], entry[13], entry[14], entry[15],
                                ]);
                                if size == 0 {
                                    continue;
                                }
                                let rva_rel = rva.saturating_sub(load_addr);
                                let abs_off =
                                    base_off.checked_add(rva_rel as u64).ok_or_else(|| {
                                        anyhow::anyhow!("resource file offset overflow")
                                    })?;
                                let read_size = size.min(meta.len().saturating_sub(abs_off) as u32);
                                f.seek(SeekFrom::Start(abs_off))?;
                                let mut data = vec![0u8; read_size as usize];
                                f.read_exact(&mut data)?;
                                let out_path = res_dir.join(&name);
                                std::fs::write(&out_path, &data)?;
                                if !args.quiet {
                                    eprintln!(
                                        "  dumped resource {name} ({read_size} bytes) to {}",
                                        out_path.display()
                                    );
                                }
                            }
                            Ok(())
                        })() {
                            Ok(()) => {
                                if !args.quiet {
                                    eprintln!("  dumped resources to {}", res_dir.display());
                                }
                            }
                            Err(e) => {
                                failures += 1;
                                eprintln!(
                                    "warning: dump-resources {}: {:#}",
                                    file_path.display(),
                                    e
                                );
                                continue;
                            }
                        }
                    } else {
                        eprintln!(
                            "warning: {}: --dump-resources only works for XEX files, skipping",
                            file_path.display()
                        );
                        failures += 1;
                        continue;
                    }
                }
                if args.verify || args.extract.is_some() {
                    let is_xiso = matches!(&other, Report::Xiso(_));
                    if is_xiso && args.extract.is_some() {
                        eprintln!(
                            "warning: {}: --extract only works for STFS containers; use --extract-iso for XISO/GDF",
                            file_path.display()
                        );
                    } else if !is_xiso {
                        eprintln!(
                            "warning: {}: verify/extract only support STFS containers, skipping",
                            file_path.display()
                        );
                    }
                }
                // XISO extraction
                if let Report::Xiso(ref info) = &other {
                    if let Some(outdir) = &args.extract_iso {
                        let title_id = info.xex_exec.as_ref().map(|x| x.title_id.as_str());
                        let quiet = args.quiet;
                        match extract_iso_fast_with_progress(
                            file_path,
                            outdir,
                            move |msg| {
                                if !quiet {
                                    eprintln!("{}", msg);
                                }
                            },
                            title_id,
                            args.skip_systemupdate,
                        ) {
                            Ok(res) => {
                                if !args.quiet {
                                    eprintln!(
                                        "  extracted {} files ({:.1} MB/s) via {}",
                                        res.files_extracted, res.throughput_mb_s, res.method
                                    );
                                }
                            }
                            Err(e) => {
                                failures += 1;
                                eprintln!("warning: extract iso {}: {:#}", file_path.display(), e);
                                continue;
                            }
                        }
                    }
                    if args.bench_extract_iso {
                        let bench_dir = std::env::temp_dir()
                            .join(format!("tu_parser_bench_{}", std::process::id()));
                        match benchmark_extract_iso(file_path, &bench_dir) {
                            Ok(results) => {
                                if args.format == OutputFormat::Json {
                                    let json = serde_json::to_string_pretty(&results)?;
                                    println!("{}", json);
                                }
                            }
                            Err(e) => {
                                eprintln!(
                                    "warning: bench extract {}: {:#}",
                                    file_path.display(),
                                    e
                                );
                            }
                        }
                        std::fs::remove_dir_all(&bench_dir).ok();
                    }
                    // Content hash reports (xdvdfs md5/checksum equivalents).
                    // The output is the result itself, so --quiet suppresses
                    // it outright (same spirit as the parse report); warnings stay.
                    if (args.md5 || args.checksum) && !args.quiet {
                        if let Err(e) = (|| -> Result<()> {
                            if args.md5 {
                                for (path, digest) in md5_iso(info)? {
                                    println!("{digest}  /{path}");
                                }
                            }
                            if args.checksum {
                                println!("{}  {}", checksum_iso(info)?, file_path.display());
                            }
                            Ok(())
                        })() {
                            failures += 1;
                            eprintln!("warning: hash {}: {:#}", file_path.display(), e);
                        }
                    }
                }
                all_results.push(other);
            }
            Err(e) => {
                failures += 1;
                eprintln!("warning: skipping {}: {:#}", file_path.display(), e);
            }
        }
    }
    if all_results.is_empty() {
        anyhow::bail!("no files parsed successfully ({failures} failed)");
    }
    if failures > 0 {
        eprintln!("{failures} file(s) skipped");
    }

    // Quiet mode suppresses the console report (still written to --output
    // file when given); warnings and errors above always print.
    if !(args.quiet && args.output.is_none()) {
        emit(&args, &all_results, args.show_entries, args.show_strings)?;
    }
    if verify_failed {
        std::process::exit(1);
    }
    Ok(())
}
