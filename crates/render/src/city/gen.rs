//! Procedural city for scale benchmarks: a street grid of blocks, parcels with towers (glass
//! curtain wall, mullions, columns, core, slabs), mid-rise slab buildings (walls, windows, slabs)
//! and rows of houses (eight types, walls and roof each one instanced prototype). Seeded and
//! deterministic: the same spec gives the same geometry, element ids and byte counts. Every
//! project covers `per_project`² blocks with its own origin far from zero; each building (or row
//! of houses) is one unit that loads on demand. Repeated parts are instanced from prototypes
//! shared by the whole city.

use super::project::{mix, Content, Project, Unit};
use crate::chunks::{box_mesh, transform_box, Inst, Instanced};
use crate::scene::{tag_rgb, Aabb, SceneMesh, TAG_GLASS, TAG_MULLION, TAG_WINDOW};
use glam::{Mat4, Vec3};
use std::f32::consts::FRAC_PI_2;
use std::sync::Arc;

pub const FLOOR_H: f32 = 3.5;
const SLAB_T: f32 = 0.3;
const PANEL: f32 = 1.5;
const WIN_STEP: f32 = 3.0;
const HOUSE_TYPES: usize = 8;
const P_GLASS: usize = 0;
const P_MULLION: usize = 1;
const P_COLUMN: usize = 2;
const P_WINDOW: usize = 3;
const P_WALLS: usize = 4;
const P_ROOF: usize = P_WALLS + HOUSE_TYPES;
const PROTOS: usize = P_ROOF + HOUSE_TYPES;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CitySpec {
    pub seed: u64,
    /// Blocks along each side.
    pub blocks: u32,
    /// Side of a block, metres.
    pub block: f32,
    pub street: f32,
    /// Blocks along each side of one project.
    pub per_project: u32,
    /// World position of the south-west city corner.
    pub origin: [f64; 3],
}

impl CitySpec {
    pub fn with_blocks(blocks: u32) -> Self {
        Self {
            seed: 0x5eed_c171,
            blocks,
            block: 110.0,
            street: 20.0,
            per_project: 2,
            origin: [512_000.0, 6_200_000.0, 35.0],
        }
    }
    /// `S`, `M`, `L` (≈1.5k, 6k, 24k buildings).
    pub fn preset(name: &str) -> Option<Self> {
        match name.to_ascii_uppercase().as_str() {
            "S" => Some(Self::with_blocks(16)),
            "M" => Some(Self::with_blocks(32)),
            "L" => Some(Self::with_blocks(64)),
            _ => None,
        }
    }
    pub fn pitch(&self) -> f32 {
        self.block + self.street
    }
    pub fn project_side(&self) -> u32 {
        self.blocks.div_ceil(self.per_project.max(1))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Tower,
    Slab,
    Houses,
}

/// A building (or a row of houses) of a project: one unit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Building {
    pub kind: Kind,
    /// Project-local footprint corner and size (height = floors × `FLOOR_H`).
    pub min: Vec3,
    pub size: Vec3,
    pub floors: u32,
    pub seed: u64,
    /// First local element id and the count.
    pub first: u32,
    pub count: u32,
    pub bytes: u64,
    pub bounds: Aabb,
    /// Houses in the unit (1 for other kinds).
    pub buildings: u32,
}

impl Building {
    /// A building before planning (element range, bytes and bounds come from
    /// `CityProject::from_buildings`). `size.z` is ignored: height = floors × `FLOOR_H`.
    pub fn new(kind: Kind, min: Vec3, size: Vec3, floors: u32, seed: u64) -> Self {
        Self {
            kind,
            min,
            size,
            floors: floors.max(1),
            seed,
            first: 0,
            count: 0,
            bytes: 0,
            bounds: Aabb::EMPTY,
            buildings: if kind == Kind::Houses { 6 } else { 1 },
        }
    }
}

