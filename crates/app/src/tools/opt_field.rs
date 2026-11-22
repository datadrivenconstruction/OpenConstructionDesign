// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Fields of the tool option bars on the design kit: length in project units, angle, percent,
//! count, toggle, dropdown and a quiet button. Values stay in metres / degrees in the model; the
//! parse, clamp and wrap rules are plain functions without UI state.
use crate::units::{edit_length, edit_text_canonical, parse_signed_len, ui_decimal_mark, DecimalMark, InputError, Units};
use ddc_ui::theme::{control, space, Role};
use ddc_ui::widgets::{self, field::NumberFormat, field::TextField, Button};
use ddc_ui::Theme;

/// Dropdown width for short lists (shape, justification).
pub const NARROW_W: f32 = 112.0;
/// Rotation and direction bounds, degrees.
pub const FULL_TURN: (f64, f64) = (-360.0, 360.0);

/// Why a typed length was not taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    NotLength,
    /// English `1,5`: ambiguous, so refused with a hint to type a point.
    CommaDecimal,
}

impl Reject {
    pub fn key(self) -> &'static str {
        match self {
            Self::NotLength => "length_input.opt.bad_length",
            Self::CommaDecimal => "decimal_input.comma_decimal",
        }
    }
}

/// Number for the edit box in project units: mm without unit, ft-in with its own marks.
pub fn to_display(m: f64, u: Units) -> String {
    edit_length(m, u)
}

/// Unit text shown muted beside the field; imperial values carry their own marks.
pub fn unit_text(u: Units, ru: bool) -> String {
    match u {
        Units::Metric => crate::tr_lang(ru, "unit_fmt.unit.mm", &[]),
        Units::Imperial => String::new(),
    }
}

/// Value with its unit, as a read-only label shows it: «200 mm».
#[allow(dead_code)]
pub fn display_with_unit(m: f64, u: Units, ru: bool) -> String {
    let n = to_display(m, u);
    match unit_text(u, ru) {
        s if s.is_empty() => n,
        s => format!("{n} {s}"),
    }
}

/// Typed text in project units to metres, clamped to `range` (metres), in the UI locale.
pub fn from_display(text: &str, u: Units, range: (f64, f64)) -> Result<f64, Reject> {
    from_display_mark(text, u, range, ui_decimal_mark())
}

/// [`from_display`] in the locale of `mark` (shared rules of `units::parse_locale_number`):
/// group separators, the decimal mark and a unit suffix are read; anything else is refused.
pub fn from_display_mark(text: &str, u: Units, range: (f64, f64), mark: DecimalMark) -> Result<f64, Reject> {
    match parse_signed_edit(text, u, mark) {
        Ok(v) if v.is_finite() => Ok(v.clamp(range.0, range.1)),
        Ok(_) => Err(Reject::NotLength),
        Err(e) => Err(e),
    }
}

/// Edit-box text to a signed length in metres: option fields hold offsets, so zero and a leading
/// minus are valid here (the plain length parser takes positive values only).
fn parse_signed_edit(text: &str, u: Units, mark: DecimalMark) -> Result<f64, Reject> {
    let s = text.trim();
    let (neg, rest) = match s.strip_prefix(['-', '\u{2212}']) {
        Some(r) => (true, r.trim()),
        None => (false, s),
    };
    if rest.is_empty() || rest.starts_with(['-', '+', '\u{2212}']) {
        return Err(Reject::NotLength);
    }
    let why = |e: InputError| match e {
        InputError::CommaDecimal => Reject::CommaDecimal,
        _ => Reject::NotLength,
    };
    let t = edit_text_canonical(rest, mark).map_err(why)?;
    let v = parse_signed_len(&t, u).map_err(why)?;
    Ok(if neg { -v } else { v })
}

/// Value after committing `text`: the last valid one stays when the text is refused.
pub fn commit_len(last: f64, text: &str, u: Units, range: (f64, f64)) -> (f64, Option<Reject>) {
    match from_display(text, u, range) {
        Ok(v) => (v, None),
        Err(r) => (last, Some(r)),
    }
}

/// Angle inside `range`: a range of a full turn or more wraps into (-180, 180] first and keeps
/// values already inside; a narrower range clamps. Not a number gives `None`.
pub fn norm_angle(deg: f64, range: (f64, f64)) -> Option<f64> {
    if !deg.is_finite() {
        return None;
    }
    if range.1 - range.0 >= 360.0 {
        if deg >= range.0 && deg <= range.1 {
            return Some(deg);
        }
        let w = 180.0 - (180.0 - deg).rem_euclid(360.0);
        return Some(w.clamp(range.0, range.1));
    }
    Some(deg.clamp(range.0, range.1))
}

