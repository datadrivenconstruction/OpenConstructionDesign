// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Project materials (schema 35): the `material` entity — name, color, hatch, density, thermal
//! properties. Element rows reference a material by name (the `material` column): names are unique,
//! renames cascade, deleting a material in use is refused, replacing one moves the references.
//! This keeps existing files, import and quantities working unchanged: migration creates an
//! entity for every text it meets, and the text in rows stays as is.
//! Text written after the file was opened shows up in the list as a material with `id == 0`
//! (default properties) and becomes a row on the first edit or the next open.

use std::collections::BTreeSet;

use rusqlite::{params, types::Value, Connection, OptionalExtension};

use crate::ops::Inv;
use crate::{Error, FieldErr, Model};

const DDL: &str = "
CREATE TABLE IF NOT EXISTS material(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, color INTEGER,
    hatch TEXT NOT NULL DEFAULT 'none', density REAL, conductivity REAL, specific_heat REAL,
    note TEXT NOT NULL DEFAULT '');
";

/// Hatch patterns: keys are stored in the file; drawing is the view's concern.
pub const MATERIAL_HATCHES: [&str; 5] = ["none", "diagonal", "back_diagonal", "cross", "dots"];

/// Prefix of a hatch key that names an entry of the project hatch library.
pub const LIB_HATCH_PREFIX: &str = "lib:";

/// A `lib:<name>` hatch key: a library entry, drawn on sheets and plans from the library.
pub fn is_lib_hatch(key: &str) -> bool {
    key.strip_prefix(LIB_HATCH_PREFIX).is_some_and(|n| {
        !n.trim().is_empty() && n.chars().count() <= 40 && !n.chars().any(char::is_control)
    })
}

/// Built-in materials of a new project (UI labels are `mat.<id>`).
pub const BUILTIN_MATERIALS: [&str; 12] = [
    "concrete",
    "masonry",
    "brick",
    "insulation",
    "plaster",
    "render",
    "gypsum",
    "glass",
    "wood",
    "steel",
    "membrane",
    "other",
];

#[derive(Clone, Debug, PartialEq)]
pub struct MaterialSpec {
    pub name: String,
    /// 0xRRGGBB; None means the theme color of the material class.
    pub color: Option<u32>,
    /// Key from [`MATERIAL_HATCHES`].
    pub hatch: String,
    /// Density, kg/m³.
    pub density: Option<f64>,
    /// Thermal conductivity, W/(m·K).
    pub conductivity: Option<f64>,
    /// Specific heat capacity, J/(kg·K).
    pub specific_heat: Option<f64>,
    pub note: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Material {
    /// 0 means the material exists only as text in elements (default properties).
    pub id: i64,
    pub spec: MaterialSpec,
}

/// Reference values of the material class by name: (hatch, density, λ, c).
fn class_values(name: &str) -> Option<(&'static str, f64, f64, f64)> {
    let m = name.to_lowercase();
    let has = |k: &[&str]| k.iter().any(|k| m.contains(k));
    if has(&["concrete", "\u{431}\u{435}\u{442}\u{43e}\u{43d}"]) {
        Some(("dots", 2400.0, 1.75, 840.0))
    } else if has(&["masonry", "brick", "\u{43a}\u{438}\u{440}\u{43f}\u{438}\u{447}", "\u{43a}\u{43b}\u{430}\u{434}\u{43a}"]) {
        Some(("diagonal", 1800.0, 0.81, 880.0))
    } else if has(&["insul", "\u{443}\u{442}\u{435}\u{43f}\u{43b}", "wool", "\u{432}\u{430}\u{442}\u{430}"]) {
        Some(("cross", 40.0, 0.04, 1030.0))
    } else if has(&["glass", "\u{441}\u{442}\u{435}\u{43a}\u{43b}"]) {
        Some(("none", 2500.0, 1.0, 750.0))
    } else if has(&["glulam"]) {
        Some(("back_diagonal", 470.0, 0.13, 1600.0))
    } else if has(&["cross-laminated", "cross laminated"]) {
        Some(("back_diagonal", 500.0, 0.13, 1600.0))
    } else if has(&["door", "wood", "timber", "\u{434}\u{435}\u{440}\u{435}\u{432}"]) {
        Some(("back_diagonal", 500.0, 0.13, 1600.0))
    } else if has(&["steel", "\u{441}\u{442}\u{430}\u{43b}"]) {
        Some(("none", 7850.0, 50.0, 460.0))
    } else if has(&["gypsum", "\u{433}\u{438}\u{43f}\u{441}", "\u{433}\u{43a}\u{43b}"]) {
        Some(("none", 850.0, 0.21, 1090.0))
    } else if has(&["plaster", "render", "\u{448}\u{442}\u{443}\u{43a}\u{430}\u{442}\u{443}\u{440}"]) {
        Some(("none", 1600.0, 0.7, 840.0))
    } else if has(&["membrane", "\u{43c}\u{435}\u{43c}\u{431}\u{440}\u{430}\u{43d}"]) {
        Some(("none", 950.0, 0.2, 1800.0))
    } else {
        None
    }
}

/// Hatch of a material text without a row: the same its row gets when it is created.
pub fn material_default_hatch(name: &str) -> &'static str {
    class_values(name).map_or("none", |c| c.0)
}

