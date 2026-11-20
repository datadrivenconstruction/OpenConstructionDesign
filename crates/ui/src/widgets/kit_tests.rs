// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Headless tests of the widget kit: every widget draws in both themes and densities, keyboard
//! rules hold, button and row states stay readable.
use super::button::{colors, Visual};
use super::field::{field_name, format_number, parse_number, FieldOutput};
use super::props::prop_columns;
use super::table::{column_widths, next_sort, reveal_offset, row_colors};
use super::tabs::tab_widths;
use super::*;
use crate::sem::{AA_GRAPHIC, AA_TEXT as AA};
use crate::theme::{contrast, layout, over_rgba, sem, Density, Mode};
use crate::test_fx::fx;
use egui::{Event, Key, RawInput};

fn key(k: Key) -> RawInput {
    RawInput {
        events: vec![Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        }],
        ..Default::default()
    }
}

fn ready(mode: Mode, density: Density) -> (egui::Context, Theme) {
    let ctx = egui::Context::default();
    let mut th = Theme::new(mode, density);
    let _ = ctx.run(Default::default(), |c| {
        th.apply(c);
    });
    let _ = ctx.run(Default::default(), |c| {
        th.apply(c);
    });
    (ctx, th)
}

#[test]
fn all_widgets_draw_in_every_theme_and_density() {
    for mode in [Mode::Light, Mode::Dark] {
        for density in [Density::Compact, Density::Comfortable] {
            let (ctx, th) = ready(mode, density);
            let mut buf = String::from(fx("kt_text"));
            let mut num = 1234.5;
            let (mut sel, mut tab, mut on) = (0usize, 0usize, true);
            let cols = [Column::text(fx("kt_name")), Column::number(fx("kt_len"), 96.0)];
            let mut sort = Some(Sort { col: 1, desc: true });
            for _ in 0..2 {
                let _ = ctx.run(Default::default(), |c| {
                    egui::CentralPanel::default().show(c, |ui| {
                        Button::primary(fx("kt_apply")).show(ui, &th);
                        Button::secondary(fx("kt_cancel")).small().show(ui, &th);
                        Button::quiet(fx("kt_more")).large().show(ui, &th);
                        Button::danger(fx("kt_delete"))
                            .tooltip(fx("kt_delete"), "Del")
                            .show(ui, &th);
                        ui.add_enabled_ui(false, |ui| Button::primary(fx("kt_no")).show(ui, &th));
                        TextField::new(&mut buf).unit(fx("mm")).hint("0").show(ui, &th);
                        NumberField::new("n", &mut num, NumberFormat::RU)
                            .decimals(2)
                            .unit(fx("kt_m2"))
                            .show(ui, &th);
                        dropdown(ui, &th, "d", &mut sel, &[fx("kt_one"), fx("kt_two")], None);
                        segmented(ui, &th, &[fx("plan"), "3D"], 0);
                        toggle(ui, &th, &mut on, fx("kt_grid"));
                        checkbox(ui, &th, &mut on, fx("kt_snap"));
                        checkbox_mixed(ui, &th, &mut on, true, fx("kt_mixed"));
                        tab_bar(ui, &th, &[fx("kt_general"), fx("kt_view")], &mut tab);
                        section(ui, &th, fx("kt_sizes"), |ui| {
                            prop_value(ui, &th, fx("kt_height"), "3\u{a0}000", fx("mm"));
                            prop_text(ui, &th, fx("kt_name"), fx("kt_longname"));
                            prop_row(ui, &th, fx("kt_mode"), |ui| ui.label("x"));
                        });
                        Table::new("t", &cols, 3).max_height(120.0).show(
                            ui,
                            &th,
                            &mut sort,
                            |r| match r {
                                0 => RowState::Selected,
                                1 => RowState::Warning,
                                _ => RowState::Normal,
                            },
                            |r, c| {
                                if c == 0 {
                                    Cell::text(format!("{} {r}", fx("kt_wall_cap")))
                                } else {
                                    Cell::num("3\u{a0}000", fx("mm"))
                                }
                            },
                        );
                        card(ui, &th, |ui| ui.label(fx("kt_card")));
                        card_button(ui, &th, true, |ui| {
                            ui.label(fx("kt_selected"));
                        });
                        badge(ui, &th, fx("kt_ready"), Tone::Success);
                        empty_state(
                            ui,
                            &th,
                            None,
                            fx("kt_nosheets"),
                            fx("kt_makesheet"),
                            Some(fx("kt_create")),
                        );
                        loading_state(ui, &th, fx("kt_loading"));
                        error_state(ui, &th, fx("kt_error"), fx("kt_retry_hint"), Some(fx("kt_retry")));
                        progress(ui, &th, Some(0.4), fx("kt_import"));
                        progress(ui, &th, None, "");
                    });
                });
            }
        }
    }
}

/// Enabled button states are readable in both themes.
#[test]
fn button_states_pass_aa() {
    for dark in [false, true] {
        let s = sem(dark);
        for v in [
            Variant::Primary,
            Variant::Secondary,
            Variant::Quiet,
            Variant::Danger,
            Variant::DangerText,
        ] {
            for st in [Visual::Rest, Visual::Hover, Visual::Pressed] {
                let c = colors(s, v, st);
                for surface in [s.bg_panel, s.bg_elevated] {
                    let r = contrast(c.text, over_rgba(c.fill, surface));
                    assert!(r >= AA, "dark={dark} {v:?} {st:?} on {surface:?}: {r:.2}");
                }
            }
        }
    }
}

/// Disabled is dimmed once: a disabled Ui does not fade, the kit draws flat `text.disabled`.
/// Computed on what reaches the screen (fill over the surface), not on the tokens alone.
#[test]
fn disabled_is_dimmed_once() {
    let (ctx, _) = ready(Mode::Dark, Density::Compact);
    assert_eq!(ctx.style().visuals.disabled_alpha(), 1.0, "a disabled Ui must not fade again");
    for dark in [false, true] {
        let s = sem(dark);
        for v in [
            Variant::Primary,
            Variant::Secondary,
            Variant::Quiet,
            Variant::Danger,
            Variant::DangerText,
        ] {
            let c = colors(s, v, Visual::Disabled);
            assert_eq!(c.text, s.text_disabled, "dark={dark} {v:?}");
            assert_eq!(c.text.a(), 255, "dark={dark} {v:?}: disabled text is opaque");
            for surface in [s.bg_panel, s.bg_elevated] {
                let fill = over_rgba(c.fill, surface);
                let r = contrast(c.text, fill);
                assert!(r >= AA_GRAPHIC, "dark={dark} {v:?}: {r:.2}");
            }
        }
        // drawn without a disabled Ui, once: the flat token
        let (fill, fg, muted) = row_colors(s, RowState::Disabled, false);
        assert_eq!(fill, egui::Color32::TRANSPARENT);
        assert!(contrast(fg, s.bg_panel) >= AA_GRAPHIC, "dark={dark}");
        assert!(contrast(muted, s.bg_panel) >= AA_GRAPHIC, "dark={dark}");
    }
}

