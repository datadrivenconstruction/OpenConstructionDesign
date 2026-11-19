// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;
use crate::quick::overlay::RECENT_SECS;
use crate::wall_tool::{ortho_point, Rig};
use ddc_geom::v2;

fn app() -> EditorApp {
    let ed = crate::editor::Editor::new_in_memory().unwrap();
    EditorApp::new(ed, None, "test".into())
}

/// A bar like the real one: recent button and three toggles, X/Y/Z, a wall hint, five controls.
fn need(hint: f32) -> Need {
    Need {
        left_fixed: BTN + ITEM_GAP + 3.0 * BTN + 2.0 * TOGGLE_GAP,
        coords: 260.0,
        hint,
        right: vec![
            Slot {
                full: 120.0,
                compact: 40.0,
            },
            Slot {
                full: 150.0,
                compact: 40.0,
            },
            Slot {
                full: 150.0,
                compact: 40.0,
            },
            Slot {
                full: 48.0,
                compact: 48.0,
            },
            Slot {
                full: 150.0,
                compact: 40.0,
            },
        ],
    }
}

#[test]
fn narrow_bar_drops_the_hint_before_any_label() {
    let n = need(520.0);
    let f = |w| fit(w, &n);
    // wide: everything, the hint whole
    for w in [1920.0, 2560.0] {
        let x = f(w);
        assert!(x.hint == 520.0 && x.coords && x.labels && x.compacted == 0, "{w}: {x:?}");
    }
    // 1280: the hint is cut, every label stays
    let x = f(1280.0);
    assert!(x.hint < 520.0 && x.labels, "{x:?}");
    // 1024: no hint, no cursor, every label stays
    let x = f(1024.0);
    assert!(x.hint == 0.0 && !x.coords && x.labels, "{x:?}");
    // a label goes only after the hint and the cursor are gone
    let mut w = 2560.0;
    while w >= 300.0 {
        let x = f(w);
        assert!(x.compacted == 0 || (x.hint == 0.0 && !x.coords), "w {w}: {x:?}");
        w -= 8.0;
    }
    let tiny = f(300.0);
    assert!(!tiny.coords && !tiny.labels && tiny.hint == 0.0);
}

#[test]
fn compaction_goes_right_to_left() {
    let n = need(520.0);
    let mut prev = 0;
    let mut w = 2560.0;
    while w >= 300.0 {
        let f = fit(w, &n);
        assert!(f.compacted >= prev, "w {w}");
        let last = n.right.len() - 1;
        assert!(f.label_at(0) || f.compacted == n.right.len(), "w {w}: first label lost early");
        assert_eq!(f.label_at(last), f.compacted == 0);
        prev = f.compacted;
        w -= 8.0;
    }
}

#[test]
fn zones_never_overlap_and_keep_the_order() {
    for hint in [0.0, 120.0, 520.0, 1600.0] {
        let n = need(hint);
        let mut prev: Option<Fit> = None;
        let mut w = 1024.0;
        while w <= 2560.0 {
            let f = fit(w, &n);
            assert!(f.used() <= w + 0.01, "w {w}: {f:?} uses {}", f.used());
            // what stays is always a prefix of the drop order
            assert!(f.hint == 0.0 || f.coords, "w {w}: hint without cursor");
            assert!(f.compacted == 0 || !f.coords, "w {w}: label lost before the cursor");
            if let Some(p) = prev {
                // a wider window never shows less
                assert!(f.hint >= p.hint && f.coords >= p.coords && f.compacted <= p.compacted);
            }
            prev = Some(f);
            w += 8.0;
        }
    }
}

#[test]
fn coordinates_in_millimetres_and_feet_inches() {
    let m = Units::Metric;
    assert_eq!(fmt_coord(12.3456, m, Lang::En, 16), "12,346");
    assert_eq!(fmt_coord(12.3456, m, Lang::Ru, 16), "12\u{a0}346");
    assert_eq!(fmt_coord(-1.5, m, Lang::En, 16), "-1,500");
    assert_eq!(fmt_coord(0.25, m, Lang::En, 16), "250");
    let i = Units::Imperial;
    assert_eq!(fmt_coord(3.048, i, Lang::En, 16), "10'-0\"");
    assert_eq!(fmt_coord(381.0, i, Lang::En, 16), "1,250'-0\"");
    assert_eq!(fmt_coord(381.0, i, Lang::Ru, 16), "1\u{a0}250'-0\"");
    assert_eq!(fmt_coord(-381.0, i, Lang::En, 16), "-1,250'-0\"");
    assert_eq!(fmt_coord(0.0254 * 18.5, i, Lang::En, 16), "1'-6 1/2\"");
    let xyz = fmt_xyz((1.0, -2.0, 0.0), m, Lang::En, 16);
    assert_eq!(xyz, ["1,000".to_string(), "-2,000".into(), "0".into()]);
}

