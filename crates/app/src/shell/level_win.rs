// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! "Level" window: create and edit (name, elevation in project units).
use super::EditorApp;
use crate::editor::Editor;
use crate::ui_kit as kit;
use crate::units::{self, Units};
use ddc_ui::theme::space;
use ddc_ui::widgets::TextField;

/// Elevation (metres) beyond which the dialog warns about a wrong unit.
const FAR_M: f64 = 1000.0;

/// Dialog state; `err` is a model refusal shown inline while the typed text stays.
pub struct LevelEdit {
    pub id: Option<i64>,
    pub name: String,
    pub z: String,
    pub err: Option<String>,
}

pub enum LevelWarn {
    Far,
    Past(String),
}

/// Language of the dialog: text, and the decimal mark its numbers are typed in.
#[derive(Clone, Copy)]
pub struct Loc {
    pub ru: bool,
    pub mark: units::DecimalMark,
}

impl Loc {
    pub fn ui() -> Self {
        Self { ru: crate::ui_is_ru(), mark: units::ui_decimal_mark() }
    }

    pub fn of(ru: bool) -> Self {
        Self { ru, mark: units::DecimalMark::for_ru(ru) }
    }
}

/// Elevation text in project units → metres, numbers read in the UI language.
pub fn parse_elevation_in(s: &str, u: Units) -> Option<f64> {
    parse_elevation_lang(s, u, units::ui_decimal_mark())
}

/// Elevation text in project units → metres. Metric: bare number = mm; imperial: bare = inches.
/// `10'-6"`, `10 ft 6 in`, `3 m`, `+3000`, `±0`; numbers by the locale of `mark` (en `1,000` is a
/// thousand, ru `10,5'` is ten and a half). Size is not limited here (see [`level_warning`]).
pub fn parse_elevation_lang(s: &str, u: Units, mark: units::DecimalMark) -> Option<f64> {
    let t = s.trim().trim_start_matches('\u{b1}').trim();
    let mut t = t.strip_prefix('+').unwrap_or(t).trim().to_string();
    if t.contains("ft") {
        t = t.replace("ft", "'");
        if let Some(r) = t.trim_end().strip_suffix("in") {
            t = format!("{}\"", r.trim_end());
        }
    }
    let t = units::edit_text_canonical(&t, mark).ok()?;
    units::parse_signed_unbounded(&t, u).ok().filter(|z| z.is_finite())
}

/// Elevation as the dialog field shows it.
pub fn format_elevation(z: f64, u: Units) -> String {
    units::format_length(z, u)
}

/// Unit shown muted at the right edge of the elevation field.
pub fn unit_text(u: Units, ru: bool) -> String {
    match u {
        Units::Metric => crate::tr_str(ru, "unit_fmt.unit.mm").to_string(),
        Units::Imperial => crate::tr_lang(ru, "dlg.level.unit_imp", &[]),
    }
}

pub fn elevation_label(u: Units, ru: bool) -> String {
    crate::tr_lang(ru, "dlg.level.elevation", &[("u", unit_text(u, ru).as_str())])
}

/// "Not a number" hint with an example in project units (a bare metric number is mm).
pub fn elevation_bad(u: Units, ru: bool) -> String {
    let ex = match u {
        Units::Metric => format_elevation(3.0, u),
        Units::Imperial => format_elevation(10.0 * units::FOOT + 6.0 * units::INCH, u),
    };
    crate::tr_lang(ru, "dlg.level.elevation_bad", &[("ex", ex.as_str())])
}

/// Non-blocking hint: very far elevation, or the level jumping over a neighbour.
pub fn level_warning(levels: &[(i64, String, f64)], id: Option<i64>, z: f64) -> Option<LevelWarn> {
    if z.abs() > FAR_M {
        return Some(LevelWarn::Far);
    }
    let old = levels.iter().find(|l| Some(l.0) == id)?.2;
    levels
        .iter()
        .filter(|l| Some(l.0) != id)
        .find(|l| (l.2 - old) * (l.2 - z) < 0.0)
        .map(|l| LevelWarn::Past(l.1.clone()))
}

pub fn warning_text(w: &LevelWarn, z: f64, u: Units, ru: bool) -> String {
    match w {
        LevelWarn::Far => {
            let v = units::display_length_lang(z, u, ru);
            crate::tr_lang(ru, "dlg.level.warn_high", &[("v", v.as_str())])
        }
        LevelWarn::Past(n) => crate::tr_lang(ru, "dlg.level.warn_past", &[("name", n.as_str())]),
    }
}

