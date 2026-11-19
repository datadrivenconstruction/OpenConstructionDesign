// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Adapter from the XLSX bulk `Edit` vocabulary to the shared wire `Envelope`.
//!
//! Additive and pure: it never touches the model or the import path. An XLSX batch
//! becomes one envelope with actor Xlsx (one journal entry, one undo step, provenance
//! in `why`). An edit the wire cannot carry losslessly is refused with a reason and is
//! left out of the envelope; the caller must treat a non-empty `refused` as "not
//! representable" and keep using the `Edit` path for that batch.

use ddc_model::{BeamSpec, ColumnSpec, Edit, PVal, ParamType, WallSpec};
use ddc_ops::{
    Actor, Angle, Cat, End, Envelope, Field, Group, Len, LevelOp, NewEl, Op, ParamKind, ParamOp,
    Place, Ref, Side, Targets, TypeOp, Unit, Value, P3,
};
use serde_json::json;

use super::Report;

/// One edit the wire cannot carry, with its position in the batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub index: usize,
    pub edit: &'static str,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Adapted {
    pub envelope: Envelope,
    pub refused: Vec<Refusal>,
}

impl Adapted {
    /// True when every edit of the batch is in the envelope.
    pub fn complete(&self) -> bool {
        self.refused.is_empty()
    }
}

impl Report {
    /// The accepted batch (`batch()`) as one XLSX envelope; `source` is the workbook name.
    pub fn to_envelope(&self, source: &str) -> Adapted {
        edits_to_envelope(&self.batch(), source)
    }
}

/// Variant name of an edit, for refusals and tests.
pub fn edit_name(e: &Edit) -> &'static str {
    match e {
        Edit::Level { .. } => "level",
        Edit::NewLevel { .. } => "new_level",
        Edit::WallType { .. } => "wall_type",
        Edit::Layer { .. } => "layer",
        Edit::SlabType { .. } => "slab_type",
        Edit::Material { .. } => "material",
        Edit::Wall { .. } => "wall",
        Edit::NewWall { .. } => "new_wall",
        Edit::WallShape { .. } => "wall_shape",
        Edit::Delete { table, .. } => match *table {
            "wall" => "delete_wall",
            "opening" => "delete_opening",
            "slab" => "delete_slab",
            _ => "delete",
        },
        Edit::Opening { .. } => "opening",
        Edit::NewOpening { .. } => "new_opening",
        Edit::OpeningSwing { .. } => "opening_swing",
        Edit::Room { .. } => "room",
        Edit::NewRoom(_) => "new_room",
        Edit::DeleteRoom(_) => "delete_room",
        Edit::Slab { .. } => "slab",
        Edit::Column { .. } => "column",
        Edit::NewColumn(_) => "new_column",
        Edit::DeleteColumn(_) => "delete_column",
        Edit::Beam { .. } => "beam",
        Edit::NewBeam(_) => "new_beam",
        Edit::DeleteBeam(_) => "delete_beam",
        Edit::Roof { .. } => "roof",
        Edit::SlabBoundary { .. } => "slab_boundary",
        Edit::NewSlab(_) => "new_slab",
        Edit::CurtainProps { .. } => "curtain_props",
        Edit::NewCurtain(_) => "new_curtain",
        Edit::DeleteCurtain(_) => "delete_curtain",
        Edit::NewParam(_) => "new_param",
        Edit::ParamValue { .. } => "param_value",
        Edit::Phase { .. } => "phase",
        Edit::Mark { .. } => "mark",
        Edit::ParamValueNew { .. } => "param_value_new",
    }
}

