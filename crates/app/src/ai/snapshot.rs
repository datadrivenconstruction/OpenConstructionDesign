// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `view_snapshot`: an image of the model from a named pose, drawn by the same code as the
//! window and without one. Plans and sections are the app's own views painted offscreen
//! (`render_off`); 3D goes through `ddc_render::headless::render_rgba_on`. The window answers
//! only for the screen's own camera. The pose maths is `pose`.

use super::fmt::Out;
use super::plan::ElRef;
use super::pose::{self, LabelPx, Pose, PoseResolved, Refusal};
use super::render_off::{self, Overlay};
use super::session::{encode_png, Core, Done, FromWorker};
use crate::editor::{Editor, Sel};
use crate::units::Units;
use ddc_ai::ImageData;
use ddc_render::glam::Vec3;
use ddc_render::headless::{self, Look};
use ddc_render::{OrbitCamera, Quality, Ray, Scene};
use eframe::egui_wgpu::RenderState;
use serde_json::{json, Value as J};
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

/// Longest side of an image the model may ask for.
pub const MAX_SIDE: u32 = 1280;
/// A picture heavier than this is drawn again, smaller.
pub const MAX_PNG: usize = 1_500_000;
/// A label is hidden when something this much nearer than its point stands in the way.
const SEE_TOL: f32 = 0.01;
/// Highlight colour of 3D images; plans use the screen's selection look.
pub const HIGHLIGHT_RGBA: [u8; 4] = [230, 90, 20, 255];
const WINDOW_WAIT: Duration = Duration::from_secs(5);
/// Each retry draws at this share of the previous size.
const SHRINK: [f64; 6] = [1.0, 0.8, 0.64, 0.5, 0.4, 0.32];

thread_local! {
    // a window that did not answer once is not waited for again in this session
    static WINDOW_SILENT: Cell<bool> = const { Cell::new(false) };
}

/// A request names a pose, highlights or labels; without any the screen is shown as it is.
#[cfg(test)]
pub(super) fn is_pose_request(a: &J) -> bool {
    ["pose", "highlight", "labels"].iter().any(|k| !a[*k].is_null())
}

/// Frame size for a longest side: 16:10, never above the cap.
pub fn frame_size(max_px: u32) -> (u32, u32) {
    let w = max_px.clamp(16, MAX_SIDE);
    (w, ((w as f64 * 0.625).round() as u32).max(16))
}

fn strings(a: &J, k: &str) -> Result<Vec<String>, String> {
    match &a[k] {
        J::Null => Ok(vec![]),
        J::Array(v) => v
            .iter()
            .map(|s| {
                s.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| format!("{k} takes text entries"))
            })
            .collect(),
        _ => Err(format!("{k} must be a list of text")),
    }
}

fn unavailable(why: &str) -> Refusal {
    Refusal::new(
        "snapshot_unavailable",
        why,
        "enable --safe-graphics (software rendering) or run where a graphics adapter exists",
    )
}

/// The refusal on the wire (typed code, reason as `detail`).
fn wire_of(e: &Refusal) -> String {
    e.wire()
}

// ---- sources ------------------------------------------------------------------------------

/// What a source is asked to draw.
pub struct Req<'a> {
    pub ed: &'a Editor,
    pub units: Units,
    pub pose: &'a Pose,
    /// `None` for the screen's own camera.
    pub resolved: Option<&'a PoseResolved>,
    pub size: (u32, u32),
    /// Elements to show highlighted.
    pub select: &'a [Sel],
    pub labels: &'a [LabelPx],
}

pub struct Rendered {
    pub image: ImageData,
    pub size: (u32, u32),
    /// Per label: `Some(true)` when something stands in front of it; `None` when the image
    /// cannot tell (plans, sections, out of frame).
    pub hidden: Vec<Option<bool>>,
}

pub trait SnapshotSource {
    fn render(&mut self, req: &Req<'_>) -> Result<Rendered, Refusal>;
}

/// Draws without a window: plans and sections as the app paints them, 3D with the renderer
/// of the window on a device of its own, created on first use and kept for the session.
pub struct HeadlessSource {
    gpu: fn() -> Result<Rc<RenderState>, String>,
    /// An editor the source may consume for the next plan or section picture, so that
    /// picture needs no second copy of the database.
    owned: Option<Editor>,
}

impl HeadlessSource {
    pub fn new() -> HeadlessSource {
        HeadlessSource { gpu: render_off::gpu, owned: None }
    }

