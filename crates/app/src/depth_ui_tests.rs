// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Depth in the editor: chip and estimate texts in both languages, the setting written as
//! one undo step, the heat map rows and its on/off through the view state, and the setting
//! editor laid out inside the narrowest AI panel.

use super::*;
use crate::modify_tools::tests::walls;
use ddc_ui::Lang;

fn editor_with_walls(n: usize) -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    let segs: Vec<((f64, f64), (f64, f64))> =
        (0..n).map(|i| ((0.0, i as f64 * 3.0), (6.0, i as f64 * 3.0))).collect();
    walls(&mut ed, &segs);
    ed
}

fn no_key_left(lang: Lang, text: &str) {
    assert!(!text.contains("depth."), "{lang:?}: untranslated key in {text:?}");
    assert!(!text.contains('{'), "{lang:?}: placeholder left in {text:?}");
}

#[test]
fn chip_text_names_depth_and_disciplines_in_both_languages() {
    let (ru_head, ru_arch, ru_mep) = (
        crate::app2_fx::fx("depth_head"),
        crate::app2_fx::fx("depth_arch"),
        crate::app2_fx::fx("depth_mep"),
    );
    for (lang, head, arch, mep) in [
        (Lang::En, "Depth D3", "Arch", "MEP"),
        (Lang::Ru, ru_head.as_str(), ru_arch.as_str(), ru_mep.as_str()),
    ] {
        let cat = Catalog::new(lang);
        let mut s = DepthSettings { project: 3, ..DepthSettings::default() };
        let t = chip_text(&cat, &s);
        no_key_left(lang, &t);
        assert!(t.starts_with(head), "{t}");
        assert!(t.contains(arch) && t.contains(mep), "{t}");
        // own depth is named, an off discipline is left out
        s.disc[Discipline::Mep.index()].own = Some(4);
        s.disc[Discipline::Arch.index()].on = false;
        let t = chip_text(&cat, &s);
        assert!(t.contains(&format!("{mep} D4")), "{t}");
        assert!(!t.split(" · ").nth(1).unwrap_or("").contains(arch), "{t}");
        for d in &mut s.disc {
            d.on = false;
        }
        let t = chip_text(&cat, &s);
        no_key_left(lang, &t);
        assert!(t.contains(&cat.t("depth.all_off")), "{t}");
    }
}

#[test]
fn every_depth_text_exists_in_both_languages() {
    let mut keys = vec![
        "depth.chip",
        "depth.chip_tip",
        "depth.all_off",
        "depth.project",
        "depth.as_project",
        "depth.estimate",
        "depth.estimate_none",
        "depth.heat",
        "depth.heat.row",
        "depth.settings",
        "depth.settings_desc",
    ];
    keys.extend(Discipline::ALL.map(disc_key));
    let levels: Vec<String> = (1..=5).map(|i| format!("depth.level.{i}")).collect();
    keys.extend(levels.iter().map(String::as_str));
    for lang in [Lang::En, Lang::Ru] {
        let cat = Catalog::new(lang);
        for k in &keys {
            let t = cat.t_in(lang, k);
            assert!(t != *k && !t.trim().is_empty(), "{k} missing in {lang:?}");
        }
    }
    // the languages differ where a word is shown
    let (en, ru) = (Catalog::new(Lang::En), Catalog::new(Lang::Ru));
    assert_ne!(en.t("depth.heat"), ru.t("depth.heat"));
}

#[test]
fn estimate_text_fills_every_placeholder() {
    let ed = editor_with_walls(4);
    let e = model_estimate(&ed.model, &DepthSettings::default()).expect("walls give an estimate");
    for lang in [Lang::En, Lang::Ru] {
        let t = estimate_text(&Catalog::new(lang), &e);
        no_key_left(lang, &t);
        assert!(t.contains(&e.elements.to_string()), "{t}");
    }
    // an empty model has nothing to estimate from
    let empty = Editor::new_in_memory().unwrap();
    assert!(model_estimate(&empty.model, &DepthSettings::default()).is_none());
}

