//! Straight segments from a one-pixel skeleton: a (theta, rho) vote, peaks in vote order, the
//! pixels near each peak line split into gap-free runs, each run fitted by total least squares.
//! Then near-orthogonal segments are snapped to the dominant direction and collinear pieces
//! merged. Pixel units throughout; a pixel's centre is (x + 0.5, y + 0.5). Deterministic: peaks
//! are taken by votes, then angle, then offset.

pub(crate) type P = (f64, f64);

/// Angle bins over a half turn.
const NT: usize = 360;
/// Pixels this far from a peak line are collected for it.
const BAND: f64 = 1.25;
/// After the fit, pixels this far from the fitted line stay in the run.
const KEEP: f64 = 1.0;
/// A run breaks at a gap longer than this along the line.
const GAP: f64 = 2.5;
/// Residual of a straight run; an arc or a staircase of blobs is above it.
const MAX_RMS: f64 = 0.5;
/// Peaks examined at most, strongest first.
const MAX_PEAKS: usize = 40_000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PxSeg {
    pub a: P,
    pub b: P,
}

impl PxSeg {
    pub fn len(&self) -> f64 {
        (self.b.0 - self.a.0).hypot(self.b.1 - self.a.1)
    }
}

/// Line through `m` along unit `d`, with unit normal `n = (-d.1, d.0)`.
#[derive(Clone, Copy, Debug)]
struct Line {
    m: P,
    d: P,
}

impl Line {
    fn n(&self) -> P {
        (-self.d.1, self.d.0)
    }
    fn s(&self, p: P) -> f64 {
        let n = self.n();
        (p.0 - self.m.0) * n.0 + (p.1 - self.m.1) * n.1
    }
    fn t(&self, p: P) -> f64 {
        (p.0 - self.m.0) * self.d.0 + (p.1 - self.m.1) * self.d.1
    }
    fn at(&self, t: f64) -> P {
        (self.m.0 + self.d.0 * t, self.m.1 + self.d.1 * t)
    }
}

fn centre(i: usize, w: usize) -> P {
    ((i % w) as f64 + 0.5, (i / w) as f64 + 0.5)
}

/// Total least squares line of the points; None for fewer than two distinct points.
fn fit(pts: &[P]) -> Option<(Line, f64)> {
    if pts.len() < 2 {
        return None;
    }
    let n = pts.len() as f64;
    let (mx, my) = pts.iter().fold((0.0, 0.0), |a, p| (a.0 + p.0, a.1 + p.1));
    let m = (mx / n, my / n);
    let (mut sxx, mut syy, mut sxy) = (0.0, 0.0, 0.0);
    for p in pts {
        let (dx, dy) = (p.0 - m.0, p.1 - m.1);
        sxx += dx * dx;
        syy += dy * dy;
        sxy += dx * dy;
    }
    if sxx + syy < 1e-12 {
        return None;
    }
    let ang = 0.5 * (2.0 * sxy).atan2(sxx - syy);
    let l = Line { m, d: (ang.cos(), ang.sin()) };
    let rms = (pts.iter().map(|&p| l.s(p).powi(2)).sum::<f64>() / n).sqrt();
    Some((l, rms))
}

/// Calls `f` with each pixel index whose centre lies within `smax` of the line between `t0`
/// and `t1` (clipped to the image). A pixel may come more than once.
fn walk(w: usize, h: usize, l: &Line, t0: f64, t1: f64, smax: f64, mut f: impl FnMut(usize)) {
    // clip the parameter range to the image rectangle grown by the band
    let (mut lo, mut hi) = (t0, t1);
    let g = smax + 1.0;
    for (o, d, max) in [(l.m.0, l.d.0, w as f64), (l.m.1, l.d.1, h as f64)] {
        if d.abs() < 1e-12 {
            if o < -g || o > max + g {
                return;
            }
            continue;
        }
        let (ta, tb) = ((-g - o) / d, (max + g - o) / d);
        lo = lo.max(ta.min(tb));
        hi = hi.min(ta.max(tb));
    }
    if lo > hi {
        return;
    }
    let n = l.n();
    let steps = (2.0 * smax / 0.5).ceil() as i32;
    let mut t = lo;
    while t <= hi {
        let p = l.at(t);
        for k in 0..=steps {
            let s = -smax + k as f64 * 0.5;
            let (x, y) = ((p.0 + n.0 * s).floor(), (p.1 + n.1 * s).floor());
            if x < 0.0 || y < 0.0 || x >= w as f64 || y >= h as f64 {
                continue;
            }
            f(y as usize * w + x as usize);
        }
        t += 0.5;
    }
}