    /// The next plan or section picture is painted on `ed` itself (a private copy the caller
    /// made once); later pictures copy again.
    pub fn with_owned(mut self, ed: Editor) -> HeadlessSource {
        self.owned = Some(ed);
        self
    }

    /// A source on another way to a device (tests: one that has none).
    pub fn with_gpu(gpu: fn() -> Result<Rc<RenderState>, String>) -> HeadlessSource {
        HeadlessSource { gpu, owned: None }
    }
}

impl Default for HeadlessSource {
    fn default() -> Self {
        Self::new()
    }
}

fn marks_of(labels: &[LabelPx]) -> Vec<(f64, f64, String)> {
    labels
        .iter()
        .enumerate()
        .filter(|(_, l)| l.in_frame)
        .map(|(i, l)| (l.px, l.py, (i + 1).to_string()))
        .collect()
}

impl SnapshotSource for HeadlessSource {
    fn render(&mut self, r: &Req<'_>) -> Result<Rendered, Refusal> {
        let Some(p) = r.resolved else {
            return Err(Refusal::new(
                "current_unresolved",
                "the screen's camera is not known here",
                "ask for an explicit pose",
            ));
        };
        let rs = (self.gpu)().map_err(|e| unavailable(&e))?;
        let none = vec![None; r.labels.len()];
        let ui = |drawn: Result<(ImageData, (u32, u32)), String>| {
            drawn
                .map(|(image, size)| Rendered {
                    image,
                    size,
                    hidden: none.clone(),
                })
                .map_err(|e| unavailable(&e))
        };
        let marks = marks_of(r.labels);
        let ov = Overlay {
            select: r.select,
            marks: &marks,
        };
        let centre = p.region.map(|g| ((g[0] + g[2]) / 2.0, (g[1] + g[3]) / 2.0));
        match (r.pose, centre) {
            (Pose::Plan { level, .. }, Some(c)) => ui(match self.owned.take() {
                Some(ed) => render_off::plan_pose_on(ed, *level, c, p.px_per_m, p.size, &ov),
                None => render_off::plan_pose(r.ed, r.units, *level, c, p.px_per_m, p.size, &ov),
            }),
            (Pose::Section { view_def }, Some(c)) => ui(match self.owned.take() {
                Some(ed) => render_off::section_pose_on(ed, *view_def, c, p.px_per_m, p.size, &ov),
                None => render_off::section_pose(r.ed, r.units, *view_def, c, p.px_per_m, p.size, &ov),
            }),
            _ => view3d(r, p, &rs),
        }
    }
}

/// The orbit camera that sees what the pose sees.
pub fn camera_of(p: &PoseResolved) -> OrbitCamera {
    let (t, yaw, pitch, dist) = p.orbit();
    let b = p.bounds_world;
    let radius = ((b[3] - b[0]).powi(2) + (b[4] - b[1]).powi(2) + (b[5] - b[2]).powi(2)).sqrt() / 2.0;
    OrbitCamera {
        target: Vec3::new(t[0] as f32, t[1] as f32, t[2] as f32),
        yaw: yaw as f32,
        pitch: pitch as f32,
        distance: dist as f32,
        fov_y: (p.fov_deg as f32).to_radians(),
        scene_radius: (radius as f32).max(1.0),
        ortho: p.ortho,
    }
}

fn v3(p: [f64; 3]) -> Vec3 {
    Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32)
}

