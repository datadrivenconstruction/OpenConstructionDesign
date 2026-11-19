//! Wall type library: layer function and priority (`wall_layer_flag`), whole-type edits that
//! keep placed walls aligned on the core or the exterior face, duplicate and rename, an archive
//! of unused types (moved, never deleted) and copying types with their materials from another
//! project file. Every write is one journal operation: one undo step.

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::{params, params_from_iter, types::Value, Connection, OpenFlags, OptionalExtension};
use serde_json::{json, Value as J};

use crate::ops::{from_json, to_json, Inv};
use crate::{Error, LayerSpec, MaterialSpec, Model};

/// Most layers one wall type may have.
pub const MAX_TYPE_LAYERS: usize = 32;
/// Thinnest and thickest layer accepted, metres.
pub const MIN_LAYER_T: f64 = 0.0001;
pub const MAX_LAYER_T: f64 = 5.0;
const EPS: f64 = 1e-9;

/// What a layer does in the wall (the order follows pro BIM tools).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LayerFn {
    Structure,
    Substrate,
    Insulation,
    Air,
    Membrane,
    Finish1,
    Finish2,
}

impl LayerFn {
    pub const ALL: [LayerFn; 7] = [
        LayerFn::Structure,
        LayerFn::Substrate,
        LayerFn::Insulation,
        LayerFn::Air,
        LayerFn::Membrane,
        LayerFn::Finish1,
        LayerFn::Finish2,
    ];

    /// Stable key stored in the file and used by the AI tools.
    pub fn key(self) -> &'static str {
        match self {
            LayerFn::Structure => "structure",
            LayerFn::Substrate => "substrate",
            LayerFn::Insulation => "insulation",
            LayerFn::Air => "air",
            LayerFn::Membrane => "membrane",
            LayerFn::Finish1 => "finish1",
            LayerFn::Finish2 => "finish2",
        }
    }

    pub fn parse(s: &str) -> Option<LayerFn> {
        let s = s.trim().to_ascii_lowercase();
        let alias = match s.as_str() {
            "core" => Some(LayerFn::Structure),
            "thermal" => Some(LayerFn::Insulation),
            "finish" => Some(LayerFn::Finish1),
            "vapour" | "vapor" => Some(LayerFn::Membrane),
            _ => None,
        };
        alias.or_else(|| LayerFn::ALL.into_iter().find(|f| f.key() == s))
    }

    /// Default cleanup priority: lower joins first (1 structure .. 5 outer finish).
    pub fn priority(self) -> u8 {
        match self {
            LayerFn::Structure => 1,
            LayerFn::Substrate => 2,
            LayerFn::Insulation | LayerFn::Air => 3,
            LayerFn::Finish1 => 4,
            LayerFn::Finish2 | LayerFn::Membrane => 5,
        }
    }

    /// A guess from the material text, for types saved before functions were stored.
    pub fn guess(material: &str) -> LayerFn {
        let m = material.to_lowercase();
        let has = |k: &[&str]| k.iter().any(|k| m.contains(k));
        let insul = [
            "insul",
            "wool",
            "\u{443}\u{442}\u{435}\u{43f}\u{43b}",
            "\u{432}\u{430}\u{442}\u{430}",
        ];
        let membrane = [
            "membrane",
            "vapo",
            "foil",
            "\u{43c}\u{435}\u{43c}\u{431}\u{440}",
            "\u{43f}\u{430}\u{440}\u{43e}\u{438}\u{437}",
        ];
        if has(&insul) {
            LayerFn::Insulation
        } else if has(&membrane) {
            LayerFn::Membrane
        } else if has(&["air", "\u{432}\u{43e}\u{437}\u{434}\u{443}\u{448}"]) {
            LayerFn::Air
        } else if has(&[
            "plaster", "render", "gypsum", "paint", "tile", "\u{448}\u{442}\u{443}\u{43a}",
            "\u{433}\u{438}\u{43f}\u{441}", "\u{43e}\u{431}\u{43b}\u{438}\u{446}",
        ]) {
            LayerFn::Finish1
        } else {
            LayerFn::Structure
        }
    }
}

/// One layer of a wall type, exterior first.
#[derive(Clone, Debug, PartialEq)]
pub struct TypeLayer {
    pub material: String,
    /// Metres.
    pub thickness: f64,
    pub function: LayerFn,
    /// Cleanup priority 1..=9 (lower joins first).
    pub priority: u8,
}

impl TypeLayer {
    pub fn new(material: &str, thickness: f64, function: LayerFn) -> Self {
        TypeLayer {
            material: material.to_string(),
            thickness,
            function,
            priority: function.priority(),
        }
    }

