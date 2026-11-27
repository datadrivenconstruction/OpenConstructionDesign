// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! AI modes: the chat, build and review tabs at the right end of the view-tab bar. Each draws
//! the assistant's one conversation (`PanelUi`: the same log, cards and composer as the quick
//! panel) full size in a work column beside a live preview of the model. Nothing here keeps a
//! second copy of the conversation; every action runs through `PanelUi::run` like the panel's.

use super::kit::space;
use super::Entry;
pub use crate::shell::fold::AiFold;
use crate::shell::fold::RAIL_W;
use crate::shell::EditorApp;
use ddc_ui::sem::{control, stroke};
use ddc_ui::theme::Role;
use ddc_ui::{widgets, Catalog};
use egui::{self, Ui};

/// One of the AI tabs of the view bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AiMode {
    Chat,
    Build,
    Review,
}

impl AiMode {
    pub const ALL: [AiMode; 3] = [AiMode::Chat, AiMode::Build, AiMode::Review];

    pub fn label_key(self) -> &'static str {
        match self {
            AiMode::Chat => "ai_modes.tab.chat",
            AiMode::Build => "ai_modes.tab.build",
            AiMode::Review => "ai_modes.tab.review",
        }
    }

    /// The palette command that opens the tab.
    pub fn command(self) -> &'static str {
        match self {
            AiMode::Chat => "ai.mode_chat",
            AiMode::Build => "ai.mode_build",
            AiMode::Review => "ai.mode_review",
        }
    }

    /// Icon of the tab: what the mode does, not the AI mark (the three would read alike).
    pub fn icon(self) -> &'static str {
        match self {
            AiMode::Chat => "ai-chat",
            AiMode::Build => "ai-build",
            AiMode::Review => "ai-review",
        }
    }

    /// Title of the work column.
    pub fn title_key(self) -> &'static str {
        match self {
            AiMode::Chat => "ai_modes.title.chat",
            AiMode::Build => "ai_modes.title.build",
            AiMode::Review => "ai_modes.title.review",
        }
    }
}

/// What the preview pane beside the work column shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Preview {
    #[default]
    Plan,
    D3,
    Sheet,
}

/// The view a mode opened over. When another view opens (a tab, the browser, a sheet) the view
/// state no longer matches and the mode gives way to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Base {
    pub sheet: Option<i64>,
    pub drafting: Option<i64>,
    pub view3d: bool,
    pub section: Option<i64>,
}

/// The view state of `app`, read the way the view area decides what it draws.
pub fn base_of(app: &EditorApp) -> Base {
    let sheet = crate::sheet_view::active();
    let drafting = crate::drafting_view::current(&app.ed)
        .filter(|_| sheet.is_none())
        .map(crate::drafting_view::tab_id);
    Base {
        sheet,
        drafting,
        view3d: app.view3d,
        section: app.section.filter(|_| !app.view3d),
    }
}

/// Which step of draft, review and Apply, drawings and files the build tab is at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildStep {
    /// Writing the brief and drafting a proposal.
    Brief,
    Review,
    Drawings,
    Files,
}

impl BuildStep {
    pub const ALL: [BuildStep; 4] = [
        BuildStep::Brief,
        BuildStep::Review,
        BuildStep::Drawings,
        BuildStep::Files,
    ];

    pub fn key(self) -> &'static str {
        match self {
            BuildStep::Brief => "ai_modes.build.step.draft",
            BuildStep::Review => "ai_modes.build.step.review",
            BuildStep::Drawings => "ai_modes.build.step.drawings",
            BuildStep::Files => "ai_modes.build.step.files",
        }
    }
}

/// The step from the shared conversation and the model: a draft waiting is Review; an Apply
/// newer than the last request and not undone is Drawings, or Files once the model has sheets;
/// anything else (a running turn, "New brief", nothing yet) is the Draft step with the brief.
pub fn build_step(
    log: &[Entry],
    pending: bool,
    busy: bool,
    fresh: bool,
    sheets: bool,
) -> BuildStep {
    if pending {
        return BuildStep::Review;
    }
    if busy || fresh {
        return BuildStep::Brief;
    }
    let asked = log.iter().rposition(|e| matches!(e, Entry::User(_)));
    let applied = log.iter().enumerate().rev().find_map(|(i, e)| match e {
        Entry::Applied(r) => Some((i, r.undone)),
        _ => None,
    });
    match applied {
        Some((i, false)) if asked.is_none_or(|u| i > u) && sheets => BuildStep::Files,
        Some((i, false)) if asked.is_none_or(|u| i > u) => BuildStep::Drawings,
        _ => BuildStep::Brief,
    }
}

