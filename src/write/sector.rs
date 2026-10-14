//! Sector allocation helpers for writing XISO output.

#![allow(dead_code)]

/// Size of a single sector on an Xbox 360 disc image (bytes).
pub const SECTOR_SIZE: u64 = 2048;

/// Number of 2048-byte sectors required to store `len` bytes.
/// An empty payload still occupies one sector.
pub fn required_sectors(len: u64) -> u32 {
    if len == 0 {
        1
    } else {
        len.div_ceil(SECTOR_SIZE) as u32
    }
}

/// Monotonically advancing allocator handing out contiguous sector ranges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectorAllocator {
    pub next_free: u32,
}

impl Default for SectorAllocator {
    fn default() -> Self {
        Self { next_free: 33 }
    }
}

impl SectorAllocator {
    /// Create an allocator whose first free sector is `start_sector`.
    pub fn new(start_sector: u32) -> Self {
        Self {
            next_free: start_sector,
        }
    }

    /// Reserve `bytes` worth of contiguous sectors, returning the starting sector.
    pub fn allocate_contiguous(&mut self, bytes: u64) -> u32 {
        let start = self.next_free;
        self.next_free += required_sectors(bytes);
        start
    }

    /// Total number of sectors consumed so far (equals next free sector).
    pub fn total_sectors(&self) -> u32 {
        self.next_free
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_required_sectors() {
        assert_eq!(required_sectors(0), 1);
        assert_eq!(required_sectors(1), 1);
        assert_eq!(required_sectors(2048), 1);
        assert_eq!(required_sectors(2049), 2);
        assert_eq!(required_sectors(4096), 2);
        assert_eq!(required_sectors(4097), 3);
    }

    #[test]
    fn test_sector_allocator() {
        let mut alloc = SectorAllocator::default();
        assert_eq!(alloc.next_free, 33);
        assert_eq!(alloc.allocate_contiguous(10), 33);
        assert_eq!(alloc.allocate_contiguous(2048), 34);
        assert_eq!(alloc.allocate_contiguous(4096), 35);
    }
}
