//! Option bar of the modify tools (Move, Copy, Rotate, Array, Offset) and the temporary isolate
//! banner, built from the design-kit widgets and tokens only. `opts_of` decides which options a
//! tool shows; `check_dist` / `clamp_count` are the validation rules, free of any UI state.
use crate::modify_tools::Op;
use crate::expr_field::Scope;
use crate::units::{parse_edit_length_lang, DecimalMark, InputError, Units};
use ddc_ui::theme::{control, space, Role};
use ddc_ui::widgets::{self, field::NumberFormat, field::TextField, Button};
use ddc_ui::Theme;

/// One option of the bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptKind {
    Copy,
    Constrain,
    Disjoin,
    Multiple,
    Radial,
    Count,
    ToLast,
    Dist,
}

impl OptKind {
    /// Catalog key of the option label.
    pub fn label_key(self) -> &'static str {
        match self {
            OptKind::Copy => "mt.copy",
            OptKind::Constrain => "mt.constrain",
            OptKind::Disjoin => "mt.disjoin",
            OptKind::Multiple => "mt.multiple",
            OptKind::Radial => "mt.radial",
            OptKind::Count => "mt.count",
            OptKind::ToLast => "mt.to_last",
            OptKind::Dist => "mt.dist",
        }
    }
}

/// Options a tool shows, in drawing order.
pub fn opts_of(op: Op) -> &'static [OptKind] {
    match op {
        Op::Move => &[OptKind::Copy, OptKind::Constrain, OptKind::Disjoin],
        Op::Copy => &[OptKind::Multiple, OptKind::Constrain],
        Op::Rotate | Op::Mirror | Op::MirrorDraw | Op::Scale => &[OptKind::Copy],
        Op::Array => &[
            OptKind::Radial,
            OptKind::Count,
            OptKind::ToLast,
            OptKind::Constrain,
        ],
        Op::Offset => &[OptKind::Dist],
        _ => &[],
    }
}

/// Array instances, the original included.
pub const COUNT_MIN: usize = 2;
pub const COUNT_MAX: usize = 200;

/// Nearest whole count inside the bounds; non-numbers give the minimum.
pub fn clamp_count(n: f64) -> usize {
    if !n.is_finite() {
        return COUNT_MIN;
    }
    (n.round().max(COUNT_MIN as f64) as usize).min(COUNT_MAX)
}

/// Decimal and group separators of the UI language, for an explicit language flag.
pub fn number_format(ru: bool) -> NumberFormat {
    if ru {
        NumberFormat::RU
    } else {
        NumberFormat::EN
    }
}

/// Why a typed offset distance was not taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    NotLength,
    NotPositive,
}

impl Reject {
    pub fn key(self) -> &'static str {
        match self {
            Reject::NotLength => "length_input.opt.bad_length",
            Reject::NotPositive => "offset_opts.dist_positive",
        }
    }
}

/// An offset distance the model takes: finite and above zero.
pub fn dist_ok(v: f64) -> bool {
    v > 0.0 && v.is_finite()
}

/// Typed distance in metres. The side of the offset comes from the cursor, so the distance
/// itself is a positive length: zero and negative values are refused.
pub fn check_dist(text: &str, u: Units) -> Result<f64, Reject> {
    check_dist_mark(text, u, crate::units::ui_decimal_mark())
}

/// [`check_dist`] with the decimal mark of the bar's language.
pub fn check_dist_mark(text: &str, u: Units, mark: DecimalMark) -> Result<f64, Reject> {
    let parse = |t: &str| parse_edit_length_lang(t, u, &Scope::none(), mark).ok();
    let v = match parse_edit_length_lang(text, u, &Scope::none(), mark) {
        Ok(v) => v,
        // a well-formed number that is zero or below: refused as a value, not as text
        Err(InputError::NonPositive) => return Err(Reject::NotPositive),
        Err(_) => {
            // a signed number the parser does not take is still a length, just not a valid one
            let rest = text.trim().strip_prefix('-').map(str::trim);
            return match rest.and_then(parse) {
                Some(_) => Err(Reject::NotPositive),
                None => Err(Reject::NotLength),
            };
        }
    };
    if dist_ok(v) {
        Ok(v)
    } else {
        Err(Reject::NotPositive)
    }
}

/// Value after committing `text`: the last valid one stays when the text is refused.
pub fn commit_dist(last: f64, text: &str, u: Units) -> (f64, Option<Reject>) {
    commit_dist_mark(last, text, u, crate::units::ui_decimal_mark())
}

