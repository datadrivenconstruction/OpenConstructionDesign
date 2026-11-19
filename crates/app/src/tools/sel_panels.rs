// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Selection filter, "Tag all" and the distribute/number option bar on the design kit: category
//! rows with a tri-state header, count and length fields with their validation rules, free of any
//! panel state so the rules can be tested without a window.
use crate::editor::{Editor, Sel};
use crate::selection;
use crate::units::{DecimalMark, InputError, Units};
use ddc_ui::theme::{control, space, Role};
use ddc_ui::widgets::{self, field::NumberFormat, Button};
use ddc_ui::Theme;
use std::collections::HashSet;

/// Distribute count bounds, both inclusive.
pub const DIST_MIN: usize = 1;
pub const DIST_MAX: usize = 200;

/// A whole count inside the bounds; anything else is refused, not rounded or clamped.
pub fn check_dist_count(v: f64) -> Option<usize> {
    if !v.is_finite() {
        return None;
    }
    let r = v.round();
    if (v - r).abs() > 1e-9 || r < DIST_MIN as f64 || r > DIST_MAX as f64 {
        return None;
    }
    Some(r as usize)
}

/// State of the "all" header over the rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tri {
    None,
    Some,
    All,
}

pub fn tri(on: usize, total: usize) -> Tri {
    if total == 0 || on == 0 {
        Tri::None
    } else if on >= total {
        Tri::All
    } else {
        Tri::Some
    }
}

/// Catalog key of the reason the filter OK is off; `None` when it can run.
pub fn filter_ok_block(checked: usize) -> Option<&'static str> {
    (checked == 0).then_some("sel_filter.flt.none")
}

/// Same for "Tag all".
pub fn tag_ok_block(checked: usize) -> Option<&'static str> {
    (checked == 0).then_some("sel_filter.tg.none")
}

/// Keeps only the ticked categories. With nothing ticked the selection stays as it is: an empty
/// filter is not a request to drop everything. True when the selection was replaced.
pub fn apply_filter(ed: &mut Editor, keep: &HashSet<&str>) -> bool {
    if keep.is_empty() {
        return false;
    }
    let next: Vec<Sel> = selection::filter(ed, &ed.selection, keep);
    ed.selection = next;
    true
}

/// One category line.
pub struct CatRow {
    pub label: String,
    pub count: String,
    pub on: bool,
}

/// What the user did in the list this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowEvent {
    Toggle(usize, bool),
    Only(usize),
    SetAll(bool),
}

fn muted(ui: &mut egui::Ui, th: &Theme, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .font(th.font(Role::Body))
            .color(th.sem().text_secondary),
    );
}

/// Tri-state header, then one checkbox row per category with a muted right-aligned count and an
/// "only this" quiet link while the row is hovered or focused.
pub fn cat_list(
    ui: &mut egui::Ui,
    th: &Theme,
    salt: &str,
    all_label: &str,
    col_title: &str,
    only_label: &str,
    rows: &[CatRow],
) -> Option<RowEvent> {
    let mut ev = None;
    let on = rows.iter().filter(|r| r.on).count();
    let state = tri(on, rows.len());
    ui.horizontal(|ui| {
        let mut v = state == Tri::All;
        if widgets::checkbox_mixed(ui, th, &mut v, state == Tri::Some, all_label).changed() {
            ev = Some(RowEvent::SetAll(v));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            muted(ui, th, col_title);
        });
    });
    ui.separator();
    for (i, r) in rows.iter().enumerate() {
        let rid = egui::Id::new(("ux55e.row", salt, i));
        let prev = ui.data(|d| d.get_temp::<egui::Rect>(rid));
        let hover = prev.is_some_and(|p| ui.rect_contains_pointer(p));
        let line = ui.horizontal(|ui| {
            let mut v = r.on;
            let cb = widgets::checkbox(ui, th, &mut v, &r.label);
            if cb.changed() {
                ev = Some(RowEvent::Toggle(i, v));
            }
            let focus = cb.has_focus();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                muted(ui, th, &r.count);
                if (hover || focus) && Button::quiet(only_label).small().show(ui, th).clicked() {
                    ev = Some(RowEvent::Only(i));
                }
            });
        });
        ui.data_mut(|d| d.insert_temp(rid, line.response.rect));
    }
    ev
}