/// True when something other than the label's own element stands between the eye and its
/// point, inside the section box when there is one. The nearest hit that is the element
/// itself, or lies beyond the point, means the label is seen.
pub fn label_hidden(scene: &Scene, p: &PoseResolved, at: [f64; 3], own: Option<(u32, u32)>) -> bool {
    let to = v3(at);
    let (mut origin, dir) = if p.ortho {
        let (_, _, f) = pose::axes(p.eye, p.target, p.up);
        let f = v3(f);
        let b = p.bounds_world;
        let reach = ((b[3] - b[0]).hypot(b[4] - b[1]).hypot(b[5] - b[2]) * 2.0 + 10.0) as f32;
        (to - f * reach, f)
    } else {
        let eye = v3(p.eye);
        (eye, (to - eye).normalize_or_zero())
    };
    if dir == Vec3::ZERO {
        return false;
    }
    let mut left = (to - origin).length();
    let inside = |q: Vec3| match p.section_box {
        None => true,
        Some(b) => (0..3).all(|k| q[k] >= b[k] as f32 - 1e-4 && q[k] <= b[k + 3] as f32 + 1e-4),
    };
    // a hit outside the section box is cut away in the image: look past it
    for _ in 0..16 {
        let Some(h) = scene.pick(&Ray { origin, dir }) else {
            return false;
        };
        if h.t >= left - SEE_TOL {
            return false;
        }
        if own.is_some_and(|(a, b)| h.element >= a && h.element < b) {
            return false;
        }
        if inside(h.point) {
            return true;
        }
        let step = h.t + 1e-3;
        origin += dir * 1e-3 + dir * h.t;
        left -= step;
    }
    false
}

fn view3d(r: &Req<'_>, p: &PoseResolved, rs: &RenderState) -> Result<Rendered, Refusal> {
    if !p.level_dz.is_empty() {
        return Err(Refusal::new(
            "unsupported_pose",
            "exploded levels cannot be drawn offscreen",
            "use iso or compass for the whole model",
        ));
    }
    let scene = r.ed.scene();
    let cam = camera_of(p);
    let hl: Vec<(u32, u32)> = r.select.iter().filter_map(|s| r.ed.element_range(*s)).collect();
    let look = Look {
        highlight: &hl,
        accent: Some(HIGHLIGHT_RGBA),
        clip: p.section_box.map(|b| b.map(|v| v as f32)),
        quality: Quality::High,
    };
    let img = headless::render_rgba_on(&rs.device, &rs.queue, &rs.adapter.get_info(), &scene, &cam, p.size, &look)
        .map_err(|e| unavailable(&e))?;
    let png = encode_png(&img.rgba, img.w, img.h).map_err(|e| unavailable(&e))?;
    let hidden = r
        .labels
        .iter()
        .map(|l| {
            l.in_frame.then(|| {
                let own = ElRef::parse(&l.reference)
                    .ok()
                    .and_then(|e| pose::sel_of(&e))
                    .and_then(|s| r.ed.element_range(s));
                label_hidden(&scene, p, l.world, own)
            })
        })
        .collect();
    Ok(Rendered {
        image: ImageData {
            mime: "image/png",
            bytes: std::sync::Arc::new(png),
            width: img.w,
            height: img.h,
        },
        size: (img.w, img.h),
        hidden,
    })
}

/// The screen's own picture: asks the UI thread, which owns the window.
pub struct WindowSource<'a> {
    pub core: &'a Core,
}

impl SnapshotSource for WindowSource<'_> {
    fn render(&mut self, r: &Req<'_>) -> Result<Rendered, Refusal> {
        let (tx, rx) = mpsc::channel();
        self.core.send(FromWorker::Snapshot {
            max_px: r.size.0.min(MAX_SIDE),
            reply: tx,
        });
        let silent = || {
            WINDOW_SILENT.with(|s| s.set(true));
            Refusal::new(
                "window_silent",
                "no window answered",
                "ask for an explicit pose, for example {compass:\"SE\"}, or open the editor window",
            )
        };
        let image = rx
            .recv_timeout(WINDOW_WAIT)
            .map_err(|_| silent())?
            .map_err(|e| unavailable(&e))?;
        WINDOW_SILENT.with(|s| s.set(false));
        let size = (image.width, image.height);
        Ok(Rendered {
            image,
            size,
            hidden: vec![None; r.labels.len()],
        })
    }
}

// ---- one picture --------------------------------------------------------------------------

pub struct Shot {
    pub resolved: PoseResolved,
    pub labels: Vec<LabelPx>,
    pub cut: usize,
    pub rendered: Rendered,
    /// Share of the asked size the picture was drawn at to stay under `MAX_PNG`.
    pub shrunk: f64,
}

