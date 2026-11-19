// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Glyph buttons became icon buttons: icons exist, tooltips exist in both languages.
use std::path::Path;

fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn move_icons_are_registered() {
    for n in ["move-up", "move-down", "delete"] {
        assert!(ddc_ui::icons::source(n).is_some(), "{n}");
        assert!(ddc_ui::icons::source(&format!("{n}.fill")).is_some(), "{n}.fill");
    }
}

#[test]
fn replaced_buttons_have_tooltips_in_both_languages() {
    for k in [
        "rebar_list.move_up",
        "rebar_list.move_down",
        "rebar_list.remove",
        "phui.delete_tip",
        "phui.delete_last",
        "sch.ui.delete_col",
        "core_tools.zone.remove",
        "rebar_list.zone_remove_last",
        "view_gfx.vv.priority",
    ] {
        for ru in [false, true] {
            assert_ne!(crate::tr_lang(ru, k, &[]), k, "{k} ru={ru}");
        }
    }
}

#[test]
fn no_glyph_text_buttons_for_move_and_remove() {
    let mut files = vec![];
    walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
    let glyphs = ['\u{2191}', '\u{2193}', '\u{2715}'];
    let mut bad = vec![];
    for f in files {
        let s = std::fs::read_to_string(&f).unwrap();
        for g in glyphs {
            for ctor in ["egui::Button::new(\"", "small_button(\"", "ui.button(\""] {
                if s.contains(&format!("{ctor}{g}")) {
                    bad.push(format!("{} {g}", f.display()));
                }
            }
        }
    }
    assert!(bad.is_empty(), "{bad:?}");
}
