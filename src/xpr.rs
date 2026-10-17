//! XPR0 texture unpacking (dashboard / GPD images).
//!
//! Reference: `../FtpContentManager/Src/Readers/Xpr/XprPackage.cs`
//! (magic `XPR0`, LE header, square textures sized `2^res2`, DXT1 and
//! ARGB pixel data after the header block; format enum in
//! `Src/Constants/SystemAndHardware.cs`).

use anyhow::{bail, Context, Result};

/// Texture format byte at 0x19 (`XprFormat` enum).
const FORMAT_DXT1: u8 = 0x0C;
const FORMAT_ARGB: u8 = 0x14;

struct XprImage<'a> {
    data: &'a [u8],
    format: u8,
    /// Width == height (square), 2^res2.
    size: u32,
}

fn parse_header(blob: &[u8]) -> Result<XprImage<'_>> {
    if blob.len() < 0x1C || &blob[0..4] != b"XPR0" {
        bail!("not an XPR0 image");
    }
    let rd =
        |off: usize| u32::from_le_bytes(blob[off..off + 4].try_into().expect("bounds checked"));
    let file_size = rd(0x04) as usize;
    let header_size = rd(0x08) as usize;
    let res2 = blob[0x1B];
    if file_size > blob.len() || file_size < header_size || header_size < 0x1C {
        bail!("malformed XPR0 header (file_size {file_size:#x}, header_size {header_size:#x})");
    }
    if res2 > 20 {
        bail!("unsupported XPR0 size 2^{res2}");
    }
    Ok(XprImage {
        data: &blob[header_size..file_size],
        format: blob[0x19],
        size: 1u32 << res2,
    })
}

/// Decode an XPR0 texture blob to PNG bytes (RGBA8).
pub fn decode_xpr_to_png(blob: &[u8]) -> Result<Vec<u8>> {
    let img = parse_header(blob)?;
    let n = (img.size * img.size * 4) as usize;
    let rgba = match img.format {
        FORMAT_DXT1 => decode_dxt1(img.data, img.size, n)?,
        FORMAT_ARGB => decode_argb(img.data, n)?,
        f => bail!("unsupported XPR0 texture format 0x{f:02X}"),
    };
    encode_png(&rgba, img.size)
}

/// Exact port of `XprPackage.cs` 565 expansion: `(n/denom + n)/denom`
/// with `n = c*255 + offset`, which rounds to nearest.
fn expand5(c5: u32) -> u8 {
    let n = c5 * 255 + 16;
    ((n / 32 + n) / 32) as u8
}

fn expand6(c6: u32) -> u8 {
    let n = c6 * 255 + 32;
    ((n / 64 + n) / 64) as u8
}

fn decode_dxt1(data: &[u8], size: u32, n: usize) -> Result<Vec<u8>> {
    let mut px = vec![0u8; n];
    let blocks_x = (size as usize).div_ceil(4);
    let blocks_y = (size as usize).div_ceil(4);
    let mut src = 0usize;
    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            if src + 8 > data.len() {
                bail!("truncated DXT1 data");
            }
            let c0 = u16::from_le_bytes([data[src], data[src + 1]]);
            let c1 = u16::from_le_bytes([data[src + 2], data[src + 3]]);
            let bits = u32::from_le_bytes(data[src + 4..src + 8].try_into().unwrap());
            src += 8;
            let (r0, g0, b0) = (
                u32::from(expand5(u32::from(c0 >> 11))),
                u32::from(expand6(u32::from((c0 >> 5) & 0x3F))),
                u32::from(expand5(u32::from(c0 & 0x1F))),
            );
            let (r1, g1, b1) = (
                u32::from(expand5(u32::from(c1 >> 11))),
                u32::from(expand6(u32::from((c1 >> 5) & 0x3F))),
                u32::from(expand5(u32::from(c1 & 0x1F))),
            );
            for i in 0..4usize {
                for j in 0..4usize {
                    let idx = (bits >> (2 * (4 * i + j))) & 3;
                    let color: [u8; 4] = if c0 > c1 {
                        match idx {
                            0 => [r0, g0, b0, 255],
                            1 => [r1, g1, b1, 255],
                            2 => [(2 * r0 + r1) / 3, (2 * g0 + g1) / 3, (2 * b0 + b1) / 3, 255],
                            _ => [(r0 + 2 * r1) / 3, (g0 + 2 * g1) / 3, (b0 + 2 * b1) / 3, 255],
                        }
                    } else {
                        match idx {
                            0 => [r0, g0, b0, 255],
                            1 => [r1, g1, b1, 255],
                            2 => [(r0 + r1) / 2, (g0 + g1) / 2, (b0 + b1) / 2, 255],
                            _ => [0, 0, 0, 255],
                        }
                    }
                    .map(|v| v as u8);
                    let x = bx * 4 + j;
                    let y = by * 4 + i;
                    if x < size as usize && y < size as usize {
                        let off = (y * size as usize + x) * 4;
                        px[off..off + 4].copy_from_slice(&color);
                    }
                }
            }
        }
    }
    Ok(px)
}

fn decode_argb(data: &[u8], n: usize) -> Result<Vec<u8>> {
    if data.len() < n {
        bail!("truncated ARGB data");
    }
    // Reference reads B,G,R,A and emits RGBA.
    let mut px = vec![0u8; n];
    for i in 0..n / 4 {
        px[i * 4] = data[i * 4 + 2];
        px[i * 4 + 1] = data[i * 4 + 1];
        px[i * 4 + 2] = data[i * 4];
        px[i * 4 + 3] = data[i * 4 + 3];
    }
    Ok(px)
}

fn encode_png(rgba: &[u8], size: u32) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, size, size);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().context("png header")?;
        writer.write_image_data(rgba).context("png data")?;
    }
    Ok(out)
}
