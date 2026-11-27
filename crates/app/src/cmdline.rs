// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The command line under the canvas. A word is a short alias (`W` wall, `M` move, `RO` rotate)
//! or a registry id and runs that command; while a tool draws, a point is typed: `#x;y` absolute,
//! `@dx;dy` relative, `d<a` / `@d<a` / `<a` polar, `d` along the cursor, any number an expression
//! (`3000*2`, `(6000-200)/2`, `1.5m`). Ctrl+J sends the line to the assistant instead; Up and
//! Down walk the history. Commands go through `EditorApp::run`, so undo and toasts stay theirs.
use crate::editor::Editor;
use crate::expr_field::{eval, Dom, Scope};
use crate::shell::EditorApp;
use crate::units::{DecimalMark, InputError, Units, INCH, PAIR_SEP};
use crate::wall_tool::{Event, Key, Mode};
use egui;
use std::cell::RefCell;

/// Short names a person types, the habits common to CAD command lines. Ids are checked against the
/// registry by a test: an alias whose command is gone fails the build.
pub const ALIASES: &[(&str, &str)] = &[
    ("L", "ann.detail_line"),
    ("LI", "ann.detail_line"),
    ("W", "arch.wall"),
    ("WA", "arch.wall"),
    ("D", "arch.door"),
    ("DR", "arch.door"),
    ("WI", "arch.window"),
    ("WN", "arch.window"),
    ("M", "mod.move"),
    ("MV", "mod.move"),
    ("C", "mod.copy"),
    ("CO", "mod.copy"),
    ("CP", "mod.copy"),
    ("RO", "mod.rotate"),
    ("MI", "mod.mirror"),
    ("MM", "mod.mirror"),
    ("O", "mod.offset"),
    ("OF", "mod.offset"),
    ("TR", "mod.trim"),
    ("EX", "mod.extend"),
    ("SC", "mod.scale"),
    ("AR", "mod.array"),
    ("AL", "mod.align"),
    ("SP", "mod.split"),
    ("E", "mod.delete"),
    ("DEL", "mod.delete"),
    ("DI", "ann.dim"),
    ("DIM", "ann.dim"),
    ("R", "arch.room"),
    ("RM", "arch.room"),
    ("F", "arch.floor"),
    ("FL", "arch.floor"),
    ("CL", "arch.column"),
    ("COL", "arch.column"),
    ("B", "str.beam"),
    ("BM", "str.beam"),
    ("ST", "arch.stair"),
    ("RA", "arch.railing"),
    ("RF", "arch.roof"),
    ("CE", "arch.ceiling"),
    ("CW", "arch.curtain_wall"),
    ("GR", "arch.grid"),
    ("LV", "arch.level"),
    ("LE", "arch.level"),
    ("U", "edit.undo"),
    ("REDO", "edit.redo"),
    ("Z", "view.zoom_fit"),
    ("ZE", "view.zoom_fit"),
];

const HISTORY_MAX: usize = 100;

/// A typed point, lengths in metres and angles in degrees.
#[derive(Clone, Debug, PartialEq)]
pub enum Pt {
    /// `x;y` (`#` makes it absolute even where a pair means a size).
    Pair { x: f64, y: f64, rel: bool, hash: bool },
    /// `d<a`, `<a`, `d`: from the last point, along the cursor when the angle is left out.
    Polar { len: Option<f64>, ang: Option<f64> },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Line {
    Empty,
    /// A registry command id.
    Run(String),
    Point(Pt),
    /// A word that names no command.
    Unknown(String),
}

fn alias(word: &str) -> Option<&'static str> {
    let w = word.to_uppercase();
    ALIASES.iter().find(|(a, _)| *a == w).map(|(_, id)| *id)
}

fn is_point(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_digit() || "@#<-+(.".contains(c))
}

/// Why a point was not read.
#[derive(Clone, Debug, PartialEq)]
pub enum PtErr {
    /// A number in it.
    Input(InputError),
    /// `#` without a pair, a distance that is not positive, nothing after `@`.
    Shape,
}

/// One number of a point: signed, any expression, a bare number in the field unit (mm / in).
fn len(s: &str, u: Units) -> Result<f64, PtErr> {
    eval(s.trim(), Dom::Length(u), &Scope::none()).map_err(PtErr::Input)
}

