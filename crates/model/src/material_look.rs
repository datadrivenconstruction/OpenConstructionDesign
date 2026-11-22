// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! How a material looks: base colour, procedural texture, tile size, roughness and metallic.
//! One library of material classes resolved from any material text by keywords, so names written
//! by scripts, generators or people ("steel S355", "concrete C40/50", "glulam GL24h") find a look
//! without a row of their own. A row colour overrides the class colour. Also the default material
//! of an element kind and the colour of an engineering service, and the check that every element
//! of a model has a material with a look (`Model::material_gaps`).

use std::collections::HashMap;

use ddc_geom::mep::SystemKind;

use crate::{Error, Model};

/// Look of one material class. `texture` is a key of the built-in procedural textures.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    /// Library key, also the canonical material name when lower-cased words are capitalised.
    pub key: &'static str,
    /// Material name written when this class is chosen as a default.
    pub name: &'static str,
    /// Base colour, sRGB 0xRRGGBB.
    pub rgb: u32,
    pub texture: &'static str,
    /// Metres per texture repeat.
    pub tile_m: f32,
    pub roughness: f32,
    pub metallic: f32,
    /// A finish: shaded styles show its colour, not the category tone.
    pub finish: bool,
}

const fn look(
    key: &'static str,
    name: &'static str,
    rgb: u32,
    texture: &'static str,
    tile_m: f32,
    roughness: f32,
    metallic: f32,
    finish: bool,
) -> Look {
    Look { key, name, rgb, texture, tile_m, roughness, metallic, finish }
}

/// The library. Order matters for resolution: the first class whose words match wins, so the
/// specific classes ("concrete paving", "glass wool", "bitumen roofing") come before the general.
pub const LOOKS: [Look; 34] = [
    look("foliage", "Foliage", 0x54803A, "grass", 0.6, 0.9, 0.0, true),
    look("bark", "Bark", 0x604834, "wood", 0.5, 0.85, 0.0, true),
    look("rubber", "Rubber", 0x2A2B2D, "concrete", 0.5, 0.8, 0.0, true),
    look("insulation", "Insulation", 0xE3D49A, "concrete", 0.4, 1.0, 0.0, true),
    look("paving", "Paving", 0x9E9A92, "tile", 0.6, 0.85, 0.0, true),
    look("roofing", "Roofing", 0x5C5F64, "concrete", 0.8, 0.8, 0.0, true),
    look("roof_tile", "Clay Roof Tile", 0xA3563C, "tile", 0.35, 0.7, 0.0, true),
    look("slate", "Slate", 0x4A4E54, "tile", 0.3, 0.6, 0.0, true),
    look("asphalt", "Asphalt", 0x3E4044, "concrete", 0.5, 0.95, 0.0, true),
    look("paint", "Paint", 0xF2F2EE, "concrete", 4.0, 0.6, 0.0, true),
    look("fabric", "Tensile Fabric", 0xF0EFEA, "concrete", 3.0, 0.7, 0.0, true),
    look("light", "Light Panel", 0xF7F6F0, "glass", 1.0, 0.2, 0.0, true),
    look("polycarbonate", "Polycarbonate", 0xDCE6EA, "glass", 1.0, 0.15, 0.0, false),
    look("glass", "Glass", 0x9DB8C4, "glass", 1.0, 0.05, 0.0, false),
    look("grass", "Grass", 0x5E8C3A, "grass", 1.0, 1.0, 0.0, true),
    look("render", "Render", 0xE6E0D4, "concrete", 2.0, 0.85, 0.0, true),
    look("plaster", "Plaster", 0xEEEBE4, "concrete", 3.0, 0.85, 0.0, true),
    look("cladding", "Cladding Panel", 0x6E737A, "metal", 0.8, 0.45, 0.6, true),
    look("brick", "Brick", 0xA85C46, "brick", 0.6, 0.85, 0.0, true),
    look("masonry", "Masonry", 0xC9C5BB, "brick", 0.8, 0.85, 0.0, false),
    look("ceramic", "Ceramic Tile", 0xD8D4CC, "tile", 0.6, 0.25, 0.0, true),
    look("stone", "Stone", 0xB8B0A0, "concrete", 1.5, 0.75, 0.0, true),
    look("gravel", "Gravel", 0x9A958C, "concrete", 0.3, 1.0, 0.0, true),
    look("soil", "Soil", 0x7A5C3E, "concrete", 0.6, 1.0, 0.0, true),
    look("seating", "Seating", 0x2F5C9E, "concrete", 1.0, 0.5, 0.0, true),
    look("plastic", "Plastic", 0xD0D0CC, "concrete", 1.0, 0.45, 0.0, true),
    look("timber", "Timber", 0xB07A45, "wood", 0.8, 0.6, 0.0, true),
    // after timber: "laminate" is a fragment of "cross-laminated timber"
    look("floor", "Vinyl Floor", 0x6A6E78, "concrete", 1.0, 0.7, 0.0, true),
    look("copper", "Copper", 0xB87333, "metal", 0.5, 0.3, 1.0, true),
    look("aluminium", "Aluminium", 0xC4C8CC, "metal", 0.5, 0.3, 1.0, false),
    look("iron", "Cast Iron", 0x4A4C50, "metal", 0.5, 0.55, 1.0, false),
    look("steel", "Steel", 0x8A9097, "metal", 0.5, 0.35, 1.0, false),
    look("concrete", "Concrete", 0xA9A69F, "concrete", 1.0, 0.9, 0.0, false),
    look("water", "Water", 0x3D6E8C, "glass", 2.0, 0.03, 0.0, true),
];