/// `ui.button` paints the theme fill whatever surface it sits on: its text must read on that fill.
#[test]
fn themed_buttons_pass_aa() {
    for mode in [Mode::Light, Mode::Dark] {
        let style = Theme::new(mode, Density::Compact).style();
        let w = &style.visuals.widgets;
        for (name, v) in [
            ("inactive", &w.inactive),
            ("hovered", &w.hovered),
            ("active", &w.active),
            ("open", &w.open),
        ] {
            let r = contrast(v.fg_stroke.color, v.weak_bg_fill);
            assert!(r >= AA, "{mode:?} {name}: {r:.2}");
        }
    }
}

#[test]
fn row_states_pass_aa() {
    for dark in [false, true] {
        let s = sem(dark);
        for st in [
            RowState::Normal,
            RowState::Selected,
            RowState::Warning,
            RowState::Error,
        ] {
            for hovered in [false, true] {
                let (fill, fg, muted) = row_colors(s, st, hovered);
                let bg = if fill == egui::Color32::TRANSPARENT {
                    s.bg_panel
                } else {
                    over_rgba(fill, s.bg_panel)
                };
                assert!(contrast(fg, bg) >= AA, "dark={dark} {st:?}");
                assert!(contrast(muted, bg) >= AA, "dark={dark} {st:?} muted");
            }
        }
    }
}

#[test]
fn tones_pass_aa() {
    for dark in [false, true] {
        let s = sem(dark);
        for t in [
            Tone::Neutral,
            Tone::Accent,
            Tone::Success,
            Tone::Warning,
            Tone::Danger,
        ] {
            let r = contrast(t.foreground(s), over_rgba(t.background(s), s.bg_panel));
            assert!(r >= AA, "dark={dark} {t:?}: {r:.2}");
        }
    }
}

#[test]
fn numbers_format_for_locale() {
    assert_eq!(
        format_number(1234.567, 2, NumberFormat::RU),
        "1\u{a0}234,57"
    );
    assert_eq!(format_number(1234.567, 2, NumberFormat::EN), "1,234.57");
    assert_eq!(format_number(3000.0, 0, NumberFormat::RU), "3\u{a0}000");
    assert_eq!(format_number(-1.95, 3, NumberFormat::EN), "-1.950");
    assert_eq!(format_number(999.0, 0, NumberFormat::RU), "999");
    assert_eq!(format_number(1_000_000.0, 0, NumberFormat::EN), "1,000,000");
    assert_eq!(format_number(-0.001, 1, NumberFormat::RU), "0,0");
}

#[test]
fn numbers_parse_any_locale() {
    let (ru, en) = (NumberFormat::RU, NumberFormat::EN);
    assert_eq!(parse_number("1\u{a0}234,57", ru), Some(1234.57));
    assert_eq!(parse_number("1,234.57", ru), Some(1234.57));
    assert_eq!(parse_number("1,234.57", en), Some(1234.57));
    assert_eq!(parse_number("1.234,57", en), Some(1234.57));
    assert_eq!(parse_number("3 000", ru), Some(3000.0));
    assert_eq!(parse_number("1,5", ru), Some(1.5));
    // ambiguous in English: refused, never read as 15 or 1500
    assert_eq!(parse_number("1,5", en), None);
    assert_eq!(parse_number("\u{2212}2", en), Some(-2.0));
    assert_eq!(parse_number("1,000,000", en), Some(1_000_000.0));
    assert_eq!(parse_number("1.000.000", ru), Some(1_000_000.0));
    for nf in [ru, en] {
        assert_eq!(parse_number("", nf), None);
        assert_eq!(parse_number("abc", nf), None);
        assert_eq!(parse_number("inf", nf), None);
        assert_eq!(parse_number(",", nf), None);
    }
}

/// A lone separator: an English comma followed by three digits groups, any other English comma is
/// refused; the Russian comma and any point are decimals.
#[test]
fn lone_separator_reads_by_locale() {
    let (ru, en) = (NumberFormat::RU, NumberFormat::EN);
    assert_eq!(parse_number("1,234", en), Some(1234.0));
    assert_eq!(parse_number("-1,234", en), Some(-1234.0));
    assert_eq!(parse_number("123,456", en), Some(123_456.0));
    assert_eq!(parse_number("1,234", ru), Some(1.234));
    assert_eq!(parse_number("1.234", en), Some(1.234));
    assert_eq!(parse_number("1.234", ru), Some(1.234));
    assert_eq!(parse_number("0,234", en), None);
    assert_eq!(parse_number("1234,567", en), None);
    assert_eq!(parse_number("1,2345", en), None);
    assert_eq!(parse_number("0,234", ru), Some(0.234));
    assert_eq!(parse_number("1,2345", ru), Some(1.2345));
}

#[test]
fn misplaced_groups_are_invalid() {
    let en = NumberFormat::EN;
    assert_eq!(parse_number("1,23,456", en), None);
    assert_eq!(parse_number("1,000,00", en), None);
    assert_eq!(parse_number("1234,567.5", en), None);
    assert_eq!(parse_number("1,2.5", en), None);
    assert_eq!(parse_number("1.5,2,3", en), None);
}

#[test]
fn number_round_trips_through_format() {
    for nf in [NumberFormat::RU, NumberFormat::EN] {
        for v in [0.0, 1.25, -7.5, 1234.5, 1_000_000.25] {
            assert_eq!(
                parse_number(&format_number(v, 2, nf), nf),
                Some(v),
                "{nf:?}"
            );
        }
        for v in [0.0, 7.0, 999.0, -1234.0, 1234.0, 123_456.0, 1_000_000.0] {
            assert_eq!(
                parse_number(&format_number(v, 0, nf), nf),
                Some(v),
                "{nf:?}"
            );
        }
    }
}

