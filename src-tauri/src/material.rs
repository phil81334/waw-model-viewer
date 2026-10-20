//! Colour-map references and the native-derived rules needed for a basic preview.
use crate::reader::Reader;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::Path, sync::OnceLock};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Material {
    pub name: String,
    pub image: Option<String>,
    pub alpha: String,
    pub cull: String,
    pub depth_write: bool,
    pub blend_op: String,
    pub source_blend: String,
    pub destination_blend: String,
    pub fallback: bool,
}

fn rules() -> &'static BTreeMap<String, String> {
    static RULES: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    RULES.get_or_init(|| {
        serde_json::from_str(include_str!("material_rules.json"))
            .expect("Invalid built-in material rules")
    })
}

pub fn read(path: &Path, name: &str) -> Result<Material, String> {
    let data = fs::read(path).map_err(|error| error.to_string())?;
    let mut reader = Reader::new(&data);
    reader.skip(40)?;
    let word = reader.u32()?;
    let second_word = reader.u32()?;
    let count = usize::from(reader.u16()?);
    reader.skip(2)?;
    let techset_offset = reader.u32()? as usize;
    let texture_table = reader.u32()? as usize;
    let family = reader.string_at(techset_offset)?;
    let mut image = None;
    for index in 0..count {
        reader.position = texture_table
            .checked_add(index * 12)
            .ok_or("Texture table overflow.")?;
        let slot = reader.u32()? as usize;
        reader.skip(4)?;
        let image_offset = reader.u32()? as usize;
        if reader.string_at(slot)? == "colorMap" {
            image = Some(reader.string_at(image_offset)?);
        }
    }
    let operations = [
        "Disable",
        "Add",
        "Subtract",
        "RevSubtract",
        "Min",
        "Max",
        "Unknown",
        "Unknown",
    ];
    let factors = [
        "Unknown",
        "Zero",
        "One",
        "SrcColor",
        "InvSrcColor",
        "SrcAlpha",
        "InvSrcAlpha",
        "DestAlpha",
        "InvDestAlpha",
        "DestColor",
        "InvDestColor",
        "Unknown",
        "Unknown",
        "Unknown",
        "Unknown",
        "Unknown",
    ];
    let mut result = Material {
        name: name.into(),
        image,
        alpha: if word & 2048 != 0 {
            "Always"
        } else {
            ["Always", "GT0", "LT128", "GE128"][((word >> 12) & 3) as usize]
        }
        .into(),
        cull: ["None", "None", "Back", "Front"][((word >> 14) & 3) as usize].into(),
        depth_write: second_word & 1 != 0,
        blend_op: operations[((word >> 8) & 7) as usize].into(),
        source_blend: factors[(word & 15) as usize].into(),
        destination_blend: factors[((word >> 4) & 15) as usize].into(),
        fallback: false,
    };
    let raw_blend_disabled = result.blend_op == "Disable";
    match rules().get(&family).map(String::as_str) {
        Some("default" | "effect_add" | "premulalpha" | "skybox") => {
            if result.alpha == "Always"
                && result.blend_op == "Add"
                && result.source_blend == "SrcAlpha"
                && matches!(result.destination_blend.as_str(), "InvSrcAlpha" | "One")
            {
                result.alpha = "GT0".into();
            }
            match rules()[&family].as_str() {
                "default" | "effect_add" => {
                    result.depth_write = raw_blend_disabled;
                }
                "premulalpha" => {
                    result.blend_op = "Add".into();
                    result.source_blend = "One".into();
                    result.destination_blend = "InvSrcAlpha".into();
                    if raw_blend_disabled {
                        result.depth_write = true;
                    }
                }
                _ => {}
            }
        }
        Some("stencil_toggle") => {
            // Preserve this pass's ordinary face/depth rules. Its stencil effect
            // is outside a colour-only preview, so retain the fallback label.
            result.cull = "None".into();
            result.depth_write = false;
            result.fallback = true;
        }
        _ => {
            // Objective/stencil and absent shader definitions get an explicit
            // colour-only fallback. Do not pretend to reproduce those shaders.
            result.fallback = true;
        }
    }
    Ok(result)
}

pub fn placeholder(name: &str) -> Material {
    Material {
        name: name.into(),
        image: None,
        alpha: "Always".into(),
        cull: "Back".into(),
        depth_write: true,
        blend_op: "Disable".into(),
        source_blend: "One".into(),
        destination_blend: "Zero".into(),
        fallback: true,
    }
}
