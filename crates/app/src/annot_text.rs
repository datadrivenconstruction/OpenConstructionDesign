//! View annotations in the app: commands, placement (each one `run_op`), selection and grips, in-place
//! text editing, the parameters panel, drawing on plan and section. Screen and sheet use the same
//! model strokes (`Anno::strokes`), so label size depends only on the view scale.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use crate::editor::{Editor, OpFn, Sel};
use crate::mass_tools::Step;
use crate::shell::EditorApp;
use crate::ui_kit::Tokens;
use crate::wall_tool::{Event, Key, Mode};
use ddc_geom::{v2, V2};
use ddc_io::sheets::{view_anno_fmt, AnnoFmt};
use ddc_model::{
    dash_segments, stroke_hit, strokes_bbox, Anno, AnnoCtx, AnnoShape, AnnoStroke, AnnoView,
    AnnoXf, DetailDef, DetailFit, Leader, LeaderHead, Model, RegionFill, SpotKind, TextAlign,
    TextNote, SCALES,
};
use ddc_ui::i18n::{Catalog, Lang};
use ddc_ui::theme::Role;
use ddc_ui::widgets;
use ddc_ui::Theme;
use egui::{self, Color32, Pos2, Rect, Stroke, Vec2};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod ux50d_tests;
mod win;

type P = (f64, f64);

/// Labels smaller than this are not drawn on screen; larger ones are clamped (font atlas).
const TEXT_MIN_PX: f32 = 3.0;
const TEXT_MAX_PX: f32 = 256.0;
const HIT_PX: f32 = 8.0;
/// Dashes shorter than this blur together on screen, so draw a solid line.
const DASH_MIN_PX: f64 = 4.0;
/// Hatching with a pitch finer than this is not drawn on screen: thousands of lines blur into a blob.
const HATCH_MIN_PX: f64 = 3.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Leader,
    DetailLine,
    Region,
    Detail,
    SpotElev,
    SpotCoord,
    Cloud,
    DimChain,
    AddLeader,
}

impl Kind {
    fn key(self) -> &'static str {
        match self {
            Kind::Text => "text",
            Kind::Leader => "leader",
            Kind::DetailLine => "detail_line",
            Kind::Region => "region",
            Kind::Detail => "detail",
            Kind::SpotElev => "spot_elev",
            Kind::SpotCoord => "spot_coord",
            Kind::Cloud => "cloud",
            Kind::DimChain => "dim_chain",
            Kind::AddLeader => "add_leader",
        }
    }
    fn cmd(self) -> &'static str {
        match self {
            Kind::AddLeader => "cmd.ann.leader",
            Kind::Text => "cmd.ann.text",
            Kind::Leader => "cmd.ann.leader",
            Kind::DetailLine => "cmd.ann.detail_line",
            Kind::Region => "cmd.ann.region",
            Kind::Detail => "cmd.ann.detail",
            Kind::SpotElev => "cmd.ann.spot_elev",
            Kind::SpotCoord => "cmd.ann.spot_coord",
            Kind::Cloud => "cmd.ann.cloud",
            Kind::DimChain => "cmd.ann.dim_chain",
        }
    }
}

#[derive(Clone)]
struct Opts {
    text_style: Option<i64>,
    line_style: Option<i64>,
    align: TextAlign,
    head: LeaderHead,
    wrap: bool,
    wrap_mm: f64,
    elbow: bool,
    /// 0 = fill, 1 = mask, 2 = hatching.
    fill: u8,
    gray: u8,
    deg: f64,
    step_mm: f64,
    cross: bool,
    boundary: bool,
    def: Option<i64>,
    rot_deg: f64,
    flip: bool,
    mark: String,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            text_style: None,
            line_style: None,
            align: TextAlign::Left,
            head: LeaderHead::Arrow,
            wrap: false,
            wrap_mm: 60.0,
            elbow: false,
            fill: 2,
            gray: 180,
            deg: 45.0,
            step_mm: 2.0,
            cross: false,
            boundary: true,
            def: None,
            rot_deg: 0.0,
            flip: false,
            mark: String::new(),
        }
    }
}

impl Opts {
    fn region_fill(&self) -> RegionFill {
        match self.fill {
            0 => RegionFill::Solid { gray: self.gray },
            1 => RegionFill::Mask,
            _ => RegionFill::Hatch {
                deg: self.deg,
                step_mm: self.step_mm,
                cross: self.cross,
            },
        }
    }
}

struct Draft {
    kind: Kind,
    pts: Vec<V2>,
    rings: Vec<Vec<P>>,
    cur: V2,
    target: Option<i64>,
}

impl Draft {
    fn reset(&mut self) {
        self.pts.clear();
        self.rings.clear();
    }
}

struct Drag {
    id: i64,
    grip: Option<usize>,
    shape: AnnoShape,
    from: V2,
    cur: V2,
    view: AnnoView,
}

impl Drag {
    /// Shape with the cursor at `p`; moving text leaves leader endpoints in place.
    fn shape_at(&self, p: V2) -> AnnoShape {
        match (self.grip, &self.shape) {
            (Some(i), s) => s.with_grip(i, (p.x, p.y)),
            (None, AnnoShape::Text(t)) => {
                let (dx, dy) = (p.x - self.from.x, p.y - self.from.y);
                self.shape.with_grip(0, (t.pos.0 + dx, t.pos.1 + dy))
            }
            (None, s) => s.translated((p.x - self.from.x, p.y - self.from.y)),
        }
    }
}

struct TextEditing {
    id: Option<i64>,
    view: AnnoView,
    note: TextNote,
    text_style: Option<i64>,
    line_style: Option<i64>,
    buf: String,
    focus: bool,
}

struct Item {
    anno: Anno,
    strokes: Vec<AnnoStroke>,
    /// An area fill does not block picking elements beneath it: hit-testing is by outline.
    hit: Vec<AnnoStroke>,
    bbox: Option<[f64; 4]>,
    /// How many leading strokes are hatch lines, and their pitch on the sheet, mm.
    hatch: (usize, f64),
    /// Even-odd fill trapezoids for each `AnnoStroke::Fill` (view coordinates).
    fills: Vec<Vec<[P; 4]>>,
}

struct Items {
    list: Vec<Item>,
    scale: f64,
    fmt: AnnoFmt,
}

fn ctx_of(its: &Items) -> AnnoCtx<'_> {
    AnnoCtx {
        scale: its.scale,
        sep: &its.fmt.sep,
        north: &its.fmt.north,
        east: &its.fmt.east,
        measure: &measure,
    }
}

/// Which view is on screen: actions on the selection leave other views' annotations alone.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Shown {
    #[default]
    Plan,
    Section(i64),
    Drafting(i64),
    Other,
}

struct Cache {
    view: AnnoView,
    rev: (u64, u64),
    items: Rc<Items>,
}

#[derive(Default)]
struct State {
    draft: Option<Draft>,
    opts: Opts,
    drag: Option<Drag>,
    edit: Option<TextEditing>,
    cache: Option<Cache>,
    shown: Shown,
    /// Parameters-panel lists: read the model, live until the revision or language changes.
    lists: Option<((u64, u64), Lang, Rc<Lists>)>,
    cat: Option<Rc<Catalog>>,
    legend: win::LegendUi,
}

impl State {
    fn set_lang(&mut self, lang: Lang) {
        if self.cat.as_ref().map(|c| c.lang) != Some(lang) {
            self.cat = Some(Rc::new(Catalog::new(lang)));
        }
    }
    fn catalog(&mut self) -> Rc<Catalog> {
        self.cat
            .get_or_insert_with(|| Rc::new(Catalog::new(Lang::Ru)))
            .clone()
    }
}

thread_local! {
    static ST: RefCell<State> = RefCell::new(State::default());
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    ST.with(|s| f(&mut s.borrow_mut()))
}

pub(crate) fn cat() -> Rc<Catalog> {
    with(|s| s.catalog())
}

fn measure(s: &str, h: f64) -> f64 {
    ddc_io::sheets::text_width(s, h)
}

fn hit_strokes(a: &Anno, strokes: &[AnnoStroke]) -> Vec<AnnoStroke> {
    match &a.shape {
        AnnoShape::Region { rings, .. } => rings
            .iter()
            .map(|r| AnnoStroke::Line {
                pts: r.clone(),
                closed: true,
                weight_mm: 0.0,
                dash_mm: vec![],
            })
            .collect(),
        _ => strokes.to_vec(),
    }
}

fn hatch_of(a: &Anno, n: usize) -> (usize, f64) {
    match &a.shape {
        AnnoShape::Region {
            rings,
            fill: RegionFill::Hatch { step_mm, .. },
            boundary,
        } => (
            n.saturating_sub(if *boundary { rings.len() } else { 0 }),
            *step_mm,
        ),
        _ => (0, 0.0),
    }
}

