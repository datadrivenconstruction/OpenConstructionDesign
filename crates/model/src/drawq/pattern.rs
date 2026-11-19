//! Line patterns drawn in mm on paper: dashes, gaps, dots, zigzag and wave runs. The library is
//! our own; users add patterns next to the built-in ones. A pattern whose period is too small to
//! read on screen draws as a solid line instead of a grey smear.

use serde_json::{json, Value as J};

type P = (f64, f64);

/// Shortest period, in screen px, that still reads as a pattern.
pub const SMALLEST_REPEAT_PX: f64 = 4.0;
/// Upper bound of output polylines per call: a dense pattern on a long line never hangs.
pub const MAX_PIECES: usize = 100_000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seg {
    /// Drawn run, mm.
    Dash(f64),
    /// Empty run, mm.
    Gap(f64),
    /// A dot (a very short drawn run).
    Dot,
    /// One period of a zigzag, mm long, `amp` mm to each side of the line.
    Zig { len: f64, amp: f64 },
    /// One period of a smooth wave.
    Wave { len: f64, amp: f64 },
}

impl Seg {
    fn len(self) -> f64 {
        match self {
            Seg::Dash(l) | Seg::Gap(l) => l,
            Seg::Dot => DOT_MM,
            Seg::Zig { len, .. } | Seg::Wave { len, .. } => len,
        }
    }
}

const DOT_MM: f64 = 0.25;

#[derive(Clone, Debug, PartialEq)]
pub struct LinePattern {
    pub name: String,
    pub segs: Vec<Seg>,
}

impl LinePattern {
    pub fn solid(&self) -> bool {
        self.segs.is_empty()
    }

    /// Length of one repeat, mm.
    pub fn period(&self) -> f64 {
        self.segs.iter().map(|s| s.len()).sum()
    }

    pub fn check(&self) -> bool {
        let ok = |v: f64| v.is_finite() && v > 0.0 && v <= 500.0;
        !self.name.trim().is_empty()
            && self.segs.len() <= 64
            && self.segs.iter().all(|s| match *s {
                Seg::Dash(l) | Seg::Gap(l) => ok(l),
                Seg::Dot => true,
                Seg::Zig { len, amp } | Seg::Wave { len, amp } => ok(len) && amp.is_finite() && (0.0..=50.0).contains(&amp),
            })
            && (self.segs.is_empty() || self.period() > 0.0)
    }

    /// Whether the pattern is readable at `px_per_mm`.
    pub fn readable(&self, px_per_mm: f64) -> bool {
        !self.solid() && self.period() * px_per_mm >= SMALLEST_REPEAT_PX
    }

    /// Dash array in mm for strokers that only know dashes (drawn, gap, drawn, ...). None when the
    /// pattern has shaped runs or is solid.
    pub fn dash_array(&self) -> Option<Vec<f64>> {
        if self.solid() {
            return None;
        }
        let mut out: Vec<f64> = vec![];
        let mut drawn_last = false;
        for s in &self.segs {
            let (l, drawn) = match *s {
                Seg::Dash(l) => (l, true),
                Seg::Gap(l) => (l, false),
                Seg::Dot => (DOT_MM, true),
                _ => return None,
            };
            if drawn == drawn_last && !out.is_empty() {
                *out.last_mut()? += l;
            } else {
                if out.is_empty() && !drawn {
                    out.push(0.0);
                }
                out.push(l);
            }
            drawn_last = drawn;
        }
        if out.len() % 2 == 1 {
            out.push(0.0);
        }
        Some(out)
    }