/// [`commit_dist`] with the decimal mark of the bar's language.
pub fn commit_dist_mark(last: f64, text: &str, u: Units, mark: DecimalMark) -> (f64, Option<Reject>) {
    match check_dist_mark(text, u, mark) {
        Ok(v) => (v, None),
        Err(r) => (last, Some(r)),
    }
}

/// Texts of the temporary isolate banner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Banner {
    pub label: String,
    pub apply: String,
    pub apply_tip: String,
    pub exit_tip: String,
}

pub fn banner(isolate: bool, ru: bool) -> Banner {
    let tr = |k: &str| crate::tr_lang(ru, k, &[]);
    Banner {
        label: tr(if isolate { "iso.isolate" } else { "iso.hide" }),
        apply: tr("iso.apply"),
        apply_tip: tr("offset_opts.iso.apply_tip"),
        exit_tip: tr("offset_opts.iso.exit"),
    }
}

/// Leaves the temporary mode without applying it.
pub fn exit_banner(vis: &mut crate::isolate::Vis) {
    vis.reset();
}

/// Surface of the floating bars over the plan: same panel fill, border and radius as the shell.
pub fn bar_frame(th: &Theme) -> egui::Frame {
    egui::Frame::new()
        .fill(th.c("bg.panel"))
        .stroke(egui::Stroke::new(1.0, th.c("border.subtle")))
        .corner_radius(th.radius("md"))
        .inner_margin(egui::Margin::symmetric(space::SM as i8, space::XS as i8))
        .shadow(th.shadow(1))
}

/// Row spacing of a bar: one gap between controls, control height for every item.
pub fn row_style(ui: &mut egui::Ui, th: &Theme) {
    ui.spacing_mut().item_spacing.x = space::SM;
    ui.spacing_mut().interact_size.y = th.d().control;
}

fn field_w() -> f32 {
    control::FIELD_MIN_W + space::XXL
}

fn caption(ui: &mut egui::Ui, th: &Theme, text: &str) -> egui::Response {
    widgets::elided_label(ui, text, th.font(Role::Label), th.sem().text_secondary)
}

/// Remembered refusal of a field, shown under the bar until the next valid edit.
fn reject_id() -> egui::Id {
    egui::Id::new("ux48f.reject")
}

fn set_reject(ui: &egui::Ui, r: Option<Reject>) {
    let id = reject_id();
    ui.data_mut(|d| match r {
        Some(r) => d.insert_temp(id, r),
        None => {
            d.remove::<Reject>(id);
        }
    });
}

/// Reason line of the last refused edit; true when it was drawn.
pub fn reject_line(ui: &mut egui::Ui, th: &Theme, ru: bool) -> bool {
    let id = reject_id();
    let Some(r) = ui.data(|d| d.get_temp::<Reject>(id)) else {
        return false;
    };
    let text = crate::tr_lang(ru, r.key(), &[]);
    widgets::elided_label(ui, &text, th.font(Role::Caption), th.sem().error);
    true
}

/// Whole-number field with the array bounds; true when the count changed.
pub fn count_field(ui: &mut egui::Ui, th: &Theme, ru: bool, count: &mut usize) -> bool {
    let label = crate::tr_lang(ru, OptKind::Count.label_key(), &[]);
    let (lo, hi) = (COUNT_MIN.to_string(), COUNT_MAX.to_string());
    let err = crate::tr_lang(ru, "offset_opts.count_bad", &[("min", &lo), ("max", &hi)]);
    let lr = caption(ui, th, &label);
    let mut n = *count as f64;
    let out = widgets::NumberField::new("offset_opts.count", &mut n, number_format(ru))
        .labelled_by(lr.id, &label)
        .error_text(&err)
        .decimals(0)
        .step(1.0)
        .range(COUNT_MIN as f64, COUNT_MAX as f64)
        .width(field_w())
        .show(ui, th);
    let _ = out;
    let next = clamp_count(n);
    let changed = next != *count;
    *count = next;
    changed
}