/// Words of each class (lower case). A word matches a whole word of the name or its plural; a
/// word of 6 letters or more, or one with a space, also matches inside the name.
fn words(key: &str) -> &'static [&'static str] {
    match key {
        "foliage" => &["foliage", "leaf", "leaves", "hedge", "shrub", "tree crown", "canopy green"],
        "bark" => &["bark", "trunk"],
        "rubber" => &["rubber", "elastomer", "epdm", "neoprene"],
        "insulation" => &["insulation", "wool", "xps", "eps", "foam", "pir"],
        "paving" => &["paving", "paver", "pavement", "flagstone", "cobble", "sett"],
        "roofing" => &["roofing", "membrane", "bitumen", "felt", "waterproof"],
        "roof_tile" => &["roof tile", "clay tile", "pantile"],
        "slate" => &["slate", "shingle"],
        "asphalt" => &["asphalt", "tarmac", "road surface", "macadam"],
        "paint" => &["paint", "marking", "coating"],
        "fabric" => &["ptfe", "etfe", "fabric", "textile", "canvas", "tensile"],
        "light" => &["led", "lamp", "luminaire", "light panel", "lighting"],
        "water" => &["water", "pool", "pond"],
        "polycarbonate" => &["polycarbonate", "acrylic", "translucent"],
        "glass" => &["glass", "glazing", "glazed", "pane"],
        "grass" => &["grass", "lawn", "turf", "pitch", "vegetat", "green roof"],
        "gravel" => &["gravel", "ballast", "pebble", "aggregate", "shingle bed"],
        "soil" => &["soil", "topsoil", "earth", "ground", "sand", "clay", "fill", "terrain"],
        "stone" => &["stone", "granite", "limestone", "marble", "sandstone", "basalt"],
        "render" => &["render", "stucco", "facade finish"],
        "plaster" => &["plaster", "gypsum", "drywall", "plasterboard", "board"],
        "cladding" => &["cladding", "sandwich", "profiled", "louvre", "panel", "composite", "sheet"],
        "brick" => &["brick", "clinker"],
        "masonry" => &["masonry", "block", "aerated", "silicate", "cmu"],
        "ceramic" => &["ceramic", "porcelain", "tile"],
        "floor" => &["carpet", "vinyl", "linoleum", "laminate", "screed finish"],
        "seating" => &["seat", "seating", "bench", "chair"],
        "plastic" => &["plastic", "polyethylene", "polypropylene", "hdpe", "pvc", "upvc", "frp", "grp"],
        "timber" => &[
            "timber", "wood", "glulam", "clt", "lvl", "osb", "oak", "pine", "spruce", "larch",
            "birch", "beech", "cedar", "plywood", "decking", "laminated veneer",
        ],
        "copper" => &["copper", "brass", "bronze", "zinc"],
        "aluminium" => &["aluminium", "aluminum", "alu", "anodised", "anodized"],
        "iron" => &["iron", "ductile"],
        "steel" => &[
            "steel", "metal", "strand", "cable", "rebar", "grating", "galvanis", "galvaniz", "tube",
            "pipe", "chs", "rhs", "shs", "bolt", "plate", "rod", "wire", "finned",
        ],
        "concrete" => &["concrete", "beton", "cement", "screed", "precast", "reinforced", "grout", "mortar"],
        _ => &[],
    }
}