/// Colours that tell the timber of a building apart: golden glulam, pale CLT, stud darker.
fn timber_color(name: &str) -> Option<u32> {
    let n = name.to_lowercase();
    if n.contains("glulam") {
        Some(0xB0_7A_45)
    } else if n.contains("cross-laminated") || n.contains("cross laminated") {
        Some(0xD9_BC_8A)
    } else if n.contains("timber stud") {
        Some(0xC9_A0_6A)
    } else {
        None
    }
}

/// Colour of the planting materials the library tree families name; others follow the theme.
fn plant_color(name: &str) -> Option<u32> {
    Some(match name {
        "Foliage Green" => 0x54_80_3A,
        "Foliage Conifer" => 0x2C_56_3C,
        "Foliage Autumn" => 0xBA_68_26,
        "Foliage Blossom" => 0xE2_AA_BA,
        "Foliage Red" => 0xA6_40_2C,
        "Foliage Gold" => 0xD9_B0_3A,
        "Foliage Spring" => 0x8C_BA_4E,
        "Foliage White" => 0xEE_E9_DC,
        "Foliage Olive" => 0x7C_8D_60,
        "Bark Brown" => 0x60_48_34,
        "Bark Birch" => 0xE2_DE_D2,
        "Bark Grey" => 0x85_80_78,
        "Bark Dark" => 0x3C_30_28,
        "Bark Cherry" => 0x6E_3A_30,
        "Bark Red" => 0x7C_4C_38,
        "Bark Palm" => 0x9A_84_60,
        "Foliage Shade" => 0x3A_5C_2A,
        "Foliage Light" => 0x76_A2_4C,
        "Foliage Conifer Light" => 0x3E_72_52,
        "Foliage Autumn Light" => 0xDC_86_3C,
        "Foliage Blossom Light" => 0xF2_C8_D2,
        "Foliage Red Light" => 0xC8_5C_44,
        "Foliage Gold Light" => 0xEE_CC_5E,
        "Foliage Spring Light" => 0xA8_D2_6A,
        "Foliage White Light" => 0xF8_F4_EA,
        "Foliage Olive Light" => 0x9C_AE_80,
        "Bark Brown Streak" => 0x3E_2E_22,
        "Bark Birch Streak" => 0x2A_26_22,
        "Bark Grey Streak" => 0x58_54_4E,
        "Bark Dark Streak" => 0x22_1C_18,
        "Bark Cherry Streak" => 0x44_24_20,
        "Bark Red Streak" => 0x52_30_24,
        "Bark Palm Streak" => 0x6C_5C_40,
        "Foliage Conifer Shade" => 0x1E_3E_2A,
        "Foliage Autumn Shade" => 0x8A_4A_1C,
        "Foliage Blossom Shade" => 0xB8_7A_8A,
        "Foliage Red Shade" => 0x78_2C_1E,
        "Foliage Gold Shade" => 0xA0_7C_28,
        "Foliage Spring Shade" => 0x5E_8A_38,
        "Foliage White Shade" => 0xB8_B2_A2,
        "Foliage Olive Shade" => 0x58_66_44,
        "Lawn Grass" => 0x6C_9A_46,
        // ungrouped: a trailing "_32" reads as an i32 suffix
        "Planting Soil" => 0x5A4432,
        _ => return None,
    })
}