#[allow(clippy::too_many_arguments)]
pub fn take(
    src: &mut dyn SnapshotSource,
    ed: &Editor,
    units: Units,
    facts: &pose::Facts,
    want: &Pose,
    label_specs: &[String],
    select: &[Sel],
    max_px: u32,
) -> Result<Shot, Refusal> {
    let via_query = |t: &str| crate::query::eval_tool(ed, t);
    let mut last_len = 0;
    for k in SHRINK {
        let size = frame_size(((max_px.clamp(16, MAX_SIDE)) as f64 * k).round() as u32);
        let mut resolved = pose::resolve_in(facts, want, size)?;
        let (mut labels, mut cut) = pose::labels_in(facts, &resolved, label_specs, &via_query)?;
        let rendered = src.render(&Req {
            ed,
            units,
            pose: want,
            resolved: Some(&resolved),
            size,
            select,
            labels: &labels,
        })?;
        if rendered.size != resolved.size {
            // a view painted by the app can come out a pixel off: the matrix follows the image
            resolved = pose::resolve_in(facts, want, rendered.size)?;
            (labels, cut) = pose::labels_in(facts, &resolved, label_specs, &via_query)?;
        }
        last_len = rendered.image.bytes.len();
        if last_len <= MAX_PNG {
            return Ok(Shot {
                resolved,
                labels,
                cut,
                rendered,
                shrunk: k,
            });
        }
    }
    Err(Refusal::new(
        "image_too_large",
        format!("the picture stays at {last_len} bytes after shrinking; the limit is {MAX_PNG}"),
        "ask for a smaller max_px or a plainer view",
    ))
}

/// FNV-1a 64 over bytes: the same value on every run, platform and toolchain.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3))
}

/// Stable hash of the raw RGBA pixels of a picture (not of its PNG encoding).
pub fn pixel_hash(img: &ImageData) -> Result<u64, String> {
    let mut r = png::Decoder::new(std::io::Cursor::new(&img.bytes[..]))
        .read_info()
        .map_err(|e| e.to_string())?;
    let mut buf = vec![0; r.output_buffer_size()];
    let f = r.next_frame(&mut buf).map_err(|e| e.to_string())?;
    Ok(fnv1a(&buf[..f.buffer_size()]))
}

fn audit_image(c: &Core, what: &str, extra: &str, img: &ImageData) {
    c.audit(
        what,
        None,
        Some(format!(
            "{extra}{}x{} {} bytes {:016x}",
            img.width,
            img.height,
            img.bytes.len(),
            fnv1a(&img.bytes)
        )),
        "ok",
    );
}

/// The reply of one picture: pose, label places, what was highlighted.
fn reply(c: &Core, shot: &Shot, highlight: &[ElRef]) -> super::fmt::Reply {
    let seen: HashMap<&str, Option<bool>> = shot
        .labels
        .iter()
        .zip(&shot.rendered.hidden)
        .map(|(l, h)| (l.reference.as_str(), *h))
        .collect();
    let mut o = Out::new(c.max_chars);
    o.put("width", &json!(shot.rendered.image.width));
    o.put("height", &json!(shot.rendered.image.height));
    o.put("pose", &shot.resolved.to_json());
    o.put(
        "labels",
        &pose::labels_json(&shot.labels, shot.cut, &|l| seen.get(l.reference.as_str()).copied().flatten()),
    );
    o.put(
        "highlight",
        &json!(highlight.iter().map(|r| r.text()).collect::<Vec<_>>()),
    );
    if !highlight.is_empty() {
        let [r, g, b, _] = HIGHLIGHT_RGBA;
        let look = match shot.resolved.kind {
            "plan" | "section" => "the screen's selection colour".to_string(),
            _ => format!("#{r:02x}{g:02x}{b:02x}"),
        };
        o.put("highlight_colour", &json!(look));
    }
    if shot.shrunk < 1.0 {
        o.put("drawn_at", &json!(shot.shrunk));
    }
    o.finish()
}