#[test]
fn save_state_reads_and_speaks_both_languages() {
    assert_eq!(save_state(true, true, true, None), SaveState::Saving);
    assert_eq!(
        save_state(false, true, true, Some("05:12")),
        SaveState::Unsaved
    );
    assert_eq!(
        save_state(false, false, true, Some("05:12")),
        SaveState::Saved(Some("05:12".into()))
    );
    assert_eq!(save_state(false, false, true, None), SaveState::Saved(None));
    assert_eq!(save_state(false, false, false, None), SaveState::Fresh);
    let en = Catalog::new(Lang::En);
    let ru = Catalog::new(Lang::Ru);
    let all = [
        SaveState::Saving,
        SaveState::Unsaved,
        SaveState::Saved(Some("05:12".into())),
        SaveState::Saved(None),
        SaveState::Fresh,
    ];
    assert_eq!(save_text(&en, &all[0]), "Saving\u{2026}");
    assert_eq!(save_text(&en, &all[1]), "Unsaved changes");
    assert_eq!(save_text(&en, &all[2]), "Saved 05:12");
    assert_eq!(save_text(&en, &all[3]), "Saved");
    assert_eq!(save_text(&en, &all[4]), "No changes");
    let mut seen = std::collections::HashSet::new();
    for s in &all {
        let (e, r) = (save_text(&en, s), save_text(&ru, s));
        assert!(
            !r.is_empty() && !r.contains('{') && !r.starts_with("sb."),
            "{r}"
        );
        assert_ne!(e, r, "ru falls back to en");
        assert!(seen.insert(r));
    }
    assert!(save_text(&ru, &all[2]).contains("05:12"));
}

/// Russian right-zone labels fit `LABEL_MAX` whole, so the save state is never cut.
#[test]
fn ru_status_labels_fit_without_an_ellipsis() {
    let ru = Catalog::new(Lang::Ru);
    let th = Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact);
    let ctx = egui::Context::default();
    ddc_ui::theme::install_fonts(&ctx);
    let mut labels = vec![
        save_text(&ru, &SaveState::Saving),
        save_text(&ru, &SaveState::Unsaved),
        save_text(&ru, &SaveState::Saved(Some("05:12".into()))),
        save_text(&ru, &SaveState::Saved(None)),
        save_text(&ru, &SaveState::Fresh),
        ru.t("pick_rules.filter"),
    ];
    labels.push(ru.tf("pick_rules.filter.on", &[("n", "12")]));
    let _ = ctx.run(Default::default(), |ctx| {
        for s in &labels {
            let w = ctx
                .fonts(|f| f.layout_no_wrap(s.clone(), th.font(Role::Body), Color32::WHITE))
                .size()
                .x;
            assert!(w <= LABEL_MAX, "{s}: {w}");
        }
    });
}

/// Narrowing the bar cuts the hint before any right-zone label goes.
#[test]
fn labels_keep_their_text_until_the_hint_is_gone() {
    let mut w = 1600.0;
    while w >= 300.0 {
        let f = fit(w, &need(400.0));
        if !f.labels {
            assert!(f.hint == 0.0 && !f.coords, "w={w} {f:?}");
        }
        w -= 10.0;
    }
}

fn font_ctx() -> egui::Context {
    let ctx = egui::Context::default();
    ddc_ui::theme::install_fonts(&ctx);
    let _ = ctx.run(Default::default(), |_| {});
    ctx
}

