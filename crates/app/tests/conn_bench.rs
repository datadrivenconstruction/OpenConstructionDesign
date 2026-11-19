//! Steel connection bench on a plain member grid (columns, beams, diagonal braces; no named
//! object): two equal batches placed side by side in one model. Auto connections of the second
//! batch cost what the first did, each connection stays within a time budget, and the 3D scene
//! draws repeated connection parts as instances, not one mesh per bolt.

use ddc_app::edit3d::scene3d;
use ddc_app::editor::Editor;
use ddc_geom::steel::lattice::{Frame, MemRole};
use ddc_geom::v3;
use std::time::Instant;

/// Bays per side, storeys, bay and storey size (m).
const BAYS: usize = 8;
const STOREYS: usize = 12;
const BAY: f64 = 6.0;
const STOREY: f64 = 4.0;

/// One batch: a column at every grid node per storey, beams along both axes, one diagonal
/// brace in every vertical bay; `dx` shifts it clear of the other batch.
fn grid(dx: f64) -> Frame {
    let mut f = Frame::new();
    let up = v3(0.0, 0.0, 1.0);
    let p = |i: usize, j: usize, k: usize| v3(dx + i as f64 * BAY, j as f64 * BAY, k as f64 * STOREY);
    for k in 0..STOREYS {
        for i in 0..=BAYS {
            for j in 0..=BAYS {
                f.add(p(i, j, k), p(i, j, k + 1), "HEB 300", MemRole::Column, v3(1.0, 0.0, 0.0));
                if i < BAYS {
                    f.add(p(i, j, k + 1), p(i + 1, j, k + 1), "IPE 300", MemRole::Beam, up);
                    f.add(p(i, j, k), p(i + 1, j, k + 1), "L 100x100x10", MemRole::Diagonal, v3(0.0, 1.0, 0.0));
                }
                if j < BAYS {
                    f.add(p(i, j, k + 1), p(i, j + 1, k + 1), "IPE 300", MemRole::Beam, up);
                    f.add(p(i, j, k), p(i, j + 1, k + 1), "L 100x100x10", MemRole::Diagonal, v3(1.0, 0.0, 0.0));
                }
            }
        }
    }
    f
}

fn phases() -> String {
    let mut v = ddc_model::perf::take();
    v.sort_by(|a, b| b.ms.total_cmp(&a.ms));
    v.iter()
        .take(14)
        .map(|p| format!("{} {:.0} ms x{}", p.name, p.ms, p.calls))
        .collect::<Vec<_>>()
        .join("; ")
}

#[test]
#[ignore = "heavy: cargo test --release -p ddc-app --test conn_bench -- --ignored --nocapture"]
fn grid_connections_scale_with_the_batch_and_the_scene_instances_parts() {
    let mut ed = Editor::new_in_memory().unwrap();
    let level = ed.level;
    ddc_model::perf::enable(true);
    let (mut secs, mut counts, mut members) = (vec![], vec![], 0usize);
    for b in 0..2 {
        let f = grid(b as f64 * (BAYS as f64 * BAY + 40.0));
        members += f.members.len();
        let ids = ed.model.add_steel_frame(level, &f).unwrap();
        ddc_model::perf::take();
        let t = Instant::now();
        counts.push(ed.model.connect_steel_frame(&ids).unwrap().len());
        secs.push(t.elapsed().as_secs_f64());
        eprintln!("conn bench batch {b}: {} joints in {:.2} s; {}", counts[b], secs[b], phases());
    }
    ed.reload().unwrap();
    ddc_model::perf::take();
    let t = Instant::now();
    let (sc, _) = scene3d::compose(&mut ed);
    let scene = t.elapsed().as_secs_f64();
    let tris: usize = sc.meshes.iter().map(|m| m.triangles.len()).sum();
    let groups = sc.instanced.len();
    let placed: usize = sc.instanced.iter().map(|g| g.inst.len()).sum();
    let conns: usize = counts.iter().sum();
    eprintln!(
        "conn bench: {members} members, {conns} joints; scene {scene:.2} s, {} meshes, {tris} triangles, {groups} instance groups of {placed}; {}",
        sc.meshes.len(),
        phases()
    );
    assert!(counts[0] > 1000 && counts[1] == counts[0], "joints {counts:?}");
    // the second batch must not pay for the first one already in the model
    assert!(secs[1] <= 1.3 * secs[0] + 0.5, "second batch {:.2} s, first {:.2} s", secs[1], secs[0]);
    let per_ms = 1e3 * secs[0] / counts[0] as f64;
    assert!(per_ms <= 4.0, "{per_ms:.2} ms per connection");
    // repeated parts are instances: plain meshes stay a few per connection
    assert!(sc.meshes.len() <= 4 * conns + members + 1000, "{} meshes for {conns} joints", sc.meshes.len());
    assert!(placed > 0);
    assert!(scene <= 15.0, "scene {scene:.2} s");
}
