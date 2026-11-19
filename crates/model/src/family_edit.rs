// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Family editor (schema 29): `fam_doc` drafts (definition text, link to the project family),
//! load/reload into the project as one operation that refreshes instances, type catalog (CSV),
//! and `comp_thumb` thumbnails: a derived cache keyed by the definition hash (not journaled).

use std::hash::{Hash, Hasher};

use ddc_geom::component::{catalog, thumb, Family, FamilyType, Lod};
use rusqlite::{params, Connection, OptionalExtension};

use crate::components::{family_into, reload_into};
use crate::ops::Inv;
use crate::{Error, Model};

pub(crate) const DDL: &str = "
CREATE TABLE IF NOT EXISTS fam_doc(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, def TEXT NOT NULL,
    family INTEGER);
CREATE TABLE IF NOT EXISTS comp_thumb(family INTEGER NOT NULL, size INTEGER NOT NULL, hash TEXT NOT NULL,
    rgba BLOB NOT NULL, PRIMARY KEY(family, size));
";

/// Additive and idempotent: tables are created only if missing. The thumbnail cache of the earlier
/// shape (keyed by family only) is derived: it is recreated with the key (family, size).
pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    let old_key = db
        .query_row(
            "SELECT pk FROM pragma_table_info('comp_thumb') WHERE name='size'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some_and(|pk| pk == 0);
    if old_key {
        db.execute_batch("DROP TABLE comp_thumb")?;
    }
    db.execute_batch(DDL)?;
    Ok(())
}

fn fam_err(e: impl std::fmt::Display) -> Error {
    Error::Invalid(format!("component: {e}"))
}

/// Result of loading a family into the project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FamilyLoad {
    pub family: i64,
    /// A new family (otherwise a reload of the existing one with the same name).
    pub created: bool,
    /// Instances rebuilt from the new definition.
    pub instances: usize,
    /// Types after loading.
    pub types: usize,
    /// Project families whose embedded copy was updated (`begin … end`).
    pub nested: usize,
}

/// A project type whose values a load with `overwrite_types` replaces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeOverwrite {
    pub name: String,
    /// Parameters whose value changes, appears or goes away.
    pub values: usize,
    /// Instances placed with this type.
    pub instances: usize,
}

fn same_value(a: &ddc_geom::component::Value, b: &ddc_geom::component::Value) -> bool {
    use ddc_geom::component::Value;
    match (a, b) {
        // values come back through their text form: allow the round trip
        (Value::Num(x), Value::Num(y)) => (x - y).abs() <= 1e-9 * x.abs().max(y.abs()).max(1.0),
        _ => a == b,
    }
}

fn changed_values(old: &[(String, ddc_geom::component::Value)], new: &FamilyType) -> usize {
    let differs = |k: &str, v: &ddc_geom::component::Value| {
        !new.values.iter().any(|(n, w)| n == k && same_value(v, w))
    };
    let gone = old.iter().filter(|(k, v)| differs(k, v)).count();
    let added = new
        .values
        .iter()
        .filter(|(k, _)| !old.iter().any(|(n, _)| n == k))
        .count();
    gone + added
}

/// Catalog import result: types accepted, warnings (per header row).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogLoad {
    pub types: usize,
    /// Cells not applied (bad number, unit, yes/no), with line and column.
    pub skipped: Vec<String>,
    pub warnings: Vec<String>,
}

/// Draft save result: written (id) or the name is taken by another draft (its id).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocSave {
    Saved(i64),
    NameTaken(i64),
}

fn def_hash(def: &str) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    def.hash(&mut h);
    format!("{:016x}", h.finish())
}

fn count(db: &Connection, sql: &str, id: i64) -> Result<usize, Error> {
    Ok(db.query_row(sql, [id], |r| r.get::<_, i64>(0))? as usize)
}