pub(super) fn request(c: &mut Core, a: &J) -> Result<Done, String> {
    let max_px = a["max_px"].as_u64().map_or(MAX_SIDE, |n| n.min(u64::from(MAX_SIDE)) as u32);
    let facts = pose::facts_of(&c.ed);
    let want = match &a["pose"] {
        J::Null => Pose::Current,
        p => pose::parse_pose(p, &facts).map_err(|e| wire_of(&e))?,
    };
    let specs = strings(a, "labels")?;
    let highlight = pose::parse_highlight(&strings(a, "highlight")?, &facts).map_err(|e| wire_of(&e))?;
    if matches!(want, Pose::Current) {
        if !highlight.is_empty() || !specs.is_empty() {
            return Err(pose::resolve_in(&facts, &want, frame_size(max_px))
                .err()
                .map_or_else(String::new, |e| wire_of(&e)));
        }
        return window(c, &facts, max_px);
    }
    let select: Vec<Sel> = highlight.iter().filter_map(pose::sel_of).collect();
    let units = c.view.units;
    let shot = take(
        &mut HeadlessSource::new(),
        &c.ed,
        units,
        &facts,
        &want,
        &specs,
        &select,
        max_px,
    )
    .map_err(|e| wire_of(&e))?;
    audit_image(c, "snapshot_pose", &format!("{} ", shot.resolved.kind), &shot.rendered.image);
    let reply = reply(c, &shot, &highlight);
    Ok(Done {
        reply,
        images: vec![shot.rendered.image],
    })
}

/// The screen as shown; without a window to ask, an isometric view of the model instead.
fn window(c: &mut Core, facts: &pose::Facts, max_px: u32) -> Result<Done, String> {
    if !WINDOW_SILENT.with(Cell::get) {
        let got = WindowSource { core: &*c }.render(&Req {
            ed: &c.ed,
            units: c.view.units,
            pose: &Pose::Current,
            resolved: None,
            size: (max_px, max_px),
            select: &[],
            labels: &[],
        });
        // the window answered with its own reason: that is the answer, not a reason to draw
        if let Err(e) = &got {
            if e.code != "window_silent" {
                return Err(wire_of(e));
            }
        }
        if let Ok(r) = got {
            audit_image(c, "snapshot", "", &r.image);
            let mut o = Out::new(c.max_chars);
            o.put("width", &json!(r.image.width));
            o.put("height", &json!(r.image.height));
            return Ok(Done {
                reply: o.finish(),
                images: vec![r.image],
            });
        }
    }
    let iso = Pose::Iso { az_deg: Side8Se::BEARING };
    let units = c.view.units;
    let shot = take(&mut HeadlessSource::new(), &c.ed, units, facts, &iso, &[], &[], max_px)
        .map_err(|e| wire_of(&e))?;
    audit_image(c, "snapshot_pose", "window_silent iso ", &shot.rendered.image);
    let mut o = Out::new(c.max_chars);
    o.put("width", &json!(shot.rendered.image.width));
    o.put("height", &json!(shot.rendered.image.height));
    o.put("pose", &shot.resolved.to_json());
    o.put(
        "note",
        &json!("no window answered, so this is an isometric view from the south-east, not the screen"),
    );
    Ok(Done {
        reply: o.finish(),
        images: vec![shot.rendered.image],
    })
}

/// South-east bearing of the fallback view.
struct Side8Se;

impl Side8Se {
    const BEARING: f64 = 135.0;
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_render::SceneMesh;
    use pose::{Facts, LevelFact, PointFact, RoomFact};

    fn level(id: i64, name: &str, elev: f64) -> LevelFact {
        LevelFact {
            id,
            name: name.into(),
            elev,
            height: 3.0,
            foot: Some([0.0, 0.0, 6.0, 4.0]),
        }
    }

    fn house() -> Facts {
        let ring = vec![[0.0, 0.0], [6.0, 0.0], [6.0, 4.0], [0.0, 4.0]];
        Facts {
            levels: vec![level(1, "L1", 0.0), level(2, "L2", 3.0)],
            rooms: vec![RoomFact {
                id: 21,
                level: 2,
                number: "2.14".into(),
                name: "Hall".into(),
                ring,
            }],
            points: vec![PointFact {
                kind: "wall",
                cat: "wall",
                id: 812,
                level: 1,
                at: [3.0, 0.0, 1.5],
                text: "wall:812".into(),
            }],
            views: vec![],
            bounds: Some([0.0, 0.0, 0.0, 6.0, 4.0, 6.0]),
        }
    }

