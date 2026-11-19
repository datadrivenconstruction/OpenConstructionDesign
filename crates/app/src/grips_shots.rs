//! Acceptance snapshots of grips G1-G6 (light+dark) in docs/design/shots/edit/.
//! `cargo test -p ddc-app shots_g -- --ignored`

use super::*;
use crate::editor::{Editor, Sel};
use crate::shell::EditorApp;
use ddc_model::{
    BeamJoin, ConstraintKind, ConstraintSpec, Just, PipeMaterial, RailingHost, RailingSpec,
    SlabSpec, SystemKind, WallSpec,
};

fn wall(ed: &mut Editor, a: (f64, f64), b: (f64, f64)) -> i64 {
    let t = ed.types[0].id;
    ed.model
        .add_wall(&WallSpec {
            a,
            b,
            level: ed.level,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        })
        .unwrap()
}

fn render(rs: &crate::RenderState, app: &mut EditorApp, size: (u32, u32)) -> Vec<u8> {
    let ctx = egui::Context::default();
    crate::shot::frame(rs, &ctx, size, 1.0, 8, None, |c, _| app.ui(c, Some(rs)))
}

/// Snapshot scene; returns the active grip (a drag with preview) if any.
fn scene(ed: &mut Editor, id: &str) -> Option<(Grip, V2)> {
    match id {
        "G1" => {
            // EQ chain plus a length lock: stretching by grip conflicts, with explanatory text
            let ws: Vec<i64> = [0.0, 3.0, 6.0]
                .iter()
                .map(|&x| wall(ed, (x, 0.0), (x, 5.0)))
                .collect();
            let arc = wall(ed, (9.0, 0.0), (9.0, 5.0));
            ed.model.set_wall_bulge(arc, 0.3).unwrap();
            ed.model
                .add_constraint(&ConstraintSpec {
                    kind: ConstraintKind::Equal,
                    walls: ws.clone(),
                    value: None,
                })
                .unwrap();
            ed.model
                .add_constraint(&ConstraintSpec {
                    kind: ConstraintKind::LockLength,
                    walls: vec![ws[0]],
                    value: None,
                })
                .unwrap();
            ed.reload().unwrap();
            ed.selection = vec![Sel::Wall(ws[0]), Sel::Wall(arc)];
            let g = grips(&ed.model, Kind::Wall, ws[0])
                .unwrap()
                .into_iter()
                .find(|g| g.role == Role::End(1))
                .unwrap();
            let to = g.pos + v2(0.0, 1.5);
            // the edit is rolled back entirely: the snapshot model is unchanged
            let e = apply(&mut ed.model, &g, to).unwrap_err();
            ed.status = Some(crate::constraints_ui::report(&e));
            Some((g, to))
        }
        "G2" => {
            let st = ed
                .model
                .add_slab_type(
                    "S",
                    &[ddc_model::LayerSpec {
                        material: "c".into(),
                        thickness: 0.2,
                    }],
                )
                .unwrap();
            let s = ed
                .model
                .add_slab(&SlabSpec {
                    level: ed.level,
                    slab_type: st,
                    offset: 0.0,
                    outer: vec![(0.0, 0.0), (8.0, 0.0), (8.0, 5.0), (0.0, 5.0)],
                    holes: vec![vec![(2.0, 1.5), (4.0, 1.5), (4.0, 3.5), (2.0, 3.5)]],
                })
                .unwrap();
            ed.reload().unwrap();
            ed.selection = vec![Sel::Slab(s)];
            let g = grips(&ed.model, Kind::Slab, s)
                .unwrap()
                .into_iter()
                .find(|g| g.role == Role::HoleVertex(0, 2))
                .unwrap();
            Some((g, v2(5.0, 4.0)))
        }
        "G3" => {
            let mt = ed.model.add_catalog_member_type("R 300x500").unwrap();
            let b = ed
                .model
                .add_beam(&BeamSpec {
                    level: ed.level,
                    member_type: mt,
                    a: (0.0, 0.0, 2.8),
                    b: (6.0, 0.0, 2.8),
                    bulge: 0.0,
                    yjust: Just::Center,
                    zjust: Just::Center,
                    rotation: 0.0,
                    start_ext: 0.0,
                    end_ext: 0.0,
                    join_start: BeamJoin::default(),
                    join_end: BeamJoin::default(),
                })
                .unwrap();
            ed.reload().unwrap();
            ed.grip.focus = vec![(Kind::Beam, b)];
            let g = grips(&ed.model, Kind::Beam, b)
                .unwrap()
                .into_iter()
                .find(|g| g.role == Role::Rotate)
                .unwrap();
            Some((g, v2(1.6, 2.2)))
        }
        "G4" => {
            let a = ed
                .model
                .add_grid(&GridSpec::line("1", (0.0, -1.0), (0.0, 7.0)))
                .unwrap();
            ed.model
                .add_grid(&GridSpec::line("2", (6.0, -1.0), (6.0, 7.0)))
                .unwrap();
            let mut s = grid(&ed.model, a).unwrap();
            s.bubble_start = false;
            ed.model.update_grid(a, &s).unwrap();
            let w = wall(ed, (0.0, 1.0), (0.0, 5.0));
            ed.model
                .add_constraint(&ConstraintSpec {
                    kind: ConstraintKind::AlignGrid,
                    walls: vec![w, a],
                    value: None,
                })
                .unwrap();
            let v = format!("plan:{}", ed.level);
            ed.model
                .set_datum_extent(
                    &v,
                    DatumKind::Grid,
                    a,
                    Some(DatumExtent {
                        a: (0.0, 0.0),
                        b: (0.0, 6.0),
                        bubbles: (false, true),
                    }),
                )
                .unwrap();
            ed.reload().unwrap();
            ed.grip.focus = vec![(Kind::Grid, a)];
            let g = grips_in(&ed.model, Kind::Grid, a, &v)
                .unwrap()
                .into_iter()
                .find(|g| g.role == Role::End(1))
                .unwrap();
            Some((g, v2(0.0, 7.5)))
        }
        "G5" => {
            let top = ed.model.add_level("L2", 3.0).unwrap();
            let mut sp = StairSpec::straight(ed.level, top, (0.0, 0.0), 0.0);
            sp.shape = StairShape::U {
                left: true,
                first: Some(9),
                gap: 0.2,
            };
            let s = ed.model.add_stair(&sp).unwrap();
            let rs = ed
                .model
                .add_stair_railings(s, &RailingSpec::new(ed.level, RailingHost::Path(vec![])))
                .unwrap();
            ed.reload().unwrap();
            ed.grip.focus = vec![(Kind::Stair, s), (Kind::Railing, rs[1])];
            let g = grips(&ed.model, Kind::Stair, s)
                .unwrap()
                .into_iter()
                .find(|g| g.role == Role::Width)
                .unwrap();
            let at = g.pos + v2(0.0, -0.3);
            Some((g, at))
        }
        _ => {
            let sys = ed
                .model
                .add_mep_system("CW", SystemKind::ColdWater)
                .unwrap();
            let r = ed
                .model
                .add_mep_run(
                    &RunSpec::pipe(
                        sys,
                        ed.level,
                        PipeMaterial::Copper,
                        22,
                        vec![
                            (0.0, 0.0, 2.6),
                            (5.0, 0.0, 2.6),
                            (5.0, 4.0, 2.6),
                            (9.0, 4.0, 2.6),
                        ],
                    )
                    .unwrap(),
                )
                .unwrap();
            ed.reload().unwrap();
            ed.grip.focus = vec![(Kind::Mep, r)];
            let g = grips(&ed.model, Kind::Mep, r)
                .unwrap()
                .into_iter()
                .find(|g| g.role == Role::Insert(1))
                .unwrap();
            Some((g, v2(6.2, 2.0)))
        }
    }
}

