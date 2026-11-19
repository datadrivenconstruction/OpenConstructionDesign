// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The change record: one per-element list of what differs from the imported source, keyed by the
//! source's stable id. Derived from `src_base` against the current model (`local_changes`), never
//! logged, so an undone edit leaves no trace. Runs on a read-only snapshot.
//! Its JSON form, `ddc-change-package/1`, is documented in docs/plugins/CHANGE_PACKAGE.md.

use std::collections::BTreeMap;

use rusqlite::types::Value as Sql;
use serde_json::{json, Value as J};
use sha2::{Digest, Sha256};

use super::{enc, ident, props_of, ChangeKind, DELETABLE};
use crate::{Error, Model};

pub const PACKAGE_FORMAT: &str = "ddc-change-package/1";

/// Kinds whose rows without a source link count as added.
pub const TRACKED: [&str; 20] = [
    "solid",
    "wall",
    "opening",
    "slab",
    "room",
    "mesh",
    "curtain_wall",
    "grid",
    "col",
    "beam",
    "ceiling",
    "comp",
    "room_sep",
    "roof",
    "level",
    "opening_fill",
    "mep_run",
    "stair",
    "ramp",
    "railing",
];

const TYPE_ROWS: [&str; 4] = ["wall_type", "slab_type", "member_type", "comp_type"];

