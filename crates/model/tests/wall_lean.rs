//! Leaning and tapered walls in the model: quantities against the analytic solids, a door cut
//! normal to the surface, file round trip, undo, refusal where the lean cannot hold.

use ddc_geom::wall_lean::cut_area;
use ddc_model::{Lean, LayerSpec, Model, OpeningSpec, WallSpec};

fn one_wall(m: &mut Model, t: f64, len: f64, h: f64) -> i64 {
    let lv = m.add_level("L1", 0.0).unwrap();
    let ty = m
        .add_wall_type(
            "w",
            &[LayerSpec {
                material: "brick".into(),
                thickness: t,
            }],
        )
        .unwrap();
    m.add_wall(&WallSpec {
        a: (0.0, 0.0),
        b: (len, 0.0),
        level: lv,
        base_offset: 0.0,
        height: h,
        wall_type: ty,
        offset0: None,
    })
    .unwrap()
}

fn volume(m: &Model) -> f64 {
    m.material_volumes().unwrap().iter().map(|v| v.1).sum()
}

#[test]
fn leaning_and_tapered_volumes_are_analytic() {
    let mut m = Model::open(":memory:").unwrap();
    let w = one_wall(&mut m, 0.3, 6.0, 3.0);
    let plain = volume(&m);
    assert!((plain - 6.0 * 0.3 * 3.0).abs() < 1e-9);
    // lean only: a shear, the volume stays plan area times height
    m.set_wall_lean(w, Lean::new(10f64.to_radians(), 1.0)).unwrap();
    assert!((volume(&m) - plain).abs() < 1e-9, "{}", volume(&m));
    // taper to 40 %: trapezoid section (0.3 + 0.12) / 2 * 3 times the length
    m.set_wall_lean(w, Lean::new(10f64.to_radians(), 0.4)).unwrap();
    let want = 6.0 * (0.3 + 0.12) / 2.0 * 3.0;
    assert!((volume(&m) - want).abs() < 1e-9, "{} vs {want}", volume(&m));
    // undo steps back to the lean only, then to the vertical wall
    m.undo().unwrap();
    assert!((m.wall_lean(w).unwrap().top_scale - 1.0).abs() < 1e-12);
    m.undo().unwrap();
    assert!(m.wall_lean(w).unwrap().is_plain());
    assert!((volume(&m) - plain).abs() < 1e-9);
}

#[test]
fn door_in_leaning_wall_removes_its_normal_section() {
    let mut m = Model::open(":memory:").unwrap();
    let w = one_wall(&mut m, 0.3, 5.0, 3.0);
    let l = Lean::new(12f64.to_radians(), 1.0);
    m.set_wall_lean(w, l).unwrap();
    m.add_opening(
        w,
        &OpeningSpec {
            along: 1.5,
            width: 0.9,
            sill: 0.0,
            height: 2.1,
            kind: "door".into(),
        },
    )
    .unwrap();
    // cut normal to the surface: width x height along the surface x thickness normal to it
    let cut = 0.9 * cut_area(&l, 3.0, -0.15, 0.15, 0.0, 2.1);
    assert!((cut - 0.9 * 2.1 * 0.3 * l.angle.cos()).abs() < 1e-9);
    let want = 5.0 * 0.3 * 3.0 - cut;
    assert!((volume(&m) - want).abs() < 1e-9, "{} vs {want}", volume(&m));
}

