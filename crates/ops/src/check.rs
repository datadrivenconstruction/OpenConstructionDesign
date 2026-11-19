// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Structural checks that need no model: version, group ids, dependencies, slots.
//! Model-level validation (ranges, hosts, fit) happens in the executor.

use std::collections::{BTreeMap, BTreeSet};

use crate::geo::{Form, GeoOp, Path, Pattern, Place, Profile, Tool};
use crate::op::{FamilyOp, GridOp, LevelOp, NewEl, Op, OptionOp, TypeOp};
use crate::refs::{Ref, Targets};
use crate::{Envelope, OP_VERSION};

/// A structural problem. `path` is a JSON pointer into the envelope; `code` is stable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    pub path: String,
    pub code: &'static str,
}

fn issue(path: String, code: &'static str) -> Issue {
    Issue { path, code }
}

impl Envelope {
    /// Empty when the envelope is structurally sound. Rules: `v == OP_VERSION`; at least one
    /// op; unique group ids; `depends_on` names earlier groups only (so no cycles); unique
    /// slots; part names parse; a `new` ref names a slot created earlier, in its own group or
    /// one it depends on.
    pub fn check(&self) -> Vec<Issue> {
        let mut out = Vec::new();
        if self.v != OP_VERSION {
            out.push(issue("/v".into(), "version"));
        }
        if self.op_count() == 0 {
            out.push(issue("/groups".into(), "no_ops"));
        }
        // group id -> transitive dependencies (including itself)
        let mut closure: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
        // slot -> group id that creates it
        let mut slots: BTreeMap<u32, u32> = BTreeMap::new();
        for (gi, g) in self.groups.iter().enumerate() {
            if closure.contains_key(&g.id) {
                out.push(issue(format!("/groups/{gi}/id"), "duplicate_group"));
                continue;
            }
            let mut deps = BTreeSet::from([g.id]);
            for (di, d) in g.depends_on.iter().enumerate() {
                match closure.get(d) {
                    Some(c) if *d != g.id => deps.extend(c.iter().copied()),
                    _ => out.push(issue(format!("/groups/{gi}/depends_on/{di}"), "bad_dependency")),
                }
            }
            for (oi, op) in g.ops.iter().enumerate() {
                let at = format!("/groups/{gi}/ops/{oi}");
                op.for_each_ref(&mut |r: &Ref| {
                    if !parts_ok(r) {
                        out.push(issue(at.clone(), "bad_part"));
                    }
                    if let Ref::New { slot } = r.root() {
                        match slots.get(slot) {
                            None => out.push(issue(at.clone(), "unknown_slot")),
                            Some(owner) if !deps.contains(owner) => {
                                out.push(issue(at.clone(), "undeclared_dependency"))
                            }
                            Some(_) => {}
                        }
                    }
                });
                if let Some(s) = op.slot() {
                    if slots.insert(s, g.id).is_some() {
                        out.push(issue(at, "duplicate_slot"));
                    }
                }
            }
            closure.insert(g.id, deps);
        }
        out
    }
}

/// Every step of a `Part` chain is a known part name.
fn parts_ok(r: &Ref) -> bool {
    match r {
        Ref::Part { of, part } => crate::parts::PartName::parse(part).is_ok() && parts_ok(of),
        _ => true,
    }
}

impl Op {
    /// Visit every `Ref` the op mentions (outer refs only; use `Ref::root` for `Part`).
    pub fn for_each_ref(&self, f: &mut dyn FnMut(&Ref)) {
        match self {
            Op::Create { el, .. } => new_el_refs(el, f),
            Op::Set { target, .. } | Op::Delete { target } => targets_refs(target, f),
            Op::SetType { target, to } => {
                targets_refs(target, f);
                f(to);
            }
            Op::Transform { target, .. } => targets_refs(target, f),
            Op::Pattern { source, pattern, .. } => {
                targets_refs(source, f);
                pattern_refs(pattern, f);
            }
            Op::Geometry(g) => match g {
                GeoOp::Solid { level, form, .. } | GeoOp::Mass { level, form, .. } => {
                    level.iter().for_each(|r| f(r));
                    form_refs(form, f);
                }
                GeoOp::Boolean { a, b, .. } => {
                    f(a);
                    match b {
                        Tool::Ref { of } => f(of),
                        Tool::Form { form } => form_refs(form, f),
                    }
                }
                GeoOp::Void { host: r, form } | GeoOp::Reshape { target: r, form } => {
                    f(r);
                    form_refs(form, f);
                }
                GeoOp::Convert { target, .. } => f(target),
                GeoOp::Freeform { level, .. }
                | GeoOp::Gridshell { level, .. }
                | GeoOp::Member { level, .. }
                | GeoOp::Plate { level, .. } => {
                    level.iter().for_each(|r| f(r))
                }
                GeoOp::EditPoint { target, .. }
                | GeoOp::Thicken { target, .. }
                | GeoOp::Trim { target, .. }
                | GeoOp::Rebuild { target, .. }
                | GeoOp::Panelize { host: target, .. }
                | GeoOp::SurfaceShell { host: target, .. } => f(target),
            },
            Op::Constrain { rule } => rule.refs.iter().for_each(|r| f(r)),
            Op::Type(t) => match t {
                TypeOp::Duplicate { from: r, .. }
                | TypeOp::Rename { target: r, .. }
                | TypeOp::Set { target: r, .. } => f(r),
                TypeOp::Delete { target, replace_with } => {
                    f(target);
                    replace_with.iter().for_each(|r| f(r));
                }
            },
            Op::Param(_) => {}
            Op::Family(fam) => match fam {
                FamilyOp::Define { forms, .. } => forms.iter().for_each(|x| form_refs(&x.form, f)),
                FamilyOp::SetFormula { target, .. } => f(target),
            },
            Op::Level(l) => match l {
                LevelOp::Create { .. } => {}
                LevelOp::Rename { target, .. } | LevelOp::SetElevation { target, .. } => f(target),
            },
            Op::Grid(g) => match g {
                GridOp::Create { .. } => {}
                GridOp::Rename { target, .. } => f(target),
            },
            Op::Option(o) => match o {
                OptionOp::CreateSet { .. } => {}
                OptionOp::Add { set, .. } => f(set),
                OptionOp::Assign { target, option } => {
                    targets_refs(target, f);
                    f(option);
                }
                OptionOp::Accept { option } => f(option),
            },
        }
    }
}