#[test]
#[ignore]
fn shots_g() {
    let (pick, desc) = crate::gfx::choose(&crate::gfx::candidates(None, false), &mut |_| {})
        .expect("graphics adapter");
    let rs = crate::shot::render_state(pick).unwrap();
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/design/shots/edit");
    std::fs::create_dir_all(&dir).unwrap();
    let size = (1440u32, 900u32);
    for theme in ["light", "dark"] {
        for id in ["G1", "G2", "G3", "G4", "G5", "G6"] {
            let mut ed = Editor::new_in_memory().unwrap();
            ed.set_mode(crate::wall_tool::Mode::Select);
            let act = scene(&mut ed, id);
            if id != "G1" {
                ed.status = None;
            }
            let mut app = EditorApp::new(ed, Some(&rs), desc.clone());
            app.prefs.theme = theme.into();
            app.plan.fit(&app.ed);
            render(&rs, &mut app, size);
            if let Some((g, to)) = act {
                let (preview, mut error) = match compute(&app.ed.model, &g, to) {
                    Ok(c) => (outline(&c), None),
                    Err(e) => (vec![], Some(e.to_string())),
                };
                if id == "G1" {
                    error = app.ed.status.clone();
                }
                let d = dims(&app.ed.model, &g, to).unwrap_or_default();
                app.ed.grip.active = Some(Active {
                    grip: g,
                    now: to,
                    preview,
                    dims: d,
                    error,
                });
            }
            let px = render(&rs, &mut app, size);
            crate::shot::save_png(&dir.join(format!("{id}-{theme}.png")), size, &px).unwrap();
        }
    }
}
