//! HUD at the cursor: key parameters of the selected elements of one category. For a multi-selection
//! it shows the common value or a "mixed" placeholder; an edit (Enter) applies to all selected in one operation.
use crate::editor::{Editor, Sel};
use crate::shell::panels::{edit_openings, edit_walls};
use crate::shell::EditorApp;
use crate::shell::panels::type_dialog;
use crate::units::{display_length, edit_length, unit_label, Units};
use ddc_ui::theme::{space, Role};
use ddc_ui::widgets::type_pick::{Pick, TypePick};
use ddc_ui::widgets::{self, form};

/// Value across the selection: no elements, one common value, or mixed.
#[derive(Clone, Debug, PartialEq)]
pub enum Mixed<T> {
    Empty,
    Same(T),
    Various,
}

pub fn mixed<T: PartialEq>(vals: impl IntoIterator<Item = T>) -> Mixed<T> {
    let mut it = vals.into_iter();
    let Some(first) = it.next() else {
        return Mixed::Empty;
    };
    if it.all(|v| v == first) {
        Mixed::Same(first)
    } else {
        Mixed::Various
    }
}

/// Lengths match to 1 µm (model values after input rounding).
pub fn mixed_len(vals: impl IntoIterator<Item = f64>) -> Mixed<f64> {
    let v: Vec<f64> = vals.into_iter().collect();
    match v.first() {
        None => Mixed::Empty,
        Some(&a) if v.iter().all(|b| (b - a).abs() < 1e-6) => Mixed::Same(a),
        _ => Mixed::Various,
    }
}

/// What the field edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    WallHeight,
    /// Index into the opening's [width, height, sill].
    Opening(usize),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Len(Mixed<f64>),
    AreaSum(f64),
    Text(Mixed<String>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    /// Label key `quick.hud.*`.
    pub key: &'static str,
    pub val: Val,
    pub edit: Option<Target>,
}

/// Selection parameters: category of the first element, its id, fields; for walls, the common type.
#[derive(Clone, Debug, PartialEq)]
pub struct Hud {
    pub cat: &'static str,
    pub ids: Vec<i64>,
    /// Total selected (across all categories).
    pub total: usize,
    pub wall_type: Option<Mixed<i64>>,
    pub fields: Vec<Field>,
}

fn id_of(s: &Sel) -> Option<i64> {
    match s {
        Sel::Wall(i)
        | Sel::Opening(i)
        | Sel::Curtain(i)
        | Sel::Slab(i)
        | Sel::Room(i)
        | Sel::Mass(i)
        | Sel::Mep(i)
        | Sel::Comp(i)
        | Sel::Body(_, i)
        | Sel::Ext(_, i) => Some(*i),
        _ => None,
    }
}