/// The real chips measured with real fonts: at 1280 and 1024 every state item keeps its
/// label while the hint is cut or gone, and the zones never overlap.
#[test]
fn state_labels_survive_narrow_bars_in_both_languages() {
    for lang in [Lang::En, Lang::Ru] {
        let mut a = app();
        a.sh.cat = Catalog::new(lang);
        let ctx = font_ctx();
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let v = gather(&mut a, 0.0);
                let mut acts = Vec::new();
                let sh = &mut a.sh;
                let p = Painter {
                    th: &sh.theme,
                    c: &sh.cat,
                    icons: &mut sh.icons,
                    acts: &mut acts,
                };
                let items = p.right_items(ui, &v);
                let wide_hint = 520.0;
                let n = Need {
                    left_fixed: BTN + ITEM_GAP + 3.0 * BTN + 2.0 * TOGGLE_GAP,
                    coords: Coords::measure(ui, p.th, &v).width,
                    hint: wide_hint,
                    right: items
                        .iter()
                        .map(|(_, ch)| Slot {
                            full: ch.as_ref().map_or(TASK_W, |c| c.width(true)),
                            compact: ch.as_ref().map_or(TASK_W, |c| c.width(false)),
                        })
                        .collect(),
                };
                for w in [1280.0, 1024.0] {
                    let f = fit(w, &n);
                    assert!(f.labels, "{lang:?} {w}: {f:?}");
                    assert!(f.hint < wide_hint, "{lang:?} {w}: {f:?}");
                    assert!(f.used() <= w + 0.01, "{lang:?} {w}: {f:?}");
                    for (i, (_, ch)) in items.iter().enumerate() {
                        assert!(f.label_at(i));
                        if let Some(ch) = ch {
                            assert!(ch.label.is_some(), "{lang:?} item {i}");
                            assert_eq!(ch.width(f.label_at(i)), ch.width(true));
                        }
                    }
                }
            });
        });
    }
}

#[test]
fn a_moved_save_mark_on_the_same_project_is_a_save() {
    assert!(!saved_now(None, (1, 5)));
    assert!(!saved_now(Some((1, 5)), (1, 5)));
    assert!(saved_now(Some((1, 5)), (1, 9)));
    // another project opened: not a save of this one
    assert!(!saved_now(Some((1, 5)), (2, 9)));
}

#[test]
fn hint_splits_into_text_and_keycaps() {
    let p = parse_hint(
        "Wall: pick the start point \u{b7} type a length \u{b7} Tab snap \u{b7} Esc cancel",
    );
    assert_eq!(
        p,
        vec![
            Part::Text("Wall: pick the start point".into()),
            Part::Text("type a length".into()),
            Part::Key {
                keys: "Tab".into(),
                desc: "snap".into()
            },
            Part::Key {
                keys: "Esc".into(),
                desc: "cancel".into()
            },
        ]
    );
    let p = parse_hint("Pick \u{b7} Ctrl+Shift \u{2014} add both \u{b7} F2 rename \u{b7} two letters run a command");
    assert_eq!(
        p[1],
        Part::Key {
            keys: "Ctrl+Shift".into(),
            desc: "add both".into()
        }
    );
    assert_eq!(
        p[2],
        Part::Key {
            keys: "F2".into(),
            desc: "rename".into()
        }
    );
    assert_eq!(p[3], Part::Text("two letters run a command".into()));
    // the instruction is never a key, even when it starts like one
    assert_eq!(parse_hint("Esc"), vec![Part::Text("Esc".into())]);
    assert!(parse_hint("").is_empty());
}

#[test]
fn every_step_hint_parses_in_both_languages() {
    for lang in [Lang::En, Lang::Ru] {
        let c = Catalog::new(lang);
        for (k, v) in c.table(lang) {
            let Some(s) = v.as_str().filter(|_| k.starts_with("hint.")) else {
                continue;
            };
            let p = parse_hint(s);
            assert!(matches!(p.first(), Some(Part::Text(_))), "{k}: {p:?}");
            for part in &p {
                if let Part::Key { desc, .. } = part {
                    assert!(!desc.is_empty(), "{k}: key without an action");
                }
            }
        }
    }
}

