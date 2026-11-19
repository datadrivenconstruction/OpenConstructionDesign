//! Editor of the line pattern and hatch libraries of the project. The built-in entries are read
//! only (copy one to start from it); a user entry may share a built-in name and then replaces it.
//! Save and Delete are one model operation each, so one undo step.

use std::cell::RefCell;

use ddc_model::drawq::{
    HatchFamily, HatchLib, HatchPattern, LinePattern, PatternLib, Seg,
};
use ddc_ui::theme::{control, radius, space, stroke, Role};
use ddc_ui::widgets::{self, Button, RowState};
use ddc_ui::Theme;
use egui::{self, Color32, Pos2, Sense, Stroke, Vec2};

use crate::editor::Editor;
use crate::modify_opts::number_format;
use crate::ui_kit as kit;
use crate::unsaved::{decide_switch, Choice, Switch};
use crate::view_gfx_ui::run;

/// Catalog text in the window's own language.
fn t(ru: bool, key: &str) -> String {
    crate::tr_lang(ru, key, &[])
}

// holds the longest built-in pattern name in Russian without a cut
const LIST_W: f32 = 192.0;
const SWATCH_W: f32 = 200.0;
const SWATCH_H: f32 = 22.0;
/// Paper-to-screen scale of the previews, px per mm.
const PREVIEW_PX_PER_MM: f64 = 4.0;
/// Side of the hatch preview in mm on paper (drafting) or m on the model.
const HATCH_MM: f64 = 20.0;
const MAX_NAME: usize = 40;
const NUM_W: f32 = 112.0;
const NAME_W: f32 = 200.0;

// ---- drafts (pure) ----

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegKind {
    Dash,
    Gap,
    Dot,
    Zig,
    Wave,
}

impl SegKind {
    pub const ALL: [SegKind; 5] = [
        SegKind::Dash,
        SegKind::Gap,
        SegKind::Dot,
        SegKind::Zig,
        SegKind::Wave,
    ];

