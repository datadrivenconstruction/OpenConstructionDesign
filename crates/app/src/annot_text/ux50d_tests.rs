// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;

const SRC: &str = include_str!("../annot_text.rs");

/// Source of the bar functions: from the shared widgets to `apply`.
fn bar_src() -> &'static str {
    let a = SRC.find("const DD_W").unwrap();
    let b = SRC.find("\nfn apply(").unwrap();
    &SRC[a..b]
}

fn json_keys(lang: &str) -> std::collections::HashSet<String> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../ui/data/i18n")
        .join(lang)
        .join("text_style.json");
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    v.as_object().unwrap().keys().cloned().collect()
}

/// Every `"text_style.…"` key written in the bar functions.
fn used_keys() -> Vec<String> {
    let s = bar_src();
    let mut out = Vec::new();
    for (at, _) in s.match_indices("\"text_style.") {
        let rest = &s[at + 1..];
        out.push(rest[..rest.find('"').unwrap()].to_string());
    }
    out
}

#[test]
fn every_dropdown_has_a_label_key_in_both_languages() {
    let (en, ru) = (json_keys("en"), json_keys("ru"));
    assert_eq!(en, ru);
    let s = bar_src();
    let mut calls = 0;
    for call in s.match_indices("labelled_combo(").map(|x| x.0) {
        let rest = &s[call..];
        let args = &rest[..rest.find(')').unwrap()];
        let key = args.split(',').nth(3).unwrap().trim();
        assert!(
            key.starts_with("\"text_style.lbl.") || key == "\"annot_text.opt.scale\"",
            "dropdown without label key: {args}"
        );
        calls += 1;
    }
    assert!(calls >= 10, "{calls}");
    for k in used_keys() {
        assert!(en.contains(&k), "missing key {k}");
    }
    let c = Catalog::new(Lang::En);
    assert_ne!(c.t("annot_text.opt.scale"), "annot_text.opt.scale");
    for (_, k) in align_items() {
        assert_ne!(c.t(k), k);
    }
}

#[test]
fn labels_exist_in_catalogs_without_touching_global_language() {
    for lang in [Lang::En, Lang::Ru] {
        let c = Catalog::new(lang);
        for k in json_keys("en") {
            assert_ne!(c.t(&k), k, "{k} missing in {lang:?}");
        }
    }
}

#[test]
fn align_items_order_is_left_center_right() {
    let a = align_items();
    assert_eq!(a[0].0, TextAlign::Left);
    assert_eq!(a[1].0, TextAlign::Center);
    assert_eq!(a[2].0, TextAlign::Right);
}

#[test]
fn gray_percent_round_trips_every_byte() {
    for g in 0..=255u8 {
        assert_eq!(pct_gray(gray_pct(g)), g, "gray {g}");
    }
    assert_eq!(gray_pct(0), 0.0);
    assert_eq!(gray_pct(255), 100.0);
    assert_eq!(gray_pct(128), 50.2);
    assert_eq!(pct_gray(0.0), 0);
    assert_eq!(pct_gray(100.0), 255);
}

#[test]
fn typed_whole_percent_rounds_to_nearest_byte() {
    assert_eq!(pct_gray(50.0), 128);
    assert_eq!(pct_gray(25.0), 64);
    assert_eq!(pct_gray(49.9), 127);
}

#[test]
fn out_of_range_percent_is_invalid_and_clamped() {
    assert!(gray_pct_valid(0.0) && gray_pct_valid(100.0) && gray_pct_valid(37.5));
    for bad in [-0.1, 100.1, 250.0, f64::NAN, f64::INFINITY] {
        assert!(!gray_pct_valid(bad), "{bad}");
    }
    assert_eq!(pct_gray(-20.0), 0);
    assert_eq!(pct_gray(400.0), 255);
    assert_eq!(pct_gray(f64::NAN), 0);
    assert_eq!(pct_gray(f64::INFINITY), 0);
}

#[test]
fn bars_use_only_kit_widgets() {
    let s = bar_src();
    for raw in [
        "egui::ComboBox",
        "egui::DragValue",
        "egui::TextEdit",
        "ui.checkbox(",
        "ui.button(",
        "ui.selectable_label(",
    ] {
        assert!(!s.contains(raw), "raw widget in annotation bars: {raw}");
    }
    assert!(s.contains("widgets::dropdown("));
    assert!(s.contains("widgets::NumberField::new("));
    assert!(s.contains("widgets::checkbox("));
}

#[test]
fn bar_inputs_are_named_and_rows_share_one_height() {
    let s = bar_src();
    assert!(s.contains(".labelled_by(lr.id, &label)"));
    assert!(s.contains("WidgetInfo::labeled"));
    assert!(s.contains("row_style(ui, th)"));
    assert!(s.contains("th.d().control"));
}