/// Paint colour words: a name that carries one takes that colour (painted metal is not metallic).
const COLOUR_WORDS: [(&str, u32); 12] = [
    ("white", 0xF0F0EC),
    ("black", 0x262729),
    ("red", 0xB0302A),
    ("yellow", 0xE3B52A),
    ("orange", 0xD9702A),
    ("green", 0x3F7A45),
    ("blue", 0x2F5C9E),
    ("grey", 0x8C8F92),
    ("gray", 0x8C8F92),
    ("anthracite", 0x3C3F44),
    ("silver", 0xC0C4C8),
    ("beige", 0xD8CCB0),
];

fn tokens(s: &str) -> impl Iterator<Item = &str> {
    s.split(|c: char| !c.is_ascii_alphanumeric()).filter(|t| !t.is_empty())
}

fn has_word(name: &str, w: &str) -> bool {
    if (w.len() >= 6 || w.contains(' ')) && name.contains(w) {
        return true;
    }
    tokens(name).any(|t| t == w || t.strip_suffix('s') == Some(w))
}

/// Strength grades written without a class word: S355 and S235GD steel, C30/37 concrete, GL24h
/// and C24 timber.
fn grade(name: &str, raw: &str) -> Option<&'static str> {
    for t in tokens(name) {
        let b = t.as_bytes();
        let digits = |from: usize| b.len() > from && b[from].is_ascii_digit();
        if b[0] == b's' && b.len() >= 4 && b[1..4].iter().all(u8::is_ascii_digit) {
            return Some("steel");
        }
        if t.starts_with("gl") && digits(2) {
            return Some("timber");
        }
        if b[0] == b'c' && digits(1) && b.len() <= 4 {
            // C30/37 has a slash after the grade; C24 alone is a timber class
            let after = raw.to_lowercase();
            let slash = after.find(t).is_some_and(|i| after[i + t.len()..].starts_with('/'));
            return Some(if slash { "concrete" } else { "timber" });
        }
    }
    None
}

/// Library class of a material key.
pub fn look_by_key(key: &str) -> Option<&'static Look> {
    LOOKS.iter().find(|l| l.key == key)
}

/// Library class a material text names, by its words and grades; None for a text that names no
/// known class.
pub fn look_of(name: &str) -> Option<&'static Look> {
    let n = name.trim().to_lowercase();
    if n.is_empty() {
        return None;
    }
    LOOKS
        .iter()
        .find(|l| words(l.key).iter().any(|w| has_word(&n, w)))
        .or_else(|| grade(&n, name).and_then(look_by_key))
}

/// Colour word in a material text, if any.
fn paint_colour(name: &str) -> Option<u32> {
    let n = name.to_lowercase();
    COLOUR_WORDS.iter().find(|(w, _)| tokens(&n).any(|t| t == *w)).map(|c| c.1)
}

/// What a face of this material shows: the class with the colour a row or the name gives.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Resolved {
    pub look: &'static Look,
    pub rgb: u32,
    pub metallic: f32,
    pub roughness: f32,
}

