//! Host-to-image path mapping for `--create --map`.
//!
//! Mirrors xdvdfs `build-image` map rules (`xdvdfs-core/src/write/fs/remap.rs`):
//! rules are `"host/path:image/path"` with `wax` globs plus `{n}` capture
//! substitution, or `"!host/path"` exclusions. Rules are scanned in order per
//! path; the first non-negated match wins (a later negation clears it, and a
//! still-later rule may match again). A rule matching a directory prefix pulls
//! the whole subtree, re-appending the remainder to the rewritten prefix.
//! Paths matching nothing are not packed.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::write::dirtab::ArenaNode;
use crate::write::dirtab::DirEntry;
use wax::Program as _;

/// A parsed `--map` rule: host glob pattern plus rewrite template.
/// Globs are compiled once per build (see `CompiledRule`) because
/// `wax::Glob` borrows its pattern string.
#[derive(Debug, Clone)]
pub struct MapRule {
    pub pattern: String,
    pub negated: bool,
    pub rewrite: String,
}

struct CompiledRule<'a> {
    glob: wax::Glob<'a>,
    negated: bool,
    rewrite: &'a str,
    source: &'a str,
}

/// Parse one `--map` rule: `"host/path:image/path"` or `"!host/path"`.
/// Split is on the first `:` (xdvdfs drops anything past a second one;
/// keeping the rest is a deliberate superset for paths containing `:`).
pub fn parse_map_rule(s: &str) -> Result<MapRule> {
    let (negated, pat, rewrite) = if let Some(rest) = s.strip_prefix('!') {
        // Exclusion; any `:image` part is ignored like xdvdfs does.
        let pat = rest.split(':').next().unwrap_or(rest);
        (true, pat, String::new())
    } else {
        match s.split_once(':') {
            Some((h, i)) => (false, h, i.to_string()),
            None => anyhow::bail!(
                "map rule {s:?} must have an image path unless it is an exclusion rule (starting with '!')"
            ),
        }
    };
    if pat.is_empty() {
        anyhow::bail!("map rule {s:?} has an empty host pattern");
    }
    // Validate the pattern now so bad globs fail fast at the CLI.
    wax::Glob::new(pat).map_err(|e| anyhow::anyhow!("invalid map pattern {pat:?}: {e}"))?;
    Ok(MapRule {
        pattern: pat.to_string(),
        negated,
        rewrite,
    })
}

/// Forward-slash relative path components of a host path under `root`.
fn host_components(root: &Path, path: &Path) -> Vec<String> {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().to_string()),
            _ => None,
        })
        .collect()
}

