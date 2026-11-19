//! Every probe runs for every row of `FAMILIES`, with no per-family test code. `GAPS` is the
//! ratchet: what fails today is listed, a listed gap that now passes fails the test, an unlisted
//! failure fails the test, and `GAPS_CEILING` only goes down.

use super::{by_table, of_cat, of_sel, Cap, Family, Pick, FAMILIES, NOT_YET};
use crate::edit3d::pick::{Cat, Target as Pick3d};
use crate::edit3d::scene3d::Cache;
use crate::editor::{structx, Editor, OpFn, Sel};
use crate::geom_props::tests::{fixture, plan_sig, scene_fp, square};
use crate::geom_props::{rows, type_options, Unit, ELSEWHERE};
use crate::grips::{self, Role};
use crate::scene_ids::{EXT_ANALYTICAL, EXT_REBAR, EXT_SOLID};
use crate::selection::{candidates, in_box, outlines, BoxMode};
use crate::wall_tool::{Event, Mode};
use ddc_geom::{v2, V2};
use ddc_io::xlsx;
use ddc_model::{
    AnalyticalAdj, AnnoShape, AnnoView, BeamLayout, BeamSystemSpec, BraceSpec, CompSpec,
    FootingSpec, FootingStep, Form, GridSpec, Just, MassSpec, Model, RaftSpec, RampSpec, RebarHook,
    RebarLayout, RebarPos, RebarSetSpec, RebarShape, SepSpec, ShaftSpec, SolidForm, SolidFrame,
    SolidSection, SolidSpec, Spacing, StripFootingSpec, Target, TopoSpec, TrussChord, TrussSpec,
    TrussWeb, ZoneSpec, STRUCT_KINDS,
};
use ddc_ui::i18n::{Catalog, Lang};
use std::collections::BTreeSet;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

pub(super) type Probe = Result<(), String>;

const PROBES: [&str; 14] = [
    "plan", "3d", "props", "grips", "move", "copy", "rotate", "mirror", "array", "delete",
    "schedule", "xlsx", "strings", "snap",
];

/// Only these rows may have gaps: a new family cannot be added with one.
const LEGACY_GAP_TABLES: [&str; 32] = [
    "roof",
    "stair",
    "railing",
    "shaft",
    "zone",
    "grid",
    "ref_plane",
    "col",
    "beam",
    "ceiling",
    "mesh",
    "opening",
    "slab",
    "wall",
    "curtain_wall",
    "mep_run",
    "comp",
    "iso_footing",
    "strip_footing",
    "raft",
    "brace",
    "truss",
    "beam_system",
    "ramp",
    "rebar_set",
    "analytical_adj",
    "solid",
    "mass",
    "toposolid",
    "anno",
    "room",
    "room_sep",
];

/// Groups of the contract that may own a gap.
const LANES: [&str; 17] = [
    "A1", "A2", "B1", "D1", "D2", "D3", "D4", "D5", "D6", "E1", "E2", "E3", "E4", "E5", "E6", "E7",
    "E8",
];

/// Lower it whenever a gap goes; never raise it.
const GAPS_CEILING: usize = 34;

/// (table, probe, group). Seeded from reading the code, before any run: the first build replaces it
/// once with the output of `print_gaps`, after which it can only shrink.
#[rustfmt::skip]
pub(super) const GAPS: &[(&str, &str, &str)] = &[
    ("mesh", "rotate", "D5"),
    ("opening", "grips", "B1"), ("opening", "array", "B1"),
    ("curtain_wall", "move", "E3"), ("curtain_wall", "copy", "E3"), ("curtain_wall", "rotate", "E3"), ("curtain_wall", "mirror", "E3"), ("curtain_wall", "array", "E3"),
    ("mep_run", "plan", "A1"), ("mep_run", "move", "A1"), ("mep_run", "copy", "A1"), ("mep_run", "rotate", "A1"), ("mep_run", "mirror", "A1"), ("mep_run", "array", "A1"),
    ("comp", "plan", "E4"),
    ("rebar_set", "plan", "E6"), ("rebar_set", "3d", "E6"),
    ("analytical_adj", "plan", "E6"), ("analytical_adj", "3d", "E6"), ("analytical_adj", "props", "E6"), ("analytical_adj", "schedule", "E6"),
    ("solid", "plan", "E7"), ("solid", "move", "E7"), ("solid", "copy", "E7"), ("solid", "rotate", "E7"), ("solid", "mirror", "E7"), ("solid", "array", "E7"),
    ("anno", "3d", "E8"), ("anno", "grips", "E8"),
];