    pub fn spec(&self) -> LayerSpec {
        LayerSpec {
            material: self.material.clone(),
            thickness: self.thickness,
        }
    }

    fn same(&self, o: &TypeLayer) -> bool {
        self.material == o.material
            && (self.thickness - o.thickness).abs() < EPS
            && self.function == o.function
            && self.priority == o.priority
    }
}

/// Which face of a placed wall stays where it is when its type changes thickness.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    /// The centre of the structural core stays.
    #[default]
    Core,
    /// The exterior face (layer 1) stays.
    Exterior,
}

impl Align {
    pub fn key(self) -> &'static str {
        match self {
            Align::Core => "core",
            Align::Exterior => "exterior",
        }
    }

    pub fn parse(s: &str) -> Option<Align> {
        match s.trim().to_ascii_lowercase().as_str() {
            "core" => Some(Align::Core),
            "exterior" | "face" => Some(Align::Exterior),
            _ => None,
        }
    }
}

/// A wall type as the library panel lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct TypeUse {
    pub id: i64,
    pub name: String,
    /// Placed walls of this type.
    pub walls: usize,
    /// Stacked wall types this type is a part of.
    pub stacks: usize,
    pub layers: usize,
    pub thickness: f64,
}

/// An archived (purged) wall type; restorable.
#[derive(Clone, Debug, PartialEq)]
pub struct ArchivedType {
    pub id: i64,
    pub name: String,
    /// The type's id before it was archived.
    pub src_id: i64,
    /// UTC time stamp of the archive.
    pub at: String,
}

/// Outcome of a whole-type edit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EditReport {
    /// Placed walls of the type (all of them change size when the total changes).
    pub walls: usize,
    /// Walls whose offset was rewritten to keep the chosen face.
    pub shifted: usize,
}

/// Wall types read from another project file, with the materials they use.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TypePack {
    pub source: String,
    pub types: Vec<PackType>,
    pub materials: Vec<MaterialSpec>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PackType {
    pub name: String,
    pub layers: Vec<TypeLayer>,
}

/// What to do with an imported type whose name the project already uses for another build-up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Conflict {
    /// Import under a free name ("Name 2").
    #[default]
    Rename,
    Skip,
    /// Replace the project type's layers; placed walls follow (see [`Align`]).
    Overwrite,
}

