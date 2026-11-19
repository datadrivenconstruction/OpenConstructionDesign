// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The free-form museum briefs (H06 English, H13 Chinese) end to end through the tool path: the
//! assistant drafts the project, Apply writes it in one step, and every oracle of the eval case
//! reads the model the way the judge does.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_app::ai::eval::case::{evals_dir, Case};
use ddc_app::ai::eval::facts::Facts;
use ddc_app::ai::eval::oracle::{evaluate, EvalCx};
use ddc_app::ai::receipt::WriteReceipt;
use ddc_app::editor::Editor;
use e2e_ai_common::*;
use serde_json::json;

/// What the assistant sends for the brief: a sculptural three-level museum, about 8,000 m2.
fn museum_spec(shell: bool) -> serde_json::Value {
    let mut v = json!({
        "typology": "office", "floors": 3, "lang": "en", "name": "Museum",
        "footprint": {"width": 70.0, "depth": 40.0},
        "features": ["sculptural", "wave", "double_curvature", "curved_roof"],
        "ground_rooms": [
            {"kind": "atrium", "area": 450, "name": "Atrium"},
            {"kind": "foyer", "area": 150, "name": "Foyer"},
            {"kind": "gallery", "area": 220, "count": 5, "name": "Gallery"},
            {"kind": "cafe", "area": 120, "name": "Cafe"},
            {"kind": "retail", "area": 80, "name": "Shop"},
            {"kind": "office", "area": 40, "count": 2, "name": "Office"},
            {"kind": "storage", "area": 60, "name": "Storage"}
        ],
        "rooms": [
            {"kind": "gallery", "area": 260, "count": 4, "name": "Gallery"},
            {"kind": "office", "area": 40, "count": 3, "name": "Office"},
            {"kind": "storage", "area": 60, "name": "Storage"}
        ]
    });
    if shell {
        v["design"] = json!({"shell": true});
    }
    v
}

fn judge(id: &str, shell: bool) {
    let case = Case::load(&evals_dir().join("cases").join(format!("{id}.toml"))).unwrap();
    let mut ed = Editor::new_in_memory().unwrap();
    ed.reload().unwrap();
    let errors_before = 0;
    let (mut s, provider) = run(&mut ed, vec![tool("generate_project", json!({"spec": museum_spec(shell)}))]);
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    let pv = plan_of(&s);
    let issues: Vec<_> = pv.issues.iter().chain(&pv.main_issues).cloned().collect();
    let op = apply_and_check(&mut s, &mut ed);
    let receipt = WriteReceipt::build(&pv.diff, &pv.qty, &issues, op.max(0) as u64, true, id);
    drop(s);
    let facts = Facts::read(&ed.model).unwrap();
    let ifc = ddc_io::ifc::export_ifc(&ed.model).ok();
    let cx = EvalCx {
        ed: &ed,
        facts: &facts,
        before: None,
        answer: "",
        ifc: ifc.as_deref(),
        errors_before,
        receipts: std::slice::from_ref(&receipt),
    };
    let bad: Vec<String> = evaluate(&case, &cx)
        .into_iter()
        .filter(|o| !o.pass)
        .map(|o| format!("{}: expected {}, measured {}", o.name, o.expected, o.measured))
        .collect();
    assert!(bad.is_empty(), "{id}: {} oracles failed:\n{}", bad.len(), bad.join("\n"));
}

#[test]
fn h06_free_form_museum_meets_every_oracle() {
    judge("H06_museum_freeform", true);
}

#[test]
fn h13_chinese_free_form_museum_meets_every_oracle() {
    // the Chinese brief itself: its words load the scripts group, and the feature words the
    // assistant takes from them choose the shell with no design.shell in the spec
    let case = Case::load(&evals_dir().join("cases").join("H13_zh_museum_freeform.toml")).unwrap();
    let say = &case.turns[0].say;
    assert!(["曲面", "波浪", "流线"].iter().all(|w| say.contains(w)), "{say}");
    assert!(ddc_ai::toolset::intent_groups(say).contains("scripts"), "the zh brief loads no scripts group");
    let spec = museum_spec(false);
    assert!(spec["design"].is_null());
    let parsed = ddc_gen::project::parse_spec(&spec).unwrap_or_else(|e| panic!("{e}"));
    assert!(parsed.design.shell, "the shell is not chosen from the feature words");
    judge("H13_zh_museum_freeform", false);
}

/// Plan point `p` on the shell's reference surface: (u, v) by Newton from the nearest grid node.
fn shell_uv(s: &ddc_geom::nurbs::Surface, p: (f64, f64)) -> Option<(f64, f64)> {
    let ((u0, u1), (v0, v1)) = (s.domain_u(), s.domain_v());
    let at = |i: usize, n: usize, a: f64, b: f64| a + (b - a) * i as f64 / n as f64;
    let mut best = (f64::MAX, u0, v0);
    for i in 0..=48 {
        for j in 0..=48 {
            let (u, v) = (at(i, 48, u0, u1), at(j, 48, v0, v1));
            let q = s.point(u, v);
            let d = (q.x - p.0).powi(2) + (q.y - p.1).powi(2);
            if d < best.0 {
                best = (d, u, v);
            }
        }
    }
    let (mut u, mut v) = (best.1, best.2);
    for _ in 0..12 {
        let d = s.derivs(u, v, 1);
        let (su, sv, q) = (d[1][0], d[0][1], d[0][0]);
        let (rx, ry) = (p.0 - q.x, p.1 - q.y);
        let det = su.x * sv.y - su.y * sv.x;
        if det.abs() < 1e-12 {
            return None;
        }
        u = (u + (rx * sv.y - ry * sv.x) / det).clamp(u0, u1);
        v = (v + (su.x * ry - su.y * rx) / det).clamp(v0, v1);
    }
    let q = s.point(u, v);
    ((q.x - p.0).hypot(q.y - p.1) < 1e-4).then_some((u, v))
}

