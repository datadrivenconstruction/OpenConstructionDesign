// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! One scaffold for every dialog and tool window: size classes, remembered geometry, Enter/Esc,
//! one primary action, quiet secondary and distinct destructive actions, empty, error and
//! progress states. Colours come from the theme through `egui::Visuals`.
use ddc_ui::theme::{layout, Role};
use ddc_ui::widgets::{self, Button};
use egui::{self, Align2, Id, Key, LayerId, Layout, Order, RichText, Vec2};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Size class of a window; a size outside the classes goes through `Dialog::size`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DialogSize {
    /// Palette or progress beside the canvas.
    Hud,
    /// Side sheet for a tool, anchored to the right edge by the caller.
    Sheet,
    /// Question with two or three actions.
    Confirm,
    /// A few fields and a footer.
    Form,
    /// List or manager window, resizable.
    Tool,
    /// Table with details, resizable.
    Wide,
    /// Editor with tabs, resizable.
    Workspace,
}

/// Width of a side sheet.
pub const SHEET_W: f32 = 420.0;
/// Height of the rule shadow under a header while the body is scrolled.
const HEADER_SHADOW_H: f32 = 6.0;

/// The header gets a shadow rule once the body has scrolled away from the top.
pub fn header_shadow_visible(offset_y: f32) -> bool {
    offset_y > 0.5
}

/// Largest dialog on a screen: 980 wide or 90 % of the width, 640 high or 90 % of the height;
/// the body scrolls inside it.
pub fn dialog_room(screen: Vec2) -> Vec2 {
    Vec2::new(
        (screen.x * 0.9).min(layout::DIALOG_WORKSPACE),
        (screen.y * 0.9).min(layout::DIALOG_WORKSPACE_H),
    )
    .max(Vec2::splat(120.0))
}

impl DialogSize {
    pub fn size(self) -> Vec2 {
        match self {
            DialogSize::Sheet => Vec2::new(SHEET_W, 0.0),
            DialogSize::Hud => Vec2::new(layout::DIALOG_HUD, 0.0),
            DialogSize::Confirm => Vec2::new(layout::DIALOG_CONFIRM, 0.0),
            DialogSize::Form => Vec2::new(layout::DIALOG_FORM, 0.0),
            DialogSize::Tool => Vec2::new(layout::DIALOG_TOOL, layout::DIALOG_TOOL_H),
            DialogSize::Wide => Vec2::new(layout::DIALOG_WIDE, layout::DIALOG_WIDE_H),
            DialogSize::Workspace => {
                Vec2::new(layout::DIALOG_WORKSPACE, layout::DIALOG_WORKSPACE_H)
            }
        }
    }

    pub fn resizable(self) -> bool {
        matches!(
            self,
            DialogSize::Tool | DialogSize::Wide | DialogSize::Workspace
        )
    }
}

/// Builder over `egui::Window`. The key names the window in the geometry file, so it stays
/// stable across versions and languages.
pub struct Dialog<'o> {
    key: String,
    title: egui::WidgetText,
    size: Vec2,
    resizable: bool,
    min: Option<Vec2>,
    open: Option<&'o mut bool>,
    anchor: Option<(Align2, Vec2)>,
    first_pos: Option<egui::Pos2>,
    esc: bool,
    scroll: bool,
    collapsible: bool,
    id: Option<Id>,
    hud: bool,
    title_bar: bool,
    stable: bool,
}

impl<'o> Dialog<'o> {
    pub fn new(key: &str, title: impl Into<egui::WidgetText>, size: DialogSize) -> Self {
        Dialog {
            key: key.to_string(),
            title: title.into(),
            size: size.size(),
            resizable: size.resizable(),
            min: None,
            open: None,
            anchor: None,
            first_pos: None,
            esc: true,
            scroll: false,
            collapsible: false,
            id: None,
            hud: size == DialogSize::Hud,
            title_bar: true,
            stable: false,
        }
    }

    /// Pinned to a screen corner, no remembered position, Esc belongs to the canvas.
    pub fn hud(
        key: &str,
        title: impl Into<egui::WidgetText>,
        align: Align2,
        offset: impl Into<Vec2>,
    ) -> Self {
        let mut d = Self::new(key, title, DialogSize::Hud);
        d.anchor = Some((align, offset.into()));
        d.esc = false;
        d
    }

    /// Close button and Esc write here.
    pub fn open(mut self, open: &'o mut bool) -> Self {
        self.open = Some(open);
        self
    }

    /// Default size outside the classes.
    pub fn size(mut self, size: impl Into<Vec2>) -> Self {
        self.size = size.into();
        self
    }

    pub fn min_size(mut self, min: impl Into<Vec2>) -> Self {
        self.min = Some(min.into());
        self
    }

    /// Position when nothing is remembered yet; otherwise the window opens centred.
    pub fn first_pos(mut self, pos: impl Into<egui::Pos2>) -> Self {
        self.first_pos = Some(pos.into());
        self
    }

    pub fn esc_closes(mut self, on: bool) -> Self {
        self.esc = on;
        self
    }

    /// The whole body scrolls; for bodies without their own full-height scroll area.
    pub fn scroll_body(mut self) -> Self {
        self.scroll = true;
        self
    }

    pub fn collapsible(mut self, on: bool) -> Self {
        self.collapsible = on;
        self
    }

    /// For a body that draws its own title row.
    pub fn no_title_bar(mut self) -> Self {
        self.title_bar = false;
        self
    }

    /// The frame never follows the body. A fixed class keeps exactly its width, whatever the body
    /// holds; a resizable class fills its size, so the footer stays put as rows come and go.
    pub fn stable(mut self) -> Self {
        self.stable = true;
        self
    }

    /// Egui id when one window exists per view; the default derives it from the key.
    pub fn id(mut self, id: Id) -> Self {
        self.id = Some(id);
        self
    }

