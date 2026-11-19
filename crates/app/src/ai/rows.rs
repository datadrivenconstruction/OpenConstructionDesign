// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Element rows the AI queries: the palette rows plus categories the palette cannot select
//! (roofs, stairs, railings have no `Sel` of their own), with project parameters as props.

use super::sets::{Key, Marks};
use crate::editor::bodies::BodyKind;
use crate::editor::{Editor, Sel};
use crate::query::{self, Row};
use ddc_model::Model;
use rusqlite::types::Value as Sql;
use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

/// `Sel::Ext` codes of rows that never reach a selection: only the AI thread sees them.
const EXT_ROOF: u16 = 60_001;
const EXT_STAIR: u16 = 60_002;
const EXT_RAILING: u16 = 60_003;

type CacheKey = (usize, u64, u64);

thread_local! {
    static CACHE: RefCell<Option<(CacheKey, Rc<Vec<Row>>)>> = const { RefCell::new(None) };
}

fn cache_key(ed: &Editor) -> CacheKey {
    let c = ed.model.sql();
    (
        c as *const rusqlite::Connection as usize,
        c.total_changes(),
        ed.revision,
    )
}

pub fn rows(ed: &Editor) -> Rc<Vec<Row>> {
    let key = cache_key(ed);
    if let Some(r) = CACHE.with(|c| {
        c.borrow()
            .as_ref()
            .filter(|x| x.0 == key)
            .map(|x| x.1.clone())
    }) {
        return r;
    }
    let mut out: Vec<Row> = query::rows(ed).iter().cloned().collect();
    extend(&ed.model, &mut out);
    merge_params(&ed.model, &mut out);
    let r = Rc::new(out);
    CACHE.with(|c| *c.borrow_mut() = Some((key, r.clone())));
    r
}

pub fn key_of(r: &Row) -> Key {
    Key {
        cat: r.cat,
        id: r.id(),
    }
}

/// What a frozen key remembers of its element: another element on a reused rowid usually differs.
pub fn mark_of(r: &Row) -> u32 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    r.cat.hash(&mut h);
    r.ty.hash(&mut h);
    r.level.hash(&mut h);
    h.finish() as u32 | 1
}

pub fn marks(ed: &Editor) -> Marks {
    rows(ed)
        .iter()
        .map(|r| ((key_of(r).table(), r.id()), (r.cat, mark_of(r))))
        .collect()
}

/// Selection of a key; `None` for categories the assign logic does not know.
pub fn sel_of(k: Key) -> Option<Sel> {
    Some(match k.cat {
        "wall" => Sel::Wall(k.id),
        "door" | "window" | "opening" => Sel::Opening(k.id),
        "curtain" => Sel::Curtain(k.id),
        "floor" => Sel::Slab(k.id),
        "room" => Sel::Room(k.id),
        "mep" => Sel::Mep(k.id),
        "comp" => Sel::Comp(k.id),
        "column" => Sel::Body(BodyKind::Column, k.id),
        "beam" => Sel::Body(BodyKind::Beam, k.id),
        "ceiling" => Sel::Body(BodyKind::Ceiling, k.id),
        "imported" => Sel::Body(BodyKind::Mesh, k.id),
        "roof" => Sel::Ext(EXT_ROOF, k.id),
        "stair" => Sel::Ext(EXT_STAIR, k.id),
        "railing" => Sel::Ext(EXT_RAILING, k.id),
        _ => return None,
    })
}

/// Key of a live selection; the window's own `Sel::Ext` kinds have no AI category.
pub fn key_of_sel(s: Sel, door: impl Fn(i64) -> &'static str) -> Option<Key> {
    Some(match s {
        Sel::Wall(id) => Key { cat: "wall", id },
        Sel::Opening(id) => Key { cat: door(id), id },
        Sel::Curtain(id) | Sel::Panel(id, _) => Key { cat: "curtain", id },
        Sel::Slab(id) => Key { cat: "floor", id },
        Sel::Room(id) => Key { cat: "room", id },
        Sel::Mep(id) => Key { cat: "mep", id },
        Sel::Comp(id) => Key { cat: "comp", id },
        Sel::Body(BodyKind::Column, id) => Key { cat: "column", id },
        Sel::Body(BodyKind::Beam, id) => Key { cat: "beam", id },
        Sel::Body(BodyKind::Ceiling, id) => Key { cat: "ceiling", id },
        Sel::Body(BodyKind::Mesh, id) => Key {
            cat: "imported",
            id,
        },
        Sel::Mass(_) | Sel::Ext(..) | Sel::Anno(_) => return None,
    })
}

