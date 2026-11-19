//! One table of typed numbers fed to every typed-value path with the decimal mark passed
//! explicitly: a path gives the table's metres or refuses with a reason, never another number.
//!
//! Paths: length fields (`parse_edit_length_lang`: layers window, imported props, schedules,
//! clash), `ui_kit::len_commit` (property rows, signed), 3D array/mirror bar (`read_typed`),
//! plan Move/Copy/Offset typed distance (`modify_tools::typed_length`) and array count
//! (`typed_count`), wall/arch tool bar (`parse_typed`), slab shape-edit typed elevation
//! (`parse_typed_signed`, signed), plain numbers (`parse_locale_number`: rotate, scale, topo,
//! 3D angle).

use crate::edit3d::mirror_array::{read_typed, TypedErr};
use crate::modify_tools::{typed_count, typed_length, ModTool, Op, Out};
use crate::ui_kit::len_commit;
use crate::units::{
    parse_edit_length_lang, parse_locale_number, parse_typed, DecimalMark, Typed, Units, FOOT, INCH,
};

const EN: DecimalMark = DecimalMark::Point;
const RU: DecimalMark = DecimalMark::Comma;
const EPS: f64 = 1e-9;

/// text, metres under the English mark, metres under the Russian mark (None: refused).
type Row = (&'static str, Option<f64>, Option<f64>);

fn same(a: f64, b: f64) -> bool {
    (a - b).abs() < EPS
}

fn matches_want(got: Option<f64>, want: Option<f64>) -> bool {
    match (got, want) {
        (Some(g), Some(w)) => same(g, w),
        (None, None) => true,
        _ => false,
    }
}

const METRIC: &[Row] = &[
    ("3 000", Some(3.0), Some(3.0)),
    ("3\u{a0}000", Some(3.0), Some(3.0)),
    ("3\u{202f}000", Some(3.0), Some(3.0)),
    ("3,000", Some(3.0), Some(0.003)),
    ("3,5", None, Some(0.0035)),
    ("1.5", Some(0.0015), Some(0.0015)),
    ("-2", None, None),
    ("300 mm", Some(0.3), Some(0.3)),
    ("300 \u{43c}\u{43c}", Some(0.3), Some(0.3)),
    ("1,5 m", None, Some(1.5)),
    ("1.5 m", Some(1.5), Some(1.5)),
    ("0", None, None),
    ("abc", None, None),
    ("3 0000", None, None),
];

const IMPERIAL: &[Row] = &[
    ("10'-6\"", Some(10.5 * FOOT), Some(10.5 * FOOT)),
    ("10'6\"", Some(10.5 * FOOT), Some(10.5 * FOOT)),
    ("2' 6\"", Some(2.5 * FOOT), Some(2.5 * FOOT)),
    ("6 1/2\"", Some(6.5 * INCH), Some(6.5 * INCH)),
    ("12.5\"", Some(12.5 * INCH), Some(12.5 * INCH)),
    ("12,5\"", None, Some(12.5 * INCH)),
    ("300 mm", Some(0.3), Some(0.3)),
    ("-2\"", None, None),
];

fn rows() -> impl Iterator<Item = (Units, &'static Row)> {
    METRIC
        .iter()
        .map(|r| (Units::Metric, r))
        .chain(IMPERIAL.iter().map(|r| (Units::Imperial, r)))
}

fn want_for(r: &Row, m: DecimalMark) -> Option<f64> {
    if m == EN {
        r.1
    } else {
        r.2
    }
}

#[test]
fn length_field_table() {
    for (u, r) in rows() {
        for m in [EN, RU] {
            let got = parse_edit_length_lang(r.0, u, &crate::expr_field::Scope::none(), m);
            if let Err(e) = &got {
                assert!(!e.text(true).is_empty() && !e.text(false).is_empty(), "{:?}: silent refusal", r.0);
            }
            assert!(matches_want(got.ok(), want_for(r, m)), "{:?} {u:?} {m:?}", r.0);
        }
    }
}

#[test]
fn property_row_commit_table() {
    for (u, r) in rows() {
        for m in [EN, RU] {
            let got = len_commit(r.0, u, 0.0, 1.0e6, m);
            assert!(matches_want(got.ok(), want_for(r, m)), "{:?} {u:?} {m:?}", r.0);
        }
    }
}

#[test]
fn property_row_commit_signed() {
    let rows: [Row; 5] = [
        ("-2", Some(-0.002), Some(-0.002)),
        ("\u{2212}2", Some(-0.002), Some(-0.002)),
        ("-2,5", None, Some(-0.0025)),
        ("-2.5", Some(-0.0025), Some(-0.0025)),
        ("-3 000", Some(-3.0), Some(-3.0)),
    ];
    for r in &rows {
        for m in [EN, RU] {
            let got = len_commit(r.0, Units::Metric, -1.0e6, 1.0e6, m);
            assert!(matches_want(got.ok(), want_for(r, m)), "{:?} {m:?}", r.0);
        }
    }
}

#[test]
fn array_bar_value_table() {
    for (u, r) in rows() {
        for m in [EN, RU] {
            let got = match read_typed(r.0, false, u, m) {
                Ok((None, Some(v))) => Some(v),
                // a count taken out of a bare value is a different number
                Ok((c, v)) => panic!("{:?} {u:?} {m:?}: count {c:?} value {v:?}", r.0),
                Err(_) => None,
            };
            assert!(matches_want(got, want_for(r, m)), "{:?} {u:?} {m:?}", r.0);
        }
    }
}

#[test]
fn array_bar_count_and_value() {
    for m in [EN, RU] {
        assert_eq!(read_typed("4 3 000", false, Units::Metric, m), Ok((Some(4), Some(3.0))));
        assert_eq!(read_typed("4;300 mm", false, Units::Metric, m), Ok((Some(4), Some(0.3))));
        assert_eq!(read_typed("4 2' 6\"", false, Units::Imperial, m).map(|x| x.0), Ok(Some(4)));
        assert_eq!(read_typed("1;300", false, Units::Metric, m), Err(TypedErr::Count));
        assert_eq!(read_typed("4.5 300", false, Units::Metric, m), Err(TypedErr::Count));
    }
    assert_eq!(read_typed("4 3,5", false, Units::Metric, EN), Err(TypedErr::CommaDecimal));
    assert_eq!(read_typed("4 3,5", false, Units::Metric, RU), Ok((Some(4), Some(0.0035))));
}

#[test]
fn array_bar_angle_table() {
    // radial input: degrees, either sign, (0, 360]
    let rows: [(&str, Option<f64>, Option<f64>); 6] = [
        ("90", Some(90.0), Some(90.0)),
        ("-90", Some(-90.0), Some(-90.0)),
        ("22.5", Some(22.5), Some(22.5)),
        ("22,5", None, Some(22.5)),
        ("360.5", None, None),
        ("0", None, None),
    ];
    for (s, en, ru) in rows {
        for (m, want) in [(EN, en), (RU, ru)] {
            let got = read_typed(s, true, Units::Metric, m).ok().and_then(|x| x.1);
            assert!(matches_want(got, want), "{s:?} {m:?}: {got:?}");
        }
    }
}

/// Plan Move/Copy/Offset typed distance: the table's metres in the tool's language, or a
/// refusal with a reason.
#[test]
fn plan_distance_bar_table() {
    for (u, r) in rows() {
        for m in [EN, RU] {
            let got = typed_length(r.0, u, m);
            if let Err(e) = &got {
                assert!(!e.text(true).is_empty() && !e.text(false).is_empty(), "{:?}: silent refusal", r.0);
            }
            assert!(matches_want(got.ok(), want_for(r, m)), "{:?} {u:?} {m:?}", r.0);
        }
    }
}

/// Offset typed on the plan canvas: a refused text changes nothing and says why.
#[test]
fn plan_offset_typed_applies_or_refuses_visibly() {
    let mut ed = crate::editor::Editor::new_in_memory().unwrap();
    // ASCII rows only: the canvas takes digits, `.,-+=m` and a space inside the number
    let rows: [Row; 6] = [
        ("3 000", Some(3.0), Some(3.0)),
        ("3,5", None, Some(0.0035)),
        ("3,000", Some(3.0), Some(0.003)),
        ("1.5", Some(0.0015), Some(0.0015)),
        ("-2", None, None),
        ("0", None, None),
    ];
    for r in &rows {
        for m in [EN, RU] {
            let mut t = ModTool::new(Op::Offset, &[]);
            t.mark = m;
            let before = t.opts.dist;
            r.0.chars().for_each(|c| t.char(c));
            assert_eq!(t.input, r.0, "the canvas keeps what was typed");
            let out = t.enter(&mut ed);
            match want_for(r, m) {
                Some(w) => {
                    assert_eq!(out, Out::Nothing, "{:?} {m:?}", r.0);
                    assert!(same(t.opts.dist, w), "{:?} {m:?}: {}", r.0, t.opts.dist);
                }
                None => {
                    let why = match out {
                        Out::Refused(why) => why,
                        o => panic!("{:?} {m:?}: {o:?}", r.0),
                    };
                    assert!(!why.is_empty() && !why.contains("pair_input."), "{:?}: {why}", r.0);
                    assert!(same(t.opts.dist, before), "{:?} {m:?}: distance changed", r.0);
                }
            }
        }
    }
}

/// Plan array count typed on the canvas: a whole number from 2 to 200 in either language.
#[test]
fn plan_array_count_table() {
    let rows: [(&str, Option<usize>); 9] = [
        ("5", Some(5)),
        (" 12 ", Some(12)),
        ("200", Some(200)),
        ("1", None),
        ("201", None),
        ("4,0", None),
        ("4.5", None),
        ("3,000", None),
        ("4;300", None),
    ];
    for (s, want) in rows {
        for m in [EN, RU] {
            assert_eq!(typed_count(s, m), want, "{s:?} {m:?}");
        }
    }
    for m in [EN, RU] {
        let mut t = ModTool::new(Op::Array, &[crate::editor::Sel::Wall(1)]);
        t.mark = m;
        let before = t.opts.count;
        "3,5".chars().for_each(|c| t.char(c));
        let mut ed = crate::editor::Editor::new_in_memory().unwrap();
        assert!(matches!(t.enter(&mut ed), Out::Refused(_)), "{m:?}");
        assert_eq!(t.opts.count, before, "a refused count changes nothing");
        "7".chars().for_each(|c| t.char(c));
        assert_eq!(t.enter(&mut ed), Out::Nothing);
        assert_eq!(t.opts.count, 7);
    }
}

/// Wall/arch tool bar: a lone length reads exactly like a length field.
#[test]
fn tool_bar_polar_table() {
    for (u, r) in rows() {
        for m in [EN, RU] {
            let got = match parse_typed(r.0, u, m) {
                Ok(Typed::Polar(Some(v), None)) => Some(v),
                Ok(t) => panic!("{:?} {u:?} {m:?}: {t:?}", r.0),
                Err(_) => None,
            };
            assert!(matches_want(got, want_for(r, m)), "{:?} {u:?} {m:?}: {got:?}", r.0);
        }
    }
    for m in [EN, RU] {
        assert_eq!(parse_typed("3000<90", Units::Metric, m), Ok(Typed::Polar(Some(3.0), Some(90.0))));
        assert_eq!(parse_typed("@1000;500", Units::Metric, m), Ok(Typed::Pair(1.0, 0.5)));
        assert_eq!(parse_typed("@3 000;-1 500", Units::Metric, m), Ok(Typed::Pair(3.0, -1.5)));
    }
    assert_eq!(parse_typed("@1000;2,5", Units::Metric, RU), Ok(Typed::Pair(1.0, 0.0025)));
    assert!(parse_typed("@1000;2,5", Units::Metric, EN).is_err());
}

/// Slab shape-edit elevation typed on the canvas: the table's metres in the project units, or a
/// visible refusal that leaves the elevation and the shape as they were. Elevations are signed,
/// so zero and a minus are values here.
#[test]
fn shape_edit_elevation_table() {
    use crate::shape_edit::{Grip, ShapeEditor};
    use crate::wall_tool::{Event, Key};
    let signed = |r: &Row, u: Units| -> Row {
        match (r.0, u) {
            ("-2", Units::Metric) => ("-2", Some(-0.002), Some(-0.002)),
            ("0", _) => ("0", Some(0.0), Some(0.0)),
            ("-2\"", Units::Imperial) => ("-2\"", Some(-2.0 * INCH), Some(-2.0 * INCH)),
            _ => *r,
        }
    };
    let ring: Vec<_> = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)]
        .iter()
        .map(|&(x, y)| ddc_geom::v2(x, y))
        .collect();
    for (u, r) in rows() {
        let r = signed(r, u);
        for m in [EN, RU] {
            let mut s = ShapeEditor::new(1, vec![ring.clone()], 0.3, None);
            s.units = u;
            s.mark = m;
            s.grip = Some(Grip::Vertex(0, 1));
            let (dz0, shape0) = (s.dz, format!("{:?}", s.shape));
            r.0.chars().for_each(|c| assert_eq!(s.handle(Event::Char(c)), None));
            assert_eq!(s.input, r.0, "the canvas keeps what was typed");
            assert_eq!(s.handle(Event::Key(Key::Enter)), None, "{:?}", r.0);
            match want_for(&r, m) {
                Some(w) => {
                    assert!(same(s.dz, w), "{:?} {u:?} {m:?}: {}", r.0, s.dz);
                    assert!(same(s.shape.ring_dz[0][1], w), "{:?} {u:?} {m:?}", r.0);
                    assert!(s.input.is_empty());
                }
                None => {
                    let why = s.error.clone().unwrap_or_default();
                    let raw = why.contains(&["host_shape", "."].concat());
                    assert!(!why.is_empty() && !raw, "{:?} {m:?}: {why}", r.0);
                    assert!(same(s.dz, dz0), "{:?} {u:?} {m:?}: elevation changed", r.0);
                    assert_eq!(format!("{:?}", s.shape), shape0, "{:?}: shape changed", r.0);
                    assert_eq!(s.input, r.0, "refused text stays for correction");
                }
            }
        }
    }
}

