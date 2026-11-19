// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! QA: items without a direct test: text codecs, small tables,
//! base element API, undo round trips, plus parse paths fed hostile text.

use crate::changes::Changes;
use crate::xlsx_rec::{split_key, split_value};
use crate::num_text::{group_like, parse_invariant, parse_stored};
use crate::params::ParamType;
use crate::phases::std_phase_en;
use crate::sketch::{parse_shape, parse_sketch, shape_text, sketch_text};
use crate::slabs::{parse_slope, slope_text};
use crate::wall_types::LayerFn;
use crate::{join_priority, Model};
use ddc_geom::math::v2;
use ddc_geom::shape_edit::{ShapeLine, SlabShape, Thickness};
use ddc_geom::sketch::{Sketch, SketchCurve};
use ddc_geom::slope::Plane;

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 11) as f64) / ((1u64 << 53) as f64)
    }
}

// ---- num_text ----
#[test]
fn group_like_cases() {
    assert!(group_like("1", "000"));
    assert!(group_like("12", "500"));
    assert!(group_like("123", "456"));
    assert!(!group_like("0", "250"), "leading zero is a decimal");
    assert!(!group_like("1234", "567"), "four leading digits");
    assert!(!group_like("1", "50"));
    assert!(!group_like("", "000"));
    assert!(!group_like("1", "0000"));
}

#[test]
fn stored_numbers_follow_the_documented_rule() {
    assert_eq!(parse_stored("1,5"), Some(1.5));
    assert_eq!(parse_stored("0,250"), Some(0.25));
    assert_eq!(parse_stored("1234,567"), Some(1234.567));
    assert_eq!(parse_stored("1,000"), None);
    assert_eq!(parse_stored("-12,500"), None);
    assert_eq!(parse_stored("-1250.5"), Some(-1250.5));
    assert_eq!(parse_stored("1.5e3"), Some(1500.0));
    assert_eq!(parse_stored(".5"), Some(0.5));
    for bad in ["", " ", "abc", "NaN", "inf", "1,2,3", "1 5", "1.2,3", "--1", ","] {
        assert_eq!(parse_stored(bad), None, "{bad:?}");
    }
    assert!(parse_invariant("1e999").is_none(), "overflow is not a number");
    // writer form always reads back
    let mut r = Lcg(3);
    for _ in 0..1000 {
        let v = (r.next() - 0.5) * 10f64.powi((r.next() * 20.0) as i32 - 6);
        assert_eq!(parse_invariant(&v.to_string()), Some(v));
    }
}

// ---- xlsx_rec ----
#[test]
fn record_key_and_value_split() {
    assert_eq!(split_key("book|sheet|fp|3"), Some(("book", "sheet", "fp", "3")));
    assert_eq!(split_key("sheet|fp|3"), Some(("sheet", "fp", "", "3")));
    assert_eq!(split_key("a|b"), None);
    assert_eq!(split_key(""), None);
    assert_eq!(split_key("a|b|c|d|e"), Some(("a|b", "c", "d", "e")), "book keeps pipes");
    assert_eq!(split_value("42"), Some((42, None)));
    assert_eq!(split_value("42@stamp"), Some((42, Some("stamp"))));
    assert_eq!(split_value("-7@"), Some((-7, Some(""))));
    assert_eq!(split_value("x@1"), None);
    assert_eq!(split_value(""), None);
    assert_eq!(split_value("99999999999999999999"), None);
}

// ---- slabs ----
#[test]
fn slope_text_round_trip_and_errors() {
    let p = Plane { p0: v2(1.5, -2.0), gx: 0.02, gy: -0.125 };
    assert_eq!(parse_slope(&slope_text(&p)).unwrap(), p);
    assert!(parse_slope("").is_err());
    assert!(parse_slope("1 2 3").is_err());
    assert!(parse_slope("1 2 3 4 5").is_err());
    assert!(parse_slope("1 2 3 x").is_err());
    assert!(parse_slope("1 2 3 NaN").is_err(), "non-finite refused");
    assert!(parse_slope("1 2 inf 0").is_err());
    let mut r = Lcg(8);
    for _ in 0..300 {
        let p = Plane {
            p0: v2((r.next() - 0.5) * 1e4, (r.next() - 0.5) * 1e4),
            gx: (r.next() - 0.5) * 0.3,
            gy: (r.next() - 0.5) * 0.3,
        };
        assert_eq!(parse_slope(&slope_text(&p)).unwrap(), p, "bit-exact");
    }
}

