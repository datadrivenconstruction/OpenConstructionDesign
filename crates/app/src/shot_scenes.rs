//! Acceptance screenshot scenes (`--scene all3d|topo|struct|roof_edges|solids|ifc`): one scene per line.

use ddc_geom::footing::Step;
use ddc_geom::gsolid::{Form, Frame, Section};
use ddc_geom::sweep::SweepKind;
use ddc_geom::{v2, v3};
use ddc_model::{
    BeamJoin, BeamSpec, BraceSpec, ColumnSpec, FootingSpec, Just, LayerSpec, MemberShape, Model,
    PadSpec, RaftSpec, RoofKind, RoofSpec, SkylightSpec, SolidSpec, SweepHost, SweepSpec, TopoSpec,
    WallSpec,
};

use crate::editor::Editor;

/// Seeded model text in the run language, from the same catalog as the app demo seed.
fn t(key: &str) -> String {
    crate::tr(key, &[])
}

const W: f64 = 12.0;
const D: f64 = 8.0;

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<(f64, f64)> {
    vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
}

fn gable(m: &mut Model, level: i64, offset: f64, x0: f64) -> Result<i64, ddc_model::Error> {
    let st = m.add_slab_type(
        "Roof",
        &[LayerSpec {
            material: "roofing".into(),
            thickness: 0.2,
        }],
    )?;
    m.add_roof(&RoofSpec {
        level,
        slab_type: st,
        offset,
        overhang: 0.5,
        fascia: 0.2,
        kind: RoofKind::Footprint {
            ring: rect(x0, 0.0, W, D),
            slopes: vec![Some(25.0), None, Some(25.0), None],
            mansard: None,
        },
    })
}

fn boxed(ed: &mut Editor, h: f64) -> Result<Vec<i64>, ddc_model::Error> {
    let (level, wall_type) = (ed.level, ed.types[0].id);
    [
        ((0.0, 0.0), (W, 0.0)),
        ((W, 0.0), (W, D)),
        ((W, D), (0.0, D)),
        ((0.0, D), (0.0, 0.0)),
    ]
    .iter()
    .map(|&(a, b)| {
        ed.model.add_wall(&WallSpec {
            a,
            b,
            level,
            base_offset: 0.0,
            height: h,
            wall_type,
            offset0: None,
        })
    })
    .collect()
}

fn column(m: &mut Model, level: i64, t: i64, at: (f64, f64)) -> Result<i64, ddc_model::Error> {
    m.add_column(&ColumnSpec {
        level,
        top_level: None,
        member_type: t,
        structural: true,
        at,
        base_offset: 0.0,
        top_offset: 3.3,
        rotation: 0.0,
        top_at: None,
    })
}

/// Walls, roof, columns and MEP together; two walls removed and the roof covers half the plan
/// so the MEP is visible.
fn all3d(ed: &mut Editor) -> Result<(), String> {
    let e = |e: ddc_model::Error| e.to_string();
    crate::mep3d::demo(ed)?;
    let level = ed.level;
    let open: Vec<i64> = ed.walls.iter().take(2).map(|w| w.id).collect();
    ed.model.delete_walls(&open).map_err(e)?;
    let m = &mut ed.model;
    let t = m
        .add_member_type(
            "C 300x300",
            &MemberShape::Rect { b: 0.3, h: 0.3 },
            "Concrete",
        )
        .map_err(e)?;
    for at in [(0.0, 0.0), (6.0, 0.0), (W, 0.0), (W, 4.0), (W, D)] {
        column(m, level, t, at).map_err(e)?;
    }
    gable(m, level, 3.6, W / 2.0).map_err(e)?;
    ed.reload().map_err(e)?;
    ed.selection.clear();
    Ok(())
}

fn topo(ed: &mut Editor) -> Result<(), String> {
    let e = |e: ddc_model::Error| e.to_string();
    let hill = |x: f64, y: f64| {
        let d = ((x - 20.0).powi(2) + (y - 22.0).powi(2)) / 180.0;
        3.0 * (-d).exp() + 0.05 * x
    };
    let mut points = vec![];
    for j in 0..13 {
        for i in 0..13 {
            let (x, y) = (i as f64 * 3.0, j as f64 * 3.0);
            points.push((x, y, hill(x, y)));
        }
    }
    let m = &mut ed.model;
    let id = m
        .add_toposolid(&TopoSpec {
            name: t("shot_scenes.site"),
            thickness: 2.0,
            material: "earth".into(),
            points,
            ..Default::default()
        })
        .map_err(e)?;
    m.add_topo_region(id, &t("shot_scenes.lawn"), "grass", &rect(24.0, 3.0, 34.0, 12.0))
        .map_err(e)?;
    m.add_pad(&PadSpec {
        topo: id,
        name: t("shot_scenes.pad"),
        elevation: 1.0,
        slope: 0.0,
        thickness: 0.3,
        material: "gravel".into(),
        outline: rect(6.0, 6.0, 15.0, 13.0),
    })
    .map_err(e)?;
    ed.reload().map_err(e)
}

