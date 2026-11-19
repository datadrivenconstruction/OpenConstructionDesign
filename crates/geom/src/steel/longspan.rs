//! Long-span frames whose supports are columns of the frame itself: a truss hall (plane trusses
//! on two column lines, purlins, ties and end-bay bracing), a tied arch bridge on end piers,
//! and a cantilever roof ring (radial trusses on a front and a back column, the back-span
//! holding the reach down). Every column top is a node of the members it carries.

use super::lattice::{Frame, MemRole, Pattern, Secs};
use crate::math::{v3, V3};

/// Truss depth of a span: span / 12, within 1.5..10 m.
pub fn hall_depth(span: f64) -> f64 {
    (span / 12.0).clamp(1.5, 10.0)
}

/// Panels of a length at about `panel` each, at least `min`, an even count.
fn panels(len: f64, panel: f64, min: usize) -> usize {
    let n = ((len / panel.max(0.5)).ceil() as usize).max(min);
    n + n % 2
}

/// Truss hall: `bays` + 1 parallel-chord trusses of `span` at `spacing`, bottom chord at
/// `eaves` on a column at each end, top chord [`hall_depth`] higher.
pub fn truss_hall(span: f64, eaves: f64, bays: usize, spacing: f64, panel: f64, pat: Pattern) -> Result<Frame, String> {
    truss_hall_split(span, eaves, bays, spacing, panel, pat, &|_| vec![])
}

/// [`truss_hall`] whose columns of frame `b` are split at the heights `split(b)`, so a bracket
/// or a beam framing in there meets a node.
pub fn truss_hall_split(span: f64, eaves: f64, bays: usize, spacing: f64, panel: f64, pat: Pattern, split: &dyn Fn(usize) -> Vec<f64>) -> Result<Frame, String> {
    if !(span >= 6.0 && eaves >= 3.0 && bays >= 1 && spacing >= 2.0) {
        return Err("truss_hall: span >= 6 m, eaves >= 3 m, bays >= 1, spacing >= 2 m".into());
    }
    let d = hall_depth(span);
    let n = panels(span, panel, 4);
    let big = span > 40.0;
    let s = Secs {
        chord: if big { "HEB 500" } else { "HEB 300" },
        diag: if big { "CHS 219.1x10" } else { "CHS 168.3x8" },
        strut: "CHS 168.3x8",
    };
    let col = if big { "HEB 600" } else { "HEB 400" };
    let mut f = Frame::new();
    let x = |i: usize| span * i as f64 / n as f64;
    for b in 0..=bays {
        let y = b as f64 * spacing;
        let top: Vec<V3> = (0..=n).map(|i| v3(x(i), y, eaves + d)).collect();
        let bot: Vec<V3> = (0..=n).map(|i| v3(x(i), y, eaves)).collect();
        f.truss(&top, &bot, pat, s, MemRole::Chord);
        f.add(top[n], bot[n], s.strut, MemRole::Strut, v3(0.0, 1.0, 0.0));
        let mut zs: Vec<f64> = split(b).into_iter().filter(|z| *z > 1e-3 && *z < eaves - 1e-3).collect();
        zs.sort_by(f64::total_cmp);
        zs.dedup_by(|a, b| (*a - *b).abs() < 1e-3);
        zs.insert(0, 0.0);
        zs.push(eaves);
        for &cx in &[0.0, span] {
            for w in zs.windows(2) {
                f.add(v3(cx, y, w[0]), v3(cx, y, w[1]), col, MemRole::Column, v3(0.0, 1.0, 0.0));
            }
        }
    }
    for b in 0..bays {
        let (y0, y1) = (b as f64 * spacing, (b + 1) as f64 * spacing);
        let end = b == 0 || b + 1 == bays;
        for i in 0..=n {
            f.add(v3(x(i), y0, eaves + d), v3(x(i), y1, eaves + d), "IPE 300", MemRole::Beam, v3(0.0, 0.0, 1.0));
            if i % 2 == 0 {
                f.add(v3(x(i), y0, eaves), v3(x(i), y1, eaves), s.strut, MemRole::Strut, v3(0.0, 0.0, 1.0));
            }
            if end && i < n {
                f.add(v3(x(i), y0, eaves + d), v3(x(i + 1), y1, eaves + d), "CHS 114.3x6", MemRole::PlanBrace, v3(0.0, 0.0, 1.0));
            }
        }
        if end {
            for &cx in &[0.0, span] {
                f.add(v3(cx, y0, 0.0), v3(cx, y1, eaves), "CHS 168.3x8", MemRole::Diagonal, v3(1.0, 0.0, 0.0));
                f.add(v3(cx, y1, 0.0), v3(cx, y0, eaves), "CHS 168.3x8", MemRole::Diagonal, v3(1.0, 0.0, 0.0));
            }
        }
    }
    Ok(f)
}