/// Nearest whole count inside the bounds; non-numbers give the minimum.
pub fn clamp_count_in(n: f64, lo: usize, hi: usize) -> usize {
    if !n.is_finite() {
        return lo;
    }
    (n.round().max(lo as f64) as usize).min(hi)
}

fn nf(ru: bool) -> NumberFormat {
    if ru {
        NumberFormat::RU
    } else {
        NumberFormat::EN
    }
}

fn field_w() -> f32 {
    control::FIELD_MIN_W + space::XXL
}

fn theme(ui: &egui::Ui) -> std::sync::Arc<Theme> {
    ddc_ui::theme::of(ui.ctx())
}

/// The caption in its own font on the line of the field text beside it: egui centres boxes, and
/// the box of the smaller caption font reads off the field's line.
fn caption(ui: &mut egui::Ui, th: &Theme, text: &str) -> egui::Response {
    let col = th.sem().text_secondary;
    let g = widgets::elide(ui, text, th.font(Role::Label), col, ui.available_width());
    let line = ui.painter().layout_no_wrap("x".into(), th.font(Role::Body), col);
    let off = |g: &egui::Galley| crate::ui_kit::x_mid(g) - g.size().y / 2.0;
    let dy = off(&line) - off(&g);
    let cut = g.elided;
    let (r, resp) = ui.allocate_exact_size(g.size(), egui::Sense::hover());
    ui.painter().galley(r.min + egui::vec2(0.0, dy), g, col);
    if cut {
        resp.on_hover_text(text)
    } else {
        resp
    }
}

/// Length in metres, shown and typed in project units. Enter or leaving the field commits; text
/// that is not a length keeps the last valid value and shows the field's error state with the
/// reason on hover. Returns true when the value changed.
pub fn len(ui: &mut egui::Ui, label: &str, v: &mut f64, range: (f64, f64), u: Units) -> bool {
    let th = theme(ui);
    let th: &Theme = &th;
    let ru = crate::ui_is_ru();
    let lr = caption(ui, th, label);
    let buf_id = ui.id().with(("ux50b.len.buf", label));
    let stored = ui.data(|d| d.get_temp::<String>(buf_id));
    let mut text = stored
        .clone()
        .unwrap_or_else(|| to_display((*v).clamp(range.0, range.1), u));
    let bad = stored
        .as_deref()
        .and_then(|s| from_display(s, u, range).err());
    let reason = crate::tr_lang(ru, bad.unwrap_or(Reject::NotLength).key(), &[]);
    let unit = unit_text(u, ru);
    let out = TextField::new(&mut text)
        .id_salt(("ux50b.len", label))
        .labelled_by(lr.id, label)
        .error_text(&reason)
        .error(bad.is_some())
        .unit(&unit)
        .right_aligned()
        .width(field_w())
        .show(ui, th);
    let mut changed = false;
    if out.committed && !out.cancelled {
        let (next, _) = commit_len(*v, &text, u, range);
        changed = next != *v;
        *v = next;
    }
    if bad.is_some() {
        let _ = out.response.clone().on_hover_text(reason.as_str());
    }
    if out.response.has_focus() {
        ui.data_mut(|d| d.insert_temp(buf_id, text));
    } else {
        ui.data_mut(|d| d.remove_temp::<String>(buf_id));
    }
    changed
}

fn number(
    ui: &mut egui::Ui,
    label: &str,
    v: &mut f64,
    decimals: usize,
    range: Option<(f64, f64)>,
    unit: &str,
    err: &str,
) -> bool {
    let th = theme(ui);
    let th: &Theme = &th;
    let lr = caption(ui, th, label);
    let before = *v;
    let mut f = widgets::NumberField::new(("ux50b.num", label), v, nf(crate::ui_is_ru()))
        .labelled_by(lr.id, label)
        .error_text(err)
        .decimals(decimals)
        .unit(unit)
        .width(field_w());
    if let Some((lo, hi)) = range {
        f = f.range(lo, hi);
    }
    f.show(ui, th);
    *v != before
}