/// Reads a line. `known` says whether a full registry id exists.
pub fn parse(text: &str, u: Units, known: &dyn Fn(&str) -> bool) -> Result<Line, PtErr> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(Line::Empty);
    }
    if !is_point(t) {
        if let Some(id) = alias(t) {
            return Ok(Line::Run(id.to_string()));
        }
        if known(t) {
            return Ok(Line::Run(t.to_string()));
        }
        return Ok(Line::Unknown(t.to_string()));
    }
    let (hash, t) = match t.strip_prefix('#') {
        Some(r) => (true, r),
        None => (false, t),
    };
    let (rel, t) = match t.strip_prefix('@') {
        Some(r) => (true, r),
        None => (false, t),
    };
    if let Some((a, b)) = t.split_once(PAIR_SEP) {
        return Ok(Line::Point(Pt::Pair { x: len(a, u)?, y: len(b, u)?, rel, hash }));
    }
    if hash {
        return Err(PtErr::Shape);
    }
    let (l, a) = match t.split_once('<') {
        Some((l, a)) => (l, Some(a)),
        None => (t, None),
    };
    let len = if l.trim().is_empty() { None } else { Some(len(l, u)?) };
    if len.is_some_and(|d| d <= 0.0) {
        return Err(PtErr::Shape);
    }
    let ang = match a {
        Some(a) => Some(eval(a.trim(), Dom::Angle, &Scope::none()).map_err(PtErr::Input)?),
        None => None,
    };
    if len.is_none() && ang.is_none() {
        return Err(PtErr::Shape);
    }
    Ok(Line::Point(Pt::Polar { len, ang }))
}

