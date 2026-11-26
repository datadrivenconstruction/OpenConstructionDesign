//! Walls built through the assistant's paths (a generated project with every roof preset, a
//! free-form shell added by ops) reach the roof underside: the roof-trim sync runs on each.

use super::tests::editor;
use crate::ai::plan::{compile_one, CompileCx, PlanOp};
use crate::ai::project_plan::ProjectOp;
use crate::editor::Editor;
use ddc_geom::arc::ArcAxis;
use ddc_geom::{v2, Tol, V2};
use serde_json::json;

const POKE: f64 = 5e-3;
const SHORT: f64 = 0.05;

type Under<'a> = &'a dyn Fn(V2) -> Option<f64>;

fn at(top: &[(f64, f64)], u: f64) -> f64 {
    top.windows(2)
        .find(|w| u >= w[0].0 - 1e-9 && u <= w[1].0 + 1e-9 && w[1].0 - w[0].0 > 1e-9)
        .map_or_else(
            || top.last().map_or(0.0, |p| p.1),
            |w| w[0].1 + (w[1].1 - w[0].1) * (u - w[0].0) / (w[1].0 - w[0].0),
        )
}

/// Walls whose nominal top reaches `eave` (the roof's base line): the ones standing under it.
fn roof_walls(ed: &Editor, eave: f64) -> Vec<i64> {
    let mut st = ed
        .model
        .sql()
        .prepare(
            "SELECT w.id FROM wall w JOIN level l ON l.id = w.level \
             WHERE l.elevation + w.base_offset + w.height >= ?1 - 0.3",
        )
        .unwrap();
    st.query_map([eave], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
}

/// Every wall in `walls` that has the roof over most of its length is trimmed to the underside
/// `under` (absolute z): nothing above it, no gap over `SHORT`. Returns how many it checked.
fn reach(ed: &Editor, walls: &[i64], what: &str, under: Under) -> usize {
    let mut checked = 0;
    for &w in walls {
        let (a, b, bulge, base): ((f64, f64), (f64, f64), f64, f64) = ed
            .model
            .sql()
            .query_row(
                "SELECT w.ax,w.ay,w.bx,w.by,w.bulge,l.elevation + w.base_offset \
                 FROM wall w JOIN level l ON l.id=w.level WHERE w.id=?1",
                [w],
                |r| Ok(((r.get(0)?, r.get(1)?), (r.get(2)?, r.get(3)?), r.get(4)?, r.get(5)?)),
            )
            .unwrap();
        let arc = (bulge != 0.0).then(|| ArcAxis::new(v2(a.0, a.1), v2(b.0, b.1), bulge, Tol::default()).unwrap());
        let len = arc.as_ref().map_or(((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt(), |x| x.length());
        let pt = |u: f64| match &arc {
            Some(x) => x.point_at(u.clamp(0.0, len)),
            None => {
                let s = (u / len).clamp(0.0, 1.0);
                v2(a.0 + (b.0 - a.0) * s, a.1 + (b.1 - a.1) * s)
            }
        };
        let (mut seen, mut total, mut u) = (0, 0, 0.0);
        while u <= len + 1e-9 {
            total += 1;
            seen += under(pt(u)).is_some() as usize;
            u += 0.1;
        }
        if seen * 10 < total * 7 {
            continue;
        }
        let top = ed
            .model
            .wall_top_profile(w)
            .unwrap()
            .unwrap_or_else(|| panic!("{what}: wall {w} stands under the roof and has no trimmed top"));
        checked += 1;
        let mut u = 0.0;
        while u <= len + 1e-9 {
            if let Some(z) = under(pt(u)) {
                let wz = base + at(&top, u);
                let near: Vec<f64> = [1e-3, -1e-3].iter().filter_map(|d| under(pt(u + d))).collect();
                let lo = near.iter().cloned().fold(z, f64::min);
                let hi = near.iter().cloned().fold(z, f64::max);
                assert!(wz <= hi + POKE, "{what}: wall {w} pokes through the roof at u={u:.2}: {wz} > {hi}");
                assert!(lo - wz <= SHORT, "{what}: wall {w} stops {} m short of the roof at u={u:.2}", lo - wz);
            }
            u += 0.1;
        }
    }
    checked
}

fn project(input: serde_json::Value) -> Editor {
    let mut ed = editor();
    let op = PlanOp::Project(ProjectOp { input, level: ed.level, slot0: 0, pid: "P1".into() });
    let mut slots = std::collections::HashMap::new();
    let sets = crate::ai::sets::Sets::default();
    let mut cx = CompileCx {
        ed: &mut ed,
        slots: &mut slots,
        sets: &sets,
        gens: &[],
        rules: &[],
        declared: Default::default(),
        skips: vec![],
    };
    compile_one(&mut cx, &op).map_err(|e| e.0).unwrap();
    ed
}

#[test]
fn a_generated_house_has_its_walls_trimmed_under_every_roof_preset() {
    for kind in ["pitched", "hip", "gable", "shed", "barrel"] {
        let ed = project(json!({"typology": "house", "floors": 2, "lang": "en", "roof": {"kind": kind}}));
        let ids: Vec<i64> = ed
            .model
            .sql()
            .prepare("SELECT id FROM roof")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(!ids.is_empty(), "{kind}: no roof was built");
        let mut checked = 0;
        for id in ids {
            let spec = ed.model.roof(id).unwrap();
            let elev: f64 = ed
                .model
                .sql()
                .query_row("SELECT elevation FROM level WHERE id=?1", [spec.level], |r| r.get(0))
                .unwrap();
            let base = elev + spec.offset;
            let g = ed.model.roof_geometry(id).unwrap();
            let walls = roof_walls(&ed, base);
            checked += reach(&ed, &walls, kind, &|p| g.underside_at(p, 0.2).map(|z| base + z));
        }
        assert!(checked > 0, "{kind}: no wall stands under the roof");
    }
}

#[test]
fn walls_around_a_free_form_shell_added_by_ops_are_trimmed_to_it() {
    use ddc_model::WallSpec;
    use ddc_ops::{Actor, Cat, Envelope, GeoOp, Group, Len, Op, Ref, ShellKind, ShellLayer, SurfaceBy, P3};
    let mut ed = editor();
    let level = ed.level;
    let elev: f64 = ed.model.sql().query_row("SELECT elevation FROM level WHERE id=?1", [level], |r| r.get(0)).unwrap();
    assert!(elev.abs() < 1e-9, "the test assumes a ground level at 0");
    let wtype: i64 = ed.model.sql().query_row("SELECT id FROM wall_type LIMIT 1", [], |r| r.get(0)).unwrap();
    let (r, rise) = (4.5, 2.0);
    let dome = Op::Geometry(GeoOp::Freeform {
        slot: 0,
        level: Some(Ref::id(Cat::Level, level)),
        surface: SurfaceBy::Dome { center: P3::m(5.0, 3.0, 3.0), radius: Len::m(r), rise: Len::m(rise) },
        thickness: None,
        side: None,
        trim: None,
        material: None,
        name: Some("Museum shell".into()),
    });
    let roof = Op::Geometry(GeoOp::SurfaceShell {
        slot: 1,
        host: Ref::slot(0),
        kind: ShellKind::Roof,
        layers: vec![ShellLayer { thickness: Len::mm(200.0), material: None, name: None }],
        side: None,
        openings: None,
        name: Some("Museum roof".into()),
    });
    let add = |ed: &mut Editor, a: (f64, f64), b: (f64, f64)| {
        let spec = WallSpec { a, b, level, base_offset: 0.0, height: 6.0, wall_type: wtype, offset0: None };
        ed.model.add_wall(&spec).unwrap()
    };
    // two walls before the shell (its op re-trims them), one after (the wall op trims it)
    let mut walls = vec![add(&mut ed, (1.0, 3.0), (9.0, 3.0)), add(&mut ed, (5.0, 1.5), (5.0, 4.5))];
    let mut env = Envelope::new(Actor::Agent { name: "test".into() }, "museum");
    env.groups.push(Group { id: 1, title: "g".into(), ops: vec![dome, roof], depends_on: vec![], why: String::new() });
    ed.model.apply_envelope(&env).unwrap();
    walls.push(add(&mut ed, (3.0, 2.0), (7.0, 4.0)));
    let rho = (r * r + rise * rise) / (2.0 * rise);
    let z = move |p: V2| {
        let d2 = (p.x - 5.0).powi(2) + (p.y - 3.0).powi(2);
        (d2 < (r - 0.05).powi(2)).then(|| 3.0 + rise - rho + (rho * rho - d2).sqrt())
    };
    assert_eq!(reach(&ed, &walls, "museum shell", &z), walls.len());
}

/// An L-shaped hip roof opened round a head whose corner sits on the valley: every piece reaches
/// the model as a roof that builds, the corner piece on its own eave line as one plane at the
/// roof's slope.
#[test]
fn an_l_hip_opened_at_a_valley_corner_reaches_the_model_piece_by_piece() {
    use crate::ai::plan::NewElem;
    let r = ddc_gen::project::PRoof {
        name: "Roof".into(),
        elevation: 0.0,
        ring: vec![(0.0, 0.0), (20.0, 0.0), (20.0, 8.0), (8.0, 8.0), (8.0, 20.0), (0.0, 20.0)],
        slope: 30.0,
        overhang: 0.0,
        edges: vec![],
        vault: None,
        lift: 0.0,
        eave: None,
    };
    let head = [(6.0, 3.0), (10.0, 3.0), (10.0, 6.0), (6.0, 6.0)];
    let parts = ddc_gen::project::fire::plane_pieces(&r, &head).expect("the roof opens");
    assert!(parts.iter().any(|q| q.eave.is_some()), "the valley corner hangs on its own eave line");
    let mut ed = editor();
    super::tests::sql(&ed, "INSERT INTO slab_type(id, name) VALUES(901, 'Roof'); INSERT INTO slab_type_layer(stype, pos, material, thickness) VALUES(901, 0, 'Concrete', 0.2)");
    let items: Vec<NewElem> = parts
        .iter()
        .map(|q| NewElem::Roof {
            level: ed.level,
            stype: None,
            offset: q.lift,
            overhang: q.overhang,
            ring: q.ring.clone(),
            slope: Some(q.slope),
            edges: q.edges.clone(),
            vault: None,
            eave: q.eave,
        })
        .collect();
    let op = PlanOp::Create { slot0: 0, items };
    let mut slots = std::collections::HashMap::new();
    let sets = crate::ai::sets::Sets::default();
    let mut cx =
        CompileCx { ed: &mut ed, slots: &mut slots, sets: &sets, gens: &[], rules: &[], declared: Default::default(), skips: vec![] };
    compile_one(&mut cx, &op).map_err(|e| e.0).unwrap();
    let ids: Vec<i64> =
        ed.model.sql().prepare("SELECT id FROM roof").unwrap().query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
    assert_eq!(ids.len(), parts.len(), "every piece is a roof in the model");
    let tan = 30f64.to_radians().tan();
    for (id, q) in ids.iter().zip(&parts) {
        let g = ed.model.roof_geometry(*id).unwrap_or_else(|e| panic!("piece {:?} does not build: {e:?}", q.ring));
        let Some((a, b)) = q.eave else { continue };
        let n = q.ring.len() as f64;
        let c = q.ring.iter().fold((0.0, 0.0), |s, p| (s.0 + p.0 / n, s.1 + p.1 / n));
        let len = (b.0 - a.0).hypot(b.1 - a.1);
        let up = (-(b.1 - a.1) / len, (b.0 - a.0) / len);
        let (z0, z1) = (g.top_at(v2(c.0, c.1)), g.top_at(v2(c.0 + 0.05 * up.0, c.1 + 0.05 * up.1)));
        let (z0, z1) = (z0.expect("the piece covers its centroid"), z1.expect("and a step uphill of it"));
        assert!((z1 - z0 - 0.05 * tan).abs() < 1e-6, "piece {:?} rises {} over 5 cm, not {}", q.ring, z1 - z0, 0.05 * tan);
    }
}
