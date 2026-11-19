//! "Grid set" dialog: two families of grid lines from a count, spacing and first label, an angle
//! and an origin. One Create is one undo step; a label that already exists refuses it.

use crate::editor::{Editor, OpFn};
use crate::grid_set::{lines, Family, GridSet, Line};
use crate::ui_kit as kit;
use ddc_model::GridSpec;
use ddc_ui::theme::space;
use ddc_ui::widgets::TextField;
use std::cell::RefCell;
use std::sync::Arc;

pub struct State {
    pub open: bool,
    pub x_count: String,
    pub x_step: String,
    pub x_start: String,
    pub y_count: String,
    pub y_step: String,
    pub y_start: String,
    pub angle: String,
    pub ox: String,
    pub oy: String,
    pub error: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        let s = |t: &str| t.to_string();
        State {
            open: false,
            x_count: s("4"),
            x_step: s("6"),
            x_start: s("1"),
            y_count: s("3"),
            y_step: s("6"),
            y_start: s("A"),
            angle: s("0"),
            ox: s("0"),
            oy: s("0"),
            error: None,
        }
    }
}

thread_local! {
    static WIN: RefCell<State> = RefCell::new(State::default());
}

pub fn register(app: &mut crate::shell::EditorApp) {
    app.register_command("arch.grid_set", |_| {
        WIN.with(|w| w.borrow_mut().open = true);
    });
}

fn number(s: &str) -> Option<f64> {
    crate::units::parse_locale_number(s.trim(), crate::units::ui_decimal_mark())
        .ok()
        .filter(|x| x.is_finite())
}

fn family(count: &str, step: &str, start: &str, u: crate::units::Units) -> Result<Family, String> {
    let n = count.trim().parse::<usize>().map_err(|_| "count".to_string())?;
    if n == 0 {
        return Ok(Family::new(0, 1.0, start.trim()));
    }
    let gap = crate::units::parse_edit_length(step, u).ok_or_else(|| "spacing".to_string())?;
    Ok(Family::new(n, gap, start.trim()))
}

/// The lines the fields describe; `Err` is the key of the field that is wrong or the
/// generator's own text.
pub fn lines_of(st: &State, u: crate::units::Units) -> Result<Vec<Line>, String> {
    let set = GridSet {
        origin: (
            crate::units::parse_typed_signed(&st.ox, u, crate::units::ui_decimal_mark()).map_err(|_| "origin")?,
            crate::units::parse_typed_signed(&st.oy, u, crate::units::ui_decimal_mark()).map_err(|_| "origin")?,
        ),
        angle: number(&st.angle).ok_or("angle")?,
        overhang: 1.5,
        x: family(&st.x_count, &st.x_step, &st.x_start, u)?,
        y: family(&st.y_count, &st.y_step, &st.y_start, u)?,
    };
    lines(&set)
}

/// Adds every line as one undo step; a name already in the model refuses all of them.
pub fn create(ed: &mut Editor, ls: &[Line]) -> Result<(), String> {
    let taken: Vec<String> = ed
        .model
        .grids()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|g| g.name)
        .collect();
    if let Some(l) = ls.iter().find(|l| taken.contains(&l.name)) {
        return Err(format!("clash:{}", l.name));
    }
    let specs: Vec<GridSpec> = ls.iter().map(|l| GridSpec::line(&l.name, l.a, l.b)).collect();
    ed.run_op(OpFn(Arc::new(move |m| {
        for s in &specs {
            m.add_grid(s)?;
        }
        Ok(())
    })))
    .map_err(|e| e.to_string())
}

fn problem(key: &str, ru: bool) -> String {
    match key.split_once(':') {
        Some(("clash", name)) => crate::tr_lang(ru, "gridset.err.clash", &[("name", name)]),
        None if ["count", "spacing", "angle", "origin"].contains(&key) => {
            crate::tr_lang(ru, &format!("gridset.err.{key}"), &[])
        }
        _ => crate::tr_lang(ru, "gridset.err.other", &[("text", key)]),
    }
}

fn field(ui: &mut egui::Ui, th: &ddc_ui::Theme, id: &str, buf: &mut String, ru: bool) {
    let lbl = crate::tr_str(ru, "gridset.field");
    TextField::new(buf)
        .labelled_by(egui::Id::new(("gridset.lbl", id)), lbl)
        .id_salt(id)
        .show(ui, th);
}

fn note(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(text).wrap());
}