/// Prototype meshes shared by every project of the city.
pub struct Protos {
    pub meshes: Vec<Arc<SceneMesh>>,
    pub lods: Vec<Vec<(Arc<SceneMesh>, f32)>>,
    local: Vec<Aabb>,
}

fn cuboid(min: Vec3, max: Vec3, tag: u32) -> SceneMesh {
    box_mesh(&Aabb { min, max }, 0, tag)
}

/// Closed prism of `n` sides, radius `r`, from `z0` to `z1`, faces outward.
fn prism(n: u32, r: f32, z0: f32, z1: f32, tag: u32) -> SceneMesh {
    let mut positions = vec![];
    for i in 0..n {
        let a = i as f32 / n as f32 * std::f32::consts::TAU;
        let (s, c) = a.sin_cos();
        positions.push([c * r, s * r, z0]);
        positions.push([c * r, s * r, z1]);
    }
    let cb = positions.len() as u32;
    positions.push([0.0, 0.0, z0]);
    positions.push([0.0, 0.0, z1]);
    let ct = cb + 1;
    let mut triangles = vec![];
    for i in 0..n {
        let j = (i + 1) % n;
        let (b0, t0, b1, t1) = (2 * i, 2 * i + 1, 2 * j, 2 * j + 1);
        triangles.push([b0, b1, t1]);
        triangles.push([b0, t1, t0]);
        triangles.push([ct, t0, t1]);
        triangles.push([cb, b1, b0]);
    }
    SceneMesh {
        element: 0,
        tag,
        positions,
        triangles,
    }
}

/// Gable roof over `[0, w] × [0, d]` with the ridge along x at height `rh`, faces outward.
fn gable(w: f32, d: f32, rh: f32, tag: u32) -> SceneMesh {
    let ym = d * 0.5;
    let positions = vec![
        [0.0, 0.0, 0.0],
        [w, 0.0, 0.0],
        [w, d, 0.0],
        [0.0, d, 0.0],
        [0.0, ym, rh],
        [w, ym, rh],
    ];
    let (a, b, c, dd, r0, r1) = (0, 1, 2, 3, 4, 5);
    let triangles = vec![
        [a, b, r1],
        [a, r1, r0],
        [c, dd, r0],
        [c, r0, r1],
        [a, r0, dd],
        [b, c, r1],
        [a, dd, c],
        [a, c, b],
    ];
    SceneMesh {
        element: 0,
        tag,
        positions,
        triangles,
    }
}

fn house_dims(v: usize) -> (f32, f32, u32) {
    (8.0 + (v % 4) as f32 * 1.5, 9.0 + (v / 4) as f32 * 2.0, 1 + (v % 3 == 0) as u32)
}

const HOUSE_WALLS: [[u8; 3]; 4] = [
    [226, 214, 190],
    [204, 170, 140],
    [236, 232, 222],
    [184, 196, 186],
];
const HOUSE_ROOFS: [[u8; 3]; 2] = [[150, 74, 58], [88, 92, 100]];

impl Protos {
    pub fn new() -> Self {
        let mut meshes = vec![SceneMesh::default(); PROTOS];
        let mut lods = vec![vec![]; PROTOS];
        meshes[P_GLASS] = cuboid(
            Vec3::new(0.0, -0.015, SLAB_T),
            Vec3::new(PANEL, 0.015, FLOOR_H),
            TAG_GLASS,
        );
        meshes[P_MULLION] = cuboid(
            Vec3::new(-0.04, -0.12, SLAB_T),
            Vec3::new(0.04, 0.04, FLOOR_H),
            TAG_MULLION,
        );
        let concrete = tag_rgb([176, 172, 166], false);
        meshes[P_COLUMN] = prism(12, 0.3, SLAB_T, FLOOR_H, concrete);
        lods[P_COLUMN] = vec![(
            Arc::new(cuboid(
                Vec3::new(-0.3, -0.3, SLAB_T),
                Vec3::new(0.3, 0.3, FLOOR_H),
                concrete,
            )),
            0.3 * (std::f32::consts::SQRT_2 - 1.0),
        )];
        meshes[P_WINDOW] = cuboid(
            Vec3::new(0.0, -0.08, 0.0),
            Vec3::new(1.4, 0.04, 1.6),
            TAG_WINDOW,
        );
        for v in 0..HOUSE_TYPES {
            let (w, d, floors) = house_dims(v);
            let wall_h = floors as f32 * 3.0;
            meshes[P_WALLS + v] = cuboid(
                Vec3::ZERO,
                Vec3::new(w, d, wall_h),
                tag_rgb(HOUSE_WALLS[v % 4], false),
            );
            let roof_tag = tag_rgb(HOUSE_ROOFS[v % 2], false);
            let mut roof = gable(w + 0.6, d + 0.6, d * 0.35, roof_tag);
            for p in &mut roof.positions {
                *p = [p[0] - 0.3, p[1] - 0.3, p[2] + wall_h];
            }
            meshes[P_ROOF + v] = roof;
        }
        let local = meshes.iter().map(crate::scene::mesh_box).collect();
        Self {
            meshes: meshes.into_iter().map(Arc::new).collect(),
            lods,
            local,
        }
    }
}

