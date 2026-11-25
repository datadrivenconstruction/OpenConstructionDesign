// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! 3D viewport: input (orbit LMB, pan MMB/Shift, wheel zoom toward the cursor, F or double MMB:
//! zoom to fit, click: select), orientation cube in the corner, drawing via a paint callback.

use ddc_render::glam::Vec3;
use ddc_render::{Aabb, FrameUniforms, OrbitCamera, Quality, Scene};
#[cfg(feature = "gui")]
use ddc_render::Renderer;
use egui::{self, PointerButton, Pos2, Sense, Stroke, Vec2};
#[cfg(feature = "gui")]
use eframe::egui_wgpu;

pub struct Viewport {
    pub camera: OrbitCamera,
    pub selected: Option<ddc_render::Hit>,
    /// Ray of the last click (origin, direction): elements without a body are picked by it.
    pub click_ray: Option<([f32; 3], [f32; 3])>,
    /// Highlighted elements [start, end); the editor sets them from the shared selection every frame.
    pub highlight: Option<(u32, u32)>,
    /// The whole selection as merged ranges (`sel3d`); `highlight` is its first item.
    pub sel_set: std::sync::Arc<[(u32, u32)]>,
    /// Show-as-built frame (`reveal::Player::current`): change counter and per-element state.
    pub reveal: Option<(u64, std::sync::Arc<crate::reveal::Frame>)>,
    /// Playback scale and frame: the camera smoothly keeps the batch being worked on in view.
    pub follow: Option<(std::sync::Arc<ddc_render::reveal::Timeline>, u32)>,
    pub sel_cache: crate::sel3d::SelCache,
    /// Element under the cursor and its highlight range (set by the editor: a panel or the whole wall).
    pub hovered: Option<u32>,
    pub hover_range: Option<(u32, u32)>,
    /// Model point under the cursor (a face or the ground plane), for the status bar.
    pub cursor: Option<Vec3>,
    /// Selection color (sRGB), shared with the plan.
    pub accent: [u8; 3],
    /// Background: vertical gradient (top, bottom).
    pub bg: [egui::Color32; 2],
    /// Dark theme: grid, edges and orientation cube use the dark palette.
    pub dark: bool,
    /// Performance level, adaptive hold during motion, fallback state.
    pub gfx: crate::gfx_ui::GfxState,
    pub frame_ms: f32,
    /// Section box: everything outside it is hidden; faces move with handles.
    pub section_box: Option<Aabb>,
    /// Temporary section plane: the box it is made of (`view3d::section_plane`).
    pub section_plane: Option<crate::view3d::section_plane::Cut>,
    /// Gesture and undo log of the section box face handles (`view3d::boxdrag`).
    pub boxed: crate::view3d::boxdrag::BoxEditor,
    /// LMB drag is handed to the tool (push-pull): offset per gesture, release.
    pub capture: bool,
    /// A tool owns the pointer: the empty-view card must not take its clicks.
    pub modelling: bool,
    pub grab: Option<Vec2>,
    pub released: bool,
    /// Cursor over the view in the last frame (ray for tools).
    pub pointer: Option<Pos2>,
    /// Rectangle and aspect of the last frame (point projection for tools).
    frame: Option<(egui::Rect, f32)>,
    /// Window rect of the last frame: a canvas change in the same window is the chrome's.
    screen: Option<egui::Rect>,
    /// The chrome is folding: a moved canvas keeps the model under the same pixel.
    pub follow_chrome: bool,
    /// Renderer slot of this view: each 3D pane keeps its own frame targets.
    pub slot: u32,
    fitted: bool,
    /// Navigation and 3D view: cube, transitions, walkthrough, style, category rules (`view3d`).
    pub nav: crate::view3d::Nav,
    /// Scene index for ray picking (`spatial`); if it does not match the scene, falls back to brute force.
    pub index: Option<std::sync::Arc<crate::spatial::Index>>,
    /// Direct editing in 3D (`edit3d`, including the 3D handles); like push-pull,
    /// it is given the left button via `capture`.
    pub e3: crate::edit3d::Edit3d,
    /// Cursor over the orientation cube, section box handles or overlays (this frame).
    pub overlay_hover: bool,
    /// Timed frames while the picture still refines with no input (`view3d::settle`).
    pub settle: crate::view3d::settle::Settle,
}

