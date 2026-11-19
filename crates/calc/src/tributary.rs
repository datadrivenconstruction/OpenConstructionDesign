//! Area load distribution of a flat panel to its supporting edges (design 1.7), in the panel
//! plane. Two-way: every point goes to the nearest supporting edge line (the 45 degree rule for
//! rectangles; exact partition for convex outlines). One-way: strips along the span direction,
//! half to each supported end, all to the one supported end of a cantilever strip. The result
//! per edge piece is a piecewise linear depth d(s) (line load = p d) plus point weights for
//! openings (negative areas). Totals equal p times the net area for convex outlines whose
//! strips all reach a supported edge; anything else is reported as lost load by the caller.

pub type P2 = [f64; 2];

#[derive(Clone, Debug, PartialEq)]
pub struct Piece {
    pub a: P2,
    pub b: P2,
    pub supported: bool,
}

/// Load of one piece per unit pressure: trapezoids (s0, d0, s1, d1) along the piece from `a`,
/// and point areas (s, area) (negative for openings).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PieceLoad {
    pub trap: Vec<(f64, f64, f64, f64)>,
    pub point: Vec<(f64, f64)>,
}

impl PieceLoad {
    /// Total area carried (integral of d plus the point areas).
    pub fn area(&self) -> f64 {
        let t: f64 = self.trap.iter().map(|t| 0.5 * (t.1 + t.3) * (t.2 - t.0)).sum();
        t + self.point.iter().map(|p| p.1).sum::<f64>()
    }
}

fn sub(a: P2, b: P2) -> P2 {
    [a[0] - b[0], a[1] - b[1]]
}
fn dot(a: P2, b: P2) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}

/// Signed area (counter-clockwise > 0).
pub fn signed_area(r: &[P2]) -> f64 {
    let n = r.len();
    (0..n)
        .map(|i| {
            let (p, q) = (r[i], r[(i + 1) % n]);
            p[0] * q[1] - q[0] * p[1]
        })
        .sum::<f64>()
        / 2.0
}

/// Area centroid of a ring.
pub fn centroid(r: &[P2]) -> P2 {
    let n = r.len();
    let (mut cx, mut cy, mut a2) = (0.0, 0.0, 0.0);
    for i in 0..n {
        let (p, q) = (r[i], r[(i + 1) % n]);
        let c = p[0] * q[1] - q[0] * p[1];
        a2 += c;
        cx += (p[0] + q[0]) * c;
        cy += (p[1] + q[1]) * c;
    }
    if a2.abs() < 1e-300 {
        return r.first().copied().unwrap_or([0.0, 0.0]);
    }
    [cx / (3.0 * a2), cy / (3.0 * a2)]
}

/// True when every turn of the ring has the same sign (collinear runs allowed).
pub fn is_convex(r: &[P2]) -> bool {
    let n = r.len();
    let s = signed_area(r).signum();
    let scale = r.iter().map(|p| p[0].abs().max(p[1].abs())).fold(1.0, f64::max);
    (0..n).all(|i| {
        let (p, q, w) = (r[i], r[(i + 1) % n], r[(i + 2) % n]);
        let (u, v) = (sub(q, p), sub(w, q));
        (u[0] * v[1] - u[1] * v[0]) * s >= -1e-9 * scale * scale
    })
}

/// Origin, unit tangent, inward unit normal and length of a piece of a ring of orientation `ccw`.
struct Fr {
    o: P2,
    t: P2,
    n: P2,
    len: f64,
}

fn frames(ps: &[Piece]) -> Vec<Fr> {
    let ring: Vec<P2> = ps.iter().map(|p| p.a).collect();
    let ccw = signed_area(&ring) >= 0.0;
    ps.iter()
        .map(|p| {
            let d = sub(p.b, p.a);
            let len = dot(d, d).sqrt();
            let t = if len > 0.0 { [d[0] / len, d[1] / len] } else { [1.0, 0.0] };
            let n = if ccw { [-t[1], t[0]] } else { [t[1], -t[0]] };
            Fr { o: p.a, t, n, len }
        })
        .collect()
}

