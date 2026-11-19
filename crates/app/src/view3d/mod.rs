//! 3D views and navigation: the view cube (faces, edges, corners, drag, home), orbit around
//! the selection or the point under the cursor, smooth transitions, perspective/ortho, walk
//! and fly (WASD), the section box (handles, numbers, to selection, to level) with section
//! fills by material, visual styles, category rules (visibility, transparency), temporary
//! hide/isolate of categories, saved 3D views (`view_def` + `prop`, see ddc-model).
//! State is [`Nav`] in the `Viewport`; [`sync`] decorates the scene after the base build.

pub mod boxdrag;
pub mod cube;
pub mod empty;
pub mod index_job;
pub mod overlay;
pub mod perf_overlay;
pub mod saved;
pub mod scene;
pub mod settle;
#[cfg(test)]
mod tests;

use crate::editor::Sel;
use crate::shell::EditorApp;
use ddc_render::glam::Vec3;
use ddc_render::nav::{self, Pose, Transition, TRANSITION_S};
use ddc_render::{Aabb, Move, OrbitCamera, Quality, VisualStyle};
use crate::RenderState;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// 3D categories for visibility/transparency rules and temporary hiding.
pub const CATS: [&str; 7] = ["wall", "door", "window", "curtain", "floor", "mep", "other"];

/// Orbit pivot at the point under the cursor — no farther than this many camera distances.
pub const PIVOT_MAX: f32 = 4.0;
/// Coverage below which a category is not picked by the ray (it is see-through).
pub const PICK_COVERAGE: f32 = 0.5;

/// A ray hit is pickable: its category is neither hidden nor transparent (coverage `cov` indexed by
/// mesh category number) and the point is not cut off by the section box.
pub fn pickable(
    cov: &[f32; 16],
    scene: &ddc_render::Scene,
    section: Option<Aabb>,
    h: &ddc_render::Hit,
) -> bool {
    const EPS: f32 = 1e-3;
    let inside = section.is_none_or(|b| {
        (0..3).all(|k| h.point[k] >= b.min[k] - EPS && h.point[k] <= b.max[k] + EPS)
    });
    inside && cov[scene.cat(h.mesh) as usize] >= PICK_COVERAGE
}

/// Default cut height of a level's 3D view above the level (same as the plan), m.
pub const LEVEL_CUT: f64 = 1.2;
/// Bottom of a level's 3D view box below the level (captures the slab), m.
pub const LEVEL_BELOW: f64 = 0.3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CatRule {
    pub visible: bool,
    /// Transparency, %.
    pub transparency: u8,
}

impl Default for CatRule {
    fn default() -> Self {
        Self {
            visible: true,
            transparency: 0,
        }
    }
}

/// Temporary hiding or isolation of categories (3D only, not saved in the view).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TempCats {
    Hide(BTreeSet<String>),
    Isolate(BTreeSet<String>),
}

/// Walk (horizontal) or fly (along the view direction); speed multiplier.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Walk {
    pub fly: bool,
    pub mult: f32,
}

/// Saved-view actions from the overlay (executed in `sync`, where the model is available).
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Save(String),
    Overwrite(i64),
    Restore(i64),
    Delete(i64),
}