    /// Cuts a polyline (paper mm) into drawn pieces. A solid or unreadable pattern returns the
    /// line unchanged. The result is clipped at [`MAX_PIECES`].
    pub fn stroke(&self, line: &[P], px_per_mm: f64) -> Vec<Vec<P>> {
        if line.len() < 2 {
            return vec![];
        }
        if !self.readable(px_per_mm) || !self.check() {
            return vec![line.to_vec()];
        }
        let cum = cumulative(line);
        let total = *cum.last().unwrap_or(&0.0);
        let mut out: Vec<Vec<P>> = vec![];
        let mut s = 0.0;
        'outer: loop {
            for seg in &self.segs {
                if s >= total || out.len() >= MAX_PIECES {
                    break 'outer;
                }
                let e = (s + seg.len()).min(total);
                match *seg {
                    Seg::Gap(_) => {}
                    Seg::Dash(_) => out.push(sub(line, &cum, s, e)),
                    Seg::Dot => out.push(sub(line, &cum, s, e.max(s + 1e-6))),
                    Seg::Zig { len, amp } => out.push(shaped(line, &cum, s, e, len, amp, false)),
                    Seg::Wave { len, amp } => out.push(shaped(line, &cum, s, e, len, amp, true)),
                }
                s += seg.len();
            }
        }
        out
    }

    pub fn to_json(&self) -> J {
        let segs: Vec<J> = self
            .segs
            .iter()
            .map(|s| match *s {
                Seg::Dash(l) => json!({"k": "dash", "len": l}),
                Seg::Gap(l) => json!({"k": "gap", "len": l}),
                Seg::Dot => json!({"k": "dot"}),
                Seg::Zig { len, amp } => json!({"k": "zig", "len": len, "amp": amp}),
                Seg::Wave { len, amp } => json!({"k": "wave", "len": len, "amp": amp}),
            })
            .collect();
        json!({"name": self.name, "segs": segs})
    }

    /// Unknown run kinds make the whole pattern invalid (None) rather than silently change it.
    pub fn from_json(j: &J) -> Option<Self> {
        let mut segs = vec![];
        for s in j["segs"].as_array()? {
            let len = s["len"].as_f64().unwrap_or(0.0);
            let amp = s["amp"].as_f64().unwrap_or(0.0);
            segs.push(match s["k"].as_str()? {
                "dash" => Seg::Dash(len),
                "gap" => Seg::Gap(len),
                "dot" => Seg::Dot,
                "zig" => Seg::Zig { len, amp },
                "wave" => Seg::Wave { len, amp },
                _ => return None,
            });
        }
        let p = LinePattern {
            name: j["name"].as_str()?.into(),
            segs,
        };
        p.check().then_some(p)
    }
}

fn cumulative(line: &[P]) -> Vec<f64> {
    let mut c = Vec::with_capacity(line.len());
    let mut s = 0.0;
    c.push(0.0);
    for w in line.windows(2) {
        s += (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1);
        c.push(s);
    }
    c
}

/// Point and unit direction at arc length `s`.
fn at(line: &[P], cum: &[f64], s: f64) -> (P, P) {
    let i = cum
        .partition_point(|c| *c <= s)
        .clamp(1, line.len() - 1);
    let (a, b) = (line[i - 1], line[i]);
    let l = (cum[i] - cum[i - 1]).max(1e-12);
    let t = ((s - cum[i - 1]) / l).clamp(0.0, 1.0);
    let d = ((b.0 - a.0) / l, (b.1 - a.1) / l);
    ((a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t), d)
}

/// The part of the polyline between arc lengths `s0` and `s1`.
fn sub(line: &[P], cum: &[f64], s0: f64, s1: f64) -> Vec<P> {
    let mut out = vec![at(line, cum, s0).0];
    for i in 1..line.len() - 1 {
        if cum[i] > s0 && cum[i] < s1 {
            out.push(line[i]);
        }
    }
    out.push(at(line, cum, s1).0);
    out
}

/// A zigzag or wave over `s0..s1`: samples every quarter period, offset to the left normal.
fn shaped(line: &[P], cum: &[f64], s0: f64, s1: f64, period: f64, amp: f64, smooth: bool) -> Vec<P> {
    let step = period / 4.0;
    let n = (((s1 - s0) / step).ceil() as usize).max(1);
    let mut out = Vec::with_capacity(n + 1);
    for k in 0..=n {
        let s = (s0 + k as f64 * step).min(s1);
        let ph = (s - s0) / period;
        let off = if smooth {
            amp * (ph * std::f64::consts::TAU).sin()
        } else {
            // triangle wave: 0, +amp, 0, -amp per period
            let f = ph.rem_euclid(1.0);
            amp * if f < 0.25 {
                f * 4.0
            } else if f < 0.75 {
                2.0 - f * 4.0
            } else {
                f * 4.0 - 4.0
            }
        };
        let (p, d) = at(line, cum, s);
        out.push((p.0 - d.1 * off, p.1 + d.0 * off));
    }
    out
}

