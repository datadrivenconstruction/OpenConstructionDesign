//! City benchmark: a generated city, camera paths through it (orbit, flyover, street with fast
//! turns, dive from the whole city to one building), frame time to completion on the GPU and the
//! memory of the city layer. Budgets: `docs/CITY1.md`; `examples/city_bench.rs` checks them.

use super::gen::{city_projects, totals, CitySpec};
use super::project::offset;
use super::{CityConfig, CityView};
use crate::camera::OrbitCamera;
use crate::renderer::{Quality, Renderer};
use crate::scene::{Aabb, Scene};
use crate::style::VisualStyle;
use crate::synth::{env_backends, phase_split, rig, Phase, Rig};
use glam::Vec3;
use std::sync::Arc;
use web_time::Instant;

#[derive(Clone, Copy, Debug)]
pub struct CityBenchSpec {
    pub city: CitySpec,
    pub size: (u32, u32),
    /// Moving frames per path (rest frames: half of it).
    pub frames: u32,
    /// Full-content memory budget, bytes.
    pub budget: u64,
    pub workers: usize,
    pub quality: Quality,
}

impl CityBenchSpec {
    pub fn new(city: CitySpec) -> Self {
        Self {
            city,
            size: (1920, 1080),
            frames: 240,
            budget: 1 << 30,
            workers: std::thread::available_parallelism()
                .map_or(2, |n| n.get().saturating_sub(1))
                .clamp(1, 6),
            quality: Quality::Medium,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct PathReport {
    pub name: &'static str,
    pub moving: Phase,
    pub rest: Phase,
    /// City layer update (cut, residency, scene compose and chunk sync), ms.
    pub update_p95_ms: f64,
    pub update_max_ms: f64,
    /// From the end of motion until nothing more refines or loads.
    pub settle_ms: f64,
    pub settled: bool,
    pub peak_full_bytes: u64,
    pub peak_triangles: usize,
    pub max_held: usize,
}

#[derive(Clone, Debug, Default)]
pub struct CityBenchReport {
    pub adapter: String,
    pub backend: String,
    pub projects: usize,
    pub buildings: u64,
    pub units: u64,
    pub elements: u64,
    /// Full content of the whole city, bytes (what streaming avoids holding).
    pub source_bytes: u64,
    pub budget: u64,
    /// Until every project is cooked (proxies of the whole city on screen).
    pub first_image_ms: f64,
    pub proxy_bytes: u64,
    pub paths: Vec<PathReport>,
}

struct Run<'a> {
    rig: &'a Rig,
    r: Renderer,
    view: CityView,
    aspect: f32,
}

impl Run<'_> {
    /// One frame: city update, scene sync, render to completion. `(total, cpu, update)` ms.
    fn step(&mut self, cam: &OrbitCamera) -> (f64, f64, f64) {
        let t = Instant::now();
        let vp = cam.view_proj(self.aspect);
        let scene = self
            .view
            .update(vp, cam.eye(), self.rig.size, self.r.occlusion_snapshot(&vp));
        if let Some(s) = scene {
            self.r.set_scene(&self.rig.device, &s);
        }
        let upd = t.elapsed().as_secs_f64() * 1000.0;
        let (total, cpu) = self.rig.frame(&mut self.r, cam);
        (upd + total, upd + cpu, upd)
    }

    fn path(
        &mut self,
        name: &'static str,
        frames: u32,
        cam: &mut OrbitCamera,
        mut move_cam: impl FnMut(&mut OrbitCamera, u32),
    ) -> PathReport {
        let mut rep = PathReport {
            name,
            ..PathReport::default()
        };
        let mut moving = vec![];
        let mut upd = vec![];
        let watch = |v: &CityView, rep: &mut PathReport| {
            let s = v.stats();
            rep.peak_full_bytes = rep.peak_full_bytes.max(s.full_bytes);
            rep.peak_triangles = rep.peak_triangles.max(s.triangles);
            rep.max_held = rep.max_held.max(s.held);
        };
        for f in 0..frames {
            move_cam(cam, f);
            let (t, c, u) = self.step(cam);
            moving.push((t, c));
            upd.push(u);
            watch(&self.view, &mut rep);
        }
        rep.moving = phase_split(&moving, self.r.chunks.frame_stats());
        let t = Instant::now();
        rep.settled = false;
        for _ in 0..4000 {
            let (_, _, u) = self.step(cam);
            upd.push(u);
            watch(&self.view, &mut rep);
            if self.view.settled() && !self.r.chunks.frame_stats().refining {
                rep.settled = true;
                break;
            }
            if t.elapsed().as_secs_f64() > 120.0 {
                break;
            }
        }
        rep.settle_ms = t.elapsed().as_secs_f64() * 1000.0;
        let rest: Vec<(f64, f64)> = (0..(frames / 2).max(1))
            .map(|_| {
                let (t, c, u) = self.step(cam);
                upd.push(u);
                (t, c)
            })
            .collect();
        rep.rest = phase_split(&rest, self.r.chunks.frame_stats());
        upd.sort_by(f64::total_cmp);
        rep.update_p95_ms = upd[(upd.len() * 95 / 100).min(upd.len() - 1)];
        rep.update_max_ms = upd.last().copied().unwrap_or(0.0);
        rep
    }
}

/// Generates the city, opens it and runs the four camera paths.
pub fn bench_city(spec: &CityBenchSpec) -> Result<CityBenchReport, String> {
    let projects = city_projects(&spec.city);
    let (buildings, units, elements, source_bytes) = totals(&projects);
    let rig = pollster::block_on(rig(
        env_backends(),
        spec.size,
        spec.quality,
        VisualStyle::default(),
    ))?;
    let info = rig.info.clone();
    let view = CityView::new(
        projects
            .into_iter()
            .map(|p| p as Arc<dyn super::Project>)
            .collect(),
        CityConfig {
            budget: spec.budget,
            workers: spec.workers,
            ..CityConfig::default()
        },
    );
    let mut r = Renderer::new(&rig.device, rig.targets(), &Scene::default());
    r.set_occlusion(&rig.device, info.backend != wgpu::Backend::Gl);
    r.set_adapter(&info);
    r.redraw_at_rest = true;
    let bounds = view.bounds().ok_or("empty city")?;
    let corner = offset(spec.city.origin, view.anchor());
    let n_projects = view.project_count();
    let mut run = Run {
        rig: &rig,
        r,
        view,
        aspect: spec.size.0 as f32 / spec.size.1 as f32,
    };
    let mut cam = OrbitCamera::default();
    cam.fit(&bounds, run.aspect);
    let t = Instant::now();
    loop {
        run.step(&cam);
        let s = run.view.stats();
        if s.cooked == s.projects || t.elapsed().as_secs_f64() > 600.0 {
            break;
        }
    }
    let first_image_ms = t.elapsed().as_secs_f64() * 1000.0;
    let frames = spec.frames.max(2);
    let mut paths = vec![];
    let fit = cam.clone();
    paths.push(run.path("orbit", frames, &mut cam, |c, _| c.orbit(4.0, 0.0)));
    let mut fly = fit.clone();
    fly.distance = 450.0;
    fly.pitch = 0.7;
    fly.target = Vec3::new(bounds.min.x, bounds.min.y, 0.0);
    let diag = (bounds.max - bounds.min).truncate().normalize_or_zero().extend(0.0);
    paths.push(run.path("flyover", frames, &mut fly, |c, _| {
        c.target += diag * 3.3;
    }));
    let pitch = spec.city.pitch();
    let k = (spec.city.blocks / 2).max(1) as f32;
    let street = corner + Vec3::new(k * pitch - spec.city.street * 0.5, k * pitch, 1.7);
    let mut walk = fit.clone();
    walk.target = street;
    walk.distance = 3.0;
    walk.pitch = 0.05;
    walk.yaw = 0.0;
    paths.push(run.path("street", frames, &mut walk, |c, f| {
        if f % 48 < 30 {
            c.target += c.back() * Vec3::new(-1.0, -1.0, 0.0) * 0.05;
        } else {
            c.yaw += 10f32.to_radians();
        }
    }));
    let mut dive = fit;
    let start = dive.distance;
    dive.target = tallest(&run.view).unwrap_or(dive.target);
    dive.pitch = 0.5;
    paths.push(run.path("dive", frames, &mut dive, |c, f| {
        let s = f as f32 / frames as f32;
        c.distance = start * (5.0 / start).powf(s);
    }));
    let s = run.view.stats();
    Ok(CityBenchReport {
        adapter: format!("{} ({:?})", info.name, info.backend),
        backend: format!("{:?}", info.backend).to_lowercase(),
        projects: n_projects,
        buildings,
        units,
        elements,
        source_bytes,
        budget: spec.budget,
        first_image_ms,
        proxy_bytes: s.proxy_bytes,
        paths,
    })
}

/// Top centre of the tallest unit of the cooked projects.
fn tallest(view: &CityView) -> Option<Vec3> {
    view.unit_bounds()
        .into_iter()
        .max_by(|a, b| a.max.z.total_cmp(&b.max.z))
        .map(|b: Aabb| {
            let c = (b.min + b.max) * 0.5;
            Vec3::new(c.x, c.y, b.max.z)
        })
}
