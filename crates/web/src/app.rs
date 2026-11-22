// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The viewer window: 3D and plan views, levels, properties and checks.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32, RichText};
use eframe::glow;

use crate::cam::Orbit;
use crate::core::{self, AuditRow, Project, Scene};
use crate::gl::{Frame, Gpu};
use crate::plan::Plan;
use ddc_model::checks::Severity;

thread_local! {
    static PENDING: RefCell<Option<(String, Vec<u8>)>> = const { RefCell::new(None) };
    static NOTE: RefCell<Option<String>> = const { RefCell::new(None) };
    static CTX: RefCell<Option<egui::Context>> = const { RefCell::new(None) };
    static STATE: RefCell<String> = RefCell::new("{}".into());
    static COMMAND: RefCell<Vec<String>> = const { RefCell::new(vec![]) };
}

fn repaint() {
    CTX.with(|c| {
        if let Some(c) = c.borrow().as_ref() {
            c.request_repaint();
        }
    });
}

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    View3d,
    Plan,
}

type Shared = Arc<Mutex<Option<Gpu>>>;

pub struct Viewer {
    gl: Arc<glow::Context>,
    gpu: Shared,
    proj: Option<Project>,
    scene: Option<Arc<Scene>>,
    origin: [f32; 3],
    cam: Orbit,
    mode: Mode,
    sel: Option<(String, i64)>,
    props: Vec<(String, String)>,
    audit: Option<Result<Vec<AuditRow>, String>>,
    show_audit: bool,
    hidden: HashSet<Option<i64>>,
    plan: Option<Plan>,
    plan_level: Option<i64>,
    status: String,
    warnings: Vec<String>,
    /// Opening waits one frame so the "Opening…" status is on screen first.
    queued: Option<(String, Vec<u8>)>,
}

