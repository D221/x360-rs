//! Command-line arguments and input collection.

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "x360-rs",
    version,
    about = "Xbox 360 container and image parser (STFS, XISO, XEX, GoD)"
)]
pub(crate) struct Args {
    /// One or more files to parse (format auto-detected).
    #[arg(help_heading = "Input")]
    pub files: Vec<PathBuf>,

    /// Parse all files directly inside a directory.
    #[arg(long, conflicts_with = "files", help_heading = "Input")]
    pub dir: Option<PathBuf>,

    /// With --dir: descend into subdirectories.
    #[arg(long, requires = "dir", help_heading = "Input")]
    pub recursive: bool,

    /// Parse an extracted game directory (tree stats + default.xex/.xbe).
    #[arg(long, conflicts_with_all = ["files", "dir", "extract", "bench_extract_iso"], help_heading = "Input")]
    pub game_dir: Option<PathBuf>,

    /// Report output format.
    #[arg(short, long, default_value = "text", help_heading = "Report")]
    pub format: OutputFormat,

    /// Write the report to a file instead of stdout.
    #[arg(short, long, help_heading = "Report")]
    pub output: Option<PathBuf>,

    /// List raw strings found in the file.
    #[arg(long, help_heading = "Report")]
    pub show_strings: bool,

    /// Always list all directory entries (default lists them when <= 50).
    #[arg(long, help_heading = "Report")]
    pub show_entries: bool,

    /// Verify STFS hashes (header, tables, data blocks).
    #[arg(long, help_heading = "STFS")]
    pub verify: bool,

    /// Extract STFS contents into OUTDIR (hash-verified during extraction).
    #[arg(long, value_name = "OUTDIR", help_heading = "STFS")]
    pub extract: Option<PathBuf>,

    /// Extract XISO contents into OUTDIR (fast, multi-method).
    #[arg(
        long,
        value_name = "OUTDIR",
        conflicts_with = "extract",
        help_heading = "XISO"
    )]
    pub extract_iso: Option<PathBuf>,

    /// Copy a single file or directory out of an XISO image
    /// (same as xdvdfs copy-out). Uses `-o/--output` as the destination
    /// path; requires exactly one input file.
    #[arg(
        long,
        value_name = "PATH",
        help_heading = "XISO",
        conflicts_with_all = [
            "extract",
            "extract_iso",
            "bench_extract_iso",
            "optimize",
            "create",
            "game_dir",
            "dir"
        ]
    )]
    pub copy_out: Option<String>,

    /// Print MD5 checksums for entries in XISO image(s)
    /// (same as xdvdfs md5).
    #[arg(long, help_heading = "XISO")]
    pub md5: bool,

    /// Print a deterministic content checksum
    /// (same as xdvdfs checksum).
    #[arg(long, help_heading = "XISO")]
    pub checksum: bool,

    /// Benchmark all XISO extraction methods and show fastest.
    #[arg(long, conflicts_with = "extract", help_heading = "XISO")]
    pub bench_extract_iso: bool,

    /// Optimize/repack existing XISO (removes padding, contiguous sectors).
    /// Produces XSF by default (smallest, same as extract-xiso); use
    /// --preserve-format to keep the input format. Already-optimized inputs
    /// (extract-xiso tag) are skipped. Uses `-o/--output` as destination.
    #[arg(
        long,
        help_heading = "XISO",
        conflicts_with_all = [
            "create",
            "game_dir",
            "dir",
            "extract",
            "extract_iso",
            "bench_extract_iso",
            "truncate"
        ]
    )]
    pub optimize: bool,

    /// Truncate an XISO past its last used sector (Iso2God "Partial"
    /// equivalent). Preserves the original layout and format byte-for-byte;
    /// only trailing padding is dropped. Uses `-o/--output` as destination.
    #[arg(
        long,
        help_heading = "XISO",
        conflicts_with_all = [
            "create",
            "game_dir",
            "dir",
            "extract",
            "extract_iso",
            "bench_extract_iso",
            "optimize"
        ]
    )]
    pub truncate: bool,

    /// Create a new XISO from an extracted game directory.
    /// Uses `-o/--output` as the destination ISO path.
    #[arg(
        long,
        value_name = "DIR",
        help_heading = "XISO",
        conflicts_with_all = [
            "optimize",
            "game_dir",
            "dir",
            "extract",
            "extract_iso",
            "bench_extract_iso",
            "files"
        ]
    )]
    pub create: Option<PathBuf>,

    /// Output image format for --create (default: xsf).
    /// --optimize produces xsf unless --preserve-format is given.
    #[arg(
        long,
        value_name = "FORMAT",
        default_value = "xsf",
        help_heading = "XISO"
    )]
    pub create_format: crate::write::format::OutputFormat,

    /// Overwrite input file in place (creates a .bak backup first).
    #[arg(long, help_heading = "XISO")]
    pub in_place: bool,

    /// Skip the $SystemUpdate folder during extract, optimize and create
    /// (same as extract-xiso -s).
    #[arg(short = 's', long, help_heading = "XISO")]
    pub skip_systemupdate: bool,

    /// Suppress console output (progress, reports and informational messages).
    /// Warnings and errors are still printed; the --output file is still
    /// written when given.
    #[arg(short = 'q', long, conflicts_with = "verbose", help_heading = "Report")]
    pub quiet: bool,

    /// Map host paths to image paths when creating (same as xdvdfs
    /// build-image -m). Repeatable. Form "host/path:image/path" with glob
    /// and {n} capture substitution, or "!host/path" to exclude.
    /// Unmatched files are not packed.
    #[arg(
        short = 'm',
        long,
        value_name = "RULE",
        requires = "create",
        help_heading = "XISO"
    )]
    pub map: Vec<String>,

    /// List what --create would pack without writing the image
    /// (same as xdvdfs build-image -D).
    #[arg(long, requires = "create", help_heading = "XISO")]
    pub dry_run: bool,

    /// Preserve the original XISO format (XGD3, XGD2, XGD1) during optimization.
    /// By default, --optimize produces XSF (smallest file, same as extract-xiso).
    /// This is an advanced option; use with caution.
    #[arg(long, requires = "optimize", help_heading = "XISO")]
    pub preserve_format: bool,

    /// Disable automatic .xbe media-enable patching during create/optimize
    /// (same as extract-xiso -m). Patching is on by default.
    #[arg(long, help_heading = "XISO")]
    pub no_media_patch: bool,

    /// Re-parse the output and verify entry fidelity (with --optimize or
    /// --truncate).
    #[arg(long, help_heading = "XISO")]
    pub verify_output: bool,

    /// Print parse progress to stderr.
    #[arg(short, long, help_heading = "Report")]
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
