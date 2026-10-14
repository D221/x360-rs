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

    /// Optimize/repack existing XISO (removes padding, contiguous sectors).
    /// Uses `-o/--output` as the destination ISO path.
    #[arg(
        long,
        conflicts_with_all = ["create", "game_dir", "dir", "extract", "extract_iso"]
    )]
    pub optimize: bool,

    /// Create a new XISO from an extracted game directory.
    /// Uses `-o/--output` as the destination ISO path.
    #[arg(
        long,
        value_name = "DIR",
        conflicts_with_all = [
            "optimize",
            "game_dir",
            "dir",
            "extract",
            "extract_iso",
            "files"
        ]
    )]
    pub create: Option<PathBuf>,

    /// Output image format for --create (default: xsf).
    /// For --optimize the input format is preserved.
    #[arg(long, value_name = "FORMAT", default_value = "xsf")]
    pub create_format: XisoFormat,

    /// Overwrite input file in place (creates a .bak backup first).
    #[arg(long, requires = "optimize")]
    pub in_place: bool,

    /// Re-parse the optimized output and verify entry fidelity.
    #[arg(long, requires = "optimize")]
    pub verify_output: bool,

    #[arg(short, long)]
    pub verbose: bool,
}

#[derive(Debug, Clone, clap::ValueEnum, PartialEq)]
pub(crate) enum OutputFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum, PartialEq, Eq)]
pub(crate) enum XisoFormat {
    Xsf,
    Xgd2,
    Xgd3,
    Xgd1,
}

impl From<XisoFormat> for crate::write::format::OutputFormat {
    fn from(value: XisoFormat) -> Self {
        match value {
            XisoFormat::Xsf => crate::write::format::OutputFormat::Xsf,
            XisoFormat::Xgd2 => crate::write::format::OutputFormat::Xgd2,
            XisoFormat::Xgd3 => crate::write::format::OutputFormat::Xgd3,
            XisoFormat::Xgd1 => crate::write::format::OutputFormat::Xgd1,
        }
    }
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