    pub fn key(self) -> &'static str {
        match self {
            SegKind::Dash => "dash",
            SegKind::Gap => "gap",
            SegKind::Dot => "dot",
            SegKind::Zig => "zig",
            SegKind::Wave => "wave",
        }
    }

    fn has_len(self) -> bool {
        self != SegKind::Dot
    }

    fn has_amp(self) -> bool {
        matches!(self, SegKind::Zig | SegKind::Wave)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SegDraft {
    pub kind: SegKind,
    pub len: f64,
    pub amp: f64,
}

impl SegDraft {
    pub fn from_seg(s: Seg) -> Self {
        let (kind, len, amp) = match s {
            Seg::Dash(l) => (SegKind::Dash, l, 0.5),
            Seg::Gap(l) => (SegKind::Gap, l, 0.5),
            Seg::Dot => (SegKind::Dot, 1.0, 0.5),
            Seg::Zig { len, amp } => (SegKind::Zig, len, amp),
            Seg::Wave { len, amp } => (SegKind::Wave, len, amp),
        };
        Self { kind, len, amp }
    }

    pub fn seg(&self) -> Seg {
        match self.kind {
            SegKind::Dash => Seg::Dash(self.len),
            SegKind::Gap => Seg::Gap(self.len),
            SegKind::Dot => Seg::Dot,
            SegKind::Zig => Seg::Zig {
                len: self.len,
                amp: self.amp,
            },
            SegKind::Wave => Seg::Wave {
                len: self.len,
                amp: self.amp,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PatternDraft {
    pub name: String,
    pub segs: Vec<SegDraft>,
}

impl Default for PatternDraft {
    fn default() -> Self {
        Self {
            name: String::new(),
            segs: vec![
                SegDraft::from_seg(Seg::Dash(4.0)),
                SegDraft::from_seg(Seg::Gap(2.0)),
            ],
        }
    }
}

fn name_problem(name: &str, reserved: &[&str]) -> Option<&'static str> {
    let n = name.trim();
    if n.is_empty() {
        return Some("draw_quality.lib.err.name");
    }
    if n.chars().count() > MAX_NAME || n.chars().any(char::is_control) {
        return Some("draw_quality.lib.err.name_long");
    }
    if reserved.iter().any(|r| r.eq_ignore_ascii_case(n)) {
        return Some("draw_quality.lib.err.reserved");
    }
    None
}

impl PatternDraft {
    pub fn from_pattern(p: &LinePattern) -> Self {
        Self {
            name: p.name.clone(),
            segs: p.segs.iter().copied().map(SegDraft::from_seg).collect(),
        }
    }

    pub fn build(&self) -> LinePattern {
        LinePattern {
            name: self.name.trim().to_string(),
            segs: self.segs.iter().map(SegDraft::seg).collect(),
        }
    }

    /// i18n key of what stops Save, if anything.
    pub fn problem(&self) -> Option<&'static str> {
        if let Some(k) = name_problem(&self.name, &["solid"]) {
            return Some(k);
        }
        if self.segs.is_empty() {
            return Some("draw_quality.lib.err.no_segs");
        }
        if !self.build().check() {
            return Some("draw_quality.lib.err.seg");
        }
        None
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FamilyDraft {
    pub angle: f64,
    pub origin: (f64, f64),
    pub shift: f64,
    pub spacing: f64,
    /// Drawn and gap lengths separated by spaces; empty is a solid line.
    pub dashes: String,
}

/// Lengths separated by spaces or semicolons; a comma is a decimal comma. None on junk.
pub fn parse_dashes(s: &str) -> Option<Vec<f64>> {
    s.split(|c: char| c.is_whitespace() || c == ';')
        .filter(|w| !w.is_empty())
        .map(|w| crate::units::parse_locale_number(w, crate::units::ui_decimal_mark()).ok())
        .collect()
}

pub fn format_dashes(d: &[f64]) -> String {
    d.iter().map(|v| format!("{v}")).collect::<Vec<_>>().join(" ")
}

impl FamilyDraft {
    pub fn from_family(f: &HatchFamily) -> Self {
        Self {
            angle: f.angle,
            origin: f.origin,
            shift: f.shift,
            spacing: f.spacing,
            dashes: format_dashes(&f.dashes),
        }
    }

    fn family(&self) -> Option<HatchFamily> {
        Some(HatchFamily {
            angle: self.angle,
            origin: self.origin,
            shift: self.shift,
            spacing: self.spacing,
            dashes: parse_dashes(&self.dashes)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct HatchDraft {
    pub name: String,
    /// Lengths in metres on the model instead of mm on paper.
    pub model: bool,
    pub fams: Vec<FamilyDraft>,
}

impl Default for HatchDraft {
    fn default() -> Self {
        Self {
            name: String::new(),
            model: false,
            fams: vec![FamilyDraft {
                angle: 45.0,
                origin: (0.0, 0.0),
                shift: 0.0,
                spacing: 3.0,
                dashes: String::new(),
            }],
        }
    }
}

impl HatchDraft {
    pub fn from_pattern(p: &HatchPattern) -> Self {
        Self {
            name: p.name.clone(),
            model: p.model,
            fams: p.families.iter().map(FamilyDraft::from_family).collect(),
        }
    }

    pub fn build(&self) -> Result<HatchPattern, &'static str> {
        if let Some(k) = name_problem(&self.name, &["none"]) {
            return Err(k);
        }
        if self.fams.is_empty() {
            return Err("draw_quality.lib.err.no_fams");
        }
        let mut families = vec![];
        for f in &self.fams {
            families.push(f.family().ok_or("draw_quality.lib.err.dashes")?);
        }
        let p = HatchPattern {
            name: self.name.trim().to_string(),
            families,
            model: self.model,
        };
        if p.check() {
            Ok(p)
        } else {
            Err("draw_quality.lib.err.family")
        }
    }

    pub fn problem(&self) -> Option<&'static str> {
        self.build().err()
    }
}

// ---- window state ----

#[derive(Clone, Debug, Default, PartialEq)]
enum Pick {
    #[default]
    None,
    Pattern(String),
    Hatch(String),
    NewPattern,
    NewHatch,
}

#[derive(Default)]
struct State {
    pick: Pick,
    pat: PatternDraft,
    hat: HatchDraft,
    /// The draft as last loaded or saved: tells whether Save has anything to do.
    clean_pat: PatternDraft,
    clean_hat: HatchDraft,
    /// The draft as it was when the entry was opened or last saved: differs only after the
    /// user's own edits, also for a new or copied entry.
    base_pat: PatternDraft,
    base_hat: HatchDraft,
    /// Entry asked for while the current one has unsaved edits.
    pending: Option<Pick>,
    asked: bool,
    /// Bumped on every switch so a field half-typed in one entry never lands in the next.
    gen: u32,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Guard {
    Go,
    Ask,
}

/// Switching away from `pick` asks only when its draft differs from the one it started with.
/// An untouched new entry, a copy, a built-in and no selection never ask.
fn switch_guard(
    pick: &Pick,
    pat: (&PatternDraft, &PatternDraft),
    hat: (&HatchDraft, &HatchDraft),
) -> Guard {
    let dirty = match pick {
        Pick::Pattern(_) | Pick::NewPattern => pat.0 != pat.1,
        Pick::Hatch(_) | Pick::NewHatch => hat.0 != hat.1,
        Pick::None => false,
    };
    if dirty {
        Guard::Ask
    } else {
        Guard::Go
    }
}

fn dirty(st: &State) -> bool {
    switch_guard(&st.pick, (&st.pat, &st.base_pat), (&st.hat, &st.base_hat)) == Guard::Ask
}

/// True while the open entry has edits that are neither saved nor discarded.
pub fn is_dirty() -> bool {
    STATE.with(|s| dirty(&s.borrow()))
}

/// True while the "switch with unsaved edits" question is open.
pub fn asking() -> bool {
    STATE.with(|s| s.try_borrow().is_ok_and(|s| s.pending.is_some() && dirty(&s)))
}

/// Drops the selection and its draft (the user chose Discard on closing).
pub fn discard() {
    STATE.with(|s| *s.borrow_mut() = State::default());
}

/// Opens a new hatch named `name` with unsaved edits in this thread (project question tests).
#[cfg(test)]
pub(crate) fn seed_hatch_draft_for_test(name: &str) {
    let mut st = State::default();
    load_pick(&mut st, Pick::NewHatch, &PatternLib::default(), &HatchLib::default());
    st.hat.name = name.into();
    STATE.with(|s| *s.borrow_mut() = st);
}

/// Name of the entry being edited, for the question.
pub fn dirty_name(ru: bool) -> String {
    STATE.with(|s| entry_name(ru, &s.borrow()))
}

fn entry_name(ru: bool, st: &State) -> String {
    let n = match st.pick {
        Pick::Pattern(_) | Pick::NewPattern => st.pat.name.trim(),
        Pick::Hatch(_) | Pick::NewHatch => st.hat.name.trim(),
        Pick::None => "",
    };
    if n.is_empty() {
        t(ru, match st.pick {
            Pick::Hatch(_) | Pick::NewHatch => "draw_quality.lib.new_hatch",
            _ => "draw_quality.lib.new_pattern",
        })
    } else {
        pattern_label(ru, n)
    }
}

/// Opens `target`: its stored draft, or a fresh one for a new entry.
fn load_pick(st: &mut State, target: Pick, pats: &PatternLib, hats: &HatchLib) {
    match &target {
        Pick::None => {}
        Pick::Pattern(n) => {
            let Some(p) = pats.get(n) else { return };
            st.pat = PatternDraft::from_pattern(&p);
            st.clean_pat = st.pat.clone();
            st.base_pat = st.pat.clone();
        }
        Pick::Hatch(n) => {
            let Some(h) = hats.get(n) else { return };
            st.hat = HatchDraft::from_pattern(&h);
            st.clean_hat = st.hat.clone();
            st.base_hat = st.hat.clone();
        }
        Pick::NewPattern => {
            st.pat = PatternDraft::default();
            st.base_pat = st.pat.clone();
            st.clean_pat = PatternDraft {
                name: "\u{0}".into(),
                ..PatternDraft::default()
            };
        }
        Pick::NewHatch => {
            st.hat = HatchDraft::default();
            st.base_hat = st.hat.clone();
            st.clean_hat = HatchDraft {
                name: "\u{0}".into(),
                ..HatchDraft::default()
            };
        }
    }
    st.pick = target;
    st.gen = st.gen.wrapping_add(1);
}

/// Save of the open entry as one model operation. False when nothing was stored: the form shows
/// why, the draft stays.
fn save_pattern(ed: &mut Editor, st: &mut State, pats: &mut PatternLib) -> bool {
    if st.pat.problem().is_some() {
        return false;
    }
    let mut lib = pats.clone();
    let p = st.pat.build();
    if lib.set(p.clone()) && run(ed, {
        let lib = lib.clone();
        move |m| m.set_drawq_patterns(&lib)
    }) {
        *pats = lib;
        st.clean_pat = st.pat.clone();
        st.base_pat = st.pat.clone();
        st.pick = Pick::Pattern(p.name);
        return true;
    }
    false
}

fn save_hatch(ed: &mut Editor, st: &mut State, hats: &mut HatchLib) -> bool {
    let Ok(p) = st.hat.build() else { return false };
    let mut lib = hats.clone();
    if lib.set(p.clone()) && run(ed, {
        let lib = lib.clone();
        move |m| m.set_drawq_hatches(&lib)
    }) {
        *hats = lib;
        st.clean_hat = st.hat.clone();
        st.base_hat = st.hat.clone();
        st.pick = Pick::Hatch(p.name);
        return true;
    }
    false
}

fn save_pick(ed: &mut Editor, st: &mut State, pats: &mut PatternLib, hats: &mut HatchLib) -> bool {
    match st.pick {
        Pick::Pattern(_) | Pick::NewPattern => save_pattern(ed, st, pats),
        Pick::Hatch(_) | Pick::NewHatch => save_hatch(ed, st, hats),
        Pick::None => true,
    }
}

/// Saves the open entry (used when the window is closed with Apply).
pub fn save_current(ed: &mut Editor, pats: &mut PatternLib, hats: &mut HatchLib) -> bool {
    let mut st = STATE.with(|s| std::mem::take(&mut *s.borrow_mut()));
    let ok = save_pick(ed, &mut st, pats, hats);
    STATE.with(|s| *s.borrow_mut() = st);
    ok
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Moved,
    Asking,
    Stayed,
}

/// Carries out the requested switch once the user's answer is known. Edits are dropped only on
/// Discard; Cancel and a failed Save keep the draft and the selection.
fn resolve(
    st: &mut State,
    ed: &mut Editor,
    pats: &mut PatternLib,
    hats: &mut HatchLib,
    choice: Option<Choice>,
) -> Outcome {
    let Some(target) = st.pending.clone() else {
        return Outcome::Moved;
    };
    match decide_switch(dirty(st), choice) {
        Switch::Ask => return Outcome::Asking,
        Switch::Stay => {
            st.pending = None;
            return Outcome::Stayed;
        }
        Switch::ApplyThenGo => {
            if !save_pick(ed, st, pats, hats) {
                st.pending = None;
                return Outcome::Stayed;
            }
        }
        Switch::Now | Switch::DiscardThenGo => {}
    }
    st.pending = None;
    load_pick(st, target, pats, hats);
    Outcome::Moved
}

/// Look of one form: theme, language and the switch counter that keys the fields.
struct Cx<'a> {
    th: &'a Theme,
    ru: bool,
    gen: u32,
}

/// Limits and steps of a number field.
#[derive(Clone, Copy)]
struct Spec {
    decimals: usize,
    step: f64,
    lo: f64,
    hi: f64,
}

fn num(
    ui: &mut egui::Ui,
    cx: &Cx,
    salt: impl std::hash::Hash,
    label: &str,
    v: &mut f64,
    spec: Spec,
    unit: &str,
) {
    widgets::NumberField::new((cx.gen, salt), v, number_format(cx.ru))
        .labelled_by(egui::Id::new(("ux49d.lbl", label)), label)
        .decimals(spec.decimals)
        .step(spec.step)
        .range(spec.lo, spec.hi)
        .unit(unit)
        .width(NUM_W)
        .show(ui, cx.th);
}

fn text_field(
    ui: &mut egui::Ui,
    cx: &Cx,
    salt: impl std::hash::Hash,
    label: &str,
    buf: &mut String,
    hint: &str,
    w: f32,
) {
    widgets::TextField::new(buf)
        .labelled_by(egui::Id::new(("ux49d.lbl", label)), label)
        .id_salt((cx.gen, salt))
        .hint(hint)
        .width(w)
        .show(ui, cx.th);
}

fn button(ui: &mut egui::Ui, th: &Theme, label: &str, icon: Option<&str>, full: bool) -> egui::Response {
    let b = Button::secondary(label);
    let b = if full { b.full_width() } else { b };
    match icon {
        Some(i) => {
            let ctx = ui.ctx().clone();
            widgets::with_icons(&ctx, |icons| b.show_icon(ui, th, icons, i))
        }
        None => b.show(ui, th),
    }
}

/// One list row: elided name (full name on hover) and a pill for entries of this project.
fn list_row(
    ui: &mut egui::Ui,
    th: &Theme,
    ru: bool,
    selected: bool,
    label: &str,
    custom: bool,
) -> egui::Response {
    let size = egui::vec2(ui.available_width(), th.d().row);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let state = if selected {
        RowState::Selected
    } else {
        RowState::Normal
    };
    let (fill, text_c, _) = widgets::table::row_colors(th.sem(), state, resp.hovered());
    ui.painter().rect_filled(rect, radius::SM, fill);
    if resp.has_focus() {
        ui.painter().rect_stroke(
            rect,
            radius::SM,
            Stroke::new(stroke::FOCUS, th.sem().focus_ring),
            egui::StrokeKind::Inside,
        );
    }
    let mut right = rect.right() - space::SM;
    if custom {
        let tag = t(ru, "draw_quality.custom");
        let font = th.font(Role::Caption);
        let ink = th.sem().text_secondary;
        let w = ui
            .painter()
            .layout_no_wrap(tag.clone(), font.clone(), ink)
            .size()
            .x
            + 2.0 * space::SM;
        let pill = egui::Rect::from_min_max(
            Pos2::new(right - w, rect.center().y - control::BADGE_H / 2.0),
            Pos2::new(right, rect.center().y + control::BADGE_H / 2.0),
        );
        ui.painter()
            .rect_filled(pill, th.radius("sm"), th.sem().neutral_subtle);
        widgets::text::paint_in(ui, pill, &tag, font, ink, egui::Align::Center);
        right = pill.left() - space::XS;
    }
    let text_rect = egui::Rect::from_min_max(
        Pos2::new(rect.left() + space::SM, rect.top()),
        Pos2::new(right.max(rect.left() + space::SM), rect.bottom()),
    );
    let cut = widgets::text::paint_in(ui, text_rect, label, th.font(Role::Body), text_c, egui::Align::Min);
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, label)
    });
    if custom {
        resp.on_hover_text(crate::tr_lang(ru, "draw_quality.custom.tip", &[("name", label)]))
    } else if cut {
        resp.on_hover_text(label)
    } else {
        resp
    }
}

fn pattern_label(ru: bool, name: &str) -> String {
    let key = format!("draw_quality.pat.{name}");
    let s = t(ru, &key);
    if s == key {
        name.to_string()
    } else {
        s
    }
}

/// The two libraries side by side with the form of the selected entry.
pub fn show(ui: &mut egui::Ui, ed: &mut Editor, pats: &mut PatternLib, hats: &mut HatchLib, ru: bool) {
    let mut st = STATE.with(|s| std::mem::take(&mut *s.borrow_mut()));
    let th = ddc_ui::theme::of(ui.ctx());
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(LIST_W);
            if let Some(p) = list(ui, &th, ru, &st, pats, hats) {
                if p != st.pick {
                    st.pending = Some(p);
                }
            }
        });
        ui.separator();
        let cx = Cx {
            th: &th,
            ru,
            gen: st.gen,
        };
        ui.vertical(|ui| match st.pick.clone() {
            Pick::None => kit::empty_note(ui, t(ru, "draw_quality.lib.pick"), Some(t(ru, "draw_quality.lib.pick.hint").as_str())),
            Pick::Pattern(_) | Pick::NewPattern => pattern_form(ui, &cx, ed, &mut st, pats),
            Pick::Hatch(_) | Pick::NewHatch => hatch_form(ui, &cx, ed, &mut st, hats),
        });
    });
    if st.pending.is_some() {
        let choice = if dirty(&st) {
            crate::unsaved::ask(
                ui.ctx(),
                ru,
                "ux49d.libs",
                &entry_name(ru, &st),
                &crate::tr_lang(ru, "draw_quality.unsaved.lib_body", &[]),
                &crate::tr_lang(ru, "draw_quality.lib.save", &[]),
                st.asked,
            )
        } else {
            None
        };
        st.asked = resolve(&mut st, ed, pats, hats, choice) == Outcome::Asking;
    }
    STATE.with(|s| *s.borrow_mut() = st);
}