/// Even-odd split of rings into trapezoids between `y` breakpoints (like `fill_even_odd`, but
/// computed once per revision and in view coordinates).
fn even_odd_cells(rings: &[Vec<P>]) -> Vec<[P; 4]> {
    let edges: Vec<(P, P)> = rings
        .iter()
        .flat_map(|r| (0..r.len()).map(move |i| (r[i], r[(i + 1) % r.len()])))
        .filter(|(a, b)| (a.1 - b.1).abs() > 1e-9)
        .collect();
    let mut ys: Vec<f64> = rings.iter().flatten().map(|p| p.1).collect();
    ys.sort_by(f64::total_cmp);
    ys.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    let x_at = |(a, b): (P, P), y: f64| a.0 + (b.0 - a.0) * (y - a.1) / (b.1 - a.1);
    let mut out = vec![];
    for w in ys.windows(2) {
        let (y0, y1) = (w[0], w[1]);
        let ym = (y0 + y1) / 2.0;
        let mut xs: Vec<(f64, (P, P))> = edges
            .iter()
            .filter(|(a, b)| a.1.min(b.1) <= ym && a.1.max(b.1) >= ym)
            .map(|&e| (x_at(e, ym), e))
            .collect();
        xs.sort_by(|a, b| a.0.total_cmp(&b.0));
        for pair in xs.chunks_exact(2) {
            let (l, r) = (pair[0].1, pair[1].1);
            out.push([
                (x_at(l, y0), y0),
                (x_at(r, y0), y0),
                (x_at(r, y1), y1),
                (x_at(l, y1), y1),
            ]);
        }
    }
    out
}

fn fill_cells(strokes: &[AnnoStroke]) -> Vec<Vec<[P; 4]>> {
    strokes
        .iter()
        .filter_map(|st| match st {
            AnnoStroke::Fill { rings, .. } => Some(even_odd_cells(rings)),
            _ => None,
        })
        .collect()
}

fn items(ed: &Editor, view: AnnoView) -> Rc<Items> {
    let rev = ed.cache_key();
    let cached = with(|s| {
        s.cache
            .as_ref()
            .filter(|c| c.view == view && c.rev == rev)
            .map(|c| c.items.clone())
    });
    if let Some(c) = cached {
        return c;
    }
    let scale = ed.model.anno_scale(view).unwrap_or(100.0);
    let fmt = view_anno_fmt(&ed.model, view);
    let ctx = AnnoCtx {
        scale,
        sep: &fmt.sep,
        north: &fmt.north,
        east: &fmt.east,
        measure: &measure,
    };
    let list = ed
        .model
        .annos(view)
        .unwrap_or_default()
        .into_iter()
        .map(|anno| {
            let strokes = anno.strokes(&ctx);
            let hit = hit_strokes(&anno, &strokes);
            let bbox = strokes_bbox(&strokes, &ctx);
            let hatch = hatch_of(&anno, strokes.len());
            let fills = fill_cells(&strokes);
            Item {
                anno,
                strokes,
                hit,
                bbox,
                hatch,
                fills,
            }
        })
        .collect();
    let items = Rc::new(Items { list, scale, fmt });
    with(|s| {
        s.cache = Some(Cache {
            view,
            rev,
            items: items.clone(),
        })
    });
    items
}

fn hit(ed: &Editor, view: AnnoView, p: V2, r: f64) -> Option<i64> {
    let its = items(ed, view);
    let ctx = ctx_of(&its);
    its.list
        .iter()
        .rev()
        .find(|i| stroke_hit(&i.hit, (p.x, p.y), r, &ctx))
        .map(|i| i.anno.id)
}

// ---------- selection: `ed.selection`, same as for elements ----------

fn anno_ids(ed: &Editor) -> Vec<i64> {
    ed.selection
        .iter()
        .filter_map(|s| match s {
            Sel::Anno(id) => Some(*id),
            _ => None,
        })
        .collect()
}

/// The single selected annotation: it gets grips and a parameters panel.
fn single(ed: &Editor) -> Option<i64> {
    match ed.selection.as_slice() {
        [Sel::Anno(id)] => Some(*id),
        _ => None,
    }
}

fn drop_annos(ed: &mut Editor) {
    ed.selection.retain(|s| !matches!(s, Sel::Anno(_)));
}

fn current_view(ed: &Editor) -> Option<AnnoView> {
    match with(|s| s.shown) {
        Shown::Plan => Some(AnnoView::Plan(ed.level)),
        Shown::Section(id) => Some(AnnoView::Section(id)),
        Shown::Drafting(id) => Some(AnnoView::Drafting(id)),
        Shown::Other => None,
    }
}

fn in_view(ed: &Editor, view: AnnoView, id: i64) -> bool {
    items(ed, view).list.iter().any(|i| i.anno.id == id)
}

/// Shell frame hook: which view is shown. On a change the move is reset and text editing is committed.
pub fn sync_view(ed: &mut Editor, other: bool, section: Option<i64>) {
    let now = match (other, section) {
        (true, _) => Shown::Other,
        (false, Some(id)) => Shown::Section(id),
        (false, None) => Shown::Plan,
    };
    set_shown(ed, now);
}

/// Shell frame with a drafting view tab on screen.
pub fn sync_drafting(ed: &mut Editor, id: i64) {
    set_shown(ed, Shown::Drafting(id));
}

fn set_shown(ed: &mut Editor, now: Shown) {
    if with(|s| std::mem::replace(&mut s.shown, now)) != now {
        with(|s| s.drag = None);
        commit_edit(ed);
    }
}

/// View the edit tools act on: the open drafting view, else the current plan.
pub fn tool_view(ed: &Editor) -> AnnoView {
    match with(|s| s.shown) {
        Shown::Drafting(id) => AnnoView::Drafting(id),
        _ => AnnoView::Plan(ed.level),
    }
}

/// A drafting view is on screen: the edit tools work on its items alone.
pub fn in_drafting(ed: &Editor) -> bool {
    matches!(tool_view(ed), AnnoView::Drafting(_))
}

/// Outline for box selection and the rotation center: the bounding box in tool views, otherwise empty.
pub fn outlines(ed: &Editor, id: i64) -> Vec<Vec<V2>> {
    let its = items(ed, tool_view(ed));
    its.list
        .iter()
        .find(|i| i.anno.id == id)
        .and_then(|i| i.bbox)
        .map(|b| {
            // a straight line has no area: its outline is the segment, so its middle stays an edge
            if b[2] - b[0] < 1e-9 || b[3] - b[1] < 1e-9 {
                return vec![vec![v2(b[0], b[1]), v2(b[2], b[3])]];
            }
            vec![vec![
                v2(b[0], b[1]),
                v2(b[2], b[1]),
                v2(b[2], b[3]),
                v2(b[0], b[3]),
            ]]
        })
        .unwrap_or_default()
}

/// Vertices of the tool view's items (line points, text anchors): in a drafting view these are
/// the only snap points, the hidden plan has none.
pub fn view_vertices(ed: &Editor) -> Vec<V2> {
    let its = items(ed, tool_view(ed));
    let mut v = vec![];
    for i in &its.list {
        for s in &i.strokes {
            match s {
                AnnoStroke::Line { pts, .. } => v.extend(pts.iter().map(|q| v2(q.0, q.1))),
                AnnoStroke::Text { pos, .. } => v.push(v2(pos.0, pos.1)),
                AnnoStroke::Fill { .. } => {}
            }
        }
    }
    v
}

/// Item of the tool view under `p` (topmost).
pub fn item_at(ed: &Editor, p: V2, r: f64) -> Option<i64> {
    hit(ed, tool_view(ed), p, r)
}

/// Annotations of the current plan under the point, topmost first.
pub fn hits(ed: &Editor, p: V2, r: f64) -> Vec<i64> {
    let its = items(ed, AnnoView::Plan(ed.level));
    let ctx = ctx_of(&its);
    its.list
        .iter()
        .rev()
        .filter(|i| stroke_hit(&i.hit, (p.x, p.y), r, &ctx))
        .map(|i| i.anno.id)
        .collect()
}

pub fn plan_ids(ed: &Editor) -> Vec<i64> {
    let its = items(ed, AnnoView::Plan(ed.level));
    its.list.iter().map(|i| i.anno.id).collect()
}

/// Targets of edit tools: excludes other views' annotations; in a drafting view only its own
/// annotations (the plan behind it is hidden and untouched).
pub fn plan_targets(ed: &Editor) -> Vec<Sel> {
    let view = tool_view(ed);
    let drafting = in_drafting(ed);
    ed.selection
        .iter()
        .copied()
        .filter(|s| match s {
            Sel::Anno(id) => in_view(ed, view, *id),
            _ => !drafting,
        })
        .collect()
}

/// `s` belongs to the view the edit tools act on now (the rule of [`plan_targets`]).
pub fn in_tool_view(ed: &Editor, s: Sel) -> bool {
    match s {
        Sel::Anno(id) => in_view(ed, tool_view(ed), id),
        _ => !in_drafting(ed),
    }
}

