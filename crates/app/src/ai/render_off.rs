// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Offscreen renders for `render_plan` and `render_3d`: the real app UI is painted headless on a
//! private copy of the scratch model (`shot::frame`), then the canvas is cropped out. Nothing
//! reaches the person's screen or the live model.

use super::render_req::{self as rq, Legend, PlanReq, Render3d, Size, Target};
use super::session::{encode_png, open_editor, view_rect};
use crate::editor::{Editor, Sel};
use crate::shell::EditorApp;
use crate::units::Units;
#[cfg(feature = "gui")]
use crate::gfx;
use crate::shot;
use ddc_ai::ImageData;
use ddc_geom::v2;
use ddc_render::glam::Vec3;
use ddc_render::Aabb;
use egui::{self, Align2, Color32, FontId};
use crate::RenderState;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Chrome around the canvas is measured on a first pass at this much extra room.
const PAD: (u32, u32) = (900, 600);
static COPY_SEQ: AtomicU64 = AtomicU64::new(0);

thread_local! {
    // never dropped: at thread exit the wgpu thread-locals may already be gone, and dropping
    // a handle then aborts the process. It is a clone of the one shared device, not a device:
    // a device per thread leaked the whole device with every assistant session.
    static GPU: std::mem::ManuallyDrop<RefCell<Option<(u64, Rc<RenderState>)>>> =
        const { std::mem::ManuallyDrop::new(RefCell::new(None)) };
}

/// The one offscreen device of the process, and where its errors land instead of a panic.
#[cfg(feature = "gui")]
struct Shared {
    rs: RenderState,
    flag: gfx::LostFlag,
    gen: u64,
}

#[cfg(feature = "gui")]
static SHARED: std::sync::Mutex<Option<Shared>> = std::sync::Mutex::new(None);
#[cfg(feature = "gui")]
static GEN: AtomicU64 = AtomicU64::new(1);
/// One offscreen picture at a time: the egui renderer of the shared device is not per picture.
static PAINTING: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(feature = "gui")]
fn shared() -> Result<(u64, RenderState), String> {
    let mut s = SHARED.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(sh) = s.as_ref() {
        return Ok((sh.gen, sh.rs.clone()));
    }
    let mut last = "no graphics adapter".to_string();
    for pick in gfx::candidates(None, false) {
        match shot::render_state(pick) {
            Ok(rs) => {
                let flag = gfx::LostFlag::default();
                let f = flag.clone();
                rs.device.set_device_lost_callback(move |reason, msg| {
                    if reason != eframe::egui_wgpu::wgpu::DeviceLostReason::Destroyed {
                        f.mark(format!("device lost: {msg}"));
                    }
                });
                let f = flag.clone();
                // out of memory and the like fail the picture, never the assistant's thread
                rs.device.on_uncaptured_error(Box::new(move |e| f.mark(format!("device error: {e}"))));
                let gen = GEN.fetch_add(1, Ordering::Relaxed);
                *s = Some(Shared { rs: rs.clone(), flag, gen });
                return Ok((gen, rs));
            }
            Err(e) => last = e,
        }
    }
    Err(format!("offscreen rendering unavailable: {last}"))
}

/// A device error since the last picture: the shared device is let go, the next call opens
/// a fresh one.
#[cfg(feature = "gui")]
fn take_device_error() -> Option<String> {
    let mut s = SHARED.lock().unwrap_or_else(|p| p.into_inner());
    let e = s.as_ref()?.flag.take()?;
    *s = None;
    Some(e)
}

#[cfg(not(feature = "gui"))]
fn take_device_error() -> Option<String> {
    None
}

/// The generation of the shared device, 0 when none is open.
#[cfg(feature = "gui")]
fn shared_gen() -> u64 {
    SHARED.lock().unwrap_or_else(|p| p.into_inner()).as_ref().map_or(0, |s| s.gen)
}

pub struct Out {
    pub image: ImageData,
    pub legend: Vec<Legend>,
    pub cut: usize,
}

pub struct PlanOut {
    pub out: Out,
    pub size: Size,
    /// The area the image really shows, metres.
    pub region: [f64; 4],
}

