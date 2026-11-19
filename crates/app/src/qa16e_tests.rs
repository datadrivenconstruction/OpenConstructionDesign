// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Model-shape and annotation tool texts come from the catalog, in both languages.
//! No test here changes the global UI language: every site is rendered with an explicit one.

use crate::units::Units;
use std::collections::BTreeSet;

const EN: &str = include_str!("../../ui/data/i18n/en/sketch_edit.json");
const RU: &str = include_str!("../../ui/data/i18n/ru/sketch_edit.json");

fn cyr(s: &str) -> bool {
    s.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c))
}

fn table(src: &str) -> std::collections::BTreeMap<String, String> {
    let v: serde_json::Value = serde_json::from_str(src).unwrap();
    v.as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
        .collect()
}

/// Source of a changed file without `//` comments (string literals never hold `//` here).
fn code(src: &str) -> String {
    src.lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

const FILES: [(&str, &str); 8] = [
    ("shape_edit.rs", include_str!("shape_edit.rs")),
    ("sketch_mode.rs", include_str!("sketch_mode.rs")),
    ("tools/tags.rs", include_str!("tools/tags.rs")),
    ("tools/distribute.rs", include_str!("tools/distribute.rs")),
    ("opening_tool.rs", include_str!("opening_tool.rs")),
    ("params_ui.rs", include_str!("params_ui.rs")),
    ("main.rs", include_str!("main.rs")),
    ("unit_text.rs", include_str!("unit_text.rs")),
];

#[test]
fn qa16e_keys_en_ru_same_set() {
    let (en, ru) = (table(EN), table(RU));
    assert_eq!(
        en.keys().collect::<BTreeSet<_>>(),
        ru.keys().collect::<BTreeSet<_>>()
    );
    assert!(en.len() > 100);
    for (k, v) in &en {
        assert!(k.starts_with("sketch_edit."), "{k}");
        assert!(!v.is_empty() && !cyr(v), "en {k}: {v}");
        assert!(!ru[k].is_empty(), "ru {k}");
    }
}

#[test]
fn qa16e_every_used_key_exists_in_both_languages() {
    let en = table(EN);
    let mut used = BTreeSet::new();
    for (_, src) in FILES {
        for part in src.split("\"sketch_edit.").skip(1) {
            let end = part.find('"').unwrap();
            used.insert(format!("sketch_edit.{}", &part[..end]));
        }
    }
    assert!(used.len() > 60, "{}", used.len());
    for k in used {
        assert!(en.contains_key(&k), "missing {k}");
        assert_ne!(crate::tr_lang(false, &k, &[]), k, "en catalog lacks {k}");
        assert_ne!(crate::tr_lang(true, &k, &[]), k, "ru catalog lacks {k}");
    }
}

#[test]
fn qa16e_changed_files_have_no_cyrillic_outside_comments() {
    for (name, src) in FILES {
        // the key-and-fixture scan below must not flag this file's own scanner
        let c = code(src);
        for (i, l) in c.lines().enumerate() {
            assert!(!cyr(l), "{name}:{}: {l}", i + 1);
        }
    }
}

#[test]
fn shape_hud_en_no_cyrillic() {
    use crate::shape_edit::{hud_qty, hud_title, ShapeTool};
    use ddc_geom::shape_edit::Thickness;
    for tool in [
        ShapeTool::Modify,
        ShapeTool::AddPoint,
        ShapeTool::SplitLine,
        ShapeTool::Drain,
    ] {
        for mode in [Thickness::Varying, Thickness::Constant] {
            for u in [Units::Metric, Units::Imperial] {
                let en = hud_title(tool, -0.08, mode, false, u);
                assert!(!cyr(&en), "{en}");
                assert!(cyr(&hud_title(tool, -0.08, mode, true, u)));
            }
        }
        assert!(!cyr(&tool.label_in(false)));
    }
    let q = hud_qty(1.5, 6.0, false, Units::Metric);
    assert!(!cyr(&q), "{q}");
    assert!(q.contains("m³") && q.contains("m²"), "{q}");
    let t = hud_title(ShapeTool::Drain, -0.08, Thickness::Varying, false, Units::Metric);
    assert!(t.contains("-80") && t.contains("mm"), "{t}");
    assert!(hud_qty(1.5, 6.0, true, Units::Metric).contains("\u{43c}\u{b3}"));
}

#[test]
fn sketch_texts_en_no_cyrillic() {
    use crate::sketch_mode::{bar_texts, Draw, SketchMode};
    use ddc_model::SketchHost;
    let mut m = SketchMode::new(SketchHost::Slab(1), None);
    for h in [
        SketchHost::Slab(1),
        SketchHost::Roof(1),
        SketchHost::Ceiling(1),
        SketchHost::Shaft(1),
    ] {
        m.host = h;
        let (title, msg, _) = bar_texts(&m, false, Units::Metric);
        assert!(!cyr(&title) && !cyr(&msg), "{title} / {msg}");
        let (title_ru, _, _) = bar_texts(&m, true, Units::Metric);
        assert!(cyr(&title_ru));
    }
    for d in [
        Draw::Line,
        Draw::Arc3,
        Draw::Ellipse,
        Draw::Spline,
        Draw::PickWalls,
        Draw::SlopeArrow,
    ] {
        assert!(!cyr(&d.label_in(false)), "{d:?}");
    }
    // an untouched sketch reports nothing (0ebba6f75), so give it a curve first
    m.sketch.curves.push(ddc_geom::sketch::SketchCurve::Line {
        a: ddc_geom::v2(0.0, 0.0),
        b: ddc_geom::v2(1.0, 0.0),
    });
    m.issues = vec![ddc_geom::sketch::SketchIssue::Empty; 3];
    let t = m.issue_text_in(false).unwrap();
    assert!(!cyr(&t) && t.contains("+2"), "{t}");
    assert!(cyr(&m.issue_text_in(true).unwrap()));
}

#[test]
fn annotation_texts_en_no_cyrillic() {
    use crate::modify_tools::distribute::conflict_text_in;
    use ddc_model::SlotConflict;
    for c in [
        SlotConflict::OffWall,
        SlotConflict::Overlap,
        SlotConflict::Opening(7),
    ] {
        assert!(!cyr(&conflict_text_in(c, false)));
        assert!(cyr(&conflict_text_in(c, true)));
    }
    for k in [
        "sketch_edit.dist_partial",
        "sketch_edit.dist_done",
        "sketch_edit.num_ok",
        "sketch_edit.dims_ok",
        "sketch_edit.tag_all_ok",
    ] {
        let args = [
            ("placed", "3"),
            ("n", "5"),
            ("skipped", "2"),
            ("s", "D1"),
            ("next", "D2"),
            ("after", "4"),
            ("before", "1"),
        ];
        let en = crate::tr_lang(false, k, &args);
        assert!(!cyr(&en) && !en.contains('{'), "{en}");
    }
}

#[test]
fn opening_along_hint_in_mm_project() {
    use crate::opening_tool::{AlongSnap, PlaceError};
    let en = AlongSnap::FromEnd(0.25).label_in(false, Units::Metric);
    assert_eq!(en, "from end 250\u{a0}mm");
    let ru = AlongSnap::FromStart(0.25).label_in(true, Units::Metric);
    assert!(ru.contains("250\u{a0}\u{43c}\u{43c}"), "{ru}");
    assert!(!cyr(&AlongSnap::Center.label_in(false, Units::Metric)));
    let imp = AlongSnap::FromEnd(0.25).label_in(false, Units::Imperial);
    assert!(!imp.contains("mm") && !cyr(&imp), "{imp}");
    for e in [
        PlaceError::NoWall,
        PlaceError::NonPositive,
        PlaceError::Outside {
            len: 6.0,
            from: -0.2,
            to: 0.8,
        },
        PlaceError::TooTall {
            top: 3.1,
            height: 3.0,
        },
        PlaceError::Overlap { other: 4 },
    ] {
        let t = e.text(false, Units::Metric);
        assert!(!cyr(&t) && !t.contains('{'), "{t}");
    }
}

#[test]
fn params_labels_en_no_cyrillic() {
    use ddc_model::ParamType;
    for t in ParamType::ALL {
        assert!(!cyr(crate::params_ui::type_name(t, false)));
        assert!(cyr(crate::params_ui::type_name(t, true)));
    }
    assert!(!cyr(crate::params_ui::category_name("door", false)));
    assert!(cyr(crate::params_ui::category_name("window", true)));
}