/// Deterministic: same batch, same envelope (slots count up in batch order; groups are
/// runs of consecutive edits of one kind of thing, each depending on the previous one).
pub fn edits_to_envelope(edits: &[Edit], source: &str) -> Adapted {
    let mut env = Envelope::new(Actor::Xlsx, "XLSX import");
    env.why = Some(format!("xlsx: {source}"));
    let mut refused = Vec::new();
    let mut slot = 0u32;
    let mut last: Option<&'static str> = None;
    // slot of the latest created row per table, and of each marked workbook row
    let mut last_new: std::collections::HashMap<&'static str, u32> = Default::default();
    let mut marks: std::collections::HashMap<(String, u32), u32> = Default::default();
    for (index, e) in edits.iter().enumerate() {
        let mapped = match e {
            Edit::Mark { kind, row } => {
                match last_new.get(kind.as_str()) {
                    Some(s) => {
                        marks.insert((kind.clone(), *row), *s);
                    }
                    None => refused.push(Refusal {
                        index,
                        edit: edit_name(e),
                        reason: format!("no new {kind} before the mark of row {row}"),
                    }),
                }
                continue;
            }
            Edit::ParamValueNew { param, kind, row, value } => match marks.get(&(kind.clone(), *row)) {
                Some(s) => Ok(("params", vec![Op::Set {
                    target: Targets::one(Ref::slot(*s)),
                    fields: vec![f(param, pval_value(value))],
                }])),
                None => Err(format!("no new {kind} for row {row} of `{param}`")),
            },
            _ if on_batch_level(e) => Err("sits on a level made by the same book".to_owned()),
            _ => map_edit(e, &mut slot),
        };
        if let (Some(table), Ok((_, ops))) = (new_table(e), &mapped) {
            if let Some(s) = ops.iter().find_map(|o| match o {
                Op::Create { slot, .. } => Some(*slot),
                _ => None,
            }) {
                last_new.insert(table, s);
            }
        }
        match mapped {
            Ok((group, ops)) => {
                if last != Some(group) || env.groups.is_empty() {
                    let gid = env.groups.len() as u32 + 1;
                    let depends_on = if gid > 1 { vec![gid - 1] } else { vec![] };
                    env.groups.push(Group {
                        id: gid,
                        title: group.to_owned(),
                        ops: vec![],
                        depends_on,
                        why: String::new(),
                    });
                    last = Some(group);
                }
                if let Some(g) = env.groups.last_mut() {
                    g.ops.extend(ops);
                }
            }
            Err(reason) => refused.push(Refusal { index, edit: edit_name(e), reason }),
        }
    }
    Adapted { envelope: env, refused }
}

/// A level id below zero is a batch-local reference to a level created earlier in the book.
fn on_batch_level(e: &Edit) -> bool {
    let l = match e {
        Edit::Wall { spec, .. } | Edit::NewWall { spec, .. } => spec.level,
        Edit::Room { spec, .. } | Edit::NewRoom(spec) => spec.level,
        Edit::NewSlab(spec) => spec.level,
        Edit::NewCurtain(spec) => spec.level,
        Edit::Slab { level, .. } | Edit::CurtainProps { level, .. } => *level,
        _ => 0,
    };
    l < 0
}

fn m(v: f64) -> Value {
    Value::num(v, Unit::M)
}
fn num(v: f64) -> Value {
    Value::plain(json!(v))
}
fn int(v: i64) -> Value {
    Value::plain(json!(v))
}
fn txt(s: &str) -> Value {
    Value::text(s)
}
fn flag(b: bool) -> Value {
    Value::plain(json!(b))
}
fn null() -> Value {
    Value::plain(serde_json::Value::Null)
}
fn f(name: &str, v: Value) -> Field {
    Field::new(name, v)
}
fn rf(cat: Cat, i: i64) -> Ref {
    Ref::id(cat, i)
}
fn tgt(cat: Cat, i: i64) -> Targets {
    Targets::one(rf(cat, i))
}
fn at(x: f64, y: f64, z: f64) -> Place {
    Place::At { p: P3::m(x, y, z) }
}
fn set(cat: Cat, i: i64, fields: Vec<Field>) -> Op {
    Op::Set { target: tgt(cat, i), fields }
}
fn del(cat: Cat, i: i64) -> Op {
    Op::Delete { target: tgt(cat, i) }
}
fn ring(v: &[(f64, f64)]) -> serde_json::Value {
    json!(v.iter().map(|p| [p.0, p.1]).collect::<Vec<_>>())
}
fn rings(v: &[Vec<(f64, f64)>]) -> serde_json::Value {
    json!(v.iter().map(|h| ring(h)).collect::<Vec<_>>())
}
fn opt_m(v: Option<f64>) -> Value {
    v.map_or_else(null, m)
}
fn opt_int(v: Option<i64>) -> Value {
    v.map_or_else(null, int)
}