pub struct View3dOut {
    pub out: Out,
    pub legend_scope: String,
}

#[cfg(not(feature = "gui"))]
pub(super) fn gpu() -> Result<Rc<RenderState>, String> {
    Err("offscreen rendering unavailable: this build has no window or GPU".into())
}

#[cfg(feature = "gui")]
pub(super) fn gpu() -> Result<Rc<RenderState>, String> {
    GPU.with(|g| {
        if let Some((gen, rs)) = g.borrow().as_ref() {
            if *gen == shared_gen() {
                return Ok(rs.clone());
            }
        }
        let (gen, rs) = shared()?;
        let rs = Rc::new(rs);
        *g.borrow_mut() = Some((gen, rs.clone()));
        Ok(rs)
    })
}

/// A private editor over a copy of the scratch model, so the render cannot touch the original.
pub fn copy_of(src: &Editor, units: Units) -> Result<Editor, String> {
    let p = std::env::temp_dir().join(format!(
        "ddc_render_{}_{}.db",
        std::process::id(),
        COPY_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    src.model.snapshot_to(&p).map_err(|e| e.to_string())?;
    let ed = open_editor(&p, units).map_err(|e| format!("{e:?}"));
    let _ = std::fs::remove_file(&p);
    ed
}

/// A private editor over an already open model (file copy or in-memory), for callers that
/// built their own database.
pub fn editor_over(m: ddc_model::Model, units: Units) -> Result<Editor, String> {
    let mut ed = Editor::seeded(m).map_err(|e| e.to_string())?;
    ed.set_units(units);
    Ok(ed)
}

thread_local! {
    static CAPTURING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// True while an image is being painted for the agent: app overlays that are notes to the
/// person at the screen (marks, hints, badges of other modules) must not draw then.
pub fn capturing() -> bool {
    CAPTURING.with(std::cell::Cell::get)
}

pub(crate) struct CaptureGuard;

impl CaptureGuard {
    pub(crate) fn on() -> CaptureGuard {
        CAPTURING.with(|c| c.set(true));
        CaptureGuard
    }
}

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        CAPTURING.with(|c| c.set(false));
    }
}

fn app_for(ed: Editor, rs: &RenderState) -> EditorApp {
    let mut app = EditorApp::new(ed, Some(rs), "offscreen".to_string());
    app.prefs.theme = "light".into();
    app
}

/// Paints frames with the app UI, returns the canvas pixels (`view_rect`) cropped out, and their size.
fn capture(
    rs: &RenderState,
    app: &mut EditorApp,
    want: (u32, u32),
    mut step: impl FnMut(&mut EditorApp, &egui::Context, usize, egui::Rect),
) -> Result<(Vec<u8>, (u32, u32)), String> {
    let _one = PAINTING.lock().unwrap_or_else(|p| p.into_inner());
    // an error left by an earlier picture is not this one's
    let _ = take_device_error();
    let r = capture_now(rs, app, want, &mut step);
    match take_device_error() {
        Some(e) => Err(format!("offscreen rendering failed: {e}")),
        None => r,
    }
}

fn capture_now(
    rs: &RenderState,
    app: &mut EditorApp,
    want: (u32, u32),
    mut step: impl FnMut(&mut EditorApp, &egui::Context, usize, egui::Rect),
) -> Result<(Vec<u8>, (u32, u32)), String> {
    let _quiet = CaptureGuard::on();
    // a picture of the model is the canvas alone: without chrome it fills the window at any size
    let clean = std::mem::replace(&mut app.chrome.fold.clean, true);
    let r = capture_clean(rs, app, want, &mut step);
    app.chrome.fold.clean = clean;
    r
}

fn capture_clean(
    rs: &RenderState,
    app: &mut EditorApp,
    want: (u32, u32),
    mut step: &mut impl FnMut(&mut EditorApp, &egui::Context, usize, egui::Rect),
) -> Result<(Vec<u8>, (u32, u32)), String> {
    // a picture of the model carries no question marks
    let _marks = super::ui::point_marks::hide();
    let canvas = |app: &mut EditorApp, dim: (u32, u32), frames: usize, step: &mut dyn FnMut(&mut EditorApp, &egui::Context, usize, egui::Rect)| {
        let ctx = egui::Context::default();
        let mut seen = None;
        let px = shot::frame(rs, &ctx, dim, 1.0, frames, None, |c, i| {
            app.ui(c, Some(rs));
            if let Some(r) = view_rect() {
                step(app, c, i, r);
                seen = Some(r);
            }
        });
        (px, seen)
    };
    let big = (want.0 + PAD.0, want.1 + PAD.1);
    let (_, probe) = canvas(app, big, 3, &mut |_, _, _, _| {});
    let probe = probe.ok_or("the view has no canvas")?;
    let chrome = (
        big.0.saturating_sub(probe.width() as u32),
        big.1.saturating_sub(probe.height() as u32),
    );
    let mut dim = (want.0 + chrome.0, want.1 + chrome.1);
    let (mut px, mut seen) = canvas(app, dim, 8, &mut step);
    // panels need not keep their width when the window shrinks: correct until the canvas is `want`
    for _ in 0..4 {
        let Some(off) = seen.and_then(|r| canvas_off(r, want)) else { break };
        dim = (shifted(dim.0, off.0), shifted(dim.1, off.1));
        (px, seen) = canvas(app, dim, 8, &mut step);
    }
    let r = seen.ok_or("the view has no canvas")?;
    let (x0, y0) = (r.min.x.max(0.0) as u32, r.min.y.max(0.0) as u32);
    let w = (r.width() as u32).min(dim.0.saturating_sub(x0));
    let h = (r.height() as u32).min(dim.1.saturating_sub(y0));
    if w == 0 || h == 0 {
        return Err("empty view".into());
    }
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in y0..y0 + h {
        let s = ((y * dim.0 + x0) * 4) as usize;
        out.extend_from_slice(&px[s..s + (w * 4) as usize]);
    }
    Ok((out, (w, h)))
}

/// How far the canvas `r` is from `want` in whole pixels, `None` when it is `want`.
pub(crate) fn canvas_off(r: egui::Rect, want: (u32, u32)) -> Option<(i64, i64)> {
    let off = (
        i64::from(want.0) - r.width().round() as i64,
        i64::from(want.1) - r.height().round() as i64,
    );
    (off != (0, 0)).then_some(off)
}

/// A window side `v` grown by `by` pixels, at least one.
pub(crate) fn shifted(v: u32, by: i64) -> u32 {
    (i64::from(v) + by).clamp(1, i64::from(u32::MAX)) as u32
}

fn image(rgba: &[u8], (w, h): (u32, u32)) -> Result<ImageData, String> {
    Ok(ImageData {
        mime: "image/png",
        bytes: Arc::new(encode_png(rgba, w, h)?),
        width: w,
        height: h,
    })
}

fn badge(ctx: &egui::Context, at: egui::Pos2, label: &str) {
    let p = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("render_legend"),
    ));
    p.circle_filled(at, 9.0, Color32::from_rgb(200, 40, 40)); // design-lint: data
    p.text(
        at,
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(11.0), // design-lint: data
        Color32::WHITE, // design-lint: data
    );
}