/// Frame: columns on pad footings, beams, bracing, pile-cap slab.
fn structure(ed: &mut Editor) -> Result<(), String> {
    let e = |e: ddc_model::Error| e.to_string();
    let level = ed.level;
    let m = &mut ed.model;
    let ct = m
        .add_member_type(
            "C 400x400",
            &MemberShape::Rect { b: 0.4, h: 0.4 },
            "Concrete",
        )
        .map_err(e)?;
    let bt = m
        .add_member_type(
            "B 250x450",
            &MemberShape::Rect { b: 0.25, h: 0.45 },
            "Steel",
        )
        .map_err(e)?;
    let br = m
        .add_member_type(
            "X 120x120",
            &MemberShape::Rect { b: 0.12, h: 0.12 },
            "Steel",
        )
        .map_err(e)?;
    let spots = [
        (0.0, 0.0),
        (6.0, 0.0),
        (12.0, 0.0),
        (0.0, 6.0),
        (6.0, 6.0),
        (12.0, 6.0),
    ];
    for at in spots {
        let c = column(m, level, ct, at).map_err(e)?;
        m.add_footing(&FootingSpec {
            level,
            column: Some(c),
            at,
            rotation: 0.0,
            round: false,
            steps: vec![
                Step {
                    w: 2.0,
                    l: 2.0,
                    h: 0.4,
                },
                Step {
                    w: 1.2,
                    l: 1.2,
                    h: 0.4,
                },
            ],
            top_offset: 0.0,
            material: "Concrete".into(),
        })
        .map_err(e)?;
    }
    let beam = |a: (f64, f64), b: (f64, f64)| BeamSpec {
        level,
        member_type: bt,
        a: (a.0, a.1, 3.6),
        b: (b.0, b.1, 3.6),
        bulge: 0.0,
        yjust: Just::default(),
        zjust: Just::default(),
        rotation: 0.0,
        start_ext: 0.0,
        end_ext: 0.0,
        join_start: BeamJoin::default(),
        join_end: BeamJoin::default(),
    };
    for (a, b) in [
        ((0.0, 0.0), (6.0, 0.0)),
        ((6.0, 0.0), (12.0, 0.0)),
        ((0.0, 6.0), (6.0, 6.0)),
        ((6.0, 6.0), (12.0, 6.0)),
        ((0.0, 0.0), (0.0, 6.0)),
        ((12.0, 0.0), (12.0, 6.0)),
    ] {
        m.add_beam(&beam(a, b)).map_err(e)?;
    }
    for (a, b) in [
        ((0.0, 0.0, 0.3), (6.0, 0.0, 3.3)),
        ((6.0, 0.0, 0.3), (0.0, 0.0, 3.3)),
        ((12.0, 0.0, 0.3), (12.0, 6.0, 3.3)),
    ] {
        m.add_brace(&BraceSpec {
            level,
            member_type: br,
            a,
            b,
            start: None,
            end: None,
            rotation: 0.0,
        })
        .map_err(e)?;
    }
    m.add_raft(&RaftSpec {
        level,
        ring: rect(16.0, -1.0, 24.0, 7.0),
        thickness: 0.4,
        edge_width: 0.6,
        edge_depth: 0.4,
        top_offset: 0.0,
        material: "Concrete".into(),
    })
    .map_err(e)?;
    ed.reload().map_err(e)
}

fn roof_edges(ed: &mut Editor) -> Result<(), String> {
    let e = |e: ddc_model::Error| e.to_string();
    boxed(ed, 3.0).map_err(e)?;
    let level = ed.level;
    let m = &mut ed.model;
    let roof = gable(m, level, 3.0, 0.0).map_err(e)?;
    for (kind, profile, rakes) in [
        (SweepKind::Fascia, "fascia_200", true),
        (SweepKind::Gutter, "gutter_half_round_150", false),
    ] {
        m.add_sweep(&SweepSpec {
            kind,
            profile: profile.into(),
            host: SweepHost::Roof { roof, rakes },
            side: 1,
            z: 0.0,
            dx: 0.0,
            dy: 0.0,
            material: None,
        })
        .map_err(e)?;
    }
    m.add_skylight(&SkylightSpec {
        roof,
        center: (4.0, 2.0),
        width: 1.2,
        length: 1.6,
        curb: 0.15,
    })
    .map_err(e)?;
    ed.reload().map_err(e)
}