fn text(s: &str) -> RawInput {
    RawInput {
        events: vec![Event::Text(s.to_owned())],
        ..Default::default()
    }
}

fn num_pass(
    ctx: &egui::Context,
    th: &Theme,
    input: RawInput,
    v: &mut f64,
    decimals: usize,
    nf: NumberFormat,
    focus: bool,
) -> FieldOutput {
    let mut got = None;
    let _ = ctx.run(input, |c| {
        egui::CentralPanel::default().show(c, |ui| {
            let out = NumberField::new("n", &mut *v, nf)
                .decimals(decimals)
                .show(ui, th);
            if focus {
                out.response.request_focus();
            }
            got = Some(out);
        });
    });
    got.expect("the field was drawn")
}

/// Puts focus on the field and lets egui settle, so arrows and typing reach it.
fn focus_num(ctx: &egui::Context, th: &Theme, v: &mut f64, decimals: usize, nf: NumberFormat) {
    num_pass(ctx, th, Default::default(), v, decimals, nf, true);
    num_pass(ctx, th, Default::default(), v, decimals, nf, false);
}

#[test]
fn tab_through_a_number_changes_nothing() {
    for nf in [NumberFormat::RU, NumberFormat::EN] {
        for (start, decimals) in [
            (1234.0, 0),
            (1234.5678, 2),
            (0.1 + 0.2, 1),
            (-1_000_000.0, 0),
            (1234.0, 3),
        ] {
            let (ctx, th) = ready(Mode::Light, Density::Compact);
            let mut v: f64 = start;
            focus_num(&ctx, &th, &mut v, decimals, nf);
            for input in [key(Key::Tab), RawInput::default(), RawInput::default()] {
                let out = num_pass(&ctx, &th, input, &mut v, decimals, nf, false);
                assert!(
                    !out.committed && !out.cancelled,
                    "{nf:?} {start} at {decimals}"
                );
                assert_eq!(
                    v.to_bits(),
                    f64::to_bits(start),
                    "{nf:?} {start} at {decimals}"
                );
            }
        }
    }
}

#[test]
fn enter_on_an_untouched_number_changes_nothing() {
    let (ctx, th) = ready(Mode::Light, Density::Compact);
    let mut v = 1234.5678;
    focus_num(&ctx, &th, &mut v, 2, NumberFormat::EN);
    let out = num_pass(
        &ctx,
        &th,
        key(Key::Enter),
        &mut v,
        2,
        NumberFormat::EN,
        false,
    );
    assert!(!out.committed);
    assert_eq!(v, 1234.5678);
}

#[test]
fn typed_number_commits_in_both_locales() {
    // the focused text has no group sign: appending to «1,234» must not read as a decimal
    for nf in [NumberFormat::RU, NumberFormat::EN] {
        let (ctx, th) = ready(Mode::Light, Density::Compact);
        let mut v = 1234.0;
        focus_num(&ctx, &th, &mut v, 0, nf);
        let typed = num_pass(&ctx, &th, text("5"), &mut v, 0, nf, false);
        assert!(typed.changed && !typed.committed, "{nf:?}");
        assert_eq!(v, 1234.0, "{nf:?}: typing alone does not apply");
        let out = num_pass(&ctx, &th, key(Key::Tab), &mut v, 0, nf, false);
        assert!(out.committed, "{nf:?}");
        assert_eq!(v, 12345.0, "{nf:?}");
    }
}

#[test]
fn deleting_a_digit_keeps_the_magnitude() {
    let (ctx, th) = ready(Mode::Light, Density::Compact);
    let nf = NumberFormat::EN;
    let mut v = 1234.0;
    focus_num(&ctx, &th, &mut v, 0, nf);
    num_pass(&ctx, &th, key(Key::Backspace), &mut v, 0, nf, false);
    let out = num_pass(&ctx, &th, key(Key::Enter), &mut v, 0, nf, false);
    assert!(out.committed);
    assert_eq!(v, 123.0);
}

#[test]
fn locale_decimal_is_typed_in_place() {
    let (ctx, th) = ready(Mode::Light, Density::Compact);
    let nf = NumberFormat::RU;
    let mut v = 1234.5;
    focus_num(&ctx, &th, &mut v, 2, nf);
    num_pass(&ctx, &th, text("5"), &mut v, 2, nf, false);
    let out = num_pass(&ctx, &th, key(Key::Tab), &mut v, 2, nf, false);
    assert!(out.committed);
    assert_eq!(v, 1234.505);
}

#[test]
fn arrows_step_from_the_stored_value() {
    let (ctx, th) = ready(Mode::Light, Density::Compact);
    let nf = NumberFormat::EN;
    let mut v = 1234.5678;
    focus_num(&ctx, &th, &mut v, 2, nf);
    let out = num_pass(&ctx, &th, key(Key::ArrowUp), &mut v, 2, nf, false);
    assert!(out.committed);
    assert_eq!(v, 1234.5678 + 1.0);
    // the step was committed: leaving afterwards is not a second undo step
    let stepped = v;
    let out = num_pass(&ctx, &th, key(Key::Tab), &mut v, 2, nf, false);
    assert!(!out.committed);
    assert_eq!(v, stepped);
}

#[test]
fn esc_drops_a_typed_number() {
    let (ctx, th) = ready(Mode::Light, Density::Compact);
    let nf = NumberFormat::EN;
    let mut v = 1234.0;
    focus_num(&ctx, &th, &mut v, 0, nf);
    num_pass(&ctx, &th, text("5"), &mut v, 0, nf, false);
    let out = num_pass(&ctx, &th, key(Key::Escape), &mut v, 0, nf, false);
    assert!(out.cancelled && !out.committed);
    assert_eq!(v, 1234.0);
    let after = num_pass(&ctx, &th, Default::default(), &mut v, 0, nf, false);
    assert!(!after.committed);
    assert_eq!(v, 1234.0);
}