fn level_elev(ed: &Editor, id: i64) -> f64 {
    ed.levels()
        .into_iter()
        .find(|l| l.0 == id)
        .map_or(0.0, |l| l.2)
}

/// `wall:N` boxes (metres) on the editor's current level.
fn wall_box(ed: &Editor, w: &crate::editor::Wall) -> Aabb {
    let z0 = level_elev(ed, ed.level) + w.base_offset;
    let pad = 0.1;
    Aabb {
        min: Vec3::new(
            (w.a.x.min(w.b.x) - pad) as f32,
            (w.a.y.min(w.b.y) - pad) as f32,
            z0 as f32,
        ),
        max: Vec3::new(
            (w.a.x.max(w.b.x) + pad) as f32,
            (w.a.y.max(w.b.y) + pad) as f32,
            (z0 + w.height.max(0.1)) as f32,
        ),
    }
}

fn union(a: Aabb, b: Aabb) -> Aabb {
    Aabb {
        min: a.min.min(b.min),
        max: a.max.max(b.max),
    }
}

pub fn plan(src: &Editor, units: Units, req: &PlanReq) -> Result<PlanOut, String> {
    let rs = gpu()?;
    let mut ed = copy_of(src, units)?;
    ed.set_level(req.level);
    let elev = level_elev(&ed, req.level);
    let region = match req.region {
        Some(r) => r,
        None => {
            let pts: Vec<_> = ed.walls.iter().flat_map(|w| [w.a, w.b]).collect();
            if pts.is_empty() {
                return Err("nothing to draw on this level: no walls; pass an explicit region".into());
            }
            let f = |g: fn(f64, f64) -> f64, k: fn(&ddc_geom::V2) -> f64, init| {
                pts.iter().map(k).fold(init, |a, b| g(a, b))
            };
            [
                f(f64::min, |p| p.x, f64::MAX) - 1.0,
                f(f64::min, |p| p.y, f64::MAX) - 1.0,
                f(f64::max, |p| p.x, f64::MIN) + 1.0,
                f(f64::max, |p| p.y, f64::MIN) + 1.0,
            ]
        }
    };
    let size = rq::plan_size(region[2] - region[0], region[3] - region[1], req.scale);
    let (cx, cy) = ((region[0] + region[2]) / 2.0, (region[1] + region[3]) / 2.0);
    let items = ed
        .walls
        .iter()
        .filter(|w| {
            let (x0, x1) = (w.a.x.min(w.b.x), w.a.x.max(w.b.x));
            let (y0, y1) = (w.a.y.min(w.b.y), w.a.y.max(w.b.y));
            x1 >= region[0] && x0 <= region[2] && y1 >= region[1] && y0 <= region[3]
        })
        .map(|w| {
            (
                format!("wall:{}", w.id),
                "wall".to_string(),
                [(w.a.x + w.b.x) / 2.0, (w.a.y + w.b.y) / 2.0, elev],
            )
        })
        .collect();
    let (legend, cut) = rq::build_legend(items);
    let mut app = app_for(ed, &rs);
    app.view3d = false;
    let scale = size.scale;
    let marks = legend.clone();
    let (rgba, dim) = capture(&rs, &mut app, (size.w, size.h), |app, ctx, _, r| {
        app.plan.center = v2(cx, cy);
        app.plan.scale = scale;
        let c = r.center();
        for m in &marks {
            let at = egui::pos2(
                c.x + ((m.at[0] - cx) * scale) as f32,
                c.y - ((m.at[1] - cy) * scale) as f32,
            );
            if r.contains(at) {
                badge(ctx, at, &m.label);
            }
        }
    })?;
    let shown = [
        cx - dim.0 as f64 / scale / 2.0,
        cy - dim.1 as f64 / scale / 2.0,
        cx + dim.0 as f64 / scale / 2.0,
        cy + dim.1 as f64 / scale / 2.0,
    ];
    Ok(PlanOut {
        out: Out {
            image: image(&rgba, dim)?,
            legend,
            cut,
        },
        size,
        region: shown,
    })
}

