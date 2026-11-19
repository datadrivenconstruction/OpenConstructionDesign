// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A rerun with new params as edits of the recipe's own elements, matched by part key:
//! unchanged parts stay, changed extrusions are reshaped in place (same id), other changed
//! forms are replaced, new keys are created and vanished keys deleted. Nothing outside the
//! recipe's links is named, so the rest of the model cannot change.

use std::collections::{BTreeSet, HashMap};

use ddc_ops::{Actor, Cat, Envelope, Form, GeoOp, Group, Op, Ref, Targets};

use crate::{PartOut, ScriptRun};

/// A live element of a recipe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub key: String,
    pub cat: Cat,
    pub id: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RerunPlan {
    /// `None` when nothing changes.
    pub env: Option<Envelope>,
    pub kept: usize,
    pub reshaped: Vec<String>,
    /// Keys deleted and created again (a form kind that cannot be reshaped in place).
    pub replaced: Vec<String>,
    /// New parts with the envelope slot that creates them (replaced ones included).
    pub created: Vec<PartOut>,
    pub deleted: Vec<String>,
}

fn op_of<'a>(run: &'a ScriptRun, slot: u32) -> Option<&'a Op> {
    run.env
        .groups
        .iter()
        .flat_map(|g| g.ops.iter())
        .nth(slot as usize)
}

fn other_ops(run: &ScriptRun) -> Vec<&Op> {
    let slots: BTreeSet<u32> = run.parts.iter().map(|p| p.slot).collect();
    run.env
        .groups
        .iter()
        .flat_map(|g| g.ops.iter())
        .enumerate()
        .filter(|(i, _)| !slots.contains(&(*i as u32)))
        .map(|x| x.1)
        .collect()
}

/// Same op apart from its slot.
fn same(a: &Op, b: &Op) -> bool {
    match (a, b) {
        (
            Op::Geometry(GeoOp::Solid {
                level: la,
                form: fa,
                material: ma,
                name: na,
                ifc: ia,
                role: ra,
                ..
            }),
            Op::Geometry(GeoOp::Solid {
                level: lb,
                form: fb,
                material: mb,
                name: nb,
                ifc: ib,
                role: rb,
                ..
            }),
        ) => la == lb && fa == fb && ma == mb && na == nb && ia == ib && ra == rb,
        (Op::Geometry(GeoOp::Gridshell { .. }), Op::Geometry(GeoOp::Gridshell { .. })) => {
            with_slot(a, 0) == with_slot(b, 0)
        }
        (
            Op::Geometry(GeoOp::Mass {
                level: la,
                form: fa,
                name: na,
                ..
            }),
            Op::Geometry(GeoOp::Mass {
                level: lb,
                form: fb,
                name: nb,
                ..
            }),
        ) => la == lb && fa == fb && na == nb,
        _ => a == b,
    }
}

fn with_slot(op: &Op, slot: u32) -> Op {
    match op {
        Op::Geometry(GeoOp::Solid {
            level,
            form,
            material,
            name,
            ifc,
            role,
            ..
        }) => Op::Geometry(GeoOp::Solid {
            slot,
            level: level.clone(),
            form: form.clone(),
            material: material.clone(),
            name: name.clone(),
            ifc: ifc.clone(),
            role: role.clone(),
        }),
        Op::Geometry(g @ GeoOp::Gridshell { .. }) => {
            let mut g = g.clone();
            if let GeoOp::Gridshell { slot: s, .. } = &mut g {
                *s = slot;
            }
            Op::Geometry(g)
        }
        Op::Geometry(GeoOp::Mass {
            level, form, name, ..
        }) => Op::Geometry(GeoOp::Mass {
            slot,
            level: level.clone(),
            form: form.clone(),
            name: name.clone(),
        }),
        other => other.clone(),
    }
}

fn is_mass(op: &Op) -> bool {
    matches!(op, Op::Geometry(GeoOp::Mass { .. }))
}