/// The two lists; the entry a click asks for, if any (the caller decides whether to switch).
fn list(
    ui: &mut egui::Ui,
    th: &Theme,
    ru: bool,
    st: &State,
    pats: &PatternLib,
    hats: &HatchLib,
) -> Option<Pick> {
    let mut want = None;
    kit::section_title(ui, t(ru, "draw_quality.lines.patterns"));
    for name in pats.names() {
        let custom = pats.user.iter().any(|u| u.name == name);
        let on = st.pick == Pick::Pattern(name.clone());
        if list_row(ui, th, ru, on, &pattern_label(ru, &name), custom).clicked() {
            want = Some(Pick::Pattern(name));
        }
    }
    if button(ui, th, &t(ru, "draw_quality.lib.new_pattern"), Some("plus"), true).clicked() {
        want = Some(Pick::NewPattern);
    }
    ui.add_space(space::MD);
    kit::section_title(ui, t(ru, "draw_quality.lines.hatches"));
    for name in hats.names() {
        let custom = hats.user.iter().any(|u| u.name == name);
        let on = st.pick == Pick::Hatch(name.clone());
        if list_row(ui, th, ru, on, &pattern_label(ru, &name), custom).clicked() {
            want = Some(Pick::Hatch(name));
        }
    }
    if button(ui, th, &t(ru, "draw_quality.lib.new_hatch"), Some("plus"), true).clicked() {
        want = Some(Pick::NewHatch);
    }
    ui.add_space(space::SM);
    ui.add(
        egui::Label::new(
            egui::RichText::new(t(ru, "draw_quality.custom_note"))
                .font(th.font(Role::Caption))
                .color(th.sem().text_secondary),
        )
        .wrap(),
    );
    want
}