#[test]
fn enter_on_invalid_number_keeps_the_field_open() {
    let (ctx, th) = ready(Mode::Light, Density::Compact);
    let nf = NumberFormat::EN;
    let mut v = 1234.0;
    focus_num(&ctx, &th, &mut v, 0, nf);
    num_pass(&ctx, &th, text("x"), &mut v, 0, nf, false);
    let out = num_pass(&ctx, &th, key(Key::Enter), &mut v, 0, nf, false);
    assert!(!out.committed && !out.cancelled);
    assert!(
        out.response.has_focus(),
        "focus stays so a dialog cannot take this Enter"
    );
    assert_eq!(v, 1234.0);
    let out = num_pass(&ctx, &th, key(Key::Tab), &mut v, 0, nf, false);
    assert!(out.cancelled && !out.committed);
    assert_eq!(v, 1234.0);
}

/// One pass of two fields; `focus` moves focus right after that field is drawn, like a press.
fn pair_pass(
    ctx: &egui::Context,
    th: &Theme,
    input: RawInput,
    top: &mut f64,
    low: &mut f64,
    focus: Option<&str>,
) -> (FieldOutput, FieldOutput) {
    let mut got = None;
    let _ = ctx.run(input, |c| {
        egui::CentralPanel::default().show(c, |ui| {
            let a = NumberField::new("top", &mut *top, NumberFormat::EN).show(ui, th);
            if focus == Some("top") {
                a.response.request_focus();
            }
            let b = NumberField::new("low", &mut *low, NumberFormat::EN).show(ui, th);
            if focus == Some("low") {
                b.response.request_focus();
            }
            got = Some((a, b));
        });
    });
    got.expect("the fields were drawn")
}

#[test]
fn press_on_an_earlier_field_keeps_the_typed_number() {
    let (ctx, th) = ready(Mode::Light, Density::Compact);
    let (mut top, mut low) = (1.0, 1234.0);
    pair_pass(
        &ctx,
        &th,
        Default::default(),
        &mut top,
        &mut low,
        Some("low"),
    );
    pair_pass(&ctx, &th, Default::default(), &mut top, &mut low, None);
    pair_pass(&ctx, &th, text("7"), &mut top, &mut low, None);
    // the earlier field takes focus before the typed one runs
    let (a, b) = pair_pass(
        &ctx,
        &th,
        Default::default(),
        &mut top,
        &mut low,
        Some("top"),
    );
    assert!(b.committed, "the typed field commits");
    assert!(!a.committed);
    assert_eq!((top, low), (1.0, 12347.0));
}

#[test]
fn press_on_a_later_field_keeps_the_typed_number() {
    let (ctx, th) = ready(Mode::Light, Density::Compact);
    let (mut top, mut low) = (1234.0, 1.0);
    pair_pass(
        &ctx,
        &th,
        Default::default(),
        &mut top,
        &mut low,
        Some("top"),
    );
    pair_pass(&ctx, &th, Default::default(), &mut top, &mut low, None);
    pair_pass(&ctx, &th, text("7"), &mut top, &mut low, None);
    // the later field takes focus after the typed one ran; egui reports no lost focus next pass
    pair_pass(
        &ctx,
        &th,
        Default::default(),
        &mut top,
        &mut low,
        Some("low"),
    );
    let (a, b) = pair_pass(&ctx, &th, Default::default(), &mut top, &mut low, None);
    assert!(a.committed, "the typed field commits");
    assert!(!b.committed);
    assert_eq!((top, low), (12347.0, 1.0));
}

#[test]
fn edit_of_a_vanished_field_is_dropped() {
    let (ctx, th) = ready(Mode::Light, Density::Compact);
    let nf = NumberFormat::EN;
    let mut v = 1234.0;
    focus_num(&ctx, &th, &mut v, 0, nf);
    num_pass(&ctx, &th, text("7"), &mut v, 0, nf, false);
    let _ = ctx.run(Default::default(), |_| {});
    let out = num_pass(&ctx, &th, Default::default(), &mut v, 0, nf, false);
    assert!(!out.committed);
    assert_eq!(v, 1234.0);
}

#[test]
fn text_field_commits_only_an_edit() {
    let (ctx, th) = ready(Mode::Light, Density::Compact);
    let mut buf = String::from("abc");
    let mut pass = |input: RawInput, focus: bool| {
        let mut got = None;
        let _ = ctx.run(input, |c| {
            egui::CentralPanel::default().show(c, |ui| {
                let out = TextField::new(&mut buf).id_salt("t").show(ui, &th);
                if focus {
                    out.response.request_focus();
                }
                got = Some(out);
            });
        });
        got.expect("the field was drawn")
    };
    pass(Default::default(), true);
    pass(Default::default(), false);
    assert!(
        !pass(key(Key::Tab), false).committed,
        "Tab through an untouched field"
    );
    pass(Default::default(), true);
    pass(Default::default(), false);
    assert!(
        pass(key(Key::Enter), false).committed,
        "Enter always commits"
    );
    pass(Default::default(), true);
    pass(Default::default(), false);
    pass(text("d"), false);
    assert!(pass(key(Key::Tab), false).committed, "Tab after an edit");
    pass(Default::default(), true);
    pass(Default::default(), false);
    pass(text("e"), false);
    let esc = pass(key(Key::Escape), false);
    assert!(esc.cancelled && !esc.committed);
    assert_eq!(buf, "abcd", "Esc restores the text from focus");
}

#[test]
fn sort_cycle() {
    let a = next_sort(None, 2);
    assert_eq!(
        a,
        Some(Sort {
            col: 2,
            desc: false
        })
    );
    let b = next_sort(a, 2);
    assert_eq!(b, Some(Sort { col: 2, desc: true }));
    assert_eq!(next_sort(b, 2), None);
    assert_eq!(
        next_sort(b, 1),
        Some(Sort {
            col: 1,
            desc: false
        })
    );
}

#[test]
fn columns_share_free_width() {
    let cols = [
        Column::text("a"),
        Column::text("b").fill(3.0),
        Column::number("c", 100.0),
    ];
    let w = column_widths(&cols, 500.0);
    assert_eq!(w, vec![100.0, 300.0, 100.0]);
    let narrow = column_widths(&cols, 50.0);
    assert!(narrow[0] > 0.0 && narrow[1] > 0.0, "{narrow:?}");
}