/// Solids and voids: a cube with a prism void cut by its host, a wall with a void.
fn solids(ed: &mut Editor) -> Result<(), String> {
    let e = |e: ddc_model::Error| e.to_string();
    let level = ed.level;
    let wall_type = ed.types[0].id;
    let m = &mut ed.model;
    let square = |x0: f64, y0: f64, s: f64| {
        Section::polygon(vec![
            v2(x0, y0),
            v2(x0 + s, y0),
            v2(x0 + s, y0 + s),
            v2(x0, y0 + s),
        ])
    };
    let solid = |name: &str, void: bool, profile: Section, start: f64, end: f64| SolidSpec {
        level,
        name: name.into(),
        void,
        material: "Concrete".into(),
        ifc: None,
        frame: Frame::default(),
        form: Form::Extrusion {
            profile,
            start,
            end,
        },
    };
    let body = m
        .add_solid(&solid(&t("shot_scenes.block"), false, square(0.0, 0.0, 4.0), 0.0, 3.0))
        .map_err(e)?;
    let hole = m
        .add_solid(&solid(&t("shot_scenes.void"), true, square(1.2, 1.2, 1.6), 1.0, 4.0))
        .map_err(e)?;
    m.set_solid_hosts(hole, &[("solid".to_string(), body)])
        .map_err(e)?;
    m.add_solid(&SolidSpec {
        frame: Frame {
            o: v3(8.0, 0.0, 0.0),
            ..Frame::default()
        },
        ..solid(&t("shot_scenes.tower"), false, square(0.0, 0.0, 2.0), 0.0, 5.0)
    })
    .map_err(e)?;
    let w = m
        .add_wall(&WallSpec {
            a: (0.0, 6.0),
            b: (10.0, 6.0),
            level,
            base_offset: 0.0,
            height: 3.0,
            wall_type,
            offset0: None,
        })
        .map_err(e)?;
    let cut = m
        .add_solid(&solid(&t("shot_scenes.wall_void"), true, square(4.0, 5.2, 1.6), 0.8, 2.4))
        .map_err(e)?;
    m.set_solid_hosts(cut, &[("wall".to_string(), w)])
        .map_err(e)?;
    ed.reload().map_err(e)
}

/// Sample project via exchange: export and import into an empty model.
fn ifc(ed: &mut Editor) -> Result<(), String> {
    let e = |e: ddc_model::Error| e.to_string();
    let src = crate::help::sample::build(false).map_err(e)?;
    let text = ddc_io::ifc::export_ifc(&src).map_err(|e| e.to_string())?;
    ddc_io::ifc::import_ifc(&mut ed.model, text.as_bytes()).map_err(|e| e.to_string())?;
    if let Some(l) = crate::help::sample::first_level(&ed.model) {
        ed.level = l;
    }
    ed.reload().map_err(e)
}