// ---- pattern form ----

fn pattern_form(ui: &mut egui::Ui, cx: &Cx, ed: &mut Editor, st: &mut State, pats: &mut PatternLib) {
    let existing = match &st.pick {
        Pick::Pattern(n) => Some(n.clone()),
        _ => None,
    };
    let is_user = existing
        .as_ref()
        .is_some_and(|n| pats.user.iter().any(|u| &u.name == n));
    // a built-in entry is read only: copying it starts a new entry from it
    let locked = existing.is_some() && !is_user;
    let th = cx.th;
    let ru = cx.ru;
    let t = |k: &str| t(ru, k);
    let mm = t("draw_quality.unit.mm");
    let kinds: Vec<String> = SegKind::ALL
        .iter()
        .map(|k| t(&format!("draw_quality.lib.seg.{}", k.key())))
        .collect();
    let kind_refs: Vec<&str> = kinds.iter().map(String::as_str).collect();
    ui.add_enabled_ui(!locked, |ui| {
        let name_label = t("draw_quality.lib.name");
        widgets::prop_row(ui, th, &name_label, |ui| {
            text_field(ui, cx, "pname", &name_label, &mut st.pat.name, "", NAME_W);
        });
        let mut drop: Option<usize> = None;
        for (i, s) in st.pat.segs.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                let mut sel = SegKind::ALL.iter().position(|k| *k == s.kind).unwrap_or(0);
                if widgets::dropdown(ui, th, (cx.gen, "seg", i), &mut sel, &kind_refs, Some(96.0)).changed() {
                    s.kind = SegKind::ALL[sel];
                }
                if s.kind.has_len() {
                    let spec = Spec { decimals: 1, step: 0.1, lo: 0.1, hi: 500.0 };
                    num(ui, cx, ("len", i), &t("draw_quality.lib.lengths"), &mut s.len, spec, &mm);
                }
                if s.kind.has_amp() {
                    let spec = Spec { decimals: 2, step: 0.05, lo: 0.0, hi: 50.0 };
                    num(ui, cx, ("amp", i), &t("draw_quality.lib.amp"), &mut s.amp, spec, &mm);
                }
                if widgets::close_button(ui, &t("draw_quality.lib.remove")).clicked() {
                    drop = Some(i);
                }
            });
        }
        if let Some(i) = drop {
            st.pat.segs.remove(i);
        }
        if st.pat.segs.len() < 64 && button(ui, th, &t("draw_quality.lib.add_seg"), Some("plus"), false).clicked() {
            st.pat.segs.push(SegDraft::from_seg(Seg::Dash(3.0)));
        }
    });
    ui.add_space(space::SM);
    let shown = if locked || st.pat.problem().is_none() {
        st.pat.build()
    } else {
        LinePattern {
            name: String::new(),
            segs: vec![],
        }
    };
    preview_line(ui, &shown);
    let problem = st.pat.problem();
    if let (Some(k), false) = (problem, locked) {
        kit::error_note(ui, &t(k));
    }
    ui.horizontal(|ui| {
        if locked {
            if button(ui, th, &t("draw_quality.lib.copy"), None, false).clicked() {
                st.pat.name = format!("{} {}", pattern_label(ru, &st.pat.name), t("draw_quality.lib.copy_suffix"));
                st.clean_pat = PatternDraft {
                    name: "\u{0}".into(),
                    ..st.pat.clone()
                };
                st.base_pat = st.pat.clone();
                st.pick = Pick::NewPattern;
                st.gen = st.gen.wrapping_add(1);
            }
            return;
        }
        let same = st.pat == st.clean_pat;
        let no_change = t("draw_quality.no_change");
        let why = problem.map(t).or_else(|| same.then_some(no_change));
        if kit::primary_action(ui, t("draw_quality.lib.save"), why.as_deref()) {
            save_pattern(ed, st, pats);
        }
        if let Some(n) = existing.as_ref().filter(|_| is_user) {
            let builtin = PatternLib::default().get(n).is_some();
            let label = t(if builtin { "draw_quality.lib.restore" } else { "draw_quality.lib.delete" });
            if kit::danger_button(ui, label, true).clicked() {
                let mut lib = pats.clone();
                lib.remove(n);
                if run(ed, {
                    let lib = lib.clone();
                    move |m| m.set_drawq_patterns(&lib)
                }) {
                    *pats = lib;
                    st.pick = Pick::None;
                }
            }
        }
    });
}