/// Tables of the clip registry that are not elements of their own.
const NOT_ELEMENTS: &[(&str, &str)] = &[
    ("roof_vertex", "part of a roof"),
    ("roof_ext", "part of a roof"),
    ("slab_vertex", "part of a slab"),
    ("slab_shape", "part of a slab"),
    ("wall_profile", "part of a wall"),
    ("wall_join", "relation between walls"),
    ("wall_sweep", "part of a wall"),
    ("wall_stack", "part of a wall"),
    ("wall_attach", "relation of a wall"),
    ("wall_attach_level", "relation of a wall"),
    ("wall_spline", "axis of a spline wall: its pieces are walls"),
    ("wall_spline_piece", "relation of a spline wall to its pieces"),
    ("curtain_panel", "sub-element of a curtain wall"),
    ("curtain_edit", "part of a curtain wall"),
    ("view_def", "a view, not a model element"),
    ("skylight", "edited by the roof edge tools"),
    ("soffit", "edited by the roof edge tools"),
    ("sweep", "edited by the roof edge tools"),
    ("roof_taper", "edited by the roof edge tools"),
    ("roof_drain", "edited by the roof edge tools"),
    ("shaft_vertex", "part of a shaft"),
    ("ceiling_vertex", "part of a ceiling"),
    ("railing_vertex", "part of a railing"),
    ("mass_paint", "part of a mass"),
    ("mass_host", "relation of a mass"),
    ("solid_host", "relation of a solid"),
    ("host_opening", "cut of its host, edited by the sketch tools"),
    ("raft_vertex", "part of a raft"),
    ("beam_system_vertex", "part of a beam system"),
    ("topo_point", "part of a topography"),
    ("topo_region", "part of a topography"),
    ("topo_pad", "edited by the topography tools"),
    ("d_line", "drawing entity, belongs to underlay"),
    ("d_polyline", "drawing entity, belongs to underlay"),
    ("d_vertex", "drawing entity, belongs to underlay"),
    ("d_arc", "drawing entity, belongs to underlay"),
    ("d_text", "drawing entity, belongs to underlay"),
    ("d_insert", "drawing entity, belongs to underlay"),
    ("d_attr", "drawing entity, belongs to underlay"),
    ("prop", "property of an element"),
    ("pparam_val", "parameter value of an element"),
    ("sketch", "part of an element"),
    ("elem_phase", "phase of an element"),
    ("datum_extent", "part of a grid or level"),
    ("dim", "annotation, handled by its own tools"),
    ("constraint_rule", "relation between elements"),
    ("geom_join", "relation between elements"),
    ("zone_room", "relation of a zone"),
    ("mep_port", "part of a network element"),
    ("rebar_cover", "part of a rebar host"),
    ("wall_type", "dictionary"),
    ("slab_type", "dictionary"),
    ("member_type", "dictionary"),
    ("comp_family", "dictionary"),
    ("comp_type", "dictionary"),
    ("mep_system", "dictionary"),
    ("stack_type", "dictionary"),
    ("d_layer", "dictionary"),
    ("d_block", "dictionary"),
    ("pparam", "dictionary"),
    ("sweep_profile", "dictionary"),
    ("phase", "dictionary"),
    ("rebar_type", "dictionary"),
    ("anno_text_style", "dictionary"),
    ("anno_line_style", "dictionary"),
];

const OFFSET: (f64, f64) = (0.5, 0.25);

/// One headless editor for a family: the shared fixture plus its element and a free wall.
pub(super) struct Ctx {
    pub(super) ed: Editor,
    pub(super) id: i64,
    pub(super) free_wall: i64,
    pub(super) cache: Cache,
}

