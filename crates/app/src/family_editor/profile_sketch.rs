//! Profile sketch panel (Solids tab): points, lines (construction per entity and per tool),
//! rules and typed dimensions over `ddc_geom::sketch_dims`. A duplicate dimension turns
//! driven instead of locking the sketch; a conflict marks its whole set and offers
//! "make driven" or delete. Each user action is one undo step of the panel's own history;
//! the result goes to the solid profile field only through "Use as profile".

use std::collections::HashMap;

use ddc_geom::component::doc::mm;
use ddc_geom::component::expr::Q;
use ddc_geom::component::{Axis, Kind};
use ddc_geom::sketch_dims::{Analysis, ProfileSketch, ValueError};
use ddc_geom::sketch_solve::{Geo, Verdict};
use ddc_ui::widgets::field::NumberFormat;
use ddc_ui::widgets::{self, Button, TextField};
use ddc_ui::{Catalog, Theme};
use egui::{self, RichText};

use super::tabs::{axis_key, model_field};
use super::{Project, Session};

/// Rule kinds offered by the form: (i18n key, takes two operands, operands are lines).
pub const RULES: [(&str, bool, bool); 6] = [
    ("profile_sketch.rule.horizontal", false, true),
    ("profile_sketch.rule.vertical", false, true),
    ("profile_sketch.rule.coincident", true, false),
    ("profile_sketch.rule.parallel", true, true),
    ("profile_sketch.rule.perp", true, true),
    ("profile_sketch.rule.equal", true, true),
];

/// i18n key of the refusal for unknown point or line numbers.
const BAD_POINTS: &str = "profile_sketch.err.points";

type Snap = (ProfileSketch, Option<Analysis>);

#[derive(Default)]
pub struct Panel {
    pub sk: ProfileSketch,
    pub last: Option<Analysis>,
    undo: Vec<Snap>,
    redo: Vec<Snap>,
    /// New lines are construction lines.
    pub construction_tool: bool,
    /// Point form, metres (the fields show project units).
    pub px: f64,
    pub py: f64,
    pub la: String,
    pub lb: String,
    pub r_kind: usize,
    pub r_a: String,
    pub r_b: String,
    pub da: String,
    pub db: String,
    /// Dimension label position, metres.
    pub dlx: f64,
    pub dly: f64,
    /// Dimension whose value field is open, and its text.
    pub editing: Option<i64>,
    pub edit_text: String,
    focus: bool,
    /// Reason under the value field or the forms (translated).
    pub err: Option<String>,
    /// Visible rect of the panel last frame; Ctrl+Z/Ctrl+Y route here while the pointer or
    /// focus is inside it. `None` while the panel is not drawn (other tab, collapsed).
    pub area: Option<egui::Rect>,
}

/// Captions of the point fields: the catalog's axis names.
fn point_labels(cat: &Catalog) -> (String, String) {
    (cat.t(axis_key(Axis::X)), cat.t(axis_key(Axis::Y)))
}

/// i18n key and argument for a refused value.
pub fn value_err(e: &ValueError) -> (&'static str, String) {
    match e {
        ValueError::UnknownDim => ("profile_sketch.err.unknown", String::new()),
        ValueError::Driven => ("profile_sketch.err.driven", String::new()),
        ValueError::Parse(m) => ("profile_sketch.err.parse", m.clone()),
        ValueError::NotLength => ("profile_sketch.err.not_length", String::new()),
        ValueError::Negative => ("profile_sketch.err.negative", String::new()),
    }
}

