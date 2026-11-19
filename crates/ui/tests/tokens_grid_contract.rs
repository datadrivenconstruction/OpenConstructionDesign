//! Every size in space/radius/size/control/ribbon/layout sits on the 4 px grid; the few
//! documented half-steps are listed by path. Elevation is per theme, dark carries an outline.
use ddc_ui::theme::{focus, ribbon, Density, Mode, Theme, TOKENS_JSON};
use serde_json::Value;

/// Path → allowed off-grid value (hairlines, the 2 px half-step, type line height).
const EXEMPT: &[(&str, f64)] = &[
    ("space.scale[1]", 2.0),
    ("space.named.xxs", 2.0),
    ("radius.xs", 2.0),
    ("radius.pill", 999.0),
    ("ribbon.rows", 3.0),
    ("ribbon.line_h", 14.0),
    ("ribbon.col_gap", 2.0),
];

fn walk(path: &str, v: &Value, bad: &mut Vec<String>) {
    match v {
        Value::Number(n) => {
            let x = n.as_f64().unwrap();
            let ok = x % 4.0 == 0.0 || EXEMPT.iter().any(|(p, e)| *p == path && *e == x);
            if !ok {
                bad.push(format!("{path} = {x}"));
            }
        }
        Value::Object(m) => {
            for (k, x) in m {
                if path == "ribbon" && k == "motion_ms" {
                    continue;
                }
                walk(&format!("{path}.{k}"), x, bad);
            }
        }
        Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                walk(&format!("{path}[{i}]"), x, bad);
            }
        }
        _ => {}
    }
}

#[test]
fn tokens_grid_contract() {
    let v: Value = serde_json::from_str(TOKENS_JSON).unwrap();
    assert_eq!(v["$schema"], "ddc-editor/design-tokens/v3");
    let mut bad = Vec::new();
    for g in ["space", "radius", "size", "control", "layout", "ribbon"] {
        assert!(v[g].is_object(), "{g} missing");
        walk(g, &v[g], &mut bad);
    }
    assert!(bad.is_empty(), "off the 4 px grid: {bad:#?}");
    let r = |k: &str| v["radius"][k].as_f64().unwrap();
    assert_eq!([r("xs"), r("sm"), r("md"), r("lg")], [2.0, 4.0, 8.0, 12.0]);
    assert_eq!(v["space"]["named"]["40"], 40);
    for (d, base) in [("compact", 20), ("comfortable", 24)] {
        let c = &v["size"]["control"][d];
        let got: Vec<i64> = ["xs", "sm", "md", "lg"].iter().map(|k| c[k].as_i64().unwrap()).collect();
        assert_eq!(got, [base, base + 4, base + 8, base + 12], "size.control.{d}");
    }
    assert_eq!((ribbon::ROW_H, ribbon::GROUP_GAP, ribbon::TITLE_H), (24.0, 8.0, 16.0));
    assert_eq!(ribbon::BAND_H, ribbon::ROWS as f32 * ribbon::ROW_H);
    assert_eq!(v["font"]["family"]["ui"].as_str().unwrap().split(' ').take(2).collect::<Vec<_>>(), ["DDC", "Sans"]);
    assert_eq!(ddc_ui::theme::W_REGULAR, "DDC Sans");
}

#[test]
fn elevation_per_theme() {
    let light = Theme::new(Mode::Light, Density::Compact);
    let dark = Theme::new(Mode::Dark, Density::Compact);
    for lvl in 1..=3u8 {
        let (l, d) = (light.shadow(lvl), dark.shadow(lvl));
        assert!(l.blur > 0 && d.blur > 0, "level {lvl}");
        assert!(d.color.a() >= 2 * l.color.a() - 1, "dark shadow {lvl} is not doubled");
        assert!(light.elevation_outline(lvl).is_none(), "light {lvl} has no outline");
    }
    assert!(dark.elevation_outline(1).is_none());
    for lvl in 2..=3u8 {
        let o = dark.elevation_outline(lvl).expect("dark levels 2-3 carry an outline");
        assert_eq!((o.width, o.color), (1.0, dark.c("border.overlay")));
    }
}

#[test]
fn focus_ring_geometry() {
    let v: Value = serde_json::from_str(TOKENS_JSON).unwrap();
    assert_eq!(v["focus"]["ring"]["width"].as_f64().unwrap() as f32, focus::WIDTH);
    assert_eq!(v["focus"]["ring"]["offset"].as_f64().unwrap() as f32, focus::OFFSET);
    assert_eq!(v["focus"]["visible_only"], "keyboard");
    let c = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(64.0, 28.0));
    let (r, cr) = focus::ring(c, 8.0);
    assert_eq!(r, c.expand(2.0));
    assert_eq!(cr, egui::CornerRadius::same(10));
    // keyboard only: a pointer press after the last key hides it
    assert!(ddc_ui::widgets::focus_visible(Some(2), Some(1)));
    assert!(!ddc_ui::widgets::focus_visible(Some(1), Some(2)));
}