/// Adds the family's element when the shared fixture has none.
pub(super) fn spawn(table: &str, ed: &mut Editor, ids: &std::collections::HashMap<&'static str, i64>) -> i64 {
    if let Some(id) = ids.get(table) {
        return *id;
    }
    let level = ed.level;
    let concrete = || "Concrete".to_string();
    let m = &mut ed.model;
    let member = m.column(ids["col"]).unwrap().member_type;
    let top = m.add_level("C0 top", 6.0).unwrap();
    let id = match table {
        "room_sep" => m.add_room_sep(&SepSpec {
            level,
            a: (200.0, 0.0),
            b: (204.0, 0.0),
        }),
        "shaft" => m.add_shaft(&ShaftSpec {
            level,
            top_level: top,
            base_offset: 0.0,
            top_offset: 0.0,
            ring: square(200.0, 0.0, 2.0),
        }),
        "zone" => m.add_zone(&ZoneSpec {
            name: "z".into(),
            rooms: vec![ids["room"]],
        }),
        "grid" => m.add_grid(&GridSpec::line("C0", (200.0, 0.0), (200.0, 10.0))),
        "ref_plane" => m.add_ref_plane("C0", (200.0, 0.0), (200.0, 10.0)),
        "comp" => {
            let family = m.load_library_family("Sink").unwrap();
            let ctype = m.comp_types(family).unwrap()[0].0;
            m.add_component(&CompSpec::at(level, ctype, 200.0, 0.0))
        }
        "iso_footing" => m.add_footing(&FootingSpec {
            level,
            column: None,
            at: (200.0, 0.0),
            rotation: 0.0,
            round: false,
            steps: vec![FootingStep {
                w: 1.2,
                l: 1.2,
                h: 0.5,
            }],
            top_offset: 0.0,
            material: concrete(),
        }),
        "strip_footing" => m
            .add_strip_footings(&[StripFootingSpec {
                level,
                wall: None,
                a: (200.0, 0.0),
                b: (206.0, 0.0),
                bulge: 0.0,
                width: 0.6,
                depth: 0.4,
                ecc: 0.0,
                top_offset: 0.0,
                material: concrete(),
            }])
            .map(|v| v[0]),
        "raft" => m.add_raft(&RaftSpec {
            level,
            ring: square(200.0, 0.0, 6.0),
            thickness: 0.4,
            edge_width: 0.0,
            edge_depth: 0.0,
            top_offset: 0.0,
            material: concrete(),
        }),
        "brace" => m.add_brace(&BraceSpec {
            level,
            member_type: member,
            a: (200.0, 0.0, 0.0),
            b: (204.0, 0.0, 3.0),
            start: None,
            end: None,
            rotation: 0.0,
        }),
        "truss" => m.add_truss(&TrussSpec {
            level,
            web: TrussWeb::parse("pratt"),
            chord: TrussChord::parse("flat"),
            a: (200.0, 0.0, 3.0),
            b: (208.0, 0.0, 3.0),
            height: 1.2,
            heel: 0.2,
            panels: 4,
            chord_type: member,
            web_type: member,
        }),
        "beam_system" => m.add_beam_system(&BeamSystemSpec {
            level,
            member_type: member,
            ring: square(200.0, 0.0, 6.0),
            holes: vec![],
            start_edge: None,
            z: 2.75,
            angle: 0.0,
            spacing: 1.0,
            layout: BeamLayout::parse_with("spacing", 0),
            end_gap: 0.0,
            zjust: Just::Center,
        }),
        "ramp" => {
            let rise = m.add_level("C0 ramp top", 0.5).unwrap();
            m.add_ramp(&RampSpec::new(
                level,
                Some(rise),
                vec![(200.0, 0.0), (212.0, 0.0)],
            ))
        }
        "rebar_set" => {
            let rtype = m.add_rebar_type("T8", 0.008, "A500", 0.032).unwrap();
            // a second type, so the type row of the sheet has another value to take
            m.add_rebar_type("T12", 0.012, "A500", 0.0).unwrap();
            // straight bottom bars: plain lines in plan, so an edge click and Tab can find them
            m.add_rebar_sets(&[RebarSetSpec {
                host_kind: "beam".into(),
                host: ids["beam"],
                rtype,
                layout: RebarLayout::Long {
                    pos: RebarPos::Bottom,
                    count: 2,
                    shape: RebarShape::Straight,
                    hooks: (RebarHook::None, RebarHook::None),
                    leg: 0.0,
                },
                mark: None,
            }])
            .map(|v| v[0])
        }
        // the first row of an empty table: the model hands out no row id for an adjustment
        "analytical_adj" => m
            .set_analytical_adj(
                "beam",
                &[ids["beam"]],
                AnalyticalAdj {
                    start: (0.0, 0.0, 0.1),
                    end: (0.0, 0.0, 0.0),
                    release_start: None,
                    release_end: None,
                },
            )
            .map(|_| 1),
        "solid" => m.add_solid(&SolidSpec {
            level,
            name: "s".into(),
            void: false,
            material: concrete(),
            ifc: None,
            frame: SolidFrame::default(),
            form: SolidForm::Extrusion {
                profile: SolidSection::polygon(
                    square(200.0, 0.0, 4.0)
                        .into_iter()
                        .map(|p| v2(p.0, p.1))
                        .collect(),
                ),
                start: 0.0,
                end: 3.0,
            },
        }),
        "mass" => m.add_mass(&MassSpec {
            level,
            name: "m".into(),
            void: false,
            inplace: false,
            form: Form::Cuboid {
                lo: (200.0, 0.0, 0.0),
                hi: (206.0, 6.0, 3.0),
            },
        }),
        "toposolid" => m.add_toposolid(&TopoSpec {
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
        }),
        "anno" => m.add_anno(
            AnnoView::Plan(level),
            &AnnoShape::Line {
                pts: vec![(200.0, 0.0), (204.0, 0.0)],
                closed: false,
            },
            None,
            None,
        ),
        other => panic!("family {other} has no fixture"),
    };
    id.unwrap_or_else(|e| panic!("fixture of {table}: {e}"))
}

pub(super) fn ctx(table: &str) -> Ctx {
    // process-wide selection left by other tests would join a delete
    crate::solid_tools::select(None);
    // armed paste/match and half-drawn edits of the previous family would swallow the click
    crate::clip_tools::reset();
    crate::roof_edge_tools::reset();
    // the app registers its 3D providers at start; without them the scene has no tool bodies
    crate::edit3d::struct3d::register();
    crate::edit3d::ramp3d::register();
    let mut fx = fixture();
    let id = spawn(table, &mut fx.ed, &fx.ids);
    // the fixture solid and mass cover the square where other families are spawned and are
    // picked first
    for t in ["solid", "mass"].into_iter().filter(|t| *t != table) {
        fx.ed.model.delete_items("elem_delete", &[(t, fx.ids[t])]).unwrap();
    }
    let mut spec = crate::edit3d::grips::wall_spec(&fx.ed.model, fx.ids["wall"]).unwrap();
    spec.a = (0.0, 100.0);
    spec.b = (6.0, 100.0);
    let free_wall = fx.ed.model.add_wall(&spec).unwrap();
    fx.ed.reload().unwrap();
    fx.ed.set_mode(Mode::Select);
    Ctx {
        ed: fx.ed,
        id,
        free_wall,
        cache: Cache::default(),
    }
}

