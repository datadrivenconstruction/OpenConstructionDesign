//! Exact NURBS shapes behind scene bodies, for curve and surface snaps and the tangent work
//! plane. Providers (freeform elements and others) register here; the 3D view collects the
//! shapes when its bodies are rebuilt. Coordinates are the scene's (z from the active level).

use std::sync::{Arc, RwLock};

use ddc_geom::nurbs::{Curve, Surface};
use ddc_geom::V3;
use ddc_render::glam::Vec3;
use ddc_render::pick::ray_aabb;
use ddc_render::{Aabb, Ray};

use super::pick::Target;
use crate::editor::Editor;

/// Exact geometry of one element: edge curves and an optional surface.
#[derive(Clone, Debug)]
pub struct Shape3 {
    pub target: Target,
    pub curves: Vec<Curve>,
    pub surface: Option<Surface>,
    /// Box of all control points (the shape lies inside it).
    pub aabb: Aabb,
}

pub fn g(v: V3) -> Vec3 {
    Vec3::new(v.x as f32, v.y as f32, v.z as f32)
}

pub fn d(v: Vec3) -> V3 {
    ddc_geom::v3(v.x as f64, v.y as f64, v.z as f64)
}

impl Shape3 {
    pub fn new(target: Target, curves: Vec<Curve>, surface: Option<Surface>) -> Shape3 {
        let mut b = Aabb { min: Vec3::splat(f32::INFINITY), max: Vec3::splat(f32::NEG_INFINITY) };
        let pts = curves.iter().flat_map(|c| c.ctrl().iter()).chain(surface.iter().flat_map(|s| s.ctrl().iter()));
        for p in pts {
            b.min = b.min.min(g(*p));
            b.max = b.max.max(g(*p));
        }
        Shape3 { target, curves, surface, aabb: b }
    }

    /// The ray passes within `tol` of the box.
    pub fn near_ray(&self, r: &Ray, tol: f32) -> bool {
        let fat = Aabb { min: self.aabb.min - Vec3::splat(tol), max: self.aabb.max + Vec3::splat(tol) };
        fat.min.x <= fat.max.x && ray_aabb(r, &fat).is_some()
    }

    /// Exact tangent plane at the surface point nearest `p`: (point, unit normal) with the
    /// normal turned toward `toward` (e.g. the mesh normal).
    pub fn tangent_plane(&self, p: Vec3, toward: Vec3) -> Option<(Vec3, Vec3)> {
        let s = self.surface.as_ref()?;
        let c = s.closest(d(p));
        let n = g(s.normal(c.u, c.v)?);
        let n = if n.dot(toward) < 0.0 { -n } else { n };
        Some((g(c.point), n.normalize()))
    }
}

pub trait ExactProvider: Send + Sync {
    fn key(&self) -> &'static str;
    fn shapes(&self, ed: &Editor) -> Vec<Shape3>;
}

static EXTRA: RwLock<Vec<Arc<dyn ExactProvider>>> = RwLock::new(Vec::new());

/// Registers a provider; a second one with the same key replaces the first.
pub fn register(p: Arc<dyn ExactProvider>) {
    if let Ok(mut v) = EXTRA.write() {
        v.retain(|q| q.key() != p.key());
        v.push(p);
    }
}

/// Shapes of every registered provider.
pub fn collect(ed: &Editor) -> Vec<Shape3> {
    let v: Vec<Arc<dyn ExactProvider>> = EXTRA.read().map(|v| v.clone()).unwrap_or_default();
    v.iter().flat_map(|p| p.shapes(ed)).collect()
}
