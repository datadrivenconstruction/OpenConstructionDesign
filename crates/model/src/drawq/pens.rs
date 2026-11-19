// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Pens: named line weights in mm on paper, one column per drawing scale. The same table feeds the
//! screen and the sheet output, so what is seen is what is printed. Cut and projection lines have
//! separate styles; halftone lightens a line towards the background.

use std::collections::BTreeMap;

use serde_json::{json, Value as J};

use super::range::Band;

/// Scale denominators of the columns of the built-in pens.
const COLUMNS: [u32; 5] = [20, 50, 100, 200, 500];
/// Weight factor per column: coarser scales get slightly finer lines.
const FACTOR: [f32; 5] = [1.25, 1.0, 0.85, 0.7, 0.5];
/// Built-in pens: name and weight (mm) at the 1:50 column.
const BASE: [(&str, f32); 7] = [
    ("hair", 0.10),
    ("fine", 0.15),
    ("light", 0.20),
    ("medium", 0.30),
    ("bold", 0.45),
    ("heavy", 0.65),
    ("extra", 0.90),
];
/// Thinnest and thickest line the table can hold, mm.
pub const MIN_MM: f32 = 0.05;
pub const MAX_MM: f32 = 5.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Pen {
    pub name: String,
    /// (scale denominator, mm), sorted by denominator.
    pub columns: Vec<(u32, f32)>,
}

impl Pen {
    /// Weight at `denom`: the column nearest on a log scale.
    pub fn at(&self, denom: u32) -> f32 {
        let d = f64::from(denom.max(1)).ln();
        self.columns
            .iter()
            .min_by(|a, b| {
                let da = (f64::from(a.0.max(1)).ln() - d).abs();
                let db = (f64::from(b.0.max(1)).ln() - d).abs();
                da.total_cmp(&db)
            })
            .map_or(MIN_MM, |c| c.1)
    }

    fn check(&self) -> bool {
        !self.name.trim().is_empty()
            && !self.columns.is_empty()
            && self
                .columns
                .iter()
                .all(|c| c.0 > 0 && c.1.is_finite() && (MIN_MM..=MAX_MM).contains(&c.1))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PenTable {
    pub pens: Vec<Pen>,
}

impl Default for PenTable {
    fn default() -> Self {
        let pens = BASE
            .iter()
            .map(|(name, w)| Pen {
                name: (*name).into(),
                columns: COLUMNS
                    .iter()
                    .zip(FACTOR)
                    .map(|(d, f)| (*d, ((w * f * 100.0).round() / 100.0).max(MIN_MM)))
                    .collect(),
            })
            .collect();
        Self { pens }
    }
}

impl PenTable {
    /// Every pen valid and at least one pen.
    pub fn check(&self) -> bool {
        !self.pens.is_empty() && self.pens.iter().all(Pen::check)
    }

    pub fn get(&self, name: &str) -> Option<&Pen> {
        self.pens.iter().find(|p| p.name == name)
    }

    /// Weight of a pen in mm at a scale; an unknown pen falls back to the finest line.
    pub fn mm(&self, name: &str, denom: u32) -> f32 {
        self.get(name).map_or(MIN_MM, |p| p.at(denom))
    }

    /// Adds or replaces a pen (by name), keeping columns sorted. False when the pen is invalid.
    pub fn set(&mut self, mut pen: Pen) -> bool {
        pen.columns.sort_by_key(|c| c.0);
        pen.columns.dedup_by_key(|c| c.0);
        if !pen.check() {
            return false;
        }
        match self.pens.iter_mut().find(|p| p.name == pen.name) {
            Some(p) => *p = pen,
            None => self.pens.push(pen),
        }
        true
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let n = self.pens.len();
        self.pens.retain(|p| p.name != name);
        self.pens.len() != n
    }

    pub fn to_json(&self) -> J {
        J::Array(
            self.pens
                .iter()
                .map(|p| {
                    let cols: Vec<J> = p.columns.iter().map(|c| json!([c.0, c.1])).collect();
                    json!({"name": p.name, "columns": cols})
                })
                .collect(),
        )
    }

    /// Bad entries are skipped; an empty or broken table gives the built-in one.
    pub fn from_json(j: &J) -> Self {
        let mut t = PenTable { pens: vec![] };
        for p in j.as_array().into_iter().flatten() {
            let columns: Vec<(u32, f32)> = p["columns"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| Some((c[0].as_u64()? as u32, c[1].as_f64()? as f32)))
                .collect();
            if let Some(name) = p["name"].as_str() {
                t.set(Pen {
                    name: name.into(),
                    columns,
                });
            }
        }
        if t.pens.is_empty() {
            Self::default()
        } else {
            t
        }
    }
}

/// Pen, pattern and colour of one line kind.
#[derive(Clone, Debug, PartialEq)]
pub struct LineStyle {
    pub pen: String,
    /// Name from the line pattern library; "solid" draws a plain line.
    pub pattern: String,
    /// 0xRRGGBB; None keeps the category colour.
    pub color: Option<u32>,
}

impl LineStyle {
    pub fn new(pen: &str, pattern: &str) -> Self {
        Self {
            pen: pen.into(),
            pattern: pattern.into(),
            color: None,
        }
    }

    fn to_json(&self) -> J {
        json!({"pen": self.pen, "pattern": self.pattern, "color": self.color})
    }

    fn from_json(j: &J) -> Option<Self> {
        Some(Self {
            pen: j["pen"].as_str()?.into(),
            pattern: j["pattern"].as_str().unwrap_or("solid").into(),
            color: j["color"]
                .as_u64()
                .filter(|c| *c <= 0xFF_FFFF)
                .map(|c| c as u32),
        })
    }
}

/// Per-category graphic override: separate cut and projection styles and halftone.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StyleOverride {
    pub cut: Option<LineStyle>,
    pub projection: Option<LineStyle>,
    pub halftone: bool,
}

impl StyleOverride {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    pub fn to_json(&self) -> J {
        json!({
            "cut": self.cut.as_ref().map(LineStyle::to_json),
            "projection": self.projection.as_ref().map(LineStyle::to_json),
            "halftone": self.halftone,
        })
    }