pub fn build(ed: &Editor) -> Option<Hud> {
    let first = *ed.selection.first()?;
    let cat = crate::selection::category(ed, first);
    let ids: Vec<i64> = ed
        .selection
        .iter()
        .filter(|s| crate::selection::category(ed, **s) == cat)
        .filter_map(id_of)
        .collect();
    let mut h = Hud {
        cat,
        ids: ids.clone(),
        total: ed.selection.len(),
        wall_type: None,
        fields: vec![],
    };
    let f = |key, val, edit| Field { key, val, edit };
    match cat {
        "wall" => {
            let ws: Vec<&crate::editor::Wall> =
                ed.walls.iter().filter(|w| ids.contains(&w.id)).collect();
            h.wall_type = Some(mixed(ws.iter().map(|w| w.wall_type)));
            h.fields.push(f(
                "quick.hud.height",
                Val::Len(mixed_len(ws.iter().map(|w| w.height))),
                Some(Target::WallHeight),
            ));
            h.fields.push(f(
                "quick.hud.length",
                Val::Len(mixed_len(ws.iter().map(|w| (w.b - w.a).len()))),
                None,
            ));
        }
        "door" | "window" | "opening" => {
            let os: Vec<_> = ids.iter().filter_map(|i| ed.opening(*i)).collect();
            let parts: [(&'static str, fn(&crate::opening_tool::Placed) -> f64); 3] = [
                ("quick.hud.width", |o| o.width),
                ("quick.hud.height", |o| o.height),
                ("quick.hud.sill", |o| o.sill),
            ];
            for (k, (key, get)) in parts.into_iter().enumerate() {
                h.fields.push(f(
                    key,
                    Val::Len(mixed_len(os.iter().map(|o| get(o)))),
                    Some(Target::Opening(k)),
                ));
            }
        }
        "floor" => {
            let ss: Vec<_> = ed.slabs.iter().filter(|s| ids.contains(&s.id)).collect();
            h.fields.push(f(
                "quick.hud.thickness",
                Val::Len(mixed_len(ss.iter().map(|s| s.thickness))),
                None,
            ));
            h.fields.push(f(
                "quick.hud.area",
                Val::AreaSum(ss.iter().map(|s| s.area).sum()),
                None,
            ));
        }
        "room" => {
            let rs: Vec<_> = ed.rooms.iter().filter(|r| ids.contains(&r.id)).collect();
            h.fields.push(f(
                "quick.hud.name",
                Val::Text(mixed(rs.iter().map(|r| r.name.clone()))),
                None,
            ));
        }
        "curtain" => {
            let cs: Vec<_> = ed.curtains.iter().filter(|c| ids.contains(&c.id)).collect();
            h.fields.push(f(
                "quick.hud.length",
                Val::Len(mixed_len(cs.iter().map(|c| c.length()))),
                None,
            ));
        }
        _ => {}
    }
    Some(h)
}

/// Value text: for editing, a number in input units; otherwise with the unit.
pub fn text(v: &Val, u: Units, editable: bool, various: &str) -> String {
    match v {
        Val::Len(Mixed::Same(x)) if editable => edit_length(*x, u),
        Val::Len(Mixed::Same(x)) => display_length(*x, u),
        Val::AreaSum(a) => crate::unit_text::area_ui(*a),
        Val::Text(Mixed::Same(s)) => s.clone(),
        Val::Len(Mixed::Various) | Val::Text(Mixed::Various) => various.to_string(),
        Val::Len(Mixed::Empty) | Val::Text(Mixed::Empty) => "—".into(),
    }
}

/// Apply the entered value to all HUD elements; false means it did not parse or nothing changed.
pub fn commit(ed: &mut Editor, h: &Hud, t: Target, input: &str, u: Units) -> bool {
    let mark = crate::units::ui_decimal_mark();
    let v = match crate::units::parse_edit_length_lang(input, u, &crate::expr_field::Scope::none(), mark) {
        Ok(v) => v,
        Err(e) => {
            // a value the checks refuse (0, garbage) is a refusal like a refused size
            ed.say(crate::status::Severity::Refusal, e.to_string());
            return false;
        }
    };
    match t {
        Target::WallHeight => edit_walls(ed, &h.ids, None, Some(v)),
        Target::Opening(_) if v > super::args::LEN_MAX / 5.0 => false,
        Target::Opening(k) => {
            let mut dims = [None; 3];
            dims[k.min(2)] = Some(v);
            edit_openings(ed, &h.ids, dims)
        }
    }
}

pub fn commit_type(ed: &mut Editor, h: &Hud, ty: i64) -> bool {
    h.cat == "wall" && edit_walls(ed, &h.ids, Some(ty), None)
}

/// HUD position and input buffers.
#[derive(Clone, Debug, Default)]
pub struct Ui {
    pub at: Option<egui::Pos2>,
    /// Hidden via the close button until the selection changes.
    pub closed: bool,
    pub bufs: Vec<String>,
    pub for_ids: Vec<i64>,
    /// HUD rect in the previous frame: a press on it does not hide the HUD.
    pub rect: Option<egui::Rect>,
    /// Canvas and corner overlays the last frame placed the HUD against.
    pub canvas: Option<egui::Rect>,
    pub corner: Vec<egui::Rect>,
    /// The view the HUD stands in (from the last selection change).
    pub area: Option<egui::Rect>,
}

/// A press that starts on the HUD itself or on a popup layer (type list) does not hide the HUD;
/// a press in the view (drawing, box select) hides it.
pub fn press_keeps(
    rect: Option<egui::Rect>,
    origin: Option<egui::Pos2>,
    layer: Option<egui::Order>,
) -> bool {
    let Some(o) = origin else { return false };
    rect.is_some_and(|r| r.expand(2.0).contains(o))
        || matches!(
            layer,
            Some(egui::Order::Foreground | egui::Order::Tooltip | egui::Order::Debug)
        )
}

impl Ui {
    /// Selection changed: HUD goes to the cursor (inside the view), buffers are reset.
    pub fn moved(&mut self, pointer: Option<egui::Pos2>, area: egui::Rect) {
        self.closed = false;
        self.bufs.clear();
        self.for_ids.clear();
        self.area = Some(area);
        // without a pointer in the view: the second canvas bar row, under the first bar
        let p = pointer
            .filter(|p| area.contains(*p))
            .map(|p| p + egui::vec2(18.0, 22.0))
            .unwrap_or(crate::ui_kit::canvas_bar_pos(area, 1));
        let max = egui::pos2(area.right() - 380.0, area.bottom() - 48.0);
        self.at = Some(egui::pos2(
            p.x.min(max.x).max(area.left() + 4.0),
            p.y.min(max.y).max(area.top() + 4.0),
        ));
    }
}

/// Without a value or a type the HUD would only repeat the option bar's "Stairs: 1" chip.
pub fn worth_showing(h: &Hud) -> bool {
    h.wall_type.is_some() || !h.fields.is_empty()
}

/// Canvas edge to an overlay, and the step between overlays stacked in one corner.
pub const EDGE: f32 = ddc_ui::theme::space::MD;
pub const STEP: f32 = ddc_ui::theme::space::SM;

fn corner_id() -> egui::Id {
    egui::Id::new("ddc.canvas.corner_tl")
}

/// A canvas overlay that lives in the top-left corner (the 3D view bar) reports its rect each
/// frame; the edit HUD stacks below it instead of under or over it.
pub fn reserve_corner(ctx: &egui::Context, rect: egui::Rect) {
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| {
        let v = d.get_temp_mut_or_default::<(u64, Vec<egui::Rect>)>(corner_id());
        if v.0 != pass {
            *v = (pass, vec![]);
        }
        v.1.push(rect);
    });
}