#[test]
fn table_scrolls_only_as_far_as_needed() {
    // visible rows 10..20 at 20 px per row
    assert_eq!(reveal_offset(200.0, 15, 20.0, 200.0), 200.0);
    assert_eq!(reveal_offset(200.0, 9, 20.0, 200.0), 180.0);
    assert_eq!(reveal_offset(200.0, 20, 20.0, 200.0), 220.0);
}

#[test]
fn tabs_shrink_only_when_needed() {
    assert_eq!(tab_widths(&[50.0, 50.0], 300.0), vec![50.0, 50.0]);
    assert_eq!(tab_widths(&[100.0, 100.0], 100.0), vec![50.0, 50.0]);
}

#[test]
fn prop_columns_stay_in_bounds() {
    let (k, v, u) = prop_columns(300.0);
    assert_eq!((v, u), (layout::PROP_VALUE, layout::PROP_UNIT));
    assert!((layout::PROP_KEY_MIN..=layout::PROP_KEY_MAX).contains(&k));
    assert_eq!(prop_columns(100.0).0, layout::PROP_KEY_MIN);
    assert_eq!(prop_columns(2000.0).0, layout::PROP_KEY_MAX);
}

#[test]
fn accesskit_named_node_per_ribbon_button() {
    use super::ribbon::a11y_label;
    assert_eq!(a11y_label("Save", "Ctrl+S", None), "Save (Ctrl+S)");
    assert_eq!(a11y_label("Wall", "", None), "Wall");
}

#[test]
fn disabled_ribbon_button_reports_reason() {
    use super::ribbon::a11y_label;
    assert_eq!(
        a11y_label("Save", "Ctrl+S", Some("No model open")),
        "Save (Ctrl+S) - No model open"
    );
    assert_eq!(a11y_label("Save", "", Some("")), "Save");
}

#[test]
fn split_button_halves_both_named() {
    use super::ribbon::a11y_label;
    let main = a11y_label("Wall", "W", None);
    let caret = a11y_label("Wall - More wall types", "", None);
    assert!(!main.is_empty() && !caret.is_empty());
    assert_ne!(main, caret);
}

#[test]
fn overflow_row_named() {
    use super::ribbon::a11y_label;
    let row = a11y_label("Door", "D", Some("Open a level first"));
    assert!(row.starts_with("Door") && row.contains("Open a level first"));
}

// The accessibility tree is not built in headless runs, so the name is tested through the pure
// helper that feeds widget_info; the focus order is tested on real widgets.
#[test]
fn field_input_named_by_label() {
    assert_eq!(field_name("Height", "", ""), "Height");
    assert_eq!(field_name("", "", ""), "");
}

#[test]
fn field_name_includes_unit() {
    assert_eq!(field_name("Height", "mm", ""), "Height, mm");
}

#[test]
fn invalid_field_reports_error_text() {
    assert_eq!(field_name("Height", "mm", "Not a number"), "Height, mm - Not a number");
}

#[test]
fn field_decoration_not_focusable() {
    let (ctx, th) = ready(Mode::Light, Density::Comfortable);
    let mut buf = String::new();
    let mut ids = Vec::new();
    let _ = ctx.run(Default::default(), |c| {
        egui::CentralPanel::default().show(c, |ui| {
            let label = ui.label("Height");
            let out = TextField::new(&mut buf)
                .id_salt("h")
                .unit("mm")
                .labelled_by(label.id, "Height")
                .show(ui, &th);
            ids.push((label.sense.is_focusable(), out.response.sense.is_focusable()));
        });
    });
    assert_eq!(ids, vec![(false, true)]);
}

/// `labelled_by` relates the input to a label that was drawn; an id no widget owns is never
/// put in the tree, and the text still names the input.
#[test]
fn field_relation_points_only_at_a_drawn_label() {
    let (ctx, th) = ready(Mode::Light, Density::Comfortable);
    ctx.enable_accesskit();
    let (mut a, mut b, mut d) = (String::new(), String::new(), String::new());
    let (mut label, mut painted) = (egui::Id::NULL, egui::Id::NULL);
    let mut out = None;
    for _ in 0..2 {
        out = Some(ctx.run(Default::default(), |c| {
            egui::CentralPanel::default().show(c, |ui| {
                label = ui.label("Height").id;
                TextField::new(&mut a).id_salt("a").labelled_by(label, "Height").show(ui, &th);
                let ghost = egui::Id::new("nobody.draws.this");
                TextField::new(&mut b).id_salt("b").labelled_by(ghost, "Width").show(ui, &th);
                // a caption that only paints (no node of its own)
                let (font, ink) = (th.font(crate::theme::Role::Body), th.sem().text_primary);
                let cap = elided_label(ui, "Depth", font, ink);
                TextField::new(&mut d).id_salt("d").labelled_by(cap.id, "Depth").show(ui, &th);
                painted = cap.id;
            });
        }));
    }
    let nodes = out.unwrap().platform_output.accesskit_update.expect("tree").nodes;
    let ids: std::collections::HashSet<_> = nodes.iter().map(|(id, _)| *id).collect();
    let input = |name: &str| {
        let n = nodes.iter().find(|(_, n)| {
            let named = n.label().is_some_and(|l| l.starts_with(name));
            named && n.role() == egui::accesskit::Role::TextInput
        });
        n.unwrap_or_else(|| panic!("no node {name}")).1.labelled_by().to_vec()
    };
    let node = |id: egui::Id| egui::accesskit::NodeId::from(id.value());
    assert_eq!(input("Height"), vec![node(label)]);
    assert!(input("Width").is_empty(), "no relation to an absent node");
    assert_eq!(input("Depth"), vec![node(painted)], "a painted caption gets a node");
    for (_, n) in &nodes {
        assert!(n.labelled_by().iter().all(|l| ids.contains(l)), "{n:?}");
    }
}

struct SegRun {
    changed: Option<usize>,
    ids: Vec<egui::Id>,
    after: egui::Id,
}

fn seg_frame(
    ctx: &egui::Context,
    th: &Theme,
    input: RawInput,
    items: &[&str],
    sel: usize,
    enabled: &[bool],
) -> SegRun {
    let mut run = SegRun {
        changed: None,
        ids: vec![],
        after: egui::Id::NULL,
    };
    let _ = ctx.run(input, |c| {
        egui::CentralPanel::default().show(c, |ui| {
            run.ids = items.iter().map(|t| ui.id().with(("seg", *t))).collect();
            run.changed = segmented_enabled(ui, th, items, sel, enabled);
            run.after = ui.button("after").id;
        });
    });
    run
}