impl Conflict {
    pub fn key(self) -> &'static str {
        match self {
            Conflict::Rename => "rename",
            Conflict::Skip => "skip",
            Conflict::Overwrite => "overwrite",
        }
    }

    pub fn parse(s: &str) -> Option<Conflict> {
        match s.trim().to_ascii_lowercase().as_str() {
            "rename" => Some(Conflict::Rename),
            "skip" => Some(Conflict::Skip),
            "overwrite" | "replace" => Some(Conflict::Overwrite),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportFate {
    /// No type of that name: added.
    New,
    /// Same name, same layers: nothing to do.
    Same,
    /// Same name, other layers: the conflict rule decides.
    Conflict,
}

/// One row of the import preview.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportRow {
    pub name: String,
    pub fate: ImportFate,
    /// Placed walls of the project type with this name (resized on overwrite).
    pub walls: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportReport {
    pub added: Vec<String>,
    /// (name in the source, name given here).
    pub renamed: Vec<(String, String)>,
    pub overwritten: Vec<String>,
    pub skipped: Vec<String>,
    pub same: Vec<String>,
    pub materials: Vec<String>,
    /// Placed walls resized by overwritten types.
    pub walls: usize,
}

impl ImportReport {
    pub fn written(&self) -> usize {
        self.added.len() + self.renamed.len() + self.overwritten.len()
    }
}

/// A wall type library change as one replayable step (AI plans carry these).
#[derive(Clone, Debug, PartialEq)]
pub enum TypeLibStep {
    Edit {
        id: i64,
        name: String,
        layers: Vec<TypeLayer>,
        align: Align,
    },
    Import {
        pack: TypePack,
        /// Type names to take; empty: all.
        pick: Vec<String>,
        rule: Conflict,
        align: Align,
    },
}

fn bad<T>(m: &str) -> Result<T, Error> {
    Err(Error::Invalid(m.into()))
}

/// Rules of a wall type: a name, 1..=MAX layers, each with a material, a positive thickness
/// and a priority 1..=9.
pub fn check_type(name: &str, layers: &[TypeLayer]) -> Result<(), Error> {
    if name.trim().is_empty() {
        return bad("empty type name");
    }
    if layers.is_empty() {
        return bad("a wall type needs at least one layer");
    }
    if layers.len() > MAX_TYPE_LAYERS {
        return bad("too many layers");
    }
    for l in layers {
        if !(l.thickness.is_finite() && (MIN_LAYER_T..=MAX_LAYER_T).contains(&l.thickness)) {
            return Err(Error::Invalid(format!(
                "layer '{}': thickness must be above zero",
                l.material
            )));
        }
        if l.material.trim().is_empty() {
            return bad("every layer needs a material");
        }
        if !(1..=9).contains(&l.priority) {
            return bad("layer priority must be 1..9");
        }
    }
    Ok(())
}

pub fn total(layers: &[TypeLayer]) -> f64 {
    layers.iter().map(|l| l.thickness).sum()
}

/// First and last structural layer; without one, the thickest layer.
pub fn core_range(layers: &[TypeLayer]) -> (usize, usize) {
    let s: Vec<usize> = (0..layers.len())
        .filter(|&i| layers[i].function == LayerFn::Structure)
        .collect();
    if let (Some(&a), Some(&b)) = (s.first(), s.last()) {
        return (a, b);
    }
    let k = (0..layers.len())
        .max_by(|&a, &b| layers[a].thickness.total_cmp(&layers[b].thickness))
        .unwrap_or(0);
    (k, k)
}

/// New `offset0` of a placed wall when its type goes from `old` to `new` (layers are laid from
/// `offset0` along the left normal, exterior first; `None` is a wall centred on its axis).
/// A centred wall stays `None` when the change keeps it centred.
pub fn realign(
    old: &[TypeLayer],
    new: &[TypeLayer],
    off: Option<f64>,
    align: Align,
) -> Option<f64> {
    let (t_old, t_new) = (total(old), total(new));
    let s = off.unwrap_or(-t_old / 2.0);
    let s2 = match align {
        Align::Exterior => s,
        Align::Core if old.is_empty() || new.is_empty() => s + t_old / 2.0 - t_new / 2.0,
        Align::Core => {
            let (a0, b0) = core_range(old);
            let (a1, b1) = core_range(new);
            let c = s + total(&old[..a0]) + total(&old[a0..=b0]) / 2.0;
            c - total(&new[..a1]) - total(&new[a1..=b1]) / 2.0
        }
    };
    if off.is_none() && (s2 + t_new / 2.0).abs() < EPS {
        None
    } else {
        Some(s2)
    }
}

fn has_table(db: &Connection, name: &str) -> Result<bool, Error> {
    Ok(db
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            [name],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

use crate::schema::has_col;

type Flags = BTreeMap<i64, (bool, Option<i64>, Option<String>)>;

fn read_flags(db: &Connection, id: i64) -> Result<Flags, Error> {
    if !has_table(db, "wall_layer_flag")? {
        return Ok(Flags::new());
    }
    let sql = if has_col(db, "wall_layer_flag", "function")? {
        "SELECT pos, core, priority, function FROM wall_layer_flag WHERE wtype=?1"
    } else {
        "SELECT pos, core, priority, NULL FROM wall_layer_flag WHERE wtype=?1"
    };
    let mut st = db.prepare(sql)?;
    let rows = st.query_map([id], |r| {
        Ok((r.get::<_, i64>(0)?, (r.get::<_, i64>(1)? != 0, r.get(2)?, r.get(3)?)))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Layers of a type with functions: stored ones first, then the core flag, then a guess; a type
/// with no structural layer gets its thickest non-insulating layer as the structure.
fn read_layers(db: &Connection, id: i64) -> Result<Vec<TypeLayer>, Error> {
    let mut st = db.prepare(
        "SELECT pos, material, thickness FROM wall_type_layer WHERE wtype=?1 ORDER BY pos",
    )?;
    let base: Vec<(i64, String, f64)> = st
        .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    let flags = read_flags(db, id)?;
    let mut stored_any = false;
    let mut out: Vec<TypeLayer> = base
        .into_iter()
        .map(|(pos, material, thickness)| {
            let f = flags.get(&pos);
            let function = match f {
                Some((_, _, Some(k))) if LayerFn::parse(k).is_some() => {
                    stored_any = true;
                    LayerFn::parse(k).unwrap_or(LayerFn::Structure)
                }
                Some((true, _, _)) => {
                    stored_any = true;
                    LayerFn::Structure
                }
                _ => match LayerFn::guess(&material) {
                    LayerFn::Structure => LayerFn::Finish1,
                    g => g,
                },
            };
            let priority = f
                .and_then(|x| x.1)
                .map_or(function.priority(), |p| p.clamp(1, 9) as u8);
            TypeLayer {
                material,
                thickness,
                function,
                priority,
            }
        })
        .collect();
    if !stored_any && !out.iter().any(|l| l.function == LayerFn::Structure) {
        // nothing stored: the thickest layer that does not insulate carries the wall
        let soft = |f: LayerFn| matches!(f, LayerFn::Insulation | LayerFn::Air | LayerFn::Membrane);
        let pick = (0..out.len())
            .filter(|&i| !soft(out[i].function))
            .max_by(|&a, &b| out[a].thickness.total_cmp(&out[b].thickness))
            .or_else(|| {
                (0..out.len()).max_by(|&a, &b| out[a].thickness.total_cmp(&out[b].thickness))
            });
        if let Some(k) = pick {
            out[k].function = LayerFn::Structure;
            if !flags.get(&(k as i64)).is_some_and(|f| f.1.is_some()) {
                out[k].priority = LayerFn::Structure.priority();
            }
        }
    }
    Ok(out)
}

fn write_layers(db: &Connection, id: i64, layers: &[TypeLayer]) -> Result<(), Error> {
    for (pos, l) in layers.iter().enumerate() {
        db.execute(
            "INSERT INTO wall_type_layer(wtype, pos, material, thickness) VALUES (?1,?2,?3,?4)",
            params![id, pos as i64, l.material.trim(), l.thickness],
        )?;
        db.execute(
            "INSERT OR REPLACE INTO wall_layer_flag(wtype, pos, core, priority, function) \
             VALUES (?1,?2,?3,?4,?5)",
            params![
                id,
                pos as i64,
                l.function == LayerFn::Structure,
                l.priority as i64,
                l.function.key()
            ],
        )?;
    }
    Ok(())
}

fn name_taken(db: &Connection, name: &str, except: i64) -> Result<bool, Error> {
    let n: i64 = db.query_row(
        "SELECT count(*) FROM wall_type WHERE name=?1 AND id<>?2",
        params![name, except],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// `base`, or the first of "base 2", "base 3"… no wall type uses.
fn free_name_in(db: &Connection, base: &str) -> Result<String, Error> {
    let base = base.trim();
    if !name_taken(db, base, 0)? {
        return Ok(base.to_string());
    }
    for k in 2..10_000 {
        let n = format!("{base} {k}");
        if !name_taken(db, &n, 0)? {
            return Ok(n);
        }
    }
    bad("no free type name")
}

/// Replaces name and layers of type `id`; placed walls keep the `align` face. The undo of the
/// type rows, flags and every moved wall is returned.
fn write_type(
    db: &Connection,
    id: i64,
    name: &str,
    layers: &[TypeLayer],
    align: Align,
) -> Result<(EditReport, Inv), Error> {
    crate::wall_edit::ensure(db)?;
    let n: i64 = db.query_row("SELECT count(*) FROM wall_type WHERE id=?1", [id], |r| r.get(0))?;
    if n == 0 {
        return bad("no such wall type");
    }
    if name_taken(db, name.trim(), id)? {
        return Err(Error::Conflict(format!(
            "a wall type named '{}' already exists",
            name.trim()
        )));
    }
    let old = read_layers(db, id)?;
    let mut inv = Inv::default()
        .delete("wall_type_layer", "wtype", id)
        .delete("wall_layer_flag", "wtype", id)
        .restore(db, "wall_type", "id", id)?
        .restore(db, "wall_type_layer", "wtype", id)?
        .restore(db, "wall_layer_flag", "wtype", id)?;
    db.execute(
        "UPDATE wall_type SET name=?2 WHERE id=?1",
        params![id, name.trim()],
    )?;
    db.execute("DELETE FROM wall_type_layer WHERE wtype=?1", [id])?;
    db.execute("DELETE FROM wall_layer_flag WHERE wtype=?1", [id])?;
    write_layers(db, id, layers)?;
    let walls: Vec<(i64, Option<f64>)> = {
        let mut st = db.prepare("SELECT id, offset0 FROM wall WHERE wtype=?1 ORDER BY id")?;
        let v = st
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        v
    };
    let mut rep = EditReport {
        walls: walls.len(),
        shifted: 0,
    };
    for (w, off) in walls {
        let to = realign(&old, layers, off, align);
        let same = match (off, to) {
            (None, None) => true,
            (Some(a), Some(b)) => (a - b).abs() < EPS,
            _ => false,
        };
        if same {
            continue;
        }
        inv = inv.restore(db, "wall", "id", w)?;
        db.execute("UPDATE wall SET offset0=?2 WHERE id=?1", params![w, to])?;
        rep.shifted += 1;
    }
    Ok((rep, inv))
}

/// Adds a type; the undo removes it (and puts back orphan flags a reused id had).
fn insert_type(db: &Connection, name: &str, layers: &[TypeLayer]) -> Result<(i64, Inv), Error> {
    crate::wall_edit::ensure(db)?;
    db.execute("INSERT INTO wall_type(name) VALUES (?1)", [name.trim()])?;
    let id = db.last_insert_rowid();
    let orphans = Inv::default()
        .restore(db, "wall_layer_flag", "wtype", id)?
        .restore(db, "wall_type_layer", "wtype", id)?;
    db.execute("DELETE FROM wall_layer_flag WHERE wtype=?1", [id])?;
    db.execute("DELETE FROM wall_type_layer WHERE wtype=?1", [id])?;
    write_layers(db, id, layers)?;
    let inv = Inv::default()
        .delete("wall_layer_flag", "wtype", id)
        .delete("wall_type_layer", "wtype", id)
        .delete("wall_type", "id", id)
        .then(orphans);
    Ok((id, inv))
}

/// Rows that belong to a wall type, by table and condition (`?1` = type id).
const OWNED: [(&str, &str); 8] = [
    ("wall_type", "id=?1"),
    ("wall_type_layer", "wtype=?1"),
    ("wall_layer_flag", "wtype=?1"),
    ("wall_region", "wtype=?1"),
    ("wall_wrap", "wtype=?1"),
    ("pparam_val", "kind='wall_type' AND ref=?1"),
    ("prop", "kind='wall_type' AND ref=?1"),
    ("ext_ref", "kind='wall_type' AND ref=?1"),
];

/// Column holding the type id in an owned table.
fn id_col(table: &str) -> &'static str {
    match table {
        "wall_type" => "id",
        "pparam_val" | "prop" | "ext_ref" => "ref",
        _ => "wtype",
    }
}

const ARCHIVE_DDL: &str = "CREATE TABLE IF NOT EXISTS wall_type_archive(id INTEGER PRIMARY KEY, \
     name TEXT NOT NULL, src_id INTEGER NOT NULL, at TEXT NOT NULL, rows TEXT NOT NULL)";

fn snapshot(db: &Connection, id: i64) -> Result<J, Error> {
    let mut out = vec![];
    for (t, cond) in OWNED {
        if !has_table(db, t)? {
            continue;
        }
        let mut st = db.prepare(&format!("SELECT * FROM {t} WHERE {cond} ORDER BY rowid"))?;
        let cols: Vec<String> = st.column_names().map(|c| c.to_string()).collect();
        let n = cols.len();
        let rows: Vec<Vec<Value>> = st
            .query_map([id], |r| (0..n).map(|i| r.get::<_, Value>(i)).collect())?
            .collect::<Result<_, _>>()?;
        let rows: Vec<J> = rows
            .iter()
            .map(|r| J::Array(r.iter().map(to_json).collect()))
            .collect();
        out.push(json!({ "table": t, "cols": cols, "rows": rows }));
    }
    Ok(J::Array(out))
}

impl Model {
    /// Layers of a wall type with function and priority, exterior first.
    pub fn wall_type_layers_full(&self, id: i64) -> Result<Vec<TypeLayer>, Error> {
        read_layers(&self.db, id)
    }

    /// Every wall type with its use, by name.
    pub fn wall_type_usage(&self) -> Result<Vec<TypeUse>, Error> {
        let stacks = if has_table(&self.db, "stack_part")? {
            "(SELECT count(DISTINCT s.stack) FROM stack_part s WHERE s.wtype=t.id)"
        } else {
            "0"
        };
        let mut st = self.db.prepare(&format!(
            "SELECT t.id, t.name, (SELECT count(*) FROM wall w WHERE w.wtype=t.id), {stacks}, \
             (SELECT count(*) FROM wall_type_layer l WHERE l.wtype=t.id), \
             (SELECT total(l.thickness) FROM wall_type_layer l WHERE l.wtype=t.id) \
             FROM wall_type t ORDER BY t.name COLLATE NOCASE, t.id"
        ))?;
        let v = st
            .query_map([], |r| {
                Ok(TypeUse {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    walls: r.get::<_, i64>(2)?.max(0) as usize,
                    stacks: r.get::<_, i64>(3)?.max(0) as usize,
                    layers: r.get::<_, i64>(4)?.max(0) as usize,
                    thickness: r.get(5)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(v)
    }

    /// Placed walls of a type, by id.
    pub fn walls_of_type(&self, id: i64) -> Result<Vec<i64>, Error> {
        let mut st = self.db.prepare("SELECT id FROM wall WHERE wtype=?1 ORDER BY id")?;
        let v = st.query_map([id], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(v)
    }

    /// `base`, or the first free "base 2", "base 3"…
    pub fn free_wall_type_name(&self, base: &str) -> Result<String, Error> {
        free_name_in(&self.db, base)
    }

    /// Name, layers, functions and priorities of a wall type in one undo step; placed walls of
    /// the type keep their core centre or exterior face where it was.
    pub fn edit_wall_type(
        &mut self,
        id: i64,
        name: &str,
        layers: &[TypeLayer],
        align: Align,
    ) -> Result<EditReport, Error> {
        check_type(name, layers)?;
        let (name, layers) = (name.trim().to_string(), layers.to_vec());
        self.op("wall_type_edit", move |db| {
            let (rep, inv) = write_type(db, id, &name, &layers, align)?;
            Ok((rep, id, inv))
        })
    }

    /// Renames a wall type; a name another type uses is refused.
    pub fn rename_wall_type(&mut self, id: i64, name: &str) -> Result<(), Error> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return bad("empty type name");
        }
        self.op("wall_type_rename", move |db| {
            if name_taken(db, &name, id)? {
                return Err(Error::Conflict(format!(
                    "a wall type named '{name}' already exists"
                )));
            }
            let inv = Inv::default().restore(db, "wall_type", "id", id)?;
            if db.execute("UPDATE wall_type SET name=?2 WHERE id=?1", params![id, name])? == 0 {
                return bad("no such wall type");
            }
            Ok(((), id, inv))
        })
    }

    /// Copy of a wall type (layers, functions, type parameters) under `name`; the copy's id.
    pub fn duplicate_wall_type(&mut self, id: i64, name: &str) -> Result<i64, Error> {
        let layers = read_layers(&self.db, id)?;
        check_type(name, &layers)?;
        let name = name.trim().to_string();
        self.op("wall_type_dup", move |db| {
            if name_taken(db, &name, 0)? {
                return Err(Error::Conflict(format!(
                    "a wall type named '{name}' already exists"
                )));
            }
            let (new, inv) = insert_type(db, &name, &layers)?;
            let inv = crate::types::copy_type_data_inv(db, "wall_type", id, new, inv)?;
            Ok((new, new, inv))
        })
    }

    /// Wall types nothing uses that may be archived (the last wall type always stays).
    pub fn archivable_wall_types(&self) -> Result<Vec<(i64, String)>, Error> {
        crate::wall_edit::ensure(&self.db)?;
        Ok(crate::purge::candidates(&self.db)?
            .into_iter()
            .filter(|i| i.table == "wall_type")
            .map(|i| (i.id, i.name))
            .collect())
    }

    /// Moves unused wall types `ids` into the archive (nothing is deleted: every row is kept and
    /// [`Self::restore_archived_wall_type`] brings it back). One undo step; returns the names.
    pub fn archive_wall_types(&mut self, ids: &[i64]) -> Result<Vec<String>, Error> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        crate::wall_edit::ensure(&self.db)?;
        let ids = ids.to_vec();
        self.op("wall_type_archive", move |db| {
            db.execute_batch(ARCHIVE_DDL)?;
            let free: BTreeMap<i64, String> = crate::purge::candidates(db)?
                .into_iter()
                .filter(|i| i.table == "wall_type")
                .map(|i| (i.id, i.name))
                .collect();
            let mut inv = Inv::default();
            let mut names = vec![];
            for id in &ids {
                let Some(name) = free.get(id) else {
                    return Err(Error::Invalid(format!("wall type {id} is in use or missing")));
                };
                let rows = snapshot(db, *id)?;
                for (t, cond) in OWNED {
                    if has_table(db, t)? {
                        inv = inv.restore_where(db, t, cond, &[Value::Integer(*id)])?;
                        db.execute(&format!("DELETE FROM {t} WHERE {cond}"), [id])?;
                    }
                }
                db.execute(
                    "INSERT INTO wall_type_archive(name, src_id, at, rows) \
                     VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%SZ','now'), ?3)",
                    params![name, id, rows.to_string()],
                )?;
                inv = inv.delete("wall_type_archive", "id", db.last_insert_rowid());
                names.push(name.clone());
            }
            Ok((names, ids[0], inv))
        })
    }

    /// The archive, newest first.
    pub fn archived_wall_types(&self) -> Result<Vec<ArchivedType>, Error> {
        if !has_table(&self.db, "wall_type_archive")? {
            return Ok(vec![]);
        }
        let mut st = self
            .db
            .prepare("SELECT id, name, src_id, at FROM wall_type_archive ORDER BY id DESC")?;
        let v = st
            .query_map([], |r| {
                Ok(ArchivedType {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    src_id: r.get(2)?,
                    at: r.get(3)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(v)
    }

    /// Brings an archived type back (its old id when free, a free name when the old one is
    /// taken); returns the type id. Links to source files stay in the archive record only.
    pub fn restore_archived_wall_type(&mut self, archive_id: i64) -> Result<i64, Error> {
        if !has_table(&self.db, "wall_type_archive")? {
            return bad("the archive is empty");
        }
        crate::wall_edit::ensure(&self.db)?;
        self.op("wall_type_restore", move |db| {
            let (name, src_id, rows): (String, i64, String) = db
                .query_row(
                    "SELECT name, src_id, rows FROM wall_type_archive WHERE id=?1",
                    [archive_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?
                .ok_or_else(|| Error::Invalid("no such archived type".into()))?;
            let tables: J = serde_json::from_str(&rows)?;
            let id_free: i64 =
                db.query_row("SELECT count(*) FROM wall_type WHERE id=?1", [src_id], |r| r.get(0))?;
            let new_name = free_name_in(db, &name)?;
            let new = if id_free == 0 {
                db.execute(
                    "INSERT INTO wall_type(id, name) VALUES (?1, ?2)",
                    params![src_id, new_name],
                )?;
                src_id
            } else {
                db.execute("INSERT INTO wall_type(name) VALUES (?1)", [&new_name])?;
                db.last_insert_rowid()
            };
            // undo: the archive row comes back, the restored rows go (ext_ref is never restored)
            let mut inv = Inv::default().restore(db, "wall_type_archive", "id", archive_id)?;
            for (t, cond) in OWNED.iter().rev() {
                if *t != "ext_ref" && has_table(db, t)? {
                    let args = vec![Value::Integer(new)];
                    inv = inv.sql(&format!("DELETE FROM {t} WHERE {cond}"), args);
                }
            }
            for tb in tables.as_array().into_iter().flatten() {
                let t = tb["table"].as_str().unwrap_or_default();
                if t == "wall_type" || t == "ext_ref" || !OWNED.iter().any(|o| o.0 == t) {
                    continue;
                }
                if !has_table(db, t)? {
                    continue;
                }
                let cols: Vec<String> = tb["cols"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|c| c.as_str().map(String::from))
                    .collect();
                let key = id_col(t);
                let ph = (1..=cols.len()).map(|i| format!("?{i}")).collect::<Vec<_>>();
                let sql = format!(
                    "INSERT OR REPLACE INTO {t}({}) VALUES ({})",
                    cols.join(","),
                    ph.join(",")
                );
                for r in tb["rows"].as_array().into_iter().flatten() {
                    let vals: Vec<Value> = cols
                        .iter()
                        .zip(r.as_array().into_iter().flatten())
                        .map(|(c, v)| {
                            if c == key {
                                Value::Integer(new)
                            } else {
                                from_json(v)
                            }
                        })
                        .collect();
                    db.execute(&sql, params_from_iter(vals.iter()))?;
                }
            }
            db.execute("DELETE FROM wall_type_archive WHERE id=?1", [archive_id])?;
            Ok((new, new, inv))
        })
    }

    /// Preview of an import: per type, new / same / conflict, and the walls an overwrite resizes.
    pub fn plan_type_import(&self, pack: &TypePack) -> Result<Vec<ImportRow>, Error> {
        let mut out = vec![];
        for t in &pack.types {
            let found: Option<i64> = self
                .db
                .query_row("SELECT id FROM wall_type WHERE name=?1", [t.name.trim()], |r| {
                    r.get(0)
                })
                .optional()?;
            let (fate, walls) = match found {
                None => (ImportFate::New, 0),
                Some(id) => {
                    let have = read_layers(&self.db, id)?;
                    let same = have.len() == t.layers.len()
                        && have.iter().zip(&t.layers).all(|(a, b)| a.same(b));
                    let fate = if same {
                        ImportFate::Same
                    } else {
                        ImportFate::Conflict
                    };
                    (fate, self.walls_of_type(id)?.len())
                }
            };
            out.push(ImportRow {
                name: t.name.trim().to_string(),
                fate,
                walls,
            });
        }
        Ok(out)
    }

    /// Copies wall types of `pack` (all, or the names in `pick`) with the materials their layers
    /// use and the project lacks. Name conflicts follow `rule`. One undo step; nothing to write:
    /// no journal entry.
    pub fn import_wall_types(
        &mut self,
        pack: &TypePack,
        pick: &[String],
        rule: Conflict,
        align: Align,
    ) -> Result<ImportReport, Error> {
        let plan = self.plan_type_import(pack)?;
        let mut rep = ImportReport::default();
        let mut todo: Vec<(PackType, ImportFate)> = vec![];
        for (t, row) in pack.types.iter().zip(&plan) {
            if !pick.is_empty() && !pick.iter().any(|p| p.trim() == row.name) {
                continue;
            }
            if check_type(&t.name, &t.layers).is_err() {
                rep.skipped.push(row.name.clone());
                continue;
            }
            match (row.fate, rule) {
                (ImportFate::Same, _) => rep.same.push(row.name.clone()),
                (ImportFate::Conflict, Conflict::Skip) => rep.skipped.push(row.name.clone()),
                (f, _) => todo.push((t.clone(), f)),
            }
        }
        if todo.is_empty() {
            return Ok(rep);
        }
        let mats = pack.materials.clone();
        self.op("wall_type_import", move |db| {
            let mut inv = Inv::default();
            let mut first = 0;
            for (t, fate) in &todo {
                let name = t.name.trim();
                match (fate, rule) {
                    (ImportFate::Conflict, Conflict::Overwrite) => {
                        let id: i64 = db.query_row(
                            "SELECT id FROM wall_type WHERE name=?1",
                            [name],
                            |r| r.get(0),
                        )?;
                        let (e, i) = write_type(db, id, name, &t.layers, align)?;
                        inv = inv.then(i);
                        rep.walls += e.walls;
                        rep.overwritten.push(name.to_string());
                        first = if first == 0 { id } else { first };
                    }
                    (ImportFate::Conflict, _) => {
                        let n = free_name_in(db, name)?;
                        let (id, i) = insert_type(db, &n, &t.layers)?;
                        inv = inv.then(i);
                        rep.renamed.push((name.to_string(), n));
                        first = if first == 0 { id } else { first };
                    }
                    _ => {
                        let (id, i) = insert_type(db, name, &t.layers)?;
                        inv = inv.then(i);
                        rep.added.push(name.to_string());
                        first = if first == 0 { id } else { first };
                    }
                }
                for l in &t.layers {
                    let m = l.material.trim();
                    if rep.materials.iter().any(|x| x == m) {
                        continue;
                    }
                    let Some(spec) = mats.iter().find(|s| s.name == m) else {
                        continue;
                    };
                    let here: i64 = db.query_row(
                        "SELECT count(*) FROM material WHERE name=?1",
                        [m],
                        |r| r.get(0),
                    )?;
                    if here > 0 {
                        continue;
                    }
                    crate::materials::insert_spec_ignore(db, spec)?;
                    inv = inv.sql(
                        "DELETE FROM material WHERE name=?1",
                        vec![Value::Text(m.to_string())],
                    );
                    rep.materials.push(m.to_string());
                }
            }
            Ok((rep, first, inv))
        })
    }

    /// Runs one library step (the AI plan replays these on Apply).
    pub fn apply_type_lib(&mut self, s: &TypeLibStep) -> Result<(), Error> {
        match s {
            TypeLibStep::Edit {
                id,
                name,
                layers,
                align,
            } => self.edit_wall_type(*id, name, layers, *align).map(|_| ()),
            TypeLibStep::Import {
                pack,
                pick,
                rule,
                align,
            } => {
                let r = self.import_wall_types(pack, pick, *rule, *align)?;
                if r.written() == 0 {
                    return bad("nothing to import: every type is skipped or already the same");
                }
                Ok(())
            }
        }
    }
}

/// Reads the wall types and materials of another project file without changing it (read-only
/// connection, no migration). Archived types are not offered.
pub fn read_type_pack(path: &Path) -> Result<TypePack, Error> {
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    if !has_table(&db, "wall_type")? || !has_table(&db, "wall_type_layer")? {
        return bad("the file holds no wall types");
    }
    let ids: Vec<(i64, String)> = {
        let mut st = db.prepare("SELECT id, name FROM wall_type ORDER BY name COLLATE NOCASE, id")?;
        let v = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        v
    };
    let mut types = vec![];
    for (id, name) in ids {
        let layers = read_layers(&db, id)?;
        if !layers.is_empty() && !name.trim().is_empty() {
            types.push(PackType { name, layers });
        }
    }
    let mut materials = vec![];
    if has_table(&db, "material")? {
        if let Ok(mut st) = db.prepare(
            "SELECT name, color, hatch, density, conductivity, specific_heat, note FROM material",
        ) {
            let rows = st.query_map([], |r| {
                Ok(MaterialSpec {
                    name: r.get(0)?,
                    color: r.get::<_, Option<i64>>(1)?.map(|c| c as u32),
                    hatch: r.get(2)?,
                    density: r.get(3)?,
                    conductivity: r.get(4)?,
                    specific_heat: r.get(5)?,
                    note: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
                })
            })?;
            let used: Vec<&str> = types
                .iter()
                .flat_map(|t| t.layers.iter().map(|l| l.material.trim()))
                .collect();
            for m in rows.flatten() {
                if used.contains(&m.name.as_str()) {
                    materials.push(m);
                }
            }
        }
    }
    Ok(TypePack {
        source: path.display().to_string(),
        types,
        materials,
    })
}

#[cfg(test)]
#[path = "wall_types_tests.rs"]
mod tests;
