//! Spline walls in `ddc-ops` envelopes. A spline wall is created as a `wall` with the field
//! `through` (plan points between `a` and `b`) and is addressed afterwards by any of its
//! pieces (`wall:N`): `set`, `set_type`, `delete` and `transform` act on the whole spline wall.

use ddc_ops::{Cat, Dim, Field, Len, Place, Ref};

use super::set::{len, opt_len, positive_len, row_id};
use super::{at_point, bad_value, level_row, resolve_as, unsupported, Fail, Run, EPS};
use crate::{Model, SplineWallSpec, Target};

/// Fields of a spline wall as a whole; the rest of a `set` goes to the piece row.
const OWN: &[&str] = &["height", "base_offset", "offset0", "wall_type", "level", "through", "tolerance"];

/// `[[x, y], ...]` plan points; metres without a unit, else in its length unit.
fn points_of(f: &Field) -> Result<Vec<(f64, f64)>, Fail> {
    let bad = || bad_value(format!("`{}` needs a list of [x, y] plan points", f.name));
    let k = match f.value.unit {
        None => 1.0,
        Some(u) if u.dim() == Dim::Length => u.to_si(1.0),
        Some(_) => return Err(bad()),
    };
    let mut out = vec![];
    for p in f.value.v.as_array().ok_or_else(bad)? {
        match p.as_array().map(Vec::as_slice) {
            Some([x, y]) => match (x.as_f64(), y.as_f64()) {
                (Some(x), Some(y)) if x.is_finite() && y.is_finite() => out.push((x * k, y * k)),
                _ => return Err(bad()),
            },
            _ => return Err(bad()),
        }
    }
    if out.is_empty() {
        return Err(bad_value(format!("`{}` needs at least one point between the wall ends", f.name)));
    }
    Ok(out)
}

pub(super) fn is_spline(fields: &[Field]) -> bool {
    fields.iter().any(|f| f.name == "through")
}

/// `create` of a wall with `through`: a spline wall through `a`, the points and `b`. The slot
/// names its first piece.
#[allow(clippy::too_many_arguments)]
pub(super) fn create(
    m: &mut Model,
    run: &mut Run,
    slot: u32,
    level: &Ref,
    (a, b): (&Place, &Place),
    height: &Len,
    ty: Option<&Ref>,
    fields: &[Field],
) -> Result<(), Fail> {
    let (mut through, mut tol) = (vec![], None);
    for f in fields {
        match f.name.as_str() {
            "through" => through = points_of(f)?,
            "tolerance" => tol = Some(positive_len(f)?),
            other => {
                return Err(unsupported(format!(
                    "`{other}` is not taken by a spline wall on create (fields: through, tolerance)"
                )))
            }
        }
    }
    let lv = resolve_as(m, run, level, Cat::Level)?;
    let (_, elev) = level_row(m, lv)?;
    let (pa, pb) = (at_point(a)?, at_point(b)?);
    if (pa[2] - pb[2]).abs() > EPS {
        return Err(bad_value("wall ends have different z; sloped bases are not supported"));
    }
    let wall_type = match ty {
        Some(t) => resolve_as(m, run, t, Cat::WallType)?,
        None => super::default_wall_type(&m.db)?,
    };
    let mut pts = vec![(pa[0], pa[1])];
    pts.extend(through);
    pts.push((pb[0], pb[1]));
    let mut s = SplineWallSpec::through(lv, wall_type, &pts, height.to_m())?;
    s.base_offset = pa[2] - elev;
    if let Some(t) = tol {
        s.tol = t;
    }
    let id = m.add_spline_wall(&s)?;
    let first = m.spline_wall_pieces(id)?[0];
    run.made(slot, Cat::Wall, first);
    Ok(())
}

/// Splits a `set` on a piece: (fields of the spline wall, fields of the piece row).
pub(super) fn split(fields: &[Field]) -> (Vec<Field>, Vec<Field>) {
    fields.iter().cloned().partition(|f| OWN.contains(&f.name.as_str()))
}

/// `set` of the spline wall's own fields: one rewrite of the whole wall. `through` replaces
/// the points between the ends, which stay.
pub(super) fn set(m: &mut Model, run: &mut Run, spline: i64, own: &[Field]) -> Result<(), Fail> {
    if own.is_empty() {
        return Ok(());
    }
    let mut s = m.spline_wall(spline)?;
    for f in own {
        match f.name.as_str() {
            "height" => s.height = positive_len(f)?,
            "base_offset" => s.base_offset = len(f)?,
            "offset0" => s.offset0 = opt_len(f)?,
            "wall_type" => s.wall_type = row_id(m, f, "wall_type")?,
            "level" => s.level = row_id(m, f, "level")?,
            "tolerance" => s.tol = positive_len(f)?,
            "through" => {
                let k = s.axis.knots();
                let (p0, p1) = (s.axis.point(k[0]), s.axis.point(k[k.len() - 1]));
                let mut pts = vec![(p0.x, p0.y)];
                pts.extend(points_of(f)?);
                pts.push((p1.x, p1.y));
                s.axis = SplineWallSpec::through(s.level, s.wall_type, &pts, s.height)?.axis;
            }
            other => return Err(unsupported(format!("`{other}` is not a spline wall field"))),
        }
    }
    m.update_spline_wall(spline, &s)?;
    let first = m.spline_wall_pieces(spline)?[0];
    run.receipt.changed.push(Ref::id(Cat::Wall, first));
    Ok(())
}

/// `set_type` of a spline wall.
pub(super) fn set_type(m: &mut Model, spline: i64, wall_type: i64) -> Result<(), Fail> {
    let s = m.spline_wall(spline)?;
    m.update_spline_wall(spline, &SplineWallSpec { wall_type, ..s })?;
    Ok(())
}

/// Wall rows split into plain walls and the spline walls their pieces belong to (each once).
pub(super) fn split_walls(m: &Model, walls: &[i64]) -> Result<(Vec<i64>, Vec<i64>), Fail> {
    let (mut plain, mut splines) = (vec![], vec![]);
    for &w in walls {
        match m.spline_of_wall(w)? {
            Some(s) if !splines.contains(&s) => splines.push(s),
            Some(_) => {}
            None => plain.push(w),
        }
    }
    Ok((plain, splines))
}

/// Transform targets with every piece of a spline wall any piece of which is among them.
pub(super) fn whole(m: &Model, ts: &mut Vec<Target>) -> Result<(), Fail> {
    let walls: Vec<i64> = ts.iter().filter_map(|t| if let Target::Wall(w) = t { Some(*w) } else { None }).collect();
    for s in split_walls(m, &walls)?.1 {
        for p in m.spline_wall_pieces(s)? {
            if !ts.contains(&Target::Wall(p)) {
                ts.push(Target::Wall(p));
            }
        }
    }
    Ok(())
}
