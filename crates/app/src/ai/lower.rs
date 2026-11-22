// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Plan items in the op language: `lower_plan` turns checked plan items into one
//! `ddc_ops::Envelope` (one item = one group, plan slots stay slots as `Ref::New`), `lift` turns
//! such an envelope back into plan items. The plan path (scratch copy, diff, replay) is unchanged;
//! `lift` lets an envelope run through it until the envelope executor lands, and the tests use it
//! to prove the lowering loses nothing.
//!
//! Conventions where v1 has no typed slot for plan data (all named fields, SI units):
//! - type refs are `type:<id>`; the type table follows from the element or the target;
//! - `Set` carries the literal text the person wrote, `{"rel": "+", "by": "100"}` for a
//!   relative value, and `@units` (`metric`/`imperial`): the project units the text is in;
//! - a plan type is built from scratch: `type.duplicate` names the type table (or the family)
//!   in `from`; `cat`, `layers`, `shape`, `material`, `family` and `value:<param>` fields hold it;
//! - a plan opening's `along` is its start; the envelope places the centre (`along + width/2`);
//! - a room is `create` plus a `set` of its `height` on the new slot (the typed room has no
//!   height row), as the spreadsheet path sends it;
//! - roof, stair, curtain wall and shaft travel as `generic` with named fields.

use super::plan::{self, ElRef, IdOrNew, NewElem, NewType, PlanItem, PlanOp, PlanState, Targets};
use super::sets::{norm_cat, parse_set, Sets};
use crate::query::{self, Assign, Expr, Val};
use crate::units::Units;
use ddc_model::{LayerSpec, ParamSpec, ParamType, ParamValue, StairShape};
use ddc_ops::{
    Actor, Angle, Cat, Dir3, End, Envelope, Field, GridOp, Group, Len, NewEl, Op, ParamKind,
    ParamOp, Place, Ref, Side, TypeOp, Unit, Value, Xform, P2, P3, V3,
};
use super::modify_plan::{ModifyAct, ToolAct};
use ddc_ops::LevelOp;
use serde_json::{json, Value as J};
use std::collections::{HashMap, HashSet};

type OpTargets = ddc_ops::Targets;

/// Reserved `Set` field: the project units of the literal values.
pub const UNITS_FIELD: &str = "@units";
const VALUE_PREFIX: &str = "value:";

/// A plan item the envelope does not carry, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub item: u32,
    pub kind: &'static str,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Lowered {
    pub envelope: Envelope,
    pub refused: Vec<Refusal>,
}

/// A plan item read back from an envelope group.
#[derive(Clone, Debug)]
pub struct LiftedItem {
    pub item: PlanItem,
    pub units: Option<Units>,
}

/// Stable kind name of a plan op; exhaustive so a new variant must decide how it lowers.
pub fn op_kind(op: &PlanOp) -> &'static str {
    match op {
        PlanOp::Assign { .. } => "assign",
        PlanOp::ReplaceType { .. } => "replace_type",
        PlanOp::Create { .. } => "create",
        PlanOp::Delete { .. } => "delete",
        PlanOp::NewType(_) => "new_type",
        PlanOp::NewParam(_) => "new_param",
        PlanOp::LoadFamily { .. } => "load_family",
        PlanOp::DefineFamily { .. } => "define_family",
        PlanOp::Geometry { .. } => "geometry",
        PlanOp::Script { .. } => "script",
        PlanOp::Generate { .. } => "generate",
        PlanOp::Fix(_) => "fix",
        PlanOp::WallType(_) => "wall_type",
        PlanOp::Mep(_) => "mep",
        PlanOp::Project(_) => "project",
        PlanOp::Plant(_) => "plant",
        PlanOp::DrawingSet(..) => "drawing_set",
        PlanOp::Modify { .. } => "modify",
        PlanOp::Tool(_) => "tool",
    }
}

/// The v1 level ops of a level edit; the other tool acts have no v1 form yet.
fn lower_tool(act: &ToolAct) -> Result<Vec<Op>, String> {
    let level = |id: i64| Ref::id(Cat::Level, id);
    Ok(match act {
        // a new level holds no plan slot, so a v1 create could not be referenced
        ToolAct::AddLevel { .. } => return Err("a new level has no v1 form yet".into()),
        ToolAct::EditLevel { id, name, elevation } => {
            let mut ops = vec![];
            if let Some(n) = name {
                ops.push(Op::Level(LevelOp::Rename { target: level(*id), name: n.clone() }));
            }
            if let Some(z) = elevation {
                let elevation = Len::m(*z);
                ops.push(Op::Level(LevelOp::SetElevation { target: level(*id), elevation }));
            }
            ops
        }
        ToolAct::DeleteLevel { .. } => return Err("deleting a level has no v1 form yet".into()),
        ToolAct::PlaceRooms { .. } => return Err("automatic rooms have no v1 form yet".into()),
        ToolAct::FootingsUnderColumns { .. } => {
            return Err("footings under columns have no v1 form yet".into())
        }
        ToolAct::Terrain(_) => return Err("a terrain has no v1 form yet".into()),
        ToolAct::SetMaterial { .. } => return Err("a material look has no v1 form yet".into()),
        ToolAct::Measure { .. } => return Err("a saved measurement has no v1 form yet".into()),
        ToolAct::MassEdit { .. } => return Err("a mass history step has no v1 form yet".into()),
        ToolAct::TimelineFamily { .. } => return Err("a timeline family has no v1 form yet".into()),
    })
}

/// The v1 transform of a modify act; the acts without one name themselves.
fn lower_modify(target: OpTargets, act: &ModifyAct) -> Result<Vec<Op>, String> {
    let p3 = |p: (f64, f64)| P3::m(p.0, p.1, 0.0);
    let (xf, copy) = match act {
        ModifyAct::Move { by } => (Xform::Move { by: V3::m(by.0, by.1, by.2) }, false),
        ModifyAct::Copy { by } if by.len() == 1 => {
            (Xform::Move { by: V3::m(by[0].0, by[0].1, by[0].2) }, true)
        }
        ModifyAct::Rotate { center, deg, copy } => (
            Xform::Rotate { center: p3(*center), axis: Dir3::Z, angle: Angle::deg(*deg) },
            *copy,
        ),
        ModifyAct::Mirror { a, b, copy } => (Xform::Mirror { a: p3(*a), b: p3(*b) }, *copy),
        other => return Err(format!("{} has no v1 form yet", other.ui_command())),
    };
    Ok(vec![Op::Transform { target, xf, copy }])
}

/// Checked items in plan order. An item that cannot be lowered, or that depends on an item
/// missing from the envelope, is refused with its reason; the rest still lowers.
pub fn lower_plan(st: &PlanState, actor: Actor, title: &str) -> Lowered {
    lower_plan_in(st, None, actor, title)
}

/// `lower_plan` with the session's frozen sets: a set target `s<n>` lowers to the refs it
/// froze, so the executor never sees a set name.
pub fn lower_plan_in(st: &PlanState, sets: Option<&Sets>, actor: Actor, title: &str) -> Lowered {
    let mut envelope = Envelope::new(actor, title);
    let mut refused = vec![];
    let mut emitted: HashSet<u32> = HashSet::new();
    for it in st.items.iter().filter(|i| i.checked) {
        let kind = op_kind(&it.op);
        if let Some(d) = it.depends_on.iter().find(|d| !emitted.contains(d)) {
            refused.push(Refusal {
                item: it.id,
                kind,
                reason: format!("depends on item {d}, which is not in the envelope"),
            });
            continue;
        }
        match lower_op_in(&it.op, st.units.get(&it.id).copied(), sets) {
            Ok(ops) => {
                emitted.insert(it.id);
                envelope.groups.push(Group {
                    id: it.id,
                    title: it.title.clone(),
                    ops,
                    depends_on: it.depends_on.clone(),
                    why: it.why.clone(),
                });
            }
            Err(reason) => refused.push(Refusal {
                item: it.id,
                kind,
                reason,
            }),
        }
    }
    Lowered { envelope, refused }
}