impl Model {
    /// Load from the editor: a new name creates a new family, an existing one replaces the definition
    /// (one operation and one undo step); with `overwrite_types`, same-named type values are taken
    /// from the text, otherwise the project's are kept. All types and instances must build.
    pub fn load_family_def(
        &mut self,
        def: &str,
        overwrite_types: bool,
    ) -> Result<FamilyLoad, Error> {
        let f = Family::parse(def).map_err(fam_err)?;
        for t in &f.types {
            let r = f.resolve(&t.values, &[]).map_err(fam_err)?;
            f.build(&r, Lod::Medium).map_err(fam_err)?;
        }
        if f.types.is_empty() {
            let r = f.resolve(&[], &[]).map_err(fam_err)?;
            f.build(&r, Lod::Medium).map_err(fam_err)?;
        }
        let existing = self.family_id(&f.name)?;
        let kind = if existing.is_some() {
            "comp_family_edit"
        } else {
            "comp_family_add"
        };
        self.op(kind, |db| {
            let types = |id| count(db, "SELECT count(*) FROM comp_type WHERE family=?1", id);
            let Some(id) = existing else {
                let id = family_into(db, def)?;
                let inv = Inv::default()
                    .delete("comp_type", "family", id)
                    .delete("comp_family", "id", id);
                let load = FamilyLoad {
                    family: id,
                    created: true,
                    instances: 0,
                    types: types(id)?,
                    nested: 0,
                };
                return Ok((load, id, inv));
            };
            // same path as a reload: project types without instances are checked too
            let (inv, nested) = reload_into(db, id, &f, def, overwrite_types)?;
            let instances = count(
                db,
                "SELECT count(*) FROM comp WHERE ctype IN (SELECT id FROM comp_type WHERE family=?1)",
                id,
            )?;
            let load = FamilyLoad {
                family: id,
                created: false,
                instances,
                types: types(id)?,
                nested,
            };
            Ok((load, id, inv))
        })
    }

    /// Read-only preview of `load_family_def(def, true)`: the project types of the same name
    /// whose values would be replaced, in project order. A new family or an unreadable text
    /// changes no project type.
    pub fn type_overwrites(&self, def: &str) -> Result<Vec<TypeOverwrite>, Error> {
        let Ok(f) = Family::parse(def) else {
            return Ok(vec![]);
        };
        let Some(id) = self.family_id(&f.name)? else {
            return Ok(vec![]);
        };
        let mut out = vec![];
        for (tid, name) in self.comp_types(id)? {
            let Some(new) = f.types.iter().find(|t| t.name == name) else {
                continue;
            };
            let values = changed_values(&self.comp_type_values(tid)?, new);
            if values == 0 {
                continue;
            }
            let instances = count(&self.db, "SELECT count(*) FROM comp WHERE ctype=?1", tid)?;
            out.push(TypeOverwrite {
                name,
                values,
                instances,
            });
        }
        Ok(out)
    }

    /// Definition text of a project family (to open in the editor).
    pub fn family_def(&self, family: i64) -> Result<String, Error> {
        self.db
            .query_row("SELECT def FROM comp_family WHERE id=?1", [family], |r| {
                r.get(0)
            })
            .optional()?
            .ok_or_else(|| Error::Invalid(format!("no component family {family}")))
    }

    /// Types of a project family as in the definition (name, values).
    pub fn family_types(&self, family: i64) -> Result<Vec<FamilyType>, Error> {
        self.comp_types(family)?
            .into_iter()
            .map(|(id, name)| {
                Ok(FamilyType {
                    name,
                    values: self.comp_type_values(id)?,
                })
            })
            .collect()
    }

    /// Type catalog as CSV (columns are type parameters without formulas, units in the header).
    pub fn export_type_catalog(&self, family: i64) -> Result<String, Error> {
        let f = self.family(family)?;
        Ok(catalog::export_csv(&f, &self.family_types(family)?))
    }

    /// Writes a catalog plan made against [`Model::family_types`] (the caller reads the file
    /// with its number rule): added and changed types only, as one operation; an empty plan
    /// writes nothing. Cells the plan skipped are reported, not fatal.
    pub fn import_type_catalog(
        &mut self,
        family: i64,
        plan: &catalog::Plan,
    ) -> Result<CatalogLoad, Error> {
        let rows: Vec<(String, Vec<(String, ddc_geom::component::Value)>)> = plan
            .types
            .iter()
            .map(|t| (t.name.clone(), t.values.clone()))
            .collect();
        if !rows.is_empty() {
            self.set_comp_types(family, &rows)?;
        }
        Ok(CatalogLoad {
            types: rows.len(),
            skipped: plan.skipped.iter().map(|e| e.to_string()).collect(),
            warnings: plan.warnings.iter().map(|w| w.to_string()).collect(),
        })
    }

