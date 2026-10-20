//! A read-only model catalogue and disposable preview cache.
use crate::{
    material::{self, Material},
    model::{self, Bone, Surface},
    textures::{digest, Textures},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub group: String,
    pub lods: Vec<usize>,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Statistics {
    pub total: usize,
    pub missing_mesh: usize,
    pub missing_files: usize,
    pub empty_geometry: usize,
    pub other: usize,
    pub failed_previews: usize,
}

impl Statistics {
    fn eligible(&self) -> usize {
        self.total.saturating_sub(
            self.missing_mesh + self.missing_files + self.empty_geometry + self.other,
        )
    }

    fn record(&mut self, exclusion: &Exclusion) {
        match exclusion {
            Exclusion::MissingMesh(_) => self.missing_mesh += 1,
            Exclusion::MissingFiles(_) => self.missing_files += 1,
            Exclusion::EmptyGeometry(_) => self.empty_geometry += 1,
            Exclusion::Other(_) => self.other += 1,
        }
    }
}

#[derive(Clone)]
enum Exclusion {
    MissingMesh(String),
    MissingFiles(String),
    EmptyGeometry(String),
    Other(String),
}

impl Exclusion {
    fn message(&self) -> &str {
        match self {
            Self::MissingMesh(message)
            | Self::MissingFiles(message)
            | Self::EmptyGeometry(message)
            | Self::Other(message) => message,
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Index {
    pub entries: Vec<Entry>,
    pub warnings: Vec<String>,
    pub cache_path: String,
    pub statistics: Statistics,
}

#[derive(Serialize)]
pub struct ModelSurface {
    #[serde(flatten)]
    pub geometry: Surface,
    pub material: Material,
}

#[derive(Serialize)]
pub struct Model {
    pub name: String,
    pub lod: usize,
    pub lods: Vec<usize>,
    pub surfaces: Vec<ModelSurface>,
    pub bones: Vec<Bone>,
    pub warnings: Vec<String>,
}

pub struct Catalog {
    pub root: PathBuf,
    pub cache: PathBuf,
    pub index: Index,
    textures: Option<Textures>,
}

fn group(name: &str) -> &str {
    if name.starts_with("char_") || name.starts_with("viewhands_") {
        "Characters"
    } else if name.starts_with("weapon_") || name.starts_with("viewmodel_") {
        "Weapons"
    } else if name.starts_with("foliage_") {
        "Foliage"
    } else if name.starts_with("vehicle_") {
        "Vehicles"
    } else if name.starts_with("anim_") {
        "Animated props"
    } else if name.starts_with("dest_") {
        "Destructibles"
    } else {
        "Props and other"
    }
}

fn stamp(path: &Path, output: &mut String) -> Result<(), String> {
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    let time = metadata
        .modified()
        .map_err(|error| error.to_string())?
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    output.push_str(&format!("|{}:{}:{time}", path.display(), metadata.len()));
    Ok(())
}

impl Catalog {
    pub fn new(root: PathBuf, cache: PathBuf) -> Self {
        Self {
            root,
            cache,
            index: Index::default(),
            textures: None,
        }
    }

    pub fn scan(&mut self, force: bool) -> Result<Index, String> {
        if !self.root.join("raw/xmodel").is_dir() {
            return Err("Install the WaW Modtools first. The raw/xmodel folder is missing. Then click Rescan.".into());
        }
        if force {
            self.clear_cache()?;
        }
        fs::create_dir_all(self.cache.join("thumbnails")).map_err(|error| error.to_string())?;
        self.textures = Some(Textures::scan(&self.root, self.cache.join("textures"))?);
        // Changing eligibility also invalidates older blank thumbnail images.
        let mut fingerprint = format!("models-v3-unlit-thumbnails|{}", self.root.display());
        let mut paths = Vec::new();
        for folder in ["xmodel", "xmodelsurfs", "xmodelparts", "materials"] {
            let directory = self.root.join("raw").join(folder);
            if !directory.is_dir() {
                continue;
            }
            for entry in fs::read_dir(directory).map_err(|error| error.to_string())? {
                let path = entry.map_err(|error| error.to_string())?.path();
                if path.is_file() {
                    paths.push(path);
                }
            }
        }
        paths.sort();
        for path in &paths {
            stamp(path, &mut fingerprint)?;
        }
        for name in &self.textures.as_ref().unwrap().archives {
            stamp(&self.root.join("main").join(name), &mut fingerprint)?;
        }
        let key = digest(fingerprint.as_bytes());
        let index_path = self.cache.join("index.json");
        if !force {
            if let Ok(bytes) = fs::read(&index_path) {
                if let Ok((cached_key, mut index)) =
                    serde_json::from_slice::<(String, Index)>(&bytes)
                {
                    if cached_key == key {
                        index.cache_path = self.cache.display().to_string();
                        self.remove_hidden(&mut index);
                        self.index = index;
                        return Ok(self.index.clone());
                    }
                }
            }
        }
        let mut index = Index {
            cache_path: self.cache.display().to_string(),
            ..Index::default()
        };
        let mut material_cache: HashMap<String, Result<Material, Exclusion>> = HashMap::new();
        let mut mesh_cache: HashMap<String, Result<(usize, bool), Exclusion>> = HashMap::new();
        for path in paths
            .iter()
            .filter(|path| path.parent() == Some(self.root.join("raw/xmodel").as_path()))
        {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            index.statistics.total += 1;
            let result = (|| {
                let descriptor = model::descriptor(path).map_err(Exclusion::Other)?;
                let first = descriptor
                    .lods
                    .first()
                    .ok_or_else(|| Exclusion::Other("Model has no LOD records.".into()))?;
                // Browse complete models only. The primary LOD must work; lower
                // LODs are offered only when their own inputs pass the same check.
                let mut lods = Vec::new();
                for lod in &descriptor.lods {
                    let result = validate_lod(
                        &self.root,
                        lod,
                        self.textures.as_ref().unwrap(),
                        &mut material_cache,
                        &mut mesh_cache,
                    );
                    match result {
                        Ok(()) => lods.push(lod.slot),
                        Err(error) if lod.slot == first.slot => return Err(error),
                        Err(_) => {}
                    }
                }
                Ok::<Entry, Exclusion>(Entry {
                    id: digest(format!("{key}|{name}").as_bytes()),
                    group: group(&name.to_lowercase()).into(),
                    name: name.clone(),
                    lods,
                })
            })();
            match result {
                Ok(entry) => {
                    index.entries.push(entry);
                }
                Err(error) => {
                    index.statistics.record(&error);
                    index.warnings.push(format!("{name}: {}", error.message()));
                }
            }
        }
        self.remove_hidden(&mut index);
        index.entries.sort_by(|a, b| a.name.cmp(&b.name));
        fs::write(
            index_path,
            serde_json::to_vec(&(&key, &index)).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        self.index = index;
        Ok(self.index.clone())
    }

    fn remove_hidden(&self, index: &mut Index) {
        index.entries.retain(|entry| {
            !self
                .cache
                .join("unavailable")
                .join(format!("{}.txt", entry.id))
                .is_file()
        });
        index.statistics.failed_previews = index
            .statistics
            .eligible()
            .saturating_sub(index.entries.len());
    }

    pub fn hide(&mut self, id: &str) -> Result<(), String> {
        self.entry(id)?;
        let folder = self.cache.join("unavailable");
        fs::create_dir_all(&folder).map_err(|error| error.to_string())?;
        fs::write(
            folder.join(format!("{id}.txt")),
            "No usable preview. Rescan retries this model.\n",
        )
        .map_err(|error| error.to_string())?;
        self.index.entries.retain(|entry| entry.id != id);
        self.index.statistics.failed_previews = self
            .index
            .statistics
            .eligible()
            .saturating_sub(self.index.entries.len());
        Ok(())
    }

    fn entry(&self, id: &str) -> Result<&Entry, String> {
        if id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("Invalid model identifier.".into());
        }
        self.index
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .ok_or("Model is no longer in the catalogue. Click Rescan.".into())
    }

    pub fn load(&self, id: &str, slot: Option<usize>) -> Result<Model, String> {
        let entry = self.entry(id)?;
        let descriptor = model::descriptor(&model::asset_path(&self.root, "xmodel", &entry.name)?)?;
        let slot = slot
            .or_else(|| entry.lods.first().copied())
            .ok_or("No usable LOD.")?;
        if !entry.lods.contains(&slot) {
            return Err("This LOD is not available for preview.".into());
        }
        let lod = descriptor
            .lods
            .iter()
            .find(|lod| lod.slot == slot)
            .ok_or("Model LOD changed. Click Rescan.")?;
        let geometry = model::surfaces(&model::asset_path(&self.root, "xmodelsurfs", &lod.name)?)?;
        if geometry.len() != lod.materials.len()
            || !geometry.iter().any(Surface::has_visible_triangle)
        {
            return Err("No usable model geometry.".into());
        }
        let mut warnings = Vec::new();
        let mut surfaces = Vec::new();
        for (geometry, name) in geometry.into_iter().zip(&lod.materials) {
            let material = model::asset_path(&self.root, "materials", name)
                .and_then(|path| material::read(&path, name))?;
            let image = material
                .image
                .as_ref()
                .ok_or("Material has no colour image.")?;
            if !self
                .textures
                .as_ref()
                .ok_or("Scan models first.")?
                .contains(image)
            {
                return Err(format!("Missing colour image: {image}"));
            }
            surfaces.push(ModelSurface { geometry, material });
        }
        let primary = descriptor.lods.first().ok_or("Model has no primary LOD.")?;
        let bones = match model::asset_path(&self.root, "xmodelparts", &primary.name)
            .and_then(|path| model::bones(&path))
        {
            Ok(bones) => bones,
            Err(error) => {
                warnings.push(format!("Bone data unavailable: {error}"));
                Vec::new()
            }
        };
        Ok(Model {
            name: entry.name.clone(),
            lod: lod.slot,
            lods: entry.lods.clone(),
            surfaces,
            bones,
            warnings,
        })
    }

    pub fn pixels(&self, name: &str, size: u32) -> Result<Vec<u8>, String> {
        self.textures
            .as_ref()
            .ok_or("Scan models first.")?
            .pixels(name, size)
    }

    pub fn thumbnail(&self, id: &str) -> Result<Option<String>, String> {
        self.entry(id)?;
        let path = self.cache.join("thumbnails").join(format!("{id}.png"));
        match fs::read(path) {
            Ok(bytes) => Ok(Some(format!(
                "data:image/png;base64,{}",
                STANDARD.encode(bytes)
            ))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    pub fn save_thumbnail(&self, id: &str, png: &str) -> Result<(), String> {
        self.entry(id)?;
        let encoded = png
            .strip_prefix("data:image/png;base64,")
            .ok_or("Thumbnail must be a PNG.")?;
        if encoded.len() > 512 * 1024 {
            return Err("Thumbnail exceeds the size limit.".into());
        }
        let bytes = STANDARD
            .decode(encoded)
            .map_err(|error| error.to_string())?;
        if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err("Invalid thumbnail PNG.".into());
        }
        fs::create_dir_all(self.cache.join("thumbnails")).map_err(|error| error.to_string())?;
        fs::write(
            self.cache.join("thumbnails").join(format!("{id}.png")),
            bytes,
        )
        .map_err(|error| error.to_string())
    }

    pub fn clear_cache(&mut self) -> Result<(), String> {
        if self.cache.is_dir() {
            fs::remove_dir_all(&self.cache).map_err(|error| error.to_string())?;
        }
        fs::create_dir_all(&self.cache).map_err(|error| error.to_string())?;
        self.textures = None;
        self.index = Index::default();
        Ok(())
    }
}

// Cache lightweight validation results, not every mesh's vertex arrays.
fn validate_lod(
    root: &Path,
    lod: &model::Lod,
    textures: &Textures,
    materials: &mut HashMap<String, Result<Material, Exclusion>>,
    meshes: &mut HashMap<String, Result<(usize, bool), Exclusion>>,
) -> Result<(), Exclusion> {
    let mesh_path = model::asset_path(root, "xmodelsurfs", &lod.name).map_err(Exclusion::Other)?;
    if !mesh_path.is_file() {
        return Err(Exclusion::MissingMesh(format!(
            "Missing mesh: {}",
            lod.name
        )));
    }
    // Count one reason per excluded model. Missing required inputs take priority
    // over empty geometry, so a file problem is not reported as an empty model.
    for material_name in &lod.materials {
        if !materials.contains_key(material_name) {
            let value = (|| {
                let path = model::asset_path(root, "materials", material_name)
                    .map_err(Exclusion::Other)?;
                if !path.is_file() {
                    return Err(Exclusion::MissingFiles(format!(
                        "Missing material: {material_name}"
                    )));
                }
                material::read(&path, material_name).map_err(Exclusion::Other)
            })();
            materials.insert(material_name.clone(), value);
        }
        let material = materials[material_name].as_ref().map_err(Clone::clone)?;
        let image = material
            .image
            .as_ref()
            .ok_or_else(|| Exclusion::Other("Material has no colour image.".into()))?;
        if !textures.contains(image) {
            return Err(Exclusion::MissingFiles(format!(
                "Missing colour image: {image}"
            )));
        }
    }
    if !meshes.contains_key(&lod.name) {
        let value = model::surfaces(&mesh_path)
            .map_err(Exclusion::Other)
            .map(|surfaces| {
                (
                    surfaces.len(),
                    surfaces.iter().any(Surface::has_visible_triangle),
                )
            });
        meshes.insert(lod.name.clone(), value);
    }
    let (count, visible) = meshes[&lod.name].as_ref().map_err(Clone::clone)?;
    if !visible {
        return Err(Exclusion::EmptyGeometry(
            "No visible coloured triangles in this mesh.".into(),
        ));
    }
    if *count != lod.materials.len() {
        return Err(Exclusion::Other(
            "Surface and material counts differ.".into(),
        ));
    }
    Ok(())
}