    /// A box mesh `[x0, y0, z0, x1, y1, z1]` of one element.
    fn block(element: u32, b: [f32; 6]) -> SceneMesh {
        let [x0, y0, z0, x1, y1, z1] = b;
        let positions = vec![
            [x0, y0, z0],
            [x1, y0, z0],
            [x1, y1, z0],
            [x0, y1, z0],
            [x0, y0, z1],
            [x1, y0, z1],
            [x1, y1, z1],
            [x0, y1, z1],
        ];
        let q = |a: u32, b: u32, c: u32, d: u32| [[a, b, c], [a, c, d]];
        let triangles = [
            q(0, 3, 2, 1),
            q(4, 5, 6, 7),
            q(0, 1, 5, 4),
            q(1, 2, 6, 5),
            q(2, 3, 7, 6),
            q(3, 0, 4, 7),
        ]
        .concat();
        SceneMesh {
            element,
            tag: 0,
            positions,
            triangles,
        }
    }

    fn pose_of(f: &Facts, a: J) -> PoseResolved {
        let p = pose::parse_pose(&a, f).unwrap();
        pose::resolve_in(f, &p, (1000, 800)).unwrap()
    }

    #[test]
    fn the_hash_is_stable_and_overlays_know_when_to_hide() {
        // FNV-1a 64 reference values
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
        let img = |px: u8| {
            let bytes = encode_png(&[px, 0, 0, 255], 1, 1).unwrap();
            ImageData { mime: "image/png", bytes: std::sync::Arc::new(bytes), width: 1, height: 1 }
        };
        assert_eq!(pixel_hash(&img(7)).unwrap(), fnv1a(&[7, 0, 0, 255]));
        assert_ne!(pixel_hash(&img(7)).unwrap(), pixel_hash(&img(8)).unwrap());
        assert!(!render_off::capturing());
    }

    #[test]
    fn frame_size_is_capped_and_wide() {
        assert_eq!(frame_size(1280), (1280, 800));
        assert_eq!(frame_size(5000), (1280, 800));
        assert_eq!(frame_size(16), (16, 16));
        assert_eq!(frame_size(320), (320, 200));
    }

    #[test]
    fn only_pose_arguments_make_a_pose_request() {
        assert!(!is_pose_request(&json!({})));
        assert!(!is_pose_request(&json!({"max_px": 256})));
        assert!(is_pose_request(&json!({"pose": {"top": true}})));
        assert!(is_pose_request(&json!({"labels": ["door:*"]})));
    }

    #[test]
    fn label_behind_wall_hidden() {
        let f = house();
        // a front wall (element 1) south of a back wall (element 2); the model spans 0..6 x 0..4
        // and both storeys (0..6 m): the 30 degree compass eye looks over a one-storey wall
        let scene = Scene::new(vec![
            block(1, [-1.0, -1.1, 0.0, 7.0, -1.0, 6.0]),
            block(2, [-1.0, 1.9, 0.0, 7.0, 2.1, 6.0]),
        ]);
        let south = pose_of(&f, json!({"compass": "S"}));
        // S: the eye stands south of the model, looking north
        assert!(south.eye[1] < 0.0);
        let behind = [3.0, 2.0, 1.5];
        assert!(label_hidden(&scene, &south, behind, Some((2, 3))), "the front wall is in the way");
        let on_front = [3.0, -1.05, 1.5];
        assert!(!label_hidden(&scene, &south, on_front, Some((1, 2))), "a label on the wall in view is seen");
        let north = pose_of(&f, json!({"compass": "N"}));
        assert!(!label_hidden(&scene, &north, behind, Some((2, 3))), "from the north the back wall is first");
        assert!(label_hidden(&scene, &north, on_front, Some((1, 2))), "the back wall hides the front one");
        // an element that is not in the scene (a room) is hidden by anything in front of it
        assert!(label_hidden(&scene, &south, behind, None));
        assert!(!label_hidden(&scene, &south, [3.0, -3.0, 1.0], None), "nothing between the eye and a point outside");
        // a top view looks past walls that are not above the point
        let top = pose_of(&f, json!({"top": true}));
        assert!(!label_hidden(&scene, &top, behind, Some((2, 3))));
    }

    #[test]
    fn what_the_section_box_cuts_away_does_not_hide() {
        let f = house();
        // a roof slab over the room, above the cut at 0.65 of the storey
        let scene = Scene::new(vec![block(5, [-1.0, -1.0, 5.5, 7.0, 5.0, 5.7])]);
        let room = pose_of(&f, json!({"room": "2.14"}));
        let b = room.section_box.unwrap();
        assert!(b[5] < 5.5);
        let point = [3.0, 2.0, 4.0];
        assert!(!label_hidden(&scene, &room, point, None), "the roof is cut away in the image");
        let mut whole = room.clone();
        whole.section_box = None;
        assert!(label_hidden(&scene, &whole, point, None), "without the cut the roof is in the way");
    }