impl Default for Protos {
    fn default() -> Self {
        Self::new()
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        mix(self.0)
    }
    fn f(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
    fn range(&mut self, lo: u32, hi: u32) -> u32 {
        lo + (self.next() % u64::from(hi - lo + 1)) as u32
    }
}

/// Writes a building: counts only (plan) or real content (load), through the same code, so the
/// planned counts, bytes and bounds always match what loads.
struct Emit<'a> {
    real: bool,
    protos: &'a Protos,
    next: u32,
    bytes: u64,
    bounds: Aabb,
    meshes: Vec<Arc<SceneMesh>>,
    rows: Vec<Vec<Inst>>,
}

impl<'a> Emit<'a> {
    fn new(protos: &'a Protos, first: u32, real: bool) -> Self {
        Self {
            real,
            protos,
            next: first,
            bytes: 0,
            bounds: Aabb::EMPTY,
            meshes: vec![],
            rows: vec![vec![]; PROTOS],
        }
    }
    fn id(&mut self) -> u32 {
        let e = self.next;
        self.next += 1;
        e
    }
    fn cuboid(&mut self, min: Vec3, max: Vec3, tag: u32) {
        let e = self.id();
        // 8 positions and 12 triangles of 12 bytes each, as `content_bytes` counts them
        self.bytes += 8 * 12 + 12 * 12;
        let b = Aabb { min, max };
        self.bounds.union(&b);
        if self.real {
            self.meshes.push(Arc::new(box_mesh(&b, e, tag)));
        }
    }
    fn inst(&mut self, proto: usize, m: Mat4) {
        let e = self.id();
        self.bytes += std::mem::size_of::<Inst>() as u64;
        self.bounds.union(&transform_box(&self.protos.local[proto], &m));
        if self.real {
            self.rows[proto].push(Inst { m, element: e });
        }
    }
}

