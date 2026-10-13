//! Command-line arguments and input collection.

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "tu_parser",
    version,
    about = "Xbox 360 container and image parser (STFS, XISO, XEX, GoD)"
)]
pub(crate) struct Args {
    #[arg()]
    pub files: Vec<PathBuf>,

    #[arg(long, conflicts_with = "files")]
    pub dir: Option<PathBuf>,

    #[arg(long, requires = "dir")]
    pub recursive: bool,

    #[arg(long, conflicts_with_all = ["files", "dir", "extract", "bench_extract_iso"])]
    pub game_dir: Option<PathBuf>,

    #[arg(short, long, default_value = "text")]
    pub format: OutputFormat,

    #[arg(short, long)]
    pub output: Option<PathBuf>,

    #[arg(long)]
    pub show_strings: bool,

    #[arg(long)]
    pub show_entries: bool,

    /// Verify STFS hashes (header, tables, data blocks).
    #[arg(long)]
    pub verify: bool,

    /// Extract STFS contents into OUTDIR (hash-verified during extraction).
    #[arg(long, value_name = "OUTDIR")]
    pub extract: Option<PathBuf>,

    /// Extract XISO contents into OUTDIR (fast, multi-method).
    #[arg(long, value_name = "OUTDIR", conflicts_with = "extract")]
    pub extract_iso: Option<PathBuf>,

    /// Benchmark all XISO extraction methods and show fastest.
    #[arg(long, conflicts_with = "extract")]
    pub bench_extract_iso: bool,

    #[arg(short, long)]
    pub verbose: bool,
}

#[derive(Debug, Clone, clap::ValueEnum, PartialEq)]
pub(crate) enum OutputFormat {
    Text,
    Json,
}

pub(crate) fn collect_files(
    files: &[PathBuf],
    dir: Option<&PathBuf>,
    recursive: bool,
) -> Result<Vec<PathBuf>> {
    let mut result = Vec::new();

    // Add explicit files
    result.extend(files.iter().cloned());

    // Add files from directory
    if let Some(dir) = dir {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                result.push(path);
            } else if path.is_dir() && recursive {
                // Recursively collect files from subdirectory
                let mut sub_files = collect_files(&[], Some(&path), true)?;
                result.append(&mut sub_files);
            }
        }
    }

    Ok(result)
}