/// The canvas moved from `r0` to `r1` in the same window: the camera follows so that what is
/// at the target's depth stays at the same pixel and the same size.
pub fn keep_on_screen(cam: &mut OrbitCamera, r0: egui::Rect, r1: egui::Rect) {
    if r0 == r1 || r0.height() < 1.0 || r1.height() < 1.0 {
        return;
    }
    cam.distance *= r1.height() / r0.height();
    let d = r1.center() - r0.center();
    cam.pan(-d.x, -d.y, r1.height());
}

impl Default for Viewport {
    fn default() -> Self {
        Self::new()
    }
}

impl Viewport {
    pub fn new() -> Self {
        Self {
            camera: OrbitCamera::default(),
            selected: None,
            click_ray: None,
            highlight: None,
            sel_set: std::sync::Arc::from(Vec::new()),
            reveal: None,
            follow: None,
            sel_cache: Default::default(),
            hovered: None,
            hover_range: None,
            cursor: None,
            accent: {
                let a = ddc_ui::theme::sem(false).accent;
                [a.r(), a.g(), a.b()]
            },
            bg: [
                ddc_ui::theme::sem(false).canvas_bg_top,
                ddc_ui::theme::sem(false).canvas_bg_bottom,
            ],
            dark: false,
            gfx: Default::default(),
            frame_ms: 0.0,
            section_box: None,
            section_plane: None,
            boxed: Default::default(),
            capture: false,
            modelling: false,
            grab: None,
            released: false,
            pointer: None,
            frame: None,
            screen: None,
            follow_chrome: false,
            slot: 0,
            fitted: false,
            nav: Default::default(),
            index: None,
            e3: Default::default(),
            overlay_hover: false,
            settle: Default::default(),
        }
    }

    /// Another 3D view of the same model: this camera, style and section box, renderer `slot`.
    pub fn twin(&self, slot: u32) -> Self {
        let mut v = Self::new();
        v.camera = self.camera.clone();
        v.nav.style = self.nav.style;
        v.section_box = self.section_box;
        v.section_plane = self.section_plane;
        v.fitted = self.fitted;
        v.slot = slot;
        v
    }

    /// Selection, hover and theme are the editor's: a second view shows them like the first.
    pub fn share_from(&mut self, main: &Viewport) {
        self.highlight = main.highlight;
        self.sel_set = main.sel_set.clone();
        self.reveal = main.reveal.clone();
        self.hovered = main.hovered;
        self.hover_range = main.hover_range;
        self.accent = main.accent;
        self.bg = main.bg;
        self.dark = main.dark;
    }

    /// Zoom to fit on the next frame (after a scene change).
    pub fn refit(&mut self) {
        self.fitted = false;
    }

    /// "Show all" is due on the next frame.
    pub fn needs_fit(&self) -> bool {
        !self.fitted
    }

    /// Cancel "zoom to fit" after a scene change (model edit, restored view).
    pub fn keep_camera(&mut self) {
        self.fitted = true;
    }

    /// Section box of the 3D view (picking and tools ignore what it cuts off).
    pub fn section_box(&self) -> Option<Aabb> {
        self.section_box
    }