/// Length field of the offset distance with a muted unit; refused text keeps the last valid
/// value and leaves the reason under the bar.
pub fn dist_field(
    ui: &mut egui::Ui,
    th: &Theme,
    ru: bool,
    u: Units,
    dist: &mut f64,
) -> bool {
    let label = crate::tr_lang(ru, OptKind::Dist.label_key(), &[]);
    let unit = match u {
        Units::Metric => crate::tr_lang(ru, "unit_fmt.unit.mm", &[]),
        Units::Imperial => String::new(),
    };
    let lr = caption(ui, th, &label);
    let mark = DecimalMark::for_ru(ru);
    let buf_id = ui.id().with("ux48f.dist.buf");
    let stored = ui.data(|d| d.get_temp::<String>(buf_id));
    let mut text = stored
        .clone()
        .unwrap_or_else(|| crate::units::edit_length_mark(*dist, u, mark));
    let bad = stored.as_deref().and_then(|s| check_dist_mark(s, u, mark).err());
    let err = crate::tr_lang(ru, bad.unwrap_or(Reject::NotLength).key(), &[]);
    let out = TextField::new(&mut text)
        .id_salt("offset_opts.dist")
        .labelled_by(lr.id, &label)
        .error_text(&err)
        .error(bad.is_some())
        .unit(&unit)
        .right_aligned()
        .width(field_w())
        .show(ui, th);
    let mut changed = false;
    if out.committed && !out.cancelled {
        let (v, why) = commit_dist_mark(*dist, &text, u, mark);
        changed = v != *dist;
        *dist = v;
        set_reject(ui, why);
    } else if out.cancelled {
        set_reject(ui, None);
    }
    if out.response.has_focus() {
        ui.data_mut(|d| d.insert_temp(buf_id, text));
    } else {
        ui.data_mut(|d| d.remove_temp::<String>(buf_id));
    }
    changed
}

/// Typed value so far: a read-only field chip with a painted accent caret.
pub fn typed_chip(ui: &mut egui::Ui, th: &Theme, text: &str) -> egui::Response {
    let s = th.sem();
    let g = ui
        .painter()
        .layout_no_wrap(text.to_owned(), th.font(Role::Body), s.text_primary);
    let h = th.d().control;
    let w = (g.size().x + 2.0 * space::SM + space::XS).max(control::FIELD_MIN_W);
    let (r, resp) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text));
    let p = ui.painter();
    p.rect_filled(r, th.radius("sm"), s.bg_input);
    p.rect_stroke(
        r,
        th.radius("sm"),
        egui::Stroke::new(1.0, s.border_strong),
        egui::StrokeKind::Inside,
    );
    let x0 = r.left() + space::SM;
    p.galley(egui::pos2(x0, r.center().y - g.size().y / 2.0), g.clone(), s.text_primary);
    let cx = x0 + g.size().x + 1.0;
    let half = (g.size().y / 2.0).min(h / 2.0 - space::XS / 2.0);
    p.line_segment(
        [
            egui::pos2(cx, r.center().y - half),
            egui::pos2(cx, r.center().y + half),
        ],
        egui::Stroke::new(1.5, s.accent),
    );
    resp
}

/// Primary «Done» with the Enter hint; true when clicked.
pub fn done_button(ui: &mut egui::Ui, th: &Theme, ru: bool, label: &str) -> bool {
    let tip = crate::tr_lang(ru, "offset_opts.done_tip", &[]);
    Button::primary(label)
        .small()
        .tooltip(&tip, "Enter")
        .show(ui, th)
        .clicked()
}