/// Segments at least `min_len` pixels long. Pixels used by a segment are cleared from `skel`;
/// what is left (arcs, ticks, noise) stays for the arc pass.
pub(crate) fn segments(w: usize, h: usize, skel: &mut [bool], min_len: f64) -> Vec<PxSeg> {
    let pts: Vec<usize> = (0..w * h).filter(|&i| skel[i]).collect();
    if pts.is_empty() {
        return vec![];
    }
    let r0 = ((w * w + h * h) as f64).sqrt().ceil() as usize + 1;
    let nr = 2 * r0 + 1;
    let (cs, sn): (Vec<f64>, Vec<f64>) = (0..NT)
        .map(|k| {
            let a = k as f64 * std::f64::consts::PI / NT as f64;
            (a.cos(), a.sin())
        })
        .unzip();
    let bin = |i: usize, k: usize| -> usize {
        let (x, y) = centre(i, w);
        let rho = x * cs[k] + y * sn[k];
        k * nr + (rho.round() as isize + r0 as isize) as usize
    };
    let mut acc = vec![0u32; NT * nr];
    for &i in &pts {
        for k in 0..NT {
            acc[bin(i, k)] += 1;
        }
    }
    // a cleared pixel takes its votes back, so stale peaks of used lines are skipped unwalked
    let clear = |i: usize, skel: &mut [bool], acc: &mut [u32]| {
        if skel[i] {
            skel[i] = false;
            for k in 0..NT {
                let c = &mut acc[bin(i, k)];
                *c = c.saturating_sub(1);
            }
        }
    };
    let thr = ((min_len * 0.6).floor() as u32).max(3);
    let mut out = vec![];
    let mut got: Vec<usize> = vec![];
    // new peaks can surface once stronger lines are gone: a few rounds until nothing is found
    for _round in 0..3 {
        let found = out.len();
        let cand = peaks(&acc, nr, thr);
        for &(_, k, r) in &cand {
            if acc[k * nr + r] < thr {
                continue;
            }
            let rho = r as f64 - r0 as f64;
            let n = (cs[k], sn[k]);
            let line = Line { m: (n.0 * rho, n.1 * rho), d: (-n.1, n.0) };
            got.clear();
            walk(w, h, &line, f64::NEG_INFINITY, f64::INFINITY, BAND, |i| {
                if skel[i] {
                    got.push(i);
                }
            });
            got.sort_unstable();
            got.dedup();
            if (got.len() as f64) < min_len * 0.6 {
                continue;
            }
            let near: Vec<P> = got.iter().map(|&i| centre(i, w)).collect();
            let Some((mut fl, _)) = fit(&near) else {
                continue;
            };
            // the fit of a whole peak may lean on stray pixels: keep it only near the peak angle
            let dot = fl.d.0 * line.d.0 + fl.d.1 * line.d.1;
            if dot.abs() < (1.5f64).to_radians().cos() {
                fl = line;
            } else if dot < 0.0 {
                fl.d = (-fl.d.0, -fl.d.1);
            }
            // collect again along the fitted line: a long line leaves the peak band at its ends
            got.clear();
            walk(w, h, &fl, f64::NEG_INFINITY, f64::INFINITY, KEEP, |i| {
                if skel[i] {
                    got.push(i);
                }
            });
            got.sort_unstable();
            got.dedup();
            let mut on: Vec<(f64, usize)> = got
                .iter()
                .map(|&i| (centre(i, w), i))
                .filter(|(p, _)| fl.s(*p).abs() <= KEEP)
                .map(|(p, i)| (fl.t(p), i))
                .collect();
            on.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            let mut start = 0;
            while start < on.len() {
                let mut end = start + 1;
                while end < on.len() && on[end].0 - on[end - 1].0 <= GAP {
                    end += 1;
                }
                if let Some(s) = run(w, &on[start..end], min_len) {
                    for &(_, i) in &on[start..end] {
                        clear(i, &mut *skel, &mut acc);
                    }
                    // staircase pixels beside the run belong to it as well
                    if let Some((rl, _)) = fit(&[s.a, s.b]) {
                        let (ta, tb) = (rl.t(s.a), rl.t(s.b));
                        let mut side = vec![];
                        walk(w, h, &rl, ta.min(tb) - 1.0, ta.max(tb) + 1.0, 1.5, |i| side.push(i));
                        for i in side {
                            clear(i, &mut *skel, &mut acc);
                        }
                    }
                    out.push(s);
                }
                start = end;
            }
        }
        if out.len() == found {
            break;
        }
    }
    out
}