/// 3D UI string keys (i18n `v3d.json`).
pub const LABEL_KEYS: &[&str] = &[
    "v3d.cube.top",
    "v3d.cube.bottom",
    "v3d.cube.front",
    "v3d.cube.back",
    "v3d.cube.right",
    "v3d.cube.left",
    "v3d.cube.tip",
    "v3d.home.set",
    "v3d.home.reset",
    "v3d.home.tip",
    "view_cube.cube.up",
    "view_cube.cube.down",
    "view_cube.cube.left",
    "view_cube.cube.right",
    "view_cube.cube.cw",
    "view_cube.cube.ccw",
    "view_cube.cube.persp",
    "view_cube.cube.ortho",
    "view_cube.cube.home",
    "view_cube.cube.fit",
    "v3d.compass.n",
    "v3d.compass.e",
    "v3d.compass.s",
    "v3d.compass.w",
    "v3d.compass.tip",
    "v3d.walk.hud",
    "v3d.fly.hud",
    "v3d.walk.speed",
    "v3d.walk.unit.m",
    "v3d.walk.unit.ft",
    "v3d.proj.to_ortho",
    "v3d.proj.to_persp",
    "v3d.temp.show_all",
    "v3d.box.title",
    "v3d.box.min",
    "v3d.box.max",
    "v3d.box.caps",
    "v3d.box.sel",
    "v3d.box.off",
    "v3d.box.handle.tip",
    "v3d.box.level",
    "v3d.views",
    "v3d.views.save",
    "v3d.views.name",
    "v3d.views.update",
    "v3d.views.delete",
    "v3d.views.none",
    "v3d.hint.nav",
    "v3d.style",
    "v3d.style.shaded",
    "v3d.style.consistent",
    "v3d.style.hidden_line",
    "v3d.style.wireframe",
    "v3d.style.xray",
    "v3d.style.shaded_plain",
    "v3d.style.realistic",
    "v3d.style.tip.wireframe",
    "v3d.style.tip.hidden_line",
    "v3d.style.tip.shaded_plain",
    "v3d.style.tip.shaded",
    "v3d.style.tip.realistic",
    "v3d.style.tip.consistent",
    "v3d.style.tip.xray",
    "v3d.gfx.unavailable",
    "v3d.vg.title",
    "v3d.vg.visible",
    "v3d.vg.transparency",
    "v3d.vg.reset",
    "v3d.cat.wall",
    "v3d.cat.door",
    "v3d.cat.window",
    "v3d.cat.curtain",
    "v3d.cat.floor",
    "v3d.cat.mep",
    "v3d.cat.other",
    "v3d.temp.hide",
    "v3d.temp.isolate",
    "v3d.msg.no_sel",
    "v3d.msg.saved",
    "v3d.msg.deleted",
    "v3d.msg.restored",
    "v3d.msg.empty",
    "v3d.view_name",
    "v3d.perf.title",
    "v3d.perf.frame",
    "v3d.perf.cpu",
    "v3d.perf.draws",
    "v3d.perf.tris",
    "v3d.perf.chunks",
    "v3d.perf.culled",
    "v3d.perf.occluded",
    "v3d.perf.small",
    "v3d.perf.boxes",
    "v3d.perf.dropped",
    "v3d.perf.gpu",
    "v3d.perf.upload",
    "v3d.perf.queue",
    "v3d.perf.evicted",
    "v3d.perf.state",
    "v3d.perf.moving",
    "v3d.perf.refining",
    "v3d.perf.partial",
    "v3d.perf.still",
];

