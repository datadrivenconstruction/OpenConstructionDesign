// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Length unit of a terrain source. A survey file or drawing may be in m, cm, mm, ft, US survey ft
//! or in; the import dialog always asks. The default comes from the file (DXF `$INSUNITS`, a unit
//! in the survey header row or a `# units: mm` comment), else from the extent as a suggestion.

use super::TopoSource;

/// Length unit of the coordinates in a terrain file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SurveyUnit {
    #[default]
    M,
    Cm,
    Mm,
    Ft,
    UsFt,
    In,
}

/// Where the dialog's default unit comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnitSource {
    /// The file names its unit.
    Header,
    /// The file names none: a guess from the extent, shown as a suggestion.
    Extent,
}

/// Default unit of a file and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnitGuess {
    pub unit: SurveyUnit,
    pub source: UnitSource,
}

impl SurveyUnit {
    pub const ALL: [SurveyUnit; 6] = [
        SurveyUnit::M,
        SurveyUnit::Cm,
        SurveyUnit::Mm,
        SurveyUnit::Ft,
        SurveyUnit::UsFt,
        SurveyUnit::In,
    ];

    /// Metres per unit.
    pub fn meters(self) -> f64 {
        match self {
            SurveyUnit::M => 1.0,
            SurveyUnit::Cm => 0.01,
            SurveyUnit::Mm => 0.001,
            SurveyUnit::Ft => 0.3048,
            SurveyUnit::UsFt => 1200.0 / 3937.0,
            SurveyUnit::In => 0.0254,
        }
    }

    /// Stable key.
    pub fn key(self) -> &'static str {
        match self {
            SurveyUnit::M => "m",
            SurveyUnit::Cm => "cm",
            SurveyUnit::Mm => "mm",
            SurveyUnit::Ft => "ft",
            SurveyUnit::UsFt => "usft",
            SurveyUnit::In => "in",
        }
    }

    /// Unit of a DXF `$INSUNITS` code; None for unitless and units the dialog does not offer.
    pub fn from_insunits(code: i32) -> Option<SurveyUnit> {
        match code {
            1 => Some(SurveyUnit::In),
            2 => Some(SurveyUnit::Ft),
            4 => Some(SurveyUnit::Mm),
            5 => Some(SurveyUnit::Cm),
            6 => Some(SurveyUnit::M),
            21 => Some(SurveyUnit::UsFt),
            _ => None,
        }
    }

    /// Unit named by a header or comment token (`m`, `mm`, `metres`, `ft`, `usft`, ...).
    pub fn from_token(t: &str) -> Option<SurveyUnit> {
        let t = t.trim().trim_end_matches('.').to_ascii_lowercase();
        Some(match t.as_str() {
            "m" | "meter" | "meters" | "metre" | "metres" => SurveyUnit::M,
            "cm" | "centimeter" | "centimeters" | "centimetre" | "centimetres" => SurveyUnit::Cm,
            "mm" | "millimeter" | "millimeters" | "millimetre" | "millimetres" => SurveyUnit::Mm,
            "ft" | "feet" | "foot" | "'" | "intl ft" | "international feet" => SurveyUnit::Ft,
            "usft" | "us ft" | "us_ft" | "us-ft" | "sft" | "ftus" | "us survey feet"
            | "us survey foot" | "survey ft" | "survey feet" => SurveyUnit::UsFt,
            "in" | "inch" | "inches" | "\"" => SurveyUnit::In,
            // Russian unit marks in header rows (m, cm, mm)
            "\u{43c}" => SurveyUnit::M,
            "\u{441}\u{43c}" => SurveyUnit::Cm,
            "\u{43c}\u{43c}" => SurveyUnit::Mm,
            _ => return None,
        })
    }
}

/// Header cell split into the column name and its unit: `x(mm)`, `X [m]`, `x, ft`, `z_mm`, `E ft`.
pub fn split_unit(cell: &str) -> (String, Option<SurveyUnit>) {
    let c = cell.trim();
    if let Some(open) = c.rfind(['(', '[']) {
        let close = if c[open..].starts_with('(') { ')' } else { ']' };
        if let Some(inner) = c[open + 1..].strip_suffix(close) {
            if let Some(u) = SurveyUnit::from_token(inner) {
                return (c[..open].trim().to_string(), Some(u));
            }
        }
    }
    for sep in [',', '_', ' '] {
        if let Some(k) = c.rfind(sep) {
            if let Some(u) = SurveyUnit::from_token(&c[k + 1..]) {
                let name = c[..k].trim();
                if !name.is_empty() {
                    return (name.to_string(), Some(u));
                }
            }
        }
    }
    (c.to_string(), None)
}

