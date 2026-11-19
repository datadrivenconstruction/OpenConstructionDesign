// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Imported solids: move, rotate, mirror, copy with properties, delete, undo.

use ddc_model::{Model, Target};
use rusqlite::types::Value;

fn blob(ps: &[[f64; 3]]) -> Vec<u8> {
    ps.iter().flatten().flat_map(|v| v.to_le_bytes()).collect()
}

fn pts(m: &Model, id: i64) -> Vec<[f64; 3]> {
    let b: Vec<u8> = m
        .sql()
        .query_row("SELECT positions FROM mesh WHERE id=?1", [id], |r| r.get(0))
        .unwrap();
    b.chunks_exact(24)
        .map(|c| {
            let g = |k: usize| f64::from_le_bytes(c[k * 8..k * 8 + 8].try_into().unwrap());
            [g(0), g(1), g(2)]
        })
        .collect()
}

fn tri(m: &Model, id: i64) -> Vec<u32> {
    let b: Vec<u8> = m
        .sql()
        .query_row("SELECT indices FROM mesh WHERE id=?1", [id], |r| r.get(0))
        .unwrap();
    b.chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

fn near(a: [f64; 3], b: [f64; 3]) {
    for k in 0..3 {
        assert!((a[k] - b[k]).abs() < 1e-9, "{a:?} vs {b:?}");
    }
}

#[test]
fn imported_mesh_is_editable() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let p0 = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 2.0]];
    let ib: Vec<u8> = [0u32, 1, 2].iter().flat_map(|i| i.to_le_bytes()).collect();
    m.sql()
        .execute(
            "INSERT INTO mesh(level,category,name,positions,indices) VALUES (?1,'furniture','Chair',?2,?3)",
            rusqlite::params![l, blob(&p0), ib],
        )
        .unwrap();
    let id = m.sql().last_insert_rowid();
    m.set_prop("mesh", id, "Mark", Some((Value::Text("A1".into()), "")))
        .unwrap();

    m.move_elements(&[Target::Mesh(id)], (2.0, 3.0)).unwrap();
    near(pts(&m, id)[1], [3.0, 3.0, 0.0]);
    m.rotate_elements(
        &[Target::Mesh(id)],
        (2.0, 3.0),
        std::f64::consts::FRAC_PI_2,
        false,
    )
    .unwrap();
    near(pts(&m, id)[1], [2.0, 4.0, 0.0]);
    near(pts(&m, id)[2], [1.0, 3.0, 2.0]);
    m.mirror_elements(&[Target::Mesh(id)], (0.0, 0.0), (0.0, 1.0), false)
        .unwrap();
    near(pts(&m, id)[1], [-2.0, 4.0, 0.0]);
    assert_eq!(tri(&m, id), vec![0, 2, 1], "mirror flips winding");

    let c = m
        .copy_elements(&[Target::Mesh(id)], &[(10.0, 0.0)])
        .unwrap();
    let Target::Mesh(cid) = c[0] else { panic!() };
    near(pts(&m, cid)[1], [8.0, 4.0, 0.0]);
    assert_eq!(m.props("mesh", cid).unwrap()[0].1, Value::Text("A1".into()));

    m.delete_mesh(id).unwrap();
    assert!(!m.meshes(None).unwrap().contains(&id));
    m.undo().unwrap();
    assert_eq!(m.props("mesh", id).unwrap().len(), 1);
    near(pts(&m, id)[1], [-2.0, 4.0, 0.0]);
    m.undo().unwrap(); // copy
    assert_eq!(m.meshes(None).unwrap(), vec![id]);
    assert!(m.props("mesh", cid).unwrap().is_empty());
    m.undo().unwrap(); // mirror
    m.undo().unwrap(); // rotate
    m.undo().unwrap(); // move
    near(pts(&m, id)[1], [1.0, 0.0, 0.0]);
    m.set_prop("mesh", id, "Mark", None).unwrap();
    assert!(m.props("mesh", id).unwrap().is_empty());
    m.undo().unwrap();
    assert_eq!(m.props("mesh", id).unwrap().len(), 1);
}

