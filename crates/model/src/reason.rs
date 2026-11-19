// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Model errors as `ddc_ops::Rejection` (stable code, expected/got, refs) for the op executor
//! and AI agents. Paths here are relative to the op payload; the executor adds
//! `/groups/{g}/ops/{o}` with `Rejection::in_op`.

use ddc_ops::{Cat, Code, Ref, Rejection};

use crate::{Error, Model, ModifyError, OpeningSpec, Target};

const EPS: f64 = 1e-6;

/// Stable ref of a modify target; `None` for targets without an op category (views, room
/// separation lines).
pub fn target_ref(t: Target) -> Option<Ref> {
    let (cat, id) = match t {
        Target::Wall(i) => (Cat::Wall, i),
        Target::Opening(i) => (Cat::Opening, i),
        Target::Curtain(i) => (Cat::CurtainWall, i),
        Target::Slab(i) => (Cat::Slab, i),
        Target::Room(i) => (Cat::Room, i),
        Target::Dim(i) => (Cat::Annotation, i),
        Target::Component(i) => (Cat::Component, i),
        Target::MepRun(i) => (Cat::MepRun, i),
        Target::Solid(i) => (Cat::Solid, i),
        Target::Mesh(i) => (Cat::Mesh, i),
        Target::Column(i) => (Cat::Column, i),
        Target::Beam(i) => (Cat::Beam, i),
        Target::Ceiling(i) => (Cat::Ceiling, i),
        Target::Ramp(i) => (Cat::Ramp, i),
        Target::Roof(i) => (Cat::Roof, i),
        Target::Shaft(i) => (Cat::Shaft, i),
        Target::Grid(i) => (Cat::Grid, i),
        Target::RefPlane(i) => (Cat::RefPlane, i),
        Target::Footing(i) => (Cat::Footing, i),
        Target::StripFooting(i) => (Cat::StripFooting, i),
        Target::Raft(i) => (Cat::Raft, i),
        Target::Brace(i) => (Cat::Brace, i),
        Target::Truss(i) => (Cat::Truss, i),
        Target::BeamSystem(i) => (Cat::BeamSystem, i),
        Target::Mass(i) => (Cat::Mass, i),
        Target::Topo(i) => (Cat::Topography, i),
        #[allow(unreachable_patterns)]
        _ => return None,
    };
    Some(Ref::id(cat, id))
}

fn with_target(r: Rejection, t: Target) -> Rejection {
    match target_ref(t) {
        Some(x) => r.with_ref(x),
        None => r,
    }
}

/// Ref of a table row named by its table (`Error::Referenced`, `OpeningOutsideHost`).
fn row_ref(kind: &str, id: i64) -> Option<Ref> {
    let cat = match kind {
        "curtain" => Some(Cat::CurtainWall),
        "topo" => Some(Cat::Topography),
        "dim" | "dimension" => Some(Cat::Annotation),
        k => Cat::from_key(k),
    }?;
    Some(Ref::id(cat, id))
}

pub fn modify_rejection(e: &ModifyError) -> Rejection {
    match e {
        ModifyError::Empty => Rejection::new(Code::EmptySelection),
        ModifyError::NotFound(t) => with_target(Rejection::new(Code::NotFound), *t),
        ModifyError::Unsupported(what, t) => {
            with_target(Rejection::new(Code::Unsupported).got(*what), *t)
        }
        ModifyError::Parallel => Rejection::new(Code::Parallel),
        ModifyError::NotParallel => Rejection::new(Code::NotParallel),
        ModifyError::BadCount => Rejection::new(Code::BadCount).expected(">= 2"),
        ModifyError::TooMany(n) => Rejection::new(Code::TooMany).expected(format!("<= {n}")),
        ModifyError::Degenerate(t) => with_target(Rejection::new(Code::Degenerate), *t),
        ModifyError::OpeningOutside(id) => {
            Rejection::new(Code::OutsideHost).with_ref(Ref::id(Cat::Opening, *id))
        }
        ModifyError::ComponentOutside(id) => {
            Rejection::new(Code::OutsideHost).with_ref(Ref::id(Cat::Component, *id))
        }
        ModifyError::OffWall(id) => Rejection::new(Code::OffHost).with_ref(Ref::id(Cat::Wall, *id)),
        ModifyError::OnBoundary => Rejection::new(Code::Geometry).got("point on the boundary"),
    }
}

/// Code for free-text `Error::Invalid` from the `check_*` helpers; the text stays in `got`.
fn invalid_code(s: &str) -> Code {
    let t = s.to_ascii_lowercase();
    if t.contains("non-finite") || t.contains("must be finite") {
        Code::NonFinite
    } else if t.contains("not found") || t.starts_with("no such ") || t.contains(": no ") {
        Code::NotFound
    } else if t.contains("outside") {
        Code::OutsideHost
    } else if t.contains("kind '") {
        Code::BadKind
    } else if t.contains("cells") && s.contains('\u{d7}') {
        Code::TooMany
    } else if t.starts_with("arc ") || t.contains("bad geometry") || t.contains("half circle") {
        Code::Geometry
    } else if t.contains("cancelled") || t.contains("busy") || t.contains("temp space") {
        Code::Internal
    } else {
        Code::Invalid
    }
}