pub fn view3d(src: &Editor, units: Units, req: &Render3d) -> Result<View3dOut, String> {
    let rs = gpu()?;
    let ed = copy_of(src, units)?;
    let level = ed.level;
    let refs = match &req.target {
        Target::All => None,
        Target::Refs(r) => Some(r.clone()),
    };
    let mut target: Option<Aabb> = None;
    let mut items = vec![];
    for w in &ed.walls {
        let key = format!("wall:{}", w.id);
        if refs.as_ref().is_some_and(|r| !r.contains(&key)) {
            continue;
        }
        let b = wall_box(&ed, w);
        target = Some(target.map_or(b, |t| union(t, b)));
        let z = (b.min.z + b.max.z) as f64 / 2.0;
        items.push((
            key,
            "wall".to_string(),
            [(w.a.x + w.b.x) / 2.0, (w.a.y + w.b.y) / 2.0, z],
        ));
    }
    if let Some(r) = &refs {
        if let Some(bad) = r
            .iter()
            .find(|k| !items.iter().any(|i: &(String, String, [f64; 3])| &i.0 == *k))
        {
            return Err(format!(
                "cannot frame {bad}: render_3d targets wall refs on the current level (all frames the whole model)"
            ));
        }
    }
    let (legend, cut) = rq::build_legend(items);
    let mut app = app_for(ed, &rs);
    app.view3d = true;
    let want = rq::size_3d(req.max_px);
    let marks = legend.clone();
    let req = req.clone();
    let (rgba, dim) = capture(&rs, &mut app, want, |app, ctx, i, r| {
        if i < 1 {
            return;
        }
        let aspect = r.width() / r.height().max(1.0);
        let b = target.or(app.scene.focus).or(app.scene.bounds);
        if let Some(b) = b {
            app.view.camera = rq::camera_for(&req, &b, (r.width() as u32, r.height() as u32));
            for m in &marks {
                let n = app
                    .view
                    .camera
                    .project(Vec3::new(m.at[0] as f32, m.at[1] as f32, m.at[2] as f32), aspect);
                if n.x.abs() <= 1.0 && n.y.abs() <= 1.0 {
                    let at = egui::pos2(
                        r.min.x + (n.x + 1.0) / 2.0 * r.width(),
                        r.min.y + (1.0 - n.y) / 2.0 * r.height(),
                    );
                    badge(ctx, at, &m.label);
                }
            }
        }
    })?;
    Ok(View3dOut {
        out: Out {
            image: image(&rgba, dim)?,
            legend,
            cut,
        },
        legend_scope: format!("walls on level {level}"),
    })
}