/// Angle in degrees; values outside `range` wrap or clamp by `norm_angle`.
pub fn angle(ui: &mut egui::Ui, label: &str, v: &mut f64, range: (f64, f64)) -> bool {
    let err = crate::tr_lang(crate::ui_is_ru(), "number_input.bad_number", &[]);
    let before = *v;
    number(ui, label, v, 1, None, "°", &err);
    *v = norm_angle(*v, range).unwrap_or(before);
    *v != before
}

/// Percent (slope), clamped to `range`.
pub fn percent(ui: &mut egui::Ui, label: &str, v: &mut f64, range: (f64, f64)) -> bool {
    let err = crate::tr_lang(crate::ui_is_ru(), "number_input.bad_number", &[]);
    number(ui, label, v, 1, Some(range), "%", &err)
}

/// Whole number inside `range`.
pub fn count(ui: &mut egui::Ui, label: &str, v: &mut usize, range: (usize, usize)) -> bool {
    let ru = crate::ui_is_ru();
    let (lo, hi) = (range.0.to_string(), range.1.to_string());
    let err = crate::tr_lang(ru, "offset_opts.count_bad", &[("min", &lo), ("max", &hi)]);
    let mut n = *v as f64;
    number(
        ui,
        label,
        &mut n,
        0,
        Some((range.0 as f64, range.1 as f64)),
        "",
        &err,
    );
    let next = clamp_count_in(n, range.0, range.1);
    let changed = next != *v;
    *v = next;
    changed
}

/// Checkbox of the kit.
pub fn toggle(ui: &mut egui::Ui, label: &str, v: &mut bool) -> bool {
    let th = theme(ui);
    widgets::checkbox(ui, &th, v, label).changed()
}

/// Dropdown over `items`; `narrow` for short lists. `changed()` on the response when the
/// choice moved.
pub fn pick(
    ui: &mut egui::Ui,
    salt: &str,
    sel: &mut usize,
    items: &[String],
    narrow: bool,
) -> egui::Response {
    let th = theme(ui);
    let refs: Vec<&str> = items.iter().map(String::as_str).collect();
    let w = if narrow { Some(NARROW_W) } else { None };
    // a closed list keeps its width: in a wrapping row it starts a new line instead of running past the edge
    if ui.available_width() < w.unwrap_or(control::DROPDOWN_W) {
        ui.end_row();
    }
    widgets::field::dropdown(ui, &th, ("ux50b.pick", salt), sel, &refs, w)
}

/// Dropdown whose choice is a small index (`u8` option fields).
pub fn pick_u8(
    ui: &mut egui::Ui,
    salt: &str,
    sel: &mut u8,
    items: &[String],
    narrow: bool,
) -> egui::Response {
    let mut i = (*sel as usize).min(items.len().saturating_sub(1));
    let r = pick(ui, salt, &mut i, items, narrow);
    *sel = i as u8;
    r
}

/// Quiet button with a tooltip; true when clicked.
pub fn button(ui: &mut egui::Ui, label: &str, tip: &str) -> bool {
    let th = theme(ui);
    Button::secondary(label)
        .small()
        .tooltip(tip, "")
        .show(ui, &th)
        .clicked()
}

