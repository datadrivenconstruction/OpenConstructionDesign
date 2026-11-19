// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Report images of a loaded project (`--file … --screenshot … --view`): the plan of the ground
//! floor, a 3/4 view from the south-west and a vertical section through the long axis. Each is
//! the view canvas alone, cropped out of the painted app at exactly the requested size.

use crate::editor::{sec, Editor};
use crate::headless_file::{FileArgs, Loaded};
use crate::shell::EditorApp;
use crate::shot;
use ddc_geom::v2;
use ddc_model::SectionSpec;
use ddc_render::{Aabb, OrbitCamera};
use eframe::egui::{self, Color32};
use eframe::egui_wgpu::RenderState;
use serde_json::{json, Value};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use web_time::Instant;

/// Default image size of `--file` screenshots.
pub const SIZE: (u32, u32) = (1600, 1000);
/// `--view` values.
pub const VIEWS: [&str; 5] = ["plan", "3d", "section", "both", "all"];
/// Chrome around the canvas is measured on a first pass at this much extra room.
const PAD: (u32, u32) = (900, 600);
const STEADY_FRAMES: usize = 5;
/// Section line past the building on both ends, and the view depth past its far side, metres.
const MARGIN: f64 = 2.0;
/// The 3D camera: azimuth from the south-west, a 3/4 elevation.
const YAW_DEG: f32 = -135.0;
const PITCH_DEG: f32 = 28.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shot {
    Plan,
    View3d,
    Section,
}

impl Shot {
    pub fn key(self) -> &'static str {
        match self {
            Shot::Plan => "plan",
            Shot::View3d => "3d",
            Shot::Section => "section",
        }
    }
}

/// The images one `--view` value asks for, in painting order.
pub fn shots(view: &str) -> &'static [Shot] {
    match view {
        "3d" => &[Shot::View3d],
        "section" => &[Shot::Section],
        "both" => &[Shot::Plan, Shot::View3d],
        "all" => &[Shot::Plan, Shot::View3d, Shot::Section],
        _ => &[Shot::Plan],
    }
}

/// `out` for the first image of a multi-view request, `<stem>_3d.png` / `<stem>_section.png`
/// for the others; a single view always writes `out`.
pub fn out_path(out: &Path, view: &str, s: Shot) -> PathBuf {
    if shots(view).len() == 1 || s == Shot::Plan {
        return out.to_path_buf();
    }
    let stem = out
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    out.with_file_name(format!("{stem}_{}.png", s.key()))
}

thread_local! {
    static PAPER: Cell<bool> = const { Cell::new(false) };
}

/// The 3D ground while a report image is painted on this thread: plain white.
pub fn paper() -> Option<Color32> {
    PAPER.with(Cell::get).then_some(Color32::WHITE)
}

/// The plan level: `pick` is a level name or a storey number counted from the ground floor
/// (0 ground, 1 the next one up, -1 the first basement); none picks the ground floor, the
/// lowest level at or above elevation 0.
pub fn plan_level(ed: &Editor, pick: Option<&str>) -> Result<i64, String> {
    let mut lv = ed.levels();
    if lv.is_empty() {
        return Err("the project has no levels".into());
    }
    lv.sort_by(|a, b| a.2.total_cmp(&b.2));
    let ground = lv.iter().position(|l| l.2 >= -0.05).unwrap_or(0);
    let Some(p) = pick.map(str::trim) else {
        return Ok(lv[ground].0);
    };
    if let Some(l) = lv.iter().find(|l| l.1.eq_ignore_ascii_case(p)) {
        return Ok(l.0);
    }
    let names = || lv.iter().map(|l| l.1.as_str()).collect::<Vec<_>>().join(", ");
    let n: i64 = p
        .parse()
        .map_err(|_| format!("--level {p}: no such level; levels: {}", names()))?;
    usize::try_from(ground as i64 + n)
        .ok()
        .and_then(|i| lv.get(i))
        .map(|l| l.0)
        .ok_or_else(|| format!("--level {p}: out of range; levels: {}", names()))
}