pub(super) fn op(
    ed: &mut Editor,
    f: impl Fn(&mut Model) -> Result<(), ddc_model::Error> + Send + Sync + 'static,
) -> Probe {
    ed.run_op(OpFn(Arc::new(f))).map_err(|e| e.to_string())
}

fn rows_text(ed: &Editor, items: &[(&str, i64)]) -> String {
    ed.model
        .clip_copy(items)
        .map_or_else(|e| e.to_string(), |c| c.to_text())
}

pub(super) fn row_exists(ed: &Editor, table: &str, id: i64) -> bool {
    // a clip lists its roots even for a row that is gone, so ask the table itself
    let q = format!("SELECT 1 FROM {table} WHERE id=?1");
    match ed.model.sql().query_row(&q, [id], |_| Ok(())) {
        Ok(()) => true,
        Err(rusqlite::Error::QueryReturnedNoRows) => false,
        Err(_) => ed
            .model
            .clip_copy(&[(table, id)])
            .is_ok_and(|c| !c.is_empty()),
    }
}

#[derive(PartialEq)]
struct Snap {
    scene: Vec<String>,
    plan: String,
    rows: String,
}

fn snap(c: &mut Ctx, items: &[(&str, i64)]) -> Snap {
    Snap {
        scene: scene_fp(&mut c.ed, &mut c.cache),
        plan: plan_sig(&c.ed),
        rows: rows_text(&c.ed, items),
    }
}

fn exact_after_undo(c: &mut Ctx, items: &[(&str, i64)], before: &Snap) -> Probe {
    c.ed.undo();
    if snap(c, items) == *before {
        Ok(())
    } else {
        Err("one undo did not restore the exact state".into())
    }
}

fn catalog() -> &'static Catalog {
    static CAT: OnceLock<Catalog> = OnceLock::new();
    CAT.get_or_init(|| Catalog::new(Lang::En))
}

fn has_key(key: &str) -> bool {
    [Lang::En, Lang::Ru]
        .into_iter()
        .all(|l| catalog().table(l).contains_key(key))
}

pub(super) fn plan(c: &mut Ctx, f: &Family) -> Probe {
    let sel = f.sel(c.id).ok_or("no selection")?;
    let rings = outlines(&c.ed, sel);
    let ring = rings.iter().find(|r| r.len() >= 2).ok_or("no plan outline")?;
    let n = ring.len() as f64;
    let inner = ring.iter().fold(v2(0.0, 0.0), |a, p| a + *p * (1.0 / n));
    let edge = (ring[0] + ring[1]) * 0.5;
    let mut click = |p: V2| {
        c.ed.selection.clear();
        c.ed.handle(Event::Click(p));
        c.ed.selection.clone()
    };
    match f.pick {
        Pick::Solid | Pick::Last => {
            let got = click(inner);
            if got != [sel] {
                return Err(format!("click inside did not select it (got {got:?})"));
            }
        }
        Pick::EdgeOnly => {
            if click(edge) != [sel] {
                return Err("click on the edge did not select it".into());
            }
            if ring.len() >= 3 && click(inner).contains(&sel) {
                return Err("click inside an edge-only family selected it".into());
            }
        }
    }
    let lo = ring.iter().fold(ring[0], |a, p| v2(a.x.min(p.x), a.y.min(p.y)));
    let hi = ring.iter().fold(ring[0], |a, p| v2(a.x.max(p.x), a.y.max(p.y)));
    let boxed = in_box(
        &c.ed,
        lo - v2(1.0, 1.0),
        hi + v2(1.0, 1.0),
        BoxMode::Crossing,
        |_| true,
    );
    if !boxed.contains(&sel) {
        return Err("box did not select it".into());
    }
    let at = if f.pick == Pick::EdgeOnly { edge } else { inner };
    let tab = candidates(&c.ed, at, 0.25, |_| true);
    if !tab.iter().flatten().any(|s| *s == sel) {
        return Err("Tab does not offer it".into());
    }
    Ok(())
}

pub(super) fn three_d(c: &mut Ctx, f: &Family) -> Probe {
    let cat = f.cat3d.ok_or("no 3D category")?;
    let sel = f.sel(c.id).ok_or("no selection")?;
    let t = Pick3d::new(cat, c.id);
    if t.sel() != Some(sel) || Pick3d::from_sel(sel) != Some(t) {
        return Err("Target -> Sel -> Target is not identity".into());
    }
    let (table, id) = (f.table, c.id);
    let items = [(table, id)];
    let before = snap(c, &items);
    op(&mut c.ed, move |m| m.delete_items("wall_cut", &[(table, id)]))?;
    let gone = scene_fp(&mut c.ed, &mut c.cache);
    if gone == before.scene {
        return Err("the scene has no body for it".into());
    }
    exact_after_undo(c, &items, &before)
}

