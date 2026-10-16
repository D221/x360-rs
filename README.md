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
- Extracts **region mask** (BE u32 at `XEX2` + `0x1A0`) and decodes to region names using same bit tests as Title Update Checker
- Lists STFS file table (64-byte entries at `0xC000`, densely packed — stops at first all-zero slot): filename, table offset, file size, directory flag
- Parses raw **XISO/GDF** images (`MICROSOFT*XBOX*MEDIA` at sector `0x20`, XSF/XGD2/XGD1/XGD3): volume descriptor (magic, root table, **creation timestamp**), recursive directory tables with **file attributes** (read-only, hidden, system, directory, archive, device, normal) and **tree structure** (left/right child offsets), `default.xex`/`default.xbe` resolution — fully streaming, safe on multi-GB images
- Parses standalone **XEX** (`ExecutionId` + extended headers: module/system/image flags, ratings, LAN key, regions, entry addresses, TLS, static libs, PE name, page descriptors, bounding path/device ID, restrict KV/dates/console IDs, disc profile ID, alt title IDs, multidisc media IDs, image checksum/filetime, page heap) and **XBE** (title/version + certificate: title name, allowed media, region, ratings, LAN key) executables, **XDBF** title databases (XEX-embedded + GPD variants, with GPD achievement/title/setting decoders), **SPA** title databases embedded in XEX (`XTHD` title header, `XACH` achievement catalog, `XITB` image inventory, `XSTR` string table), and extracted game directories (`--game-dir`: tree stats + executable info)
- **Verifies** STFS hash chains (`--verify`): header SHA1, top hash (T0/L1/L2 by size), every table and used data block hash across all three levels. Exit code 1 on failure.
- **Extracts** STFS contents (`--extract <DIR>`) with tree recreation, verifying every block during the walk
- **Extracts** XISO/GDF contents (`--extract-iso <DIR>`) with multi-threaded parallel extraction (auto-selects fastest method: `parallel_4` on Windows, `mmap` otherwise). Benchmark mode (`--bench-extract-iso`) tests all methods and reports throughput
- **Extracts** XEX basefile (PE executable) to disk (`--dump-basefile`)
- **Extracts** XEX resources (SPA blobs, game icons) to files (`--dump-resources`)
- **Decrypts** Xbox 360 Profile `Account` files (`--dump-account <PATH>`): title name, allowed media, game region, ratings, disc number, LAN key, signature key; REDACTs OnlineKey and passport fields by default
- **Decrypts** STFS thumbnails (`--extract-thumbnail <DIR>`)
- Text or JSON output, single files or whole directories (`--dir`, `--recursive`). JSON reports are tagged unions (`"type": "stfs" | "xiso" | "xex" | "xbe" | "xdbf" | "gamedir"`). Unrecognized files in directory sweeps warn and continue instead of aborting.
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

# Dump XEX basefile (PE executable) from a XEX
x360-rs file.xex --dump-basefile basefile.bin

# Dump XEX resources (SPA blobs, game icons) to a directory
x360-rs file.xex --dump-resources res/

# Decrypt and dump an Xbox 360 Profile Account file
x360-rs file.account --dump-account -o account.json
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
| `--extract-thumbnail <DIR>` | Extract STFS dashboard thumbnails into `DIR` as `<base-title-id>_<kind>.<format>` files |
| `--extract-iso <DIR>` | Extract XISO/GDF contents into `DIR/<TitleID>/` (multi-threaded, auto-best method) |
| `--copy-out <PATH>` | Copy a single file or directory out of an XISO image (same as xdvdfs `copy-out`); uses `-o` as destination; requires one input file |
| `--md5` | Print MD5 checksums for entries in XISO image(s) (same as xdvdfs `md5`; files and directory tables) |
| `--checksum` | Print a deterministic content checksum identical to xdvdfs `checksum` (SHA3-256 over sorted `/path` keys plus file bytes) |
| `--bench-extract-iso` | Benchmark all XISO extraction methods, report fastest |
| `--dump-basefile <PATH>` | Extract XEX basefile (PE executable) to a file; use with a single XEX input file; `-o/--output` sets the destination |
| `--dump-resources <DIR>` | Extract XEX resources (SPA blobs, game icons) to a directory; use with a single XEX input file; creates one file per resource |
| `--dump-account <PATH>` | Decrypt and dump an Xbox 360 Profile Account file; use with a single Account file input; `-o/--output` sets the destination for the decrypted JSON report (default: stdout) |
| `--optimize` | Rebuild XISO with contiguous sectors (removes padding); uses `-o` as destination; `--in-place` overwrites with `.bak` backup; `--verify-output` re-parses and compares |
| `--truncate` | Drop trailing sectors past the last used one (Iso2God "Partial" equivalent); preserves layout and format byte-for-byte; uses `-o` as destination |
| `--create <DIR>` | Build a new XISO from an extracted game directory; uses `-o` as destination |
| `-m, --map <RULE>` | With `--create`: map host paths to image paths (same as xdvdfs `build-image -m`); repeatable; `"host/path:image/path"` with glob + `{n}` captures, or `"!host/path"` to exclude; unmatched files are not packed |
| `--dry-run` | With `--create`: list what would be packed without writing the image |
| `--create-format <xsf\|xgd2\|xgd3\|xgd1>` | Output image format for `--create` (default: `xsf`); `--optimize` produces `xsf` unless `--preserve-format` is given |
| `--in-place` | With `--optimize`: overwrite input (backup to `.bak.iso` first, restore on failure) |
| `--preserve-format` | With `--optimize`: keep the input format (`xgd3`/`xgd2`/`xgd1`) instead of default `xsf` output (advanced; larger files) |
| `--no-media-patch` | Disable automatic `.xbe` media-enable patching during create/optimize (same as extract-xiso `-m`; patching is on by default) |
| `-s, --skip-systemupdate` | Skip the `$SystemUpdate` folder during extract, optimize and create (same as extract-xiso `-s`) |
| `-q, --quiet` | Suppress console output (progress, reports); warnings/errors still print; `--output` file still written (conflicts with `-v`) |
| `--verify-output` | With `--optimize` or `--truncate`: re-parse output and verify entry fidelity |
| `-f, --format <text\|json>` | Output format (default: `text`) |
| `-o, --output <FILE>` | Write output to a file instead of stdout |
| `--show-entries` | Always list all directory entries (default lists them when ≤ 50); for XISO also shows attributes & tree offsets |
| `--show-strings` | List raw strings found in the file |
| `-v, --verbose` | Print parse progress to stderr |