/// One model operation over the annotations in `sel`; `None` if there are none. The step must be a
/// single operation: `steps_rec` counts undo by the number of steps.
pub fn tool_step(sel: &[Sel], xfs: Vec<AnnoXf>, copy: bool) -> Option<Step> {
    let ids: Vec<i64> = sel
        .iter()
        .filter_map(|s| match s {
            Sel::Anno(id) => Some(*id),
            _ => None,
        })
        .collect();
    if ids.is_empty() || xfs.is_empty() {
        return None;
    }
    Some(Box::new(move |m: &mut Model| {
        if copy {
            return m.copy_annos(&ids, &xfs).map(|_| ());
        }
        match xfs[0] {
            AnnoXf::Move(d) => m.move_annos(&ids, d),
            x => m.transform_annos(&ids, x),
        }
    }))
}

/// Radial array copies: same angles as `Model::array_radial`.
pub fn radial_xfs(o: V2, total: f64, n: usize) -> Vec<AnnoXf> {
    let full = (total.abs() - std::f64::consts::TAU).abs() < 1e-9;
    let parts = if full { n } else { n.saturating_sub(1) };
    let step = total / parts.max(1) as f64;
    (1..n)
        .map(|k| AnnoXf::Rotate {
            o: (o.x, o.y),
            a: step * k as f64,
        })
        .collect()
}

/// Linear array copies: `d` is the pitch or (`fit`) the distance to the last copy.
pub fn linear_xfs(d: V2, n: usize, fit: bool) -> Vec<AnnoXf> {
    let parts = if fit { n.saturating_sub(1).max(1) } else { 1 };
    (1..n)
        .map(|k| {
            let k = k as f64 / parts as f64;
            AnnoXf::Move((d.x * k, d.y * k))
        })
        .collect()
}

fn op(
    ed: &mut Editor,
    f: impl Fn(&mut Model) -> Result<(), ddc_model::Error> + Send + Sync + 'static,
) {
    if let Err(e) = ed.run_op(OpFn(Arc::new(f))) {
        ed.status = Some(cat().tf("annot_text.err", &[("e", &e.to_string())]));
    }
}

fn place(
    ed: &mut Editor,
    view: AnnoView,
    shape: AnnoShape,
    text_style: Option<i64>,
    line_style: Option<i64>,
) {
    op(ed, move |m| {
        m.add_anno(view, &shape, text_style, line_style).map(|_| ())
    });
}

// ---------- commands ----------

/// Arm a placement tool.
pub fn arm(ed: &mut Editor, kind: Kind, lang: Lang) {
    // arm through the session: the wall stays unarmed, the selection goes
    ed.set_mode(Mode::Select);
    ed.selection.clear();
    crate::interact::sessions::model_tool::stop(ed);
    let c = with(|s| {
        s.set_lang(lang);
        s.draft = Some(Draft {
            kind,
            pts: vec![],
            rings: vec![],
            cur: v2(0.0, 0.0),
            target: None,
        });
        s.catalog()
    });
    ed.status = Some(c.t(&format!("annot_text.hint.{}", kind.key())));
    crate::interact::Interaction::refresh(ed);
}

macro_rules! commands {
    ($($f:ident => $k:expr, $id:literal;)*) => {
        $(fn $f(app: &mut EditorApp) {
            let lang = app.sh.cat.lang;
            arm(&mut app.ed, $k, lang);
        })*
        const COMMANDS: &[(&str, fn(&mut EditorApp))] = &[$(($id, $f)),*];
    };
}

commands! {
    h_text => Kind::Text, "ann.text";
    h_leader => Kind::Leader, "ann.leader";
    h_detail_line => Kind::DetailLine, "ann.detail_line";
    h_region => Kind::Region, "ann.region";
    h_detail => Kind::Detail, "ann.detail";
    h_spot_elev => Kind::SpotElev, "ann.spot_elev";
    h_spot_coord => Kind::SpotCoord, "ann.spot_coord";
    h_cloud => Kind::Cloud, "ann.cloud";
}

pub(crate) fn h_legend(app: &mut EditorApp) {
    let lang = app.sh.cat.lang;
    with(|s| {
        s.set_lang(lang);
        s.legend.open = true;
    });
}

fn h_dim_chain(app: &mut EditorApp) {
    let lang = app.sh.cat.lang;
    let walls = selected_walls(&app.ed);
    if walls.is_empty() {
        arm(&mut app.ed, Kind::DimChain, lang);
    } else {
        with(|s| s.set_lang(lang));
        let level = app.ed.level;
        dim_chain(&mut app.ed, level, walls);
    }
}

/// Command registration in the shell (`shell::REGISTER`).
pub fn register(app: &mut EditorApp) {
    for (id, h) in COMMANDS {
        app.register_command(id, *h);
    }
    app.register_command("ann.legend", h_legend);
    app.register_command("ann.dim_chain", h_dim_chain);
}

fn selected_walls(ed: &Editor) -> Vec<i64> {
    ed.selection
        .iter()
        .filter_map(|s| match s {
            Sel::Wall(w) => Some(*w),
            _ => None,
        })
        .collect()
}

fn dim_chain(ed: &mut Editor, level: i64, walls: Vec<i64>) {
    let skipped = ed.model.chain_skipped(&walls).unwrap_or(0);
    if skipped == walls.len() {
        ed.status = Some(cat().t("annot_text.dim_no_straight"));
        return;
    }
    let rev = ed.revision;
    op(ed, move |m| {
        m.auto_dimension_chain(level, &walls).map(|_| ())
    });
    if ed.revision == rev {
        return;
    }
    let n = ed.model.dims(level).map_or(0, |d| d.len()).to_string();
    ed.status = Some(if skipped > 0 {
        cat().tf(
            "annot_text.dim_done_skipped",
            &[("n", &n), ("skipped", &skipped.to_string())],
        )
    } else {
        cat().tf("annot_text.dim_done", &[("n", &n)])
    });
}

// ---------- state for the shell ----------

/// Tool change: sketch and move are reset, typed text is committed; the selection stays.
pub fn cancel(ed: &mut Editor) {
    with(|s| {
        s.draft = None;
        s.drag = None;
    });
    commit_edit(ed);
}

/// A tool is armed or an edit is in progress.
pub fn busy() -> bool {
    with(|s| s.draft.is_some() || s.drag.is_some() || s.edit.is_some())
}

/// A placement tool is armed: clicks belong to it, not to underlay tracing.
pub fn armed() -> bool {
    with(|s| s.draft.is_some())
}

/// Command id of the armed placement tool, so the option bar names it instead of the wall tool.
pub fn armed_command() -> Option<&'static str> {
    with(|s| s.draft.as_ref().map(|d| d.kind)).map(|k| match k {
        Kind::Text => "ann.text",
        Kind::Leader | Kind::AddLeader => "ann.leader",
        Kind::DetailLine => "ann.detail_line",
        Kind::Region => "ann.region",
        Kind::Detail => "ann.detail",
        Kind::SpotElev => "ann.spot_elev",
        Kind::SpotCoord => "ann.spot_coord",
        Kind::Cloud => "ann.cloud",
        Kind::DimChain => "ann.dim_chain",
    })
}

/// An annotation move is in progress (the view must not pan with the same gesture).
pub fn dragging() -> bool {
    with(|s| s.drag.is_some())
}

/// The model changed (undo, snapshot replaced): move and edit are stale.
pub fn on_model_reset() {
    with(|s| {
        s.drag = None;
        s.edit = None;
        s.cache = None;
        s.lists = None;
    });
}

/// Undo during a sketch removes placed points; true if the undo was consumed.
pub fn undo_draft(ed: &mut Editor) -> bool {
    let done = with(|s| {
        s.drag = None;
        match &mut s.draft {
            Some(d) if !d.pts.is_empty() || !d.rings.is_empty() => {
                d.reset();
                Some(d.kind)
            }
            _ => None,
        }
    });
    match done {
        Some(k) => {
            ed.status = Some(cat().t(&format!("annot_text.hint.{}", k.key())));
            true
        }
        None => false,
    }
}

/// An annotation or a selected one's grip is under the cursor: a press starts a move, not a selection box.
pub fn grabs(ed: &Editor, p: V2, r: f64) -> bool {
    if ed.tool_armed() || ed.opening_mode {
        return false;
    }
    press_target(ed, AnnoView::Plan(ed.level), p, r, true).is_some()
}

/// Selected annotations of the shown view: Delete never reaches into another view.
pub(crate) fn deletable(ed: &Editor) -> Vec<i64> {
    let chosen = anno_ids(ed);
    if chosen.is_empty() {
        return chosen;
    }
    match current_view(ed) {
        Some(view) => chosen
            .into_iter()
            .filter(|id| in_view(ed, view, *id))
            .collect(),
        None => vec![],
    }
}