/// UI strings in the current language; without a catalog the keys themselves (visible in checks).
#[derive(Clone, Debug, Default)]
pub struct Labels(HashMap<&'static str, String>);

impl Labels {
    pub fn load(cat: &ddc_ui::Catalog) -> Self {
        Self(LABEL_KEYS.iter().map(|k| (*k, cat.t(k))).collect())
    }
    pub fn get<'a>(&'a self, k: &'a str) -> &'a str {
        self.0.get(k).map_or(k, |s| s.as_str())
    }
}

pub struct Nav {
    /// Frame counters and the background picking index.
    pub perf: perf_overlay::PerfUi,
    pub style: VisualStyle,
    /// Section-cut fill by the section box.
    pub caps: bool,
    /// View category rules (saved in the 3D view).
    pub rules: BTreeMap<String, CatRule>,
    /// Categories hidden by view graphics overrides (VV).
    pub gfx_hidden: Vec<String>,
    pub temp: Option<TempCats>,
    /// Click on the empty-state card, run in `sync`.
    pub empty_act: Option<empty::Act>,
    /// A model job or the 3D build runs: `Some(percent)`, `Some(None)` when no amount is known.
    pub loading: Option<Option<u8>>,
    pub walk: Option<Walk>,
    pub anim: Option<Transition>,
    /// Home view (pose, ortho); `None` — the default angle and "zoom to fit".
    pub home: Option<(Pose, bool)>,
    /// Pivot of the current orbit (selection or the point under the cursor at press time).
    pub pivot: Option<Vec3>,
    /// Level 3D view: (level, cut height above it).
    pub level_view: Option<(i64, f64)>,
    pub labels: Labels,
    labels_lang: Option<ddc_ui::Lang>,
    pub units: crate::units::Units,
    /// True north of the site (degrees from +Y, counter-clockwise) for the cube's compass.
    pub north_deg: f64,
    north_rev: u64,
    pub vg_open: bool,
    pub save_name: String,
    /// Saved 3D views of the model (id, name) and the revision the list was read at.
    pub saved: Vec<(i64, String)>,
    saved_rev: u64,
    /// Currently open saved view (for "Update").
    pub current: Option<i64>,
    pub pending: Vec<Action>,
    /// Mouse button is down (a box handle is being dragged — fills are recomputed on release).
    pub pointer_down: bool,
    /// Aspect ratio of the last frame — for "show" commands issued outside a frame.
    pub aspect: f32,
    /// Elevation of the current level the scene was built at (scene z is measured from it).
    pub elevation: Option<f64>,
    /// Elements hidden by the open saved view (3D only; the plan is untouched).
    pub view_hidden: HashSet<Sel>,
    /// Change counter of `view_hidden` (the view scene is rebuilt when it changes).
    pub hidden_rev: u64,
    /// Hit point under the cursor in the previous frame (none if nothing was hit).
    pub hover_hit: Option<Vec3>,
    pub(crate) deco: scene::Deco,
    /// Navigation hint is hidden after the first action in the view.
    pub hint_used: bool,
}

impl Default for Nav {
    fn default() -> Self {
        Self {
            hint_used: false,
            perf: Default::default(),
            style: VisualStyle::ShadedEdges,
            caps: true,
            rules: BTreeMap::new(),
            gfx_hidden: vec![],
            temp: None,
            empty_act: None,
            loading: None,
            walk: None,
            anim: None,
            home: None,
            pivot: None,
            level_view: None,
            labels: Labels::default(),
            labels_lang: None,
            units: Default::default(),
            north_deg: 0.0,
            north_rev: u64::MAX,
            vg_open: false,
            save_name: String::new(),
            saved: vec![],
            saved_rev: u64::MAX,
            current: None,
            pending: vec![],
            pointer_down: false,
            aspect: 1.5,
            elevation: None,
            view_hidden: HashSet::new(),
            hidden_rev: 0,
            hover_hit: None,
            deco: Default::default(),
        }
    }
}

impl Nav {
    pub fn t<'a>(&'a self, k: &'a str) -> &'a str {
        self.labels.get(k)
    }

    /// Effect quality taking the style into account: shadows and shading apply only to the shaded style.
    pub fn quality(&self, q: Quality) -> Quality {
        if self.style.effects() {
            q
        } else {
            Quality::Off
        }
    }

    /// Whether a category is visible (view rule and temporary mode).
    pub fn cat_visible(&self, cat: &str) -> bool {
        let rule = self.rules.get(cat).is_none_or(|r| r.visible)
            && !self.gfx_hidden.iter().any(|c| c == cat);
        rule && match &self.temp {
            None => true,
            Some(TempCats::Hide(h)) => !h.contains(cat),
            Some(TempCats::Isolate(k)) => k.contains(cat),
        }
    }

    pub fn cat_transparency(&self, cat: &str) -> u8 {
        self.rules.get(cat).map_or(0, |r| r.transparency)
    }

    /// Category coverage by index (`scene::cat_index`) for the frame uniform: view rule and
    /// temporary mode without rebuilding the scene.
    pub fn coverage_table(&self) -> [f32; 16] {
        let mut t = [1.0; 16];
        for (i, c) in CATS.iter().enumerate() {
            t[i + 1] = ddc_render::style::coverage(self.cat_visible(c), self.cat_transparency(c));
        }
        t
    }

    /// Hidden elements of the open view (empty — the view hides nothing).
    pub fn set_view_hidden(&mut self, h: HashSet<Sel>) {
        if h != self.view_hidden {
            self.view_hidden = h;
            self.hidden_rev += 1;
        }
    }

    /// Esc is needed by navigation (leaving walk mode): 3D tools do not take it.
    pub fn wants_escape(&self) -> bool {
        self.walk.is_some()
    }

    /// Frame start: transition step (user input interrupts it), pivot reset.
    pub fn begin(
        &mut self,
        ui: &egui::Ui,
        resp: &egui::Response,
        cam: &mut OrbitCamera,
        aspect: f32,
    ) {
        self.aspect = aspect;
        self.pointer_down = ui.input(|i| i.pointer.any_down());
        if !resp.dragged() {
            self.pivot = None;
        }
        let scroll = resp.hovered() && ui.input(|i| i.smooth_scroll_delta.y != 0.0);
        if resp.dragged() || scroll {
            self.anim = None;
        }
        if let Some(a) = &mut self.anim {
            let dt = ui.input(|i| i.stable_dt).clamp(0.0, 0.1);
            let (p, done) = a.step(dt);
            cam.set_pose(&p);
            if done {
                self.anim = None;
            } else {
                ui.ctx().request_repaint();
            }
        }
    }

    /// Smooth transition to a pose.
    pub fn go(&mut self, cam: &OrbitCamera, to: Pose) {
        self.anim = Some(Transition::new(cam.pose(), to, TRANSITION_S));
    }

    /// What to fit entirely: the visible part of the model (inside the section box).
    pub fn extents(bounds: Option<Aabb>, section: Option<Aabb>) -> Option<Aabb> {
        match (bounds, section) {
            (Some(b), Some(s)) => nav::intersect(&b, &s).or(Some(s)),
            (b, s) => b.or(s),
        }
    }

    /// What F frames: the selection when there is one, else the whole model.
    pub fn frame_target(
        sel: Option<Aabb>,
        focus: Option<Aabb>,
        bounds: Option<Aabb>,
        section: Option<Aabb>,
    ) -> Option<Aabb> {
        sel.or_else(|| Self::extents(focus.or(bounds), section))
    }

    /// Smooth "zoom to fit" keeping the same view angle; false — nothing to show.
    pub fn fit_animated(&mut self, cam: &mut OrbitCamera, b: Option<Aabb>, aspect: f32) -> bool {
        let Some(b) = b else {
            return false;
        };
        cam.scene_radius = cam.scene_radius.max((b.max - b.min).length() * 0.5);
        let to = cam.fit_pose(&b, aspect, cam.yaw, cam.pitch);
        self.go(cam, to);
        true
    }

    /// View from direction `dir` (target to eye) with "zoom to fit" on `b`.
    pub fn view_from(&mut self, cam: &mut OrbitCamera, dir: Vec3, b: Option<Aabb>, aspect: f32) {
        let (yaw, pitch) = nav::orient(dir, cam.yaw);
        let to = match b {
            Some(b) => {
                cam.scene_radius = cam.scene_radius.max((b.max - b.min).length() * 0.5);
                cam.fit_pose(&b, aspect, yaw, pitch)
            }
            None => Pose {
                yaw,
                pitch,
                ..cam.pose()
            },
        };
        self.go(cam, to);
    }

    /// Home view: the saved or default angle with "zoom to fit".
    pub fn go_home(&mut self, cam: &mut OrbitCamera, b: Option<Aabb>, aspect: f32) {
        self.walk = None;
        match self.home {
            Some((p, ortho)) => {
                cam.ortho = ortho;
                self.go(cam, p);
            }
            None => {
                let d = OrbitCamera::default();
                let to = match b {
                    Some(b) => cam.fit_pose(&b, aspect, d.yaw, d.pitch),
                    None => Pose {
                        yaw: d.yaw,
                        pitch: d.pitch,
                        ..cam.pose()
                    },
                };
                self.go(cam, to);
            }
        }
    }

    /// Left-button drag: orbit around the pivot (selection centre or the hit under the cursor at
    /// press time; off the model or too far — the camera target); in walk mode — look around.
    pub fn orbit_drag(
        &mut self,
        cam: &mut OrbitCamera,
        d: egui::Vec2,
        hit: Option<Vec3>,
        selection: Option<Aabb>,
    ) {
        self.anim = None;
        if self.walk.is_some() {
            cam.look(d.x, d.y);
            return;
        }
        if self.pivot.is_none() {
            let near = |p: &Vec3| (*p - cam.eye()).length() <= PIVOT_MAX * cam.distance.max(1.0);
            self.pivot = selection
                .map(|b| (b.min + b.max) * 0.5)
                .or(hit.filter(near))
                .or(Some(cam.target));
        }
        match self.pivot {
            Some(p) => cam.orbit_about(p, d.x, d.y),
            None => cam.orbit(d.x, d.y),
        }
    }

    pub fn toggle_walk(&mut self, cam: &mut OrbitCamera) {
        self.anim = None;
        self.walk = match self.walk {
            Some(_) => None,
            None => {
                cam.ortho = false;
                cam.to_first_person(1.0);
                Some(Walk {
                    fly: false,
                    mult: 1.0,
                })
            }
        };
    }

    /// Walk mode: WASD/arrows — move, E/Q — up/down, Shift — ×4, [ ] — speed,
    /// G — walk/fly, Esc — exit. While walking, the viewport holds keyboard focus
    /// (letter command shortcuts do not fire).
    pub fn walk_input(
        &mut self,
        ui: &egui::Ui,
        resp: &egui::Response,
        cam: &mut OrbitCamera,
        bounds: Option<&Aabb>,
    ) {
        let Some(mut w) = self.walk else {
            return;
        };
        use egui::Key;
        // the keyboard belongs to an input field (view name, box numbers): do not take focus and do not move
        if ui.memory(|m| m.focused().is_some_and(|f| f != resp.id)) {
            return;
        }
        resp.request_focus();
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                resp.id,
                egui::EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                },
            )
        });
        let (keys, dt, esc, toggle, faster, slower, shift) = ui.input(|i| {
            let k = |a: Key, b: Key| f32::from(u8::from(i.key_down(a) || i.key_down(b)));
            (
                Move {
                    fwd: k(Key::W, Key::ArrowUp) - k(Key::S, Key::ArrowDown),
                    right: k(Key::D, Key::ArrowRight) - k(Key::A, Key::ArrowLeft),
                    up: k(Key::E, Key::PageUp) - k(Key::Q, Key::PageDown),
                },
                i.stable_dt.clamp(0.0, 0.1),
                i.key_pressed(Key::Escape),
                i.key_pressed(Key::G),
                i.key_pressed(Key::CloseBracket),
                i.key_pressed(Key::OpenBracket),
                i.modifiers.shift,
            )
        });
        if esc && crate::ui_kit::canvas_owns_keys(ui.ctx()) {
            // the same Esc must not also drop temp hide/isolate in the overlay this frame
            ui.ctx()
                .input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape));
            self.walk = None;
            resp.surrender_focus();
            return;
        }
        if toggle {
            w.fly = !w.fly;
        }
        if faster {
            w.mult = (w.mult * 1.25).min(20.0);
        }
        if slower {
            w.mult = (w.mult / 1.25).max(0.05);
        }
        cam.ortho = false;
        cam.to_first_person(1.0);
        if keys != Move::default() {
            let mult = w.mult * if shift { 4.0 } else { 1.0 };
            let speed = nav::clamped_speed(nav::walk_speed(bounds, w.fly), mult);
            cam.walk(keys, speed, dt, w.fly);
            ui.ctx().request_repaint();
        }
        self.walk = Some(w);
    }

    /// Walk speed for the hint, m/s (walking — human pace, flying — scaled to the model).
    pub fn walk_speed(&self, bounds: Option<&Aabb>) -> f32 {
        let (fly, mult) = self.walk.map_or((false, 1.0), |w| (w.fly, w.mult));
        nav::clamped_speed(nav::walk_speed(bounds, fly), mult)
    }
}