// ---- sketch codecs ----
fn sample_sketch() -> Sketch {
    Sketch {
        curves: vec![
            SketchCurve::Line { a: v2(0., 0.), b: v2(1., 2.) },
            SketchCurve::Arc { c: v2(1., 1.), r: 2.0, a0: 0.5, sweep: 1.5 },
            SketchCurve::Ellipse { c: v2(0., 0.), rx: 3., ry: 2., rot: 0.1, t0: 0., sweep: 3. },
            SketchCurve::Spline { ctrl: vec![v2(0., 0.), v2(1., 1.), v2(2., 0.)] },
        ],
    }
}

#[test]
fn sketch_text_round_trip() {
    let s = sample_sketch();
    let t = sketch_text(&s);
    assert_eq!(parse_sketch(&t).unwrap(), s);
    assert_eq!(sketch_text(&parse_sketch(&t).unwrap()), t, "text is canonical");
    assert_eq!(parse_sketch("").unwrap(), Sketch::default());
    assert_eq!(parse_sketch("\n  \n").unwrap(), Sketch::default());
    assert_eq!(sketch_text(&Sketch::default()), "");
}

#[test]
fn sketch_parse_errors() {
    for bad in ["L 1 2 3", "L 1 2 3 4 5", "A 1 2 3 4", "E 1 2 3 4 5 6", "S 1 2", "S 1 2 3", "Q 1 2", "L a b c d"] {
        assert!(parse_sketch(bad).is_err(), "{bad}");
    }
}

#[test]
fn sketch_parse_rejects_non_finite_numbers() {
    // slab slope text refuses NaN/inf; the sketch codec must too, or a NaN reaches geometry
    assert!(parse_sketch("L NaN 0 1 1").is_err());
    assert!(parse_sketch("L inf 0 1 1").is_err());
    assert!(parse_shape("P NaN 0 1", Thickness::Varying).is_err());
}

#[test]
fn sketch_parse_non_ascii_does_not_panic() {
    let r = std::panic::catch_unwind(|| parse_sketch("\u{416} 1 2 3 4"));
    assert!(matches!(r, Ok(Err(_))), "multibyte first char must be an error, not a panic");
    let r = std::panic::catch_unwind(|| parse_shape("\u{416} 1 2 3", Thickness::Varying));
    assert!(matches!(r, Ok(Err(_))));
}

#[test]
fn shape_text_round_trip() {
    let s = SlabShape {
        ring_dz: vec![vec![0.0, 0.05, 0.0, 0.1], vec![], vec![0.2]],
        points: vec![(v2(1., 1.), 0.3)],
        lines: vec![ShapeLine { a: v2(0., 0.), b: v2(2., 2.), dza: 0.1, dzb: 0.2 }],
        mode: Thickness::Constant,
    };
    let t = shape_text(&s);
    let back = parse_shape(&t, Thickness::Constant).unwrap();
    assert_eq!(back.points, s.points);
    assert_eq!(back.lines, s.lines);
    assert_eq!(back.ring_dz[0], vec![0.0, 0.05, 0.0, 0.1]);
    assert_eq!(back.ring_dz[2], vec![0.2]);
    assert_eq!(shape_text(&back), t);
    assert_eq!(shape_text(&SlabShape::default()), "");
    assert!(parse_shape("R 0 1", Thickness::Varying).is_err());
    assert!(parse_shape("S 1 2 3", Thickness::Varying).is_err());
    assert!(parse_shape("Z 1 2 3", Thickness::Varying).is_err());
}