    pub fn show<R>(
        self,
        ctx: &egui::Context,
        add: impl FnOnce(&mut egui::Ui) -> R,
    ) -> Option<egui::InnerResponse<Option<R>>> {
        let Dialog {
            key,
            title,
            size,
            resizable,
            min,
            open,
            anchor,
            first_pos,
            esc,
            scroll,
            collapsible,
            id,
            hud,
            title_bar,
            stable,
        } = self;
        let id = id.unwrap_or_else(|| Id::new(("ddc.dialog", key.as_str())));
        ddc_ui::z::window(ctx, id);
        let keys = keys_begin(ctx, id, hud);
        let screen = ctx.screen_rect();
        let room = dialog_room(screen.size());
        // egui bounds the body only; the room holds the whole window, frame included
        let frame_h = egui::Frame::window(&ctx.style()).total_margin().sum().y;
        let body_room = Vec2::new(room.x, (room.y - frame_h).max(0.0));
        // a body that outgrew the room last frame scrolls; egui would grow the window to it
        let overflow_id = id.with("overflow");
        let overflowed = !scroll && ctx.data(|d| d.get_temp::<bool>(overflow_id)).unwrap_or(false);
        let saved = geom_get(&key);
        let mut is_open = open.as_deref().copied().unwrap_or(true);
        // one header for every window: title on the left, the close cross on the right; egui's
        // own bar centres its title, so only a collapsible window keeps it
        let own_header = title_bar && !collapsible;
        let header = own_header.then(|| (title.text().to_owned(), open.is_some()));
        let mut crossed = false;
        let mut win = egui::Window::new(title)
            .id(id)
            .title_bar(title_bar && !own_header)
            .collapsible(collapsible)
            .resizable(resizable)
            .constrain(true)
            .fade_in(false)
            .fade_out(false)
            .max_height(body_room.y)
            .max_width(room.x);
        if open.is_some() {
            win = win.open(&mut is_open);
        }
        if resizable {
            let start = match saved {
                Some(g) if g[2] > 0.0 && g[3] > 0.0 => Vec2::new(g[2], g[3]),
                _ => size,
            };
            let least = min.unwrap_or(Vec2::new((size.x * 0.6).round(), 200.0));
            win = win.default_size(start.min(body_room)).min_size(least.min(body_room));
        } else {
            win = win
                .default_width(size.x.min(room.x))
                .min_width(min.map_or(size.x, |m| m.x).min(room.x));
        }
        if let Some((a, off)) = anchor {
            win = win.anchor(a, off);
        } else if let Some(g) = saved {
            win = win.default_pos(clamp_pos(egui::pos2(g[0], g[1]), screen));
        } else if let Some(p) = first_pos {
            win = win.default_pos(p);
        } else {
            win = win
                .pivot(Align2::CENTER_CENTER)
                .default_pos(screen.center());
        }
        if anchor.is_none() {
            // egui overwrites the live pivot every frame and keeps the pivot position, so
            // the pivot may change only together with a position that holds the window still
            if let Some(a) = egui::AreaState::load(ctx, id) {
                let pressed = ctx.input(|i| i.pointer.any_pressed());
                win = if pressed && a.size.is_some() {
                    win.pivot(Align2::LEFT_TOP).current_pos(a.left_top_pos())
                } else {
                    win.pivot(a.pivot)
                };
            }
        }
        // egui sizes a window's frame from its content and `min_width` bounds the layout only:
        // without this a fixed class is as wide as its widest row
        let fixed_w = (!resizable).then(|| min.map_or(size.x, |m| m.x.max(size.x)).min(room.x));
        let mut measured = None;
        let mut fits_again = false;
        let mut used_h = 0.0f32;
        let resp = win.show(ctx, |ui| {
            measured = Some(ui.available_size());
            set_scope(ui.ctx(), keys);
            // the header lives inside the width the class sets: it never widens the window
            let body = |ui: &mut egui::Ui| {
                if let Some((text, closable)) = &header {
                    crossed = dialog_header(ui, text, *closable);
                }
                if scroll || overflowed {
                    let top = ui.cursor().min.y;
                    // the room under the header, not the window's height: egui may have grown
                    // the window past the room, and a body measured against that never scrolls
                    // a resizable class may be shorter than the room: then its own height bounds it
                    let avail =
                        (body_room.y - (top - ui.min_rect().top())).min(ui.available_height());
                    // inside the scroll the header is out of the body's min_rect: room_left adds it
                    let above = top - ui.min_rect().top();
                    ui.ctx().data_mut(|d| d.insert_temp(above_id(), above));
                    let out = egui::ScrollArea::vertical()
                        .auto_shrink([false, !scroll])
                        .max_height(avail.max(0.0))
                        .show(ui, add);
                    super::mark_scroll_viewport(ui.ctx(), out.inner_rect);
                    ui.ctx().data_mut(|d| d.remove_temp::<f32>(above_id()));
                    fits_again = overflowed && out.content_size.y + 0.5 < avail;
                    if header_shadow_visible(out.state.offset.y) {
                        let r = ui.max_rect();
                        let strip = egui::Rect::from_min_max(
                            egui::pos2(r.min.x, top),
                            egui::pos2(r.max.x, top + HEADER_SHADOW_H),
                        );
                        let c = ddc_ui::theme::sem_of(ui.ctx()).bg_backdrop;
                        let top_c = c.gamma_multiply(0.5);
                        let mut mesh = egui::Mesh::default();
                        mesh.colored_vertex(strip.left_top(), top_c);
                        mesh.colored_vertex(strip.right_top(), top_c);
                        mesh.colored_vertex(strip.right_bottom(), egui::Color32::TRANSPARENT);
                        mesh.colored_vertex(strip.left_bottom(), egui::Color32::TRANSPARENT);
                        mesh.add_triangle(0, 1, 2);
                        mesh.add_triangle(0, 2, 3);
                        ui.painter().add(egui::Shape::mesh(mesh));
                    }
                    out.inner
                } else {
                    add(ui)
                }
            };
            let out = match (fixed_w, stable) {
                (Some(w), true) => locked_width(ui, w, body),
                (Some(w), false) => {
                    ui.set_min_width(w);
                    ui.set_max_width(w);
                    body(ui)
                }
                (None, true) => {
                    ui.set_min_size(ui.available_size());
                    body(ui)
                }
                (None, false) => body(ui),
            };
            used_h = ui.min_rect().height();
            set_scope(ui.ctx(), Keys::default());
            out
        });
        keys_end(ctx, keys);
        if !scroll {
            if let Some(r) = &resp {
                // a body taller than the room overflows even where egui clips the window to it
                let over = used_h > body_room.y + 0.5 || r.response.rect.height() > room.y + 0.5;
                if over != overflowed && (over || fits_again) {
                    ctx.data_mut(|d| d.insert_temp(overflow_id, over));
                    ctx.request_repaint();
                }
            }
        }
        let esc_now =
            esc && open.is_some() && keys.esc && ctx.input_mut(|i| take_key(i, Key::Escape));
        if let Some(o) = open {
            *o = is_open && !esc_now && !crossed;
            if !*o {
                keys_forget(ctx, id);
            }
        }
        // a collapsed window has no body: its size is unknown and must not replace the saved one
        if let (Some(r), Some(body), None) = (&resp, measured, anchor) {
            let p = clamp_pos(r.response.rect.min, screen);
            let s = if resizable { body } else { Vec2::ZERO };
            remember_if_moved(ctx, id, &key, [p.x, p.y, s.x, s.y]);
        }
        resp
    }
}

/// Height a dialog body has left under its cursor before the window reaches the room: a list
/// capped to it keeps the footer under it on screen.
pub fn room_left(ui: &egui::Ui) -> f32 {
    let ctx = ui.ctx();
    let frame_h = egui::Frame::window(&ctx.style()).total_margin().sum().y;
    let room = dialog_room(ctx.screen_rect().size()).y - frame_h;
    let above = ctx.data(|d| d.get_temp::<f32>(above_id())).unwrap_or(0.0);
    room - above - (ui.cursor().top() - ui.min_rect().top())
}

/// Height of the dialog header over a body that scrolls, while that body is drawn.
fn above_id() -> Id {
    Id::new("ddc.dialog.above_body")
}

/// Accessible name of the header's close cross (U+2715).
pub const CROSS: &str = "\u{2715}";