/// One plan op as ops; `Err` is the reason it has no v1 form.
pub fn lower_op(op: &PlanOp, units: Option<Units>) -> Result<Vec<Op>, String> {
    lower_op_in(op, units, None)
}

/// `lower_op` with frozen sets (see `lower_plan_in`).
pub fn lower_op_in(
    op: &PlanOp,
    units: Option<Units>,
    sets: Option<&Sets>,
) -> Result<Vec<Op>, String> {
    Ok(match op {
        PlanOp::Assign { target, set } => {
            let mut fields = Vec::with_capacity(set.len() + 1);
            if let Some(u) = units {
                fields.push(text(UNITS_FIELD, units_word(u)));
            }
            for a in set {
                let v = match &a.val {
                    Val::Lit(s) => Value::text(s),
                    Val::Rel(c, by) => Value::plain(json!({ "rel": c.to_string(), "by": by })),
                };
                fields.push(Field::new(&a.key, v));
            }
            vec![Op::Set {
                target: lower_targets(target, sets)?,
                fields,
            }]
        }
        PlanOp::ReplaceType { target, to } => vec![Op::SetType {
            target: lower_targets(target, sets)?,
            to: lower_ref(to)?,
        }],
        PlanOp::Create { slot0, items } => {
            let mut ops = Vec::with_capacity(items.len());
            for (i, e) in items.iter().enumerate() {
                let slot = slot0 + i as u32;
                ops.push(lower_elem(slot, e)?);
                if let NewElem::Room { height, .. } = e {
                    ops.push(room_height(slot, *height));
                }
            }
            ops
        }
        PlanOp::Delete { target } => vec![Op::Delete {
            target: lower_targets(target, sets)?,
        }],
        PlanOp::NewType(t) => vec![lower_type(t)?],
        PlanOp::NewParam(p) => vec![lower_param(p)?],
        PlanOp::LoadFamily { .. } => {
            return Err("family text has no v1 form: family.define takes structured \
                        parameters, planes and forms"
                .into())
        }
        PlanOp::DefineFamily { .. } => {
            return Err("a defined family keeps its formulas: it travels as the structured \
                        definition, not as family.define"
                .into())
        }
        PlanOp::Geometry { slot0, ops } => super::geo_tools::geo_ops(*slot0, ops)?,
        PlanOp::Script { slot0, item } => super::script_tools::ops_at(*slot0, item)?,
        PlanOp::Generate { .. } => {
            return Err("a generator runs when the plan compiles: its output is lowered \
                        once a variant is chosen"
                .into())
        }
        PlanOp::Fix(_) => {
            return Err("a fix is taken again from a re-run of its rule: lower the edits \
                        it produced"
                .into())
        }
        PlanOp::WallType(_) => {
            return Err("a wall type layer change or type copy has no v1 form yet".into())
        }
        PlanOp::Mep(_) => {
            return Err("a routed network is planned when the plan compiles: lower the \
                        runs it produced"
                .into())
        }
        PlanOp::Project(_) => {
            return Err("a project is built when the plan compiles: lower the elements \
                        it produced"
                .into())
        }
        PlanOp::Plant(_) => return Err("a plant is built when the plan compiles: lower the elements it produced".into()),
        PlanOp::DrawingSet(..) => return Err("a drawing set has no v1 form yet".into()),
        PlanOp::Modify { target, act } => lower_modify(lower_targets(target, sets)?, act)?,
        PlanOp::Tool(act) => lower_tool(act)?,
    })
}

/// The inverse of `lower_plan` for what it produces; anything else is an error naming the op.
pub fn lift(env: &Envelope) -> Result<Vec<LiftedItem>, String> {
    let mut slots: HashMap<u32, String> = HashMap::new();
    let mut out = Vec::with_capacity(env.groups.len());
    for g in &env.groups {
        let (op, units) = lift_group(g, &mut slots).map_err(|e| format!("group {}: {e}", g.id))?;
        out.push(LiftedItem {
            item: PlanItem {
                id: g.id,
                title: g.title.clone(),
                why: g.why.clone(),
                op,
                checked: true,
                depends_on: g.depends_on.clone(),
                actor: "ops".into(),
                conflict: None,
                sets: Default::default(),
            },
            units,
        });
    }
    Ok(out)
}

// ---- lowering ----

fn units_word(u: Units) -> &'static str {
    match u {
        Units::Metric => "metric",
        Units::Imperial => "imperial",
    }
}

fn lower_targets(t: &Targets, sets: Option<&Sets>) -> Result<OpTargets, String> {
    Ok(match t {
        Targets::Filter(q) => OpTargets::Filter {
            cat: None,
            query: q.clone(),
        },
        Targets::Set(n) => match sets {
            None => OpTargets::Set {
                name: format!("s{n}"),
            },
            Some(sets) => {
                let s = sets.get(*n).ok_or_else(|| format!("set s{n} is gone"))?;
                let refs = super::freeze::key_refs(&s.keys, None)?;
                if refs.is_empty() {
                    return Err(format!("set s{n} is empty"));
                }
                OpTargets::Refs { refs }
            }
        },
        Targets::Refs(refs) => OpTargets::Refs {
            refs: refs.iter().map(lower_ref).collect::<Result<_, _>>()?,
        },
        Targets::Selection => return Err("selection was not frozen".into()),
    })
}

fn lower_ref(r: &ElRef) -> Result<Ref, String> {
    Ok(match r.id {
        IdOrNew::New(slot) => Ref::New { slot },
        IdOrNew::Id(id) => Ref::Id {
            cat: cat_of(&r.cat)?,
            id,
        },
    })
}

/// Category of a plan reference word (`door`, `slab`, `wall_type`, ...).
pub(crate) fn cat_of(word: &str) -> Result<Cat, String> {
    if let Ok(table) = plan::type_table(word) {
        return Ok(type_cat(table));
    }
    let w = norm_cat(word).unwrap_or(word.trim());
    Ok(match w {
        "wall" => Cat::Wall,
        "door" => Cat::Door,
        "window" => Cat::Window,
        "opening" => Cat::Opening,
        "curtain" => Cat::CurtainWall,
        "floor" => Cat::Slab,
        "room" => Cat::Room,
        "column" => Cat::Column,
        "beam" => Cat::Beam,
        "roof" => Cat::Roof,
        "ceiling" => Cat::Ceiling,
        "stair" => Cat::Stair,
        "railing" => Cat::Railing,
        "mep" => Cat::MepRun,
        "comp" => Cat::Component,
        "shaft" => Cat::Shaft,
        "grid" => Cat::Grid,
        "level" => Cat::Level,
        "imported" => Cat::Mesh,
        _ => return Err(format!("unknown category: {}", super::fmt::text(word))),
    })
}

fn level(id: i64) -> Ref {
    Ref::Id {
        cat: Cat::Level,
        id,
    }
}

fn at2(p: (f64, f64)) -> Place {
    Place::At {
        p: P3::m(p.0, p.1, 0.0),
    }
}

fn at3(p: (f64, f64, f64)) -> Place {
    Place::At {
        p: P3::m(p.0, p.1, p.2),
    }
}

