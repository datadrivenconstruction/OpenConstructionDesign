// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The view a project opens with: the last one it had (per project, remembered in a small
//! file next to the preferences), otherwise the plan of the level with most walls, otherwise
//! 3D. The plan fit ignores far outliers (1st to 99th percentile of the element points).
use crate::shell::EditorApp;
use ddc_geom::{v2, V2};
use std::path::{Path, PathBuf};
use web_time::{Duration, Instant};

/// Projects remembered; older ones drop out.
const KEEP: usize = 64;
/// How often the current view is compared with the remembered one.
const CHECK_EVERY: Duration = Duration::from_millis(1500);

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    Plan,
    ThreeD,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LastView {
    pub mode: Mode,
    pub level: i64,
    pub cx: f64,
    pub cy: f64,
    pub scale: f64,
    /// The 3D orbit when the view was 3D.
    pub cam: Option<Cam3>,
}

/// Orbit camera of a 3D view: target, angles, distance, lens, scene radius.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cam3 {
    pub target: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    pub fov_y: f32,
    pub ortho: bool,
    pub radius: f32,
}

impl Cam3 {
    fn usable(&self) -> bool {
        let n = [self.yaw, self.pitch, self.distance, self.fov_y, self.radius];
        self.target.iter().chain(n.iter()).all(|v| v.is_finite())
            && self.distance > 0.0
            && self.fov_y > 0.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum First {
    Last(LastView),
    /// Plan of this level, fitted.
    Plan(i64),
    ThreeD,
}

/// The rule in one place. `walls` is walls per level; `levels` the ids that exist.
pub fn decide(last: Option<LastView>, levels: &[i64], walls: &[(i64, usize)]) -> First {
    if let Some(v) =
        last.filter(|v| levels.contains(&v.level) && v.scale.is_finite() && v.scale > 0.0)
    {
        return First::Last(v);
    }
    let best = walls
        .iter()
        .filter(|w| w.1 > 0 && levels.contains(&w.0))
        .fold(None::<(i64, usize)>, |b, &w| match b {
            Some(x) if x.1 >= w.1 => Some(x),
            _ => Some(w),
        });
    best.map_or(First::ThreeD, |b| First::Plan(b.0))
}

/// Box holding the 1st to 99th percentile of the x and of the y values: site slabs and stray
/// points do not shrink the model on screen.
pub fn percentile_bounds(pts: &[V2]) -> Option<(V2, V2)> {
    if pts.is_empty() {
        return None;
    }
    let pick = |mut v: Vec<f64>, p: f64| {
        v.sort_by(f64::total_cmp);
        v[((v.len() - 1) as f64 * p).round() as usize]
    };
    let xs: Vec<f64> = pts.iter().map(|p| p.x).collect();
    let ys: Vec<f64> = pts.iter().map(|p| p.y).collect();
    Some((
        v2(pick(xs.clone(), 0.01), pick(ys.clone(), 0.01)),
        v2(pick(xs, 0.99), pick(ys, 0.99)),
    ))
}

/// Scale (pixels per metre) that shows a box of this span in about 600 points.
pub fn scale_for(lo: V2, hi: V2) -> f64 {
    let span = (hi.x - lo.x).max(hi.y - lo.y).max(1.0);
    (600.0 / span).clamp(0.002, 2000.0)
}

// ---- the remembered views ----------------------------------------------------------------

#[derive(Default)]
pub struct Store {
    items: Vec<(String, LastView)>,
    loaded: bool,
    checked: Option<Instant>,
}

fn file() -> Option<PathBuf> {
    crate::gfx::data_dir().map(|d| d.join("last_views.txt"))
}

/// One line a view; a 3D view with its camera is `3dc` (older builds skip such lines).
pub fn render(items: &[(String, LastView)]) -> String {
    let mut s = String::new();
    for (p, v) in items {
        let head = format!("{}\t{}\t{}\t{}", v.level, v.cx, v.cy, v.scale);
        match (v.mode, v.cam) {
            (Mode::Plan, _) => s.push_str(&format!("plan\t{head}\t{p}\n")),
            (Mode::ThreeD, None) => s.push_str(&format!("3d\t{head}\t{p}\n")),
            (Mode::ThreeD, Some(c)) => {
                let [x, y, z] = c.target;
                let o = u8::from(c.ortho);
                let cam = format!(
                    "{x}\t{y}\t{z}\t{}\t{}\t{}\t{}\t{o}\t{}",
                    c.yaw, c.pitch, c.distance, c.fov_y, c.radius
                );
                s.push_str(&format!("3dc\t{head}\t{cam}\t{p}\n"));
            }
        }
    }
    s
}

/// Bad lines are skipped.
pub fn parse(text: &str) -> Vec<(String, LastView)> {
    text.lines().filter_map(parse_line).collect()
}

fn parse_line(l: &str) -> Option<(String, LastView)> {
    let (mode, with_cam) = match l.split('\t').next()? {
        "plan" => (Mode::Plan, false),
        "3d" => (Mode::ThreeD, false),
        "3dc" => (Mode::ThreeD, true),
        _ => return None,
    };
    let mut f = l.splitn(if with_cam { 15 } else { 6 }, '\t').skip(1);
    let level = f.next()?.parse().ok()?;
    let cx = f.next()?.parse().ok()?;
    let cy = f.next()?.parse().ok()?;
    let scale = f.next()?.parse().ok()?;
    let cam = if with_cam {
        // num-lint: machine file written by `render`, never user text
        let mut n = || f.next()?.parse::<f32>().ok();
        let target = [n()?, n()?, n()?];
        let (yaw, pitch, distance, fov_y) = (n()?, n()?, n()?, n()?);
        let ortho = f.next()? == "1";
        let radius = f.next()?.parse().ok()?;
        let c = Cam3 { target, yaw, pitch, distance, fov_y, ortho, radius };
        // a broken camera falls back to a fitted 3D view, the line itself stays
        c.usable().then_some(c)
    } else {
        None
    };
    let path = f.next()?.to_string();
    Some((path, LastView { mode, level, cx, cy, scale, cam }))
}

impl Store {
    fn load(&mut self) {
        if !self.loaded {
            self.loaded = true;
            if let Some(f) = file() {
                self.load_from(&f);
            }
        }
    }

    fn load_from(&mut self, f: &Path) {
        if let Ok(t) = std::fs::read_to_string(f) {
            self.items = parse(&t);
        }
    }

    pub fn get(&mut self, path: &str) -> Option<LastView> {
        self.load();
        self.items.iter().find(|i| i.0 == path).map(|i| i.1)
    }

    /// True when the entry changed.
    pub fn put(&mut self, path: &str, v: LastView) -> bool {
        self.load();
        if self.items.first().is_some_and(|i| i.0 == path && i.1 == v) {
            return false;
        }
        self.items.retain(|i| i.0 != path);
        self.items.insert(0, (path.to_string(), v));
        self.items.truncate(KEEP);
        true
    }

    fn save(&self) -> Result<(), String> {
        file().map_or(Ok(()), |f| self.save_to(&f))
    }

    /// Whole or not at all: a torn write leaves the previous file (temporary file, rename).
    fn save_to(&self, f: &Path) -> Result<(), String> {
        let text = render(&self.items);
        crate::safe_file::write_atomic(f, text.as_bytes()).map_err(|e| e.to_string())
    }
}

// ---- the application side ----------------------------------------------------------------

fn project_key(a: &EditorApp) -> Option<String> {
    a.ed.project
        .path()
        .map(|p| p.to_string_lossy().into_owned())
}

fn current(a: &EditorApp) -> LastView {
    let c = &a.view.camera;
    let cam = a.view3d.then(|| Cam3 {
        target: [c.target.x, c.target.y, c.target.z],
        yaw: c.yaw,
        pitch: c.pitch,
        distance: c.distance,
        fov_y: c.fov_y,
        ortho: c.ortho,
        radius: c.scene_radius,
    });
    LastView {
        mode: if a.view3d { Mode::ThreeD } else { Mode::Plan },
        level: a.ed.level,
        cx: a.plan.center.x,
        cy: a.plan.center.y,
        scale: a.plan.scale,
        cam,
    }
}

/// The remembered orbit, or a fitted 3D view when there is none.
fn show_3d(a: &mut EditorApp, cam: Option<Cam3>) {
    a.view3d = true;
    let Some(s) = cam.filter(Cam3::usable) else {
        a.view.refit();
        return;
    };
    let c = &mut a.view.camera;
    (c.target.x, c.target.y, c.target.z) = (s.target[0], s.target[1], s.target[2]);
    c.yaw = s.yaw;
    c.pitch = s.pitch.clamp(-ddc_render::nav::PITCH_TOP, ddc_render::nav::PITCH_TOP);
    c.distance = s.distance.max(1e-3);
    c.fov_y = s.fov_y;
    c.ortho = s.ortho;
    c.scene_radius = s.radius.max(s.distance);
    a.view.keep_camera();
}

/// The points the plan shows: wall ends, curtain lines, slab outlines, non-site bodies.
fn plan_points(a: &EditorApp) -> Vec<V2> {
    let ed = &a.ed;
    let mut v: Vec<V2> = ed.walls.iter().flat_map(|w| [w.a, w.b]).collect();
    v.extend(ed.curtain_lines.iter().flat_map(|l| [l.1, l.2]));
    v.extend(
        ed.slabs
            .iter()
            .flat_map(|s| s.rings.first().cloned().unwrap_or_default()),
    );
    v.extend(
        ed.bodies
            .iter()
            .filter(|b| !crate::editor::bodies::is_site(&b.category))
            .flat_map(|b| b.hull.iter().copied()),
    );
    v
}

fn fit_plan(a: &mut EditorApp) {
    match percentile_bounds(&plan_points(a)) {
        Some((lo, hi)) => {
            a.plan.center = (lo + hi) * 0.5;
            a.plan.scale = scale_for(lo, hi);
        }
        None => a.plan.fit(&a.ed),
    }
}

/// After a project replaced the model (`project_ui::after_swap`): picks and shows the view.
pub fn on_swap(a: &mut EditorApp) {
    let levels: Vec<i64> = a.ed.levels().iter().map(|l| l.0).collect();
    let walls: Vec<(i64, usize)> =
        a.ed.model
            .sql()
            .prepare("SELECT level, count(*) FROM wall GROUP BY level ORDER BY level")
            .and_then(|mut st| {
                let rows = st.query_map([], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? as usize))
                })?;
                rows.collect::<Result<Vec<_>, _>>()
            })
            .unwrap_or_default();
    let last = project_key(a).and_then(|k| a.open.views.get(&k));
    match decide(last, &levels, &walls) {
        First::Last(v) => {
            a.ed.set_level(v.level);
            if v.mode == Mode::Plan {
                a.view3d = false;
                a.plan.center = v2(v.cx, v.cy);
                a.plan.scale = v.scale;
            } else {
                show_3d(a, v.cam);
            }
        }
        First::Plan(l) => {
            a.ed.set_level(l);
            a.view3d = false;
            fit_plan(a);
        }
        First::ThreeD => {
            a.view3d = true;
            a.view.refit();
        }
    }
    a.open.views.checked = None;
}