    /// Editor draft: created or replaced by name. Outside the project journal: the
    /// editor has its own history, and project undo does not touch drafts.
    pub fn save_fam_doc(
        &mut self,
        name: &str,
        def: &str,
        family: Option<i64>,
    ) -> Result<i64, Error> {
        if name.trim().is_empty() {
            return Err(Error::Invalid("empty family document name".into()));
        }
        self.db.execute(
            "INSERT INTO fam_doc(name, def, family) VALUES (?1,?2,?3) \
             ON CONFLICT(name) DO UPDATE SET def=excluded.def, family=excluded.family",
            params![name, def, family],
        )?;
        Ok(self
            .db
            .query_row("SELECT id FROM fam_doc WHERE name=?1", [name], |r| r.get(0))?)
    }

    /// Saves a draft by id: an open draft (`id`) is renamed in its own row,
    /// a new one is inserted. Another draft's name is not overwritten without `replace`: then nothing
    /// is written and `NameTaken(other id)` is returned. With `replace` the other row is replaced.
    pub fn save_fam_doc_as(
        &mut self,
        id: Option<i64>,
        name: &str,
        def: &str,
        family: Option<i64>,
        replace: bool,
    ) -> Result<DocSave, Error> {
        if name.trim().is_empty() {
            return Err(Error::Invalid("empty family document name".into()));
        }
        let other: Option<i64> = self
            .db
            .query_row(
                "SELECT id FROM fam_doc WHERE name=?1 AND (?2 IS NULL OR id<>?2)",
                params![name, id],
                |r| r.get(0),
            )
            .optional()?;
        if other.is_some() && !replace {
            return Ok(DocSave::NameTaken(other.unwrap_or_default()));
        }
        let tx = self.db.unchecked_transaction()?;
        let own = match id {
            Some(i) => tx
                .query_row("SELECT id FROM fam_doc WHERE id=?1", [i], |r| r.get::<_, i64>(0))
                .optional()?,
            None => None,
        };
        let out = match (own, other) {
            (Some(i), o) => {
                if let Some(o) = o {
                    tx.execute("DELETE FROM fam_doc WHERE id=?1", [o])?;
                }
                tx.execute(
                    "UPDATE fam_doc SET name=?2, def=?3, family=?4 WHERE id=?1",
                    params![i, name, def, family],
                )?;
                i
            }
            (None, Some(o)) => {
                tx.execute(
                    "UPDATE fam_doc SET def=?2, family=?3 WHERE id=?1",
                    params![o, def, family],
                )?;
                o
            }
            (None, None) => {
                tx.execute(
                    "INSERT INTO fam_doc(name, def, family) VALUES (?1,?2,?3)",
                    params![name, def, family],
                )?;
                tx.last_insert_rowid()
            }
        };
        tx.commit()?;
        Ok(DocSave::Saved(out))
    }

    /// Drafts: (id, name, linked family).
    pub fn fam_docs(&self) -> Result<Vec<(i64, String, Option<i64>)>, Error> {
        let mut st = self
            .db
            .prepare("SELECT id, name, family FROM fam_doc ORDER BY name")?;
        let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(r.collect::<Result<_, _>>()?)
    }

    pub fn fam_doc(&self, id: i64) -> Result<String, Error> {
        self.db
            .query_row("SELECT def FROM fam_doc WHERE id=?1", [id], |r| r.get(0))
            .optional()?
            .ok_or_else(|| Error::Invalid(format!("no family document {id}")))
    }

    /// Deletes a draft (outside the project journal, like saving).
    pub fn delete_fam_doc(&mut self, id: i64) -> Result<(), Error> {
        self.db.execute("DELETE FROM fam_doc WHERE id=?1", [id])?;
        Ok(())
    }

