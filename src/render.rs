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
        Report::Xdbf(i) => format!(
            "XDBF {} resources{}",
            i.num_entries,
            i.title_name
                .as_ref()
                .map(|t| format!(" ({t})"))
                .unwrap_or_default()
        ),
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
            if info.is_title_update {
                out.push_str("Title Update: YES\n");
            }
            out.push_str(&format!("Kind: {}\n", info.content_kind));
            if info.media_id.chars().any(|c| c != '0') {
                out.push_str(&format!("Media ID: {}\n", info.media_id));
            }
            out.push_str(&format!(
                "Base title ID: {} ({})\n",
                info.base_title_id, info.title_class
            ));
            out.push_str(&format!("Content type: {}\n", info.content_type));
            out.push_str(&format!(
                "Bootable: {}\n",
                if info.bootable { "YES" } else { "NO" }
            ));
            out.push_str(&format!(
                "Content size: {}\n",
                format_size(info.content_size)
            ));
            if info.version.chars().any(|c| c != '0') || info.base_version.chars().any(|c| c != '0')
            {
                out.push_str(&format!(
                    "Version: {} (base {})\n",
                    info.version, info.base_version
                ));
            }
            out.push_str(&format!("Descriptor: {}\n", info.descriptor_type));
            out.push_str(&format!("Metadata version: {}\n", info.metadata_version));
            if info.data_file_count > 0 {
                out.push_str(&format!(
                    "Data files: {} ({})\n",
                    info.data_file_count,
                    format_size(info.data_file_combined_size)
                ));
            }
            if info.transfer_flags != "00" {
                out.push_str(&format!("Transfer flags: {}\n", info.transfer_flags));
            }
            if !info.licenses.is_empty() {
                out.push_str(&format!("Licenses ({}):\n", info.licenses.len()));
                for l in info.licenses.iter().take(16) {
                    out.push_str(&format!("  {l}\n"));
                }
            }
            if !info.thumbnails.is_empty() {
                out.push_str("Thumbnails:\n");
                for t in &info.thumbnails {
                    out.push_str(&format!(
                        "  {} | Offset: 0x{:X} | Size: {} ({})\n",
                        t.kind,
                        t.offset,
                        format_size(t.size),
                        t.format
                    ));
                }
            }
            if info.save_game_id != "00000000" {
                out.push_str(&format!("Save game ID: {}\n", info.save_game_id));
            }
            if info.console_id.chars().any(|c| c != '0') {
                out.push_str(&format!("Console ID: {}\n", info.console_id));
            }
            if info.profile_id.chars().any(|c| c != '0') {
                out.push_str(&format!("Profile ID: {}\n", info.profile_id));
            }
            if info.device_id.chars().any(|c| c != '0') {
                out.push_str(&format!("Device ID: {}\n", info.device_id));
            }
            out.push_str(&format!("Title: {}\n", info.display_title));
            if !info.publisher_name.is_empty() {
                out.push_str(&format!("Publisher: {}\n", info.publisher_name));
            }
            if !info.game_title_name.is_empty() && info.game_title_name != info.display_title {
                out.push_str(&format!("Game title: {}\n", info.game_title_name));
            }
            if !info.description.is_empty() {
                out.push_str(&format!("Description: {}\n", info.description));
            }
            if !info.locale_descriptions.is_empty() {
                out.push_str(&format!(
                    "Also described as: {}\n",
                    info.locale_descriptions.join(" / ")
                ));
            }
            if !info.locale_titles.is_empty() {
                out.push_str(&format!(
                    "Also known as: {}\n",
                    info.locale_titles.join(" / ")
                ));
            }
            if let Some(mask) = info.region_mask.as_deref() {
                out.push_str(&format!("Region mask: {mask}\n"));
            }
            if !info.regions.is_empty() {
                out.push_str(&format!("Regions: {}\n", info.regions.join(" ")));
            }
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
                // Windows FILETIME (100ns intervals since 1601-01-01 UTC),
                // converted without extra deps (same algorithm as gpd.rs).
                let readable = crate::gpd::filetime_string(ts)
                    .map(|s| format!(" = {s}"))
                    .unwrap_or_default();
                out.push_str(&format!("Volume timestamp: {ts} (FILETIME){readable}\n"));
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
                    if e.attrs.device {
                        attr_strs.push("V");
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
            if let Some(x) = &info.extended {
                out.push_str(&format!(
                    "Module flags: {:08X}{}\n",
                    x.module_flags,
                    if x.module_flag_names.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", x.module_flag_names.join(", "))
                    }
                ));
                match &x.ratings {
                    Some(r) => {
                        let rated: Vec<&str> = x
                            .rating_labels
                            .iter()
                            .filter(|l| !l.ends_with("=Unrated"))
                            .map(String::as_str)
                            .collect();
                        if rated.is_empty() {
                            out.push_str(&format!("Game ratings: Unrated ({r})\n"));
                        } else {
                            out.push_str(&format!("Game ratings: {} ({r})\n", rated.join(" ")));
                        }
                    }
                    None => out.push_str("Game ratings: not present\n"),
                }
                match &x.lan_key {
                    Some(k) => out.push_str(&format!("LAN key: {k}\n")),
                    None => out.push_str("LAN key: not present\n"),
                }
                match x.security_region {
                    Some(r) => out.push_str(&format!(
                        "Security region: {r:08X} ({})\n",
                        x.region_names.join(" ")
                    )),
                    None => out.push_str("Security region: not present\n"),
                }
                if !x.headers.is_empty() {
                    out.push_str(&format!("Optional headers ({}):\n", x.headers.len()));
                    for h in &x.headers {
                        out.push_str(&format!("  {}: {}\n", h.id, h.data));
                    }
                }
                if let Some(v) = x.entry_point {
                    out.push_str(&format!("Entry point: {v:08X}\n"));
                }
                if let Some(v) = x.image_base {
                    out.push_str(&format!("Image base: {v:08X}\n"));
                }
                if let Some(v) = x.original_base {
                    out.push_str(&format!("Original base: {v:08X}\n"));
                }
                if let Some(v) = x.stack_size {
                    out.push_str(&format!("Stack size: {}\n", format_size(v as u64)));
                }
                if let Some(v) = x.heap_size {
                    out.push_str(&format!("Heap size: {}\n", format_size(v as u64)));
                }
                if let Some(v) = x.fs_cache_size {
                    out.push_str(&format!("FS cache size: {}\n", format_size(v as u64)));
                }
                if let Some(v) = x.system_flags {
                    out.push_str(&format!(
                        "System flags: {v:08X}{}\n",
                        if x.system_flag_names.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", x.system_flag_names.join(", "))
                        }
                    ));
                }
                if let Some(t) = &x.tls {
                    out.push_str(&format!(
                        "TLS: {} slots, data {:08X} ({} / raw {})\n",
                        t.slots,
                        t.data_address,
                        format_size(t.data_size as u64),
                        format_size(t.raw_size as u64)
                    ));
                }
                if !x.static_libs.is_empty() {
                    out.push_str(&format!("Static libs ({}):\n", x.static_libs.len()));
                    for l in x.static_libs.iter().take(20) {
                        out.push_str(&format!("  {} {} ({})\n", l.name, l.version, l.approval));
                    }
                }
                if let Some(n) = &x.pe_name {
                    out.push_str(&format!("Original PE name: {n}\n"));
                }
                if let Some(v) = x.image_flags {
                    out.push_str(&format!(
                        "Image flags: {v:08X}{}\n",
                        if x.image_flag_names.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", x.image_flag_names.join(", "))
                        }
                    ));
                }
                if let Some(v) = x.allowed_media {
                    out.push_str(&format!(
                        "Allowed media: {v:08X}{}\n",
                        if x.allowed_media_names.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", x.allowed_media_names.join(", "))
                        }
                    ));
                }
                if let Some(m) = &x.xgd2_media_id {
                    out.push_str(&format!("XGD2 media ID: {m}\n"));
                }
                if let Some(v) = x.load_address {
                    out.push_str(&format!("Load address: {v:08X}\n"));
                }
                if !x.page_descriptors.is_empty() {
                    out.push_str(&format!(
                        "Page descriptors ({}):\n",
                        x.page_descriptors.len()
                    ));
                    for p in x.page_descriptors.iter().take(8) {
                        out.push_str(&format!(
                            "  info={:08X} pages={} digest={}…\n",
                            p.info,
                            p.page_count,
                            &p.digest[..16.min(p.digest.len())]
                        ));
                    }
                    if x.page_descriptors.len() > 8 {
                        out.push_str(&format!(
                            "  ... and {} more\n",
                            x.page_descriptors.len() - 8
                        ));
                    }
                }
                if let Some(v) = x.system_flags2 {
                    out.push_str(&format!(
                        "System flags 2: {v:08X}{}\n",
                        if x.system_flag2_names.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", x.system_flag2_names.join(", "))
                        }
                    ));
                }
                if let Some(v) = &x.savegame_id {
                    out.push_str(&format!("Save game ID: {v}\n"));
                }
                if let Some(v) = &x.bounding_path {
                    out.push_str(&format!("Bounding path: {v}\n"));
                }
                if let Some(v) = &x.bounding_device_id {
                    out.push_str(&format!("Bounding device ID: {v}\n"));
                }
                if !x.restrict_console_ids.is_empty() {
                    out.push_str(&format!(
                        "Restrict console IDs ({}):\n",
                        x.restrict_console_ids.len()
                    ));
                    for id in x.restrict_console_ids.iter().take(10) {
                        out.push_str(&format!("  {id}\n"));
                    }
                    if x.restrict_console_ids.len() > 10 {
                        out.push_str(&format!(
                            "  ... and {} more\n",
                            x.restrict_console_ids.len() - 10
                        ));
                    }
                }
                if let Some((mask, val)) = x.restrict_kv_privs {
                    out.push_str(&format!(
                        "Restrict KV privs: mask={mask:016X} val={val:016X}\n"
                    ));
                }
                if let Some((start, end)) = x.restrict_dates {
                    out.push_str(&format!(
                        "Restrict dates: start={start:016X} end={end:016X}\n"
                    ));
                }
                if let Some(v) = &x.disc_profile_id {
                    out.push_str(&format!("Disc profile ID: {v}\n"));
                }
                if !x.alt_title_ids.is_empty() {
                    out.push_str(&format!("Alt title IDs ({}):\n", x.alt_title_ids.len()));
                    for id in x.alt_title_ids.iter().take(10) {
                        out.push_str(&format!("  {id:08X}\n"));
                    }
                    if x.alt_title_ids.len() > 10 {
                        out.push_str(&format!("  ... and {} more\n", x.alt_title_ids.len() - 10));
                    }
                }
                if !x.multidisc_media_ids.is_empty() {
                    out.push_str(&format!(
                        "Multidisc media IDs ({}):\n",
                        x.multidisc_media_ids.len()
                    ));
                    for id in x.multidisc_media_ids.iter().take(10) {
                        out.push_str(&format!("  {id}\n"));
                    }
                    if x.multidisc_media_ids.len() > 10 {
                        out.push_str(&format!(
                            "  ... and {} more\n",
                            x.multidisc_media_ids.len() - 10
                        ));
                    }
                }
                if let Some(v) = x.image_checksum {
                    out.push_str(&format!("Image checksum: {v:08X}\n"));
                }
                if let Some(v) = x.image_filetime {
                    out.push_str(&format!("Image filetime: {v:016X}\n"));
                }
                if let Some((size, flags)) = x.page_heap {
                    out.push_str(&format!("Page heap: size={size} flags={flags:08X}\n"));
                }
                if let Some(v) = &x.logo_data {
                    out.push_str(&format!("Logo data: {v}…\n"));
                }
                if let Some(v) = x.basefile_enc_type {
                    out.push_str(&format!(
                        "Basefile encryption: {v}{}\n",
                        if v == 0 {
                            " (not encrypted)"
                        } else {
                            " (encrypted)"
                        }
                    ));
                }
                if let Some(v) = x.basefile_comp_type {
                    out.push_str(&format!(
                        "Basefile compression: {v}{}\n",
                        match v {
                            1 => " (raw)",
                            2 => " (compressed)",
                            3 => " (delta-compressed)",
                            _ => " (unknown)",
                        }
                    ));
                }
                if let Some(v) = x.basefile_offset {
                    out.push_str(&format!("Basefile offset: 0x{:X}\n", v));
                }
                if let Some(v) = x.basefile_size {
                    out.push_str(&format!("Basefile size: {} ({})\n", format_size(v), v));
                }
            }
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
            if let Some(name) = &info.info.cert_title_name {
                out.push_str(&format!("Certificate title: {name}\n"));
            }
            if let Some(v) = info.info.cert_size {
                out.push_str(&format!("Certificate size: {v}\n"));
            }
            if let Some(v) = info.info.allowed_media {
                out.push_str(&format!(
                    "Allowed media: {v:08X}{}\n",
                    if info.info.allowed_media_names.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", info.info.allowed_media_names.join(", "))
                    }
                ));
            }
            if let Some(v) = info.info.game_region {
                out.push_str(&format!(
                    "Game region: {v:08X}{}\n",
                    if info.info.game_region_names.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", info.info.game_region_names.join(", "))
                    }
                ));
            }
            if let Some(v) = info.info.game_ratings {
                out.push_str(&format!("Game ratings: {v:08X}\n"));
            }
            if let Some(v) = info.info.disk_number {
                out.push_str(&format!("Disc number: {v}\n"));
            }
            if let Some(v) = &info.info.cert_version {
                out.push_str(&format!("Certificate version: {v}\n"));
            }
            if let Some(k) = &info.info.lan_key {
                out.push_str(&format!("LAN key: {k}\n"));
            }
            if let Some(k) = &info.info.signature_key {
                out.push_str(&format!("Signature key: {k}\n"));
            }
            out.push('\n');
        }
        Report::Xdbf(info) => {
            out.push_str(&format!("=== {} ===\n", info.file_path));
            out.push_str(&format!("File size: {}\n", format_size(info.file_size)));
            out.push_str(&format!(
                "Format: XDBF title database ({} variant, magic {}, version {})\n",
                info.variant, info.magic, info.version
            ));
            match &info.title_name {
                Some(t) => out.push_str(&format!("Title: {t}\n")),
                None => out.push_str("Title: not found\n"),
            }
            let rows: Vec<(&str, u64, u64, bool)> = info
                .entries
                .iter()
                .map(|e| (e.resource.as_str(), e.offset, e.size, false))
                .collect();
            render_entries(&mut out, &rows, show_entries);
            if !info.records.is_empty() {
                out.push_str(&format!("\nDecoded records ({}):\n", info.records.len()));
                for r in &info.records {
                    match r {
                        crate::gpd::GpdRecord::Achievement(a) => {
                            out.push_str(&format!(
                                "  Achievement #{}: {} ({}G{}{}{})\n    {}\n",
                                a.achievement_id,
                                a.name,
                                a.gamerscore,
                                if a.earned { ", earned" } else { "" },
                                if a.secret { ", secret" } else { "" },
                                if a.online { ", online" } else { "" },
                                if a.unlocked_desc.is_empty() {
                                    a.locked_desc.clone()
                                } else {
                                    a.unlocked_desc.clone()
                                },
                            ));
                        }
                        crate::gpd::GpdRecord::Title(t) => {
                            out.push_str(&format!(
                                "  Title {}: {} ({}/{} achievements, {}/{}G{})\n",
                                t.title_id,
                                t.title_name,
                                t.unlocked,
                                t.achievements,
                                t.gamerscore_unlocked,
                                t.gamerscore_total,
                                t.last_played
                                    .as_ref()
                                    .map(|d| format!(", last played {d}"))
                                    .unwrap_or_default(),
                            ));
                        }
                        crate::gpd::GpdRecord::Setting(s) => {
                            out.push_str(&format!(
                                "  Setting {} [{}] [{}]: {}\n",
                                s.setting_name, s.setting_id, s.data_type, s.value
                            ));
                        }
                        crate::gpd::GpdRecord::SyncList(items) => {
                            out.push_str(&format!("  Sync list: {} items\n", items.len()));
                        }
                        crate::gpd::GpdRecord::SyncData { next, last, time } => {
                            out.push_str(&format!(
                                "  Sync data: next={next:016X} last={last:016X} {time}\n"
                            ));
                        }
                    }
                }
            }
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