/// Tied arch bridge of `span`: two parabolic arches of `rise` over deck girders at `deck_z`,
/// `width` apart, vertical hangers at every panel point, cross beams, deck and top wind
/// bracing, and a pier column under each girder end.
pub fn arch_bridge(span: f64, rise: f64, deck_z: f64, width: f64, panel: f64) -> Result<Frame, String> {
    if !(span >= 10.0 && rise >= 2.0 && deck_z >= 1.0 && width >= 2.0) {
        return Err("arch_bridge: span >= 10 m, rise >= 2 m, deck >= 1 m up, width >= 2 m".into());
    }
    let n = panels(span, panel, 6);
    let mut f = Frame::new();
    let x = |i: usize| span * i as f64 / n as f64;
    let z = |i: usize| {
        let t = i as f64 / n as f64;
        deck_z + 4.0 * rise * t * (1.0 - t)
    };
    let (arch, girder) = ("PG 900/600/40/600/40/20", "PG 1200/500/35/500/35/16");
    for &y in &[0.0, width] {
        let up: Vec<V3> = (0..=n).map(|i| v3(x(i), y, z(i))).collect();
        let deck: Vec<V3> = (0..=n).map(|i| v3(x(i), y, deck_z)).collect();
        f.polyline(&up, arch, MemRole::Arch, v3(0.0, 1.0, 0.0));
        f.polyline(&deck, girder, MemRole::Girder, v3(0.0, 1.0, 0.0));
        for i in 1..n {
            f.add(deck[i], up[i], "CHS 114.3x6", MemRole::Strut, v3(0.0, 1.0, 0.0));
        }
        for &px in &[0.0, span] {
            f.add(v3(px, y, 0.0), v3(px, y, deck_z), "HEB 600", MemRole::Column, v3(0.0, 1.0, 0.0));
        }
    }
    for i in 0..=n {
        f.add(v3(x(i), 0.0, deck_z), v3(x(i), width, deck_z), "IPE 600", MemRole::Beam, v3(0.0, 0.0, 1.0));
        if i < n {
            f.add(v3(x(i), 0.0, deck_z), v3(x(i + 1), width, deck_z), "CHS 168.3x8", MemRole::PlanBrace, v3(0.0, 0.0, 1.0));
        }
        // wind bracing between the arches where a person walks under it
        if i > 0 && i < n && z(i) - deck_z >= 5.0 {
            f.add(v3(x(i), 0.0, z(i)), v3(x(i), width, z(i)), "CHS 219.1x10", MemRole::Strut, v3(1.0, 0.0, 0.0));
        }
    }
    Ok(f)
}

/// Options of a cantilever roof ring round an elliptic opening of half-axes `a` x `b`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RingRoof {
    pub a: f64,
    pub b: f64,
    /// Radial trusses round the opening.
    pub bays: usize,
    /// Cantilever from the front column line to the opening edge.
    pub reach: f64,
    /// Back-span from the front to the back column line: the back column holds the reach down.
    pub backspan: f64,
    /// Top chord height.
    pub height: f64,
    pub panel: f64,
}

/// Truss depth over the front column of a cantilever: reach / 6, within 1.5..10 m.
pub fn cantilever_depth(reach: f64) -> f64 {
    (reach / 6.0).clamp(1.5, 10.0)
}