/// Files the build tab makes after the drawings: label and the export command.
pub const OUTPUTS: [(&str, &str); 2] = [
    ("ai_modes.build.out.pdf", "data.export_pdf"),
    ("ai_modes.build.out.ifc", "data.export_ifc"),
];

/// The typed part of the build brief; it goes to the model as the spec's own field values.
#[derive(Clone, Debug, PartialEq)]
pub struct BuildForm {
    /// Index into `ddc_gen::project::Typology::NAMES`.
    pub kind: usize,
    pub floors: u32,
    pub floor_h: f64,
    pub outputs: [bool; 2],
}

impl Default for BuildForm {
    fn default() -> Self {
        Self {
            kind: 0,
            floors: 2,
            floor_h: 3.0,
            outputs: [true, true],
        }
    }
}

impl BuildForm {
    /// The fields as the generator spec names them, kept inside its limits.
    pub fn spec_line(&self) -> String {
        use ddc_gen::project::{limits::FLOOR_HEIGHT, slots::MAX_STOREYS, Typology};
        let kind = Typology::NAMES[self.kind.min(Typology::NAMES.len() - 1)];
        let floors = self.floors.clamp(1, MAX_STOREYS);
        let h = self.floor_h.clamp(FLOOR_HEIGHT.min, FLOOR_HEIGHT.max);
        format!("typology={kind}, floors={floors}, floor_height={h:.2}")
    }

    /// The export commands of the ticked outputs, in form order.
    pub fn output_cmds(&self) -> impl Iterator<Item = &'static str> + '_ {
        OUTPUTS
            .iter()
            .zip(self.outputs)
            .filter(|(_, on)| *on)
            .map(|((_, id), _)| *id)
    }
}

/// State of the AI tabs; part of the one `PanelUi`.
pub struct Modes {
    pub cur: Option<AiMode>,
    base: Base,
    /// Share of the view area the work column takes.
    pub split: f32,
    /// Share of the review column the checks take; the answers get the rest.
    pub review_split: f32,
    pub preview: Preview,
    pub brief: String,
    pub form: BuildForm,
    pub focus_brief: bool,
    /// "New brief" after a finished build: the brief step until the next request.
    pub fresh: bool,
}

impl Default for Modes {
    fn default() -> Self {
        Self {
            cur: None,
            base: Base::default(),
            split: 0.5,
            review_split: 0.5,
            preview: Preview::Plan,
            brief: String::new(),
            form: BuildForm::default(),
            focus_brief: false,
            fresh: false,
        }
    }
}

impl Modes {
    /// Opens tab `m` over the view `base`; an open sheet becomes the preview.
    pub fn enter(&mut self, m: AiMode, base: Base) {
        self.cur = Some(m);
        self.base = base;
        if base.sheet.is_some() {
            self.preview = Preview::Sheet;
        } else if base.view3d {
            self.preview = Preview::D3;
        } else if self.preview == Preview::Sheet {
            self.preview = Preview::Plan;
        }
    }

    pub fn leave(&mut self) {
        self.cur = None;
    }

    /// Once a frame with the view state: another view opened since the mode did leaves it.
    pub fn sync(&mut self, now: Base) -> Option<AiMode> {
        if self.cur.is_some() && now != self.base {
            self.cur = None;
        }
        self.cur
    }

    /// The preview pane changed what is open (a sheet): that is the mode's own view now.
    pub fn rebase(&mut self, now: Base) {
        self.base = now;
    }

    /// The pane shows `p`: a sheet opens in the sheet view, leaving the sheet closes it. The
    /// caller rebases on the view state that results.
    pub fn to_preview(&mut self, p: Preview, sheet: Option<i64>) {
        match (p, sheet) {
            (Preview::Sheet, Some(id)) => crate::sheet_view::open(id),
            (Preview::Sheet, None) => return,
            _ => crate::sheet_view::deactivate(),
        }
        self.preview = p;
    }
}