impl Viewer {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        CTX.with(|c| *c.borrow_mut() = Some(cc.egui_ctx.clone()));
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        Viewer {
            gl: cc.gl.clone().expect("the glow backend"),
            gpu: Arc::new(Mutex::new(None)),
            proj: None,
            scene: None,
            origin: [0.0; 3],
            cam: Orbit::default(),
            mode: Mode::View3d,
            sel: None,
            props: vec![],
            audit: None,
            show_audit: false,
            hidden: HashSet::new(),
            plan: None,
            plan_level: None,
            status: "Open a .ddc project or an IFC file (button above, drag and drop, or ?project=<url>).".into(),
            warnings: vec![],
            queued: None,
        }
    }

    fn open(&mut self, name: String, bytes: Vec<u8>) {
        let t0 = web_time::Instant::now();
        self.sel = None;
        self.props.clear();
        self.audit = None;
        self.plan = None;
        self.hidden.clear();
        if let Some(g) = self.gpu.lock().unwrap().take() {
            g.destroy(&self.gl);
        }
        self.scene = None;
        self.proj = None;
        let p = match core::open(&name, &bytes) {
            Ok(p) => p,
            Err(e) => {
                self.status = format!("Could not open {name}: {e}");
                log::error!("{}", self.status);
                return;
            }
        };
        let (scene, mut warns) = core::tessellate(&p.model);
        warns.splice(0..0, p.warnings.iter().cloned());
        self.origin = [0, 1, 2].map(|k| ((scene.lo[k] + scene.hi[k]) * 0.5).round());
        match Gpu::new(&self.gl, &scene, self.origin) {
            Ok(g) => *self.gpu.lock().unwrap() = Some(g),
            Err(e) => {
                self.status = format!("WebGL: {e}");
                log::error!("{}", self.status);
                return;
            }
        }
        let lo = [0, 1, 2].map(|k| scene.lo[k] - self.origin[k]);
        let hi = [0, 1, 2].map(|k| scene.hi[k] - self.origin[k]);
        self.cam = Orbit::default();
        self.cam.fit(lo, hi);
        self.status = format!(
            "{name}: {} elements, {} triangles, {} levels, opened in {:.1} s",
            scene.items.len(),
            scene.triangles(),
            p.levels.len(),
            t0.elapsed().as_secs_f32()
        );
        self.plan_level = p.levels.first().map(|l| l.0);
        self.warnings = warns;
        self.scene = Some(Arc::new(scene));
        self.proj = Some(p);
    }

    fn select(&mut self, s: Option<(String, i64)>) {
        self.props = match (&self.proj, &s) {
            (Some(p), Some((t, id))) => core::properties(p, t, *id),
            _ => vec![],
        };
        self.sel = s;
    }

    fn frame_selected(&mut self) {
        let (Some(sc), Some((t, id))) = (&self.scene, &self.sel) else {
            return;
        };
        if let Some(i) = sc.find(t, *id) {
            let it = &sc.items[i];
            let lo = [0, 1, 2].map(|k| it.lo[k] - self.origin[k]);
            let hi = [0, 1, 2].map(|k| it.hi[k] - self.origin[k]);
            self.cam.fit(lo, hi);
            self.cam.dist = self.cam.dist.max(4.0);
        }
    }

    fn run_audit(&mut self) {
        if let Some(p) = &self.proj {
            let t0 = web_time::Instant::now();
            self.audit = Some(core::audit(p, 500));
            if let Some(Ok(a)) = &self.audit {
                let e = a.iter().filter(|r| r.severity == Severity::Error).count();
                let w = a.iter().filter(|r| r.severity == Severity::Warning).count();
                self.status = format!(
                    "Checks: {e} errors, {w} warnings, {} notes in {:.1} s",
                    a.len() - e - w,
                    t0.elapsed().as_secs_f32()
                );
            }
            self.show_audit = true;
        }
    }

    fn publish_state(&self) {
        let sc = self.scene.as_ref();
        let audit = match &self.audit {
            Some(Ok(a)) => serde_json::json!({
                "findings": a.len(),
                "errors": a.iter().filter(|r| r.severity == Severity::Error).count(),
            }),
            Some(Err(e)) => serde_json::json!({ "error": e }),
            None => serde_json::Value::Null,
        };
        let v = serde_json::json!({
            "project": self.proj.as_ref().map(|p| p.name.clone()),
            "kind": self.proj.as_ref().map(|p| p.kind),
            "levels": self.proj.as_ref().map_or(0, |p| p.levels.len()),
            "items": sc.map_or(0, |s| s.items.len()),
            "triangles": sc.map_or(0, |s| s.triangles()),
            "mode": if self.mode == Mode::Plan { "plan" } else { "3d" },
            "plan_prims": self.plan.as_ref().map_or(0, |p| p.prims.len()),
            "selected": self.sel.as_ref().map(|s| format!("{}:{}", s.0, s.1)),
            "properties": self.props.len(),
            "audit": audit,
            "camera": { "yaw": self.cam.yaw, "pitch": self.cam.pitch, "dist": self.cam.dist },
            "status": self.status,
        });
        STATE.with(|s| *s.borrow_mut() = v.to_string());
    }

    fn commands(&mut self) {
        let cmds = COMMAND.with(|c| std::mem::take(&mut *c.borrow_mut()));
        for c in cmds {
            match c.as_str() {
                "audit" => self.run_audit(),
                "plan" => self.mode = Mode::Plan,
                "3d" => self.mode = Mode::View3d,
                _ => {}
            }
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("OpenConstructionDesign").strong());
            ui.label(RichText::new("browser viewer").weak());
            ui.separator();
            if ui.button("Open…").on_hover_text("A .ddc project or an IFC file").clicked() {
                pick_file();
            }
            ui.separator();
            let has = self.proj.is_some();
            ui.add_enabled_ui(has, |ui| {
                ui.selectable_value(&mut self.mode, Mode::View3d, "3D");
                ui.selectable_value(&mut self.mode, Mode::Plan, "Plan");
                if self.mode == Mode::Plan {
                    if let Some(p) = &self.proj {
                        let cur = self.plan_level.and_then(|l| p.levels.iter().find(|x| x.0 == l));
                        egui::ComboBox::from_id_salt("plan_level")
                            .selected_text(cur.map_or("level".into(), |l| l.1.clone()))
                            .show_ui(ui, |ui| {
                                for l in &p.levels {
                                    ui.selectable_value(&mut self.plan_level, Some(l.0), &l.1);
                                }
                            });
                    }
                }
                ui.separator();
                if ui.button("Fit").clicked() {
                    if let Some(s) = &self.scene {
                        let lo = [0, 1, 2].map(|k| s.lo[k] - self.origin[k]);
                        let hi = [0, 1, 2].map(|k| s.hi[k] - self.origin[k]);
                        self.cam.fit(lo, hi);
                    }
                    if let Some(p) = &mut self.plan {
                        p.refit();
                    }
                }
                if ui.button("Run checks").clicked() {
                    self.run_audit();
                }
            });
        });
    }

    fn levels_panel(&mut self, ui: &mut egui::Ui) {
        let (Some(p), Some(sc)) = (&self.proj, &self.scene) else {
            ui.label("No project open.");
            return;
        };
        ui.heading("Levels");
        let mut by_level: BTreeMap<Option<i64>, usize> = BTreeMap::new();
        for it in &sc.items {
            *by_level.entry(it.level).or_default() += 1;
        }
        let mut rows: Vec<(Option<i64>, String)> = p.levels.iter().map(|l| (Some(l.0), l.1.clone())).collect();
        if by_level.contains_key(&None) {
            rows.push((None, "No level".into()));
        }
        for (lv, name) in rows {
            let n = by_level.get(&lv).copied().unwrap_or(0);
            let mut on = !self.hidden.contains(&lv);
            if ui.checkbox(&mut on, format!("{name}  ({n})")).changed() {
                if on {
                    self.hidden.remove(&lv);
                } else {
                    self.hidden.insert(lv);
                }
            }
        }
        ui.separator();
        ui.heading("Elements");
        let mut cats: BTreeMap<&str, usize> = BTreeMap::new();
        for it in &sc.items {
            *cats.entry(it.cat.as_str()).or_default() += 1;
        }
        egui::ScrollArea::vertical().id_salt("cats").show(ui, |ui| {
            for (c, n) in cats {
                ui.label(format!("{c}: {n}"));
            }
        });
    }

    fn props_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Properties");
        let Some((t, id)) = &self.sel else {
            ui.label(RichText::new("Click an element to see its properties.").weak());
            return;
        };
        let name = self
            .scene
            .as_ref()
            .and_then(|s| s.find(t, *id).map(|i| s.items[i].name.clone()))
            .unwrap_or_else(|| format!("{t} {id}"));
        ui.label(RichText::new(name).strong());
        if ui.small_button("Zoom to").clicked() {
            self.mode = Mode::View3d;
            self.frame_selected();
        }
        ui.separator();
        egui::ScrollArea::vertical().id_salt("props").show(ui, |ui| {
            egui::Grid::new("props_grid").num_columns(2).striped(true).show(ui, |ui| {
                for (k, v) in &self.props {
                    ui.label(RichText::new(k).weak());
                    ui.label(v);
                    ui.end_row();
                }
            });
        });
    }

    fn audit_panel(&mut self, ui: &mut egui::Ui) {
        let mut pick = None;
        ui.horizontal(|ui| {
            ui.heading("Checks");
            if ui.small_button("Close").clicked() {
                self.show_audit = false;
            }
        });
        match &self.audit {
            None => {
                ui.label("Not run yet.");
            }
            Some(Err(e)) => {
                ui.colored_label(Color32::LIGHT_RED, e);
            }
            Some(Ok(rows)) if rows.is_empty() => {
                ui.label("No findings: every enabled rule is met.");
            }
            Some(Ok(rows)) => {
                egui::ScrollArea::vertical().id_salt("audit").show(ui, |ui| {
                    for (i, r) in rows.iter().enumerate() {
                        let (c, s) = match r.severity {
                            Severity::Error => (Color32::from_rgb(235, 90, 80), "error"),
                            Severity::Warning => (Color32::from_rgb(230, 180, 60), "warning"),
                            Severity::Info => (Color32::from_rgb(120, 170, 230), "note"),
                        };
                        ui.horizontal(|ui| {
                            ui.colored_label(c, s);
                            ui.label(RichText::new(r.rule).weak());
                            let txt = if r.elements.is_empty() {
                                r.text.clone()
                            } else {
                                let e: Vec<String> = r.elements.iter().take(3).map(|e| format!("{}:{}", e.0, e.1)).collect();
                                format!("{}  [{}]", r.text, e.join(", "))
                            };
                            if ui.add(egui::Label::new(txt).sense(egui::Sense::click())).clicked() {
                                pick = Some(i);
                            }
                        });
                    }
                });
            }
        }
        if let (Some(i), Some(Ok(rows))) = (pick, &self.audit) {
            if let Some(e) = rows[i].elements.first().cloned() {
                self.select(Some(e));
                self.frame_selected();
            }
        }
    }

    fn view3d(&mut self, ui: &mut egui::Ui) {
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        let Some(sc) = self.scene.clone() else {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Drop a .ddc or .ifc file here",
                egui::FontId::proportional(18.0),
                Color32::GRAY,
            );
            return;
        };
        let (prim, sec) = ui.input(|i| (i.pointer.primary_down(), i.pointer.secondary_down() || i.pointer.middle_down()));
        if resp.dragged() {
            let d = resp.drag_delta();
            let shift = ui.input(|i| i.modifiers.shift);
            if sec || (prim && shift) {
                self.cam.pan(d.x, d.y, rect.height());
            } else {
                self.cam.orbit(d.x, d.y);
            }
        }
        if resp.hovered() {
            let s = ui.input(|i| i.smooth_scroll_delta.y + (i.zoom_delta() - 1.0) * 400.0);
            if s != 0.0 {
                self.cam.zoom(s);
            }
        }
        let aspect = rect.width() / rect.height().max(1.0);
        let hidden = self.hidden.clone();
        if resp.clicked() {
            if let Some(at) = resp.interact_pointer_pos() {
                let x = (at.x - rect.left()) / rect.width() * 2.0 - 1.0;
                let y = 1.0 - (at.y - rect.top()) / rect.height() * 2.0;
                let (o, d) = self.cam.ray(x, y, aspect);
                let o = [0, 1, 2].map(|k| o[k] + self.origin[k]);
                let hit = sc.pick(o, d, |it| !hidden.contains(&it.level));
                self.select(hit.map(|i| (sc.items[i].table.clone(), sc.items[i].id)));
            }
        }
        if resp.double_clicked() {
            self.frame_selected();
        }
        // contiguous level runs (items are sorted by level)
        let mut ranges: Vec<(i32, i32)> = vec![];
        for it in sc.items.iter().filter(|it| !hidden.contains(&it.level)) {
            match ranges.last_mut() {
                Some(r) if r.0 + r.1 == it.first as i32 => r.1 += it.count as i32,
                _ => ranges.push((it.first as i32, it.count as i32)),
            }
        }
        let sel = self
            .sel
            .as_ref()
            .and_then(|(t, id)| sc.find(t, *id))
            .map_or((-1, -1), |i| (sc.items[i].first as i32, (sc.items[i].first + sc.items[i].count) as i32));
        let frame = Frame {
            mvp: self.cam.matrix(aspect),
            ranges,
            sel,
            bg: [0.11, 0.12, 0.14],
        };
        let gpu = self.gpu.clone();
        let cb = egui::PaintCallback {
            rect,
            callback: Arc::new(eframe::egui_glow::CallbackFn::new(move |info, painter| {
                if let Some(g) = gpu.lock().unwrap().as_ref() {
                    let v = info.viewport_in_pixels();
                    g.draw(painter.gl(), &frame, [v.left_px, v.from_bottom_px, v.width_px, v.height_px]);
                }
            })),
        };
        ui.painter().add(cb);
        ui.painter().text(
            rect.left_bottom() + egui::vec2(8.0, -8.0),
            egui::Align2::LEFT_BOTTOM,
            "drag: orbit · right drag or shift+drag: pan · wheel: zoom · click: select · double click: zoom to",
            egui::FontId::proportional(11.0),
            Color32::from_gray(150),
        );
    }

    fn view_plan(&mut self, ui: &mut egui::Ui) {
        let Some(p) = &self.proj else {
            return;
        };
        let Some(lv) = self.plan_level else {
            ui.label("The project has no levels.");
            return;
        };
        if self.plan.as_ref().is_none_or(|pl| pl.level != lv) {
            match Plan::build(&p.model, lv) {
                Ok(pl) => self.plan = Some(pl),
                Err(e) => {
                    ui.colored_label(Color32::LIGHT_RED, format!("Plan: {e}"));
                    return;
                }
            }
        }
        let sel = self.sel.as_ref().map(|(t, id)| (t.as_str(), *id));
        let hit = self.plan.as_mut().unwrap().ui(ui, sel, true);
        if let Some(h) = hit {
            self.select(h);
        }
    }
}

