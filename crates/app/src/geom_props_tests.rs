//! Every registry row: a real change in 3D (and plan where the footprint moves), then exact undo.

use super::*;
use crate::edit3d::scene3d::{compose_with, Cache};
use crate::editor::bodies::BodyKind;
use ddc_model::{
    BeamJoin, BeamLayout, BeamSystemSpec, BraceSpec, ColumnSpec, CompSpec, FootingSpec,
    FootingStep, LayerSpec, OpeningSpec as Op, PipeMaterial, RaftSpec,
    RailingHost, RampSpec, StripFootingSpec, TrussChord, TrussSpec, TrussWeb,
    RailingSpec, RoofKind, RoofSpec, RunSpec, SepSpec, ShaftSpec, SlabSpec, StairSpec, SystemKind, ZoneSpec,
};
use std::collections::HashMap;

pub(crate) struct Fx {
    pub(crate) ed: Editor,
    pub(crate) ids: HashMap<&'static str, i64>,
}

pub(crate) fn square(x: f64, y: f64, s: f64) -> Vec<(f64, f64)> {
    vec![(x, y), (x + s, y), (x + s, y + s), (x, y + s)]
}

fn add_mesh(ed: &mut Editor, category: &str, pts: &[(f64, f64, f64)], tris: &[[u32; 3]]) -> i64 {
    let mesh = Mesh {
        positions: pts.iter().map(|p| v3(p.0, p.1, p.2)).collect(),
        triangles: tris.to_vec(),
        face_of: vec![0; tris.len()],
    };
    let (pb, ib) = ddc_model::mesh_blobs(&mesh);
    let (level, category) = (ed.level, category.to_string());
    ed.model
        .batch_walls("wall_e1_test_mesh", |db| {
            db.execute(
                "INSERT INTO mesh(level,category,name,positions,indices) VALUES (?1,?2,'m',?3,?4)",
                (level, category.as_str(), pb, ib),
            )?;
            Ok(db.last_insert_rowid())
        })
        .unwrap()
}

