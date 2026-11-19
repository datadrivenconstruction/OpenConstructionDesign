//! Checked booleans for typed requests: both operands must be closed solids, and an empty or
//! non-manifold result is refused with a reason instead of being returned as a silent mesh.

use crate::csg::{Body, BodyError, BoolOp};
use crate::math::Tol;
use crate::nurbs::Loss;

impl BoolOp {
    pub fn name(self) -> &'static str {
        match self {
            BoolOp::Union => "union",
            BoolOp::Subtract => "subtract",
            BoolOp::Intersect => "intersect",
        }
    }

    pub fn parse(s: &str) -> Option<BoolOp> {
        match s {
            "union" => Some(BoolOp::Union),
            "subtract" | "cut" => Some(BoolOp::Subtract),
            "intersect" => Some(BoolOp::Intersect),
            _ => None,
        }
    }
}

/// Why a boolean was refused.
#[derive(Clone, Debug, PartialEq)]
pub enum BoolReason {
    /// Operand `a` or `b` is not a closed solid (open mesh, NaN, no faces).
    OpenOperand { which: char, err: BodyError },
    /// The result has no volume (disjoint intersection, tool swallows the whole body).
    EmptyResult { op: BoolOp },
    /// The result is not a manifold solid (bodies touch along an edge only).
    InvalidResult { op: BoolOp, err: BodyError },
}

impl std::fmt::Display for BoolReason {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            BoolReason::OpenOperand { which, err } => {
                write!(f, "operand {which} is not a closed solid: {err}")
            }
            BoolReason::EmptyResult { op } => write!(f, "{} result is empty", op.name()),
            BoolReason::InvalidResult { op, err } => {
                write!(f, "{} result is not a valid solid: {err}", op.name())
            }
        }
    }
}

/// Kernel that built a boolean result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoolPath {
    /// Triangle mesh boolean with exact predicates (large faceted pairs).
    Mesh,
    /// Planar BSP boolean (small pairs, or a refused mesh pair).
    Planar,
}

/// `a op b` without checks: large faceted pairs take the mesh path; the BSP path stays the
/// general one. A mesh refusal is stated as a [`Loss`], never silent.
pub fn routed_boolean(a: &Body, b: &Body, op: BoolOp, tol: Tol) -> (Body, BoolPath, Vec<Loss>) {
    let faces = a.polys.len() + b.polys.len();
    if faces < crate::csg2::MIN_FACES {
        return (a.boolean(b, op, tol), BoolPath::Planar, vec![]);
    }
    match crate::csg2::boolean_bodies(a, b, op, tol) {
        Ok(out) => (out, BoolPath::Mesh, vec![]),
        Err(e) => {
            let note = Loss {
                what: e.fallback_note(),
                measure: faces as f64,
            };
            (a.boolean(b, op, tol), BoolPath::Planar, vec![note])
        }
    }
}

/// Volume below this (m^3) counts as empty: slivers left by touching faces.
const EMPTY_VOLUME: f64 = 1e-9;

/// `a op b` with operand and result checks; the bodies are not changed.
pub fn checked_boolean(a: &Body, b: &Body, op: BoolOp, tol: Tol) -> Result<Body, BoolReason> {
    checked_boolean_noted(a, b, op, tol).map(|(out, _)| out)
}