/// Corner rects reported this frame or the last one (draw order may put them after the HUD).
pub fn corner_rects(ctx: &egui::Context) -> Vec<egui::Rect> {
    let pass = ctx.cumulative_pass_nr();
    ctx.data(|d| d.get_temp::<(u64, Vec<egui::Rect>)>(corner_id()))
        .filter(|(p, _)| *p + 1 >= pass)
        .map(|(_, v)| v)
        .unwrap_or_default()
}

/// Where a HUD of `size` goes when asked for `at`: inside `canvas` less the edge, below every
/// corner overlay it would touch.
pub fn place(
    at: egui::Pos2,
    size: egui::Vec2,
    canvas: egui::Rect,
    corner: &[egui::Rect],
) -> egui::Pos2 {
    let inner = canvas.shrink(EDGE);
    let mut p = at;
    for _ in 0..corner.len() + 1 {
        let r = egui::Rect::from_min_size(p, size);
        let hit = corner.iter().filter(|c| c.expand(STEP - 0.5).intersects(r));
        match hit.map(|c| c.bottom()).reduce(f32::max) {
            Some(b) => p.y = b + STEP,
            None => break,
        }
    }
    egui::pos2(
        p.x.min(inner.right() - size.x).max(inner.left()),
        p.y.min(inner.bottom() - size.y).max(inner.top()),
    )
}

