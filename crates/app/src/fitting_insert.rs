//! `ctx.mep.fitting` ("Insert fitting"): puts a fitting on the selected run. A transition cuts
//! the run at a distance from its start and gives the rest another size (a catalog DN for a
//! pipe, a section for a duct, tray or conduit); an end cap closes an open end. Each is one
//! undo step; the network checks the fitting fits and refuses it whole when it does not.

use crate::editor::{Editor, OpFn, Sel};
use crate::shell::EditorApp;
use crate::status::Severity;
use crate::ui_kit as kit;
use ddc_geom::mep::{Domain, Shape};
use ddc_model::{Error, MepNewSize, RunSpec};
use ddc_ui::theme::space;
use ddc_ui::widgets;
use ddc_ui::Catalog;
use std::cell::RefCell;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Kind {
    #[default]
    Transition,
    Cap,
}

#[derive(Default)]
pub struct Win {
    pub open: bool,
    pub run: i64,
    pub kind: Kind,
    /// Distance of the transition from the run start, m.
    pub at: f64,
    /// Index into the other catalog sizes of a pipe.
    pub dn: usize,
    /// New section of a duct, tray or conduit: diameter, or width and height, m.
    pub w: f64,
    pub h: f64,
    /// 0: start, 1: end.
    pub end: u8,
    pub note: Option<String>,
}

thread_local! {
    static WIN: RefCell<Win> = RefCell::new(Win::default());
}

/// Catalog DNs a pipe run can change to: every size of its material but its own.
pub fn other_dns(s: &RunSpec) -> Vec<u32> {
    let Some(mat) = s.material else {
        return vec![];
    };
    mat.sizes().into_iter().map(|z| z.dn).filter(|dn| Some(*dn) != s.dn).collect()
}

/// The editor for run `id`: a transition at mid-length, one size down.
pub fn win_for(ed: &Editor, id: i64) -> Result<Win, Error> {
    let s = ed.model.mep_run(id)?;
    let len = ddc_model::run_axis_length(&s);
    let dns = other_dns(&s);
    // the next smaller catalog size, else the first other one
    let dn = s.dn.and_then(|d| dns.iter().rposition(|x| *x < d)).unwrap_or(0);
    let (w, h) = s.section.shape.wh();
    Ok(Win {
        open: true,
        run: id,
        kind: Kind::Transition,
        at: (len / 2.0 * 100.0).round() / 100.0,
        dn,
        w: (w * 0.75 * 1000.0).round() / 1000.0,
        h: (h * 0.75 * 1000.0).round() / 1000.0,
        end: if s.caps.1 && !s.caps.0 { 0 } else { 1 },
        note: None,
    })
}

/// Section of the same kind as `old` with the sizes of the window.
pub fn new_shape(old: Shape, w: f64, h: f64) -> Shape {
    match old {
        Shape::Round { .. } => Shape::Round { d: w },
        Shape::Rect { .. } => Shape::Rect { w, h },
        Shape::Oval { .. } => Shape::Oval { w, h },
    }
}

/// Inserts what the window describes as one undo step.
pub fn apply(ed: &mut Editor, w: &Win) -> Result<(), Error> {
    let s = ed.model.mep_run(w.run)?;
    let run = w.run;
    match w.kind {
        Kind::Cap => {
            let (end, on) = (w.end, !if w.end == 0 { s.caps.0 } else { s.caps.1 });
            ed.run_op(OpFn(Arc::new(move |m| m.mep_cap_end(run, end, on))))
        }
        Kind::Transition => {
            let size = if s.section.domain == Domain::Pipe {
                let dn = *other_dns(&s)
                    .get(w.dn)
                    .ok_or_else(|| Error::Invalid("mep.err.no_size".into()))?;
                MepNewSize::Dn(dn)
            } else {
                MepNewSize::Shape(new_shape(s.section.shape, w.w, w.h))
            };
            let at = w.at;
            ed.run_op(OpFn(Arc::new(move |m| m.mep_insert_transition(run, at, size).map(drop))))
        }
    }
}

fn say_error(cat: &Catalog, e: &Error) -> String {
    match e {
        Error::Invalid(k) if k.starts_with("mep.err.") => cat.t(k),
        e => cat.tf("fit.err.other", &[("e", &e.to_string())]),
    }
}

