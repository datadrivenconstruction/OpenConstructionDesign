//! XLSX delta of local changes against an imported source: one row per changed field (edits),
//! one row per deleted or added element. Rows carry the source unique id and the IFC GlobalId
//! so pro BIM tools can apply the changes to the source project; references to levels, types
//! and hosts are written as the source ids (or names) of those rows, numbers as numbers.

use std::path::Path;

use ddc_model::source::{show, Change, ChangeKind, JsonValue as J};
use ddc_model::Model;
use rusqlite::{params, OptionalExtension};

use super::{export_table, TableCell};

pub const DELTA_HEADERS: [&str; 10] = [
    "source_id",
    "ifc_guid",
    "category",
    "element",
    "change",
    "field",
    "parameter",
    "imported",
    "current",
    "unit",
];

/// Spreadsheet cells hold at most this many characters (contours are cut with "…").
const MAX_CELL: usize = 32_000;

/// Table a reference column of `kind` points to (None: not a reference).
pub fn ref_table(kind: &str, field: &str) -> Option<&'static str> {
    Some(match (kind, field) {
        (_, "level" | "top_level" | "base_level") => "level",
        ("wall", "wtype") => "wall_type",
        ("slab" | "ceiling" | "roof", "stype") => "slab_type",
        ("col" | "beam", "mtype") => "member_type",
        ("comp", "ctype") => "comp_type",
        ("opening", "wall") | ("comp", "host_wall") => "wall",
        ("comp", "host_slab") => "slab",
        ("comp", "host_ceiling") => "ceiling",
        _ => return None,
    })
}

/// SI unit of a native length column (parameters carry their own).
fn column_unit(field: &str) -> &'static str {
    match field {
        "height" | "base_offset" | "top_offset" | "offset" | "ax" | "ay" | "az" | "bx" | "by"
        | "bz" | "x" | "y" | "z" | "px" | "py" | "width" | "sill" | "elevation" | "thickness"
        | "along" | "dx" | "dy" | "dz" => "m",
        "rotation" => "rad",
        _ => "",
    }
}

fn cell(j: &J) -> TableCell {
    match j {
        J::Number(n) => n
            .as_f64()
            .map_or_else(|| TableCell::Text(n.to_string()), TableCell::Num),
        J::Bool(b) => TableCell::Num(*b as i64 as f64),
        other => {
            let mut s = show(other);
            if s.chars().count() > MAX_CELL {
                s = s.chars().take(MAX_CELL).collect::<String>() + "…";
            }
            TableCell::Text(s)
        }
    }
}

/// Source id (else name) of the row a reference value points to.
type Lookup<'a> = dyn Fn(&str, i64) -> Option<String> + 'a;
/// Unit of parameter `key` of element (kind, id).
type UnitOf<'a> = dyn Fn(&str, i64, &str) -> Option<String> + 'a;

/// Table rows of `changes` (headers [`DELTA_HEADERS`]), without a model: references stay ids.
pub fn delta_rows(changes: &[Change]) -> Vec<Vec<TableCell>> {
    rows_with(changes, &|_, _| None, &|_, _, _| None)
}

fn rows_with(changes: &[Change], look: &Lookup, unit_of: &UnitOf) -> Vec<Vec<TableCell>> {
    let t = |s: &str| TableCell::Text(s.to_string());
    let mut out = vec![];
    for c in changes {
        let head = [
            t(&c.guid),
            t(c.ifc_guid.as_deref().unwrap_or("")),
            t(&c.kind),
            TableCell::Num(c.id as f64),
            t(c.change.key()),
        ];
        if c.change != ChangeKind::Edited || c.fields.is_empty() {
            let mut r = head.to_vec();
            r.extend([t(""), t(""), t(""), t(""), t("")]);
            out.push(r);
            continue;
        }
        for f in &c.fields {
            let (field, param) = match f.field.strip_prefix("p:") {
                Some(k) => ("", k),
                None => (f.field.as_str(), ""),
            };
            let value = |j: &J| -> TableCell {
                let target = ref_table(&c.kind, field);
                match (target, j.as_i64()) {
                    (Some(table), Some(id)) => {
                        look(table, id).map_or_else(|| cell(j), TableCell::Text)
                    }
                    _ => cell(j),
                }
            };
            let unit = if !param.is_empty() {
                unit_of(&c.kind, c.id, param).unwrap_or_default()
            } else {
                column_unit(field).to_string()
            };
            let mut r = head.to_vec();
            r.extend([
                t(field),
                t(param),
                value(&f.base),
                value(&f.mine),
                TableCell::Text(unit),
            ]);
            out.push(r);
        }
    }
    out
}