#[test]
fn shape_parse_huge_index_is_refused() {
    // an untrusted row must not allocate unbounded memory
    for row in ["R 4000000000 0 1", "R 0 4000000000 1", "R 100001 0 1"] {
        assert!(parse_shape(row, Thickness::Varying).is_err(), "{row}");
    }
    assert!(parse_shape("R 100000 3 1", Thickness::Varying).is_ok());
}

// ---- small tables ----
#[test]
fn join_priority_order() {
    let order = ["col", "beam", "wall", "slab", "roof", "mass"];
    for w in order.windows(2) {
        assert!(join_priority(w[0]) > join_priority(w[1]), "{w:?}");
    }
    assert_eq!(join_priority("unknown"), 0);
    assert_eq!(join_priority(""), 0);
    assert_eq!(join_priority("COL"), 0, "case-sensitive");
}

#[test]
fn std_phase_names() {
    assert_eq!(std_phase_en("existing"), "Existing");
    assert_eq!(std_phase_en("demolition"), "Demolition");
    assert_eq!(std_phase_en("new"), "New construction");
    assert_eq!(std_phase_en(""), "New construction");
}

#[test]
fn layer_fn_guess() {
    assert_eq!(LayerFn::guess("Mineral wool"), LayerFn::Insulation);
    assert_eq!(LayerFn::guess("INSULATION board"), LayerFn::Insulation);
    assert_eq!(LayerFn::guess("Vapour membrane"), LayerFn::Membrane);
    assert_eq!(LayerFn::guess("Air gap"), LayerFn::Air);
    assert_eq!(LayerFn::guess("Gypsum board"), LayerFn::Finish1);
    assert_eq!(LayerFn::guess("Cement plaster"), LayerFn::Finish1);
    assert_eq!(LayerFn::guess("\u{423}\u{442}\u{435}\u{43f}\u{43b}\u{438}\u{442}\u{435}\u{43b}\u{44c}"), LayerFn::Insulation);
    assert_eq!(LayerFn::guess("Concrete"), LayerFn::Structure);
    assert_eq!(LayerFn::guess(""), LayerFn::Structure);
}

#[test]
fn param_type_numeric() {
    for t in ParamType::ALL {
        assert_eq!(t.numeric(), !matches!(t, ParamType::Text | ParamType::YesNo));
    }
}

#[test]
fn changes_is_empty() {
    let mut c = Changes::default();
    assert!(c.is_empty());
    c.all = true;
    assert!(!c.is_empty());
    c.all = false;
    c.rows.entry("wall".into()).or_default().insert(1);
    assert!(!c.is_empty());
    assert!(c.touched("wall") && !c.touched("slab"));
}

// ---- base element API ----
#[test]
fn element_create_set_get_count_undo() {
    let mut m = Model::open(":memory:").unwrap();
    assert_eq!(m.count().unwrap(), 0);
    let id = m.create("thing").unwrap();
    assert_eq!(m.count().unwrap(), 1);
    assert_eq!(m.get(id, "w"), None);
    m.set(id, "w", 2.5).unwrap();
    assert_eq!(m.get(id, "w"), Some(2.5));
    m.set(id, "w", -0.0).unwrap();
    m.set(id, "w", 7.0).unwrap();
    assert_eq!(m.get(id, "w"), Some(7.0));
    m.undo().unwrap();
    assert_eq!(m.get(id, "w"), Some(-0.0));
    m.undo().unwrap();
    assert_eq!(m.get(id, "w"), Some(2.5));
    m.undo().unwrap();
    assert_eq!(m.get(id, "w"), None);
    m.undo().unwrap();
    assert_eq!(m.count().unwrap(), 0, "create is undone");
    assert_eq!(m.get(9999, "w"), None, "unknown element");
}

