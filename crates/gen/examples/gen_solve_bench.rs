// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Circulation score and steer timing per task, one JSON line each, then a summary line:
//! `cargo run --release -p ddc-gen --example gen_solve_bench [-- task ...]`.
//! Tasks: the twelve eval briefs (`tests/eval.rs`) and the bench briefs (`data/bench`).
//! Circulation score: 100 up to a quarter of the room area in circulation, 0 from a half.
//! Steer: the brief built warm, then one room area edited and rebuilt with the score, as the
//! editor does; `DDC_STEER_REPS` edits per task (default 7), p50/p95 of their wall time.
//! `steer_*`: the free layout, on briefs where it searches zones (the solver target);
//! `auto_steer_*`: `layout.strategy` auto, which also builds and serves the band.

#[path = "../bench/suite.rs"]
#[allow(dead_code)]
mod suite;
#[path = "../tests/eval.rs"]
#[allow(dead_code)]
mod eval;

use ddc_gen::project::score::score;
use ddc_gen::project::{build_project, parse_spec, steer, RoomKind};
use serde_json::{json, Value as J};
use std::time::Instant;

fn circ_score(ratio_pct: f64) -> f64 {
    (100.0 * (0.5 - ratio_pct / 100.0) / 0.25).clamp(0.0, 100.0)
}

fn with_strategy(v: &J, s: &str) -> J {
    let mut a = v.clone();
    a["rules"]["layout.strategy"] = json!(s);
    a
}

/// The brief with one room area changed by `k` half metres: the last ground room, else the last
/// typical room, else the last room of the first apartment; else one floor more.
fn edit(v: &J, k: usize) -> (J, &'static str) {
    let mut b = v.clone();
    let d = 0.5 * k as f64;
    for (key, what) in [("ground_rooms", "ground_room_area"), ("rooms", "room_area")] {
        if let Some(last) = b.get_mut(key).and_then(J::as_array_mut).and_then(|a| a.last_mut()) {
            if let Some(a) = last["area"].as_f64() {
                last["area"] = json!(a + d);
                return (b, what);
            }
        }
    }
    if let Some(last) = b.pointer_mut("/apartments/0/rooms").and_then(J::as_array_mut).and_then(|a| a.last_mut()) {
        if let Some(a) = last["area"].as_f64() {
            last["area"] = json!(a + d);
            return (b, "flat_room_area");
        }
    }
    let f = b["floors"].as_u64().unwrap_or(1);
    b["floors"] = json!(f + 1);
    (b, "floors")
}

fn pct(xs: &mut [f64], q: f64) -> f64 {
    if xs.is_empty() {
        return f64::NAN;
    }
    xs.sort_by(f64::total_cmp);
    let i = ((xs.len() - 1) as f64 * q).round() as usize;
    xs[i]
}

fn r1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

#[derive(Clone, Copy)]
struct Steer {
    p50: f64,
    p95: f64,
    /// Mean zones searched per edit.
    zones: f64,
    /// Zones a cold build searches: 0 when the free layout does not apply to the brief.
    cold: usize,
    /// Longest single zone search of the cold builds, ms.
    zone_ms: f64,
    edit: &'static str,
}

/// Steer timing of `v`; `None` when the brief or its edit does not build.
fn steer_ms(v: &J, reps: usize) -> Option<Steer> {
    let a = parse_spec(v).ok()?;
    let mut ms = vec![];
    let mut zones = 0usize;
    let mut kind = "";
    let mut cold = 0;
    let mut zone_ms: f64 = 0.0;
    for k in 1..=reps {
        steer::forget();
        steer::slowest_ms();
        let before = steer::zones().1;
        build_project(&a).ok()?;
        cold = steer::zones().1 - before;
        zone_ms = zone_ms.max(steer::slowest_ms());
        let (e, what) = edit(v, k);
        kind = what;
        let before = steer::zones().1;
        let t = Instant::now();
        let s = parse_spec(&e).ok()?;
        let p = build_project(&s).ok()?;
        let _ = score(&s, &p);
        ms.push(t.elapsed().as_secs_f64() * 1000.0);
        zones += steer::zones().1 - before;
    }
    let (p50, p95) = (pct(&mut ms, 0.5), pct(&mut ms, 0.95));
    Some(Steer { p50: r1(p50), p95: r1(p95), zones: r1(zones as f64 / reps as f64), cold, zone_ms: r1(zone_ms), edit: kind })
}