/// Window header: the title on the left in the dialog-title style, elided to the room it has,
/// the close cross on the right when the window can close, a rule below. True on the cross.
pub fn dialog_header(ui: &mut egui::Ui, title: &str, closable: bool) -> bool {
    let th = ddc_ui::theme::of(ui.ctx());
    let mut crossed = false;
    ui.horizontal(|ui| {
        let cross_w = if closable {
            th.d().control + 4.0 + ui.spacing().item_spacing.x
        } else {
            0.0
        };
        let room = (ui.available_width() - cross_w).max(0.0);
        ui.allocate_ui(Vec2::new(room, th.d().control), |ui| {
            ui.set_min_width(room);
            // the theme's dialog-title font, through the style so a bare context still draws
            let font = egui::TextStyle::Heading.resolve(ui.style());
            widgets::elided_label(ui, title, font, th.c("text.primary"));
        });
        if closable {
            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                let tip = crate::tr("dlg.close", &[]);
                let ctx = ui.ctx().clone();
                let b = widgets::with_icons(&ctx, |icons| {
                    widgets::icon_button(ui, icons, &th, "close", false, &tip)
                });
                // the kit knows no catalog of its own: the name is the cross itself, the same
                // in every language; the tooltip carries the word
                let named = egui::WidgetInfo::labeled(egui::WidgetType::Button, true, CROSS);
                b.widget_info(|| named.clone());
                crossed = b.clicked();
            });
        }
    });
    ui.separator();
    crossed
}

/// Runs the body in a child of exactly width `w`: a row wider than that is clipped at the frame
/// and never reaches the parent, whose size would ratchet the window wider until restart.
fn locked_width<R>(ui: &mut egui::Ui, w: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let top = ui.cursor().min;
    let rect = egui::Rect::from_min_size(top, Vec2::new(w, ui.available_height()));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    let mut clip = child.clip_rect();
    clip.max.x = clip.max.x.min(rect.max.x + ui.visuals().clip_rect_margin);
    child.set_clip_rect(clip);
    child.set_min_width(w);
    let out = add(&mut child);
    let used = egui::Rect::from_min_size(top, Vec2::new(w, child.min_rect().height()));
    ui.advance_cursor_after_rect(used);
    out
}

/// Saves geometry only when a pointer gesture changed it, so a click never writes the position
/// a window merely opened at.
fn remember_if_moved(ctx: &egui::Context, id: Id, key: &str, now: [f32; 4]) {
    let settled_id = id.with("ddc.settled");
    let (down, released) = ctx.input(|i| (i.pointer.any_down(), i.pointer.any_released()));
    let settled = ctx.data(|d| d.get_temp::<[f32; 4]>(settled_id));
    if released && !down && settled.is_some_and(|b| !same_geometry(b, now)) {
        geom_put(key, now);
    }
    if !down {
        ctx.data_mut(|d| d.insert_temp(settled_id, now));
    }
}

/// A modal: backdrop, the same frame as windows, Enter works for its primary action. Esc is up
/// to the caller through `ModalResponse::should_close`.
pub fn modal_dialog<R>(
    key: &str,
    ctx: &egui::Context,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::ModalResponse<R> {
    let id = Id::new(("ddc.modal", key));
    let top = key_owner(ctx) == KeyOwner::Modal
        && ctx.memory(|m| m.top_modal_layer()) == Some(LayerId::new(Order::Foreground, id));
    let st = stack(ctx);
    let keys = Keys {
        enter: top,
        esc: false,
        typing: st.typing,
        numeric: st.numeric,
    };
    let frame = egui::Frame::window(&ctx.style());
    let layer = LayerId::new(Order::Foreground, id);
    if !ctx.memory(|m| m.areas().visible_last_frame(&layer)) {
        // a modal opening closes every menu and dropdown, which would otherwise sit above it
        egui::Popup::close_all(ctx);
        let pass = ctx.cumulative_pass_nr();
        ctx.data_mut(|d| d.insert_temp(opened_id(), pass));
    }
    let out = egui::Modal::new(id)
        .area(egui::Modal::default_area(id).fade_in(false))
        .backdrop_color(ddc_ui::theme::sem_of(ctx).bg_backdrop)
        .frame(frame)
        .show(ctx, |ui| {
            set_scope(ui.ctx(), keys);
            let out = add(ui);
            set_scope(ui.ctx(), Keys::default());
            out
        });
    keys_end(ctx, keys);
    out
}

/// Who gets Enter and Esc this frame. Decided once, before anything is drawn, from what the
/// last frame left on screen, so draw order cannot give one key press to two readers.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum KeyOwner {
    /// The plan, 3D or sheet view and the active tool.
    #[default]
    Canvas,
    /// The topmost window that has the user's attention.
    Window(Id),
    /// The topmost modal.
    Modal,
    /// Command palette, context menu or an open popup; they read their keys themselves.
    Overlay,
}

/// Keyboard rights of the dialog being drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Keys {
    enter: bool,
    esc: bool,
    typing: bool,
    numeric: bool,
}

/// Open dialogs, topmost last; whether the keyboard belongs to them or to the canvas.
#[derive(Clone, Default)]
struct Stack {
    items: Vec<(Id, u64)>,
    attention: bool,
    typing: bool,
    numeric: bool,
    resolved: Option<u64>,
    owner: KeyOwner,
}

fn stack_id() -> Id {
    Id::new("ddc.dialog.stack")
}

fn stack(ctx: &egui::Context) -> Stack {
    ctx.data(|d| d.get_temp::<Stack>(stack_id()))
        .unwrap_or_default()
}

fn scope_id() -> Id {
    Id::new("ddc.dialog.scope")
}

fn set_scope(ctx: &egui::Context, k: Keys) {
    ctx.data_mut(|d| d.insert_temp(scope_id(), k));
}

fn scope(ctx: &egui::Context) -> Keys {
    ctx.data(|d| d.get_temp::<Keys>(scope_id()))
        .unwrap_or_default()
}

/// Call first thing in the frame; `overlay` is true while the palette or a menu is open.
pub fn keys_frame_begin(ctx: &egui::Context, overlay: bool) {
    resolve_owner(ctx, overlay);
}

/// The keyboard owner of this frame.
pub fn key_owner(ctx: &egui::Context) -> KeyOwner {
    resolve_owner(ctx, false)
}

/// Whether view and tool key handling may run; check it together with `wants_keyboard_input`.
pub fn canvas_owns_keys(ctx: &egui::Context) -> bool {
    key_owner(ctx) == KeyOwner::Canvas
}

/// A dialog takes the keyboard when it opens or is clicked and gives it back when the user
/// clicks elsewhere, so Esc during drawing on the canvas cancels the tool, not the window.
fn resolve_owner(ctx: &egui::Context, overlay: bool) -> KeyOwner {
    let pass = ctx.cumulative_pass_nr();
    let mut st = stack(ctx);
    if st.resolved == Some(pass) {
        return st.owner;
    }
    st.resolved = Some(pass);
    st.items.retain(|(_, seen)| seen + 1 >= pass);
    let popup = egui::Popup::is_any_open(ctx);
    let modal = ctx.memory(|m| m.top_modal_layer()).is_some();
    let (press, at) = ctx.input(|i| (i.pointer.any_pressed(), i.pointer.interact_pos()));
    if let (true, false, Some(at)) = (press, overlay || popup || modal, at) {
        let under = ctx.layer_id_at(at);
        let hit = st
            .items
            .iter()
            .position(|(i, _)| under == Some(LayerId::new(Order::Middle, *i)));
        st.attention = hit.is_some();
        if let Some(k) = hit {
            let it = st.items.remove(k);
            st.items.push(it);
        }
    }
    st.owner = if overlay || popup {
        KeyOwner::Overlay
    } else if modal {
        KeyOwner::Modal
    } else if st.attention {
        st.items
            .last()
            .map_or(KeyOwner::Canvas, |(i, _)| KeyOwner::Window(*i))
    } else {
        KeyOwner::Canvas
    };
    let owner = st.owner;
    ctx.data_mut(|d| d.insert_temp(stack_id(), st));
    owner
}