/// Dialog body (apart from the frame, so layout probes can run it). `true`: close the dialog.
pub fn body(ui: &mut egui::Ui, st: &mut State, ed: &mut Editor, ru: bool) -> bool {
    let th = ddc_ui::theme::of(ui.ctx());
    let tr = |k: &'static str| crate::tr_str(ru, k);
    let u = ed.tool.units;
    for id in ["x", "y"] {
        kit::section_title(ui, tr(if id == "x" { "gridset.x" } else { "gridset.y" }));
        let (count, step, start) = if id == "x" {
            (&mut st.x_count, &mut st.x_step, &mut st.x_start)
        } else {
            (&mut st.y_count, &mut st.y_step, &mut st.y_start)
        };
        kit::labeled_row(ui, tr("gridset.count"), |ui| field(ui, &th, &format!("{id}c"), count, ru));
        kit::labeled_row(ui, tr("gridset.spacing"), |ui| field(ui, &th, &format!("{id}s"), step, ru));
        kit::labeled_row(ui, tr("gridset.start"), |ui| field(ui, &th, &format!("{id}l"), start, ru));
        ui.add_space(space::XS);
    }
    kit::section_title(ui, tr("gridset.frame"));
    kit::labeled_row(ui, tr("gridset.angle"), |ui| field(ui, &th, "ang", &mut st.angle, ru));
    kit::labeled_row(ui, tr("gridset.origin_x"), |ui| field(ui, &th, "ox", &mut st.ox, ru));
    kit::labeled_row(ui, tr("gridset.origin_y"), |ui| field(ui, &th, "oy", &mut st.oy, ru));
    let preview = lines_of(st, u);
    match &preview {
        Ok(ls) => note(ui, &crate::tr_lang(ru, "gridset.total", &[("n", &ls.len().to_string())])),
        Err(e) => note(ui, &problem(e, ru)),
    }
    if let Some(e) = &st.error {
        note(ui, &problem(e, ru));
    }
    let (mut go, mut cancel) = (false, false);
    let blocked = match &preview {
        Ok(ls) if !ls.is_empty() => None,
        _ => Some(tr("gridset.nothing")),
    };
    kit::dialog_footer(ui, |ui| {
        go = kit::primary_action(ui, tr("gridset.create"), blocked);
        cancel = kit::cancel_button(ui, crate::tr_lang(ru, "dlg.cancel", &[])).clicked();
    });
    if go {
        if let Ok(ls) = preview {
            match create(ed, &ls) {
                Ok(()) => {
                    st.error = None;
                    return true;
                }
                Err(e) => st.error = Some(e),
            }
        }
    }
    cancel
}

/// Called every frame.
pub fn window(ctx: &egui::Context, ed: &mut Editor, ru: bool) {
    let mut st = WIN.with(|w| std::mem::take(&mut *w.borrow_mut()));
    if !st.open {
        WIN.with(|w| *w.borrow_mut() = st);
        return;
    }
    let (mut open, mut done) = (true, false);
    kit::Dialog::new("grid_set", crate::tr_str(ru, "gridset.title"), kit::DialogSize::Tool)
        .open(&mut open)
        .scroll_body()
        .show(ctx, |ui| done = body(ui, &mut st, ed, ru));
    st.open = open && !done;
    if done {
        st.error = None;
    }
    WIN.with(|w| *w.borrow_mut() = st);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::Units;

    #[test]
    fn defaults_make_seven_lines_and_one_undo_step() {
        let mut ed = Editor::new_in_memory().unwrap();
        let ls = lines_of(&State::default(), Units::Metric).unwrap();
        assert_eq!(ls.len(), 7);
        let before = ed.model.grids().unwrap().len();
        create(&mut ed, &ls).unwrap();
        assert_eq!(ed.model.grids().unwrap().len(), before + 7);
        ed.undo();
        assert_eq!(ed.model.grids().unwrap().len(), before);
    }

    #[test]
    fn a_taken_label_refuses_everything() {
        let mut ed = Editor::new_in_memory().unwrap();
        let ls = lines_of(&State::default(), Units::Metric).unwrap();
        create(&mut ed, &ls).unwrap();
        let n = ed.model.grids().unwrap().len();
        let e = create(&mut ed, &ls).unwrap_err();
        assert!(e.starts_with("clash:"), "{e}");
        assert_eq!(ed.model.grids().unwrap().len(), n);
    }

    #[test]
    fn bad_fields_name_themselves() {
        let mut st = State::default();
        st.x_count = "many".into();
        assert_eq!(lines_of(&st, Units::Metric).unwrap_err(), "count");
        let mut st = State::default();
        st.angle = "up".into();
        assert_eq!(lines_of(&st, Units::Metric).unwrap_err(), "angle");
        let mut st = State::default();
        st.y_step = "-".into();
        assert_eq!(lines_of(&st, Units::Metric).unwrap_err(), "spacing");
    }
}
