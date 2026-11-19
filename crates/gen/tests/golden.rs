//! Fifteen projects pinned by a hash of everything the builder made, so moving the rules from
//! code into the rule set changes no project. Re-pin with `DDC_GOLDEN_UPDATE=1` only when a
//! change of result is intended.

use ddc_gen::project::{build_project, parse_spec, GProject, RoomKind};
use serde_json::{json, Value as J};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests")
}

/// The six example briefs and nine more over typologies, massings and design fields.
fn briefs() -> Vec<(String, J)> {
    let mut out = vec![];
    for name in ["apartment_block", "hotel", "house", "mixed_use", "office", "school"] {
        let text = std::fs::read_to_string(dir().join("data").join(format!("{name}.json"))).unwrap();
        out.push((format!("data_{name}"), serde_json::from_str(&text).unwrap()));
    }
    let more = [
        ("office_9", json!({"typology": "office", "floors": 9, "lift": true})),
        ("section_5", json!({"typology": "apartment_section", "floors": 5})),
        ("school_2", json!({"typology": "school", "floors": 2})),
        ("house_1", json!({"typology": "house", "floors": 1})),
        ("office_l", json!({"typology": "office", "floors": 4, "design": {"massing": "l", "facade": "fins"}})),
        ("office_court", json!({"typology": "office", "floors": 3, "design": {"massing": "courtyard"}})),
        ("tower_podium", json!({"typology": "office", "floors": 10,
            "design": {"massing": "tower_podium", "setback": 3, "terrace": true}})),
        ("cantilever", json!({"typology": "office", "floors": 5,
            "design": {"cantilever": 2, "facade": "curtain", "balconies": "balcony"}})),
        ("hotel_basement_curve", json!({"typology": "hotel", "floors": 6, "basements": 1,
            "design": {"curve": 2.5}})),
    ];
    out.extend(more.into_iter().map(|(k, v)| (k.to_string(), v)));
    out
}

/// FNV-1a over the debug text: stable across toolchains, unlike the std hasher.
fn fnv(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

fn ring_area(r: &[(f64, f64)]) -> f64 {
    let n = r.len();
    (0..n).map(|i| r[i].0 * r[(i + 1) % n].1 - r[(i + 1) % n].0 * r[i].1).sum::<f64>().abs() / 2.0
}

fn cents(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

const SANITARY: [&str; 7] = ["WC", "Washbasin", "Sink", "Bathtub", "Shower Tray", "Urinal", "Floor Drain"];

/// What a reader checks at a glance: storeys, rooms, room area by kind, the corridor share and
/// the counts of walls, openings, sanitary fixtures and air terminals.
fn metrics(p: &GProject) -> BTreeMap<String, f64> {
    let mut m = BTreeMap::new();
    let mut add = |k: String, x: f64| *m.entry(k).or_insert(0.0) += x;
    add("storeys".into(), p.storeys.len() as f64);
    let (mut total, mut corridor) = (0.0, 0.0);
    for st in &p.storeys {
        let l = &st.layout;
        add("rooms".into(), l.rooms.len() as f64);
        add("walls".into(), l.walls.len() as f64);
        add("openings".into(), l.openings.len() as f64);
        for f in &l.fixtures {
            if SANITARY.contains(&f.family) {
                add("sanitary".into(), 1.0);
            } else if matches!(f.family, "Supply Diffuser" | "Exhaust Grille") {
                add("air_terminals".into(), 1.0);
            }
        }
        for (r, pr) in l.rooms.iter().zip(&st.rooms) {
            let a = ring_area(&r.polygon);
            total += a;
            if pr.kind == RoomKind::Corridor {
                corridor += a;
            }
            add(format!("area.{:?}", pr.kind), a);
        }
    }
    add("area.total".into(), total);
    add("corridor_share".into(), if total > 0.0 { corridor / total } else { 0.0 });
    m.into_iter()
        .map(|(k, v)| {
            let x = if k == "corridor_share" { (v * 1000.0).round() / 1000.0 } else { cents(v) };
            (k, x)
        })
        .collect()
}

/// A pinned project: the hash, and the metrics to print when it differs. An entry that is a bare
/// hash string (the first pin) has no metrics.
fn pinned_of(v: &J) -> (Option<&str>, BTreeMap<String, f64>) {
    match v {
        J::String(h) => (Some(h), BTreeMap::new()),
        _ => (
            v["hash"].as_str(),
            v["metrics"].as_object().into_iter().flatten().filter_map(|(k, x)| Some((k.clone(), x.as_f64()?))).collect(),
        ),
    }
}

fn metric_diff(was: &BTreeMap<String, f64>, now: &BTreeMap<String, f64>) -> Vec<String> {
    let keys: std::collections::BTreeSet<&String> = was.keys().chain(now.keys()).collect();
    keys.into_iter()
        .filter(|k| was.get(*k) != now.get(*k))
        .map(|k| format!("{k}: {:?} -> {:?}", was.get(k), now.get(k)))
        .collect()
}

#[test]
fn the_fifteen_pinned_projects_build_unchanged() {
    let mut now: BTreeMap<String, (String, BTreeMap<String, f64>)> = BTreeMap::new();
    for (name, v) in briefs() {
        let s = parse_spec(&v).unwrap_or_else(|e| panic!("{name}: {e}"));
        let p = build_project(&s).unwrap_or_else(|e| panic!("{name}: {e}"));
        now.insert(name, (fnv(&format!("{p:?}")), metrics(&p)));
    }
    assert_eq!(now.len(), 15);
    let file = dir().join("golden_projects.json");
    if std::env::var("DDC_GOLDEN_UPDATE").is_ok() {
        let out: BTreeMap<&String, J> = now.iter().map(|(k, (h, m))| (k, json!({"hash": h, "metrics": m}))).collect();
        std::fs::write(&file, serde_json::to_string_pretty(&out).unwrap() + "\n").unwrap();
        return;
    }
    let text = std::fs::read_to_string(&file).expect("golden_projects.json: pin with DDC_GOLDEN_UPDATE=1");
    let pinned: BTreeMap<String, J> = serde_json::from_str(&text).unwrap();
    let mut changed = vec![];
    for (name, (hash, m)) in &now {
        let (was_hash, was_m) = pinned.get(name).map(pinned_of).unwrap_or((None, BTreeMap::new()));
        if was_hash != Some(hash.as_str()) {
            let d = metric_diff(&was_m, m);
            let why = if was_m.is_empty() {
                "not pinned with metrics".to_string()
            } else if d.is_empty() {
                "same metrics, geometry differs".to_string()
            } else {
                d.join("; ")
            };
            changed.push(format!("{name}: {why}"));
        }
    }
    assert!(changed.is_empty(), "projects changed:\n{}", changed.join("\n"));
}