#[test]
fn element_set_bad_values() {
    let mut m = Model::open(":memory:").unwrap();
    let id = m.create("thing").unwrap();
    let _ = m.set(id, "w", f64::NAN);
    let v = m.get(id, "w");
    assert!(v.map_or(true, |x| x.is_nan()), "NaN either refused or stored as NaN only");
}

#[test]
fn element_set_f64_extremes_round_trip() {
    let mut m = Model::open(":memory:").unwrap();
    let id = m.create("thing").unwrap();
    for v in [0.0, 1e-300, -1e300, f64::MAX, f64::MIN_POSITIVE, 0.1 + 0.2] {
        m.set(id, "v", v).unwrap();
        assert_eq!(m.get(id, "v"), Some(v));
    }
}

#[test]
fn category_of_unknown_rows() {
    let m = Model::open(":memory:").unwrap();
    assert_eq!(m.category_of("opening", 1).unwrap(), None);
    assert_eq!(m.category_of("nonsense", 1).unwrap(), None);
    assert!(m.params_for("wall").is_ok());
}

// ---- drawing libraries ----
mod libs {
    use crate::drawq::*;

    fn pen(name: &str, cols: &[(u32, f32)]) -> Pen {
        Pen { name: name.into(), columns: cols.to_vec() }
    }

    #[test]
    fn pen_table_get_set_remove() {
        let mut t = PenTable::default();
        assert!(t.check());
        let n0 = t.pens.len();
        assert!(t.get("nope").is_none());
        assert_eq!(t.mm("nope", 100), MIN_MM, "unknown pen is the finest line");
        // unsorted + duplicate columns are normalised
        assert!(t.set(pen("mine", &[(200, 0.3), (50, 0.5), (50, 0.9)])));
        assert_eq!(t.pens.len(), n0 + 1);
        let p = t.get("mine").unwrap();
        assert_eq!(p.columns.iter().map(|c| c.0).collect::<Vec<_>>(), vec![50, 200]);
        assert!(t.set(pen("mine", &[(100, 0.35)])));
        assert_eq!(t.pens.len(), n0 + 1, "replace by name");
        assert_eq!(t.mm("mine", 1), 0.35);
        // invalid pens are refused and leave the table unchanged
        let before = t.clone();
        for bad in [
            pen("", &[(100, 0.3)]),
            pen("x", &[]),
            pen("x", &[(0, 0.3)]),
            pen("x", &[(100, f32::NAN)]),
            pen("x", &[(100, MAX_MM + 1.0)]),
            pen("x", &[(100, MIN_MM / 2.0)]),
        ] {
            assert!(!t.set(bad));
        }
        assert_eq!(t, before);
        assert!(t.remove("mine"));
        assert!(!t.remove("mine"));
        assert_eq!(t.pens.len(), n0);
        let j = t.to_json();
        assert_eq!(PenTable::from_json(&j), t, "json round trip");
    }

    #[test]
    fn pen_at_picks_nearest_column_on_log_scale() {
        let p = pen("p", &[(50, 0.2), (100, 0.4), (500, 0.8)]);
        assert_eq!(p.at(1), 0.2);
        assert_eq!(p.at(60), 0.2);
        assert_eq!(p.at(90), 0.4);
        assert_eq!(p.at(10_000), 0.8);
        assert_eq!(p.at(0), 0.2, "denominator 0 is clamped");
        assert_eq!(pen("e", &[]).at(100), MIN_MM);
    }

    #[test]
    fn style_override_empty_and_default_style() {
        let mut o = StyleOverride::default();
        assert!(o.is_empty());
        o.halftone = true;
        assert!(!o.is_empty());
        assert_eq!(StyleOverride::from_json(&o.to_json()), o);
        assert!(StyleOverride::from_json(&serde_json::Value::Null).is_empty());
        for b in [Band::Cut, Band::Seen, Band::Hidden, Band::Overhead, Band::Beyond] {
            let s = default_style(b);
            assert!(!s.pen.is_empty() && !s.pattern.is_empty());
            assert!(PenTable::default().get(&s.pen).is_some(), "default pen exists");
        }
        assert_eq!(default_style(Band::Seen), default_style(Band::Hidden));
    }