/// Quiet one-line reason under a list, for the disabled primary action.
pub fn note(ui: &mut egui::Ui, th: &Theme, text: &str) {
    ui.add_space(space::XS);
    widgets::elided_label(ui, text, th.font(Role::Caption), th.sem().text_secondary);
}

fn field_w() -> f32 {
    control::FIELD_MIN_W + space::XXL
}

pub fn caption(ui: &mut egui::Ui, th: &Theme, text: &str) -> egui::Response {
    widgets::elided_label(ui, text, th.font(Role::Label), th.sem().text_secondary)
}

fn bad_count_id() -> egui::Id {
    egui::Id::new("ux55e.dist.count_bad")
}

fn set_bad_count(ui: &egui::Ui, bad: bool) {
    ui.data_mut(|d| {
        if bad {
            d.insert_temp(bad_count_id(), true);
        } else {
            d.remove::<bool>(bad_count_id());
        }
    });
}

/// Reason line of a refused count, shown under the bar until the next valid edit.
pub fn count_reject_line(ui: &mut egui::Ui, th: &Theme, ru: bool) -> bool {
    if ui.data(|d| d.get_temp::<bool>(bad_count_id())) != Some(true) {
        return false;
    }
    let (lo, hi) = (DIST_MIN.to_string(), DIST_MAX.to_string());
    let text = crate::tr_lang(ru, "offset_opts.count_bad", &[("min", &lo), ("max", &hi)]);
    widgets::elided_label(ui, &text, th.font(Role::Caption), th.sem().error);
    true
}

/// Count field of the distribute bar: 1..=200, a value outside is refused and the old count stays.
/// True when the count changed.
pub fn count_field(ui: &mut egui::Ui, th: &Theme, ru: bool, n: &mut usize) -> bool {
    let label = crate::tr_lang(ru, "dt.count", &[]);
    let (lo, hi) = (DIST_MIN.to_string(), DIST_MAX.to_string());
    let err = crate::tr_lang(ru, "offset_opts.count_bad", &[("min", &lo), ("max", &hi)]);
    let lr = caption(ui, th, &label);
    let mut v = *n as f64;
    let nf = if ru { NumberFormat::RU } else { NumberFormat::EN };
    let out = widgets::NumberField::new("ux55e.dist.count", &mut v, nf)
        .labelled_by(lr.id, &label)
        .error_text(&err)
        .decimals(0)
        .step(1.0)
        .width(field_w())
        .show(ui, th);
    let mut changed = false;
    if out.committed {
        match check_dist_count(v) {
            Some(c) => {
                changed = c != *n;
                *n = c;
                set_bad_count(ui, false);
            }
            None => set_bad_count(ui, true),
        }
    } else if out.cancelled {
        set_bad_count(ui, false);
    }
    changed
}

/// Typed length in metres, `0..=max`. Zero is a valid margin; `allow_zero == false` refuses it.
pub fn commit_len(
    text: &str,
    u: Units,
    mark: DecimalMark,
    max: f64,
    allow_zero: bool,
) -> Result<f64, InputError> {
    match crate::ui_kit::len_commit(text, u, 0.0, max, mark) {
        Err(InputError::NonPositive) if allow_zero => {
            let core = text
                .trim()
                .trim_end_matches(|c: char| c.is_alphabetic() || c == '\'' || c == '"')
                .trim();
            let zero = !core.is_empty()
                && core.chars().any(|c| c == '0')
                && core.chars().all(|c| matches!(c, '0' | '.' | ',' | '\u{a0}' | ' '));
            if zero {
                Ok(0.0)
            } else {
                Err(InputError::NonPositive)
            }
        }
        other => other,
    }
}