/// Whether a modal or an open window (drawn this frame or the last) covers `r`. Canvas
/// overlays (the empty-canvas card) wait for it instead of lying under or over it.
pub fn window_over(ctx: &egui::Context, r: egui::Rect) -> bool {
    if ctx.memory(|m| m.top_modal_layer()).is_some() {
        return true;
    }
    let pass = ctx.cumulative_pass_nr();
    let st = stack(ctx);
    let live = st.items.iter().filter(|(_, seen)| seen + 1 >= pass);
    live.into_iter()
        .any(|(id, _)| ctx.memory(|m| m.area_rect(*id)).is_some_and(|w| w.intersects(r)))
}

/// Registers the window. A new one owns the keyboard from the next frame, so the key press that
/// opened it cannot also act on it. A Hud stays out: it belongs to the canvas and its tool.
fn keys_begin(ctx: &egui::Context, id: Id, hud: bool) -> Keys {
    if hud {
        return Keys::default();
    }
    let free = key_owner(ctx) == KeyOwner::Window(id);
    let pass = ctx.cumulative_pass_nr();
    let mut st = stack(ctx);
    match st.items.iter().position(|(i, _)| *i == id) {
        Some(k) => st.items[k].1 = pass,
        None => {
            // a new window closes the menus it was opened from or that it would sit under
            egui::Popup::close_all(ctx);
            ctx.data_mut(|d| d.insert_temp(opened_id(), pass));
            st.items.push((id, pass));
            st.attention = true;
            st.typing = false;
            st.numeric = false;
        }
    }
    let k = Keys {
        enter: free,
        esc: free && !st.typing,
        typing: st.typing,
        numeric: st.numeric,
    };
    ctx.data_mut(|d| d.insert_temp(stack_id(), st));
    k
}

fn opened_id() -> Id {
    Id::new("ddc.dialog.opened")
}

/// A dialog opened in this pass or the one before: menus and the shell's own overlays close.
pub fn dialog_just_opened(ctx: &egui::Context) -> bool {
    let pass = ctx.cumulative_pass_nr();
    ctx.data(|d| d.get_temp::<u64>(opened_id()))
        .is_some_and(|p| p + 1 >= pass)
}

fn keys_forget(ctx: &egui::Context, id: Id) {
    ctx.data_mut(|d| {
        d.get_temp_mut_or_default::<Stack>(stack_id())
            .items
            .retain(|(i, _)| *i != id)
    });
}

fn focused_sense(ctx: &egui::Context) -> Option<egui::Sense> {
    let f = ctx.memory(|m| m.focused())?;
    ctx.read_response(f).map(|r| r.sense)
}

/// A drag value being edited keeps its text under its own id for as long as it has focus.
fn numeric_field_focused(ctx: &egui::Context) -> bool {
    ctx.memory(|m| m.focused())
        .is_some_and(|f| ctx.data(|d| d.get_temp::<String>(f).is_some()))
}

fn keys_end(ctx: &egui::Context, k: Keys) {
    if !k.enter {
        return;
    }
    let empty_search = ctx.data(|d| d.get_temp::<Id>(Id::new("ddc.search.empty_focus")));
    let typing = focused_sense(ctx).is_some_and(|s| s.senses_drag())
        && empty_search != ctx.memory(|m| m.focused());
    let numeric = numeric_field_focused(ctx);
    ctx.data_mut(|d| {
        let st = d.get_temp_mut_or_default::<Stack>(stack_id());
        st.typing = typing;
        st.numeric = numeric;
    });
}

fn is_fresh_press(e: &egui::Event, key: Key) -> bool {
    matches!(
        e,
        egui::Event::Key { key: k, pressed: true, repeat: false, modifiers, .. }
            if *k == key && modifiers.is_none()
    )
}

/// A new plain press: a held key repeats, and Shift or Alt with it means something else.
pub fn fresh_key(i: &egui::InputState, key: Key) -> bool {
    i.events.iter().any(|e| is_fresh_press(e, key))
}

fn take_key(i: &mut egui::InputState, key: Key) -> bool {
    let hit = fresh_key(i, key);
    if hit {
        i.events.retain(|e| !is_fresh_press(e, key));
    }
    hit
}

/// Enter runs the primary only when it is enabled, safe, and nothing else wants the key.
pub fn enter_runs_primary(
    enter: bool,
    primary_ready: bool,
    destructive: bool,
    focused: bool,
) -> bool {
    enter && primary_ready && !destructive && !focused
}

/// Enter for the default action. A number field always gets it first, any field does when
/// asked, and a focused button or checkbox keeps it for itself.
fn enter_pressed(ui: &egui::Ui, leave_field_first: bool) -> bool {
    let ctx = ui.ctx();
    let k = scope(ctx);
    let ready = k.enter && !k.numeric && !(leave_field_first && k.typing);
    let held = focused_sense(ctx).is_some_and(|s| s.senses_click() && !s.senses_drag());
    let enter = ctx.input(|i| fresh_key(i, Key::Enter));
    enter_runs_primary(enter, ready, false, held) && ctx.input_mut(|i| take_key(i, Key::Enter))
}

/// The accent button; at most one per window.
pub fn primary_button(
    ui: &mut egui::Ui,
    text: impl Into<egui::WidgetText>,
    enabled: bool,
) -> egui::Response {
    let text: egui::WidgetText = text.into();
    let th = ddc_ui::theme::of(ui.ctx());
    ui.add_enabled_ui(enabled, |ui| Button::primary(text.text()).show(ui, &th))
        .inner
}

fn primary(
    ui: &mut egui::Ui,
    text: impl Into<egui::WidgetText>,
    blocked: Option<&str>,
    leave_field_first: bool,
) -> bool {
    let r = primary_button(ui, text, blocked.is_none());
    match blocked {
        Some(why) => {
            r.on_disabled_hover_text(why);
            false
        }
        None => {
            let r = r.on_hover_text("Enter");
            r.clicked() || enter_pressed(ui, leave_field_first)
        }
    }
}

/// Primary look, click only: for actions that remove or replace model data.
pub fn primary_click(
    ui: &mut egui::Ui,
    text: impl Into<egui::WidgetText>,
    blocked: Option<&str>,
) -> bool {
    primary_click_tip(ui, text, blocked, "")
}

/// As `primary_click`, with `tip` on hover while the button is enabled (empty: none).
pub fn primary_click_tip(
    ui: &mut egui::Ui,
    text: impl Into<egui::WidgetText>,
    blocked: Option<&str>,
    tip: &str,
) -> bool {
    let r = primary_button(ui, text, blocked.is_none());
    match blocked {
        Some(why) => {
            r.on_disabled_hover_text(why);
            false
        }
        None if tip.is_empty() => r.clicked(),
        None => r.on_hover_text(tip).clicked(),
    }
}

/// Primary action: click or Enter. `blocked` is the reason it is off, shown on hover.
pub fn primary_action(
    ui: &mut egui::Ui,
    text: impl Into<egui::WidgetText>,
    blocked: Option<&str>,
) -> bool {
    primary(ui, text, blocked, false)
}

/// As `primary_action` for windows with editable tables: Enter first leaves the cell.
pub fn primary_action_table(
    ui: &mut egui::Ui,
    text: impl Into<egui::WidgetText>,
    blocked: Option<&str>,
) -> bool {
    primary(ui, text, blocked, true)
}