/// Local maxima of the vote at or above `thr`, strongest first.
fn peaks(acc: &[u32], nr: usize, thr: u32) -> Vec<(u32, usize, usize)> {
    let mut peaks: Vec<(u32, usize, usize)> = vec![];
    for k in 0..NT {
        for r in 1..nr - 1 {
            let v = acc[k * nr + r];
            if v < thr {
                continue;
            }
            let mut top = true;
            'nb: for dk in [-1isize, 0, 1] {
                let kk = k as isize + dk;
                if kk < 0 || kk >= NT as isize {
                    continue;
                }
                for dr in [-1isize, 0, 1] {
                    if dk == 0 && dr == 0 {
                        continue;
                    }
                    let u = acc[kk as usize * nr + (r as isize + dr) as usize];
                    // plateaus: the first cell in scan order wins
                    let earlier = dk < 0 || (dk == 0 && dr < 0);
                    if u > v || (u == v && earlier) {
                        top = false;
                        break 'nb;
                    }
                }
            }
            if top {
                peaks.push((v, k, r));
            }
        }
    }
    peaks.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    peaks.truncate(MAX_PEAKS);
    peaks
}

/// A straight segment from one run of pixels, or None when it is short, sparse or curved.
fn run(w: usize, on: &[(f64, usize)], min_len: f64) -> Option<PxSeg> {
    let span = on.last()?.0 - on.first()?.0 + 1.0;
    if span < min_len {
        return None;
    }
    let pts: Vec<P> = on.iter().map(|&(_, i)| centre(i, w)).collect();
    let (l, rms) = fit(&pts)?;
    if rms > MAX_RMS {
        return None;
    }
    // a digital line has one pixel per step of its major axis
    let major = l.d.0.abs().max(l.d.1.abs());
    if (pts.len() as f64) < 0.75 * span * major {
        return None;
    }
    let ts = pts.iter().map(|&p| l.t(p));
    let (t0, t1) = ts.fold((f64::INFINITY, f64::NEG_INFINITY), |a, t| (a.0.min(t), a.1.max(t)));
    Some(PxSeg { a: l.at(t0 - 0.5), b: l.at(t1 + 0.5) })
}