impl Error {
    /// The same failure as a rejection a model can act on.
    pub fn rejection(&self) -> Rejection {
        match self {
            Error::Modify(m) => modify_rejection(m),
            Error::Invalid(s) => Rejection::new(invalid_code(s)).got(s.clone()),
            Error::Geom(s) => Rejection::new(Code::Geometry).got(s.clone()),
            Error::Degenerate(s) => Rejection::new(Code::Degenerate).got(s.clone()),
            Error::Conflict(s) => Rejection::new(Code::Conflict).got(s.clone()),
            Error::Corrupt(s) => Rejection::new(Code::Corrupt).got(s.clone()),
            Error::Sheet(e) => {
                use crate::sheets::{SheetErr, SheetKind};
                // same codes as the free-text errors these replaced (callers branch on them)
                let code = match e {
                    SheetErr::NoSuch(_) => Code::NotFound,
                    SheetErr::BadCut | SheetErr::BadPosition => Code::NonFinite,
                    _ => match e.kind() {
                        SheetKind::Conflict => Code::Conflict,
                        SheetKind::Invalid => Code::Invalid,
                    },
                };
                Rejection::new(code).got(e.to_string())
            }
            Error::Field(e) => Rejection::new(Code::Invalid).got(e.to_string()),
            Error::Stage(e) => Rejection::new(Code::Conflict).got(e.to_string()),
            Error::Referenced { kind, id, by } => {
                let mut r = Rejection::new(Code::Referenced);
                if let Some(x) = row_ref(kind, *id) {
                    r = r.with_ref(x);
                }
                let labels: Vec<String> = by.iter().map(|(k, i)| format!("{k}:{i}")).collect();
                r.got(labels.join(", "))
            }
            Error::OpeningOutsideHost { kind, host } => {
                let r = Rejection::new(Code::OutsideHost);
                match row_ref(kind, *host) {
                    Some(x) => r.with_ref(x),
                    None => r.got(format!("{kind}:{host}")),
                }
            }
            Error::NewerVersion { found, supported } => Rejection::new(Code::NewerFile)
                .expected(format!("<= {supported}"))
                .got(found.to_string()),
            #[allow(unreachable_patterns)]
            e => Rejection::new(Code::Internal).got(e.to_string()),
        }
    }
}

impl From<&Error> for Rejection {
    fn from(e: &Error) -> Rejection {
        e.rejection()
    }
}

/// Why opening `o` does not fit wall `wall` (length `len`, height `wall_h`), the same test as
/// `Model::opening_fits`, naming the field to change and its allowed range in metres. A wall
/// with a top profile clips the head, so only the sill and along range apply then.
pub fn opening_fit(
    wall: i64,
    len: f64,
    wall_h: f64,
    top_profile: bool,
    o: &OpeningSpec,
) -> Option<Rejection> {
    let along_ok = o.along >= -EPS && o.along + o.width <= len + EPS;
    let r = if !along_ok && o.width > len + EPS {
        Rejection::out_of_range("/width", 0.0, len, "m", o.width)
    } else if !along_ok {
        Rejection::out_of_range("/along", 0.0, len - o.width, "m", o.along)
    } else if o.sill < -EPS {
        if top_profile {
            Rejection::at_least("/sill", 0.0, "m", o.sill)
        } else {
            Rejection::out_of_range("/sill", 0.0, (wall_h - o.height).max(0.0), "m", o.sill)
        }
    } else if !top_profile && o.sill + o.height > wall_h + EPS {
        if o.height > wall_h + EPS {
            Rejection::out_of_range("/height", 0.0, wall_h, "m", o.height)
        } else {
            Rejection::out_of_range("/sill", 0.0, wall_h - o.height, "m", o.sill)
        }
    } else {
        return None;
    };
    Some(r.with_ref(Ref::id(Cat::Wall, wall)))
}

impl Model {
    /// `opening_fits` as a rejection: `Ok(None)` when the opening fits.
    pub fn opening_fit_rejection(
        &self,
        wall: i64,
        o: &OpeningSpec,
    ) -> Result<Option<Rejection>, Error> {
        let len = self.wall_length(wall)?;
        let (height, profile): (f64, Option<String>) = self.db.query_row(
            "SELECT height, top_profile FROM wall WHERE id=?1",
            [wall],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(opening_fit(wall, len, height, profile.is_some(), o))
    }
}

#[cfg(test)]
#[path = "reason_tests.rs"]
mod tests;