/// Built-in library; every entry passes [`LinePattern::check`].
pub fn builtin() -> Vec<LinePattern> {
    let p = |name: &str, segs: Vec<Seg>| LinePattern {
        name: name.into(),
        segs,
    };
    vec![
        p("solid", vec![]),
        p("dash", vec![Seg::Dash(3.0), Seg::Gap(1.5)]),
        p("long_dash", vec![Seg::Dash(8.0), Seg::Gap(2.0)]),
        p("dot", vec![Seg::Dot, Seg::Gap(1.5)]),
        p(
            "dash_dot",
            vec![Seg::Dash(6.0), Seg::Gap(1.5), Seg::Dot, Seg::Gap(1.5)],
        ),
        p(
            "dash_dot_dot",
            vec![
                Seg::Dash(6.0),
                Seg::Gap(1.5),
                Seg::Dot,
                Seg::Gap(1.5),
                Seg::Dot,
                Seg::Gap(1.5),
            ],
        ),
        p("overhead", vec![Seg::Dash(4.0), Seg::Gap(2.0)]),
        p(
            "fire",
            vec![Seg::Dash(8.0), Seg::Gap(1.5), Seg::Dash(1.0), Seg::Gap(1.5)],
        ),
        p("gas", vec![Seg::Dash(10.0), Seg::Gap(2.0), Seg::Zig { len: 3.0, amp: 0.8 }, Seg::Gap(2.0)]),
        p("water", vec![Seg::Wave { len: 5.0, amp: 0.9 }]),
        p("zigzag", vec![Seg::Zig { len: 4.0, amp: 1.0 }]),
    ]
}

/// Built-in patterns with the user's on top; a user pattern replaces a built-in of the same name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PatternLib {
    pub user: Vec<LinePattern>,
}

impl PatternLib {
    pub fn get(&self, name: &str) -> Option<LinePattern> {
        self.user
            .iter()
            .find(|p| p.name == name)
            .cloned()
            .or_else(|| builtin().into_iter().find(|p| p.name == name))
    }