    /// RGBA thumbnail n x n, n = `thumb::clamp_size(size)`: cached by (family, size) with the
    /// definition hash (revision), otherwise drawn and cached (a derived cache outside the journal;
    /// a cache write failure is not an error).
    pub fn family_thumbnail(&self, family: i64, size: u32) -> Result<Vec<u8>, Error> {
        let n = thumb::clamp_size(size);
        let def = self.family_def(family)?;
        let hash = def_hash(&def);
        let cached: Option<Vec<u8>> = self
            .db
            .query_row(
                "SELECT rgba FROM comp_thumb WHERE family=?1 AND size=?2 AND hash=?3",
                params![family, n as i64, hash],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(rgba) = cached.filter(|b| b.len() == (n * n * 4) as usize) {
            return Ok(rgba);
        }
        let f = self.family(family)?;
        let img = thumb::thumbnail(&f, n).map_err(fam_err)?;
        let _ = self.db.execute(
            "INSERT OR REPLACE INTO comp_thumb(family, size, hash, rgba) VALUES (?1,?2,?3,?4)",
            params![family, n as i64, hash, img],
        );
        Ok(img)
    }
}

#[cfg(test)]
mod tests {
    use crate::{CompSpec, Model, ParamValue};

    const BOX: &str = "family Crate\ncategory generic\nparam W length type 1m\n\
        ref L x ~ -400mm\nref R x ~ 400mm\nref C x = 0\neq L C R\ndim W L R\n\
        extrude B rect(L, 0, R, 1m) z(0, 1m)\ntype \"Std\" W=1m\n";

    fn model() -> (Model, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let level = m.add_level("L1", 0.0).unwrap();
        (m, level)
    }

    /// Plan with the plain reader (the app passes the per-file rule), then write it.
    fn import(m: &mut Model, family: i64, csv: &str) -> crate::CatalogLoad {
        use ddc_geom::component::catalog;
        let f = m.family(family).unwrap();
        let cur = m.family_types(family).unwrap();
        let sheet = catalog::read_sheet(csv).unwrap();
        let num = catalog::plain_num(&sheet);
        let plan = catalog::plan_import(&f, &cur, &sheet, "mm", &num).unwrap();
        m.import_type_catalog(family, &plan).unwrap()
    }

    #[test]
    fn reload_updates_instances_and_undoes_in_one_step() {
        let (mut m, level) = model();
        assert_eq!(m.schema_version().unwrap(), crate::schema::VERSION);
        let ld = m.load_family_def(BOX, false).unwrap();
        assert!(ld.created);
        let t = m.comp_type_id(ld.family, "Std").unwrap().unwrap();
        let id = m.add_component(&CompSpec::at(level, t, 0.0, 0.0)).unwrap();
        let v0 = m.component_quantities(id).unwrap().volume;
        assert!((v0 - 1.0).abs() < 1e-9, "{v0}");
        // depth 2 m: the instance is rebuilt
        let def2 = BOX.replace("rect(L, 0, R, 1m)", "rect(L, 0, R, 2m)");
        let ld2 = m.load_family_def(&def2, false).unwrap();
        assert!(!ld2.created);
        assert_eq!((ld2.family, ld2.instances), (ld.family, 1));
        let v1 = m.component_quantities(id).unwrap().volume;
        assert!((v1 - 2.0).abs() < 1e-9, "{v1}");
        // type values: without overwrite the project's are kept, with overwrite they come from the text
        let def3 = def2.replace("W=1m", "W=500mm");
        m.load_family_def(&def3, false).unwrap();
        assert!((m.component_quantities(id).unwrap().volume - 2.0).abs() < 1e-9);
        m.load_family_def(&def3, true).unwrap();
        assert!((m.component_quantities(id).unwrap().volume - 1.0).abs() < 1e-9);
        m.undo().unwrap();
        assert!((m.component_quantities(id).unwrap().volume - 2.0).abs() < 1e-9);
        m.undo().unwrap();
        m.undo().unwrap();
        assert!((m.component_quantities(id).unwrap().volume - 1.0).abs() < 1e-9);
        // a definition that breaks an instance is rejected entirely
        let bad = BOX.replace("dim W L R", "dim W L R\ndim 300mm L C");
        assert!(m.load_family_def(&bad, false).is_err());
        assert_eq!(m.family_def(ld.family).unwrap(), BOX);
    }

