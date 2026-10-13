//! Xbox 360 container and image parser (STFS, XISO, XEX, GoD).

mod cli;
mod constants;
mod extract;
mod iso;
mod parse;
mod regions;
mod render;
mod report;
mod stfs;
#[cfg(test)]
mod tests;
mod tu_header;
mod tu_parser;
mod xex;

use anyhow::Result;
use clap::Parser;

use crate::cli::{collect_files, Args, OutputFormat};
use crate::extract::{benchmark_extract_iso, extract_iso_fast_with_progress};
use crate::parse::{parse_file, parse_game_dir};
use crate::render::{emit, report_summary};
use crate::report::Report;
use crate::stfs::{extract_report, open_stfs_reader, verify_stfs};

fn main() -> Result<()> {
    let args = Args::parse();

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
                if args.verify || args.extract.is_some() {
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
                        match extract_iso_fast_with_progress(
                            file_path,
                            outdir,
                            |msg| eprintln!("{}", msg),
                            title_id,
                        ) {
                            Ok(res) => {
                                eprintln!(
                                    "  extracted {} files ({:.1} MB/s) via {}",
                                    res.files_extracted, res.throughput_mb_s, res.method
                                );
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

    emit(&args, &all_results, args.show_entries, args.show_strings)?;
    if verify_failed {
        std::process::exit(1);
    }
    Ok(())
}
