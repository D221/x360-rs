use crate::iso::IsoEntry;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub attributes: u8,
    pub source_sector: u32,
    pub child_index: Option<usize>,
    pub on_disk: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirTreeNode {
    pub path: PathBuf,
    pub entries: Vec<DirEntry>,
    pub children: Vec<DirTreeNode>,
    pub table_size_sectors: u32,
}

/// Convert an `IsoEntryAttrs` value into the single GDF attribute byte.
fn attrs_to_byte(attrs: &crate::iso::IsoEntryAttrs) -> u8 {
    let mut byte = 0u8;
    if attrs.read_only {
        byte |= 0x01;
    }
    if attrs.hidden {
        byte |= 0x02;
    }
    if attrs.system {
        byte |= 0x04;
    }
    if attrs.directory {
        byte |= 0x10;
    }
    if attrs.archive {
        byte |= 0x20;
    }
    if attrs.normal {
        byte |= 0x80;
    }
    byte
}

/// Split a stored entry path into its components, accepting both separators.
fn split_components(path: &str) -> Vec<String> {
    path.trim_matches(|c| c == '\\' || c == '/')
        .split(['\\', '/'])
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Join path components using the platform-independent `/` separator.
fn join_components(components: &[String]) -> String {
    components.join("/")
}

/// Best-effort absolute path without requiring existence.
fn absolutize(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

/// Build an arena directly from an on-disk directory, iteratively.
///
/// Single pass with an explicit stack, so arbitrarily deep trees cannot
/// overflow the call stack. `output` (the ISO being created) is excluded
/// from the walk so re-runs don't absorb a stale output left inside the
/// source directory. Symlinks are rejected.
pub(crate) fn build_tree_from_dir_arena(dir: &Path, output: &Path) -> Result<Vec<ArenaNode>> {
    use anyhow::Context as _;

    let dir_canon = dir
        .canonicalize()
        .with_context(|| format!("cannot resolve {}", dir.display()))?;
    let out_abs = match std::fs::canonicalize(output) {
        Ok(p) => p,
        Err(_) => absolutize(output),
    };

    let mut arena = vec![ArenaNode {
        path: PathBuf::new(),
        entries: Vec::new(),
        children: Vec::new(),
        table_size_sectors: 0,
    }];

    // Stack of (on-disk directory, arena index). Children are pushed in
    // reverse name order so they pop (and land in `children`) sorted.
    let mut stack = vec![(dir_canon.clone(), 0usize)];
    while let Some((fs_dir, idx)) = stack.pop() {
        let mut collected: Vec<(String, std::fs::FileType, PathBuf)> = Vec::new();
        for entry in
            std::fs::read_dir(&fs_dir).with_context(|| format!("read_dir {}", fs_dir.display()))?
        {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let name = entry.file_name().to_string_lossy().to_string();
            collected.push((name, file_type, entry.path()));
        }
        collected.sort_by(|a, b| a.0.cmp(&b.0));

        // Group child directories to push after files for deterministic order.
        let mut pending_dirs: Vec<(PathBuf, usize)> = Vec::new();
        for (name, file_type, path) in collected {
            if path == out_abs {
                if file_type.is_dir() {
                    anyhow::bail!(
                        "output path is a directory inside the source: {}",
                        path.display()
                    );
                }
                continue;
            }
            if file_type.is_symlink() {
                anyhow::bail!("symlinks not supported: {}", path.display());
            }
            if file_type.is_dir() {
                let child_idx = arena.len();
                arena.push(ArenaNode {
                    path: path.strip_prefix(&dir_canon).unwrap_or(&path).to_path_buf(),
                    entries: Vec::new(),
                    children: Vec::new(),
                    table_size_sectors: 0,
                });
                let pos = arena[idx].children.len();
                arena[idx].entries.push(DirEntry {
                    name: name.clone(),
                    is_dir: true,
                    size: 0,
                    attributes: 0x10,
                    source_sector: 0,
                    child_index: Some(pos),
                    on_disk: None,
                });
                arena[idx].children.push(child_idx);
                pending_dirs.push((path, child_idx));
            } else if file_type.is_file() {
                let size = std::fs::metadata(&path)
                    .with_context(|| format!("metadata {}", path.display()))?
                    .len();
                arena[idx].entries.push(DirEntry {
                    name,
                    is_dir: false,
                    size,
                    attributes: 0x20 | 0x80,
                    source_sector: 0,
                    child_index: None,
                    on_disk: Some(path),
                });
            } else {
                anyhow::bail!("unsupported file type: {}", path.display());
            }
        }
        // Visit subdirectories depth-first; reversed so names pop sorted.
        for (path, child_idx) in pending_dirs.into_iter().rev() {
            stack.push((path, child_idx));
        }
    }
    Ok(arena)
}

fn ensure_dir_path(
    nodes: &mut Vec<DirTreeNode>,
    index_by_path: &mut BTreeMap<String, usize>,
    parent_indices: &mut Vec<Option<usize>>,
    components: &[String],
    default_attrs: u8,
) -> usize {
    if components.is_empty() {
        return *index_by_path.get("").unwrap_or(&0);
    }

    let mut current_idx = 0usize;
    let mut prefix: Vec<String> = Vec::with_capacity(components.len());

    for comp in components {
        prefix.push(comp.clone());
        let key = join_components(&prefix);

        if let Some(&existing) = index_by_path.get(&key) {
            current_idx = existing;
            continue;
        }

        let new_idx = nodes.len();
        nodes.push(DirTreeNode {
            path: PathBuf::from(prefix.join("/")),
            entries: Vec::new(),
            children: Vec::new(),
            table_size_sectors: 0,
        });
        index_by_path.insert(key, new_idx);
        parent_indices.push(Some(current_idx));

        nodes[current_idx].entries.push(DirEntry {
            name: comp.clone(),
            is_dir: true,
            size: 0,
            attributes: default_attrs,
            source_sector: 0,
            child_index: Some(new_idx),
            on_disk: None,
        });

        current_idx = new_idx;
    }

    current_idx
}

/// Build a directory tree from a flat list of `IsoEntry` values.
///
/// The returned `DirTreeNode` is the root. All descendant directories are
/// stored recursively in `children`, making deeply nested traversal possible
/// without relying on a separate flat vector. No recursion is used during
/// construction, so deeply nested inputs (>1000 levels) do not overflow
/// the stack.
pub fn build_dir_tree_from_entries(entries: &[IsoEntry]) -> DirTreeNode {
    let mut nodes: Vec<DirTreeNode> = Vec::new();
    let mut index_by_path: BTreeMap<String, usize> = BTreeMap::new();
    let mut parent_indices: Vec<Option<usize>> = Vec::new();

    // Root node is always index 0.
    nodes.push(DirTreeNode {
        path: PathBuf::new(),
        entries: Vec::new(),
        children: Vec::new(),
        table_size_sectors: 0,
    });
    index_by_path.insert(String::new(), 0);
    parent_indices.push(None);

    for entry in entries {
        let components = split_components(&entry.path);
        if components.is_empty() {
            continue;
        }

        // ISO files have no separate explicit directory entries sometimes; ensure
        // parents on demand. Explicitly listed directories also create nodes.
        if entry.is_dir {
            ensure_dir_path(
                &mut nodes,
                &mut index_by_path,
                &mut parent_indices,
                &components,
                attrs_to_byte(&entry.attrs),
            );
        } else {
            let parent_components = if components.len() > 1 {
                &components[..components.len() - 1]
            } else {
                &components[..0]
            };
            ensure_dir_path(
                &mut nodes,
                &mut index_by_path,
                &mut parent_indices,
                parent_components,
                0x10,
            );

            let parent_key = join_components(parent_components);
            let parent_idx = *index_by_path.get(&parent_key).unwrap_or(&0);

            let name = components.last().cloned().unwrap_or_default();
            nodes[parent_idx].entries.push(DirEntry {
                name,
                is_dir: false,
                size: entry.size,
                attributes: attrs_to_byte(&entry.attrs),
                source_sector: entry.sector,
                child_index: None,
                on_disk: None,
            });
        }
    }

    // Convert the flat node vector into a recursively owned tree. Processing
    // indices in reverse guarantees that all descendants are moved into their
    // parent before the parent itself is moved.
    let total = nodes.len();
    let mut nodes_opt: Vec<Option<DirTreeNode>> = nodes.into_iter().map(Some).collect();

    // Move children into their parents (reverse order so descendants settle
    // before parents), recording each (parent, global index, local position)
    // triple for the remap below.
    let mut remap: Vec<(usize, usize, usize)> = Vec::new();
    for idx in (1..total).rev() {
        let parent_idx = parent_indices[idx].unwrap_or(0);
        if let Some(parent) = nodes_opt[parent_idx].as_ref() {
            remap.push((parent_idx, idx, parent.children.len()));
        }
        let child = nodes_opt[idx].take().expect("child node present");
        if let Some(parent) = nodes_opt[parent_idx].as_mut() {
            parent.children.push(child);
        }
    }
    // Resolve entries to local positions with one lookup per entry: entries
    // still hold global indices here, and each entry is visited exactly
    // once, so a remapped value can never match a second lookup. (Matching
    // remap items against live entry values instead aliases remapped
    // positions against pending globals — e.g. globals 1..4 remap to
    // 3,2,1,0 and global 3's entry would then also match global 1.)
    {
        let mut maps: BTreeMap<usize, BTreeMap<usize, usize>> = BTreeMap::new();
        for (parent_idx, global_idx, local_pos) in remap {
            maps.entry(parent_idx)
                .or_default()
                .insert(global_idx, local_pos);
        }
        for (parent_idx, map) in &maps {
            if let Some(parent) = nodes_opt[*parent_idx].as_mut() {
                for entry in &mut parent.entries {
                    if let Some(global_idx) = entry.child_index {
                        if let Some(&local_pos) = map.get(&global_idx) {
                            entry.child_index = Some(local_pos);
                        }
                    }
                }
            }
        }
    }

    nodes_opt[0].take().expect("root node present")
}

/// Calculate the GDF directory table size for an entry list, in sectors.
///
/// Entry layout is 14 header bytes plus the name, padded to a 4-byte
/// boundary (matching the parser and the writer).
pub fn table_size_for_entries(entries: &[DirEntry]) -> u32 {
    let mut size = 0usize;
    for entry in entries {
        let entry_bytes = 14 + entry.name.len();
        let aligned = (entry_bytes + 3) & !3;
        size += aligned;
    }
    if size == 0 {
        1
    } else {
        size.div_ceil(2048) as u32
    }
}

/// Flat, index-addressed view of a directory tree.
///
/// All write passes address nodes by index, so no raw pointers are needed
/// anywhere in the writer. `children` preserves the order of the source
/// node's children, keeping `DirEntry.child_index` positions valid.
#[derive(Debug, Clone)]
pub(crate) struct ArenaNode {
    pub path: PathBuf,
    pub entries: Vec<DirEntry>,
    pub children: Vec<usize>,
    pub table_size_sectors: u32,
}

/// Flatten a tree into an arena. Parents always precede their children,
/// so a reverse-order pass runs bottom-up. Iterative: safe for any depth.
pub(crate) fn flatten_tree(root: &DirTreeNode) -> Vec<ArenaNode> {
    let mut arena: Vec<ArenaNode> = Vec::new();
    let mut stack: Vec<(&DirTreeNode, Option<usize>)> = vec![(root, None)];
    while let Some((node, parent)) = stack.pop() {
        let idx = arena.len();
        arena.push(ArenaNode {
            path: node.path.clone(),
            entries: node.entries.clone(),
            children: Vec::with_capacity(node.children.len()),
            table_size_sectors: 0,
        });
        if let Some(p) = parent {
            arena[p].children.push(idx);
        }
        for child in node.children.iter().rev() {
            stack.push((child, Some(idx)));
        }
    }
    arena
}

/// Calculate table sizes for every arena node, bottom-up.
pub(crate) fn calculate_arena_sizes(arena: &mut [ArenaNode]) {
    for i in (0..arena.len()).rev() {
        let child_sizes: Vec<u32> = arena[i]
            .children
            .iter()
            .map(|&c| arena[c].table_size_sectors)
            .collect();
        let node = &mut arena[i];
        for entry in &mut node.entries {
            if entry.is_dir {
                if let Some(pos) = entry.child_index {
                    if let Some(&sectors) = child_sizes.get(pos) {
                        entry.size = sectors as u64 * 2048;
                    }
                }
            }
        }
        node.table_size_sectors = table_size_for_entries(&node.entries);
    }
}

/// Reject duplicate names per directory, case-sensitively and
/// case-insensitively (GDF lookup is case-insensitive, so `A.TXT` and
/// `a.txt` would collide on a real Xbox). Iterative over the arena.
pub(crate) fn check_duplicates_arena(arena: &[ArenaNode]) -> Result<()> {
    use std::collections::BTreeSet;
    for node in arena {
        let mut seen = BTreeSet::new();
        let mut seen_folded = BTreeSet::new();
        for entry in &node.entries {
            if !seen.insert(entry.name.clone()) {
                anyhow::bail!("duplicate entry: {}", entry.name);
            }
            if !seen_folded.insert(entry.name.to_lowercase()) {
                anyhow::bail!("duplicate entry (case-insensitive): {}", entry.name);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iso::IsoEntry;

    fn make_entry(path: &str, is_dir: bool, size: u64, sector: u32) -> IsoEntry {
        IsoEntry {
            path: path.to_string(),
            sector,
            size,
            is_dir,
            attrs: crate::iso::IsoEntryAttrs::default(),
            left_child_offset: None,
            right_child_offset: None,
        }
    }

    #[test]
    fn test_build_tree_from_entries() {
        let attrs = crate::iso::IsoEntryAttrs {
            archive: true,
            normal: true,
            ..crate::iso::IsoEntryAttrs::default()
        };
        let mut entries = vec![IsoEntry {
            path: "a.txt".into(),
            sector: 0,
            size: 100,
            is_dir: false,
            attrs: attrs.clone(),
            left_child_offset: None,
            right_child_offset: None,
        }];
        entries.push(make_entry("sub", true, 0, 0));
        entries.push(IsoEntry {
            path: "sub/b.txt".into(),
            sector: 1,
            size: 50,
            is_dir: false,
            attrs,
            left_child_offset: None,
            right_child_offset: None,
        });

        let root = build_dir_tree_from_entries(&entries);
        assert_eq!(root.entries.len(), 2);
        assert!(root.entries.iter().any(|e| e.is_dir));
        // children exposed
        assert!(!root.children.is_empty());
    }

    #[test]
    fn test_calculate_table_size_empty() {
        assert_eq!(table_size_for_entries(&[]), 1);
    }

    #[test]
    fn test_build_tree_deep_nesting_no_stack_overflow() {
        let path = (0..1000)
            .map(|i| format!("d{i}"))
            .collect::<Vec<_>>()
            .join("\\")
            + "\\leaf.txt";
        let entries = [IsoEntry {
            path,
            sector: 5,
            size: 10,
            is_dir: false,
            attrs: crate::iso::IsoEntryAttrs::default(),
            left_child_offset: None,
            right_child_offset: None,
        }];
        let root = build_dir_tree_from_entries(&entries);
        // Root should contain a single nested directory entry.
        assert_eq!(root.entries.len(), 1);
        assert!(root.entries[0].is_dir);
    }

    #[test]
    fn test_calculate_sizes_roundtrip_sizes() {
        let entries = [make_entry("a.txt", false, 100, 7)];
        let root = build_dir_tree_from_entries(&entries);
        let mut arena = flatten_tree(&root);
        calculate_arena_sizes(&mut arena);
        assert!(arena[0].table_size_sectors >= 1);
    }

    #[test]
    fn test_case_insensitive_duplicates_rejected() {
        let entries = [
            make_entry("A.TXT", false, 10, 1),
            make_entry("a.txt", false, 10, 2),
        ];
        let root = build_dir_tree_from_entries(&entries);
        let arena = flatten_tree(&root);
        assert!(check_duplicates_arena(&arena).is_err());
    }

    #[test]
    fn test_sibling_child_indices_not_aliased() {
        // Four subdirectories at globals 1..4 (the Sonic shape): the
        // remap must resolve every entry to its own child node.
        let mut entries = Vec::new();
        for name in ["d1", "d2", "d3", "d4"] {
            entries.push(make_entry(name, true, 0, 0));
        }
        for name in ["d1/f.txt", "d2/f.txt", "d3/f.txt", "d4/f.txt"] {
            entries.push(make_entry(name, false, 10, 5));
        }
        let root = build_dir_tree_from_entries(&entries);
        assert_eq!(root.children.len(), 4);
        for entry in &root.entries {
            assert!(entry.is_dir);
            let child = &root.children[entry.child_index.unwrap()];
            assert!(
                child.path.ends_with(&entry.name),
                "entry {} points at {:?}",
                entry.name,
                child.path
            );
            assert_eq!(child.entries.len(), 1);
            assert_eq!(child.entries[0].name, "f.txt");
        }
    }
}
