// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Ribbon grid, text size and caret contracts.
use ddc_ui::theme::ribbon as k;
use ddc_ui::typo::{font, Role};
use ddc_ui::widgets::ribbon_layout::Metrics;

#[test]
fn ribbon_buttons_on_grid() {
    for (n, v) in [
        ("row", k::ROW_H),
        ("band", k::BAND_H),
        ("title", k::TITLE_H),
        ("group_gap", k::GROUP_GAP),
        ("large_min_w", k::LARGE_MIN_W),
        ("large_pad_x", k::LARGE_PAD_X),
        ("medium_gap", k::MEDIUM_GAP),
        ("medium_pad_l", k::MEDIUM_PAD_L),
        ("medium_pad_r", k::MEDIUM_PAD_R),
        ("small_w", k::SMALL_W),
        ("caret_w", k::CARET_W),
        ("launcher_w", k::LAUNCHER_W),
        ("edge", k::EDGE),
        ("label_top", k::LABEL_TOP),
        ("icon_top", k::LARGE_ICON_TOP),
    ] {
        assert_eq!(v % 4.0, 0.0, "ribbon {n} = {v} is off the 4 px grid");
    }
    assert_eq!(k::BAND_H, k::ROWS as f32 * k::ROW_H);
    assert!(k::LARGE_MIN_W >= 48.0);
}

#[test]
fn ribbon_text_at_least_12px() {
    for r in [Role::RibbonLabel, Role::GroupTitle, Role::Tab] {
        assert!(font(r).size >= 12.0, "{r:?} below 12 px");
    }
    // two label lines plus the icon still fit the band; the title fits its row
    assert!(k::LABEL_TOP + 2.0 * k::LINE_H <= k::BAND_H);
    assert!(font(Role::GroupTitle).size <= k::TITLE_H);
}

#[test]
fn one_caret_zone_for_all_sizes() {
    let m = Metrics::default();
    assert_eq!(m.caret_w, k::CARET_W);
    assert!(k::CARET_W >= 16.0 && k::LAUNCHER_W == k::CARET_W);
}