    pub fn from_json(j: &J) -> Self {
        Self {
            cut: LineStyle::from_json(&j["cut"]),
            projection: LineStyle::from_json(&j["projection"]),
            halftone: j["halftone"].as_bool().unwrap_or(false),
        }
    }
}

/// Overrides of one view by category key.
pub type StyleMap = BTreeMap<String, StyleOverride>;

/// A line ready to draw.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedLine {
    pub mm: f32,
    pub pattern: String,
    pub color: Option<u32>,
    pub halftone: bool,
}

/// Default style per band when no override is set.
pub fn default_style(band: Band) -> LineStyle {
    match band {
        Band::Cut => LineStyle::new("bold", "solid"),
        Band::Seen | Band::Hidden => LineStyle::new("light", "solid"),
        Band::Overhead => LineStyle::new("fine", "overhead"),
        Band::Beyond => LineStyle::new("fine", "solid"),
    }
}

/// Style of a line in `band`: the override's cut or projection style over the default, weight from
/// the pen table at `denom`. Beyond lines are always halftone.
pub fn resolve(
    table: &PenTable,
    ov: Option<&StyleOverride>,
    band: Band,
    denom: u32,
) -> ResolvedLine {
    let own = ov.and_then(|o| match band {
        Band::Cut => o.cut.as_ref(),
        _ => o.projection.as_ref(),
    });
    let st = own.cloned().unwrap_or_else(|| default_style(band));
    ResolvedLine {
        mm: table.mm(&st.pen, denom),
        pattern: st.pattern,
        color: st.color,
        halftone: ov.is_some_and(|o| o.halftone) || band == Band::Beyond,
    }
}

/// Screen width in px for a paper weight: `px_per_mm` follows zoom and scale; a floor keeps thin
/// pens visible, and `thin_lines` forces the floor for every pen.
pub fn screen_px(mm: f32, px_per_mm: f32, thin_lines: bool) -> f32 {
    const FLOOR: f32 = 1.0;
    if thin_lines {
        FLOOR
    } else {
        (mm * px_per_mm).max(FLOOR)
    }
}