fn len(n: &str, v: f64) -> Field {
    Field::new(n, Value::num(v, Unit::M))
}

fn ang(n: &str, v: f64) -> Field {
    Field::new(n, Value::num(v, Unit::Rad))
}

fn text(n: &str, s: &str) -> Field {
    Field::new(n, Value::text(s))
}

fn plain(n: &str, v: J) -> Field {
    Field::new(n, Value::plain(v))
}

/// Coordinates in metres as a JSON array.
fn coords(n: &str, v: J) -> Field {
    Field::new(
        n,
        Value {
            v,
            unit: Some(Unit::M),
        },
    )
}

fn ring_json(r: &[(f64, f64)]) -> J {
    J::Array(r.iter().map(|p| json!([p.0, p.1])).collect())
}

fn rings_json(r: &[Vec<(f64, f64)>]) -> J {
    J::Array(r.iter().map(|x| ring_json(x)).collect())
}

/// The height of a room made in the same group: the typed room carries none.
fn room_height(slot: u32, h: f64) -> Op {
    Op::Set {
        target: OpTargets::Refs {
            refs: vec![Ref::New { slot }],
        },
        fields: vec![len("height", h)],
    }
}

fn opt_ty(r: &Option<ElRef>) -> Result<Option<Ref>, String> {
    r.as_ref().map(lower_ref).transpose()
}

fn lower_elem(slot: u32, e: &NewElem) -> Result<Op, String> {
    let el = match e {
        NewElem::Wall {
            level: l,
            a,
            b,
            bulge,
            base_offset,
            height,
            wtype,
        } => {
            let mut fields = vec![len("base_offset", *base_offset)];
            if *bulge != 0.0 {
                fields.push(plain("bulge", serde_json::json!(*bulge)));
            }
            NewEl::Wall {
                level: level(*l),
                a: at2(*a),
                b: at2(*b),
                height: Len::m(*height),
                top: None,
                ty: opt_ty(wtype)?,
                fields,
            }
        }
        NewElem::Opening {
            wall,
            along,
            width,
            height,
            sill,
            kind,
        } => {
            let host = lower_ref(wall)?;
            let at = Place::OnHost {
                host: host.clone(),
                along: Len::m(*along + *width / 2.0),
                from_end: End::A,
                side: Side::Center,
                z: Len::m(*sill),
            };
            let size = vec![len("width", *width), len("height", *height)];
            match kind.as_str() {
                "door" => NewEl::Door {
                    host,
                    at,
                    ty: None,
                    fields: size,
                },
                "window" => NewEl::Window {
                    host,
                    at,
                    sill: Len::m(*sill),
                    ty: None,
                    fields: size,
                },
                "void" => NewEl::Opening {
                    host,
                    at,
                    w: Len::m(*width),
                    h: Len::m(*height),
                    sill: Len::m(*sill),
                },
                other => {
                    return Err(format!(
                        "opening kind {} has no v1 element",
                        super::fmt::text(other)
                    ))
                }
            }
        }
        NewElem::Slab {
            level: l,
            stype,
            offset,
            outer,
            holes,
        } => NewEl::Slab {
            level: level(*l),
            boundary: outer.iter().map(|p| at2(*p)).collect(),
            offset: Len::m(*offset),
            ty: opt_ty(stype)?,
            fields: vec![coords("holes", rings_json(holes))],
        },
        NewElem::Ceiling {
            level: l,
            stype,
            height,
            outer,
            holes,
        } => NewEl::Ceiling {
            level: level(*l),
            boundary: outer.iter().map(|p| at2(*p)).collect(),
            height: Len::m(*height),
            ty: opt_ty(stype)?,
            fields: vec![coords("holes", rings_json(holes))],
        },
        NewElem::Beam {
            level: l,
            mtype,
            a,
            b,
        } => NewEl::Beam {
            level: level(*l),
            a: at3(*a),
            b: at3(*b),
            ty: opt_ty(mtype)?,
            fields: vec![],
        },
        NewElem::Comp {
            level: l,
            ctype,
            at,
            rotation,
            host,
            scale,
            min_clear,
        } => {
            let t = lower_ref(ctype)?;
            NewEl::Component {
                level: level(*l),
                at: at3(*at),
                family: Ref::part(t.clone(), "family"),
                host: opt_ty(host)?,
                rotation: Angle::rad(*rotation),
                ty: Some(t),
                fields: [("scale", *scale, 1.0), ("min_clear", *min_clear, 0.0)]
                    .into_iter()
                    .filter(|f| f.1 != f.2)
                    .map(|f| Field::new(f.0, Value::plain(json!(f.1))))
                    .collect(),
            }
        }
        NewElem::Pipe {
            system,
            level: l,
            material,
            dn,
            points,
            section,
        } => NewEl::MepRun {
            level: level(*l),
            system: system.text(),
            path: points.iter().map(|p| at3(*p)).collect(),
            size: match section {
                Some(s) => Len::mm(s.w * 1000.0),
                None => Len::mm(f64::from(*dn)),
            },
            ty: None,
            fields: match section {
                Some(s) => vec![
                    text("domain", s.domain.name()),
                    text("height_mm", &format!("{}", s.h * 1000.0)),
                ],
                None => vec![text("material", material)],
            },
        },
        NewElem::Room {
            level: l,
            name,
            number,
            point,
            // the height goes in the `set` that follows the create
            height: _,
        } => NewEl::Room {
            level: level(*l),
            at: at2(*point),
            name: name.clone(),
            number: (!number.is_empty()).then(|| number.clone()),
        },
        NewElem::Column {
            level: l,
            top_level,
            mtype,
            at,
            base_offset,
            top_offset,
            rotation,
            structural,
            top_at,
        } => NewEl::Column {
            level: level(*l),
            at: at2(*at),
            top: top_level.map(level),
            // 0: the top follows from the top level (or the base level) and `top_offset`
            height: Len::m(0.0),
            rotation: Angle::rad(*rotation),
            ty: opt_ty(mtype)?,
            fields: [
                len("base_offset", *base_offset),
                len("top_offset", *top_offset),
                plain("structural", json!(structural)),
            ]
            .into_iter()
            // the executor's slanted-column fields (`ops_create::COLUMN_FIELDS`)
            .chain(top_at.into_iter().flat_map(|p| [len("top_at_x", p.0), len("top_at_y", p.1)]))
            .collect(),
        },
        NewElem::Roof {
            level: l,
            stype,
            offset,
            overhang,
            ring,
            slope,
            edges,
            vault,
            eave,
        } => NewEl::Generic {
            cat: Cat::Roof,
            level: Some(level(*l)),
            at: ring.iter().map(|p| at2(*p)).collect(),
            ty: opt_ty(stype)?,
            fields: {
                let mut v = vec![
                    len("offset", *offset),
                    len("overhang", *overhang),
                    match slope {
                        Some(s) => Field::new("slope", Value::num(*s, Unit::Deg)),
                        None => plain("slope", J::Null),
                    },
                ];
                if !edges.is_empty() {
                    v.push(plain("edge_slopes", json!(edges)));
                }
                if let Some(vt) = vault {
                    let pts: Vec<[f64; 2]> = vt.profile.iter().map(|p| [p.0, p.1]).collect();
                    let body = json!({"a": [vt.a.0, vt.a.1], "b": [vt.b.0, vt.b.1], "profile": pts, "depth": vt.depth});
                    v.push(plain("vault", body));
                }
                if let Some((a, b)) = eave {
                    v.push(plain("eave", json!([[a.0, a.1], [b.0, b.1]])));
                }
                v
            },
        },
        NewElem::Stair {
            level: l,
            top_level,
            origin,
            angle,
        } => NewEl::Generic {
            cat: Cat::Stair,
            level: Some(level(*l)),
            at: vec![at2(*origin)],
            ty: None,
            fields: vec![plain("top_level", json!(top_level)), ang("angle", *angle)],
        },
        NewElem::GenStair {
            level: l,
            top_level,
            top_offset,
            origin,
            angle,
            shape,
            width,
            max_riser,
            landing,
        } => {
            let mut fields = vec![
                plain("top_level", json!(top_level)),
                len("top_offset", *top_offset),
                ang("angle", *angle),
                len("width", *width),
                len("max_riser", *max_riser),
                len("landing", *landing),
            ];
            let (shape, gap) = shape_json(shape);
            fields.push(plain("shape", shape));
            if let Some(g) = gap {
                fields.push(len("gap", g));
            }
            NewEl::Generic {
                cat: Cat::Stair,
                level: Some(level(*l)),
                at: vec![at2(*origin)],
                ty: None,
                fields,
            }
        }
        NewElem::Curtain {
            level: l,
            name,
            a,
            b,
            base_offset,
            height,
            spacing,
            mullion,
            spandrel,
        } => NewEl::Generic {
            cat: Cat::CurtainWall,
            level: Some(level(*l)),
            at: vec![at2(*a), at2(*b)],
            ty: None,
            fields: vec![
                text("name", name),
                len("base_offset", *base_offset),
                len("height", *height),
                len("spacing_u", spacing.0),
                len("spacing_v", spacing.1),
                len("mullion_w", mullion.0),
                len("mullion_d", mullion.1),
                len("spandrel", *spandrel),
            ],
        },
        NewElem::Shaft {
            level: l,
            top_level,
            top_offset,
            ring,
        } => NewEl::Generic {
            cat: Cat::Shaft,
            level: Some(level(*l)),
            at: ring.iter().map(|p| at2(*p)).collect(),
            ty: None,
            fields: vec![plain("top_level", json!(top_level)), len("top_offset", *top_offset)],
        },
        NewElem::Grid { name, a, b } => {
            return Ok(Op::Grid(GridOp::Create {
                slot,
                name: name.clone(),
                a: P2::m(a.0, a.1),
                b: P2::m(b.0, b.1),
                bulge: 0.0,
            }))
        }
        NewElem::SteelConn { .. } => {
            return Err("steel connections run as model steps, not as edit ops".into())
        }
        NewElem::Railing { level: l, path, stair, height } => NewEl::Railing {
            level: level(*l),
            path: path.iter().map(|p| at3(*p)).collect(),
            host: stair.as_ref().map(|(r, _)| lower_ref(r)).transpose()?,
            height: Len::m(*height),
            ty: None,
            fields: stair.iter().map(|(_, s)| plain("side", json!(s))).collect(),
        },
    };
    Ok(Op::Create { slot, el })
}