    #[test]
    fn pattern_lib_get_set_remove_period() {
        let mut l = PatternLib::default();
        assert!(l.get("overhead").is_some());
        assert!(l.get("nope").is_none());
        assert!(l.get_or_solid("nope").solid());
        let p = LinePattern { name: "custom".into(), segs: vec![Seg::Dash(4.0), Seg::Gap(1.0)] };
        assert_eq!(p.period(), 5.0);
        assert!(l.set(p.clone()));
        assert_eq!(l.get("custom"), Some(p.clone()));
        assert!(l.names().contains(&"custom".to_string()));
        // user pattern shadows a built-in of the same name
        let shadow = LinePattern { name: "overhead".into(), segs: vec![Seg::Dash(9.0), Seg::Gap(9.0)] };
        assert!(l.set(shadow.clone()));
        assert_eq!(l.get("overhead"), Some(shadow));
        assert_eq!(l.names().iter().filter(|n| *n == "overhead").count(), 1);
        for bad in [
            LinePattern { name: " ".into(), segs: vec![Seg::Dot] },
            LinePattern { name: "z".into(), segs: vec![Seg::Dash(0.0)] },
            LinePattern { name: "z".into(), segs: vec![Seg::Gap(f64::NAN)] },
            LinePattern { name: "z".into(), segs: vec![Seg::Dash(501.0)] },
            LinePattern { name: "z".into(), segs: vec![Seg::Zig { len: 1.0, amp: 51.0 }] },
            LinePattern { name: "z".into(), segs: vec![Seg::Dot; 65] },
        ] {
            assert!(!l.set(bad));
        }
        assert!(l.remove("custom"));
        assert!(!l.remove("custom"), "built-ins are not removable");
        assert!(!l.remove("water"));
        assert_eq!(PatternLib::from_json(&l.to_json()), l);
        assert_eq!(LinePattern { name: "s".into(), segs: vec![] }.period(), 0.0);
    }

    #[test]
    fn hatch_lib_get_set_remove() {
        let mut l = HatchLib::default();
        let first = l.names().into_iter().next().unwrap();
        assert!(l.get(&first).is_some());
        assert!(l.get("nope").is_none());
        let fam = HatchFamily { angle: 45.0, origin: (0.0, 0.0), shift: 0.0, spacing: 2.0, dashes: vec![] };
        let p = HatchPattern { name: "mine".into(), families: vec![fam.clone()], model: false };
        assert!(l.set(p.clone()));
        assert_eq!(l.get("mine"), Some(p));
        let mut bad = fam.clone();
        bad.spacing = 0.0;
        assert!(!l.set(HatchPattern { name: "b".into(), families: vec![bad], model: false }));
        let mut nan = fam.clone();
        nan.angle = f64::NAN;
        assert!(!l.set(HatchPattern { name: "b".into(), families: vec![nan], model: false }));
        assert!(!l.set(HatchPattern { name: "b".into(), families: vec![], model: false }));
        assert!(!l.set(HatchPattern { name: "b".into(), families: vec![fam; 9], model: false }));
        assert!(l.remove("mine"));
        assert!(!l.remove("mine"));
        assert!(l.get("mine").is_none());
        assert_eq!(HatchLib::from_json(&l.to_json()), l);
    }
}

// ---- small enums and predicates ----
#[test]
fn target_ref_maps_categories() {
    use crate::reason::target_ref;
    use crate::Target;
    use ddc_ops::{Cat, Ref};
    assert_eq!(target_ref(Target::Wall(5)), Some(Ref::id(Cat::Wall, 5)));
    assert_eq!(target_ref(Target::Slab(-1)), Some(Ref::id(Cat::Slab, -1)));
    assert_eq!(target_ref(Target::Dim(i64::MAX)), Some(Ref::id(Cat::Annotation, i64::MAX)));
    assert_eq!(target_ref(Target::Topo(3)), Some(Ref::id(Cat::Topography, 3)));
    assert_eq!(target_ref(Target::View(1)), None, "views have no op category");
    assert_eq!(target_ref(Target::RoomSep(1)), None);
}