/// Category of a pick element (as in the 3D rules).
pub fn sel_category(ed: &crate::editor::Editor, s: Sel) -> &'static str {
    match s {
        Sel::Wall(_) => "wall",
        Sel::Opening(id) => match ed.opening(id).map(|o| o.kind) {
            Some(crate::opening_tool::Kind::Door) => "door",
            Some(crate::opening_tool::Kind::Void) => "other",
            _ => "window",
        },
        Sel::Curtain(_) | Sel::Panel(..) => "curtain",
        Sel::Slab(_) => "floor",
        Sel::Mep(_) => "mep",
        Sel::Comp(_) | Sel::Room(_) => "other",
        #[allow(unreachable_patterns)]
        _ => "other",
    }
}

/// Frame: strings, units, deferred view actions, level shift, scene decoration.
/// Called from `prepare_3d` after the base scene is built.
pub fn sync(app: &mut EditorApp, rs: Option<&RenderState>, t: &crate::ui_kit::Tokens) {
    let lang = app.sh.cat.lang;
    if app.view.nav.labels_lang != Some(lang) {
        app.view.nav.labels = Labels::load(&app.sh.cat);
        app.view.nav.labels_lang = Some(lang);
    }
    app.view.nav.units = app.ed.tool.units;
    app.view.nav.loading = match app.big.view() {
        Some(j) => {
            let pct = (j.fraction * 100.0).round().clamp(0.0, 99.0) as u8;
            Some((!j.indeterminate).then_some(pct))
        }
        None => app.view.nav.deco.busy().then_some(None),
    };
    if app.view.nav.north_rev != app.ed.revision {
        app.view.nav.north_deg = app.ed.model.site().map_or(0.0, |s| s.north_deg);
        app.view.nav.north_rev = app.ed.revision;
    }
    // scene z is measured from the current level: on a level change the camera and box stay
    // in place in model coordinates
    let e = app.ed.elevation;
    if let Some(old) = app.view.nav.elevation {
        let dz = (e - old) as f32;
        if dz != 0.0 {
            app.view.camera.target.z -= dz;
            if let Some(b) = &mut app.view.section_box {
                b.min.z -= dz;
                b.max.z -= dz;
            }
            // the home view is also in model coordinates
            if let Some((p, _)) = &mut app.view.nav.home {
                p.target.z -= dz;
            }
            app.view.nav.anim = None;
        }
    }
    app.view.nav.elevation = Some(e);
    for a in std::mem::take(&mut app.view.nav.pending) {
        saved::apply(app, a);
    }
    if let Some(k) = app.view.nav.empty_act.take() {
        empty::apply(app, k);
    }
    if app.view.nav.saved_rev != app.ed.revision {
        app.view.nav.saved = app
            .ed
            .model
            .view3ds()
            .map(|v| v.into_iter().map(|(id, s)| (id, s.name)).collect())
            .unwrap_or_default();
        app.view.nav.saved_rev = app.ed.revision;
        // the open view was deleted (including by undoing its save)
        let nav = &mut app.view.nav;
        if nav
            .current
            .is_some_and(|id| !nav.saved.iter().any(|v| v.0 == id))
        {
            nav.current = None;
            nav.set_view_hidden(HashSet::new());
        }
    }
    scene::decorate(app, rs, t);
}

