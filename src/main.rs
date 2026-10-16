//! Xbox 360 container and image parser (STFS, XISO, XEX, GoD).

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