fn size_text(s: &RunSpec, ru: bool, u: crate::units::Units) -> String {
    match (s.dn, s.section.shape) {
        (Some(dn), _) => format!("DN {dn}"),
        (None, Shape::Round { d }) => format!("\u{2300} {}", crate::unit_text::len(d, ru, u)),
        (None, sh) => {
            let (w, h) = sh.wh();
            let l = |m: f64| crate::unit_text::len(m, ru, u);
            format!("{} \u{d7} {}", l(w), l(h))
        }
    }
}

fn transition_rows(
    ui: &mut egui::Ui,
    w: &mut Win,
    s: &RunSpec,
    len: f64,
    ed: &Editor,
    cat: &Catalog,
) {
    let th = ddc_ui::theme::of(ui.ctx());
    let ru = cat.lang == ddc_ui::Lang::Ru;
    let u = ed.tool.units;
    let at = cat.t("fit.at");
    kit::labeled_row_id(ui, &at, |ui, lid| {
        kit::len_field(ui, &th, egui::Id::new("fit.at"), &mut w.at, u, 0.0, len, ru, lid, &at);
    });
    if s.section.domain == Domain::Pipe {
        let dns = other_dns(s);
        let items: Vec<String> = dns.iter().map(|d| format!("DN {d}")).collect();
        let items: Vec<&str> = items.iter().map(String::as_str).collect();
        if items.is_empty() {
            kit::warn_note(ui, cat.t("mep.err.no_size"));
        } else {
            w.dn = w.dn.min(items.len() - 1);
            kit::labeled_row(ui, &cat.t("fit.new_dn"), |ui| {
                widgets::dropdown(ui, &th, "fit.dn", &mut w.dn, &items, None);
            });
        }
        return;
    }
    let round = matches!(s.section.shape, Shape::Round { .. });
    let wl = cat.t(if round { "fit.diameter" } else { "fit.width" });
    kit::labeled_row_id(ui, &wl, |ui, lid| {
        kit::len_field(ui, &th, egui::Id::new("fit.w"), &mut w.w, u, 0.005, 10.0, ru, lid, &wl);
    });
    if !round {
        let hl = cat.t("fit.height");
        kit::labeled_row_id(ui, &hl, |ui, lid| {
            kit::len_field(ui, &th, egui::Id::new("fit.h"), &mut w.h, u, 0.005, 10.0, ru, lid, &hl);
        });
    }
}

/// Window body (apart from the frame, for the layout probes). `true`: close.
pub fn body(ui: &mut egui::Ui, w: &mut Win, ed: &mut Editor, cat: &Catalog) -> bool {
    let th = ddc_ui::theme::of(ui.ctx());
    let ru = cat.lang == ddc_ui::Lang::Ru;
    let Ok(s) = ed.model.mep_run(w.run) else {
        return true;
    };
    let len = ddc_model::run_axis_length(&s);
    ui.add(egui::Label::new(cat.t("fit.lead")).wrap());
    ui.add_space(space::XS);
    let u = ed.tool.units;
    kit::props(ui, "fit.run", |ui| {
        kit::kv(ui, &cat.t("fit.size"), size_text(&s, ru, u));
        kit::kv(ui, &cat.t("fit.length"), crate::unit_text::len(len, ru, u));
    });
    ui.add_space(space::SM);
    let items = [cat.t("fit.transition"), cat.t("fit.cap")];
    let items: Vec<&str> = items.iter().map(String::as_str).collect();
    let at = usize::from(w.kind == Kind::Cap);
    if let Some(i) = widgets::segmented(ui, &th, &items, at) {
        w.kind = if i == 1 { Kind::Cap } else { Kind::Transition };
        w.note = None;
    }
    ui.add_space(space::XS);
    let label = match w.kind {
        Kind::Transition => {
            transition_rows(ui, w, &s, len, ed, cat);
            cat.t("fit.insert")
        }
        Kind::Cap => {
            let ends = [cat.t("fit.start"), cat.t("fit.end")];
            let ends: Vec<&str> = ends.iter().map(String::as_str).collect();
            kit::labeled_row(ui, &cat.t("fit.which_end"), |ui| {
                if let Some(i) = widgets::segmented(ui, &th, &ends, usize::from(w.end)) {
                    w.end = i as u8;
                }
            });
            let on = if w.end == 0 { s.caps.0 } else { s.caps.1 };
            kit::props(ui, "fit.cap.now", |ui| {
                let v = cat.t(if on { "fit.capped" } else { "fit.open" });
                kit::kv(ui, &cat.t("fit.now"), v);
            });
            cat.t(if on { "fit.uncap" } else { "fit.add_cap" })
        }
    };
    if let Some(n) = &w.note {
        kit::error_note(ui, n);
    }
    let (mut go, mut close) = (false, false);
    kit::dialog_footer(ui, |ui| {
        go = kit::primary_action(ui, label, None);
        close = kit::cancel_button(ui, cat.t("dlg.close")).clicked();
    });
    if go {
        match apply(ed, w) {
            Ok(()) => {
                w.note = None;
                if w.kind == Kind::Transition {
                    return true;
                }
            }
            Err(e) => w.note = Some(say_error(cat, &e)),
        }
    }
    close
}