impl Panel {
    /// One user action = one undo step; a failed or no-op action leaves no step and no change.
    pub fn act<E>(
        &mut self,
        f: impl FnOnce(&mut ProfileSketch) -> Result<Analysis, E>,
    ) -> Result<(), E> {
        let before = (self.sk.clone(), self.last.clone());
        match f(&mut self.sk) {
            Ok(an) => {
                if self.sk != before.0 {
                    self.undo.push(before);
                    self.redo.clear();
                }
                self.last = Some(an);
                Ok(())
            }
            Err(e) => {
                self.sk = before.0;
                Err(e)
            }
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Nothing drawn: closing loses no work.
    pub fn is_empty(&self) -> bool {
        let sk = &self.sk;
        sk.pts.is_empty() && sk.segs.is_empty() && sk.rules.is_empty() && sk.dims.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self) -> bool {
        let Some(s) = self.undo.pop() else {
            return false;
        };
        let cur = (std::mem::replace(&mut self.sk, s.0), self.last.take());
        self.last = s.1;
        self.redo.push(cur);
        self.editing = None;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(s) = self.redo.pop() else {
            return false;
        };
        let cur = (std::mem::replace(&mut self.sk, s.0), self.last.take());
        self.last = s.1;
        self.undo.push(cur);
        self.editing = None;
        true
    }

    /// Places a dimension and opens its value field at once.
    pub fn place_dim(&mut self, a: usize, b: usize, label: [f64; 2]) -> Option<i64> {
        let mut tag = None;
        let r = self.act(|sk| {
            let (t, an) = sk.place_dim(a, b, label).ok_or(())?;
            tag = Some(t);
            Ok::<_, ()>(an)
        });
        let tag = r.ok().and(tag)?;
        let d = self.sk.dim(tag)?;
        // a dimension that went driven at once only shows its measured value
        if !d.driven {
            self.editing = Some(tag);
            self.edit_text = d.text.clone();
            self.focus = true;
        }
        Some(tag)
    }

    /// Commits the open value field; on error the field stays open with the reason.
    pub fn commit(&mut self, names: &dyn Fn(&str) -> Option<Q>) -> Result<(), ValueError> {
        let Some(tag) = self.editing else {
            return Ok(());
        };
        let text = self.edit_text.clone();
        self.act(|sk| sk.set_value(tag, &text, names))?;
        self.editing = None;
        Ok(())
    }

    /// Adds the point typed in the form: one undo step.
    pub fn add_point(&mut self) {
        let (x, y) = (self.px, self.py);
        let _ = self.act(|sk| {
            sk.point(x, y);
            Ok::<_, ()>(sk.analyze())
        });
        self.err = None;
    }

    /// Adds the line between the typed points; a bad index adds nothing.
    pub fn add_line(&mut self) -> Result<(), &'static str> {
        let n = self.sk.pts.len();
        match (idx(&self.la), idx(&self.lb)) {
            (Some(a), Some(b)) if a < n && b < n && a != b => {
                let c = self.construction_tool;
                let _ = self.act(|sk| {
                    sk.seg(a, b, c);
                    Ok::<_, ()>(sk.analyze())
                });
                self.err = None;
                Ok(())
            }
            _ => Err(BAD_POINTS),
        }
    }

    /// Adds the rule of the form; unknown operands add nothing.
    pub fn add_rule(&mut self) -> Result<(), &'static str> {
        let (_, two, _) = RULES[self.r_kind.min(RULES.len() - 1)];
        let a = idx(&self.r_a);
        let b = if two { idx(&self.r_b) } else { a };
        let geo = a.zip(b).and_then(|(a, b)| rule_geo(self.r_kind, a, b, &self.sk));
        let g = geo.ok_or(BAD_POINTS)?;
        let _ = self.act(|sk| {
            sk.rule(g);
            Ok::<_, ()>(sk.analyze())
        });
        self.err = None;
        Ok(())
    }

    /// Places the dimension of the form and opens its value field.
    pub fn add_dim(&mut self) -> Result<(), &'static str> {
        let n = self.sk.pts.len();
        match (idx(&self.da), idx(&self.db)) {
            (Some(a), Some(b)) if a < n && b < n && a != b => {
                self.err = None;
                let at = [self.dlx, self.dly];
                self.place_dim(a, b, at).map(|_| ()).ok_or(BAD_POINTS)
            }
            _ => Err(BAD_POINTS),
        }
    }

    /// Live degrees of freedom and verdict, for the status line.
    pub fn status(&self) -> Option<(usize, Verdict)> {
        if self.sk.pts.is_empty() {
            return None;
        }
        self.last.as_ref().map(|a| (a.dof, a.verdict))
    }
}

fn idx(s: &str) -> Option<usize> {
    s.trim()
        .trim_start_matches(['P', 'p', 'L', 'l'])
        .parse()
        .ok()
}

