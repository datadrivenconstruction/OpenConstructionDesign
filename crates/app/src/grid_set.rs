// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A set of grid lines from a count, spacing, angle, origin and labelling: two families of lines
//! in a frame turned by `angle` about `origin`. Pure geometry; the callers write the lines
//! (the assistant through a plan item, the command window as one undo step).

/// A family holds at most this many lines, and a set this many in all.
pub const MAX_FAMILY: usize = 100;
pub const MAX_LINES: usize = 200;
/// Length of a family's lines when the other family gives no extent, metres.
const DEFAULT_SPAN: f64 = 6.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Family {
    pub count: usize,
    /// Gaps in metres; the last one repeats when there are fewer than `count - 1`.
    pub spacing: Vec<f64>,
    /// First label: a whole number (1, 2, 3) or letters (A, B, C ... Z, AA).
    pub start: String,
    /// Degrees this family's lines are turned from the frame's axis: a skewed, non-orthogonal grid.
    pub skew: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GridSet {
    pub origin: (f64, f64),
    /// Degrees, counter-clockwise from the plan's x axis.
    pub angle: f64,
    /// How far lines run past the outermost line of the other family, metres.
    pub overhang: f64,
    /// Lines across the frame's x axis (running along its y), at the frame's x positions.
    pub x: Family,
    /// Lines across the frame's y axis (running along its x).
    pub y: Family,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub name: String,
    pub a: (f64, f64),
    pub b: (f64, f64),
}

impl Family {
    pub fn new(count: usize, spacing: f64, start: &str) -> Family {
        Family { count, spacing: vec![spacing], start: start.into(), skew: 0.0 }
    }
}

/// Index in the shared letter series, any case, Latin or Cyrillic: `A` is 0, `AA` 24.
fn letters_index(s: &str) -> Option<(usize, bool)> {
    let up = s.to_uppercase();
    [false, true]
        .into_iter()
        .find_map(|cyr| ddc_geom::axes::axis_index(&up, cyr).map(|i| (i, cyr)))
}

fn letters(i: usize, cyr: bool, lower: bool) -> String {
    let s = ddc_geom::axes::axis_letter(i, cyr);
    if lower {
        s.to_lowercase()
    } else {
        s
    }
}

fn names(f: &Family) -> Result<Vec<String>, String> {
    let s = f.start.trim();
    if let Ok(n) = s.parse::<i64>() {
        return Ok((0..f.count as i64).map(|i| (n + i).to_string()).collect());
    }
    let (first, cyr) = letters_index(s).ok_or_else(|| {
        format!(
            "start label must be a whole number or axis letters (no I or O; AA, BB past Z), \
             got: {}",
            crate::ai::fmt::text(s)
        )
    })?;
    let lower = s.chars().all(|c| c.is_lowercase());
    Ok((0..f.count).map(|i| letters(first + i, cyr, lower)).collect())
}

fn positions(f: &Family) -> Result<Vec<f64>, String> {
    if f.count > MAX_FAMILY {
        return Err(format!("at most {MAX_FAMILY} lines in a family"));
    }
    if f.count > 1 && f.spacing.is_empty() {
        return Err("spacing is needed for more than one line".to_string());
    }
    if f.spacing.iter().any(|s| !s.is_finite() || *s <= 0.0) {
        return Err("spacing must be above zero".to_string());
    }
    if !f.skew.is_finite() || f.skew.abs() > 80.0 {
        return Err("skew must be within 80 degrees".to_string());
    }
    let mut out = Vec::with_capacity(f.count);
    let mut at = 0.0;
    for i in 0..f.count {
        if i > 0 {
            at += f.spacing[(i - 1).min(f.spacing.len() - 1)];
        }
        out.push(at);
    }
    Ok(out)
}

/// The lines of `s`, the x family first, in label order.
pub fn lines(s: &GridSet) -> Result<Vec<Line>, String> {
    if s.x.count + s.y.count == 0 {
        return Err("count is zero".to_string());
    }
    if s.x.count + s.y.count > MAX_LINES {
        return Err(format!("at most {MAX_LINES} lines in a set"));
    }
    let finite = [s.origin.0, s.origin.1, s.angle, s.overhang];
    if finite.iter().any(|v| !v.is_finite()) || s.overhang < 0.0 {
        return Err("origin, angle and overhang must be finite numbers".to_string());
    }
    let (xs, ys) = (positions(&s.x)?, positions(&s.y)?);
    let (xn, yn) = (names(&s.x)?, names(&s.y)?);
    let mut all: Vec<&String> = xn.iter().chain(yn.iter()).collect();
    all.sort();
    if let Some(w) = all.windows(2).find(|w| w[0] == w[1]) {
        return Err(format!(
            "label {} comes twice: give the families different starts (numbers and letters)",
            crate::ai::fmt::text(w[0])
        ));
    }
    let span = |p: &[f64]| -> (f64, f64) {
        match (p.first(), p.last()) {
            (Some(a), Some(b)) if b - a > 1e-9 => (a - s.overhang, b + s.overhang),
            (Some(a), _) => (a - s.overhang, a + s.overhang + DEFAULT_SPAN),
            _ => (-s.overhang, s.overhang + DEFAULT_SPAN),
        }
    };
    let (xa, xb) = span(&xs);
    let (ya, yb) = span(&ys);
    let th = s.angle.to_radians();
    let (u, v) = ((th.cos(), th.sin()), (-th.sin(), th.cos()));
    let world = |lx: f64, ly: f64| {
        (s.origin.0 + lx * u.0 + ly * v.0, s.origin.1 + lx * u.1 + ly * v.1)
    };
    let mut out = Vec::with_capacity(xs.len() + ys.len());
    // x lines run along the frame's y: through (x, middle of the y extent)
    let phi = s.x.skew.to_radians();
    for (x, name) in xs.iter().zip(xn) {
        let (cy, half) = ((ya + yb) / 2.0, (yb - ya) / 2.0 / phi.cos());
        let d = (-phi.sin(), phi.cos());
        out.push(Line {
            name,
            a: world(x - d.0 * half, cy - d.1 * half),
            b: world(x + d.0 * half, cy + d.1 * half),
        });
    }
    let phi = s.y.skew.to_radians();
    for (y, name) in ys.iter().zip(yn) {
        let (cx, half) = ((xa + xb) / 2.0, (xb - xa) / 2.0 / phi.cos());
        let d = (phi.cos(), phi.sin());
        out.push(Line {
            name,
            a: world(cx - d.0 * half, y - d.1 * half),
            b: world(cx + d.0 * half, y + d.1 * half),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(x: Family, y: Family) -> GridSet {
        GridSet { origin: (0.0, 0.0), angle: 0.0, overhang: 1.0, x, y }
    }

    fn near(p: (f64, f64), q: (f64, f64)) -> bool {
        (p.0 - q.0).abs() < 1e-9 && (p.1 - q.1).abs() < 1e-9
    }

    #[test]
    fn a_plain_grid_has_numbers_one_way_and_letters_the_other() {
        let l = lines(&set(Family::new(3, 6.0, "1"), Family::new(2, 4.0, "A"))).unwrap();
        let n: Vec<&str> = l.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(n, ["1", "2", "3", "A", "B"]);
        // line 2 stands at x = 6 and runs from y = -1 to y = 5
        assert!(near(l[1].a, (6.0, -1.0)) && near(l[1].b, (6.0, 5.0)), "{:?}", l[1]);
        // line B stands at y = 4 and runs from x = -1 to x = 13
        assert!(near(l[4].a, (-1.0, 4.0)) && near(l[4].b, (13.0, 4.0)), "{:?}", l[4]);
    }

    #[test]
    fn the_frame_turns_about_the_origin() {
        let mut s = set(Family::new(2, 5.0, "1"), Family::new(0, 1.0, "A"));
        s.origin = (10.0, 20.0);
        s.angle = 90.0;
        s.overhang = 0.0;
        let l = lines(&s).unwrap();
        // the frame's x axis points along the plan's y: line 2 is 5 m above the origin
        assert!(near(l[0].a, (10.0, 20.0)) && near(l[0].b, (4.0, 20.0)), "{:?}", l[0]);
        assert!((l[1].a.1 - 25.0).abs() < 1e-9 && (l[1].b.1 - 25.0).abs() < 1e-9, "{:?}", l[1]);
        assert!((l[0].a.1 - 20.0).abs() < 1e-9);
        // both lines are as long as the default span, and parallel
        let len = |x: &Line| ((x.b.0 - x.a.0).hypot(x.b.1 - x.a.1));
        assert!((len(&l[0]) - DEFAULT_SPAN).abs() < 1e-9);
        assert!((len(&l[0]) - len(&l[1])).abs() < 1e-9);
    }

    #[test]
    fn uneven_spacing_repeats_the_last_gap() {
        let mut x = Family::new(4, 1.0, "1");
        x.spacing = vec![3.0, 5.0];
        let p = positions(&x).unwrap();
        assert_eq!(p, vec![0.0, 3.0, 8.0, 13.0]);
        x.spacing.clear();
        assert!(positions(&x).is_err());
        assert_eq!(positions(&Family { count: 1, ..x }).unwrap(), vec![0.0]);
    }

    #[test]
    fn labels_run_past_z_and_keep_their_case() {
        assert_eq!(names(&Family::new(3, 1.0, "Y")).unwrap(), ["Y", "Z", "AA"]);
        assert_eq!(names(&Family::new(2, 1.0, "zz")).unwrap(), ["zz", "aaa"]);
        assert_eq!(names(&Family::new(3, 1.0, "10")).unwrap(), ["10", "11", "12"]);
        assert!(names(&Family::new(2, 1.0, "A1")).is_err());
        assert!(names(&Family::new(2, 1.0, "")).is_err());
        // the shared series: no I or O, doubled past the end, Cyrillic as well
        assert!(names(&Family::new(2, 1.0, "I")).is_err());
        assert!(names(&Family::new(2, 1.0, "AB")).is_err());
        assert_eq!(names(&Family::new(2, 1.0, "H")).unwrap(), ["H", "J"]);
        let (ya, a) = ("\u{42f}", "\u{410}");
        assert_eq!(names(&Family::new(2, 1.0, ya)).unwrap(), [ya.to_string(), format!("{a}{a}")]);
        assert_eq!(letters(47, false, false), "ZZ");
        assert_eq!(letters(48, false, false), "AAA");
    }

    #[test]
    fn a_skewed_family_leans_and_clashing_labels_are_refused() {
        let mut x = Family::new(2, 5.0, "1");
        x.skew = 45.0;
        let l = lines(&set(x, Family::new(2, 4.0, "A"))).unwrap();
        let d = (l[0].b.0 - l[0].a.0, l[0].b.1 - l[0].a.1);
        assert!((d.0.abs() - d.1.abs()).abs() < 1e-9 && d.0 < 0.0 && d.1 > 0.0, "{d:?}");
        let e = lines(&set(Family::new(2, 5.0, "1"), Family::new(2, 5.0, "2"))).unwrap_err();
        assert!(e.contains("twice"), "{e}");
    }

    #[test]
    fn limits_and_bad_numbers_are_refused() {
        let ok = Family::new(2, 5.0, "1");
        assert!(lines(&set(Family::new(0, 1.0, "1"), Family::new(0, 1.0, "A"))).is_err());
        assert!(lines(&set(Family::new(101, 1.0, "1"), Family::new(1, 1.0, "A"))).is_err());
        assert!(lines(&set(Family::new(2, 0.0, "1"), Family::new(1, 1.0, "A"))).is_err());
        assert!(lines(&set(Family::new(2, f64::NAN, "1"), Family::new(1, 1.0, "A"))).is_err());
        let mut s = set(ok.clone(), Family::new(1, 1.0, "A"));
        s.angle = f64::INFINITY;
        assert!(lines(&s).is_err());
        let mut sk = ok;
        sk.skew = 85.0;
        assert!(lines(&set(sk, Family::new(1, 1.0, "A"))).is_err());
        let wide = set(Family::new(100, 1.0, "1"), Family::new(100, 1.0, "A"));
        assert!(lines(&wide).is_ok());
        let over = set(Family::new(100, 1.0, "1"), Family::new(101, 1.0, "A"));
        assert!(lines(&over).is_err());
    }
}