### Example output

```
=== C:\Users\...\ExtractedDLCs\58410991\00000002\67BFF16771D4B5BA11057CC49F97E3BFB88D0AD858 ===
File size: 2691072 (2.6 MB)
Container: LIVE
Kind: Marketplace Content (DLC)
Base title ID: 58410991 (XBLA Game)
Content type: 0x00000002
Bootable: NO
Content size: 2646016 (2.5 MB)
Descriptor: STFS
Metadata version: 2
Transfer flags: C0 (DeviceIdTransfer, ProfileIdTransfer)
Licenses (2):
  FFFFFFFFFFFFFFFF bits=00000001 flags=00000001
  FFFFFFFFFFFFFFFF bits=00000001 flags=00000000
Thumbnails:
  thumbnail | Offset: 0x171A | Size: 13439 (13.1 KB) (png)
  title | Offset: 0x571A | Size: 13439 (13.1 KB) (png)
Title: ARKANOID Live! Episode Add-on Pack 2
Game title: ARKANOID Live!
Description: ARKANOID Live! Episode Pack 2
Also described as: ARKANOID Live! Episoden Add-On Pack 2 / ARKANOID Live! Episode en option-Série 2 / Pack de Episodio 2 de ARKANOID Live! / ARKANOID Live! Pack 2 aggiuntivo
Also known as: ARKANOID Live! Episode Pack 2 / ARKANOID Live! Episoden Add-On Pack 2 / ARKANOID Live! Episode en option-Série 2 / Pack de Episodio 2 de ARKANOID Live! / ARKANOID Live! Pack 2 aggiuntivo

Directory entries (8):
  [0] Season4 | Offset: 0xC000 | Size: 0 B (dir)
  [1] Season4/map.bin | Offset: 0xC040 | Size: 2511868 (2.4 MB)
  ...
```

```
=== C:\Users\...\4d53080f.xdbf ===
File size: 304575 (297.4 KB)
Format: XDBF title database (xex variant, magic 58444246, version 65536)
Title: Halo 2 (PC)

SPA records (11):
  XACH: 41 achievements
  XITB: 42 images
  XTHD: title 4D53080F type 1 (Full), version 1.0.133.0
  XSTR: 159 strings
```

```
=== C:\temp_x360\xenia\content\E030000078A5E451\FFFE07D1\00010000\E030000078A5E451\Account ===
{
  "flags": 0,
  "live_flags": 0,
  "gamertag": "test",
  "xuid": 0,
  "cached_user_flags": 0,
  "service_provider": "",
  "passcode": "00000000",
  "online_domain": "",
  "kerberos_realm": "",
  "online_key": "REDACTED",
  "passport_membername": "REDACTED",
  "passport_password": "REDACTED",
  "owner_passport_membername": "REDACTED"
}
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
- Optimization/extraction behavior verified against [extract-xiso](https://github.com/XboxDev/extract-xiso)
  (`-r` rewrite, `-s` skip, `-c` create, `in!xiso!` tag, `.xbe` media-enable
  patch) by in &lt;in@fishtank.com&gt;, and [xdvdfs](https://github.com/antangelo/xdvdfs)
  (`pack`/`unpack`/`copy-out`/`md5`/`checksum`/`build-image` semantics).
- XEX rating/region value tables from [Xenia](https://github.com/xenia-project/xenia)
  (`src/xenia/kernel/util/xex2_info.h`, BSD license).
- XBE certificate fields (title name, allowed media, region, ratings, LAN key)
  from [XexTool](https://github.com/xorloser/xextool) / [XexTool-RE](https://github.com/mrtitanic777/XexTool-RE)
  (xorloser, 2006-2017).
- SPA format layouts from [Free60 SPA docs](https://free60.org/System-Software/Formats/SPA/).
- Account file decryption from [Free60 Profile_Account docs](https://free60.org/System-Software/Profile_Account/)
  and Xenia `profile_manager.cc`.
- No code was copied from any of these projects; all parsing code here is original.

## License

MIT — see [LICENSE](LICENSE).