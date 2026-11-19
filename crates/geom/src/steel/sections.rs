// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Section table (`data/steel/sections.csv`): standard rolled and hollow shapes by designation,
//! plus parametric built-up plate girders. Lengths in metres.

use crate::member::Shape;
use std::sync::OnceLock;

const SECTIONS: &str = include_str!("../../data/steel/sections.csv");

#[derive(Clone, Debug, PartialEq)]
pub struct SectionRow {
    pub kind: String,
    pub name: String,
    pub shape: Shape,
}

fn row(kind: &str, f: [f64; 4]) -> Result<Shape, String> {
    let m = |v: f64| v / 1000.0;
    let [h, b, tw, tf] = f.map(m);
    let s = match kind {
        "I" => Shape::I { h, b, tw, tf },
        "C" => Shape::Channel { h, b, tw, tf },
        "L" => Shape::Angle { h, b, t: tw },
        "T" => Shape::Tee { h, b, tw, tf },
        "SHS" | "RHS" => Shape::Tube { h, b, t: tw },
        "CHS" => Shape::Pipe { d: h, t: tw },
        "FL" => Shape::Rect { b, h: tw },
        "RB" => Shape::Circle { d: h },
        _ => return Err(format!("unknown section kind '{kind}'")),
    };
    s.check()?;
    Ok(s)
}

pub fn parse(text: &str) -> Result<Vec<SectionRow>, String> {
    let mut out = vec![];
    for (n, line) in text.lines().enumerate() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') || l.starts_with("kind,") {
            continue;
        }
        let c: Vec<&str> = l.split(',').map(str::trim).collect();
        if c.len() != 6 {
            return Err(format!("sections.csv:{}: 6 fields expected", n + 1));
        }
        let mut f = [0.0; 4];
        for (i, v) in c[2..].iter().enumerate() {
            f[i] = v.parse().map_err(|_| format!("sections.csv:{}: bad number '{v}'", n + 1))?;
        }
        let shape = row(c[0], f).map_err(|e| format!("sections.csv:{}: {e}", n + 1))?;
        out.push(SectionRow { kind: c[0].into(), name: c[1].into(), shape });
    }
    Ok(out)
}

pub fn table() -> &'static [SectionRow] {
    static T: OnceLock<Vec<SectionRow>> = OnceLock::new();
    T.get_or_init(|| parse(SECTIONS).expect("built-in section table"))
}

/// A section by designation ("HEB 300", "L 100x100x10", any rolled section of
/// [`super::rolled`] such as "UB 305x165x40" or "W12x26"), or a built-up girder
/// `PG h/bt/tft/bb/tfb/tw` in mm (see [`plate_girder`]).
pub fn find(name: &str) -> Option<Shape> {
    let n = name.trim();
    if let Some(r) = table().iter().find(|r| r.name.eq_ignore_ascii_case(n)) {
        return Some(r.shape.clone());
    }
    if let Some(r) = super::rolled::find(n) {
        return Some(r.shape.clone());
    }
    let p = n.strip_prefix("PG ")?;
    let v: Vec<f64> = p.split('/').filter_map(|x| x.trim().parse().ok()).collect();
    if v.len() != 6 {
        return None;
    }
    let m: Vec<f64> = v.iter().map(|x| x / 1000.0).collect();
    plate_girder(m[0], m[1], m[2], m[3], m[4], m[5]).ok()
}

/// Welded plate girder, possibly mono-symmetric: total depth `h`, top flange `bt x tft`,
/// bottom flange `bb x tfb`, web `tw`. Outline centred on the bounding box, counter-clockwise.
pub fn plate_girder(h: f64, bt: f64, tft: f64, bb: f64, tfb: f64, tw: f64) -> Result<Shape, String> {
    let ok = [h, bt, tft, bb, tfb, tw].iter().all(|x| x.is_finite() && *x > 0.0);
    if !ok || tft + tfb >= h || tw >= bt.min(bb) {
        return Err("plate girder: dimensions must be positive, flanges thinner than depth, web narrower than flanges".into());
    }
    if h / tw > 300.0 {
        return Err(format!("plate girder: web slenderness h/tw = {:.0} > 300", h / tw));
    }
    let (z0, z1) = (-h / 2.0, h / 2.0);
    let (w, t2, b2) = (tw / 2.0, bt / 2.0, bb / 2.0);
    let pts = vec![
        (-b2, z0),
        (b2, z0),
        (b2, z0 + tfb),
        (w, z0 + tfb),
        (w, z1 - tft),
        (t2, z1 - tft),
        (t2, z1),
        (-t2, z1),
        (-t2, z1 - tft),
        (-w, z1 - tft),
        (-w, z0 + tfb),
        (-b2, z0 + tfb),
    ];
    let s = Shape::Poly(pts);
    s.check()?;
    Ok(s)
}

/// Gross area of a section, m2 (closed forms, for weights and the lattice sizing).
pub fn area(s: &Shape) -> f64 {
    use std::f64::consts::PI;
    match s {
        Shape::Rect { b, h } => b * h,
        Shape::Circle { d } => PI * d * d / 4.0,
        Shape::I { h, b, tw, tf } | Shape::Channel { h, b, tw, tf } => 2.0 * b * tf + (h - 2.0 * tf) * tw,
        Shape::Tee { h, b, tw, tf } => b * tf + (h - tf) * tw,
        Shape::Angle { h, b, t } => (h + b - t) * t,
        Shape::Pipe { d, t } => PI * (d * d - (d - 2.0 * t).powi(2)) / 4.0,
        Shape::Tube { h, b, t } => h * b - (h - 2.0 * t) * (b - 2.0 * t),
        Shape::Poly(p) => {
            let n = p.len();
            (0..n).map(|i| { let (a, b) = (p[i], p[(i + 1) % n]); a.0 * b.1 - b.0 * a.1 }).sum::<f64>().abs() / 2.0
        }
    }
}

/// Elastic modulus of structural steel and wrought iron used by every steel check, Pa.
pub const E_STEEL: f64 = 210e9;