/// false — the name is not in this list.
/// Ids made by the `elems` scene, for `--select-elem`.
pub static ELEMS: std::sync::Mutex<Vec<(&'static str, i64)>> = std::sync::Mutex::new(Vec::new());

/// Roof, stair, railing, shaft, grid and a bent duct run (one fitting) for the selection shots.
fn elems(ed: &mut Editor) -> Result<(), String> {
    use ddc_model::*;
    let e = |e: ddc_model::Error| e.to_string();
    let (lv, wt) = (ed.level, ed.types[0].id);
    let top = ed.model.add_level(&t("sketch_edit.seed_level2"), 3.0).map_err(e)?;
    let m = &mut ed.model;
    for (a, b) in [((0.0, 0.0), (W, 0.0)), ((W, 0.0), (W, D)), ((W, D), (0.0, D)), ((0.0, D), (0.0, 0.0))] {
        m.add_wall(&WallSpec { a, b, level: lv, base_offset: 0.0, height: 3.0, wall_type: wt, offset0: None })
            .map_err(e)?;
    }
    let mut ids = vec![("roof", gable(m, lv, 3.0, 0.0).map_err(e)?)];
    let mut st = StairSpec::straight(lv, top, (1.0, 1.0), 0.0);
    st.stringer = Some((0.05, 0.3));
    ids.push(("stair", m.add_stair(&st).map_err(e)?));
    let r = m.add_railing(&RailingSpec::new(lv, RailingHost::Path(vec![(5.0, 1.0, 0.0), (9.0, 1.0, 0.0), (9.0, 4.0, 0.0)])))
        .map_err(e)?;
    ids.push(("railing", r));
    let sh = m.add_shaft(&ShaftSpec {
        level: lv,
        top_level: top,
        base_offset: 0.0,
        top_offset: 0.0,
        ring: rect(9.0, 5.0, 11.0, 7.0),
    })
    .map_err(e)?;
    ids.push(("shaft", sh));
    ids.push(("grid", m.add_grid(&GridSpec::line("A", (-1.0, -2.0), (-1.0, D + 2.0))).map_err(e)?));
    let sys = m.add_mep_system("s", SystemKind::SupplyAir).map_err(e)?;
    let mut duct = MepSection::new(MepDomain::Duct, MepShape::Rect { w: 0.5, h: 0.25 });
    duct.wall = 0.001;
    m.add_mep_run(&RunSpec::new(sys, lv, duct, vec![(2.0, 6.0, 2.6), (6.0, 6.0, 2.6), (6.0, 7.5, 2.6)]))
        .map_err(e)?;
    *ELEMS.lock().unwrap() = ids;
    ed.reload().map_err(e)
}

/// Outline of the spiked floor: a 45-degree run out to a tip and nearly straight back.
fn spiked_ring() -> Vec<(f64, f64)> {
    let s = std::f64::consts::FRAC_1_SQRT_2;
    let c = 4.0 - s * 1.5;
    [(1., 1.), (4., 4.), (c, c + 1e-15), (0., 3.)].map(|(x, y)| (x + 3.0, y + 2.0)).to_vec()
}

/// The `elems` scene plus a floor saved with a spike, written past the check as an old file
/// holds it, so the Clean up outline preview can be shot (`--select-elem floor`). The `elems`
/// scene itself stays as it was.
fn elems_floor(ed: &mut Editor) -> Result<(), String> {
    elems(ed)?;
    let s = spiked_floor(ed)?;
    ELEMS.lock().unwrap().push(("floor", s));
    Ok(())
}

fn spiked_floor(ed: &mut Editor) -> Result<i64, String> {
    let e = |e: ddc_model::Error| e.to_string();
    let q = |e: rusqlite::Error| e.to_string();
    let lv = ed.level;
    let m = &mut ed.model;
    let layer = LayerSpec { material: "concrete".into(), thickness: 0.2 };
    let st = m.add_slab_type("Floor", &[layer]).map_err(e)?;
    let spec = ddc_model::SlabSpec {
        level: lv,
        slab_type: st,
        offset: 0.0,
        outer: rect(3.0, 2.0, 7.0, 6.0),
        holes: vec![],
    };
    let s = m.add_slab(&spec).map_err(e)?;
    let db = m.sql();
    db.execute("DELETE FROM slab_vertex WHERE slab=?1", [s]).map_err(q)?;
    for (i, p) in spiked_ring().iter().enumerate() {
        let row = rusqlite::params![s, i as i64, p.0, p.1];
        db.execute("INSERT INTO slab_vertex VALUES (?1,0,?2,?3,?4)", row).map_err(q)?;
    }
    ed.reload().map_err(e)?;
    Ok(s)
}

const DOOR_DEF: &str = "family Door\ncategory generic\nhost wall\nparam Width length type 900mm\nparam Depth length type 200mm\nparam Height length type 2100mm\nparam Leaf material type \"Oak\"\nref Left x = -Width / 2\nref Right x = Width / 2\nref Front y = -Depth / 2\nref Back y = Depth / 2\ntype \"EI30\" Width=1000mm\nextrude Panel rect(Left, Front, Right, Back) z(0, Height) mat(Leaf) show(3d, elev)\nsymbol rect(Left, Front, Right, Back)\n";
const TREE_DEF: &str = "family Tree\ncategory generic\nhost floor\nparam Trunk length type 300mm\nparam Crown length type 3000mm\nparam Height length type 5000mm\nparam Bark material type \"Wood\"\nref Left x = -Trunk / 2\nref Right x = Trunk / 2\nref Front y = -Trunk / 2\nref Back y = Trunk / 2\ntype \"Oak\" Height=6000mm\nextrude Stem rect(Left, Front, Right, Back) z(0, Height - Crown) mat(Bark) show(medium, fine, 3d, elev)\nextrude Top rect(-Crown / 2, -Crown / 2, Crown / 2, Crown / 2) z(Height - Crown, Height) mat(Bark) show(3d, elev)\nsymbol rect(-Crown / 2, -Crown / 2, Crown / 2, Crown / 2)\n";

/// Component library shot: a walled room with a door in the south wall, a table and chairs and a
/// tree placed through the same call as a drop from the library window.
fn complib(ed: &mut Editor) -> Result<(), String> {
    use crate::tools::component::{place, HostHint, Pick};
    let e = |e: ddc_model::Error| e.to_string();
    let (lv, wt) = (ed.level, ed.types[0].id);
    for (a, b) in [((0.0, 0.0), (6.0, 0.0)), ((6.0, 0.0), (6.0, 4.0)), ((6.0, 4.0), (0.0, 4.0)), ((0.0, 4.0), (0.0, 0.0))] {
        ed.model
            .add_wall(&WallSpec { a, b, level: lv, base_offset: 0.0, height: 3.0, wall_type: wt, offset0: None })
            .map_err(e)?;
    }
    ed.reload().map_err(e)?;
    let lib = |n: &str| ddc_model::library_sources().into_iter().find(|s| s.contains(&format!("family {n}\n"))).map(|s| std::sync::Arc::<str>::from(s));
    let items: [(&str, &str, Option<std::sync::Arc<str>>, (f64, f64)); 6] = [
        ("Door", "EI30", Some(DOOR_DEF.into()), (2.0, 0.0)),
        ("Table", "", lib("Table"), (3.0, 2.0)),
        ("Chair", "", lib("Chair"), (2.2, 2.0)),
        ("Chair", "", lib("Chair"), (3.8, 2.0)),
        ("Sofa", "", lib("Sofa"), (1.5, 3.3)),
        ("Tree", "Oak", Some(TREE_DEF.into()), (7.5, 2.0)),
    ];
    for (fam, ty, def, q) in items {
        let ty = if ty.is_empty() {
            crate::complib::family_of(&ed.model, fam)
                .or_else(|| def.as_deref().and_then(|d| ddc_geom::component::Family::parse(d).ok()))
                .and_then(|f| f.types.first().map(|t| t.name.clone()))
                .unwrap_or_default()
        } else {
            ty.to_string()
        };
        let pick = Pick { family: fam.into(), ty, def };
        place(ed, &pick, v2(q.0, q.1), HostHint::Auto)?;
    }
    Ok(())
}

/// Site shot: every type of the library tree family in a row, a walled block for scale and a
/// grove cycling the same types, so species and neighbours can be compared.
fn trees(ed: &mut Editor) -> Result<(), String> {
    use crate::tools::component::{place, HostHint, Pick};
    let e = |e: ddc_model::Error| e.to_string();
    let (lv, wt) = (ed.level, ed.types[0].id);
    for (a, b) in [((0.0, 0.0), (10.0, 0.0)), ((10.0, 0.0), (10.0, 8.0)), ((10.0, 8.0), (0.0, 8.0)), ((0.0, 8.0), (0.0, 0.0))] {
        ed.model
            .add_wall(&WallSpec { a, b, level: lv, base_offset: 0.0, height: 6.0, wall_type: wt, offset0: None })
            .map_err(e)?;
    }
    ed.reload().map_err(e)?;
    let src = ddc_model::library_sources()
        .into_iter()
        .find(|s| s.contains("family Tree\n"))
        .map(std::sync::Arc::<str>::from)
        .ok_or("no Tree family in the library")?;
    let fam = ddc_geom::component::Family::parse(&src).map_err(|x| x.to_string())?;
    let names: Vec<String> = fam.types.iter().map(|t| t.name.clone()).collect();
    let put = |ed: &mut Editor, k: usize, x: f64, y: f64| {
        let pick = Pick { family: "Tree".into(), ty: names[k % names.len()].clone(), def: Some(src.clone()) };
        place(ed, &pick, v2(x, y), HostHint::Auto)
    };
    for k in 0..names.len().min(16) {
        put(ed, k, -4.0 + 7.0 * (k % 8) as f64, -9.0 - 8.0 * (k / 8) as f64)?;
    }
    // four species in the four seasons, one row each
    for (r, sp) in ["Oak A", "Birch A", "Cherry A", "Maple A"].iter().enumerate() {
        for (c, suf) in ["", " Autumn", " Bare", " Spring"].iter().enumerate() {
            if let Some(k) = names.iter().position(|n| *n == format!("{sp}{suf}")) {
                put(ed, k, -4.0 + 7.0 * c as f64, -26.0 - 8.0 * r as f64)?;
            }
        }
    }
    for i in 0..8 {
        for j in 0..8 {
            let (jx, jy) = (((i * 7 + j * 3) % 5) as f64 * 0.3, ((i * 5 + j * 11) % 7) as f64 * 0.25);
            put(ed, i * 3 + j * 5 + i * j, 16.0 + 6.0 * i as f64 + jx, -2.0 + 6.0 * j as f64 + jy)?;
        }
    }
    Ok(())
}

pub fn build(ed: &mut Editor, name: &str) -> Result<bool, String> {
    match name {
        "complib" => complib(ed)?,
        "trees" => trees(ed)?,
        "all3d" => all3d(ed)?,
        "topo" => topo(ed)?,
        "struct" => structure(ed)?,
        "roof_edges" => roof_edges(ed)?,
        "solids" => solids(ed)?,
        "ifc" => ifc(ed)?,
        "elems" => elems(ed)?,
        "elems_floor" => elems_floor(ed)?,
        _ => return Ok(false),
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cyr(s: &str) -> bool {
        s.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c))
    }

    fn level_names(ru: bool) -> Vec<String> {
        crate::with_test_lang(ru, || {
            let mut ed = Editor::new_in_memory().unwrap();
            elems(&mut ed).unwrap();
            ed.model.level_list().unwrap().into_iter().map(|l| l.1).collect()
        })
    }

    #[test]
    fn scene_levels_follow_demo_seed_language() {
        for ru in [false, true] {
            let want = vec![
                crate::tr_lang(ru, "project_seed.seed_level", &[]),
                crate::tr_lang(ru, "sketch_edit.seed_level2", &[]),
            ];
            assert_eq!(level_names(ru), want, "ru={ru}");
        }
    }

    #[test]
    fn ru_scene_has_no_english_seed_names() {
        let en = level_names(false);
        for n in level_names(true) {
            assert!(cyr(&n) && !en.contains(&n), "{n}");
        }
        for k in ["site", "lawn", "pad", "block", "void", "tower", "wall_void"] {
            let key = format!("shot_scenes.{k}");
            for ru in [false, true] {
                let v = crate::tr_lang(ru, &key, &[]);
                assert_ne!(v, key, "{key} missing (ru={ru})");
                assert_eq!(cyr(&v), ru, "{key}: {v}");
            }
        }
    }

    /// `elems_floor` adds one spiked floor the Clean up outline preview finds; the `elems` scene
    /// does not call it, so its shots do not change.
    #[test]
    fn elems_floor_scene_offers_a_cleanup() {
        let mut ed = Editor::new_in_memory().unwrap();
        elems(&mut ed).unwrap();
        let before = ed.slabs.len();
        let id = spiked_floor(&mut ed).unwrap();
        assert_eq!(ed.slabs.len(), before + 1, "one floor more, nothing else");
        let sel = crate::editor::Sel::Slab(id);
        let host = crate::outline_cleanup::Host::of(sel).expect("a floor host");
        let p = crate::outline_cleanup::prepare(&ed, host).unwrap().expect("a spike to clean");
        assert_eq!(p.spikes, 1);
        assert_eq!(p.before[0], spiked_ring(), "the preview shows the saved outline");
    }

    #[test]
    fn struct_scene_has_every_kind_in_3d() {
        let mut ed = Editor::new_in_memory().unwrap();
        structure(&mut ed).unwrap();
        let parts = ed.model.struct_meshes(Some(ed.level)).unwrap();
        for kind in ["footing", "raft", "brace"] {
            assert!(
                parts
                    .iter()
                    .any(|p| p.0 == kind && !p.2.triangles.is_empty()),
                "{kind}: {:?}",
                parts
                    .iter()
                    .map(|p| (p.0, p.2.triangles.len()))
                    .collect::<Vec<_>>()
            );
        }
        crate::edit3d::struct3d::register();
        let (sc, targets) = crate::edit3d::scene3d::compose(&mut ed);
        let ext = targets
            .iter()
            .filter(|t| matches!(t.cat, crate::edit3d::pick::Cat::Ext(_)))
            .count();
        assert!(
            ext >= parts.len(),
            "{ext} of {} ({} meshes)",
            parts.len(),
            sc.meshes.len()
        );
    }
}