impl LevelEdit {
    /// Apply in one undo step. On refusal the typed text is kept and `err` is set.
    pub fn commit(&mut self, ed: &mut Editor, u: Units, loc: Loc) -> bool {
        let old = self.id.and_then(|id| ed.levels().into_iter().find(|l| l.0 == id));
        // an untouched field keeps the exact stored value (the field shows rounded text)
        let z = match &old {
            Some(l) if format_elevation(l.2, u) == self.z.trim() => Some(l.2),
            _ => parse_elevation_lang(&self.z, u, loc.mark),
        };
        let Some(z) = z else {
            self.err = Some(elevation_bad(u, loc.ru));
            return false;
        };
        let r = match self.id {
            Some(id) => ed.try_edit_level(id, &self.name, z),
            None => ed.try_add_level(&self.name, z).map(drop),
        };
        match r {
            Ok(()) => {
                self.err = None;
                true
            }
            Err(e) => {
                self.err = Some(e);
                false
            }
        }
    }
}

/// What the user asked for this frame.
#[derive(Default, Clone, Copy)]
pub struct FormOut {
    pub done: bool,
    pub cancel: bool,
}

/// Dialog body on kit widgets: name and elevation fields in the shared label column, unit muted
/// inside the field, red field state for an elevation that is not a length, Enter = primary.
pub fn level_form(
    ui: &mut egui::Ui,
    st: &mut LevelEdit,
    u: Units,
    loc: Loc,
    levels: &[(i64, String, f64)],
) -> FormOut {
    let th = ddc_ui::theme::of(ui.ctx());
    let ru = loc.ru;
    let zv = parse_elevation_lang(&st.z, u, loc.mark);
    let warn = zv.and_then(|z| level_warning(levels, st.id, z).map(|w| warning_text(&w, z, u, ru)));
    let before = (st.name.clone(), st.z.clone());
    let err = st.err.clone();
    let name_l = crate::tr_lang(ru, "dlg.level.name", &[]);
    let elev_l = crate::tr_lang(ru, "level_props.level.elevation", &[]);
    let unit = unit_text(u, ru);
    let bad = elevation_bad(u, ru);
    kit::labeled_row_id(ui, &name_l, |ui, lid| {
        TextField::new(&mut st.name)
            .id_salt("ux56a.level.name")
            .labelled_by(lid, &name_l)
            .show(ui, &th);
    });
    ui.add_space(space::XS);
    kit::labeled_row_id(ui, &elev_l, |ui, lid| {
        let r = TextField::new(&mut st.z)
            .id_salt("level_props.level.elevation")
            .labelled_by(lid, &elev_l)
            .unit(&unit)
            .right_aligned()
            .error(zv.is_none())
            .error_text(&bad)
            .show(ui, &th);
        if zv.is_none() {
            r.response.on_hover_text(&bad);
        }
    });
    if let Some(w) = &warn {
        kit::warn_note(ui, w.clone());
    }
    if let Some(e) = &err {
        kit::error_note(ui, e);
    }
    let mut out = FormOut::default();
    kit::dialog_footer(ui, |ui| {
        let why = if st.name.trim().is_empty() {
            Some(crate::tr_lang(ru, "dlg.level.name_empty", &[]))
        } else if zv.is_none() {
            Some(bad.clone())
        } else {
            None
        };
        let label = crate::tr_lang(ru, if st.id.is_some() { "dlg.save" } else { "dlg.create" }, &[]);
        out.done = kit::primary_action(ui, label, why.as_deref());
        out.cancel = kit::cancel_button(ui, crate::tr_lang(ru, "dlg.cancel", &[])).clicked();
    });
    if (st.name.as_str(), st.z.as_str()) != (before.0.as_str(), before.1.as_str()) {
        st.err = None;
    }
    out
}

impl EditorApp {
    /// Open the window: `None` creates a new level with the default name and elevation.
    pub fn start_level_edit(&mut self, id: Option<i64>) {
        let (name, z) = match id.and_then(|id| self.ed.levels().into_iter().find(|l| l.0 == id)) {
            Some((_, n, z)) => (n, z),
            None => self.ed.next_level_defaults(),
        };
        let z = format_elevation(z, self.ed.tool.units);
        self.level_edit = Some(LevelEdit { id, name, z, err: None });
    }