/// Snaps segments within `tol_deg` of the dominant direction (or square to it) onto it and
/// merges collinear overlapping pieces. Returns the dominant angle in radians.
pub(crate) fn orthogonalize(segs: &mut Vec<PxSeg>, tol_deg: f64) -> f64 {
    // length-weighted histogram of the direction modulo a quarter turn, 0.25 degree bins
    let mut hist = [0.0f64; 360];
    let quarter = |s: &PxSeg| {
        let a = (s.b.1 - s.a.1).atan2(s.b.0 - s.a.0).to_degrees();
        a.rem_euclid(90.0)
    };
    for s in segs.iter() {
        let b = ((quarter(s) / 0.25).floor() as usize).min(359);
        hist[b] += s.len();
    }
    let mut best = 0;
    for i in 0..360 {
        // a bin and its neighbours, wrapping at the quarter turn
        let v = |j: usize| hist[(j + 360) % 360];
        let score = v(i + 359) + v(i) + v(i + 1);
        let cur = v(best + 359) + v(best) + v(best + 1);
        if score > cur + 1e-9 {
            best = i;
        }
    }
    let centre_deg = (best as f64 + 0.5) * 0.25;
    let wrap = |a: f64| (a - centre_deg + 45.0).rem_euclid(90.0) - 45.0;
    let (mut sw, mut sa) = (0.0, 0.0);
    for s in segs.iter() {
        let d = wrap(quarter(s));
        if d.abs() <= 1.0 {
            sw += s.len();
            sa += s.len() * d;
        }
    }
    let dom = if sw > 0.0 { centre_deg + sa / sw } else { centre_deg };
    // straight angles stay exact when the drawing is square to the image
    let dom = if (dom - dom.round()).abs() < 0.05 { dom.round() } else { dom };
    let dom = dom.rem_euclid(90.0);
    for s in segs.iter_mut() {
        let a = (s.b.1 - s.a.1).atan2(s.b.0 - s.a.0).to_degrees();
        let d = (a - dom + 45.0).rem_euclid(90.0) - 45.0;
        if d.abs() > tol_deg {
            continue;
        }
        let target = (a - d).to_radians();
        let (mx, my) = ((s.a.0 + s.b.0) / 2.0, (s.a.1 + s.b.1) / 2.0);
        let half = s.len() / 2.0;
        let (c, sn) = (target.cos(), target.sin());
        s.a = (mx - c * half, my - sn * half);
        s.b = (mx + c * half, my + sn * half);
    }
    merge(segs, dom.to_radians());
    dom.to_radians()
}

/// Collinear pieces along the two snapped directions that overlap or nearly touch become one.
fn merge(segs: &mut Vec<PxSeg>, dom: f64) {
    let dirs = [(dom.cos(), dom.sin()), (-dom.sin(), dom.cos())];
    let mut keyed: Vec<(usize, f64, f64, f64)> = vec![];
    let mut rest = vec![];
    for s in segs.iter() {
        let v = ((s.b.0 - s.a.0) / s.len(), (s.b.1 - s.a.1) / s.len());
        let k = dirs.iter().position(|d| (d.0 * v.1 - d.1 * v.0).abs() < 1e-9);
        match k {
            Some(k) => {
                let d = dirs[k];
                let off = -d.1 * s.a.0 + d.0 * s.a.1;
                let (ta, tb) = (d.0 * s.a.0 + d.1 * s.a.1, d.0 * s.b.0 + d.1 * s.b.1);
                keyed.push((k, off, ta.min(tb), ta.max(tb)));
            }
            None => rest.push(*s),
        }
    }
    keyed.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)).then(a.2.total_cmp(&b.2)));
    // group by direction and offset (within 0.75 px), then merge intervals in each group
    let mut out = vec![];
    let mut i = 0;
    while i < keyed.len() {
        let mut j = i + 1;
        while j < keyed.len() && keyed[j].0 == keyed[i].0 && keyed[j].1 - keyed[j - 1].1 <= 0.75 {
            j += 1;
        }
        let mut g: Vec<(f64, f64, f64)> = keyed[i..j].iter().map(|x| (x.2, x.3, x.1)).collect();
        g.sort_by(|a, b| a.0.total_cmp(&b.0));
        let d = dirs[keyed[i].0];
        let mut cur: Option<(f64, f64, f64, f64)> = None; // t0, t1, sum(off*len), sum(len)
        let flush = |c: (f64, f64, f64, f64), out: &mut Vec<PxSeg>| {
            let off = c.2 / c.3;
            let p = |t: f64| (d.0 * t - d.1 * off, d.1 * t + d.0 * off);
            out.push(PxSeg { a: p(c.0), b: p(c.1) });
        };
        for (t0, t1, off) in g {
            let l = t1 - t0;
            cur = Some(match cur {
                Some(c) if t0 <= c.1 + 1.5 => (c.0, c.1.max(t1), c.2 + off * l, c.3 + l),
                Some(c) => {
                    flush(c, &mut out);
                    (t0, t1, off * l, l)
                }
                None => (t0, t1, off * l, l),
            });
        }
        if let Some(c) = cur {
            flush(c, &mut out);
        }
        i = j;
    }
    out.extend(rest);
    *segs = out;
}