fn seg_ready() -> (egui::Context, Theme) {
    ready(Mode::Light, Density::Comfortable)
}

fn focus(ctx: &egui::Context, id: egui::Id) {
    ctx.memory_mut(|m| m.request_focus(id));
}

#[test]
fn segmented_arrows_move_selection() {
    let (ctx, th) = seg_ready();
    let items = ["a", "b", "c"];
    let r = seg_frame(&ctx, &th, Default::default(), &items, 0, &[]);
    focus(&ctx, r.ids[0]);
    let _ = seg_frame(&ctx, &th, Default::default(), &items, 0, &[]);
    let r = seg_frame(&ctx, &th, key(Key::ArrowRight), &items, 0, &[]);
    assert_eq!(r.changed, Some(1));
    let r = seg_frame(&ctx, &th, key(Key::End), &items, 1, &[]);
    assert_eq!(r.changed, Some(2));
    let r = seg_frame(&ctx, &th, key(Key::Home), &items, 2, &[]);
    assert_eq!(r.changed, Some(0));
    let r = seg_frame(&ctx, &th, key(Key::ArrowLeft), &items, 0, &[]);
    assert_eq!(r.changed, Some(2), "Left from the first pill wraps to the last");
}

#[test]
fn segmented_wraps_at_last_pill() {
    let (ctx, th) = seg_ready();
    let items = ["a", "b", "c"];
    let r = seg_frame(&ctx, &th, Default::default(), &items, 2, &[]);
    focus(&ctx, r.ids[2]);
    let _ = seg_frame(&ctx, &th, Default::default(), &items, 2, &[]);
    let r = seg_frame(&ctx, &th, key(Key::ArrowRight), &items, 2, &[]);
    assert_eq!(r.changed, Some(0));
}

#[test]
fn segmented_skips_disabled_pill() {
    let (ctx, th) = seg_ready();
    let items = ["a", "b", "c"];
    let en = [true, false, true];
    let r = seg_frame(&ctx, &th, Default::default(), &items, 0, &en);
    focus(&ctx, r.ids[0]);
    let _ = seg_frame(&ctx, &th, Default::default(), &items, 0, &en);
    let r = seg_frame(&ctx, &th, key(Key::ArrowRight), &items, 0, &en);
    assert_eq!(r.changed, Some(2));
    assert_eq!(super::seg_step(&en, 0, true), Some(2));
    assert_eq!(super::seg_step(&en, 2, false), Some(0));
}

#[test]
fn segmented_single_pill_has_nowhere_to_go() {
    let (ctx, th) = seg_ready();
    let items = ["only"];
    let r = seg_frame(&ctx, &th, Default::default(), &items, 0, &[]);
    focus(&ctx, r.ids[0]);
    let _ = seg_frame(&ctx, &th, Default::default(), &items, 0, &[]);
    for k in [Key::ArrowLeft, Key::ArrowRight, Key::Home, Key::End] {
        let r = seg_frame(&ctx, &th, key(k), &items, 0, &[]);
        assert_eq!(r.changed, None, "{k:?}");
    }
    assert_eq!(super::seg_step(&[true], 0, true), None);
}

#[test]
fn segmented_single_tab_stop() {
    let (ctx, th) = seg_ready();
    let items = ["a", "b", "c"];
    let tab = || RawInput {
        events: vec![Event::Key {
            key: Key::Tab,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        }],
        ..Default::default()
    };
    let _ = seg_frame(&ctx, &th, Default::default(), &items, 1, &[]);
    let _ = seg_frame(&ctx, &th, Default::default(), &items, 1, &[]);
    let r = seg_frame(&ctx, &th, tab(), &items, 1, &[]);
    let _ = seg_frame(&ctx, &th, Default::default(), &items, 1, &[]);
    assert_eq!(ctx.memory(|m| m.focused()), Some(r.ids[1]), "first Tab lands on the selected pill");
    let r = seg_frame(&ctx, &th, tab(), &items, 1, &[]);
    let _ = seg_frame(&ctx, &th, Default::default(), &items, 1, &[]);
    assert_eq!(ctx.memory(|m| m.focused()), Some(r.after), "second Tab leaves the group");
}

#[test]
fn segmented_accesskit_radio_named() {
    let on = super::seg_info("Plan", true, true);
    assert_eq!(on.typ, egui::WidgetType::RadioButton);
    assert_eq!(on.label.as_deref(), Some("Plan"));
    assert_eq!(on.selected, Some(true));
    assert!(on.enabled);
    let off = super::seg_info("3D", false, false);
    assert_eq!((off.selected, off.enabled), (Some(false), false));
}

#[test]
fn disclosure_is_one_small_square_in_both_themes() {
    for mode in [Mode::Light, Mode::Dark] {
        let (ctx, th) = ready(mode, Density::Compact);
        for open in [false, true] {
            let mut size = egui::Vec2::ZERO;
            let _ = ctx.run(Default::default(), |c| {
                egui::CentralPanel::default().show(c, |ui| {
                    let r = super::shell_kit::disclosure(ui, &th, open, "Show elements");
                    size = r.rect.size();
                });
            });
            assert_eq!(size, egui::Vec2::splat(th.d().control_sm));
        }
    }
}

// ---- keyboard focus ring: own layer, clipped, keyboard only ----

fn ring_count(out: &egui::FullOutput, th: &Theme) -> usize {
    ring_rects(out, th).len()
}

/// Focus rings drawn this frame: their inner rect and corner radius.
fn ring_rects(out: &egui::FullOutput, th: &Theme) -> Vec<(Rect, egui::CornerRadius)> {
    let col = th.c("focus.ring");
    out.shapes
        .iter()
        .filter_map(|c| match &c.shape {
            egui::Shape::Rect(r) if r.stroke.width == stroke::FOCUS && r.stroke.color == col => {
                Some((r.rect, r.corner_radius))
            }
            _ => None,
        })
        .collect()
}

fn ring_frame(
    ctx: &egui::Context,
    th: &Theme,
    input: RawInput,
    mut f: impl FnMut(&egui::Context),
) -> usize {
    let out = ctx.run(input, |c| {
        f(c);
        focus_ring(c, th);
    });
    ring_count(&out, th)
}