/// Which parts fit `avail`: (type kept, fields kept). `fixed` is the head and close button,
/// `more` the "…" that stands for what left. Fields leave from the last (Length, then Height),
/// then the type; the head and the count always stay.
pub fn fit(avail: f32, fixed: f32, ty: Option<f32>, fields: &[f32], more: f32) -> (bool, usize) {
    let total = |t: bool, n: usize| {
        let gone = n < fields.len() || (ty.is_some() && !t);
        fixed
            + if t { ty.unwrap_or(0.0) } else { 0.0 }
            + fields[..n].iter().sum::<f32>()
            + if gone { more } else { 0.0 }
    };
    let (mut t, mut n) = (ty.is_some(), fields.len());
    while total(t, n) > avail {
        if n > 0 {
            n -= 1;
        } else if t {
            t = false;
        } else {
            break;
        }
    }
    (t, n)
}

/// A window, modal or not, covers `r`: canvas overlays never draw over windows.
pub(crate) fn under_a_window(ctx: &egui::Context, r: egui::Rect, own: egui::Id) -> bool {
    ctx.memory(|m| {
        m.top_modal_layer().is_some()
            || m.areas().visible_layer_ids().into_iter().any(|l| {
                l.order == egui::Order::Middle
                    && l.id != own
                    && l.id != egui::Id::new("ddc.quick.bar")
                    && m.area_rect(l.id).is_some_and(|w| w.intersects(r))
            })
    })
}