    #[test]
    fn catalog_docs_and_thumbnails() {
        let (mut m, _) = model();
        let ld = m.load_family_def(BOX, false).unwrap();
        let csv = m.export_type_catalog(ld.family).unwrap();
        assert!(csv.starts_with("Type,"), "{csv}");
        // byte order mark, semicolon separator, a length without a unit is in the project unit
        let n = import(&mut m, ld.family, "\u{feff}Type;W\nStd;1200\nWide;2000\n");
        assert_eq!((n.types, n.warnings.len()), (2, 1));
        let ty = m.family_types(ld.family).unwrap();
        assert_eq!(ty.len(), 2);
        assert_eq!(ty[1].name, "Wide");
        assert!(
            matches!(ty[1].values.as_slice(), [(k, ParamValue::Num(v))] if k == "W" && (v - 2.0).abs() < 1e-9)
        );
        // a bad cell is reported, not fatal; a type with nothing valid to write is not added
        let n = import(&mut m, ld.family, "Type,W (mm)\nWide,abc\n");
        assert_eq!((n.types, n.skipped.len()), (0, 1), "{:?}", n.skipped);
        assert!(n.skipped[0].contains("W"), "{:?}", n.skipped);
        // a draft is outside the project journal: project undo does not roll it back
        let d = m.save_fam_doc("Crate", BOX, Some(ld.family)).unwrap();
        assert_eq!(m.save_fam_doc("Crate", "family Crate\n", None).unwrap(), d);
        assert_eq!(m.fam_doc(d).unwrap(), "family Crate\n");
        m.undo().unwrap();
        assert_eq!(m.fam_doc(d).unwrap(), "family Crate\n");
        assert_eq!(
            m.family_types(ld.family).unwrap().len(),
            1,
            "undo reverted the import"
        );
        assert_eq!(m.fam_docs().unwrap(), vec![(d, "Crate".to_string(), None)]);
        // thumbnail cache is keyed by (family, size); the size is clamped to limits
        let a = m.family_thumbnail(ld.family, 32).unwrap();
        assert_eq!(a.len(), 32 * 32 * 4);
        let b = m.family_thumbnail(ld.family, 64).unwrap();
        assert_eq!(b.len(), 64 * 64 * 4);
        assert_eq!(m.family_thumbnail(ld.family, 32).unwrap(), a);
        assert_eq!(m.family_thumbnail(ld.family, 2).unwrap().len(), 8 * 8 * 4);
        let rows: i64 =
            m.db.query_row(
                "SELECT count(*) FROM comp_thumb WHERE family=?1",
                [ld.family],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rows, 3);
        assert!(a.chunks(4).any(|p| p[3] == 255));
        m.delete_fam_doc(d).unwrap();
        assert!(m.fam_docs().unwrap().is_empty());
    }

    #[test]
    fn catalog_import_merges_and_skips_untouched() {
        let (mut m, _) = model();
        let ld = m.load_family_def(BOX, false).unwrap();
        let csv = m.export_type_catalog(ld.family).unwrap();
        let ops = m.journal_len().unwrap();
        let n = import(&mut m, ld.family, &csv);
        assert_eq!(n.types, 0, "untouched re-import writes nothing");
        assert_eq!(m.journal_len().unwrap(), ops, "no operation for zero changes");
        // empty cell keeps Std's W; only the new type is written, as one undo step
        let n = import(&mut m, ld.family, "Type,W (mm)\nStd,\nWide,2000\n");
        assert_eq!(n.types, 1);
        let ty = m.family_types(ld.family).unwrap();
        assert_eq!(ty.len(), 2);
        assert!(
            matches!(ty[0].values.as_slice(), [(k, ParamValue::Num(v))] if k == "W" && (v - 1.0).abs() < 1e-9)
        );
        m.undo().unwrap();
        assert_eq!(m.family_types(ld.family).unwrap().len(), 1);
    }