    #[test]
    fn the_orbit_camera_sees_what_the_pose_sees() {
        let f = house();
        for a in [
            json!({"compass": "SE"}),
            json!({"compass": "W", "eye_h": 1.6}),
            json!({"iso": 200.0}),
            json!({"top": true}),
            json!({"room": "2.14"}),
        ] {
            let p = pose_of(&f, a.clone());
            let cam = camera_of(&p);
            let aspect = p.size.0 as f32 / p.size.1 as f32;
            for q in [[0.0, 0.0, 0.0], [6.0, 4.0, 3.0], [3.0, 2.0, 1.0], [1.0, 3.0, 2.5]] {
                let n = cam.project(Vec3::new(q[0] as f32, q[1] as f32, q[2] as f32), aspect);
                let (px, py) = ((n.x as f64 + 1.0) / 2.0 * p.size.0 as f64, (1.0 - n.y as f64) / 2.0 * p.size.1 as f64);
                let [ex, ey, _] = p.project(q).unwrap();
                assert!(
                    (px - ex).abs() < 0.5 && (py - ey).abs() < 0.5,
                    "{a} {q:?}: camera {px:.2},{py:.2} vs pose {ex:.2},{ey:.2}"
                );
            }
        }
    }

    #[test]
    fn no_adapter_refusal() {
        let f = house();
        let ed = Editor::new_in_memory().unwrap();
        let mut src = HeadlessSource::with_gpu(|| Err("no graphics adapter".into()));
        for want in [
            Pose::Compass { side: pose::Side8::N, eye_h: None },
            Pose::Plan { level: 1, rect: None },
        ] {
            let e = take(&mut src, &ed, Units::default(), &f, &want, &[], &[], 640)
                .err()
                .expect("no adapter, no image");
            assert_eq!(e.code, "snapshot_unavailable");
            assert!(e.fix.contains("--safe-graphics"), "{}", e.fix);
        }
    }

    /// A source that draws a flat grey picture of the asked size and counts its calls.
    struct Fake {
        calls: usize,
        pad: usize,
    }

    impl SnapshotSource for Fake {
        fn render(&mut self, r: &Req<'_>) -> Result<Rendered, Refusal> {
            self.calls += 1;
            // the first picture is too heavy, whatever its size
            let heavy = if self.calls == 1 { self.pad } else { 100 };
            Ok(Rendered {
                image: ImageData {
                    mime: "image/png",
                    bytes: std::sync::Arc::new(vec![0; heavy]),
                    width: r.size.0,
                    height: r.size.1,
                },
                size: r.size,
                hidden: vec![None; r.labels.len()],
            })
        }
    }

    #[test]
    fn a_heavy_picture_is_drawn_again_smaller() {
        let f = house();
        let ed = Editor::new_in_memory().unwrap();
        let want = Pose::Top;
        let mut fake = Fake { calls: 0, pad: MAX_PNG + 1 };
        let s = take(&mut fake, &ed, Units::default(), &f, &want, &[], &[], 1280).unwrap();
        assert_eq!(fake.calls, 2);
        assert_eq!(s.shrunk, 0.8);
        assert_eq!(s.resolved.size, frame_size(1024));
        assert_eq!(s.rendered.size, s.resolved.size);
        let mut always = Fake { calls: 0, pad: MAX_PNG + 1 };
        always.calls = 0;
        struct Heavy;
        impl SnapshotSource for Heavy {
            fn render(&mut self, r: &Req<'_>) -> Result<Rendered, Refusal> {
                Ok(Rendered {
                    image: ImageData {
                        mime: "image/png",
                        bytes: std::sync::Arc::new(vec![0; MAX_PNG + 1]),
                        width: r.size.0,
                        height: r.size.1,
                    },
                    size: r.size,
                    hidden: vec![],
                })
            }
        }
        let e = take(&mut Heavy, &ed, Units::default(), &f, &want, &[], &[], 1280).err().unwrap();
        assert_eq!(e.code, "image_too_large");
    }

