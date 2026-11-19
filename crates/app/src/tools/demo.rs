//! Acceptance screenshot scenes of the model tools (`--tool-demo A1..A5`): model + scripted input.

use super::{arm, Kind};
use crate::shell::EditorApp;
use crate::wall_tool::{Event, Key, Mode};
use ddc_geom::{v2, V2};

fn click(app: &mut EditorApp, p: V2) {
    app.ed.handle(Event::Move(p));
    app.ed.handle(Event::Click(p));
}

fn chars(app: &mut EditorApp, s: &str) {
    for c in s.chars() {
        app.ed.handle(Event::Char(c));
    }
}

fn rect_walls(app: &mut EditorApp, a: V2, b: V2) {
    let ed = &mut app.ed;
    ed.set_mode(Mode::Rect);
    ed.handle(Event::Move(a));
    ed.handle(Event::Press(a));
    ed.handle(Event::Move(b));
    ed.handle(Event::Release(b));
    ed.set_mode(Mode::Select);
}

fn grids(app: &mut EditorApp, xs: &[f64], ys: &[f64]) {
    let lang = app.sh.cat.lang;
    arm(&mut app.ed, Kind::Grid, lang);
    let (y0, y1) = (ys[0] - 1.5, ys[ys.len() - 1] + 1.5);
    let (x0, x1) = (xs[0] - 1.5, xs[xs.len() - 1] + 1.5);
    for &x in xs {
        click(app, v2(x, y0));
        click(app, v2(x, y1));
    }
    for &y in ys {
        click(app, v2(x0, y));
        click(app, v2(x1, y));
    }
}

fn frame(app: &mut EditorApp, center: V2, scale: f64) {
    app.plan.center = center;
    app.plan.scale = scale;
    app.ed.tool.radius = 12.0 / scale;
}

/// Prepare a snapshot scene; `id` is A1..A5.
pub fn demo(app: &mut EditorApp, id: &str) -> Result<(), String> {
    let lang = app.sh.cat.lang;
    match id {
        "A1" => {
            rect_walls(app, v2(0.0, 0.0), v2(9.0, 6.0));
            grids(app, &[0.0, 4.5, 9.0], &[0.0, 6.0]);
            frame(app, v2(4.5, 3.0), 62.0);
            arm(&mut app.ed, Kind::Beam, lang);
            click(app, v2(0.0, 3.0));
            app.ed.handle(Event::Move(v2(3.6, 3.0)));
            chars(app, "4500");
        }
        "A2" => {
            grids(app, &[0.0, 6.0, 12.0, 18.0], &[0.0, 6.0, 12.0]);
            arm(&mut app.ed, Kind::StructColumn, lang);
            crate::interact::sessions::model_tool::with(&mut app.ed, |ed, a| super::column::at_grids(ed, a));
            frame(app, v2(9.0, 6.0), 38.0);
            app.ed.handle(Event::Move(v2(21.0, 9.1)));
        }
        "A3" => {
            rect_walls(app, v2(0.0, 0.0), v2(10.0, 6.0));
            frame(app, v2(5.0, 3.0), 58.0);
            arm(&mut app.ed, Kind::Roof, lang);
            click(app, v2(0.0, 0.0));
            click(app, v2(10.0, 0.0));
            click(app, v2(10.0, 6.0));
            app.ed.handle(Event::Char(' ')); // right end wall — gable
            click(app, v2(0.0, 6.0));
            app.ed.handle(Event::Move(v2(0.0, 1.2)));
        }
        "A4" => {
            let name = crate::create_names::level_name(&app.sh.cat, 2);
            app.ed
                .model
                .add_level(&name, 3.24)
                .map_err(|e| e.to_string())?;
            rect_walls(app, v2(0.0, 0.0), v2(6.0, 5.0));
            frame(app, v2(3.0, 2.5), 90.0);
            arm(&mut app.ed, Kind::Stair, lang);
            click(app, v2(1.2, 0.6));
            app.ed.handle(Event::Move(v2(1.2, 3.75)));
        }
        "A5" => {
            rect_walls(app, v2(0.0, 0.0), v2(12.0, 7.0));
            frame(app, v2(6.0, 3.5), 62.0);
            arm(&mut app.ed, Kind::Duct, lang);
            for p in [v2(1.0, 5.8), v2(8.0, 5.8), v2(8.0, 2.0)] {
                click(app, p);
            }
            app.ed.handle(Event::Key(Key::Enter));
            arm(&mut app.ed, Kind::Pipe, lang);
            for p in [v2(1.0, 1.0), v2(5.0, 1.0), v2(5.0, 4.0)] {
                click(app, p);
            }
            app.ed.handle(Event::Move(v2(10.5, 4.0)));
        }
        // saved measurements: a clear gap between two wall faces, the tool still running
        "M1" => {
            rect_walls(app, v2(0.0, 0.0), v2(8.0, 5.0));
            frame(app, v2(4.0, 2.5), 70.0);
            app.plan.b.start_measure(crate::edit3d::measure_saved::Mode::Clear, &mut app.ed);
            if let Some(m) = &mut app.plan.b.ms {
                m.click(&mut app.ed, v2(2.0, 0.1), 0.5);
                m.click(&mut app.ed, v2(6.0, 4.9), 0.5);
                m.cursor = v2(5.0, 0.1);
            }
        }
        "E2" | "E3" | "E4" | "E5" | "E6" | "E8" => edit_demo(app, id).map_err(|e| e.to_string())?,
        // floor shape points — shape-edit scene
        "E7" => return crate::shape_edit::demo(app, "E3").unwrap_or(Ok(())),
        _ => {
            if let Some(r) =
                crate::sketch_mode::demo(app, id).or_else(|| crate::shape_edit::demo(app, id))
            {
                return r;
            }
            return Err(format!("unknown tool demo {id}"));
        }
    }
    app.ed.status = None;
    Ok(())
}