/// Unit of a comment line such as `# units: mm` or `// unit = ft`.
pub fn comment_unit(line: &str) -> Option<SurveyUnit> {
    let l = line
        .trim_start_matches(['#', '/', ';', '!'])
        .trim()
        .to_ascii_lowercase();
    let rest = l
        .strip_prefix("units")
        .or_else(|| l.strip_prefix("unit"))?
        .trim_start();
    let rest = rest.strip_prefix([':', '='])?;
    SurveyUnit::from_token(rest)
}

/// Spans of the coordinates (max - min) along x, y, z and the largest |z|.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Span {
    pub dx: f64,
    pub dy: f64,
    pub dz: f64,
    pub max_abs_z: f64,
}

impl Span {
    /// The same span in another unit (`k` - metres per file unit).
    pub fn scaled(self, k: f64) -> Span {
        Span {
            dx: self.dx * k,
            dy: self.dy * k,
            dz: self.dz * k,
            max_abs_z: self.max_abs_z * k,
        }
    }
}

/// Span of all points, breakline vertices and the boundary; None when there is nothing.
pub fn span(src: &TopoSource) -> Option<Span> {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    let mut z_abs = 0.0f64;
    let mut any = false;
    let mut add = |p: (f64, f64, f64), with_z: bool| {
        any = true;
        let n = if with_z { 3 } else { 2 };
        for ((l, h), v) in lo.iter_mut().zip(hi.iter_mut()).zip([p.0, p.1, p.2]).take(n) {
            *l = l.min(v);
            *h = h.max(v);
        }
        if with_z {
            z_abs = z_abs.max(p.2.abs());
        }
    };
    for &p in src.points.iter().chain(src.breaklines.iter().flatten()) {
        add(p, true);
    }
    for &(x, y) in &src.boundary {
        add((x, y, 0.0), false);
    }
    if !any {
        return None;
    }
    let d = |k: usize| if hi[k] >= lo[k] { hi[k] - lo[k] } else { 0.0 };
    Some(Span {
        dx: d(0),
        dy: d(1),
        dz: d(2),
        max_abs_z: z_abs,
    })
}

/// Largest plan span read as metres that still looks like one site.
const SITE_MAX_M: f64 = 10_000.0;
/// No ground point on Earth is this far from sea level.
const ELEV_MAX_M: f64 = 9_000.0;

/// Unit suggested by the extent when the file names none: a plan span over 10 km or an elevation
/// over 9 km cannot be metres, so the survey is taken as mm; else metres. Feet cannot be told
/// from metres by size: the dialog shows the guess as a suggestion and the resulting extent.
pub fn guess_by_extent(s: Option<Span>) -> SurveyUnit {
    match s {
        Some(s) if s.dx.max(s.dy) > SITE_MAX_M || s.max_abs_z > ELEV_MAX_M => SurveyUnit::Mm,
        _ => SurveyUnit::M,
    }
}

/// The file's unit when it names one, else the extent's suggestion.
pub fn guess(header: Option<SurveyUnit>, s: Option<Span>) -> UnitGuess {
    match header {
        Some(unit) => UnitGuess {
            unit,
            source: UnitSource::Header,
        },
        None => UnitGuess {
            unit: guess_by_extent(s),
            source: UnitSource::Extent,
        },
    }
}

/// Whether a plan span in metres looks wrong for a site (over 10 km, or under 0.5 m).
pub fn implausible(s: &Span) -> bool {
    let plan = s.dx.max(s.dy);
    plan > SITE_MAX_M || (plan > 0.0 && plan < 0.5)
}