fn selected_run(ed: &Editor) -> Option<i64> {
    let runs: Vec<i64> = ed
        .selection
        .iter()
        .filter_map(|s| if let Sel::Mep(id) = s { Some(*id) } else { None })
        .collect();
    match runs[..] {
        [id] if ed.model.mep_run(id).is_ok() => Some(id),
        _ => None,
    }
}

fn run(a: &mut EditorApp) {
    let Some(id) = selected_run(&a.ed) else {
        a.ed.say(Severity::Refusal, a.sh.cat.t("fit.err.one"));
        return;
    };
    match win_for(&a.ed, id) {
        Ok(w) => WIN.with(|s| *s.borrow_mut() = w),
        Err(e) => {
            let m = say_error(&a.sh.cat, &e);
            a.ed.say(Severity::Refusal, m);
        }
    }
}

pub fn open_for_layout(ed: &Editor, id: i64, kind: Kind) {
    let mut w = win_for(ed, id).expect("run");
    w.kind = kind;
    WIN.with(|s| *s.borrow_mut() = w);
}

/// Called every frame.
pub fn frame(a: &mut EditorApp, ctx: &egui::Context) {
    let mut w = WIN.with(|w| std::mem::take(&mut *w.borrow_mut()));
    if !w.open || a.ed.model.mep_run(w.run).is_err() {
        return;
    }
    let (mut open, mut close) = (true, false);
    kit::Dialog::new("mep_fitting", a.sh.cat.t("cmd.ctx.mep.fitting"), kit::DialogSize::Form)
        .open(&mut open)
        .scroll_body()
        .show(ctx, |ui| close = body(ui, &mut w, &mut a.ed, &a.sh.cat));
    w.open = open && !close;
    WIN.with(|s| *s.borrow_mut() = w);
}

pub fn register(a: &mut EditorApp) {
    a.register_command("ctx.mep.fitting", run);
}