#[test]
fn style_and_performance_pick_through_their_commands_and_survive_a_layout() {
    let mut a = app();
    for s in VisualStyle::ALL {
        assert!(a.sh.reg.get(style_cmd(s)).is_some(), "{}", style_cmd(s));
        assert!(a.run(style_cmd(s)));
        assert_eq!(a.view.nav.style, s);
    }
    for l in PerfLevel::ALL {
        assert!(a.run(perf_cmd(l)));
        assert_eq!(a.view.gfx.level, l);
        assert_eq!(a.prefs.gfx_level, l.key());
    }
    a.run(style_cmd(VisualStyle::XRay));
    a.run(perf_cmd(PerfLevel::Speed));
    for layout in ["split", "focus", "classic"] {
        a.set_layout(layout);
        assert_eq!(a.view.nav.style, VisualStyle::XRay, "{layout}");
        assert_eq!(a.view.gfx.level, PerfLevel::Speed, "{layout}");
    }
}

#[test]
fn drawing_aid_toggles_reach_the_plan_tool() {
    let mut a = app();
    assert!(a.sbar.grid && a.sbar.snap && !a.sbar.ortho);
    for (id, key) in [
        ("view.grid_toggle", "F7"),
        ("view.snap_toggle", "F3"),
        ("view.ortho_toggle", "F8"),
    ] {
        assert!(a.sh.reg.get(id).is_some(), "{id}");
        assert_eq!(a.sh.reg.get(id).unwrap().shortcuts, [key], "{id}");
        let c = &a.sh.cat;
        assert_ne!(c.t(&format!("cmd.{id}")), format!("cmd.{id}"));
        assert!(a.run(id));
    }
    assert!(!a.sbar.grid && !a.sbar.snap && a.sbar.ortho);
    assert!(!a.ed.tool.grid_on && !a.ed.tool.snap_on && a.ed.tool.ortho);
    // in 3D the snap toggle is the 3D editor's own switch
    a.view3d = true;
    a.run("view.snap_toggle");
    assert!(a.sbar.snap && !a.view.e3.snaps_off);
    a.run("view.snap_toggle");
    assert!(a.view.e3.snaps_off);
}

#[test]
fn ortho_keeps_segments_horizontal_or_vertical() {
    let f = v2(1.0, 1.0);
    assert_eq!(ortho_point(f, v2(5.0, 2.0)), v2(5.0, 1.0));
    assert_eq!(ortho_point(f, v2(1.5, -4.0)), v2(1.0, -4.0));
    let mut t = Rig::new();
    t.ortho = true;
    t.draft.points.push(v2(0.0, 0.0));
    t.track(v2(3.0, 0.4));
    let p = t.snapped();
    assert!((p.y - 0.0).abs() < 1e-9, "{p:?}");
    t.ortho = false;
    t.snap_on = false;
    t.grid_on = false;
    t.track(v2(3.0, 0.4));
    assert!(t.snap().is_none(), "no snaps when both are off");
    assert_eq!(t.snapped(), v2(3.0, 0.4));
}

#[test]
fn recent_strip_shows_after_a_command_and_hides_after_the_timeout() {
    let mut r = Recent::default();
    // the stored history at start is not a run
    r.note(5, 0.0);
    assert!(!r.visible(0.1, false));
    r.note(5, 1.0);
    assert!(!r.visible(1.0, false));
    // a command ran
    r.note(6, 2.0);
    assert!(r.visible(2.0, false));
    assert!(r.visible(2.0 + RECENT_SECS - 0.1, false));
    assert!((r.left(3.0).unwrap() - (RECENT_SECS - 1.0)).abs() < 1e-9);
    assert!(!r.visible(2.0 + RECENT_SECS + 0.1, false));
    assert_eq!(r.left(2.0 + RECENT_SECS + 0.1), None);
    // hovered: it stays
    assert!(r.visible(100.0, true));
    r.hold = true;
    assert!(r.visible(100.0, false));
}

#[test]
fn fps_speaks_only_while_drawing() {
    let mut f = Fps::default();
    f.push(false, 1.0 / 60.0, 0.0);
    assert_eq!(f.show(0.0), None);
    for i in 0..30 {
        f.push(true, 1.0 / 60.0, f64::from(i) / 60.0);
    }
    assert_eq!(f.show(0.5), Some(60));
    // a long pause is not a frame and the readout goes quiet
    f.push(true, 2.0, 3.0);
    assert_eq!(f.show(3.0), None);
}