fn tower(b: &Building, out: &mut Emit) {
    let mut rng = Rng(b.seed);
    let (min, size) = (b.min, b.size);
    let max = min + size;
    let nx = (size.x / PANEL).round() as u32;
    let ny = (size.y / PANEL).round() as u32;
    let slab = tag_rgb([204, 204, 200], false);
    let core = tag_rgb([176, 172, 166], false);
    let cx = ((size.x / 8.0).floor() as u32).max(1);
    let cy = ((size.y / 8.0).floor() as u32).max(1);
    let c = min + size * 0.5;
    for f in 0..b.floors {
        let z = f as f32 * FLOOR_H;
        out.cuboid(
            Vec3::new(min.x - 0.25, min.y - 0.25, z),
            Vec3::new(max.x + 0.25, max.y + 0.25, z + SLAB_T),
            slab,
        );
        out.cuboid(
            Vec3::new(c.x - size.x * 0.15, c.y - size.y * 0.15, z + SLAB_T),
            Vec3::new(c.x + size.x * 0.15, c.y + size.y * 0.15, z + FLOOR_H),
            core,
        );
        let sides = [
            (nx, Vec3::new(min.x, min.y, z), Vec3::X, 0.0),
            (ny, Vec3::new(max.x, min.y, z), Vec3::Y, FRAC_PI_2),
            (nx, Vec3::new(max.x, max.y, z), Vec3::NEG_X, 2.0 * FRAC_PI_2),
            (ny, Vec3::new(min.x, max.y, z), Vec3::NEG_Y, 3.0 * FRAC_PI_2),
        ];
        for (n, start, dir, rot) in sides {
            for i in 0..n {
                let m = Mat4::from_translation(start + dir * (i as f32 * PANEL))
                    * Mat4::from_rotation_z(rot);
                out.inst(P_GLASS, m);
                out.inst(P_MULLION, m);
            }
        }
        for i in 0..cx {
            for j in 0..cy {
                let p = Vec3::new(
                    min.x + (i as f32 + 0.5) * size.x / cx as f32,
                    min.y + (j as f32 + 0.5) * size.y / cy as f32,
                    z,
                );
                out.inst(P_COLUMN, Mat4::from_translation(p));
            }
        }
    }
    let top = b.floors as f32 * FLOOR_H;
    out.cuboid(
        Vec3::new(min.x - 0.25, min.y - 0.25, top),
        Vec3::new(max.x + 0.25, max.y + 0.25, top + SLAB_T),
        slab,
    );
    let pw = size.x * (0.3 + 0.3 * rng.f());
    let pd = size.y * (0.3 + 0.3 * rng.f());
    out.cuboid(
        Vec3::new(c.x - pw * 0.5, c.y - pd * 0.5, top + SLAB_T),
        Vec3::new(c.x + pw * 0.5, c.y + pd * 0.5, top + SLAB_T + 4.0),
        tag_rgb([150, 150, 150], false),
    );
}

const SLAB_WALLS: [[u8; 3]; 4] = [
    [214, 196, 170],
    [188, 120, 96],
    [222, 222, 214],
    [170, 180, 190],
];

fn slab_building(b: &Building, out: &mut Emit) {
    let (min, size) = (b.min, b.size);
    let max = min + size;
    let wall = tag_rgb(SLAB_WALLS[(b.seed % 4) as usize], false);
    let slab = tag_rgb([204, 204, 200], false);
    let t = 0.3;
    let nwx = ((size.x - 1.6) / WIN_STEP).floor().max(0.0) as u32;
    let nwy = ((size.y - 1.6) / WIN_STEP).floor().max(0.0) as u32;
    for f in 0..b.floors {
        let z = f as f32 * FLOOR_H;
        let (z0, z1) = (z + SLAB_T, z + FLOOR_H);
        out.cuboid(Vec3::new(min.x, min.y, z), Vec3::new(max.x, max.y, z0), slab);
        out.cuboid(Vec3::new(min.x, min.y, z0), Vec3::new(max.x, min.y + t, z1), wall);
        out.cuboid(Vec3::new(min.x, max.y - t, z0), Vec3::new(max.x, max.y, z1), wall);
        out.cuboid(Vec3::new(min.x, min.y + t, z0), Vec3::new(min.x + t, max.y - t, z1), wall);
        out.cuboid(Vec3::new(max.x - t, min.y + t, z0), Vec3::new(max.x, max.y - t, z1), wall);
        let wz = z + 1.0;
        for i in 0..nwx {
            let s = 0.8 + i as f32 * WIN_STEP;
            out.inst(
                P_WINDOW,
                Mat4::from_translation(Vec3::new(min.x + s, min.y, wz)),
            );
            out.inst(
                P_WINDOW,
                Mat4::from_translation(Vec3::new(max.x - s, max.y, wz))
                    * Mat4::from_rotation_z(2.0 * FRAC_PI_2),
            );
        }
        for i in 0..nwy {
            let s = 0.8 + i as f32 * WIN_STEP;
            out.inst(
                P_WINDOW,
                Mat4::from_translation(Vec3::new(max.x, min.y + s, wz))
                    * Mat4::from_rotation_z(FRAC_PI_2),
            );
            out.inst(
                P_WINDOW,
                Mat4::from_translation(Vec3::new(min.x, max.y - s, wz))
                    * Mat4::from_rotation_z(3.0 * FRAC_PI_2),
            );
        }
    }
    let top = b.floors as f32 * FLOOR_H;
    out.cuboid(
        Vec3::new(min.x - 0.2, min.y - 0.2, top),
        Vec3::new(max.x + 0.2, max.y + 0.2, top + SLAB_T),
        slab,
    );
}