impl eframe::App for Viewer {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some((name, bytes)) = self.queued.take() {
            self.open(name, bytes);
        }
        if let Some((name, bytes)) = PENDING.with(|p| p.borrow_mut().take()) {
            self.status = format!("Opening {name} ({:.1} MB)…", bytes.len() as f64 / 1e6);
            self.queued = Some((name, bytes));
            ctx.request_repaint();
        }
        if let Some(n) = NOTE.with(|p| p.borrow_mut().take()) {
            self.status = n;
        }
        let dropped: Vec<egui::DroppedFile> = ctx.input(|i| i.raw.dropped_files.clone());
        if let Some(f) = dropped.into_iter().find(|f| f.bytes.is_some()) {
            let b = f.bytes.unwrap().to_vec();
            self.status = format!("Opening {}…", f.name);
            self.queued = Some((f.name, b));
            ctx.request_repaint();
        }
        self.commands();
        egui::TopBottomPanel::top("top").show(ctx, |ui| self.top_bar(ui));
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(&self.status);
                if !self.warnings.is_empty() {
                    ui.label(RichText::new(format!("{} notes", self.warnings.len())).weak())
                        .on_hover_text(self.warnings.join("\n"));
                }
            });
        });
        if self.show_audit {
            egui::TopBottomPanel::bottom("audit")
                .resizable(true)
                .default_height(200.0)
                .show(ctx, |ui| self.audit_panel(ui));
        }
        egui::SidePanel::left("levels").default_width(190.0).show(ctx, |ui| self.levels_panel(ui));
        egui::SidePanel::right("props").default_width(300.0).show(ctx, |ui| self.props_panel(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| match self.mode {
                Mode::View3d => self.view3d(ui),
                Mode::Plan => self.view_plan(ui),
            });
        self.publish_state();
    }

    fn on_exit(&mut self, gl: Option<&glow::Context>) {
        if let (Some(gl), Some(g)) = (gl, self.gpu.lock().unwrap().take()) {
            g.destroy(gl);
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub mod web {
    use super::*;
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_namespace = window, js_name = ddcPickFile)]
        fn ddc_pick_file();
    }

    pub(super) fn pick_file() {
        ddc_pick_file();
    }

    /// Opens a project from its bytes (called by the page after a file pick, drop or fetch).
    #[wasm_bindgen]
    pub fn open_bytes(name: String, bytes: Vec<u8>) {
        PENDING.with(|p| *p.borrow_mut() = Some((name, bytes)));
        repaint();
    }

    /// A status line from the page (download progress, a failed fetch).
    #[wasm_bindgen]
    pub fn note(text: String) {
        NOTE.with(|p| *p.borrow_mut() = Some(text));
        repaint();
    }

    /// Viewer state as JSON, for the page and automated tests.
    #[wasm_bindgen]
    pub fn state() -> String {
        STATE.with(|s| s.borrow().clone())
    }

    /// `audit`, `plan` or `3d`, applied on the next frame.
    #[wasm_bindgen]
    pub fn command(c: String) {
        COMMAND.with(|q| q.borrow_mut().push(c));
        repaint();
    }

    #[wasm_bindgen]
    pub async fn start(canvas_id: String) -> Result<(), JsValue> {
        console_log();
        let doc = web_sys::window().and_then(|w| w.document()).ok_or("no document")?;
        let canvas = doc
            .get_element_by_id(&canvas_id)
            .ok_or("no canvas")?
            .dyn_into::<web_sys::HtmlCanvasElement>()?;
        let opts = eframe::WebOptions {
            depth_buffer: 24,
            ..Default::default()
        };
        eframe::WebRunner::new()
            .start(canvas, opts, Box::new(|cc| Ok(Box::new(Viewer::new(cc)))))
            .await
    }

    fn console_log() {
        struct L;
        impl log::Log for L {
            fn enabled(&self, m: &log::Metadata) -> bool {
                m.level() <= log::Level::Warn
            }
            fn log(&self, r: &log::Record) {
                if !self.enabled(r.metadata()) {
                    return;
                }
                let s = format!("{}", r.args());
                if r.level() == log::Level::Error {
                    web_sys::console::error_1(&s.into());
                } else {
                    web_sys::console::warn_1(&s.into());
                }
            }
            fn flush(&self) {}
        }
        static LOGGER: L = L;
        let _ = log::set_logger(&LOGGER).map(|()| log::set_max_level(log::LevelFilter::Warn));
        std::panic::set_hook(Box::new(|i| web_sys::console::error_1(&format!("panic: {i}").into())));
    }
}

#[cfg(target_arch = "wasm32")]
use web::pick_file;