fn msg(a: &mut EditorApp, key: &str) {
    a.ed.status = Some(a.sh.cat.t(key));
}

fn style(a: &mut EditorApp, s: VisualStyle) {
    a.view.nav.style = s;
    a.view3d = true;
}
fn h_perf_stats(a: &mut EditorApp) {
    a.view.nav.perf.shown = !a.view.nav.perf.shown;
}
fn h_style_cycle(a: &mut EditorApp) {
    let all = VisualStyle::ALL;
    let i = all.iter().position(|s| *s == a.view.nav.style).unwrap_or(0);
    style(a, all[(i + 1) % all.len()]);
}
fn h_shaded(a: &mut EditorApp) {
    style(a, VisualStyle::ShadedEdges);
}
fn h_consistent(a: &mut EditorApp) {
    style(a, VisualStyle::Consistent);
}
fn h_hidden(a: &mut EditorApp) {
    style(a, VisualStyle::HiddenLine);
}
fn h_wireframe(a: &mut EditorApp) {
    style(a, VisualStyle::Wireframe);
}
fn h_xray(a: &mut EditorApp) {
    style(a, VisualStyle::XRay);
}
fn h_projection(a: &mut EditorApp) {
    let c = &mut a.view.camera;
    c.ortho = !c.ortho;
    if c.ortho {
        a.view.nav.walk = None;
    }
    a.view3d = true;
}
fn h_walk(a: &mut EditorApp) {
    a.view.nav.toggle_walk(&mut a.view.camera);
    a.view3d = true;
}
fn h_home(a: &mut EditorApp) {
    let b = Nav::extents(a.scene.focus.or(a.scene.bounds), a.view.section_box);
    let aspect = a.view.nav.aspect;
    a.view.nav.go_home(&mut a.view.camera, b, aspect);
    a.view3d = true;
}
fn selection_box(a: &EditorApp) -> Option<Aabb> {
    a.ed.highlight().and_then(|r| a.scene.bounds_of(r))
}
fn h_zoom_sel(a: &mut EditorApp) {
    let Some(b) = selection_box(a) else {
        return msg(a, "v3d.msg.no_sel");
    };
    let aspect = a.view.nav.aspect;
    a.view.nav.walk = None;
    a.view.nav.fit_animated(&mut a.view.camera, Some(b), aspect);
    a.view3d = true;
}
fn h_box_fit(a: &mut EditorApp) {
    let Some(b) = selection_box(a).or(a.scene.focus).or(a.scene.bounds) else {
        return msg(a, "v3d.msg.empty");
    };
    let pad = Vec3::splat(0.3);
    a.view.section_box = Some(Aabb {
        min: b.min - pad,
        max: b.max + pad,
    });
    a.view.nav.level_view = None;
    a.view3d = true;
}
fn h_caps(a: &mut EditorApp) {
    a.view.nav.caps = !a.view.nav.caps;
}
/// Box of a level's 3D view: the whole model in plan, in height from just below the level up to the cut.
pub fn level_box(bounds: Aabb, cut: f64) -> Aabb {
    let pad = Vec3::new(0.5, 0.5, 0.0);
    Aabb {
        min: Vec3::new(bounds.min.x, bounds.min.y, -(LEVEL_BELOW as f32)) - pad,
        max: Vec3::new(bounds.max.x, bounds.max.y, (cut as f32).max(0.05)) + pad,
    }
}
fn h_level(a: &mut EditorApp) {
    let Some(b) = a.view.nav.deco.base_bounds.or(a.scene.bounds) else {
        return msg(a, "v3d.msg.empty");
    };
    let cut = a.view.nav.level_view.map_or(LEVEL_CUT, |l| l.1);
    let bx = level_box(b, cut);
    a.view.section_box = Some(bx);
    a.view.nav.level_view = Some((a.ed.level, cut));
    a.view.nav.caps = true;
    a.view.nav.walk = None;
    let aspect = a.view.nav.aspect;
    let d = OrbitCamera::default();
    let to = a.view.camera.fit_pose(&bx, aspect, d.yaw, 0.9);
    a.view.nav.go(&a.view.camera, to);
    a.view3d = true;
}
fn h_save(a: &mut EditorApp) {
    let n = a.view.nav.saved.len() + 1;
    let name = format!("{} {n}", a.sh.cat.t("v3d.view_name"));
    a.view.nav.pending.push(Action::Save(name));
    a.view3d = true;
}
fn h_vg(a: &mut EditorApp) {
    a.view.nav.vg_open = !a.view.nav.vg_open;
    a.view3d = true;
}
fn selected_cats(a: &EditorApp) -> BTreeSet<String> {
    a.ed.selection
        .iter()
        .map(|s| sel_category(&a.ed, *s).to_string())
        .collect()
}
fn h_hide_cat(a: &mut EditorApp) {
    let c = selected_cats(a);
    if c.is_empty() {
        return msg(a, "v3d.msg.no_sel");
    }
    a.view.nav.temp = Some(match a.view.nav.temp.take() {
        Some(TempCats::Hide(mut h)) => {
            h.extend(c);
            TempCats::Hide(h)
        }
        _ => TempCats::Hide(c),
    });
    a.ed.selection.clear();
}
fn h_isolate_cat(a: &mut EditorApp) {
    let c = selected_cats(a);
    if c.is_empty() {
        return msg(a, "v3d.msg.no_sel");
    }
    a.view.nav.temp = Some(TempCats::Isolate(c));
}
fn h_reveal(a: &mut EditorApp) {
    a.view.nav.temp = None;
    a.view.nav.set_view_hidden(HashSet::new());
}

/// Commands of the 3D views (`commands.d/g8.json`).
pub fn register(a: &mut EditorApp) {
    let list: &[(&str, crate::shell::Handler)] = &[
        ("view.perf_stats", h_perf_stats),
        ("view.style", h_style_cycle),
        ("view.style_shaded", h_shaded),
        ("view.style_consistent", h_consistent),
        ("view.style_hidden", h_hidden),
        ("view.style_wireframe", h_wireframe),
        ("view.style_xray", h_xray),
        ("view.projection", h_projection),
        ("view.walk", h_walk),
        ("view.home_3d", h_home),
        ("view.zoom_sel_3d", h_zoom_sel),
        ("view.section_box_fit", h_box_fit),
        ("view.section_caps", h_caps),
        ("view.level_3d", h_level),
        ("view.save_3d", h_save),
        ("view.vg_3d", h_vg),
        ("view.hide_cat_3d", h_hide_cat),
        ("view.isolate_cat_3d", h_isolate_cat),
        ("view.reveal_3d", h_reveal),
    ];
    for (id, h) in list {
        a.register_command(id, *h);
    }
}
