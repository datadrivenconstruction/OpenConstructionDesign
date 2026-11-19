//! Vertex removal on boundary elements (slab, ceiling, roof footprint, shaft): the element
//! always stays, the edit is refused with a reason when the outline or a hosted item would break.

use super::*;
use ddc_geom::predicates::{point_in_ring, segments_meet};

fn ring_v2(r: &[(f64, f64)]) -> Vec<V2> {
    r.iter().map(|&p| pv(p)).collect()
}

/// Removes point `i`; a ring keeps at least three points and stays simple.
fn drop_point(what: &str, ring: &mut Vec<(f64, f64)>, i: usize) -> Result<(), Error> {
    if i >= ring.len() {
        return Err(bad());
    }
    if ring.len() <= 3 {
        return Err(degenerate(&format!("{what} needs at least three points")));
    }
    ring.remove(i);
    simple(what, &ring_v2(ring))
}

fn edges(r: &[V2]) -> impl Iterator<Item = (V2, V2)> + '_ {
    (0..r.len()).map(move |i| (r[i], r[(i + 1) % r.len()]))
}

fn touch(a: &[V2], b: &[V2]) -> bool {
    edges(a).any(|(p, q)| edges(b).any(|(r, s)| segments_meet(p, q, r, s)))
}

/// i18n keys (delsafe.json) of the `rings_ok` refusals, in the user's language.
pub(super) const RING_SHAPE: &str = "delsafe.ring.opening_shape";
pub(super) const RING_BOUNDARY: &str = "delsafe.ring.opening_boundary";
pub(super) const RING_OVERLAP: &str = "delsafe.ring.openings_overlap";
pub(super) const RING_KEYS: [&str; 3] = [RING_SHAPE, RING_BOUNDARY, RING_OVERLAP];

fn refuse(key: &str) -> Error {
    degenerate(&crate::tr(key, &[]))
}

/// True for a `rings_ok` refusal text: it is already in a UI language.
pub fn is_ring_refusal(text: &str) -> bool {
    use ddc_ui::i18n::Lang;
    RING_KEYS.iter().any(|k| [Lang::En, Lang::Ru].into_iter().any(|l| crate::tr_in(l, k, &[]) == text))
}

/// Every hole is a simple ring, lies strictly inside the outer ring (edges too) and
/// neither crosses, touches nor contains another hole. Checked as a whole: no partial edit.
pub(super) fn rings_ok(outer: &[V2], holes: &[Vec<(f64, f64)>]) -> Result<(), Error> {
    let hs: Vec<Vec<V2>> = holes.iter().map(|h| ring_v2(h)).collect();
    for (k, h) in hs.iter().enumerate() {
        if simple("opening", h).is_err() {
            return Err(refuse(RING_SHAPE));
        }
        if h.iter().any(|&p| point_in_ring(p, outer) <= 0) || touch(h, outer) {
            return Err(refuse(RING_BOUNDARY));
        }
        for o in &hs[k + 1..] {
            if touch(h, o) || point_in_ring(h[0], o) >= 0 || point_in_ring(o[0], h) >= 0 {
                return Err(refuse(RING_OVERLAP));
            }
        }
    }
    Ok(())
}

/// Holes and hosted items must stay inside the new outer ring.
fn inside_ok(
    m: &Model,
    kind: Kind,
    id: i64,
    outer: &[(f64, f64)],
    holes: &[Vec<(f64, f64)>],
) -> Result<(), Error> {
    let o = ring_v2(outer);
    let out = |what: &str| degenerate(&format!("{what} would fall outside the new boundary"));
    if holes
        .iter()
        .flatten()
        .any(|&p| point_in_ring(pv(p), &o) < 0)
    {
        return Err(out("an opening"));
    }
    rings_ok(&o, holes)?;
    for c in m.components(None)? {
        let s = m.component(c)?;
        let hosted = match s.host {
            CompHost::Floor(h) => kind == Kind::Slab && h == id,
            CompHost::Ceiling(h) => kind == Kind::Ceiling && h == id,
            _ => false,
        };
        if hosted && point_in_ring(v2(s.at.0, s.at.1), &o) < 0 {
            return Err(out("a hosted item"));
        }
    }
    Ok(())
}

/// Removes the vertex named by `role` (`Vertex` of the outline or `HoleVertex`) in one model op.
pub(super) fn remove(m: &mut Model, kind: Kind, id: i64, role: &Role) -> Result<(), Error> {
    let (hole, i) = match *role {
        Role::Vertex(i) => (None, i),
        Role::HoleVertex(h, i) => (Some(h), i),
        _ => return Err(bad()),
    };
    match kind {
        Kind::Slab => {
            let mut rings = m.slab_rings(id)?;
            let mut outer = rings.remove(0);
            match hole {
                None => {
                    drop_point("slab boundary", &mut outer, i)?;
                    inside_ok(m, kind, id, &outer, &rings)?;
                }
                Some(h) => {
                    drop_point("slab opening", rings.get_mut(h).ok_or_else(bad)?, i)?;
                    rings_ok(&ring_v2(&outer), &rings)?;
                }
            }
            m.set_slab_boundary(id, &outer, &rings)
        }
        Kind::Ceiling => {
            let mut s = ceiling_spec(m, id)?;
            match hole {
                None => {
                    drop_point("ceiling boundary", &mut s.outer, i)?;
                    inside_ok(m, kind, id, &s.outer, &s.holes)?;
                }
                Some(h) => {
                    drop_point("ceiling opening", s.holes.get_mut(h).ok_or_else(bad)?, i)?;
                    rings_ok(&ring_v2(&s.outer), &s.holes)?;
                }
            }
            m.update_ceiling(id, &s)
        }
        Kind::Shaft if hole.is_none() => {
            let mut s = m.shaft(id)?;
            drop_point("shaft boundary", &mut s.ring, i)?;
            m.update_shaft(id, &s)
        }
        Kind::Roof if hole.is_none() => {
            let mut s = m.roof(id)?;
            let ring = match &mut s.kind {
                RoofKind::Footprint { ring, slopes, .. } => {
                    drop_point("roof footprint", ring, i)?;
                    // the merged edge keeps the slope of the edge before the vertex
                    if slopes.len() == ring.len() + 1 {
                        slopes.remove(i);
                    }
                    ring
                }
                RoofKind::Arrow { ring, .. } => {
                    drop_point("roof footprint", ring, i)?;
                    ring
                }
                RoofKind::Extrusion { .. } => return Err(bad()),
            };
            let r = ring_v2(ring);
            for k in m.skylights(id)? {
                if point_in_ring(pv(m.skylight(k)?.center), &r) < 0 {
                    return Err(degenerate("a skylight would fall outside the new footprint"));
                }
            }
            m.update_roof(id, &s)
        }
        _ => Err(bad()),
    }
}

#[cfg(test)]
#[path = "grips_ring_tests.rs"]
mod tests;