/// Section view Delete key: only annotations of the shown view go, others just leave the
/// selection; true when the key is consumed.
pub fn delete_selected(ed: &mut Editor) -> bool {
    if anno_ids(ed).is_empty() {
        return false;
    }
    let ids = deletable(ed);
    drop_annos(ed);
    if ids.is_empty() {
        return false;
    }
    op(ed, move |m| m.delete_annos(&ids));
    true
}

// ---------- events ----------

#[derive(Clone, Copy)]
struct Vw {
    view: AnnoView,
    /// The wall tool's snaps exist only on the plan.
    snap: bool,
    r: f64,
}

/// Plan event: the active annotation tool, or selection and grips; false if not ours.
pub fn handle(ed: &mut Editor, e: Event) -> bool {
    let vw = Vw {
        view: AnnoView::Plan(ed.level),
        snap: true,
        r: ed.tool.radius,
    };
    dispatch(ed, e, vw)
}

fn dispatch(ed: &mut Editor, e: Event, vw: Vw) -> bool {
    if with(|s| s.edit.is_some()) {
        return match e {
            Event::Click(_) => {
                commit_edit(ed);
                true
            }
            Event::Key(Key::Escape) => {
                with(|s| s.edit = None);
                true
            }
            Event::Move(_) => false,
            _ => true,
        };
    }
    if let Some(mut d) = with(|s| s.draft.take()) {
        // another tool took over the view: sketch reset
        if ed.model_tool().is_some() || ed.opening_mode {
            return false;
        }
        if step(ed, &mut d, e, vw) {
            with(|s| s.draft = Some(d));
        } else {
            ed.set_mode(Mode::Select);
            ed.status = None;
        }
        return true;
    }
    if ed.tool_armed() || ed.opening_mode {
        return false;
    }
    select_event(ed, e, vw)
}

fn track(ed: &mut Editor, p: V2, vw: Vw) -> V2 {
    if vw.snap {
        // a text's anchor is a one-point chain of its own, not the wall draft
        let z = ed.tool.snap_state.z;
        ed.tool.track_chain(p, crate::wall_tool::Chain::of(&[], z, true));
        ed.wall_snapped()
    } else {
        p
    }
}

fn step(ed: &mut Editor, d: &mut Draft, e: Event, vw: Vw) -> bool {
    match e {
        Event::Move(p) => d.cur = track(ed, p, vw),
        Event::Click(p) => {
            d.cur = track(ed, p, vw);
            return click(ed, d, vw);
        }
        Event::Press(_) | Event::Release(_) | Event::Char(_) | Event::Key(Key::Arc) => {}
        Event::Key(Key::Tab) => {
            if vw.snap && !ed.tool.snaps.is_empty() {
                ed.tool.snap_idx = (ed.tool.snap_idx + 1) % ed.tool.snaps.len();
                d.cur = ed.wall_snapped();
            }
        }
        Event::Key(Key::Backspace) => {
            d.pts.pop();
        }
        Event::Key(Key::Escape) => {
            if d.pts.is_empty() && d.rings.is_empty() {
                return false;
            }
            d.reset();
        }
        Event::Key(Key::Enter) => finish(ed, d, vw, false),
        Event::Key(Key::Close) => finish(ed, d, vw, true),
    }
    true
}

fn ring_of(pts: &[V2]) -> Vec<P> {
    pts.iter().map(|q| (q.x, q.y)).collect()
}

/// Tool click; false once the tool has finished.
fn click(ed: &mut Editor, d: &mut Draft, vw: Vw) -> bool {
    let p = (d.cur.x, d.cur.y);
    let o = with(|s| s.opts.clone());
    match d.kind {
        Kind::Text => {
            let mut n = TextNote::new(p, "");
            n.align = o.align;
            n.head = o.head;
            n.wrap_mm = o.wrap.then_some(o.wrap_mm);
            begin_edit(None, vw.view, n, o.text_style, o.line_style);
        }
        Kind::Leader => {
            d.pts.push(d.cur);
            if d.pts.len() >= if o.elbow { 3 } else { 2 } {
                let mut n = TextNote::new(p, "");
                n.align = o.align;
                n.head = o.head;
                n.wrap_mm = o.wrap.then_some(o.wrap_mm);
                n.leaders.push(Leader {
                    to: (d.pts[0].x, d.pts[0].y),
                    elbow: o.elbow.then_some((d.pts[1].x, d.pts[1].y)),
                });
                begin_edit(None, vw.view, n, o.text_style, o.line_style);
                d.pts.clear();
            }
        }
        Kind::DetailLine | Kind::Region | Kind::Cloud => d.pts.push(d.cur),
        Kind::Detail => click_detail(ed, d, vw, &o),
        Kind::SpotElev | Kind::SpotCoord => {
            d.pts.push(d.cur);
            if d.pts.len() == 2 {
                let kind = if d.kind == Kind::SpotElev {
                    SpotKind::Elevation
                } else {
                    SpotKind::Coordinate
                };
                let shape = AnnoShape::Spot {
                    kind,
                    at: (d.pts[0].x, d.pts[0].y),
                    head: p,
                };
                place(ed, vw.view, shape, o.text_style, o.line_style);
                d.pts.clear();
            }
        }
        Kind::DimChain => {
            let AnnoView::Plan(level) = vw.view else {
                ed.status = Some(cat().t("annot_text.dim_plan_only"));
                return true;
            };
            match wall_at(ed, d.cur, vw.r) {
                Some(w) => dim_chain(ed, level, vec![w]),
                None => ed.status = Some(cat().t("annot_text.hint.dim_chain")),
            }
        }
        Kind::AddLeader => {
            let Some(id) = d.target else { return false };
            if let Ok(a) = ed.model.anno(id) {
                if let AnnoShape::Text(mut t) = a.shape {
                    t.leaders.push(Leader { to: p, elbow: None });
                    let shape = AnnoShape::Text(t);
                    op(ed, move |m| m.update_anno(id, &shape));
                }
            }
            return false;
        }
    }
    true
}

/// A fixed-size element takes one click; a stretchable or repeatable one takes a start and an end.
fn click_detail(ed: &mut Editor, d: &mut Draft, vw: Vw, o: &Opts) {
    let defs = ed.model.detail_defs().unwrap_or_default();
    let Some(def) = chosen_def(&defs, o.def) else {
        return;
    };
    let id = def.id;
    let shape = if def.fit == DetailFit::Fixed {
        AnnoShape::Detail {
            def: id,
            at: (d.cur.x, d.cur.y),
            rot_deg: o.rot_deg,
            length: None,
            flip: o.flip,
        }
    } else {
        let Some(a) = d.pts.first().copied() else {
            d.pts.push(d.cur);
            return;
        };
        let along = d.cur - a;
        if along.len() < 1e-6 {
            return;
        }
        AnnoShape::Detail {
            def: id,
            at: (a.x, a.y),
            rot_deg: along.y.atan2(along.x).to_degrees(),
            length: Some(along.len()),
            flip: o.flip,
        }
    };
    d.pts.clear();
    place(ed, vw.view, shape, o.text_style, o.line_style);
}

fn chosen_def(defs: &[DetailDef], want: Option<i64>) -> Option<&DetailDef> {
    want.and_then(|i| defs.iter().find(|x| x.id == i))
        .or(defs.first())
}

fn wall_at(ed: &Editor, p: V2, r: f64) -> Option<i64> {
    crate::selection::candidates(ed, p, r, |_| true)
        .into_iter()
        .flatten()
        .find_map(|s| match s {
            Sel::Wall(w) => Some(w),
            _ => None,
        })
}

fn finish(ed: &mut Editor, d: &mut Draft, vw: Vw, close: bool) {
    let o = with(|s| s.opts.clone());
    let pts = ring_of(&d.pts);
    let shape = match d.kind {
        Kind::DetailLine if pts.len() >= 2 + usize::from(close) => {
            AnnoShape::Line { pts, closed: close }
        }
        Kind::Region if pts.len() >= 3 || (pts.is_empty() && !d.rings.is_empty()) => {
            let mut rings = d.rings.clone();
            if !pts.is_empty() {
                rings.push(pts);
            }
            AnnoShape::Region {
                rings,
                fill: o.region_fill(),
                boundary: o.boundary,
            }
        }
        Kind::Cloud if pts.len() >= 3 => AnnoShape::Cloud {
            ring: pts,
            mark: o.mark.clone(),
        },
        _ => return,
    };
    d.reset();
    place(ed, vw.view, shape, o.text_style, o.line_style);
}

fn begin_edit(
    id: Option<i64>,
    view: AnnoView,
    note: TextNote,
    text_style: Option<i64>,
    line_style: Option<i64>,
) {
    let buf = note.text.clone();
    with(|s| {
        s.edit = Some(TextEditing {
            id,
            view,
            note,
            text_style,
            line_style,
            buf,
            focus: true,
        })
    });
}

