//! Flat storage form of sketch rules: a kind name and a list of numbers (point indices as
//! numbers, then values), so a store can keep them without knowing the solver types.

use crate::sketch_solve::{Geo, L, P};

fn l(v: &[f64], i: usize) -> Option<L> {
    Some((p(v, i)?, p(v, i + 1)?))
}

fn p(v: &[f64], i: usize) -> Option<P> {
    let x = *v.get(i)?;
    (x >= 0.0 && x.fract() == 0.0 && x < 1e9).then_some(x as P)
}

fn f(v: &[f64], i: usize) -> Option<f64> {
    v.get(i).copied().filter(|x| x.is_finite())
}

pub fn encode(g: &Geo) -> (&'static str, Vec<f64>) {
    let n = |x: P| x as f64;
    match g {
        Geo::Fix(a, x, y) => ("fix", vec![n(*a), *x, *y]),
        Geo::FixX(a, x) => ("fix_x", vec![n(*a), *x]),
        Geo::FixY(a, y) => ("fix_y", vec![n(*a), *y]),
        Geo::Coincident(a, b) => ("coincident", vec![n(*a), n(*b)]),
        Geo::Horizontal(a) => ("horizontal", vec![n(a.0), n(a.1)]),
        Geo::Vertical(a) => ("vertical", vec![n(a.0), n(a.1)]),
        Geo::Dist(a, b, d) => ("dist", vec![n(*a), n(*b), *d]),
        Geo::PointLine(a, m, d) => ("point_line", vec![n(*a), n(m.0), n(m.1), *d]),
        Geo::LineDist(a, b, d) => ("line_dist", vec![n(a.0), n(a.1), n(b.0), n(b.1), *d]),
        Geo::Parallel(a, b) => ("parallel", vec![n(a.0), n(a.1), n(b.0), n(b.1)]),
        Geo::Perp(a, b) => ("perp", vec![n(a.0), n(a.1), n(b.0), n(b.1)]),
        Geo::Angle(a, b, d) => ("angle", vec![n(a.0), n(a.1), n(b.0), n(b.1), *d]),
        Geo::EqualLen(a, b) => ("equal_len", vec![n(a.0), n(a.1), n(b.0), n(b.1)]),
        Geo::EqualGaps(v) => ("equal_gaps", v.iter().flat_map(|a| [n(a.0), n(a.1)]).collect()),
        Geo::Midpoint(a, m) => ("midpoint", vec![n(*a), n(m.0), n(m.1)]),
        Geo::DistX(a, b, d) => ("dist_x", vec![n(*a), n(*b), *d]),
        Geo::DistY(a, b, d) => ("dist_y", vec![n(*a), n(*b), *d]),
    }
}

pub fn decode(kind: &str, v: &[f64]) -> Option<Geo> {
    Some(match kind {
        "fix" => Geo::Fix(p(v, 0)?, f(v, 1)?, f(v, 2)?),
        "fix_x" => Geo::FixX(p(v, 0)?, f(v, 1)?),
        "fix_y" => Geo::FixY(p(v, 0)?, f(v, 1)?),
        "coincident" => Geo::Coincident(p(v, 0)?, p(v, 1)?),
        "horizontal" => Geo::Horizontal(l(v, 0)?),
        "vertical" => Geo::Vertical(l(v, 0)?),
        "dist" => Geo::Dist(p(v, 0)?, p(v, 1)?, f(v, 2)?),
        "point_line" => Geo::PointLine(p(v, 0)?, l(v, 1)?, f(v, 3)?),
        "line_dist" => Geo::LineDist(l(v, 0)?, l(v, 2)?, f(v, 4)?),
        "parallel" => Geo::Parallel(l(v, 0)?, l(v, 2)?),
        "perp" => Geo::Perp(l(v, 0)?, l(v, 2)?),
        "angle" => Geo::Angle(l(v, 0)?, l(v, 2)?, f(v, 4)?),
        "equal_len" => Geo::EqualLen(l(v, 0)?, l(v, 2)?),
        "equal_gaps" if v.len() % 2 == 0 => {
            Geo::EqualGaps((0..v.len() / 2).map(|i| l(v, 2 * i)).collect::<Option<_>>()?)
        }
        "midpoint" => Geo::Midpoint(p(v, 0)?, l(v, 1)?),
        "dist_x" => Geo::DistX(p(v, 0)?, p(v, 1)?, f(v, 2)?),
        "dist_y" => Geo::DistY(p(v, 0)?, p(v, 1)?, f(v, 2)?),
        _ => return None,
    })
}