fn shape_json(s: &StairShape) -> (J, Option<f64>) {
    match s {
        StairShape::Straight => (json!({ "kind": "straight" }), None),
        StairShape::Spiral { radius, core, turn } => (
            json!({ "kind": "spiral", "radius": radius, "core": core, "turn": turn }),
            None,
        ),
        StairShape::L { left, first } => (
            json!({ "kind": "l", "left": left, "first": first }),
            None,
        ),
        StairShape::U { left, first, gap } => (
            json!({ "kind": "u", "left": left, "first": first }),
            Some(*gap),
        ),
    }
}

/// Type table a new plan type lands in (as `compile_type` decides it).
fn new_type_table(cat: &str) -> Option<&'static str> {
    Some(match norm_cat(cat)? {
        "wall" => "wall_type",
        "floor" | "roof" | "ceiling" => "slab_type",
        "column" | "beam" => "member_type",
        "comp" => "comp_type",
        _ => return None,
    })
}

fn lower_type(t: &NewType) -> Result<Op, String> {
    let table = new_type_table(&t.cat)
        .ok_or_else(|| format!("{} has no types", super::fmt::text(&t.cat)))?;
    let from = match &t.family {
        Some(f) => Ref::name(Cat::Family, f),
        None => Ref::name(type_cat(table), table),
    };
    let layers: Vec<J> = t
        .layers
        .iter()
        .map(|l| json!({ "material": l.material, "thickness": l.thickness }))
        .collect();
    let mut fields = vec![
        text("cat", &t.cat),
        coords("layers", J::Array(layers)),
        text("material", &t.material),
    ];
    if let Some(s) = &t.shape {
        fields.push(text("shape", s));
    }
    if let Some(f) = &t.family {
        fields.push(text("family", f));
    }
    for (name, v) in &t.values {
        let v = match v {
            ParamValue::Num(x) => Value::plain(json!(x)),
            ParamValue::Text(s) => Value::text(s),
        };
        fields.push(Field::new(&format!("{VALUE_PREFIX}{name}"), v));
    }
    Ok(Op::Type(TypeOp::Duplicate {
        slot: t.slot,
        from,
        name: t.name.clone(),
        fields,
    }))
}

/// Parameter category words (`params::CATEGORIES`) and their wire categories.
const PARAM_CATS: [(&str, Cat); 16] = [
    ("wall", Cat::Wall),
    ("door", Cat::Door),
    ("window", Cat::Window),
    ("curtain_wall", Cat::CurtainWall),
    ("slab", Cat::Slab),
    ("room", Cat::Room),
    ("col", Cat::Column),
    ("beam", Cat::Beam),
    ("roof", Cat::Roof),
    ("ceiling", Cat::Ceiling),
    ("stair", Cat::Stair),
    ("railing", Cat::Railing),
    ("mep_run", Cat::MepRun),
    ("comp", Cat::Component),
    ("shaft", Cat::Shaft),
    ("ramp", Cat::Ramp),
];

fn lower_param(p: &ParamSpec) -> Result<Op, String> {
    if p.guid.is_some() {
        return Err("a shared parameter guid has no v1 field".into());
    }
    if !p.unit.trim().is_empty() {
        return Err(format!(
            "unit label {} has no v1 field",
            super::fmt::text(&p.unit)
        ));
    }
    let (kind, unit) = match p.ptype {
        ParamType::Text => (ParamKind::Text, None),
        ParamType::Int => (ParamKind::Integer, None),
        ParamType::Number => (ParamKind::Number, None),
        ParamType::Length => (ParamKind::Length, Some(Unit::M)),
        ParamType::Area => (ParamKind::Area, Some(Unit::M2)),
        ParamType::Volume => (ParamKind::Volume, Some(Unit::M3)),
        ParamType::YesNo => (ParamKind::Bool, None),
    };
    let mut cats = Vec::with_capacity(p.categories.len());
    for c in &p.categories {
        let cat = PARAM_CATS
            .iter()
            .find(|(w, _)| w == c)
            .map(|(_, cat)| *cat)
            .ok_or_else(|| format!("unknown parameter category: {}", super::fmt::text(c)))?;
        cats.push(cat);
    }
    Ok(Op::Param(ParamOp::Define {
        name: p.name.clone(),
        kind,
        unit,
        cats,
        instance: !p.per_type,
        group: if p.group.is_empty() {
            None
        } else {
            Some(p.group.clone())
        },
    }))
}

// ---- lifting ----

