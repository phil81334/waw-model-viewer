//! Read main IWDs in the verified native priority. Never use raw/images.
use crate::iwi;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

struct Source {
    archive: PathBuf,
    entry: String,
    crc: u32,
    size: u64,
}

pub struct Textures {
    sources: HashMap<String, Source>,
    cache: PathBuf,
    pub archives: Vec<String>,
}

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn archive_priority(names: &mut [String]) {
    names.sort_by(|a, b| {
        let a_lower = a.to_lowercase();
        let b_lower = b.to_lowercase();
        let a_localised = a_lower.starts_with("localized_");
        let b_localised = b_lower.starts_with("localized_");
        a_localised
            .cmp(&b_localised)
            .then_with(|| b_lower.cmp(&a_lower))
    });
}

impl Textures {
    pub fn scan(root: &Path, cache: PathBuf) -> Result<Self, String> {
        let mut archives = Vec::new();
        for entry in
            fs::read_dir(root.join("main")).map_err(|error| format!("Cannot read main: {error}"))?
        {
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            if path.is_file()
                && path
                    .extension()
                    .is_some_and(|value| value.eq_ignore_ascii_case("iwd"))
            {
                archives.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        archive_priority(&mut archives);
        if archives.is_empty() {
            return Err("No IWD archives found in the game's main folder.".into());
        }
        let mut sources = HashMap::new();
        for name in &archives {
            let path = root.join("main").join(name);
            let file = File::open(&path).map_err(|error| error.to_string())?;
            let mut archive =
                zip::ZipArchive::new(file).map_err(|error| format!("{name}: {error}"))?;
            for index in 0..archive.len() {
                let member = archive.by_index(index).map_err(|error| error.to_string())?;
                let lower = member.name().replace('\\', "/").to_lowercase();
                if member.is_dir() || !lower.starts_with("images/") || !lower.ends_with(".iwi") {
                    continue;
                }
                let image = lower[7..lower.len() - 4].to_owned();
                sources.entry(image).or_insert_with(|| Source {
                    archive: path.clone(),
                    entry: member.name().into(),
                    crc: member.crc32(),
                    size: member.size(),
                });
            }
        }
        Ok(Self {
            sources,
            cache,
            archives,
        })
    }

    pub fn contains(&self, name: &str) -> bool {
        self.sources.contains_key(&name.to_lowercase())
    }

    pub fn archive_for(&self, name: &str) -> Option<String> {
        self.sources.get(&name.to_lowercase()).and_then(|source| {
            source
                .archive
                .file_name()
                .map(|value| value.to_string_lossy().into_owned())
        })
    }

    pub fn pixels(&self, name: &str, max_size: u32) -> Result<Vec<u8>, String> {
        if ![512, 2048].contains(&max_size) {
            return Err("Unsupported preview size.".into());
        }
        let source = self
            .sources
            .get(&name.to_lowercase())
            .ok_or_else(|| format!("Missing colour image: {name}"))?;
        let metadata = fs::metadata(&source.archive).map_err(|error| error.to_string())?;
        let key = digest(
            format!(
                "{}|{}|{}|{}|{:?}|{max_size}",
                source.archive.display(),
                source.entry,
                source.crc,
                metadata.len(),
                metadata.modified()
            )
            .as_bytes(),
        );
        let path = self.cache.join(format!("{key}.rgba"));
        if let Ok(bytes) = fs::read(&path) {
            if bytes.len() >= 8 {
                let width = u32::from_le_bytes(bytes[..4].try_into().unwrap());
                let height = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
                if width > 0
                    && height > 0
                    && width <= max_size
                    && height <= max_size
                    && bytes.len() == 8 + width as usize * height as usize * 4
                {
                    return Ok(bytes);
                }
            }
        }
        if source.size > 128 * 1024 * 1024 {
            return Err("Colour image exceeds the supported file size.".into());
        }
        let file = File::open(&source.archive).map_err(|error| error.to_string())?;
        let mut archive = zip::ZipArchive::new(file).map_err(|error| error.to_string())?;
        let member = archive
            .by_name(&source.entry)
            .map_err(|error| error.to_string())?;
        let mut input = Vec::new();
        member
            .take(128 * 1024 * 1024 + 1)
            .read_to_end(&mut input)
            .map_err(|error| error.to_string())?;
        let levels = iwi::decode(&input)?;
        let level = levels
            .iter()
            .find(|level| level.width <= max_size && level.height <= max_size)
            .or_else(|| levels.last())
            .ok_or("Colour image has no mip levels.")?;
        if level.faces.len() != 1 {
            return Err("A cubemap cannot be used as a flat colour texture.".into());
        }
        let mut output = Vec::with_capacity(8 + level.faces[0].len());
        output.extend(level.width.to_le_bytes());
        output.extend(level.height.to_le_bytes());
        output.extend(&level.faces[0]);
        fs::create_dir_all(&self.cache).map_err(|error| error.to_string())?;
        fs::write(path, &output).map_err(|error| error.to_string())?;
        Ok(output)
    }
}
