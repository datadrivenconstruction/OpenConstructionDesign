// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! WCAG floors for all seven looks in both themes: text 4.5:1, graphics and focus 3:1,
//! disabled text 3:1 drawn flat; the AI accent stays apart from the canvas preview.
use ddc_ui::looks::{contrast_failures, pair_ratio, patches, presets};
use ddc_ui::sem::{DISABLED_FLOOR, DISABLED_PAIRS, GRAPHIC_PAIRS, TEXT_PAIRS};
use ddc_ui::theme::{parse_hex, Mode, Tokens};
use std::collections::BTreeMap;

fn full(mode: Mode, patch: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut p = Tokens::embedded().unwrap().palette(mode).clone();
    p.extend(patch.clone());
    p
}

fn lab(hex: &str) -> [f64; 3] {
    let c = parse_hex(hex).unwrap();
    let lin = |v: u8| {
        let v = v as f64 / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    let (r, g, b) = (lin(c.r()), lin(c.g()), lin(c.b()));
    let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
    let f = |t: f64| if t > 0.008856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 };
    [116.0 * f(y) - 16.0, 500.0 * (f(x) - f(y)), 200.0 * (f(y) - f(z))]
}

#[test]
fn contrast_contract() {
    let looks = presets();
    assert_eq!(looks.len(), 7);
    for need in [
        ("text.on_subtle", "bg.selected"),
        ("ai.accent", "bg.panel"),
        ("text.primary", "bg.canvas_chip"),
    ] {
        assert!(TEXT_PAIRS.iter().any(|p| (p.0, p.1) == need), "{need:?} not in TEXT_PAIRS");
    }
    for need in [("border.focus", "bg.input"), ("focus.ring", "bg.panel"), ("border.control", "bg.input")] {
        assert!(GRAPHIC_PAIRS.iter().any(|p| (p.0, p.1) == need), "{need:?} not in GRAPHIC_PAIRS");
    }
    let mut bad = Vec::new();
    for l in looks {
        let p = patches(l, None);
        for (mode, patch) in [(Mode::Light, &p.0), (Mode::Dark, &p.1)] {
            let pal = full(mode, patch);
            for f in contrast_failures(&pal) {
                bad.push(format!("{} {mode:?}: {f}", l.id));
            }
            for d in DISABLED_PAIRS {
                let r = pair_ratio(&pal, d).unwrap_or(0.0);
                if r < DISABLED_FLOOR {
                    bad.push(format!("{} {mode:?}: {} on {}: {r:.2}", l.id, d.0, d.1));
                }
            }
            let de = {
                let (a, b) = (lab(&pal["ai.accent"]), lab(&pal["preview"]));
                ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
            };
            if de < 10.0 {
                bad.push(format!("{} {mode:?}: ai.accent vs preview dE {de:.1} < 10", l.id));
            }
        }
    }
    assert!(bad.is_empty(), "{bad:#?}");
}