pub fn ui(app: &mut EditorApp, ctx: &egui::Context, canvas: egui::Rect) {
    // stays None unless the HUD is drawn this frame
    let last = app.quick.hud.rect.take();
    app.quick.hud.canvas = Some(canvas);
    if app.quick.ux.hud_off
        || app.quick.hud.closed
        || app.ed.selection.is_empty()
        || app.quick.menu.is_some()
        || !super::idle(app)
    {
        return;
    }
    let press = ctx.input(|i| i.pointer.primary_down().then(|| i.pointer.press_origin()));
    if let Some(origin) = press {
        let layer = origin.and_then(|o| ctx.layer_id_at(o)).map(|l| l.order);
        if !press_keeps(last, origin, layer) {
            return;
        }
    }
    let Some(at) = app.quick.hud.at else { return };
    let Some(h) = build(&app.ed) else { return };
    if !worth_showing(&h) {
        return;
    }
    if app.quick.hud.for_ids != h.ids || app.quick.hud.bufs.len() != h.fields.len() {
        app.quick.hud.for_ids = h.ids.clone();
        let u = app.ed.tool.units;
        app.quick.hud.bufs = h.fields.iter().map(|f| field_text(&f.val, u)).collect();
    }
    let u = app.ed.tool.units;
    let c = &app.sh.cat;
    let various = c.t("quick.various");
    let mut commit_at: Option<usize> = None;
    let mut new_type: Option<i64> = None;
    let mut close = false;
    let head = if h.total > h.ids.len() {
        c.tf(
            "quick.hud.head_more",
            &[
                ("cat", &c.t(&format!("quick.cat.{}", h.cat))),
                ("n", &h.ids.len().to_string()),
                ("all", &h.total.to_string()),
            ],
        )
    } else {
        c.tf(
            "quick.hud.head",
            &[
                ("cat", &c.t(&format!("quick.cat.{}", h.cat))),
                ("n", &h.ids.len().to_string()),
            ],
        )
    };
    // what fits the canvas: the HUD never reaches past it (narrow canvas in the AI modes)
    let style = ctx.style();
    let font = egui::TextStyle::Body.resolve(&style);
    let w = |s: &str| {
        ctx.fonts(|f| f.layout_no_wrap(s.to_string(), font.clone(), egui::Color32::PLACEHOLDER))
            .size()
            .x
    };
    let sp = style.spacing.item_spacing.x;
    let field_w: Vec<f32> = h
        .fields
        .iter()
        .map(|fl| {
            let v = if fl.edit.is_some() {
                FIELD_W + 2.0 * style.spacing.button_padding.x
            } else {
                w(&text(&fl.val, u, false, &various))
            };
            w(&c.t(fl.key)).min(LABEL_MAX) + sp + v + sp + w(unit_label(u)) + sp
        })
        .collect();
    let type_w = h
        .wall_type
        .as_ref()
        .map(|_| w(&c.t("quick.hud.type")).min(LABEL_MAX) + sp + TYPE_W + sp);
    let frame_w = 2.0 * (8.0 + 1.0);
    let fixed = frame_w + w(&head) + sp + ddc_ui::theme::control::HIT_MIN;
    let more_w = w("\u{2026}") + 2.0 * sp;
    let avail = canvas.width() - 2.0 * EDGE;
    let (keep_type, keep_n) = fit(avail, fixed, type_w, &field_w, more_w);
    let hidden: Vec<String> = h
        .wall_type
        .iter()
        .filter(|_| !keep_type)
        .map(|_| c.t("quick.hud.type"))
        .chain(h.fields[keep_n..].iter().map(|fl| c.t(fl.key)))
        .collect();
    let more_tip = c.tf("quick.hud.more", &[("names", &hidden.join(", "))]);
    let id = egui::Id::new("ddc.quick.hud");
    let size = last
        .map_or(egui::vec2(avail.min(380.0), 36.0), |r| r.size())
        .min(egui::vec2(avail, canvas.height()));
    // inside the view, clear of the panels beside it, then clear of the corner bars
    let at = app.quick.hud.area.map_or(at, |area| fit_in(at, size, area));
    let corner = corner_rects(ctx);
    let at = place(at, size, canvas, &corner);
    app.quick.hud.corner = corner;
    if under_a_window(ctx, egui::Rect::from_min_size(at, size), id) {
        return;
    }
    let th = ddc_ui::theme::of(ctx);
    let ru = c.lang == ddc_ui::Lang::Ru;
    let rows = h
        .wall_type
        .as_ref()
        .map(|_| crate::shell::panels::type_items::items(&app.ed.types, u, ru));
    let first_type = app.ed.types.first().map(|t| t.id);
    let mut edit_type = None;
    let close_tip = c.t("quick.hud.close");
    let type_label = c.t("quick.hud.type");
    let labels: Vec<String> = h.fields.iter().map(|f| c.t(f.key)).collect();
    let bufs = &mut app.quick.hud.bufs;
    let mut focused = vec![false; h.fields.len()];
    let shown_rect = egui::Area::new(id)
        .fixed_pos(at)
        .constrain_to(canvas.shrink(EDGE))
        // canvas overlay order; windows over it hide it (`under_a_window`)
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            ui.set_clip_rect(canvas);
            ui.set_max_width(avail);
            egui::Frame::popup(ui.style())
                .inner_margin(egui::Margin::symmetric(space::SM as i8, space::XXS as i8))
                .show(ui, |ui| {
                    form::inline(ui, &th, |ui| {
                        ui.spacing_mut().item_spacing.x = space::SM;
                        let title = th.font(Role::Label);
                        let col = th.sem().text_primary;
                        ui.label(egui::RichText::new(head).font(title).color(col));
                        let shown_type = h.wall_type.as_ref().filter(|_| keep_type);
                        if let (Some(mt), Some(rows)) = (shown_type, &rows) {
                            widgets::vdivider(ui, &th, th.d().control);
                            form::label(ui, &th, &type_label, LABEL_MAX);
                            let cur = match mt {
                                Mixed::Same(id) => Some(*id),
                                _ => None,
                            };
                            let texts = crate::shell::panels::type_items::texts(ru);
                            let pick = TypePick::new("ddc.quick.hud.type", rows, cur)
                                .texts(texts)
                                .width(TYPE_W)
                                .show(ui, &th);
                            match pick {
                                Some(Pick::Id(id)) => new_type = Some(id),
                                Some(Pick::EditType) => edit_type = cur.or(first_type),
                                None => {}
                            }
                        }
                        for (i, fl) in h.fields.iter().enumerate().take(keep_n) {
                            let lab = form::label(ui, &th, &labels[i], LABEL_MAX);
                            let editable = fl.edit.is_some();
                            if !editable {
                                // read-only: plain text, never a frame that invites typing
                                let shown = text(&fl.val, u, false, &various);
                                let (n, unit) = crate::ui_kit::split_unit(&shown);
                                form::value(ui, &th, n, None);
                                if !unit.is_empty() {
                                    form::unit(ui, &th, unit);
                                }
                                continue;
                            }
                            let hint = if matches!(fl.val, Val::Len(Mixed::Various)) {
                                various.as_str()
                            } else {
                                ""
                            };
                            let f = widgets::TextField::new(&mut bufs[i])
                                .id_salt(("ddc.quick.hud.field", i))
                                .labelled_by(lab.id, &labels[i])
                                .right_aligned()
                                .hint(hint)
                                .width(FIELD_W)
                                .show(ui, &th);
                            form::unit(ui, &th, unit_label(u));
                            focused[i] = f.response.has_focus();
                            if f.committed && !f.cancelled {
                                commit_at = Some(i);
                            }
                        }
                        if !hidden.is_empty() {
                            ui.label(egui::RichText::new("\u{2026}").weak())
                                .on_hover_text(&more_tip);
                        }
                        if widgets::close_button(ui, &close_tip).clicked() {
                            close = true;
                        }
                    });
                });
        })
        .response
        .rect;
    app.quick.hud.rect = Some(shown_rect);
    if close {
        app.quick.hud.closed = true;
    }
    if let Some(t) = new_type {
        commit_type(&mut app.ed, &h, t);
    }
    if let Some(id) = edit_type {
        crate::shell::panels::with_type_dialog(&app.ed, type_dialog::Fam::Wall, id);
    }
    if let Some(i) = commit_at {
        let input = app.quick.hud.bufs[i].clone();
        let shown = text(&h.fields[i].val, u, true, &various);
        if let Some(t) = h.fields[i].edit {
            let typed = !input.trim().is_empty() && input.trim() != shown;
            if typed && !commit(&mut app.ed, &h, t, &input, u) {
                super::toast(
                    app,
                    ddc_ui::notify::Level::Warning,
                    "quick.hud.bad",
                    &[("v", &input)],
                );
            }
        }
    }
    // a field not being typed in shows the stored value (blank for different values)
    let fresh = build(&app.ed).filter(|n| n.fields.len() == app.quick.hud.bufs.len());
    if let Some(n) = fresh {
        for (i, fl) in n.fields.iter().enumerate() {
            if !focused[i] && fl.edit.is_some() {
                app.quick.hud.bufs[i] = field_text(&fl.val, u);
            }
        }
    }
}

