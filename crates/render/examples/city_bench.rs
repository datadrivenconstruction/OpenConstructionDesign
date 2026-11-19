// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! City-scale benchmark with budgets (`docs/CITY1.md`):
//! `cargo run --release -p ddc-render --example city_bench -- [S|M|L] [frames] [budget_mb]`.
//! Prints one line per camera path and a JSON summary; exits with 1 when a budget fails.

use ddc_render::city::bench::{bench_city, CityBenchReport, CityBenchSpec};
use ddc_render::city::gen::CitySpec;

/// Budgets per preset: moving p95, rest p95, city update p95 (ms), proxies (MiB).
struct Budget {
    moving_ms: f64,
    rest_ms: f64,
    update_ms: f64,
    proxy_mib: u64,
    /// Triangles of the whole-city view (orbit path).
    orbit_tris: usize,
}

fn budget(preset: &str) -> Budget {
    match preset {
        "M" => Budget {
            moving_ms: 50.0,
            rest_ms: 66.0,
            update_ms: 12.0,
            proxy_mib: 256,
            orbit_tris: 3_000_000,
        },
        "L" => Budget {
            moving_ms: 66.0,
            rest_ms: 100.0,
            update_ms: 16.0,
            proxy_mib: 1024,
            orbit_tris: 3_000_000,
        },
        _ => Budget {
            moving_ms: 33.0,
            rest_ms: 50.0,
            update_ms: 8.0,
            proxy_mib: 64,
            orbit_tris: 3_000_000,
        },
    }
}

fn check(rep: &CityBenchReport, b: &Budget) -> Vec<String> {
    let mut fails = vec![];
    for p in &rep.paths {
        if p.moving.p95_ms > b.moving_ms {
            fails.push(format!("{}: moving p95 {:.1} > {} ms", p.name, p.moving.p95_ms, b.moving_ms));
        }
        if p.rest.p95_ms > b.rest_ms {
            fails.push(format!("{}: rest p95 {:.1} > {} ms", p.name, p.rest.p95_ms, b.rest_ms));
        }
        if p.update_p95_ms > b.update_ms {
            fails.push(format!("{}: update p95 {:.2} > {} ms", p.name, p.update_p95_ms, b.update_ms));
        }
        if p.peak_full_bytes > rep.budget {
            fails.push(format!("{}: full content {} > budget {}", p.name, p.peak_full_bytes, rep.budget));
        }
        if !p.settled {
            fails.push(format!("{}: did not converge at rest", p.name));
        }
        if p.name == "orbit" && p.peak_triangles > b.orbit_tris {
            fails.push(format!("orbit: {} triangles > {}", p.peak_triangles, b.orbit_tris));
        }
    }
    if rep.proxy_bytes > b.proxy_mib << 20 {
        fails.push(format!("proxies {} MiB > {} MiB", rep.proxy_bytes >> 20, b.proxy_mib));
    }
    fails
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let preset = args.first().map_or("S".to_string(), |s| s.to_ascii_uppercase());
    let Some(city) = CitySpec::preset(&preset) else {
        eprintln!("unknown preset {preset}: S, M or L");
        std::process::exit(2);
    };
    let mut spec = CityBenchSpec::new(city);
    if let Some(f) = args.get(1).and_then(|s| s.parse().ok()) {
        spec.frames = f;
    }
    if let Some(mb) = args.get(2).and_then(|s| s.parse::<u64>().ok()) {
        spec.budget = mb << 20;
    }
    let rep = match bench_city(&spec) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("city bench: {e}");
            std::process::exit(2);
        }
    };
    println!(
        "city {preset}: {} projects, {} buildings, {} units, {} elements, source {} MiB, \
         budget {} MiB, proxies {} MiB, first image {:.0} ms, adapter {}",
        rep.projects,
        rep.buildings,
        rep.units,
        rep.elements,
        rep.source_bytes >> 20,
        rep.budget >> 20,
        rep.proxy_bytes >> 20,
        rep.first_image_ms,
        rep.adapter
    );
    let mut json = vec![];
    for p in &rep.paths {
        println!(
            "{:8} moving p50 {:6.2} p95 {:6.2} ms | rest p95 {:6.2} ms | update p95 {:5.2} max \
             {:6.2} ms | settle {:6.0} ms{} | full peak {:5} MiB | tris peak {:9} | held {}",
            p.name,
            p.moving.p50_ms,
            p.moving.p95_ms,
            p.rest.p95_ms,
            p.update_p95_ms,
            p.update_max_ms,
            p.settle_ms,
            if p.settled { "" } else { " (no)" },
            p.peak_full_bytes >> 20,
            p.peak_triangles,
            p.max_held
        );
        json.push(format!(
            "\"{}\":{{\"moving_p95\":{:.2},\"rest_p95\":{:.2},\"update_p95\":{:.2},\
             \"settle_ms\":{:.0},\"full_peak\":{},\"tris_peak\":{}}}",
            p.name,
            p.moving.p95_ms,
            p.rest.p95_ms,
            p.update_p95_ms,
            p.settle_ms,
            p.peak_full_bytes,
            p.peak_triangles
        ));
    }
    let fails = check(&rep, &budget(&preset));
    println!(
        "CITY_BENCH {{\"preset\":\"{preset}\",\"backend\":\"{}\",\"first_image_ms\":{:.0},\
         \"proxy_bytes\":{},\"pass\":{},{}}}",
        rep.backend,
        rep.first_image_ms,
        rep.proxy_bytes,
        fails.is_empty(),
        json.join(",")
    );
    if !fails.is_empty() {
        for f in &fails {
            eprintln!("BUDGET FAIL {f}");
        }
        std::process::exit(1);
    }
}
