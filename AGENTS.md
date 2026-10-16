# AGENTS.md - x360-rs (tu_parser)

## Build / test / lint

| Task | Command |
|------|---------|
| Build release | `cargo build --release --locked` |
| Run tests | `cargo test --locked --verbose` |
| Single test | `cargo test <test_name> -- --nocapture` |
| Lint (clippy) | `cargo clippy --all-targets -- -D warnings` |
| Format check | `cargo fmt --check` |
| Full CI locally | `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test --locked --verbose && cargo build --release --locked --verbose` |

CI runs `fmt → clippy -D warnings → test --locked → release build --locked` on ubuntu-latest + windows-latest. Keep the tree warning-free under your local toolchain — newer clippy flags pre-existing code too.

Binary output: `target/release/x360-rs` (`.exe` on Windows).

## Key facts

- Crate/binary name: `x360-rs`. Entry point: `src/main.rs` (CLI dispatch in `src/cli.rs`).
- Auto-detects format by magic: `LIVE`/`CON `/`PIRS` (STFS), `XEX2`, `XBEH`, else XISO probe (`MICROSOFT*XBOX*MEDIA` at sector `0x20`).
- All parsing is streaming/seek-based — multi-GB ISOs never load fully into memory.
- All tests generate synthetic binary data in temp dirs; no external fixtures or network access. Do not add deps requiring network.
- STFS file table: 64-byte entries at `0xC000`, densely packed, stops at first all-zero slot.
- Region mask at XEX2 + `0x1A0` (BE u32), decoded via bit tests matching original Title Update Checker.
- STFS verify (`--verify`): header SHA1, top hash (T0/L1/L2 by package size), every table and used data block hash across all three levels.
- STFS extraction and XISO extraction both create subfolder named by Title ID (XISO) or Base Title ID (STFS).
- XISO extraction auto-selects fastest method: parallel on Windows for >2GB, mmap otherwise.
- Warnings (`warning: ...`) must stay visible — never hide skips/fallbacks behind `-q` or `-v`.
- `TODO.local.md` is gitignored — check boxes off as items land, but do not commit it.

## CLI flags (current)

Input: `[FILES]...`, `--dir <DIR>`, `--recursive`, `--game-dir <GAME_DIR>`
Report: `-f/--format <text|json>`, `-o/--output <FILE>`, `--show-strings`, `--show-entries`, `-q/--quiet`, `-v/--verbose`
STFS: `--verify`, `--extract <OUTDIR>`, `--extract-thumbnail <OUTDIR>`
XEX: `--dump-basefile <PATH>`, `--dump-resources <DIR>`
Profile: `--dump-account <PATH>` (decrypts RC4+HMAC-SHA1 encrypted Account file; REDACTs OnlineKey/passport fields)
XISO: `--extract-iso <OUTDIR>`, `--copy-out <PATH>`, `--md5`, `--checksum`, `--bench-extract-iso`, `--optimize`, `--truncate`, `--create <DIR>`, `--create-format <xsf|xgd2|xgd3|xgd1>`, `--in-place`, `-s/--skip-systemupdate`, `-m/--map <RULE>`, `--dry-run`, `--preserve-format`, `--no-media-patch`, `--verify-output`

When adding a CLI flag: add to `cli.rs::Args` with clap attributes + doc comment, handle in `main.rs` dispatch, update `README.md` Options table, verify `--help` output.

## Common tasks

### Adding a new format / parser
1. Add detection logic in `parse.rs::parse_file()` (magic bytes)
2. Create new module or extend existing (e.g., `iso.rs` for XISO variants)
3. Add report variant in `report.rs` and rendering in `render.rs`
4. Add tests in `tests.rs` with synthetic data

### Modifying extraction
- STFS: `src/extract/basic.rs`, `parallel.rs`, `mmap.rs`
- XISO: same files, entry point `extract_iso_fast_with_progress` in `extract/mod.rs`
- Single-file: `--copy-out <PATH> -o <DEST>` (xdvdfs `copy-out` equivalent); single input file, case-insensitive GDF match
- Content hashes: `--md5` (per-entry, `extract::md5_iso`) and `--checksum` (SHA3-256 over sorted `/path` keys plus file bytes, `extract::checksum_iso`)

## Platform notes

- Windows: `windows` crate for overlapped I/O in parallel extraction
- Unix: `libc` + mmap for fast extraction
- CI tests on ubuntu-latest + windows-latest
- Windows pwsh quirks: no `sed`; paths with spaces break naive quoting — prefer `python subprocess.run([...])` via script file, or copy test ISOs to short space-free paths first
- Release targets: `x86_64-unknown-linux-gnu` and `x86_64-pc-windows-msvc`

## Interop references (parent folder, not this repo)

- `../extract-xiso/extract-xiso.c` — reference implementation source
- `../extract-xiso.exe`, `../xdvdfs.exe` — prebuilt reference binaries; `xdvdfs md5/checksum/info` is the arbiter for content-equality checks
- `../games/` — test ISOs (large, multi-GB; copy to short paths before use)

## Out of scope

- FTP upload, GoD package creation, XCP, FMIM music rips, NAND/keyvault/SMC/XConfig, `xdvdfs.toml` spec files, GPU/audio/kernel emulation, UI, per-title memory patches, Xenia legacy `xe_xex2_*` C API.