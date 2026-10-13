# tu_parser

[![CI](https://github.com/D221/tu_parser/actions/workflows/ci.yml/badge.svg)](https://github.com/D221/tu_parser/actions/workflows/ci.yml)

A small command-line tool that reads Xbox 360 **Title Update (TU)**,
**DLC / Marketplace**, **XBLA**, **avatar**, **theme**, and **indie** content
(`TU_*`, `tu*`, content hashes) stored in `LIVE`/STFS containers and prints
what they contain:
game title, media ID, region mask/regions (TU only), XEX execution info
(full game XEXs), and the file listing with sizes.

No GUI, no dependencies on the original tools — everything is parsed
directly from the TU file itself. Accepts all three STFS container magics:
`LIVE`, `CON `, and `PIRS` (identical header/table layout).

## Features

- Validates the `LIVE` container magic and (for title updates) the `TUPD`
  signature; detects content kind (`Title Update`, `Marketplace Content (DLC)`,
  `Xbox Live Arcade (XBLA)`, `Avatar Item`, `Theme`)
- Extracts the **media ID** (`0x354`), **base title ID** (`0x360`),
  and **display title** (UTF-16BE at `0x411`) from the STFS/CON header
- Parses the embedded game XEX `ExecutionId` field (`0x00040006`, per
  iso2god-rs `src/executable/xex.rs`) for media/version/title IDs
- Extracts the **region mask** (big-endian u32 at embedded `XEX2` + `0x1A0`)
  and decodes it to region names using the same bit tests as the original
  Title Update Checker (`PAL(A)`, `PAL(E)`, `NTSC/J`, `NTSC/J(J)`,
  `NTSC/J(C)`, `NTSC/J(0)`, `NTSC/U`, `Region Free`)
- Lists the STFS file table (64-byte entries at `0xC000`, densely packed —
  scan stops at the first all-zero slot, bounded by the embedded XEX offset):
  filename, table offset, file size (BE u32 at entry+`0x34`), directory flag
- Text or JSON output, single files or whole directories (`--dir`, `--recursive`)

## Building

Requires a Rust toolchain ([rustup](https://rustup.rs/)):

```bash
cargo build --release
# binary: target/release/tu_parser(.exe)
```

Prebuilt Windows and Linux binaries are attached to each
[release](https://github.com/D221/tu_parser/releases).

## Usage

```bash
# Single file
tu_parser "TU_1A58236_0000004000000.0000000000101"

# Multiple files
tu_parser file1 file2 --format json -o results.json

# Whole directory, optionally recursive
tu_parser --dir ./TUs --recursive

# Extra detail
tu_parser file --show-entries --show-strings -v
```

### Options

| Flag | Description |
| ---- | ----------- |
| `[FILES]...` | One or more TU files to parse |
| `--dir <DIR>` | Parse all files directly inside a directory (conflicts with `[FILES]`) |
| `--recursive` | With `--dir`: descend into subdirectories |
| `-f, --format <text\|json>` | Output format (default: `text`) |
| `-o, --output <FILE>` | Write output to a file instead of stdout |
| `--show-entries` | Always list all directory entries (default lists them when ≤ 50) |
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