fn rect_sketch(x0: f64, y0: f64, x1: f64, y1: f64) -> ddc_geom::sketch::Sketch {
    let p = [v2(x0, y0), v2(x1, y0), v2(x1, y1), v2(x0, y1)];
    ddc_geom::sketch::Sketch {
        curves: (0..4)
            .map(|i| ddc_geom::sketch::SketchCurve::Line {
                a: p[i],
                b: p[(i + 1) % 4],
            })
            .collect(),
    }
}

/// Host-edit scenes (`--tool-demo E2..E8`, 3D view): cut-outs, roofs, attach, shaft.
fn edit_demo(app: &mut EditorApp, id: &str) -> Result<(), ddc_model::Error> {
    use ddc_geom::sketch::SketchCurve;
    use ddc_model::{OpeningHost, RoofKind, RoofSpec, ShaftSpec, SlabSpec};
    let lvl = app.ed.level;
    let slab_name = app.sh.cat.t("names.slab");
    let roof_name = app.sh.cat.t("names.roof_slab");
    let (slab_t, _) = super::ensure_slab_type(&mut app.ed.model, &slab_name)?;
    let (roof_t, _) = super::ensure_slab_type(&mut app.ed.model, &roof_name)?;
    let slab = |m: &mut ddc_model::Model, level: i64, x: f64, y: f64| {
        m.add_slab(&SlabSpec {
            level,
            slab_type: slab_t,
            offset: 0.0,
            outer: vec![(0.0, 0.0), (x, 0.0), (x, y), (0.0, y)],
            holes: vec![],
        })
    };
    let footprint = |x: f64, y: f64, slopes: Vec<Option<f64>>, off: f64| RoofSpec {
        level: lvl,
        slab_type: roof_t,
        offset: off,
        overhang: 0.4,
        fascia: 0.2,
        kind: RoofKind::Footprint {
            ring: vec![(0.0, 0.0), (x, 0.0), (x, y), (0.0, y)],
            slopes,
            mansard: None,
        },
    };
    match id {
        "E2" => {
            // floor with rectangular and elliptical openings from a sketch
            rect_walls(app, v2(0.0, 0.0), v2(10.0, 6.0));
            let m = &mut app.ed.model;
            let s = slab(m, lvl, 10.0, 6.0)?;
            m.add_host_opening(
                OpeningHost::Slab(s),
                &rect_sketch(1.5, 1.5, 4.0, 3.5),
                false,
            )?;
            let ell = ddc_geom::sketch::Sketch {
                curves: vec![SketchCurve::Ellipse {
                    c: v2(7.0, 3.0),
                    rx: 1.6,
                    ry: 0.9,
                    rot: 0.3,
                    t0: 0.0,
                    sweep: std::f64::consts::TAU,
                }],
            };
            m.add_host_opening(OpeningHost::Slab(s), &ell, false)?;
        }
        "E3" => {
            // slope from the slope arrow: rise 1.8 m from south to north
            rect_walls(app, v2(0.0, 0.0), v2(10.0, 6.0));
            let h = app.ed.walls.first().map_or(3.0, |w| w.height);
            let mut r = footprint(10.0, 6.0, vec![None; 4], h);
            r.kind = RoofKind::Arrow {
                ring: vec![(0.0, 0.0), (10.0, 0.0), (10.0, 6.0), (0.0, 6.0)],
                tail: (5.0, 0.0),
                head: (5.0, 6.0),
                rise: 1.8,
            };
            let roof = app.ed.model.add_roof(&r)?;
            for w in app.ed.walls.iter().map(|w| w.id).collect::<Vec<_>>() {
                app.ed.model.attach_wall_top(w, roof)?;
            }
        }
        "E4" => {
            // roof profile in the left end-wall plane, extruded 10 m along the house
            rect_walls(app, v2(0.0, 0.0), v2(10.0, 6.0));
            let h = app.ed.walls.first().map_or(3.0, |w| w.height);
            let mut r = footprint(10.0, 6.0, vec![], h);
            r.overhang = 0.0;
            r.kind = RoofKind::Extrusion {
                a: (-0.3, 6.5),
                b: (-0.3, -0.5),
                profile: vec![(0.0, 0.0), (1.2, 1.4), (3.5, 2.2), (5.8, 1.4), (7.0, 0.0)],
                depth: 10.6,
            };
            app.ed.model.add_roof(&r)?;
        }
        "E5" => {
            // gable roof; the dormer cuts only the slope
            rect_walls(app, v2(0.0, 0.0), v2(12.0, 8.0));
            let h = app.ed.walls.first().map_or(3.0, |w| w.height);
            let roof = app.ed.model.add_roof(&footprint(
                12.0,
                8.0,
                vec![Some(35.0), None, Some(35.0), None],
                h,
            ))?;
            let m = &mut app.ed.model;
            m.add_host_opening(
                OpeningHost::Roof(roof),
                &rect_sketch(3.0, 0.8, 5.0, 2.8),
                true,
            )?;
            m.add_host_opening(
                OpeningHost::Roof(roof),
                &rect_sketch(7.5, 0.8, 9.5, 2.8),
                true,
            )?;
        }
        "E6" => {
            // walls up to the slope: gables rise to the ridge
            rect_walls(app, v2(0.0, 0.0), v2(10.0, 6.0));
            let h = app.ed.walls.first().map_or(3.0, |w| w.height);
            let mut r = footprint(10.0, 6.0, vec![Some(30.0), None, Some(30.0), None], h);
            r.overhang = 0.0;
            let roof = app.ed.model.add_roof(&r)?;
            for w in app.ed.walls.iter().map(|w| w.id).collect::<Vec<_>>() {
                app.ed.model.attach_wall_top(w, roof)?;
            }
        }
        _ => {
            // shaft through three floors
            let (n2, n3) = (
                crate::create_names::level_name(&app.sh.cat, 2),
                crate::create_names::level_name(&app.sh.cat, 3),
            );
            let m = &mut app.ed.model;
            let l2 = m.add_level(&n2, 3.3)?;
            let l3 = m.add_level(&n3, 6.6)?;
            for l in [lvl, l2, l3] {
                slab(m, l, 10.0, 6.0)?;
            }
            m.add_shaft(&ShaftSpec {
                level: lvl,
                top_level: l3,
                base_offset: -0.5,
                top_offset: 1.0,
                ring: vec![(4.0, 2.0), (6.5, 2.0), (6.5, 4.0), (4.0, 4.0)],
            })?;
        }
    }
    app.ed.reload()?;
    app.view3d = true;
    Ok(())
}