/// Entry for bars that still pass a unit suffix: degrees and percent go to their fields, every
/// other suffix is a length in the project units `u` (passed in, never remembered).
pub fn legacy_num(
    ui: &mut egui::Ui,
    label: &str,
    v: &mut f64,
    suffix: &str,
    range: (f64, f64),
    u: Units,
) {
    match suffix.trim() {
        "°" => {
            angle(ui, label, v, range);
        }
        "%" => {
            percent(ui, label, v, range);
        }
        _ => {
            len(ui, label, v, range, u);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::{Units::Imperial, Units::Metric, INCH};
    use ddc_ui::widgets::field::{format_number, parse_number};

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    const R: (f64, f64) = (-10.0, 50.0);

    #[test]
    fn metres_show_as_millimetres() {
        assert_eq!(to_display(0.2, Metric), "200");
        assert_eq!(display_with_unit(0.2, Metric, false), "200 mm");
        let ru = display_with_unit(0.2, Metric, true);
        assert!(ru.starts_with("200 "), "{ru}");
        assert_ne!(ru, display_with_unit(0.2, Metric, false), "unit is translated");
        assert_eq!(unit_text(Imperial, false), "");
    }

    #[test]
    fn length_round_trips_per_unit_setting() {
        for m in [0.0, 0.2, 3.0, -1.25, 12.5] {
            close(from_display(&to_display(m, Metric), Metric, R).unwrap(), m);
        }
        let m = 66.5 * INCH;
        close(from_display(&to_display(m, Imperial), Imperial, R).unwrap(), m);
        close(
            from_display(&display_with_unit(0.2, Metric, true), Metric, R).unwrap(),
            0.2,
        );
    }

    const RU: DecimalMark = DecimalMark::Comma;
    const EN: DecimalMark = DecimalMark::Point;

    #[test]
    fn length_reads_decimal_comma_point_and_unit_suffix() {
        let ru = |s: &str| from_display_mark(s, Metric, R, RU).unwrap();
        close(ru("250"), 0.25);
        close(ru("1,5"), 0.0015);
        close(ru("1.5"), 0.0015);
        close(ru("1,5 m"), 1.5);
        close(ru("1.5m"), 1.5);
        close(ru("200 mm"), 0.2);
        close(ru("200mm"), 0.2);
        close(ru("3 000"), 3.0);
        close(
            from_display_mark("5'-6 1/2\"", Imperial, R, RU).unwrap(),
            66.5 * INCH,
        );
        close(
            from_display_mark("5'-6 1/2\"", Imperial, R, EN).unwrap(),
            66.5 * INCH,
        );
    }

    /// English `1,000` mm is a thousand millimetres, never 1 mm; `1,5` is refused.
    #[test]
    fn english_comma_groups_thousands_in_mm_fields() {
        let w = (-100.0, 100.0);
        let en = |s: &str| from_display_mark(s, Metric, w, EN);
        let ru = |s: &str| from_display_mark(s, Metric, w, RU);
        close(en("1,000").unwrap(), 1.0);
        close(en("12,500").unwrap(), 12.5);
        close(en("1,000.5").unwrap(), 1.0005);
        close(en("1.000").unwrap(), 0.001);
        close(en("1 000").unwrap(), 1.0);
        close(en("-1,000 mm").unwrap(), -1.0);
        close(en("1.5").unwrap(), 0.0015);
        close(en("-0.5").unwrap(), -0.0005);
        close(en("1e3").unwrap(), 1.0);
        for s in ["1,5", "-0,5", "1 000,5", "1,5 m"] {
            assert_eq!(en(s), Err(Reject::CommaDecimal), "{s}");
        }
        close(ru("1,000").unwrap(), 0.001);
        close(ru("12,500").unwrap(), 0.0125);
        close(ru("1 000,5").unwrap(), 1.0005);
        close(ru("1\u{a0}000,5").unwrap(), 1.0005);
        close(ru("1.000").unwrap(), 0.001);
        close(ru("-0,5").unwrap(), -0.0005);
        close(ru("1e3").unwrap(), 1.0);
        for m in [EN, RU] {
            for s in ["", "abc", "1 5", "1,23,456", "--5"] {
                assert_eq!(from_display_mark(s, Metric, w, m), Err(Reject::NotLength), "{s}");
            }
        }
        for ru in [false, true] {
            let s = crate::tr_lang(ru, Reject::CommaDecimal.key(), &[]);
            assert_ne!(s, Reject::CommaDecimal.key(), "hint has a translation");
        }
    }

    #[test]
    fn shown_text_reads_back_in_its_own_locale() {
        let w = (-100.0, 100.0);
        for m in [EN, RU] {
            for v in [0.0125, 1.25, -12.5, 0.0005, 3.0] {
                let shown = crate::units::edit_length_mark(v, Metric, m);
                close(from_display_mark(&shown, Metric, w, m).unwrap(), v);
            }
        }
        assert_eq!(crate::units::edit_length_mark(1.2505, Metric, EN), "1,250.5");
        assert_eq!(crate::units::edit_length_mark(1.2505, Metric, RU), "1\u{a0}250,5");
    }

    #[test]
    fn offsets_may_be_zero_or_negative() {
        close(from_display("0", Metric, R).unwrap(), 0.0);
        close(from_display("-200", Metric, R).unwrap(), -0.2);
        close(from_display("-200 mm", Metric, R).unwrap(), -0.2);
        close(from_display("-1,5 m", Metric, R).unwrap(), -1.5);
        close(from_display("-6'-0\"", Imperial, R).unwrap(), -6.0 * 0.3048);
        // a negative ft-in value the field shows reads back as itself (the inner '-' splits
        // feet from inches, only the leading one is the sign)
        for m in [-66.5 * INCH, -0.5 * INCH, -1.25] {
            let shown = to_display(m, Imperial);
            assert!(shown.starts_with('-'), "{shown}");
            let back = from_display(&shown, Imperial, R).unwrap();
            assert!(back < 0.0 && (back - m).abs() <= INCH / 2.0, "{shown} -> {back}");
        }
        close(from_display("\u{2212}0'-6\"", Imperial, R).unwrap(), -6.0 * INCH);
        assert!(from_display("--5", Metric, R).is_err());
        assert!(from_display("-", Metric, R).is_err());
    }

    #[test]
    fn length_is_clamped_in_metres() {
        close(from_display("99999", Metric, (0.0, 10.0)).unwrap(), 10.0);
        close(from_display("-5000", Metric, (-2.0, 2.0)).unwrap(), -2.0);
        close(from_display("1", Metric, (0.5, 2.0)).unwrap(), 0.5);
    }

    #[test]
    fn unreadable_length_keeps_the_last_valid_value_with_a_reason() {
        for bad in ["", "abc", "12 parsecs", "--"] {
            let (v, why) = commit_len(0.3, bad, Metric, R);
            close(v, 0.3);
            assert_eq!(why, Some(Reject::NotLength), "{bad:?}");
        }
        let (v, why) = commit_len(0.3, "400", Metric, R);
        close(v, 0.4);
        assert_eq!(why, None);
        for ru in [false, true] {
            let s = crate::tr_lang(ru, Reject::NotLength.key(), &[]);
            assert_ne!(s, Reject::NotLength.key(), "reason has a translation");
        }
    }

    #[test]
    fn angle_wraps_inside_a_full_turn_and_clamps_in_a_narrow_range() {
        assert_eq!(norm_angle(40.0, FULL_TURN), Some(40.0));
        assert_eq!(norm_angle(270.0, FULL_TURN), Some(270.0));
        assert_eq!(norm_angle(400.0, FULL_TURN), Some(40.0));
        assert_eq!(norm_angle(-400.0, FULL_TURN), Some(-40.0));
        assert_eq!(norm_angle(720.0, FULL_TURN), Some(0.0));
        assert_eq!(norm_angle(80.0, (0.0, 75.0)), Some(75.0));
        assert_eq!(norm_angle(-5.0, (0.0, 75.0)), Some(0.0));
        assert_eq!(norm_angle(f64::NAN, FULL_TURN), None);
        assert_eq!(norm_angle(f64::INFINITY, (0.0, 75.0)), None);
    }

    #[test]
    fn count_stays_inside_its_bounds() {
        assert_eq!(clamp_count_in(1.2, 2, 200), 2);
        assert_eq!(clamp_count_in(7.6, 2, 200), 8);
        assert_eq!(clamp_count_in(500.0, 2, 200), 200);
        assert_eq!(clamp_count_in(-3.0, 3, 64), 3);
        assert_eq!(clamp_count_in(f64::NAN, 3, 64), 3);
    }

    #[test]
    fn angle_and_percent_use_the_language_decimal_separator() {
        assert_eq!(parse_number("12,5", NumberFormat::RU), Some(12.5));
        assert_eq!(parse_number("12.5", NumberFormat::EN), Some(12.5));
        assert_eq!(format_number(12.5, 1, NumberFormat::RU), "12,5");
        assert_eq!(format_number(12.5, 1, NumberFormat::EN), "12.5");
    }

    #[test]
    fn new_texts_exist_in_both_languages() {
        for k in ["number_input.bad_number", "number_input.at_grids_tip"] {
            for ru in [false, true] {
                assert_ne!(crate::tr_lang(ru, k, &[]), k, "{k} ru={ru}");
            }
        }
    }

    fn body(src: &str) -> &str {
        src.split("#[cfg(test)]").next().unwrap_or(src)
    }

    #[test]
    fn migrated_panels_use_the_shared_fields() {
        let files = [
            ("ui.rs", include_str!("ui.rs")),
            ("structx.rs", include_str!("structx.rs")),
            ("floor.rs", include_str!("floor.rs")),
            ("ramp.rs", include_str!("ramp.rs")),
        ];
        for (name, src) in files {
            let b = body(src);
            assert!(!b.contains("DragValue"), "{name}: raw DragValue");
            assert!(!b.contains("ComboBox"), "{name}: raw ComboBox");
            // the MEP duct toggle belongs to the MEP bar and moves with its bar
            let raw = b
                .lines()
                .filter(|l| l.contains(".checkbox(") && !l.contains("mep.round"))
                .count();
            assert_eq!(raw, 0, "{name}: raw checkbox");
            assert!(b.contains("opt_field"), "{name}: not on opt_field");
        }
    }
}