/// Properties of a new material by name: class reference values, color from the theme.
pub fn material_defaults(name: &str) -> MaterialSpec {
    let (hatch, density, conductivity, specific_heat) = match class_values(name) {
        Some((h, d, c, s)) => (h, Some(d), Some(c), Some(s)),
        None => ("none", None, None, None),
    };
    MaterialSpec {
        name: name.to_string(),
        color: plant_color(name).or_else(|| timber_color(name)),
        hatch: hatch.into(),
        density,
        conductivity,
        specific_heat,
        note: String::new(),
    }
}

/// Tables with a text `material` column (strip merge order does not matter: the list comes from
/// the file itself). `wall_region.material` is a layer number, not a name, so it is excluded.
fn material_tables(db: &Connection, with_derived: bool) -> Result<Vec<String>, Error> {
    let mut st = db.prepare(
        "SELECT m.name FROM sqlite_master m, pragma_table_info(m.name) p \
         WHERE m.type='table' AND p.name='material' AND upper(p.type)='TEXT' AND m.name<>'material' \
         AND m.name NOT LIKE 'sqlite_%' AND m.name NOT LIKE 'src_%' ORDER BY m.name",
    )?;
    let v: Vec<String> = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
    Ok(v.into_iter()
        .filter(|t| with_derived || !t.ends_with("_qty"))
        .collect())
}

/// Non-empty material texts in element and type rows.
fn used_names(db: &Connection) -> Result<BTreeSet<String>, Error> {
    let mut out = BTreeSet::new();
    for t in material_tables(db, false)? {
        let mut st = db.prepare(&format!(
            "SELECT DISTINCT material FROM {t} WHERE typeof(material)='text'"
        ))?;
        for n in st.query_map([], |r| r.get::<_, String>(0))? {
            let n = n?;
            // str::trim, not SQLite trim(): the latter does not treat NBSP as whitespace
            if !n.trim().is_empty() {
                out.insert(n);
            }
        }
    }
    Ok(out)
}

pub(crate) fn insert_spec_ignore(db: &Connection, s: &MaterialSpec) -> Result<(), Error> {
    db.execute(
        "INSERT OR IGNORE INTO material(name,color,hatch,density,conductivity,specific_heat,note) \
         VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            s.name,
            s.color.map(i64::from),
            s.hatch,
            s.density,
            s.conductivity,
            s.specific_heat,
            s.note
        ],
    )?;
    Ok(())
}

/// Additive; texts in element rows never change. The builtin set only when the table is created
/// (one the user deleted does not come back).
/// Rows for texts in use are added only when the table is created or `upgrading` an older file:
/// the model reads a text without a row with defaults, so a current file opens with no writes
/// (saved bytes stay the same, nothing outside the journal survives undo).
pub(crate) fn migrate(db: &Connection, upgrading: bool) -> Result<(), Error> {
    let existed: i64 = db.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='material'",
        [],
        |r| r.get(0),
    )?;
    db.execute_batch(DDL)?;
    crate::material_texture::migrate(db)?;
    if existed == 0 {
        for b in BUILTIN_MATERIALS {
            insert_spec_ignore(db, &material_defaults(b))?;
        }
    }
    // a plan's work copy must stay row for row what was copied, or its diff shows rows nobody made
    if (existed == 0 || upgrading) && !COPY_OPEN.with(std::cell::Cell::get) {
        for n in used_names(db)? {
            insert_spec_ignore(db, &material_defaults(&n))?;
        }
    }
    Ok(())
}