/// Rows of houses: 3 × 2 lots over the parcel, alternate rows face the other way.
fn houses(b: &Building, out: &mut Emit) {
    let mut rng = Rng(b.seed);
    let (lx, ly) = (b.size.x / 3.0, b.size.y / 2.0);
    for j in 0..2 {
        for i in 0..3 {
            let v = (rng.next() % HOUSE_TYPES as u64) as usize;
            let (w, d, _) = house_dims(v);
            let c = b.min + Vec3::new((i as f32 + 0.5) * lx, (j as f32 + 0.5) * ly, 0.0);
            let m = Mat4::from_translation(c)
                * Mat4::from_rotation_z(j as f32 * 2.0 * FRAC_PI_2)
                * Mat4::from_translation(Vec3::new(-w * 0.5, -d * 0.5, 0.0));
            out.inst(P_WALLS + v, m);
            out.inst(P_ROOF + v, m);
        }
    }
}

fn emit(b: &Building, out: &mut Emit) {
    match b.kind {
        Kind::Tower => tower(b, out),
        Kind::Slab => slab_building(b, out),
        Kind::Houses => houses(b, out),
    }
}

/// One project of the generated city.
pub struct CityProject {
    pub origin: [f64; 3],
    pub buildings: Vec<Building>,
    protos: Arc<Protos>,
}

impl CityProject {
    /// A project of the given buildings (local ids from 1 in list order).
    pub fn from_buildings(origin: [f64; 3], list: Vec<Building>, protos: Arc<Protos>) -> Self {
        let mut first = 1;
        let buildings = list
            .into_iter()
            .map(|b| {
                let b = planned(&protos, b, first);
                first += b.count;
                b
            })
            .collect();
        Self {
            origin,
            buildings,
            protos,
        }
    }

    pub fn elements(&self) -> u32 {
        self.buildings.last().map_or(0, |b| b.first + b.count)
    }
}

impl Project for CityProject {
    fn origin(&self) -> [f64; 3] {
        self.origin
    }
    fn units(&self) -> Vec<Unit> {
        self.buildings
            .iter()
            .map(|b| Unit {
                bounds: b.bounds,
                elements: b.first..b.first + b.count,
                bytes: b.bytes,
            })
            .collect()
    }
    fn load(&self, unit: usize) -> Content {
        let b = &self.buildings[unit];
        let mut out = Emit::new(&self.protos, b.first, true);
        emit(b, &mut out);
        let groups = out
            .rows
            .into_iter()
            .enumerate()
            .filter(|(_, r)| !r.is_empty())
            .map(|(p, r)| {
                Instanced::new(p as u64, self.protos.meshes[p].clone(), r)
                    .with_lods(self.protos.lods[p].clone())
            })
            .collect();
        Content {
            meshes: out.meshes,
            groups,
        }
    }
}

/// Plans one building: element range, bytes and bounds from a counting pass.
fn planned(protos: &Protos, mut b: Building, first: u32) -> Building {
    let mut out = Emit::new(protos, first, false);
    emit(&b, &mut out);
    b.first = first;
    b.count = out.next - first;
    b.bytes = out.bytes;
    b.bounds = out.bounds;
    b
}

