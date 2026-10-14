#![allow(dead_code)]

use crate::iso::IsoEntry;
use std::collections::BTreeMap;
use std::path::PathBuf;

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
    pub allocated_sector: u32,
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
            allocated_sector: 0,
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
        allocated_sector: 0,
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
        let final_attrs = if entry.is_dir {
            attrs_to_byte(&entry.attrs)
        } else {
            crate::iso::GDF_ATTR_DIRECTORY
        };
        // Directory entries should carry the directory attribute bit.
        let _ = final_attrs;

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

    for idx in (1..total).rev() {
        let parent_idx = parent_indices[idx].unwrap_or(0);
        if let Some(parent) = nodes_opt[parent_idx].as_mut() {
            // Remap from a global child index to an index inside `parent.children`.
            for entry in &mut parent.entries {
                if entry.child_index == Some(idx) {
                    entry.child_index = Some(parent.children.len());
                }
            }
        }
        let child = nodes_opt[idx].take().expect("child node present");
        if let Some(parent) = nodes_opt[parent_idx].as_mut() {
            parent.children.push(child);
        }
    }

    nodes_opt[0].take().expect("root node present")
}

/// Calculate the GDF directory table size for a single node, in sectors.
///
/// The node's `children` must already have had their sizes calculated so
/// that directory entries receive their child-table byte sizes.
pub fn calculate_table_size(node: &DirTreeNode) -> u32 {
    let mut size = 0usize;
    for entry in &node.entries {
        let entry_bytes = 14 + entry.name.len();
        // Parser pads each entry to a 4-byte boundary.
        let aligned = (entry_bytes + 3) & !3;
        size += aligned;
    }
    if size == 0 {
        1
    } else {
        size.div_ceil(2048) as u32
    }
}

/// Calculate table sizes for every node, bottom-up, without recursion.
pub fn calculate_sizes(node: &mut DirTreeNode) {
    let root_ptr = node as *mut DirTreeNode;
    let mut order = Vec::new();
    let mut stack = vec![(root_ptr, false)];
    while let Some((ptr, visited)) = stack.pop() {
        if visited {
            order.push(ptr);
        } else {
            stack.push((ptr, true));
            let node = unsafe { ptr.as_mut().unwrap_unchecked() };
            for i in (0..node.children.len()).rev() {
                stack.push(((&mut node.children[i]) as *mut DirTreeNode, false));
            }
        }
    }

    for ptr in order {
        let node = unsafe { &mut *ptr };
        for entry in &mut node.entries {
            if entry.is_dir {
                if let Some(idx) = entry.child_index {
                    if idx < node.children.len() {
                        entry.size = node.children[idx].table_size_sectors as u64 * 2048;
                    }
                }
            }
        }
        node.table_size_sectors = calculate_table_size(node);
    }
}

/// Backwards/plan-friendly alias.
pub fn calculate_sizes_recursive(node: &mut DirTreeNode) {
    calculate_sizes(node);
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
        let mut node = DirTreeNode {
            path: "".into(),
            entries: vec![],
            children: vec![],
            table_size_sectors: 0,
            allocated_sector: 0,
        };
        node.table_size_sectors = calculate_table_size(&node);
        assert_eq!(node.table_size_sectors, 1);
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
        let mut root = build_dir_tree_from_entries(&entries);
        calculate_sizes(&mut root);
        assert!(root.table_size_sectors >= 1);
    }
}