fn lift_group(
    g: &Group,
    slots: &mut HashMap<u32, String>,
) -> Result<(PlanOp, Option<Units>), String> {
    let is_create = |o: &Op| matches!(o, Op::Create { .. } | Op::Grid(GridOp::Create { .. }));
    let creates = g.ops.first().is_some_and(is_create)
        && g.ops
            .iter()
            .all(|o| is_create(o) || lifted_room_height(o).is_some());
    if creates {
        let mut items: Vec<NewElem> = Vec::with_capacity(g.ops.len());
        let mut slot0 = 0;
        for o in &g.ops {
            if let Some((slot, h)) = lifted_room_height(o) {
                // only right after the room it sizes
                let last = (slot0 + items.len() as u32).checked_sub(1);
                match items.last_mut() {
                    Some(NewElem::Room { height, .. }) if Some(slot) == last => {
                        *height = h?;
                    }
                    _ => return Err(format!("the height of new:{slot} follows no room create")),
                }
                continue;
            }
            let (slot, e) = lift_elem(o, slots)?;
            if items.is_empty() {
                slot0 = slot;
            } else if slot != slot0 + items.len() as u32 {
                return Err(format!("slot {slot} does not follow slot {slot0}"));
            }
            items.push(e);
        }
        for (i, e) in items.iter().enumerate() {
            slots.insert(slot0 + i as u32, e.cat().to_string());
        }
        return Ok((PlanOp::Create { slot0, items }, None));
    }
    let [op] = g.ops.as_slice() else {
        return Err(format!("{} ops make no single plan item", g.ops.len()));
    };
    Ok(match op {
        Op::Set { target, fields } => {
            let mut units = None;
            let mut set = Vec::with_capacity(fields.len());
            for f in fields {
                if f.name == UNITS_FIELD {
                    units = Some(match f.value.v.as_str() {
                        Some("metric") => Units::Metric,
                        Some("imperial") => Units::Imperial,
                        _ => return Err(format!("{UNITS_FIELD} is metric or imperial")),
                    });
                    continue;
                }
                let val = match &f.value.v {
                    J::String(s) => Val::Lit(s.clone()),
                    J::Object(o) => {
                        let rel = o.get("rel").and_then(J::as_str).and_then(|s| {
                            let mut c = s.chars();
                            c.next().filter(|_| c.next().is_none())
                        });
                        let by = o.get("by").and_then(J::as_str);
                        match (rel, by) {
                            (Some(c), Some(by)) => Val::Rel(c, by.to_string()),
                            _ => return Err(format!("field {}: bad relative value", f.name)),
                        }
                    }
                    J::Number(n) => Val::Lit(n.to_string()),
                    _ => return Err(format!("field {}: text or relative value expected", f.name)),
                };
                set.push(Assign {
                    key: f.name.clone(),
                    val,
                });
            }
            (
                PlanOp::Assign {
                    target: lift_targets(target, slots)?,
                    set,
                },
                units,
            )
        }
        Op::SetType { target, to } => {
            let table = target_type_table(target, slots);
            (
                PlanOp::ReplaceType {
                    target: lift_targets(target, slots)?,
                    to: lift_type_ref(to, table, slots)?,
                },
                None,
            )
        }
        Op::Delete { target } => (
            PlanOp::Delete {
                target: lift_targets(target, slots)?,
            },
            None,
        ),
        Op::Type(TypeOp::Duplicate {
            slot, name, fields, ..
        }) => {
            let t = lift_type(*slot, name, fields)?;
            let table = new_type_table(&t.cat)
                .ok_or_else(|| format!("{} has no types", t.cat))?;
            slots.insert(*slot, table.to_string());
            (PlanOp::NewType(t), None)
        }
        Op::Param(ParamOp::Define {
            name,
            kind,
            cats,
            instance,
            group,
            ..
        }) => (PlanOp::NewParam(lift_param(name, *kind, cats, *instance, group)?), None),
        other => return Err(format!("{} has no plan item yet", other.kind())),
    })
}

fn lift_targets(t: &OpTargets, slots: &HashMap<u32, String>) -> Result<Targets, String> {
    Ok(match t {
        OpTargets::Refs { refs } => Targets::Refs(
            refs.iter()
                .map(|r| lift_ref(r, slots))
                .collect::<Result<_, _>>()?,
        ),
        OpTargets::Set { name } => Targets::Set(
            parse_set(name).ok_or_else(|| format!("unknown set {}", super::fmt::text(name)))?,
        ),
        OpTargets::Filter { cat: None, query } => Targets::Filter(query.clone()),
        OpTargets::Filter { cat: Some(_), .. } => {
            return Err("a filter category goes into the query text".into())
        }
    })
}

/// Plan word of a wire category (what `norm_cat` returns).
fn word_of(cat: Cat) -> Result<&'static str, String> {
    Ok(match cat {
        Cat::Wall => "wall",
        Cat::Door => "door",
        Cat::Window => "window",
        Cat::Opening => "opening",
        Cat::CurtainWall => "curtain",
        Cat::Slab => "floor",
        Cat::Room => "room",
        Cat::Column => "column",
        Cat::Beam => "beam",
        Cat::Roof => "roof",
        Cat::Ceiling => "ceiling",
        Cat::Stair => "stair",
        Cat::Railing => "railing",
        Cat::MepRun => "mep",
        Cat::Component => "comp",
        Cat::Shaft => "shaft",
        Cat::Grid => "grid",
        Cat::Level => "level",
        Cat::Mesh => "imported",
        other => return Err(format!("category {} has no plan reference", other.key())),
    })
}

fn lift_ref(r: &Ref, slots: &HashMap<u32, String>) -> Result<ElRef, String> {
    match r {
        Ref::New { slot } => slots
            .get(slot)
            .map(|w| ElRef::new(w, *slot))
            .ok_or_else(|| format!("no earlier op creates slot {slot}")),
        Ref::Id { cat: Cat::Type, .. } => Err(format!("{}: type table is unknown here", r.label())),
        Ref::Id { cat, id } if cat_table(*cat).is_some() => Ok(ElRef {
            cat: cat_table(*cat).unwrap_or_default().to_string(),
            id: IdOrNew::Id(*id),
        }),
        Ref::Id { cat, id } => Ok(ElRef {
            cat: word_of(*cat)?.to_string(),
            id: IdOrNew::Id(*id),
        }),
        other => Err(format!("{} has no plan reference", other.label())),
    }
}

/// A type ref; `table` is the type table the context implies for `type:<id>`.
fn lift_type_ref(
    r: &Ref,
    table: Option<&'static str>,
    slots: &HashMap<u32, String>,
) -> Result<ElRef, String> {
    match r {
        Ref::New { .. } => lift_ref(r, slots),
        Ref::Id { cat, id } if *cat == Cat::Type || cat_table(*cat).is_some() => {
            // a typed ref names its table; a bare v1 `type` takes the one the context implies
            let table = cat_table(*cat)
                .or(table)
                .ok_or_else(|| format!("{}: type table is unknown here", r.label()))?;
            Ok(ElRef {
                cat: table.to_string(),
                id: IdOrNew::Id(*id),
            })
        }
        other => Err(format!("{} is not a type", other.label())),
    }
}

/// Wire category of a type table; tables without their own cat (`comp_type`) stay bare `type`.
fn type_cat(table: &str) -> Cat {
    match table {
        "wall_type" => Cat::WallType,
        "slab_type" => Cat::SlabType,
        "member_type" => Cat::MemberType,
        _ => Cat::Type,
    }
}

/// Type table of a typed wire category (`None` for bare `type` and element cats).
fn cat_table(cat: Cat) -> Option<&'static str> {
    Some(match cat {
        Cat::WallType => "wall_type",
        Cat::SlabType => "slab_type",
        Cat::MemberType => "member_type",
        _ => return None,
    })
}