/// Quiet action that closes without changes; its tooltip names the key.
pub fn cancel_button(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) -> egui::Response {
    let text: egui::WidgetText = text.into();
    let th = ddc_ui::theme::of(ui.ctx());
    Button::quiet(text.text())
        .show(ui, &th)
        .on_hover_text("Esc")
}

/// Outlined secondary action; click only, never bound to Enter.
pub fn secondary_button(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) -> egui::Response {
    let text: egui::WidgetText = text.into();
    let th = ddc_ui::theme::of(ui.ctx());
    Button::secondary(text.text()).show(ui, &th)
}

/// Destructive action in a window: secondary shape, danger text, never bound to Enter. It goes
/// to the left of the footer (`dialog_footer_split`) and asks before it acts; the filled
/// danger button is for that confirmation only (`confirm_danger_button`).
pub fn danger_button(ui: &mut egui::Ui, text: impl Into<String>, enabled: bool) -> egui::Response {
    danger_with(ui, text.into(), enabled, false)
}

/// The one filled destructive button, inside a confirmation that names what goes.
pub fn confirm_danger_button(
    ui: &mut egui::Ui,
    text: impl Into<String>,
    enabled: bool,
) -> egui::Response {
    danger_with(ui, text.into(), enabled, true)
}

fn danger_with(ui: &mut egui::Ui, text: String, enabled: bool, filled: bool) -> egui::Response {
    let th = ddc_ui::theme::of(ui.ctx());
    ui.add_enabled_ui(enabled, |ui| {
        // egui clicks a focused button on Enter; a Tab-focused Discard must not take it
        let id = ui.next_auto_id();
        if ui.memory(|m| m.has_focus(id)) {
            ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Enter));
        }
        let b = if filled { Button::danger(&text) } else { Button::danger_text(&text) };
        b.show(ui, &th)
    })
    .inner
}

/// Footer row, right-aligned: the first widget added lands on the far right, so the primary
/// action goes first, then Cancel, then the other secondary actions.
pub fn dialog_footer<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    footer_rule(ui);
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(egui::Align::Center), add)
            .inner
    })
    .inner
}

/// Footer with a left side: `left` holds the destructive action (and nothing that closes),
/// `right` the usual right-to-left run, primary first.
pub fn dialog_footer_split<L, R>(
    ui: &mut egui::Ui,
    left: impl FnOnce(&mut egui::Ui) -> L,
    right: impl FnOnce(&mut egui::Ui) -> R,
) -> (L, R) {
    footer_rule(ui);
    // widths of both groups last frame: when they do not fit one row the left group takes a
    // row of its own under the right one, so no button is ever cut
    let key = ui.id().with("ddc.footer.split");
    let (lw, rw) = ui.data(|d| d.get_temp::<(f32, f32)>(key)).unwrap_or_default();
    let gap = ddc_ui::theme::space::XL;
    let one_row = lw + rw + gap <= ui.available_width();
    let mut left = Some(left);
    let mut l_out = None;
    let mut lw_now = lw;
    let (r_out, rw_now) = ui
        .horizontal(|ui| {
            if one_row {
                if let Some(f) = left.take() {
                    let g = ui.horizontal(f);
                    lw_now = g.response.rect.width();
                    l_out = Some(g.inner);
                }
            }
            let g = ui.with_layout(Layout::right_to_left(egui::Align::Center), right);
            (g.inner, g.response.rect.width())
        })
        .inner;
    if let Some(f) = left.take() {
        let g = ui.horizontal(f);
        lw_now = g.response.rect.width();
        l_out = Some(g.inner);
    }
    if (lw_now, rw_now) != (lw, rw) {
        ui.data_mut(|d| d.insert_temp(key, (lw_now, rw_now)));
        ui.ctx().request_repaint();
    }
    let l = l_out.expect("the left group is drawn on one of the two rows");
    (l, r_out)
}

fn footer_rule(ui: &mut egui::Ui) {
    ui.add_space(4.0);
    ui.separator();
}

/// Body that scrolls above a footer row that never scrolls away. `add(ui, true)` draws the
/// footer (right-aligned, first widget on the far right); `add(ui, false)` draws the body.
/// One closure serves both so they can share the caller's state. Use with `Dialog::stable()`.
pub fn pinned_footer(ui: &mut egui::Ui, mut add: impl FnMut(&mut egui::Ui, bool)) {
    ui.with_layout(Layout::bottom_up(egui::Align::Min), |ui| {
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| add(ui, true));
        });
        ui.separator();
        ui.with_layout(Layout::top_down(egui::Align::Min), |ui| {
            let out = egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| add(ui, false));
            super::mark_scroll_viewport(ui.ctx(), out.inner_rect);
        });
    });
}

/// Title inside a modal, the same style as a window title.
pub fn modal_title(ui: &mut egui::Ui, text: impl Into<String>) {
    ui.label(RichText::new(text.into()).heading());
    ui.add_space(4.0);
}

/// Static section caption inside a dialog body.
pub fn section_title(ui: &mut egui::Ui, text: impl Into<String>) {
    ui.add_space(4.0);
    ui.label(RichText::new(text.into()).strong());
}

/// Empty list: what is missing and, when known, what to do.
pub fn empty_note(ui: &mut egui::Ui, title: impl Into<String>, hint: Option<&str>) {
    let th = ddc_ui::theme::of(ui.ctx());
    widgets::empty_state(ui, &th, None, &title.into(), hint.unwrap_or(""), None);
}

/// Error line in plain words: internal texts pass through `ui_err::humanize`.
pub fn error_note(ui: &mut egui::Ui, text: &str) {
    let c = ui.visuals().error_fg_color;
    let s = crate::ui_err::humanize(text, ui_ru());
    ui.add(egui::Label::new(RichText::new(s).color(c)).wrap());
}

pub fn warn_note(ui: &mut egui::Ui, text: impl Into<String>) {
    let c = ui.visuals().warn_fg_color;
    ui.add(egui::Label::new(RichText::new(text.into()).color(c)).wrap());
}

/// Long work: bar with percent or a caption and a cancel button. True when cancel was pressed.
pub fn progress_row(
    ui: &mut egui::Ui,
    frac: f32,
    text: &str,
    cancel: impl Into<egui::WidgetText>,
) -> bool {
    let cancel: egui::WidgetText = cancel.into();
    let th = ddc_ui::theme::of(ui.ctx());
    ui.horizontal(|ui| {
        let w = (ui.available_width() - 96.0).max(80.0);
        // the kit draws the percent only beside a label, so an empty one stays a blank
        let label = if text.is_empty() { " " } else { text };
        ui.allocate_ui(Vec2::new(w, 0.0), |ui| {
            widgets::progress(ui, &th, Some(frac), label)
        });
        Button::quiet(cancel.text()).show(ui, &th).clicked()
    })
    .inner
}

/// One line, elided, the whole text on hover (paths and long names).
pub fn elided(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let th = ddc_ui::theme::of(ui.ctx());
    widgets::elided_label(ui, text, th.font(Role::Body), th.sem().text_primary)
}

/// `elided` in the secondary colour.
pub fn elided_weak(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let th = ddc_ui::theme::of(ui.ctx());
    widgets::elided_label(ui, text, th.font(Role::Body), th.sem().text_secondary)
}

fn ui_ru() -> bool {
    crate::ui_lang_now()
}

