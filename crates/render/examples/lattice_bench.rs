// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! lattice_tower_300 at full detail drawn as instanced prototypes:
//! `cargo run --release -p ddc-render --example lattice_bench -- [levels] [frames]`.
//! Prints generation time, mesh preparation time, counts and the frame times.

use ddc_geom::steel::hier::{self, HierRules};
use ddc_geom::steel::hier_render;
use ddc_geom::steel::tower::TowerParams;
use ddc_render::glam::{Mat4, Vec4};
use ddc_render::scene::{Scene, SceneMesh};
use ddc_render::synth::bench;
use ddc_render::{Inst, Instanced, Quality};
use std::sync::Arc;
use std::time::Instant;

fn mesh(m: &hier_render::LodMesh, element: u32) -> Arc<SceneMesh> {
    Arc::new(SceneMesh { element, tag: 3, positions: m.positions.clone(), triangles: m.triangles.clone() })
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let levels: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(3);
    let frames: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(120);
    let t = Instant::now();
    let h = hier::tower(&TowerParams::default(), &HierRules { levels, ..Default::default() }).expect("lattice_tower_300");
    let gen_ms = t.elapsed().as_secs_f64() * 1e3;
    let tot = h.totals();
    let t = Instant::now();
    let (groups, rolled) = hier_render::groups(&h);
    let mut element = 1u32;
    let mut inst_groups = vec![];
    let (mut tris, mut instances, mut mesh_bytes) = (0usize, 0usize, 0usize);
    for (gi, g) in groups.iter().enumerate() {
        let inst: Vec<Inst> = g
            .xfs
            .iter()
            .map(|x| {
                element += 1;
                let m = Mat4::from_cols(Vec4::new(x[0], x[1], x[2], 0.0), Vec4::new(x[3], x[4], x[5], 0.0), Vec4::new(x[6], x[7], x[8], 0.0), Vec4::new(x[9], x[10], x[11], 1.0));
                Inst { m, element }
            })
            .collect();
        tris += g.lods[0].0.triangles.len() * inst.len();
        instances += inst.len();
        mesh_bytes += g.lods.iter().map(|l| l.0.bytes()).sum::<usize>();
        let lods: Vec<(Arc<SceneMesh>, f32)> = g.lods.iter().skip(1).map(|(m, e)| (mesh(m, 0), *e)).collect();
        inst_groups.push(Instanced::new(gi as u64 + 1, mesh(&g.lods[0].0, 0), inst).with_lods(lods));
    }
    tris += rolled.triangles.len();
    let scene = Scene::shared(vec![mesh(&rolled, 1)]).with_instanced(inst_groups);
    let prep_ms = t.elapsed().as_secs_f64() * 1e3;
    println!(
        "lattice_tower_300 levels {levels}: gen {gen_ms:.0} ms, prep {prep_ms:.0} ms, pieces {}, plates {}, rivets {}, protos {}, instances {instances}, triangles {tris}, proto mesh {:.1} MiB",
        tot.pieces,
        tot.plates,
        tot.rivets,
        h.protos.len(),
        mesh_bytes as f64 / 1048576.0
    );
    match bench(&scene, (1600, 900), frames, Quality::Medium) {
        Ok(r) => println!(
            "{} ({}): setup {:.0} ms, first frame {:.0} ms, moving p50 {:.1} p95 {:.1} ms (cpu p50 {:.1}), still p50 {:.1} p95 {:.1} ms",
            r.adapter, r.backend, r.setup_ms, r.first_frame_ms, r.moving.p50_ms, r.moving.p95_ms, r.moving.cpu_p50_ms, r.still.p50_ms, r.still.p95_ms
        ),
        Err(e) => println!("no GPU frame bench: {e}"),
    }
}
