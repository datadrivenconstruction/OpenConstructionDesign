// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;

fn cube(min: [f64; 3], max: [f64; 3]) -> Body {
    let [x0, y0, z0] = min;
    let [x1, y1, z1] = max;
    let pos = vec![
        [x0, y0, z0],
        [x1, y0, z0],
        [x1, y1, z0],
        [x0, y1, z0],
        [x0, y0, z1],
        [x1, y0, z1],
        [x1, y1, z1],
        [x0, y1, z1],
    ];
    let tri = vec![
        [0, 2, 1],
        [0, 3, 2],
        [4, 5, 6],
        [4, 6, 7],
        [0, 1, 5],
        [0, 5, 4],
        [1, 2, 6],
        [1, 6, 5],
        [2, 3, 7],
        [2, 7, 6],
        [3, 0, 4],
        [3, 4, 7],
    ];
    Body { pos, tri }
}

#[test]
fn link_crud_and_undo() {
    let mut m = Model::open(":memory:").unwrap();
    assert_eq!(m.schema_version().unwrap(), crate::schema::VERSION);
    assert!(m.add_link("a/b.dwg", "", Transform::default()).is_err());
    let xf = Transform {
        dx: 10.0,
        dy: -2.0,
        dz: 0.5,
        rot: 0.3,
    };
    let id = m.add_link("dir/House.IFC", "", xf).unwrap();
    let l = m.link(id).unwrap().unwrap();
    assert_eq!(
        (l.name.as_str(), l.format.as_str(), l.xf, l.visible),
        ("House", "ifc", xf, true)
    );
    let moved = Transform { dx: 1.0, ..xf };
    m.update_link(id, moved, false).unwrap();
    let l = m.link(id).unwrap().unwrap();
    assert_eq!((l.xf, l.visible), (moved, false));
    // the load stamp is not journaled: undo reverts the placement edit, not the load
    m.mark_link_loaded(id, 1_700_000_000, "").unwrap();
    m.undo().unwrap();
    let l = m.link(id).unwrap().unwrap();
    assert_eq!((l.xf, l.visible), (xf, true));
    m.remove_link(id).unwrap();
    assert!(m.links().unwrap().is_empty());
    m.undo().unwrap();
    assert_eq!(m.links().unwrap().len(), 1);
    assert_eq!(m.links().unwrap()[0].id, id);
    m.undo().unwrap();
    assert!(m.links().unwrap().is_empty());
    assert!(m.update_link(id, xf, true).is_err());
    assert!(m.remove_link(id).is_err());
    let bad = Transform { dx: f64::NAN, ..xf };
    assert!(m.add_link("x.rvt", "X", bad).is_err());
    assert_eq!(link_format(" p/q.Rvt "), Some("rvt"));
}

#[test]
fn transform_round_trip() {
    let xf = Transform {
        dx: 3.0,
        dy: 4.0,
        dz: -1.0,
        rot: std::f64::consts::FRAC_PI_2,
    };
    let q = xf.apply([1.0, 0.0, 2.0]);
    assert!((q[0] - 3.0).abs() < 1e-12 && (q[1] - 5.0).abs() < 1e-12 && (q[2] - 1.0).abs() < 1e-12);
    for p in [[0.0, 0.0, 0.0], [12.5, -7.25, 3.0], [-1e3, 2e3, 0.1]] {
        let r = xf.inverse(xf.apply(p));
        assert!((0..3).all(|k| (r[k] - p[k]).abs() < 1e-9), "{p:?} -> {r:?}");
    }
    assert_eq!(Transform::default().apply([1.0, 2.0, 3.0]), [1.0, 2.0, 3.0]);
}

#[test]
fn clash_overlap_touch_and_separate() {
    let host = vec![cube([0.0; 3], [1.0, 1.0, 1.0])];
    let link = vec![
        cube([0.5, 0.5, 0.5], [1.5, 1.5, 1.5]),
        cube([1.0, 0.0, 0.0], [2.0, 1.0, 1.0]),
        cube([3.0, 3.0, 3.0], [4.0, 4.0, 4.0]),
        cube([0.25, 0.25, 0.25], [0.75, 0.75, 0.75]),
    ];
    let c = clash(&host, &link, 1e-3, 100);
    let pairs: Vec<(usize, usize)> = c.iter().map(|c| (c.host, c.link)).collect();
    // intersection and containment: yes; face contact and separated solids: no
    assert_eq!(pairs, vec![(0, 0), (0, 3)]);
    assert!((c[0].min[0] - 0.5).abs() < 1e-12 && (c[0].max[0] - 1.0).abs() < 1e-12);
    assert_eq!(clash(&host, &link, 1e-3, 1).len(), 1);
}

#[test]
fn section_of_box() {
    let b = cube([0.0; 3], [2.0, 1.0, 3.0]);
    let segs = section(&b.pos, &b.tri, 1.2);
    // four side faces, two triangles each
    assert_eq!(segs.len(), 8);
    let near = |v: f64, w: f64| (v - w).abs() < 1e-9;
    let on_side =
        |p: &[f64; 2]| near(p[0], 0.0) || near(p[0], 2.0) || near(p[1], 0.0) || near(p[1], 1.0);
    assert!(segs.iter().flatten().all(on_side));
    assert!(section(&b.pos, &b.tri, 5.0).is_empty());
    assert!(section(&b.pos, &b.tri, -0.5).is_empty());
}

#[test]
fn clash_sweep_matches_all_pairs() {
    // a row of host boxes and link boxes shifted by half a box: each link body meets two hosts
    let host: Vec<Body> = (0..40)
        .map(|i| cube([i as f64 * 2.0, 0.0, 0.0], [i as f64 * 2.0 + 1.5, 1.0, 1.0]))
        .collect();
    let link: Vec<Body> = (0..40)
        .map(|i| {
            cube(
                [i as f64 * 2.0 + 1.0, 0.25, 0.25],
                [i as f64 * 2.0 + 2.5, 0.75, 0.75],
            )
        })
        .collect();
    let c = clash(&host, &link, 1e-3, usize::MAX);
    let mut want = vec![];
    for i in 0..40usize {
        for j in 0..40usize {
            if j == i || j + 1 == i {
                want.push((i, j));
            }
        }
    }
    let got: Vec<(usize, usize)> = c.iter().map(|c| (c.host, c.link)).collect();
    assert_eq!(got, want);
    assert_eq!(clash(&host, &link, 1e-3, 3).len(), 3);
}

#[test]
fn moving_a_link_after_reload_keeps_the_reload_on_undo() {
    let mut m = Model::open(":memory:").unwrap();
    let id = m.add_link("a.ifc", "", Transform::default()).unwrap();
    m.mark_link_loaded(id, 100, "old").unwrap();
    m.update_link(
        id,
        Transform {
            dx: 2.0,
            ..Default::default()
        },
        true,
    )
    .unwrap();
    m.mark_link_loaded(id, 200, "new").unwrap();
    m.undo().unwrap();
    let l = m.link(id).unwrap().unwrap();
    assert_eq!(
        (l.xf, l.loaded_at, l.cache.as_str()),
        (Transform::default(), 200, "new")
    );
}