pub(crate) fn sql_rows<T>(
    m: &Model,
    sql: &str,
    f: impl Fn(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> Vec<T> {
    let Ok(mut st) = m.sql().prepare(sql) else {
        return vec![];
    };
    let Ok(it) = st.query_map([], |r| f(r)) else {
        return vec![];
    };
    it.flatten().collect()
}

fn row(
    sel: Sel,
    cat: &'static str,
    level: i64,
    ty: String,
    name: String,
    nums: Vec<(&'static str, f64)>,
) -> Row {
    Row {
        sel,
        cat,
        level,
        ty,
        name,
        mark: String::new(),
        nums,
        props: vec![],
    }
}

fn nums(pairs: [(&'static str, Option<f64>); 2]) -> Vec<(&'static str, f64)> {
    pairs
        .into_iter()
        .filter_map(|(k, v)| v.map(|x| (k, x)))
        .collect()
}

fn extend(m: &Model, out: &mut Vec<Row>) {
    let member = |table: &str, kind: &str| {
        format!(
            "SELECT e.id, e.level, t.name, q.volume, q.surface FROM {table} e
             JOIN member_type t ON t.id=e.mtype
             LEFT JOIN member_qty q ON q.kind='{kind}' AND q.ref=e.id ORDER BY e.id"
        )
    };
    out.extend(sql_rows(m, &member("col", "column"), |r| {
        let n = nums([("volume", r.get(3)?), ("area", r.get(4)?)]);
        Ok(row(
            Sel::Body(BodyKind::Column, r.get(0)?),
            "column",
            r.get(1)?,
            r.get(2)?,
            String::new(),
            n,
        ))
    }));
    out.extend(sql_rows(m, &member("beam", "beam"), |r| {
        let n = nums([("volume", r.get(3)?), ("area", r.get(4)?)]);
        Ok(row(
            Sel::Body(BodyKind::Beam, r.get(0)?),
            "beam",
            r.get(1)?,
            r.get(2)?,
            String::new(),
            n,
        ))
    }));
    out.extend(sql_rows(
        m,
        "SELECT e.id, e.level, t.name,
            (SELECT sum(volume) FROM roof_qty WHERE roof=e.id),
            (SELECT max(area) FROM roof_qty WHERE roof=e.id)
         FROM roof e JOIN slab_type t ON t.id=e.stype ORDER BY e.id",
        |r| {
            let n = nums([("volume", r.get(3)?), ("area", r.get(4)?)]);
            Ok(row(
                Sel::Ext(EXT_ROOF, r.get(0)?),
                "roof",
                r.get(1)?,
                r.get(2)?,
                String::new(),
                n,
            ))
        },
    ));
    out.extend(sql_rows(
        m,
        "SELECT e.id, e.level, t.name,
            (SELECT sum(volume) FROM ceiling_qty WHERE ceiling=e.id),
            (SELECT max(area) FROM ceiling_qty WHERE ceiling=e.id)
         FROM ceiling e JOIN slab_type t ON t.id=e.stype ORDER BY e.id",
        |r| {
            let n = nums([("volume", r.get(3)?), ("area", r.get(4)?)]);
            Ok(row(
                Sel::Body(BodyKind::Ceiling, r.get(0)?),
                "ceiling",
                r.get(1)?,
                r.get(2)?,
                String::new(),
                n,
            ))
        },
    ));
    out.extend(sql_rows(
        m,
        "SELECT e.id, e.level, e.material, q.volume FROM stair e
         LEFT JOIN stair_qty q ON q.stair=e.id ORDER BY e.id",
        |r| {
            let n = nums([("volume", r.get(3)?), ("area", None)]);
            Ok(row(
                Sel::Ext(EXT_STAIR, r.get(0)?),
                "stair",
                r.get(1)?,
                r.get(2)?,
                String::new(),
                n,
            ))
        },
    ));
    out.extend(sql_rows(
        m,
        "SELECT e.id, e.level, e.material, q.length, q.volume FROM railing e
         LEFT JOIN railing_qty q ON q.railing=e.id ORDER BY e.id",
        |r| {
            let n = nums([("length", r.get(3)?), ("volume", r.get(4)?)]);
            Ok(row(
                Sel::Ext(EXT_RAILING, r.get(0)?),
                "railing",
                r.get(1)?,
                r.get(2)?,
                String::new(),
                n,
            ))
        },
    ));
    out.extend(sql_rows(
        m,
        "SELECT e.id, e.level, s.name FROM mep_run e JOIN mep_system s ON s.id=e.system ORDER BY e.id",
        |r| Ok(row(Sel::Mep(r.get(0)?), "mep", r.get(1)?, r.get(2)?, String::new(), vec![])),
    ));
    out.extend(sql_rows(
        m,
        "SELECT e.id, e.level, f.name || ': ' || t.name FROM comp e
         JOIN comp_type t ON t.id=e.ctype JOIN comp_family f ON f.id=t.family ORDER BY e.id",
        |r| {
            Ok(row(
                Sel::Comp(r.get(0)?),
                "comp",
                r.get(1)?,
                r.get(2)?,
                String::new(),
                vec![],
            ))
        },
    ));
    out.extend(sql_rows(
        m,
        "SELECT id, coalesce(level, 0), category, name FROM mesh ORDER BY id",
        |r| {
            Ok(row(
                Sel::Body(BodyKind::Mesh, r.get(0)?),
                "imported",
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                vec![],
            ))
        },
    ));
}

fn sql_text(v: Sql) -> Option<String> {
    match v {
        Sql::Integer(i) => Some(i.to_string()),
        Sql::Real(x) => Some(x.to_string()),
        Sql::Text(s) => Some(s),
        Sql::Null | Sql::Blob(_) => None,
    }
}

/// Tables whose rows are elements the person can see in a plan card.
pub const ELEMENT_TABLES: [&str; 14] = [
    "wall",
    "opening",
    "curtain_wall",
    "slab",
    "room",
    "col",
    "beam",
    "roof",
    "ceiling",
    "stair",
    "railing",
    "mep_run",
    "comp",
    "mesh",
];

const TYPE_COLS: [(&str, &str, &str); 7] = [
    ("wall", "wtype", "wall_type"),
    ("slab", "stype", "slab_type"),
    ("roof", "stype", "slab_type"),
    ("ceiling", "stype", "slab_type"),
    ("col", "mtype", "member_type"),
    ("beam", "mtype", "member_type"),
    ("comp", "ctype", "comp_type"),
];

/// A filter reads a project parameter by its lowercase name, like an imported property.
fn merge_params(m: &Model, out: &mut [Row]) {
    let vals = sql_rows(
        m,
        "SELECT p.name, v.kind, v.ref, v.value, p.per_type FROM pparam_val v JOIN pparam p ON p.id=v.param",
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Sql>(3)?,
                r.get::<_, i64>(4)? != 0,
            ))
        },
    );
    if vals.is_empty() {
        return;
    }
    let at: HashMap<(&'static str, i64), usize> = out
        .iter()
        .enumerate()
        .map(|(i, r)| ((key_of(r).table(), r.id()), i))
        .collect();
    let mut by_type: HashMap<(&'static str, i64), Vec<usize>> = HashMap::new();
    if vals.iter().any(|v| v.4) {
        for (table, col, ty) in TYPE_COLS {
            let pairs = sql_rows(m, &format!("SELECT id, {col} FROM {table}"), |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
            });
            for (id, t) in pairs {
                if let Some(&i) = at.get(&(table, id)) {
                    by_type.entry((ty, t)).or_default().push(i);
                }
            }
        }
    }
    for (name, kind, id, value, per_type) in vals {
        let Some(text) = sql_text(value) else {
            continue;
        };
        let key = name.to_lowercase();
        if per_type {
            let ty = TYPE_COLS.iter().find(|t| t.2 == kind).map(|t| t.2);
            if let Some(list) = ty.and_then(|t| by_type.get(&(t, id))) {
                for &i in list {
                    out[i].props.push((key.clone(), text.clone()));
                }
            }
        } else if let Some(t) = ELEMENT_TABLES.iter().find(|t| **t == kind) {
            if let Some(&i) = at.get(&(*t, id)) {
                out[i].props.push((key, text));
            }
        }
    }
}

/// One element's share of a material.
#[derive(Clone, Debug, PartialEq)]
pub struct ElemMaterial {
    pub table: &'static str,
    pub id: i64,
    pub material: String,
    pub volume: f64,
    pub area: f64,
}

/// Material quantities per element, from the same derived tables as `Model::material_totals`,
/// so a grouped answer adds up to the project total.
pub fn element_materials(m: &Model) -> Vec<ElemMaterial> {
    let mut out = vec![];
    let sources: [(&'static str, &str); 7] = [
        (
            "wall",
            "SELECT wall, material, volume, plan_area FROM wall_qty",
        ),
        ("slab", "SELECT slab, material, volume, area FROM slab_qty"),
        ("roof", "SELECT roof, material, volume, area FROM roof_qty"),
        (
            "ceiling",
            "SELECT ceiling, material, volume, area FROM ceiling_qty",
        ),
        (
            "col",
            "SELECT e.id, t.material, q.volume, q.surface FROM member_qty q
             JOIN col e ON q.kind='column' AND e.id=q.ref JOIN member_type t ON t.id=e.mtype",
        ),
        (
            "beam",
            "SELECT e.id, t.material, q.volume, q.surface FROM member_qty q
             JOIN beam e ON q.kind='beam' AND e.id=q.ref JOIN member_type t ON t.id=e.mtype",
        ),
        (
            "stair",
            "SELECT e.id, e.material, q.volume, 0.0 FROM stair_qty q JOIN stair e ON e.id=q.stair",
        ),
    ];
    for (table, sql) in sources {
        out.extend(sql_rows(m, sql, |r| {
            Ok(ElemMaterial {
                table,
                id: r.get(0)?,
                material: r.get(1)?,
                volume: r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                area: r.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
            })
        }));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor() -> Editor {
        Editor::new_in_memory().unwrap()
    }

    #[test]
    fn extra_categories_appear_and_the_palette_rows_stay() {
        let mut ed = editor();
        let l = ed.level;
        let t = ed.types[0].id;
        ed.model
            .add_walls(&[ddc_model::WallSpec {
                a: (0.0, 0.0),
                b: (4.0, 0.0),
                level: l,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            }])
            .unwrap();
        let shape = ddc_model::MemberShape::parse("rect 0.3 0.3").unwrap();
        let mt = ed.model.add_member_type("C1", &shape, "concrete").unwrap();
        ed.model
            .add_columns(&[ddc_model::ColumnSpec {
                level: l,
                top_level: None,
                member_type: mt,
                structural: true,
                at: (1.0, 1.0),
                base_offset: 0.0,
                top_offset: 3.0,
                rotation: 0.0,
                top_at: None,
            }])
            .unwrap();
        ed.revision += 1;
        let r = rows(&ed);
        assert!(r.iter().any(|x| x.cat == "wall"));
        let col = r.iter().find(|x| x.cat == "column").expect("column row");
        assert_eq!(col.ty, "C1");
        assert_eq!(key_of(col).table(), "col");
    }

    #[test]
    fn project_parameters_become_props() {
        let mut ed = editor();
        let l = ed.level;
        let t = ed.types[0].id;
        let w = ed
            .model
            .add_walls(&[ddc_model::WallSpec {
                a: (0.0, 0.0),
                b: (4.0, 0.0),
                level: l,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            }])
            .unwrap()[0];
        ed.model
            .add_params(&[ddc_model::ParamSpec {
                name: "Fire".into(),
                ptype: ddc_model::ParamType::Text,
                per_type: false,
                unit: String::new(),
                group: String::new(),
                guid: None,
                categories: vec!["wall".into()],
            }])
            .unwrap();
        ed.model
            .apply_edits(&[ddc_model::Edit::ParamValue {
                param: "Fire".into(),
                kind: "wall".into(),
                id: w,
                value: Some(ddc_model::PVal::Text("EI30".into())),
            }])
            .unwrap();
        ed.revision += 1;
        let r = rows(&ed);
        let x = r.iter().find(|x| x.cat == "wall").unwrap();
        assert!(
            x.props.contains(&("fire".to_string(), "EI30".to_string())),
            "{:?}",
            x.props
        );
    }

    #[test]
    fn every_ai_category_maps_to_a_selection_and_back() {
        for cat in [
            "wall", "curtain", "floor", "room", "mep", "comp", "column", "beam", "ceiling",
            "imported",
        ] {
            let k = Key { cat, id: 7 };
            let s = sel_of(k).unwrap();
            assert_eq!(key_of_sel(s, |_| "door"), Some(k));
        }
        assert_eq!(
            key_of_sel(Sel::Opening(3), |_| "window"),
            Some(Key {
                cat: "window",
                id: 3
            })
        );
    }
}