/// [`checked_boolean`] with its receipt lines: when the mesh path refuses a large operand
/// pair and the planar BSP path runs instead, the refusal is stated as a [`Loss`].
pub fn checked_boolean_noted(
    a: &Body,
    b: &Body,
    op: BoolOp,
    tol: Tol,
) -> Result<(Body, Vec<Loss>), BoolReason> {
    for (which, body) in [('a', a), ('b', b)] {
        body.check_closed(tol)
            .map_err(|err| BoolReason::OpenOperand { which, err })?;
    }
    let (out, _, notes) = routed_boolean(a, b, op, tol);
    if out.is_empty() || out.volume().abs() <= EMPTY_VOLUME {
        return Err(BoolReason::EmptyResult { op });
    }
    out.validate(tol)
        .map_err(|err| BoolReason::InvalidResult { op, err })?;
    Ok((out, notes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mass::cuboid;
    use crate::math::v3;

    const TOL: Tol = Tol {
        length: 1e-7,
        angle: 1e-9,
    };

    fn bx(lo: (f64, f64, f64), hi: (f64, f64, f64)) -> Body {
        cuboid(v3(lo.0, lo.1, lo.2), v3(hi.0, hi.1, hi.2))
    }

    #[test]
    fn three_ops_give_expected_volumes() {
        let a = bx((0., 0., 0.), (2., 2., 2.));
        let b = bx((1., 0., 0.), (3., 2., 2.));
        let u = checked_boolean(&a, &b, BoolOp::Union, TOL).unwrap();
        let s = checked_boolean(&a, &b, BoolOp::Subtract, TOL).unwrap();
        let i = checked_boolean(&a, &b, BoolOp::Intersect, TOL).unwrap();
        assert!((u.volume() - 12.0).abs() < 1e-9, "{}", u.volume());
        assert!((s.volume() - 4.0).abs() < 1e-9, "{}", s.volume());
        assert!((i.volume() - 4.0).abs() < 1e-9, "{}", i.volume());
    }

    #[test]
    fn boolean_on_open_mesh_rejected() {
        let a = bx((0., 0., 0.), (2., 2., 2.));
        let mut open = bx((1., 1., 1.), (3., 3., 3.));
        open.polys.pop();
        let before = (a.clone(), open.clone());
        let r = checked_boolean(&a, &open, BoolOp::Union, TOL);
        assert!(
            matches!(r, Err(BoolReason::OpenOperand { which: 'b', .. })),
            "{r:?}"
        );
        assert!(r.unwrap_err().to_string().contains("not a closed solid"));
        let r = checked_boolean(&open, &a, BoolOp::Subtract, TOL);
        assert!(matches!(r, Err(BoolReason::OpenOperand { which: 'a', .. })));
        assert_eq!((a, open), before);
    }

    #[test]
    fn empty_intersection_refused_with_reason() {
        let a = bx((0., 0., 0.), (1., 1., 1.));
        let b = bx((5., 5., 5.), (6., 6., 6.));
        let r = checked_boolean(&a, &b, BoolOp::Intersect, TOL);
        assert_eq!(r, Err(BoolReason::EmptyResult { op: BoolOp::Intersect }));
        assert_eq!(r.unwrap_err().to_string(), "intersect result is empty");
        // a tool that swallows the body leaves nothing either
        let big = bx((-1., -1., -1.), (2., 2., 2.));
        let r = checked_boolean(&a, &big, BoolOp::Subtract, TOL);
        assert_eq!(r, Err(BoolReason::EmptyResult { op: BoolOp::Subtract }));
    }

    #[test]
    fn mesh_path_refusal_is_stated_not_silent() {
        // operand a: two overlapping boxes in one body (intersects itself); b: a fine sphere
        // far away, so the operands are large enough for the mesh path
        let mut a = bx((0., 0., 0.), (1., 1., 1.));
        a.polys.extend(bx((0.5, 0.3, 0.6), (1.5, 1.3, 1.6)).polys);
        let mut s = crate::mesh::Mesh::default();
        let (nu, nv) = (48usize, 25usize);
        s.positions.push(v3(50., 0., -1.));
        for j in 1..nv {
            let th = std::f64::consts::PI * j as f64 / nv as f64 - std::f64::consts::FRAC_PI_2;
            for i in 0..nu {
                let ph = std::f64::consts::TAU * i as f64 / nu as f64;
                s.positions.push(v3(
                    50. + th.cos() * ph.cos(),
                    th.cos() * ph.sin(),
                    th.sin(),
                ));
            }
        }
        s.positions.push(v3(50., 0., 1.));
        let top = (s.positions.len() - 1) as u32;
        let ring = |j: usize, i: usize| (1 + (j - 1) * nu + i % nu) as u32;
        for i in 0..nu {
            s.triangles.push([0, ring(1, i + 1), ring(1, i)]);
            s.triangles.push([top, ring(nv - 1, i), ring(nv - 1, i + 1)]);
        }
        for j in 1..nv - 1 {
            for i in 0..nu {
                let (p, q) = (ring(j, i), ring(j, i + 1));
                let (r, t) = (ring(j + 1, i + 1), ring(j + 1, i));
                s.triangles.push([p, q, r]);
                s.triangles.push([p, r, t]);
            }
        }
        let b = Body::from_mesh(&s, 20);
        assert!(a.polys.len() + b.polys.len() >= crate::csg2::MIN_FACES);
        let (out, notes) = checked_boolean_noted(&a, &b, BoolOp::Subtract, TOL).unwrap();
        assert_eq!(out, a);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].what.contains("intersects itself"), "{}", notes[0].what);
        assert_eq!(notes[0].measure, (a.polys.len() + b.polys.len()) as f64);
        // small operands never try the mesh path: no note
        let (_, quiet) = checked_boolean_noted(
            &bx((0., 0., 0.), (2., 2., 2.)),
            &bx((1., 1., 1.), (3., 3., 3.)),
            BoolOp::Union,
            TOL,
        )
        .unwrap();
        assert!(quiet.is_empty());
    }

    #[test]
    fn op_names_round_trip() {
        for op in [BoolOp::Union, BoolOp::Subtract, BoolOp::Intersect] {
            assert_eq!(BoolOp::parse(op.name()), Some(op));
        }
        assert_eq!(BoolOp::parse("cut"), Some(BoolOp::Subtract));
        assert_eq!(BoolOp::parse("xor"), None);
    }
}