    #[test]
    fn catalog_quoted_names_round_trip_without_twins() {
        let (mut m, _) = model();
        let def = BOX.replace("type \"Std\" W=1m", "type \"Pipe 12' long\" W=1m");
        let ld = m.load_family_def(&def, false).unwrap();
        let n = import(&mut m, ld.family, "Type,W (mm)\n\"Rod \"\"A\"\"\",1500\n");
        assert_eq!(n.types, 1);
        let names = |m: &Model| -> Vec<String> {
            m.family_types(ld.family)
                .unwrap()
                .into_iter()
                .map(|t| t.name)
                .collect()
        };
        assert_eq!(names(&m), ["Pipe 12' long", "Rod \"A\""]);
        // export, then re-import untouched: the quoted name is found again, nothing written
        let ops = m.journal_len().unwrap();
        let csv = m.export_type_catalog(ld.family).unwrap();
        assert_eq!(import(&mut m, ld.family, &csv).types, 0, "{csv}");
        assert_eq!(m.journal_len().unwrap(), ops);
        // `"` in the file updates the type stored with `'` instead of adding a twin
        let n = import(&mut m, ld.family, "Type,W (mm)\n\"Pipe 12\"\" long\",2000\n");
        assert_eq!(n.types, 1);
        assert_eq!(names(&m), ["Pipe 12' long", "Rod \"A\""]);
        let ty = m.family_types(ld.family).unwrap();
        assert!(
            matches!(ty[0].values.as_slice(), [(k, ParamValue::Num(v))] if k == "W" && (v - 2.0).abs() < 1e-9)
        );
        // one undo step brings the old value back
        m.undo().unwrap();
        let ty = m.family_types(ld.family).unwrap();
        assert_eq!(ty.len(), 2);
        assert!(
            matches!(ty[0].values.as_slice(), [(k, ParamValue::Num(v))] if k == "W" && (v - 1.0).abs() < 1e-9)
        );
    }

    #[test]
    fn reload_checks_unused_types_and_updates_nested_copies() {
        let (mut m, level) = model();
        let ld = m.load_family_def(BOX, false).unwrap();
        // a project type without instances that the new definition cannot build
        import(&mut m, ld.family, "Type,W (mm)\nTiny,100\n");
        // division by zero at W = 100 mm: Std (1 m) builds, Tiny does not
        let thin = BOX.replace(
            "dim W L R",
            "dim W L R\nparam K length type = 1m * 1m / (W - 100mm)",
        );
        assert!(
            m.load_family_def(&thin, false).is_err(),
            "type Tiny must be validated"
        );
        assert_eq!(m.family_def(ld.family).unwrap(), BOX);
        // a family with an embedded Crate copy is updated from the project one
        let host = format!(
            "family Shelf\ncategory generic\ntype \"One\"\nnest A Crate type(Std) at(0, 0, 0)\nbegin\n{BOX}end\n"
        );
        let sh = m.load_family_def(&host, false).unwrap();
        let t = m.comp_type_id(sh.family, "One").unwrap().unwrap();
        let id = m.add_component(&CompSpec::at(level, t, 0.0, 0.0)).unwrap();
        let v0 = m.component_quantities(id).unwrap().volume;
        let deep = BOX.replace("rect(L, 0, R, 1m)", "rect(L, 0, R, 3m)");
        let ld2 = m.load_family_def(&deep, false).unwrap();
        assert_eq!(ld2.nested, 1);
        assert!(m
            .family_def(sh.family)
            .unwrap()
            .contains("rect(L, 0, R, 3m)"));
        let v1 = m.component_quantities(id).unwrap().volume;
        assert!((v1 - 3.0 * v0).abs() < 1e-6, "{v0} → {v1}");
        // one operation: undo restores the nesting as well
        m.undo().unwrap();
        assert!(!m.family_def(sh.family).unwrap().contains("3m)"));
    }