/// A level, a supply air system and an 8 m round duct at 3 m: (level, run).
#[cfg(test)]
pub(crate) fn test_duct(ed: &mut Editor) -> i64 {
    use ddc_geom::mep::{Section, SystemKind};
    let sa = ed.model.add_mep_system("SA", SystemKind::SupplyAir).unwrap();
    let sec = Section::new(Domain::Duct, Shape::Round { d: 0.4 });
    let spec = RunSpec::new(sa, ed.level, sec, vec![(0.0, 0.0, 3.0), (8.0, 0.0, 3.0)]);
    let r = ed.model.add_mep_run(&spec).unwrap();
    ed.reload().unwrap();
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_geom::mep::{FittingKind, PipeMaterial, SystemKind};
    use ddc_ui::Lang;

    #[test]
    fn a_transition_is_one_undo_step_and_takes_the_typed_section() {
        let mut ed = Editor::new_in_memory().unwrap();
        let r = test_duct(&mut ed);
        let mut w = win_for(&ed, r).unwrap();
        assert_eq!(w.at, 4.0);
        assert!((w.w - 0.3).abs() < 1e-9);
        w.at = 5.0;
        w.w = 0.25;
        apply(&mut ed, &w).unwrap();
        let runs = ed.model.mep_runs(None).unwrap();
        assert_eq!(runs.len(), 2);
        let b = ed.model.mep_run(runs[1]).unwrap();
        assert_eq!(b.section.shape, Shape::Round { d: 0.25 });
        let net = ed.model.mep_network(None).unwrap();
        assert_eq!(net.count(FittingKind::Transition), 1);
        ed.undo();
        assert_eq!(ed.model.mep_runs(None).unwrap(), vec![r]);
    }

    #[test]
    fn a_pipe_goes_one_catalog_size_down() {
        let mut ed = Editor::new_in_memory().unwrap();
        let hw = ed.model.add_mep_system("HW", SystemKind::Heating).unwrap();
        let pts = vec![(0.0, 0.0, 3.0), (6.0, 0.0, 3.0)];
        let s = RunSpec::pipe(hw, ed.level, PipeMaterial::Steel, 100, pts).unwrap();
        let r = ed.model.add_mep_run(&s).unwrap();
        ed.reload().unwrap();
        let w = win_for(&ed, r).unwrap();
        let dns = other_dns(&ed.model.mep_run(r).unwrap());
        assert!(!dns.contains(&100));
        let dn = dns[w.dn];
        assert!(dn < 100 && dns.iter().filter(|d| **d < 100).all(|d| *d <= dn), "{dn}");
        apply(&mut ed, &w).unwrap();
        let b = ed.model.mep_runs(None).unwrap()[1];
        assert_eq!(ed.model.mep_run(b).unwrap().dn, Some(dn));
    }

    #[test]
    fn a_cap_toggles_on_the_chosen_end() {
        let mut ed = Editor::new_in_memory().unwrap();
        let r = test_duct(&mut ed);
        let mut w = win_for(&ed, r).unwrap();
        w.kind = Kind::Cap;
        w.end = 0;
        apply(&mut ed, &w).unwrap();
        assert_eq!(ed.model.mep_run(r).unwrap().caps, (true, false));
        apply(&mut ed, &w).unwrap();
        assert_eq!(ed.model.mep_run(r).unwrap().caps, (false, false));
        ed.undo();
        assert_eq!(ed.model.mep_run(r).unwrap().caps, (true, false));
    }

    #[test]
    fn refusals_are_said_in_the_person_s_language() {
        let mut ed = Editor::new_in_memory().unwrap();
        let r = test_duct(&mut ed);
        let mut w = win_for(&ed, r).unwrap();
        w.at = 8.0;
        let e = apply(&mut ed, &w).unwrap_err();
        for lang in [Lang::En, Lang::Ru] {
            let cat = Catalog::new(lang);
            let said = say_error(&cat, &e);
            assert_eq!(said, cat.t("mep.err.cut_ends"));
            assert_ne!(said, "mep.err.cut_ends");
        }
        assert_eq!(ed.model.mep_runs(None).unwrap(), vec![r], "nothing written");
    }

    #[test]
    fn the_command_needs_one_run() {
        let mut a = crate::layout_contract::app_in(Lang::En);
        assert!(a.run("ctx.mep.fitting"));
        assert!(!WIN.with(|w| w.borrow().open));
        let r = test_duct(&mut a.ed);
        a.ed.selection = vec![Sel::Mep(r)];
        a.run("ctx.mep.fitting");
        assert!(WIN.with(|w| w.borrow().open && w.borrow().run == r));
        WIN.with(|w| w.borrow_mut().open = false);
    }

    #[test]
    fn every_text_is_in_both_languages() {
        for lang in [Lang::En, Lang::Ru] {
            let cat = Catalog::new(lang);
            for k in [
                "fit.lead", "fit.size", "fit.length", "fit.transition", "fit.cap", "fit.at",
                "fit.new_dn", "fit.diameter", "fit.width", "fit.height", "fit.insert",
                "fit.start", "fit.end", "fit.which_end", "fit.capped", "fit.open", "fit.now",
                "fit.uncap", "fit.add_cap", "fit.err.one", "fit.err.other", "mep.err.cut_ends",
                "mep.err.cut_device", "mep.err.same_size", "mep.err.no_size", "mep.err.no_room",
            ] {
                assert_ne!(cat.t(k), k, "{lang:?} {k}");
            }
        }
    }
}