/// Cantilever roof ring: radial trusses from the opening edge back over a front and a back
/// column, a front ring at the tips, a back ring, purlins at every top chord node and plan
/// bracing in every fourth bay.
pub fn cantilever_ring(r: &RingRoof, pat: Pattern) -> Result<Frame, String> {
    let RingRoof { a, b, bays, reach, backspan, height, panel } = *r;
    if !(a >= 5.0 && b >= 5.0 && bays >= 8 && reach >= 2.0 && height >= 6.0) {
        return Err("cantilever_ring: half-axes >= 5 m, >= 8 trusses, reach >= 2 m, height >= 6 m".into());
    }
    if backspan < reach / 4.0 {
        return Err(format!("cantilever_ring: back-span {backspan:.1} m under a quarter of the {reach:.1} m reach cannot hold it down"));
    }
    let d = cantilever_depth(reach);
    if d + 3.0 > height {
        return Err("cantilever_ring: the truss leaves less than 3 m under it".into());
    }
    let (m1, m2) = (panels(reach, panel, 2), panels(backspan, panel, 2));
    let tip = 1.0_f64.min(d);
    // radial offset from the opening edge and truss depth, tip first
    let mut st: Vec<(f64, f64)> = (0..=m1).map(|k| (reach * k as f64 / m1 as f64, tip + (d - tip) * k as f64 / m1 as f64)).collect();
    st.extend((1..=m2).map(|k| (reach + backspan * k as f64 / m2 as f64, d - 0.4 * d * k as f64 / m2 as f64)));
    let at = |i: usize, s: f64, dz: f64| {
        let th = std::f64::consts::TAU * (i % bays) as f64 / bays as f64;
        let edge = v3(a * th.cos(), b * th.sin(), 0.0);
        let nn = v3(th.cos() / a, th.sin() / b, 0.0);
        let nn = nn * (1.0 / nn.len());
        let p = edge + nn * s;
        v3(p.x, p.y, height - dz)
    };
    let s = Secs { chord: "HEB 500", diag: "CHS 219.1x10", strut: "CHS 168.3x8" };
    let mut f = Frame::new();
    let back = st.len() - 1;
    for i in 0..bays {
        let top: Vec<V3> = st.iter().map(|&(r, _)| at(i, r, 0.0)).collect();
        let bot: Vec<V3> = st.iter().map(|&(r, dz)| at(i, r, dz)).collect();
        f.truss(&top, &bot, pat, s, MemRole::Chord);
        f.add(top[back], bot[back], s.strut, MemRole::Strut, v3(1.0, 0.0, 0.0));
        for k in [m1, back] {
            let c = bot[k];
            f.add(v3(c.x, c.y, 0.0), c, "HEB 600", MemRole::Column, v3(1.0, 0.0, 0.0));
        }
        let j = i + 1;
        for (k, &(r, dz)) in st.iter().enumerate() {
            let (sec, role) = if k == 0 { ("CHS 323.9x12.5", MemRole::Girder) } else { ("IPE 300", MemRole::Beam) };
            f.add(at(i, r, 0.0), at(j, r, 0.0), sec, role, v3(0.0, 0.0, 1.0));
            if k == back || k == m1 {
                f.add(at(i, r, dz), at(j, r, dz), "CHS 323.9x12.5", MemRole::Girder, v3(0.0, 0.0, 1.0));
            }
            if i % 4 == 0 && k < back {
                let (r1, _) = st[k + 1];
                f.add(at(i, r, 0.0), at(j, r1, 0.0), "CHS 114.3x6", MemRole::PlanBrace, v3(0.0, 0.0, 1.0));
            }
        }
    }
    Ok(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols(f: &Frame) -> Vec<(V3, V3)> {
        f.members
            .iter()
            .filter(|m| m.role == MemRole::Column)
            .map(|m| {
                let (p, q) = (f.nodes[m.a as usize], f.nodes[m.b as usize]);
                if p.z <= q.z { (p, q) } else { (q, p) }
            })
            .collect()
    }

    /// Every column top is a node some other member uses, every column stands on the ground.
    fn carried(f: &Frame) {
        for (lo, hi) in cols(f) {
            assert!(lo.z.abs() < 1e-9, "{lo:?}");
            let used = f.members.iter().filter(|m| m.role != MemRole::Column).any(|m| {
                [f.nodes[m.a as usize], f.nodes[m.b as usize]].iter().any(|p| (*p - hi).len() < 1e-6)
            });
            assert!(used, "column top {hi:?} carries nothing");
        }
        assert!(f.validate().is_empty(), "{:?}", f.validate());
    }

    /// Largest gap between neighbouring column lines along x.
    fn gap_x(f: &Frame) -> f64 {
        let mut xs: Vec<f64> = cols(f).iter().map(|c| c.0.x).collect();
        xs.sort_by(f64::total_cmp);
        xs.dedup_by(|a, b| (*a - *b).abs() < 0.5);
        xs.windows(2).map(|w| w[1] - w[0]).fold(0.0, f64::max)
    }

    #[test]
    fn a_truss_hall_spans_80_m_clear_on_its_columns() {
        let f = truss_hall(80.0, 20.0, 10, 10.0, 4.0, Pattern::Warren).unwrap();
        carried(&f);
        assert!((gap_x(&f) - 80.0).abs() < 1e-9);
        assert_eq!(cols(&f).len(), 22);
        assert!(f.nodes.iter().all(|p| p.z <= 20.0 + hall_depth(80.0) + 1e-9));
        assert!(truss_hall(4.0, 20.0, 1, 6.0, 2.0, Pattern::Warren).is_err());
    }

    #[test]
    fn an_arch_bridge_spans_90_m_between_its_piers() {
        let f = arch_bridge(90.0, 15.0, 6.0, 4.5, 5.0).unwrap();
        carried(&f);
        assert!((gap_x(&f) - 90.0).abs() < 1e-9);
        assert_eq!(cols(&f).len(), 4);
        let top = f.nodes.iter().map(|p| p.z).fold(0.0, f64::max);
        assert!((top - 21.0).abs() < 1e-9, "{top}");
        assert!(f.members.iter().filter(|m| m.role == MemRole::Strut).count() >= 2 * 16);
    }

    #[test]
    fn a_cantilever_ring_reaches_past_its_front_column_with_a_back_span() {
        let r = RingRoof { a: 60.0, b: 42.0, bays: 32, reach: 25.0, backspan: 9.0, height: 30.0, panel: 4.0 };
        let f = cantilever_ring(&r, Pattern::Warren).unwrap();
        carried(&f);
        let c = cols(&f);
        assert_eq!(c.len(), 64);
        // the farthest node from any column is a tip, the reach away from its front column
        let far = f
            .nodes
            .iter()
            .map(|p| c.iter().map(|(lo, _)| (lo.x - p.x).hypot(lo.y - p.y)).fold(f64::MAX, f64::min))
            .fold(0.0, f64::max);
        assert!((far - 25.0).abs() < 0.5, "{far}");
        let short = RingRoof { backspan: 5.0, ..r };
        let e = cantilever_ring(&short, Pattern::Warren).unwrap_err();
        assert!(e.contains("back-span"), "{e}");
    }
    /// The free-ends check of the evals: every node above the ground has two members at least.
    fn free_nodes(f: &Frame) -> usize {
        let mut deg = vec![0usize; f.nodes.len()];
        for m in &f.members {
            deg[m.a as usize] += 1;
            deg[m.b as usize] += 1;
        }
        f.nodes.iter().zip(&deg).filter(|(p, d)| p.z > 1e-6 && **d < 2).count()
    }

    #[test]
    fn long_spans_have_no_free_ends_and_the_arch_springs_from_the_deck() {
        for pat in [Pattern::Warren, Pattern::Pratt, Pattern::K, Pattern::X] {
            assert_eq!(free_nodes(&truss_hall(80.0, 20.0, 4, 10.0, 4.0, pat).unwrap()), 0, "{pat:?} hall");
            let r = RingRoof { a: 60.0, b: 42.0, bays: 16, reach: 25.0, backspan: 9.0, height: 30.0, panel: 4.0 };
            assert_eq!(free_nodes(&cantilever_ring(&r, pat).unwrap()), 0, "{pat:?} ring");
        }
        let f = arch_bridge(90.0, 15.0, 6.0, 4.5, 5.0).unwrap();
        assert_eq!(free_nodes(&f), 0);
        // both arch ends are on deck level, over the pier tops
        let arch: Vec<V3> = f
            .members
            .iter()
            .filter(|m| m.role == MemRole::Arch)
            .flat_map(|m| [f.nodes[m.a as usize], f.nodes[m.b as usize]])
            .collect();
        let low = arch.iter().map(|p| p.z).fold(f64::MAX, f64::min);
        assert!((low - 6.0).abs() < 1e-9, "{low}");
        for p in arch.iter().filter(|p| p.z < 6.0 + 1e-9) {
            assert!(cols(&f).iter().any(|(_, hi)| (*hi - *p).len() < 1e-6), "arch end {p:?} is not on a pier");
        }
    }
}
