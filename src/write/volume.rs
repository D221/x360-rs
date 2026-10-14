//! XISO volume descriptor sector.

#![allow(dead_code)]

use byteorder::{LittleEndian, WriteBytesExt};
use std::io::{Cursor, Write};

/// Magic bytes identifying an XISO volume descriptor.
pub const VOLUME_MAGIC: &[u8; 20] = b"MICROSOFT*XBOX*MEDIA";

/// Top-level volume descriptor written at the start of an XISO image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeDescriptor {
    pub root_table_sector: u32,
    pub root_table_size: u32,
    pub creation_time: u64,
}

impl VolumeDescriptor {
    /// Create a new volume descriptor.
    pub fn new(root_sector: u32, root_size: u32, creation_time: u64) -> Self {
        Self {
            root_table_sector: root_sector,
            root_table_size: root_size,
            creation_time,
        }
    }

    /// Serialize the descriptor into a 2048-byte sector.
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = vec![0u8; 2048];
        let mut cursor = Cursor::new(&mut buf[..]);
        cursor.write_all(VOLUME_MAGIC).unwrap();
        cursor
            .write_u32::<LittleEndian>(self.root_table_sector)
            .unwrap();
        cursor
            .write_u32::<LittleEndian>(self.root_table_size)
            .unwrap();
        cursor
            .write_u64::<LittleEndian>(self.creation_time)
            .unwrap();
        cursor.set_position(44);
        cursor.write_all(VOLUME_MAGIC).unwrap();
        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_volume_descriptor_magic() {
        let vd = VolumeDescriptor::new(100, 200, 12345);
        let bytes = vd.serialize();
        assert_eq!(&bytes[0..20], b"MICROSOFT*XBOX*MEDIA");
        assert_eq!(bytes.len(), 2048);
    }

    #[test]
    fn test_volume_descriptor_root_table() {
        let root_sector = 0x1234u32;
        let root_size = 0x5678u32;
        let vd = VolumeDescriptor::new(root_sector, root_size, 999);
        let bytes = vd.serialize();
        let sector = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
        let size = u32::from_le_bytes(bytes[24..28].try_into().unwrap());
        assert_eq!(sector, root_sector);
        assert_eq!(size, root_size);
    }

    #[test]
    fn test_volume_descriptor_tail_magic() {
        let vd = VolumeDescriptor::new(1, 2, 3);
        let bytes = vd.serialize();
        assert_eq!(&bytes[44..64], b"MICROSOFT*XBOX*MEDIA");
    }
}