/// Rows with references resolved through `model`: the referenced row's id in `source`, else
/// its name; parameter units from the model.
pub fn delta_rows_for(model: &Model, source: &str, changes: &[Change]) -> Vec<Vec<TableCell>> {
    let db = model.sql();
    let look = |table: &str, id: i64| -> Option<String> {
        let g: Option<String> = db
            .query_row(
                "SELECT guid FROM ext_ref WHERE source=?1 AND kind=?2 AND ref=?3",
                params![source, table, id],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten();
        g.or_else(|| {
            db.query_row(
                &format!("SELECT name FROM {table} WHERE id=?1"),
                [id],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
        })
    };
    let unit_of = |kind: &str, id: i64, key: &str| -> Option<String> {
        db.query_row(
            "SELECT unit FROM prop WHERE kind=?1 AND ref=?2 AND key=?3",
            params![kind, id, key],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()
    };
    rows_with(changes, &look, &unit_of)
}

/// Writes the delta of `source` (with locally added elements when `added`); returns the
/// number of changed elements.
pub fn export_delta(
    model: &Model,
    source: &str,
    added: bool,
    path: &Path,
) -> Result<usize, String> {
    let changes = model
        .local_changes(source, added)
        .map_err(|e| e.to_string())?;
    let headers: Vec<String> = DELTA_HEADERS.iter().map(|h| h.to_string()).collect();
    let rows = delta_rows_for(model, source, &changes);
    export_table(path, "delta", &headers, &[], &rows, None)?;
    Ok(changes.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::source::FieldDiff;
    use serde_json::json;

    fn change(kind: ChangeKind, fields: Vec<FieldDiff>) -> Change {
        Change {
            source: "b|f".into(),
            guid: "g1".into(),
            ifc_guid: Some("0YvctVUKr0kugbFTf53O9L".into()),
            kind: "wall".into(),
            id: 7,
            change: kind,
            fields,
        }
    }

    fn diff(field: &str, base: J, mine: J) -> FieldDiff {
        FieldDiff {
            field: field.into(),
            base,
            mine,
        }
    }

    #[test]
    fn one_row_per_field_and_per_deleted_element() {
        let edit = change(
            ChangeKind::Edited,
            vec![
                diff("height", json!(3.0), json!(3.5)),
                diff("p:Fire rating", json!("EI 60"), json!("EI 90")),
            ],
        );
        let rows = delta_rows(&[edit, change(ChangeKind::DeletedLocally, vec![])]);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| r.len() == DELTA_HEADERS.len()));
        assert_eq!(rows[0][5], TableCell::Text("height".into()));
        assert_eq!(
            (&rows[0][7], &rows[0][8]),
            (&TableCell::Num(3.0), &TableCell::Num(3.5))
        );
        assert_eq!(rows[0][9], TableCell::Text("m".into()));
        assert_eq!(rows[1][6], TableCell::Text("Fire rating".into()));
        assert_eq!(rows[1][8], TableCell::Text("EI 90".into()));
        assert_eq!(rows[2][4], TableCell::Text("deleted".into()));
        assert_eq!(rows[2][1], TableCell::Text("0YvctVUKr0kugbFTf53O9L".into()));
    }

    #[test]
    fn references_are_written_as_source_ids_and_long_text_is_cut() {
        let edit = change(
            ChangeKind::Edited,
            vec![
                diff("level", json!(1), json!(2)),
                diff("rings", json!("x"), json!("1,2 ".repeat(20_000))),
            ],
        );
        let look = |table: &str, id: i64| (table == "level").then(|| format!("L{id}"));
        let rows = rows_with(&[edit], &look, &|_, _, _| None);
        assert_eq!(
            (&rows[0][7], &rows[0][8]),
            (&TableCell::Text("L1".into()), &TableCell::Text("L2".into()))
        );
        let TableCell::Text(s) = &rows[1][8] else {
            panic!()
        };
        assert!(s.chars().count() <= MAX_CELL + 1);
    }
}
