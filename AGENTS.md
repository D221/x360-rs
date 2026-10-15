# AGENTS.md - x360-rs (tu_parser)

## Project Overview

**x360-rs** - Fast, streaming Xbox 360 container parser and extractor for:
- STFS containers: Title Updates (TU), DLC/Marketplace, XBLA, Avatar, Theme, Indie, GoD
- XISO/GDF images: XSF, XGD2, XGD1, XGD3 (raw and iso2god-optimized)
- Standalone executables: XEX (ExecutionId), XBE (original Xbox)
- Extracted game directories

Outputs: game title, media ID, base title ID, region mask/regions (TU only), XEX execution info, file listings with sizes.

No GUI, no dependencies on original tools - everything parsed directly from the container. Accepts all three STFS container magics: LIVE, CON , PIRS (identical layout).

Directory name is `tu_parser`, binary/crate name is `x360-rs` (`clap` command name is `x360-rs`).

---

## Build and Test Commands

| Task | Command |
|------|---------|
| Build release | cargo build --release --locked |
| Run tests | cargo test --locked --verbose |
| Single test | cargo test <test_name> -- --nocapture |
| Lint (clippy) | cargo clippy --all-targets -- -D warnings |
| Format check | cargo fmt --check |
| Full CI locally | cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test --locked --verbose && cargo build --release --locked --verbose |

CI (`.github/workflows/ci.yml`) runs exactly: fmt check → clippy `-D warnings` → test `--locked` → release build `--locked`, on ubuntu-latest + windows-latest. Newer clippy versions flag pre-existing code too, so keep the tree warning-free under the local toolchain, not just CI's.

Binary output: target/release/x360-rs (.exe on Windows)

---

## Project Structure

tu_parser/
├── Cargo.toml          # deps: anyhow, byteorder, clap 4 (derive), serde, sha1 (STFS), md5 + sha3 (hash commands), wax (map rules)
├── Cargo.lock
├── LICENSE (MIT)
├── README.md           # update the Options table when adding CLI flags
├── .gitignore
├── .github/workflows/
│   ├── ci.yml      # fmt, clippy, test, build (ubuntu + windows)
│   └── release.yml # Triggered on v* tags, builds for Linux/Windows, publishes release
├── src/
│   ├── main.rs           # Entry point, CLI dispatch, optimize_one, verify_optimize_fidelity
│   ├── cli.rs            # clap argument parsing (help text lives here)
│   ├── constants.rs      # Magic bytes, offsets, error types
│   ├── parse.rs          # Top-level parse dispatch (file magic to STFS/XISO/XEX/XBE/game-dir)
│   ├── stfs.rs           # STFS parsing, hash verification, extraction
│   ├── iso.rs            # XISO/GDF parsing + is_systemupdate_path() matcher
│   ├── xex.rs            # XEX ExecutionId parsing
│   ├── tu_parser.rs      # TU-specific STFS parsing (header, file table, XEX region mask)
│   ├── tu_header.rs      # STFS/CON header field extraction
│   ├── regions.rs        # Region mask bit decoding (matches original Title Update Checker)
│   ├── report.rs         # Report types (Stfs, Xiso, Xex, Xbe, GameDir) for JSON/text output
│   ├── render.rs         # Text/JSON output formatting (emit writes -o file or stdout)
│   ├── write/            # XISO creation/optimization (padding removal)
│   │   ├── mod.rs        # Public API (create/optimize entry points only)
│   │   ├── format.rs     # OutputFormat (Xsf/Xgd2/Xgd3/Xgd1) + sector offsets
│   │   ├── sector.rs     # SectorAllocator (linear, starts at 33)
│   │   ├── volume.rs     # Volume descriptor writer (sector 32, ECMA-119 tail at 0x7EC)
│   │   ├── dirtab.rs     # Tree builder, BST table layout, index arena, dup checks (all iterative, no unsafe)
│   │   ├── img.rs        # Two-pass writer via Writer struct over arena indices (no unsafe)
│   │   └── create.rs     # Directory-to-ISO creation (iterative walk, skips output, rejects symlinks)
│   ├── extract/
│   │   ├── mod.rs        # Extraction orchestration (entry point: extract_iso_fast_with_progress)
│   │   ├── basic.rs      # Basic sequential extraction
│   │   ├── parallel.rs   # Multi-threaded extraction (Windows large-ISO path)
│   │   ├── mmap.rs       # mmap-based extraction (Unix default)
│   │   └── bench.rs      # Benchmark all extraction methods
│   └── tests.rs          # Unit tests with synthetic data (no external fixtures needed)

Design docs (not in this dir): `../docs/superpowers/specs/` + `../docs/superpowers/plans/`.

---

## Key Architecture Notes