fn rule_geo(kind: usize, a: usize, b: usize, sk: &ProfileSketch) -> Option<Geo> {
    let line = |k: usize| sk.segs.get(k).map(|s| (s.a, s.b));
    let (_, _, lines) = *RULES.get(kind)?;
    if !lines && (a >= sk.pts.len() || b >= sk.pts.len()) {
        return None;
    }
    Some(match kind {
        0 => Geo::Horizontal(line(a)?),
        1 => Geo::Vertical(line(a)?),
        2 => Geo::Coincident(a, b),
        3 => Geo::Parallel(line(a)?, line(b)?),
        4 => Geo::Perp(line(a)?, line(b)?),
        _ => Geo::EqualLen(line(a)?, line(b)?),
    })
}

/// Status-line text: degrees of freedom with the verdict colour.
pub fn status_label(ui: &mut egui::Ui, th: &Theme, cat: &Catalog, p: &Panel) {
    let Some((dof, v)) = p.status() else {
        return;
    };
    let col = match v {
        Verdict::Well => th.c("success"),
        Verdict::Under | Verdict::Over => th.c("warning"),
        Verdict::Conflict => th.c("error"),
    };
    ui.label(RichText::new(cat.tf("profile_sketch.status", &[("dof", &dof.to_string())])).color(col));
}

fn field(ui: &mut egui::Ui, th: &Theme, salt: &str, v: &mut String, hint: &str, w: f32) -> egui::Response {
    TextField::new(v)
        .id_salt(salt)
        .labelled_by(egui::Id::new((salt, "lbl")), hint)
        .hint(hint)
        .width(w)
        .show(ui, th)
        .response
}

/// Small quiet button; a disabled one keeps its tooltip.
fn quiet(ui: &mut egui::Ui, th: &Theme, on: bool, label: &str, tip: &str) -> bool {
    let r = ui.add_enabled_ui(on, |ui| Button::quiet(label).small().show(ui, th)).inner;
    r.on_hover_text(tip).on_disabled_hover_text(tip).clicked()
}

/// i18n key of a rule's name in the list.
pub fn rule_key(g: &Geo) -> &'static str {
    match g {
        Geo::Horizontal(_) => "profile_sketch.rule.horizontal",
        Geo::Vertical(_) => "profile_sketch.rule.vertical",
        Geo::Coincident(..) => "profile_sketch.rule.coincident",
        Geo::Parallel(..) => "profile_sketch.rule.parallel",
        Geo::Perp(..) => "profile_sketch.rule.perp",
        Geo::EqualLen(..) => "profile_sketch.rule.equal",
        Geo::Fix(..) | Geo::FixX(..) | Geo::FixY(..) => "profile_sketch.rule.fix",
        _ => "profile_sketch.rule.other",
    }
}

