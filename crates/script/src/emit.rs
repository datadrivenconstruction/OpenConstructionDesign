// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! What a run builds: parts (keyed solids), the ops that make them, declared params,
//! measures, constraints and the log. Turned into an envelope once the script ends.

use std::collections::HashMap;

use ddc_ops::{
    Actor, BoolKind, Dir3, Envelope, Form, Frame, GeoOp, Group, IfcClass, Len, LenUnit, Op, Path, Ref,
    MemberRole, RoleTag, Section, Tool, P3,
};
use serde_json::{Map, Value as J};

use crate::values::{FormV, Shape, V3};
use crate::{LimitKind, Measure, ParamOut, PartOut};

/// Smallest part a realistic model holds (AI_GEOMETRY_PLAN.md 3.4), metres.
pub(crate) const MIN_PART: f64 = 0.005;
const LOG_LINES: usize = 200;
const LOG_CHARS: usize = 500;

pub(crate) struct PartRec {
    pub name: String,
    /// Group names, outermost first.
    pub path: Vec<String>,
    pub slot: u32,
}

#[derive(Default)]
pub(crate) struct Builder {
    pub level: Option<Ref>,
    pub ops: Vec<Op>,
    pub parts: Vec<PartRec>,
    pub params_in: Map<String, J>,
    pub params: Vec<ParamOut>,
    pub measures: Vec<Measure>,
    pub constraints: usize,
    pub log: Vec<String>,
    pub elements: usize,
    /// Points of all pattern copies so far (Rust memory the Lua limit does not see).
    pub copy_points: usize,
    pub max_elements: usize,
    /// Set when a limit stopped the run, so a `pcall` in the script cannot hide it.
    pub limit: Option<LimitKind>,
    /// Line of the latest executed script line (from the hook), for errors without one.
    pub line: u32,
    /// Bounds and axis of each part, in part order, for `audit`.
    pub geo: Vec<crate::audit::Geo>,
    /// CPU time spent in structural checks; the script's CPU limit does not count it.
    pub cpu_credit: std::time::Duration,
}

fn r9(v: f64) -> f64 {
    (v * 1e9).round() / 1e9 + 0.0
}

fn r12(v: f64) -> f64 {
    (v * 1e12).round() / 1e12 + 0.0
}

fn p3(v: V3) -> P3 {
    P3 {
        x: r9(v.x),
        y: r9(v.y),
        z: r9(v.z),
        unit: LenUnit::M,
    }
}

fn dir(v: V3) -> Dir3 {
    Dir3 {
        x: r12(v.x),
        y: r12(v.y),
        z: r12(v.z),
    }
}

/// The wire form of a script form; coordinates absolute, metres.
pub(crate) fn wire_form(f: &FormV) -> Form {
    match &f.0 {
        Shape::Extrude { prof, o, x, z, len } => Form::Extrude {
            profile: prof.clone(),
            frame: Frame {
                origin: p3(*o),
                x: dir(*x),
                z: dir(*z),
            },
            start: Len::m(0.0),
            end: Len::m(r9(*len)),
        },
        Shape::Sweep { prof, path, twist, scale } => Form::Sweep {
            twist: (*twist != 0.0).then(|| ddc_ops::Angle::rad(r12(*twist))),
            scale: ((*scale - 1.0).abs() > 1e-12).then(|| r12(*scale)),
            profile: prof.clone(),
            path: Path::Polyline {
                pts: path.pts.iter().map(|p| p3(*p)).collect(),
                bulges: path.bulges.iter().map(|b| r12(*b)).collect(),
                closed: path.closed,
            },
        },
        Shape::Loft { secs } => Form::Loft {
            sections: secs
                .iter()
                .map(|(p, at)| Section {
                    profile: p.clone(),
                    frame: Frame {
                        origin: p3(*at),
                        x: Dir3::X,
                        z: Dir3::Z,
                    },
                })
                .collect(),
            path: None,
        },
    }
}

impl Builder {
    fn count(&mut self) -> Result<(), String> {
        self.elements += 1;
        if self.elements > self.max_elements {
            self.limit = Some(LimitKind::Elements);
            return Err(format!("more than {} elements", self.max_elements));
        }
        Ok(())
    }

    /// One solid part with an optional semantic IFC class; returns its index.
    pub fn part(
        &mut self,
        name: &str,
        f: &FormV,
        material: Option<String>,
        ifc: Option<IfcClass>,
    ) -> Result<usize, String> {
        self.part_in(name, f, material, ifc, None)
    }

    /// As `part`, on `level` (the run's level when none).
    pub fn part_in(
        &mut self,
        name: &str,
        f: &FormV,
        material: Option<String>,
        ifc: Option<IfcClass>,
        level: Option<Ref>,
    ) -> Result<usize, String> {
        check_name(name)?;
        let len = f.length();
        if !(len >= MIN_PART) {
            return Err(format!(
                "part `{name}` is {:.1} mm long; parts shorter than 5 mm are not built",
                len * 1000.0
            ));
        }
        self.count()?;
        let slot = self.ops.len() as u32;
        self.ops.push(Op::Geometry(GeoOp::Solid {
            slot,
            level: level.or_else(|| self.level.clone()),
            form: wire_form(f),
            material,
            name: Some(format!("{name} {}", crate::audit::label(f))),
            ifc,
            role: None,
        }));
        self.geo.push(crate::audit::geo_of(f));
        self.parts.push(PartRec {
            name: name.to_string(),
            path: vec![],
            slot,
        });
        Ok(self.parts.len() - 1)
    }