/// A section through the plan extent `[x0, y0, x1, y1]` along its long side at mid-depth,
/// looking north (or east when the building is deeper than wide), reaching past the far side.
pub fn long_section(bb: [f64; 4]) -> Option<SectionSpec> {
    let (cx, cy) = ((bb[0] + bb[2]) / 2.0, (bb[1] + bb[3]) / 2.0);
    let (a, b, far) = if bb[2] - bb[0] >= bb[3] - bb[1] {
        (
            v2(bb[0] - MARGIN, cy),
            v2(bb[2] + MARGIN, cy),
            v2(cx, bb[3] + MARGIN),
        )
    } else {
        (
            v2(cx, bb[3] + MARGIN),
            v2(cx, bb[1] - MARGIN),
            v2(bb[2] + MARGIN, cy),
        )
    };
    sec::spec_from("A-A", a, b, Some(far))
}

/// Perspective 3/4 view from the south-west framing the whole box.
pub fn camera_sw(b: &Aabb, aspect: f32) -> OrbitCamera {
    let mut c = OrbitCamera::default();
    c.yaw = YAW_DEG.to_radians();
    c.pitch = PITCH_DEG.to_radians();
    c.ortho = false;
    c.fit(b, aspect);
    c
}

/// Spread of the luminance (standard deviation, 0..255): near 0 for a blank image.
pub fn luma_spread(rgba: &[u8]) -> f64 {
    let n = (rgba.len() / 4).max(1) as f64;
    let (mut s, mut s2) = (0.0, 0.0);
    for p in rgba.chunks_exact(4) {
        let y = 0.299 * p[0] as f64 + 0.587 * p[1] as f64 + 0.114 * p[2] as f64;
        s += y;
        s2 += y * y;
    }
    let m = s / n;
    (s2 / n - m * m).max(0.0).sqrt()
}

/// `src` (`dim`) centred into `want`: cropped where larger, white where smaller.
fn fit_into(src: &[u8], dim: (u32, u32), want: (u32, u32)) -> Vec<u8> {
    if dim == want {
        return src.to_vec();
    }
    let mut out = vec![255u8; (want.0 * want.1 * 4) as usize];
    let (dx, dy) = (
        (dim.0 as i64 - want.0 as i64) / 2,
        (dim.1 as i64 - want.1 as i64) / 2,
    );
    for y in 0..want.1 as i64 {
        let sy = y + dy;
        if sy < 0 || sy >= dim.1 as i64 {
            continue;
        }
        for x in 0..want.0 as i64 {
            let sx = x + dx;
            if sx < 0 || sx >= dim.0 as i64 {
                continue;
            }
            let s = ((sy * dim.0 as i64 + sx) * 4) as usize;
            let d = ((y * want.0 as i64 + x) * 4) as usize;
            out[d..d + 4].copy_from_slice(&src[s..s + 4]);
        }
    }
    out
}

/// Pixels of `r` out of a `dim` frame.
fn crop(px: &[u8], dim: (u32, u32), r: egui::Rect) -> (Vec<u8>, (u32, u32)) {
    let (x0, y0) = (r.min.x.max(0.0).round() as u32, r.min.y.max(0.0).round() as u32);
    let w = (r.width().round() as u32).min(dim.0.saturating_sub(x0));
    let h = (r.height().round() as u32).min(dim.1.saturating_sub(y0));
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in y0..y0 + h {
        let s = ((y * dim.0 + x0) * 4) as usize;
        out.extend_from_slice(&px[s..s + (w * 4) as usize]);
    }
    (out, (w, h))
}

pub(crate) struct Timed {
    pub(crate) rgba: Vec<u8>,
    first_ms: f64,
    steady_ms: f64,
}

type Step<'a> = &'a mut dyn FnMut(&mut EditorApp, egui::Rect);

/// `frames` frames of the app at `dim`; the pixels of the last one and its canvas rectangle.
fn paint(
    rs: &RenderState,
    app: &mut EditorApp,
    ctx: &egui::Context,
    dim: (u32, u32),
    frames: usize,
    step: Step<'_>,
) -> (Vec<u8>, Option<egui::Rect>) {
    let mut seen = None;
    let px = shot::frame(rs, ctx, dim, 1.0, frames, None, |c, _| {
        if let Some(r) = seen {
            step(app, r);
        }
        // a report image shows the model, not the notes of loading it
        app.sh.toasts.live.clear();
        app.ui(c, Some(rs));
        seen = crate::ai::session::view_rect();
    });
    (px, seen)
}