    #[test]
    fn a_picture_a_pixel_off_gets_a_matching_matrix() {
        struct Off;
        impl SnapshotSource for Off {
            fn render(&mut self, r: &Req<'_>) -> Result<Rendered, Refusal> {
                let size = (r.size.0 - 1, r.size.1);
                Ok(Rendered {
                    image: ImageData {
                        mime: "image/png",
                        bytes: std::sync::Arc::new(vec![0; 10]),
                        width: size.0,
                        height: size.1,
                    },
                    size,
                    hidden: vec![None; r.labels.len()],
                })
            }
        }
        let f = house();
        let ed = Editor::new_in_memory().unwrap();
        let s = take(&mut Off, &ed, Units::default(), &f, &Pose::Plan { level: 1, rect: None }, &[], &[], 1000).unwrap();
        assert_eq!(s.resolved.size, (999, 625));
    }

    fn gpu_scene() -> Editor {
        let mut ed = Editor::new_in_memory().unwrap();
        crate::shot_scenes::build(&mut ed, "all3d").unwrap();
        ed.reload().unwrap();
        ed
    }

    #[test]
    fn headless_plan_matches_window() {
        if render_off::gpu().is_err() {
            return;
        }
        let ed = gpu_scene();
        let facts = pose::facts_of(&ed);
        let level = facts.levels[0].id;
        let size = (640, 400);
        let p = pose::resolve_in(&facts, &Pose::Plan { level, rect: None }, size).unwrap();
        let g = p.region.unwrap();
        let c = ((g[0] + g[2]) / 2.0, (g[1] + g[3]) / 2.0);
        let ov = Overlay { select: &[], marks: &[] };
        let (img, dim) = render_off::plan_pose(&ed, Units::default(), level, c, p.px_per_m, size, &ov).unwrap();
        // the same view through the path of `--screenshot --view plan`
        let rs = render_off::gpu().unwrap();
        let mut app = crate::shell::EditorApp::new(gpu_scene(), Some(&rs), "offscreen".to_string());
        app.prefs.theme = "light".into();
        app.view3d = false;
        app.ed.set_level(level);
        let shot = crate::report_shot::capture(&rs, &mut app, size, &mut |app, _| {
            app.plan.center = ddc_geom::v2(c.0, c.1);
            app.plan.scale = p.px_per_m;
        })
        .unwrap();
        let theirs = encode_png(&shot.rgba, dim.0, dim.1).unwrap();
        assert_eq!(fnv1a(&img.bytes), fnv1a(&theirs), "same pixels as the screenshot path");
    }

    #[test]
    fn headless_3d_highlight_pixel() {
        if render_off::gpu().is_err() {
            return;
        }
        let ed = gpu_scene();
        let facts = pose::facts_of(&ed);
        let wall = facts.points.iter().find(|p| p.kind == "wall").expect("a wall").id;
        let want = Pose::Top;
        let size = 480;
        let go = |select: &[Sel], labels: &[String]| {
            take(&mut HeadlessSource::new(), &ed, Units::default(), &facts, &want, labels, select, size).unwrap()
        };
        let plain = go(&[], &[]);
        let hot = go(&[Sel::Wall(wall)], &[format!("wall:{wall}")]);
        assert_eq!(plain.resolved.size, hot.resolved.size);
        // the highlighted wall's own pixel, from the label place the answer gives
        let l = &hot.labels[0];
        assert!(l.in_frame);
        let px = |s: &Shot| {
            let img = &s.rendered.image;
            let d = ddc_io_png(&img.bytes);
            let (x, y) = (l.px.round() as usize, l.py.round() as usize);
            let k = (y * img.width as usize + x) * 4;
            [d[k], d[k + 1], d[k + 2]]
        };
        let lean = |p: [u8; 3]| i32::from(p[0]) - i32::from(p[2]);
        assert!(
            lean(px(&hot)) > lean(px(&plain)) + 15,
            "the wall takes the highlight colour: {:?} vs {:?}",
            px(&hot),
            px(&plain)
        );
    }

    /// RGBA rows of a PNG made by `encode_png`.
    fn ddc_io_png(bytes: &[u8]) -> Vec<u8> {
        let mut r = png::Decoder::new(std::io::Cursor::new(bytes)).read_info().unwrap();
        let mut buf = vec![0; r.output_buffer_size()];
        r.next_frame(&mut buf).unwrap();
        buf
    }
}