thread_local! {
    /// Set while a verified copy is opened: no rows are added from the texts in use.
    pub(crate) static COPY_OPEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The name is validated only when it is being set: imported text longer than 120 characters or
/// with surrounding whitespace stays editable.
fn check_name(s: &MaterialSpec) -> Result<(), Error> {
    let n = s.name.trim();
    if n.is_empty() || n != s.name || n.chars().count() > 120 {
        return Err(FieldErr::MaterialName.into());
    }
    Ok(())
}

fn check_props(s: &MaterialSpec) -> Result<(), Error> {
    if s.color.is_some_and(|c| c > 0xFF_FFFF) {
        return Err(FieldErr::MaterialColor.into());
    }
    if !MATERIAL_HATCHES.contains(&s.hatch.as_str()) && !is_lib_hatch(&s.hatch) {
        return Err(FieldErr::MaterialHatch(s.hatch.clone()).into());
    }
    for v in [s.density, s.conductivity, s.specific_heat]
        .into_iter()
        .flatten()
    {
        if !(v.is_finite() && v >= 0.0) {
            return Err(FieldErr::MaterialValue.into());
        }
    }
    Ok(())
}

pub(crate) fn row_id(db: &Connection, name: &str) -> Result<Option<i64>, Error> {
    Ok(db
        .prepare_cached("SELECT id FROM material WHERE name=?1")?
        .query_row([name], |r| r.get(0))
        .optional()?)
}

fn read_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Material> {
    Ok(Material {
        id: r.get(0)?,
        spec: MaterialSpec {
            name: r.get(1)?,
            color: r.get::<_, Option<i64>>(2)?.map(|c| c as u32),
            hatch: r.get(3)?,
            density: r.get(4)?,
            conductivity: r.get(5)?,
            specific_heat: r.get(6)?,
            note: r.get(7)?,
        },
    })
}

const SELECT: &str =
    "SELECT id,name,color,hatch,density,conductivity,specific_heat,note FROM material";

/// Rows referencing a material: (table, count); derived quantities are not counted.
pub(crate) fn usage(db: &Connection, name: &str) -> Result<Vec<(String, usize)>, Error> {
    let mut out = vec![];
    for t in material_tables(db, false)? {
        let n: i64 = db.query_row(
            &format!("SELECT count(*) FROM {t} WHERE material=?1"),
            [name],
            |r| r.get(0),
        )?;
        if n > 0 {
            out.push((t, n as usize));
        }
    }
    Ok(out)
}

/// Replaces the text in all tables; inverse commands restore only the affected rows.
fn repoint(db: &Connection, from: &str, to: &str) -> Result<Inv, Error> {
    let mut inv = Inv::default();
    for t in material_tables(db, true)? {
        let ids: Vec<i64> = {
            let mut st = db.prepare(&format!("SELECT rowid FROM {t} WHERE material=?1"))?;
            let v = st
                .query_map([from], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            v
        };
        if ids.is_empty() {
            continue;
        }
        let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        inv = inv.sql(
            &format!("UPDATE {t} SET material=?1 WHERE rowid IN ({list})"),
            vec![Value::Text(from.to_string())],
        );
        db.execute(
            &format!("UPDATE {t} SET material=?1 WHERE material=?2"),
            [to, from],
        )?;
    }
    Ok(inv)
}

/// Body of `Model::set_material` inside a running operation (also used by the XLSX batch):
/// writes the properties of `name`, renames every reference when `s.name` differs.
pub(crate) fn write_material(
    db: &Connection,
    name: &str,
    s: &MaterialSpec,
) -> Result<(i64, Inv), Error> {
    if s.name != name {
        check_name(s)?;
    }
    check_props(s)?;
    let renamed = s.name != name;
    if renamed && (row_id(db, &s.name)?.is_some() || !usage(db, &s.name)?.is_empty()) {
        return Err(Error::Conflict(format!(
            "material '{}' already exists",
            s.name
        )));
    }
    let (id, mut inv) = match row_id(db, name)? {
        Some(id) => {
            let inv = Inv::default().restore(db, "material", "id", id)?;
            db.execute(
                "UPDATE material SET name=?2,color=?3,hatch=?4,density=?5,conductivity=?6,\
                 specific_heat=?7,note=?8 WHERE id=?1",
                params![
                    id,
                    s.name,
                    s.color.map(i64::from),
                    s.hatch,
                    s.density,
                    s.conductivity,
                    s.specific_heat,
                    s.note
                ],
            )?;
            (id, inv)
        }
        None => {
            insert_spec_ignore(db, s)?;
            let id = db.last_insert_rowid();
            (id, Inv::default().delete("material", "id", id))
        }
    };
    if renamed {
        inv = inv.then(repoint(db, name, &s.name)?);
    }
    Ok((id, inv))
}

impl Model {
    /// Only rows of the materials table, without scanning element tables (for view drawing).
    pub fn material_rows(&self) -> Result<Vec<Material>, Error> {
        let mut st = self.db.prepare_cached(SELECT)?;
        let v = st.query_map([], read_row)?.collect::<Result<_, _>>()?;
        Ok(v)
    }

    /// Project materials by name; texts without a row get `id == 0` and default properties.
    pub fn materials(&self) -> Result<Vec<Material>, Error> {
        let mut st = self.db.prepare(&format!("{SELECT} ORDER BY name"))?;
        let mut out: Vec<Material> = st.query_map([], read_row)?.collect::<Result<_, _>>()?;
        for n in used_names(&self.db)? {
            if !out.iter().any(|m| m.spec.name == n) {
                out.push(Material {
                    id: 0,
                    spec: material_defaults(&n),
                });
            }
        }
        out.sort_by(|a, b| a.spec.name.cmp(&b.spec.name));
        Ok(out)
    }

    /// Material by name (a text without a row gets default properties).
    pub fn material(&self, name: &str) -> Result<Material, Error> {
        let row = self
            .db
            .prepare_cached(&format!("{SELECT} WHERE name=?1"))?
            .query_row([name], read_row)
            .optional()?;
        Ok(row.unwrap_or_else(|| Material {
            id: 0,
            spec: material_defaults(name),
        }))
    }

    /// Where the material is used: (table, row count).
    pub fn material_usage(&self, name: &str) -> Result<Vec<(String, usize)>, Error> {
        usage(&self.db, name)
    }

    /// Creates a material; an error if the name is taken.
    pub fn add_material(&mut self, s: &MaterialSpec) -> Result<i64, Error> {
        check_name(s)?;
        check_props(s)?;
        let s = s.clone();
        self.op("material_add", move |db| {
            if row_id(db, &s.name)?.is_some() {
                return Err(Error::Conflict(format!(
                    "material '{}' already exists",
                    s.name
                )));
            }
            insert_spec_ignore(db, &s)?;
            let id = db.last_insert_rowid();
            Ok((id, id, Inv::default().delete("material", "id", id)))
        })
    }

    /// Sets properties of material `name` (a row is created if it was only text); a new name in
    /// `s` renames all references in the same operation. Returns the row id.
    pub fn set_material(&mut self, name: &str, s: &MaterialSpec) -> Result<i64, Error> {
        if s.name != name {
            check_name(s)?;
        }
        check_props(s)?;
        let (name, s) = (name.to_string(), s.clone());
        self.op("material_edit", move |db| {
            let (id, inv) = write_material(db, &name, &s)?;
            Ok((id, id, inv))
        })
    }

    /// Deletes an unused material; a used one is an error naming the tables.
    pub fn delete_material(&mut self, name: &str) -> Result<(), Error> {
        let name = name.to_string();
        self.op("material_delete", move |db| {
            let used = usage(db, &name)?;
            if !used.is_empty() {
                let by: Vec<String> = used.iter().map(|(t, n)| format!("{t}: {n}")).collect();
                return Err(Error::Conflict(format!(
                    "material '{name}' is in use ({})",
                    by.join(", ")
                )));
            }
            let Some(id) = row_id(db, &name)? else {
                return Err(Error::Invalid(format!("no material '{name}'")));
            };
            let inv = Inv::default().restore(db, "material", "id", id)?;
            db.execute("DELETE FROM material WHERE id=?1", [id])?;
            Ok(((), id, inv))
        })
    }

    /// Moves all references from `from` to `to` and deletes `from` in one operation.
    pub fn replace_material(&mut self, from: &str, to: &str) -> Result<usize, Error> {
        let (from, to) = (from.to_string(), to.to_string());
        if from == to {
            return Err(Error::Invalid("replacement is the same material".into()));
        }
        self.op("material_replace", move |db| {
            let n: usize = usage(db, &from)?.iter().map(|u| u.1).sum();
            let mut inv = Inv::default();
            let to_id = match row_id(db, &to)? {
                Some(id) => id,
                None => {
                    insert_spec_ignore(db, &material_defaults(&to))?;
                    let id = db.last_insert_rowid();
                    inv = inv.delete("material", "id", id);
                    id
                }
            };
            inv = inv.then(repoint(db, &from, &to)?);
            if let Some(id) = row_id(db, &from)? {
                inv = inv.restore(db, "material", "id", id)?;
                db.execute("DELETE FROM material WHERE id=?1", [id])?;
            }
            Ok((n, to_id, inv))
        })
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn glulam_clt_and_studs_get_distinct_colours_and_the_timber_density() {
        let c = |n: &str| material_defaults(n);
        let (g, l, t) = (c("Glulam Timber"), c("Cross-Laminated Timber"), c("Timber Stud"));
        let cols = [g.color, l.color, t.color];
        assert!(cols.iter().all(Option::is_some) && cols[0] != cols[1] && cols[1] != cols[2] && cols[0] != cols[2], "{cols:?}");
        assert!(g.density.unwrap() > 400.0 && l.density.unwrap() > 400.0);
        assert_eq!(c("Timber Cladding").color, None);
    }
    use super::*;
    use crate::LayerSpec;

    fn with_wall_type(mats: &[&str]) -> (Model, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let ls: Vec<LayerSpec> = mats
            .iter()
            .map(|n| LayerSpec {
                material: n.to_string(),
                thickness: 0.1,
            })
            .collect();
        let wt = m.add_wall_type("W", &ls).unwrap();
        (m, wt)
    }

    fn layers(m: &Model, wt: i64) -> Vec<String> {
        m.wall_type_materials(wt).unwrap()
    }

    /// A file predating the materials schema: no table, free text in layers.
    #[test]
    fn migration_keeps_text_materials() {
        let (m, wt) = with_wall_type(&["Brick M150", "Minvata 80 kg", "  "]);
        m.db.execute_batch("DROP TABLE material").unwrap();
        migrate(&m.db, false).unwrap();
        // layer texts are untouched
        assert_eq!(layers(&m, wt), ["Brick M150", "Minvata 80 kg", "  "]);
        let names: Vec<String> = m
            .materials()
            .unwrap()
            .into_iter()
            .map(|x| x.spec.name)
            .collect();
        assert!(names.contains(&"Brick M150".to_string()));
        assert!(names.contains(&"Minvata 80 kg".to_string()));
        assert!(
            names.contains(&"concrete".to_string()),
            "builtin set in {names:?}"
        );
        assert!(!names.iter().any(|n| n.trim().is_empty()));
        // the material class supplies properties: brick gets diagonal hatch and density
        let brick = m.material("Brick M150").unwrap();
        assert!(brick.id > 0);
        assert_eq!(brick.spec.hatch, "diagonal");
        assert_eq!(brick.spec.density, Some(1800.0));
        // a repeat neither duplicates nor resurrects a deleted built-in material
        let before = names.len();
        m.db.execute("DELETE FROM material WHERE name='glass'", [])
            .unwrap();
        migrate(&m.db, false).unwrap();
        assert_eq!(m.materials().unwrap().len(), before - 1);
    }

    /// Project with a layer region: `wall_region.material` is a number; opening must not fail.
    #[test]
    fn project_with_wall_region_opens_and_keeps_the_layer_number() {
        let path =
            std::env::temp_dir().join(format!("ddc_material_region_{}.ddc", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let p = path.to_str().unwrap();
        let wt = {
            let mut m = Model::open(p).unwrap();
            let wt = m
                .add_wall_type(
                    "W",
                    &[LayerSpec {
                        material: "2".into(),
                        thickness: 0.2,
                    }],
                )
                .unwrap();
            crate::wall_edit::ensure(&m.db).unwrap();
            m.db.execute(
                "INSERT INTO wall_region(wtype,idx,layer,z0,z1,material) VALUES (?1,0,0,0.0,1.0,2)",
                [wt],
            )
            .unwrap();
            wt
        };
        let region = |m: &Model| -> Value {
            m.db.query_row(
                "SELECT material FROM wall_region WHERE wtype=?1",
                [wt],
                |r| r.get(0),
            )
            .unwrap()
        };
        let mut m = Model::open(p).unwrap();
        let names: Vec<String> = m
            .materials()
            .unwrap()
            .into_iter()
            .map(|x| x.spec.name)
            .collect();
        assert!(
            names.contains(&"2".to_string()),
            "the layer text, {names:?}"
        );
        assert_eq!(
            m.material_usage("2").unwrap(),
            [("wall_type_layer".to_string(), 1)]
        );
        let mut s = m.material("2").unwrap().spec;
        s.name = "Tuff".into();
        m.set_material("2", &s).unwrap();
        assert_eq!(layers(&m, wt), ["Tuff"]);
        assert_eq!(region(&m), Value::Integer(2), "layer number is not a name");
        m.replace_material("Tuff", "concrete").unwrap();
        assert_eq!(region(&m), Value::Integer(2));
        drop(m);
        let _ = std::fs::remove_file(&path);
    }

    /// NBSP is not a name; a long imported name stays editable without renaming.
    #[test]
    fn blank_and_long_imported_names() {
        let long = "\u{41c}".repeat(130);
        let (mut m, _) = with_wall_type(&["\u{a0}", &long]);
        assert!(!m
            .materials()
            .unwrap()
            .iter()
            .any(|x| x.spec.name.trim().is_empty()));
        let mut s = m.material(&long).unwrap().spec;
        s.density = Some(1000.0);
        m.set_material(&long, &s).unwrap();
        assert_eq!(m.material(&long).unwrap().spec.density, Some(1000.0));
        s.name = "\u{41c}".repeat(121);
        assert!(m.set_material(&long, &s).is_err(), "a new name is checked");
    }

    #[test]
    fn hatch_keys_follow_the_class_and_the_row_less_default() {
        for (name, key) in [
            (crate::test_ru::ru("mat_concrete_b25"), "dots"),
            (crate::test_ru::ru("mat_brick"), "diagonal"),
            (crate::test_ru::ru("mat_mineral_wool"), "cross"),
            ("Timber", "back_diagonal"),
            ("Tuff", "none"),
        ] {
            assert_eq!(material_default_hatch(name), key, "{name}");
            assert_eq!(material_defaults(name).hatch, key, "{name}");
            assert!(MATERIAL_HATCHES.contains(&key));
        }
    }

    #[test]
    fn library_hatch_keys_are_accepted_only_when_well_formed() {
        assert!(is_lib_hatch("lib:brick"));
        assert!(!is_lib_hatch("lib:"));
        assert!(!is_lib_hatch("lib:  "));
        assert!(!is_lib_hatch("brick"));
        assert!(!is_lib_hatch(&format!("lib:{}", "x".repeat(41))));
        assert!(!is_lib_hatch("lib:a\u{7}b"));
    }

    #[test]
    fn new_text_is_listed_before_it_has_a_row() {
        let (m, _) = with_wall_type(&["Tuff"]);
        let t = m
            .materials()
            .unwrap()
            .into_iter()
            .find(|x| x.spec.name == "Tuff")
            .unwrap();
        assert_eq!(t.id, 0);
        assert_eq!(m.material("Tuff").unwrap().id, 0);
    }

    #[test]
    fn edit_rename_cascades_and_undoes() {
        let (mut m, wt) = with_wall_type(&["Tuff", "Tuff"]);
        let mut s = m.material("Tuff").unwrap().spec;
        s.color = Some(0xAABBCC);
        s.density = Some(1300.0);
        s.name = "Tuff M50".into();
        let id = m.set_material("Tuff", &s).unwrap();
        assert!(id > 0);
        assert_eq!(layers(&m, wt), ["Tuff M50", "Tuff M50"]);
        assert_eq!(m.material("Tuff M50").unwrap().spec.color, Some(0xAABBCC));
        m.undo().unwrap();
        assert_eq!(layers(&m, wt), ["Tuff", "Tuff"]);
        assert_eq!(m.material("Tuff").unwrap().id, 0);
    }

    #[test]
    fn rename_onto_existing_name_is_refused() {
        let (mut m, wt) = with_wall_type(&["brick"]);
        let mut s = m.material("brick").unwrap().spec;
        s.name = "concrete".into();
        assert!(m.set_material("brick", &s).is_err());
        assert_eq!(layers(&m, wt), ["brick"]);
    }

    #[test]
    fn delete_refuses_used_and_replace_moves_references() {
        let (mut m, wt) = with_wall_type(&["brick", "wood"]);
        assert!(matches!(
            m.delete_material("brick"),
            Err(Error::Conflict(_))
        ));
        assert_eq!(
            m.material_usage("brick").unwrap(),
            [("wall_type_layer".to_string(), 1)]
        );
        assert_eq!(m.replace_material("brick", "concrete").unwrap(), 1);
        assert_eq!(layers(&m, wt), ["concrete", "wood"]);
        assert_eq!(m.material("brick").unwrap().id, 0);
        m.undo().unwrap();
        assert_eq!(layers(&m, wt), ["brick", "wood"]);
        assert!(m.material("brick").unwrap().id > 0);
        m.delete_material("glass").unwrap();
        assert_eq!(m.material("glass").unwrap().id, 0);
        m.undo().unwrap();
        assert!(m.material("glass").unwrap().id > 0);
    }

    fn mig_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ddc_mig1_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn rows(db: &Connection) -> i64 {
        db.query_row("SELECT count(*) FROM material", [], |r| r.get(0))
            .unwrap()
    }

    /// The open-time upgrade on a plain connection: how many rows it changed.
    fn upgrade_writes(p: &str) -> u64 {
        let mut c = Connection::open(p).unwrap();
        crate::schema::upgrade(&mut c, Some(p)).unwrap();
        c.total_changes()
    }

    /// A current file with a layer text that has no row: opening writes nothing, saves the same
    /// bytes, and undo after the open leaves the rows the file was created with.
    #[test]
    fn reopening_a_current_file_writes_nothing() {
        let d = mig_dir("reopen");
        let (p, a, b) = (d.join("m.ddc"), d.join("a.ddc"), d.join("b.ddc"));
        let ps = p.to_str().unwrap();
        let created = {
            let mut m = Model::open(ps).unwrap();
            let n = rows(&m.db);
            let tuff = LayerSpec {
                material: "Tuff".into(),
                thickness: 0.2,
            };
            m.add_wall_type("W", &[tuff]).unwrap();
            m.snapshot_to(&a).unwrap();
            n
        };
        assert_eq!(upgrade_writes(ps), 0, "first reopen");
        assert_eq!(upgrade_writes(ps), 0, "second reopen");
        let mut m = Model::open(ps).unwrap();
        assert_eq!(rows(&m.db), created, "no row added by opening");
        assert_eq!(m.material("Tuff").unwrap().id, 0);
        m.snapshot_to(&b).unwrap();
        assert!(std::fs::read(&a).unwrap() == std::fs::read(&b).unwrap(), "saves differ");
        while m.undo().is_ok() {}
        assert!(matches!(m.undo(), Err(Error::NothingToUndo)));
        assert_eq!(rows(&m.db), created, "undo leaves no row from the open");
        drop(m);
        let _ = std::fs::remove_dir_all(d);
    }

    /// An older file whose text has no row gets the row once, by the upgrade, not as an edit.
    #[test]
    fn an_old_file_is_upgraded_once_and_leaves_nothing_to_undo() {
        let d = mig_dir("old");
        let p = d.join("m.ddc");
        let ps = p.to_str().unwrap();
        {
            let (m, _) = with_wall_type(&["Tuff"]);
            m.snapshot_to(&p).unwrap();
        }
        {
            let c = Connection::open(ps).unwrap();
            c.execute("DELETE FROM op", []).unwrap();
            c.execute(
                "UPDATE meta SET value=?1 WHERE key='schema_version'",
                [crate::schema::VERSION - 1],
            )
            .unwrap();
        }
        let mut m = Model::open(ps).unwrap();
        assert!(m.material("Tuff").unwrap().id > 0, "the upgrade adds the row");
        assert!(matches!(m.undo(), Err(Error::NothingToUndo)));
        let n = rows(&m.db);
        drop(m);
        assert_eq!(upgrade_writes(ps), 0, "upgraded once");
        let m = Model::open(ps).unwrap();
        assert_eq!(rows(&m.db), n);
        drop(m);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn bad_specs_are_refused() {
        let (mut m, _) = with_wall_type(&["brick"]);
        let mut s = material_defaults("Fresh");
        s.hatch = "plaid".into();
        assert!(m.add_material(&s).is_err());
        s.hatch = "none".into();
        s.density = Some(-1.0);
        assert!(m.add_material(&s).is_err());
        s.density = None;
        s.name = "  ".into();
        assert!(m.add_material(&s).is_err());
    }
}
