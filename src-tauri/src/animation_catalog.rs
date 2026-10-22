//! Source-backed, read-only single-clip choices. Parsing a clip is not pairing it.
use crate::{
    animation::{self, Clip},
    catalog::Entry,
    model::{self, Bone},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

const CACHE_VERSION: &str = "animation-catalogue-2";
const HUMAN_CORE: [(&str, &str); 20] = [
    ("j_mainroot", "tag_origin"),
    ("j_hip_le", "j_mainroot"),
    ("j_hip_ri", "j_mainroot"),
    ("j_knee_le", "j_hip_le"),
    ("j_knee_ri", "j_hip_ri"),
    ("j_ankle_le", "j_knee_le"),
    ("j_ankle_ri", "j_knee_ri"),
    ("j_spinelower", "j_mainroot"),
    ("j_spineupper", "j_spinelower"),
    ("j_spine4", "j_spineupper"),
    ("j_neck", "j_spine4"),
    ("j_head", "j_neck"),
    ("j_clavicle_le", "j_spine4"),
    ("j_clavicle_ri", "j_spine4"),
    ("j_shoulder_le", "j_clavicle_le"),
    ("j_shoulder_ri", "j_clavicle_ri"),
    ("j_elbow_le", "j_shoulder_le"),
    ("j_elbow_ri", "j_shoulder_ri"),
    ("j_wrist_le", "j_elbow_le"),
    ("j_wrist_ri", "j_elbow_ri"),
];

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Statistics {
    pub total: usize,
    pub invalid_files: usize,
    pub unsupported_files: usize,
    pub supported_clips: usize,
    pub models_with_animations: usize,
    pub model_clip_pairs: usize,
    pub missing_references: usize,
    pub available: usize,
    pub not_offered: usize,
}

#[derive(Clone, Serialize)]
pub struct OptionEntry {
    pub name: String,
    pub context: String,
}

#[derive(Serialize)]
pub struct Selection {
    pub entries: Vec<OptionEntry>,
    pub statistics: Statistics,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Playback {
    pub clip: Clip,
    pub parent_bones: Vec<Bone>,
    pub context: String,
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Stamp {
    path: PathBuf,
    length: u64,
    modified: u128,
}

impl Stamp {
    fn read(path: &Path) -> Result<Self, String> {
        let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
        let modified = metadata
            .modified()
            .map_err(|error| error.to_string())?
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_nanos();
        Ok(Self {
            path: path.to_owned(),
            length: metadata.len(),
            modified,
        })
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Metadata {
    name: String,
    stamp: Stamp,
    error: Option<String>,
    asset_type: u8,
    names: BTreeSet<String>,
    root_keys: bool,
}

impl Metadata {
    fn from_clip(clip: &Clip, name: String, stamp: Stamp) -> Self {
        let root_keys = clip.delta.as_ref().is_some_and(|delta| {
            delta
                .rotation
                .as_ref()
                .is_some_and(|track| !track.values.is_empty())
                || delta
                    .translation
                    .as_ref()
                    .is_some_and(|track| !track.values.is_empty())
        });
        Self {
            name,
            stamp,
            error: None,
            asset_type: clip.asset_type,
            names: clip.bones.iter().map(|track| key(&track.name)).collect(),
            root_keys,
        }
    }

    fn supported(&self) -> bool {
        self.error.is_none() && animation::supports_type(self.asset_type)
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Rig {
    bones: Vec<Bone>,
    names: BTreeSet<String>,
    weighted: BTreeSet<usize>,
    required: BTreeSet<String>,
    stamps: Vec<Stamp>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Association {
    context: String,
    parent_bones: Vec<Bone>,
    input_stamps: Vec<Stamp>,
}

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    fingerprint: String,
    clips: BTreeMap<String, Metadata>,
    rigs: BTreeMap<String, Rig>,
    choices: BTreeMap<String, BTreeMap<String, Association>>,
    missing: BTreeSet<String>,
    diagnostics: BTreeMap<String, usize>,
    blocked_contexts: BTreeSet<String>,
}

pub struct AnimationCatalog {
    root: PathBuf,
    cache: PathBuf,
    saved: Saved,
    loaded: Option<(String, Clip)>,
    counts: Statistics,
}

fn key(value: &str) -> String {
    value.replace('\\', "/").to_ascii_lowercase()
}

fn files(folder: &Path) -> Vec<PathBuf> {
    let mut output = Vec::new();
    if let Ok(entries) = fs::read_dir(folder) {
        for entry in entries.flatten() {
            if let Ok(kind) = entry.file_type() {
                if kind.is_dir() {
                    output.extend(files(&entry.path()));
                } else if kind.is_file() {
                    output.push(entry.path());
                }
            }
        }
    }
    output.sort();
    output
}

fn asset_files(folder: &Path) -> BTreeMap<String, String> {
    files(folder)
        .into_iter()
        .filter_map(|path| {
            let relative = path
                .strip_prefix(folder)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/");
            Some((key(&relative), relative))
        })
        .collect()
}

#[derive(Clone)]
struct Token {
    text: String,
    literal: bool,
}

/// Keep strings and line boundaries while discarding comments. This is a
/// deliberately small literal-reference reader, not a GSC interpreter.
fn tokens(text: &str) -> Vec<Token> {
    let bytes = text.as_bytes();
    let mut at = 0;
    let mut output = Vec::new();
    while at < bytes.len() {
        let start = at;
        let byte = bytes[at];
        if byte == b'\n' {
            output.push(Token {
                text: "\n".into(),
                literal: false,
            });
            at += 1;
        } else if byte.is_ascii_whitespace() {
            at += 1;
        } else if bytes.get(at..at + 2) == Some(b"//") {
            while at < bytes.len() && bytes[at] != b'\n' {
                at += 1;
            }
        } else if bytes.get(at..at + 2) == Some(b"/*") {
            at += 2;
            while at + 1 < bytes.len() && &bytes[at..at + 2] != b"*/" {
                if bytes[at] == b'\n' {
                    output.push(Token {
                        text: "\n".into(),
                        literal: false,
                    });
                }
                at += 1;
            }
            at = (at + 2).min(bytes.len());
        } else if byte == b'"' {
            at += 1;
            let value = at;
            while at < bytes.len() && bytes[at] != b'"' {
                if bytes[at] == b'\\' && bytes.get(at + 1) == Some(&b'"') {
                    at += 1;
                }
                at += 1;
            }
            output.push(Token {
                text: text[value..at].to_owned(),
                literal: true,
            });
            at = (at + 1).min(bytes.len());
        } else if byte.is_ascii_alphanumeric() || byte == b'_' {
            at += 1;
            while at < bytes.len()
                && (bytes[at].is_ascii_alphanumeric()
                    || matches!(bytes[at], b'_' | b'\\' | b'/' | b'-'))
            {
                at += 1;
            }
            output.push(Token {
                text: text[start..at].to_owned(),
                literal: false,
            });
        } else {
            output.push(Token {
                text: (byte as char).to_string(),
                literal: false,
            });
            at += 1;
        }
    }
    output
}

fn source(path: &Path) -> Vec<Token> {
    // Original scripts/trees can contain Windows-1252 punctuation in comments.
    // Their ASCII identifiers remain usable; a UTF-8-only read would silently
    // discard otherwise valid source associations.
    fs::read(path)
        .map(|bytes| tokens(&String::from_utf8_lossy(&bytes)))
        .unwrap_or_default()
}

fn flat_source(path: &Path) -> Vec<Token> {
    source(path)
        .into_iter()
        .filter(|token| token.text != "\n")
        .collect()
}

fn word(items: &[Token], at: usize, value: &str) -> bool {
    items
        .get(at)
        .is_some_and(|token| token.text.eq_ignore_ascii_case(value))
}

fn literal(items: &[Token], at: usize) -> Option<String> {
    items
        .get(at)
        .filter(|token| token.literal)
        .map(|token| key(&token.text))
}

fn next_line(items: &[Token], mut at: usize) -> usize {
    while word(items, at, "\n") {
        at += 1;
    }
    at
}

/// A leaf can appear in several branches; a plain occurrence is sufficient,
/// but an additive-only leaf always needs an unimplemented base/blend context.
fn tree_leaves(items: &[Token]) -> BTreeMap<String, bool> {
    let mut output = BTreeMap::new();
    let mut stack = Vec::new();
    let mut at = 0;
    while at < items.len() {
        at = next_line(items, at);
        if at >= items.len() {
            break;
        }
        if word(items, at, "}") {
            stack.pop();
            at += 1;
            continue;
        }
        let name = key(&items[at].text);
        at += 1;
        let mut additive = stack.iter().any(|value| *value);
        if word(items, at, ":") {
            at += 1;
            while at < items.len() && !["\n", "{", "}"].contains(&items[at].text.as_str()) {
                additive |= word(items, at, "additive");
                at += 1;
            }
        }
        at = next_line(items, at);
        if word(items, at, "{") {
            stack.push(additive);
            at += 1;
        } else if !["{", ":"].contains(&name.as_str()) {
            output
                .entry(name)
                .and_modify(|allowed| *allowed |= !additive)
                .or_insert(!additive);
        }
    }
    output
}

#[derive(Default)]
struct Sources {
    weapons: Vec<(String, String, String)>,
    props: Vec<(String, String, String)>,
    bodies: BTreeSet<String>,
    dogs: Vec<(String, String)>,
    trees: BTreeMap<String, BTreeMap<String, bool>>,
}

fn reference(items: &[Token], prefix: &str) -> Vec<String> {
    items
        .iter()
        .enumerate()
        .filter_map(|(at, token)| {
            let value = key(&token.text);
            if value.starts_with(prefix)
                && word(items, at + 1, ":")
                && word(items, at + 2, ":")
                && word(items, at + 3, "main")
            {
                Some(value[prefix.len()..].to_owned())
            } else {
                None
            }
        })
        .collect()
}

impl Sources {
    fn read(root: &Path) -> Self {
        let raw = root.join("raw");
        let mut output = Self::default();
        for path in files(&raw.join("animtrees")) {
            if path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("atr"))
            {
                if let Some(name) = path.file_stem() {
                    output
                        .trees
                        .insert(key(&name.to_string_lossy()), tree_leaves(&source(&path)));
                }
            }
        }
        for mode in ["sp", "mp"] {
            for path in files(&raw.join("weapons").join(mode)) {
                let Ok(text) = fs::read_to_string(path) else {
                    continue;
                };
                let values: Vec<_> = text.split('\\').collect();
                let fields: BTreeMap<_, _> = values
                    .get(1..)
                    .unwrap_or_default()
                    .chunks_exact(2)
                    .map(|pair| (key(pair[0]), key(pair[1])))
                    .collect();
                let Some(hands) = fields.get("handmodel") else {
                    continue;
                };
                for (field, gun) in &fields {
                    if gun.is_empty()
                        || !(field == "gunmodel"
                            || field.strip_prefix("gunmodel").is_some_and(|suffix| {
                                !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit())
                            }))
                    {
                        continue;
                    }
                    for (field, clip) in &fields {
                        if field.ends_with("anim") && !clip.is_empty() {
                            output
                                .weapons
                                .push((gun.clone(), hands.clone(), clip.clone()));
                        }
                    }
                }
            }
        }
        let mut aliases: HashMap<String, Vec<String>> = HashMap::new();
        for path in files(&raw.join("xmodelalias")) {
            let items = flat_source(&path);
            let mut names = Vec::new();
            for at in 0..items.len() {
                if word(&items, at, "a")
                    && word(&items, at + 1, "[")
                    && word(&items, at + 3, "]")
                    && word(&items, at + 4, "=")
                {
                    if let Some(name) = literal(&items, at + 5) {
                        names.push(name);
                    }
                }
            }
            if let Some(name) = path.file_stem() {
                aliases.insert(key(&name.to_string_lossy()), names);
            }
        }
        let mut characters = HashMap::new();
        for path in files(&raw.join("character")) {
            let items = flat_source(&path);
            let mut names = Vec::new();
            for at in 0..items.len() {
                if word(&items, at, "self")
                    && word(&items, at + 1, "setmodel")
                    && word(&items, at + 2, "(")
                {
                    if let Some(name) = literal(&items, at + 3) {
                        names.push(name);
                    }
                }
                if word(&items, at, "setmodelfromarray") && word(&items, at + 1, "(") {
                    if let Some(token) = items.get(at + 2) {
                        if let Some(alias) = key(&token.text).strip_prefix("xmodelalias/") {
                            names.extend(aliases.get(alias).into_iter().flatten().cloned());
                        }
                    }
                }
            }
            output.bodies.extend(names.iter().cloned());
            if let Some(name) = path.file_stem() {
                characters.insert(key(&name.to_string_lossy()), names);
            }
        }
        for path in files(&raw.join("aitype")) {
            let items = flat_source(&path);
            let trees: Vec<_> = (0..items.len())
                .filter_map(|at| {
                    if word(&items, at, "animtree") && word(&items, at + 1, "=") {
                        literal(&items, at + 2)
                    } else {
                        None
                    }
                })
                .collect();
            if !trees
                .iter()
                .any(|tree| tree.trim_end_matches(".atr") == "dog")
            {
                continue;
            }
            for character in reference(&items, "character/") {
                for model in characters.get(&character).into_iter().flatten() {
                    output.dogs.push((model.clone(), "dog".into()));
                }
            }
        }
        for path in files(&raw.join("maps")) {
            if path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("gsc"))
            {
                output.props.extend(prop_references(&source(&path)));
            }
        }
        output
    }
}

#[derive(Clone)]
struct Assignment {
    field: String,
    group: String,
    scope: usize,
    value: Option<String>,
    tree: String,
}

struct Call {
    method: String,
    receiver: String,
    scope: usize,
    at: usize,
    model: Option<String>,
    clips: Vec<String>,
    tree: String,
}

fn prop_references(input: &[Token]) -> Vec<(String, String, String)> {
    let items: Vec<_> = input
        .iter()
        .filter(|token| token.text != "\n")
        .cloned()
        .collect();
    let mut assignments = Vec::new();
    let mut calls = Vec::new();
    let mut tree = String::new();
    let mut depth = 0usize;
    let mut scope = 0usize;
    let mut sequence = 0usize;
    let mut at = 0;
    while at < items.len() {
        if word(&items, at, "using_animtree") && word(&items, at + 1, "(") {
            tree = literal(&items, at + 2).unwrap_or_default();
        }
        if word(&items, at, "{") {
            if depth == 0 {
                sequence += 1;
                scope = sequence;
            }
            depth += 1;
        } else if word(&items, at, "}") {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                scope = 0;
            }
        }
        if word(&items, at, "level") && word(&items, at + 1, ".") {
            let field = items
                .get(at + 2)
                .map(|token| key(&token.text))
                .unwrap_or_default();
            if matches!(field.as_str(), "scr_model" | "scr_anim") {
                let mut next = at + 3;
                let mut group = None;
                while word(&items, next, "[") {
                    let start = next + 1;
                    next += 1;
                    while next < items.len() && !word(&items, next, "]") {
                        next += 1;
                    }
                    if group.is_none() && next == start + 1 {
                        if let Some(token) = items.get(start) {
                            if token.literal
                                || token.text.chars().all(|value| value.is_ascii_digit())
                            {
                                group = Some(key(&token.text));
                            }
                        }
                    }
                    next += 1;
                }
                if word(&items, next, "=") {
                    let start = next + 1;
                    next = start;
                    while next < items.len() && !word(&items, next, ";") {
                        next += 1;
                    }
                    if let Some(group) = group {
                        let value = if field == "scr_model" && next == start + 1 {
                            literal(&items, start)
                        } else if field == "scr_anim"
                            && next == start + 2
                            && word(&items, start, "%")
                        {
                            items.get(start + 1).map(|token| key(&token.text))
                        } else {
                            None
                        };
                        assignments.push(Assignment {
                            field,
                            group,
                            scope,
                            value,
                            tree: tree.clone(),
                        });
                    }
                    at = next;
                    continue;
                }
            }
        }
        let method = key(&items[at].text);
        if [
            "setmodel",
            "setanim",
            "animscripted",
            "setflaggedanim",
            "setflaggedanimknob",
            "setflaggedanimknobrestart",
        ]
        .contains(&method.as_str())
            && word(&items, at + 1, "(")
        {
            let mut next = at + 2;
            let start = next;
            let mut nesting = 1usize;
            while next < items.len() {
                if word(&items, next, "(") {
                    nesting += 1;
                } else if word(&items, next, ")") {
                    nesting -= 1;
                    if nesting == 0 {
                        break;
                    }
                }
                next += 1;
            }
            let clips = (start..next)
                .filter_map(|index| {
                    if word(&items, index, "%") {
                        items.get(index + 1).map(|token| key(&token.text))
                    } else {
                        None
                    }
                })
                .collect();
            let model = if method == "setmodel" && next == start + 1 {
                literal(&items, start)
            } else {
                None
            };
            calls.push(Call {
                method,
                receiver: at
                    .checked_sub(1)
                    .and_then(|index| items.get(index))
                    .map(|token| key(&token.text))
                    .unwrap_or_default(),
                scope,
                at,
                model,
                clips,
                tree: tree.clone(),
            });
        }
        at += 1;
    }
    let mut output = Vec::new();
    for animation in assignments.iter().filter(|value| value.field == "scr_anim") {
        let Some(clip) = &animation.value else {
            continue;
        };
        let mut models: Vec<_> = assignments
            .iter()
            .filter(|value| value.field == "scr_model" && value.group == animation.group)
            .collect();
        if models.iter().any(|value| value.scope == animation.scope) {
            models.retain(|value| value.scope == animation.scope);
        }
        let names: BTreeSet<_> = models
            .iter()
            .filter_map(|value| value.value.clone())
            .collect();
        if names.len() == 1 && models.iter().all(|value| value.value.is_some()) {
            output.push((
                names.into_iter().next().unwrap(),
                clip.clone(),
                animation.tree.clone(),
            ));
        }
    }
    for animation in calls.iter().filter(|call| !call.clips.is_empty()) {
        let models: Vec<_> = calls
            .iter()
            .filter(|call| {
                call.method == "setmodel"
                    && call.scope == animation.scope
                    && call.receiver == animation.receiver
                    && call.at < animation.at
            })
            .collect();
        let names: BTreeSet<_> = models
            .iter()
            .filter_map(|call| call.model.clone())
            .collect();
        if names.len() != 1 || models.iter().any(|call| call.model.is_none()) {
            continue;
        }
        // A literal SetModel does not survive reassignment of its receiver.
        let start = models.last().map(|call| call.at).unwrap_or(0);
        if (start..animation.at)
            .any(|index| word(&items, index, &animation.receiver) && word(&items, index + 1, "="))
        {
            continue;
        }
        let name = names.into_iter().next().unwrap();
        for clip in &animation.clips {
            output.push((name.clone(), clip.clone(), animation.tree.clone()));
        }
    }
    output
}

fn read_rig(root: &Path, name: &str, entry: Option<&Entry>) -> Result<Rig, String> {
    let descriptor_path = model::asset_path(root, "xmodel", name)?;
    let descriptor = model::descriptor(&descriptor_path)?;
    let primary = descriptor.lods.first().ok_or("Model has no primary LOD.")?;
    let parts = model::asset_path(root, "xmodelparts", &primary.name)?;
    let bones = model::bones(&parts)?;
    if bones.is_empty()
        || bones.len() > 128
        || bones.iter().filter(|bone| bone.parent.is_none()).count() != 1
    {
        return Err("Unsupported primary animation rig.".into());
    }
    let mut names = BTreeSet::new();
    for (index, bone) in bones.iter().enumerate() {
        if !names.insert(key(&bone.name))
            || bone.parent.is_some_and(|parent| parent >= index)
            || !bone
                .local_position
                .iter()
                .chain(bone.local_rotation.iter())
                .all(|value| value.is_finite())
            || bone
                .local_rotation
                .iter()
                .map(|value| value * value)
                .sum::<f64>()
                <= 0.0
        {
            return Err("Invalid or ambiguous bind rig.".into());
        }
    }
    let mut weighted = BTreeSet::new();
    let mut stamps = vec![Stamp::read(&descriptor_path)?, Stamp::read(&parts)?];
    for lod in &descriptor.lods {
        if entry.is_some_and(|entry| !entry.lods.contains(&lod.slot)) {
            continue;
        }
        if entry.is_none() && lod.slot != primary.slot {
            continue;
        }
        let path = model::asset_path(root, "xmodelsurfs", &lod.name)?;
        stamps.push(Stamp::read(&path)?);
        for surface in model::surfaces(&path)? {
            let count = surface.positions.len() / 3;
            if surface.skin_indices.len() != count * 4 || surface.skin_weights.len() != count * 4 {
                return Err("Incomplete vertex skinning.".into());
            }
            for (indices, weights) in surface
                .skin_indices
                .chunks_exact(4)
                .zip(surface.skin_weights.chunks_exact(4))
            {
                if indices
                    .iter()
                    .any(|index| usize::from(*index) >= bones.len())
                    || weights
                        .iter()
                        .any(|weight| !weight.is_finite() || *weight < 0.0)
                    || (weights.iter().sum::<f32>() - 1.0).abs() > 0.00001
                {
                    return Err("Vertex influences do not fit the primary rig.".into());
                }
                // Pair eligibility follows the primary mesh. Every offered LOD
                // still receives the index/weight gate above.
                if lod.slot == primary.slot {
                    for (index, weight) in indices.iter().zip(weights) {
                        if *weight > 0.0 {
                            weighted.insert(usize::from(*index));
                        }
                    }
                }
            }
        }
    }
    let mut required = BTreeSet::new();
    for index in &weighted {
        let mut next = Some(*index);
        while let Some(index) = next {
            required.insert(key(&bones[index].name));
            next = bones[index].parent;
        }
    }
    Ok(Rig {
        bones,
        names,
        weighted,
        required,
        stamps,
    })
}

fn parent_chain(rig: &Rig) -> Option<Vec<Bone>> {
    let mut index = rig
        .bones
        .iter()
        .position(|bone| bone.name.eq_ignore_ascii_case("tag_weapon"))?;
    let mut chain = Vec::new();
    loop {
        chain.push(rig.bones[index].clone());
        match rig.bones[index].parent {
            Some(parent) => index = parent,
            None => break,
        }
    }
    chain.reverse();
    for (index, bone) in chain.iter_mut().enumerate() {
        bone.parent = index.checked_sub(1);
    }
    Some(chain)
}

fn human_rig(rig: &Rig) -> bool {
    let roots: Vec<_> = rig
        .bones
        .iter()
        .filter(|bone| bone.parent.is_none())
        .collect();
    if roots.len() != 1 || !roots[0].name.eq_ignore_ascii_case("tag_origin") {
        return false;
    }
    HUMAN_CORE.iter().all(|(child, parent)| {
        rig.bones
            .iter()
            .find(|bone| bone.name.eq_ignore_ascii_case(child))
            .and_then(|bone| bone.parent)
            .is_some_and(|index| rig.bones[index].name.eq_ignore_ascii_case(parent))
    })
}

fn equipment_weighted(rig: &Rig) -> bool {
    rig.weighted.iter().any(|index| {
        let name = key(&rig.bones[*index].name);
        (name.starts_with("tag_") && name != "tag_origin")
            || name.starts_with("j_hose")
            || name
                .strip_prefix("j_gun")
                .is_some_and(|suffix| suffix.chars().all(|value| value.is_ascii_digit()))
    })
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

fn normalized(mut value: [f64; 4]) -> Option<[f64; 4]> {
    let length = value.iter().map(|value| value * value).sum::<f64>().sqrt();
    if !length.is_finite() || length <= 0.0 {
        return None;
    }
    for component in &mut value {
        *component /= length;
    }
    Some(value)
}

fn rotate(q: [f64; 4], value: [f64; 3]) -> [f64; 3] {
    let result = multiply(
        multiply(q, [value[0], value[1], value[2], 0.0]),
        [-q[0], -q[1], -q[2], q[3]],
    );
    [result[0], result[1], result[2]]
}

fn interval(indices: &[u16], count: usize, frame: f64) -> (usize, usize, f64) {
    if count <= 1 {
        return (0, 0, 0.0);
    }
    let lower = indices
        .partition_point(|value| f64::from(*value) <= frame)
        .saturating_sub(1);
    let upper = (lower + 1).min(count - 1);
    let weight = if lower == upper {
        0.0
    } else {
        (frame - f64::from(indices[lower])) / f64::from(indices[upper] - indices[lower])
    };
    (lower, upper, weight)
}

fn sampled_rotation(track: &animation::Keys4, frame: f64) -> Option<[f64; 4]> {
    if track.values.is_empty() {
        return Some([0.0, 0.0, 0.0, 1.0]);
    }
    let (lower, upper, weight) = interval(&track.indices, track.values.len(), frame);
    normalized(std::array::from_fn(|axis| {
        f64::from(track.values[lower][axis]) * (1.0 - weight)
            + f64::from(track.values[upper][axis]) * weight
    }))
}

fn sampled_translation(track: &animation::Keys3, frame: f64) -> [f64; 3] {
    if track.values.is_empty() {
        return [0.0; 3];
    }
    let (lower, upper, weight) = interval(&track.indices, track.values.len(), frame);
    std::array::from_fn(|axis| {
        f64::from(track.values[lower][axis]) * (1.0 - weight)
            + f64::from(track.values[upper][axis]) * weight
    })
}

type Pose = Vec<([f64; 4], [f64; 3])>;

fn world_pose(
    bones: &[Bone],
    tracks: &HashMap<String, &animation::Track>,
    frame: f64,
) -> Option<Pose> {
    let mut output: Pose = Vec::with_capacity(bones.len());
    for bone in bones {
        let (mut q, offset) = if let Some(track) = tracks.get(&key(&bone.name)) {
            (
                sampled_rotation(&track.rotation, frame)?,
                sampled_translation(&track.translation, frame),
            )
        } else {
            (normalized(bone.local_rotation)?, [0.0; 3])
        };
        let mut position = std::array::from_fn(|axis| bone.local_position[axis] + offset[axis]);
        if let Some(parent) = bone.parent {
            let (parent_q, parent_position) = *output.get(parent)?;
            q = normalized(multiply(parent_q, q))?;
            let rotated = rotate(parent_q, position);
            position = std::array::from_fn(|axis| rotated[axis] + parent_position[axis]);
        }
        if !q
            .iter()
            .chain(position.iter())
            .all(|value| value.is_finite())
        {
            return None;
        }
        output.push((q, position));
    }
    Some(output)
}

/// Finite local/world materialisation is a live gate, even after key validation.
/// Exact complete bind rigs share this check; weighted-chain admission remains
/// model specific. It does not claim to reproduce gameplay blending or events.
fn finite_poses(clip: &Clip, rig: &Rig, parent_bones: &[Bone]) -> bool {
    let tracks: HashMap<_, _> = clip
        .bones
        .iter()
        .map(|track| (key(&track.name), track))
        .collect();
    for frame in [0.0, clip.last_frame as f64 * 0.5, clip.last_frame as f64] {
        let Some(world) = world_pose(&rig.bones, &tracks, frame) else {
            return false;
        };
        let parent = if parent_bones.is_empty() {
            None
        } else {
            let Some(parent) = world_pose(parent_bones, &tracks, frame) else {
                return false;
            };
            parent.last().copied()
        };
        let root = if let Some(delta) = &clip.delta {
            let rotation = match &delta.rotation {
                Some(track) => match sampled_rotation(track, frame) {
                    Some(value) => value,
                    None => return false,
                },
                None => [0.0, 0.0, 0.0, 1.0],
            };
            let translation = delta
                .translation
                .as_ref()
                .map(|track| sampled_translation(track, frame))
                .unwrap_or([0.0; 3]);
            Some((rotation, translation))
        } else {
            None
        };
        for (mut q, mut position) in world {
            for (parent_q, parent_position) in [parent, root].into_iter().flatten() {
                q = match normalized(multiply(parent_q, q)) {
                    Some(value) => value,
                    None => return false,
                };
                let rotated = rotate(parent_q, position);
                position = std::array::from_fn(|axis| rotated[axis] + parent_position[axis]);
            }
            if !q
                .iter()
                .chain(position.iter())
                .all(|value| value.is_finite())
            {
                return false;
            }
        }
    }
    true
}

fn binding_signature(rig_signature: &str, association: &Association) -> String {
    if association.parent_bones.is_empty() {
        return rig_signature.to_owned();
    }
    let mut hash = Sha256::new();
    hash.update(rig_signature);
    if let Ok(bytes) = serde_json::to_vec(&association.parent_bones) {
        hash.update(bytes);
    }
    format!("{:x}", hash.finalize())
}

impl AnimationCatalog {
    pub fn new(root: PathBuf, cache: PathBuf) -> Self {
        Self {
            root,
            cache,
            saved: Saved::default(),
            loaded: None,
            counts: Statistics::default(),
        }
    }

    fn recount(&mut self) {
        let clips: BTreeSet<_> = self
            .saved
            .choices
            .values()
            .flat_map(|choices| choices.keys())
            .collect();
        self.counts = Statistics {
            total: self.saved.clips.len(),
            invalid_files: self
                .saved
                .clips
                .values()
                .filter(|clip| clip.error.is_some())
                .count(),
            unsupported_files: self
                .saved
                .clips
                .values()
                .filter(|clip| clip.error.is_none() && !clip.supported())
                .count(),
            supported_clips: clips.len(),
            models_with_animations: self
                .saved
                .choices
                .values()
                .filter(|choices| !choices.is_empty())
                .count(),
            model_clip_pairs: self.saved.choices.values().map(BTreeMap::len).sum(),
            missing_references: self.saved.missing.len(),
            available: 0,
            not_offered: self.saved.clips.len(),
        };
    }

    fn statistics(&self, selected: Option<&str>) -> Statistics {
        let mut statistics = self.counts.clone();
        statistics.available = selected
            .and_then(|name| self.saved.choices.get(&key(name)))
            .map(BTreeMap::len)
            .unwrap_or(0);
        statistics.not_offered = statistics.total.saturating_sub(statistics.available);
        statistics
    }

    fn fingerprint(&self, entries: &[Entry]) -> String {
        let mut hash = Sha256::new();
        hash.update(CACHE_VERSION);
        hash.update(self.root.to_string_lossy().as_bytes());
        for folder in [
            "xanim",
            "xmodel",
            "xmodelparts",
            "xmodelsurfs",
            "weapons",
            "animtrees",
            "character",
            "xmodelalias",
            "aitype",
            "maps",
        ] {
            for path in files(&self.root.join("raw").join(folder)) {
                if let Ok(stamp) = Stamp::read(&path) {
                    hash.update(stamp.path.to_string_lossy().as_bytes());
                    hash.update(stamp.length.to_le_bytes());
                    hash.update(stamp.modified.to_le_bytes());
                }
            }
        }
        for entry in entries {
            hash.update(entry.name.as_bytes());
            for slot in &entry.lods {
                hash.update(slot.to_le_bytes());
            }
        }
        format!("{:x}", hash.finalize())
    }

    pub fn scan(&mut self, entries: &[Entry], force: bool) -> Result<Statistics, String> {
        self.scan_impl(entries, force, true)
    }

    pub fn scan_detached(&mut self, entries: &[Entry], force: bool) -> Result<Statistics, String> {
        self.scan_impl(entries, force, false)
    }

    fn scan_impl(
        &mut self,
        entries: &[Entry],
        force: bool,
        persist: bool,
    ) -> Result<Statistics, String> {
        let fingerprint = self.fingerprint(entries);
        let cache_path = self.cache.join("animations.json");
        if !force {
            if self.saved.fingerprint == fingerprint {
                return Ok(self.statistics(None));
            }
            if let Ok(bytes) = fs::read(&cache_path) {
                if let Ok(saved) = serde_json::from_slice::<Saved>(&bytes) {
                    if saved.fingerprint == fingerprint {
                        self.saved = saved;
                        self.loaded = None;
                        self.recount();
                        return Ok(self.statistics(None));
                    }
                }
            }
        }
        self.loaded = None;
        self.saved = Saved {
            fingerprint,
            ..Saved::default()
        };
        let raw = self.root.join("raw");
        let animation_files = asset_files(&raw.join("xanim"));
        for (id, name) in animation_files {
            let path = model::asset_path(&self.root, "xanim", &name)?;
            let stamp = Stamp::read(&path)?;
            let metadata = match animation::read(&path) {
                Ok(clip) => Metadata::from_clip(&clip, name, stamp),
                Err(error) => Metadata {
                    name,
                    stamp,
                    error: Some(error),
                    asset_type: 0,
                    names: BTreeSet::new(),
                    root_keys: false,
                },
            };
            self.saved.clips.insert(id, metadata);
        }
        let sources = Sources::read(&self.root);
        self.saved
            .diagnostics
            .insert("source-body-models".into(), sources.bodies.len());
        self.saved
            .diagnostics
            .insert("source-prop-references".into(), sources.props.len());
        self.saved.diagnostics.insert(
            "generic-human-leaves".into(),
            sources
                .trees
                .get("generic_human")
                .map(BTreeMap::len)
                .unwrap_or(0),
        );
        for clip in sources
            .weapons
            .iter()
            .map(|row| &row.2)
            .chain(sources.props.iter().map(|row| &row.1))
        {
            if !self.saved.clips.contains_key(clip) {
                self.saved.missing.insert(clip.clone());
            }
        }
        let visible: BTreeMap<_, _> = entries
            .iter()
            .map(|entry| (key(&entry.name), entry))
            .collect();
        let model_files = asset_files(&raw.join("xmodel"));
        let mut needed = sources.bodies.clone();
        needed.extend(sources.props.iter().map(|row| row.0.clone()));
        needed.extend(sources.dogs.iter().map(|row| row.0.clone()));
        needed.extend(sources.weapons.iter().map(|row| row.0.clone()));
        let hands: BTreeSet<_> = sources.weapons.iter().map(|row| row.1.clone()).collect();
        needed.extend(hands.iter().cloned());
        for name in needed {
            if !visible.contains_key(&name) && !hands.contains(&name) {
                continue;
            }
            let Some(actual) = model_files.get(&name) else {
                continue;
            };
            match read_rig(&self.root, actual, visible.get(&name).copied()) {
                Ok(rig) => {
                    self.saved.rigs.insert(name, rig);
                }
                Err(_) => {
                    *self
                        .saved
                        .diagnostics
                        .entry("model-rig-inputs".into())
                        .or_default() += 1;
                }
            }
        }
        self.weapon_options(&sources, &visible);
        self.prop_options(&sources, &visible);
        self.family_options(&sources, &visible);
        self.validate_pairs();
        self.recount();
        // This cache is disposable. A read-only cache folder must not prevent
        // viewing assets that have just passed the live scan.
        if persist {
            self.save_cache();
        }
        Ok(self.statistics(None))
    }

    pub fn save_cache(&self) {
        if let Ok(bytes) = serde_json::to_vec(&self.saved) {
            if fs::create_dir_all(&self.cache).is_ok() {
                let _ = fs::write(self.cache.join("animations.json"), bytes);
            }
        }
    }

    fn admit(
        &mut self,
        model: &str,
        clip: &str,
        context: &str,
        parent_bones: Vec<Bone>,
        input_stamps: Vec<Stamp>,
    ) {
        let context_id = format!("{model}\0{clip}");
        if self.saved.blocked_contexts.contains(&context_id) {
            return;
        }
        let Some(metadata) = self.saved.clips.get(clip) else {
            self.saved.missing.insert(clip.to_owned());
            return;
        };
        if !metadata.supported() {
            return;
        }
        let choices = self.saved.choices.entry(model.to_owned()).or_default();
        if let Some(previous) = choices.get(clip) {
            if !previous.parent_bones.is_empty() && !parent_bones.is_empty() {
                let before = serde_json::to_vec(&previous.parent_bones).ok();
                let after = serde_json::to_vec(&parent_bones).ok();
                if before != after {
                    choices.remove(clip);
                    self.saved.blocked_contexts.insert(context_id);
                    *self
                        .saved
                        .diagnostics
                        .entry("different-parent-contexts".into())
                        .or_default() += 1;
                }
            }
            return;
        }
        choices.insert(
            clip.to_owned(),
            Association {
                context: context.to_owned(),
                parent_bones,
                input_stamps,
            },
        );
    }

    fn weapon_options(&mut self, sources: &Sources, visible: &BTreeMap<String, &Entry>) {
        for (gun, hands, clip) in &sources.weapons {
            if !visible.contains_key(gun) {
                continue;
            }
            let (Some(rig), Some(parent), Some(metadata)) = (
                self.saved.rigs.get(gun),
                self.saved.rigs.get(hands),
                self.saved.clips.get(clip),
            ) else {
                if !self.saved.clips.contains_key(clip) {
                    self.saved.missing.insert(clip.clone());
                }
                continue;
            };
            if !rig.names.is_disjoint(&parent.names) {
                continue;
            }
            let Some(chain) = parent_chain(parent) else {
                continue;
            };
            let relevant = rig
                .required
                .iter()
                .any(|name| metadata.names.contains(name))
                || chain
                    .iter()
                    .any(|bone| metadata.names.contains(&key(&bone.name)));
            let input_stamps = parent.stamps.clone();
            if relevant {
                self.admit(gun, clip, "Weapon rig", chain, input_stamps);
            }
        }
    }

    fn prop_options(&mut self, sources: &Sources, visible: &BTreeMap<String, &Entry>) {
        for (model, clip, tree) in &sources.props {
            if !visible.contains_key(model)
                || !sources
                    .trees
                    .get(tree.trim_end_matches(".atr"))
                    .is_some_and(|leaves| leaves.get(clip) == Some(&true))
            {
                continue;
            }
            let (Some(rig), Some(metadata)) =
                (self.saved.rigs.get(model), self.saved.clips.get(clip))
            else {
                continue;
            };
            if metadata.root_keys || !rig.required.is_disjoint(&metadata.names) {
                self.admit(model, clip, "Scripted model", Vec::new(), Vec::new());
            }
        }
    }

    fn family_options(&mut self, sources: &Sources, visible: &BTreeMap<String, &Entry>) {
        for (model, tree) in &sources.dogs {
            if !visible.contains_key(model) {
                continue;
            }
            let (Some(rig), Some(leaves)) = (self.saved.rigs.get(model), sources.trees.get(tree))
            else {
                continue;
            };
            let names: Vec<_> = leaves
                .iter()
                .filter_map(|(clip, allowed)| {
                    let metadata = self.saved.clips.get(clip)?;
                    if *allowed
                        && metadata.supported()
                        && rig
                            .required
                            .iter()
                            .filter(|name| name.as_str() != "tag_origin")
                            .all(|name| metadata.names.contains(name))
                    {
                        Some(clip.clone())
                    } else {
                        None
                    }
                })
                .collect();
            for clip in names {
                self.admit(model, &clip, "Dog rig", Vec::new(), Vec::new());
            }
        }
        let Some(leaves) = sources.trees.get("generic_human") else {
            return;
        };
        for model in &sources.bodies {
            if !visible.contains_key(model) {
                continue;
            }
            let Some(rig) = self.saved.rigs.get(model) else {
                continue;
            };
            if !human_rig(rig) || equipment_weighted(rig) {
                continue;
            }
            *self
                .saved
                .diagnostics
                .entry("source-human-core-models".into())
                .or_default() += 1;
            let names: Vec<_> = leaves
                .iter()
                .filter_map(|(clip, allowed)| {
                    let metadata = self.saved.clips.get(clip)?;
                    if *allowed
                        && metadata.supported()
                        && HUMAN_CORE
                            .iter()
                            .all(|(child, _)| metadata.names.contains(*child))
                        && rig
                            .required
                            .iter()
                            .filter(|name| name.as_str() != "tag_origin")
                            .all(|name| metadata.names.contains(name))
                    {
                        Some(clip.clone())
                    } else {
                        None
                    }
                })
                .collect();
            for clip in names {
                self.admit(model, &clip, "Human body rig", Vec::new(), Vec::new());
            }
        }
    }

    fn validate_pairs(&mut self) {
        let signatures: BTreeMap<_, _> = self
            .saved
            .rigs
            .iter()
            .filter_map(|(name, rig)| {
                let bytes = serde_json::to_vec(&rig.bones).ok()?;
                Some((name.clone(), format!("{:x}", Sha256::digest(bytes))))
            })
            .collect();
        let mut requests: BTreeMap<String, BTreeMap<String, (String, Vec<Bone>)>> = BTreeMap::new();
        for (model, choices) in &self.saved.choices {
            let Some(signature) = signatures.get(model) else {
                continue;
            };
            for (clip, association) in choices {
                requests
                    .entry(clip.clone())
                    .or_default()
                    .entry(binding_signature(signature, association))
                    .or_insert_with(|| (model.clone(), association.parent_bones.clone()));
            }
        }
        let mut outcomes: BTreeMap<String, BTreeMap<String, bool>> = BTreeMap::new();
        let mut tested = 0;
        let mut failed = 0;
        for (name, groups) in requests {
            let decoded = self
                .saved
                .clips
                .get(&name)
                .and_then(|metadata| model::asset_path(&self.root, "xanim", &metadata.name).ok())
                .and_then(|path| animation::read(&path).ok());
            for (signature, (model, parent_bones)) in groups {
                let passed = decoded
                    .as_ref()
                    .zip(self.saved.rigs.get(&model))
                    .is_some_and(|(clip, rig)| finite_poses(clip, rig, &parent_bones));
                tested += 1;
                if !passed {
                    failed += 1;
                }
                outcomes
                    .entry(name.clone())
                    .or_default()
                    .insert(signature, passed);
            }
        }
        for (model, choices) in &mut self.saved.choices {
            let Some(signature) = signatures.get(model) else {
                choices.clear();
                continue;
            };
            choices.retain(|clip, association| {
                outcomes
                    .get(clip)
                    .and_then(|groups| groups.get(&binding_signature(signature, association)))
                    == Some(&true)
            });
        }
        self.saved
            .diagnostics
            .insert("numeric-rig-clip-groups".into(), tested);
        self.saved
            .diagnostics
            .insert("numeric-failed-groups".into(), failed);
    }

    pub fn options(&self, model: &str) -> Selection {
        let entries = self
            .saved
            .choices
            .get(&key(model))
            .into_iter()
            .flat_map(|choices| choices.iter())
            .filter_map(|(clip, association)| {
                self.saved.clips.get(clip).map(|metadata| OptionEntry {
                    name: metadata.name.clone(),
                    context: association.context.clone(),
                })
            })
            .collect();
        Selection {
            entries,
            statistics: self.statistics(Some(model)),
        }
    }

    pub fn playable_models(&self) -> Vec<String> {
        self.saved
            .choices
            .iter()
            .filter_map(|(model, choices)| {
                if choices.is_empty() {
                    None
                } else {
                    Some(model.clone())
                }
            })
            .collect()
    }

    /// Thumbnail rejection remains the static catalogue's responsibility. Its
    /// live visible set may only remove animation choices, never add them.
    pub fn retain_visible(&mut self, entries: &[Entry]) -> Statistics {
        let visible: BTreeSet<_> = entries.iter().map(|entry| key(&entry.name)).collect();
        self.saved.choices.retain(|name, _| visible.contains(name));
        self.recount();
        self.statistics(None)
    }

    pub fn load(&mut self, model: &str, clip: &str) -> Result<Playback, String> {
        let model_id = key(model);
        let clip_id = key(clip);
        let association = self
            .saved
            .choices
            .get(&model_id)
            .and_then(|choices| choices.get(&clip_id))
            .cloned()
            .ok_or("This animation is not available for the selected model.")?;
        if association
            .input_stamps
            .iter()
            .any(|stamp| Stamp::read(&stamp.path).as_ref() != Ok(stamp))
        {
            for choices in self.saved.choices.values_mut() {
                choices.retain(|_, association| {
                    association
                        .input_stamps
                        .iter()
                        .all(|stamp| Stamp::read(&stamp.path).as_ref() == Ok(stamp))
                });
            }
            self.recount();
            return Err(
                "Weapon parent files changed. Click Rescan before playing this clip.".into(),
            );
        }
        let rig = self
            .saved
            .rigs
            .get(&model_id)
            .ok_or("Animation rig is unavailable.")?;
        if rig
            .stamps
            .iter()
            .any(|stamp| Stamp::read(&stamp.path).as_ref() != Ok(stamp))
        {
            self.saved.choices.remove(&model_id);
            self.recount();
            return Err("Model files changed. Click Rescan before playing animations.".into());
        }
        let metadata = self
            .saved
            .clips
            .get(&clip_id)
            .ok_or("Animation file is unavailable.")?
            .clone();
        let path = model::asset_path(&self.root, "xanim", &metadata.name)?;
        if Stamp::read(&path).as_ref() != Ok(&metadata.stamp) {
            self.withdraw(&clip_id);
            return Err("Animation files changed. Click Rescan before playing this clip.".into());
        }
        let clip = if let Some((name, clip)) = &self.loaded {
            if name == &clip_id {
                Ok(clip.clone())
            } else {
                animation::read(&path)
            }
        } else {
            animation::read(&path)
        };
        let clip = match clip {
            Ok(clip) if animation::supports_type(clip.asset_type) => clip,
            Ok(_) => {
                self.withdraw(&clip_id);
                return Err("This animation mode is not supported.".into());
            }
            Err(error) => {
                self.withdraw(&clip_id);
                return Err(error);
            }
        };
        let valid = self
            .saved
            .rigs
            .get(&model_id)
            .is_some_and(|rig| finite_poses(&clip, rig, &association.parent_bones));
        if !valid {
            if let Some(choices) = self.saved.choices.get_mut(&model_id) {
                choices.remove(&clip_id);
            }
            self.recount();
            return Err("The animation pose could not be displayed reliably.".into());
        }
        self.loaded = Some((clip_id, clip.clone()));
        Ok(Playback {
            clip,
            parent_bones: association.parent_bones,
            context: association.context,
        })
    }

    fn withdraw(&mut self, clip: &str) {
        for choices in self.saved.choices.values_mut() {
            choices.remove(clip);
        }
        self.loaded = None;
        self.recount();
    }
}