#[test]
fn imported_native_rows_move_copy_delete_with_one_undo() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let db = m.sql();
    db.execute_batch(&format!(
        "INSERT INTO member_type(name,shape,material) VALUES ('R','rect 0.3 0.5','x');
         INSERT INTO col(level,mtype,structural,x,y,base_offset,top_offset,rotation)
             VALUES ({l},1,1,1.0,1.0,0.0,3.0,0.0);
         INSERT INTO beam(level,mtype,ax,ay,az,bx,by,bz,bulge,yjust,zjust,rotation,start_ext,
             end_ext,join_start,join_end) VALUES ({l},1,0,0,3,4,0,3,0,0,0,0.2,0,0,'auto','auto');
         INSERT INTO mep_system(name,kind,color) VALUES ('S','cold_water',255);
         INSERT INTO mep_run(system,level,domain,shape,w,h,wall,insulation,lining,slope,points,
             cap0,cap1) VALUES (1,{l},'pipe','round',0.05,0.05,0,0,0,0,'0 2 2.5;4 2 2.5',0,0);
         INSERT INTO slab_type(name) VALUES ('C');
         INSERT INTO slab_type_layer(stype,pos,material,thickness) VALUES (1,0,'x',0.02);
         INSERT INTO ceiling(level,stype,height) VALUES ({l},1,2.6);
         INSERT INTO ceiling_vertex(ceiling,ring,idx,x,y) VALUES (1,0,0,0,0),(1,0,1,4,0),(1,0,2,4,3),(1,0,3,0,3);"
    ))
    .unwrap();
    m.set_prop("col", 1, "Mark", Some((Value::Text("C1".into()), "")))
        .unwrap();
    let ts = [
        Target::Column(1),
        Target::Beam(1),
        Target::MepRun(1),
        Target::Ceiling(1),
    ];
    let q = |m: &Model, s: &str| -> f64 { m.sql().query_row(s, [], |r| r.get(0)).unwrap() };
    m.move_elements(&ts, (1.0, 2.0)).unwrap();
    assert_eq!(q(&m, "SELECT x FROM col"), 2.0);
    assert_eq!(q(&m, "SELECT by FROM beam"), 2.0);
    assert_eq!(q(&m, "SELECT max(y) FROM ceiling_vertex"), 5.0);
    let p: String = m
        .sql()
        .query_row("SELECT points FROM mep_run", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ddc_model::parse_points(&p).unwrap()[1], (5.0, 4.0, 2.5));
    assert!(m.ceiling_mesh(1).is_ok());
    m.undo().unwrap();
    assert_eq!(q(&m, "SELECT x FROM col"), 1.0);
    assert_eq!(q(&m, "SELECT max(y) FROM ceiling_vertex"), 3.0);
    m.rotate_elements(&ts, (0.0, 0.0), std::f64::consts::FRAC_PI_2, false)
        .unwrap();
    assert!((q(&m, "SELECT rotation FROM col") - std::f64::consts::FRAC_PI_2).abs() < 1e-9);
    m.undo().unwrap();
    let copies = m.copy_elements(&ts, &[(10.0, 0.0)]).unwrap();
    assert_eq!(copies.len(), 4);
    assert_eq!(q(&m, "SELECT count(*) FROM ceiling_vertex"), 8.0);
    assert_eq!(q(&m, "SELECT count(*) FROM prop WHERE kind='col'"), 2.0);
    m.undo().unwrap();
    m.delete_imported(&[("col", 1), ("beam", 1), ("mep_run", 1), ("ceiling", 1)])
        .unwrap();
    assert_eq!(
        q(&m, "SELECT count(*) FROM col") + q(&m, "SELECT count(*) FROM ceiling_vertex"),
        0.0
    );
    m.undo().unwrap();
    assert_eq!(q(&m, "SELECT count(*) FROM col"), 1.0);
    assert_eq!(q(&m, "SELECT count(*) FROM ceiling_vertex"), 4.0);
    assert_eq!(q(&m, "SELECT count(*) FROM prop WHERE kind='col'"), 1.0);
}