/// What a pose image carries besides the drawing: elements shown selected, and numbered
/// badges at image pixels `(px, py, text)`.
pub struct Overlay<'a> {
    pub select: &'a [Sel],
    pub marks: &'a [(f64, f64, String)],
}

fn badges(ctx: &egui::Context, r: egui::Rect, ov: &Overlay<'_>) {
    for (x, y, t) in ov.marks {
        let at = egui::pos2(r.min.x + *x as f32, r.min.y + *y as f32);
        if r.contains(at) {
            badge(ctx, at, t);
        }
    }
}

/// The plan of `level` as the screen draws it, `centre` metres in the middle of an image of
/// `want` pixels at `scale` pixels per metre. Returns the image and its real size.
pub fn plan_pose(
    src: &Editor,
    units: Units,
    level: i64,
    centre: (f64, f64),
    scale: f64,
    want: (u32, u32),
    ov: &Overlay<'_>,
) -> Result<(ImageData, (u32, u32)), String> {
    plan_pose_on(copy_of(src, units)?, level, centre, scale, want, ov)
}

/// `plan_pose` on an editor the caller owns and may change: no second copy of the database.
pub fn plan_pose_on(
    mut ed: Editor,
    level: i64,
    centre: (f64, f64),
    scale: f64,
    want: (u32, u32),
    ov: &Overlay<'_>,
) -> Result<(ImageData, (u32, u32)), String> {
    let rs = gpu()?;
    ed.set_level(level);
    ed.selection = ov.select.to_vec();
    let mut app = app_for(ed, &rs);
    app.view3d = false;
    let (rgba, dim) = capture(&rs, &mut app, want, |app, ctx, _, r| {
        app.plan.center = v2(centre.0, centre.1);
        app.plan.scale = scale;
        badges(ctx, r, ov);
    })?;
    Ok((image(&rgba, dim)?, dim))
}

/// Section or elevation view `view` as the screen draws it, `centre` = (distance along the
/// line, height) in the middle of the image.
pub fn section_pose(
    src: &Editor,
    units: Units,
    view: i64,
    centre: (f64, f64),
    scale: f64,
    want: (u32, u32),
    ov: &Overlay<'_>,
) -> Result<(ImageData, (u32, u32)), String> {
    section_pose_on(copy_of(src, units)?, view, centre, scale, want, ov)
}