impl TopoSource {
    /// Converts coordinates to metres (`k` - metres per file unit).
    pub fn scale(&mut self, k: f64) {
        if k == 1.0 {
            return;
        }
        let p3 = |p: &mut (f64, f64, f64)| *p = (p.0 * k, p.1 * k, p.2 * k);
        self.points.iter_mut().for_each(p3);
        self.breaklines.iter_mut().flatten().for_each(p3);
        for b in &mut self.boundary {
            *b = (b.0 * k, b.1 * k);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(points: Vec<(f64, f64, f64)>) -> TopoSource {
        TopoSource {
            points,
            ..Default::default()
        }
    }

    #[test]
    fn each_unit_scales_to_metres() {
        let want = [
            (SurveyUnit::M, 1.0),
            (SurveyUnit::Cm, 0.01),
            (SurveyUnit::Mm, 0.001),
            (SurveyUnit::Ft, 0.3048),
            (SurveyUnit::UsFt, 0.304_800_609_601_219_2),
            (SurveyUnit::In, 0.0254),
        ];
        for (u, k) in want {
            assert!((u.meters() - k).abs() < 1e-15, "{u:?}");
            let mut s = src(vec![(100.0, 200.0, 10.0)]);
            s.breaklines = vec![vec![(1.0, 2.0, 3.0)]];
            s.boundary = vec![(4.0, 5.0)];
            s.scale(u.meters());
            let p = s.points[0];
            assert!((p.0 - 100.0 * k).abs() < 1e-9 && (p.1 - 200.0 * k).abs() < 1e-9);
            assert!((p.2 - 10.0 * k).abs() < 1e-9, "{u:?} z");
            assert!((s.breaklines[0][0].2 - 3.0 * k).abs() < 1e-12);
            assert!((s.boundary[0].1 - 5.0 * k).abs() < 1e-12);
        }
        assert_eq!(SurveyUnit::ALL.len(), 6);
        assert_eq!(SurveyUnit::default(), SurveyUnit::M);
    }

    #[test]
    fn insunits_codes() {
        assert_eq!(SurveyUnit::from_insunits(4), Some(SurveyUnit::Mm));
        assert_eq!(SurveyUnit::from_insunits(5), Some(SurveyUnit::Cm));
        assert_eq!(SurveyUnit::from_insunits(6), Some(SurveyUnit::M));
        assert_eq!(SurveyUnit::from_insunits(2), Some(SurveyUnit::Ft));
        assert_eq!(SurveyUnit::from_insunits(21), Some(SurveyUnit::UsFt));
        assert_eq!(SurveyUnit::from_insunits(1), Some(SurveyUnit::In));
        assert_eq!(SurveyUnit::from_insunits(0), None, "unitless asks");
        assert_eq!(SurveyUnit::from_insunits(7), None, "km is not offered");
    }

    #[test]
    fn header_cells_and_comments_name_units() {
        assert_eq!(split_unit("x(mm)"), ("x".into(), Some(SurveyUnit::Mm)));
        assert_eq!(split_unit("Easting [m]"), ("Easting".into(), Some(SurveyUnit::M)));
        assert_eq!(split_unit("z_ft"), ("z".into(), Some(SurveyUnit::Ft)));
        assert_eq!(split_unit("Z (US ft)"), ("Z".into(), Some(SurveyUnit::UsFt)));
        assert_eq!(split_unit("x, cm"), ("x".into(), Some(SurveyUnit::Cm)));
        assert_eq!(split_unit("N in"), ("N".into(), Some(SurveyUnit::In)));
        assert_eq!(split_unit("x(\u{43c}\u{43c})"), ("x".into(), Some(SurveyUnit::Mm)));
        assert_eq!(split_unit("elevation"), ("elevation".into(), None));
        assert_eq!(split_unit("m"), ("m".into(), None), "a bare unit is no column");
        assert_eq!(comment_unit("# units: mm"), Some(SurveyUnit::Mm));
        assert_eq!(comment_unit("// Unit = ft"), Some(SurveyUnit::Ft));
        assert_eq!(comment_unit("# survey of lot 5"), None);
    }

    #[test]
    fn extent_guess_picks_mm_for_a_mm_survey() {
        // 80 m x 60 m site at 101..104 m elevation, written in mm
        let mm = src(vec![
            (0.0, 0.0, 101_000.0),
            (80_000.0, 0.0, 102_500.0),
            (0.0, 60_000.0, 104_000.0),
        ]);
        let s = span(&mm).unwrap();
        assert_eq!((s.dx, s.dy, s.dz), (80_000.0, 60_000.0, 3_000.0));
        assert_eq!(guess_by_extent(Some(s)), SurveyUnit::Mm);
        // local mm survey near z = 0: the plan span decides
        let flat = src(vec![(0.0, 0.0, 0.0), (45_000.0, 30_000.0, 1_200.0)]);
        assert_eq!(guess_by_extent(span(&flat)), SurveyUnit::Mm);
        // geodetic metres: large coordinates, small span
        let m = src(vec![(512_300.0, 6_123_400.0, 100.0), (512_380.0, 6_123_460.0, 104.0)]);
        assert_eq!(guess_by_extent(span(&m)), SurveyUnit::M);
        assert_eq!(guess_by_extent(None), SurveyUnit::M);
        let g = guess(None, span(&mm));
        assert_eq!((g.unit, g.source), (SurveyUnit::Mm, UnitSource::Extent));
        let h = guess(Some(SurveyUnit::Ft), span(&mm));
        assert_eq!((h.unit, h.source), (SurveyUnit::Ft, UnitSource::Header), "header wins");
    }

    #[test]
    fn plausibility_of_the_result() {
        let s = |d: f64| Span {
            dx: d,
            dy: d / 2.0,
            ..Default::default()
        };
        assert!(implausible(&s(80_000.0)), "80 km");
        assert!(implausible(&s(0.08)), "8 cm");
        assert!(!implausible(&s(80.0)));
        assert!(!implausible(&s(0.0)), "one point");
        let k = s(80_000.0).scaled(0.001);
        assert!((k.dx - 80.0).abs() < 1e-12);
    }
}