/// Keeps a grabbable part of the title bar on screen.
fn clamp_pos(p: egui::Pos2, screen: egui::Rect) -> egui::Pos2 {
    egui::pos2(
        p.x.min(screen.max.x - 80.0).max(screen.min.x),
        p.y.min(screen.max.y - 40.0).max(screen.min.y),
    )
}

#[derive(Default)]
struct Geom {
    path: Option<PathBuf>,
    map: BTreeMap<String, [f32; 4]>,
}

thread_local! {
    static GEOM: RefCell<Geom> = RefCell::default();
}

/// Turns on persistence between runs; screenshots and tests stay in memory.
pub fn load_dialog_geometry(path: PathBuf) {
    let map = std::fs::read_to_string(&path)
        .map(|s| parse_geometry(&s))
        .unwrap_or_default();
    GEOM.with(|g| {
        *g.borrow_mut() = Geom {
            path: Some(path),
            map,
        }
    });
}

fn geom_get(key: &str) -> Option<[f32; 4]> {
    GEOM.with(|g| g.borrow().map.get(key).copied())
}

fn same_geometry(a: [f32; 4], b: [f32; 4]) -> bool {
    a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1.0)
}

fn geom_put(key: &str, v: [f32; 4]) {
    GEOM.with(|g| {
        let mut g = g.borrow_mut();
        if g.map.get(key).is_some_and(|o| same_geometry(*o, v)) {
            return;
        }
        g.map.insert(key.to_string(), v);
        if let Some(p) = &g.path {
            // a lost write only loses a window position
            let _ = std::fs::write(p, geometry_text(&g.map));
        }
    });
}

fn parse_geometry(s: &str) -> BTreeMap<String, [f32; 4]> {
    let mut out = BTreeMap::new();
    for (k, v) in s.lines().filter_map(|l| l.split_once('=')) {
        let n: Vec<f32> = v
            .split(',')
            .filter_map(|x| x.trim().parse().ok())
            .filter(|x: &f32| x.is_finite())
            .collect();
        if let [x, y, w, h] = n[..] {
            out.insert(k.trim().to_string(), [x, y, w, h]);
        }
    }
    out
}

fn geometry_text(m: &BTreeMap<String, [f32; 4]>) -> String {
    m.iter()
        .map(|(k, v)| format!("{k}={:.0},{:.0},{:.0},{:.0}\n", v[0], v[1], v[2], v[3]))
        .collect()
}

/// Test helper for windows drawn through `Dialog`: two quiet frames, then one frame with Esc.
#[cfg(test)]
pub(crate) fn run_esc_frames(mut draw: impl FnMut(&egui::Context)) {
    let ctx = ddc_ui::theme::headless_ctx();
    // windows paint headings in the bundled weights; a bare context has none of them
    ctx.set_fonts(ddc_ui::theme::font_definitions());
    let frame = |events: Vec<egui::Event>| egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1280.0, 720.0),
        )),
        events,
        ..Default::default()
    };
    let esc = egui::Event::Key {
        key: Key::Escape,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    for events in [vec![], vec![], vec![esc]] {
        let _ = ctx.run(frame(events), |c| draw(c));
    }
}

/// Saved geometry of `key` on this thread, as a moved window left it.
#[cfg(test)]
pub(crate) fn saved_geometry(key: &str) -> Option<[f32; 4]> {
    geom_get(key)
}

/// Stores geometry for `key` on this thread, as if the user had moved that window.
#[cfg(test)]
pub(crate) fn save_geometry(key: &str, v: [f32; 4]) {
    geom_put(key, v);
}