fn next(slot: &mut u32) -> u32 {
    *slot += 1;
    *slot
}

fn pval_value(value: &Option<PVal>) -> Value {
    match value {
        None => null(),
        Some(PVal::Text(s)) => txt(s),
        Some(PVal::Int(n)) => int(*n),
        Some(PVal::Num(x)) => num(*x),
        Some(PVal::Bool(b)) => flag(*b),
    }
}

/// Table a `New*` edit creates a row in, as `Edit::Mark` names it.
fn new_table(e: &Edit) -> Option<&'static str> {
    Some(match e {
        Edit::NewWall { .. } => "wall",
        Edit::NewOpening { .. } => "opening",
        Edit::NewRoom(_) => "room",
        Edit::NewSlab(_) => "slab",
        Edit::NewCurtain(_) => "curtain_wall",
        Edit::NewColumn(_) => "column",
        Edit::NewBeam(_) => "beam",
        Edit::NewLevel { .. } => "level",
        _ => return None,
    })
}

/// Table name used by parameter values and phases -> wire category.
fn table_cat(t: &str) -> Option<Cat> {
    Some(match t {
        "wall" => Cat::Wall,
        "opening" => Cat::Opening,
        "room" => Cat::Room,
        "slab" => Cat::Slab,
        "col" => Cat::Column,
        "beam" => Cat::Beam,
        "roof" => Cat::Roof,
        "curtain_wall" => Cat::CurtainWall,
        "level" => Cat::Level,
        "wall_type" => Cat::WallType,
        "slab_type" => Cat::SlabType,
        "member_type" => Cat::MemberType,
        _ => return None,
    })
}

fn param_cat(key: &str) -> Option<Cat> {
    match key {
        "col" => Some(Cat::Column),
        "comp" => Some(Cat::Component),
        k => Cat::from_key(k),
    }
}

fn wall_fields(s: &WallSpec, bulge: f64) -> Vec<Field> {
    vec![
        f("level", int(s.level)),
        f("ax", m(s.a.0)),
        f("ay", m(s.a.1)),
        f("bx", m(s.b.0)),
        f("by", m(s.b.1)),
        f("base_offset", m(s.base_offset)),
        f("height", m(s.height)),
        f("wall_type", int(s.wall_type)),
        f("offset0", opt_m(s.offset0)),
        f("bulge", num(bulge)),
    ]
}

fn column_fields(s: &ColumnSpec) -> Vec<Field> {
    vec![
        f("level", int(s.level)),
        f("top_level", opt_int(s.top_level)),
        f("member_type", int(s.member_type)),
        f("structural", flag(s.structural)),
        f("x", m(s.at.0)),
        f("y", m(s.at.1)),
        f("base_offset", m(s.base_offset)),
        f("top_offset", m(s.top_offset)),
        f("rotation", Value::num(s.rotation, Unit::Rad)),
        f("top_at_x", opt_m(s.top_at.map(|p| p.0))),
        f("top_at_y", opt_m(s.top_at.map(|p| p.1))),
    ]
}

/// Beam rows that are not part of `NewEl::Beam`'s typed slots.
fn beam_extra(s: &BeamSpec) -> Vec<Field> {
    vec![
        f("bulge", num(s.bulge)),
        f("yjust", txt(&format!("{:?}", s.yjust).to_lowercase())),
        f("zjust", txt(&format!("{:?}", s.zjust).to_lowercase())),
        f("rotation", Value::num(s.rotation, Unit::Rad)),
        f("start_ext", m(s.start_ext)),
        f("end_ext", m(s.end_ext)),
        f("join_start", txt(&format!("{:?}", s.join_start).to_lowercase())),
        f("join_end", txt(&format!("{:?}", s.join_end).to_lowercase())),
    ]
}

fn beam_fields(s: &BeamSpec) -> Vec<Field> {
    let mut v = vec![
        f("level", int(s.level)),
        f("member_type", int(s.member_type)),
        f("ax", m(s.a.0)),
        f("ay", m(s.a.1)),
        f("az", m(s.a.2)),
        f("bx", m(s.b.0)),
        f("by", m(s.b.1)),
        f("bz", m(s.b.2)),
    ];
    v.extend(beam_extra(s));
    v
}

