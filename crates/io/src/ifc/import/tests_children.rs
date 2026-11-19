// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Re-import merges a row's child rows (vertices, panels) as one field: a kept column edit must
//! not put back the local children, and the file's child changes must not be dropped silently.

use super::*;

type Pts = Vec<(f64, f64, f64)>;

fn path0() -> Pts {
    vec![(0.0, 0.0, 0.0), (4.0, 0.0, 0.0)]
}

fn path_local() -> Pts {
    vec![(0.0, 0.0, 0.0), (2.0, 1.0, 0.0), (4.0, 1.0, 0.0)]
}

fn path_file() -> Pts {
    vec![(0.0, 0.5, 0.0), (5.0, 0.5, 0.0)]
}

/// A model with one sketch railing, exported as the "file".
fn railing_model() -> (Model, i64) {
    let mut a = Model::open(":memory:").unwrap();
    let l1 = a.add_level("L1", 0.0).unwrap();
    let id = a
        .add_railing(&ddc_model::RailingSpec::new(
            l1,
            ddc_model::RailingHost::Path(path0()),
        ))
        .unwrap();
    (a, id)
}

fn verts(m: &Model) -> Pts {
    let mut st = m
        .sql()
        .prepare("SELECT x, y, z FROM railing_vertex ORDER BY railing, idx")
        .unwrap();
    let rows = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap();
    rows.map(|x| x.unwrap()).collect()
}

fn height(m: &Model) -> f64 {
    m.sql()
        .query_row("SELECT height FROM railing", [], |r| r.get(0))
        .unwrap()
}

fn rail_id(m: &Model) -> i64 {
    m.sql()
        .query_row("SELECT id FROM railing", [], |r| r.get(0))
        .unwrap()
}

fn same(a: &Pts, b: &Pts) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(p, q)| {
            (p.0 - q.0).abs() < 1e-6 && (p.1 - q.1).abs() < 1e-6 && (p.2 - q.2).abs() < 1e-6
        })
}

/// The project after the first import of the file, plus the file model.
fn first_import() -> (Model, Model, i64) {
    let (a, id) = railing_model();
    let mut m = Model::open(":memory:").unwrap();
    import_ifc(&mut m, file_of(&a).as_bytes()).unwrap();
    (m, a, id)
}

fn file_of(a: &Model) -> String {
    crate::ifc::export_ifc(a).unwrap()
}

#[test]
fn kept_height_takes_file_vertices() {
    let (mut m, mut a, id) = first_import();
    let base = verts(&m);
    assert_eq!(base.len(), 2);
    let h = height(&m);
    // height edited here, vertices changed only in the file
    let rid = rail_id(&m);
    m.sql()
        .execute("UPDATE railing SET height=?2 WHERE id=?1", rusqlite::params![rid, h + 0.2])
        .unwrap();
    a.set_railing_path(id, &path_file()).unwrap();
    let r = import_ifc(&mut m, file_of(&a).as_bytes()).unwrap();
    assert!((height(&m) - (h + 0.2)).abs() < 1e-9, "local height kept");
    assert!(same(&verts(&m), &verts(&a)), "file vertices taken");
    assert!(!same(&verts(&m), &base));
    assert_eq!(r.conflicts, 0);
    assert!(r.updated >= 1, "the row did change");
}

#[test]
fn edited_vertices_kept_file_height_taken() {
    let (mut m, a, id) = first_import();
    let h = height(&m);
    let rid = rail_id(&m);
    m.set_railing_path(rid, &path_local()).unwrap();
    let local = verts(&m);
    assert_eq!(local.len(), 3);
    a.sql()
        .execute("UPDATE railing SET height=?2 WHERE id=?1", rusqlite::params![id, h + 0.1])
        .unwrap();
    let r = import_ifc(&mut m, file_of(&a).as_bytes()).unwrap();
    assert!(same(&verts(&m), &local), "local vertices kept");
    assert!((height(&m) - (h + 0.1)).abs() < 1e-9, "file height taken");
    assert_eq!(r.conflicts, 0);
    assert!(r.updated >= 1);
}

#[test]
fn both_changed_children_conflict_not_silent() {
    let (mut m, mut a, id) = first_import();
    let rid = rail_id(&m);
    m.set_railing_path(rid, &path_local()).unwrap();
    let local = verts(&m);
    a.set_railing_path(id, &path_file()).unwrap();
    let r = import_ifc(&mut m, file_of(&a).as_bytes()).unwrap();
    assert!(same(&verts(&m), &local), "local vertices stay");
    assert!(r.conflicts >= 1, "the file's change is reported");
    assert!(!m.conflicts(None).unwrap().is_empty());
}

#[test]
fn unchanged_children_counted_unchanged() {
    let (mut m, a, _) = first_import();
    let base = verts(&m);
    let h = height(&m);
    let rid = rail_id(&m);
    m.sql()
        .execute("UPDATE railing SET height=?2 WHERE id=?1", rusqlite::params![rid, h + 0.3])
        .unwrap();
    let r = import_ifc(&mut m, file_of(&a).as_bytes()).unwrap();
    assert!(same(&verts(&m), &base));
    assert!((height(&m) - (h + 0.3)).abs() < 1e-9);
    assert_eq!((r.updated, r.conflicts), (0, 0));
    assert!(r.unchanged >= 1);
}