    /// A grid structure the model builds on its analytic host (`geometry.gridshell`) as one
    /// part, keyed and grouped like a solid; `pts` (its lattice points) bound it for `audit`.
    pub fn lattice(
        &mut self,
        name: &str,
        make: impl FnOnce(u32, Option<Ref>) -> GeoOp,
        pts: &[V3],
        r: f64,
    ) -> Result<usize, String> {
        check_name(name)?;
        self.count()?;
        let slot = self.ops.len() as u32;
        self.ops.push(Op::Geometry(make(slot, self.level.clone())));
        self.geo.push(crate::audit::geo_of_points(pts, r));
        self.parts.push(PartRec {
            name: name.to_string(),
            path: vec![],
            slot,
        });
        Ok(self.parts.len() - 1)
    }

    /// A straight structural member (`geometry.member`): a beam or brace row of the catalogue
    /// `section`, keyed and grouped like a solid; `r` is half its section depth for `audit`.
    pub fn member(
        &mut self,
        name: &str,
        a: V3,
        b: V3,
        section: &str,
        role: MemberRole,
        r: f64,
    ) -> Result<usize, String> {
        check_name(name)?;
        if !(b.sub(a).len() >= MIN_PART) {
            return Err(format!("member `{name}` is shorter than 5 mm"));
        }
        self.count()?;
        let slot = self.ops.len() as u32;
        self.ops.push(Op::Geometry(GeoOp::Member {
            slot,
            level: self.level.clone(),
            start: p3(a),
            end: p3(b),
            section: section.to_string(),
            role,
            rotation: None,
        }));
        self.geo.push(crate::audit::geo_of_member(a, b, r));
        self.parts.push(PartRec { name: name.to_string(), path: vec![], slot });
        Ok(self.parts.len() - 1)
    }

    pub fn boolean(&mut self, kind: BoolKind, a: usize, tool: &FormV) -> Result<(), String> {
        if !matches!(tool.0, Shape::Extrude { .. }) {
            return Err("the second argument must be an extrusion: extrude(profile, a, b)".into());
        }
        let (slot, name) = (self.parts[a].slot, self.parts[a].name.clone());
        // the model's linked boolean works on masses and keeps both operands: the part
        // becomes a mass, the tool a second mass, and the result a third that follows them
        let made = match &self.ops[slot as usize] {
            Op::Geometry(GeoOp::Solid {
                level,
                form: form @ Form::Extrude { .. },
                material: None,
                ..
            }) => Some(Op::Geometry(GeoOp::Mass {
                slot,
                level: level.clone(),
                form: form.clone(),
                name: Some(name.clone()),
            })),
            Op::Geometry(GeoOp::Solid { .. }) => {
                return Err(format!(
                    "part `{name}` is not a plain extrusion without a material: booleans \
                     work on extruded parts and make them masses, which carry no material"
                ))
            }
            _ => None,
        };
        self.count()?;
        if let Some(op) = made {
            self.ops[slot as usize] = op;
        }
        self.ops.push(Op::Geometry(GeoOp::Boolean {
            kind,
            a: Ref::slot(slot),
            b: Tool::Form {
                form: wire_form(tool),
            },
            keep_tool: true,
        }));
        Ok(())
    }

    pub fn group(&mut self, name: &str, parts: &[usize]) -> Result<(), String> {
        check_name(name)?;
        for &i in parts {
            self.parts[i].path.insert(0, name.to_string());
        }
        Ok(())
    }

    /// The first part of a placed group is also a component of `role` at `at`.
    pub fn tag(&mut self, parts: &[usize], role: &str, at: V3) -> Result<(), String> {
        check_name(role)?;
        let Some(&first) = parts.first() else { return Ok(()) };
        let slot = self.parts[first].slot as usize;
        if let Op::Geometry(GeoOp::Solid { role: r, .. }) = &mut self.ops[slot] {
            *r = Some(RoleTag { name: role.to_string(), at: p3(at) });
        }
        Ok(())
    }

    pub fn print(&mut self, s: String) {
        if self.log.len() < LOG_LINES {
            self.log.push(s.chars().take(LOG_CHARS).collect());
        }
    }

    /// Stable keys: `group/sub/name`, repeats numbered `#2`, `#3` in creation order.
    pub fn keys(&self) -> Vec<PartOut> {
        let mut seen: HashMap<String, u32> = HashMap::new();
        self.parts
            .iter()
            .map(|p| {
                let mut base = p.path.join("/");
                if !base.is_empty() {
                    base.push('/');
                }
                base.push_str(&p.name);
                let n = seen.entry(base.clone()).or_insert(0);
                *n += 1;
                let key = if *n == 1 { base } else { format!("{base}#{n}") };
                PartOut { key, slot: p.slot }
            })
            .collect()
    }

    pub fn envelope(&self, actor: Actor, title: &str) -> Envelope {
        let mut env = Envelope::new(actor, title);
        env.groups.push(Group {
            id: 1,
            title: title.to_string(),
            ops: self.ops.clone(),
            depends_on: vec![],
            why: String::new(),
        });
        env
    }
}

fn check_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() || name.contains(['/', '#']) || name.len() > 80 {
        return Err(format!("name `{name}`: 1-80 characters without / or #"));
    }
    Ok(())
}
