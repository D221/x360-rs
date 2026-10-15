# x360-rs

[![CI](https://github.com/D221/x360-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/D221/x360-rs/actions/workflows/ci.yml)

A fast, streaming Xbox 360 container parser & extractor for:
- **STFS** containers: Title Updates (TU), DLC/Marketplace, XBLA, Avatar, Theme, Indie, GoD
- **XISO/GDF** images: XSF, XGD2, XGD1, XGD3 (raw & iso2god-optimized)
- **Standalone executables**: XEX (ExecutionId), XBE (original Xbox)
- **Extracted game directories**

Outputs game title, media ID, base title ID, region mask/regions (TU only), XEX execution info, and file listings with sizes.

No GUI, no dependencies on original tools — everything parsed directly from the container. Accepts all three STFS container magics: `LIVE`, `CON `, `PIRS` (identical layout).

## Features

- Validates `LIVE` container magic and (for TUs) `TUPD` signature; detects content kind (`Title Update`, `Marketplace Content (DLC)`, `Xbox Live Arcade (XBLA)`, `Avatar Item`, `Theme`, `Games on Demand (GoD)`)
- Extracts **media ID** (`0x354`), **base title ID** (`0x360`), **display title** (UTF-16BE at `0x411`) from STFS/CON header
- Parses embedded game XEX `ExecutionId` field (`0x00040006`, per iso2god-rs) for media/version/title IDs
- Extracts **region mask** (BE u32 at `XEX2` + `0x1A0`) and decodes to region names using same bit tests as Title Update Checker (`PAL(A)`, `PAL(E)`, `NTSC/J`, `NTSC/J(J)`, `NTSC/J(C)`, `NTSC/J(0)`, `NTSC/U`, `Region Free`)
- Lists STFS file table (64-byte entries at `0xC000`, densely packed — stops at first all-zero slot): filename, table offset, file size (BE u32 at entry+`0x34`), directory flag
- Parses raw **XISO/GDF** images (`MICROSOFT*XBOX*MEDIA` at sector `0x20`, XSF/XGD2/XGD1/XGD3): volume descriptor (magic, root table, **creation timestamp**), recursive directory tables with **file attributes** (read-only, hidden, system, directory, archive, normal) and **tree structure** (left/right child offsets), `default.xex`/`default.xbe` resolution — fully streaming, safe on multi-GB images
- Parses standalone **XEX** (`ExecutionId`) and **XBE** (title/version) executables, and extracted game directories (`--game-dir`: tree stats + executable info)
- **Verifies** STFS hash chains (`--verify`): header SHA1, top hash, level-0 table hashes, every used data block hash. Exit code 1 on failure.
- **Extracts** STFS contents (`--extract <DIR>`) with tree recreation, verifying every block during the walk
- **Extracts** XISO/GDF contents (`--extract-iso <DIR>`) with multi-threaded parallel extraction (auto-selects fastest method: `parallel_4` on Windows, `mmap` on Unix). Benchmark mode (`--bench-extract-iso`) tests all methods and reports throughput.
- Text or JSON output, single files or whole directories (`--dir`, `--recursive`). JSON reports are tagged unions (`"type": "stfs" | "xiso" | "xex" | "xbe" | "gamedir"`). Unrecognized files in directory sweeps warn and continue instead of aborting.
- Extraction creates a subfolder named by Title ID (XISO) or Base Title ID (STFS) inside the output directory.

## Building

Requires a Rust toolchain ([rustup](https://rustup.rs/)):

```bash
cargo build --release
# binary: target/release/x360-rs(.exe)
```

Prebuilt Windows and Linux binaries are attached to each
[release](https://github.com/D221/x360-rs/releases).

## Usage

```bash
# Single file
x360-rs "TU_1A58236_0000004000000.0000000000101"

# Multiple files
x360-rs file1 file2 --format json -o results.json

# Whole directory, optionally recursive
x360-rs --dir ./TUs --recursive

# Extracted game directory
x360-rs --game-dir ./extracted-game

# Verify hashes (exit 1 on failure)
x360-rs file --verify

# Extract STFS contents with tree recreation (hash-verified)
x360-rs file --extract ./out

# Extract XISO/GDF contents (auto-creates TitleID subfolder)
x360-rs file --extract-iso ./out

# Benchmark extraction methods
x360-rs file --bench-extract-iso

# Optimize/repack XISO (removes padding, contiguous sectors)
x360-rs game.iso --optimize -o game_opt.iso
x360-rs game.iso --optimize --in-place --verify-output

# Create XISO from an extracted directory
x360-rs --create ./extracted-game -o game.iso
x360-rs --create ./extracted-game -o game.xgd3.iso --create-format xgd3

# Extra detail
x360-rs file --show-entries --show-strings -v
```

### Options

| Flag | Description |
| ---- | ----------- |
| `[FILES]...` | One or more files to parse (format auto-detected) |
| `--dir <DIR>` | Parse all files directly inside a directory (conflicts with `[FILES]`) |
| `--recursive` | With `--dir`: descend into subdirectories |
| `--game-dir <DIR>` | Parse an extracted game directory (tree stats + `default.xex`/`.xbe`) |
| `--verify` | Verify STFS hashes (header, tables, data blocks); exit 1 on failure |
| `--extract <DIR>` | Extract STFS contents into `DIR/<BaseTitleID>/` with tree recreation (hash-verified) |
| `--extract-iso <DIR>` | Extract XISO/GDF contents into `DIR/<TitleID>/` (multi-threaded, auto-best method) |
| `--copy-out <PATH>` | Copy a single file or directory out of an XISO image (same as xdvdfs `copy-out`); uses `-o` as destination; requires one input file |
| `--md5` | Print MD5 checksums for files in XISO image(s) (same as xdvdfs `md5`, files only) |
| `--checksum` | Print a deterministic content checksum identical to xdvdfs `checksum` (SHA3-256 over sorted `/path` keys plus file bytes) |
| `--bench-extract-iso` | Benchmark all XISO extraction methods, report fastest |
| `--optimize` | Rebuild XISO with contiguous sectors (removes padding); uses `-o` as destination; `--in-place` overwrites with `.bak` backup; `--verify-output` re-parses and compares |
| `--create <DIR>` | Build a new XISO from an extracted game directory; uses `-o` as destination |
| `-m, --map <RULE>` | With `--create`: map host paths to image paths (same as xdvdfs `build-image -m`); repeatable; `"host/path:image/path"` with glob + `{n}` captures, or `"!host/path"` to exclude; unmatched files are not packed |
| `--dry-run` | With `--create`: list what would be packed without writing the image |
| `--create-format <xsf\|xgd2\|xgd3\|xgd1>` | Output image format for `--create` (default: `xsf`); `--optimize` produces `xsf` unless `--preserve-format` is given |
| `--in-place` | With `--optimize`: overwrite input (backup to `.bak.iso` first, restore on failure) |
| `--preserve-format` | With `--optimize`: keep the input format (`xgd3`/`xgd2`/`xgd1`) instead of default `xsf` output (advanced; larger files) |
| `--no-media-patch` | Disable automatic `.xbe` media-enable patching during create/optimize (same as extract-xiso `-m`; patching is on by default) |
| `-s, --skip-systemupdate` | Skip the `$SystemUpdate` folder during extract, optimize and create (same as extract-xiso `-s`) |
| `-q, --quiet` | Suppress console output (progress, reports); warnings/errors still print, `--output` file still written (conflicts with `-v`) |
| `--verify-output` | With `--optimize`: re-parse output and verify entry fidelity |
| `-f, --format <text\|json>` | Output format (default: `text`) |
| `-o, --output <FILE>` | Write output to a file instead of stdout |
| `--show-entries` | Always list all directory entries (default lists them when ≤ 50); for XISO also shows attributes & tree offsets |
| `--show-strings` | List raw strings found in the file |
| `-v, --verbose` | Print parse progress to stderr |

### Example output

```
=== TU_1A58236_0000004000000.0000000000101 ===
File size: 2985984 (2.8 MB)
LIVE container: YES
Title Update: YES
Media ID: 2D46D3C7
Base title ID: 54540866
Content type: 0x000B0000
Title: Borderlands Title Update #4
Region mask: 0000FD00
Regions: NTSC/J(J) NTSC/J(0)
Regions (detailed): NTSC/J(J) NTSC/J(0)

Directory entries (22):
  [0] Combined.cfg | Offset: 0xC000 | Size: 223250 (218.0 KB)
  [1] default.xexp | Offset: 0xC040 | Size: 1865728 (1.8 MB)
  [2] WillowGame | Offset: 0xC080 | Size: 0 B (dir)
  ...
```

## File format notes

All offsets are file offsets into the TU container:

| Offset | Size | Content |
| ------ | ---- | ------- |
| `0x0000` | 4 | `LIVE` magic (LE u32 `0x4556494C`) |
| `0x0344` | 4 | Content type, BE u32 (`0x000B0000` for title updates) |
| `0x0354` | 4 | Media ID, BE u32 |
| `0x0360` | 4 | Base game title ID, BE u32 |
| `0x0411` | var | Display title, UTF-16BE, NUL-terminated |
| `0x971A` | 4 | `TUPD` signature (LE u32 `0x44505554`) |
| `0xC000` | `n × 64` | STFS file table (dense; stops at first zero slot, bounded by XEX offset) |
| `XEX2+0x1A0` | 4 | Region mask, BE u32 |

STFS file-table entry (64 bytes):

| Offset in entry | Content |
| --------------- | ------- |
| `0x00` | Filename, ASCII, NUL-padded (40 bytes) |
| `0x28` | Flags (`0x80` = directory) |
| `0x34` | File size, BE u32 (0 for directories) |

Region mask bits (as tested by the original checker):

| Bits | Region |
| ---- | ------ |
| `0x00010000` | PAL(A) |
| `0x00FE0000` | PAL(E) |
| `0x0000FF00` | NTSC/J |
| `0x00000100` | NTSC/J(J) |
| `0x00000200` | NTSC/J(C) |
| `0x0000FC00` | NTSC/J(0) |
| `0x000000FF` | NTSC/U |
| `0xFFFFFFFF` | Region Free |

## Credits / references

- Format layout cross-checked against [iso2god-rs](https://github.com/iliazeus/iso2god-rs)
  (`src/god/con_header.rs`: content type `0x344`, media ID `0x354`,
  title ID `0x360`, UTF-16BE title `0x411`) and the STFS format documentation
  on [Free60](https://free60.org/System-Software/Formats/STFS/).
- Region bit mapping confirmed by reverse-engineering the original
  *Title Update Checker* binary (region checks at `0x4032FE–0x4036A6`).
- No code was copied from any of these projects; all parsing code here is original.

## License

MIT — see [LICENSE](LICENSE).