// ---- hatch form ----

fn hatch_form(ui: &mut egui::Ui, cx: &Cx, ed: &mut Editor, st: &mut State, hats: &mut HatchLib) {
    let existing = match &st.pick {
        Pick::Hatch(n) => Some(n.clone()),
        _ => None,
    };
    let is_user = existing
        .as_ref()
        .is_some_and(|n| hats.user.iter().any(|u| &u.name == n));
    let locked = existing.is_some() && !is_user;
    let th = cx.th;
    let ru = cx.ru;
    let t = |k: &str| t(ru, k);
    ui.add_enabled_ui(!locked, |ui| {
        let name_label = t("draw_quality.lib.name");
        widgets::prop_row(ui, th, &name_label, |ui| {
            text_field(ui, cx, "hname", &name_label, &mut st.hat.name, "", NAME_W);
        });
        widgets::prop_row(ui, th, &t("draw_quality.lib.lengths"), |ui| {
            let items = [t("draw_quality.lib.on_paper"), t("draw_quality.lib.on_model")];
            let refs = [items[0].as_str(), items[1].as_str()];
            if let Some(i) = widgets::segmented(ui, th, &refs, usize::from(st.hat.model)) {
                st.hat.model = i == 1;
            }
        });
        let unit = t(if st.hat.model { "draw_quality.unit.m" } else { "draw_quality.unit.mm" });
        let deg = "\u{b0}";
        let mut drop: Option<usize> = None;
        for (i, f) in st.hat.fams.iter_mut().enumerate() {
            widgets::card(ui, th, |ui| {
                let spec = |decimals, step, lo, hi| Spec { decimals, step, lo, hi };
                let angle = t("draw_quality.lib.angle");
                widgets::prop_row(ui, th, &angle, |ui| {
                    num(ui, cx, ("angle", i), &angle, &mut f.angle, spec(1, 1.0, -360.0, 360.0), deg);
                });
                let spacing = t("draw_quality.lib.spacing");
                widgets::prop_row(ui, th, &spacing, |ui| {
                    num(ui, cx, ("spacing", i), &spacing, &mut f.spacing, spec(2, 0.1, 0.05, 1000.0), &unit);
                });
                let shift = t("draw_quality.lib.shift");
                widgets::prop_row(ui, th, &shift, |ui| {
                    num(ui, cx, ("shift", i), &shift, &mut f.shift, spec(2, 0.1, -1000.0, 1000.0), &unit);
                });
                let origin = t("draw_quality.lib.origin");
                widgets::prop_row(ui, th, &origin, |ui| {
                    // right-to-left row: the second field is added first
                    let any = spec(2, 0.1, -1000.0, 1000.0);
                    num(ui, cx, ("oy", i), &origin, &mut f.origin.1, any, &unit);
                    num(ui, cx, ("ox", i), &origin, &mut f.origin.0, any, &unit);
                });
                let dashes = t("draw_quality.lib.dashes");
                widgets::prop_row(ui, th, &dashes, |ui| {
                    text_field(ui, cx, ("dashes", i), &dashes, &mut f.dashes, &t("draw_quality.lib.dashes.hint"), NAME_W);
                });
                if button(ui, th, &t("draw_quality.lib.remove"), None, false).clicked() {
                    drop = Some(i);
                }
            });
        }
        if let Some(i) = drop {
            st.hat.fams.remove(i);
        }
        if st.hat.fams.len() < 8 && button(ui, th, &t("draw_quality.lib.add_family"), Some("plus"), false).clicked() {
            st.hat.fams.push(FamilyDraft {
                angle: 135.0,
                origin: (0.0, 0.0),
                shift: 0.0,
                spacing: if st.hat.model { 0.1 } else { 3.0 },
                dashes: String::new(),
            });
        }
    });
    ui.add_space(space::SM);
    let problem = st.hat.problem();
    match st.hat.build() {
        Ok(p) => preview_hatch(ui, ru, &p),
        Err(_) => {
            ui.allocate_space(Vec2::splat(SWATCH_H * 2.0));
        }
    }
    if let (Some(k), false) = (problem, locked) {
        kit::error_note(ui, &t(k));
    }
    ui.horizontal(|ui| {
        if locked {
            if button(ui, th, &t("draw_quality.lib.copy"), None, false).clicked() {
                st.hat.name = format!("{} {}", pattern_label(ru, &st.hat.name), t("draw_quality.lib.copy_suffix"));
                st.clean_hat = HatchDraft {
                    name: "\u{0}".into(),
                    ..st.hat.clone()
                };
                st.base_hat = st.hat.clone();
                st.pick = Pick::NewHatch;
                st.gen = st.gen.wrapping_add(1);
            }
            return;
        }
        let same = st.hat == st.clean_hat;
        let no_change = t("draw_quality.no_change");
        let why = problem.map(t).or_else(|| same.then_some(no_change));
        if kit::primary_action(ui, t("draw_quality.lib.save"), why.as_deref()) {
            save_hatch(ed, st, hats);
        }
        if let Some(n) = existing.as_ref().filter(|_| is_user) {
            let builtin = HatchLib::default().get(n).is_some();
            let label = t(if builtin { "draw_quality.lib.restore" } else { "draw_quality.lib.delete" });
            if kit::danger_button(ui, label, true).clicked() {
                let mut lib = hats.clone();
                lib.remove(n);
                if run(ed, {
                    let lib = lib.clone();
                    move |m| m.set_drawq_hatches(&lib)
                }) {
                    *hats = lib;
                    st.pick = Pick::None;
                }
            }
        }
    });
}

