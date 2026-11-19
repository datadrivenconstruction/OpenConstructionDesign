//! Delete probes across every family: another level is never touched, the status names what
//! went along, and one undo restores the whole table state.

use super::matrix::{row_exists, spawn, GAPS};
use super::{Cap, Family, FAMILIES};
use crate::editor::delete_deps::{collect_exact, Deps};
use crate::editor::Editor;
use crate::geom_props::tests::{fixture, square};
use ddc_model::SlabSpec;
use rusqlite::types::Value;
use std::hash::{Hash, Hasher};

struct World {
    ed: Editor,
    id: i64,
    other: i64,
}

fn dump(ed: &Editor, sql: &str) -> Vec<String> {
    let mut st = ed.model.sql().prepare(sql).unwrap();
    let n = st.column_count();
    let mut rows: Vec<String> = st
        .query_map([], |r| {
            Ok((0..n)
                .map(|i| format!("{:?}", r.get::<_, Value>(i).unwrap()))
                .collect::<Vec<_>>()
                .join("|"))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    rows.sort();
    rows
}

/// Content of every table but the journal, rows sorted so rowid order does not matter.
fn table_hash(ed: &Editor) -> u64 {
    let db = ed.model.sql();
    let mut st = db
        .prepare(
            "SELECT name FROM sqlite_master WHERE type='table' AND name<>'op' \
             AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .unwrap();
    let names: Vec<String> = st
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for t in names {
        t.hash(&mut h);
        dump(ed, &format!("SELECT * FROM {t}")).hash(&mut h);
    }
    h.finish()
}

/// Rows of the sentinel level: walls, slabs, their vertices and openings.
fn other_level_hash(w: &World) -> u64 {
    let l = w.other;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for sql in [
        format!("SELECT * FROM wall WHERE level={l}"),
        format!("SELECT * FROM slab WHERE level={l}"),
        format!("SELECT * FROM slab_vertex WHERE slab IN (SELECT id FROM slab WHERE level={l})"),
        format!("SELECT * FROM opening WHERE wall IN (SELECT id FROM wall WHERE level={l})"),
    ] {
        dump(&w.ed, &sql).hash(&mut h);
    }
    h.finish()
}

fn deletable() -> Vec<&'static Family> {
    FAMILIES
        .iter()
        .filter(|f| !matches!(f.xf.delete, Cap::No(_)))
        .filter(|f| !GAPS.iter().any(|g| g.0 == f.table && g.1 == "delete"))
        .collect()
}

/// The family's element on the view's level, a wall and a slab on another level, and the
/// dependents the delete scan knows (rebar, a tag).
fn world(f: &Family) -> World {
    let mut fx = fixture();
    let id = spawn(f.table, &mut fx.ed, &fx.ids);
    let mut ed = fx.ed;
    let other = ed.model.add_level("Probe other", 12.0).unwrap();
    let mut ws = crate::edit3d::grips::wall_spec(&ed.model, fx.ids["wall"]).unwrap();
    ws.level = other;
    ws.a = (500.0, 500.0);
    ws.b = (506.0, 500.0);
    ed.model.add_wall(&ws).unwrap();
    let stype: i64 = ed
        .model
        .sql()
        .query_row("SELECT id FROM slab_type LIMIT 1", [], |r| r.get(0))
        .unwrap();
    ed.model
        .add_slab(&SlabSpec {
            level: other,
            slab_type: stype,
            offset: 0.0,
            outer: square(500.0, 510.0, 6.0),
            holes: vec![],
        })
        .unwrap();
    let kind = match f.table {
        "wall" | "slab" | "beam" | "raft" => Some(f.table),
        "col" => Some("column"),
        "iso_footing" => Some("footing"),
        _ => None,
    };
    if let Some(kind) = kind {
        let db = ed.model.sql();
        for _ in 0..2 {
            db.execute(
                "INSERT INTO rebar_set(host_kind,host,layout,rtype) VALUES (?1,?2,'x',1)",
                rusqlite::params![kind, id],
            )
            .unwrap();
        }
        db.execute(
            "INSERT INTO rebar_cover(host_kind,host,face,cover) VALUES (?1,?2,'top',0.05)",
            rusqlite::params![kind, id],
        )
        .unwrap();
    }
    if f.table == "wall" {
        let level = ed.level;
        ed.model
            .sql()
            .execute(
                "INSERT INTO dim(level,kind,refs,dx,dy,side,offset,tier,auto) \
                 VALUES (?1,'tag',?2,0,0,0,0,-1,0)",
                rusqlite::params![level, format!("H w {id}")],
            )
            .unwrap();
    }
    ed.reload().unwrap();
    ed.set_mode(crate::wall_tool::Mode::Select);
    World { ed, id, other }
}

/// Deletes the family's element, answering the confirm dialog when it comes up. Returns the
/// dependents scanned before the delete.
fn delete_it(f: &Family, w: &mut World) -> Result<Deps, String> {
    // an axis is selected by its scene id, not by the id of its adjustment row
    let key = super::rebar::axis_key(&w.ed.model, w.id).filter(|_| f.table == "analytical_adj");
    let sel = f.sel(key.unwrap_or(w.id)).ok_or("no selection")?;
    w.ed.selection = vec![sel];
    let item = crate::clip_tools::sel_item(sel).ok_or("no clip item")?;
    let deps = collect_exact(&w.ed.model, &[item], w.ed.level);
    w.ed.status = None;
    w.ed.delete_selection();
    if w.ed.pending_delete.is_some() && !w.ed.confirm_delete() {
        return Err("the confirmed delete did not run".into());
    }
    Ok(deps)
}

fn each(check: impl Fn(&Family, &mut World) -> Result<(), String>) {
    let mut bad = vec![];
    for f in deletable() {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut w = world(f);
            check(f, &mut w)
        }))
        .unwrap_or_else(|_| Err("panicked".into()));
        if let Err(e) = r {
            bad.push(format!("{}: {e}", f.table));
        }
    }
    assert!(bad.is_empty(), "delete probes failed: {bad:#?}");
}