/// Smallest work column and preview; with less room than both the preview folds to a rail.
/// The preview minimum keeps a usable canvas beside the work column at 1280 px.
pub const WORK_MIN: f32 = 400.0;
pub const PREVIEW_MIN: f32 = 560.0;
const GRIP: f32 = 8.0;

/// Where a mode draws inside the view area.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    /// Empty when the work column is folded.
    pub work: egui::Rect,
    pub grip: egui::Rect,
    /// The preview's title bar with the Plan / 3D / Sheet switch.
    pub bar: egui::Rect,
    /// Where the plan, 3D or sheet draws; empty when the preview is folded.
    pub view: egui::Rect,
    /// The folded side's rail, and which side it is; `AiFold::Open` with an empty rect.
    pub rail: egui::Rect,
    pub folded: AiFold,
}

/// Work column on the left (`split` of the width, kept between the minimums), preview right;
/// `fold` puts one of them on a rail, and so does a lack of room for the preview.
pub fn layout(r: egui::Rect, split: f32, fold: AiFold) -> Layout {
    let none = egui::Rect::NOTHING;
    let bar_h = control::TAB_H + space::SM;
    let preview = |right: egui::Rect| {
        let bar = egui::Rect::from_min_size(right.min, egui::vec2(right.width(), bar_h));
        let view = egui::Rect::from_min_max(egui::pos2(right.left(), bar.bottom()), right.max);
        (bar, view)
    };
    let narrow = r.width() < WORK_MIN + PREVIEW_MIN + GRIP;
    match fold {
        AiFold::Work => {
            let (rail, right) = split_left(r, RAIL_W);
            let (bar, view) = preview(right);
            Layout { work: none, grip: none, bar, view, rail, folded: AiFold::Work }
        }
        _ if fold == AiFold::Preview || narrow => {
            let (work, rail) = split_left(r, r.width() - RAIL_W);
            Layout { work, grip: none, bar: none, view: none, rail, folded: AiFold::Preview }
        }
        _ => {
            let w = (r.width() * split).clamp(WORK_MIN, r.width() - PREVIEW_MIN - GRIP);
            let (work, rest) = split_left(r, w);
            let (grip, right) = split_left(rest, GRIP);
            let (bar, view) = preview(right);
            Layout { work, grip, bar, view, rail: none, folded: AiFold::Open }
        }
    }
}

fn split_left(r: egui::Rect, w: f32) -> (egui::Rect, egui::Rect) {
    let x = r.left() + w;
    (
        egui::Rect::from_min_max(r.min, egui::pos2(x, r.bottom())),
        egui::Rect::from_min_max(egui::pos2(x, r.top()), r.max),
    )
}

/// The tab shown this frame, after checking that the view under it did not change.
pub fn sync(now: Base) -> Option<(AiMode, Preview, f32)> {
    super::with(|st| st.modes.sync(now).map(|m| (m, st.modes.preview, st.modes.split)))
}

pub fn current() -> Option<AiMode> {
    super::with(|st| st.modes.cur)
}

/// Opens tab `m`: the chat takes the keyboard at once, the build tab its brief.
pub fn enter(app: &mut EditorApp, m: AiMode) {
    let base = base_of(app);
    super::with(|st| {
        st.init(app);
        st.watch_project(app);
        st.modes.enter(m, base);
        match m {
            AiMode::Chat | AiMode::Review => st.focus_input = true,
            AiMode::Build => st.modes.focus_brief = true,
        }
    });
}

pub fn leave() {
    super::with(|st| st.modes.leave());
}

/// Width of the AI tabs for `paint_tabs`, separator included.
pub fn tabs_width(ui: &Ui, cat: &Catalog) -> f32 {
    let font = ddc_ui::typo::font(Role::Tab);
    let mark = ddc_ui::sem::icon_size::SM + space::XS;
    let gap = space::XXS;
    let sum: f32 = AiMode::ALL
        .iter()
        .map(|m| {
            let g = ui
                .painter()
                .layout_no_wrap(cat.t(m.label_key()), font.clone(), egui::Color32::PLACEHOLDER);
            g.size().x + mark + 2.0 * space::MD + gap
        })
        .sum();
    sum + space::SM
}