/// Length field in project units with a muted unit; refused text keeps the last valid value and
/// shows the reason on the field. True when the value changed.
#[allow(clippy::too_many_arguments)]
pub fn len_field(
    ui: &mut egui::Ui,
    th: &Theme,
    ru: bool,
    u: Units,
    salt: &str,
    label: &str,
    v: &mut f64,
    max: f64,
    allow_zero: bool,
) -> bool {
    let mark = DecimalMark::for_ru(ru);
    let unit = match u {
        Units::Metric => crate::tr_lang(ru, "unit_fmt.unit.mm", &[]),
        Units::Imperial => String::new(),
    };
    let lr = caption(ui, th, label);
    let buf_id = ui.id().with(("ux55e.len.buf", salt));
    let stored = ui.data(|d| d.get_temp::<String>(buf_id));
    let mut text = stored
        .clone()
        .unwrap_or_else(|| crate::ui_kit::len_text(*v, u, mark));
    let bad = stored
        .as_deref()
        .and_then(|s| commit_len(s, u, mark, max, allow_zero).err());
    let err = bad.as_ref().map(|e| e.text(ru)).unwrap_or_default();
    let out = widgets::TextField::new(&mut text)
        .id_salt(("ux55e.len", salt))
        .labelled_by(lr.id, label)
        .error_text(&err)
        .error(bad.is_some())
        .unit(&unit)
        .right_aligned()
        .width(field_w())
        .show(ui, th);
    let mut changed = false;
    // an untouched field writes nothing: the shown text is rounded to the display precision
    if out.committed && !out.cancelled && text.trim() != crate::ui_kit::len_text(*v, u, mark) {
        if let Ok(m) = commit_len(&text, u, mark, max, allow_zero) {
            changed = (m - *v).abs() > 1e-12;
            *v = m;
        }
    }
    if out.response.has_focus() {
        ui.data_mut(|d| d.insert_temp(buf_id, text));
    } else {
        ui.data_mut(|d| d.remove_temp::<String>(buf_id));
    }
    changed
}