fn type_table_of_word(w: &str) -> Option<&'static str> {
    Some(match w {
        "wall" => "wall_type",
        "floor" | "slab" | "roof" | "ceiling" => "slab_type",
        "column" | "col" | "beam" => "member_type",
        "comp" => "comp_type",
        _ => return None,
    })
}

fn expr_cat(e: &Expr) -> Option<&'static str> {
    match e {
        Expr::Cat(c) => Some(*c),
        Expr::And(v) => v.iter().find_map(expr_cat),
        _ => None,
    }
}

/// Type table implied by what a `set_type` targets.
fn target_type_table(t: &OpTargets, slots: &HashMap<u32, String>) -> Option<&'static str> {
    match t {
        OpTargets::Refs { refs } => match refs.first()? {
            Ref::Id { cat, .. } => type_table_of_word(word_of(*cat).ok()?),
            Ref::New { slot } => type_table_of_word(slots.get(slot)?),
            _ => None,
        },
        OpTargets::Filter { query: q, .. } => {
            type_table_of_word(expr_cat(&query::parse(q).ok()?.expr)?)
        }
        OpTargets::Set { .. } => None,
    }
}

fn lift_level(r: &Ref) -> Result<i64, String> {
    match r {
        Ref::Id {
            cat: Cat::Level,
            id,
        } => Ok(*id),
        other => Err(format!("{} is not a level id", other.label())),
    }
}

fn opt_level(r: &Option<Ref>) -> Result<i64, String> {
    lift_level(r.as_ref().ok_or("a level is required")?)
}

fn xyz(p: &Place) -> Result<(f64, f64, f64), String> {
    match p {
        Place::At { p } => {
            let [x, y, z] = p.to_m();
            Ok((x, y, z))
        }
        _ => Err("only a point place has a plan form".into()),
    }
}

fn xy(p: &Place) -> Result<(f64, f64), String> {
    xyz(p).map(|(x, y, _)| (x, y))
}

fn ring(ps: &[Place]) -> Result<Vec<(f64, f64)>, String> {
    ps.iter().map(xy).collect()
}

fn nth(ps: &[Place], i: usize) -> Result<(f64, f64), String> {
    xy(ps.get(i).ok_or_else(|| format!("point {i} is missing"))?)
}

/// Room height of a lifted room create with no `set` after it (the executor default).
const ROOM_HEIGHT: f64 = 3.0;

/// Plan start of an opening from the envelope centre; rounded to 1e-9 m so a centre shift
/// and back gives the plan value, not one off by the last bit.
fn start_of(centre: f64, width: f64) -> f64 {
    ((centre - width / 2.0) * 1e9).round() / 1e9
}

/// The `height` a room create's `set` gives that slot, if `op` is that set.
fn lifted_room_height(op: &Op) -> Option<(u32, Result<f64, String>)> {
    let Op::Set {
        target: OpTargets::Refs { refs },
        fields,
    } = op
    else {
        return None;
    };
    let ([Ref::New { slot }], [f]) = (refs.as_slice(), fields.as_slice()) else {
        return None;
    };
    (f.name == "height").then(|| (*slot, Fs(fields).num("height")))
}

/// Along and height of a hosted place.
fn on_host(p: &Place) -> Result<(f64, f64), String> {
    match p {
        Place::OnHost {
            along,
            from_end: End::A,
            side: Side::Center,
            z,
            ..
        } => Ok((along.to_m(), z.to_m())),
        _ => Err("an opening is placed on its host from end a".into()),
    }
}

/// Named fields of an op.
struct Fs<'a>(&'a [Field]);

impl<'a> Fs<'a> {
    fn find(&self, n: &str) -> Option<&'a Value> {
        self.0.iter().find(|f| f.name == n).map(|f| &f.value)
    }
    fn get(&self, n: &str) -> Result<&'a Value, String> {
        self.find(n).ok_or_else(|| format!("field {n} is missing"))
    }
    fn num(&self, n: &str) -> Result<f64, String> {
        self.get(n)?
            .si()
            .ok_or_else(|| format!("field {n} is not a number"))
    }
    fn num_or(&self, n: &str, d: f64) -> Result<f64, String> {
        match self.find(n) {
            None => Ok(d),
            Some(_) => self.num(n),
        }
    }
    fn text(&self, n: &str) -> Result<String, String> {
        self.get(n)?
            .v
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| format!("field {n} is not text"))
    }
    fn opt_text(&self, n: &str) -> Result<Option<String>, String> {
        match self.find(n) {
            None => Ok(None),
            Some(_) => self.text(n).map(Some),
        }
    }
    fn int(&self, n: &str) -> Result<i64, String> {
        self.get(n)?
            .v
            .as_i64()
            .ok_or_else(|| format!("field {n} is not an integer"))
    }
    fn opt_int(&self, n: &str) -> Result<Option<i64>, String> {
        match self.find(n) {
            None => Ok(None),
            Some(v) if v.v.is_null() => Ok(None),
            Some(_) => self.int(n).map(Some),
        }
    }
    fn flag(&self, n: &str) -> Result<bool, String> {
        self.get(n)?
            .v
            .as_bool()
            .ok_or_else(|| format!("field {n} is not yes or no"))
    }
    /// Factor to metres of a coordinate field.
    fn coords(&self, n: &str) -> Result<Option<(&'a J, f64)>, String> {
        let Some(v) = self.find(n) else {
            return Ok(None);
        };
        let k = match v.unit {
            Some(u) if u.dim() == ddc_ops::Dim::Length => u.si_factor(),
            None => return Err(format!("field {n} needs a length unit")),
            Some(_) => return Err(format!("field {n} is not in a length unit")),
        };
        Ok(Some((&v.v, k)))
    }
    /// Top point of a slanted column (`top_at_x`, `top_at_y`), both or neither.
    fn top_at(&self) -> Result<Option<(f64, f64)>, String> {
        match (self.find("top_at_x"), self.find("top_at_y")) {
            (None, None) => Ok(None),
            (Some(_), Some(_)) => Ok(Some((self.num("top_at_x")?, self.num("top_at_y")?))),
            _ => Err("a slanted column needs both top_at_x and top_at_y".into()),
        }
    }
    fn rings(&self, n: &str) -> Result<Vec<Vec<(f64, f64)>>, String> {
        let Some((v, k)) = self.coords(n)? else {
            return Ok(vec![]);
        };
        let bad = || format!("field {n} is not a list of rings");
        let mut out = vec![];
        for r in v.as_array().ok_or_else(bad)? {
            let mut ring = vec![];
            for p in r.as_array().ok_or_else(bad)? {
                let x = p.get(0).and_then(J::as_f64).ok_or_else(bad)?;
                let y = p.get(1).and_then(J::as_f64).ok_or_else(bad)?;
                ring.push((x * k, y * k));
            }
            out.push(ring);
        }
        Ok(out)
    }
}