#[test]
fn delete_never_touches_other_level() {
    each(|f, w| {
        let before = other_level_hash(w);
        delete_it(f, w)?;
        if row_exists(&w.ed, f.table, w.id) {
            return Err("the row is still there".into());
        }
        if other_level_hash(w) != before {
            return Err("rows of another level changed".into());
        }
        w.ed.undo();
        if other_level_hash(w) != before {
            return Err("rows of another level changed by undo".into());
        }
        Ok(())
    });
}

#[test]
fn delete_status_names_dependents() {
    each(|f, w| {
        let deps = delete_it(f, w)?;
        let s = w.ed.status.clone().unwrap_or_default();
        if s.is_empty() {
            return Err("empty status".into());
        }
        let hosted = matches!(f.table, "wall" | "slab" | "col" | "beam" | "iso_footing" | "raft");
        if hosted && deps.total() == 0 {
            return Err("the scan found no dependents for a host with rebar".into());
        }
        for (kind, n) in &deps.counts {
            let name = crate::tr_n(&format!("del.k.{kind}"), *n as u64);
            if !s.contains(&name) {
                return Err(format!("status lacks '{name}': {s}"));
            }
        }
        if f.table == "wall" && !deps.counts.iter().any(|c| c.0 == "tag" && c.1 == 1) {
            return Err("the wall's tag is not counted".into());
        }
        Ok(())
    });
}

#[test]
fn delete_one_undo_restores_hash() {
    each(|f, w| {
        let before = table_hash(&w.ed);
        let steps = w.ed.undo_len();
        delete_it(f, w)?;
        if w.ed.undo_len() != steps + 1 {
            return Err(format!("{} undo steps, expected one", w.ed.undo_len() as i64 - steps as i64));
        }
        if table_hash(&w.ed) == before {
            return Err("the delete changed nothing".into());
        }
        w.ed.undo();
        if table_hash(&w.ed) != before {
            return Err("one undo did not restore the table hash".into());
        }
        Ok(())
    });
}