pub(super) fn props(f: &Family) -> Probe {
    let row = rows().iter().any(|r| r.kind == f.table && r.in_panel);
    if row || ELSEWHERE.iter().any(|e| e.0 == f.table) {
        Ok(())
    } else {
        Err("no panel row and no ELSEWHERE reason".into())
    }
}

fn grip_probe(c: &mut Ctx, f: &Family) -> Probe {
    let kind = match f.grips {
        Cap::Yes(k) => k,
        Cap::No(r) => return no_route(r),
    };
    let all = grips::grips(&c.ed.model, kind, c.id).map_err(|e| e.to_string())?;
    let g = all
        .iter()
        .find(|g| g.role == Role::Move)
        .or_else(|| {
            all.iter()
                .find(|g| matches!(g.role, Role::End(_) | Role::Vertex(_) | Role::Edge(_)))
        })
        .cloned()
        .ok_or("no draggable plan grip")?;
    // stands in for the 3D provider until the generic one lands in D1
    if f.cat3d.and_then(|cat| Pick3d::new(cat, c.id).grip_kind()) != Some(kind) {
        return Err("no 3D grips".into());
    }
    let items = [(f.table, c.id)];
    let before = snap(c, &items);
    let to = g.pos + v2(OFFSET.0, OFFSET.1);
    op(&mut c.ed, move |m| grips::apply(m, &g, to))?;
    if snap(c, &items) == before {
        return Err("the drag changed nothing".into());
    }
    exact_after_undo(c, &items, &before)
}

/// A capability with no route to try: design refusals pass, `NOT_YET` is a gap.
pub(super) fn no_route(reason: &str) -> Probe {
    if reason == NOT_YET {
        Err("not yet".into())
    } else if has_key(reason) {
        Ok(())
    } else {
        Err(format!("reason key {reason} is missing"))
    }
}

#[derive(Clone, Copy)]
enum Xf {
    Move,
    Copy,
    Rotate,
    Mirror,
    Array,
}

fn apply_xf(m: &mut Model, t: Target, x: Xf) -> Result<usize, ddc_model::Error> {
    match x {
        Xf::Move => m.move_elements(&[t], OFFSET).map(|_| 0),
        Xf::Copy => m.copy_elements(&[t], &[OFFSET]).map(|v| v.len()),
        Xf::Rotate => m.rotate_elements(&[t], (0.0, 0.0), 0.5, false).map(|_| 0),
        Xf::Mirror => m
            .mirror_elements(&[t], (-1.0, 0.0), (-1.0, 1.0), false)
            .map(|_| 0),
        Xf::Array => m
            .array_linear(&[t], 3, Spacing::Step(OFFSET))
            .map(|v| v.len()),
    }
}