#[test]
fn fps_asks_for_one_frame_at_expiry_not_a_poll() {
    let mut f = Fps::default();
    assert_eq!(f.left(0.0), None);
    f.push(true, 1.0 / 60.0, 10.0);
    let left = f.left(10.2).unwrap();
    assert!((left - (HOLD_S - 0.2)).abs() < 1e-9, "{left}");
    // the wait ends exactly where the readout goes silent
    assert!(f.show(10.2 + left - 0.001).is_some());
    assert_eq!(f.show(10.2 + left + 0.021), None);
    assert_eq!(f.left(10.0 + HOLD_S + 0.1), None);
}

#[test]
fn messages_chip_hidden_when_empty() {
    let mut a = app();
    let v = msgs_view(&a.sh.toasts);
    assert_eq!(v.total, 0);
    assert!(v.rows.is_empty());
    a.sh.toasts.push(ddc_ui::notify::Level::Error, "bad", None);
    a.sh.toasts.push(ddc_ui::notify::Level::Info, "ok", None);
    let v = msgs_view(&a.sh.toasts);
    assert_eq!(v.total, 2);
    assert_eq!(v.unseen.errors, 1);
    assert_eq!(v.rows[0].2, "ok", "newest first");
    assert!(!v.rows[0].1.is_empty(), "time is shown");
}

#[test]
fn messages_popover_lists_the_newest_fifty() {
    let mut t = ddc_ui::notify::Toasts::default();
    for i in 0..60 {
        t.record(ddc_ui::notify::Level::Info, format!("m{i}"));
    }
    let v = msgs_view(&t);
    assert_eq!(v.rows.len(), MSGS_SHOWN);
    assert_eq!(v.rows[0].2, "m59");
}

#[test]
fn full_axes_toggle_runs_from_the_palette_and_not_from_a_key_press() {
    use ddc_ui::{Catalog, Lang};
    let mut a = app();
    assert!(!a.prefs.full_axes && !a.ed.axes_pref);
    assert!(a.run(crate::plan_hud::FULL_AXES_CMD));
    assert!(a.prefs.full_axes && a.ed.axes_pref, "on");
    assert!(a.run(crate::plan_hud::FULL_AXES_CMD));
    assert!(!a.prefs.full_axes && !a.ed.axes_pref, "off again");
    // the held key is bound for the list, but a press never toggles
    let none = egui::Modifiers::NONE;
    assert_eq!(crate::quick::chord(&a, crate::plan_hud::FULL_AXES_KEY, none), None);
    crate::quick::rebuild_keys(&mut a);
    assert_eq!(crate::quick::chord(&a, crate::plan_hud::FULL_AXES_KEY, none), None);
    assert_eq!(a.quick.bind.chords_of(crate::plan_hud::FULL_AXES_CMD), ["F9"]);
    let ru_q = "\u{43f}\u{43e}\u{43b}\u{43d}\u{44b}\u{435}";
    for (lang, q) in [(Lang::En, "full axes"), (Lang::Ru, ru_q)] {
        let cat = Catalog::new(lang);
        let hit = ddc_ui::fuzzy::search(&a.sh.reg, &cat, q);
        assert!(hit.iter().any(|c| c.id == crate::plan_hud::FULL_AXES_CMD), "{lang:?}");
        for k in ["cmd.view.axes_full", "settings.axes_full_on", "status.axes_full.on"] {
            assert_ne!(cat.t(k), k, "{lang:?} {k}");
        }
        let tip = cat.tf("plan.hud.axes_tip", &[("key", "F9")]);
        assert!(tip.contains("F9") && !tip.contains('{'), "{tip}");
    }
}

#[test]
fn full_axes_shortcut_is_in_the_shortcut_list() {
    let a = app();
    let keys = ddc_ui::keymap::UserKeys::default();
    let bind = ddc_ui::keymap::Bindings::build(&a.sh.reg, &keys);
    let label = |id: &str| id.to_string();
    let rows = crate::quick::sheet::sheet_sections(&a.sh.reg, &bind, &label, "F9");
    let hit = rows.iter().flat_map(|s| &s.rows).any(|r| r.id == "view.axes_full");
    assert!(hit);
}

fn kinds(items: &[(Item, Option<Chip>)]) -> Vec<Item> {
    items.iter().map(|(i, _)| *i).collect()
}

fn tint_of(items: &[(Item, Option<Chip>)], at: Item) -> Option<Color32> {
    let (_, ch) = items.iter().find(|(i, _)| *i == at).unwrap();
    ch.as_ref().unwrap().tint
}