/// Start-number field of the numbering tool.
pub fn start_field(ui: &mut egui::Ui, th: &Theme, label: &str, next: &mut String) {
    let lr = caption(ui, th, label);
    widgets::TextField::new(next)
        .id_salt("ux55e.num.start")
        .labelled_by(lr.id, label)
        .width(field_w())
        .show(ui, th);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modify_tools::tests::walls;
    use crate::units::Units::{Imperial, Metric};

    const SRC: &str = include_str!("../modify_tools.rs");

    fn panel_fns() -> &'static str {
        let a = SRC.find("fn filter_window(").expect("filter_window");
        let b = a + SRC[a..].find("\nfn chip(").expect("chip");
        &SRC[a..b]
    }

    #[test]
    fn filter_ok_needs_a_category() {
        assert_eq!(filter_ok_block(0), Some("sel_filter.flt.none"));
        assert_eq!(filter_ok_block(1), None);
        assert_eq!(tag_ok_block(0), Some("sel_filter.tg.none"));
        assert_eq!(tag_ok_block(3), None);
        for ru in [false, true] {
            for k in ["sel_filter.flt.none", "sel_filter.tg.none", "sel_filter.flt.all_cats", "sel_filter.flt.in_sel"] {
                assert_ne!(crate::tr_lang(ru, k, &[]), k, "{k} missing");
            }
        }
        assert_ne!(
            crate::tr_lang(false, "sel_filter.flt.none", &[]),
            crate::tr_lang(true, "sel_filter.flt.none", &[])
        );
    }

    #[test]
    fn ok_with_nothing_ticked_keeps_the_selection() {
        let mut ed = Editor::new_in_memory().unwrap();
        walls(&mut ed, &[((0.0, 0.0), (4.0, 0.0)), ((0.0, 2.0), (4.0, 2.0))]);
        let sel = vec![Sel::Wall(ed.walls[0].id), Sel::Wall(ed.walls[1].id), Sel::Room(7)];
        ed.selection = sel.clone();
        assert!(!apply_filter(&mut ed, &HashSet::new()));
        assert_eq!(ed.selection, sel);
    }

    #[test]
    fn ticked_categories_filter_as_before() {
        let mut ed = Editor::new_in_memory().unwrap();
        walls(&mut ed, &[((0.0, 0.0), (4.0, 0.0)), ((0.0, 2.0), (4.0, 2.0))]);
        let (w0, w1) = (Sel::Wall(ed.walls[0].id), Sel::Wall(ed.walls[1].id));
        ed.selection = vec![w0, w1, Sel::Room(7)];
        assert!(apply_filter(&mut ed, &HashSet::from(["wall"])));
        assert_eq!(ed.selection, vec![w0, w1]);
        ed.selection = vec![w0, w1, Sel::Room(7)];
        assert!(apply_filter(&mut ed, &HashSet::from(["room"])));
        assert_eq!(ed.selection, vec![Sel::Room(7)]);
        ed.selection = vec![w0, w1, Sel::Room(7)];
        assert!(apply_filter(&mut ed, &HashSet::from(["wall", "room"])));
        assert_eq!(ed.selection.len(), 3);
    }

    #[test]
    fn header_state_follows_the_rows() {
        assert_eq!(tri(0, 3), Tri::None);
        assert_eq!(tri(2, 3), Tri::Some);
        assert_eq!(tri(3, 3), Tri::All);
        assert_eq!(tri(0, 0), Tri::None);
    }

    #[test]
    fn count_field_refuses_0_and_201() {
        assert_eq!(check_dist_count(0.0), None);
        assert_eq!(check_dist_count(201.0), None);
        assert_eq!(check_dist_count(-3.0), None);
        assert_eq!(check_dist_count(2.5), None);
        assert_eq!(check_dist_count(f64::NAN), None);
        assert_eq!(check_dist_count(1.0), Some(1));
        assert_eq!(check_dist_count(200.0), Some(200));
        assert_eq!(check_dist_count(12.0), Some(12));
    }

    #[test]
    fn margin_accepts_zero_step_does_not() {
        let en = DecimalMark::for_ru(false);
        let ru = DecimalMark::for_ru(true);
        assert_eq!(commit_len("0", Metric, en, 50.0, true).unwrap(), 0.0);
        assert_eq!(commit_len("0,0", Metric, ru, 50.0, true).unwrap(), 0.0);
        assert!(commit_len("0", Metric, en, 50.0, false).is_err());
        assert!(commit_len("-5", Metric, en, 50.0, true).is_err());
        assert!(commit_len("abc", Metric, en, 50.0, true).is_err());
        assert!((commit_len("250", Metric, en, 50.0, true).unwrap() - 0.25).abs() < 1e-9);
        assert!((commit_len("1,5", Metric, ru, 50.0, false).unwrap() - 0.0015).abs() < 1e-9);
    }

    fn run_ui(f: &mut dyn FnMut(&mut egui::Ui, &Theme)) {
        let ctx = ddc_ui::theme::headless_ctx();
        for _ in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 700.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(input, |c| {
                let th = ddc_ui::theme::of(c);
                egui::CentralPanel::default().show(c, |ui| f(ui, &th));
            });
        }
    }

    #[test]
    fn category_list_and_fields_render_headless() {
        let rows: Vec<CatRow> = (0..4)
            .map(|i| CatRow {
                label: format!("Category {i}"),
                count: (i * 3).to_string(),
                on: i % 2 == 0,
            })
            .collect();
        let mut n = 5usize;
        let (mut margin, mut step) = (0.1, 0.5);
        let mut next = String::from("D1");
        run_ui(&mut |ui, th| {
            let ev = cat_list(ui, th, "t", "All", "Count", "Only", &rows);
            assert_eq!(ev, None);
            assert!(!count_field(ui, th, false, &mut n));
            assert!(!len_field(ui, th, true, Metric, "m", "Margin", &mut margin, 50.0, true));
            assert!(!len_field(ui, th, false, Imperial, "s", "Step", &mut step, 50.0, false));
            start_field(ui, th, "Start", &mut next);
            assert!(!count_reject_line(ui, th, false));
        });
        assert_eq!((n, next.as_str()), (5, "D1"));
        assert!((margin - 0.1).abs() < 1e-12 && (step - 0.5).abs() < 1e-12);
    }

    #[test]
    fn panels_use_kit_widgets_only() {
        let body = panel_fns();
        for raw in [
            "ui.checkbox(",
            "ui.small_button(",
            "ui.button(",
            "radio_value",
            "DragValue",
            "TextEdit",
            "egui::Grid",
            "Frame::popup",
            "Stroke::new",
            "light_widgets",
            "ui.monospace",
            "length_drag",
        ] {
            assert!(!body.contains(raw), "raw widget `{raw}` in the selection panels");
        }
        assert!(body.contains("sel_panels::cat_list"));
        assert!(body.contains("sel_panels::apply_filter"));
    }

    #[test]
    fn locale_files_carry_the_same_keys() {
        for name in ["en", "ru"] {
            let p = format!("{}/../ui/data/i18n/{name}/sel_filter.json", env!("CARGO_MANIFEST_DIR"));
            let txt = std::fs::read_to_string(&p).expect("utf-8 locale file");
            let v: std::collections::BTreeMap<String, String> =
                serde_json::from_str(&txt).expect("valid json");
            for k in ["sel_filter.flt.none", "sel_filter.tg.none", "sel_filter.flt.all_cats", "sel_filter.flt.in_sel"] {
                assert!(v.get(k).is_some_and(|s| !s.is_empty()), "{name}: {k}");
            }
        }
    }
}
