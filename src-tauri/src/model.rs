//! Loose WaW v25 geometry and bind-pose bones. No game process or export step.
use crate::reader::Reader;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Lod {
    pub slot: usize,
    pub name: String,
    pub distance: f32,
    pub materials: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Descriptor {
    pub lods: Vec<Lod>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Surface {
    pub positions: Vec<f32>,
    pub normals: Vec<f32>,
    pub uvs: Vec<f32>,
    pub colours: Vec<u8>,
    pub indices: Vec<u16>,
    pub max_bone: u16,
}

impl Surface {
    /// A successfully parsed stream can still be an empty animation carrier.
    /// Check actual indexed faces, rather than the number of stored vertices.
    pub fn has_visible_triangle(&self) -> bool {
        self.indices.chunks_exact(3).any(|triangle| {
            let coloured = triangle.iter().any(|index| {
                let offset = usize::from(*index) * 4;
                self.colours[offset..offset + 3]
                    .iter()
                    .any(|channel| *channel != 0)
            });
            if !coloured {
                return false;
            }
            let points: Vec<[f64; 3]> = triangle
                .iter()
                .map(|index| {
                    let offset = usize::from(*index) * 3;
                    std::array::from_fn(|axis| f64::from(self.positions[offset + axis]))
                })
                .collect();
            let a: [f64; 3] = std::array::from_fn(|axis| points[1][axis] - points[0][axis]);
            let b: [f64; 3] = std::array::from_fn(|axis| points[2][axis] - points[0][axis]);
            let cross = [
                a[1] * b[2] - a[2] * b[1],
                a[2] * b[0] - a[0] * b[2],
                a[0] * b[1] - a[1] * b[0],
            ];
            cross.iter().any(|component| *component != 0.0)
        })
    }
}

#[derive(Clone, Serialize)]
pub struct Bone {
    pub name: String,
    pub parent: Option<usize>,
    pub position: [f64; 3],
}

pub fn asset_path(root: &Path, folder: &str, name: &str) -> Result<PathBuf, String> {
    if name.is_empty()
        || Path::new(name)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("Invalid asset path.".into());
    }
    Ok(root.join("raw").join(folder).join(name))
}

fn data(path: &Path) -> Result<Vec<u8>, String> {
    let size = fs::metadata(path).map_err(|error| error.to_string())?.len();
    if size > 128 * 1024 * 1024 {
        return Err("Asset exceeds the supported file size.".into());
    }
    fs::read(path).map_err(|error| error.to_string())
}

fn version(reader: &mut Reader<'_>) -> Result<(), String> {
    let value = reader.u16()?;
    if value != 25 {
        return Err(format!(
            "Unsupported model version {value}; expected PC WaW v25."
        ));
    }
    Ok(())
}

pub fn descriptor(path: &Path) -> Result<Descriptor, String> {
    let bytes = data(path)?;
    let mut reader = Reader::new(&bytes);
    version(&mut reader)?;
    reader.skip(1 + 24)?;
    reader.string()?;
    reader.string()?;
    let mut lods = Vec::new();
    for slot in 0..4 {
        let distance = reader.f32()?;
        let name = reader.string()?;
        if !name.is_empty() {
            lods.push(Lod {
                slot,
                name,
                distance,
                materials: Vec::new(),
            });
        }
    }
    reader.skip(4)?;
    let collision_groups = reader.u32()?;
    if collision_groups > 65536 {
        return Err("Invalid collision group count.".into());
    }
    for _ in 0..collision_groups {
        let count = reader.u32()? as usize;
        let size = count
            .checked_mul(48)
            .and_then(|value| value.checked_add(36))
            .ok_or("Collision size overflow.")?;
        reader.skip(size)?;
    }
    for lod in &mut lods {
        for _ in 0..reader.u16()? {
            lod.materials.push(reader.string()?);
        }
    }
    // The descriptor continues with data outside the geometry/material table.
    // Reading it to EOF would reject valid stock models; the viewer needs only
    // the LOD references and material names above.
    Ok(Descriptor { lods })
}

pub fn surfaces(path: &Path) -> Result<Vec<Surface>, String> {
    let bytes = data(path)?;
    let mut reader = Reader::new(&bytes);
    version(&mut reader)?;
    let count = reader.u16()?;
    if count > 4096 {
        return Err("Invalid surface count.".into());
    }
    let mut output = Vec::new();
    for _ in 0..count {
        reader.skip(3)?;
        let vertices = reader.u16()? as usize;
        let triangles = reader.u16()? as usize;
        let mut group_size = reader.u16()?;
        let mut groups = Vec::new();
        let mut grouped_vertices = 0;
        while group_size != 0 {
            grouped_vertices += usize::from(group_size);
            if grouped_vertices > vertices || groups.len() > 255 {
                return Err("Invalid rigid vertex groups.".into());
            }
            groups.push((group_size, reader.u16()?));
            group_size = reader.u16()?;
        }
        let rigid = groups.len() == 1 && grouped_vertices == vertices;
        if !rigid {
            reader.skip(2)?;
        }
        let default_bone = groups.first().map(|group| group.1).unwrap_or(0);
        let mut surface = Surface {
            positions: Vec::with_capacity(vertices * 3),
            normals: Vec::with_capacity(vertices * 3),
            uvs: Vec::with_capacity(vertices * 2),
            colours: Vec::with_capacity(vertices * 4),
            indices: Vec::with_capacity(triangles * 3),
            max_bone: 0,
        };
        for _ in 0..vertices {
            surface.normals.extend(reader.vector()?);
            let colour = reader.bytes(4)?;
            // D3DCOLOR is an ARGB word, stored as BGRA bytes on this PC format.
            surface
                .colours
                .extend([colour[2], colour[1], colour[0], colour[3]]);
            surface.uvs.extend([reader.f32()?, reader.f32()?]);
            // Tangents are not needed by this colour-only viewer. Four stock
            // vertices contain invalid tangents; their useful attributes are valid.
            reader.skip(24)?;
            let extra = if rigid { 0 } else { reader.u8()? };
            if extra > 3 {
                return Err("Unsupported vertex influence count.".into());
            }
            let primary = if rigid { default_bone } else { reader.u16()? };
            surface.max_bone = surface.max_bone.max(primary);
            surface.positions.extend(reader.vector()?);
            let mut weight_sum = 0_u32;
            for _ in 0..extra {
                surface.max_bone = surface.max_bone.max(reader.u16()?);
                weight_sum += u32::from(reader.u16()?);
            }
            if weight_sum > 65535 {
                return Err("Invalid vertex weights.".into());
            }
        }
        for _ in 0..triangles {
            let a = reader.u16()?;
            let b = reader.u16()?;
            let c = reader.u16()?;
            if [a, b, c]
                .iter()
                .any(|index| usize::from(*index) >= vertices)
            {
                return Err("Triangle index outside the vertex array.".into());
            }
            // One consistent native clockwise -> WebGL counterclockwise conversion.
            surface.indices.extend([a, c, b]);
        }
        output.push(surface);
    }
    reader.finish()?;
    Ok(output)
}

pub fn bone_count(path: &Path) -> Result<usize, String> {
    let bytes = data(path)?;
    let mut reader = Reader::new(&bytes);
    version(&mut reader)?;
    Ok(usize::from(reader.u16()?) + usize::from(reader.u16()?))
}

fn multiply(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    let [x, y, z, w] = a;
    let [xx, yy, zz, ww] = b;
    [
        w * xx + x * ww + y * zz - z * yy,
        w * yy - x * zz + y * ww + z * xx,
        w * zz + x * yy - y * xx + z * ww,
        w * ww - x * xx - y * yy - z * zz,
    ]
}

fn normalise(mut quaternion: [f64; 4]) -> [f64; 4] {
    let length = quaternion
        .iter()
        .map(|value| value * value)
        .sum::<f64>()
        .sqrt();
    for value in &mut quaternion {
        *value /= length;
    }
    quaternion
}

fn rotate(quaternion: [f64; 4], vector: [f64; 3]) -> [f64; 3] {
    let q = normalise(quaternion);
    let product = multiply(
        multiply(q, [vector[0], vector[1], vector[2], 0.0]),
        [-q[0], -q[1], -q[2], q[3]],
    );
    [product[0], product[1], product[2]]
}

pub fn bones(path: &Path) -> Result<Vec<Bone>, String> {
    let bytes = data(path)?;
    let mut reader = Reader::new(&bytes);
    version(&mut reader)?;
    let children = usize::from(reader.u16()?);
    let roots = usize::from(reader.u16()?);
    if children + roots > 256 {
        return Err("Invalid skeleton size.".into());
    }
    let mut parents = vec![None; roots];
    let mut positions = vec![[0.0; 3]; roots];
    let mut rotations = vec![[0.0, 0.0, 0.0, 1.0]; roots];
    for _ in 0..children {
        let parent = usize::from(reader.u8()?);
        if parent >= parents.len() {
            return Err("Bone parent does not precede its child.".into());
        }
        let position = reader.vector()?.map(f64::from);
        let xyz = [reader.i16()?, reader.i16()?, reader.i16()?].map(i64::from);
        let squared = 32767_i64.pow(2) - xyz.iter().map(|value| value * value).sum::<i64>();
        // Match native packed-W rounding before normalising the quaternion.
        let w = if squared > 0 {
            ((squared as f64).sqrt() as f32 + 0.5).floor() as f64
        } else {
            0.0
        };
        let local = normalise([xyz[0] as f64, xyz[1] as f64, xyz[2] as f64, w]);
        let offset = rotate(rotations[parent], position);
        positions.push(std::array::from_fn(|axis| {
            positions[parent][axis] + offset[axis]
        }));
        rotations.push(multiply(rotations[parent], local));
        parents.push(Some(parent));
    }
    let mut output = Vec::new();
    for index in 0..parents.len() {
        output.push(Bone {
            name: reader.string()?,
            parent: parents[index],
            position: positions[index],
        });
    }
    // Stock parts have additional metadata after their bone names. Bind-pose
    // inspection is complete here, matching the native reference traversal.
    Ok(output)
}