// ---- previews ----

fn colors(ui: &egui::Ui) -> (Color32, Color32) {
    let th = ddc_ui::theme::of(ui.ctx());
    (th.c("text.primary"), th.c("border.subtle"))
}

pub(crate) fn preview_line(ui: &mut egui::Ui, p: &LinePattern) {
    let (text, line) = colors(ui);
    let (resp, painter) = ui.allocate_painter(Vec2::new(SWATCH_W, SWATCH_H), Sense::hover());
    let rect = resp.rect;
    painter.rect_stroke(rect, 2.0, Stroke::new(1.0, line), egui::StrokeKind::Inside);
    let mm = f64::from(rect.width()) / PREVIEW_PX_PER_MM;
    let y = rect.center().y;
    for piece in p.stroke(&[(0.0, 0.0), (mm, 0.0)], PREVIEW_PX_PER_MM) {
        let pts: Vec<Pos2> = piece
            .iter()
            .map(|q| {
                Pos2::new(
                    rect.left() + (q.0 * PREVIEW_PX_PER_MM) as f32,
                    y + (q.1 * PREVIEW_PX_PER_MM) as f32,
                )
            })
            .collect();
        painter.add(egui::Shape::line(pts, Stroke::new(1.5, text)));
    }
}

pub(crate) fn preview_hatch(ui: &mut egui::Ui, ru: bool, h: &HatchPattern) {
    let (text, line) = colors(ui);
    let (resp, painter) = ui.allocate_painter(Vec2::splat(SWATCH_H * 2.0), Sense::hover());
    let rect = resp.rect;
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, line), egui::StrokeKind::Inside);
    // drafting patterns are in mm, model patterns in metres: draw one unit-square of each
    let side = if h.model { 1.0 } else { HATCH_MM };
    let ring = vec![(0.0, 0.0), (side, 0.0), (side, side), (0.0, side)];
    let Ok(segs) = h.strokes(&[ring], 1000.0) else {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            t(ru, "draw_quality.lib.too_dense"),
            egui::FontId::proportional(ddc_ui::theme::type_size::CAPTION),
            text,
        );
        return;
    };
    let k = rect.width() / side as f32;
    let to = |p: (f64, f64)| Pos2::new(rect.left() + p.0 as f32 * k, rect.bottom() - p.1 as f32 * k);
    for s in segs {
        painter.line_segment([to(s.0), to(s.1)], Stroke::new(1.0, text));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pat(name: &str) -> LinePattern {
        PatternLib::default().get(name).unwrap()
    }

    #[test]
    fn every_builtin_pattern_round_trips_through_a_draft() {
        for name in PatternLib::default().names() {
            let p = pat(&name);
            let d = PatternDraft::from_pattern(&p);
            assert_eq!(d.build(), p, "{name}");
        }
    }

    #[test]
    fn every_builtin_hatch_round_trips_through_a_draft() {
        for name in HatchLib::default().names() {
            let p = HatchLib::default().get(&name).unwrap();
            let d = HatchDraft::from_pattern(&p);
            assert_eq!(d.build().unwrap(), p, "{name}");
        }
    }

    #[test]
    fn pattern_problems_are_named() {
        let mut d = PatternDraft::default();
        assert_eq!(d.problem(), Some("draw_quality.lib.err.name"));
        d.name = "  ".into();
        assert_eq!(d.problem(), Some("draw_quality.lib.err.name"));
        d.name = "Solid".into();
        assert_eq!(d.problem(), Some("draw_quality.lib.err.reserved"));
        d.name = "mine".into();
        assert_eq!(d.problem(), None);
        d.segs.clear();
        assert_eq!(d.problem(), Some("draw_quality.lib.err.no_segs"));
        d.segs.push(SegDraft {
            kind: SegKind::Dash,
            len: 0.0,
            amp: 0.0,
        });
        assert_eq!(d.problem(), Some("draw_quality.lib.err.seg"));
        d.segs[0].len = f64::NAN;
        assert_eq!(d.problem(), Some("draw_quality.lib.err.seg"));
        d.segs[0].len = 2.0;
        d.name = "x".repeat(MAX_NAME + 1);
        assert_eq!(d.problem(), Some("draw_quality.lib.err.name_long"));
    }

    #[test]
    fn saved_pattern_joins_the_library_and_replaces_a_builtin_of_its_name() {
        let mut lib = PatternLib::default();
        let mut d = PatternDraft::from_pattern(&pat("dash"));
        d.name = "mine".into();
        d.segs[0].len = 7.0;
        assert!(lib.set(d.build()));
        assert_eq!(lib.get("mine").unwrap().segs[0], Seg::Dash(7.0));
        assert!(lib.names().contains(&"mine".to_string()));
        // same name as a built-in: the user entry wins, removing it restores the built-in
        let mut over = PatternDraft::from_pattern(&pat("dash"));
        over.segs[0].len = 9.0;
        assert!(lib.set(over.build()));
        assert_eq!(lib.get("dash").unwrap().segs[0], Seg::Dash(9.0));
        assert!(lib.remove("dash"));
        assert_eq!(lib.get("dash").unwrap().segs[0], Seg::Dash(3.0));
    }

    #[test]
    fn dash_text_parses_both_decimal_marks() {
        assert_eq!(parse_dashes(""), Some(vec![]));
        assert_eq!(parse_dashes("2 1"), Some(vec![2.0, 1.0]));
        assert_eq!(parse_dashes("2,5; 1"), Some(vec![2.5, 1.0]));
        assert_eq!(parse_dashes("2 x"), None);
        assert_eq!(parse_dashes("inf"), None);
        assert_eq!(format_dashes(&[2.5, 1.0]), "2.5 1");
        assert_eq!(parse_dashes(&format_dashes(&[0.1, 2.9])), Some(vec![0.1, 2.9]));
    }

    #[test]
    fn hatch_problems_are_named() {
        let mut d = HatchDraft::default();
        assert_eq!(d.problem(), Some("draw_quality.lib.err.name"));
        d.name = "none".into();
        assert_eq!(d.problem(), Some("draw_quality.lib.err.reserved"));
        d.name = "mine".into();
        assert_eq!(d.problem(), None);
        d.fams[0].dashes = "2 oops".into();
        assert_eq!(d.problem(), Some("draw_quality.lib.err.dashes"));
        d.fams[0].dashes = String::new();
        d.fams[0].spacing = 0.0;
        assert_eq!(d.problem(), Some("draw_quality.lib.err.family"));
        d.fams.clear();
        assert_eq!(d.problem(), Some("draw_quality.lib.err.no_fams"));
    }

    #[test]
    fn saved_hatch_draws_with_its_own_spacing() {
        let mut d = HatchDraft::default();
        d.name = "wide".into();
        d.fams[0].spacing = 6.0;
        let wide = d.build().unwrap();
        d.fams[0].spacing = 3.0;
        let tight = d.build().unwrap();
        let ring = vec![vec![(0.0, 0.0), (30.0, 0.0), (30.0, 30.0), (0.0, 30.0)]];
        let n = |p: &HatchPattern| p.strokes(&ring, 1000.0).unwrap().len();
        assert!(n(&tight) > n(&wide));
        let mut lib = HatchLib::default();
        assert!(lib.set(wide));
        assert!(lib.names().contains(&"wide".to_string()));
    }

    fn libs_with_mine() -> (PatternLib, HatchLib) {
        let mut pats = PatternLib::default();
        let d = PatternDraft {
            name: "mine".into(),
            ..PatternDraft::default()
        };
        assert!(pats.set(d.build()));
        let mut hats = HatchLib::default();
        let h = HatchDraft {
            name: "mine".into(),
            ..HatchDraft::default()
        };
        assert!(hats.set(h.build().unwrap()));
        (pats, hats)
    }

    fn opened(pick: Pick, pats: &PatternLib, hats: &HatchLib) -> State {
        let mut st = State::default();
        load_pick(&mut st, pick, pats, hats);
        st
    }

    fn guard(st: &State) -> Guard {
        switch_guard(&st.pick, (&st.pat, &st.base_pat), (&st.hat, &st.base_hat))
    }

    #[test]
    fn untouched_entries_never_ask() {
        let (pats, hats) = libs_with_mine();
        let first_hatch = hats.names()[0].clone();
        for pick in [
            Pick::None,
            Pick::Pattern("dash".into()),
            Pick::Pattern("mine".into()),
            Pick::Hatch(first_hatch),
            Pick::Hatch("mine".into()),
            Pick::NewPattern,
            Pick::NewHatch,
        ] {
            let st = opened(pick.clone(), &pats, &hats);
            assert_eq!(guard(&st), Guard::Go, "{pick:?}");
            assert!(!dirty(&st), "{pick:?}");
        }
    }

    #[test]
    fn edits_make_the_draft_dirty_and_undoing_them_makes_it_clean() {
        let (pats, hats) = libs_with_mine();
        let mut st = opened(Pick::Pattern("mine".into()), &pats, &hats);
        st.pat.segs[0].len = 9.0;
        assert_eq!(guard(&st), Guard::Ask);
        st.pat.segs[0].len = SegDraft::from_seg(Seg::Dash(4.0)).len;
        assert_eq!(guard(&st), Guard::Go);
        let mut st = opened(Pick::Hatch("mine".into()), &pats, &hats);
        st.hat.fams[0].spacing += 1.0;
        assert_eq!(guard(&st), Guard::Ask);
        let mut st = opened(Pick::NewPattern, &pats, &hats);
        st.pat.name = "x".into();
        assert_eq!(guard(&st), Guard::Ask, "a new entry with a typed name");
        let mut st = opened(Pick::NewHatch, &pats, &hats);
        st.hat.fams.clear();
        assert_eq!(guard(&st), Guard::Ask);
    }

    #[test]
    fn switching_with_edits_asks_and_keeps_the_draft() {
        let (mut pats, mut hats) = libs_with_mine();
        let mut ed = Editor::new_in_memory().unwrap();
        let mut st = opened(Pick::Pattern("mine".into()), &pats, &hats);
        st.pat.segs[0].len = 9.0;
        st.pending = Some(Pick::Pattern("dash".into()));
        let out = resolve(&mut st, &mut ed, &mut pats, &mut hats, None);
        assert_eq!(out, Outcome::Asking);
        assert_eq!(st.pick, Pick::Pattern("mine".into()));
        assert_eq!(st.pat.segs[0].len, 9.0);
        assert!(st.pending.is_some());
    }

    #[test]
    fn cancel_keeps_edits_and_selection() {
        let (mut pats, mut hats) = libs_with_mine();
        let mut ed = Editor::new_in_memory().unwrap();
        let mut st = opened(Pick::Hatch("mine".into()), &pats, &hats);
        st.hat.fams[0].spacing = 7.0;
        st.pending = Some(Pick::NewPattern);
        let out = resolve(&mut st, &mut ed, &mut pats, &mut hats, Some(Choice::Cancel));
        assert_eq!(out, Outcome::Stayed);
        assert_eq!(st.pick, Pick::Hatch("mine".into()));
        assert_eq!(st.hat.fams[0].spacing, 7.0);
        assert!(st.pending.is_none());
        assert!(dirty(&st), "still unsaved");
    }

    #[test]
    fn discard_reverts_to_the_stored_entry_and_switches() {
        let (mut pats, mut hats) = libs_with_mine();
        let mut ed = Editor::new_in_memory().unwrap();
        let mut st = opened(Pick::Pattern("mine".into()), &pats, &hats);
        st.pat.segs[0].len = 9.0;
        st.pending = Some(Pick::Pattern("dash".into()));
        let out = resolve(&mut st, &mut ed, &mut pats, &mut hats, Some(Choice::Discard));
        assert_eq!(out, Outcome::Moved);
        assert_eq!(st.pick, Pick::Pattern("dash".into()));
        assert_eq!(st.pat, PatternDraft::from_pattern(&pat("dash")));
        assert!(!dirty(&st));
        assert_eq!(pats.get("mine").unwrap().segs[0], Seg::Dash(4.0), "library untouched");
    }

    #[test]
    fn clean_switch_does_not_ask() {
        let (mut pats, mut hats) = libs_with_mine();
        let mut ed = Editor::new_in_memory().unwrap();
        let mut st = opened(Pick::Pattern("mine".into()), &pats, &hats);
        st.pending = Some(Pick::NewHatch);
        let out = resolve(&mut st, &mut ed, &mut pats, &mut hats, None);
        assert_eq!(out, Outcome::Moved);
        assert_eq!(st.pick, Pick::NewHatch);
    }

    #[test]
    fn apply_saves_in_one_undo_step_then_switches() {
        let mut ed = Editor::new_in_memory().unwrap();
        let mut pats = ed.model.drawq_patterns().unwrap();
        let mut hats = ed.model.drawq_hatches().unwrap();
        let mut st = opened(Pick::NewPattern, &pats, &hats);
        st.pat.name = "mine2".into();
        st.pending = Some(Pick::Pattern("dash".into()));
        let out = resolve(&mut st, &mut ed, &mut pats, &mut hats, Some(Choice::Apply));
        assert_eq!(out, Outcome::Moved);
        assert_eq!(st.pick, Pick::Pattern("dash".into()));
        assert!(pats.get("mine2").is_some());
        assert!(ed.model.drawq_patterns().unwrap().get("mine2").is_some());
        ed.undo();
        assert!(ed.model.drawq_patterns().unwrap().get("mine2").is_none());
        assert!(!ed.can_undo(), "one step");
    }

    #[test]
    fn apply_of_a_bad_draft_stays_with_the_edits() {
        let mut ed = Editor::new_in_memory().unwrap();
        let mut pats = ed.model.drawq_patterns().unwrap();
        let mut hats = ed.model.drawq_hatches().unwrap();
        let mut st = opened(Pick::NewPattern, &pats, &hats);
        st.pat.name = "  ".into();
        st.pat.segs[0].len = 3.0;
        st.pending = Some(Pick::NewHatch);
        let out = resolve(&mut st, &mut ed, &mut pats, &mut hats, Some(Choice::Apply));
        assert_eq!(out, Outcome::Stayed);
        assert_eq!(st.pick, Pick::NewPattern);
        assert!(dirty(&st));
        assert!(!ed.can_undo(), "nothing stored");
    }

    #[test]
    fn closing_with_edits_is_seen_and_saved_or_discarded() {
        let mut ed = Editor::new_in_memory().unwrap();
        let mut pats = ed.model.drawq_patterns().unwrap();
        let mut hats = ed.model.drawq_hatches().unwrap();
        discard();
        assert!(!is_dirty());
        let mut st = opened(Pick::NewHatch, &pats, &hats);
        st.hat.name = "h1".into();
        STATE.with(|s| *s.borrow_mut() = st);
        assert!(is_dirty());
        assert_eq!(dirty_name(false), "h1");
        assert!(save_current(&mut ed, &mut pats, &mut hats));
        assert!(!is_dirty());
        assert!(ed.model.drawq_hatches().unwrap().get("h1").is_some());
        let mut st = opened(Pick::NewHatch, &pats, &hats);
        st.hat.name = "h2".into();
        STATE.with(|s| *s.borrow_mut() = st);
        assert!(is_dirty());
        discard();
        assert!(!is_dirty());
    }

    #[test]
    fn question_strings_exist_in_both_languages() {
        for key in [
            "draw_quality.custom",
            "draw_quality.custom.tip",
            "draw_quality.custom_note",
            "draw_quality.unsaved.lib_body",
            "draw_quality.unsaved.styles_body",
            "draw_quality.unsaved.both_body",
            "draw_quality.halftone",
            "draw_quality.styles.name",
            "type_layers.unsaved.title",
            "type_layers.unsaved.discard",
            "type_dialog.typedial.cancel",
        ] {
            let (en, ru) = (crate::tr_lang(false, key, &[]), crate::tr_lang(true, key, &[]));
            assert_ne!(en, key, "{key} missing in en");
            assert_ne!(ru, key, "{key} missing in ru");
            if key.starts_with("draw_quality.") {
                assert_ne!(en, ru, "{key} not translated");
            }
        }
    }

    #[test]
    fn windows_use_kit_widgets_only() {
        for (name, src) in [
            ("range_libs.rs", include_str!("range_libs.rs")),
            ("range_styles.rs", include_str!("range_styles.rs")),
        ] {
            let body = src.split("#[cfg(test)]").next().unwrap();
            for bad in [
                "egui::ComboBox",
                "egui::DragValue",
                "ui.checkbox",
                "\\u{2715}",
                "egui::Button::new",
                "selectable_label",
                "ui.small_button",
                "ui.button(",
            ] {
                assert!(!body.contains(bad), "{name} still uses {bad}");
            }
        }
    }

    #[test]
    fn library_edits_are_one_undo_step() {
        let mut m = ddc_model::Model::open(":memory:").unwrap();
        let mut lib = m.drawq_patterns().unwrap();
        let mut d = PatternDraft::default();
        d.name = "mine".into();
        assert!(lib.set(d.build()));
        m.set_drawq_patterns(&lib).unwrap();
        assert!(m.drawq_patterns().unwrap().get("mine").is_some());
        m.undo().unwrap();
        assert!(m.drawq_patterns().unwrap().get("mine").is_none());
        let mut hl = m.drawq_hatches().unwrap();
        let mut h = HatchDraft::default();
        h.name = "mine".into();
        assert!(hl.set(h.build().unwrap()));
        m.set_drawq_hatches(&hl).unwrap();
        m.undo().unwrap();
        assert!(m.drawq_hatches().unwrap().get("mine").is_none());
    }
}
