//! Output format (XSF / XGD2 / XGD3 / XGD1) and its sector/header offsets.

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    Xsf,
    Xgd2,
    Xgd3,
    Xgd1,
}

pub fn sector_offset(fmt: OutputFormat) -> u64 {
    match fmt {
        OutputFormat::Xsf => 0,
        OutputFormat::Xgd2 => 0x0FD90000,
        OutputFormat::Xgd3 => 0x02080000,
        OutputFormat::Xgd1 => 0x18300000,
    }
}

/// Base file offset of the volume area for a parsed ISO type name.
/// Mirrors `sector_offset` but takes the `iso_type` string reported by
/// the parser (`"XSF"`, `"XGD2"`, `"XGD3"`, `"XGD1"`).
pub fn iso_type_offset(iso_type: &str) -> u64 {
    match iso_type {
        "XGD2" => 0x0FD90000,
        "XGD3" => 0x02080000,
        "XGD1" => 0x18300000,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sector_offsets() {
        assert_eq!(sector_offset(OutputFormat::Xsf), 0);
        assert_eq!(sector_offset(OutputFormat::Xgd2), 0x0FD90000);
        assert_eq!(sector_offset(OutputFormat::Xgd3), 0x02080000);
        assert_eq!(sector_offset(OutputFormat::Xgd1), 0x18300000);
    }

    #[test]
    fn test_iso_type_offsets_mirror_sector_offsets() {
        assert_eq!(iso_type_offset("XSF"), sector_offset(OutputFormat::Xsf));
        assert_eq!(iso_type_offset("XGD2"), sector_offset(OutputFormat::Xgd2));
        assert_eq!(iso_type_offset("XGD3"), sector_offset(OutputFormat::Xgd3));
        assert_eq!(iso_type_offset("XGD1"), sector_offset(OutputFormat::Xgd1));
        assert_eq!(iso_type_offset("unknown"), 0);
    }
}