/// An empty new text is not placed; an empty existing one is deleted.
fn commit_edit(ed: &mut Editor) {
    let Some(e) = with(|s| s.edit.take()) else {
        return;
    };
    let text = e.buf.trim_end_matches('\n').to_string();
    let empty = text.trim().is_empty();
    let mut note = e.note;
    note.text = text;
    let shape = AnnoShape::Text(note);
    let (view, ts, ls) = (e.view, e.text_style, e.line_style);
    match (e.id, empty) {
        (None, true) => {}
        (None, false) => place(ed, view, shape, ts, ls),
        (Some(id), true) => {
            ed.selection.retain(|s| *s != Sel::Anno(id));
            op(ed, move |m| m.delete_annos(&[id]));
        }
        (Some(id), false) => op(ed, move |m| m.update_anno(id, &shape)),
    }
}

fn edit_existing(ed: &Editor, id: i64) {
    let Ok(a) = ed.model.anno(id) else { return };
    if let AnnoShape::Text(n) = a.shape {
        begin_edit(Some(id), a.view, n, a.text_style, a.line_style);
    }
}

fn grip_at(ed: &Editor, view: AnnoView, p: V2, r: f64) -> Option<(i64, usize, AnnoShape)> {
    let id = single(ed)?;
    let its = items(ed, view);
    let shape = &its.list.iter().find(|i| i.anno.id == id)?.anno.shape;
    let i = shape
        .grips()
        .iter()
        .position(|g| ((g.0 - p.x).powi(2) + (g.1 - p.y).powi(2)).sqrt() <= r)?;
    Some((id, i, shape.clone()))
}

/// What starts a move under the cursor: a grip, a selected annotation, or an annotation over empty
/// space. An unselected one over an element does not steal its move (`plan`: elements exist).
fn press_target(
    ed: &Editor,
    view: AnnoView,
    p: V2,
    r: f64,
    plan: bool,
) -> Option<(i64, Option<usize>, AnnoShape)> {
    if let Some((id, i, shape)) = grip_at(ed, view, p, r) {
        return Some((id, Some(i), shape));
    }
    let id = hit(ed, view, p, r)?;
    if plan && !ed.selection.contains(&Sel::Anno(id)) {
        // rooms and slabs enclose the point and have no move of their own
        let over = crate::selection::candidates(ed, p, r, |s| {
            matches!(
                s,
                Sel::Wall(_)
                    | Sel::Opening(_)
                    | Sel::Curtain(_)
                    | Sel::Panel(..)
                    | Sel::Mep(_)
                    | Sel::Comp(_)
            )
        });
        if !over.is_empty() {
            return None;
        }
    }
    let its = items(ed, view);
    let it = its.list.iter().find(|i| i.anno.id == id)?;
    Some((id, None, it.anno.shape.clone()))
}

fn select_event(ed: &mut Editor, e: Event, vw: Vw) -> bool {
    let view = vw.view;
    match e {
        Event::Press(p) => {
            let Some((id, grip, shape)) = press_target(ed, view, p, vw.r, vw.snap) else {
                return false;
            };
            ed.selection = vec![Sel::Anno(id)];
            with(|s| {
                s.drag = Some(Drag {
                    id,
                    grip,
                    shape,
                    from: p,
                    cur: p,
                    view,
                });
            });
            true
        }
        Event::Move(p) => with(|s| match &mut s.drag {
            Some(d) => {
                d.cur = p;
                true
            }
            None => false,
        }),
        Event::Release(p) => {
            let Some(d) = with(|s| s.drag.take()) else {
                return false;
            };
            if (p - d.from).len() > 1e-9 {
                let (id, shape) = (d.id, d.shape_at(p));
                op(ed, move |m| m.update_anno(id, &shape));
            }
            true
        }
        Event::Click(p) => {
            let Some(id) = hit(ed, view, p, vw.r) else {
                // on the plan `pick` replaces the selection; in a section nothing else would clear it
                if !vw.snap {
                    drop_annos(ed);
                }
                return false;
            };
            let was = single(ed);
            ed.selection = vec![Sel::Anno(id)];
            // a second click on the selected text edits it in place
            if was == Some(id) {
                edit_existing(ed, id);
            }
            true
        }
        Event::Key(Key::Escape) => {
            let had = !anno_ids(ed).is_empty();
            drop_annos(ed);
            had && ed.selection.is_empty()
        }
        Event::Key(Key::Enter) => match single(ed).filter(|id| in_view(ed, view, *id)) {
            Some(id) => {
                edit_existing(ed, id);
                with(|s| s.edit.is_some())
            }
            None => false,
        },
        _ => false,
    }
}

/// Input for a section/elevation view; true if the click belongs to annotations.
pub fn view_input(
    ui: &egui::Ui,
    resp: &egui::Response,
    ed: &mut Editor,
    view: AnnoView,
    to_w: &dyn Fn(Pos2) -> V2,
) -> bool {
    let at = resp.hover_pos().or(resp.interact_pointer_pos());
    let r = at.map_or(0.0, |p| (to_w(p + Vec2::new(HIT_PX, 0.0)) - to_w(p)).len());
    let vw = Vw {
        view,
        snap: false,
        r,
    };
    if let Some(h) = resp.hover_pos() {
        if ui.input(|i| i.pointer.delta() != Vec2::ZERO) {
            dispatch(ed, Event::Move(to_w(h)), vw);
        }
    }
    if let Some(p) = at {
        if resp.drag_started_by(egui::PointerButton::Primary) {
            dispatch(ed, Event::Press(to_w(p)), vw);
        }
        if resp.drag_stopped_by(egui::PointerButton::Primary) {
            dispatch(ed, Event::Release(to_w(p)), vw);
        }
    }
    let mut used = false;
    if resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            used = dispatch(ed, Event::Click(to_w(p)), vw);
        }
    }
    if !ui.ctx().wants_keyboard_input() {
        let pressed = |k: egui::Key| ui.input(|i| i.key_pressed(k));
        for (key, ev) in [
            (egui::Key::Escape, Key::Escape),
            (egui::Key::Enter, Key::Enter),
            (egui::Key::Backspace, Key::Backspace),
            (egui::Key::C, Key::Close),
        ] {
            if pressed(key) {
                dispatch(ed, Event::Key(ev), vw);
            }
        }
        if pressed(egui::Key::Delete) {
            delete_selected(ed);
        }
    }
    used
}

// ---------- drawing ----------

fn p2(p: P) -> V2 {
    v2(p.0, p.1)
}

fn shade(col: Color32, paper: Color32, gray: u8) -> Color32 {
    let k = f32::from(gray) / 255.0;
    let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * k).round() as u8;
    Color32::from_rgb(
        mix(col.r(), paper.r()),
        mix(col.g(), paper.g()),
        mix(col.b(), paper.b()),
    )
}

struct Paint<'a> {
    p: &'a egui::Painter,
    to_s: &'a dyn Fn(V2) -> Pos2,
    t: &'a Tokens,
    /// Pixels per sheet mm at the view scale.
    mm_px: f64,
    /// View meters per sheet mm.
    k: f64,
}

/// Font size step is a quarter octave: zooming does not fill the atlas with glyphs of every size.
fn font_step(px: f32) -> f32 {
    2f32.powf((px.log2() * 4.0).round() / 4.0).min(TEXT_MAX_PX)
}