fn lift_elem(op: &Op, slots: &HashMap<u32, String>) -> Result<(u32, NewElem), String> {
    let ty = |r: &Option<Ref>, table: &'static str| -> Result<Option<ElRef>, String> {
        r.as_ref()
            .map(|r| lift_type_ref(r, Some(table), slots))
            .transpose()
    };
    let (slot, el) = match op {
        Op::Grid(GridOp::Create {
            slot,
            name,
            a,
            b,
            bulge,
        }) => {
            if *bulge != 0.0 {
                return Err("an arc grid has no plan form".into());
            }
            let [ax, ay] = a.to_m();
            let [bx, by] = b.to_m();
            return Ok((
                *slot,
                NewElem::Grid {
                    name: name.clone(),
                    a: (ax, ay),
                    b: (bx, by),
                },
            ));
        }
        Op::Create { slot, el } => (*slot, el),
        other => return Err(format!("{} creates no plan element", other.kind())),
    };
    let e = match el {
        NewEl::Wall {
            level,
            a,
            b,
            height,
            top,
            ty: t,
            fields,
        } => {
            if top.is_some() {
                return Err("a wall to a top level has no plan form".into());
            }
            NewElem::Wall {
                level: lift_level(level)?,
                a: xy(a)?,
                b: xy(b)?,
                bulge: Fs(fields).num_or("bulge", 0.0)?,
                base_offset: Fs(fields).num_or("base_offset", 0.0)?,
                height: height.to_m(),
                wtype: ty(t, "wall_type")?,
            }
        }
        NewEl::Door {
            host,
            at,
            ty: t,
            fields,
        } => {
            if t.is_some() {
                return Err("a door type has no plan form".into());
            }
            let (centre, sill) = on_host(at)?;
            let f = Fs(fields);
            let width = f.num("width")?;
            NewElem::Opening {
                wall: lift_ref(host, slots)?,
                along: start_of(centre, width),
                width,
                height: f.num("height")?,
                sill,
                kind: "door".into(),
            }
        }
        NewEl::Window {
            host,
            at,
            sill,
            ty: t,
            fields,
        } => {
            if t.is_some() {
                return Err("a window type has no plan form".into());
            }
            let (centre, _) = on_host(at)?;
            let f = Fs(fields);
            let width = f.num("width")?;
            NewElem::Opening {
                wall: lift_ref(host, slots)?,
                along: start_of(centre, width),
                width,
                height: f.num("height")?,
                sill: sill.to_m(),
                kind: "window".into(),
            }
        }
        NewEl::Opening {
            host,
            at,
            w,
            h,
            sill,
        } => NewElem::Opening {
            wall: lift_ref(host, slots)?,
            along: start_of(on_host(at)?.0, w.to_m()),
            width: w.to_m(),
            height: h.to_m(),
            sill: sill.to_m(),
            kind: "void".into(),
        },
        NewEl::Slab {
            level,
            boundary,
            offset,
            ty: t,
            fields,
        } => NewElem::Slab {
            level: lift_level(level)?,
            stype: ty(t, "slab_type")?,
            offset: offset.to_m(),
            outer: ring(boundary)?,
            holes: Fs(fields).rings("holes")?,
        },
        NewEl::Ceiling {
            level,
            boundary,
            height,
            ty: t,
            fields,
        } => NewElem::Ceiling {
            level: lift_level(level)?,
            stype: ty(t, "slab_type")?,
            height: height.to_m(),
            outer: ring(boundary)?,
            holes: Fs(fields).rings("holes")?,
        },
        NewEl::Beam {
            level,
            a,
            b,
            ty: t,
            ..
        } => NewElem::Beam {
            level: lift_level(level)?,
            mtype: ty(t, "member_type")?,
            a: xyz(a)?,
            b: xyz(b)?,
        },
        NewEl::Component {
            level,
            at,
            host,
            rotation,
            ty: t,
            fields,
            ..
        } => NewElem::Comp {
            level: lift_level(level)?,
            ctype: ty(t, "comp_type")?.ok_or("a component needs a type")?,
            at: xyz(at)?,
            rotation: rotation.to_rad(),
            host: host.as_ref().map(|h| lift_ref(h, slots)).transpose()?,
            scale: fields.iter().find(|f| f.name == "scale").and_then(|f| f.value.si()).unwrap_or(1.0),
            min_clear: fields.iter().find(|f| f.name == "min_clear").and_then(|f| f.value.si()).unwrap_or(0.0),
        },
        NewEl::MepRun {
            level,
            system,
            path,
            size,
            fields,
            ..
        } => {
            let section = match Fs(fields).find("domain").is_some() {
                true => {
                    let d = Fs(fields).text("domain")?;
                    let h: f64 = Fs(fields).text("height_mm")?.parse().map_err(|_| "bad height")?;
                    Some(super::plan::RunSection {
                        domain: ddc_model::MepDomain::parse(&d).ok_or("bad domain")?,
                        w: size.to_m(),
                        h: h / 1000.0,
                    })
                }
                false => None,
            };
            NewElem::Pipe {
                system: ElRef::parse(system).map_err(|e| e.0)?,
                level: lift_level(level)?,
                material: if section.is_some() {
                    String::new()
                } else {
                    Fs(fields).text("material")?
                },
                dn: (size.to_m() * 1000.0).round() as u32,
                points: path.iter().map(xyz).collect::<Result<_, _>>()?,
                section,
            }
        }
        NewEl::Room {
            level,
            at,
            name,
            number,
        } => NewElem::Room {
            level: lift_level(level)?,
            name: name.clone(),
            number: number.clone().unwrap_or_default(),
            point: xy(at)?,
            // the `set` after the create gives the height; without one the executor default
            height: ROOM_HEIGHT,
        },
        NewEl::Column {
            level,
            at,
            top,
            height,
            rotation,
            ty: t,
            fields,
        } => {
            let f = Fs(fields);
            let base_offset = f.num_or("base_offset", 0.0)?;
            let h = height.to_m();
            let (top_level, top_offset) = if h.abs() > 1e-9 {
                if top.is_some() {
                    return Err("a column takes a height or a top level, not both".into());
                }
                (None, base_offset + h)
            } else {
                (top.as_ref().map(lift_level).transpose()?, f.num_or("top_offset", 0.0)?)
            };
            NewElem::Column {
                level: lift_level(level)?,
                top_level,
                mtype: ty(t, "member_type")?,
                at: xy(at)?,
                base_offset,
                top_offset,
                rotation: rotation.to_rad(),
                structural: match f.find("structural") {
                    None => true,
                    Some(_) => f.flag("structural")?,
                },
                top_at: f.top_at()?,
            }
        }
        NewEl::Generic {
            cat,
            level,
            at,
            ty: t,
            fields,
        } => lift_generic(*cat, opt_level(level)?, at, t, Fs(fields), slots)?,
        other => {
            return Err(format!(
                "{} has no plan element yet",
                serde_json::to_value(other)
                    .ok()
                    .and_then(|v| v.get("kind").and_then(J::as_str).map(str::to_string))
                    .unwrap_or_default()
            ))
        }
    };
    Ok((slot, e))
}