/// Apply the rules to one `/`-joined host path, returning the rewritten
/// image components, or `None` when excluded/unmatched. Mirrors
/// `RemapOverlayFilesystem::new`: deepest matching ancestor-or-self prefix
/// wins, then rules scan in order with negation clearing.
fn map_one(rules: &[CompiledRule], rel: &str) -> Result<Option<Vec<String>>> {
    let comps: Vec<&str> = rel.split('/').collect();
    // Longest ancestor-or-self prefix matching any rule. The walk in xdvdfs
    // propagates matches top-down so the deepest self-match overrides;
    // longest-match is the equivalent bottom-up formulation.
    let mut best: Option<String> = None;
    let mut cur = String::new();
    for (i, c) in comps.iter().enumerate() {
        if i > 0 {
            cur.push('/');
        }
        cur.push_str(c);
        let candidate = wax::CandidatePath::from(cur.as_str());
        if rules.iter().any(|r| r.glob.matched(&candidate).is_some()) {
            best = Some(cur.clone());
        }
    }
    let prefix = match best {
        Some(p) => p,
        None => return Ok(None),
    };
    let suffix = &rel[prefix.len()..];
    let suffix = suffix.trim_start_matches('/');

    // Scan rules in order against the matched prefix.
    let mut rewritten: Option<String> = None;
    for rule in rules {
        let candidate = wax::CandidatePath::from(prefix.as_str());
        let Some(matched) = rule.glob.matched(&candidate) else {
            continue;
        };
        if rule.negated {
            rewritten = None;
            continue;
        }
        if rewritten.is_some() {
            continue;
        }
        let mut rewrite = rule.rewrite.to_string();
        // `{n}` substitutions, mirroring xdvdfs `find_match_indices`.
        let mut indices: Vec<usize> = Vec::new();
        {
            let mut cur_idx = 0usize;
            let mut matching = false;
            for c in rewrite.chars() {
                if c == '{' {
                    matching = true;
                    cur_idx = 0;
                    continue;
                }
                if !matching {
                    continue;
                }
                if c == '}' {
                    matching = false;
                    indices.push(cur_idx);
                    continue;
                }
                match c.to_digit(10) {
                    Some(d) => {
                        cur_idx = cur_idx * 10 + d as usize;
                    }
                    None => anyhow::bail!(
                        "invalid rewrite substitution in {:?}: expected digit character",
                        rule.source
                    ),
                }
            }
            if matching {
                anyhow::bail!(
                    "invalid rewrite substitution in {:?}: unclosed brace",
                    rule.source
                );
            }
        }
        for index in indices {
            let replace = matched.get(index).unwrap_or("");
            rewrite = rewrite.replace(&format!("{{{index}}}"), replace);
        }
        if !suffix.is_empty() {
            rewrite = format!("{}/{suffix}", rewrite.trim_end_matches('/'));
        }
        rewritten = Some(rewrite);
    }
    Ok(rewritten.map(|r| {
        r.trim_start_matches('.')
            .trim_start_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    }))
}

/// One mapped host entry, ready for arena construction.
struct MappedEntry {
    image: Vec<String>,
    host: PathBuf,
    is_dir: bool,
    size: u64,
}

/// Mapped arena plus `(host, image)` display pairs for `--dry-run`.
type MappedTree = (Vec<ArenaNode>, Vec<(String, String)>);