/// Idle bar: the level is left to the option bar, groups run view state then attention,
/// zero warnings are muted. A tool hides the option bar's level, so the bar brings it back.
#[test]
fn bar_groups_level_and_muted_attention_in_both_languages() {
    for lang in [Lang::En, Lang::Ru] {
        let mut a = app();
        a.sh.cat = Catalog::new(lang);
        let ctx = font_ctx();
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut v = gather(&mut a, 0.0);
                assert!(v.opt_level, "{lang:?}: select with nothing selected");
                let mut acts = Vec::new();
                let sh = &mut a.sh;
                let p = Painter {
                    th: &sh.theme,
                    c: &sh.cat,
                    icons: &mut sh.icons,
                    acts: &mut acts,
                };
                let k = kinds(&p.right_items(ui, &v));
                assert_eq!(
                    k,
                    [Item::Style, Item::Perf, Item::Pick, Item::Warn, Item::Save],
                    "{lang:?}"
                );
                let muted = p.th.c("text.disabled");
                assert_eq!(tint_of(&p.right_items(ui, &v), Item::Warn), Some(muted));
                v.faults.warnings = 2;
                let it = p.right_items(ui, &v);
                assert_eq!(tint_of(&it, Item::Warn), Some(p.th.c("warning")));
                v.faults.errors = 1;
                let it = p.right_items(ui, &v);
                assert_eq!(tint_of(&it, Item::Warn), Some(p.th.c("error")));
                v.opt_level = false;
                let k = kinds(&p.right_items(ui, &v));
                assert_eq!(k[0], Item::Level, "{lang:?}: reachable while a tool is on");
            });
        });
    }
}

/// While a tool runs the option bar has no level combo, so the bar keeps it.
#[test]
fn level_returns_to_the_bar_while_a_tool_is_active() {
    let mut a = app();
    a.ed.set_mode(crate::wall_tool::Mode::Polyline);
    let v = gather(&mut a, 0.0);
    assert!(!v.opt_level);
}

/// A palette-only layout has no option bar at all: the level stays in the bar while idle.
#[test]
fn level_stays_in_the_bar_when_the_layout_has_no_option_bar() {
    let mut a = app();
    assert!(gather(&mut a, 0.0).opt_level, "the default layout shows the option bar");
    let mut ws = a.workspace();
    ws.id = "r101.palette".into();
    ws.surface = ddc_ui::workspace::Surface::PaletteOnly;
    a.layouts.push(ws);
    a.layout = "r101.palette".into();
    assert_eq!(a.workspace().surface, ddc_ui::workspace::Surface::PaletteOnly);
    assert!(!gather(&mut a, 0.0).opt_level, "idle, yet no option bar to show the level");
}

/// Hint sits right after the left zone at every width, in both languages, with and without a
/// running task, and never runs into the right zone.
#[test]
fn hint_stays_next_to_the_left_zone_at_every_width() {
    for lang in [Lang::En, Lang::Ru] {
        let mut a = app();
        a.sh.cat = Catalog::new(lang);
        let ctx = font_ctx();
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let v = gather(&mut a, 0.0);
                let mut acts = Vec::new();
                let sh = &mut a.sh;
                let p = Painter {
                    th: &sh.theme,
                    c: &sh.cat,
                    icons: &mut sh.icons,
                    acts: &mut acts,
                };
                let items = p.right_items(ui, &v);
                let mut right: Vec<Slot> = items
                    .iter()
                    .map(|(_, ch)| Slot {
                        full: ch.as_ref().map_or(TASK_W, |c| c.width(true)),
                        compact: ch.as_ref().map_or(TASK_W, |c| c.width(false)),
                    })
                    .collect();
                for task in [false, true] {
                    if task {
                        right.insert(
                            0,
                            Slot {
                                full: TASK_W,
                                compact: TASK_W,
                            },
                        );
                    }
                    let n = Need {
                        left_fixed: BTN + ITEM_GAP + 3.0 * BTN + 2.0 * TOGGLE_GAP,
                        coords: Coords::measure(ui, p.th, &v).width,
                        hint: 520.0,
                        right: right.clone(),
                    };
                    for w in [1600.0, 1280.0, 1024.0, 800.0, 640.0] {
                        let f = fit(w, &n);
                        let tag = format!("{lang:?} task={task} w={w} {f:?}");
                        assert!(f.labels || f.hint == 0.0, "{tag}");
                        if w >= 1024.0 {
                            assert!(f.used() <= w + 0.01, "{tag}");
                        }
                        if f.hint > 0.0 {
                            let left_end = PAD + f.left;
                            assert_eq!(hint_x(left_end), left_end + ZONE_GAP, "{tag}");
                            let right_start = w - PAD - f.right;
                            assert!(hint_x(left_end) + f.hint <= right_start - ZONE_GAP + 0.01);
                        }
                    }
                }
            });
        });
    }
}

