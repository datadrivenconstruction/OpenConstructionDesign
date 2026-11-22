// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! AI build evaluation suite: twelve typical briefs, the spec a scripted model hands to
//! `generate_project`, built by the same deterministic code, scored on eight metrics and four
//! depth levels. Headless, no network, no keys. The scored report goes to
//! `target/ddc-eval-report.json` (or `DDC_EVAL_OUT`); the gate fails when any level drops more
//! than 3 points below `tests/eval_baseline.json`. Re-pin with `DDC_EVAL_UPDATE=1`.

use ddc_gen::project::{
    build_project, check_project, parse_spec, GProject, PStorey, ProjectSpec, RoomKind, Typology,
};
use ddc_gen::{GOpening, GRoom, GWall, WallRole};
use serde_json::{json, Map, Value as J};
use std::collections::BTreeMap;

type P = (f64, f64);

const METRICS: [&str; 8] = [
    "programme_fit",
    "daylight",
    "circulation",
    "wet_stacking",
    "structural_continuity",
    "closed_rooms",
    "norm_violations",
    "documentation",
];

/// Metrics that count at each depth level; a deeper level includes the shallower ones.
fn level_metrics(level: usize) -> &'static [&'static str] {
    match level {
        1 => &["closed_rooms", "circulation"],
        2 => &["closed_rooms", "circulation", "programme_fit", "daylight"],
        3 => &[
            "closed_rooms",
            "circulation",
            "programme_fit",
            "daylight",
            "wet_stacking",
            "structural_continuity",
            "norm_violations",
        ],
        _ => &METRICS,
    }
}

/// The briefs, also read by `examples/gen_solve_bench.rs`.
pub fn briefs() -> Vec<(&'static str, J)> {
    let home = json!([
        {"kind": "living", "area": 25},
        {"kind": "kitchen", "area": 12},
        {"kind": "bedroom", "area": 14, "count": 3},
        {"kind": "bath", "area": 5},
        {"kind": "wc", "area": 2}
    ]);
    vec![
        (
            "house_1_storey",
            json!({"typology": "house", "floors": 1, "rooms": home}),
        ),
        (
            "house_2_storeys",
            json!({"typology": "house", "floors": 2, "rooms": home}),
        ),
        (
            "townhouse",
            json!({"typology": "house", "floors": 3,
                   "footprint": {"width": 7, "depth": 12}, "rooms": home}),
        ),
        (
            "section_9_storeys",
            json!({"typology": "apartment_section", "floors": 9}),
        ),
        (
            "office_4_storeys",
            json!({"typology": "office", "floors": 4, "lang": "ru",
                   "rooms": [{"kind": "office", "area": 30, "count": 4},
                             {"kind": "meeting", "area": 40},
                             {"kind": "wc", "area": 4, "count": 2}],
                   "ground_rooms": [{"kind": "lobby", "area": 60},
                                    {"kind": "office", "area": 30, "count": 3},
                                    {"kind": "wc", "area": 4, "count": 2}]}),
        ),
        (
            "school_3_storeys",
            json!({"typology": "school", "floors": 3, "lang": "ru"}),
        ),
        (
            "clinic",
            json!({"typology": "office", "floors": 2,
                   "rooms": [{"kind": "office", "area": 16, "count": 8},
                             {"kind": "wc", "area": 4, "count": 2}],
                   "ground_rooms": [{"kind": "lobby", "area": 40},
                                    {"kind": "office", "area": 16, "count": 4}]}),
        ),
        (
            "warehouse",
            json!({"typology": "office", "floors": 1,
                   "footprint": {"width": 36, "depth": 24},
                   "rooms": [{"kind": "storage", "area": 200, "count": 2},
                             {"kind": "office", "area": 20, "count": 2},
                             {"kind": "wc", "area": 4}]}),
        ),
        (
            "cafe",
            json!({"typology": "office", "floors": 1,
                   "rooms": [{"kind": "lobby", "area": 60}, {"kind": "kitchen", "area": 25},
                             {"kind": "storage", "area": 10},
                             {"kind": "wc", "area": 4, "count": 2}]}),
        ),
        (
            "hotel",
            json!({"typology": "apartment_section", "floors": 5,
                   "apartments": [{"name": "room", "count": 4,
                                   "rooms": [{"kind": "bedroom", "area": 18},
                                             {"kind": "bath", "area": 4}]}]}),
        ),
        (
            "parking",
            json!({"typology": "office", "floors": 3,
                   "footprint": {"width": 40, "depth": 30},
                   "rooms": [{"kind": "technical", "area": 20}]}),
        ),
        (
            "mixed_use",
            json!({"typology": "office", "floors": 5,
                   "ground_rooms": [{"kind": "lobby", "area": 50},
                                    {"kind": "office", "area": 40, "count": 3}],
                   "rooms": [{"kind": "office", "area": 25, "count": 4},
                             {"kind": "wc", "area": 4}]}),
        ),
    ]
}