/// Look of a material text with an optional row colour (`MaterialSpec::color`); None when the
/// text names no class and no colour is set.
pub fn resolve(name: &str, row_color: Option<u32>) -> Option<Resolved> {
    let paint = paint_colour(name);
    let look = look_of(name).or_else(|| (row_color.or(paint).is_some()).then_some(look_by_key("paint")?))?;
    let (mut metallic, mut roughness) = (look.metallic, look.roughness);
    let rgb = match (row_color, paint) {
        (Some(c), _) => c,
        (None, Some(c)) => {
            // a painted metal is a coat of paint
            if metallic > 0.0 {
                metallic = 0.0;
                roughness = roughness.max(0.5);
            }
            c
        }
        (None, None) => look.rgb,
    };
    Some(Resolved { look, rgb, metallic, roughness })
}

/// Material name a new element of `kind` gets when none is given: the class its name hints at
/// (`hint`, e.g. the part name "Seat row"), otherwise the kind's own default.
pub fn default_material(kind: &str, hint: &str) -> &'static str {
    if let Some(l) = look_of(hint) {
        return l.name;
    }
    let key = match kind {
        "column" | "col" | "beam" | "brace" | "member" | "truss" | "gridshell" | "railing" | "plate" => "steel",
        "roof" => "roofing",
        "curtain" | "window" | "glazing" => "glass",
        "mullion" | "frame" => "aluminium",
        "panel" | "panel_system" => "cladding",
        "topo" | "toposolid" | "topo_pad" | "site" | "ground" => "soil",
        "duct" => "steel",
        // wall and slab layer functions (`LayerFn::key`)
        "substrate" => "masonry",
        "insulation" | "air" => "insulation",
        "membrane" => "rubber",
        "finish" | "finish1" | "finish2" => "plaster",
        _ => "concrete",
    };
    look_by_key(key).map_or("Concrete", |l| l.name)
}

/// The material an element row is written with: the given text when there is one, otherwise
/// [`default_material`] of its kind or layer function. The creating ops of typed elements call
/// this and lowered parts call [`default_material`], so generators, scripts and the assistant all
/// land on the same rule.
pub fn assign(kind: &str, given: &str, hint: &str) -> String {
    let g = given.trim();
    if g.is_empty() {
        default_material(kind, hint).to_string()
    } else {
        g.to_string()
    }
}

/// A colour without hue (0xRRGGBB): it reads as the default grey, whatever its lightness.
pub fn is_neutral(rgb: u32) -> bool {
    let c = [(rgb >> 16) & 0xFF, (rgb >> 8) & 0xFF, rgb & 0xFF];
    let (hi, lo) = (c.iter().max().copied().unwrap_or(0), c.iter().min().copied().unwrap_or(0));
    hi - lo < 16
}

/// Fuel gas: the model's system kinds have none, so a system named for gas takes this colour.
pub const GAS_RGB: [u8; 3] = [0xF5, 0xE6, 0x4B];

/// Whether a system or line name is a fuel gas service ("NG-101", "Gas supply").
pub fn is_gas_service(name: &str) -> bool {
    let n = name.to_lowercase();
    (n.contains("gas") && !n.contains("medical")) || tokens(&n).any(|t| t == "ng" || t == "lpg" || t == "fuel")
}

/// Colour of an engineering service: fuel gas by name, otherwise the system kind's colour.
pub fn service_rgb(name: &str, kind: SystemKind) -> [u8; 3] {
    if is_gas_service(name) && kind != SystemKind::MedicalGas {
        GAS_RGB
    } else {
        kind.color()
    }
}