- Streaming/zero-copy where possible - reads only needed windows from large files
- Auto-detects format by magic bytes: LIVE/CON /PIRS (STFS), XEX2, XBEH, else probes for XISO (MICROSOFT*XBOX*MEDIA at sector 0x20)
- STFS file table: 64-byte entries at 0xC000, densely packed, stops at first all-zero slot
- Region mask at XEX2 + 0x1A0 (BE u32), decoded using bit tests matching original Title Update Checker
- Hash verification (--verify): header SHA1, top hash, level-0 table hashes, every used data block hash
- Extraction: STFS (--extract) and XISO (--extract-iso) both create subfolder named by Title ID (XISO) or Base Title ID (STFS)
- XISO extraction auto-selects fastest method: parallel on Windows for >2GB, mmap otherwise
- JSON output: tagged unions with type: stfs | xiso | xex | xbe | gamedir
- Directory sweep (--dir --recursive): unrecognized files warn and continue (e.g., GOD DataNNNN parts)
- XISO optimize (--optimize): two-pass rebuild with contiguous sectors from 33; outputs **XSF by default** (smallest, same as extract-xiso); `--preserve-format` keeps input format (XGD3/XGD2/XGD1, larger files due to format offset); inputs already tagged optimized by extract-xiso (`in!xiso!` at file offset 31337) are skipped with a message; `--in-place` creates .bak backup with restore-on-failure; `--verify-output` re-parses and compares entries. Outputs carry **no** `in!xiso!` tag (pure).
- Directory tables are written as balanced BST layout (`dirtab.rs::layout_table`), not right-linked lists; readers must tree-walk. Unresolvable directory `child_index` in `img.rs` is a hard `bail!` (no silent fallback) - a failed optimize leaves no output, never a corrupt one.
- XISO create (--create DIR -o out.iso --create-format xsf): walks directory, streams file bytes from disk; symlinks rejected; `-m/--map` host→image rules with wax globs + `{n}` captures + `!` exclusions (same as xdvdfs `build-image -m`, see `write/remap.rs`); `--dry-run` lists without writing
- `-s/--skip-systemupdate`: drops top-level `$SystemUpdate` in extract, optimize and create (same as extract-xiso `-s`); matcher is `iso.rs::is_systemupdate_path` (first path component, case-insensitive)
- `-q/--quiet` (conflicts with `-v`): silences console progress and reports; warnings/errors still print; `--output` file is still written
- Warnings (`warning: ...`) must stay visible - never hide skips/fallbacks behind quiet or verbose gates
- No `.xbe` media-enable patching (extract-xiso does this by default; out of scope - 360-focused)

---

## Testing

- All tests are self-contained - they generate synthetic binary data in temp dirs
- No external test fixtures or network access required (do not add deps needing network without checking)
- Test locations: `src/tests.rs` (integration-style, incl. `$SystemUpdate` skip tests) + inline `#[cfg(test)]` modules in `write/` + `extract/`
- Tests cover: region decoding, STFS entry parsing, container magics, content kinds, XEX exec info, DLC parsing, XISO parsing, XBE parsing, unknown file rejection, game dir parsing, STFS verify/extract/tamper detection, multi-level refusal, path resolution, XISO optimize round-trip + padding removal + XGD3 create round-trip

Run specific test: cargo test <test_name> -- --nocapture

---

## Development Workflow

1. Make changes
2. Run cargo fmt (auto-formats)
3. Run cargo clippy --all-targets -- -D warnings (must pass)
4. Run cargo test --locked --verbose (must pass)
5. Run cargo build --release --locked to verify release build
6. If a CLI flag changed: update `cli.rs` doc comments AND `README.md` Options table, verify with `--help` output

---

## Common Tasks

### Adding a new format / parser
1. Add detection logic in parse.rs::parse_file() (magic bytes)
2. Create new module or extend existing (e.g., iso.rs for XISO variants)
3. Add report variant in report.rs and rendering in render.rs
4. Add tests in tests.rs with synthetic data

### Adding a CLI option
1. Add to cli.rs::Args struct with clap attributes + doc comment (doc = --help text)
2. Handle in main.rs dispatch logic
3. Update README.md Options table + verify `--help` output

### Modifying extraction
- STFS extraction: src/extract/basic.rs, parallel.rs, mmap.rs
- XISO extraction: same files, entry point extract_iso_fast_with_progress in extract/mod.rs
- Single-file extraction: `--copy-out <PATH> -o <DEST>` (xdvdfs `copy-out` equivalent, `extract::copy_out_iso`); single input file, case-insensitive GDF match, files and dir subtrees
- Content hashes: `--md5` (per-entry, `extract::md5_iso`; lines identical to xdvdfs `md5`) and `--checksum` (SHA3-256 over sorted `/path` keys plus file bytes, `extract::checksum_iso`; values identical to xdvdfs `checksum`, see `../xdvdfs/xdvdfs-core/src/checksum.rs`)
- Benchmark: cargo run -- --bench-extract-iso <file>

---

## Interop References (parent folder, not this repo)

- `../extract-xiso/extract-xiso.c` - reference implementation source; rewrite path is `decode_xiso(k_rewrite)` → `traverse_xiso(k_generate_avl)` → `create_xiso` (always XSF layout, `in!xiso!` tag at 31337, `.xbe` media patch, `-s` skips `$SystemUpdate`)
- `../extract-xiso.exe`, `../xdvdfs.exe` - prebuilt reference binaries; `xdvdfs md5/checksum/info` is the arbiter for content-equality checks (same checksum = same content even when container sizes differ due to format offsets)
- `../games/` - test ISOs (large, multi-GB; copy to short paths before use)

---

## Platform-Specific Notes

- Windows: Uses windows crate for overlapped I/O in parallel extraction
- Unix: Uses libc + mmap for fast extraction
- CI tests on both ubuntu-latest and windows-latest
- Release builds target: x86_64-unknown-linux-gnu and x86_64-pc-windows-msvc
- Windows shell quoting: paths with spaces break naive quoting in pwsh tool calls; prefer python `subprocess.run([...])` via a script file, or copy test ISOs to short space-free paths first

---

## References

- Format layout cross-checked against iso2god-rs (https://github.com/iliazeus/iso2god-rs) and Free60 STFS docs
- Region bit mapping confirmed by reverse-engineering original Title Update Checker binary
- All parsing code is original - no code copied from referenced projects