/// Closed box on two z planes, or with a middle vertex ring when `mid` is set.
fn box_mesh(ed: &mut Editor, category: &str, x: f64, mid: bool) -> i64 {
    let mut pts = vec![];
    for (z, s) in [(0.0, 1.0), (2.0, 1.0)] {
        for (dx, dy) in [(0.0, 0.0), (s, 0.0), (s, s), (0.0, s)] {
            pts.push((x + dx, dy, z));
        }
    }
    if mid {
        pts[4].2 = 1.5;
    }
    let tris = [
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
    add_mesh(ed, category, &pts, &tris)
}

pub(crate) fn fixture() -> Fx {
    let mut ed = Editor::new_in_memory().unwrap();
    let lv = ed.level;
    let layer = |t: f64| {
        [LayerSpec {
            material: "c".into(),
            thickness: t,
        }]
    };
    let m = &mut ed.model;
    let wt = m.add_wall_type("W e1", &layer(0.2)).unwrap();
    let _ = m.add_wall_type("W e1 alt", &layer(0.3)).unwrap();
    let st = m.add_slab_type("S e1", &layer(0.2)).unwrap();
    let _ = m.add_slab_type("S e1 alt", &layer(0.35)).unwrap();
    let mt = m.add_catalog_member_type("R 400x400").unwrap();
    let _ = m.add_catalog_member_type("IPE 200").unwrap();
    let top = m.add_level("E1 top", 3.0).unwrap();
    let mut ids = HashMap::new();

    let mut wall = 0;
    for (a, b) in [
        ((0.0, 0.0), (8.0, 0.0)),
        ((8.0, 0.0), (8.0, 6.0)),
        ((8.0, 6.0), (0.0, 6.0)),
        ((0.0, 6.0), (0.0, 0.0)),
    ] {
        let id = m
            .add_wall(&WallSpec {
                a,
                b,
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wt,
                offset0: None,
            })
            .unwrap();
        if wall == 0 {
            wall = id;
        }
    }
    ids.insert("wall", wall);
    let opening = Op {
        along: 2.0,
        width: 1.0,
        sill: 0.75,
        height: 1.25,
        kind: "window".into(),
    };
    ids.insert("opening", m.add_opening(wall, &opening).unwrap());
    let room = RoomSpec {
        level: lv,
        name: "r".into(),
        number: "1".into(),
        point: (4.0, 3.0),
        height: 2.75,
    };
    ids.insert("room", m.add_room(&room).unwrap());
    ids.insert(
        "solid",
        m.add_solid(&ddc_model::SolidSpec {
            level: lv,
            name: "s".into(),
            void: false,
            material: "Concrete".into(),
            ifc: None,
            frame: ddc_model::SolidFrame::default(),
            form: ddc_model::SolidForm::Extrusion {
                profile: ddc_model::SolidSection::polygon(
                    square(200.0, 0.0, 4.0)
                        .into_iter()
                        .map(|p| ddc_geom::v2(p.0, p.1))
                        .collect(),
                ),
                start: 0.0,
                end: 3.0,
            },
        })
        .unwrap(),
    );
    ids.insert(
        "mass",
        m.add_mass(&ddc_model::MassSpec {
            level: lv,
            name: "m".into(),
            void: false,
            inplace: false,
            form: ddc_model::Form::Cuboid {
                lo: (200.0, 0.0, 0.0),
                hi: (206.0, 6.0, 3.0),
            },
        })
        .unwrap(),
    );
    ids.insert(
        "toposolid",
        m.add_toposolid(&ddc_model::TopoSpec {
            name: "t".into(),
            thickness: 2.0,
            material: "earth".into(),
            points: vec![
                (200.0, 0.0, 0.0),
                (206.0, 0.0, 0.0),
                (206.0, 6.0, 0.0),
                (200.0, 6.0, 0.0),
            ],
            ..Default::default()
        })
        .unwrap(),
    );
    let slab = SlabSpec {
        level: lv,
        slab_type: st,
        offset: 0.0,
        outer: square(20.0, 0.0, 6.0),
        holes: vec![],
    };
    ids.insert("slab", m.add_slab(&slab).unwrap());
    let roof = RoofSpec {
        level: lv,
        slab_type: st,
        offset: 0.0,
        overhang: 0.25,
        fascia: 0.2,
        kind: RoofKind::Footprint {
            ring: square(30.0, 0.0, 6.0),
            slopes: vec![Some(30.0), None, Some(30.0), None],
            mansard: None,
        },
    };
    ids.insert("roof", m.add_roof(&roof).unwrap());
    let column = ColumnSpec {
        level: lv,
        top_level: None,
        member_type: mt,
        structural: true,
        at: (40.0, 0.0),
        base_offset: 0.0,
        top_offset: 3.0,
        rotation: 0.0,
        top_at: None,
    };
    ids.insert("col", m.add_column(&column).unwrap());
    let beam = BeamSpec {
        level: lv,
        member_type: mt,
        a: (44.0, 0.0, 2.75),
        b: (50.0, 0.0, 2.75),
        bulge: 0.0,
        yjust: Just::Center,
        zjust: Just::Center,
        rotation: 0.0,
        start_ext: 0.0,
        end_ext: 0.0,
        join_start: BeamJoin::Auto,
        join_end: BeamJoin::Auto,
    };
    ids.insert("beam", m.add_beam(&beam).unwrap());
    let ceiling = CeilingSpec {
        level: lv,
        slab_type: st,
        height: 2.75,
        outer: square(20.0, 10.0, 6.0),
        holes: vec![],
    };
    ids.insert("ceiling", m.add_ceiling(&ceiling).unwrap());
    let curtain = CurtainSpec {
        level: lv,
        name: "c".into(),
        a: (60.0, 0.0),
        b: (66.0, 0.0),
        bulge: 0.0,
        base_offset: 0.0,
        height: 3.0,
        u: GridLines::Spacing(1.5),
        v: GridLines::Spacing(1.5),
        interior: rect(0.05, 0.1),
        border: rect(0.05, 0.1),
        lines: Default::default(),
        top: vec![],
    };
    ids.insert("curtain_wall", m.add_curtain_wall(&curtain).unwrap());
    let mut stair = StairSpec::straight(lv, top, (70.0, 0.0), 0.0);
    stair.shape = ddc_model::StairShape::L {
        left: true,
        first: None,
    };
    stair.stringer = Some((0.05, 0.3));
    ids.insert("stair", m.add_stair(&stair).unwrap());
    let railing = RailingSpec::new(
        lv,
        RailingHost::Path(vec![(80.0, 0.0, 0.0), (86.0, 0.0, 0.0), (86.0, 6.0, 0.0)]),
    );
    ids.insert("railing", m.add_railing(&railing).unwrap());
    // clear of the footings at x 300..316: a line through one would win its plan click
    ids.insert(
        "grid",
        m.add_grid(&ddc_model::GridSpec::line("E1", (400.0, 0.0), (400.0, 10.0)))
            .unwrap(),
    );
    ids.insert(
        "ref_plane",
        m.add_ref_plane("E1", (410.0, 0.0), (410.0, 10.0)).unwrap(),
    );
    // a steel pipe, so the material row has a catalog to switch; the second system keeps the
    // pipe kind and differs in colour
    let sys = m.add_mep_system("s", SystemKind::ColdWater).unwrap();
    m.add_mep_system("s2", SystemKind::HotWater).unwrap();
    let mut run = RunSpec::pipe(
        sys,
        lv,
        PipeMaterial::Steel,
        50,
        vec![(90.0, 0.0, 2.75), (96.0, 0.0, 2.75)],
    )
    .unwrap();
    run.section.insulation = 0.03125;
    ids.insert("mep_run", m.add_mep_run(&run).unwrap());
    let m = &mut ed.model;
    let concrete = || "Concrete".to_string();
    ids.insert(
        "iso_footing",
        m.add_footing(&FootingSpec {
            level: lv,
            column: None,
            at: (300.0, 0.0),
            rotation: 0.0,
            round: false,
            steps: vec![FootingStep {
                w: 1.2,
                l: 1.2,
                h: 0.5,
            }],
            top_offset: 0.0,
            material: concrete(),
        })
        .unwrap(),
    );
    let strip = StripFootingSpec {
        level: lv,
        wall: None,
        a: (310.0, 0.0),
        b: (316.0, 0.0),
        bulge: 0.0,
        width: 0.6,
        depth: 0.4,
        ecc: 0.0,
        top_offset: 0.0,
        material: concrete(),
    };
    ids.insert("strip_footing", m.add_strip_footings(&[strip]).unwrap()[0]);
    ids.insert(
        "raft",
        m.add_raft(&RaftSpec {
            level: lv,
            ring: square(320.0, 0.0, 6.0),
            thickness: 0.4,
            edge_width: 0.0,
            edge_depth: 0.0,
            top_offset: 0.0,
            material: concrete(),
        })
        .unwrap(),
    );
    ids.insert(
        "brace",
        m.add_brace(&BraceSpec {
            level: lv,
            member_type: mt,
            a: (330.0, 0.0, 0.0),
            b: (334.0, 0.0, 3.0),
            start: None,
            end: None,
            rotation: 0.0,
        })
        .unwrap(),
    );
    ids.insert(
        "truss",
        m.add_truss(&TrussSpec {
            level: lv,
            web: TrussWeb::parse("pratt"),
            chord: TrussChord::parse("pitched"),
            a: (340.0, 0.0, 3.0),
            b: (348.0, 0.0, 3.0),
            height: 1.2,
            heel: 0.2,
            panels: 4,
            chord_type: mt,
            web_type: mt,
        })
        .unwrap(),
    );
    ids.insert(
        "beam_system",
        m.add_beam_system(&BeamSystemSpec {
            level: lv,
            member_type: mt,
            ring: square(350.0, 0.0, 6.0),
            holes: vec![],
            start_edge: None,
            z: 2.75,
            angle: 0.0,
            spacing: 1.0,
            layout: BeamLayout::parse_with("spacing", 0),
            end_gap: 0.0,
            zjust: Just::Center,
        })
        .unwrap(),
    );
    let ramp_top = m.add_level("E5 ramp top", 0.5).unwrap();
    ids.insert(
        "ramp",
        m.add_ramp(&RampSpec::new(
            lv,
            Some(ramp_top),
            vec![(360.0, 0.0), (372.0, 0.0)],
        ))
        .unwrap(),
    );
    let family = m.load_library_family("Sink").unwrap();
    let ctype = m.comp_types(family).unwrap()[0].0;
    ids.insert(
        "comp",
        m.add_component(&CompSpec::at(lv, ctype, 120.0, 0.0)).unwrap(),
    );
    ids.insert(
        "shaft",
        m.add_shaft(&ShaftSpec {
            level: lv,
            top_level: top,
            base_offset: 0.0,
            top_offset: 0.0,
            ring: square(500.0, 0.0, 2.0),
        })
        .unwrap(),
    );
    ids.insert(
        "room_sep",
        m.add_room_sep(&SepSpec {
            level: lv,
            a: (500.0, 10.0),
            b: (504.0, 10.0),
        })
        .unwrap(),
    );
    let mesh = box_mesh(&mut ed, "furniture", 100.0, false);
    ids.insert("mesh", mesh);
    ed.reload().unwrap();
    Fx { ed, ids }
}

/// Two rebar types and stirrup sets on the fixture beam: one by spacing, one by count.
fn rebar_sets(fx: &mut Fx) -> Vec<i64> {
    use ddc_model::{RebarHook, RebarLayout, RebarPath, RebarSetSpec};
    let beam = fx.ids["beam"];
    let m = &mut fx.ed.model;
    let t8 = m.add_rebar_type("E6 g 8", 0.008, "A500", 0.0).unwrap();
    m.add_rebar_type("E6 g 12", 0.012, "A500", 0.0).unwrap();
    let mut set = |rule| RebarSetSpec {
        host_kind: "beam".into(),
        host: beam,
        rtype: t8,
        layout: RebarLayout::Stirrup {
            rule,
            hook: RebarHook::H135,
        },
        mark: None,
    };
    let specs = [set(RebarPath::MaxSpacing(0.2)), set(RebarPath::Fixed(4))];
    let ids = m.add_rebar_sets(&specs).unwrap();
    fx.ed.reload().unwrap();
    ids
}

/// Exact 3D content; `Cache` persists so stale provider tracking fails here too.
pub(crate) fn scene_fp(ed: &mut Editor, c: &mut Cache) -> Vec<String> {
    let (sc, _) = compose_with(ed, c);
    let mut out: Vec<String> = sc
        .meshes
        .iter()
        .map(|m| format!("{}:{:?}:{:?}", m.tag, m.positions, m.triangles))
        .collect();
    out.extend(sc.instanced.iter().map(|i| {
        let places: Vec<_> = i.inst.iter().map(|p| p.m).collect();
        format!(
            "i:{:?}:{:?}:{places:?}",
            i.proto.positions, i.proto.triangles
        )
    }));
    out.sort();
    out
}

/// Everything the plan draws, including what `Layer` derives for tool elements.
pub(crate) fn plan_sig(ed: &Editor) -> String {
    let mut layer = crate::tools::Layer::default();
    layer.sync(&ed.model, ed.level, ed.revision);
    let slabs: Vec<_> = ed
        .slabs
        .iter()
        .map(|s| (s.id, &s.rings, s.top, s.thickness))
        .collect();
    let bodies: Vec<_> = ed
        .bodies
        .iter()
        .map(|b| (b.id, &b.hull, b.zmin, b.zmax))
        .collect();
    // the room point and number are drawn in plan even when the boundary stays put
    let rooms: Vec<(i64, f64, f64, String)> = ed
        .model
        .sql()
        .prepare("SELECT id, px, py, number FROM room ORDER BY id")
        .and_then(|mut st| {
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect()
        })
        .unwrap_or_default();
    // annotations of this plan are drawn there too
    let annos: Vec<(i64, ddc_model::AnnoShape)> = ed
        .model
        .annos(ddc_model::AnnoView::Plan(ed.level))
        .unwrap_or_default()
        .into_iter()
        .map(|a| (a.id, a.shape))
        .collect();
    format!(
        "{:?}|{:?}|{slabs:?}|{bodies:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{rooms:?}|{annos:?}",
        ed.plan,
        ed.openings,
        layer.polys,
        ed.struct_fp.items,
        ed.mass_plan,
        layer.grids,
        layer.refs,
        layer.stairs
    )
}

fn volume(ed: &Editor, room: i64) -> f64 {
    ed.model
        .room_quantities(room)
        .unwrap()
        .map_or(0.0, |q| q.volume)
}

#[test]
fn every_row_changes_geometry_and_undoes_exactly() {
    let mut fx = fixture();
    let room = fx.ids["room"];
    let mut cache = Cache::default();
    let sets = rebar_sets(&mut fx);
    for r in rows() {
        let who = format!("{} {}", r.kind, r.key);
        // a rebar row applies to the layouts that have its number: take the first that does
        let id = if r.kind == "rebar_set" {
            *sets
                .iter()
                .find(|s| (r.get)(&fx.ed.model, **s).is_ok())
                .unwrap_or_else(|| panic!("{who}: no fixture set has this number"))
        } else {
            fx.ids[r.kind]
        };
        let ed = &mut fx.ed;
        if r.key == "geom.landing" {
            // the landing depth only exists on a U stair (an L landing is as wide as the flight)
            let mut st = ed.model.stair(id).unwrap();
            st.shape = ddc_model::StairShape::U {
                left: true,
                first: None,
                gap: 0.1,
            };
            ed.model.update_stair(id, &st).unwrap();
        }
        let v0 = (r.get)(&ed.model, id).unwrap_or_else(|e| panic!("{who}: {e}"));
        let (s0, p0, q0) = (scene_fp(ed, &mut cache), plan_sig(ed), volume(ed, room));
        let v1 = match r.unit {
            Unit::Type(t) => type_options(&ed.model, t).last().unwrap().0 as f64,
            _ => v0 + r.delta,
        };
        assert_ne!(v0, v1, "{who}: the fixture must offer another value");
        let set = r.set;
        ed.run_op(OpFn(Arc::new(move |m: &mut Model| set(m, id, v1))))
            .unwrap_or_else(|e| panic!("{who}: {e}"));
        let got = (r.get)(&ed.model, id).unwrap();
        assert!((got - v1).abs() < 1e-9, "{who}: stored {got}, wanted {v1}");
        let (s1, p1, q1) = (scene_fp(ed, &mut cache), plan_sig(ed), volume(ed, room));
        match r.effect {
            Effect::Scene => assert_ne!(s0, s1, "{who}: 3D did not change"),
            Effect::ScenePlan => {
                assert_ne!(s0, s1, "{who}: 3D did not change");
                assert_ne!(p0, p1, "{who}: plan did not change");
            }
            Effect::Qty => assert_ne!(q0, q1, "{who}: quantity did not change"),
        }
        ed.undo();
        assert_eq!(
            (r.get)(&ed.model, id).unwrap(),
            v0,
            "{who}: value after undo"
        );
        assert_eq!(scene_fp(ed, &mut cache), s0, "{who}: 3D after undo");
        assert_eq!(plan_sig(ed), p0, "{who}: plan after undo");
        assert_eq!(volume(ed, room), q0, "{who}: quantity after undo");
    }
}

/// Top-level field names of a spec, read from its derived Debug output.
fn field_names(debug: &str) -> Vec<String> {
    debug
        .lines()
        .filter_map(|l| l.strip_prefix("    "))
        .filter(|l| !l.starts_with(' ') && l.contains(':'))
        .filter_map(|l| l.split(':').next())
        .map(str::to_string)
        .collect()
}

/// A new spec field fails here until it gets a row or an `ELSEWHERE` line.
#[test]
fn every_spec_field_has_a_row_or_a_reason() {
    let fx = fixture();
    let (m, id) = (&fx.ed.model, |k: &str| fx.ids[k]);
    let (level, slab_type, offset) = crate::query::slab_spec(m, id("slab")).unwrap();
    let slab = SlabSpec {
        level,
        slab_type,
        offset,
        outer: vec![],
        holes: vec![],
    };
    let specs = [
        ("wall", format!("{:#?}", wall_spec(m, id("wall")).unwrap())),
        (
            "opening",
            format!("{:#?}", opening_row(m, id("opening")).unwrap().1),
        ),
        ("slab", format!("{slab:#?}")),
        ("roof", format!("{:#?}", m.roof(id("roof")).unwrap())),
        ("col", format!("{:#?}", m.column(id("col")).unwrap())),
        ("beam", format!("{:#?}", m.beam(id("beam")).unwrap())),
        (
            "ceiling",
            format!("{:#?}", ceiling_spec(m, id("ceiling")).unwrap()),
        ),
        (
            "curtain_wall",
            format!("{:#?}", m.curtain_layout(id("curtain_wall")).unwrap().spec),
        ),
        ("stair", format!("{:#?}", m.stair(id("stair")).unwrap())),
        (
            "railing",
            format!("{:#?}", m.railing(id("railing")).unwrap()),
        ),
        (
            "mep_run",
            format!("{:#?}", m.mep_run(id("mep_run")).unwrap()),
        ),
        ("room", format!("{:#?}", room_spec(m, id("room")).unwrap())),
        (
            "iso_footing",
            format!("{:#?}", m.footing(id("iso_footing")).unwrap()),
        ),
        (
            "strip_footing",
            format!("{:#?}", m.strip_footing(id("strip_footing")).unwrap()),
        ),
        ("raft", format!("{:#?}", m.raft(id("raft")).unwrap())),
        ("brace", format!("{:#?}", m.brace(id("brace")).unwrap())),
        ("truss", format!("{:#?}", m.truss(id("truss")).unwrap())),
        (
            "beam_system",
            format!("{:#?}", m.beam_system(id("beam_system")).unwrap()),
        ),
        ("ramp", format!("{:#?}", m.ramp(id("ramp")).unwrap())),
        ("solid", format!("{:#?}", m.solid(id("solid")).unwrap())),
        ("mass", format!("{:#?}", m.mass(id("mass")).unwrap())),
        (
            "toposolid",
            format!("{:#?}", m.toposolid(id("toposolid")).unwrap()),
        ),
        ("shaft", format!("{:#?}", m.shaft(id("shaft")).unwrap())),
        // no zone in the fixture: its rooms would be drawn and change what the matrix expects
        (
            "zone",
            format!(
                "{:#?}",
                ZoneSpec {
                    name: String::new(),
                    rooms: vec![]
                }
            ),
        ),
        ("room_sep", format!("{:#?}", m.room_sep(id("room_sep")).unwrap())),
        // the detail line of the element matrix fixture; it has no panel rows
        (
            "anno",
            format!(
                "{:#?}",
                ddc_model::AnnoShape::Line {
                    pts: vec![(0.0, 0.0), (1.0, 0.0)],
                    closed: false
                }
            ),
        ),
    ];
    let all = rows();
    for (kind, debug) in &specs {
        let names = field_names(debug);
        assert!(!names.is_empty(), "{kind}: no fields parsed");
        for f in names {
            let in_row = all
                .iter()
                .any(|r| r.kind == *kind && r.fields.contains(&f.as_str()));
            let listed = ELSEWHERE.iter().any(|e| e.0 == *kind && e.1 == f);
            assert!(in_row || listed, "{kind}.{f}: neither a row nor a reason");
        }
    }
    for e in ELSEWHERE {
        assert!(
            specs
                .iter()
                .any(|(k, d)| k == &e.0 && field_names(d).iter().any(|f| f == e.1)),
            "stale ELSEWHERE line {}.{}",
            e.0,
            e.1
        );
    }
}

#[test]
fn panels_reach_the_kinds_that_were_read_only() {
    let all = rows();
    for kind in [
        "col",
        "beam",
        "ceiling",
        "roof",
        "stair",
        "railing",
        "room",
        "slab",
        "wall",
        "opening",
        "curtain_wall",
        "mesh",
        "mep_run",
        "comp",
    ] {
        assert!(all.iter().any(|r| r.kind == kind && r.in_panel), "{kind}");
    }
}

#[test]
fn labels_exist_in_both_locales() {
    let ru = include_str!("../../ui/data/i18n/ru/geom.json").to_string()
        + include_str!("../../ui/data/i18n/ru/sched_columns.json")
        + include_str!("../../ui/data/i18n/ru/mep_systems.json")
        + include_str!("../../ui/data/i18n/ru/sched_datum.json")
        + include_str!("../../ui/data/i18n/ru/room.json")
        + include_str!("../../ui/data/i18n/ru/geom_lean.json");
    let en = include_str!("../../ui/data/i18n/en/geom.json").to_string()
        + include_str!("../../ui/data/i18n/en/sched_columns.json")
        + include_str!("../../ui/data/i18n/en/mep_systems.json")
        + include_str!("../../ui/data/i18n/en/sched_datum.json")
        + include_str!("../../ui/data/i18n/en/room.json")
        + include_str!("../../ui/data/i18n/en/geom_lean.json");
    let mut keys: Vec<&str> = rows().iter().map(|r| r.key).collect();
    keys.extend(Y_JUST.iter().chain(Z_JUST));
    keys.extend(crate::elem::mep::MATERIALS);
    for k in keys {
        let q = format!("\"{k}\"");
        assert!(ru.contains(&q) && en.contains(&q), "{k}");
    }
}

/// Height over a stored top polyline used to be stored but stay invisible in 3D.
#[test]
fn wall_height_replaces_a_top_profile_and_undoes() {
    let mut fx = fixture();
    let id = fx.ids["wall"];
    let top = [(0.0, 3.0), (8.0, 4.0)];
    fx.ed
        .model
        .set_wall_top_profile(id, Some(&top[..]))
        .unwrap();
    fx.ed.reload().unwrap();
    let mut c = Cache::default();
    let s0 = scene_fp(&mut fx.ed, &mut c);
    fx.ed.edit_wall(id, None, None, Some(5.0));
    let w = fx.ed.walls.iter().find(|w| w.id == id).unwrap();
    assert_eq!((w.height, w.top.is_none()), (5.0, true));
    assert_ne!(scene_fp(&mut fx.ed, &mut c), s0);
    fx.ed.undo();
    let w = fx.ed.walls.iter().find(|w| w.id == id).unwrap();
    assert_eq!((w.height, w.top.is_some()), (3.0, true));
    assert_eq!(scene_fp(&mut fx.ed, &mut c), s0);
}

/// Base offset was ignored by the 3D strip: imported walls sat on their level.
#[test]
fn wall_base_offset_lifts_the_body() {
    let zmin = |ed: &Editor| {
        ed.scene_report()
            .0
            .meshes
            .iter()
            .flat_map(|m| m.positions.iter().map(|p| p[2]))
            .fold(f32::MAX, f32::min)
    };
    let mut ed = Editor::new_in_memory().unwrap();
    let layer = [LayerSpec {
        material: "c".into(),
        thickness: 0.2,
    }];
    let wall_type = ed.model.add_wall_type("W e1", &layer).unwrap();
    let spec = WallSpec {
        a: (0.0, 0.0),
        b: (4.0, 0.0),
        level: ed.level,
        base_offset: 0.0,
        height: 3.0,
        wall_type,
        offset0: None,
    };
    let w = ed.model.add_wall(&spec).unwrap();
    ed.reload().unwrap();
    let z0 = zmin(&ed);
    let r = row_of("wall", "geom.base_offset").unwrap();
    commit(&mut ed, &r, w, 0.5);
    assert!((zmin(&ed) - z0 - 0.5).abs() < 1e-5);
    ed.undo();
    assert_eq!(zmin(&ed), z0);
}

#[test]
fn stepped_mesh_height_is_refused_not_guessed() {
    let mut fx = fixture();
    let id = box_mesh(&mut fx.ed, "furniture", 110.0, true);
    fx.ed.reload().unwrap();
    assert!(!mesh_height_exact(
        &fx.ed.model.imported_mesh(id).unwrap().mesh
    ));
    let mut c = Cache::default();
    let s0 = scene_fp(&mut fx.ed, &mut c);
    let r = row_of("mesh", "geom.height").unwrap();
    commit(&mut fx.ed, &r, id, 3.0);
    let status = fx.ed.status.clone().unwrap_or_default();
    assert!(status.contains(&tr("geom.mesh_not_prism", &[])), "{status}");
    assert_eq!(scene_fp(&mut fx.ed, &mut c), s0);
}

#[test]
fn prism_mesh_is_flagged_exact() {
    let fx = fixture();
    let m = fx.ed.model.imported_mesh(fx.ids["mesh"]).unwrap().mesh;
    assert!(mesh_height_exact(&m));
}

#[test]
fn make_editable_converts_or_explains() {
    let mut fx = fixture();
    let id = fx.ids["mesh"];
    let before = fx.ed.model.imported_mesh(id).unwrap().mesh.positions.len();
    fx.ed.status = None;
    crate::mesh_tools::set_keep_source(false);
    make_editable(&mut fx.ed, id);
    crate::mesh_tools::set_keep_source(true);
    assert!(fx.ed.status.is_some(), "never silent");
    if fx.ed.selection.is_empty() && fx.ed.grip.focus.is_empty() {
        let left = fx.ed.model.imported_mesh(id).unwrap().mesh.positions.len();
        assert_eq!(left, before, "no conversion, mesh untouched");
    } else {
        assert!(fx.ed.model.imported_mesh(id).is_err(), "source replaced");
    }
}

#[test]
fn make_editable_keeps_the_source_by_default() {
    let mut fx = fixture();
    let id = fx.ids["mesh"];
    assert!(crate::mesh_tools::keep_source(), "default: keep");
    let before = fx.ed.model.imported_mesh(id).unwrap().mesh.positions.len();
    make_editable(&mut fx.ed, id);
    let left = fx.ed.model.imported_mesh(id).unwrap().mesh.positions.len();
    assert_eq!(left, before, "the source mesh stays whether or not it converted");
}

#[test]
fn formula_on_a_body_explains_itself() {
    let mut fx = fixture();
    let sel = Sel::Body(BodyKind::Mesh, fx.ids["mesh"]);
    assert_eq!(crate::query::assign(&mut fx.ed, &[sel], &[]), Ok(0));
    assert_eq!(
        fx.ed.status.as_deref(),
        Some(tr("geom.formula_body", &[]).as_str())
    );
}

#[test]
fn stored_values_named_like_dimensions_are_tagged() {
    for (key, geometric) in [
        ("Dimensions: Height", true),
        ("Constraints: Base Offset", true),
        ("BaseOffset", true),
        ("Pset_WallCommon.Thickness", true),
        ("Identity Data: Mark", false),
        ("Weight", false),
    ] {
        assert_eq!(looks_geometric(key), geometric, "{key}");
    }
    for (k, geometric) in [("gp_dims_height", true), ("gp_deps_offset", true), ("gp_material", false)] {
        let key = crate::app2_fx::fx(k);
        assert_eq!(looks_geometric(&key), geometric, "{key}");
    }
}

#[test]
fn edit_length_metric_shows_half_mm() {
    let m = Units::Metric;
    assert_eq!(edit_length(0.0125, m), "12,5");
    assert_eq!(edit_length(3.0, m), "3\u{a0}000");
    assert_eq!(edit_length(0.0123456, m), "12,346");
}

#[test]
fn len_drag_enter_without_edit_keeps_sub_mm_value() {
    let m = Units::Metric;
    let cur = 0.0125;
    // Enter without typing: the shown text is parsed back
    let back = crate::units::parse_edit_length(&edit_length(cur, m), m).unwrap();
    assert!((back - cur).abs() < 1e-9);
    assert!(!value_changed(cur, back, Some(m)));
}

#[test]
fn commit_skipped_when_text_unchanged() {
    let m = Units::Metric;
    // a rounded re-parse of an untouched field is not a commit
    assert!(!value_changed(0.0123456, 0.012346, Some(m)));
    // a typed value is
    assert!(value_changed(0.0125, 0.013, Some(m)));
    assert!(value_changed(0.0125, 0.013, None));
}

#[test]
fn multi_num_field_reads_the_ui_decimal_mark() {
    use crate::units::DecimalMark::{Comma as RU, Point as EN};
    assert_eq!(parse_num_mark("1,000 mm", "mm", EN), Some(1000.0));
    assert_eq!(parse_num_mark("1,000.5", "mm", EN), Some(1000.5));
    assert_eq!(parse_num_mark("1,5", "mm", EN), None);
    assert_eq!(parse_num_mark("1,5", "mm", RU), Some(1.5));
    assert_eq!(parse_num_mark("1 000,5", "", RU), Some(1000.5));
    assert_eq!(parse_num_mark("-12.5°", "°", EN), Some(-12.5));
    assert_eq!(parse_num_mark("abc", "", EN), None);
}