/// Minimum of linear functions (alpha + beta s) on [0, len]: the candidate breakpoints.
fn breaks(fs: &[(f64, f64, usize)], len: f64) -> Vec<f64> {
    let mut s = vec![0.0, len];
    for (i, f) in fs.iter().enumerate() {
        for g in &fs[i + 1..] {
            let db = f.1 - g.1;
            if db.abs() > 1e-15 {
                let x = (g.0 - f.0) / db;
                if x > 0.0 && x < len {
                    s.push(x);
                }
            }
        }
    }
    s.sort_by(f64::total_cmp);
    s.dedup_by(|a, b| (*a - *b).abs() <= 1e-12 * len.max(1.0));
    s
}

fn min_at(fs: &[(f64, f64, usize)], s: f64) -> Option<(f64, usize)> {
    fs.iter()
        .map(|f| (f.0 + f.1 * s, f.2))
        .min_by(|a, b| a.0.total_cmp(&b.0))
}

/// Depth functions d_i(s) = min_j D_j(s) / c_j of the two-way rule for supported piece `i`.
fn two_way_fns(fr: &[Fr], ps: &[Piece], i: usize) -> Vec<(f64, f64, usize)> {
    let f = &fr[i];
    let mut fs = vec![];
    for (j, g) in fr.iter().enumerate() {
        if j == i || g.len <= 0.0 {
            continue;
        }
        let c = dot(f.n, g.n);
        let d0 = dot(sub(f.o, g.o), g.n);
        let ds = dot(f.t, g.n);
        if ps[j].supported {
            if c < 1.0 - 1e-9 {
                fs.push((d0 / (1.0 - c), ds / (1.0 - c), j));
            }
        } else if c < -1e-12 {
            fs.push((d0 / -c, ds / -c, j));
        }
    }
    fs
}

/// Two-way distribution: per piece, depth trapezoids (empty for unsupported pieces).
pub fn two_way(ps: &[Piece]) -> Vec<PieceLoad> {
    let fr = frames(ps);
    (0..ps.len())
        .map(|i| {
            let mut out = PieceLoad::default();
            if !ps[i].supported || fr[i].len <= 0.0 {
                return out;
            }
            let fs = two_way_fns(&fr, ps, i);
            let s = breaks(&fs, fr[i].len);
            let d = |x: f64| min_at(&fs, x).map_or(0.0, |m| m.0.max(0.0));
            for w in s.windows(2) {
                out.trap.push((w[0], d(w[0]), w[1], d(w[1])));
            }
            out
        })
        .collect()
}

fn span_dir(deg: f64) -> P2 {
    let r = deg.to_radians();
    [r.cos(), r.sin()]
}

/// One-way distribution along the span direction `deg` (from +X, degrees).
pub fn one_way(ps: &[Piece], deg: f64) -> Vec<PieceLoad> {
    let fr = frames(ps);
    let u = span_dir(deg);
    (0..ps.len())
        .map(|i| {
            let mut out = PieceLoad::default();
            let f = &fr[i];
            let ci = dot(u, f.n);
            if !ps[i].supported || f.len <= 0.0 || ci.abs() < 1e-9 {
                return out;
            }
            let v = if ci > 0.0 { u } else { [-u[0], -u[1]] };
            let mut fs = vec![];
            for (k, g) in fr.iter().enumerate() {
                let ck = dot(v, g.n);
                if k != i && g.len > 0.0 && ck < -1e-12 {
                    fs.push((dot(sub(f.o, g.o), g.n) / -ck, dot(f.t, g.n) / -ck, k));
                }
            }
            let s = breaks(&fs, f.len);
            for w in s.windows(2) {
                let mid = 0.5 * (w[0] + w[1]);
                let Some((_, k)) = min_at(&fs, mid) else {
                    continue;
                };
                let share = if ps[k].supported { 0.5 } else { 1.0 };
                // the exit piece is fixed on the interval: its own line gives the length
                let g = fs.iter().find(|x| x.2 == k).copied().unwrap_or((0.0, 0.0, k));
                let l = |x: f64| (g.0 + g.1 * x).max(0.0) * share * ci.abs();
                out.trap.push((w[0], l(w[0]), w[1], l(w[1])));
            }
            out
        })
        .collect()
}