impl Paint<'_> {
    fn strokes(&self, list: &[AnnoStroke], cells: &[Vec<[P; 4]>], col: Color32) {
        let mut cells = cells.iter();
        for st in list {
            match st {
                AnnoStroke::Line {
                    pts,
                    closed,
                    weight_mm,
                    dash_mm,
                } => self.line(pts, *closed, *weight_mm, dash_mm, col),
                AnnoStroke::Fill { gray, mask, .. } => {
                    let fill = if *mask {
                        self.t.paper
                    } else {
                        shade(col, self.t.paper, *gray)
                    };
                    if let Some(c) = cells.next() {
                        self.fill(c, fill);
                    }
                }
                AnnoStroke::Text {
                    pos,
                    h_mm,
                    rot_deg,
                    s,
                } => self.text(*pos, *h_mm, *rot_deg, s, col),
            }
        }
    }

    fn fill(&self, cells: &[[P; 4]], col: Color32) {
        let mut mesh = egui::Mesh::default();
        for c in cells {
            let i = mesh.vertices.len() as u32;
            for q in c {
                mesh.colored_vertex((self.to_s)(p2(*q)), col);
            }
            mesh.add_triangle(i, i + 1, i + 2);
            mesh.add_triangle(i, i + 2, i + 3);
        }
        if !mesh.is_empty() {
            self.p.add(mesh);
        }
    }

    fn line(&self, pts: &[P], closed: bool, weight: f64, dash: &[f64], col: Color32) {
        let width = ((weight * self.mm_px) as f32).clamp(self.t.lw[3], self.t.lw[0] * 2.0);
        let stroke = Stroke::new(width, col);
        let solid = dash.iter().sum::<f64>() * self.mm_px < DASH_MIN_PX;
        let n = pts.len();
        let m = if closed { n } else { n.saturating_sub(1) };
        let scaled: Vec<f64> = dash.iter().map(|d| d * self.k).collect();
        for i in 0..m {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            if solid {
                self.seg(a, b, stroke);
            } else {
                for (q, r) in dash_segments(a, b, &scaled) {
                    self.seg(q, r, stroke);
                }
            }
        }
    }

    fn seg(&self, a: P, b: P, stroke: Stroke) {
        self.p
            .line_segment([(self.to_s)(p2(a)), (self.to_s)(p2(b))], stroke);
    }

    fn text(&self, pos: P, h_mm: f64, rot_deg: f64, s: &str, col: Color32) {
        let px = (h_mm * self.mm_px) as f32;
        if px < TEXT_MIN_PX {
            return;
        }
        let (sn, cs) = rot_deg.to_radians().sin_cos();
        // pos is the bottom of the first letter; a text shape's anchor point is its top
        let h = h_mm * self.k;
        let top = (self.to_s)(v2(pos.0 - sn * h, pos.1 + cs * h));
        let base = font_step(px);
        let font = egui::FontId::new(base, ddc_ui::theme::family(ddc_ui::theme::W_REGULAR));
        let g = self.p.layout_no_wrap(s.to_string(), font, col);
        let mut shape =
            egui::epaint::TextShape::new(top, g, col).with_angle(-rot_deg.to_radians() as f32);
        // the remainder to the exact size is applied as mesh scale: the label matches its frame and leader
        let f = px / base;
        if (f - 1.0).abs() > 1e-3 {
            shape.transform(egui::emath::TSTransform::from_scaling(f));
            shape.pos = top;
        }
        self.p.add(shape);
    }
}

/// View annotations, the tool sketch, the selected one's grips, the text edit field and the parameters panel.
pub fn draw(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    rect: Rect,
    ed: &mut Editor,
    t: &Tokens,
    to_s: &dyn Fn(V2) -> Pos2,
    view: AnnoView,
) {
    let its = items(ed, view);
    let ppm = f64::from((to_s(v2(1.0, 0.0)) - to_s(v2(0.0, 0.0))).length());
    let k = its.scale / 1000.0;
    let pt = Paint {
        p: painter,
        to_s,
        t,
        mm_px: k * ppm,
        k,
    };
    let selecting = !ed.tool_armed();
    let chosen: HashSet<i64> = if selecting {
        anno_ids(ed).into_iter().collect()
    } else {
        HashSet::new()
    };
    let (editing, moving) = with(|s| {
        (
            s.edit.as_ref().and_then(|e| e.id),
            s.drag.as_ref().filter(|d| d.view == view).map(|d| {
                let mut a = its
                    .list
                    .iter()
                    .find(|i| i.anno.id == d.id)
                    .map(|i| i.anno.clone());
                if let Some(x) = &mut a {
                    x.shape = d.shape_at(d.cur);
                }
                (d.id, a)
            }),
        )
    });
    let moving_id = moving.as_ref().map(|m| m.0);
    let on_screen = |b: [f64; 4]| {
        let corners = [
            to_s(v2(b[0], b[1])),
            to_s(v2(b[2], b[1])),
            to_s(v2(b[2], b[3])),
            to_s(v2(b[0], b[3])),
        ];
        Rect::from_points(&corners).intersects(rect.expand(t.spacing))
    };
    for it in &its.list {
        if Some(it.anno.id) == editing || Some(it.anno.id) == moving_id {
            continue;
        }
        if it.bbox.is_some_and(|b| !on_screen(b)) {
            continue;
        }
        let col = if chosen.contains(&it.anno.id) {
            t.accent
        } else {
            t.cut
        };
        let skip = if it.hatch.1 * pt.mm_px < HATCH_MIN_PX {
            it.hatch.0
        } else {
            0
        };
        pt.strokes(&it.strokes[skip..], &it.fills, col);
    }
    if let Some((_, Some(a))) = &moving {
        let strokes = a.strokes(&ctx_of(&its));
        pt.strokes(&strokes, &fill_cells(&strokes), t.accent);
    }
    let one = single(ed).filter(|_| selecting);
    if let Some(it) = one.and_then(|id| its.list.iter().find(|i| i.anno.id == id)) {
        for g in it.anno.shape.grips() {
            let c = to_s(p2(g));
            painter.rect_filled(
                Rect::from_center_size(c, Vec2::splat(t.spacing)),
                0.0,
                t.accent,
            );
        }
    }
    paint_draft(painter, to_s, t);
    edit_overlay(ui, ed, to_s, t, pt.mm_px, view);
    options_bar(ui, rect, ed, t, view);
}

fn paint_draft(painter: &egui::Painter, to_s: &dyn Fn(V2) -> Pos2, t: &Tokens) {
    let (pts, rings, cur) = with(|s| match &s.draft {
        Some(d) => (d.pts.clone(), d.rings.clone(), Some(d.cur)),
        None => (vec![], vec![], None),
    });
    let stroke = Stroke::new(t.lw[1], t.accent);
    for r in &rings {
        for i in 0..r.len() {
            let (a, b) = (r[i], r[(i + 1) % r.len()]);
            painter.line_segment([to_s(p2(a)), to_s(p2(b))], stroke);
        }
    }
    for w in pts.windows(2) {
        painter.line_segment([to_s(w[0]), to_s(w[1])], stroke);
    }
    if let (Some(a), Some(c)) = (pts.last(), cur) {
        painter.line_segment([to_s(*a), to_s(c)], stroke);
    }
}

enum Done {
    Commit,
    Cancel,
}

fn edit_overlay(
    ui: &mut egui::Ui,
    ed: &mut Editor,
    to_s: &dyn Fn(V2) -> Pos2,
    t: &Tokens,
    mm_px: f64,
    view: AnnoView,
) {
    // edit coordinates belong to its view: another view shows no field, and the text is not lost
    if with(|s| s.edit.as_ref().is_some_and(|e| e.view != view)) {
        commit_edit(ed);
        return;
    }
    let Some(mut e) = with(|s| s.edit.take()) else {
        return;
    };
    let pos = to_s(p2(e.note.pos));
    let styles = ed.model.text_styles().unwrap_or_default();
    let height = styles
        .iter()
        .find(|s| Some(s.id) == e.text_style)
        .or(styles.first())
        .map_or(0.0, |s| s.height_mm);
    let px = ((height * mm_px) as f32).clamp(t.spacing * 1.5, t.spacing * 5.0);
    let font = egui::FontId::new(px, ddc_ui::theme::family(ddc_ui::theme::W_REGULAR));
    let width = t.spacing * 32.0;
    let mut done = None;
    let ctx = ui.ctx().clone();
    egui::Area::new(egui::Id::new("f4_text_edit"))
        .fixed_pos(pos)
        .order(ddc_ui::z::Z::Canvas.order())
        .show(&ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                let r = ui.add(
                    egui::TextEdit::multiline(&mut e.buf)
                        .font(font)
                        .desired_rows(3)
                        .desired_width(width),
                );
                if e.focus {
                    r.request_focus();
                    e.focus = false;
                }
                ui.input(|i| {
                    if i.key_pressed(egui::Key::Escape) {
                        done = Some(Done::Cancel);
                    } else if i.key_pressed(egui::Key::Enter) && i.modifiers.command {
                        done = Some(Done::Commit);
                    }
                });
            });
        });
    match done {
        None => with(|s| s.edit = Some(e)),
        Some(Done::Cancel) => {}
        Some(Done::Commit) => {
            with(|s| s.edit = Some(e));
            commit_edit(ed);
        }
    }
}

// ---------- parameters panel ----------

enum Act {
    Styles(i64, Option<i64>, Option<i64>),
    Shape(i64, AnnoShape),
    Scale(f64),
    NextRing,
    AddLeader(i64),
    Edit(i64),
}

fn combo<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    salt: &str,
    cur: &mut T,
    items: &[(T, String)],
) -> bool {
    let th = ddc_ui::theme::of(ui.ctx());
    let names: Vec<&str> = items.iter().map(|x| x.1.as_str()).collect();
    let at = items.iter().position(|x| x.0 == *cur);
    let mut sel = at.unwrap_or(usize::MAX);
    let closed = at.is_none().then_some("");
    let r = ddc_ui::widgets::field::dropdown_shown(
        ui, &th, salt, &mut sel, &names, None, None, None, closed,
    );
    match items.get(sel) {
        Some((v, _)) if r.changed() && *cur != *v => {
            *cur = *v;
            true
        }
        _ => false,
    }
}

/// A built-in style's name comes from the catalog; a custom one is shown as is.
fn name_of(cat: &Catalog, prefix: &str, name: &str) -> String {
    let key = format!("{prefix}.{name}");
    let v = cat.t(&key);
    if v == key {
        name.to_string()
    } else {
        v
    }
}