#[test]
fn constraint_on_levels() {
    use crate::{ConstraintKind, ConstraintSpec};
    let mk = |kind| ConstraintSpec { kind, walls: vec![], value: None };
    assert!(mk(ConstraintKind::LevelGap).on_levels());
    assert!(mk(ConstraintKind::TopToLevel).on_levels());
    assert!(!mk(ConstraintKind::LockLength).on_levels());
    assert!(!mk(ConstraintKind::Parallel).on_levels());
}

#[test]
fn placement_matrix_is_rigid() {
    use crate::Placement;
    let id = Placement { origin: (1.0, 2.0, 3.0), rotation: 0.0, mirror: false, tilt: None }.matrix();
    assert_eq!(id, [1.0, -0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 2.0, 0.0, 0.0, 1.0, 3.0]);
    let mut r = Lcg(77);
    for _ in 0..200 {
        let p = Placement {
            origin: (r.next() * 10.0, r.next() * 10.0, r.next()),
            rotation: (r.next() - 0.5) * 20.0,
            mirror: r.next() > 0.5,
            tilt: None,
        };
        let m = p.matrix();
        // columns of the 2x2 block are orthonormal; determinant is -1 when mirrored
        let det = m[0] * m[5] - m[1] * m[4];
        assert!((det - if p.mirror { -1.0 } else { 1.0 }).abs() < 1e-12);
        assert!((m[0] * m[0] + m[4] * m[4] - 1.0).abs() < 1e-12);
        assert_eq!((m[3], m[7], m[11]), p.origin);
        assert_eq!((m[8], m[9], m[10]), (0.0, 0.0, 1.0));
    }
}

#[test]
fn src_kind_by_ai_and_codec() {
    use crate::SrcKind;
    let mut ai = 0;
    for k in SrcKind::ALL {
        assert_eq!(SrcKind::parse(k.as_str()), Some(k));
        if k.by_ai() {
            ai += 1;
        }
    }
    assert_eq!(ai, 3, "ai, generator and rule fix");
    assert_eq!(SrcKind::parse("from-the-future"), None);
    assert_eq!(SrcKind::parse(""), None);
}

#[test]
fn rule_op_takes_value() {
    use crate::view_gfx::RuleOp;
    for o in RuleOp::ALL {
        assert_eq!(o.takes_value(), !matches!(o, RuleOp::Exists | RuleOp::Missing));
        assert_eq!(RuleOp::from_key(o.key()), Some(o));
    }
    assert_eq!(RuleOp::from_key(""), None);
}

#[test]
fn clash_status_names() {
    use crate::mep::ClashStatus;
    for s in ClashStatus::ALL {
        assert_eq!(ClashStatus::parse(ClashStatus::name(s)), Some(s));
    }
    assert_eq!(ClashStatus::parse("New"), None);
}

#[test]
fn gfx_override_is_empty() {
    use crate::view_gfx::GfxOverride;
    let mut g = GfxOverride::default();
    assert!(g.is_empty());
    g.fill = Some(0xff0000);
    assert!(!g.is_empty());
    assert!(!g.clone().or(GfxOverride::default()).is_empty());
    assert_eq!(GfxOverride::default().or(g.clone()), g, "lower layer fills gaps");
}

#[test]
fn value_rule_kind() {
    use crate::checks::ValueRule;
    let kinds = [
        (ValueRule::NonEmpty, "non_empty"),
        (ValueRule::OneOf(vec![]), "one_of"),
        (ValueRule::Range { min: None, max: None }, "range"),
        (ValueRule::Regex(String::new()), "regex"),
        (ValueRule::Unit("mm".into()), "unit"),
    ];
    for (r, k) in kinds {
        assert_eq!(r.kind(), k);
    }
}