/// The isolate banner: badge with the mode, «Apply to view», close. Returns (apply, exit).
pub fn banner_row(ui: &mut egui::Ui, th: &Theme, b: &Banner) -> (bool, bool) {
    let mut apply = false;
    let mut exit = false;
    ui.horizontal(|ui| {
        row_style(ui, th);
        widgets::badge(ui, th, &b.label, widgets::Tone::Accent);
        apply = Button::secondary(&b.apply)
            .small()
            .tooltip(&b.apply_tip, "")
            .show(ui, th)
            .clicked();
        exit = widgets::close_button(ui, &b.exit_tip).clicked();
    });
    (apply, exit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::isolate::{Temp, Vis};
    use crate::units::Units::{Imperial, Metric};

    const ALL_OPS: [Op; 12] = [
        Op::Move,
        Op::Copy,
        Op::Rotate,
        Op::Mirror,
        Op::MirrorDraw,
        Op::Array,
        Op::Offset,
        Op::Trim,
        Op::Extend,
        Op::Split,
        Op::Align,
        Op::Scale,
    ];

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    #[test]
    fn option_counts_per_tool() {
        assert_eq!(opts_of(Op::Move).len(), 3);
        assert_eq!(opts_of(Op::Copy).len(), 2);
        for op in [Op::Rotate, Op::Mirror, Op::MirrorDraw, Op::Scale] {
            assert_eq!(opts_of(op), &[OptKind::Copy], "{op:?}");
        }
        assert_eq!(opts_of(Op::Array).len(), 4);
        assert_eq!(opts_of(Op::Offset), &[OptKind::Dist]);
        for op in [Op::Trim, Op::Extend, Op::Split, Op::Align] {
            assert!(opts_of(op).is_empty(), "{op:?}");
        }
    }

    #[test]
    fn every_option_has_a_translated_label() {
        for ru_flag in [false, true] {
            for op in ALL_OPS {
                for k in opts_of(op) {
                    let s = crate::tr_lang(ru_flag, k.label_key(), &[]);
                    assert_ne!(s, k.label_key(), "{:?} missing", k);
                }
            }
        }
        for k in [OptKind::Copy, OptKind::Count, OptKind::Dist] {
            assert_ne!(
                crate::tr_lang(false, k.label_key(), &[]),
                crate::tr_lang(true, k.label_key(), &[])
            );
        }
    }

    #[test]
    fn dist_parses_decimals_and_units_in_both_languages() {
        close(check_dist("250", Metric).unwrap(), 0.25);
        close(check_dist("1.5", Metric).unwrap(), 0.0015);
        close(check_dist("1,5", Metric).unwrap(), 0.0015);
        close(check_dist("3 000 mm", Metric).unwrap(), 3.0);
        close(check_dist("3\u{a0}000", Metric).unwrap(), 3.0);
        close(check_dist("1,5 m", Metric).unwrap(), 1.5);
        close(check_dist("5'-6 1/2\"", Imperial).unwrap(), 66.5 * 0.0254);
        close(check_dist("1.5m", Imperial).unwrap(), 1.5);
    }

    #[test]
    fn dist_reads_the_bar_language_not_the_global_mark() {
        let (en, ru) = (DecimalMark::Point, DecimalMark::Comma);
        close(check_dist_mark("1,5", Metric, ru).unwrap(), 0.0015);
        assert!(check_dist_mark("1,5", Metric, en).is_err());
        close(check_dist_mark("3,000", Metric, en).unwrap(), 3.0);
        close(check_dist_mark("3,000", Metric, ru).unwrap(), 0.003);
        let (v, why) = commit_dist_mark(0.4, "1,5", Metric, en);
        close(v, 0.4);
        assert!(why.is_some());
    }

    #[test]
    fn invalid_dist_keeps_the_last_valid_value_with_a_reason() {
        let (v, why) = commit_dist(0.4, "abc", Metric);
        close(v, 0.4);
        assert_eq!(why, Some(Reject::NotLength));
        let (v, why) = commit_dist(0.4, "", Metric);
        close(v, 0.4);
        assert_eq!(why, Some(Reject::NotLength));
        let (v, why) = commit_dist(0.4, "600", Metric);
        close(v, 0.6);
        assert_eq!(why, None);
    }

    #[test]
    fn offset_distance_must_be_positive() {
        assert_eq!(check_dist("0", Metric), Err(Reject::NotPositive));
        assert_eq!(check_dist("-300", Metric), Err(Reject::NotPositive));
        let (v, why) = commit_dist(0.4, "-300", Metric);
        close(v, 0.4);
        assert_eq!(why, Some(Reject::NotPositive));
        assert!(check_dist("0.001", Metric).is_ok());
    }

    #[test]
    fn reject_reasons_exist_in_both_languages() {
        for r in [Reject::NotLength, Reject::NotPositive] {
            let en = crate::tr_lang(false, r.key(), &[]);
            let ru = crate::tr_lang(true, r.key(), &[]);
            assert_ne!(en, r.key());
            assert_ne!(ru, r.key());
            assert_ne!(en, ru);
        }
    }

    #[test]
    fn array_count_is_bounded() {
        assert_eq!(clamp_count(0.0), COUNT_MIN);
        assert_eq!(clamp_count(1.0), 2);
        assert_eq!(clamp_count(2.0), 2);
        assert_eq!(clamp_count(7.4), 7);
        assert_eq!(clamp_count(200.0), 200);
        assert_eq!(clamp_count(201.0), 200);
        assert_eq!(clamp_count(1e12), 200);
        assert_eq!(clamp_count(-5.0), 2);
        assert_eq!(clamp_count(f64::NAN), 2);
        assert_eq!((COUNT_MIN, COUNT_MAX), (2, 200));
    }

    #[test]
    fn count_text_follows_the_locale() {
        use ddc_ui::widgets::field::parse_number;
        assert_eq!(parse_number("12", number_format(false)), Some(12.0));
        assert_eq!(parse_number("12", number_format(true)), Some(12.0));
        assert_eq!(parse_number("2,5", number_format(true)), Some(2.5));
        assert_eq!(parse_number("2.5", number_format(false)), Some(2.5));
        assert_eq!(parse_number("x", number_format(true)), None);
        let msg = crate::tr_lang(true, "offset_opts.count_bad", &[("min", "2"), ("max", "200")]);
        assert!(msg.contains('2') && msg.contains("200") && !msg.contains('{'), "{msg}");
        let msg = crate::tr_lang(false, "offset_opts.count_bad", &[("min", "2"), ("max", "200")]);
        assert!(msg.contains("200") && !msg.contains('{'), "{msg}");
    }

    #[test]
    fn banner_text_per_language_and_mode() {
        let en = banner(true, false);
        let ru = banner(true, true);
        assert_eq!(en.label, "Temporary isolate");
        assert_eq!(en.apply, "Apply to view");
        assert_ne!(ru.label, en.label);
        assert_ne!(ru.apply, en.apply);
        assert_ne!(ru.exit_tip, en.exit_tip);
        assert_ne!(ru.apply_tip, en.apply_tip);
        assert_eq!(banner(false, false).label, "Temporarily hidden");
        assert_ne!(banner(false, true).label, ru.label);
        for b in [&en, &ru] {
            for s in [&b.label, &b.apply, &b.apply_tip, &b.exit_tip] {
                assert!(!s.is_empty() && !s.starts_with("offset_opts.") && !s.starts_with("iso."), "{s}");
            }
        }
    }

    #[test]
    fn exit_action_clears_the_temporary_mode() {
        let mut v = Vis::default();
        v.temp = Temp::Isolate(Default::default());
        assert!(v.temp_on());
        exit_banner(&mut v);
        assert!(!v.temp_on());
        v.temp = Temp::Hide(Default::default());
        exit_banner(&mut v);
        assert!(!v.temp_on());
    }

    #[test]
    fn dist_ok_rejects_zero_negative_and_non_finite() {
        assert!(dist_ok(0.001));
        for v in [0.0, -0.1, f64::NAN, f64::INFINITY] {
            assert!(!dist_ok(v), "{v}");
        }
    }

    #[test]
    fn typed_offset_paths_refuse_visibly_and_banner_is_info() {
        let code = code_of(include_str!("modify_opts.rs"));
        assert!(code.contains("Tone::Accent") && !code.contains("Tone::Warning"));
        let src = include_str!("modify_tools.rs");
        assert_eq!(src.matches("modify_opts::dist_ok(v)").count(), 2);
    }

    #[test]
    fn done_and_chip_texts_exist() {
        for ru in [false, true] {
            for k in ["offset_opts.done_tip", "mt.done"] {
                assert_ne!(crate::tr_lang(ru, k, &[]), k);
            }
        }
    }

    fn code_of(src: &'static str) -> &'static str {
        &src[..src.find("#[cfg(test)]").unwrap_or(src.len())]
    }

    const RAW: [&str; 11] = [
        "ui.checkbox",
        "radio_value",
        "DragValue",
        "\u{258f}",
        "ui.monospace",
        "ui.button(",
        "ui.add(",
        "ComboBox",
        "TextEdit",
        "Color32::",
        "from_rgb",
    ];

    #[test]
    fn this_file_uses_no_raw_widgets_or_literal_colours() {
        let code = code_of(include_str!("modify_opts.rs"));
        for bad in RAW {
            // the guard list itself is in the tests, which are cut off above
            assert!(!code.contains(bad), "raw widget or glyph: {bad}");
        }
    }

    #[test]
    fn option_bar_and_banner_in_modify_tools_use_the_kit() {
        let src = include_str!("modify_tools.rs");
        let from = src.find("fn options(").expect("options");
        let to = src.find("fn filter_window(").expect("filter_window");
        let bar = &src[from..to];
        let at = src.find("ux48f banner").expect("banner");
        let banner = &src[at..from];
        for part in [bar, banner] {
            for bad in &RAW[..6] {
                assert!(!part.contains(bad), "raw widget or glyph left: {bad}");
            }
        }
        assert!(!bar.contains("Frame::popup"));
        assert!(bar.contains("modify_opts::"));
        assert!(banner.contains("modify_opts::"));
    }
}