/// Element tables with a material, how to read it (id, material) and the kind for defaults. Wall,
/// slab, ceiling and roof rows give one row per type layer. Curtain walls, openings and
/// components are outside: their glazing, frames and family parts carry their own colours.
pub const CHECKED: [(&str, &str); 21] = [
    ("wall", "SELECT e.id, l.material FROM wall e JOIN wall_type_layer l ON l.wtype=e.wtype"),
    ("slab", "SELECT e.id, l.material FROM slab e JOIN slab_type_layer l ON l.stype=e.stype"),
    ("ceiling", "SELECT e.id, l.material FROM ceiling e JOIN slab_type_layer l ON l.stype=e.stype"),
    ("roof", "SELECT e.id, l.material FROM roof e JOIN slab_type_layer l ON l.stype=e.stype"),
    ("col", "SELECT e.id, t.material FROM col e JOIN member_type t ON t.id=e.mtype"),
    ("beam", "SELECT e.id, t.material FROM beam e JOIN member_type t ON t.id=e.mtype"),
    ("brace", "SELECT e.id, t.material FROM brace e JOIN member_type t ON t.id=e.mtype"),
    ("beam_system", "SELECT e.id, t.material FROM beam_system e JOIN member_type t ON t.id=e.mtype"),
    ("gridshell", "SELECT e.id, t.material FROM gridshell e JOIN member_type t ON t.id=e.mtype"),
    ("truss", "SELECT e.id, t.material FROM truss e JOIN member_type t ON t.id=e.chord_type"),
    ("stair", "SELECT id, material FROM stair"),
    ("railing", "SELECT id, material FROM railing"),
    ("ramp", "SELECT id, material FROM ramp"),
    ("solid", "SELECT id, material FROM solid WHERE void=0"),
    ("freeform", "SELECT id, material FROM freeform"),
    ("panel_system", "SELECT id, material FROM panel_system"),
    ("iso_footing", "SELECT id, material FROM iso_footing"),
    ("strip_footing", "SELECT id, material FROM strip_footing"),
    ("raft", "SELECT id, material FROM raft"),
    ("toposolid", "SELECT id, material FROM toposolid"),
    ("topo_pad", "SELECT id, material FROM topo_pad"),
];

/// An element whose material is empty or names no look: it would draw in the neutral tone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaterialGap {
    pub table: &'static str,
    pub id: i64,
    pub material: String,
}

impl Model {
    /// Look of a material: its row colour (or the colour its name implies) over the library
    /// class. None when neither names a look.
    pub fn material_look(&self, name: &str) -> Result<Option<Resolved>, Error> {
        if name.trim().is_empty() {
            return Ok(None);
        }
        Ok(resolve(name, self.material(name)?.spec.color))
    }

    /// Every (table, id, material) of [`CHECKED`], empty materials included. Tables this file
    /// does not have are skipped.
    pub fn material_uses(&self) -> Result<Vec<(&'static str, i64, String)>, Error> {
        let mut out = vec![];
        for (table, sql) in CHECKED {
            let exists: bool = self.db.query_row(
                "SELECT count(*) > 0 FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |r| r.get(0),
            )?;
            if !exists {
                continue;
            }
            let mut st = self.db.prepare(sql)?;
            let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?)))?;
            for row in rows {
                let (id, m) = row?;
                out.push((table, id, m.unwrap_or_default()));
            }
        }
        Ok(out)
    }

    /// Elements that would draw without a material look: an empty material or a name that
    /// names no class and has no colour. Engineering runs are listed (table `mep_run`, the system
    /// name as material) when their system is missing or its colour has no hue.
    pub fn material_gaps(&self) -> Result<Vec<MaterialGap>, Error> {
        let mut seen: HashMap<String, bool> = HashMap::new();
        let mut out = vec![];
        for (table, id, material) in self.material_uses()? {
            let ok = match seen.get(&material) {
                Some(&ok) => ok,
                None => {
                    let ok = self.material_look(&material)?.is_some();
                    seen.insert(material.clone(), ok);
                    ok
                }
            };
            if !ok {
                out.push(MaterialGap { table, id, material });
            }
        }
        // engineering runs draw in their system colour: a missing system or a neutral colour is grey
        let mut st = self.db.prepare(
            "SELECT r.id, COALESCE(s.name,''), s.color FROM mep_run r LEFT JOIN mep_system s ON s.id=r.system \
             ORDER BY r.id",
        )?;
        let runs = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<i64>>(2)?)))?;
        for row in runs {
            let (id, system, color) = row?;
            let grey = match color {
                Some(c) => is_neutral(c as u32),
                None => true,
            };
            if grey {
                out.push(MaterialGap { table: "mep_run", id, material: system });
            }
        }
        Ok(out)
    }
}
