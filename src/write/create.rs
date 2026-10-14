//! Create an XISO from an extracted game directory.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::write::dirtab::{calculate_sizes, calculate_sizes_recursive, DirEntry, DirTreeNode};
use crate::write::format::OutputFormat;
use crate::write::img::{check_duplicates, create_xiso_from_tree};

pub fn create_xiso_from_dir(
    dir: &Path,
    output: &Path,
    fmt: OutputFormat,
    log: &dyn Fn(String),
) -> Result<()> {
    if !dir.is_dir() {
        anyhow::bail!("not a directory: {}", dir.display());
    }

    let mut root = build_tree_from_dir(dir)?;
    calculate_sizes(&mut root);
    calculate_sizes_recursive(&mut root);
    check_duplicates(&root)?;

    create_xiso_from_tree(root, None, 0, output, fmt, 0, log)
}

fn build_tree_from_dir(dir: &Path) -> Result<DirTreeNode> {
    let mut collected: Vec<(String, bool, u64, PathBuf, Option<DirTreeNode>)> = Vec::new();

    for entry in fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let name = entry.file_name().to_string_lossy().to_string();

        if file_type.is_symlink() {
            anyhow::bail!("symlinks not supported");
        }

        if file_type.is_dir() {
            let child = build_tree_from_dir(&entry.path())?;
            collected.push((name, true, 0, entry.path(), Some(child)));
        } else if file_type.is_file() {
            let size = entry
                .metadata()
                .with_context(|| format!("metadata {}", entry.path().display()))?
                .len();
            collected.push((name, false, size, entry.path(), None));
        } else {
            anyhow::bail!("unsupported file type: {}", entry.path().display());
        }
    }

    collected.sort_by(|a, b| a.0.cmp(&b.0));

    let mut node = DirTreeNode {
        path: PathBuf::new(),
        entries: Vec::new(),
        children: Vec::new(),
        table_size_sectors: 0,
        allocated_sector: 0,
    };

    for (name, is_dir, size, path, child) in collected {
        if is_dir {
            let child_index = node.children.len();
            node.entries.push(DirEntry {
                name,
                is_dir: true,
                size: 0,
                attributes: 0x10,
                source_sector: 0,
                child_index: Some(child_index),
                on_disk: None,
            });
            node.children.push(child.expect("directory child present"));
        } else {
            node.entries.push(DirEntry {
                name,
                is_dir: false,
                size,
                attributes: 0x20 | 0x80,
                source_sector: 0,
                child_index: None,
                on_disk: Some(path),
            });
        }
    }

    Ok(node)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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
}