#[derive(Clone, Debug, PartialEq)]
pub struct RecordField {
    /// `height`, `p:Mark`, `p:type.Mark`, ...
    pub field: String,
    /// Parameter name, for `p:` fields.
    pub param: Option<String>,
    /// "instance" or "type", for `p:` fields.
    pub scope: Option<&'static str>,
    pub unit: String,
    /// `Null` when `base_known` is false: an unedited parameter value is stored only as a hash.
    pub base: J,
    pub base_known: bool,
    pub mine: J,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecordEntry {
    /// Stable id from the source. Empty on an element added here.
    pub guid: String,
    pub ifc_guid: Option<String>,
    pub kind: String,
    /// Local row, 0 once deleted.
    pub id: i64,
    pub change: ChangeKind,
    pub fields: Vec<RecordField>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChangeRecord {
    pub source: String,
    pub name: String,
    pub format: String,
    /// SHA-256 of the file as imported. Empty when the import recorded none.
    pub source_sha256: String,
    pub reader: String,
    pub reader_version: String,
    pub entries: Vec<RecordEntry>,
}

/// Splits a `p:` field into parameter name and scope. A type row or a `type.` key is type scope.
fn param_of(kind: &str, field: &str) -> (Option<String>, Option<&'static str>) {
    let Some(key) = field.strip_prefix("p:") else {
        return (None, None);
    };
    match key.strip_prefix("type.") {
        Some(bare) => (Some(bare.to_string()), Some("type")),
        None if TYPE_ROWS.contains(&kind) => (Some(key.to_string()), Some("type")),
        None => (Some(key.to_string()), Some("instance")),
    }
}

fn table_exists(m: &Model, table: &str) -> Result<bool, Error> {
    let n: i64 = m.db.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [table],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Whole row and parameters of an element added here. Blobs become hashes in the JSON form.
fn added_fields(m: &Model, kind: &str, id: i64) -> Result<Vec<RecordField>, Error> {
    let mut st =
        m.db.prepare(&format!("SELECT * FROM {} WHERE id=?1", ident(kind)?))?;
    let names: Vec<String> = st.column_names().map(|s| s.to_string()).collect();
    let values: Vec<Sql> = st.query_row([id], |r| {
        (0..names.len())
            .map(|i| r.get::<_, Sql>(i))
            .collect::<Result<Vec<_>, _>>()
    })?;
    let mut out: Vec<RecordField> = names
        .into_iter()
        .zip(values)
        .filter(|(n, _)| n != "id")
        .map(|(field, v)| RecordField {
            field,
            param: None,
            scope: None,
            unit: String::new(),
            base: J::Null,
            base_known: false,
            mine: match &v {
                Sql::Blob(b) => blob_json(b),
                other => enc(other),
            },
        })
        .collect();
    let props: BTreeMap<String, (J, String)> = props_of(&m.db, kind, id)?.into_iter().collect();
    for (key, (value, unit)) in props {
        let field = format!("p:{key}");
        let (param, scope) = param_of(kind, &field);
        out.push(RecordField {
            field,
            param,
            scope,
            unit,
            base: J::Null,
            base_known: false,
            mine: value,
        });
    }
    Ok(out)
}

fn blob_json(b: &[u8]) -> J {
    let hash: String = Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect();
    json!({ "blob_sha256": hash, "bytes": b.len() })
}

/// Blob markers of `enc` hold a NUL: they leave the JSON form as a small object.
fn clean(j: &J) -> J {
    match j {
        J::String(s) if s.starts_with('\u{0}') => json!({ "blob": s.trim_start_matches('\u{0}') }),
        other => other.clone(),
    }
}

impl Model {
    /// The record of `source`: edits and deletions from the base, plus added rows of every
    /// [`TRACKED`] kind. Read-only.
    pub fn change_record(&self, source: &str) -> Result<ChangeRecord, Error> {
        let mut rec = ChangeRecord {
            source: source.to_string(),
            ..Default::default()
        };
        if let Some(i) = self.source_info(source)? {
            rec.name = std::path::Path::new(&i.path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            rec.format = i.format;
            rec.reader = i.reader;
            rec.reader_version = i.reader_version;
        }
        if let Some(f) = self.source_file(source)? {
            rec.source_sha256 = f.content_hash;
        }
        for c in self.local_changes(source, false)? {
            let units = if c.id != 0 {
                props_of(&self.db, &c.kind, c.id)?
            } else {
                Default::default()
            };
            let fields = c
                .fields
                .iter()
                .map(|d| {
                    let (param, scope) = param_of(&c.kind, &d.field);
                    let unit = d
                        .field
                        .strip_prefix("p:")
                        .and_then(|k| units.get(k))
                        .map_or_else(String::new, |(_, u)| u.clone());
                    RecordField {
                        field: d.field.clone(),
                        param,
                        scope,
                        unit,
                        base_known: !d.base.is_null(),
                        base: d.base.clone(),
                        mine: d.mine.clone(),
                    }
                })
                .collect();
            rec.entries.push(RecordEntry {
                guid: c.guid,
                ifc_guid: c.ifc_guid,
                kind: c.kind,
                id: c.id,
                change: c.change,
                fields,
            });
        }
        debug_assert!(DELETABLE.iter().all(|k| TRACKED.contains(k)));
        for kind in TRACKED {
            if !table_exists(self, kind)? {
                continue;
            }
            let mut st = self.db.prepare(&format!(
                "SELECT id FROM {kind} WHERE id NOT IN (SELECT ref FROM ext_ref WHERE kind='{kind}') ORDER BY id"
            ))?;
            let ids: Vec<i64> = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
            for id in ids {
                rec.entries.push(RecordEntry {
                    guid: String::new(),
                    ifc_guid: None,
                    kind: kind.to_string(),
                    id,
                    change: ChangeKind::AddedLocally,
                    fields: added_fields(self, kind, id)?,
                });
            }
        }
        Ok(rec)
    }
}

impl ChangeRecord {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `ddc-change-package/1`. Stable key order, no timestamps: the same record gives equal text.
    pub fn to_json(&self) -> J {
        let entries: Vec<J> = self
            .entries
            .iter()
            .map(|e| {
                let fields: Vec<J> = e
                    .fields
                    .iter()
                    .map(|f| {
                        json!({
                            "field": f.field,
                            "parameter": f.param,
                            "scope": f.scope,
                            "unit": f.unit,
                            "base": clean(&f.base),
                            "base_known": f.base_known,
                            "mine": clean(&f.mine),
                        })
                    })
                    .collect();
                json!({
                    "key": { "guid": e.guid, "ifc_guid": e.ifc_guid },
                    "kind": e.kind,
                    "local_id": e.id,
                    "change": e.change.key(),
                    "fields": fields,
                })
            })
            .collect();
        json!({
            "format": PACKAGE_FORMAT,
            "source": {
                "name": self.name,
                "format": self.format,
                "sha256": self.source_sha256,
                "reader": self.reader,
                "reader_version": self.reader_version,
            },
            "entries": entries,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LayerSpec, WallSpec};
    use rusqlite::params;

    const SRC: &str = "b|file";

    fn model() -> (Model, i64, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m
            .add_wall_type(
                "T",
                &[LayerSpec {
                    material: "m".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let w = m
            .add_wall(&WallSpec {
                a: (0.0, 0.0),
                b: (4.0, 0.0),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap();
        for (g, k, r) in [("g1", "wall", w), ("l1", "level", lv)] {
            m.db.execute(
                "INSERT INTO ext_ref(source,guid,kind,ref) VALUES (?1,?2,?3,?4)",
                params![SRC, g, k, r],
            )
            .unwrap();
        }
        (m, w, lv)
    }

    /// A base row for wall g1 with these JSON values and no hashed fields.
    fn base(m: &Model, vals: &J) {
        m.db.execute(
            "INSERT INTO src_base(source,guid,kind,vals,ifc_guid) VALUES (?1,'g1','wall',?2,'0abc')",
            params![SRC, vals.to_string()],
        )
        .unwrap();
    }

    fn set_height(m: &Model, w: i64, h: f64) {
        m.db.execute("UPDATE wall SET height=?1 WHERE id=?2", params![h, w])
            .unwrap();
    }

    #[test]
    fn an_edit_then_its_undo_leaves_an_empty_list() {
        let (m, w, _) = model();
        base(&m, &json!({ "height": 3.0 }));
        assert!(m.change_record(SRC).unwrap().is_empty());
        set_height(&m, w, 3.5);
        let r = m.change_record(SRC).unwrap();
        assert_eq!(r.entries.len(), 1);
        assert_eq!(r.entries[0].guid, "g1");
        assert_eq!(r.entries[0].fields[0].field, "height");
        assert_eq!(r.entries[0].fields[0].mine, json!(3.5));
        set_height(&m, w, 3.0);
        assert!(m.change_record(SRC).unwrap().is_empty(), "no log, no trace");
    }

    #[test]
    fn a_deleted_element_keeps_its_key() {
        let (m, w, _) = model();
        base(&m, &json!({ "height": 3.0 }));
        m.db.execute("DELETE FROM wall WHERE id=?1", [w]).unwrap();
        m.db.execute("DELETE FROM ext_ref WHERE guid='g1'", [])
            .unwrap();
        let r = m.change_record(SRC).unwrap();
        let e = r.entries.iter().find(|e| e.guid == "g1").unwrap();
        assert_eq!(e.change, ChangeKind::DeletedLocally);
        assert_eq!(e.ifc_guid.as_deref(), Some("0abc"));
    }

    #[test]
    fn added_covers_kinds_beyond_the_deletable_list() {
        let (m, _, lv) = model();
        base(&m, &json!({ "height": 3.0 }));
        // `level` is tracked but not deletable by a re-import
        m.db.execute("DELETE FROM ext_ref WHERE guid='l1'", [])
            .unwrap();
        let r = m.change_record(SRC).unwrap();
        let e = r
            .entries
            .iter()
            .find(|e| e.kind == "level" && e.id == lv)
            .unwrap();
        assert_eq!(e.change, ChangeKind::AddedLocally);
        assert!(e
            .fields
            .iter()
            .any(|f| f.field == "name" && f.mine == json!("L1")));
    }

    #[test]
    fn parameter_fields_name_their_scope() {
        assert_eq!(
            param_of("wall", "p:Mark"),
            (Some("Mark".into()), Some("instance"))
        );
        assert_eq!(
            param_of("wall", "p:type.Mark"),
            (Some("Mark".into()), Some("type"))
        );
        assert_eq!(
            param_of("wall_type", "p:Mark"),
            (Some("Mark".into()), Some("type"))
        );
        assert_eq!(param_of("wall", "height"), (None, None));
    }

    #[test]
    fn a_mesh_blob_leaves_the_json_form_as_a_hash() {
        let j = blob_json(&[1, 2, 3]);
        assert_eq!(j["bytes"], json!(3));
        assert_eq!(j["blob_sha256"].as_str().unwrap().len(), 64);
        let marker = enc(&Sql::Blob(vec![1, 2, 3]));
        assert!(!clean(&marker).to_string().contains("\\u0000"));
    }

    #[test]
    fn the_json_form_is_stable_and_has_no_time() {
        let (m, w, _) = model();
        base(&m, &json!({ "height": 3.0 }));
        set_height(&m, w, 3.5);
        let a = m.change_record(SRC).unwrap().to_json();
        let b = m.change_record(SRC).unwrap().to_json();
        assert_eq!(a, b);
        assert_eq!(a["format"], json!(PACKAGE_FORMAT));
        assert_eq!(a["entries"][0]["key"]["guid"], json!("g1"));
        assert_eq!(a["entries"][0]["change"], json!("edited"));
    }
}