/// Per frame: remembers the view of a project that has a file when it changed.
pub fn track(a: &mut EditorApp) {
    let now = Instant::now();
    if a.open
        .views
        .checked
        .is_some_and(|t| now.duration_since(t) < CHECK_EVERY)
    {
        return;
    }
    a.open.views.checked = Some(now);
    let Some(key) = project_key(a) else { return };
    let v = current(a);
    if a.open.views.put(&key, v) {
        // told once a session with the settings (`project_ui::tell_settings_failure`)
        if let Err(e) = a.open.views.save() {
            eprintln!("last views: not written: {e}");
            a.prefs.write_fail.set(Some(e));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lv(level: i64) -> LastView {
        LastView {
            mode: Mode::Plan,
            level,
            cx: 1.0,
            cy: 2.0,
            scale: 40.0,
            cam: None,
        }
    }

    fn cam() -> Cam3 {
        Cam3 {
            target: [1.5, -2.25, 3.0],
            yaw: 0.7,
            pitch: 0.4,
            distance: 42.5,
            fov_y: 0.8,
            ortho: true,
            radius: 30.0,
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ddc-fv-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn store(items: Vec<(String, LastView)>) -> Store {
        Store { items, loaded: true, ..Default::default() }
    }

    #[test]
    fn a_3d_camera_round_trips_and_a_broken_one_falls_back() {
        let v = LastView { mode: Mode::ThreeD, cam: Some(cam()), ..lv(1) };
        let items = vec![("C:\\p\\a.ddc".to_string(), v)];
        assert_eq!(parse(&render(&items)), items);
        let bad = render(&items).replace("42.5", "NaN");
        assert_eq!(parse(&bad)[0].1.cam, None, "line kept, camera dropped");
    }

    /// A crash between the temporary file and the rename leaves the old file whole.
    #[test]
    fn a_torn_last_views_write_keeps_the_old_file() {
        let d = scratch("torn");
        let f = d.join("last_views.txt");
        let old = store(vec![("a.ddc".into(), lv(1))]);
        old.save_to(&f).unwrap();
        let tmp = d.join("last_views.txt.tmp");
        std::fs::write(&tmp, b"3dc\t1\t").unwrap();
        let mut back = store(vec![]);
        back.load_from(&f);
        assert_eq!(back.items, old.items);
        let new = store(vec![("b.ddc".into(), lv(2))]);
        new.save_to(&f).unwrap();
        back.load_from(&f);
        assert_eq!(back.items, new.items, "the next write replaces the stale temporary");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A write that fails after the temporary file (a target that cannot be replaced) keeps
    /// the old file and leaves no temporary behind.
    #[test]
    fn a_write_failing_partway_keeps_the_old_file() {
        let d = scratch("partway");
        let f = d.join("last_views.txt");
        let old = store(vec![("a.ddc".into(), lv(1))]);
        old.save_to(&f).unwrap();
        let ro = |on: bool| {
            let mut p = std::fs::metadata(&f).unwrap().permissions();
            p.set_readonly(on);
            std::fs::set_permissions(&f, p).unwrap();
        };
        ro(true);
        let r = store(vec![("b.ddc".into(), lv(2))]).save_to(&f);
        let mut back = store(vec![]);
        back.load_from(&f);
        ro(false);
        if r.is_err() {
            assert_eq!(back.items, old.items, "old file whole");
            assert!(!d.join("last_views.txt.tmp").exists(), "no temporary left");
        }
        // a folder that is a file: the temporary cannot be made, nothing changes
        let blocked = d.join("last_views.txt").join("x.txt");
        assert!(old.save_to(&blocked).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_3d_camera_survives_reopen() {
        let d = scratch("reopen");
        let p = d.join("a.ddc");
        let ed = crate::editor::Editor::start_project().unwrap();
        let mut a = EditorApp::new(ed, None, "t".into());
        a.ed.save_project_as(&p).unwrap();
        a.open.views = store(vec![]);
        a.view3d = true;
        let c = &mut a.view.camera;
        (c.target.x, c.target.y, c.target.z) = (4.0, 5.0, 6.0);
        (c.yaw, c.pitch, c.distance, c.fov_y, c.ortho) = (1.1, 0.3, 77.0, 0.9, true);
        a.view.camera.scene_radius = 50.0;
        let key = p.to_string_lossy().into_owned();
        assert!(a.open.views.put(&key, current(&a)));
        a.view.camera = Default::default();
        a.view3d = false;
        on_swap(&mut a);
        let c = &a.view.camera;
        assert!(a.view3d);
        assert_eq!([c.target.x, c.target.y, c.target.z], [4.0, 5.0, 6.0]);
        assert_eq!((c.yaw, c.distance, c.fov_y, c.ortho), (1.1, 77.0, 0.9, true));
        assert!((c.pitch - 0.3).abs() < 1e-6);
        drop(a);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn first_view_table() {
        let levels = [1, 2, 3];
        // reopened: its last view
        assert_eq!(decide(Some(lv(2)), &levels, &[(1, 9)]), First::Last(lv(2)));
        // last view on a level that is gone: the rules below
        assert_eq!(
            decide(Some(lv(9)), &levels, &[(1, 5), (3, 8)]),
            First::Plan(3)
        );
        // native walls: the level with most walls; a tie keeps the earlier one
        assert_eq!(
            decide(None, &levels, &[(1, 4), (2, 4), (3, 1)]),
            First::Plan(1)
        );
        // no walls (meshes only): 3D
        assert_eq!(decide(None, &levels, &[]), First::ThreeD);
        assert_eq!(decide(None, &levels, &[(2, 0)]), First::ThreeD);
        // a broken remembered scale is not used
        let bad = LastView {
            scale: f64::NAN,
            ..lv(1)
        };
        assert_eq!(decide(Some(bad), &levels, &[]), First::ThreeD);
    }

    #[test]
    fn percentile_fit_ignores_an_outlier() {
        let mut pts: Vec<V2> = (0..100)
            .map(|i| v2((i % 10) as f64, (i / 10) as f64))
            .collect();
        pts.push(v2(5000.0, -3000.0));
        let (lo, hi) = percentile_bounds(&pts).unwrap();
        assert!(
            hi.x <= 9.0 && hi.y <= 9.0 && lo.x >= 0.0 && lo.y >= 0.0,
            "{lo:?} {hi:?}"
        );
        assert!(
            scale_for(lo, hi) > 30.0,
            "the model fills the view, not the outlier"
        );
        assert_eq!(percentile_bounds(&[]), None);
    }

    #[test]
    fn store_text_round_trip_and_bad_lines() {
        let items = vec![
            ("C:\\a b\\House.ddc".to_string(), lv(2)),
            (
                "x.ddc".to_string(),
                LastView {
                    mode: Mode::ThreeD,
                    ..lv(1)
                },
            ),
        ];
        assert_eq!(parse(&render(&items)), items);
        assert_eq!(parse("junk\nplan\tx\n").len(), 0);
    }

    #[test]
    fn store_keeps_newest_first_and_reports_changes() {
        let mut s = Store {
            loaded: true,
            ..Default::default()
        };
        assert!(s.put("a", lv(1)));
        assert!(!s.put("a", lv(1)));
        assert!(s.put("b", lv(2)));
        assert!(s.put("a", lv(3)));
        assert_eq!(s.items[0].0, "a");
        assert_eq!(s.items.len(), 2);
        assert_eq!(s.get("b"), Some(lv(2)));
    }
}
