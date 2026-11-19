use super::*;
use ddc_geom::{Profile, Solid, Tol};

/// Prism: profile (x, y) extruded along z by `depth`, then mapped through `f`.
fn prism(outer: Vec<V2>, depth: f64, f: &Affine) -> Mesh {
    let p = Profile::new(outer, vec![], Tol::default()).unwrap();
    let m = Solid::extrude(&p, depth, Tol::default())
        .triangulate()
        .unwrap();
    mesh_ops::transform(&m, f)
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<V2> {
    vec![v2(x0, y0), v2(x1, y0), v2(x1, y1), v2(x0, y1)]
}

fn model() -> (Model, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    (m, l)
}

fn add(m: &Model, level: i64, category: &str, mesh: &Mesh) -> i64 {
    let (p, i) = encode(mesh);
    m.sql()
        .execute(
            "INSERT INTO mesh(level,category,name,positions,indices) VALUES (?1,?2,'n',?3,?4)",
            params![level, category, p, i],
        )
        .unwrap();
    m.sql().last_insert_rowid()
}

fn count(m: &Model, t: &str) -> i64 {
    m.sql()
        .query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))
        .unwrap()
}

fn blobs(m: &Model, id: i64) -> (Vec<u8>, Vec<u8>) {
    m.sql()
        .query_row(
            "SELECT positions, indices FROM mesh WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn blob_codec_round_trip() {
    let b = prism(rect(0.0, 0.0, 1.0, 2.0), 3.0, &Affine::IDENTITY);
    let (p, i) = encode(&b);
    let d = decode(&p, &i).unwrap();
    assert_eq!(d.positions, b.positions);
    assert_eq!(d.triangles, b.triangles);
    assert!(decode(&p[1..], &i).is_err());
}

#[test]
fn box_to_wall_and_undo_restores_mesh() {
    let (mut m, l) = model();
    let id = add(
        &m,
        l,
        "Walls",
        &prism(rect(0.0, 0.0, 6.0, 0.2), 3.0, &Affine::IDENTITY),
    );
    m.sql()
        .execute(
            "INSERT INTO ext_ref(source,guid,kind,ref) VALUES ('f','g1','mesh',?1)",
            [id],
        )
        .unwrap();
    m.sql()
        .execute(
            "INSERT INTO prop(kind,ref,key,value,unit) VALUES ('mesh',?1,'Mark','W1','')",
            [id],
        )
        .unwrap();
    let before = blobs(&m, id);
    let c = m.recognize_mesh(id, None).unwrap();
    assert_eq!(c[0].kind(), Kind::Wall);
    let r = m.convert_meshes(&[(id, c[0].fit.clone())], false).unwrap();
    assert_eq!(r.done.len(), 1);
    assert_eq!(r.done[0].table, "wall");
    let w = r.done[0].id;
    let (ax, bx, h, t): (f64, f64, f64, f64) = m
        .sql()
        .query_row(
            "SELECT w.ax, w.bx, w.height, l.thickness FROM wall w \
             JOIN wall_type_layer l ON l.wtype=w.wtype WHERE w.id=?1",
            [w],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert!(near((bx - ax).abs(), 6.0) && near(h, 3.0) && near(t, 0.2));
    assert_eq!(count(&m, "mesh"), 0);
    let (k, rf): (String, i64) = m
        .sql()
        .query_row("SELECT kind, ref FROM ext_ref WHERE guid='g1'", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!((k.as_str(), rf), ("wall", w));
    let mark: String = m
        .sql()
        .query_row(
            "SELECT value FROM prop WHERE kind='wall' AND ref=?1 AND key='Mark'",
            [w],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(mark, "W1");

    m.undo().unwrap();
    assert_eq!(count(&m, "wall"), 0);
    assert_eq!(count(&m, "wall_type"), 0);
    assert_eq!(blobs(&m, id), before, "mesh blob restored exactly");
    let k: String = m
        .sql()
        .query_row("SELECT kind FROM ext_ref WHERE guid='g1'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(k, "mesh");
    assert_eq!(count(&m, "prop"), 1);
}

#[test]
fn i_beam_to_beam() {
    let (mut m, l) = model();
    let (ring, _) = Shape::I {
        h: 0.3,
        b: 0.15,
        tw: 0.0071,
        tf: 0.0107,
    }
    .rings();
    let f = Affine {
        m: [[0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        t: v3(0.0, 0.0, 3.5),
    };
    let id = add(&m, l, "Structural Framing", &prism(ring, 6.0, &f));
    let c = m.recognize_mesh(id, None).unwrap();
    assert_eq!(c[0].kind(), Kind::Beam);
    let r = m.convert_meshes(&[(id, c[0].fit.clone())], false).unwrap();
    assert_eq!(r.done[0].table, "beam");
    let (az, shape): (f64, String) = m
        .sql()
        .query_row(
            "SELECT b.az, t.shape FROM beam b JOIN member_type t ON t.id=b.mtype",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(near(az, 3.5));
    assert!(
        matches!(Shape::parse(&shape), Some(Shape::I { .. })),
        "{shape}"
    );
    // a second identical profile reuses the same type
    let id2 = add(
        &m,
        l,
        "Structural Framing",
        &prism(
            Shape::I {
                h: 0.3,
                b: 0.15,
                tw: 0.0071,
                tf: 0.0107,
            }
            .rings()
            .0,
            4.0,
            &f,
        ),
    );
    let c2 = m.recognize_mesh(id2, None).unwrap();
    m.convert_meshes(&[(id2, c2[0].fit.clone())], false)
        .unwrap();
    assert_eq!(count(&m, "member_type"), 1);
}

#[test]
fn sloped_plate_to_roof() {
    let (mut m, l) = model();
    let th = 25f64.to_radians();
    let f = Affine {
        m: [
            [1.0, 0.0, 0.0],
            [0.0, th.cos(), -th.sin()],
            [0.0, th.sin(), th.cos()],
        ],
        t: v3(0.0, 0.0, 3.0),
    };
    let id = add(&m, l, "Roofs", &prism(rect(0.0, 0.0, 8.0, 5.0), 0.2, &f));
    let c = m.recognize_mesh(id, None).unwrap();
    assert_eq!(c[0].kind(), Kind::Roof);
    let r = m.convert_meshes(&[(id, c[0].fit.clone())], false).unwrap();
    assert_eq!(r.done[0].table, "roof");
    let slopes: Vec<Option<f64>> = {
        let mut st = m
            .sql()
            .prepare("SELECT slope FROM roof_vertex ORDER BY idx")
            .unwrap();
        let v = st
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        v
    };
    assert_eq!(slopes.iter().filter(|s| s.is_some()).count(), 1);
    let s = slopes.iter().flatten().next().unwrap();
    assert!((s - 25.0).abs() < 1e-6);
    m.undo().unwrap();
    assert_eq!(count(&m, "roof"), 0);
    assert_eq!(count(&m, "mesh"), 1);
}

#[test]
fn failed_item_leaves_mesh_untouched() {
    let (mut m, l) = model();
    let id = add(
        &m,
        l,
        "Walls",
        &prism(rect(0.0, 0.0, 6.0, 0.2), 3.0, &Affine::IDENTITY),
    );
    let bad = Fit::Wall {
        a: v2(0.0, 0.0),
        b: v2(0.0, 0.0),
        z0: 0.0,
        height: 3.0,
        thickness: 0.2,
        openings: vec![],
        top: None,
    };
    assert!(m.convert_meshes(&[(id, bad)], false).is_err());
    assert_eq!(count(&m, "mesh"), 1);
    let good = m.recognize_mesh(id, None).unwrap()[0].fit.clone();
    let r = m
        .convert_meshes(&[(id, good.clone()), (999, good)], true)
        .unwrap();
    assert_eq!((r.done.len(), r.failed.len()), (1, 1));
    assert_eq!(count(&m, "mesh"), 1, "keep_source");
}

#[test]
fn cut_split_merge_with_undo() {
    let (mut m, l) = model();
    let id = add(
        &m,
        l,
        "Generic",
        &prism(rect(0.0, 0.0, 2.0, 2.0), 2.0, &Affine::IDENTITY),
    );
    let before = blobs(&m, id);
    m.cut_meshes(&[id], &Plane::horizontal(0.5), Keep::Below)
        .unwrap();
    let st = m.mesh_stats(id).unwrap();
    assert!(st.closed && near(st.volume, 2.0));
    m.undo().unwrap();
    assert_eq!(blobs(&m, id), before);

    let n = m.split_meshes(&[id], &Plane::horizontal(1.5)).unwrap()[0];
    assert!(near(m.mesh_stats(id).unwrap().volume, 6.0));
    assert!(near(m.mesh_stats(n).unwrap().volume, 2.0));
    let merged = m.merge_meshes(&[id, n]).unwrap();
    assert_eq!(merged, id);
    assert_eq!(count(&m, "mesh"), 1);
    assert!(near(m.mesh_stats(id).unwrap().volume, 8.0));
    m.undo().unwrap();
    assert_eq!(count(&m, "mesh"), 2);
    m.undo().unwrap();
    assert_eq!(count(&m, "mesh"), 1);
    assert_eq!(blobs(&m, id), before);

    m.transform_meshes(&[id], &Affine::translate(v3(10.0, 0.0, 0.0)))
        .unwrap();
    assert!(near(m.mesh_stats(id).unwrap().min.x, 10.0));
    assert!(m
        .transform_meshes(&[id], &Affine::scale(v3(0.0, 0.0, 0.0), v3(0.0, 1.0, 1.0)))
        .is_err());
}

#[test]
fn plan_and_section_of_a_mesh() {
    let (m, l) = model();
    let id = add(
        &m,
        l,
        "Generic",
        &prism(rect(0.0, 0.0, 4.0, 1.0), 3.0, &Affine::IDENTITY),
    );
    let p = m.mesh_plan(l, 1.2).unwrap();
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].mesh, id);
    assert!(near(p[0].cut.iter().map(Region::area).sum::<f64>(), 4.0));

    let mut prims = vec![];
    let mut pts = vec![];
    // view line at y = 0.5 along +x: the solid lies on both sides, so the section is 4 × 3
    section_prims(
        m.sql(),
        v2(-1.0, 0.5),
        v2(1.0, 0.0),
        Some(10.0),
        true,
        &mut prims,
        &mut |q| pts.push(q),
    )
    .unwrap();
    let cut: Vec<_> = prims
        .iter()
        .filter_map(|p| match p {
            ViewPrim::SectionCut { rings, .. } => Some(rings.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(cut.len(), 1);
    let xs: Vec<f64> = cut[0].iter().map(|q| q.0).collect();
    let span =
        xs.iter().cloned().fold(f64::MIN, f64::max) - xs.iter().cloned().fold(f64::MAX, f64::min);
    assert!(near(span, 4.0));
    assert!(!pts.is_empty());
}

fn prop_of(m: &Model, kind: &str, id: i64, key: &str) -> Option<String> {
    m.sql()
        .query_row(
            "SELECT CAST(value AS TEXT) FROM prop WHERE kind=?1 AND ref=?2 AND key=?3",
            params![kind, id, key],
            |r| r.get(0),
        )
        .optional()
        .unwrap()
}

#[test]
fn converted_element_is_marked_and_gable_keeps_top() {
    let (mut m, l) = model();
    let gable = vec![
        v2(0.0, 0.0),
        v2(6.0, 0.0),
        v2(6.0, 3.0),
        v2(3.0, 4.5),
        v2(0.0, 3.0),
    ];
    // profile in the x–z plane, thickness 0.25 along y
    let f = Affine {
        m: [[1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]],
        t: v3(0.0, 0.0, 0.0),
    };
    let id = add(&m, l, "Walls", &prism(gable, 0.25, &f));
    let c = m.recognize_mesh(id, None).unwrap();
    let Fit::Wall { a, b, .. } = c[0].fit.clone() else {
        panic!("not a wall: {:?}", c[0]);
    };
    let mut fit = c[0].fit.clone();
    // u is measured from a; a 1 × 1 window fits near the middle but not at the edge under the slope
    let from = |x: f64, w: f64| if a.x < b.x { x } else { 6.0 - x - w };
    if let Fit::Wall { openings, .. } = &mut fit {
        openings.push(mesh_fit::FitOpening {
            along: from(2.5, 1.0),
            width: 1.0,
            sill: 1.0,
            height: 1.0,
        });
        openings.push(mesh_fit::FitOpening {
            along: from(0.2, 1.0),
            width: 1.0,
            sill: 2.5,
            height: 1.0,
        });
    }
    let r = m.convert_meshes(&[(id, fit)], false).unwrap();
    let w = r.done[0].id;
    assert_eq!(r.done[0].table, "wall");
    assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
    assert_eq!(r.warnings[0].0, id);
    assert_eq!(count(&m, "opening"), 1);
    let top = m.wall_top_profile(w).unwrap().expect("top profile");
    assert_eq!(top.len(), 3, "{top:?}");
    assert!(top.iter().any(|p| near(p.1, 4.5)));
    assert_eq!(
        prop_of(&m, "wall", w, CONVERTED_FROM).as_deref(),
        Some(id.to_string().as_str())
    );
    m.undo().unwrap();
    assert_eq!(count(&m, "wall"), 0);
    assert_eq!(count(&m, "prop"), 0, "marker undone");
}

#[test]
fn extrusion_without_solids_fails_and_is_not_offered() {
    let (mut m, l) = model();
    let tri = vec![v2(0.0, 0.0), v2(3.0, 0.0), v2(1.0, 2.0)];
    let mesh = prism(tri, 2.5, &Affine::IDENTITY);
    let id = add(&m, l, "Generic Models", &mesh);
    // schema 23 always has the table: an older store is simulated
    m.db.execute_batch("DROP TABLE solid").unwrap();
    assert!(!m.solids_available().unwrap());
    let c = m.recognize_mesh(id, Some(Kind::Extrusion)).unwrap();
    assert!(c.iter().all(|c| c.kind() != Kind::Extrusion), "{c:?}");
    let with = recognize_mesh_data(&mesh, "Generic Models", true);
    let e = with
        .iter()
        .find(|c| c.kind() == Kind::Extrusion)
        .expect("offered when solids exist");
    let r = m.convert_meshes(&[(id, e.fit.clone())], false);
    assert!(r.is_err(), "nothing converted");
    assert_eq!(count(&m, "mesh"), 1);
    assert_eq!(count(&m, "prop"), 0, "no profile parked on the mesh");
}

#[test]
fn furniture_is_never_a_column() {
    let (m, l) = model();
    let id = add(
        &m,
        l,
        "Furniture",
        &prism(rect(0.0, 0.0, 0.6, 0.6), 2.0, &Affine::IDENTITY),
    );
    let c = m.recognize_mesh(id, None).unwrap();
    assert!(
        c.iter()
            .all(|c| !matches!(c.kind(), Kind::Column | Kind::Wall)),
        "{c:?}"
    );
    assert!(c.iter().all(|c| !c.consistent));
}

#[test]
fn mesh_rows_signature_follows_edits_and_undo() {
    let (mut m, l) = model();
    let id = add(
        &m,
        l,
        "Generic",
        &prism(rect(0.0, 0.0, 2.0, 2.0), 2.0, &Affine::IDENTITY),
    );
    let r0 = m.mesh_rows(Some(l), None).unwrap();
    assert_eq!(r0.len(), 1);
    assert_eq!((r0[0].id, r0[0].triangles), (id, 12));
    assert_eq!(
        m.mesh_rows(None, None).unwrap()[0].sig,
        r0[0].sig,
        "stable without edits"
    );
    m.transform_meshes(&[id], &Affine::translate(v3(1.0, 0.0, 0.0)))
        .unwrap();
    let r1 = m.mesh_rows(None, None).unwrap();
    assert_ne!(r1[0].sig, r0[0].sig);
    m.undo().unwrap();
    let r2 = m.mesh_rows(None, None).unwrap();
    assert_ne!(r2[0].sig, r1[0].sig, "undo is a revision too");
    assert!(m.mesh_rows(None, Some("Walls")).unwrap().is_empty());
}

#[test]
fn plan_cache_reuses_and_invalidates() {
    let (mut m, l) = model();
    // 2 × 2 tube with a 1 × 1 hole: the fill is a ring, not a solid square
    let p = Profile::new(
        rect(0.0, 0.0, 2.0, 2.0),
        vec![rect(0.5, 0.5, 1.5, 1.5)],
        Tol::default(),
    )
    .unwrap();
    let tube = Solid::extrude(&p, 3.0, Tol::default())
        .triangulate()
        .unwrap();
    let id = add(&m, l, "Generic", &tube);
    let mut cache = MeshPlanCache::default();
    let a = m.mesh_plan_cached(l, 1.2, &mut cache).unwrap();
    assert_eq!(a.len(), 1);
    let fill: f64 = a[0]
        .fill
        .iter()
        .map(|t| (t[1] - t[0]).cross(t[2] - t[0]) / 2.0)
        .sum();
    assert!(near(fill, 3.0), "{fill}");
    assert!(near(a[0].cut.iter().map(Region::area).sum::<f64>(), 3.0));
    let b = m.mesh_plan_cached(l, 1.2, &mut cache).unwrap();
    assert_eq!(a, b);
    m.transform_meshes(&[id], &Affine::translate(v3(5.0, 0.0, 0.0)))
        .unwrap();
    let c = m.mesh_plan_cached(l, 1.2, &mut cache).unwrap();
    let xs = c[0].cut[0]
        .outer
        .iter()
        .map(|q| q.x)
        .fold(f64::MAX, f64::min);
    assert!(near(xs, 5.0), "moved mesh re-cut: {xs}");
    assert_eq!(m.mesh_plan(l, 1.2).unwrap(), c);
}

#[test]
fn plan_takes_meshes_of_other_levels_crossing_the_storey() {
    let (mut m, l) = model();
    let l2 = m.add_level("L2", 3.0).unwrap();
    // a level-2 mesh that descends into storey 1 (a two-storey column); the second one is only above
    let tall = prism(rect(0.0, 0.0, 0.4, 0.4), 6.0, &Affine::IDENTITY);
    let high = prism(
        rect(2.0, 0.0, 2.4, 0.4),
        2.0,
        &Affine::translate(v3(0.0, 0.0, 4.0)),
    );
    let t = add(&m, l2, "Generic", &tall);
    add(&m, l2, "Generic", &high);
    let p = m.mesh_plan(l, 1.2).unwrap();
    assert_eq!(p.iter().map(|p| p.mesh).collect::<Vec<_>>(), vec![t]);
}

#[test]
fn uncappable_cut_is_an_error() {
    let c = mesh_ops::CutOut {
        mesh: prism(rect(0.0, 0.0, 1.0, 1.0), 1.0, &Affine::IDENTITY),
        capped: false,
        source_closed: true,
    };
    assert!(capped(c.clone()).is_err());
    let open = mesh_ops::CutOut {
        source_closed: false,
        ..c
    };
    assert!(capped(open).is_ok(), "an open source may stay open");
}

#[test]
fn merge_marks_and_face_delete_by_direction() {
    let (mut m, l) = model();
    let a = add(
        &m,
        l,
        "Generic",
        &prism(rect(0.0, 0.0, 1.0, 1.0), 1.0, &Affine::IDENTITY),
    );
    let b = add(
        &m,
        l,
        "Generic",
        &prism(rect(3.0, 0.0, 4.0, 1.0), 1.0, &Affine::IDENTITY),
    );
    m.merge_meshes(&[a, b]).unwrap();
    assert_eq!(
        prop_of(&m, "mesh", a, MERGED_FROM).as_deref(),
        Some(format!("{a},{b}").as_str())
    );
    m.undo().unwrap();
    assert_eq!(prop_of(&m, "mesh", a, MERGED_FROM), None);
    assert_eq!(count(&m, "mesh"), 2);

    let before = blobs(&m, a);
    let n = m
        .delete_mesh_faces_facing(&[a], v3(0.0, 0.0, 1.0), 10.0)
        .unwrap();
    assert_eq!(n, 2, "the two top triangles");
    let st = m.mesh_stats(a).unwrap();
    assert!(!st.closed && st.max.z > 0.99);
    assert!(m
        .delete_mesh_faces_facing(&[a], v3(0.0, 0.0, 1.0), 10.0)
        .is_err());
    m.undo().unwrap();
    assert_eq!(blobs(&m, a), before);
}

#[test]
fn section_hides_edges_behind_cut_faces() {
    let (m, l) = model();
    add(
        &m,
        l,
        "Generic",
        &prism(rect(0.0, 0.0, 4.0, 1.0), 3.0, &Affine::IDENTITY),
    );
    // behind the plane (y > 0.5): one mesh is entirely behind the section fill, the other is to the side
    let hidden = add(
        &m,
        l,
        "Generic",
        &prism(
            rect(1.0, 2.0, 2.0, 3.0),
            1.0,
            &Affine::translate(v3(0.0, 0.0, 1.0)),
        ),
    );
    let aside = add(
        &m,
        l,
        "Generic",
        &prism(rect(10.0, 2.0, 11.0, 3.0), 1.0, &Affine::IDENTITY),
    );
    let mut prims = vec![];
    section_prims(
        m.sql(),
        v2(-1.0, 0.5),
        v2(1.0, 0.0),
        Some(10.0),
        true,
        &mut prims,
        &mut |_| {},
    )
    .unwrap();
    let edges_of = |e: i64| {
        prims
            .iter()
            .filter(|p| matches!(p, ViewPrim::Edge { element, .. } if *element == e))
            .count()
    };
    assert_eq!(edges_of(hidden), 0);
    assert!(edges_of(aside) > 0);
}

/// UV sphere; an odd stack count keeps every vertex ring off the centre plane.
fn sphere(c: V3, r: f64) -> Mesh {
    use std::f64::consts::PI;
    let (slices, stacks) = (64u32, 31u32);
    let mut positions = vec![v3(c.x, c.y, c.z + r)];
    for i in 1..stacks {
        let th = PI * f64::from(i) / f64::from(stacks);
        for j in 0..slices {
            let ph = 2.0 * PI * f64::from(j) / f64::from(slices);
            positions.push(v3(
                c.x + r * th.sin() * ph.cos(),
                c.y + r * th.sin() * ph.sin(),
                c.z + r * th.cos(),
            ));
        }
    }
    positions.push(v3(c.x, c.y, c.z - r));
    let bottom = positions.len() as u32 - 1;
    let ring = |i: u32, j: u32| 1 + (i - 1) * slices + j % slices;
    let mut triangles = vec![];
    for j in 0..slices {
        triangles.push([0, ring(1, j), ring(1, j + 1)]);
        for i in 1..stacks - 1 {
            let (a, b) = (ring(i, j), ring(i, j + 1));
            let (c, d) = (ring(i + 1, j), ring(i + 1, j + 1));
            triangles.push([a, c, d]);
            triangles.push([a, d, b]);
        }
        triangles.push([bottom, ring(stacks - 1, j + 1), ring(stacks - 1, j)]);
    }
    let face_of = vec![0; triangles.len()];
    Mesh {
        positions,
        triangles,
        face_of,
    }
}

fn within(a: f64, b: f64, rel: f64) -> bool {
    (a - b).abs() <= rel * b.abs()
}

#[test]
fn plan_sphere_centred_at_cut_is_one_closed_circle() {
    let (m, l) = model();
    add(&m, l, "Generic", &sphere(v3(1.0, 1.0, 1.2), 0.5));
    let p = m.mesh_plan(l, 1.2).unwrap();
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].cut.len(), 1, "{:?}", p[0].cut);
    assert!(p[0].cut[0].holes.is_empty());
    assert!(p[0].open.is_empty());
    let a = p[0].cut[0].area();
    let circle = std::f64::consts::PI * 0.25;
    assert!(within(a, circle, 0.05), "{a} vs {circle}");
    assert!(!p[0].fill.is_empty());
    // a mesh crossing the cut is drawn by its section only, not by an outline too
    assert!(p[0].beyond.is_empty());
}

#[test]
fn plan_cube_cut_is_a_square() {
    let (m, l) = model();
    add(
        &m,
        l,
        "Generic",
        &prism(rect(0.0, 0.0, 1.0, 1.0), 3.0, &Affine::IDENTITY),
    );
    let p = m.mesh_plan(l, 1.2).unwrap();
    assert_eq!(p[0].cut.len(), 1);
    let o = &p[0].cut[0].outer;
    assert_eq!(o.len(), 4, "{o:?}");
    for c in rect(0.0, 0.0, 1.0, 1.0) {
        assert!(o.iter().any(|q| (*q - c).len() < 1e-6), "corner {c:?} in {o:?}");
    }
    assert!(near(p[0].cut[0].area(), 1.0));
}

#[test]
fn plan_mesh_above_cut_gives_overhead_hull() {
    let (m, l) = model();
    add(&m, l, "Generic", &sphere(v3(1.0, 1.0, 2.5), 0.5));
    let p = m.mesh_plan(l, 1.2).unwrap();
    assert_eq!(p.len(), 1);
    assert!(p[0].cut.is_empty() && p[0].open.is_empty());
    assert!(p[0].overhead);
    let a = signed_area(&p[0].beyond);
    let circle = std::f64::consts::PI * 0.25;
    assert!(a > 0.0 && within(a, circle, 0.05), "{a} vs {circle}");
}

#[test]
fn plan_smooth_mesh_below_cut_gives_full_outline() {
    // a sphere at the level, below the plan cut, was only a short dash
    let (m, l) = model();
    add(&m, l, "Generic", &sphere(v3(1.4, 1.0, 0.0), 0.5));
    let p = m.mesh_plan(l, 1.2).unwrap();
    assert_eq!(p.len(), 1);
    assert!(!p[0].overhead);
    let a = signed_area(&p[0].beyond);
    assert!(within(a, std::f64::consts::PI * 0.25, 0.05), "{a}");
}

#[test]
fn plan_outline_is_emitted_once() {
    // one closed ring per mesh, no repeated corners, and a cached frame does not add another
    let (m, l) = model();
    add(
        &m,
        l,
        "Generic",
        &prism(rect(0.0, 0.0, 2.0, 1.0), 0.5, &Affine::IDENTITY),
    );
    let mut cache = MeshPlanCache::default();
    for _ in 0..2 {
        let p = m.mesh_plan_cached(l, 1.2, &mut cache).unwrap();
        assert_eq!(p.len(), 1);
        let h = &p[0].beyond;
        assert_eq!(h.len(), 4, "{h:?}");
        for (i, a) in h.iter().enumerate() {
            assert!(h[i + 1..].iter().all(|b| (*a - *b).len() > 1e-6), "{h:?}");
        }
        assert!(near(signed_area(h), 2.0));
    }
    assert_eq!(cache.computed(), 1);
}

#[test]
fn plan_open_non_manifold_mesh_yields_open_polylines() {
    let (m, l) = model();
    // three fins sharing one vertical edge: open and non-manifold
    let mut positions = vec![v3(0.0, 0.0, 0.0), v3(0.0, 0.0, 3.0)];
    let mut triangles = vec![];
    for (dx, dy) in [(1.0, 0.0), (-0.5, 0.8), (-0.5, -0.8)] {
        let k = positions.len() as u32;
        positions.push(v3(dx, dy, 0.0));
        positions.push(v3(dx, dy, 3.0));
        triangles.push([0, k, k + 1]);
        triangles.push([0, k + 1, 1]);
    }
    let face_of = vec![0; triangles.len()];
    let fins = Mesh {
        positions,
        triangles,
        face_of,
    };
    add(&m, l, "Generic", &fins);
    let p = m.mesh_plan(l, 1.2).unwrap();
    assert_eq!(p.len(), 1);
    assert!(p[0].cut.is_empty(), "{:?}", p[0].cut);
    assert!(!p[0].open.is_empty());
    assert!(p[0].open.iter().all(|l| l.len() >= 2));
}

#[test]
fn plan_cache_hits_on_unchanged_mesh() {
    let (mut m, l) = model();
    let id = add(&m, l, "Generic", &sphere(v3(0.0, 0.0, 1.2), 0.5));
    let mut cache = MeshPlanCache::default();
    let a = m.mesh_plan_cached(l, 1.2, &mut cache).unwrap();
    assert_eq!(cache.computed(), 1);
    let b = m.mesh_plan_cached(l, 1.2, &mut cache).unwrap();
    assert_eq!(cache.computed(), 1, "unchanged mesh is not cut again");
    assert_eq!(a, b);
    m.transform_meshes(&[id], &Affine::translate(v3(1.0, 0.0, 0.0)))
        .unwrap();
    m.mesh_plan_cached(l, 1.2, &mut cache).unwrap();
    assert_eq!(cache.computed(), 2, "edited mesh is cut again");
}

fn three_walls() -> (Model, Vec<i64>, Vec<(i64, Fit)>) {
    let (m, l) = model();
    let ids: Vec<i64> = (0..3)
        .map(|k| {
            let y = k as f64 * 5.0;
            add(
                &m,
                l,
                "Walls",
                &prism(rect(0.0, y, 6.0, y + 0.2), 3.0, &Affine::IDENTITY),
            )
        })
        .collect();
    let items = ids
        .iter()
        .map(|&id| (id, m.recognize_mesh(id, None).unwrap()[0].fit.clone()))
        .collect();
    (m, ids, items)
}

#[test]
fn batch_convert_keeping_sources_leaves_every_mesh() {
    let (mut m, ids, items) = three_walls();
    let before: Vec<_> = ids.iter().map(|&i| blobs(&m, i)).collect();
    let r = m.convert_meshes(&items, true).unwrap();
    assert_eq!((r.done.len(), r.failed.len()), (3, 0));
    assert_eq!(count(&m, "wall"), 3);
    assert_eq!(count(&m, "mesh"), 3, "sources kept");
    for (i, &id) in ids.iter().enumerate() {
        assert_eq!(blobs(&m, id), before[i]);
    }
    m.undo().unwrap();
    assert_eq!(count(&m, "wall"), 0, "one undo removes all new elements");
    assert_eq!(count(&m, "mesh"), 3);
}

#[test]
fn batch_convert_removing_sources_takes_only_converted_and_undoes_in_one_step() {
    let (mut m, ids, items) = three_walls();
    let before: Vec<_> = ids.iter().map(|&i| blobs(&m, i)).collect();
    // two of three are converted; the third is not in the batch
    let r = m.convert_meshes(&items[..2], false).unwrap();
    assert_eq!(r.done.len(), 2);
    assert_eq!(count(&m, "wall"), 2);
    assert_eq!(count(&m, "mesh"), 1, "preview said: create 2, remove 2");
    let left: i64 = m
        .sql()
        .query_row("SELECT id FROM mesh", [], |r| r.get(0))
        .unwrap();
    assert_eq!(left, ids[2]);
    m.undo().unwrap();
    assert_eq!(count(&m, "wall"), 0);
    assert_eq!(count(&m, "mesh"), 3, "one undo restores every source");
    for (i, &id) in ids.iter().enumerate() {
        assert_eq!(blobs(&m, id), before[i]);
    }
}
