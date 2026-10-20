//! PC World at War IWI v6 decoding. All returned pixels use unmodified RGBA bytes.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Header {
    pub width: u32,
    pub height: u32,
    pub format: u8,
    pub faces: usize,
    pub levels: usize,
    pub alpha: bool,
}

#[derive(Clone)]
pub struct Level {
    pub width: u32,
    pub height: u32,
    pub faces: Vec<Vec<u8>>,
}

pub fn header(data: &[u8]) -> Result<Header, String> {
    if data.len() < 28 || &data[..4] != b"IWi\x06" {
        return Err("Not a PC WaW IWI v6 image.".into());
    }
    let width = u16::from_le_bytes([data[6], data[7]]) as u32;
    let height = u16::from_le_bytes([data[8], data[9]]) as u32;
    let depth = u16::from_le_bytes([data[10], data[11]]);
    if width == 0
        || height == 0
        || width > 8192
        || height > 8192
        || width as u64 * height as u64 > 16_777_216
    {
        return Err("Invalid or excessively large image dimensions.".into());
    }
    if data[5] & 8 != 0 || depth > 1 {
        return Err("Volume textures are not supported.".into());
    }
    let format = data[4];
    if !(1..=13).contains(&format) {
        return Err(format!("Unsupported IWI format {format}."));
    }
    let levels = if data[5] & 2 != 0 {
        1
    } else {
        (32 - width.max(height).leading_zeros()) as usize
    };
    Ok(Header {
        width,
        height,
        format,
        levels,
        faces: if data[5] & 4 != 0 { 6 } else { 1 },
        alpha: matches!(format, 1 | 3 | 5 | 6 | 8 | 10 | 11 | 12 | 13),
    })
}

fn colour(value: u16) -> [u8; 4] {
    let r = ((value >> 11) & 31) as u8;
    let g = ((value >> 5) & 63) as u8;
    let b = (value & 31) as u8;
    [
        (r << 3) | (r >> 2),
        (g << 2) | (g >> 4),
        (b << 3) | (b >> 2),
        255,
    ]
}

fn decode_blocks(data: &[u8], width: usize, height: usize, format: u8) -> Vec<u8> {
    let mut pixels = vec![0; width * height * 4];
    let block_size = if format == 11 { 8 } else { 16 };
    let blocks_wide = width.div_ceil(4);
    for (block_index, block) in data.chunks_exact(block_size).enumerate() {
        let colour_start = block_size - 8;
        let a = u16::from_le_bytes([block[colour_start], block[colour_start + 1]]);
        let b = u16::from_le_bytes([block[colour_start + 2], block[colour_start + 3]]);
        let mut palette = [colour(a), colour(b), [0, 0, 0, 255], [0, 0, 0, 255]];
        for channel in 0..3 {
            let first = u16::from(palette[0][channel]);
            let second = u16::from(palette[1][channel]);
            if a > b || format != 11 {
                palette[2][channel] = ((2 * first + second) / 3) as u8;
                palette[3][channel] = ((first + 2 * second) / 3) as u8;
            } else {
                palette[2][channel] = ((first + second) / 2) as u8;
                palette[3] = [0, 0, 0, 0];
            }
        }
        let selectors = u32::from_le_bytes(
            block[colour_start + 4..colour_start + 8]
                .try_into()
                .unwrap(),
        );
        let mut alpha_palette = [0u8; 8];
        let mut alpha_selectors = 0u64;
        if format == 13 {
            alpha_palette[0] = block[0];
            alpha_palette[1] = block[1];
            if block[0] > block[1] {
                for i in 1..=6 {
                    alpha_palette[i + 1] =
                        (((7 - i) * block[0] as usize + i * block[1] as usize) / 7) as u8;
                }
            } else {
                for i in 1..=4 {
                    alpha_palette[i + 1] =
                        (((5 - i) * block[0] as usize + i * block[1] as usize) / 5) as u8;
                }
                alpha_palette[6] = 0;
                alpha_palette[7] = 255;
            }
            for (i, byte) in block[2..8].iter().enumerate() {
                alpha_selectors |= (*byte as u64) << (8 * i);
            }
        }
        for pixel in 0..16 {
            let x = (block_index % blocks_wide) * 4 + pixel % 4;
            let y = (block_index / blocks_wide) * 4 + pixel / 4;
            if x >= width || y >= height {
                continue;
            }
            let mut value = palette[((selectors >> (2 * pixel)) & 3) as usize];
            if format == 12 {
                value[3] = ((block[pixel / 2] >> ((pixel % 2) * 4)) & 15) * 17;
            } else if format == 13 {
                value[3] = alpha_palette[((alpha_selectors >> (3 * pixel)) & 7) as usize];
            }
            let offset = (y * width + x) * 4;
            pixels[offset..offset + 4].copy_from_slice(&value);
        }
    }
    pixels
}

fn bitmap(data: &[u8], format: u8) -> Vec<u8> {
    let channels = match format {
        1 | 6 => 4,
        2 | 7 => 3,
        3 | 8 => 2,
        _ => 1,
    };
    let mut output = Vec::with_capacity(data.len() / channels * 4);
    for pixel in data.chunks_exact(channels) {
        let rgba = match format {
            1 | 6 => [pixel[2], pixel[1], pixel[0], pixel[3]],
            2 | 7 => [pixel[2], pixel[1], pixel[0], 255],
            3 | 8 => [pixel[0], pixel[0], pixel[0], pixel[1]],
            5 | 10 => [255, 255, 255, pixel[0]],
            _ => [pixel[0], pixel[0], pixel[0], 255],
        };
        output.extend_from_slice(&rgba);
    }
    output
}

pub fn decode(data: &[u8]) -> Result<Vec<Level>, String> {
    let info = header(data)?;
    let mut output: Vec<Level> = (0..info.levels)
        .map(|mip| Level {
            width: (info.width >> mip).max(1),
            height: (info.height >> mip).max(1),
            faces: Vec::new(),
        })
        .collect();
    if (6..=10).contains(&info.format) {
        let channels = match info.format {
            6 => 4,
            7 => 3,
            8 => 2,
            _ => 1,
        };
        let wavelets = crate::wavelet::decode(
            &data[28..],
            info.width as usize,
            info.height as usize,
            info.levels,
            info.faces,
            channels,
        )?;
        for (mip, faces) in wavelets.iter().enumerate() {
            output[mip].faces = faces.iter().map(|face| bitmap(face, info.format)).collect();
        }
        return Ok(output);
    }
    let mut offset: usize = 28;
    for level in output.iter_mut().rev() {
        let w = level.width as usize;
        let h = level.height as usize;
        let size = match info.format {
            11 => w.div_ceil(4) * h.div_ceil(4) * 8,
            12 | 13 => w.div_ceil(4) * h.div_ceil(4) * 16,
            1 => w * h * 4,
            2 => w * h * 3,
            3 => w * h * 2,
            _ => w * h,
        };
        for _ in 0..info.faces {
            let bytes = data
                .get(offset..offset + size)
                .ok_or("Incomplete IWI mip data.")?;
            let pixels = if info.format >= 11 {
                decode_blocks(bytes, w, h, info.format)
            } else {
                bitmap(bytes, info.format)
            };
            level.faces.push(pixels);
            offset += size;
        }
    }
    if offset != data.len() {
        return Err("Unexpected trailing IWI data.".into());
    }
    Ok(output)
}