    /// Draws the scene; on click returns `Some(element or None for empty space)`.
    pub fn show(&mut self, ui: &mut egui::Ui, scene: &Scene) -> Option<Option<u32>> {
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        crate::shell::pane_focus::land_canvas(ui.ctx(), &resp);
        let screen = ui.ctx().screen_rect();
        let same_window = self.screen.replace(screen) == Some(screen);
        let follow = same_window && self.fitted && self.follow_chrome;
        if let (true, Some((r0, _))) = (follow, self.frame) {
            keep_on_screen(&mut self.camera, r0, rect);
        }
        let aspect = rect.width() / rect.height().max(1.0);
        let ndc = |p: egui::Pos2| {
            (
                (p.x - rect.min.x) / rect.width() * 2.0 - 1.0,
                1.0 - (p.y - rect.min.y) / rect.height() * 2.0,
            )
        };
        self.nav.begin(ui, &resp, &mut self.camera, aspect);
        if let Some((tl, f)) = self.follow.take() {
            let b = tl.active_box(scene, f);
            self.camera = ddc_render::reveal_export::follow_clear(&b, &self.camera, aspect, self.section_box.as_ref()).cam;
        }
        let fit = (resp.hovered() && ui.input(|i| i.key_pressed(egui::Key::F)))
            || resp.double_clicked_by(PointerButton::Middle);
        if !self.fitted && rect.width() > 1.0 {
            if let Some(b) = scene.focus.or(scene.bounds) {
                self.camera.fit(&b, aspect);
                self.fitted = true;
            }
        } else if fit && self.nav.walk.is_none() {
            let sel = self.highlight.and_then(|r| scene.bounds_of(r));
            let b = crate::view3d::Nav::frame_target(sel, scene.focus, scene.bounds, self.section_box);
            self.nav.fit_animated(&mut self.camera, b, aspect);
        }
        self.frame = Some((rect, aspect));
        self.pointer = ui.input(|i| i.pointer.latest_pos());
        self.nav
            .walk_input(ui, &resp, &mut self.camera, scene.bounds.as_ref());
        let shift = ui.input(|i| i.modifiers.shift);
        let d = resp.drag_delta();
        self.released = self.grab.is_some() && resp.drag_stopped();
        if !self.capture {
            self.grab = None;
        }
        if self.capture && !shift && resp.dragged_by(PointerButton::Primary) {
            *self.grab.get_or_insert(Vec2::ZERO) += resp.drag_delta();
        } else if resp.dragged_by(PointerButton::Middle)
            || (shift && resp.dragged_by(PointerButton::Primary) && !self.capture)
        {
            self.camera.pan(d.x, d.y, rect.height());
        } else if resp.dragged_by(PointerButton::Primary) && !self.capture {
            let sel = self.highlight.and_then(|r| scene.bounds_of(r));
            let hit = self.nav.hover_hit;
            self.nav.orbit_drag(&mut self.camera, d, hit, sel);
        }
        if resp.hovered() || resp.dragged() {
            let drag = crate::nav_cursor::drag_of(
                resp.dragged_by(PointerButton::Primary),
                resp.dragged_by(PointerButton::Middle),
                shift,
                self.capture,
            );
            if let Some(i) = crate::nav_cursor::cursor_for(drag, shift, self.capture, false) {
                ui.ctx().set_cursor_icon(i);
            }
        }
        self.hovered = None;
        let index = self.index.clone();
        let cov = self.nav.coverage_table();
        let section = self.section_box();
        let accept = |h: &ddc_render::Hit| crate::view3d::pickable(&cov, scene, section, h);
        let pick =
            |r: &ddc_render::Ray| crate::spatial::pick_where(index.as_deref(), scene, r, &accept);
        // no ray needed when orbiting a large scene: the pivot is the gesture start point
        let skip_hover = resp.dragged() && crate::spatial::is_big(scene);
        let mut hover_hit = if skip_hover { self.nav.hover_hit } else { None };
        self.cursor = resp.hover_pos().and_then(|pos| {
            let (x, y) = ndc(pos);
            let ray = self.camera.ray(x, y, aspect);
            let hit = if skip_hover { None } else { pick(&ray) };
            self.hovered = hit.map(|h| h.element);
            if !skip_hover {
                hover_hit = hit.map(|h| h.point);
            }
            hit.map(|h| h.point).or_else(|| {
                (ray.dir.z.abs() > 1e-6)
                    .then(|| -ray.origin.z / ray.dir.z)
                    .filter(|t| *t > 0.0)
                    .map(|t| ray.at(t))
            })
        });
        self.nav.hover_hit = hover_hit;
        if let Some(pos) = resp.hover_pos() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                let (x, y) = ndc(pos);
                let ray = self.camera.ray(x, y, aspect);
                let p = pick(&ray)
                    .map(|h| h.point)
                    .unwrap_or_else(|| self.camera.focus_point(&ray));
                self.camera.zoom_about(p, (-scroll * 0.0025).exp());
            }
        }
        let mut clicked = None;
        if resp.clicked() && !self.capture {
            if let Some(pos) = resp.interact_pointer_pos() {
                let (x, y) = ndc(pos);
                let ray = self.camera.ray(x, y, aspect);
                self.click_ray = Some((
                    [ray.origin[0], ray.origin[1], ray.origin[2]],
                    [ray.dir[0], ray.dir[1], ray.dir[2]],
                ));
                self.selected = crate::spatial::pick_click(index.as_deref(), scene, &ray, &accept);
                let el = self.selected.map(|h| h.element);
                self.highlight_one(el.map(|e| (e, e + 1)));
                clicked = Some(el);
            }
        }
        let sbox = crate::view3d::boxdrag::gizmo(self, ui, rect, aspect, scene);
        if sbox {
            clicked = None;
        }
        let dt_ms = ui.input(|i| i.unstable_dt) * 1000.0;
        self.track_frame_time(dt_ms, resp.dragged());
        let ov = crate::overlay::pal(self.dark);
        let u = FrameUniforms::new(&self.camera, aspect, None)
            .with_ranges(
                crate::sel3d::shader_range(self.highlight, &self.sel_set),
                self.hover_range,
            )
            .with_accent(crate::overlay::rgb(ov.selection))
            .with_hover(crate::overlay::rgb(ov.hover))
            .with_theme(self.dark)
            .with_quality(self.nav.quality(Quality::High))
            .with_clip(self.section_box)
            .with_style(self.nav.style)
            .with_categories(&cov)
            .with_caps_hidden(self.nav.deco.caps_stale);
        // one-off camera jump (saved view): without a follow-up frame the motion budgets would linger
        let jumped = self.nav.perf.camera_jumped(u.view_proj);
        if jumped && ddc_render::perf::published().big {
            ui.ctx().request_repaint();
        }
        let fx = self
            .gfx
            .frame(dt_ms, jumped || resp.dragged(), self.nav.style);
        // a running fill wakes the view itself; refinement gets bounded timed frames
        self.nav.deco.wake = Some(ui.ctx().clone());
        let stats = ddc_render::perf::published();
        let input = jumped || resp.dragged() || ui.input(|i| !i.events.is_empty());
        let wants = crate::view3d::settle::Wants {
            quality: self.gfx.needs_repaint(),
            refine: stats.polling,
        };
        let now = ui.input(|i| i.time);
        let progress = stats.uploaded_bytes > 0;
        if let Some(d) = self
            .settle
            .next(now, input, scene.serial(), progress, wants)
        {
            ui.ctx().request_repaint_after(d);
        }
        let ppp = ui.ctx().pixels_per_point();
        let px = (
            (rect.width() * ppp).round() as u32,
            (rect.height() * ppp).round() as u32,
        );
        let [top, bottom] = self.bg;
        let bg = crate::overlay::sky_ground(rect, self.horizon_y(rect, aspect), top, bottom);
        ui.painter().add(bg);
        // the 3D pass itself; a headless build has no GPU to run it on
        #[cfg(feature = "gui")]
        ui.painter()
            .add(egui_wgpu::Callback::new_paint_callback(
                rect,
                Draw(u, px, fx, self.sel_set.clone(), self.slot, self.reveal.clone()),
            ));
        #[cfg(not(feature = "gui"))]
        let _ = (u, px, fx);
        if let Some(b) = self.section_box {
            self.paint_box(ui, rect, aspect, &b);
        }
        let cube = self.view_cube(ui, rect, scene, aspect);
        if cube {
            clicked = None;
        }
        let elevation = self.nav.elevation.unwrap_or(0.0);
        let over = crate::view3d::overlay::show(self, ui, rect, scene, aspect, elevation);
        if over {
            clicked = None;
        }
        self.overlay_hover = sbox || cube || over;
        clicked
    }

    /// One highlighted range (a click, a tool pick) instead of the whole selection set.
    pub fn highlight_one(&mut self, r: Option<(u32, u32)>) {
        self.highlight = r;
        self.sel_set = r.into_iter().collect::<Vec<_>>().into();
    }

    /// The whole selection: `highlight` is its first item, `set` all of it (merged).
    pub fn highlight_set(&mut self, first: Option<(u32, u32)>, set: std::sync::Arc<[(u32, u32)]>) {
        self.highlight = first;
        self.sel_set = set;
    }

    /// 3D view rectangle in the last frame.
    pub fn frame_rect(&self) -> Option<egui::Rect> {
        self.frame.map(|f| f.0)
    }

    /// Screen position of a model point in the last frame.
    pub fn screen_of(&self, p: Vec3) -> Option<Pos2> {
        let (r, a) = self.frame?;
        self.to_screen(r, a, p)
    }

    /// Camera ray through a screen point of the last frame.
    pub fn ray_at(&self, p: Pos2) -> Option<ddc_render::Ray> {
        let (r, a) = self.frame?;
        if r.width() < 1.0 || r.height() < 1.0 {
            return None;
        }
        let x = (p.x - r.min.x) / r.width() * 2.0 - 1.0;
        let y = 1.0 - (p.y - r.min.y) / r.height() * 2.0;
        Some(self.camera.ray(x, y, a))
    }

    /// Smoothed frame time during orbit or pan, when frames come one after another.
    fn track_frame_time(&mut self, dt_ms: f32, active: bool) {
        if !active || dt_ms < 1.0 {
            return;
        }
        // the first frame after idle says nothing: a long sample is capped
        let sample = dt_ms.min(200.0);
        self.frame_ms = if self.frame_ms == 0.0 {
            sample
        } else {
            self.frame_ms * 0.9 + sample * 0.1
        };
    }

    /// Toggle the section box: fitted to bounds `b` (selection or whole model) with a margin.
    pub fn toggle_section_box(&mut self, b: Option<Aabb>) {
        let pad = Vec3::splat(0.3);
        self.section_box = match self.section_box {
            Some(_) => None,
            None => b.map(|b| Aabb {
                min: b.min - pad,
                max: b.max + pad,
            }),
        };
    }

    /// Handles at face centers: (point, axis, top face).
    fn box_handles(b: &Aabb) -> [(Vec3, usize, bool); 6] {
        let c = (b.min + b.max) * 0.5;
        let mut out = [(c, 0, false); 6];
        for axis in 0..3 {
            for (k, hi) in [false, true].into_iter().enumerate() {
                let mut p = c;
                p[axis] = if hi { b.max[axis] } else { b.min[axis] };
                out[axis * 2 + k] = (p, axis, hi);
            }
        }
        out
    }

    fn to_screen(&self, rect: egui::Rect, aspect: f32, p: Vec3) -> Option<Pos2> {
        let n = self.camera.project(p, aspect);
        (n.z > 0.0 && n.z < 1.0).then(|| {
            Pos2::new(
                rect.min.x + (n.x + 1.0) * 0.5 * rect.width(),
                rect.min.y + (1.0 - n.y) * 0.5 * rect.height(),
            )
        })
    }

    /// Screen height of the horizon; there is none in ortho or when looking straight down,
    /// and looking straight up it lies below the frame.
    fn horizon_y(&self, rect: egui::Rect, aspect: f32) -> Option<f32> {
        if self.camera.ortho {
            return None;
        }
        let back = self.camera.back();
        let h = Vec3::new(-back.x, -back.y, 0.0);
        if h.length_squared() < 1e-4 {
            return (back.z < 0.0).then_some(rect.bottom());
        }
        let far = self.camera.eye() + h.normalize() * self.camera.distance.max(1.0) * 1e3;
        let n = self.camera.project(far, aspect);
        n.y.is_finite()
            .then(|| rect.min.y + (1.0 - n.y) * 0.5 * rect.height())
    }

    fn paint_box(&self, ui: &egui::Ui, rect: egui::Rect, aspect: f32, b: &Aabb) {
        let p = ui.painter_at(rect);
        let ov = crate::overlay::pal(self.dark);
        let accent = ov.selection;
        let corner = |i: usize| {
            Vec3::new(
                if i & 1 == 0 { b.min.x } else { b.max.x },
                if i & 2 == 0 { b.min.y } else { b.max.y },
                if i & 4 == 0 { b.min.z } else { b.max.z },
            )
        };
        let line = Stroke::new(1.2, accent.gamma_multiply(0.8));
        // a section plane draws its own face only
        let plane = crate::view3d::section_plane::active(self);
        let on_plane = |i: usize| plane.is_none_or(|f| (((i >> f.axis) & 1) == 1) == f.hi);
        for i in 0..8usize {
            for bit in [1usize, 2, 4] {
                if i & bit != 0 || !on_plane(i) || !on_plane(i | bit) {
                    continue;
                }
                let a = self.to_screen(rect, aspect, corner(i));
                let c = self.to_screen(rect, aspect, corner(i | bit));
                if let (Some(a), Some(c)) = (a, c) {
                    p.line_segment([a, c], line);
                }
            }
        }
        for (q, axis, hi) in Self::box_handles(b) {
            if plane.is_some_and(|f| f.axis != axis || f.hi != hi) {
                continue;
            }
            let Some(c) = self.to_screen(rect, aspect, q) else {
                continue;
            };
            // arrows along the face normal on screen, constant size
            let mut step = q;
            step[axis] += (b.max[axis] - b.min[axis]).abs().max(1e-3) * 0.1;
            let dir = self
                .to_screen(rect, aspect, step)
                .map_or(egui::Vec2::ZERO, |e| e - c);
            let hot = self.boxed.is_hot(crate::view3d::boxdrag::Face { axis, hi });
            crate::overlay::grip(&p, c, crate::overlay::Grip::Arrows(dir), hot, ov);
        }
    }

    /// View cube: faces, edges, corners, dragging, home button, projection (`view3d::cube`).
    fn view_cube(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        scene: &Scene,
        aspect: f32,
    ) -> bool {
        crate::view3d::cube::show(self, ui, rect, scene, aspect)
    }
}

