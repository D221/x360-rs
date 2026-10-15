//! Create an XISO from an extracted game directory.

use std::path::Path;

use anyhow::Result;

use crate::write::dirtab::{
    build_tree_from_dir_arena, calculate_arena_sizes, check_duplicates_arena,
};
use crate::write::format::OutputFormat;
use crate::write::img::create_xiso_from_arena_with_progress;

/// Convenience wrapper without progress reporting (the CLI uses
/// [`create_xiso_from_dir_with_progress`]).
#[allow(dead_code)]
pub fn create_xiso_from_dir(
    dir: &Path,
    output: &Path,
    fmt: OutputFormat,
    log: &dyn Fn(String),
) -> Result<()> {
    create_xiso_from_dir_with_progress(dir, output, fmt, log, &|_| ())
}

/// Like [`create_xiso_from_dir`], reporting progress through `progress` in the
/// same style as extraction (a header line, then `[n/total] path`).
pub fn create_xiso_from_dir_with_progress(
    dir: &Path,
    output: &Path,
    fmt: OutputFormat,
    log: &dyn Fn(String),
    progress: &dyn Fn(String),
) -> Result<()> {
    if !dir.is_dir() {
        anyhow::bail!("not a directory: {}", dir.display());
    }

    let mut arena = build_tree_from_dir_arena(dir, output)?;
    calculate_arena_sizes(&mut arena);
    check_duplicates_arena(&arena)?;

    create_xiso_from_arena_with_progress(arena, None, 0, output, fmt, 0, log, progress)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("tu_create_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn test_create_from_empty_dir() {
        let dir = temp_dir("empty");
        let out = dir.join("empty.iso");
        create_xiso_from_dir(dir.as_path(), &out, OutputFormat::Xsf, &(|_| ())).unwrap();
        assert!(out.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_create_from_dir_with_file() {
        let dir = temp_dir("file");
        fs::write(dir.join("hello.txt"), b"hello world").unwrap();
        let out = dir.join("test.iso");
        create_xiso_from_dir(dir.as_path(), &out, OutputFormat::Xsf, &(|_| ())).unwrap();
        let info = crate::parse::parse_iso(&out, fs::metadata(&out).unwrap().len()).unwrap();
        assert_eq!(info.entries.len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_nested_roundtrip() {
        let dir = temp_dir("nested");
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("a.txt"), b"aaa").unwrap();
        fs::write(dir.join("sub").join("b.txt"), b"bbb").unwrap();
        let out = dir.join("nested.iso");
        create_xiso_from_dir(dir.as_path(), &out, OutputFormat::Xsf, &(|_| ())).unwrap();
        let info = crate::parse::parse_iso(&out, fs::metadata(&out).unwrap().len()).unwrap();
        assert_eq!(info.entries.len(), 3); // a.txt, sub dir, sub/b.txt
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_stale_output_inside_source_is_excluded() {
        let dir = temp_dir("stale");
        fs::write(dir.join("hello.txt"), b"hello world").unwrap();
        // Leftover output from a previous run must not be absorbed.
        let out = dir.join("test.iso");
        fs::write(&out, b"stale garbage, not an iso").unwrap();
        create_xiso_from_dir(dir.as_path(), &out, OutputFormat::Xsf, &(|_| ())).unwrap();
        let info = crate::parse::parse_iso(&out, fs::metadata(&out).unwrap().len()).unwrap();
        assert_eq!(info.entries.len(), 1);
        assert_eq!(info.entries[0].path, "hello.txt");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_deeply_nested_dirs_no_stack_overflow() {
        use crate::write::dirtab::build_tree_from_dir_arena;
        let dir = temp_dir("deep");
        let mut cur = dir.clone();
        for i in 0..500 {
            cur = cur.join(format!("d{i}"));
        }
        fs::create_dir_all(&cur).unwrap();
        fs::write(cur.join("leaf.txt"), b"leaf").unwrap();
        // The builder itself handles arbitrary depth iteratively.
        let arena = build_tree_from_dir_arena(&dir, &dir.join("deep.iso")).unwrap();
        assert_eq!(arena.len(), 501); // root + 500 nested dirs
                                      // End-to-end round-trip within the parser's depth guard.
        let shallow: PathBuf = (0..30).map(|i| format!("d{i}")).collect();
        let sdir = temp_dir("shallow");
        fs::create_dir_all(sdir.join(&shallow)).unwrap();
        fs::write(sdir.join(&shallow).join("leaf.txt"), b"leaf").unwrap();
        let out = sdir.join("shallow.iso");
        create_xiso_from_dir(sdir.as_path(), &out, OutputFormat::Xsf, &(|_| ())).unwrap();
        let info = crate::parse::parse_iso(&out, fs::metadata(&out).unwrap().len()).unwrap();
        assert_eq!(info.entries.len(), 31);
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&sdir);
    }
}