/// `section_pose` on an editor the caller owns and may change.
pub fn section_pose_on(
    mut ed: Editor,
    view: i64,
    centre: (f64, f64),
    scale: f64,
    want: (u32, u32),
    ov: &Overlay<'_>,
) -> Result<(ImageData, (u32, u32)), String> {
    let rs = gpu()?;
    ed.selection = ov.select.to_vec();
    let mut app = app_for(ed, &rs);
    app.open_view(view);
    let (rgba, dim) = capture(&rs, &mut app, want, |app, ctx, _, r| {
        app.sview.pin(view, v2(centre.0, centre.1), scale);
        badges(ctx, r, ov);
    })?;
    Ok((image(&rgba, dim)?, dim))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::render_req::{parse_3d, MAX_PIXELS, MAX_SIDE};
    use serde_json::json;

    fn scene() -> Editor {
        let mut ed = Editor::new_in_memory().unwrap();
        crate::shot_scenes::build(&mut ed, "all3d").unwrap();
        ed.reload().unwrap();
        ed
    }

    /// Needs a graphics adapter; without one the case is skipped, like the other headless shots.
    fn have_gpu() -> bool {
        gpu().is_ok()
    }

    // Seen live: every assistant session ran on a new thread with its own device, never freed,
    // until the GPU ran out of memory and the panic ended the session
    #[cfg(feature = "gui")]
    #[test]
    fn sessions_on_new_threads_share_one_device() {
        if !have_gpu() {
            return;
        }
        let ed = scene();
        let req = parse_3d(&json!({"max_px": 160})).unwrap();
        view3d(&ed, Units::default(), &req).unwrap();
        let first = shared_gen();
        for _ in 0..6 {
            let g = std::thread::spawn(|| {
                let ed = scene();
                let req = parse_3d(&json!({"max_px": 160})).unwrap();
                view3d(&ed, Units::default(), &req).map(|_| shared_gen())
            })
            .join()
            .expect("a render on a session thread must not panic");
            assert_eq!(g.unwrap(), first, "a new thread opened its own device");
        }
    }

    #[cfg(feature = "gui")]
    #[test]
    fn a_device_error_fails_the_picture_and_reopens_the_device() {
        if !have_gpu() {
            return;
        }
        let before = shared_gen();
        SHARED.lock().unwrap().as_ref().unwrap().flag.mark("device error: Not enough memory left".into());
        assert!(take_device_error().is_some_and(|e| e.contains("memory")));
        let ed = scene();
        let r = view3d(&ed, Units::default(), &parse_3d(&json!({"max_px": 160})).unwrap());
        assert!(r.is_ok(), "{:?}", r.err());
        assert_ne!(shared_gen(), before);
    }

    #[test]
    fn offscreen_plan_png_size_and_legend() {
        if !have_gpu() {
            return;
        }
        let ed = scene();
        let levels = ed.levels();
        let req = rq::parse_plan(&json!({"level": levels[0].0, "scale": 40}), &levels).unwrap();
        let r = plan(&ed, Units::default(), &req).unwrap();
        let img = &r.out.image;
        assert!(img.bytes.starts_with(&[0x89, b'P', b'N', b'G']));
        assert!(img.width <= MAX_SIDE && img.height <= MAX_SIDE);
        assert!(img.width as u64 * img.height as u64 <= MAX_PIXELS);
        assert!(!r.out.legend.is_empty());
        assert!(r.out.legend.iter().all(|l| l.reference.starts_with("wall:")));
        assert!(r.size.scale > 0.0);
    }

    #[test]
    fn render_3d_renders_the_whole_model_and_a_target() {
        if !have_gpu() {
            return;
        }
        let ed = scene();
        let id = ed.walls[0].id;
        for a in [json!({"max_px": 320}), json!({"target": {"refs": [format!("wall:{id}")]}, "max_px": 320})] {
            let r = view3d(&ed, Units::default(), &parse_3d(&a).unwrap()).unwrap();
            assert!(r.out.image.width > 0 && r.out.image.width <= 320);
        }
        let bad = parse_3d(&json!({"target": {"refs": ["door:1"]}})).unwrap();
        assert!(view3d(&ed, Units::default(), &bad).err().expect("bad ref must fail").contains("door:1"));
    }
}