fn lift_generic(
    cat: Cat,
    level: i64,
    at: &[Place],
    t: &Option<Ref>,
    f: Fs<'_>,
    slots: &HashMap<u32, String>,
) -> Result<NewElem, String> {
    let ty = |table: &'static str| -> Result<Option<ElRef>, String> {
        t.as_ref()
            .map(|r| lift_type_ref(r, Some(table), slots))
            .transpose()
    };
    Ok(match cat {
        Cat::Room => NewElem::Room {
            level,
            name: f.text("name")?,
            number: f.text("number")?,
            point: nth(at, 0)?,
            height: f.num("height")?,
        },
        Cat::Column => NewElem::Column {
            level,
            top_level: f.opt_int("top_level")?,
            mtype: ty("member_type")?,
            at: nth(at, 0)?,
            base_offset: f.num("base_offset")?,
            top_offset: f.num("top_offset")?,
            rotation: f.num("rotation")?,
            structural: f.flag("structural")?,
            top_at: f.top_at()?,
        },
        Cat::Roof => {
            let s = f.get("slope")?;
            let slope = match (s.v.as_f64(), s.unit) {
                (None, _) if s.v.is_null() => None,
                (Some(x), Some(Unit::Deg)) => Some(x),
                (Some(x), Some(Unit::Rad)) => Some(x.to_degrees()),
                _ => return Err("field slope is an angle or null".into()),
            };
            NewElem::Roof {
                level,
                stype: ty("slab_type")?,
                offset: f.num("offset")?,
                overhang: f.num("overhang")?,
                ring: ring(at)?,
                slope,
                edges: match f.find("edge_slopes") {
                    None => vec![],
                    Some(v) => v
                        .v
                        .as_array()
                        .ok_or("field edge_slopes is a list of degrees or null")?
                        .iter()
                        .map(|x| match x {
                            J::Null => Ok(None),
                            x => x.as_f64().filter(|d| d.is_finite()).map(Some).ok_or("edge_slopes holds degrees or null"),
                        })
                        .collect::<Result<_, _>>()?,
                },
                vault: match f.find("vault") {
                    None => None,
                    Some(v) => Some(lift_vault(&v.v)?),
                },
                eave: match f.find("eave") {
                    None => None,
                    Some(v) => {
                        let pt = |i: usize| -> Option<(f64, f64)> {
                            let p = v.v.get(i)?;
                            Some((p.get(0)?.as_f64()?, p.get(1)?.as_f64()?))
                        };
                        Some(pt(0).zip(pt(1)).ok_or("field eave is two points [[x, y], [x, y]]")?)
                    }
                },
            }
        }
        Cat::Stair => match f.find("shape") {
            None => NewElem::Stair {
                level,
                top_level: f.int("top_level")?,
                origin: nth(at, 0)?,
                angle: f.num("angle")?,
            },
            Some(s) => NewElem::GenStair {
                level,
                top_level: f.int("top_level")?,
                top_offset: f.num("top_offset")?,
                origin: nth(at, 0)?,
                angle: f.num("angle")?,
                shape: lift_shape(&s.v, &f)?,
                width: f.num("width")?,
                max_riser: f.num("max_riser")?,
                landing: f.num("landing")?,
            },
        },
        Cat::CurtainWall => NewElem::Curtain {
            level,
            name: f.text("name")?,
            a: nth(at, 0)?,
            b: nth(at, 1)?,
            base_offset: f.num("base_offset")?,
            height: f.num("height")?,
            spacing: (f.num("spacing_u")?, f.num("spacing_v")?),
            mullion: (f.num("mullion_w")?, f.num("mullion_d")?),
            spandrel: f.num("spandrel").unwrap_or(0.0),
        },
        Cat::Shaft => NewElem::Shaft {
            level,
            top_level: f.int("top_level")?,
            top_offset: f.num("top_offset")?,
            ring: ring(at)?,
        },
        other => return Err(format!("generic {} has no plan element", other.key())),
    })
}

/// `{a: [x, y], b: [x, y], profile: [[u, z], ...], depth}` of a vault roof.
pub(crate) fn lift_vault(v: &J) -> Result<super::plan::RoofVault, String> {
    let pt = |x: &J| -> Option<(f64, f64)> {
        let a = x.as_array().filter(|a| a.len() == 2)?;
        Some((a[0].as_f64()?, a[1].as_f64()?)).filter(|p| p.0.is_finite() && p.1.is_finite())
    };
    let bad = "vault is {a: [x, y], b: [x, y], profile: [[u, z], ...], depth}";
    let profile = v["profile"].as_array().ok_or(bad)?.iter().map(|p| pt(p).ok_or(bad)).collect::<Result<Vec<_>, _>>()?;
    Ok(super::plan::RoofVault {
        a: pt(&v["a"]).ok_or(bad)?,
        b: pt(&v["b"]).ok_or(bad)?,
        profile,
        depth: v["depth"].as_f64().filter(|d| d.is_finite() && *d > 0.0).ok_or(bad)?,
    })
}

fn lift_shape(v: &J, f: &Fs<'_>) -> Result<StairShape, String> {
    let left = || v.get("left").and_then(J::as_bool).ok_or("shape needs left");
    let first = || -> Result<Option<usize>, String> {
        match v.get("first") {
            None | Some(J::Null) => Ok(None),
            Some(x) => x
                .as_u64()
                .map(|n| Some(n as usize))
                .ok_or_else(|| "shape first is a count".to_string()),
        }
    };
    Ok(match v.get("kind").and_then(J::as_str) {
        Some("straight") => StairShape::Straight,
        Some("l") => StairShape::L {
            left: left()?,
            first: first()?,
        },
        Some("u") => StairShape::U {
            left: left()?,
            first: first()?,
            gap: f.num("gap")?,
        },
        Some("spiral") => StairShape::Spiral {
            radius: f.num("radius")?,
            core: f.num("core")?,
            turn: f.num("turn")?,
        },
        _ => return Err("shape kind is straight, l, u or spiral".into()),
    })
}

fn lift_type(slot: u32, name: &str, fields: &[Field]) -> Result<NewType, String> {
    let f = Fs(fields);
    let mut layers = vec![];
    if let Some((v, k)) = f.coords("layers")? {
        let bad = || "field layers is a list of material and thickness".to_string();
        for l in v.as_array().ok_or_else(bad)? {
            layers.push(LayerSpec {
                material: l.get("material").and_then(J::as_str).ok_or_else(bad)?.to_string(),
                thickness: l.get("thickness").and_then(J::as_f64).ok_or_else(bad)? * k,
            });
        }
    }
    let mut values = vec![];
    for fl in fields {
        let Some(p) = fl.name.strip_prefix(VALUE_PREFIX) else {
            continue;
        };
        let v = match &fl.value.v {
            J::String(s) => ParamValue::Text(s.clone()),
            _ => ParamValue::Num(
                fl.value
                    .si()
                    .ok_or_else(|| format!("value {p} is text or a number"))?,
            ),
        };
        values.push((p.to_string(), v));
    }
    Ok(NewType {
        slot,
        cat: f.text("cat")?,
        name: name.to_string(),
        layers,
        shape: f.opt_text("shape")?,
        material: f.opt_text("material")?.unwrap_or_default(),
        family: f.opt_text("family")?,
        values,
    })
}

fn lift_param(
    name: &str,
    kind: ParamKind,
    cats: &[Cat],
    instance: bool,
    group: &Option<String>,
) -> Result<ParamSpec, String> {
    let ptype = match kind {
        ParamKind::Text => ParamType::Text,
        ParamKind::Integer => ParamType::Int,
        ParamKind::Number => ParamType::Number,
        ParamKind::Length => ParamType::Length,
        ParamKind::Area => ParamType::Area,
        ParamKind::Volume => ParamType::Volume,
        ParamKind::Bool => ParamType::YesNo,
        ParamKind::Angle | ParamKind::Ref => {
            return Err(format!("parameter kind {kind:?} has no model type yet"))
        }
    };
    let mut categories = Vec::with_capacity(cats.len());
    for c in cats {
        let w = PARAM_CATS
            .iter()
            .find(|(_, cat)| cat == c)
            .map(|(w, _)| *w)
            .ok_or_else(|| format!("{} takes no parameters", c.key()))?;
        categories.push(w.to_string());
    }
    Ok(ParamSpec {
        name: name.to_string(),
        ptype,
        per_type: !instance,
        unit: String::new(),
        group: group.clone().unwrap_or_default(),
        guid: None,
        categories,
    })
}

#[cfg(test)]
#[path = "lower_tests.rs"]
mod tests;