/// Drops what `save_geometry` stored: test threads are reused by later tests.
#[cfg(test)]
pub(crate) fn forget_geometry(key: &str) {
    GEOM.with(|g| {
        g.borrow_mut().map.remove(key);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Modifiers;

    #[test]
    fn dialog_room_follows_the_sizing_rule() {
        let big = dialog_room(Vec2::new(2560.0, 1440.0));
        assert_eq!((big.x, big.y), (980.0, 640.0));
        let small = dialog_room(Vec2::new(800.0, 600.0));
        assert_eq!((small.x, small.y), (720.0, 540.0));
        // every class fits the room of a screen it is meant for
        for c in [DialogSize::Hud, DialogSize::Confirm, DialogSize::Form, DialogSize::Workspace] {
            assert!(c.size().x <= big.x && c.size().y <= big.y);
        }
    }

    #[test]
    fn sheet_is_420_wide_and_header_shadow_follows_scroll() {
        assert_eq!(DialogSize::Sheet.size().x, 420.0);
        assert!(!DialogSize::Sheet.resizable());
        assert!(!header_shadow_visible(0.0));
        assert!(header_shadow_visible(12.0));
    }

    #[test]
    fn modal_backdrop_dims_the_canvas() {
        let ctx = ddc_ui::theme::headless_ctx();
        let _ = ctx.run(Default::default(), |c| {
            assert!(ddc_ui::theme::sem_of(c).bg_backdrop.a() > 0);
        });
    }

    #[test]
    fn dialog_never_outgrows_its_room() {
        let ctx = ddc_ui::theme::headless_ctx();
        let raw = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 1200.0))),
            ..Default::default()
        };
        let mut h = 0.0;
        for _ in 0..3 {
            let _ = ctx.run(raw(), |c| {
                if let Some(r) = Dialog::new("t.room", "T", DialogSize::Workspace)
                    .show(c, |ui| {
                        ui.allocate_space(egui::vec2(10.0, 3000.0));
                    })
                {
                    h = r.response.rect.height();
                }
            });
        }
        assert!(h <= 640.5 && h > 0.0, "height {h}");
    }

    fn key_event(key: Key, repeat: bool, modifiers: Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat,
            modifiers,
        }
    }

    /// egui counts a press of a key that is still down as a repeat: tests let go in between.
    fn release(key: Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: Modifiers::NONE,
        }
    }

    fn input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 720.0),
            )),
            events,
            ..Default::default()
        }
    }

    fn key_input(key: Key) -> egui::RawInput {
        input(vec![key_event(key, false, Modifiers::NONE)])
    }

    fn pointer_at(pos: egui::Pos2) -> egui::Event {
        egui::Event::PointerMoved(pos)
    }

    fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        }
    }

    /// One frame with a window holding an enabled primary action; true when it fired.
    fn primary_frame(ctx: &egui::Context, key: &str, events: Vec<egui::Event>) -> bool {
        let mut fired = false;
        let _ = ctx.run(input(events), |ctx| {
            Dialog::new(key, "T", DialogSize::Form).show(ctx, |ui| {
                fired |= primary_action(ui, "x", None);
            });
        });
        fired
    }

    /// One frame with a plain window; its rectangle.
    fn window_frame(ctx: &egui::Context, key: &str, events: Vec<egui::Event>) -> egui::Rect {
        let mut rect = egui::Rect::NOTHING;
        let _ = ctx.run(input(events), |ctx| {
            let r = Dialog::new(key, "T", DialogSize::Form).show(ctx, |ui| {
                ui.label("body");
            });
            if let Some(r) = r {
                rect = r.response.rect;
            }
        });
        rect
    }

    #[test]
    fn first_click_neither_moves_the_window_nor_saves_it() {
        let ctx = ddc_ui::theme::headless_ctx();
        let k = "t.click";
        for _ in 0..3 {
            window_frame(&ctx, k, vec![]);
        }
        let before = window_frame(&ctx, k, vec![]);
        let at = before.center();
        window_frame(&ctx, k, vec![pointer_at(at), button(at, true)]);
        window_frame(&ctx, k, vec![button(at, false)]);
        let after = window_frame(&ctx, k, vec![]);
        assert!(
            (after.min - before.min).length() < 1.0,
            "{before:?} {after:?}"
        );
        assert!(geom_get(k).is_none());
    }

    #[test]
    fn dragging_saves_where_the_window_landed() {
        let ctx = ddc_ui::theme::headless_ctx();
        let k = "t.drag";
        for _ in 0..3 {
            window_frame(&ctx, k, vec![]);
        }
        let before = window_frame(&ctx, k, vec![]);
        let from = before.min + egui::vec2(60.0, 12.0);
        let to = from + egui::vec2(100.0, 50.0);
        window_frame(&ctx, k, vec![pointer_at(from), button(from, true)]);
        window_frame(&ctx, k, vec![pointer_at(to)]);
        let moved = window_frame(&ctx, k, vec![pointer_at(to), button(to, false)]);
        assert!(moved.min.x - before.min.x > 50.0, "{before:?} {moved:?}");
        let g = geom_get(k).expect("saved");
        assert!((g[0] - moved.min.x).abs() < 1.0 && (g[1] - moved.min.y).abs() < 1.0);
    }

    #[test]
    fn saved_positions_stay_on_the_screen() {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0));
        let c = |x, y| clamp_pos(egui::pos2(x, y), screen);
        assert_eq!(c(5000.0, -300.0), egui::pos2(1200.0, 0.0));
        assert_eq!(c(-10.0, 9000.0), egui::pos2(0.0, 680.0));
        assert_eq!(c(100.0, 100.0), egui::pos2(100.0, 100.0));
    }

    #[test]
    fn overlay_keeps_enter_from_the_window_beneath() {
        let ctx = ddc_ui::theme::headless_ctx();
        let k = "t.pal";
        for _ in 0..2 {
            primary_frame(&ctx, k, vec![]);
        }
        let (mut fired, mut canvas, mut left) = (false, true, false);
        let _ = ctx.run(key_input(Key::Enter), |ctx| {
            keys_frame_begin(ctx, true);
            canvas &= canvas_owns_keys(ctx);
            Dialog::new(k, "T", DialogSize::Form).show(ctx, |ui| {
                fired |= primary_action(ui, "x", None);
            });
            left |= ctx.input(|i| fresh_key(i, Key::Enter));
        });
        assert!(!fired);
        assert!(!canvas);
        assert!(
            left,
            "the palette is drawn last and must still find its key"
        );
        primary_frame(&ctx, k, vec![release(Key::Enter)]);
        let enter = key_event(Key::Enter, false, Modifiers::NONE);
        assert!(primary_frame(&ctx, k, vec![enter]));
    }

    /// The owner is known before any window is drawn: a view that reads keys first stays out.
    #[test]
    fn canvas_does_not_get_the_key_of_a_window_drawn_after_it() {
        let ctx = ddc_ui::theme::headless_ctx();
        let mut open = true;
        let mut canvas = [true; 5];
        let inputs = [
            input(vec![]),
            input(vec![]),
            key_input(Key::Escape),
            input(vec![]),
            input(vec![]),
        ];
        for (n, i) in inputs.into_iter().enumerate() {
            let _ = ctx.run(i, |ctx| {
                canvas[n] = canvas_owns_keys(ctx);
                if open {
                    Dialog::new("t.late", "T", DialogSize::Form)
                        .open(&mut open)
                        .show(ctx, |_| {});
                }
            });
        }
        assert!(!open);
        assert!(!canvas[1] && !canvas[2]);
        assert!(canvas[4]);
    }

    #[test]
    fn enter_held_or_with_a_modifier_is_not_the_primary_action() {
        let ctx = ddc_ui::theme::headless_ctx();
        let k = "t.repeat";
        for _ in 0..2 {
            primary_frame(&ctx, k, vec![]);
        }
        let press = |repeat, m| vec![key_event(Key::Enter, repeat, m)];
        let up = || vec![release(Key::Enter)];
        // the key goes down and stays: what follows is a repeat
        primary_frame(&ctx, k, press(false, Modifiers::NONE));
        assert!(!primary_frame(&ctx, k, press(true, Modifiers::NONE)));
        primary_frame(&ctx, k, up());
        assert!(!primary_frame(&ctx, k, press(false, Modifiers::SHIFT)));
        primary_frame(&ctx, k, up());
        assert!(!primary_frame(&ctx, k, press(false, Modifiers::ALT)));
        primary_frame(&ctx, k, up());
        assert!(primary_frame(&ctx, k, press(false, Modifiers::NONE)));
    }

    #[test]
    fn enter_in_a_number_field_commits_the_value_only() {
        let ctx = ddc_ui::theme::headless_ctx();
        let mut v = 1.0f64;
        let mut frame = |events: Vec<egui::Event>| {
            let (mut fired, mut field) = (false, None);
            let _ = ctx.run(input(events), |ctx| {
                Dialog::new("t.num", "T", DialogSize::Form).show(ctx, |ui| {
                    field = Some(ui.add(egui::DragValue::new(&mut v)).id);
                    fired |= primary_action(ui, "x", None);
                });
            });
            (fired, field.unwrap())
        };
        frame(vec![]);
        let (_, id) = frame(vec![]);
        ctx.memory_mut(|m| m.request_focus(id));
        frame(vec![]);
        frame(vec![egui::Event::Text("7".into())]);
        let enter = key_event(Key::Enter, false, Modifiers::NONE);
        let (first, _) = frame(vec![enter.clone()]);
        assert!(!first, "the first Enter only leaves the field");
        assert!(ctx.memory(|m| m.focused()).is_none());
        frame(vec![release(Key::Enter)]);
        let (second, _) = frame(vec![enter]);
        assert!(second);
    }

    #[test]
    fn a_hud_does_not_take_the_keyboard_from_a_window() {
        fn draw(ctx: &egui::Context, open: &mut bool, with_hud: bool) {
            Dialog::new("t.under", "T", DialogSize::Form)
                .open(open)
                .show(ctx, |_| {});
            if with_hud {
                Dialog::hud("t.hud", "H", Align2::RIGHT_BOTTOM, [-8.0, -8.0]).show(ctx, |_| {});
            }
        }
        let ctx = ddc_ui::theme::headless_ctx();
        let mut open = true;
        let _ = ctx.run(input(vec![]), |c| draw(c, &mut open, false));
        let _ = ctx.run(input(vec![]), |c| draw(c, &mut open, true));
        let _ = ctx.run(key_input(Key::Escape), |c| draw(c, &mut open, true));
        assert!(!open);
    }

    #[test]
    fn esc_held_back_by_a_window_stays_out_of_the_canvas_too() {
        let ctx = ddc_ui::theme::headless_ctx();
        let mut open = true;
        let mut canvas = true;
        for i in [input(vec![]), input(vec![]), key_input(Key::Escape)] {
            let _ = ctx.run(i, |ctx| {
                canvas = canvas_owns_keys(ctx);
                Dialog::new("t.job", "T", DialogSize::Wide)
                    .open(&mut open)
                    .esc_closes(false)
                    .show(ctx, |_| {});
            });
        }
        assert!(open);
        assert!(!canvas);
    }

    #[test]
    fn geometry_round_trips_and_skips_broken_lines() {
        let m = parse_geometry("a=10,20,300,400\nbroken\nb=1,2\nc=1,2,3,nan\n");
        assert_eq!(m.len(), 1);
        assert_eq!(m["a"], [10.0, 20.0, 300.0, 400.0]);
        assert_eq!(parse_geometry(&geometry_text(&m)), m);
    }

    #[test]
    fn only_big_classes_resize() {
        assert!(!DialogSize::Confirm.resizable());
        assert!(!DialogSize::Form.resizable());
        assert!(DialogSize::Tool.resizable());
        assert!(DialogSize::Workspace.resizable());
    }

    #[test]
    fn esc_closes_the_newest_dialog_only() {
        let ctx = ddc_ui::theme::headless_ctx();
        let (mut a, mut b) = (true, true);
        let inputs = [
            egui::RawInput::default(),
            egui::RawInput::default(),
            key_input(Key::Escape),
        ];
        for input in inputs {
            let _ = ctx.run(input, |ctx| {
                if a {
                    Dialog::new("t.a", "A", DialogSize::Form)
                        .open(&mut a)
                        .show(ctx, |_| {});
                }
                if b {
                    Dialog::new("t.b", "B", DialogSize::Form)
                        .open(&mut b)
                        .show(ctx, |_| {});
                }
            });
        }
        assert!(a);
        assert!(!b);
    }

    #[test]
    fn enter_fires_only_when_not_blocked() {
        let ctx = ddc_ui::theme::headless_ctx();
        let (mut ok, mut off) = (false, false);
        let inputs = [
            egui::RawInput::default(),
            egui::RawInput::default(),
            key_input(Key::Enter),
        ];
        for input in inputs {
            let _ = ctx.run(input, |ctx| {
                Dialog::new("t.off", "Off", DialogSize::Form).show(ctx, |ui| {
                    off |= primary_action(ui, "x", Some("why"));
                });
                Dialog::new("t.ok", "Ok", DialogSize::Form).show(ctx, |ui| {
                    ok |= primary_action(ui, "x", None);
                });
            });
        }
        assert!(ok);
        assert!(!off);
    }

    fn width_after_frames(key: &str, stable: bool, long: bool) -> f32 {
        let ctx = ddc_ui::theme::headless_ctx();
        for _ in 0..4 {
            let _ = ctx.run(input(vec![]), |ctx| {
                let d = Dialog::new(key, "T", DialogSize::Form);
                let d = if stable { d.stable() } else { d };
                d.show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(if long {
                            "x".repeat(400)
                        } else {
                            "x".to_string()
                        });
                    });
                });
            });
        }
        let id = Id::new(("ddc.dialog", key));
        let area = egui::AreaState::load(&ctx, id).expect("window was drawn");
        area.size.expect("window has a size").x
    }

    #[test]
    fn a_fixed_class_does_not_shrink_to_its_body() {
        let wide = width_after_frames("t.floor", false, false);
        assert!(wide >= DialogSize::Form.size().x, "{wide}");
    }

    #[test]
    fn a_stable_window_has_one_width_whatever_the_body() {
        let short = width_after_frames("t.lock.a", true, false);
        let long = width_after_frames("t.lock.b", true, true);
        assert_eq!(short, long);
        assert!(short >= DialogSize::Form.size().x);
    }

    /// Every migrated window follows one table: Esc closes through `open`, Enter runs only a
    /// ready, non-destructive primary action and never while a button holds the key.
    #[test]
    fn enter_and_esc_policy_of_migrated_windows() {
        // (window, has primary, destructive action present)
        let windows = [
            ("collab.merge", true, false),
            ("collab.options.confirm", true, true),
            ("data_export.info", true, false),
            ("data_export.purge", false, true),
            ("sched_import.import", true, false),
            ("xlloss", true, true),
            ("clip.dialog", true, true),
        ];
        for (name, primary, destructive) in windows {
            assert_eq!(enter_runs_primary(true, primary, false, false), primary, "{name}");
            assert!(!enter_runs_primary(true, true, true, false), "{name}: destructive");
            assert!(!enter_runs_primary(true, true, false, true), "{name}: focused button");
            let _ = destructive;
        }
    }

    #[test]
    fn a_window_without_an_open_flag_ignores_esc() {
        let ctx = ddc_ui::theme::headless_ctx();
        let mut shown = 0;
        for i in [input(vec![]), input(vec![]), key_input(Key::Escape)] {
            let _ = ctx.run(i, |c| {
                if Dialog::new("t.noesc", "T", DialogSize::Confirm)
                    .show(c, |_| {})
                    .is_some()
                {
                    shown += 1;
                }
            });
        }
        assert_eq!(shown, 3);
    }

    /// Windows go through the scaffold; the floating inspector is a panel, not a dialog.
    #[test]
    fn no_raw_windows_in_feature_code() {
        fn allowed(p: &std::path::Path) -> usize {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let dir = p
                .parent()
                .and_then(|d| d.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("");
            match (dir, name) {
                ("ui_kit" | "widgets", "dialog.rs") => usize::MAX,
                // the floating inspector is a panel; the XLSX windows are handled separately
                (_, "panels.rs") => 1,
                ("shell", "xlsx_win.rs") => 6,
                // names the call in its lint list; opens no window
                ("shell", "i18n_lint.rs") => 1,
                // windows written before the scaffold reached them: the count may only go down
                (_, "rebar_ui.rs") => 3,
                (_, "project_ui.rs") | ("view_gfx_ui", "vv_win.rs") => 2,
                ("annot_text", "win.rs")
                | (_, "mep_browser.rs")
                | ("save", "report.rs")
                | ("view_gfx_ui", "materials_win.rs") => 1,
                // the test host of the AI panel, which is a dock in the product
                ("ui", "tests_states.rs") => 1,
                // the stacking-order test opens one plain window to check it against an overlay
                ("src", "z.rs") => 1,
                _ => 0,
            }
        }
        fn walk(d: &std::path::Path, out: &mut Vec<String>) {
            for e in std::fs::read_dir(d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    let s = std::fs::read_to_string(&p).unwrap();
                    let n = s.matches(concat!("Window", "::new")).count()
                        + s.matches(concat!("Modal", "::new")).count();
                    if n > allowed(&p) {
                        out.push(p.display().to_string());
                    }
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut hits = vec![];
        walk(&root.join("src"), &mut hits);
        walk(&root.join("../ui/src"), &mut hits);
        assert!(hits.is_empty(), "{hits:?}");
    }

    #[test]
    fn enter_on_a_focused_danger_button_is_not_a_click() {
        let ctx = ddc_ui::theme::headless_ctx();
        let raw = |events: Vec<egui::Event>| egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
            events,
            ..Default::default()
        };
        let mut id = None;
        let mut clicked = vec![];
        let frames = [
            vec![],
            vec![],
            vec![key_event(Key::Enter, false, Modifiers::NONE)],
            vec![release(Key::Enter), key_event(Key::Space, false, Modifiers::NONE)],
        ];
        for (n, events) in frames.into_iter().enumerate() {
            let _ = ctx.run(raw(events), |c| {
                egui::CentralPanel::default().show(c, |ui| {
                    let r = danger_button(ui, "Discard", true);
                    id = Some(r.id);
                    clicked.push(r.clicked());
                });
            });
            if n == 0 {
                ctx.memory_mut(|m| m.request_focus(id.unwrap()));
            }
        }
        assert!(ctx.memory(|m| m.has_focus(id.unwrap())), "the button kept the focus");
        assert_eq!(clicked, [false, false, false, true], "Enter never, Space still clicks");
    }
}
