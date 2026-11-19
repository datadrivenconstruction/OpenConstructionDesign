// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Demo plan: a grid of rooms from multilayer walls: geom, then layer solids, then scene meshes.

use ddc_geom::layered::{join, layer_footprints, layer_quantities, layer_solids, Layer, Strip};
use ddc_geom::{v2, Profile, Solid, Tol};
use ddc_render::{Scene, SceneMesh};

pub const LAYER_NAMES: [&str; 3] = ["\u{41e}\u{442}\u{434}\u{435}\u{43b}\u{43a}\u{430}", "\u{41a}\u{43b}\u{430}\u{434}\u{43a}\u{430}", "\u{41e}\u{442}\u{434}\u{435}\u{43b}\u{43a}\u{430}"];
const ROOM: (f64, f64) = (4.0, 5.0);
const HEIGHT: f64 = 3.0;

#[derive(Debug, Clone)]
pub struct WallInfo {
    pub length: f64,
    /// (thickness, volume) per layer.
    pub layers: Vec<(f64, f64)>,
}

pub struct Demo {
    pub scene: Scene,
    pub walls: Vec<WallInfo>,
    pub joined: bool,
}

/// A kx × ky grid of rooms: walls are the grid-line segments between nodes.
pub fn grid(kx: usize, ky: usize) -> Vec<Strip> {
    let layers = vec![
        Layer::new(0.015),
        Layer::with_priority(0.2, 2),
        Layer::new(0.015),
    ];
    let strip = |a, b| Strip {
        a,
        b,
        layers: layers.clone(),
        offset0: -0.115,
        base: 0.0,
        height: HEIGHT,
    };
    let p = |i: usize, j: usize| v2(i as f64 * ROOM.0, j as f64 * ROOM.1);
    let mut s = Vec::new();
    for j in 0..=ky {
        for i in 0..kx {
            s.push(strip(p(i, j), p(i + 1, j)));
        }
    }
    for i in 0..=kx {
        for j in 0..ky {
            s.push(strip(p(i, j), p(i, j + 1)));
        }
    }
    s
}

pub fn grid_for_rooms(rooms: usize) -> (usize, usize) {
    let kx = (rooms.max(1) as f64).sqrt().ceil() as usize;
    (kx, rooms.max(1).div_ceil(kx))
}

/// A square grid with at least `walls` walls: 2k(k+1) ≥ walls.
pub fn grid_for_walls(walls: usize) -> (usize, usize) {
    let mut k = 1;
    while 2 * k * (k + 1) < walls {
        k += 1;
    }
    (k, k)
}

pub fn build(kx: usize, ky: usize) -> Demo {
    let tol = Tol::default();
    let strips = grid(kx, ky);
    let joined = join(&strips, tol);
    let ok = joined.is_ok();
    let mut meshes = Vec::new();
    let mut walls = Vec::with_capacity(strips.len());
    let mut push = |element: u32, tag: u32, solid: &Solid| {
        if let Ok(m) = solid.triangulate() {
            meshes.push(SceneMesh {
                element,
                tag,
                positions: m
                    .positions
                    .iter()
                    .map(|p| [p.x as f32, p.y as f32, p.z as f32])
                    .collect(),
                triangles: m.triangles,
            });
        }
    };
    for (i, s) in strips.iter().enumerate() {
        let plain = ddc_geom::layered::plain_pieces(s);
        let pieces = match &joined {
            Ok(j) => &j.pieces[i],
            Err(_) => &plain,
        };
        let Ok(fp) = layer_footprints(s, pieces, i, tol) else {
            continue;
        };
        let qty = layer_quantities(s, &fp, &[], i).unwrap_or_default();
        walls.push(WallInfo {
            length: s.length(),
            layers: s
                .layers
                .iter()
                .zip(qty.iter().map(|q| q.volume).chain(std::iter::repeat(0.0)))
                .map(|(l, v)| (l.thickness, v))
                .collect(),
        });
        for (l, solids) in layer_solids(s, &fp, tol).iter().enumerate() {
            solids.iter().for_each(|so| push(i as u32, l as u32, so));
        }
    }
    if let Ok(j) = &joined {
        for (k, f) in j.fillers.iter().enumerate() {
            if let Ok(p) = Profile::new(f.clone(), vec![], tol) {
                push(
                    (strips.len() + k) as u32,
                    3,
                    &Solid::extrude(&p, HEIGHT, tol),
                );
            }
        }
    }
    Demo {
        scene: Scene::new(meshes),
        walls,
        joined: ok,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(grid_for_rooms(100), (10, 10));
        assert_eq!(grid(10, 10).len(), 220);
        let (k, _) = grid_for_walls(10_000);
        assert!(2 * k * (k + 1) >= 10_000 && 2 * (k - 1) * k < 10_000);
    }

    #[test]
    fn small_plan_builds_joined_and_pickable() {
        let d = build(2, 2);
        assert!(d.joined, "join failed");
        assert_eq!(d.walls.len(), 12);
        assert!(d.scene.meshes.len() >= 36);
        let core = d.walls[0].layers[1];
        assert!(core.1 > 0.0 && (core.0 - 0.2).abs() < 1e-12);
        let b = d.scene.bounds.unwrap();
        assert!((b.max.z - 3.0).abs() < 1e-5);
        // a ray from above at the middle of the first wall (axis y=0, x=2) hits wall 0
        let r = ddc_render::Ray {
            origin: ddc_render::glam::Vec3::new(2.0, 0.0, 10.0),
            dir: -ddc_render::glam::Vec3::Z,
        };
        assert_eq!(d.scene.pick(&r).map(|h| h.element), Some(0));
    }
}
