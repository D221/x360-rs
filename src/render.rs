//! Text/JSON report rendering.

use anyhow::Result;

use crate::cli::{Args, OutputFormat};
use crate::report::Report;
use crate::tu_parser::format_size;
use crate::xex::XexExecInfo;

pub(crate) fn report_summary(report: &Report) -> String {
    match report {
        Report::Stfs(i) => format!("{} {}", i.content_kind, i.display_title),
        Report::Xiso(i) => format!("XISO {} ({} entries)", i.iso_type, i.entries.len()),
        Report::Xex(i) => format!("XEX {}", i.exec.title_id),
        Report::Xbe(i) => format!("XBE {}", i.info.title_id),
        Report::GameDir(i) => format!("game dir ({} files)", i.files),
    }
}

pub(crate) fn xex_line(xex: &XexExecInfo) -> String {
    format!(
        "XEX Execution Info:\n  Media ID: {}\n  Version: {}\n  Base Version: {}\n  Title ID: {}\n  Platform: {}\n  Executable Type: {}\n  Disc: {}/{}\n",
        xex.media_id,
        xex.version,
        xex.base_version,
        xex.title_id,
        xex.platform,
        xex.executable_type,
        xex.disc_number,
        xex.disc_count
    )
}

pub(crate) fn render_report(report: &Report, show_entries: bool, show_strings: bool) -> String {
    let mut out = String::new();
    match report {
        Report::Stfs(info) => {
            out.push_str(&format!("=== {} ===\n", info.file_path));
            out.push_str(&format!("File size: {}\n", format_size(info.file_size)));
            out.push_str(&format!("Container: {}\n", info.container_magic));
            out.push_str(&format!(
                "LIVE container: {}\n",
                if info.is_live { "YES" } else { "NO" }
            ));
            out.push_str(&format!(
                "Title Update: {}\n",
                if info.is_title_update { "YES" } else { "NO" }
            ));
            out.push_str(&format!("Kind: {}\n", info.content_kind));
            out.push_str(&format!("Media ID: {}\n", info.media_id));
            out.push_str(&format!("Base title ID: {}\n", info.base_title_id));
            out.push_str(&format!("Content type: {}\n", info.content_type));
            out.push_str(&format!("Title: {}\n", info.display_title));
            out.push_str(&format!(
                "Region mask: {}\n",
                info.region_mask.as_deref().unwrap_or("n/a")
            ));
            out.push_str(&format!(
                "Regions: {}\n",
                if info.regions.is_empty() {
                    "n/a".to_string()
                } else {
                    info.regions.join(" ")
                }
            ));
            if !info.regions_detailed.is_empty() {
                out.push_str(&format!(
                    "Regions (detailed): {}\n",
                    info.regions_detailed.join(" ")
                ));
            }
            if let Some(xex) = &info.xex_exec {
                out.push_str(&xex_line(xex));
            }
            if let Some(v) = &info.verify {
                if v.ok {
                    out.push_str(&format!(
                        "Verify: OK ({} blocks, {} tables)\n",
                        v.blocks_checked, v.tables_checked
                    ));
                } else {
                    out.push_str(&format!(
                        "Verify: FAIL ({} blocks, {} tables)\n",
                        v.blocks_checked, v.tables_checked
                    ));
                    for e in v.errors.iter().take(10) {
                        out.push_str(&format!("  ! {}\n", e));
                    }
                }
            }
            render_entries(
                &mut out,
                &info
                    .directory_entries
                    .iter()
                    .map(|e| (e.path.as_str(), e.offset, e.size, e.is_dir))
                    .collect::<Vec<_>>(),
                show_entries,
            );
            if show_strings {
                out.push_str(&format!(
                    "\nRaw strings found ({}):\n",
                    info.raw_strings.len()
                ));
                for (offset, s) in &info.raw_strings {
                    out.push_str(&format!("  0x{:08X}: {}\n", offset, s));
                }
            }
            out.push('\n');
        }
        Report::Xiso(info) => {
            out.push_str(&format!("=== {} ===\n", info.file_path));
            out.push_str(&format!("File size: {}\n", format_size(info.file_size)));
            out.push_str(&format!("Format: XISO ({})\n", info.iso_type));
            out.push_str(&format!(
                "Root table: sector {} ({} bytes)\n",
                info.root_sector, info.root_size
            ));
            out.push_str(&format!("Volume sectors: {}\n", info.volume_sectors));
            if let Some(ts) = info.volume_timestamp {
                // Convert Windows FILETIME (100ns intervals since 1601-01-01) to readable format
                // For now just show the raw value; could use chrono for conversion
                out.push_str(&format!("Volume timestamp: {} (FILETIME)\n", ts));
            }
            match &info.executable {
                Some(e) => out.push_str(&format!("Executable: {}\n", e)),
                None => out.push_str("Executable: not found\n"),
            }
            if let Some(xex) = &info.xex_exec {
                out.push_str(&xex_line(xex));
            }
            if let Some(xbe) = &info.xbe_info {
                out.push_str(&format!(
                    "XBE: title {} version {}\n",
                    xbe.title_id, xbe.version
                ));
            }
            render_entries(
                &mut out,
                &info
                    .entries
                    .iter()
                    .map(|e| (e.path.as_str(), e.sector as u64, e.size, e.is_dir))
                    .collect::<Vec<_>>(),
                show_entries,
            );
            // Extended attributes display for XISO (when --show-entries)
            if show_entries && !info.entries.is_empty() {
                out.push_str("\nDirectory entry details:\n");
                for e in &info.entries {
                    let mut attr_strs = Vec::new();
                    if e.attrs.read_only {
                        attr_strs.push("R");
                    }
                    if e.attrs.hidden {
                        attr_strs.push("H");
                    }
                    if e.attrs.system {
                        attr_strs.push("S");
                    }
                    if e.attrs.directory {
                        attr_strs.push("D");
                    }
                    if e.attrs.archive {
                        attr_strs.push("A");
                    }
                    if e.attrs.normal {
                        attr_strs.push("N");
                    }
                    let attrs = if attr_strs.is_empty() {
                        "—".to_string()
                    } else {
                        attr_strs.join("")
                    };
                    let l = e
                        .left_child_offset
                        .map(|v| format!("0x{:X}", v))
                        .unwrap_or("—".to_string());
                    let r = e
                        .right_child_offset
                        .map(|v| format!("0x{:X}", v))
                        .unwrap_or("—".to_string());
                    out.push_str(&format!(
                        "  {} | Sector: 0x{:X} | Size: {} | Attrs: {} | L:{} R:{}{}\n",
                        e.path,
                        e.sector,
                        format_size(e.size),
                        attrs,
                        l,
                        r,
                        if e.is_dir { " (dir)" } else { "" }
                    ));
                }
            }
            out.push('\n');
        }
        Report::Xex(info) => {
            out.push_str(&format!("=== {} ===\n", info.file_path));
            out.push_str(&format!("File size: {}\n", format_size(info.file_size)));
            out.push_str("Format: XEX executable\n");
            out.push_str(&xex_line(&info.exec));
            out.push('\n');
        }
        Report::Xbe(info) => {
            out.push_str(&format!("=== {} ===\n", info.file_path));
            out.push_str(&format!("File size: {}\n", format_size(info.file_size)));
            out.push_str("Format: XBE executable (original Xbox)\n");
            out.push_str(&format!(
                "XBE: title {} version {}\n",
                info.info.title_id, info.info.version
            ));
            out.push('\n');
        }
        Report::GameDir(info) => {
            out.push_str(&format!("=== {} ===\n", info.dir_path));
            out.push_str("Format: extracted game directory\n");
            out.push_str(&format!(
                "Files: {}  Dirs: {}  Total: {}\n",
                info.files,
                info.dirs,
                format_size(info.total_bytes)
            ));
            match &info.executable {
                Some(e) => out.push_str(&format!("Executable: {}\n", e)),
                None => out.push_str("Executable: default.xex/.xbe not found\n"),
            }
            if let Some(xex) = &info.xex_exec {
                out.push_str(&xex_line(xex));
            }
            if let Some(xbe) = &info.xbe_info {
                out.push_str(&format!(
                    "XBE: title {} version {}\n",
                    xbe.title_id, xbe.version
                ));
            }
            out.push('\n');
        }
    }
    out
}