fn xf_probe(c: &mut Ctx, f: &Family, x: Xf) -> Probe {
    let cap = match x {
        Xf::Move => f.xf.mv,
        Xf::Copy => f.xf.copy,
        Xf::Rotate => f.xf.rotate,
        Xf::Mirror => f.xf.mirror,
        Xf::Array => f.xf.array,
    };
    let target = match (f.target, cap) {
        (_, Cap::No(r)) if r == NOT_YET => return Err("not yet".into()),
        (Cap::No(_), Cap::No(r)) => return no_route(r),
        (Cap::No(r), Cap::Yes(())) => return Err(format!("no Target: {r}")),
        (Cap::Yes(t), _) => t,
    };
    let t = target(c.id);
    if let Cap::No(r) = cap {
        no_route(r)?;
        let head = c.ed.model.change_head();
        let refused = op(&mut c.ed, move |m| apply_xf(m, t, x).map(|_| ())).is_err();
        return if refused && c.ed.model.change_head() == head {
            Ok(())
        } else {
            Err("a refused action changed the model".into())
        };
    }
    let items = [(f.table, c.id)];
    let before = snap(c, &items);
    let count = |c: &Ctx| {
        c.ed.model
            .sql()
            .query_row(&format!("SELECT count(*) FROM {}", f.table), [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap_or(-1)
    };
    let rows0 = count(c);
    let made = Arc::new(AtomicUsize::new(0));
    let out = made.clone();
    op(&mut c.ed, move |m| {
        out.store(apply_xf(m, t, x)?, Ordering::Relaxed);
        Ok(())
    })?;
    let after = snap(c, &items);
    if f.cat3d.is_none() {
        // no body in the scene (a datum line): the rows change, or the table grows
        if after.rows == before.rows && count(c) <= rows0 {
            return Err("the model rows must change".into());
        }
    } else if after.scene == before.scene || after.plan == before.plan {
        return Err("3D and plan must both change".into());
    }
    let n = made.load(Ordering::Relaxed);
    let copies_ok = match x {
        Xf::Copy => n == 1,
        Xf::Array => n >= 2,
        _ => true,
    };
    if !copies_ok {
        return Err(format!("wrong number of copies: {n}"));
    }
    exact_after_undo(c, &items, &before)
}

/// A sub-element of the family: it must never take its parent with it.
fn sub_element(f: &Family, id: i64) -> Option<Sel> {
    (f.table == "curtain_wall").then_some(Sel::Panel(id, 0))
}

fn delete(c: &mut Ctx, f: &Family) -> Probe {
    if let Cap::No(r) = f.xf.delete {
        return no_route(r);
    }
    // an axis is selected by its scene id, not by the id of its adjustment row
    let key = super::rebar::axis_key(&c.ed.model, c.id).filter(|_| f.table == "analytical_adj");
    let sel = f.sel(key.unwrap_or(c.id)).ok_or("no selection")?;
    let (table, id, wall) = (f.table, c.id, c.free_wall);
    let items = [(table, id)];
    let before = snap(c, &items);
    c.ed.selection = vec![sel];
    c.ed.delete_selection();
    if row_exists(&c.ed, table, id) {
        return Err("the row is still there".into());
    }
    exact_after_undo(c, &items, &before)?;

    if table != "wall" {
        let mixed = [(table, id), ("wall", wall)];
        let before = snap(c, &mixed);
        c.ed.selection = vec![sel, Sel::Wall(wall)];
        c.ed.delete_selection();
        if row_exists(&c.ed, table, id) || row_exists(&c.ed, "wall", wall) {
            return Err("a selection mixed with a wall left a row".into());
        }
        exact_after_undo(c, &mixed, &before)?;
    }

    if let Some(sub) = sub_element(f, id) {
        let head = c.ed.model.change_head();
        c.ed.selection = vec![sub];
        c.ed.delete_selection();
        if !row_exists(&c.ed, table, id) {
            return Err("deleting a sub-element removed its parent".into());
        }
        if c.ed.model.change_head() != head {
            c.ed.undo();
        }
    }
    Ok(())
}

fn schedule_probe(c: &Ctx, f: &Family) -> Probe {
    let cat = match f.schedule {
        Cap::Yes(cat) => cat,
        Cap::No(r) => return no_route(r),
    };
    let t = crate::schedule::build(&c.ed, cat);
    if t.rows.iter().any(|r| r.id == c.id) {
        Ok(())
    } else {
        Err("no schedule row".into())
    }
}

/// Export the family sheet, edit the first geometry row in the file, preview, apply, undo once.
fn xlsx_probe(c: &mut Ctx, f: &Family) -> Probe {
    let sheet = match f.sheet {
        Cap::Yes(s) => s,
        Cap::No(r) => return no_route(r),
    };
    let fams = super::sheets::families();
    let dir = std::env::temp_dir().join("ddc_elem_xlsx");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let (src, dst) = (
        dir.join(format!("{}_{}.xlsx", std::process::id(), f.table)),
        dir.join(format!("{}_{}_edit.xlsx", std::process::id(), f.table)),
    );
    let u = xlsx::Units::Metric;
    xlsx::export_xlsx_families(&c.ed.model, &src, u, &fams)?;
    if !xlsx::sheet_ids(&src, sheet)?.contains(&c.id) {
        return Err(format!("{sheet}: no row for id {}", c.id));
    }
    // bespoke sheets are covered by the ddc-io tests; a family without rows has nothing to edit
    let (Some(fam), Some(r)) = (
        fams.iter().find(|x| x.table == f.table),
        rows().into_iter().find(|r| r.kind == f.table),
    ) else {
        return Ok(());
    };
    let col = fam.cols.iter().find(|x| x.key == r.key).ok_or("no column for the row")?;
    let id = c.id;
    let v0 = (r.get)(&c.ed.model, id).map_err(|e| e.to_string())?;
    let v1 = match r.unit {
        Unit::Type(t) => type_options(&c.ed.model, t).last().ok_or("no type")?.0 as f64,
        _ => v0 + r.delta,
    };
    let cell = if r.unit == Unit::Deg { v1.to_degrees() } else { v1 };
    xlsx::patch_cell(&src, &dst, sheet, id, &col.header(u), cell)?;
    let o = xlsx::ImportOptions { apply: false, delete_missing: false, new_params: false };
    let rep = xlsx::import_xlsx_families(&mut c.ed.model, &dst, o, &fams)?;
    if !rep.rejected.is_empty() || rep.props.len() != 1 {
        return Err(format!("preview: {:?} {:?}", rep.rejected, rep.props));
    }
    c.ed.apply_xlsx_props(rep.batch(), rep.props.clone(), "probe")?;
    let got = (r.get)(&c.ed.model, id).map_err(|e| e.to_string())?;
    if (got - v1).abs() > 1e-6 {
        return Err(format!("applied {got}, wanted {v1}"));
    }
    c.ed.undo();
    let back = (r.get)(&c.ed.model, id).map_err(|e| e.to_string())?;
    if (back - v0).abs() > 1e-9 {
        return Err(format!("one undo left {back}, wanted {v0}"));
    }
    Ok(())
}

fn strings(f: &Family) -> Probe {
    for key in [
        format!("elem.{}.one", f.table),
        format!("elem.{}.many", f.table),
        "elem.deleted_what".to_string(),
        "elem.not_deleted_what".to_string(),
    ] {
        if !has_key(&key) {
            return Err(format!("{key} is missing"));
        }
        let en = catalog().t_in(Lang::En, &key);
        if en.chars().any(|ch| ('\u{400}'..='\u{4FF}').contains(&ch)) {
            return Err(format!("{key} is not English"));
        }
    }
    Ok(())
}

/// Every family answers with a snap profile: the free point and enough kinds to place by.
fn snap_profile(f: &Family) -> Probe {
    let p = f.snap;
    if !p.has(crate::snap::SnapKind::Free) {
        return Err("the profile leaves out the free point".into());
    }
    if p.kinds.count_ones() < 4 {
        return Err("the profile offers almost no snap kind".into());
    }
    Ok(())
}

fn run(probe: &str, c: &mut Ctx, f: &Family) -> Probe {
    match probe {
        "plan" => plan(c, f),
        "3d" => three_d(c, f),
        "props" => props(f),
        "grips" => grip_probe(c, f),
        "move" => xf_probe(c, f, Xf::Move),
        "copy" => xf_probe(c, f, Xf::Copy),
        "rotate" => xf_probe(c, f, Xf::Rotate),
        "mirror" => xf_probe(c, f, Xf::Mirror),
        "array" => xf_probe(c, f, Xf::Array),
        "delete" => delete(c, f),
        "schedule" => schedule_probe(c, f),
        "xlsx" => xlsx_probe(c, f),
        "strings" => strings(f),
        "snap" => snap_profile(f),
        other => panic!("unknown probe {other}"),
    }
}

/// Failing (table, probe) cells with their reasons. A failed probe that moved the model may leave
/// it dirty, so the next one starts from a new editor; a panic counts as a failure.
fn failures() -> Vec<(&'static str, &'static str, String)> {
    let mut out = vec![];
    for f in FAMILIES {
        let mut c = ctx(f.table);
        for probe in PROBES {
            let head = c.ed.model.change_head();
            let r = catch_unwind(AssertUnwindSafe(|| run(probe, &mut c, f)))
                .unwrap_or_else(|_| Err("panicked".into()));
            if let Err(why) = r {
                out.push((f.table, probe, why));
                if c.ed.model.change_head() != head {
                    c = ctx(f.table);
                }
            }
        }
    }
    out
}

#[test]
fn gaps_only_shrink() {
    assert!(
        GAPS.len() <= GAPS_CEILING,
        "GAPS grew: {} over {GAPS_CEILING}",
        GAPS.len()
    );
    let fails = failures();
    let failing: BTreeSet<(&str, &str)> = fails.iter().map(|x| (x.0, x.1)).collect();
    let listed: BTreeSet<(&str, &str)> = GAPS.iter().map(|x| (x.0, x.1)).collect();
    let new: Vec<String> = fails
        .iter()
        .filter(|x| !listed.contains(&(x.0, x.1)))
        .map(|x| format!("{} {}: {}", x.0, x.1, x.2))
        .collect();
    let fixed: Vec<_> = listed.difference(&failing).collect();
    assert!(new.is_empty(), "new failures, fix them: {new:#?}");
    assert!(fixed.is_empty(), "now passing, remove from GAPS: {fixed:?}");
}

/// Regenerates the literal once on the first build: `-- --ignored --nocapture print_gaps`.
#[test]
#[ignore = "prints the measured GAPS"]
fn print_gaps() {
    let fails = failures();
    for x in &fails {
        let lane = GAPS
            .iter()
            .find(|g| g.0 == x.0 && g.1 == x.1)
            .map_or("?", |g| g.2);
        println!("    (\"{}\", \"{}\", \"{lane}\"), // {}", x.0, x.1, x.2);
    }
    println!("GAPS_CEILING = {}", fails.len());
}

#[test]
fn gaps_list_is_well_formed() {
    let mut seen = BTreeSet::new();
    for (table, probe, lane) in GAPS {
        assert!(seen.insert((table, probe)), "duplicate {table} {probe}");
        assert!(PROBES.contains(probe), "unknown probe {probe}");
        assert!(LANES.contains(lane), "unknown lane {lane}");
        assert!(
            LEGACY_GAP_TABLES.contains(table),
            "{table} may not have gaps"
        );
        let f = by_table(table).unwrap_or_else(|| panic!("{table} is not a family"));
        assert!(f.code.is_none(), "{table} migrated: it may have no gaps");
    }
    for f in FAMILIES {
        assert!(
            f.code.is_some() || LEGACY_GAP_TABLES.contains(&f.table),
            "new family {} must come with a code and no gaps",
            f.table
        );
    }
}

#[test]
fn every_clip_table_is_a_family_or_has_a_reason() {
    for t in ddc_model::clip_tables() {
        assert!(
            by_table(t).is_some() || NOT_ELEMENTS.iter().any(|e| e.0 == t),
            "{t}: neither a family nor in NOT_ELEMENTS"
        );
    }
    for (t, _) in NOT_ELEMENTS {
        assert!(by_table(t).is_none(), "{t} is both");
    }
}

#[test]
fn table_is_unique_and_selection_maps_back() {
    let mut seen = BTreeSet::new();
    for f in FAMILIES {
        assert!(seen.insert(f.table), "duplicate row {}", f.table);
        if let Some(s) = f.sel(1) {
            assert_eq!(of_sel(s).map(|x| x.table), Some(f.table), "Sel of {}", f.table);
        }
        if let Some(c) = f.cat3d {
            assert_eq!(of_cat(c).map(|x| x.table), Some(f.table), "Cat of {}", f.table);
        }
    }
}

#[test]
fn every_selectable_category_names_a_family() {
    let mut codes = vec![EXT_SOLID, EXT_REBAR, EXT_ANALYTICAL];
    codes.extend(STRUCT_KINDS.iter().filter_map(|k| structx::code_of(k)));
    codes.extend(structx::CORE_KINDS.iter().filter_map(|k| structx::code_of(k)));
    codes.extend(structx::DATUM_KINDS.iter().filter_map(|k| structx::code_of(k)));
    for c in codes {
        assert!(of_sel(Sel::Ext(c, 1)).is_some(), "Sel::Ext {c}");
        assert!(of_cat(Cat::Ext(c)).is_some(), "Cat::Ext {c}");
    }
    assert_eq!(of_sel(Sel::Panel(1, 0)).map(|f| f.table), Some("curtain_wall"));
    assert_eq!(of_cat(Cat::Panel).map(|f| f.table), Some("curtain_wall"));
}

#[test]
fn every_family_has_a_fixture_with_a_row() {
    for f in FAMILIES {
        let c = ctx(f.table);
        assert!(row_exists(&c.ed, f.table, c.id), "{} fixture", f.table);
    }
}

#[test]
fn every_reason_key_exists_in_both_locales() {
    for f in FAMILIES {
        let x = f.xf;
        let mut reasons = vec![];
        for cap in [x.mv, x.copy, x.rotate, x.mirror, x.array, x.delete] {
            if let Cap::No(r) = cap {
                reasons.push(r);
            }
        }
        if let Cap::No(r) = f.grips {
            reasons.push(r);
        }
        if let Cap::No(r) = f.target {
            reasons.push(r);
        }
        if let Cap::No(r) = f.schedule {
            reasons.push(r);
        }
        if let Cap::No(r) = f.sheet {
            reasons.push(r);
        }
        for r in reasons {
            assert!(has_key(r), "{}: reason key {r}", f.table);
        }
    }
}

/// Row removal through the workbook: a delete flag previews as one deleted row, applies as one
/// undo step and comes back with one undo. A family the workbook may not delete refuses per row.
#[test]
fn xlsx_row_delete_is_one_undo_step_or_a_clear_refusal() {
    let fams = super::sheets::families();
    let dir = std::env::temp_dir().join("ddc_elem_xlsx_del");
    std::fs::create_dir_all(&dir).unwrap();
    let u = xlsx::Units::Metric;
    let o = xlsx::ImportOptions { apply: false, delete_missing: false, new_params: false };
    let mut bad = vec![];
    for f in FAMILIES {
        let Some(fam) = fams.iter().find(|x| x.table == f.table) else { continue };
        let mut c = ctx(f.table);
        let (src, dst) = (
            dir.join(format!("{}_{}.xlsx", std::process::id(), f.table)),
            dir.join(format!("{}_{}_del.xlsx", std::process::id(), f.table)),
        );
        let r = (|| -> Probe {
            xlsx::export_xlsx_families(&c.ed.model, &src, u, &fams)?;
            if !xlsx::sheet_ids(&src, fam.sheet)?.contains(&c.id) {
                return Ok(());
            }
            if let xlsx::RowDelete::Refused(why) = fam.delete {
                if why.is_empty() {
                    return Err("empty refusal".into());
                }
                return Ok(());
            }
            xlsx::patch_cell(&src, &dst, fam.sheet, c.id, xlsx::DELETE_HEAD, 1.0)?;
            let rep = xlsx::import_xlsx_families(&mut c.ed.model, &dst, o, &fams)?;
            if !rep.rejected.is_empty() || rep.counts().deleted != 1 || rep.props.len() != 1 {
                return Err(format!("preview: {:?} {:?}", rep.rejected, rep.changes));
            }
            if !row_exists(&c.ed, f.table, c.id) {
                return Err("the preview already deleted the row".into());
            }
            c.ed.apply_xlsx_props(rep.batch(), rep.props.clone(), "probe")?;
            if row_exists(&c.ed, f.table, c.id) {
                return Err("the row is still there after apply".into());
            }
            c.ed.undo();
            if !row_exists(&c.ed, f.table, c.id) {
                return Err("one undo did not bring the row back".into());
            }
            Ok(())
        })();
        if let Err(e) = r {
            bad.push(format!("{}: {e}", f.table));
        }
    }
    assert!(bad.is_empty(), "{bad:#?}");
}