struct Lists {
    text: Vec<(Option<i64>, String)>,
    line: Vec<(Option<i64>, String)>,
    defs: Vec<(Option<i64>, String)>,
    align: Vec<(TextAlign, String)>,
    head: Vec<(LeaderHead, String)>,
    fill: Vec<(u8, String)>,
}

fn lists(ed: &Editor, c: &Catalog) -> Lists {
    let dflt = || (None, c.t("annot_text.style.default"));
    let mut text = vec![dflt()];
    text.extend(
        ed.model
            .text_styles()
            .unwrap_or_default()
            .into_iter()
            .map(|s| (Some(s.id), name_of(c, "annot_text.tstyle", &s.name))),
    );
    let mut line = vec![dflt()];
    line.extend(
        ed.model
            .line_styles()
            .unwrap_or_default()
            .into_iter()
            .map(|s| (Some(s.id), name_of(c, "annot_text.lstyle", &s.name))),
    );
    let defs = ed
        .model
        .detail_defs()
        .unwrap_or_default()
        .into_iter()
        .map(|d| (Some(d.id), name_of(c, "annot_text.detail", &d.name)))
        .collect();
    Lists {
        text,
        line,
        defs,
        align: vec![
            (TextAlign::Left, c.t("annot_text.align.left")),
            (TextAlign::Center, c.t("annot_text.align.center")),
            (TextAlign::Right, c.t("annot_text.align.right")),
        ],
        head: vec![
            (LeaderHead::Arrow, c.t("annot_text.head.arrow")),
            (LeaderHead::Dot, c.t("annot_text.head.dot")),
            (LeaderHead::Tick, c.t("annot_text.head.tick")),
            (LeaderHead::None, c.t("annot_text.head.none")),
        ],
        fill: vec![
            (0, c.t("annot_text.fill.solid")),
            (1, c.t("annot_text.fill.mask")),
            (2, c.t("annot_text.fill.hatch")),
        ],
    }
}

fn cached_lists(ed: &Editor, c: &Catalog) -> Rc<Lists> {
    let key = ed.cache_key();
    let hit = with(|s| {
        s.lists
            .as_ref()
            .filter(|x| x.0 == key && x.1 == c.lang)
            .map(|x| x.2.clone())
    });
    if let Some(l) = hit {
        return l;
    }
    let l = Rc::new(lists(ed, c));
    with(|s| s.lists = Some((key, c.lang, l.clone())));
    l
}

fn num(ui: &mut egui::Ui, label: &str, v: &mut f64, unit: &str, lo: f64, hi: f64) -> bool {
    ui.label(label);
    ui.add(
        egui::DragValue::new(v)
            .speed(0.1)
            .range(lo..=hi)
            .suffix(format!(" {unit}")),
    )
    .changed()
}

// ---------- parameter panels: shared widgets of the set ----------

const DD_W: f32 = 120.0;
const FIELD_W: f32 = 72.0;

/// Alignment control: segments left to right, with the catalog key of each label.
fn align_items() -> [(TextAlign, &'static str); 3] {
    [
        (TextAlign::Left, "annot_text.align.left"),
        (TextAlign::Center, "annot_text.align.center"),
        (TextAlign::Right, "annot_text.align.right"),
    ]
}

/// Gray level of a fill as a percent, one decimal: 256 levels fit in 1000 steps, so the
/// stored byte comes back exactly through `pct_gray`.
fn gray_pct(g: u8) -> f64 {
    (f64::from(g) * 1000.0 / 255.0).round() / 10.0
}

/// Stored byte of a percent; out-of-range and non-finite input is clamped to 0..=100 %.
fn pct_gray(p: f64) -> u8 {
    if !p.is_finite() {
        return 0;
    }
    (p.clamp(0.0, 100.0) * 255.0 / 100.0).round() as u8
}

fn gray_pct_valid(p: f64) -> bool {
    p.is_finite() && (0.0..=100.0).contains(&p)
}

fn bar_caption(ui: &mut egui::Ui, th: &Theme, text: &str) -> egui::Response {
    widgets::elided_label(ui, text, th.font(Role::Label), th.sem().text_secondary)
}

/// Dropdown with a visible caption and an accessible name; one change point for the draft
/// and the selection bar.
fn labelled_combo<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    th: &Theme,
    c: &Catalog,
    label_key: &str,
    salt: &str,
    cur: &mut T,
    items: &[(T, String)],
) -> bool {
    let label = c.t(label_key);
    bar_caption(ui, th, &label);
    let names: Vec<&str> = items.iter().map(|x| x.1.as_str()).collect();
    let mut sel = items.iter().position(|x| x.0 == *cur).unwrap_or(0);
    let r = widgets::dropdown(ui, th, salt, &mut sel, &names, Some(DD_W));
    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, true, &label));
    match items.get(sel) {
        Some(it) if r.changed() && it.0 != *cur => {
            *cur = it.0;
            true
        }
        _ => false,
    }
}

/// Three-segment alignment control; true when the choice moved.
fn align_row(ui: &mut egui::Ui, th: &Theme, c: &Catalog, cur: &mut TextAlign) -> bool {
    bar_caption(ui, th, &c.t("text_style.lbl.align"));
    let items = align_items();
    let names: Vec<String> = items.iter().map(|x| c.t(x.1)).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let sel = items.iter().position(|x| x.0 == *cur).unwrap_or(0);
    match widgets::segmented(ui, th, &names, sel) {
        Some(i) if items[i].0 != *cur => {
            *cur = items[i].0;
            true
        }
        _ => false,
    }
}

/// Number field with a visible caption, muted unit and locale decimals; true when changed.
#[allow(clippy::too_many_arguments)]
fn num_field(
    ui: &mut egui::Ui,
    th: &Theme,
    c: &Catalog,
    label_key: &str,
    salt: &str,
    v: &mut f64,
    unit: &str,
    decimals: usize,
    (lo, hi): (f64, f64),
) -> bool {
    let label = c.t(label_key);
    let err = c
        .t("text_style.bad_number")
        .replace("{min}", &lo.to_string())
        .replace("{max}", &hi.to_string());
    let lr = bar_caption(ui, th, &label);
    let before = *v;
    widgets::NumberField::new(salt, v, crate::modify_opts::number_format(c.lang == Lang::Ru))
        .labelled_by(lr.id, &label)
        .error_text(&err)
        .decimals(decimals)
        .step(if decimals == 0 { 1.0 } else { 0.1 })
        .range(lo, hi)
        .unit(unit)
        .width(FIELD_W)
        .show(ui, th);
    *v != before
}

fn gray_swatch(ui: &mut egui::Ui, th: &Theme, gray: u8) {
    let side = th.d().icon;
    let (r, _) = ui.allocate_exact_size(Vec2::splat(side), egui::Sense::hover());
    let rad = th.radius("md");
    ui.painter().rect_filled(r, rad, Color32::from_gray(gray));
    ui.painter().rect_stroke(
        r,
        rad,
        Stroke::new(1.0, th.c("border.subtle")),
        egui::StrokeKind::Inside,
    );
}

fn bar_check(ui: &mut egui::Ui, th: &Theme, c: &Catalog, key: &str, v: &mut bool) -> bool {
    widgets::checkbox(ui, th, v, &c.t(key)).changed()
}

fn bar_button(ui: &mut egui::Ui, th: &Theme, c: &Catalog, key: &str, tip_key: &str) -> bool {
    let (label, tip) = (c.t(key), c.t(tip_key));
    widgets::Button::secondary(&label)
        .tooltip(&tip, "")
        .show(ui, th)
        .clicked()
}