fn plain_button(c: &egui::Context) {
    egui::CentralPanel::default().show(c, |ui| {
        ui.button("ok");
    });
}

fn tab_to_button(ctx: &egui::Context, th: &Theme) -> usize {
    let mut n = 0;
    for input in [RawInput::default(), key(Key::Tab), RawInput::default(), RawInput::default()] {
        n = ring_frame(ctx, th, input, plain_button);
    }
    n
}

#[test]
fn ring_rect_clips_and_hides() {
    let screen = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(800.0, 600.0));
    let full = Rect::from_min_size(egui::pos2(10.0, 10.0), Vec2::new(100.0, 20.0));
    assert_eq!(ring_rect(full, full, screen, true), Some(full));
    // half visible: clipped to the interact rect
    let half = Rect::from_min_size(egui::pos2(10.0, 10.0), Vec2::new(100.0, 10.0));
    assert_eq!(ring_rect(full, half, screen, true), Some(half));
    // scrolled out: empty or inverted interact rect
    let gone = Rect::from_min_max(egui::pos2(10.0, 50.0), egui::pos2(110.0, 5.0));
    assert_eq!(ring_rect(full, gone, screen, true), None);
    assert_eq!(ring_rect(full, Rect::NOTHING, screen, true), None);
    // off screen
    let off = Rect::from_min_size(egui::pos2(900.0, 10.0), Vec2::new(100.0, 20.0));
    assert_eq!(ring_rect(off, off, screen, true), None);
    // covered by another layer
    assert_eq!(ring_rect(full, full, screen, false), None);
}

#[test]
fn focus_visible_follows_last_input() {
    assert!(!focus_visible(None, None));
    assert!(!focus_visible(None, Some(3)));
    assert!(focus_visible(Some(1), None));
    assert!(focus_visible(Some(4), Some(3)));
    assert!(!focus_visible(Some(3), Some(4)));
}

#[test]
fn tab_into_a_widget_paints_one_ring() {
    let (ctx, th) = ready(Mode::Light, Density::Compact);
    assert_eq!(tab_to_button(&ctx, &th), 1);
}

#[test]
fn mouse_focus_paints_no_ring() {
    let (ctx, th) = ready(Mode::Light, Density::Compact);
    let _ = ring_frame(&ctx, &th, RawInput::default(), plain_button);
    let pos = egui::pos2(20.0, 20.0);
    let press = RawInput {
        events: vec![
            Event::PointerMoved(pos),
            Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
        ],
        ..Default::default()
    };
    let mut n = 0;
    for (i, input) in [press, RawInput::default(), RawInput::default()].into_iter().enumerate() {
        n = ring_frame(&ctx, &th, input, |c| {
            egui::CentralPanel::default().show(c, |ui| {
                let r = ui.button("ok");
                if i == 0 {
                    r.request_focus();
                }
            });
        });
        assert_eq!(n, 0, "frame {i}");
    }
    assert_eq!(n, 0);
}

#[test]
fn ring_is_hidden_under_a_covering_layer() {
    let (ctx, th) = ready(Mode::Light, Density::Compact);
    assert_eq!(tab_to_button(&ctx, &th), 1);
    let mut n = 1;
    for _ in 0..3 {
        n = ring_frame(&ctx, &th, RawInput::default(), |c| {
            plain_button(c);
            egui::Area::new(egui::Id::new("cover"))
                .order(egui::Order::Foreground)
                .fixed_pos(egui::pos2(0.0, 0.0))
                .show(c, |ui| {
                    ui.allocate_exact_size(Vec2::new(780.0, 580.0), Sense::click());
                });
        });
    }
    assert_eq!(n, 0);
}

#[test]
fn ring_is_hidden_when_scrolled_out() {
    let (ctx, th) = ready(Mode::Light, Density::Compact);
    let mut n = 1;
    let inputs = [RawInput::default(), key(Key::Tab), RawInput::default(), RawInput::default()];
    for (i, input) in inputs.into_iter().enumerate() {
        n = ring_frame(&ctx, &th, input, |c| {
            egui::CentralPanel::default().show(c, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(40.0)
                    .vertical_scroll_offset(500.0)
                    .show(ui, |ui| {
                        ui.add_space(300.0);
                        let r = ui.button("deep");
                        // Tab lands on the scroll area's own drag stop; focus the button by hand
                        if i >= 2 {
                            r.request_focus();
                        }
                        ui.add_space(300.0);
                    });
            });
        });
    }
    assert_eq!(n, 0);
}

#[test]
fn own_focus_widget_gets_no_second_ring() {
    let (ctx, th) = ready(Mode::Light, Density::Compact);
    let mut v = 1.0_f64;
    let mut rings = vec![];
    let mut field = Rect::NOTHING;
    for input in [RawInput::default(), key(Key::Tab), RawInput::default(), RawInput::default()] {
        let out = ctx.run(input, |c| {
            egui::CentralPanel::default().show(c, |ui| {
                field = NumberField::new("n", &mut v, NumberFormat::EN).show(ui, &th).rect;
            });
            focus_ring(c, &th);
        });
        rings = ring_rects(&out, &th);
    }
    assert!(ctx.memory(|m| m.focused()).is_some(), "the field took focus");
    // since 06b2512ad the field draws the one shared ring itself; the global ring stays away
    let own = crate::theme::focus::ring(field, th.radius("sm"));
    assert_eq!(rings, vec![own], "exactly the field's own ring");
}

#[test]
fn focus_ring_pairs_both_themes() {
    for p in [("focus.ring", "canvas.paper"), ("focus.ring", "accent.subtle")] {
        assert!(
            crate::sem::GRAPHIC_PAIRS.iter().any(|g| g.0 == p.0 && g.1 == p.1),
            "{p:?} missing"
        );
    }
}

/// Field and dropdown frames in every state reach 3:1 on the field fill and the panel, and the
/// three states differ from each other.
#[test]
fn field_frames_pass_graphic_contrast() {
    for dark in [false, true] {
        let s = sem(dark);
        let states = [
            ("inactive", s.border_control),
            ("hovered", s.text_secondary),
            ("focused", s.focus),
        ];
        for (name, line) in states {
            for surface in [s.bg_input, s.bg_panel] {
                let r = contrast(line, surface);
                assert!(r >= AA_GRAPHIC, "dark={dark} {name} on {surface:?}: {r:.2}");
            }
        }
        assert!(states[0].1 != states[1].1 && states[1].1 != states[2].1 && states[0].1 != states[2].1);
    }
}