#[test]
fn lean_survives_save_and_reopen() {
    let dir = std::env::temp_dir().join(format!("ddc_lean_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("lean.ddc");
    let p = path.to_string_lossy().to_string();
    let _ = std::fs::rename(&path, dir.join("old.ddc"));
    let (w, vol) = {
        let mut m = Model::open(&p).unwrap();
        let w = one_wall(&mut m, 0.25, 4.0, 3.5);
        m.set_wall_lean(w, Lean::new(-15f64.to_radians(), 0.7)).unwrap();
        (w, volume(&m))
    };
    let m = Model::open(&p).unwrap();
    let l = m.wall_lean(w).unwrap();
    assert!((l.angle + 15f64.to_radians()).abs() < 1e-12 && (l.top_scale - 0.7).abs() < 1e-12);
    assert!((volume(&m) - vol).abs() < 1e-12);
}

#[test]
fn lean_is_refused_out_of_range() {
    let mut m = Model::open(":memory:").unwrap();
    let w = one_wall(&mut m, 0.3, 6.0, 3.0);
    assert!(m.set_wall_lean(w, Lean::new(70f64.to_radians(), 1.0)).is_err());
    assert!(m.set_wall_lean(w, Lean::new(0.0, 0.0)).is_err());
    assert!(m.wall_lean(w).unwrap().is_plain());
}

/// An arc wall leans by the radial map: every face a cone about the arc's centre. The layer
/// section at z is the annular sector of sweep th between radii r - (+-t/2 k + shift), area
/// th t k (r - shift), quadratic in z; a door removes its section turned about the centre.
#[test]
fn arc_wall_leans_and_tapers_with_analytic_volume() {
    let mut m = Model::open(":memory:").unwrap();
    let (t, h, b) = (0.3, 3.0, 0.3);
    let w = one_wall(&mut m, t, 6.0, h);
    m.set_wall_bulge(w, b).unwrap();
    let plain = volume(&m);
    let l = Lean::new(7f64.to_radians(), 0.7);
    m.set_wall_lean(w, l).unwrap();
    assert_eq!(m.wall_lean(w).unwrap(), l);
    let th = 4.0 * f64::atan(b);
    let r = 6.0 * (1.0 + b * b) / (4.0 * b);
    let f = |z: f64| th * t * l.k(z, h) * (r - l.shift(z));
    let want = h / 6.0 * (f(0.0) + 4.0 * f(h / 2.0) + f(h));
    assert!((volume(&m) - want).abs() < 1e-9, "{} vs {want}", volume(&m));
    assert!((plain - th * t * r * h).abs() < 1e-9);
    // a door: the cut section's area times the radius of its centroid times the door's angle
    let door = OpeningSpec {
        along: 2.0,
        width: 0.9,
        sill: 0.0,
        height: 2.1,
        kind: "door".into(),
    };
    m.add_opening(w, &door).unwrap();
    let ops = [ddc_geom::layered::Opening {
        along: 2.0,
        width: 0.9,
        sill: 0.0,
        height: 2.1,
    }];
    let cut = ddc_geom::wall_lean::openings_volume(&l, h, -t / 2.0, t / 2.0, &ops, Some((r, 1.0)));
    assert!(cut > 0.0);
    assert!((volume(&m) - (want - cut)).abs() < 1e-9);
    // the lean holds: no warning that the geometry ignores it
    assert!(m.wall_warnings().unwrap().iter().all(|x| !x.message.contains("ignored")));
}

/// A room bounded by a wall that leans into it: its area is taken at the 1.2 m cut height,
/// where the wall face has moved in by 1.2 tan(lean); undo restores the plan area.
#[test]
fn room_sees_leaning_wall_at_the_cut_height() {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 2.0).unwrap();
    let ty = m.add_wall_type("w", &[LayerSpec { material: "brick".into(), thickness: 0.3 }]).unwrap();
    let p = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
    let walls: Vec<i64> = (0..4)
        .map(|i| {
            m.add_wall(&WallSpec {
                a: p[i],
                b: p[(i + 1) % 4],
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: ty,
                offset0: None,
            })
            .unwrap()
        })
        .collect();
    let room = m
        .add_room(&ddc_model::RoomSpec {
            level: lv,
            name: "Hall".into(),
            number: "1".into(),
            point: (5.0, 5.0),
            height: 3.0,
        })
        .unwrap();
    let area = |m: &Model| m.room_quantities(room).unwrap().expect("room closes").area;
    assert!((area(&m) - 9.7 * 9.7).abs() < 1e-6, "{}", area(&m));
    // the first wall runs along x with its left normal into the room
    let th = 8f64.to_radians();
    m.set_wall_lean(walls[0], Lean::new(th, 1.0)).unwrap();
    let want = 9.7 * (9.7 - 1.2 * th.tan());
    assert!((area(&m) - want).abs() < 1e-6, "{} vs {want}", area(&m));
    // leaning away from the room widens it the same way
    m.set_wall_lean(walls[0], Lean::new(-th, 1.0)).unwrap();
    let want = 9.7 * (9.7 + 1.2 * th.tan());
    assert!((area(&m) - want).abs() < 1e-6, "{} vs {want}", area(&m));
    m.undo().unwrap();
    m.undo().unwrap();
    assert!((area(&m) - 9.7 * 9.7).abs() < 1e-6);
}
