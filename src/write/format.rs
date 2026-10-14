//! Output format (XSF / XGD2 / XGD3 / XGD1) and its sector/header offsets.

#![allow(dead_code)]

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

pub fn header_offset(fmt: OutputFormat) -> u64 {
    sector_offset(fmt) + 0x10000
}

pub fn is_xgd(fmt: OutputFormat) -> bool {
    !matches!(fmt, OutputFormat::Xsf)
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
    fn test_header_offsets() {
        assert_eq!(header_offset(OutputFormat::Xsf), 0x10000);
        assert_eq!(header_offset(OutputFormat::Xgd2), 0x10000 + 0x0FD90000);
        assert_eq!(header_offset(OutputFormat::Xgd3), 0x10000 + 0x02080000);
        assert_eq!(header_offset(OutputFormat::Xgd1), 0x10000 + 0x18300000);
    }

    #[test]
    fn test_is_xgd() {
        assert!(!is_xgd(OutputFormat::Xsf));
        assert!(is_xgd(OutputFormat::Xgd2));
        assert!(is_xgd(OutputFormat::Xgd3));
        assert!(is_xgd(OutputFormat::Xgd1));
    }
}