fn draft_opts(
    ui: &mut egui::Ui,
    th: &Theme,
    c: &Catalog,
    kind: Kind,
    o: &mut Opts,
    l: &Lists,
    act: &mut Option<Act>,
) {
    let mm = c.t("annot_text.unit.mm");
    match kind {
        Kind::Text | Kind::Leader => {
            labelled_combo(ui, th, c, "text_style.lbl.text_style", "f4_ts", &mut o.text_style, &l.text);
            align_row(ui, th, c, &mut o.align);
            bar_check(ui, th, c, "annot_text.opt.wrap", &mut o.wrap);
            if o.wrap {
                let r = (5.0, 500.0);
                num_field(ui, th, c, "text_style.lbl.width", "f4_wrap", &mut o.wrap_mm, &mm, 0, r);
            }
            if kind == Kind::Leader {
                labelled_combo(ui, th, c, "text_style.lbl.head", "f4_hd", &mut o.head, &l.head);
                bar_check(ui, th, c, "annot_text.opt.elbow", &mut o.elbow);
            }
        }
        Kind::DetailLine => {
            labelled_combo(ui, th, c, "text_style.lbl.line_style", "f4_ls", &mut o.line_style, &l.line);
        }
        Kind::Region => {
            labelled_combo(ui, th, c, "text_style.lbl.fill", "f4_fl", &mut o.fill, &l.fill);
            match o.fill {
                0 => {
                    let mut p = gray_pct(o.gray);
                    let pct = c.t("text_style.unit.pct");
                    if num_field(ui, th, c, "annot_text.opt.gray", "f4_gray", &mut p, &pct, 0, (0.0, 100.0))
                        && gray_pct_valid(p)
                    {
                        o.gray = pct_gray(p);
                    }
                    gray_swatch(ui, th, o.gray);
                }
                1 => {}
                _ => {
                    let deg = c.t("text_style.unit.deg");
                    let a = (-180.0, 180.0);
                    num_field(ui, th, c, "annot_text.opt.angle", "f4_ang", &mut o.deg, &deg, 1, a);
                    let s = (0.2, 50.0);
                    num_field(ui, th, c, "annot_text.opt.step", "f4_step", &mut o.step_mm, &mm, 1, s);
                    bar_check(ui, th, c, "annot_text.opt.cross", &mut o.cross);
                }
            }
            bar_check(ui, th, c, "annot_text.opt.boundary", &mut o.boundary);
            if o.boundary {
                labelled_combo(
                    ui, th, c, "text_style.lbl.line_style", "f4_ls", &mut o.line_style, &l.line,
                );
            }
            if bar_button(ui, th, c, "annot_text.opt.next_ring", "text_style.tip.next_ring") {
                *act = Some(Act::NextRing);
            }
        }
        Kind::Detail => {
            if o.def.is_none() {
                o.def = l.defs.first().and_then(|x| x.0);
            }
            labelled_combo(ui, th, c, "text_style.lbl.detail", "f4_df", &mut o.def, &l.defs);
            let deg = c.t("text_style.unit.deg");
            num_field(ui, th, c, "annot_text.opt.angle", "f4_rot", &mut o.rot_deg, &deg, 1, (-360.0, 360.0));
            bar_check(ui, th, c, "annot_text.opt.flip", &mut o.flip);
            labelled_combo(ui, th, c, "text_style.lbl.line_style", "f4_ls", &mut o.line_style, &l.line);
        }
        Kind::SpotElev | Kind::SpotCoord => {
            labelled_combo(ui, th, c, "text_style.lbl.text_style", "f4_ts", &mut o.text_style, &l.text);
        }
        Kind::Cloud => {
            let label = c.t("annot_text.opt.mark");
            let lr = bar_caption(ui, th, &label);
            widgets::TextField::new(&mut o.mark)
                .id_salt("f4_mark")
                .labelled_by(lr.id, &label)
                .width(FIELD_W)
                .show(ui, th);
            labelled_combo(ui, th, c, "text_style.lbl.line_style", "f4_ls", &mut o.line_style, &l.line);
        }
        Kind::DimChain | Kind::AddLeader => {}
    }
}

fn selected_opts(
    ui: &mut egui::Ui,
    th: &Theme,
    c: &Catalog,
    a: &Anno,
    l: &Lists,
    act: &mut Option<Act>,
) {
    let (mut ts, mut ls) = (a.text_style, a.line_style);
    let id = a.id;
    match &a.shape {
        AnnoShape::Text(n) => {
            if labelled_combo(ui, th, c, "text_style.lbl.text_style", "f4_sel_ts", &mut ts, &l.text) {
                *act = Some(Act::Styles(id, ts, ls));
            }
            let mut n2 = n.clone();
            let mut moved = align_row(ui, th, c, &mut n2.align);
            if !n.leaders.is_empty() {
                moved |=
                    labelled_combo(ui, th, c, "text_style.lbl.head", "f4_sel_hd", &mut n2.head, &l.head);
            }
            if moved {
                *act = Some(Act::Shape(id, AnnoShape::Text(n2)));
            }
            if bar_button(ui, th, c, "annot_text.opt.edit_text", "text_style.tip.edit_text") {
                *act = Some(Act::Edit(id));
            }
            if bar_button(ui, th, c, "annot_text.opt.add_leader", "text_style.tip.add_leader") {
                *act = Some(Act::AddLeader(id));
            }
        }
        AnnoShape::Spot { .. } => {
            if labelled_combo(ui, th, c, "text_style.lbl.text_style", "f4_sel_ts", &mut ts, &l.text) {
                *act = Some(Act::Styles(id, ts, ls));
            }
        }
        AnnoShape::Region {
            rings,
            fill,
            boundary,
        } => {
            let mut b = *boundary;
            if bar_check(ui, th, c, "annot_text.opt.boundary", &mut b) {
                *act = Some(Act::Shape(
                    id,
                    AnnoShape::Region {
                        rings: rings.clone(),
                        fill: *fill,
                        boundary: b,
                    },
                ));
            }
            if labelled_combo(ui, th, c, "text_style.lbl.line_style", "f4_sel_ls", &mut ls, &l.line) {
                *act = Some(Act::Styles(id, ts, ls));
            }
        }
        _ => {
            if labelled_combo(ui, th, c, "text_style.lbl.line_style", "f4_sel_ls", &mut ls, &l.line) {
                *act = Some(Act::Styles(id, ts, ls));
            }
        }
    }
}

fn options_bar(ui: &mut egui::Ui, rect: Rect, ed: &mut Editor, t: &Tokens, view: AnnoView) {
    let (kind, mut o, c, editing) = with(|s| {
        (
            s.draft.as_ref().map(|d| d.kind),
            s.opts.clone(),
            s.catalog(),
            s.edit.is_some(),
        )
    });
    if editing {
        return;
    }
    let its = items(ed, view);
    let chosen = single(ed)
        .filter(|_| !ed.tool_armed())
        .and_then(|id| its.list.iter().find(|i| i.anno.id == id));
    if kind.is_none() && chosen.is_none() {
        return;
    }
    let l = cached_lists(ed, &c);
    let th = ddc_ui::theme::of(ui.ctx());
    let bar_h = (th.d().control + t.spacing).max(t.spacing * 4.0);
    let bar = Rect::from_min_size(rect.min, Vec2::new(rect.width(), bar_h));
    // empty panel space swallows the click, otherwise it would place a point on the view beneath
    let _ = ui.interact(
        bar,
        egui::Id::new("f4_opts_bar"),
        egui::Sense::click_and_drag(),
    );
    let p = ui.painter_at(bar);
    p.rect_filled(bar, 0.0, t.panel);
    p.line_segment(
        [bar.left_bottom(), bar.right_bottom()],
        Stroke::new(t.lw[3], t.border),
    );
    let mut cu = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(bar.shrink2(Vec2::new(t.spacing, t.spacing * 0.5)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    let ui = &mut cu;
    crate::modify_opts::row_style(ui, &th);
    let mut act = None;
    if let Some(k) = kind {
        let title = c.t(k.cmd());
        widgets::elided_label(ui, &title, th.font(Role::BodyStrong), th.sem().text_primary);
        ui.separator();
        draft_opts(ui, &th, &c, k, &mut o, &l, &mut act);
    } else if let Some(it) = chosen {
        selected_opts(ui, &th, &c, &it.anno, &l, &mut act);
    }
    ui.separator();
    let mut sc = its.scale;
    let scales: Vec<(f64, String)> = SCALES.iter().map(|s| (*s, format!("1:{s}"))).collect();
    if labelled_combo(ui, &th, &c, "annot_text.opt.scale", "f4_scale", &mut sc, &scales) {
        act = Some(Act::Scale(sc));
    }
    with(|s| s.opts = o);
    apply(ed, view, act);
}

fn apply(ed: &mut Editor, view: AnnoView, act: Option<Act>) {
    match act {
        None => {}
        Some(Act::Styles(id, ts, ls)) => op(ed, move |m| m.set_anno_styles(id, ts, ls)),
        Some(Act::Shape(id, shape)) => op(ed, move |m| m.update_anno(id, &shape)),
        Some(Act::Scale(sc)) => op(ed, move |m| m.set_anno_scale(view, sc)),
        Some(Act::NextRing) => with(|s| {
            if let Some(d) = &mut s.draft {
                if d.pts.len() >= 3 {
                    let ring = ring_of(&d.pts);
                    d.rings.push(ring);
                    d.pts.clear();
                }
            }
        }),
        Some(Act::Edit(id)) => edit_existing(ed, id),
        Some(Act::AddLeader(id)) => {
            let lang = cat().lang;
            arm(ed, Kind::AddLeader, lang);
            ed.selection = vec![Sel::Anno(id)];
            with(|s| {
                if let Some(d) = &mut s.draft {
                    d.target = Some(id);
                }
            });
        }
    }
}

/// Legends window; the window and tooltip language follows the shell.
pub fn window(ctx: &egui::Context, ed: &mut Editor, lang: Lang) {
    with(|s| s.set_lang(lang));
    win::show(ctx, ed);
}