/// Paints the AI tabs in `strip` (tab height `h`) from `start`, after a separator, as the end
/// of the view tabs; `tabs_width` is their room. Returns the tab clicked this frame.
pub fn paint_tabs(
    ui: &mut Ui,
    cat: &Catalog,
    icons: &mut ddc_ui::icons::IconCache,
    strip: egui::Rect,
    h: f32,
    start: f32,
    cur: Option<AiMode>,
) -> Option<AiMode> {
    use ddc_ui::sem::radius;
    let th = ddc_ui::theme::of(ui.ctx());
    let font = ddc_ui::typo::font(Role::Tab);
    let mark = ddc_ui::sem::icon_size::SM;
    let gap = space::XXS;
    let p = ui.painter_at(strip);
    let left = start + space::SM;
    p.vline(
        left - space::XS,
        strip.shrink2(egui::vec2(0.0, space::SM)).y_range(),
        egui::Stroke::new(stroke::HAIRLINE, th.c("border.subtle")),
    );
    // The modes are one segmented switch, not more document tabs beside the views
    let widths: Vec<f32> = AiMode::ALL
        .iter()
        .map(|m| {
            let g = p.layout_no_wrap(cat.t(m.label_key()), font.clone(), egui::Color32::PLACEHOLDER);
            g.size().x + mark + space::XS + 2.0 * space::MD
        })
        .collect();
    let inset = egui::vec2(0.0, space::XS);
    let total = widths.iter().sum::<f32>() + gap * (widths.len() - 1) as f32;
    let track = egui::Rect::from_min_size(egui::pos2(left, strip.top()), egui::vec2(total, h))
        .shrink2(inset);
    p.rect(
        track,
        radius::MD,
        th.c("bg.app").gamma_multiply(0.5),
        egui::Stroke::new(stroke::HAIRLINE, th.c("border.subtle")),
        egui::StrokeKind::Inside,
    );
    let mut x = left;
    let mut out = None;
    for (m, w) in AiMode::ALL.into_iter().zip(widths) {
        let name = cat.t(m.label_key());
        let r = egui::Rect::from_min_size(egui::pos2(x, strip.top()), egui::vec2(w, h));
        x += w + gap;
        let on = cur == Some(m);
        let resp = ui.interact(r, ui.id().with(("ai-mode-tab", m)), egui::Sense::click());
        resp.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, on, &name)
        });
        let seg = r.shrink2(inset).shrink(stroke::INDICATOR);
        if on {
            p.rect_filled(seg, radius::SM, th.c("bg.panel"));
        } else if resp.hovered() {
            p.rect_filled(seg, radius::SM, th.c("bg.panel").gamma_multiply(0.5));
        }
        let c = egui::pos2(r.left() + space::MD + mark / 2.0, r.center().y);
        let color = if on { th.c("accent") } else { th.c("text.secondary") };
        let ir = egui::Rect::from_center_size(c, egui::vec2(mark, mark));
        icons.paint(ui, m.icon(), mark as u32, ir, color);
        let g = p.layout_no_wrap(name.clone(), font.clone(), color);
        let gy = r.center().y - g.size().y / 2.0;
        p.galley(egui::pos2(c.x + mark / 2.0 + space::XS, gy), g, color);
        let keys = KEYS.with(|k| k.borrow()[m as usize].clone());
        let desc = cat.t(&format!("cmd.{}.desc", m.command()));
        let resp = widgets::rich_tooltip(resp, &th, &name, &desc, &keys);
        if resp.clicked() {
            out = Some(m);
        }
    }
    out
}

thread_local! {
    /// Shortcuts of the mode commands, read from the registry once when they are registered.
    static KEYS: std::cell::RefCell<[String; 3]> = Default::default();
}

/// Keeps the shortcuts of the mode commands for the tab tooltips.
pub fn note_keys(reg: &ddc_ui::Registry) {
    let keys = AiMode::ALL.map(|m| {
        reg.get(m.command())
            .map(|c| c.shortcuts.join(" / "))
            .unwrap_or_default()
    });
    KEYS.with(|k| *k.borrow_mut() = keys);
}