fn area(poly: &[P]) -> f64 {
    let n = poly.len();
    let s: f64 = (0..n)
        .map(|i| {
            let (a, b) = (poly[i], poly[(i + 1) % n]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum();
    s.abs() / 2.0
}

fn dist_seg(p: P, a: P, b: P) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let l2 = dx * dx + dy * dy;
    let t = if l2 == 0.0 {
        0.0
    } else {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0)
    };
    (p.0 - a.0 - t * dx).hypot(p.1 - a.1 - t * dy)
}

fn dist_poly(p: P, poly: &[P]) -> f64 {
    let n = poly.len();
    (0..n)
        .map(|i| dist_seg(p, poly[i], poly[(i + 1) % n]))
        .fold(f64::MAX, f64::min)
}

fn centroid(r: &GRoom) -> P {
    let n = r.polygon.len().max(1) as f64;
    (
        r.polygon.iter().map(|p| p.0).sum::<f64>() / n,
        r.polygon.iter().map(|p| p.1).sum::<f64>() / n,
    )
}

fn opening_centre(w: &GWall, o: &GOpening) -> P {
    let (dx, dy) = (w.b.0 - w.a.0, w.b.1 - w.a.1);
    let l = dx.hypot(dy).max(1e-9);
    let d = o.along + o.width / 2.0;
    (w.a.0 + dx / l * d, w.a.1 + dy / l * d)
}

fn pct(ok: usize, total: usize) -> f64 {
    if total == 0 {
        100.0
    } else {
        100.0 * ok as f64 / total as f64
    }
}

fn programme_fit(s: &ProjectSpec, p: &GProject) -> f64 {
    let house = s.typology == Typology::House;
    let mut asked = 0usize;
    let mut met = 0usize;
    // A house brief is one list for the whole building: match it across all storeys.
    let all_rooms: Vec<(RoomKind, f64)> = p
        .storeys
        .iter()
        .flat_map(|t| t.layout.rooms.iter().zip(&t.rooms))
        .map(|(g, r)| (r.kind, area(&g.polygon)))
        .collect();
    for (k, st) in p.storeys.iter().enumerate() {
        let reqs = if house {
            if k == 0 {
                s.rooms.clone()
            } else {
                Vec::new()
            }
        } else if k == 0 && !s.ground_rooms.is_empty() {
            s.ground_rooms.clone()
        } else {
            s.rooms.clone()
        };
        let pool: Vec<(RoomKind, f64)> = if house {
            all_rooms.clone()
        } else {
            st.layout
                .rooms
                .iter()
                .zip(&st.rooms)
                .map(|(g, r)| (r.kind, area(&g.polygon)))
                .collect()
        };
        for r in reqs {
            let found = pool
                .iter()
                .filter(|(kind, a)| *kind == r.kind && (a - r.area).abs() <= 0.1 * r.area)
                .count();
            asked += r.count as usize;
            met += found.min(r.count as usize);
        }
    }
    let total: f64 = all_rooms.iter().map(|r| r.1).sum();
    let leftover: f64 = p
        .storeys
        .iter()
        .flat_map(|t| t.layout.rooms.iter().zip(&t.rooms))
        .filter(|(_, r)| r.kind == RoomKind::Storage && r.asked.is_none())
        .map(|(g, _)| area(&g.polygon))
        .sum();
    let penalty = if total > 0.0 {
        200.0 * leftover / total
    } else {
        0.0
    };
    (pct(met, asked) - penalty).clamp(0.0, 100.0)
}

fn daylight(p: &GProject) -> f64 {
    let (mut ok, mut all) = (0, 0);
    for st in &p.storeys {
        for (g, r) in st.layout.rooms.iter().zip(&st.rooms) {
            if !r.kind.habitable() {
                continue;
            }
            all += 1;
            let glass: f64 = st
                .layout
                .openings
                .iter()
                .filter(|o| !o.door)
                .filter_map(|o| st.layout.walls.get(o.wall).map(|w| (w, o)))
                .filter(|(w, o)| {
                    w.role == WallRole::Exterior
                        && dist_poly(opening_centre(w, o), &g.polygon) < 0.3
                })
                .map(|(_, o)| o.width * o.height)
                .sum();
            // Glazing of at least an eighth of the floor.
            if glass >= area(&g.polygon) / 8.0 {
                ok += 1;
            }
        }
    }
    pct(ok, all)
}

fn circulation(p: &GProject) -> f64 {
    let (mut circ, mut total) = (0.0, 0.0);
    for st in &p.storeys {
        for (g, r) in st.layout.rooms.iter().zip(&st.rooms) {
            let a = area(&g.polygon);
            total += a;
            if r.kind.circulation() || r.kind == RoomKind::Stair || r.kind == RoomKind::Lift {
                circ += a;
            }
        }
    }
    let ratio = if total > 0.0 { circ / total } else { 1.0 };
    // Full marks up to a quarter of the area, none from a half.
    (100.0 * (0.5 - ratio) / 0.25).clamp(0.0, 100.0)
}

fn wet_stacking(p: &GProject) -> f64 {
    let wet = |st: &PStorey| -> Vec<P> {
        st.layout
            .rooms
            .iter()
            .zip(&st.rooms)
            .filter(|(_, r)| matches!(r.kind, RoomKind::Bath | RoomKind::Wc))
            .map(|(g, _)| centroid(g))
            .collect()
    };
    let (mut ok, mut all) = (0, 0);
    for k in 1..p.storeys.len() {
        let below = wet(&p.storeys[k - 1]);
        for c in wet(&p.storeys[k]) {
            all += 1;
            if below.iter().any(|b| (b.0 - c.0).hypot(b.1 - c.1) <= 1.5) {
                ok += 1;
            }
        }
    }
    pct(ok, all)
}

/// Violations: a column with no column under it, or one standing inside a wall.
fn structural(p: &GProject) -> (f64, usize) {
    let (mut bad, mut cols) = (0usize, 0usize);
    for (k, st) in p.storeys.iter().enumerate() {
        for c in &st.layout.columns {
            cols += 1;
            let supported = k == 0
                || p.storeys[k - 1]
                    .layout
                    .columns
                    .iter()
                    .any(|u| (u.at.0 - c.at.0).hypot(u.at.1 - c.at.1) < 0.05);
            let half = c.section.0.max(c.section.1) / 2.0;
            let clash = st
                .layout
                .walls
                .iter()
                .any(|w| dist_seg(c.at, w.a, w.b) < half + 0.15 - 1e-6);
            bad += usize::from(!supported) + usize::from(clash);
        }
    }
    let score = 100.0 * (1.0 - bad as f64 / cols.max(1) as f64).clamp(0.0, 1.0);
    (score, bad)
}

fn closed_rooms(p: &GProject) -> f64 {
    let (mut ok, mut all) = (0, 0);
    for st in &p.storeys {
        for g in &st.layout.rooms {
            all += 1;
            let n = g.polygon.len();
            let closed = n >= 3
                && (0..n).all(|i| {
                    let (a, b) = (g.polygon[i], g.polygon[(i + 1) % n]);
                    let mid = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
                    st.layout
                        .walls
                        .iter()
                        .any(|w| dist_seg(mid, w.a, w.b) < 0.25)
                });
            ok += usize::from(closed);
        }
    }
    pct(ok, all)
}

fn norms(s: &ProjectSpec, p: &GProject) -> (f64, usize) {
    let g = check_project(s, p);
    let n = g.failures.len() + g.warnings.len();
    (
        (100.0 - 10.0 * g.failures.len() as f64 - 2.0 * g.warnings.len() as f64).max(0.0),
        n,
    )
}

fn documentation(s: &ProjectSpec, p: &GProject) -> (f64, Vec<(&'static str, bool)>) {
    let names: Vec<&str> = p.storeys.iter().map(|t| t.name.as_str()).collect();
    let mut nums: Vec<&str> = p
        .storeys
        .iter()
        .flat_map(|t| t.rooms.iter().map(|r| r.number.as_str()))
        .collect();
    let n_rooms = nums.len();
    nums.sort_unstable();
    nums.dedup();
    let doors_ok = p.storeys.iter().all(|st| {
        st.layout.rooms.iter().all(|g| {
            st.layout.openings.iter().filter(|o| o.door).any(|o| {
                st.layout
                    .walls
                    .get(o.wall)
                    .is_some_and(|w| dist_poly(opening_centre(w, o), &g.polygon) < 0.3)
            })
        })
    });
    let items = vec![
        ("levels_named", names.iter().all(|n| !n.is_empty())),
        (
            "levels_in_project_language",
            names.iter().all(|n| n.is_ascii() != s.ru),
        ),
        (
            "room_numbers_unique",
            nums.len() == n_rooms && !nums.contains(&""),
        ),
        (
            "rooms_named",
            p.storeys
                .iter()
                .all(|t| t.layout.rooms.iter().all(|r| !r.name.is_empty())),
        ),
        ("grid_present", !p.storeys[0].layout.grids.is_empty()),
        ("door_into_every_room", doors_ok),
        ("roof_present", p.roof.ring.len() >= 3),
        // Not modelled by the generator yet: honest zeros for now.
        ("fixtures_in_wet_rooms", false),
        ("door_window_schedules", false),
    ];
    let ok = items.iter().filter(|i| i.1).count();
    (pct(ok, items.len()), items)
}

fn r1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

pub fn score_brief(v: &J) -> (BTreeMap<&'static str, f64>, J) {
    let refused = |why: String| -> (BTreeMap<&'static str, f64>, J) {
        (
            METRICS.iter().map(|m| (*m, 0.0)).collect(),
            json!({"refused": why}),
        )
    };
    let s = match parse_spec(v) {
        Ok(s) => s,
        Err(e) => return refused(format!("{e}")),
    };
    let p = match build_project(&s) {
        Ok(p) => p,
        Err(e) => return refused(format!("{e}")),
    };
    let (structural_score, structural_bad) = structural(&p);
    let (norm_score, norm_count) = norms(&s, &p);
    let (doc_score, items) = documentation(&s, &p);
    let m: BTreeMap<&'static str, f64> = [
        ("programme_fit", programme_fit(&s, &p)),
        ("daylight", daylight(&p)),
        ("circulation", circulation(&p)),
        ("wet_stacking", wet_stacking(&p)),
        ("structural_continuity", structural_score),
        ("closed_rooms", closed_rooms(&p)),
        ("norm_violations", norm_score),
        ("documentation", doc_score),
    ]
    .into_iter()
    .map(|(k, x)| (k, r1(x)))
    .collect();
    let missing: Vec<&str> = items.iter().filter(|i| !i.1).map(|i| i.0).collect();
    let detail = json!({
        "storeys": p.storeys.len(),
        "structural_violations": structural_bad,
        "norm_findings": norm_count,
        "documentation_missing": missing,
    });
    (m, detail)
}

fn run() -> J {
    let list = briefs();
    let mut rows = Map::new();
    let mut sums: BTreeMap<&str, f64> = BTreeMap::new();
    for (name, spec) in &list {
        let (m, detail) = score_brief(spec);
        for (k, x) in &m {
            *sums.entry(*k).or_default() += x;
        }
        rows.insert((*name).to_string(), json!({"metrics": m, "detail": detail}));
    }
    let n = list.len() as f64;
    let mean: BTreeMap<&str, f64> = sums.iter().map(|(k, v)| (*k, r1(v / n))).collect();
    let mut levels = Map::new();
    for l in 1..=4 {
        let ms = level_metrics(l);
        let avg = ms.iter().map(|m| mean[m]).sum::<f64>() / ms.len() as f64;
        levels.insert(format!("D{l}"), json!(r1(avg)));
    }
    json!({"briefs": rows, "metrics_mean": mean, "levels": levels})
}

fn report_path() -> std::path::PathBuf {
    std::env::var_os("DDC_EVAL_OUT").map_or_else(
        || {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/ddc-eval-report.json")
        },
        Into::into,
    )
}

#[test]
fn eval_is_deterministic() {
    assert_eq!(run(), run());
}

/// The twelve briefs with `layout.strategy` auto: free placement where it applies and scores
/// better. Target of the free placement: the circulation metric from 41.6 (band baseline) to
/// 60, i.e. a mean circulation share of at most 35 % of the floor area; no brief the band
/// builds may be refused.
#[test]
fn free_placement_lifts_circulation() {
    let list = briefs();
    let (mut band, mut auto) = (0.0, 0.0);
    let mut lost = vec![];
    for (name, v) in &list {
        let mut a = v.clone();
        a["rules"]["layout.strategy"] = json!("auto");
        let (mb, _) = score_brief(v);
        let (ma, _) = score_brief(&a);
        eprintln!("{name}: circulation {} -> {}, programme_fit {} -> {}", mb["circulation"], ma["circulation"], mb["programme_fit"], ma["programme_fit"]);
        if ma["closed_rooms"] < mb["closed_rooms"] {
            lost.push(*name);
        }
        band += mb["circulation"];
        auto += ma["circulation"];
    }
    let n = list.len() as f64;
    assert!(lost.is_empty(), "auto refuses or opens rooms the band closes: {lost:?}");
    assert!(auto / n >= 60.0, "circulation {:.1} -> {:.1}", band / n, auto / n);
}

#[test]
fn eval_scores_hold_against_the_baseline() {
    let report = run();
    let out = report_path();
    if let Some(dir) = out.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(&out, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    eprintln!("eval levels: {}", report["levels"]);

    let base_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/eval_baseline.json");
    if std::env::var_os("DDC_EVAL_UPDATE").is_some() {
        let b = json!({"levels": report["levels"], "metrics_mean": report["metrics_mean"]});
        std::fs::write(&base_path, serde_json::to_string_pretty(&b).unwrap() + "\n").unwrap();
        return;
    }
    let base: J = serde_json::from_str(
        &std::fs::read_to_string(&base_path).expect("tests/eval_baseline.json is committed"),
    )
    .unwrap();
    let mut drops = Vec::new();
    for (level, was) in base["levels"].as_object().into_iter().flatten() {
        let was = was.as_f64().unwrap();
        let now = report["levels"][level].as_f64().unwrap_or(0.0);
        if now < was - 3.0 {
            drops.push(format!("{level}: {was} -> {now}"));
        }
    }
    assert!(
        drops.is_empty(),
        "eval score dropped more than 3 points: {drops:?}"
    );
}