/// The museum's top-storey walls reach the free-form shell's underside within 50 mm, nothing
/// pokes through, and every opening head stays under the trimmed top.
#[test]
fn h06_walls_under_the_free_form_shell_reach_its_underside() {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.reload().unwrap();
    let (mut s, provider) = run(&mut ed, vec![tool("generate_project", json!({"spec": museum_spec(true)}))]);
    assert!(tool_results(&provider).iter().all(|r| !r.1));
    apply_and_check(&mut s, &mut ed);
    drop(s);
    let m = &ed.model;
    let shells = m.shell_elems(None).unwrap();
    assert!(!shells.is_empty(), "the museum has no shell element");
    let top_level: f64 = m.sql().query_row("SELECT max(l.elevation) FROM wall w JOIN level l ON l.id=w.level", [], |r| r.get(0)).unwrap();
    let mut st = m
        .sql()
        .prepare("SELECT w.id, w.ax, w.ay, w.bx, w.by, l.elevation + w.base_offset FROM wall w JOIN level l ON l.id=w.level WHERE l.elevation >= ?1 - 1e-9 AND w.bulge = 0")
        .unwrap();
    type Row = (i64, (f64, f64), (f64, f64), f64);
    let walls: Vec<Row> = st
        .query_map([top_level], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?), (r.get(3)?, r.get(4)?), r.get(5)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut checked = 0;
    for (id, shell) in &shells {
        let elev: f64 = m.sql().query_row("SELECT elevation FROM level WHERE id=?1", [shell.level], |r| r.get(0)).unwrap();
        let lo = shell.offsets().first().map_or(0.0, |o| o.0.min(o.1)).min(0.0);
        let under = |p: (f64, f64)| -> Option<f64> {
            let (u, v) = shell_uv(&shell.surface, p)?;
            let n = shell.surface.normal(u, v)?;
            (n.z.abs() > 0.2).then(|| elev + shell.surface.point(u, v).z + lo / n.z)
        };
        for &(w, a, b, base) in &walls {
            let len = (b.0 - a.0).hypot(b.1 - a.1);
            let pt = |u: f64| (a.0 + (b.0 - a.0) * u / len, a.1 + (b.1 - a.1) * u / len);
            let samples: Vec<f64> = (0..=(len / 0.1) as usize).map(|i| i as f64 * 0.1).collect();
            let seen = samples.iter().filter(|u| under(pt(**u)).is_some()).count();
            if seen * 10 < samples.len() * 7 {
                continue;
            }
            let top = m
                .wall_top_profile(w)
                .unwrap()
                .unwrap_or_else(|| panic!("shell {id}: wall {w} stands under it and has no trimmed top"));
            let at = |u: f64| {
                top.windows(2)
                    .find(|q| u >= q[0].0 - 1e-9 && u <= q[1].0 + 1e-9 && q[1].0 - q[0].0 > 1e-9)
                    .map_or(top.last().unwrap().1, |q| q[0].1 + (q[1].1 - q[0].1) * (u - q[0].0) / (q[1].0 - q[0].0))
            };
            for &u in &samples {
                if let Some(z) = under(pt(u)) {
                    let wz = base + at(u);
                    let near: Vec<f64> = [1e-3, -1e-3].iter().filter_map(|d| under(pt(u + d))).collect();
                    let (lo_z, hi_z) = (near.iter().cloned().fold(z, f64::min), near.iter().cloned().fold(z, f64::max));
                    assert!(wz <= hi_z + 5e-3, "wall {w} pokes through the shell at u={u:.2}: {wz} > {hi_z}");
                    assert!(lo_z - wz <= 0.05, "wall {w} stops {} m short of the shell at u={u:.2}", lo_z - wz);
                }
            }
            // opening heads stay under the trimmed top along their width
            let mut os = m.sql().prepare("SELECT along, width, sill, height FROM opening WHERE wall=?1").unwrap();
            let ops: Vec<(f64, f64, f64, f64)> =
                os.query_map([w], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap().map(Result::unwrap).collect();
            for (along, width, sill, h) in ops {
                let lowest = (0..=10).map(|i| at(along + width * i as f64 / 10.0)).fold(f64::MAX, f64::min);
                assert!(sill + h <= lowest + 1e-6, "wall {w}: opening head {} above the trimmed top {lowest}", sill + h);
            }
            checked += 1;
        }
    }
    assert!(checked > 0, "no top-storey wall stands under the shell");
}