/// Changed in place: an extrusion becoming another extrusion, keeping its level and material.
/// An old sweep or loft is replaced instead, since the model reshapes extrusions only.
fn reshape(old: &Op, new: &Op) -> Option<Form> {
    match (old, new) {
        (
            Op::Geometry(GeoOp::Solid {
                level: la,
                material: ma,
                name: na,
                ifc: ia,
                role: ra,
                form: Form::Extrude { .. },
                ..
            }),
            Op::Geometry(GeoOp::Solid {
                level: lb,
                material: mb,
                name: nb,
                ifc: ib,
                role: rb,
                form: f @ Form::Extrude { .. },
                ..
            }),
        ) if la == lb && ma == mb && na == nb && ia == ib && ra == rb => Some(f.clone()),
        _ => None,
    }
}

/// Edits turning the elements made by `old` (linked as `links`) into what `new` makes.
pub fn rerun_envelope(
    old: &ScriptRun,
    new: &ScriptRun,
    links: &[Link],
    actor: Actor,
    title: &str,
) -> Result<RerunPlan, String> {
    if other_ops(old) != other_ops(new) {
        return Err(
            "the new params change booleans of the recipe; rerun updates parts only: \
                    run the script as a new recipe instead"
                .into(),
        );
    }
    let link: HashMap<&str, &Link> = links.iter().map(|l| (l.key.as_str(), l)).collect();
    let old_op: HashMap<&str, &Op> = old
        .parts
        .iter()
        .filter_map(|p| Some((p.key.as_str(), op_of(old, p.slot)?)))
        .collect();
    let mut plan = RerunPlan {
        env: None,
        kept: 0,
        reshaped: vec![],
        replaced: vec![],
        created: vec![],
        deleted: vec![],
    };
    let (mut ops, mut gone) = (vec![], vec![]);
    let mut slot = 0u32;
    let mut create = |ops: &mut Vec<Op>, op: &Op, key: &str, plan: &mut RerunPlan| {
        ops.push(with_slot(op, slot));
        plan.created.push(PartOut {
            key: key.to_string(),
            slot,
        });
        slot += 1;
    };
    let keys: BTreeSet<&str> = new.parts.iter().map(|p| p.key.as_str()).collect();
    for p in &new.parts {
        let op = op_of(new, p.slot).ok_or("internal: part without op")?;
        let Some(l) = link.get(p.key.as_str()) else {
            create(&mut ops, op, &p.key, &mut plan);
            continue;
        };
        let target = Ref::id(l.cat, l.id);
        match old_op.get(p.key.as_str()) {
            Some(o) if same(o, op) => plan.kept += 1,
            Some(o) if is_mass(o) || is_mass(op) => {
                return Err(format!(
                    "part `{}` is a mass cut or joined by a boolean and the new params change \
                     it; a rerun cannot rebuild booleans: run the script as a new recipe",
                    p.key
                ))
            }
            Some(o) if reshape(o, op).is_some() => {
                let form = reshape(o, op).ok_or("internal")?;
                // reshape keeps the id: undo, links and references stay valid
                ops.push(Op::Geometry(GeoOp::Reshape { target, form }));
                plan.reshaped.push(p.key.clone());
            }
            _ => {
                gone.push(target);
                plan.replaced.push(p.key.clone());
                create(&mut ops, op, &p.key, &mut plan);
            }
        }
    }
    for l in links {
        if !keys.contains(l.key.as_str()) {
            gone.push(Ref::id(l.cat, l.id));
            plan.deleted.push(l.key.clone());
        }
    }
    if !gone.is_empty() {
        // deletes first: a replaced part's new row never meets its old one
        ops.insert(
            0,
            Op::Delete {
                target: Targets::Refs { refs: gone },
            },
        );
    }
    if !ops.is_empty() {
        let mut env = Envelope::new(actor, title);
        env.groups.push(Group {
            id: 1,
            title: title.to_string(),
            ops,
            depends_on: vec![],
            why: String::new(),
        });
        plan.env = Some(env);
    }
    Ok(plan)
}