#[test]
fn apply_is_one_undo_step_and_unchanged_is_none() {
    let mut ed = editor_with_walls(1);
    let s = DepthSettings { project: 4, ..DepthSettings::default() };
    assert!(apply(&mut ed, s).unwrap());
    assert_eq!(ed.model.depth_settings().unwrap().project, 4);
    assert!(!apply(&mut ed, s).unwrap(), "unchanged writes nothing");
    ed.undo();
    assert_eq!(ed.model.depth_settings().unwrap(), DepthSettings::default());
    // the walls are still there: only the setting went back
    assert_eq!(ed.walls.len(), 1);
}

#[test]
fn settings_json_names_every_discipline() {
    let mut s = DepthSettings { project: 3, ..DepthSettings::default() };
    s.disc[Discipline::Doc.index()].on = false;
    let j = settings_json(&s);
    assert_eq!(j["project"], 3);
    assert_eq!(j["arch"], 3);
    assert!(j["doc"].is_null());
    for d in Discipline::ALL {
        assert!(j.get(d.as_str()).is_some(), "{d:?}");
    }
}

#[test]
fn heat_legend_groups_by_depth_and_keeps_unset_quiet() {
    let k = |id| Key { cat: "wall", id };
    let keys = [k(1), k(2), k(3), k(4), k(4)];
    let depths: HashMap<(String, i64), u8> =
        [(("wall".to_string(), 1), 1), (("wall".to_string(), 2), 4), (("wall".to_string(), 3), 4)]
            .into_iter()
            .collect();
    let lg = heat_legend(&keys, &depths, "Depth");
    assert_eq!(lg.by, HEAT_BY);
    let rows: Vec<(String, u32, usize)> =
        lg.rows.iter().map(|r| (r.value.clone(), r.rgb, r.keys.len())).collect();
    assert_eq!(
        rows,
        vec![
            ("Depth D1".to_string(), HEAT[0], 1),
            ("Depth D4".to_string(), HEAT[3], 2),
            (String::new(), EMPTY_RGB, 1),
        ]
    );
    assert_eq!(lg.count(), 4, "a key listed twice is colored once");
}

#[test]
fn heat_map_turns_on_and_off_through_the_view() {
    let mut ed = editor_with_walls(3);
    let ids: Vec<i64> = ed.walls.iter().map(|w| w.id).collect();
    ed.model.set_elem_depth(&[("wall", ids[0]), ("wall", ids[1])], 3, None).unwrap();
    crate::ai::view_state::forget();
    let mut app = crate::shell::EditorApp::new(ed, None, "test".into());
    assert!(!heat_on());
    set_heat(&mut app, true);
    assert!(heat_on());
    let lg = crate::ai::view_state::legend().expect("the heat map colors the view");
    let d3 = lg.rows.iter().find(|r| r.rgb == HEAT[2]).expect("a D3 row");
    assert_eq!(d3.keys.len(), 2);
    assert!(lg.rows.iter().any(|r| r.bucket == Bucket::Empty && r.keys.len() == 1));
    set_heat(&mut app, false);
    assert!(!heat_on());
    assert!(crate::ai::view_state::legend().is_none());
}

/// The editor inside the narrowest panel width: every control within it, both languages.
#[test]
fn setting_editor_fits_the_narrow_panel() {
    const W: f32 = crate::ai::ui::PANEL_W.0;
    for lang in [Lang::En, Lang::Ru] {
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        let mut th = Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact);
        let cat = Catalog::new(lang);
        let mut s = DepthSettings::default();
        let mut moved = true;
        let mut used = egui::Rect::NOTHING;
        for _ in 0..3 {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 700.0))),
                ..Default::default()
            };
            let _ = ctx.run(raw, |ctx| {
                let _ = th.apply(ctx);
                egui::SidePanel::left("depth.test")
                    .exact_width(W)
                    .resizable(false)
                    .show(ctx, |ui| {
                        moved = editor(ui, &th, &cat, &mut s);
                        used = ui.min_rect();
                    });
            });
        }
        assert!(!moved, "{lang:?}: a frame without input changed the setting");
        assert_eq!(s, DepthSettings::default());
        assert!(used.height() > 0.0, "{lang:?}: nothing drawn");
        assert!(used.right() <= W + 1.0, "{lang:?}: editor {used:?} past the {W} px panel");
    }
}