#[test]
fn controls_that_do_not_fit_move_to_the_overflow_menu_from_the_left() {
    let n = need(520.0);
    let m = n.right.len();
    let mut prev = 0;
    let mut w = 2560.0;
    while w >= 200.0 {
        let f = fit(w, &n);
        assert!(f.hidden >= prev, "w {w}: a wider bar hid more");
        assert!(f.hidden < m, "w {w}: the last control must stay");
        if f.hidden > 0 {
            assert!(f.compacted == m && !f.coords && f.hint == 0.0, "w {w}: {f:?}");
            // the button and what stays fit whenever anything fits at all
            let shown = n.right[f.hidden..].iter().map(|s| s.compact).sum::<f32>();
            assert!(f.right >= BTN + shown, "w {w}: {f:?}");
        } else {
            assert!(f.used() <= w || f.compacted == m, "w {w}: {f:?}");
        }
        prev = f.hidden;
        w -= 4.0;
    }
    assert_eq!(fit(2560.0, &n).hidden, 0);
    assert!(fit(200.0, &n).hidden > 0);
}

/// One off switch: F3 in the plan and the 3D command flip the same state.
#[test]
fn f3_toggles_both_views() {
    let mut a = app();
    a.view3d = false;
    assert!(a.run("view.snap_toggle"));
    let on = a.sbar.snap;
    assert_eq!(a.view.e3.snaps_off, !on, "F3 in the plan reaches the 3D flag");
    a.view3d = true;
    assert!(a.run("e3d.snap"));
    assert_eq!(a.sbar.snap, !on, "the 3D command flips the plan flag too");
    assert_eq!(a.view.e3.snaps_off, on);
}

/// The 3D editor sees the plan tool's family profile after a sync.
#[test]
fn profile_reaches_3d() {
    let mut a = app();
    a.ed.tool.profile = crate::snap::SnapProfile::COLUMN;
    sync(&mut a);
    assert_eq!(a.view.e3.profile, crate::snap::SnapProfile::COLUMN);
    let m = a.view.e3.profile.mask3(a.view.e3.mask);
    assert!(!m.has(crate::edit3d::snap::SnapKind::Edge) && m.has(crate::edit3d::snap::SnapKind::Vertex));
    // F3 writes the editor's state and both views follow
    a.ed.tool.snap_state.off = false;
    assert!(a.run("view.snap_toggle"));
    assert!(a.ed.tool.snap_state.off && a.view.e3.snaps_off && !a.sbar.snap && !a.ed.tool.snap_on);
}

/// The 3D profile is the active 3D tool's, not the plan tool's; z follows the work plane.
#[test]
fn profile_and_z_follow_the_active_tool_and_plane() {
    let mut a = app();
    a.ed.tool.profile = crate::snap::SnapProfile::COLUMN;
    a.view3d = true;
    a.view.e3.tool = crate::edit3d::Tool::Select;
    a.view.e3.plane = crate::edit3d::plane::WorkPlane::level(1, 3.0);
    sync(&mut a);
    assert_eq!(a.view.e3.profile, crate::snap::SnapProfile::ALL, "the plan tool's profile stays out of 3D");
    assert_eq!(a.ed.tool.snap_state.z, 3.0);
    a.view3d = false;
    a.ed.elevation = 4.5;
    sync(&mut a);
    assert_eq!(a.ed.tool.snap_state.z, 4.5, "plan: the level elevation");
    a.view3d = true;
    a.view.e3.tool = crate::edit3d::Tool::Create(crate::edit3d::create::Kind::Column);
    sync(&mut a);
    assert_eq!(a.view.e3.profile, crate::elem::by_table("col").map_or(crate::snap::SnapProfile::ALL, |f| f.snap));
}