/// A number as the tool reads its own typed text: the field unit, the language's decimal mark.
fn field_num(m: f64, u: Units, mark: DecimalMark) -> String {
    let v = match u {
        Units::Metric => m * 1000.0,
        Units::Imperial => m / INCH,
    };
    let s = format!("{:.4}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    let s = if s == "-0" { "0".to_string() } else { s };
    match mark {
        DecimalMark::Comma => s.replace('.', ","),
        _ => s,
    }
}

/// What the point is for the tool: an absolute click, or the text it would have been typed as.
#[derive(Clone, Debug, PartialEq)]
pub enum Feed {
    Click(f64, f64),
    Typed(String),
}

pub fn feed_of(p: &Pt, rect: bool, u: Units, mark: DecimalMark) -> Feed {
    let n = |m: f64| field_num(m, u, mark);
    match *p {
        // a bare pair in the rectangle tool is its size, as typed there
        Pt::Pair { x, y, rel, hash } if rel || (rect && !hash) => {
            Feed::Typed(format!("{}{}{PAIR_SEP}{}", if rel { "@" } else { "" }, n(x), n(y)))
        }
        Pt::Pair { x, y, .. } => Feed::Click(x, y),
        Pt::Polar { len, ang } => {
            let mut s = len.map(n).unwrap_or_default();
            if let Some(a) = ang {
                s += "<";
                s += &field_num(a / 1000.0, Units::Metric, mark);
            }
            Feed::Typed(s)
        }
    }
}

/// Sends a point to the drawing tool as if typed or clicked there.
pub fn feed(ed: &mut Editor, f: &Feed) {
    match f {
        Feed::Click(x, y) => {
            let p = ddc_geom::v2(*x, *y);
            ed.handle(Event::Move(p));
            ed.handle(Event::Click(p));
        }
        Feed::Typed(s) => {
            ed.wall_draft_mut().input.clear();
            for c in s.chars() {
                ed.handle(Event::Char(c));
            }
            ed.handle(Event::Key(Key::Enter));
        }
    }
}

#[derive(Default)]
pub struct State {
    pub text: String,
    /// The line goes to the assistant.
    pub ai: bool,
    pub history: Vec<String>,
    /// Position while walking the history; `None` at the fresh line.
    at: Option<usize>,
    /// The last line's answer, shown at the right.
    pub note: String,
}

impl State {
    pub fn remember(&mut self, s: &str) {
        self.at = None;
        if s.trim().is_empty() || self.history.last().is_some_and(|l| l == s) {
            return;
        }
        self.history.push(s.to_string());
        if self.history.len() > HISTORY_MAX {
            self.history.remove(0);
        }
    }
    /// Up: one line further back.
    pub fn back(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let i = self.at.map_or(self.history.len() - 1, |i| i.saturating_sub(1));
        self.at = Some(i);
        self.text = self.history[i].clone();
    }
    /// Down: one line forward, the empty line after the newest.
    pub fn forward(&mut self) {
        match self.at {
            Some(i) if i + 1 < self.history.len() => {
                self.at = Some(i + 1);
                self.text = self.history[i + 1].clone();
            }
            _ => {
                self.at = None;
                self.text.clear();
            }
        }
    }
}

thread_local! {
    static ST: RefCell<State> = RefCell::new(State::default());
}

fn tool_waits(app: &EditorApp) -> bool {
    app.ed.tool_armed() || app.ed.opening_mode || app.active_command().is_some()
}

/// Runs one line; returns the note to show.
pub fn submit(app: &mut EditorApp, text: &str, ai: bool) -> String {
    let t = text.trim();
    if t.is_empty() {
        return String::new();
    }
    if ai {
        crate::ai::ui::ask(app, t);
        return crate::tr("cmdl.sent", &[]);
    }
    let reg = &app.sh.reg;
    let line = parse(t, app.ed.tool.units, &|id| reg.get(id).is_some());
    match line {
        Ok(Line::Empty) => String::new(),
        Ok(Line::Run(id)) => {
            if app.run(&id) {
                app.sh.cat.t(&format!("cmd.{id}"))
            } else {
                crate::tr("cmdl.failed", &[("text", t)])
            }
        }
        Ok(Line::Point(p)) => {
            if !tool_waits(app) {
                return crate::tr("cmdl.no_tool", &[]);
            }
            let rect = app.ed.wall_mode() == Mode::Rect;
            let f = feed_of(&p, rect, app.ed.tool.units, app.ed.tool.mark);
            feed(&mut app.ed, &f);
            app.ed.status.clone().unwrap_or_default()
        }
        Ok(Line::Unknown(w)) => crate::tr("cmdl.unknown", &[("text", &w)]),
        Err(PtErr::Input(e)) => {
            let why = crate::expr_field::error_text(&app.sh.cat, &e);
            crate::tr("cmdl.bad", &[("text", t), ("e", &why)])
        }
        Err(PtErr::Shape) => crate::tr("cmdl.shape", &[("text", t)]),
    }
}

pub const FIELD_ID: &str = "ddc.cmdline.field";

/// The bar itself, one row above the status line.
pub fn panel(app: &mut EditorApp, ctx: &egui::Context) {
    let th = app.sh.theme.clone();
    egui::TopBottomPanel::bottom("ddc.cmdline")
        .frame(egui::Frame::new().fill(th.c("bg.app")))
        .show_separator_line(false)
        .exact_height(ddc_ui::widgets::bars::STATUS_H)
        .show(ctx, |ui| ui.horizontal_centered(|ui| bar(app, ui, &th)));
}

fn bar(app: &mut EditorApp, ui: &mut egui::Ui, th: &ddc_ui::Theme) {
    let mut st = ST.with(|s| std::mem::take(&mut *s.borrow_mut()));
    let mode = if st.ai { crate::tr("cmdl.mode_ai", &[]) } else { crate::tr("cmdl.mode_cmd", &[]) };
    let chip = if st.ai { ddc_ui::widgets::Button::secondary(&mode) } else { ddc_ui::widgets::Button::quiet(&mode) };
    if chip
        .small()
        .show(ui, th)
        .on_hover_text(crate::tr("cmdl.toggle", &[]))
        .clicked()
    {
        st.ai = !st.ai;
    }
    let hint = if st.ai { crate::tr("cmdl.hint_ai", &[]) } else { crate::tr("cmdl.hint", &[]) };
    let note_w = if st.note.is_empty() { 0.0 } else { ui.available_width() * 0.35 };
    let id = egui::Id::new(FIELD_ID);
    let r = ui.add(
        egui::TextEdit::singleline(&mut st.text)
            .id(id)
            .hint_text(hint)
            .desired_width((ui.available_width() - note_w).max(80.0)),
    );
    let mut go = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    if r.has_focus() {
        let m = egui::Modifiers::COMMAND;
        if ui.input_mut(|i| i.consume_key(m, egui::Key::J)) {
            // with text: that line goes to the assistant; empty: just switch the mode
            if st.text.trim().is_empty() {
                st.ai = !st.ai;
            } else {
                st.ai = true;
                go = true;
            }
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)) {
            st.back();
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)) {
            st.forward();
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            st.text.clear();
            r.surrender_focus();
        }
    }
    if go {
        let text = std::mem::take(&mut st.text);
        st.remember(&text);
        st.note = submit(app, &text, st.ai);
        // the next point or command is typed straight on
        ui.memory_mut(|m| m.request_focus(id));
    }
    if !st.note.is_empty() {
        ui.label(egui::RichText::new(&st.note).color(th.c("text.secondary")));
    } else if !app.sh.echo.is_empty() {
        ui.label(egui::RichText::new(&app.sh.echo).color(th.c("text.secondary")));
    }
    ST.with(|s| *s.borrow_mut() = st);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<Line, PtErr> {
        parse(s, Units::Metric, &|id| id == "arch.wall_room")
    }

    #[test]
    fn aliases_and_ids_name_commands() {
        assert_eq!(p(" w "), Ok(Line::Run("arch.wall".into())));
        assert_eq!(p("RO"), Ok(Line::Run("mod.rotate".into())));
        assert_eq!(p("l"), Ok(Line::Run("ann.detail_line".into())));
        assert_eq!(p("arch.wall_room"), Ok(Line::Run("arch.wall_room".into())));
        assert_eq!(p("make a hall"), Ok(Line::Unknown("make a hall".into())));
        assert_eq!(p(""), Ok(Line::Empty));
    }

    #[test]
    fn every_alias_is_a_registry_command() {
        let reg = ddc_ui::commands::Registry::builtin();
        for (a, id) in ALIASES {
            assert!(reg.get(id).is_some(), "{a} -> {id}");
            assert_eq!(a.to_uppercase(), *a);
        }
        let mut names: Vec<_> = ALIASES.iter().map(|x| x.0).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), ALIASES.len(), "an alias names one command");
    }

    #[test]
    fn points_absolute_relative_polar_with_math() {
        let near = |a: f64, b: f64| (a - b).abs() < 1e-9;
        let Ok(Line::Point(Pt::Pair { x, y, rel: true, hash: false })) = p("@1000;-500") else { panic!() };
        assert!(near(x, 1.0) && near(y, -0.5));
        let Ok(Line::Point(Pt::Pair { x, y, rel: false, hash: true })) = p("#3000*2;(6000-200)/2") else { panic!() };
        assert!(near(x, 6.0) && near(y, 2.9));
        let Ok(Line::Point(Pt::Polar { len: Some(l), ang: Some(a) })) = p("@3500<45+45") else { panic!() };
        assert!(near(l, 3.5) && near(a, 90.0));
        assert_eq!(p("<30"), Ok(Line::Point(Pt::Polar { len: None, ang: Some(30.0) })));
        let Ok(Line::Point(Pt::Polar { len: Some(l), ang: None })) = p("2.5m") else { panic!() };
        assert!(near(l, 2.5));
        assert!(p("@1000;abc").is_err());
        assert!(p("-200").is_err(), "a distance is positive");
        assert!(p("#1000").is_err());
    }

    #[test]
    fn a_point_reaches_the_tool_as_click_or_its_own_typing() {
        let pair = |rel, hash| Pt::Pair { x: 1.5, y: -0.25, rel, hash };
        let (u, dot, comma) = (Units::Metric, DecimalMark::Point, DecimalMark::Comma);
        assert_eq!(feed_of(&pair(false, false), false, u, dot), Feed::Click(1.5, -0.25));
        assert_eq!(feed_of(&pair(true, false), false, u, dot), Feed::Typed("@1500;-250".into()));
        // in the rectangle tool a bare pair is the size; # still clicks a corner
        assert_eq!(feed_of(&pair(false, false), true, u, dot), Feed::Typed("1500;-250".into()));
        assert_eq!(feed_of(&pair(false, true), true, u, dot), Feed::Click(1.5, -0.25));
        let polar = Pt::Polar { len: Some(1.2345), ang: Some(22.5) };
        assert_eq!(feed_of(&polar, false, u, comma), Feed::Typed("1234,5<22,5".into()));
        assert_eq!(feed_of(&Pt::Polar { len: None, ang: Some(90.0) }, false, u, dot), Feed::Typed("<90".into()));
        let inch = feed_of(&Pt::Polar { len: Some(INCH * 12.0), ang: None }, false, Units::Imperial, dot);
        assert_eq!(inch, Feed::Typed("12".into()));
    }

    #[test]
    fn typed_relative_point_draws_a_wall_segment() {
        let mut ed = Editor::new_in_memory().unwrap();
        ed.set_mode(Mode::Polyline);
        feed(&mut ed, &Feed::Click(0.0, 0.0));
        let f = feed_of(&Pt::Pair { x: 4.0, y: 0.0, rel: true, hash: false }, false, ed.tool.units, ed.tool.mark);
        feed(&mut ed, &f);
        assert_eq!(ed.wall_draft().points.len(), 2, "{:?}", ed.wall_draft().points);
        assert!((ed.wall_draft().points[1].x - 4.0).abs() < 1e-9 && ed.wall_draft().points[1].y.abs() < 1e-9);
    }

    #[test]
    fn history_walks_back_and_forward() {
        let mut s = State::default();
        for l in ["W", "W", "@1000;0", "M"] {
            s.remember(l);
        }
        assert_eq!(s.history, ["W", "@1000;0", "M"]);
        s.back();
        assert_eq!(s.text, "M");
        s.back();
        s.back();
        s.back();
        assert_eq!(s.text, "W");
        s.forward();
        assert_eq!(s.text, "@1000;0");
        s.forward();
        s.forward();
        assert_eq!(s.text, "");
    }
}