/// Circulation score of `v` built as it is; `None` when it does not build.
fn circ_of(v: &J) -> Option<f64> {
    let p = build_project(&parse_spec(v).ok()?).ok()?;
    let (mut circ, mut all) = (0.0, 0.0);
    for st in &p.storeys {
        for (g, r) in st.layout.rooms.iter().zip(&st.rooms) {
            let a = suite::area(&g.polygon);
            all += a;
            if r.kind.circulation() || r.kind == RoomKind::Stair || r.kind == RoomKind::Lift {
                circ += a;
            }
        }
    }
    (all > 0.0).then(|| r1(circ_score(100.0 * circ / all)))
}

fn max(xs: &[f64]) -> Option<f64> {
    xs.iter().copied().fold(None, |m: Option<f64>, x| Some(m.map_or(x, |m| m.max(x))))
}

fn main() {
    let only: Vec<String> = std::env::args().skip(1).collect();
    let pick = |n: &str| only.is_empty() || only.iter().any(|o| o == n);
    let reps: usize = std::env::var("DDC_STEER_REPS").ok().and_then(|s| s.parse().ok()).unwrap_or(7);
    let (mut band_sum, mut auto_sum, mut n_eval) = (0.0, 0.0, 0usize);
    // p95 of the free layout where it searches zones, and of auto on every brief
    let (mut free95, mut auto95) = (vec![], vec![]);
    let mut emit = |set: &str, task: &str, band: Option<f64>, auto: Option<f64>, v: &J| {
        let fr = steer_ms(&with_strategy(v, "free"), reps).filter(|s| s.cold > 0);
        let au = steer_ms(&with_strategy(v, "auto"), reps);
        free95.extend(fr.map(|s| s.p95));
        auto95.extend(au.map(|s| s.p95));
        println!(
            "{}",
            json!({"set": set, "task": task, "circ_band": band.map(r1), "circ_auto": auto.map(r1),
                   "circ_free": circ_of(&with_strategy(v, "free")),
                   "steer_p50_ms": fr.map(|s| s.p50), "steer_p95_ms": fr.map(|s| s.p95),
                   "steer_zones": fr.map(|s| s.zones), "cold_zones": fr.map(|s| s.cold), "zone_max_ms": fr.map(|s| s.zone_ms),
                   "auto_steer_p50_ms": au.map(|s| s.p50), "auto_steer_p95_ms": au.map(|s| s.p95),
                   "edit": au.or(fr).map(|s| s.edit)})
        );
    };
    for (name, v) in eval::briefs() {
        if !pick(name) {
            continue;
        }
        let (mb, _) = eval::score_brief(&v);
        let (ma, _) = eval::score_brief(&with_strategy(&v, "auto"));
        band_sum += mb["circulation"];
        auto_sum += ma["circulation"];
        n_eval += 1;
        emit("eval", name, Some(mb["circulation"]), Some(ma["circulation"]), &v);
    }
    let (mut bb, mut ba, mut nb) = (0.0, 0.0, 0usize);
    for b in suite::load_briefs() {
        if !pick(&b.name) {
            continue;
        }
        let band = suite::measure(&b).get("circulation_ratio").map(circ_score);
        let mut auto_b = b.clone();
        auto_b.spec = with_strategy(&b.spec, "auto");
        let auto = suite::measure(&auto_b).get("circulation_ratio").map(circ_score);
        if let (Some(x), Some(y)) = (band, auto) {
            bb += x;
            ba += y;
            nb += 1;
        }
        emit("bench", &b.name, band, auto, &suite::spec_of(&b));
    }
    let mean = |s: f64, n: usize| (n > 0).then(|| r1(s / n as f64));
    println!(
        "{}",
        json!({"summary": true, "eval_circ_band": mean(band_sum, n_eval), "eval_circ_auto": mean(auto_sum, n_eval),
               "bench_circ_band": mean(bb, nb), "bench_circ_auto": mean(ba, nb),
               "steer_tasks": free95.len(), "steer_p95_max_ms": max(&free95),
               "steer_p95_median_ms": (!free95.is_empty()).then(|| pct(&mut free95.clone(), 0.5)),
               "auto_steer_p95_max_ms": max(&auto95),
               "auto_steer_p95_median_ms": (!auto95.is_empty()).then(|| pct(&mut auto95.clone(), 0.5))})
    );
}