#[test]
fn tool_bar_comma_is_not_a_silent_pair() {
    // a lone number with a comma is a decimal (ru) or a group (en) or refused; never an offset pair
    for s in ["3,5", "3,000", "1,5 m", "@1000,500", "6000,4000"] {
        for m in [EN, RU] {
            if let Ok(Typed::Pair(a, b)) = parse_typed(s, Units::Metric, m) {
                panic!("{s:?} {m:?} read as the pair ({a}, {b})");
            }
        }
    }
}

#[test]
fn plain_number_table() {
    // rotate angle, scale factor, topo fields, 3D angle: no units
    let rows: [Row; 12] = [
        ("3 000", Some(3000.0), Some(3000.0)),
        ("3\u{a0}000", Some(3000.0), Some(3000.0)),
        ("3,000", Some(3000.0), Some(3.0)),
        ("3,5", None, Some(3.5)),
        ("1.5", Some(1.5), Some(1.5)),
        ("-2", Some(-2.0), Some(-2.0)),
        ("\u{2212}2", Some(-2.0), Some(-2.0)),
        ("-2,5", None, Some(-2.5)),
        ("300 mm", None, None),
        ("10'-6\"", None, None),
        ("", None, None),
        ("1 5", None, None),
    ];
    for r in &rows {
        for m in [EN, RU] {
            let got = parse_locale_number(r.0, m).ok();
            assert!(matches_want(got, want_for(r, m)), "{:?} {m:?}: {got:?}", r.0);
        }
    }
}
