//! Checked PC WaW v17 animation reader.
//!
//! Format reference: OpenAssetTools' CompiledXAnimLoader, by Laupetin and
//! contributors (GPL-3.0), revision 06a121f7cf6658ea3989b3478b6275ce8c1c7b6d.
//! This Rust implementation keeps packed quaternion components for native-style
//! interpolation. It does not execute animation events or interpret blend trees.

use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fs::File, io::Read, path::Path};

pub const RAW_TYPE_1: u8 = 1;
pub const RAW_TYPE_2: u8 = 2;

const VERSION: u16 = 17;
const FLAG_LOOPED: u8 = 1;
const FLAG_DELTA: u8 = 2;
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_BONES: usize = 256;
const MAX_KEYS: usize = 2_000_000;
const MAX_STRING_BYTES: usize = 1024;

/// Parsing an asset type does not mean its playback semantics are supported.
/// Types 0 and 6, for example, remain readable for honest catalogue statistics.
pub fn supports_type(asset_type: u8) -> bool {
    matches!(asset_type, RAW_TYPE_1 | RAW_TYPE_2)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Clip {
    pub name: String,
    pub last_frame: usize,
    pub fps: f32,
    pub looped: bool,
    pub asset_type: u8,
    pub bones: Vec<Track>,
    pub delta: Option<Delta>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    pub name: String,
    pub rotation: Keys4,
    pub translation: Keys3,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Keys4 {
    pub indices: Vec<u16>,
    pub values: Vec<[i16; 4]>,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Keys3 {
    pub indices: Vec<u16>,
    pub values: Vec<[f32; 3]>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Delta {
    pub rotation: Option<Keys4>,
    pub translation: Option<Keys3>,
}

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
    keys: usize,
    frames: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
        let end = self
            .position
            .checked_add(count)
            .ok_or("Animation file offset overflow.")?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or("Incomplete animation data.")?;
        self.position = end;
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, String> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn i16(&mut self) -> Result<i16, String> {
        Ok(self.u16()? as i16)
    }

    fn float(&mut self) -> Result<f32, String> {
        let bytes = self.take(4)?;
        let value = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        if !value.is_finite() {
            return Err("Non-finite animation translation.".into());
        }
        Ok(value)
    }

    fn vector(&mut self) -> Result<[f32; 3], String> {
        Ok([self.float()?, self.float()?, self.float()?])
    }

    fn string(&mut self) -> Result<String, String> {
        let remaining = &self.bytes[self.position..];
        let count = remaining
            .iter()
            .take(MAX_STRING_BYTES + 1)
            .position(|byte| *byte == 0)
            .ok_or("Animation string is too long or unterminated.")?;
        let value = String::from_utf8(remaining[..count].to_vec())
            .map_err(|_| "Animation name is not valid UTF-8.".to_string())?;
        self.position += count + 1;
        Ok(value)
    }

    fn key_count(&mut self) -> Result<usize, String> {
        let count = usize::from(self.u16()?);
        if count > self.frames {
            return Err("Animation track has more keys than frames.".into());
        }
        self.keys = self
            .keys
            .checked_add(count)
            .ok_or("Animation key count overflow.")?;
        if self.keys > MAX_KEYS {
            return Err("Animation exceeds the supported key count.".into());
        }
        Ok(count)
    }

    fn indices(&mut self, count: usize) -> Result<Vec<u16>, String> {
        // Constants carry no time array. Full tracks omit it on disk; sparse
        // tracks use bytes through frame 255, and little-endian shorts above it.
        if count <= 1 {
            return Ok(Vec::new());
        }
        let mut output = Vec::with_capacity(count);
        if count == self.frames {
            output.extend(0..count as u16);
        } else {
            for _ in 0..count {
                let index = if self.frames <= 256 {
                    u16::from(self.u8()?)
                } else {
                    self.u16()?
                };
                output.push(index);
            }
        }
        if output.first().copied() != Some(0)
            || output.last().copied().map(usize::from) != Some(self.frames - 1)
            || output.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err("Animation key frames are unordered or incomplete.".into());
        }
        Ok(output)
    }

    fn packed_rotation(&mut self, half: bool) -> Result<[i16; 4], String> {
        let mut value = [0_i16; 4];
        if half {
            value[2] = self.i16()?;
        } else {
            value[0] = self.i16()?;
            value[1] = self.i16()?;
            value[2] = self.i16()?;
        }
        let square = 32767_i64.pow(2)
            - value[..3]
                .iter()
                .map(|component| i64::from(*component).pow(2))
                .sum::<i64>();
        // The original loader takes sqrt in Float32 and rounds packed W to the
        // nearest integer. Keeping that rounding matters for pose comparisons.
        if square > 0 {
            value[3] = ((square as f32).sqrt() + 0.5).floor() as i16;
        }
        Ok(value)
    }

    fn rotation(&mut self, half: bool, flip_first: bool) -> Result<Keys4, String> {
        let count = self.key_count()?;
        if count == 0 {
            return Ok(Keys4::default());
        }
        let indices = self.indices(count)?;
        let mut values: Vec<[i16; 4]> = Vec::with_capacity(count);
        for index in 0..count {
            let mut value = self.packed_rotation(half)?;
            let flip = if index == 0 {
                flip_first
            } else {
                dot(values[index - 1], value) < 0
            };
            if flip {
                for component in &mut value {
                    *component = component.wrapping_neg();
                }
            }
            if dot(value, value) == 0
                || values
                    .last()
                    .is_some_and(|previous| dot(*previous, value) < 0)
            {
                return Err("Animation contains an unsafe quaternion interval.".into());
            }
            values.push(value);
        }
        Ok(Keys4 { indices, values })
    }

    fn translation(&mut self) -> Result<Keys3, String> {
        let count = self.key_count()?;
        if count == 0 {
            return Ok(Keys3::default());
        }
        if count == 1 {
            return Ok(Keys3 {
                indices: Vec::new(),
                values: vec![self.vector()?],
            });
        }
        let indices = self.indices(count)?;
        let packed_size = self.u8()?;
        if packed_size > 1 {
            return Err("Invalid animation translation encoding.".into());
        }
        let small = packed_size == 1;
        let minimum = self.vector()?;
        // These are the linker's Float32 constants, not double-precision
        // division followed by a cast after decoding each coordinate.
        let scale = if small {
            0.003_921_569_f32
        } else {
            0.000_015_259_022_f32
        };
        let size = self.vector()?.map(|value| value * scale);
        let mut values = Vec::with_capacity(count);
        for _ in 0..count {
            let mut value = [0.0; 3];
            for axis in 0..3 {
                let packed = if small {
                    u16::from(self.u8()?)
                } else {
                    self.u16()?
                };
                value[axis] = minimum[axis] + f32::from(packed) * size[axis];
                if !value[axis].is_finite() {
                    return Err("Decoded animation translation is not finite.".into());
                }
            }
            values.push(value);
        }
        Ok(Keys3 { indices, values })
    }
}

fn dot(a: [i16; 4], b: [i16; 4]) -> i64 {
    a.iter()
        .zip(b)
        .map(|(left, right)| i64::from(*left) * i64::from(right))
        .sum()
}

/// Read and validate one loose animation. The caller separately decides whether
/// this clip's type and model association are supported by the basic player.
pub fn read(path: &Path) -> Result<Clip, String> {
    let file = File::open(path).map_err(|error| error.to_string())?;
    let size = file.metadata().map_err(|error| error.to_string())?.len();
    if size > MAX_FILE_BYTES {
        return Err("Animation exceeds the supported file size.".into());
    }
    let mut bytes = Vec::with_capacity(size as usize);
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err("Animation exceeds the supported file size.".into());
    }
    let mut reader = Reader {
        bytes: &bytes,
        position: 0,
        keys: 0,
        frames: 0,
    };
    if reader.u16()? != VERSION {
        return Err("Unsupported animation version; PC WaW v17 is required.".into());
    }
    let stored_frames = usize::from(reader.u16()?);
    let bone_count = usize::from(reader.u16()?);
    if bone_count > MAX_BONES {
        return Err("Animation exceeds the supported bone count.".into());
    }
    let flags = reader.u8()?;
    if flags & !(FLAG_LOOPED | FLAG_DELTA) != 0 {
        return Err("Unsupported flags in WaW v17 animation.".into());
    }
    let asset_type = reader.u8()?;
    let fps = f32::from(reader.u16()?);
    if fps <= 0.0 {
        return Err("Animation frame rate must be positive.".into());
    }
    let looped = flags & FLAG_LOOPED != 0;
    reader.frames = stored_frames + usize::from(looped);
    if reader.frames == 0 || reader.frames > usize::from(u16::MAX) {
        return Err("Animation has an unsupported frame count.".into());
    }
    let last_frame = reader.frames - 1;
    let delta = if flags & FLAG_DELTA != 0 {
        // Version 17 stores planar root rotation (Z/W), independently of the
        // bone tracks. It can also contain an empty delta object.
        let rotation = reader.rotation(true, false)?;
        let translation = reader.translation()?;
        Some(Delta {
            rotation: (!rotation.values.is_empty()).then_some(rotation),
            translation: (!translation.values.is_empty()).then_some(translation),
        })
    } else {
        None
    };
    let bit_count = bone_count.div_ceil(8);
    let flip = reader.take(bit_count)?.to_vec();
    let half = reader.take(bit_count)?.to_vec();
    let mut names = Vec::with_capacity(bone_count);
    let mut distinct = HashSet::new();
    for _ in 0..bone_count {
        let name = reader.string()?;
        if name.is_empty() || !distinct.insert(name.to_ascii_lowercase()) {
            return Err("Animation has an empty or duplicate bone name.".into());
        }
        names.push(name);
    }
    let mut ordered = Vec::with_capacity(bone_count);
    for (index, name) in names.into_iter().enumerate() {
        let mask = 1 << (index % 8);
        let is_half = half[index / 8] & mask != 0;
        let rotation = reader.rotation(is_half, flip[index / 8] & mask != 0)?;
        if rotation.values.is_empty() && !is_half {
            return Err("Invalid empty full-quaternion track.".into());
        }
        let category = match (rotation.values.len(), is_half) {
            (0, _) => 0,
            (1, true) => 3,
            (1, false) => 4,
            (_, true) => 1,
            (_, false) => 2,
        };
        let translation = reader.translation()?;
        ordered.push((
            category,
            Track {
                name,
                rotation,
                translation,
            },
        ));
    }
    // The engine's loaded layout groups rotation encodings. A stable sort
    // matches the reference reader; model binding still uses names, not order.
    ordered.sort_by_key(|(category, _)| *category);
    let bones = ordered.into_iter().map(|(_, track)| track).collect();
    // Consume and validate notes for EOF checking. The basic viewer deliberately
    // does not execute the game's sound/FX/script events.
    let note_count = reader.u8()?;
    for _ in 0..note_count {
        reader.string()?;
        if usize::from(reader.u16()?) > last_frame {
            return Err("Animation note lies outside its frame range.".into());
        }
    }
    if reader.position != bytes.len() {
        return Err("Unexpected trailing animation data.".into());
    }
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or("Animation filename is not valid UTF-8.")?
        .to_string();
    Ok(Clip {
        name,
        last_frame,
        fps,
        looped,
        asset_type,
        bones,
        delta,
    })
}