/// Walk `dir`, apply `rules`, and build a write arena whose entries point
/// at on-disk sources. Returns the arena plus `(host, image)` display pairs
/// for `--dry-run`. With no rules this is never called (plain walk used).
pub(crate) fn build_tree_from_mapping(
    dir: &Path,
    output: &Path,
    skip_systemupdate: bool,
    rules: &[MapRule],
) -> Result<MappedTree> {
    use anyhow::Context as _;

    let dir_canon = dir
        .canonicalize()
        .with_context(|| format!("cannot resolve {}", dir.display()))?;
    let out_abs = match std::fs::canonicalize(output) {
        Ok(p) => p,
        Err(_) => {
            let mut b = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            b.push(output);
            b
        }
    };

    // Iterative host walk (mirrors build_tree_from_dir_arena).
    let mut stack = vec![dir_canon.clone()];
    let mut collected: Vec<(String, std::fs::FileType, PathBuf)> = Vec::new();
    while let Some(fs_dir) = stack.pop() {
        for entry in
            std::fs::read_dir(&fs_dir).with_context(|| format!("read_dir {}", fs_dir.display()))?
        {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let path = entry.path();
            if path == out_abs {
                continue;
            }
            if file_type.is_symlink() {
                anyhow::bail!("symlinks not supported: {}", path.display());
            }
            let rel = host_components(&dir_canon, &path).join("/");
            collected.push((rel, file_type, path.clone()));
            if file_type.is_dir() {
                stack.push(path);
            }
        }
    }
    collected.sort_by(|a, b| a.0.cmp(&b.0));

    // Compile globs once (wax::Glob borrows its pattern).
    let mut compiled: Vec<CompiledRule> = Vec::with_capacity(rules.len());
    for rule in rules {
        let glob = wax::Glob::new(rule.pattern.as_str())
            .map_err(|e| anyhow::anyhow!("invalid map pattern {:?}: {e}", rule.pattern))?;
        compiled.push(CompiledRule {
            glob,
            negated: rule.negated,
            rewrite: rule.rewrite.as_str(),
            source: rule.rewrite.as_str(),
        });
    }

    // Map host entries to image paths.
    let mut mapped: Vec<MappedEntry> = Vec::new();
    let mut pairs: Vec<(String, String)> = Vec::new();
    for (rel, file_type, path) in &collected {
        if skip_systemupdate
            && rel
                .split('/')
                .next()
                .is_some_and(|c| c.eq_ignore_ascii_case("$SystemUpdate"))
        {
            continue;
        }
        let Some(image) = map_one(&compiled, rel)? else {
            continue;
        };
        if image.is_empty() {
            // Rewritten to image root itself: only meaningful for files.
            if file_type.is_dir() {
                continue;
            }
            let name = rel.rsplit('/').next().unwrap_or(rel).to_string();
            mapped.push(MappedEntry {
                image: vec![name.clone()],
                host: path.clone(),
                is_dir: false,
                size: std::fs::metadata(path)
                    .with_context(|| format!("metadata {}", path.display()))?
                    .len(),
            });
            pairs.push((rel.clone(), name));
            continue;
        }
        if file_type.is_dir() {
            mapped.push(MappedEntry {
                image: image.clone(),
                host: path.clone(),
                is_dir: true,
                size: 0,
            });
        } else {
            mapped.push(MappedEntry {
                image: image.clone(),
                host: path.clone(),
                is_dir: false,
                size: std::fs::metadata(path)
                    .with_context(|| format!("metadata {}", path.display()))?
                    .len(),
            });
        }
        pairs.push((rel.clone(), image.join("/")));
    }
    pairs.sort();

    // Build arena nodes keyed by image parent path (sorted for determinism).
    mapped.sort_by(|a, b| a.image.cmp(&b.image));
    let mut arena = vec![ArenaNode {
        path: PathBuf::new(),
        entries: Vec::new(),
        children: Vec::new(),
        table_size_sectors: 0,
    }];
    let mut index_by_path: std::collections::BTreeMap<Vec<String>, usize> =
        std::collections::BTreeMap::new();
    index_by_path.insert(Vec::new(), 0);
    // path -> arena idx for directory nodes (auto-created parents included).
    let ensure_dir = |arena: &mut Vec<ArenaNode>,
                      index_by_path: &mut std::collections::BTreeMap<Vec<String>, usize>,
                      comps: &[String]|
     -> usize {
        if let Some(&idx) = index_by_path.get(comps) {
            return idx;
        }
        // Create missing ancestors first.
        let mut cur: Vec<String> = Vec::new();
        let mut parent_idx = 0usize;
        for comp in comps {
            cur.push(comp.clone());
            if let Some(&idx) = index_by_path.get(&cur) {
                parent_idx = idx;
                continue;
            }
            let idx = arena.len();
            arena.push(ArenaNode {
                path: PathBuf::from(cur.join("/")),
                entries: Vec::new(),
                children: Vec::new(),
                table_size_sectors: 0,
            });
            let pos = arena[parent_idx].children.len();
            arena[parent_idx].entries.push(DirEntry {
                name: comp.clone(),
                is_dir: true,
                size: 0,
                attributes: 0x10,
                source_sector: 0,
                child_index: Some(pos),
                on_disk: None,
            });
            arena[parent_idx].children.push(idx);
            index_by_path.insert(cur.clone(), idx);
            parent_idx = idx;
        }
        parent_idx
    };

    for m in &mapped {
        let (parent, name) = match m.image.split_at(m.image.len() - 1) {
            (p, [n]) => (p, n.clone()),
            _ => continue, // image vec never empty here
        };
        let parent_idx = ensure_dir(&mut arena, &mut index_by_path, parent);
        if m.is_dir {
            // Register the mapped dir node itself (auto-created above if
            // it was an implied parent; ensure it exists as a child node).
            let mut full = parent.to_vec();
            full.push(name.clone());
            ensure_dir(&mut arena, &mut index_by_path, &full);
        } else {
            arena[parent_idx].entries.push(DirEntry {
                name,
                is_dir: false,
                size: m.size,
                attributes: 0x20 | 0x80,
                source_sector: 0,
                child_index: None,
                on_disk: Some(m.host.clone()),
            });
        }
    }

    Ok((arena, pairs))
}