/// Paints the app until the canvas is `want`, sets the view each frame through `step` (with
/// the canvas rectangle), and returns the canvas of the last steady frame.
pub(crate) fn capture(rs: &RenderState, app: &mut EditorApp, want: (u32, u32), step: Step<'_>) -> Result<Timed, String> {
    let big = (want.0 + PAD.0, want.1 + PAD.1);
    let (_, probe) = paint(rs, app, &egui::Context::default(), big, 3, &mut *step);
    let probe = probe.ok_or("the view has no canvas")?;
    let dim = (
        want.0 + big.0.saturating_sub(probe.width().round() as u32),
        want.1 + big.1.saturating_sub(probe.height().round() as u32),
    );
    let ctx = egui::Context::default();
    // the first image carries pipeline compiles and the first upload: timed apart from the
    // steady frames, the median of single painted frames (readback included)
    let t = Instant::now();
    let (mut px, mut seen) = paint(rs, app, &ctx, dim, 8, &mut *step);
    let first_ms = ms(t.elapsed().as_secs_f64() * 1000.0);
    let mut steady = Vec::with_capacity(STEADY_FRAMES);
    for _ in 0..STEADY_FRAMES {
        // symbols and refinements built in the background land in the saved frame
        std::thread::sleep(std::time::Duration::from_millis(100));
        let t = Instant::now();
        (px, seen) = paint(rs, app, &ctx, dim, 1, &mut *step);
        steady.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let r = seen.ok_or("the view has no canvas")?;
    let (c, cdim) = crop(&px, dim, r);
    if cdim.0 == 0 || cdim.1 == 0 {
        return Err("empty view".into());
    }
    Ok(Timed {
        rgba: fit_into(&c, cdim, want),
        first_ms,
        steady_ms: ms(crate::perf_bench::percentile(&mut steady, 50)),
    })
}

fn ms(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// One image of `s` from the loaded app at `size`; `info` gets what the image shows.
fn paint_one(
    rs: &RenderState,
    app: &mut EditorApp,
    s: Shot,
    size: (u32, u32),
    level: i64,
    info: &mut serde_json::Map<String, Value>,
) -> Result<Timed, String> {
    match s {
        Shot::Plan => {
            app.section = None;
            app.view3d = false;
            app.ed.set_level(level);
            let name = app.ed.levels().into_iter().find(|l| l.0 == level).map(|l| l.1);
            info.insert("plan_level".into(), json!(name));
            capture(rs, app, size, &mut |app, r| {
                app.plan.fit_in(&app.ed, r.size());
            })
        }
        Shot::View3d => {
            app.section = None;
            app.view3d = true;
            PAPER.with(|p| p.set(true));
            let r = capture(rs, app, size, &mut |app, r| {
                let aspect = r.width() / r.height().max(1.0);
                if let Some(b) = app.scene.bounds {
                    app.view.camera = camera_sw(&b, aspect);
                }
            });
            PAPER.with(|p| p.set(false));
            r
        }
        Shot::Section => {
            let bb = app
                .ed
                .model
                .plan_bbox()
                .map_err(|e| e.to_string())?
                .ok_or("nothing to cut: the project has no bodies")?;
            let spec = long_section(bb).ok_or("the building has no extent to cut")?;
            let id = app.ed.add_view(&spec).ok_or("the section view was not added")?;
            app.ed.status = None;
            app.ed.status_sev = None;
            app.open_view(id);
            app.sview.refit();
            info.insert(
                "section".into(),
                json!({"name": spec.name, "a": [spec.a.0, spec.a.1], "b": [spec.b.0, spec.b.1], "depth": spec.depth}),
            );
            capture(rs, app, size, &mut |_, _| {})
        }
    }
}

/// The images `fa.view` asks for, written next to `--screenshot`; returns the report's
/// `render` object (without the device timings the caller adds).
pub fn render(l: Loaded, fa: &FileArgs, rs: &RenderState, backend: String) -> Result<Value, String> {
    let out = fa.screenshot.clone().ok_or("no --screenshot path")?;
    let level = plan_level(&l.ed, fa.level.as_deref())?;
    let mut app = EditorApp::new(l.ed, Some(rs), backend);
    app.plan.drawings = l.drawings;
    app.prefs.theme = "light".into();
    app.ed.status = None;
    let mut info = serde_json::Map::new();
    let mut pngs = vec![];
    let mut timing = serde_json::Map::new();
    for &s in shots(&fa.view) {
        let path = out_path(&out, &fa.view, s);
        let t = paint_one(rs, &mut app, s, fa.size, level, &mut info)?;
        shot::save_png(&path, fa.size, &t.rgba)?;
        pngs.push(path.display().to_string());
        let k = match s {
            Shot::Plan => "plan",
            Shot::View3d => "view3d",
            Shot::Section => "section",
        };
        timing.insert(format!("{k}_frame_ms"), json!(t.steady_ms));
        timing.insert(format!("{k}_first_ms"), json!(t.first_ms));
    }
    let mut v = json!({
        "size": [fa.size.0, fa.size.1], "png": pngs,
        "plan_frame_ms": null, "view3d_frame_ms": null, "section_frame_ms": null,
        "plan_first_ms": null, "view3d_first_ms": null, "section_first_ms": null,
    });
    if let Some(m) = v.as_object_mut() {
        m.extend(timing);
        m.extend(info);
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::plan::{compile_one, CompileCx, PlanOp};
    use crate::ai::project_plan::ProjectOp;

    #[test]
    fn views_and_paths() {
        let out = Path::new("d/o.png");
        assert_eq!(shots("all"), &[Shot::Plan, Shot::View3d, Shot::Section]);
        assert_eq!(out_path(out, "all", Shot::Plan), Path::new("d/o.png"));
        assert_eq!(out_path(out, "all", Shot::Section), Path::new("d/o_section.png"));
        assert_eq!(out_path(out, "both", Shot::View3d), Path::new("d/o_3d.png"));
        assert_eq!(out_path(out, "section", Shot::Section), Path::new("d/o.png"));
    }

    #[test]
    fn the_section_runs_along_the_long_side() {
        let wide = long_section([0.0, 0.0, 40.0, 12.0]).unwrap();
        assert_eq!((wide.a.1, wide.b.1), (6.0, 6.0));
        assert!(wide.a.0 < 0.0 && wide.b.0 > 40.0);
        // looks north, past the far wall
        assert!(sec::look(&wide).y > 0.99 && wide.depth > 6.0);
        let deep = long_section([0.0, 0.0, 10.0, 30.0]).unwrap();
        assert_eq!((deep.a.0, deep.b.0), (5.0, 5.0));
        assert!(sec::look(&deep).x > 0.99 && deep.depth > 5.0);
    }

    #[test]
    fn white_and_noise_spreads() {
        assert!(luma_spread(&[255u8; 400]) < 0.01);
        let mixed: Vec<u8> = (0..100).flat_map(|i| if i % 2 == 0 { [0, 0, 0, 255] } else { [255; 4] }).collect();
        assert!(luma_spread(&mixed) > 100.0);
    }

    /// A small generated office, applied the way the assistant applies a drafted project.
    fn small_project() -> Editor {
        let mut ed = Editor::new_in_memory().unwrap();
        ed.reload().unwrap();
        let input = json!({"typology": "office", "floors": 2, "lang": "en", "name": "Shot test"});
        let op = PlanOp::Project(ProjectOp { input, level: ed.level, slot0: 0, pid: "P1".into() });
        let mut slots = std::collections::HashMap::new();
        let sets = crate::ai::sets::Sets::default();
        let mut cx = CompileCx {
            ed: &mut ed,
            slots: &mut slots,
            sets: &sets,
            gens: &[],
            rules: &[],
            declared: Default::default(),
            skips: vec![],
        };
        compile_one(&mut cx, &op).map_err(|e| e.0).unwrap();
        ed
    }

    #[test]
    fn plan_3d_and_section_shots_of_a_generated_project() {
        let ed = small_project();
        assert!(ed.levels().len() >= 2);
        // the ground floor by default; storeys count from it
        let ground = plan_level(&ed, None).unwrap();
        assert_eq!(plan_level(&ed, Some("0")).unwrap(), ground);
        assert_ne!(plan_level(&ed, Some("1")).unwrap(), ground);
        assert!(plan_level(&ed, Some("9")).is_err());
        let rs = match crate::gfx::candidates(None, false)
            .into_iter()
            .find_map(|p| shot::render_state(p).ok())
        {
            Some(rs) => rs,
            None => return, // no graphics adapter: skipped, like the other headless shots
        };
        let dir = std::env::temp_dir().join("ddc_report_shot");
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("model.png");
        let argv: Vec<String> = ["ddc-app", "--file", "m.ddc", "--screenshot", out.to_str().unwrap(), "--view", "all"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let fa = crate::headless_file::parse(&argv).unwrap().unwrap();
        assert_eq!(fa.size, SIZE);
        let loaded = Loaded {
            ed,
            drawings: crate::drawing::Drawings::new(),
        };
        let v = render(loaded, &fa, &rs, "test".into()).unwrap();
        assert!(v["section"]["name"].is_string(), "{v}");
        let pngs: Vec<&str> = v["png"].as_array().unwrap().iter().map(|p| p.as_str().unwrap()).collect();
        assert_eq!(pngs.len(), 3, "{v}");
        for p in pngs {
            let mut r = png::Decoder::new(std::fs::File::open(p).unwrap()).read_info().unwrap();
            let mut buf = vec![0; r.output_buffer_size()];
            let info = r.next_frame(&mut buf).unwrap();
            assert_eq!((info.width, info.height), SIZE, "{p}");
            assert_eq!(info.color_type, png::ColorType::Rgba, "{p}");
            let spread = luma_spread(&buf[..info.buffer_size()]);
            assert!(spread > 6.0, "{p} looks blank: luminance spread {spread:.2}");
        }
        // DDC_KEEP_SHOTS keeps the images for a look
        if std::env::var_os("DDC_KEEP_SHOTS").is_none() {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
    /// For visual review: three species close up and a 200-tree park, one 3D PNG each in the
    /// directory named by `DDC_TREE_SHOTS` (the test does nothing without it, or without a
    /// graphics adapter).
    #[test]
    fn tree_shots_for_the_owner() {
        let Some(dir) = std::env::var_os("DDC_TREE_SHOTS").map(PathBuf::from) else { return };
        let Some(rs) = crate::gfx::candidates(None, false).into_iter().find_map(|p| shot::render_state(p).ok()) else {
            return;
        };
        std::fs::create_dir_all(&dir).unwrap();
        let shoot = |name: &str, places: &[(&str, f64, f64)]| {
            let mut ed = Editor::new_in_memory().unwrap();
            ed.reload().unwrap();
            let level = ed.model.add_level("Ground", 0.0).unwrap();
            let fam = ed.model.load_library_family("Tree").unwrap();
            let specs: Vec<ddc_model::CompSpec> = places
                .iter()
                .map(|(t, x, y)| ddc_model::CompSpec::at(level, ed.model.comp_type_id(fam, t).unwrap().unwrap(), *x, *y))
                .collect();
            ed.model.add_components(&specs).unwrap();
            ed.reload().unwrap();
            let out = dir.join(format!("{name}.png"));
            let argv: Vec<String> = ["ddc-app", "--file", "m.ddc", "--screenshot", out.to_str().unwrap(), "--view", "3d"]
                .iter()
                .map(|s| s.to_string())
                .collect();
            let fa = crate::headless_file::parse(&argv).unwrap().unwrap();
            let loaded = Loaded { ed, drawings: crate::drawing::Drawings::new() };
            let v = render(loaded, &fa, &rs, "test".into()).unwrap();
            assert!(out.exists(), "{v}");
        };
        for t in ["Oak A", "Spruce A", "Date Palm A"] {
            shoot(&t.to_lowercase().replace(' ', "_"), &[(t, 0.0, 0.0)]);
        }
        let mix = ["Oak A", "Spruce A", "Birch A", "Linden B", "Fir A", "Maple A", "Willow A", "Poplar A", "Cypress A", "Elm B"];
        let park: Vec<(&str, f64, f64)> = (0..200).map(|k| (mix[(k * 7 + k / 10) % mix.len()], (k % 20) as f64 * 7.0, (k / 20) as f64 * 7.0)).collect();
        shoot("park_200", &park);
    }
}