fn map_edit(e: &Edit, slot: &mut u32) -> Result<(&'static str, Vec<Op>), String> {
    Ok(match e {
        Edit::Level { id, name, elevation } => (
            "levels",
            vec![
                Op::Level(LevelOp::Rename { target: rf(Cat::Level, *id), name: name.clone() }),
                Op::Level(LevelOp::SetElevation {
                    target: rf(Cat::Level, *id),
                    elevation: Len::m(*elevation),
                }),
            ],
        ),
        Edit::NewLevel { name, elevation } => (
            "levels",
            vec![Op::Level(LevelOp::Create {
                slot: next(slot),
                name: name.clone(),
                elevation: Len::m(*elevation),
            })],
        ),
        Edit::WallType { id, name } => (
            "types",
            vec![Op::Type(TypeOp::Rename { target: rf(Cat::WallType, *id), name: name.clone() })],
        ),
        Edit::Layer { wtype, pos, layer } => (
            "types",
            vec![Op::Type(TypeOp::Set {
                target: rf(Cat::WallType, *wtype),
                fields: vec![
                    f(&format!("layer.{pos}.material"), txt(&layer.material)),
                    f(&format!("layer.{pos}.thickness"), m(layer.thickness)),
                ],
            })],
        ),
        // the op set has no slab type layers yet: refused here, the batch path applies it
        Edit::SlabType { .. } => return Err("slab type layers have no op equivalent".into()),
        Edit::Material { .. } => return Err("material edits have no op equivalent".into()),
        Edit::Wall { id, spec, bulge } => {
            ("walls", vec![set(Cat::Wall, *id, wall_fields(spec, *bulge))])
        }
        Edit::NewWall { spec, bulge } => (
            "walls",
            vec![Op::Create {
                slot: next(slot),
                el: NewEl::Wall {
                    level: rf(Cat::Level, spec.level),
                    a: at(spec.a.0, spec.a.1, 0.0),
                    b: at(spec.b.0, spec.b.1, 0.0),
                    height: Len::m(spec.height),
                    top: None,
                    ty: Some(rf(Cat::WallType, spec.wall_type)),
                    // default rows are left out: wall create lowers only `bulge` (b85f505a),
                    // a set base offset or axis offset is still refused, never dropped
                    fields: [
                        (spec.base_offset != 0.0).then(|| f("base_offset", m(spec.base_offset))),
                        spec.offset0.map(|o| f("offset0", m(o))),
                        (*bulge != 0.0).then(|| f("bulge", num(*bulge))),
                    ]
                    .into_iter()
                    .flatten()
                    .collect(),
                },
            }],
        ),
        Edit::Delete { table, id } => match *table {
            "wall" => ("walls", vec![del(Cat::Wall, *id)]),
            "opening" => ("openings", vec![del(Cat::Opening, *id)]),
            "slab" => ("slabs", vec![del(Cat::Slab, *id)]),
            t => return Err(format!("delete of `{t}` has no wire form")),
        },
        Edit::Opening { id, wall, spec } => (
            "openings",
            vec![set(
                Cat::Opening,
                *id,
                vec![
                    f("wall", int(*wall)),
                    f("along", m(spec.along)),
                    f("width", m(spec.width)),
                    f("sill", m(spec.sill)),
                    f("height", m(spec.height)),
                    f("kind", txt(&spec.kind)),
                ],
            )],
        ),
        Edit::NewOpening { wall, spec, swing } => {
            let host = rf(Cat::Wall, *wall);
            // the row stores the start; the wire `along` places the centre
            let place = Place::OnHost {
                host: host.clone(),
                along: Len::m(spec.along + spec.width / 2.0),
                from_end: End::A,
                side: Side::Center,
                z: Len::m(0.0),
            };
            let mut size = vec![
                f("width", m(spec.width)),
                f("height", m(spec.height)),
                f("swing_side", flag(swing.side)),
                f("swing_hinge", flag(swing.hinge)),
            ];
            let el = match spec.kind.as_str() {
                "door" => {
                    // a door has no typed sill: it travels as a field, never dropped
                    if spec.sill != 0.0 {
                        size.push(f("sill", m(spec.sill)));
                    }
                    NewEl::Door { host, at: place, ty: None, fields: size }
                }
                "window" => NewEl::Window {
                    host,
                    at: place,
                    sill: Len::m(spec.sill),
                    ty: None,
                    fields: size,
                },
                "void" => NewEl::Opening {
                    host,
                    at: place,
                    w: Len::m(spec.width),
                    h: Len::m(spec.height),
                    sill: Len::m(spec.sill),
                },
                k => return Err(format!("opening kind `{k}` has no wire form")),
            };
            ("openings", vec![Op::Create { slot: next(slot), el }])
        }
        Edit::WallShape { id, lean, top_scale } => (
            "walls",
            vec![set(
                Cat::Wall,
                *id,
                vec![f("lean", Value::num(*lean, Unit::Rad)), f("top_scale", num(*top_scale))],
            )],
        ),
        Edit::OpeningSwing { id, swing } => (
            "openings",
            vec![set(
                Cat::Opening,
                *id,
                vec![f("swing_side", flag(swing.side)), f("swing_hinge", flag(swing.hinge))],
            )],
        ),
        Edit::Room { id, spec } => (
            "rooms",
            vec![set(
                Cat::Room,
                *id,
                vec![
                    f("level", int(spec.level)),
                    f("name", txt(&spec.name)),
                    f("number", txt(&spec.number)),
                    f("px", m(spec.point.0)),
                    f("py", m(spec.point.1)),
                    f("height", m(spec.height)),
                ],
            )],
        ),
        Edit::NewRoom(spec) => {
            let s = next(slot);
            let number = if spec.number.is_empty() { None } else { Some(spec.number.clone()) };
            (
                "rooms",
                vec![
                    Op::Create {
                        slot: s,
                        el: NewEl::Room {
                            level: rf(Cat::Level, spec.level),
                            at: at(spec.point.0, spec.point.1, 0.0),
                            name: spec.name.clone(),
                            number,
                        },
                    },
                    // the typed room slot has no height row
                    Op::Set {
                        target: Targets::one(Ref::slot(s)),
                        fields: vec![f("height", m(spec.height))],
                    },
                ],
            )
        }
        Edit::DeleteRoom(id) => ("rooms", vec![del(Cat::Room, *id)]),
        Edit::Slab { id, level, slab_type, offset } => (
            "slabs",
            vec![set(
                Cat::Slab,
                *id,
                vec![
                    f("level", int(*level)),
                    f("slab_type", int(*slab_type)),
                    f("offset", m(*offset)),
                ],
            )],
        ),
        Edit::SlabBoundary { id, outer, holes } => (
            "slabs",
            vec![set(
                Cat::Slab,
                *id,
                vec![
                    f("outer", Value::plain(ring(outer))),
                    f("holes", Value::plain(rings(holes))),
                ],
            )],
        ),
        Edit::NewSlab(spec) => (
            "slabs",
            vec![Op::Create {
                slot: next(slot),
                el: NewEl::Slab {
                    level: rf(Cat::Level, spec.level),
                    boundary: spec.outer.iter().map(|p| at(p.0, p.1, 0.0)).collect(),
                    offset: Len::m(spec.offset),
                    ty: Some(rf(Cat::SlabType, spec.slab_type)),
                    fields: vec![f("holes", Value::plain(rings(&spec.holes)))],
                },
            }],
        ),
        Edit::Column { id, spec } => ("columns", vec![set(Cat::Column, *id, column_fields(spec))]),
        Edit::NewColumn(spec) => (
            "columns",
            vec![Op::Create {
                slot: next(slot),
                el: NewEl::Column {
                    level: rf(Cat::Level, spec.level),
                    at: at(spec.at.0, spec.at.1, 0.0),
                    top: spec.top_level.map(|t| rf(Cat::Level, t)),
                    // 0 = derived from the top level and the offsets carried in `fields`
                    height: Len::m(0.0),
                    rotation: Angle::rad(spec.rotation),
                    ty: Some(rf(Cat::MemberType, spec.member_type)),
                    fields: vec![
                        f("base_offset", m(spec.base_offset)),
                        f("top_offset", m(spec.top_offset)),
                        f("structural", flag(spec.structural)),
                        f("top_at_x", opt_m(spec.top_at.map(|p| p.0))),
                        f("top_at_y", opt_m(spec.top_at.map(|p| p.1))),
                    ],
                },
            }],
        ),
        Edit::DeleteColumn(id) => ("columns", vec![del(Cat::Column, *id)]),
        Edit::Beam { id, spec } => ("beams", vec![set(Cat::Beam, *id, beam_fields(spec))]),
        Edit::NewBeam(spec) => (
            "beams",
            vec![Op::Create {
                slot: next(slot),
                el: NewEl::Beam {
                    level: rf(Cat::Level, spec.level),
                    a: at(spec.a.0, spec.a.1, spec.a.2),
                    b: at(spec.b.0, spec.b.1, spec.b.2),
                    ty: Some(rf(Cat::MemberType, spec.member_type)),
                    fields: beam_extra(spec),
                },
            }],
        ),
        Edit::DeleteBeam(id) => ("beams", vec![del(Cat::Beam, *id)]),
        Edit::Roof { id, offset, overhang, fascia } => (
            "roofs",
            vec![set(
                Cat::Roof,
                *id,
                vec![f("offset", m(*offset)), f("overhang", m(*overhang)), f("fascia", m(*fascia))],
            )],
        ),
        Edit::CurtainProps { id, level, name, base_offset } => (
            "curtains",
            vec![set(
                Cat::CurtainWall,
                *id,
                vec![
                    f("level", int(*level)),
                    f("name", txt(name)),
                    f("base_offset", m(*base_offset)),
                ],
            )],
        ),
        Edit::NewCurtain(_) => {
            return Err("curtain grid, sections and sloped top have no wire form yet".into())
        }
        Edit::DeleteCurtain(id) => ("curtains", vec![del(Cat::CurtainWall, *id)]),
        Edit::NewParam(sp) => {
            if sp.guid.is_some() {
                return Err("shared parameter guid has no wire form".into());
            }
            let (kind, unit) = match sp.ptype {
                ParamType::Text => (ParamKind::Text, None),
                ParamType::Int => (ParamKind::Integer, None),
                ParamType::Number => (ParamKind::Number, None),
                ParamType::Length => (ParamKind::Length, Some(Unit::M)),
                ParamType::Area => (ParamKind::Area, Some(Unit::M2)),
                ParamType::Volume => (ParamKind::Volume, Some(Unit::M3)),
                ParamType::YesNo => (ParamKind::Bool, None),
            };
            if !sp.unit.is_empty() && matches!(sp.ptype, ParamType::Number) {
                return Err(format!("unit label `{}` has no wire unit", sp.unit));
            }
            let mut cats = Vec::new();
            for c in &sp.categories {
                cats.push(param_cat(c).ok_or_else(|| format!("unknown parameter category `{c}`"))?);
            }
            let group = if sp.group.is_empty() { None } else { Some(sp.group.clone()) };
            (
                "params",
                vec![Op::Param(ParamOp::Define {
                    name: sp.name.clone(),
                    kind,
                    unit,
                    cats,
                    instance: !sp.per_type,
                    group,
                })],
            )
        }
        Edit::ParamValue { param, kind, id, value } => {
            let cat = table_cat(kind).ok_or_else(|| format!("unknown parameter table `{kind}`"))?;
            ("params", vec![set(cat, *id, vec![f(param, pval_value(value))])])
        }
        // handled in `edits_to_envelope`: they name a slot, not an id
        Edit::Mark { .. } | Edit::ParamValueNew { .. } => {
            return Err("row mark outside the batch walk".into())
        }
        Edit::Phase { kind, id, created, demolished } => {
            let cat = table_cat(kind).ok_or_else(|| format!("unknown phase table `{kind}`"))?;
            (
                "phases",
                vec![set(
                    cat,
                    *id,
                    vec![
                        f("phase_created", int(*created)),
                        f("phase_demolished", opt_int(*demolished)),
                    ],
                )],
            )
        }
    })
}