fn inside(p: P2, r: &[P2]) -> bool {
    let n = r.len();
    let mut c = false;
    for i in 0..n {
        let (a, b) = (r[i], r[(i + 1) % n]);
        if (a[1] > p[1]) != (b[1] > p[1]) {
            let x = a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
            if p[0] < x {
                c = !c;
            }
        }
    }
    c
}

/// Cell samples of a hole: centres and equal weights summing to the exact hole area.
fn cells(h: &[P2], n: usize) -> Vec<(P2, f64)> {
    let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
    for p in h {
        for k in 0..2 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let mut c = vec![];
    for i in 0..n {
        for j in 0..n {
            let p = [
                lo[0] + (i as f64 + 0.5) / n as f64 * (hi[0] - lo[0]),
                lo[1] + (j as f64 + 0.5) / n as f64 * (hi[1] - lo[1]),
            ];
            if inside(p, h) {
                c.push(p);
            }
        }
    }
    let w = signed_area(h).abs() / c.len().max(1) as f64;
    c.into_iter().map(|p| (p, w)).collect()
}

/// Removes the openings: each hole cell's area goes back off the piece that would carry it.
/// Returns the hole area that no supported piece carried.
pub fn holes(ps: &[Piece], loads: &mut [PieceLoad], hs: &[Vec<P2>], one: Option<f64>) -> f64 {
    let fr = frames(ps);
    let mut lost = 0.0;
    let fns: Vec<Vec<(f64, f64, usize)>> = (0..ps.len())
        .map(|i| if ps[i].supported { two_way_fns(&fr, ps, i) } else { vec![] })
        .collect();
    for h in hs {
        for (p, w) in cells(h, 32) {
            let mut put = |i: usize, s: f64, w: f64| loads[i].point.push((s, -w));
            match one {
                None => {
                    let mut best: Option<(f64, usize, f64)> = None;
                    for (i, f) in fr.iter().enumerate() {
                        if !ps[i].supported {
                            continue;
                        }
                        let (s, t) = (dot(sub(p, f.o), f.t), dot(sub(p, f.o), f.n));
                        let d = min_at(&fns[i], s).map_or(f64::MAX, |m| m.0);
                        let ok = (0.0..=f.len).contains(&s) && t >= -1e-12 && t <= d + 1e-9;
                        if ok && best.is_none_or(|b| t < b.0) {
                            best = Some((t, i, s));
                        }
                    }
                    match best {
                        Some((_, i, s)) => put(i, s, w),
                        None => lost += w,
                    }
                }
                Some(deg) => {
                    let u = span_dir(deg);
                    let hit = |v: P2| -> Option<(usize, f64, f64)> {
                        let mut b: Option<(usize, f64, f64)> = None;
                        for (k, f) in fr.iter().enumerate() {
                            let den = dot(v, f.n);
                            if den >= -1e-12 {
                                continue;
                            }
                            let lam = dot(sub(p, f.o), f.n) / -den;
                            let s = dot(sub([p[0] + lam * v[0], p[1] + lam * v[1]], f.o), f.t);
                            let on = s >= -1e-9 && s <= f.len + 1e-9 && lam >= 0.0;
                            if on && b.is_none_or(|x| lam < x.2) {
                                b = Some((k, s, lam));
                            }
                        }
                        b
                    };
                    let ends = [hit(u), hit([-u[0], -u[1]])];
                    let sup: Vec<(usize, f64)> = ends
                        .iter()
                        .flatten()
                        .filter(|e| ps[e.0].supported)
                        .map(|e| (e.0, e.1))
                        .collect();
                    match sup.len() {
                        0 => lost += w,
                        n => {
                            for (k, s) in sup {
                                put(k, s, w / n as f64);
                            }
                        }
                    }
                }
            }
        }
    }
    lost
}
