//! Profile sketch with typed dimensions on top of `sketch_solve`.
//! A dimension that adds no new information (its equation depends on earlier ones) is turned
//! into a driven (reference) dimension instead of locking the sketch; it then only shows the
//! measured value. Conflicts are reported as the whole minimal set, and the geometry is left
//! untouched until the user makes one dimension driven or deletes one member.
//! Construction segments take constraints like any segment but never enter the profile loop.

use crate::component::expr::{self, Q, LENGTH};
use crate::sketch_solve::{self as ss, Geo, Options, Verdict, P};

/// What a dimension measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DimKind {
    /// True distance between the two points.
    Aligned,
    /// Distance along x.
    Horizontal,
    /// Distance along y.
    Vertical,
}

impl DimKind {
    /// The label position picks the kind: a label above or below the points' x span measures
    /// x, a label left or right of their y span measures y, anything else is aligned.
    pub fn from_label(a: [f64; 2], b: [f64; 2], label: [f64; 2]) -> DimKind {
        let in_x = label[0] >= a[0].min(b[0]) && label[0] <= a[0].max(b[0]);
        let in_y = label[1] >= a[1].min(b[1]) && label[1] <= a[1].max(b[1]);
        match (in_x, in_y) {
            (true, false) => DimKind::Horizontal,
            (false, true) => DimKind::Vertical,
            _ => DimKind::Aligned,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            DimKind::Aligned => "aligned",
            DimKind::Horizontal => "horizontal",
            DimKind::Vertical => "vertical",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Seg {
    pub a: P,
    pub b: P,
    /// Construction geometry: constrained like others, never part of the profile.
    pub construction: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Dim {
    pub tag: i64,
    pub a: P,
    pub b: P,
    pub kind: DimKind,
    /// Value as the user typed it (number with unit or expression).
    pub text: String,
    /// Driving magnitude in metres (>= 0).
    pub value: f64,
    /// Direction of a horizontal/vertical dimension, taken from the geometry at placement.
    pub sign: f64,
    /// Driven (reference) dimension: shows the measured value, adds no equation.
    pub driven: bool,
    /// The user made it driving on purpose: never switched to driven automatically.
    pub forced: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Rule {
    pub tag: i64,
    pub geo: Geo,
}

/// Why the non-construction segments are not one closed loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileError {
    Empty,
    /// A point with one segment: the outline is open.
    Open,
    /// A point with three or more segments.
    Branch,
    /// More than one loop.
    Several,
}

impl ProfileError {
    pub fn key(self) -> &'static str {
        match self {
            ProfileError::Empty => "empty",
            ProfileError::Open => "open",
            ProfileError::Branch => "branch",
            ProfileError::Several => "several",
        }
    }
}

/// Why a typed value was refused; the old value stays.
#[derive(Clone, Debug, PartialEq)]
pub enum ValueError {
    UnknownDim,
    /// Driven dimensions only show the measured value.
    Driven,
    Parse(String),
    NotLength,
    Negative,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Analysis {
    pub verdict: Verdict,
    pub dof: usize,
    /// Dimensions this analysis switched to driven (newest last).
    pub made_driven: Vec<i64>,
    /// The whole minimal conflicting set (dimensions and rules), culprit first.
    pub conflict: Vec<i64>,
    /// Redundant rules and dimensions kept driving on purpose (shown as a warning).
    pub redundant: Vec<i64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProfileSketch {
    pub pts: Vec<[f64; 2]>,
    pub segs: Vec<Seg>,
    pub rules: Vec<Rule>,
    pub dims: Vec<Dim>,
    next_tag: i64,
}

/// Formats metres as a millimetre text the expression parser reads back.
pub fn mm_text(v: f64) -> String {
    let r = (v * 1e3 * 1e6).round() / 1e6;
    format!("{r}mm")
}

impl ProfileSketch {
    fn tag(&mut self) -> i64 {
        self.next_tag += 1;
        self.next_tag
    }

    pub fn point(&mut self, x: f64, y: f64) -> P {
        self.pts.push([x, y]);
        self.pts.len() - 1
    }

    /// A stored sketch: new tags continue after the largest tag in use.
    pub fn from_parts(pts: Vec<[f64; 2]>, segs: Vec<Seg>, rules: Vec<Rule>, dims: Vec<Dim>) -> Self {
        let next_tag = rules
            .iter()
            .map(|r| r.tag)
            .chain(dims.iter().map(|d| d.tag))
            .max()
            .unwrap_or(0);
        ProfileSketch {
            pts,
            segs,
            rules,
            dims,
            next_tag,
        }
    }

    /// Adds a segment; returns its index.
    pub fn seg(&mut self, a: P, b: P, construction: bool) -> usize {
        self.segs.push(Seg { a, b, construction });
        self.segs.len() - 1
    }

    pub fn set_construction(&mut self, seg: usize, on: bool) -> bool {
        match self.segs.get_mut(seg) {
            Some(s) => {
                s.construction = on;
                true
            }
            None => false,
        }
    }

    /// Adds a geometric rule; returns its tag.
    pub fn rule(&mut self, geo: Geo) -> i64 {
        let tag = self.tag();
        self.rules.push(Rule { tag, geo });
        tag
    }

    pub fn dim(&self, tag: i64) -> Option<&Dim> {
        self.dims.iter().find(|d| d.tag == tag)
    }

    /// Current value of a would-be dimension between two points (magnitude, metres).
    pub fn measure(&self, kind: DimKind, a: P, b: P) -> f64 {
        let (Some(p), Some(q)) = (self.pts.get(a), self.pts.get(b)) else {
            return f64::NAN;
        };
        match kind {
            DimKind::Aligned => (q[0] - p[0]).hypot(q[1] - p[1]),
            DimKind::Horizontal => (q[0] - p[0]).abs(),
            DimKind::Vertical => (q[1] - p[1]).abs(),
        }
    }

    pub fn measured(&self, tag: i64) -> Option<f64> {
        self.dim(tag).map(|d| self.measure(d.kind, d.a, d.b))
    }

    /// Places a driving dimension at its current measured value; the label position picks the
    /// kind. A dimension that adds nothing new becomes driven right away.
    pub fn place_dim(&mut self, a: P, b: P, label: [f64; 2]) -> Option<(i64, Analysis)> {
        let (p, q) = (*self.pts.get(a)?, *self.pts.get(b)?);
        let kind = DimKind::from_label(p, q, label);
        let value = self.measure(kind, a, b);
        let sign = match kind {
            DimKind::Horizontal if q[0] < p[0] => -1.0,
            DimKind::Vertical if q[1] < p[1] => -1.0,
            _ => 1.0,
        };
        let tag = self.tag();
        self.dims.push(Dim {
            tag,
            a,
            b,
            kind,
            text: mm_text(value),
            value,
            sign,
            driven: false,
            forced: false,
        });
        Some((tag, self.analyze()))
    }

    /// Sets a driving dimension from a number or expression (`names` resolves parameters).
    /// On any error nothing changes.
    pub fn set_value(
        &mut self,
        tag: i64,
        text: &str,
        names: &dyn Fn(&str) -> Option<Q>,
    ) -> Result<Analysis, ValueError> {
        let d = self.dim(tag).ok_or(ValueError::UnknownDim)?;
        if d.driven {
            return Err(ValueError::Driven);
        }
        let q = expr::parse(text.trim())
            .map_err(|e| ValueError::Parse(e.to_string()))?
            .eval(names)
            .map_err(ValueError::Parse)?;
        if !expr::zero_fits(q, LENGTH) {
            return Err(ValueError::NotLength);
        }
        if !q.v.is_finite() || q.v < 0.0 {
            return Err(ValueError::Negative);
        }
        if let Some(d) = self.dims.iter_mut().find(|d| d.tag == tag) {
            d.text = text.trim().to_string();
            d.value = q.v;
        }
        Ok(self.analyze())
    }

    /// Switches a dimension between driving and driven. A dimension made driving takes the
    /// measured value, so the switch itself never creates a conflict, and stays driving even
    /// when redundant (reported in `Analysis::redundant`).
    pub fn set_driven(&mut self, tag: i64, driven: bool) -> Option<Analysis> {
        let m = self.measured(tag)?;
        let d = self.dims.iter_mut().find(|d| d.tag == tag)?;
        d.driven = driven;
        d.forced = !driven;
        if !driven && m.is_finite() {
            d.value = m;
            d.text = mm_text(m);
        }
        Some(self.analyze())
    }

    /// Deletes a dimension or a rule by tag.
    pub fn delete(&mut self, tag: i64) -> Option<Analysis> {
        let n = self.dims.len() + self.rules.len();
        self.dims.retain(|d| d.tag != tag);
        self.rules.retain(|r| r.tag != tag);
        (self.dims.len() + self.rules.len() != n).then(|| self.analyze())
    }

    fn geo_of(d: &Dim) -> Geo {
        match d.kind {
            DimKind::Aligned => Geo::Dist(d.a, d.b, d.value),
            DimKind::Horizontal => Geo::DistX(d.a, d.b, d.sign * d.value),
            DimKind::Vertical => Geo::DistY(d.a, d.b, d.sign * d.value),
        }
    }

    /// Solver sketch: rules first, then driving dimensions in placement order, so a later
    /// duplicate dimension is the one found redundant.
    pub fn solver_sketch(&self) -> ss::Sketch {
        let mut sk = ss::Sketch {
            pts: self.pts.clone(),
            cons: vec![],
        };
        for r in &self.rules {
            sk.add(r.tag, r.geo.clone());
        }
        for d in self.dims.iter().filter(|d| !d.driven) {
            sk.add(d.tag, Self::geo_of(d));
        }
        sk
    }

    /// Solves; redundant driving dimensions become driven; on a conflict the points stay.
    pub fn analyze(&mut self) -> Analysis {
        let mut made_driven: Vec<i64> = vec![];
        for _ in 0..=self.dims.len() {
            let rep = ss::solve(&self.solver_sketch(), &Options::default());
            let conflict: Vec<i64> = rep.conflicting.iter().map(|c| c.0).collect();
            if !conflict.is_empty() {
                return Analysis {
                    verdict: Verdict::Conflict,
                    dof: rep.dof,
                    made_driven,
                    conflict,
                    redundant: rep.redundant,
                };
            }
            let flip: Vec<i64> = rep
                .redundant
                .iter()
                .copied()
                .filter(|t| self.dims.iter().any(|d| d.tag == *t && !d.driven && !d.forced))
                .collect();
            if flip.is_empty() {
                if rep.converged || rep.residual < 1e-6 {
                    self.pts = rep.pts.clone();
                }
                return Analysis {
                    verdict: rep.verdict(),
                    dof: rep.dof,
                    made_driven,
                    conflict: vec![],
                    redundant: rep.redundant,
                };
            }
            for d in self.dims.iter_mut().filter(|d| flip.contains(&d.tag)) {
                d.driven = true;
            }
            made_driven.extend(flip);
        }
        // unreachable in practice: every pass drives at least one more dimension
        let rep = ss::solve(&self.solver_sketch(), &Options::default());
        Analysis {
            verdict: rep.verdict(),
            dof: rep.dof,
            made_driven,
            conflict: rep.conflicting.iter().map(|c| c.0).collect(),
            redundant: rep.redundant,
        }
    }

    /// The closed outline from non-construction segments, as point indices in loop order.
    pub fn profile(&self) -> Result<Vec<P>, ProfileError> {
        let segs: Vec<&Seg> = self
            .segs
            .iter()
            .filter(|s| !s.construction && s.a != s.b)
            .filter(|s| s.a < self.pts.len() && s.b < self.pts.len())
            .collect();
        if segs.is_empty() {
            return Err(ProfileError::Empty);
        }
        let mut deg = vec![0usize; self.pts.len()];
        for s in &segs {
            deg[s.a] += 1;
            deg[s.b] += 1;
        }
        if deg.iter().any(|&d| d > 2) {
            return Err(ProfileError::Branch);
        }
        if deg.iter().any(|&d| d == 1) {
            return Err(ProfileError::Open);
        }
        let mut used = vec![false; segs.len()];
        let mut order = vec![segs[0].a];
        let mut cur = segs[0].b;
        used[0] = true;
        while cur != order[0] {
            order.push(cur);
            let next = |k: usize| !used[k] && (segs[k].a == cur || segs[k].b == cur);
            let Some(k) = (0..segs.len()).find(|&k| next(k)) else {
                return Err(ProfileError::Open);
            };
            used[k] = true;
            cur = if segs[k].a == cur { segs[k].b } else { segs[k].a };
        }
        if used.iter().any(|u| !u) {
            return Err(ProfileError::Several);
        }
        Ok(order)
    }

    /// The profile as the solid tools' JSON outline (`{"closed": true, "pts": [[x, y, 0], ..]}`).
    pub fn profile_json(&self) -> Result<String, ProfileError> {
        let pts: Vec<String> = self
            .profile()?
            .into_iter()
            .map(|p| format!("[{}, {}, 0]", self.pts[p][0], self.pts[p][1]))
            .collect();
        Ok(format!("{{\"closed\": true, \"pts\": [{}]}}", pts.join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &str) -> Option<Q> {
        None
    }

    /// Rectangle p0..p3 counter-clockwise from the origin, p0 fixed, sides H/V.
    /// Returns the sketch and the tags of H(0,1) and V(1,2).
    fn rect(w: f64, h: f64) -> (ProfileSketch, i64, i64) {
        let mut s = ProfileSketch::default();
        for (x, y) in [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)] {
            s.point(x, y);
        }
        for k in 0..4 {
            s.seg(k, (k + 1) % 4, false);
        }
        s.rule(Geo::Fix(0, 0.0, 0.0));
        let h01 = s.rule(Geo::Horizontal((0, 1)));
        let v12 = s.rule(Geo::Vertical((1, 2)));
        s.rule(Geo::Horizontal((2, 3)));
        s.rule(Geo::Vertical((3, 0)));
        (s, h01, v12)
    }

    #[test]
    fn label_position_picks_kind() {
        let (a, b) = ([0.0, 0.0], [4.0, 3.0]);
        assert_eq!(DimKind::from_label(a, b, [2.0, 5.0]), DimKind::Horizontal);
        assert_eq!(DimKind::from_label(a, b, [2.0, -1.0]), DimKind::Horizontal);
        assert_eq!(DimKind::from_label(a, b, [6.0, 1.5]), DimKind::Vertical);
        assert_eq!(DimKind::from_label(a, b, [1.4, 2.3]), DimKind::Aligned);
        assert_eq!(DimKind::from_label(a, b, [5.0, 5.0]), DimKind::Aligned);
    }

    #[test]
    fn redundant_dim_becomes_driven() {
        let (mut s, ..) = rect(2.0, 1.0);
        let (w, an) = s.place_dim(0, 1, [1.0, -0.5]).unwrap();
        assert!(an.made_driven.is_empty() && !s.dim(w).unwrap().driven, "{an:?}");
        assert_eq!(s.dim(w).unwrap().kind, DimKind::Horizontal);
        let (h, _) = s.place_dim(1, 2, [2.5, 0.5]).unwrap();
        assert_eq!(s.dim(h).unwrap().kind, DimKind::Vertical);
        // the top side duplicates the width: driven at once, never a lock-up
        let (top, an) = s.place_dim(3, 2, [1.0, 1.5]).unwrap();
        assert_eq!(an.made_driven, vec![top]);
        assert!(an.conflict.is_empty() && an.verdict == Verdict::Well, "{an:?}");
        let d = s.dim(top).unwrap();
        assert!(d.driven && !s.dim(w).unwrap().driven);
        // driven value follows the geometry when the driving width changes
        s.set_value(w, "3000mm", &none).unwrap();
        assert!((s.measured(top).unwrap() - 3.0).abs() < 1e-7);
        assert_eq!(
            s.set_value(top, "1m", &none),
            Err(ValueError::Driven),
            "a driven dimension takes no value"
        );
    }

    #[test]
    fn value_takes_expression_and_keeps_old_on_error() {
        let (mut s, ..) = rect(2.0, 1.0);
        let (w, _) = s.place_dim(0, 1, [1.0, -0.5]).unwrap();
        let names = |n: &str| (n == "w").then_some(expr::q(0.5, LENGTH));
        s.set_value(w, "2*w + 150mm", &names).unwrap();
        assert!((s.pts[1][0] - 1.15).abs() < 1e-7, "{:?}", s.pts);
        let before = s.clone();
        assert!(matches!(s.set_value(w, "2*", &names), Err(ValueError::Parse(_))));
        assert!(matches!(s.set_value(w, "nope", &names), Err(ValueError::Parse(_))));
        assert_eq!(s.set_value(w, "30deg", &names), Err(ValueError::NotLength));
        assert_eq!(s.set_value(w, "-1m", &names), Err(ValueError::Negative));
        assert_eq!(s, before);
        assert_eq!(s.dim(w).unwrap().text, "2*w + 150mm");
    }

    #[test]
    fn conflict_reports_full_set() {
        let (mut s, h01, v12) = rect(2.0, 1.0);
        let (w, _) = s.place_dim(0, 1, [1.0, -0.5]).unwrap();
        let (h, _) = s.place_dim(1, 2, [2.5, 0.5]).unwrap();
        // the diagonal is implied by width and height: driven
        let (dg, an) = s.place_dim(0, 2, [0.9, 0.6]).unwrap();
        assert_eq!(s.dim(dg).unwrap().kind, DimKind::Aligned);
        assert_eq!(an.made_driven, vec![dg]);
        // the user forces it driving with another value: conflict, points untouched
        let an = s.set_driven(dg, false).unwrap();
        assert!(an.conflict.is_empty() && an.redundant.contains(&dg), "{an:?}");
        assert!(!s.dim(dg).unwrap().driven, "kept driving on purpose");
        let pts = s.pts.clone();
        let an = s.set_value(dg, "3000mm", &none).unwrap();
        assert_eq!(an.verdict, Verdict::Conflict);
        let mut set = an.conflict.clone();
        set.sort_unstable();
        let mut want = vec![w, h, dg, h01, v12];
        want.sort_unstable();
        assert_eq!(set, want, "{an:?}");
        assert_eq!(an.conflict[0], dg, "culprit first");
        assert_eq!(s.pts, pts);
        // either remedy clears it: make driven, or delete a member
        let mut a = s.clone();
        assert!(a.set_driven(dg, true).unwrap().conflict.is_empty());
        let mut b = s.clone();
        let an = b.delete(w).unwrap();
        assert!(an.conflict.is_empty(), "{an:?}");
        assert!((b.measure(DimKind::Aligned, 0, 2) - 3.0).abs() < 1e-6);
    }

    #[test]
    fn construction_excluded_from_profile() {
        let (mut s, ..) = rect(2.0, 1.0);
        s.place_dim(0, 1, [1.0, -0.5]).unwrap();
        s.place_dim(1, 2, [2.5, 0.5]).unwrap();
        // a construction diagonal and centre point: constrained, but not in the outline
        let c = s.point(1.2, 0.4);
        let diag = s.seg(0, 2, true);
        s.rule(Geo::Midpoint(c, (0, 2)));
        let an = s.analyze();
        assert!(an.conflict.is_empty());
        assert!((s.pts[c][0] - 1.0).abs() < 1e-7 && (s.pts[c][1] - 0.5).abs() < 1e-7);
        assert_eq!(s.profile(), Ok(vec![0, 1, 2, 3]));
        let json = s.profile_json().unwrap();
        assert!(json.starts_with("{\"closed\": true") && json.matches('[').count() == 5, "{json}");
        // the same segment as profile geometry makes a branch
        s.set_construction(diag, false);
        assert_eq!(s.profile(), Err(ProfileError::Branch));
        s.set_construction(diag, true);
        s.set_construction(0, true);
        assert_eq!(s.profile(), Err(ProfileError::Open));
        assert_eq!(ProfileSketch::default().profile(), Err(ProfileError::Empty));
    }

    #[test]
    fn dof_count_updates_after_constraint() {
        let mut s = ProfileSketch::default();
        let a = s.point(0.0, 0.0);
        let b = s.point(1.0, 0.2);
        s.seg(a, b, false);
        assert_eq!(s.analyze().dof, 4);
        s.rule(Geo::Fix(a, 0.0, 0.0));
        assert_eq!(s.analyze().dof, 2);
        s.rule(Geo::Horizontal((a, b)));
        assert_eq!(s.analyze().dof, 1);
        let (_, an) = s.place_dim(a, b, [0.5, 1.0]).unwrap();
        assert_eq!(an.dof, 0);
        assert_eq!(an.verdict, Verdict::Well);
    }
}