    pub(super) fn level_window(&mut self, ctx: &egui::Context) {
        self.level_window_in(ctx, Loc::ui());
    }

    /// One frame of the open level window in the given language.
    #[cfg(test)]
    pub(crate) fn layout_level(&mut self, ctx: &egui::Context, ru: bool) {
        self.level_window_in(ctx, Loc::of(ru));
    }

    fn level_window_in(&mut self, ctx: &egui::Context, loc: Loc) {
        let Some(mut st) = self.level_edit.take() else {
            return;
        };
        let u = self.ed.tool.units;
        let mut open = true;
        let title = crate::tr_lang(loc.ru, if st.id.is_some() { "dlg.level.title" } else { "dlg.level.new" }, &[]);
        let levels = self.ed.levels();
        let mut out = FormOut::default();
        kit::Dialog::new("level", title, kit::DialogSize::Confirm)
            .open(&mut open)
            .show(ctx, |ui| out = level_form(ui, &mut st, u, loc, &levels));
        // Esc closes the dialog also from inside a field: it is the form's only way out by key
        let esc = open && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        let (done, cancel) = (out.done, out.cancel || esc);
        if done {
            if st.commit(&mut self.ed, u, loc) {
                if st.id.is_none() {
                    self.view3d = false;
                    self.section = None;
                }
                return;
            }
        } else if !open || cancel {
            return;
        }
        self.level_edit = Some(st);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use units::DecimalMark;

    fn dlg(id: Option<i64>, name: &str, z: &str) -> LevelEdit {
        LevelEdit { id, name: name.into(), z: z.into(), err: None }
    }

    #[test]
    fn level_dialog_parses_feet_inches_in_imperial() {
        let want = 10.0 * units::FOOT + 6.0 * units::INCH;
        for mark in [DecimalMark::Point, DecimalMark::Comma] {
            for s in ["10'-6\"", "10 ft 6 in", "10'6\""] {
                let z = parse_elevation_lang(s, Units::Imperial, mark).unwrap_or(f64::NAN);
                assert!((z - want).abs() < 1e-9, "{s}: {z}");
            }
            let neg = parse_elevation_lang("-3'", Units::Imperial, mark).unwrap();
            assert!((neg + 3.0 * units::FOOT).abs() < 1e-9);
            assert!(parse_elevation_lang("abc", Units::Imperial, mark).is_none());
            for z in [want, 10.0 * units::FOOT + 6.5 * units::INCH, -2.0 * units::FOOT] {
                let t = format_elevation(z, Units::Imperial);
                let back = parse_elevation_lang(&t, Units::Imperial, mark).unwrap_or(f64::NAN);
                assert!((back - z).abs() < 1e-9, "{t}: {back}");
            }
        }
        assert!(elevation_label(Units::Imperial, false).contains("ft-in"));
    }

    #[test]
    fn ft_in_comma_follows_the_ui_language() {
        let ten_half = 10.5 * units::FOOT;
        let ru = parse_elevation_lang("10,5'", Units::Imperial, DecimalMark::Comma).unwrap();
        assert!((ru - ten_half).abs() < 1e-9, "{ru}");
        let en = parse_elevation_lang("10.5'", Units::Imperial, DecimalMark::Point).unwrap();
        assert!((en - ten_half).abs() < 1e-9, "{en}");
        // an English comma is a group mark, not a decimal one
        assert!(parse_elevation_lang("10,5'", Units::Imperial, DecimalMark::Point).is_none());
        assert!(parse_elevation_lang("10,5 ft", Units::Imperial, DecimalMark::Point).is_none());
    }

    #[test]
    fn metric_groups_follow_the_ui_language() {
        assert_eq!(parse_elevation_lang("1,000", Units::Metric, DecimalMark::Point), Some(1.0));
        assert_eq!(parse_elevation_lang("1 000", Units::Metric, DecimalMark::Comma), Some(1.0));
        assert_eq!(parse_elevation_lang("3,5 m", Units::Metric, DecimalMark::Comma), Some(3.5));
        assert_eq!(parse_elevation_lang("3.5 m", Units::Metric, DecimalMark::Point), Some(3.5));
        let t = format_elevation(3.2, Units::Metric);
        assert_eq!(parse_elevation_lang(&t, Units::Metric, DecimalMark::Point), Some(3.2));
        assert_eq!(parse_elevation_lang(&t, Units::Metric, DecimalMark::Comma), Some(3.2));
    }

    #[test]
    fn bad_elevation_example_is_in_project_units() {
        for ru in [false, true] {
            let m = elevation_bad(Units::Metric, ru);
            assert!(m.contains("3000") && !m.contains("3.000") && !m.contains("3,000"), "{m}");
            let i = elevation_bad(Units::Imperial, ru);
            assert!(i.contains("10'"), "{i}");
        }
        assert_eq!(parse_elevation_in("3000", Units::Metric), Some(3.0));
    }

    #[test]
    fn metric_mm_project_parses_3000_as_3m() {
        assert_eq!(parse_elevation_in("3000", Units::Metric), Some(3.0));
        assert_eq!(parse_elevation_in("+3000", Units::Metric), Some(3.0));
        let ru = DecimalMark::Comma;
        assert_eq!(parse_elevation_lang("-1 200", Units::Metric, ru), Some(-1.2));
        assert_eq!(parse_elevation_lang("\u{b1}0", Units::Metric, ru), Some(0.0));
        assert_eq!(parse_elevation_lang("3,5 m", Units::Metric, ru), Some(3.5));
        assert_eq!(parse_elevation_lang("abc", Units::Metric, ru), None);
        assert_eq!(format_elevation(3.0, Units::Metric), "3000");
        assert!(elevation_label(Units::Metric, false).contains("mm"));
    }

    #[test]
    fn level_dialog_warns_on_3000_metres() {
        let z = parse_elevation_lang("3000 m", Units::Metric, DecimalMark::Point).unwrap();
        assert_eq!(z, 3000.0);
        let ls = vec![(1, "L1".to_string(), 0.0), (2, "L2".to_string(), 3.0)];
        let w = level_warning(&ls, Some(2), z).unwrap();
        assert!(matches!(w, LevelWarn::Far));
        assert!(warning_text(&w, z, Units::Metric, false).contains("1000 m"));
        assert!(level_warning(&ls, Some(2), 3.5).is_none());
        // 3 m -> -1 m passes L1 at 0
        assert!(matches!(level_warning(&ls, Some(2), -1.0), Some(LevelWarn::Past(n)) if n == "L1"));
        assert!(level_warning(&ls, None, 5.0).is_none());
    }

    #[test]
    fn refused_edit_keeps_dialog_text() {
        let mut ed = Editor::new_in_memory().unwrap();
        let n = ed.levels().len();
        let loc = Loc::of(false);
        let mut d = dlg(Some(987_654), "Roof", "6000");
        assert!(!d.commit(&mut ed, Units::Metric, loc));
        assert!(d.err.is_some());
        assert_eq!((d.name.as_str(), d.z.as_str()), ("Roof", "6000"));
        assert_eq!(ed.levels().len(), n);
        let mut bad = dlg(None, "X", "abc");
        assert!(!bad.commit(&mut ed, Units::Metric, loc));
        assert_eq!(bad.z, "abc");
        // success: one undo step restores
        let mut ok = dlg(None, "Roof", "6000");
        assert!(ok.commit(&mut ed, Units::Metric, loc) && ok.err.is_none());
        assert_eq!(ed.levels().len(), n + 1);
        ed.undo();
        assert_eq!(ed.levels().len(), n);
    }

    #[test]
    fn invalid_text_writes_nothing_and_a_valid_edit_is_one_undo_step() {
        let mut ed = Editor::new_in_memory().unwrap();
        let n = ed.levels().len();
        let en = Loc::of(false);
        // an English comma is not a decimal mark: refused, nothing written
        for z in ["10,5'", "abc", ""] {
            let mut d = dlg(None, "Bad", z);
            assert!(!d.commit(&mut ed, Units::Imperial, en), "{z}");
            assert!(d.err.is_some() && d.z == z);
            assert_eq!(ed.levels().len(), n);
        }
        let mut add = dlg(None, "Roof", "6000");
        assert!(add.commit(&mut ed, Units::Metric, en));
        let id = ed.levels().into_iter().find(|l| l.1 == "Roof").unwrap().0;
        let mut edit = dlg(Some(id), "Roof 2", "7,000");
        assert!(edit.commit(&mut ed, Units::Metric, en));
        let now = ed.levels().into_iter().find(|l| l.0 == id).unwrap();
        assert_eq!((now.1.as_str(), now.2), ("Roof 2", 7.0));
        ed.undo();
        let back = ed.levels().into_iter().find(|l| l.0 == id).unwrap();
        assert_eq!((back.1.as_str(), back.2), ("Roof", 6.0));
    }

    fn key_ev(key: egui::Key, pressed: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn frame(
        ctx: &egui::Context,
        st: &mut LevelEdit,
        u: Units,
        loc: Loc,
        events: Vec<egui::Event>,
    ) -> (FormOut, usize) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))),
            events,
            ..Default::default()
        };
        let mut out = FormOut::default();
        let full = ctx.run(input, |c| {
            kit::Dialog::new("t.ux56a.level", "T", kit::DialogSize::Confirm)
                .show(c, |ui| out = level_form(ui, &mut *st, u, loc, &[]));
        });
        (out, full.shapes.len())
    }

    #[test]
    fn enter_fires_the_primary_once_and_only_when_valid() {
        for (z, fires) in [("6000", true), ("abc", false)] {
            let ctx = ddc_ui::theme::headless_ctx();
            let mut st = dlg(None, "Roof", z);
            let (u, loc) = (Units::Metric, Loc::of(false));
            for _ in 0..2 {
                frame(&ctx, &mut st, u, loc, vec![]);
            }
            frame(&ctx, &mut st, u, loc, vec![key_ev(egui::Key::Enter, false)]);
            let (first, _) = frame(&ctx, &mut st, u, loc, vec![key_ev(egui::Key::Enter, true)]);
            assert_eq!(first.done, fires, "{z}");
            let (again, _) = frame(&ctx, &mut st, u, loc, vec![]);
            assert!(!again.done && !first.cancel, "{z}");
        }
    }

    #[test]
    fn headless_render_in_both_languages_and_unit_systems() {
        for (ru, u, z) in [
            (false, Units::Metric, "3000"),
            (true, Units::Metric, "junk"),
            (false, Units::Imperial, "10'-6\""),
            (true, Units::Imperial, "10,5'"),
        ] {
            let ctx = ddc_ui::theme::headless_ctx();
            let mut st = dlg(Some(1), "Level 1", z);
            let mut shapes = 0;
            for _ in 0..3 {
                shapes = frame(&ctx, &mut st, u, Loc::of(ru), vec![]).1;
            }
            assert!(shapes > 0, "{ru} {z}");
            assert_eq!(st.z, z);
        }
    }

    #[test]
    fn esc_closes_the_new_level_dialog_and_the_canvas_waits() {
        use crate::layout_contract::{app_in, driven_ctx, pass_on};
        let mut a = app_in(ddc_ui::Lang::En);
        a.start_level_edit(None);
        let (ctx, mut th) = driven_ctx();
        let id = egui::Id::new("uxfix4.level");
        let mut draw = |c: &egui::Context| a.layout_level(c, false);
        for _ in 0..3 {
            pass_on(&ctx, &mut th, id, (1440.0, 900.0), vec![], &mut draw);
        }
        let esc = egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        pass_on(&ctx, &mut th, id, (1440.0, 900.0), vec![esc], &mut draw);
        assert!(a.level_edit.is_none(), "Esc left the New level dialog open");
    }

    #[test]
    fn form_uses_kit_widgets_only() {
        let src = include_str!("level_win.rs");
        let body = &src[..src.find("#[cfg(test)]").unwrap()];
        for raw in ["TextEdit::singleline", "desired_width(200.0)", "[12.0, 8.0]", "egui::Grid", "DragValue"] {
            assert!(!body.contains(raw), "raw widget left: {raw}");
        }
        assert!(body.contains("TextField::new(") && body.contains("labeled_row_id("));
    }

    #[test]
    fn locale_json_is_paired() {
        let en = include_str!("../../../ui/data/i18n/en/level_props.json");
        let ru = include_str!("../../../ui/data/i18n/ru/level_props.json");
        let (en, ru): (serde_json::Value, serde_json::Value) =
            (serde_json::from_str(en).unwrap(), serde_json::from_str(ru).unwrap());
        let keys = |v: &serde_json::Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
        assert_eq!(keys(&en), keys(&ru));
        assert_eq!(crate::tr_lang(false, "level_props.level.elevation", &[]), "Elevation");
    }
}