/// Deleting imported columns and ceilings drops their derived quantities; undo brings them back.
#[test]
fn imported_delete_regenerates_quantities() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    m.sql()
        .execute_batch(&format!(
            "INSERT INTO member_type(name,shape,material) VALUES ('R','rect 0.3 0.5','x');
             INSERT INTO col(id,level,mtype,structural,x,y,base_offset,top_offset,rotation)
                 VALUES (1,{l},(SELECT id FROM member_type WHERE name='R'),1,1.0,1.0,0.0,3.0,0.0);
             INSERT INTO slab_type(name) VALUES ('C');
             INSERT INTO slab_type_layer(stype,pos,material,thickness)
                 VALUES ((SELECT id FROM slab_type WHERE name='C'),0,'x',0.02);
             INSERT INTO ceiling(id,level,stype,height)
                 VALUES (1,{l},(SELECT id FROM slab_type WHERE name='C'),2.6);
             INSERT INTO ceiling_vertex(ceiling,ring,idx,x,y)
                 VALUES (1,0,0,0,0),(1,0,1,4,0),(1,0,2,4,3),(1,0,3,0,3);"
        ))
        .unwrap();
    // any derived-kind op regenerates: a prop edit does not, so add the quantities by a move
    m.move_elements(&[Target::Column(1), Target::Ceiling(1)], (0.0, 0.0))
        .unwrap();
    let q = |m: &Model, s: &str| -> i64 { m.sql().query_row(s, [], |r| r.get(0)).unwrap() };
    assert_eq!(q(&m, "SELECT count(*) FROM member_qty WHERE ref=1"), 1);
    assert!(q(&m, "SELECT count(*) FROM ceiling_qty WHERE ceiling=1") > 0);
    m.delete_imported(&[("col", 1), ("ceiling", 1)]).unwrap();
    assert_eq!(q(&m, "SELECT count(*) FROM member_qty"), 0);
    assert_eq!(q(&m, "SELECT count(*) FROM ceiling_qty"), 0);
    m.undo().unwrap();
    assert_eq!(q(&m, "SELECT count(*) FROM member_qty WHERE ref=1"), 1);
    assert!(q(&m, "SELECT count(*) FROM ceiling_qty WHERE ceiling=1") > 0);
}

/// A mirror cannot flip a measured section: an L angle is refused, a symmetric I is mirrored.
#[test]
fn mirror_refuses_asymmetric_measured_section() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    m.sql()
        .execute_batch(&format!(
            "INSERT INTO member_type(name,shape,material)
                 VALUES ('L','poly 0 0;0.1 0;0.1 0.01;0.01 0.01;0.01 0.1;0 0.1','x');
             INSERT INTO member_type(name,shape,material)
                 VALUES ('I','poly -0.05 -0.1;0.05 -0.1;0.05 0.1;-0.05 0.1','x');
             INSERT INTO beam(id,level,mtype,ax,ay,az,bx,by,bz,bulge,yjust,zjust,rotation,start_ext,
                 end_ext,join_start,join_end)
                 VALUES (1,{l},(SELECT id FROM member_type WHERE name='L'),0,0,3,4,0,3,0,0,0,0,0,0,
                         'none','none'),
                        (2,{l},(SELECT id FROM member_type WHERE name='I'),0,2,3,4,2,3,0,0,0,0,0,0,
                         'none','none');"
        ))
        .unwrap();
    assert!(m
        .mirror_elements(&[Target::Beam(1)], (0.0, 0.0), (1.0, 0.0), false)
        .is_err());
    m.mirror_elements(&[Target::Beam(2)], (0.0, 0.0), (1.0, 0.0), false)
        .unwrap();
    let ay: f64 = m
        .sql()
        .query_row("SELECT ay FROM beam WHERE id=2", [], |r| r.get(0))
        .unwrap();
    assert!((ay + 2.0).abs() < 1e-9);
}

/// Moving a mesh journals a transform, not the body: the log row stays small.
#[test]
fn mesh_move_journal_is_small() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let ps: Vec<[f64; 3]> = (0..3000).map(|i| [i as f64, 0.0, (i % 7) as f64]).collect();
    let ib: Vec<u8> = (0u32..2998)
        .flat_map(|i| [i, i + 1, i + 2])
        .flat_map(|i| i.to_le_bytes())
        .collect();
    m.sql()
        .execute(
            "INSERT INTO mesh(level,category,name,positions,indices) VALUES (?1,'x','x',?2,?3)",
            rusqlite::params![l, blob(&ps), ib],
        )
        .unwrap();
    let id = m.sql().last_insert_rowid();
    m.move_elements(&[Target::Mesh(id)], (1.5, -2.0)).unwrap();
    m.move_mesh(id, (0.0, 0.0, 1.0)).unwrap();
    let n: i64 = m
        .sql()
        .query_row("SELECT max(length(inv)) FROM op", [], |r| r.get(0))
        .unwrap();
    assert!(n < 2000, "log row of {n} bytes");
    near(pts(&m, id)[10], [11.5, -2.0, 4.0]);
    m.undo().unwrap();
    m.undo().unwrap();
    near(pts(&m, id)[10], [10.0, 0.0, 3.0]);
}