pub fn show(ui: &mut egui::Ui, th: &Theme, cat: &Catalog, s: &mut Session, project: &Project) {
    let nf = NumberFormat::for_lang(cat.lang);
    let nums: HashMap<String, Q> = s
        .eval
        .resolved
        .as_ref()
        .map(|r| r.nums.clone())
        .unwrap_or_default();
    let names = move |n: &str| nums.get(n).copied();
    let p = &mut s.psk;
    let muted = th.c("text.secondary");
    let off = th.c("text.disabled");
    let (bad, warn) = (th.c("error"), th.c("warning"));
    let conflict = p.last.as_ref().map(|a| a.conflict.clone()).unwrap_or_default();
    let redundant = p.last.as_ref().map(|a| a.redundant.clone()).unwrap_or_default();

    ui.horizontal(|ui| {
        ui.label(RichText::new(cat.t("profile_sketch.title")).strong());
        status_label(ui, th, cat, p);
        if quiet(ui, th, p.can_undo(), &cat.t("profile_sketch.undo"), &cat.t("profile_sketch.undo_tip")) {
            p.undo();
        }
        if quiet(ui, th, p.can_redo(), &cat.t("profile_sketch.redo"), &cat.t("profile_sketch.redo_tip")) {
            p.redo();
        }
    });
    ui.label(RichText::new(cat.t("profile_sketch.scratch")).color(muted));
    if !conflict.is_empty() {
        ui.label(RichText::new(cat.t("profile_sketch.conflict_help")).color(bad));
    }

    // lines: construction toggle per entity
    let mut toggle: Option<(usize, bool)> = None;
    for (k, sg) in p.sk.segs.iter().enumerate() {
        ui.horizontal(|ui| {
            let t = cat.tf(
                "profile_sketch.line",
                &[("n", &k.to_string()), ("a", &sg.a.to_string()), ("b", &sg.b.to_string())],
            );
            let col = if sg.construction {
                off
            } else {
                th.c("text.primary")
            };
            ui.label(RichText::new(t).color(col));
            let mut c = sg.construction;
            if widgets::checkbox(ui, th, &mut c, &cat.t("profile_sketch.construction")).changed() {
                toggle = Some((k, c));
            }
        });
    }
    if let Some((k, c)) = toggle {
        let _ = p.act(|sk| {
            sk.set_construction(k, c);
            Ok::<_, ()>(sk.analyze())
        });
    }

    // rules
    let mut remove: Option<i64> = None;
    for r in &p.sk.rules {
        ui.horizontal(|ui| {
            let col = if conflict.contains(&r.tag) {
                bad
            } else if redundant.contains(&r.tag) {
                warn
            } else {
                muted
            };
            ui.label(RichText::new(format!("#{} {}", r.tag, cat.t(rule_key(&r.geo)))).color(col));
            if ddc_ui::widgets::close_button(ui, &cat.t("profile_sketch.delete")).clicked() {
                remove = Some(r.tag);
            }
        });
    }

    // dimensions: driving (value), driven (greyed, measured), conflicting (red, remedies)
    let mut drive: Option<(i64, bool)> = None;
    let mut open: Option<i64> = None;
    for d in &p.sk.dims {
        let measured = p.sk.measure(d.kind, d.a, d.b);
        ui.horizontal(|ui| {
            let kind = cat.t(&format!("profile_sketch.kind.{}", d.kind.key()));
            let head = cat.tf(
                "profile_sketch.dim",
                &[
                    ("n", &d.tag.to_string()),
                    ("kind", &kind),
                    ("a", &d.a.to_string()),
                    ("b", &d.b.to_string()),
                ],
            );
            let in_conflict = conflict.contains(&d.tag);
            let col = if in_conflict {
                bad
            } else if d.driven {
                off
            } else if redundant.contains(&d.tag) {
                warn
            } else {
                th.c("text.primary")
            };
            ui.label(RichText::new(head).color(col));
            if d.driven {
                let v = cat.tf("profile_sketch.driven_value", &[("v", &mm(measured))]);
                ui.label(RichText::new(v).color(off));
                if Button::quiet(&cat.t("profile_sketch.make_driving")).small().show(ui, th).clicked() {
                    drive = Some((d.tag, false));
                }
            } else {
                if p.editing != Some(d.tag)
                    && ui
                        .add(
                            egui::Label::new(RichText::new(&d.text).color(col))
                                .sense(egui::Sense::click()),
                        )
                        .on_hover_text(cat.t("profile_sketch.edit_value"))
                        .clicked()
                {
                    open = Some(d.tag);
                }
                if in_conflict
                    && Button::quiet(&cat.t("profile_sketch.make_driven")).small().show(ui, th).clicked()
                {
                    drive = Some((d.tag, true));
                }
            }
            if ddc_ui::widgets::close_button(ui, &cat.t("profile_sketch.delete")).clicked() {
                remove = Some(d.tag);
            }
        });
    }
    if let Some(t) = open {
        p.edit_text = p.sk.dim(t).map(|d| d.text.clone()).unwrap_or_default();
        p.editing = Some(t);
        p.focus = true;
        p.err = None;
    }
    if let Some((t, on)) = drive {
        let _ = p.act(|sk| sk.set_driven(t, on).ok_or(()));
    }
    if let Some(t) = remove {
        if p.editing == Some(t) {
            p.editing = None;
        }
        let _ = p.act(|sk| sk.delete(t).ok_or(()));
    }

    // the value field opened by placing or clicking a dimension
    if let Some(t) = p.editing {
        ui.horizontal(|ui| {
            ui.label(cat.tf("profile_sketch.value_for", &[("n", &t.to_string())]));
            let r = field(ui, th, "profile_sketch.edit", &mut p.edit_text, &cat.t("profile_sketch.value_hint"), 160.0);
            if std::mem::take(&mut p.focus) {
                r.request_focus();
            }
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
            if enter || Button::secondary(&cat.t("profile_sketch.apply")).show(ui, th).clicked() {
                p.err = match p.commit(&names) {
                    Ok(()) => None,
                    Err(e) => {
                        let (k, m) = value_err(&e);
                        Some(cat.tf(k, &[("msg", &m)]))
                    }
                };
            } else if esc {
                p.editing = None;
                p.err = None;
            }
        });
    }
    if let Some(e) = &p.err {
        ui.label(RichText::new(e).color(bad));
    }

    ui.separator();
    // point
    ui.horizontal(|ui| {
        let (ax, ay) = point_labels(cat);
        let l = (ui.label(RichText::new(ax.as_str()).color(muted)).id, ax.as_str());
        model_field(ui, th, "ri7.px", &mut p.px, Kind::Length, project, nf, l);
        let l = (ui.label(RichText::new(ay.as_str()).color(muted)).id, ay.as_str());
        model_field(ui, th, "ri7.py", &mut p.py, Kind::Length, project, nf, l);
        if Button::secondary(&cat.t("profile_sketch.add_point")).show(ui, th).clicked() {
            p.add_point();
        }
    });
    // line, with the per-tool construction toggle
    ui.horizontal(|ui| {
        let _ = field(ui, th, "ri7.la", &mut p.la, "P", 40.0);
        let _ = field(ui, th, "ri7.lb", &mut p.lb, "P", 40.0);
        widgets::checkbox(ui, th, &mut p.construction_tool, &cat.t("profile_sketch.construction_tool"));
        if Button::secondary(&cat.t("profile_sketch.add_line")).show(ui, th).clicked() {
            if let Err(k) = p.add_line() {
                p.err = Some(cat.t(k));
            }
        }
    });
    // rule
    ui.horizontal(|ui| {
        let names: Vec<String> = RULES.iter().map(|r| cat.t(r.0)).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        p.r_kind = p.r_kind.min(RULES.len() - 1);
        widgets::dropdown(ui, th, "profile_sketch.rule", &mut p.r_kind, &refs, None);
        let (_, two, lines) = RULES[p.r_kind];
        let h = if lines { "L" } else { "P" };
        let _ = field(ui, th, "ri7.ra", &mut p.r_a, h, 40.0);
        if two {
            let _ = field(ui, th, "ri7.rb", &mut p.r_b, h, 40.0);
        }
        if Button::secondary(&cat.t("profile_sketch.add_rule")).show(ui, th).clicked() {
            if let Err(k) = p.add_rule() {
                p.err = Some(cat.t(k));
            }
        }
    });
    // dimension: two points and a label position (it picks aligned / horizontal / vertical)
    ui.horizontal(|ui| {
        let _ = field(ui, th, "ri7.da", &mut p.da, "P", 40.0);
        let _ = field(ui, th, "ri7.db", &mut p.db, "P", 40.0);
        let (lx, ly) = (cat.t("profile_sketch.label_x"), cat.t("profile_sketch.label_y"));
        let l = (ui.label(RichText::new(lx.as_str()).color(muted)).id, lx.as_str());
        model_field(ui, th, "ri7.dlx", &mut p.dlx, Kind::Length, project, nf, l);
        let l = (ui.label(RichText::new(ly.as_str()).color(muted)).id, ly.as_str());
        model_field(ui, th, "ri7.dly", &mut p.dly, Kind::Length, project, nf, l);
        if Button::secondary(&cat.t("profile_sketch.add_dim"))
            .show(ui, th)
            .on_hover_text(cat.t("profile_sketch.add_dim_tip"))
            .clicked()
        {
            if let Err(k) = p.add_dim() {
                p.err = Some(cat.t(k));
            }
        }
    });
    ui.horizontal(|ui| {
        if Button::primary(&cat.t("profile_sketch.use_profile"))
            .show(ui, th)
            .on_hover_text(cat.t("profile_sketch.use_profile_tip"))
            .clicked()
        {
            match p.sk.profile_json() {
                Ok(j) => {
                    s.f.s_json = j;
                    p.err = None;
                }
                Err(e) => p.err = Some(cat.t(&format!("profile_sketch.err.profile.{}", e.key()))),
            }
        }
    });
    p.area = Some(ui.min_rect().intersect(ui.clip_rect()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &str) -> Option<Q> {
        None
    }

    fn square(p: &mut Panel) {
        p.act(|sk| {
            for (x, y) in [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)] {
                sk.point(x, y);
            }
            for k in 0..4 {
                sk.seg(k, (k + 1) % 4, false);
            }
            sk.rule(Geo::Fix(0, 0.0, 0.0));
            sk.rule(Geo::Horizontal((0, 1)));
            sk.rule(Geo::Vertical((1, 2)));
            sk.rule(Geo::Horizontal((2, 3)));
            sk.rule(Geo::Vertical((3, 0)));
            Ok::<_, ()>(sk.analyze())
        })
        .unwrap();
    }

    #[test]
    fn placing_dim_opens_value_and_is_one_undo_step() {
        let mut p = Panel::default();
        square(&mut p);
        let before = p.sk.clone();
        let t = p.place_dim(0, 1, [0.5, -0.5]).unwrap();
        assert_eq!(p.editing, Some(t), "value field opens at once");
        // a refused value keeps the old one, the field and adds no undo step
        p.edit_text = "12 +".into();
        assert!(p.commit(&none).is_err());
        assert_eq!(p.editing, Some(t));
        p.edit_text = "700mm".into();
        p.commit(&none).unwrap();
        assert_eq!(p.editing, None);
        assert!((p.sk.pts[1][0] - 0.7).abs() < 1e-7);
        assert!(p.undo(), "value step");
        assert!((p.sk.pts[1][0] - 1.0).abs() < 1e-7);
        assert!(p.undo(), "placement step");
        assert_eq!(p.sk, before);
        assert!(p.redo() && p.redo());
        assert!((p.sk.pts[1][0] - 0.7).abs() < 1e-7);
        assert_eq!(p.status().map(|s| s.0), Some(1));
    }

    #[test]
    fn duplicate_dim_goes_driven_without_value_field_in_one_step() {
        let mut p = Panel::default();
        square(&mut p);
        p.place_dim(0, 1, [0.5, -0.5]).unwrap();
        p.editing = None;
        let before = p.sk.clone();
        let t = p.place_dim(3, 2, [0.5, 1.5]).unwrap();
        assert!(p.sk.dim(t).unwrap().driven);
        assert_eq!(p.editing, None);
        assert_eq!(p.last.as_ref().unwrap().made_driven, vec![t]);
        assert!(p.undo());
        assert_eq!(p.sk, before);
    }

    fn steps(p: &mut Panel) -> usize {
        let mut n = 0;
        while p.undo() {
            n += 1;
        }
        while p.redo() {}
        n
    }

    #[test]
    fn forms_commit_values_as_one_undo_step_each() {
        let mut p = Panel::default();
        (p.px, p.py) = (0.25, 0.5);
        p.add_point();
        (p.px, p.py) = (1.0, 0.5);
        p.add_point();
        assert_eq!(p.sk.pts, vec![[0.25, 0.5], [1.0, 0.5]]);
        assert_eq!(steps(&mut p), 2, "one step per point");
        p.la = "P0".into();
        p.lb = "1".into();
        p.construction_tool = true;
        assert_eq!(p.add_line(), Ok(()));
        assert!(p.sk.segs[0].construction);
        p.r_kind = 0;
        p.r_a = "L0".into();
        assert_eq!(p.add_rule(), Ok(()));
        assert_eq!(p.sk.rules.len(), 1);
        (p.da, p.db) = ("0".into(), "1".into());
        (p.dlx, p.dly) = (0.5, -0.2);
        assert_eq!(p.add_dim(), Ok(()));
        assert_eq!(p.sk.dims.len(), 1);
        assert!(p.editing.is_some(), "value field opens");
        assert_eq!(steps(&mut p), 5, "points, line, rule, dimension: one step each");
    }

    #[test]
    fn invalid_form_input_writes_nothing() {
        let mut p = Panel::default();
        square(&mut p);
        let (before, undo) = (p.sk.clone(), steps(&mut p));
        for (a, b) in [("", ""), ("P9", "0"), ("0", "9"), ("x", "1")] {
            (p.la, p.lb) = (a.into(), b.into());
            assert_eq!(p.add_line(), Err("profile_sketch.err.points"));
            (p.r_a, p.r_b) = (a.into(), b.into());
            p.r_kind = 2;
            assert_eq!(p.add_rule(), Err("profile_sketch.err.points"));
            (p.da, p.db) = (a.into(), b.into());
            assert_eq!(p.add_dim(), Err("profile_sketch.err.points"));
        }
        // a line or dimension from a point to itself is refused too
        (p.la, p.lb, p.da, p.db) = ("1".into(), "1".into(), "1".into(), "1".into());
        assert_eq!(p.add_line(), Err("profile_sketch.err.points"));
        assert_eq!(p.add_dim(), Err("profile_sketch.err.points"));
        assert_eq!(p.sk, before);
        assert!(p.editing.is_none());
        assert_eq!(steps(&mut p), undo, "no extra undo step");
    }

    #[test]
    fn decimal_mark_follows_the_ui_language() {
        use ddc_ui::widgets::field::parse_number;
        assert_eq!(parse_number("1,5", NumberFormat::for_lang(ddc_ui::Lang::Ru)), Some(1.5));
        assert_eq!(parse_number("1.5", NumberFormat::for_lang(ddc_ui::Lang::En)), Some(1.5));
    }

    /// Context with the named font weights bound: a bare one panics on kit labels.
    fn font_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        ctx
    }

    fn draw(ctx: &egui::Context, lang: ddc_ui::Lang, s: &mut Session, imperial: bool) -> usize {
        let th = Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Comfortable);
        let cat = Catalog::new(lang);
        let project = Project { imperial, ..Project::default() };
        let full = ctx.run(Default::default(), |c| {
            egui::CentralPanel::default().show(c, |ui| show(ui, &th, &cat, s, &project));
        });
        full.shapes.len()
    }

    /// The point fields are captioned from the catalog in both languages, not literal x/y.
    #[test]
    fn point_captions_come_from_the_catalog() {
        let (x, y) = point_labels(&Catalog::new(ddc_ui::Lang::En));
        assert_eq!((x.as_str(), y.as_str()), ("X axis", "Y axis"));
        let (x, y) = point_labels(&Catalog::new(ddc_ui::Lang::Ru));
        let os = "\u{41e}\u{441}\u{44c}";
        assert_eq!((x, y), (format!("{os} X"), format!("{os} Y")));
        let src = include_str!("profile_sketch.rs");
        let lit = ["RichText::new(\"", "x\")"].concat();
        assert!(!src.contains(&lit), "literal axis caption");
    }

    #[test]
    fn panel_draws_in_en_and_ru_without_touching_the_sketch() {
        use ddc_geom::component::doc::FamDoc;
        use ddc_geom::component::{Category, Hosting};
        let ctx = font_ctx();
        for (lang, imperial) in [
            (ddc_ui::Lang::En, false),
            (ddc_ui::Lang::Ru, false),
            (ddc_ui::Lang::En, true),
        ] {
            let mut s = Session::new(FamDoc::blank("T", Category::Generic, Hosting::None), None);
            square(&mut s.psk);
            s.psk.place_dim(0, 1, [0.5, -0.5]);
            let before = s.psk.sk.clone();
            for _ in 0..3 {
                assert!(draw(&ctx, lang, &mut s, imperial) > 20);
            }
            assert_eq!(s.psk.sk, before, "drawing writes nothing");
            assert!(s.psk.area.is_some());
        }
    }

    #[test]
    fn production_code_uses_kit_widgets_only() {
        let src = include_str!("profile_sketch.rs");
        let prod = src.split_once("#[cfg(test)]").unwrap().0;
        for raw in ["ComboBox::", "DragValue::", ".button(", ".small_button(", ".checkbox(", ".selectable_value(", "TextEdit::"] {
            assert!(!prod.contains(raw), "raw widget {raw}");
        }
    }

    #[test]
    fn rule_form_builds_geometry() {
        let mut p = Panel::default();
        square(&mut p);
        assert_eq!(rule_geo(0, 1, 1, &p.sk), Some(Geo::Horizontal((1, 2))));
        assert_eq!(rule_geo(2, 0, 9, &p.sk), None);
        assert_eq!(rule_geo(3, 0, 9, &p.sk), None);
        assert_eq!(idx(" P3"), Some(3));
        let (k, m) = value_err(&ValueError::Parse("x".into()));
        assert_eq!((k, m.as_str()), ("profile_sketch.err.parse", "x"));
    }
}