    /// Pattern for a style name; a missing one draws solid.
    pub fn get_or_solid(&self, name: &str) -> LinePattern {
        self.get(name).unwrap_or(LinePattern {
            name: "solid".into(),
            segs: vec![],
        })
    }

    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = builtin().into_iter().map(|p| p.name).collect();
        for u in &self.user {
            if !v.contains(&u.name) {
                v.push(u.name.clone());
            }
        }
        v
    }

    pub fn set(&mut self, p: LinePattern) -> bool {
        if !p.check() {
            return false;
        }
        match self.user.iter_mut().find(|u| u.name == p.name) {
            Some(u) => *u = p,
            None => self.user.push(p),
        }
        true
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let n = self.user.len();
        self.user.retain(|p| p.name != name);
        self.user.len() != n
    }

    pub fn to_json(&self) -> J {
        J::Array(self.user.iter().map(LinePattern::to_json).collect())
    }

    pub fn from_json(j: &J) -> Self {
        Self {
            user: j
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(LinePattern::from_json)
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn len(piece: &[P]) -> f64 {
        piece
            .windows(2)
            .map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1))
            .sum()
    }

    #[test]
    fn builtin_patterns_are_valid_and_unique() {
        let b = builtin();
        for p in &b {
            assert!(p.check(), "{}", p.name);
        }
        let mut names: Vec<_> = b.iter().map(|p| p.name.clone()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), b.len());
    }

    #[test]
    fn dash_gap_on_a_straight_line() {
        let p = LinePattern {
            name: "t".into(),
            segs: vec![Seg::Dash(3.0), Seg::Gap(1.0)],
        };
        let out = p.stroke(&[(0.0, 0.0), (10.0, 0.0)], 10.0);
        // 0-3, 4-7, 8-10
        assert_eq!(out.len(), 3);
        assert!((len(&out[0]) - 3.0).abs() < 1e-9);
        assert!((out[1][0].0 - 4.0).abs() < 1e-9);
        assert!((len(&out[2]) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn pattern_runs_around_a_corner() {
        let p = LinePattern {
            name: "t".into(),
            segs: vec![Seg::Dash(4.0), Seg::Gap(1.0)],
        };
        let out = p.stroke(&[(0.0, 0.0), (3.0, 0.0), (3.0, 3.0)], 10.0);
        assert_eq!(out[0].len(), 3, "the first dash keeps the corner vertex");
        assert!((len(&out[0]) - 4.0).abs() < 1e-9);
    }

    #[test]
    fn too_small_a_period_draws_solid() {
        let p = LinePattern {
            name: "t".into(),
            segs: vec![Seg::Dash(1.0), Seg::Gap(1.0)],
        };
        let line = [(0.0, 0.0), (50.0, 0.0)];
        assert_eq!(p.stroke(&line, 1.0), vec![line.to_vec()]);
        assert!(p.stroke(&line, 10.0).len() > 10);
    }

    #[test]
    fn work_is_capped() {
        let p = LinePattern {
            name: "t".into(),
            segs: vec![Seg::Dash(0.5), Seg::Gap(0.5)],
        };
        let out = p.stroke(&[(0.0, 0.0), (1.0e7, 0.0)], 100.0);
        assert!(out.len() <= MAX_PIECES + 1);
    }

    #[test]
    fn zigzag_stays_within_amplitude() {
        let p = LinePattern {
            name: "t".into(),
            segs: vec![Seg::Zig { len: 4.0, amp: 1.0 }],
        };
        let out = p.stroke(&[(0.0, 0.0), (20.0, 0.0)], 10.0);
        let ys: Vec<f64> = out.iter().flatten().map(|q| q.1).collect();
        assert!(ys.iter().all(|y| y.abs() <= 1.0 + 1e-9));
        assert!(ys.iter().any(|y| (y.abs() - 1.0).abs() < 1e-9));
        let wave = LinePattern {
            name: "w".into(),
            segs: vec![Seg::Wave { len: 4.0, amp: 1.0 }],
        };
        let w = wave.stroke(&[(0.0, 0.0), (8.0, 0.0)], 10.0);
        assert!(w.iter().flatten().all(|q| q.1.abs() <= 1.0 + 1e-9));
    }

    #[test]
    fn dash_array_merges_runs() {
        let p = LinePattern {
            name: "t".into(),
            segs: vec![Seg::Dash(2.0), Seg::Dash(1.0), Seg::Gap(1.0)],
        };
        assert_eq!(p.dash_array(), Some(vec![3.0, 1.0]));
        let dd = builtin().into_iter().find(|p| p.name == "dash_dot").unwrap();
        assert_eq!(dd.dash_array(), Some(vec![6.0, 1.5, DOT_MM, 1.5]));
        assert_eq!(LinePattern { name: "s".into(), segs: vec![] }.dash_array(), None);
        let z = builtin().into_iter().find(|p| p.name == "water").unwrap();
        assert_eq!(z.dash_array(), None);
    }

    #[test]
    fn library_overrides_and_round_trips() {
        let mut lib = PatternLib::default();
        assert!(lib.get("dash").is_some());
        assert!(lib.get("nope").is_none());
        assert!(lib.get_or_solid("nope").solid());
        assert!(!lib.set(LinePattern { name: "bad".into(), segs: vec![Seg::Dash(-1.0)] }));
        let mine = LinePattern { name: "dash".into(), segs: vec![Seg::Dash(9.0), Seg::Gap(9.0)] };
        assert!(lib.set(mine.clone()));
        assert_eq!(lib.get("dash"), Some(mine));
        assert_eq!(PatternLib::from_json(&lib.to_json()), lib);
        let n = lib.names().iter().filter(|n| *n == "dash").count();
        assert_eq!(n, 1);
        assert!(lib.remove("dash") && lib.get("dash").unwrap().segs.len() == 2);
        assert_eq!(LinePattern::from_json(&json!({"name": "x", "segs": [{"k": "zap", "len": 1.0}]})), None);
    }
}