/// egui's own widgets (combo box, checkbox) take their frame from the style: inactive uses the
/// control border, hover a stronger one, dividers keep the subtle border.
#[test]
fn style_frames_use_control_tokens() {
    for mode in [Mode::Light, Mode::Dark] {
        let (ctx, _th) = ready(mode, Density::Compact);
        let s = sem(mode == Mode::Dark);
        let w = ctx.style().visuals.widgets.clone();
        assert_eq!(w.inactive.bg_stroke.color, s.border_control, "{mode:?} inactive");
        assert_eq!(w.hovered.bg_stroke.color, s.text_secondary, "{mode:?} hovered");
        assert_eq!(w.noninteractive.bg_stroke.color, s.border_subtle, "{mode:?} divider");
    }
}

/// A resting text field paints `border.control`, not the faint divider colour.
#[test]
fn text_field_paints_control_border() {
    for mode in [Mode::Light, Mode::Dark] {
        let (ctx, th) = ready(mode, Density::Compact);
        let mut buf = String::new();
        let mut shapes = Vec::new();
        for _ in 0..2 {
            let out = ctx.run(Default::default(), |c| {
                egui::CentralPanel::default().show(c, |ui| {
                    TextField::new(&mut buf).id_salt("f").show(ui, &th);
                });
            });
            shapes = out.shapes;
        }
        let want = th.sem().border_control;
        let hit = shapes.iter().any(|cs| match &cs.shape {
            egui::Shape::Rect(r) => r.stroke.color == want,
            _ => false,
        });
        assert!(hit, "{mode:?}: no rect with the control border");
    }
}

/// Buttons of a wrapping row in a screen `w` points wide.
fn wrap_row(ctx: &egui::Context, th: &Theme, w: f32, labels: &[&str]) -> Vec<egui::Rect> {
    let mut rects = vec![];
    let input = RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, 600.0))),
        ..Default::default()
    };
    let _ = ctx.run(input, |c| {
        egui::CentralPanel::default().show(c, |ui| {
            ui.horizontal_wrapped(|ui| {
                for l in labels {
                    rects.push(Button::secondary(l).show(ui, th).rect);
                }
            });
        });
    });
    rects
}

#[test]
fn a_button_short_of_room_starts_the_next_line_unsqueezed() {
    let (ctx, th) = ready(Mode::Light, Density::Comfortable);
    let labels = [fx("kt_one"), fx("kt_two"), fx("kt_sig")];
    let wide = wrap_row(&ctx, &th, 2000.0, &labels);
    assert!(wide.iter().all(|r| (r.top() - wide[0].top()).abs() < 0.5), "{wide:?}");
    // the room holds the first two and part of the third
    let w = wide[1].right() + wide[2].width() / 2.0 + 16.0;
    let narrow = wrap_row(&ctx, &th, w, &labels);
    for (n, f) in narrow.iter().zip(&wide) {
        assert!((n.width() - f.width()).abs() < 0.5, "squeezed: {narrow:?} vs {wide:?}");
    }
    assert!((narrow[1].top() - narrow[0].top()).abs() < 0.5, "{narrow:?}");
    assert!(narrow[2].top() >= narrow[0].bottom(), "the third is on the next line: {narrow:?}");
    assert!((narrow[2].left() - narrow[0].left()).abs() < 0.5, "{narrow:?}");
}

#[test]
fn a_label_longer_than_the_line_wraps_instead_of_an_ellipsis() {
    let (ctx, th) = ready(Mode::Light, Density::Comfortable);
    let long = fx("kt_long");
    let wide = wrap_row(&ctx, &th, 2000.0, &[fx("kt_yes"), long]);
    let narrow = wrap_row(&ctx, &th, 220.0, &[fx("kt_yes"), long]);
    assert!(narrow[1].width() < wide[1].width(), "fits the line: {narrow:?}");
    assert!(narrow[1].right() <= 220.0, "inside the screen: {narrow:?}");
    // more lines, so a taller button than the one-line one
    assert!(narrow[1].height() > wide[1].height() + 1.0, "{narrow:?} vs {wide:?}");
    // the painted galleys hold every word: nothing was cut to an ellipsis
    let input = RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(220.0, 600.0))),
        ..Default::default()
    };
    let out = ctx.run(input, |c| {
        egui::CentralPanel::default().show(c, |ui| {
            ui.horizontal_wrapped(|ui| Button::secondary(long).show(ui, &th));
        });
    });
    let text: String = out
        .shapes
        .iter()
        .filter_map(|cs| match &cs.shape {
            egui::Shape::Text(t) => Some(t.galley.text().to_owned()),
            _ => None,
        })
        .collect();
    assert!(text.contains(long), "{text:?}");
    assert!(!text.contains('\u{2026}'), "{text:?}");
}

/// An icon button with a tip is a button named by the tip title (keys left out), plain,
/// danger and glyph alike; a disabled one stays in the tree as a disabled button.
#[test]
fn icon_buttons_are_named_buttons() {
    let (ctx, th) = ready(Mode::Light, Density::Compact);
    ctx.enable_accesskit();
    let mut icons = crate::icons::IconCache::default();
    let mut out = None;
    for _ in 0..2 {
        out = Some(ctx.run(Default::default(), |c| {
            egui::CentralPanel::default().show(c, |ui| {
                icon_button(ui, &mut icons, &th, "close", false, "Close  \u{b7}  Esc");
                icon_button_danger(ui, &mut icons, &th, "close", "Remove");
                glyph_button(ui, "tile", false, "Split");
            });
        }));
    }
    let nodes = out.unwrap().platform_output.accesskit_update.expect("tree").nodes;
    for name in ["Close", "Remove", "Split"] {
        let n = nodes.iter().find(|(_, n)| n.label() == Some(name));
        let n = n.unwrap_or_else(|| panic!("no node named {name}")).1.clone();
        assert_eq!(n.role(), egui::accesskit::Role::Button, "{name}");
        assert_eq!(n.is_disabled(), name == "Split", "{name}");
    }
}