    #[test]
    fn type_overwrites_counts_then_one_undo_restores() {
        let (mut m, level) = model();
        assert!(m.type_overwrites(BOX).unwrap().is_empty(), "new family: nothing replaced");
        let ld = m.load_family_def(BOX, false).unwrap();
        let t = m.comp_type_id(ld.family, "Std").unwrap().unwrap();
        m.add_component(&CompSpec::at(level, t, 0.0, 0.0)).unwrap();
        m.add_component(&CompSpec::at(level, t, 2.0, 0.0)).unwrap();
        assert!(m.type_overwrites(BOX).unwrap().is_empty(), "same values: no change");
        let wide = BOX.replace("type \"Std\" W=1m", "type \"Std\" W=2m\ntype \"New\" W=3m");
        let ow = m.type_overwrites(&wide).unwrap();
        assert_eq!(
            ow,
            vec![crate::TypeOverwrite {
                name: "Std".into(),
                values: 1,
                instances: 2,
            }],
            "only the existing type counts; a new type replaces nothing"
        );
        // the preview writes nothing
        let w = |m: &Model| match m.family_types(ld.family).unwrap()[0].values.as_slice() {
            [(k, ParamValue::Num(v))] if k == "W" => *v,
            other => panic!("{other:?}"),
        };
        assert!((w(&m) - 1.0).abs() < 1e-9);
        m.load_family_def(&wide, true).unwrap();
        assert!((w(&m) - 2.0).abs() < 1e-9);
        assert!(m.type_overwrites(&wide).unwrap().is_empty(), "applied: nothing left");
        m.undo().unwrap();
        assert!((w(&m) - 1.0).abs() < 1e-9, "one undo step restores the value");
        assert_eq!(m.family_types(ld.family).unwrap().len(), 1);
    }

    fn id_of(r: crate::DocSave) -> i64 {
        match r {
            crate::DocSave::Saved(i) => i,
            other => panic!("expected Saved, got {other:?}"),
        }
    }

    #[test]
    fn save_renamed_draft_updates_own_row() {
        let (mut m, _) = model();
        let a = id_of(m.save_fam_doc_as(None, "A", "da", None, false).unwrap());
        let b = id_of(m.save_fam_doc_as(Some(a), "B", "db", None, false).unwrap());
        assert_eq!(a, b);
        let docs = m.fam_docs().unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].1, "B");
        assert_eq!(m.fam_doc(a).unwrap(), "db");
    }

    #[test]
    fn save_into_taken_name_is_refused_without_change() {
        let (mut m, _) = model();
        let a = id_of(m.save_fam_doc_as(None, "A", "da", None, false).unwrap());
        let b = id_of(m.save_fam_doc_as(None, "B", "db", None, false).unwrap());
        let r = m.save_fam_doc_as(Some(a), "B", "new", None, false).unwrap();
        assert_eq!(r, crate::DocSave::NameTaken(b));
        assert_eq!(m.fam_doc(a).unwrap(), "da");
        assert_eq!(m.fam_doc(b).unwrap(), "db");
        let names: Vec<String> = m.fam_docs().unwrap().into_iter().map(|d| d.1).collect();
        assert_eq!(names, vec!["A".to_string(), "B".to_string()]);
        // a new draft under a taken name is refused too
        let r = m.save_fam_doc_as(None, "A", "x", None, false).unwrap();
        assert_eq!(r, crate::DocSave::NameTaken(a));
        assert_eq!(m.fam_doc(a).unwrap(), "da");
    }

    #[test]
    fn new_draft_insert_keeps_others() {
        let (mut m, _) = model();
        let a = id_of(m.save_fam_doc_as(None, "A", "da", None, false).unwrap());
        let b = id_of(m.save_fam_doc_as(None, "B", "db", None, false).unwrap());
        let c = id_of(m.save_fam_doc_as(None, "C", "dc", None, false).unwrap());
        assert!(a != b && b != c && a != c);
        assert_eq!(m.fam_docs().unwrap().len(), 3);
        assert_eq!(m.fam_doc(a).unwrap(), "da");
        assert_eq!(m.fam_doc(b).unwrap(), "db");
        assert_eq!(m.fam_doc(c).unwrap(), "dc");
    }

    #[test]
    fn replace_choice_overwrites_only_after_confirm() {
        let (mut m, _) = model();
        let a = id_of(m.save_fam_doc_as(None, "A", "da", None, false).unwrap());
        let b = id_of(m.save_fam_doc_as(None, "B", "db", None, false).unwrap());
        assert_eq!(
            m.save_fam_doc_as(Some(a), "B", "new", None, false).unwrap(),
            crate::DocSave::NameTaken(b)
        );
        assert_eq!(m.fam_doc(b).unwrap(), "db");
        let r = id_of(m.save_fam_doc_as(Some(a), "B", "new", None, true).unwrap());
        assert_eq!(r, a);
        let docs = m.fam_docs().unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].1, "B");
        assert_eq!(m.fam_doc(a).unwrap(), "new");
        assert!(m.fam_doc(b).is_err());
    }
}