/// Widest label of the HUD before it ends with «…».
const LABEL_MAX: f32 = 160.0;
/// Type picker and number field widths of the HUD.
const TYPE_W: f32 = 160.0;
const FIELD_W: f32 = 80.0;

/// Text an editable HUD field holds while it is not being typed in: the shared value in input
/// units, or nothing when the selected elements differ (the hint says so).
pub fn field_text(v: &Val, u: Units) -> String {
    match v {
        Val::Len(Mixed::Same(x)) => edit_length(*x, u),
        _ => String::new(),
    }
}

/// Top-left corner of a HUD of `size` asked at `at`, moved inside `area` (the view) with a
/// small margin, so it never covers the panels beside the view.
pub fn fit_in(at: egui::Pos2, size: egui::Vec2, area: egui::Rect) -> egui::Pos2 {
    let m = space::XS;
    let x = at.x.min(area.right() - size.x - m).max(area.left() + m);
    let y = at.y.min(area.bottom() - size.y - m).max(area.top() + m);
    egui::pos2(x, y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wall_tool::{Event, Mode};
    use ddc_geom::v2;

    fn room() -> Editor {
        let mut ed = Editor::new_in_memory().unwrap();
        ed.set_mode(Mode::Rect);
        ed.handle(Event::Press(v2(0.0, 0.0)));
        ed.handle(Event::Release(v2(6.0, 4.0)));
        ed.set_mode(Mode::Select);
        ed
    }

    #[test]
    fn mixed_values() {
        assert_eq!(mixed(Vec::<i32>::new()), Mixed::Empty);
        assert_eq!(mixed([2, 2, 2]), Mixed::Same(2));
        assert_eq!(mixed(["a", "b"]), Mixed::Various);
        assert_eq!(mixed_len([3.0, 3.0 + 1e-9]), Mixed::Same(3.0));
        assert_eq!(mixed_len([3.0, 2.5]), Mixed::Various);
        let v = Val::Len(Mixed::Various);
        assert_eq!(text(&v, Units::Metric, true, "<\u{440}\u{430}\u{437}\u{43d}\u{44b}\u{435}>"), "<\u{440}\u{430}\u{437}\u{43d}\u{44b}\u{435}>");
        assert_eq!(
            text(&Val::Len(Mixed::Same(2.8)), Units::Metric, true, "-"),
            "2\u{a0}800"
        );
        assert_eq!(
            text(&Val::Text(Mixed::Empty), Units::Metric, false, "-"),
            "—"
        );
    }

    #[test]
    fn a_hud_with_nothing_to_show_does_not_repeat_the_option_bar_chip() {
        let mut ed = room();
        ed.selection = ed.walls.iter().map(|w| Sel::Wall(w.id)).collect();
        assert!(worth_showing(&build(&ed).unwrap()), "walls: height, length, type");
        let bare = Hud { cat: "stair", ids: vec![1], total: 1, wall_type: None, fields: vec![] };
        assert!(!worth_showing(&bare), "a stair has no HUD value: the chip says it all");
    }

    #[test]
    fn mixed_value_edit_one_undo_step() {
        let mut ed = room();
        let ids: Vec<i64> = ed.walls.iter().map(|w| w.id).collect();
        assert!(edit_walls(&mut ed, &ids[..1], None, Some(2.5)));
        ed.selection = ids.iter().map(|i| Sel::Wall(*i)).collect();
        let h = build(&ed).unwrap();
        assert_eq!((h.cat, h.ids.len(), h.total), ("wall", 4, 4));
        assert!(matches!(h.wall_type, Some(Mixed::Same(_))));
        assert_eq!(h.fields[0].val, Val::Len(Mixed::Various));
        assert_eq!(h.fields[0].edit, Some(Target::WallHeight));
        // lengths 6 and 4 m differ, read-only
        assert_eq!(h.fields[1].val, Val::Len(Mixed::Various));
        assert_eq!(h.fields[1].edit, None);
        let d0 = ed.undo_len();
        assert!(commit(
            &mut ed,
            &h,
            Target::WallHeight,
            "2800",
            Units::Metric
        ));
        assert_eq!(ed.undo_len(), d0 + 1);
        let h2 = build(&ed).unwrap();
        assert_eq!(h2.fields[0].val, Val::Len(Mixed::Same(2.8)));
        // repeating the same value writes nothing; garbage is not parsed
        assert!(!commit(
            &mut ed,
            &h2,
            Target::WallHeight,
            "2800",
            Units::Metric
        ));
        assert!(!commit(
            &mut ed,
            &h2,
            Target::WallHeight,
            "abc",
            Units::Metric
        ));
        assert_eq!(ed.undo_len(), d0 + 1);
        ed.undo();
        let h3 = build(&ed).unwrap();
        assert_eq!(h3.fields[0].val, Val::Len(Mixed::Various));
        let n25 = ed
            .walls
            .iter()
            .filter(|w| (w.height - 2.5).abs() < 1e-9)
            .count();
        assert_eq!(n25, 1);
        // type for all in one operation; mixed selection: the HUD follows the first category
        let w0 = ed.walls[0].wall_type;
        if let Some(t) = ed.types.iter().map(|t| t.id).find(|t| *t != w0) {
            assert!(commit_type(&mut ed, &h3, t));
            assert_eq!(ed.undo_len(), d0 + 1);
            assert!(ed.walls.iter().all(|w| w.wall_type == t));
        }
        let same = ed.walls[0].wall_type;
        assert!(!commit_type(&mut ed, &h3, same));
        ed.selection.push(Sel::Room(999));
        let h4 = build(&ed).unwrap();
        assert_eq!((h4.ids.len(), h4.total), (4, 5));
        ed.selection.clear();
        assert!(build(&ed).is_none());
    }

    #[test]
    fn hud_stays_inside_the_view_and_off_the_panels() {
        let view = egui::Rect::from_min_max(egui::pos2(236.0, 270.0), egui::pos2(1140.0, 870.0));
        let size = egui::vec2(530.0, 32.0);
        let at = fit_in(egui::pos2(708.0, 312.0), size, view);
        let r = egui::Rect::from_min_size(at, size);
        assert!(view.contains_rect(r), "{r:?} leaves {view:?}");
        assert_eq!(at.y, 312.0, "only moved sideways");
        let at = fit_in(egui::pos2(100.0, 900.0), size, view);
        assert!(view.contains_rect(egui::Rect::from_min_size(at, size)), "{at:?}");
        let three = field_text(&Val::Len(Mixed::Same(3.0)), Units::Metric);
        assert_eq!(three, edit_length(3.0, Units::Metric));
        assert!(three.starts_with('3') && three.ends_with("000"), "{three}");
        assert_eq!(field_text(&Val::Len(Mixed::Various), Units::Metric), "");
    }

    #[test]
    fn press_on_hud_or_popup_keeps_it() {
        let r = egui::Rect::from_min_size(egui::pos2(100.0, 100.0), egui::vec2(300.0, 30.0));
        let bg = Some(egui::Order::Background);
        assert!(press_keeps(Some(r), Some(egui::pos2(150.0, 110.0)), bg));
        // the popup type list is outside the HUD rect but on the top layer
        assert!(press_keeps(
            Some(r),
            Some(egui::pos2(150.0, 300.0)),
            Some(egui::Order::Foreground)
        ));
        // a press in the view hides it
        assert!(!press_keeps(Some(r), Some(egui::pos2(500.0, 500.0)), bg));
        assert!(!press_keeps(None, Some(egui::pos2(150.0, 110.0)), bg));
        assert!(!press_keeps(Some(r), None, None));
    }

    #[test]
    fn new_categories_have_ids_and_labels() {
        for s in [Sel::Mass(3), Sel::Mep(4), Sel::Comp(5)] {
            assert!(id_of(&s).is_some(), "{s:?}");
        }
        for lang in [ddc_ui::Lang::Ru, ddc_ui::Lang::En] {
            let c = ddc_ui::Catalog::new(lang);
            for cat in crate::selection::CATEGORIES {
                let k = format!("quick.cat.{cat}");
                assert!(c.table(lang).contains_key(&k), "{k}");
            }
        }
    }

    #[test]
    fn fields_leave_length_first_then_height_then_type() {
        let f = [60.0, 70.0];
        assert_eq!(fit(1000.0, 100.0, Some(200.0), &f, 20.0), (true, 2));
        assert_eq!(fit(429.0, 100.0, Some(200.0), &f, 20.0), (true, 1), "Length leaves");
        assert_eq!(fit(379.0, 100.0, Some(200.0), &f, 20.0), (true, 0), "then Height");
        assert_eq!(fit(200.0, 100.0, Some(200.0), &f, 20.0), (false, 0));
        assert_eq!(fit(10.0, 100.0, None, &f, 20.0), (false, 0), "the head always stays");
    }

    #[test]
    fn place_keeps_the_hud_in_the_canvas_and_below_the_corner_bar() {
        let canvas = egui::Rect::from_min_size(egui::pos2(240.0, 200.0), egui::vec2(445.0, 600.0));
        let size = egui::vec2(400.0, 36.0);
        let p = place(egui::pos2(600.0, 240.0), size, canvas, &[]);
        let r = egui::Rect::from_min_size(p, size);
        assert!(canvas.shrink(EDGE - 0.01).contains_rect(r), "{r:?} in {canvas:?}");
        let bar_at = canvas.min + egui::vec2(8.0, 8.0);
        let bar = egui::Rect::from_min_size(bar_at, egui::vec2(300.0, 32.0));
        let p = place(canvas.min + egui::vec2(12.0, 12.0), size, canvas, &[bar]);
        let r = egui::Rect::from_min_size(p, size);
        assert!(!r.intersects(bar), "{r:?} under the view bar {bar:?}");
        assert!((r.top() - (bar.bottom() + STEP)).abs() < 0.01, "stacked with the step");
    }
}
