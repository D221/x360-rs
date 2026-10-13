//! Region-mask decoding.

// Region bitmask mapping, confirmed via REA/Ghidra against
// Title Update Checker_unpacked.exe (evidence ev_15f0a71c..., ev_a5b10f23...,
// read_bytes at 0x4032e0-0x4036c0):
//   0x4032FE: AND ECX,0x00010000 / CMP -> "PAL(A) "
//   0x403383: AND ECX,0x00FE0000 / CMP -> "PAL(E) "
//   0x403408: AND ECX,0x0000FF00 / CMP -> "NTSC/J "
//   0x403492: AND ECX,0x00000100 / CMP -> "NTSC/J(J) "
//   0x403517: AND ECX,0x00000200 / CMP -> "NTSC/J(C) "
//   0x40359C: AND ECX,0x0000FC00 / CMP -> "NTSC/J(0) "
//   0x403621: AND ECX,0x000000FF / CMP -> "NTSC/U "
//   0x4036A6: AND ECX,0xFFFFFFFF / CMP -> "Region Free" (imm8 0xFF sign-extended)
pub(crate) const REGION_CHECKS: &[(u32, &str)] = &[
    (0x00010000, "PAL(A)"),
    (0x00FE0000, "PAL(E)"),
    (0x0000FF00, "NTSC/J"),
    (0x00000100, "NTSC/J(J)"),
    (0x00000200, "NTSC/J(C)"),
    (0x0000FC00, "NTSC/J(0)"),
    (0x000000FF, "NTSC/U"),
];

/// Decode a region mask the same way the checker does:
/// every (mask & check) == check appends its label;
/// a full 0xFFFFFFFF mask yields just "Region Free".
pub fn decode_regions(mask: u32) -> Vec<String> {
    if mask == 0xFFFFFFFF {
        return vec!["Region Free".to_string()];
    }
    let mut out = Vec::new();
    for (bits, name) in REGION_CHECKS {
        if (mask & bits) == *bits {
            out.push(name.to_string());
        }
    }
    out
}

/// Collapse detailed PAL sub-regions to "PAL" to match Title_Update_List.txt style.
pub fn collapse_regions(detailed: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut pal = false;
    for r in detailed {
        if r == "PAL(A)" || r == "PAL(E)" {
            if !pal {
                out.push("PAL".to_string());
                pal = true;
            }
        } else {
            out.push(r.clone());
        }
    }
    out
}
