# AGENTS.md - x360-rs (tu_parser)

## Project Overview

**x360-rs** - Fast, streaming Xbox 360 container parser and extractor for:
- STFS containers: Title Updates (TU), DLC/Marketplace, XBLA, Avatar, Theme, Indie, GoD
- XISO/GDF images: XSF, XGD2, XGD1, XGD3 (raw and iso2god-optimized)
- Standalone executables: XEX (ExecutionId), XBE (original Xbox)
- Extracted game directories

Outputs: game title, media ID, base title ID, region mask/regions (TU only), XEX execution info, file listings with sizes.

No GUI, no dependencies on original tools - everything parsed directly from the container. Accepts all three STFS container magics: LIVE, CON , PIRS (identical layout).

---

## Build and Test Commands

| Task | Command |
|------|---------|
| Build release | cargo build --release |
| Run tests | cargo test --locked --verbose |
| Lint (clippy) | cargo clippy --all-targets -- -D warnings |
| Format check | cargo fmt --check |
| Full CI locally | cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test --locked --verbose && cargo build --release --locked --verbose |

Binary output: target/release/x360-rs (.exe on Windows)

---

## Project Structure

tu_parser/
├── Cargo.toml
├── Cargo.lock
├── LICENSE (MIT)
├── README.md
├── .gitignore
├── .github/workflows/
│   ├── ci.yml      # Runs on ubuntu-latest, windows-latest: fmt, clippy, test, build
│   └── release.yml # Triggered on v* tags, builds for Linux/Windows, publishes release
├── src/
│   ├── main.rs           # Entry point, CLI dispatch
│   ├── cli.rs            # clap argument parsing
│   ├── constants.rs      # Magic bytes, offsets, error types
│   ├── parse.rs          # Top-level parse dispatch (file magic to STFS/XISO/XEX/XBE/game-dir)
│   ├── stfs.rs           # STFS parsing, hash verification, extraction
│   ├── iso.rs            # XISO/GDF parsing (volume descriptor, directory tables, file walk)
│   ├── xex.rs            # XEX ExecutionId parsing
│   ├── tu_parser.rs      # TU-specific STFS parsing (header, file table, XEX region mask)
│   ├── tu_header.rs      # STFS/CON header field extraction
│   ├── regions.rs        # Region mask bit decoding (matches original Title Update Checker)
│   ├── report.rs         # Report types (Stfs, Xiso, Xex, Xbe, GameDir) for JSON/text output
│   ├── render.rs         # Text/JSON output formatting
│   ├── write/            # XISO creation/optimization (padding removal)
│   │   ├── mod.rs        # Public API (create/optimize entry points only)
│   │   ├── format.rs     # OutputFormat (Xsf/Xgd2/Xgd3/Xgd1) + sector offsets
│   │   ├── sector.rs     # SectorAllocator (linear, starts at 33)
│   │   ├── volume.rs     # Volume descriptor writer (sector 32, ECMA-119 tail at 0x7EC)
│   │   ├── dirtab.rs     # Tree builder, table sizing, index arena, dup checks (all iterative, no unsafe)
│   │   ├── img.rs        # Two-pass writer via Writer struct over arena indices (no unsafe)
│   │   └── create.rs     # Directory-to-ISO creation (iterative walk, skips output, rejects symlinks)
│   ├── extract/
│   │   ├── mod.rs        # Extraction orchestration
│   │   ├── basic.rs      # Basic sequential extraction
│   │   ├── parallel.rs   # Multi-threaded extraction (Windows default: parallel_4)
│   │   ├── mmap.rs       # mmap-based extraction (Unix default)
│   │   └── bench.rs      # Benchmark all extraction methods
│   └── tests.rs          # Unit tests with synthetic data (no external fixtures needed)

---

## Key Architecture Notes

- Streaming/zero-copy where possible - reads only needed windows from large files
- Auto-detects format by magic bytes: LIVE/CON /PIRS (STFS), XEX2, XBEH, else probes for XISO (MICROSOFT*XBOX*MEDIA at sector 0x20)
- STFS file table: 64-byte entries at 0xC000, densely packed, stops at first all-zero slot
- Region mask at XEX2 + 0x1A0 (BE u32), decoded using bit tests matching original Title Update Checker
- Hash verification (--verify): header SHA1, top hash, level-0 table hashes, every used data block hash
- Extraction: STFS (--extract) and XISO (--extract-iso) both create subfolder named by Title ID (XISO) or Base Title ID (STFS)
- XISO extraction auto-selects fastest method: parallel_4 on Windows, mmap on Unix
- JSON output: tagged unions with type: stfs | xiso | xex | xbe | gamedir
- Directory sweep (--dir --recursive): unrecognized files warn and continue (e.g., GOD DataNNNN parts)
- XISO optimize (--optimize): two-pass rebuild with contiguous sectors from 33; preserves input format (XSF/XGD2/XGD3/XGD1); --in-place creates .bak backup with restore-on-failure; --verify-output re-parses and compares entries
- XISO create (--create DIR -o out.iso --create-format xsf): walks directory, streams file bytes from disk; symlinks rejected

---

## Testing

- All tests are self-contained in src/tests.rs - they generate synthetic binary data in temp dirs
- No external test fixtures or network access required
- Tests cover: region decoding, STFS entry parsing, container magics, content kinds, XEX exec info, DLC parsing, XISO parsing, XBE parsing, unknown file rejection, game dir parsing, STFS verify/extract/tamper detection, multi-level refusal, path resolution, XISO optimize round-trip + padding removal + XGD3 create round-trip

Run specific test: cargo test <test_name> -- --nocapture

---

## Development Workflow

1. Make changes
2. Run cargo fmt (auto-formats)
3. Run cargo clippy --all-targets -- -D warnings (must pass)
4. Run cargo test --locked --verbose (must pass)
5. Run cargo build --release to verify release build

---

## Common Tasks

### Adding a new format / parser
1. Add detection logic in parse.rs::parse_file() (magic bytes)
2. Create new module or extend existing (e.g., iso.rs for XISO variants)
3. Add report variant in report.rs and rendering in render.rs
4. Add tests in tests.rs with synthetic data

### Adding a CLI option
1. Add to cli.rs::Args struct with clap attributes
2. Handle in main.rs dispatch logic
3. Update README.md usage/examples

### Modifying extraction
- STFS extraction: src/extract/basic.rs, parallel.rs, mmap.rs
- XISO extraction: same files, different entry points in extract/mod.rs
- Benchmark: cargo run -- --bench-extract-iso <file>

---

## Platform-Specific Notes

- Windows: Uses windows crate for overlapped I/O in parallel extraction
- Unix: Uses libc + mmap for fast extraction
- CI tests on both ubuntu-latest and windows-latest
- Release builds target: x86_64-unknown-linux-gnu and x86_64-pc-windows-msvc

---

## References

- Format layout cross-checked against iso2god-rs (https://github.com/iliazeus/iso2god-rs) and Free60 STFS docs
- Region bit mapping confirmed by reverse-engineering original Title Update Checker binary
- All parsing code is original - no code copied from referenced projects