pub(crate) fn render_entries(out: &mut String, entries: &[(&str, u64, u64, bool)], show_all: bool) {
    // entries: (name, offset-or-sector, size, is_dir). STFS passes table
    // offsets, XISO passes sector numbers; both print as hex.
    if show_all || entries.len() <= 50 {
        out.push_str(&format!("\nDirectory entries ({}):\n", entries.len()));
        for (i, (name, at, size, is_dir)) in entries.iter().enumerate() {
            out.push_str(&format!(
                "  [{}] {} | Offset: 0x{:X} | Size: {}{}\n",
                i,
                name,
                at,
                format_size(*size),
                if *is_dir { " (dir)" } else { "" }
            ));
        }
    } else {
        out.push_str(&format!(
            "\nDirectory entries ({}): [use --show-entries to list all]\n",
            entries.len()
        ));
        for (i, (name, at, size, is_dir)) in entries.iter().take(10).enumerate() {
            out.push_str(&format!(
                "  [{}] {} | Offset: 0x{:X} | Size: {}{}\n",
                i,
                name,
                at,
                format_size(*size),
                if *is_dir { " (dir)" } else { "" }
            ));
        }
        out.push_str(&format!("  ... and {} more\n", entries.len() - 10));
    }
}

pub(crate) fn emit(
    args: &Args,
    reports: &[Report],
    show_entries: bool,
    show_strings: bool,
) -> Result<()> {
    let output = match args.format {
        OutputFormat::Json => serde_json::to_string_pretty(&reports)?,
        OutputFormat::Text => {
            let mut out = String::new();
            for report in reports {
                out.push_str(&render_report(report, show_entries, show_strings));
            }
            out
        }
    };
    if let Some(output_path) = &args.output {
        std::fs::write(output_path, output)?;
    } else {
        print!("{}", output);
    }
    Ok(())
}