fn targets_refs(t: &Targets, f: &mut dyn FnMut(&Ref)) {
    if let Targets::Refs { refs } = t {
        refs.iter().for_each(|r| f(r));
    }
}

fn opt(r: &Option<Ref>, f: &mut dyn FnMut(&Ref)) {
    if let Some(r) = r {
        f(r);
    }
}

fn place_refs(p: &Place, f: &mut dyn FnMut(&Ref)) {
    match p {
        Place::At { .. } | Place::Snap { .. } => {}
        Place::Offset { from: r, .. } | Place::OnHost { host: r, .. } | Place::Center { of: r } => {
            f(r)
        }
        Place::Crossing { a, b } | Place::Between { a, b, .. } => {
            f(a);
            f(b);
        }
        Place::AlignTo { refs } => refs.iter().for_each(|r| f(r)),
    }
}

fn places(ps: &[Place], f: &mut dyn FnMut(&Ref)) {
    ps.iter().for_each(|p| place_refs(p, f));
}

fn path_refs(p: &Path, f: &mut dyn FnMut(&Ref)) {
    if let Path::Ref { of } = p {
        f(of);
    }
}

fn profile_refs(p: &Profile, f: &mut dyn FnMut(&Ref)) {
    if let Profile::Stored { of } = p {
        f(of);
    }
}

fn form_refs(form: &Form, f: &mut dyn FnMut(&Ref)) {
    match form {
        Form::Extrude { profile, .. } | Form::Revolve { profile, .. } => profile_refs(profile, f),
        Form::Sweep { profile, path, .. } => {
            profile_refs(profile, f);
            path_refs(path, f);
        }
        Form::Loft { sections, path } => {
            sections.iter().for_each(|s| profile_refs(&s.profile, f));
            if let Some(p) = path {
                path_refs(p, f);
            }
        }
        Form::SweptBlend { a, b, path } => {
            profile_refs(a, f);
            profile_refs(b, f);
            path_refs(path, f);
        }
    }
}

fn pattern_refs(p: &Pattern, f: &mut dyn FnMut(&Ref)) {
    match p {
        Pattern::Linear { .. } | Pattern::Radial { .. } | Pattern::Grid2d { .. } => {}
        Pattern::AlongCurve { path, .. } => path_refs(path, f),
        Pattern::OnSurface { face, .. } => f(face),
        Pattern::Distribute { a, b, .. } => {
            f(a);
            f(b);
        }
    }
}

fn new_el_refs(el: &NewEl, f: &mut dyn FnMut(&Ref)) {
    match el {
        NewEl::Wall { level, a, b, top, ty, .. } => {
            f(level);
            place_refs(a, f);
            place_refs(b, f);
            opt(top, f);
            opt(ty, f);
        }
        NewEl::Door { host, at, ty, .. } | NewEl::Window { host, at, ty, .. } => {
            f(host);
            place_refs(at, f);
            opt(ty, f);
        }
        NewEl::Opening { host, at, .. } => {
            f(host);
            place_refs(at, f);
        }
        NewEl::Slab { level, boundary, ty, .. }
        | NewEl::Roof { level, boundary, ty, .. }
        | NewEl::Ceiling { level, boundary, ty, .. } => {
            f(level);
            places(boundary, f);
            opt(ty, f);
        }
        NewEl::Column { level, at, top, ty, .. } => {
            f(level);
            place_refs(at, f);
            opt(top, f);
            opt(ty, f);
        }
        NewEl::Beam { level, a, b, ty, .. } => {
            f(level);
            place_refs(a, f);
            place_refs(b, f);
            opt(ty, f);
        }
        NewEl::Stair { level, a, b, top, ty, .. } => {
            f(level);
            place_refs(a, f);
            place_refs(b, f);
            opt(top, f);
            opt(ty, f);
        }
        NewEl::Railing { level, path, host, ty, .. } => {
            f(level);
            places(path, f);
            opt(host, f);
            opt(ty, f);
        }
        NewEl::Room { level, at, .. } => {
            f(level);
            place_refs(at, f);
        }
        NewEl::Component { level, at, family, host, ty, .. } => {
            f(level);
            place_refs(at, f);
            f(family);
            opt(host, f);
            opt(ty, f);
        }
        NewEl::MepRun { level, path, ty, .. } => {
            f(level);
            places(path, f);
            opt(ty, f);
        }
        NewEl::Generic { level, at, ty, .. } => {
            opt(level, f);
            places(at, f);
            opt(ty, f);
        }
    }
}