#[cfg(feature = "gui")]
struct Draw(
    FrameUniforms,
    (u32, u32),
    Option<ddc_render::FrameFx>,
    std::sync::Arc<[(u32, u32)]>,
    u32,
    Option<(u64, std::sync::Arc<crate::reveal::Frame>)>,
);

#[cfg(feature = "gui")]
impl egui_wgpu::CallbackTrait for Draw {
    fn prepare(
        &self,
        device: &ddc_render::wgpu::Device,
        queue: &ddc_render::wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        encoder: &mut ddc_render::wgpu::CommandEncoder,
        res: &mut egui_wgpu::CallbackResources,
    ) -> Vec<ddc_render::wgpu::CommandBuffer> {
        if let Some(r) = res.get_mut::<Renderer>() {
            r.select_view(device, self.4);
            r.set_fx(self.2);
            r.set_selection(self.3.clone());
            match &self.5 {
                Some((rev, f)) => r.set_reveal_rev(*rev, Some((&f.anim[..], &f.tint[..]))),
                None => r.set_reveal_rev(0, None),
            }
            if r.view_count() > 1 {
                // buffer writes made for this camera (sorting, impostors) land before the next
                // view's writes only with a submit of its own
                let mut own = device.create_command_encoder(&Default::default());
                r.prepare(device, queue, &mut own, &self.0, self.1);
                queue.submit(Some(own.finish()));
            } else {
                r.prepare(device, queue, encoder, &self.0, self.1);
            }
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        pass: &mut ddc_render::wgpu::RenderPass<'static>,
        res: &egui_wgpu::CallbackResources,
    ) {
        if let Some(r) = res.get::<Renderer>() {
            r.draw_view(pass, self.4);
        }
    }
}

pub(crate) fn inside_convex(p: Pos2, pts: &[Pos2]) -> bool {
    let mut sign = 0.0f32;
    for i in 0..pts.len() {
        let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
        let c = (b - a).x * (p - a).y - (b - a).y * (p - a).x;
        if c.abs() < 1e-6 {
            continue;
        }
        if sign == 0.0 {
            sign = c.signum();
        } else if sign != c.signum() {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_moved_canvas_keeps_the_model_under_the_same_pixel() {
        for ortho in [false, true] {
            let mut cam = OrbitCamera {
                ortho,
                ..Default::default()
            };
            let r0 = egui::Rect::from_min_size(Pos2::new(240.0, 160.0), Vec2::new(1100.0, 700.0));
            let r1 = egui::Rect::from_min_size(Pos2::new(32.0, 40.0), Vec2::new(1500.0, 820.0));
            let px = |cam: &OrbitCamera, r: egui::Rect, p: Vec3| {
                let n = cam.project(p, r.width() / r.height());
                Pos2::new(
                    r.min.x + (n.x + 1.0) / 2.0 * r.width(),
                    r.min.y + (1.0 - n.y) / 2.0 * r.height(),
                )
            };
            // points at the target's depth, where the pointer works
            let pts = [
                cam.target,
                cam.target + cam.right_axis() * 3.0 - cam.up() * 1.5,
            ];
            let before: Vec<Pos2> = pts.iter().map(|p| px(&cam, r0, *p)).collect();
            keep_on_screen(&mut cam, r0, r1);
            for (p, b) in pts.iter().zip(before) {
                let a = px(&cam, r1, *p);
                assert!((a - b).length() < 0.05, "ortho {ortho}: {b:?} -> {a:?}");
            }
        }
    }

    #[test]
    fn convex_inside() {
        let sq = [
            Pos2::new(0., 0.),
            Pos2::new(1., 0.),
            Pos2::new(1., 1.),
            Pos2::new(0., 1.),
        ];
        assert!(inside_convex(Pos2::new(0.5, 0.5), &sq));
        assert!(!inside_convex(Pos2::new(1.5, 0.5), &sq));
    }
}