/// Buildings of block `(bx, by)` at local corner `at`; `t` — closeness to the centre, 0…1.
fn block(spec: &CitySpec, rng: &mut Rng, at: Vec3, t: f32, out: &mut Vec<Building>) {
    let s = spec.block;
    let u = rng.f();
    let new = Building::new;
    if u < t * t * 0.9 {
        let half = s * 0.5;
        for k in 0..4 {
            if rng.f() < 0.1 {
                continue; // a plaza
            }
            let px = at + Vec3::new((k % 2) as f32 * half, (k / 2) as f32 * half, 0.0);
            let nx = rng.range(12, 22) as f32;
            let ny = rng.range(12, 22) as f32;
            let size = Vec3::new(nx * PANEL, ny * PANEL, 0.0);
            let min = px + Vec3::new((half - size.x) * 0.5, (half - size.y) * 0.5, 0.0);
            let floors = 10 + (rng.f() * (10.0 + 50.0 * t)) as u32;
            let seed = rng.next();
            out.push(new(Kind::Tower, min, size, floors, seed));
        }
    } else if u < 0.35 + 0.4 * t {
        let half = s * 0.5;
        for k in 0..2 {
            let min = at + Vec3::new(5.0, k as f32 * half + 20.0, 0.0);
            let size = Vec3::new(s - 10.0, 14.0, 0.0);
            let floors = rng.range(4, 9);
            let seed = rng.next();
            out.push(new(Kind::Slab, min, size, floors, seed));
        }
    } else {
        let half = s * 0.5;
        for k in 0..4 {
            let min = at + Vec3::new((k % 2) as f32 * half, (k / 2) as f32 * half, 0.0);
            let seed = rng.next();
            out.push(new(Kind::Houses, min, Vec3::new(half, half, 0.0), 1, seed));
        }
    }
}

/// The projects of a city, row by row from the south-west corner.
pub fn city_projects(spec: &CitySpec) -> Vec<Arc<CityProject>> {
    let protos = Arc::new(Protos::new());
    let side = spec.project_side();
    let per = spec.per_project.max(1);
    let pitch = spec.pitch();
    let mid = spec.blocks as f32 * pitch * 0.5;
    let mut out = vec![];
    for py in 0..side {
        for px in 0..side {
            let mut rng = Rng(mix(spec.seed ^ mix(u64::from(py) << 32 | u64::from(px))));
            let mut list = vec![];
            for j in 0..per {
                for i in 0..per {
                    let (bx, by) = (px * per + i, py * per + j);
                    if bx >= spec.blocks || by >= spec.blocks {
                        continue;
                    }
                    let world = Vec3::new(bx as f32 * pitch, by as f32 * pitch, 0.0);
                    let c = world + Vec3::splat(spec.block * 0.5);
                    let r = Vec3::new(c.x - mid, c.y - mid, 0.0).length() / mid;
                    let at = Vec3::new(i as f32 * pitch, j as f32 * pitch, 0.0);
                    block(spec, &mut rng, at, (1.0 - r).clamp(0.0, 1.0), &mut list);
                }
            }
            let ox = f64::from(px * per) * f64::from(pitch);
            let oy = f64::from(py * per) * f64::from(pitch);
            out.push(Arc::new(CityProject::from_buildings(
                [spec.origin[0] + ox, spec.origin[1] + oy, spec.origin[2]],
                list,
                protos.clone(),
            )));
        }
    }
    out
}

/// The projects as federation members.
pub fn city(spec: &CitySpec) -> Vec<Arc<dyn Project>> {
    city_projects(spec)
        .into_iter()
        .map(|p| p as Arc<dyn Project>)
        .collect()
}

/// Totals of a generated city: (buildings, units, elements, full-content bytes).
pub fn totals(projects: &[Arc<CityProject>]) -> (u64, u64, u64, u64) {
    let mut t = (0, 0, 0, 0);
    for p in projects {
        for b in &p.buildings {
            t.0 += u64::from(b.buildings);
            t.1 += 1;
            t.2 += u64::from(b.count);
            t.3 += b.bytes;
        }
    }
    t
}