/// Blends 0xRRGGBB `c` towards `bg` by `k` (0..1); the halftone look.
pub fn halftone(c: u32, bg: u32, k: f32) -> u32 {
    let k = k.clamp(0.0, 1.0);
    let ch = |s: u32| {
        let a = ((c >> s) & 0xFF) as f32;
        let b = ((bg >> s) & 0xFF) as f32;
        ((a + (b - a) * k).round() as u32).min(255)
    };
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pen(name: &str, columns: Vec<(u32, f32)>) -> Pen {
        Pen {
            name: name.into(),
            columns,
        }
    }

    #[test]
    fn default_table_is_monotonic() {
        let t = PenTable::default();
        for p in &t.pens {
            assert!(p.check(), "{}", p.name);
        }
        for d in COLUMNS {
            let w: Vec<f32> = t.pens.iter().map(|p| p.at(d)).collect();
            assert!(w.windows(2).all(|x| x[0] < x[1]), "{d}: {w:?}");
        }
        assert!(t.mm("bold", 500) < t.mm("bold", 20));
    }

    #[test]
    fn nearest_column_on_log_scale() {
        let p = pen("x", vec![(50, 0.3), (200, 0.2)]);
        assert_eq!(p.at(50), 0.3);
        assert_eq!(p.at(90), 0.3);
        assert_eq!(p.at(120), 0.2);
        assert_eq!(p.at(1000), 0.2);
        assert_eq!(p.at(1), 0.3);
    }

    #[test]
    fn set_rejects_bad_pens_and_replaces_by_name() {
        let mut t = PenTable::default();
        let n = t.pens.len();
        assert!(!t.set(pen("", vec![(50, 0.2)])));
        assert!(!t.set(pen("z", vec![(50, f32::NAN)])));
        assert!(!t.set(pen("z", vec![])));
        assert!(!t.set(pen("z", vec![(50, 0.001)])));
        assert!(t.set(pen("bold", vec![(100, 0.5), (50, 0.6)])));
        assert_eq!(t.pens.len(), n);
        assert_eq!(t.get("bold").unwrap().columns, vec![(50, 0.6), (100, 0.5)]);
        assert!(t.set(pen("mine", vec![(50, 0.4)])));
        assert!(t.remove("mine") && !t.remove("mine"));
    }

    #[test]
    fn json_round_trip_and_fallback() {
        let mut t = PenTable::default();
        t.set(pen("mine", vec![(50, 0.42)]));
        assert_eq!(PenTable::from_json(&t.to_json()), t);
        assert_eq!(PenTable::from_json(&json!("junk")), PenTable::default());
        assert_eq!(
            PenTable::from_json(&json!([{"name": "a", "columns": []}])),
            PenTable::default()
        );
    }

    #[test]
    fn cut_and_projection_resolve_separately() {
        let t = PenTable::default();
        let ov = StyleOverride {
            cut: Some(LineStyle::new("heavy", "solid")),
            projection: Some(LineStyle::new("hair", "dash")),
            halftone: true,
        };
        let cut = resolve(&t, Some(&ov), Band::Cut, 100);
        let seen = resolve(&t, Some(&ov), Band::Seen, 100);
        assert_eq!(cut.mm, t.mm("heavy", 100));
        assert_eq!(seen.mm, t.mm("hair", 100));
        assert_eq!(seen.pattern, "dash");
        assert!(cut.halftone && seen.halftone);
        assert!(!resolve(&t, None, Band::Seen, 100).halftone);
        assert!(resolve(&t, None, Band::Beyond, 100).halftone);
        assert_eq!(resolve(&t, None, Band::Overhead, 100).pattern, "overhead");
    }

    #[test]
    fn override_json_round_trip() {
        let ov = StyleOverride {
            cut: Some(LineStyle {
                pen: "bold".into(),
                pattern: "solid".into(),
                color: Some(0x112233),
            }),
            projection: None,
            halftone: true,
        };
        assert_eq!(StyleOverride::from_json(&ov.to_json()), ov);
        assert!(StyleOverride::from_json(&json!(null)).is_empty());
    }

    #[test]
    fn screen_width_and_halftone() {
        assert_eq!(screen_px(0.1, 2.0, false), 1.0);
        assert_eq!(screen_px(0.5, 8.0, false), 4.0);
        assert_eq!(screen_px(0.5, 8.0, true), 1.0);
        assert_eq!(halftone(0x000000, 0xFFFFFF, 0.5), 0x808080);
        assert_eq!(halftone(0x123456, 0xFFFFFF, 0.0), 0x123456);
        assert_eq!(halftone(0x123456, 0xFFFFFF, 1.0), 0xFFFFFF);
    }
}
