// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::norm_kinds::*;
use super::norm_pack::*;
use super::norms::{load_norm_pack, starter_pack};
use super::{CheckScope, NormLocale, Severity};
use crate::{LayerSpec, Model, OpeningSpec, RoomSpec, StairSpec, WallSpec};
use serde_json::json;

const LEGACY: &str = include_str!("../../data/norms/legacy/starter.json");

fn files(list: &[(&str, serde_json::Value)]) -> Vec<(String, String)> {
    list.iter().map(|(p, v)| (p.to_string(), v.to_string())).collect()
}

fn meta(id: &str, parent: Option<&str>) -> serde_json::Value {
    json!({"id": id, "version": "1", "parent": parent, "units": {"length": "m"}, "status": "draft"})
}

fn rule(id: &str, src: &str, check: serde_json::Value, unit: &str) -> serde_json::Value {
    json!({"id": id, "text": {"en": id}, "source": {"doc": src, "clause": "1"}, "severity": "error",
           "check": check, "unit": unit, "verified": true})
}

fn src(id: &str) -> serde_json::Value {
    json!({"sources": [{"id": id, "title": id.to_uppercase(), "licence": "public-law"}]})
}

fn vocab() -> serde_json::Value {
    json!({"room_kinds": {"living": ["living"], "bedroom": ["bedroom"], "kitchen": ["kitchen"],
        "bath": ["bath"], "wc": ["wc"], "hall": ["hall"], "corridor": ["corridor"], "storage": ["storage"],
        "stair": ["stair"], "office": ["office"]}})
}

fn err_of(set: &PackSet, id: &str) -> String {
    set.resolve(id).unwrap_err().to_string()
}

#[test]
fn embedded_list_matches_the_pack_folder() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data/norms/packs");
    let mut set = PackSet::default();
    set.add_dir(&dir).unwrap();
    let disk = PackSet::embedded();
    assert!(set.problems.is_empty(), "{:?}", set.problems);
    assert_eq!(set.ids(), disk.ids());
    let mut on_disk = vec![];
    fn walk(d: &std::path::Path, base: &std::path::Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, base, out)
            } else {
                out.push(p.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/"));
            }
        }
    }
    walk(&dir, &dir, &mut on_disk);
    on_disk.sort();
    let embedded: Vec<String> = EMBEDDED.iter().map(|(p, _)| p.to_string()).collect();
    assert_eq!(on_disk, embedded, "every pack file is shipped, nothing else");
}

#[test]
fn shipped_packs_resolve_without_problems() {
    let set = PackSet::embedded();
    assert!(set.problems.is_empty(), "{:?}", set.problems);
    let g = set.resolve("generic").unwrap();
    assert_eq!(g.chain, vec!["generic"]);
    assert!(g.rules.iter().all(|r| !r.verified), "generic numbers are advice only");
    let ru = set.resolve("ru").unwrap();
    assert_eq!(ru.chain, vec!["generic", "ru"]);
    for k in NEW_KINDS {
        assert!(ru.rules.iter().any(|r| r.kind == k), "ru has a {k} rule");
    }
    assert!(ru.rule("generic.programme.apartment").is_none(), "disabled in ru");
    assert_eq!(ru.programme("apartment").unwrap().pack, "ru");
    assert_eq!(ru.programme("apartment_section").unwrap().variants.len(), 6);
    assert_eq!(ru.programme("house").unwrap().pack, "generic");
    assert_eq!(ru.kind_of_name(crate::test_ru::ru("room_bedroom_2")), Some("bedroom"));
    assert_eq!(ru.kind_of_name("Bedroom"), Some("bedroom"));
    for s in &ru.sources {
        assert!(LICENCES.contains(&s.licence.as_str()), "{}", s.id);
    }
    // verified numbers are the ones the research checked against the source
    let hard: Vec<&str> = ru.rules.iter().filter(|r| r.verified).map(|r| r.id.as_str()).collect();
    assert_eq!(hard, ["ex.stair.risers", "ru.apartment.area", "ru.room.low_height_share"]);
    let v = ru.programme("apartment").unwrap().variant(Some(3)).unwrap();
    assert_eq!((v.code.as_str(), v.area_min, v.verified), ("3k", Some(56.0), true));
    assert!(v.rooms.iter().all(|r| !r.verified), "room minimums are not checked yet");
    assert_eq!(ru.dim("stair.risers_per_flight").unwrap().value, json!([3, 16]));
    assert!(ru.dim("stair.risers_per_flight").unwrap().verified);
    assert!(ru.dim("stair.tread_min").unwrap().verified, "read in clause 4.4.3");
    assert_eq!(ru.drawing["titleblock"], "gost-r-21.101-f3");
    assert_eq!(ru.drawing["layer_scheme"], "gost");
    assert_eq!(ru.drawing["sheet_sizes"], g.drawing["sheet_sizes"], "inherited");
}

#[test]
fn migration_keeps_every_starter_rule_unchanged() {
    let old = load_norm_pack(LEGACY).unwrap();
    assert_eq!(old.len(), 27);
    assert_eq!(starter_pack().unwrap(), old, "same rules, same order, same fields");
    let raw: serde_json::Value = serde_json::from_str(LEGACY).unwrap();
    let ru = PackSet::embedded().resolve("ru").unwrap();
    for r in raw["rules"].as_array().unwrap() {
        let id = r["id"].as_str().unwrap();
        let n = ru.rule(id).unwrap();
        assert_eq!(n.raw["text"], r["title"], "{id}");
        assert_eq!(n.raw["scope"], r["scope"], "{id}");
        assert_eq!(n.raw["check"], r["check"], "{id}");
        assert_eq!(n.raw["severity"], r["severity"], "{id}");
        assert_eq!(n.raw["example"], r["example"], "{id}");
        assert_eq!(ru.source(&n.source).unwrap().title, r["source"]["doc"], "{id}");
        assert_eq!(n.clause, r["source"]["clause"], "{id}");
    }
    assert_eq!(ru.meta["purposes"], raw["purposes"]);
}

fn tiny() -> Vec<(&'static str, serde_json::Value)> {
    vec![
        ("generic/pack.json", meta("generic", None)),
        ("generic/sources.json", src("iso")),
        ("generic/vocab.json", vocab()),
        (
            "generic/rules/a.json",
            json!({"rules": [
                rule("g.formula", "iso", json!({"kind":"formula","cat":"stair","expr":"2*riser+tread","min":0.6,"max":0.65}), "m"),
                rule("g.dist", "iso", json!({"kind":"distance","max":30}), "m"),
                rule("g.stack", "iso", json!({"kind":"stack","room_kind":["bath"]}), ""),
            ]}),
        ),
        (
            "generic/dims.json",
            json!({"items": [{"key": "room.height_min", "value": 2.4, "unit": "m", "source": "iso", "verified": false},
                              {"key": "door.clear_min", "value": 0.8, "unit": "m", "source": "iso", "verified": false}]}),
        ),
        ("generic/drawing.json", json!({"drawing": {"titleblock": "iso7200", "scales": ["1:100"]}})),
        ("eu/pack.json", meta("eu", Some("generic"))),
        (
            "eu/rules/a.json",
            json!({"rules": [rule("g.dist", "iso", json!({"kind":"distance","max":35}), "m")]}),
        ),
        ("de/pack.json", meta("de", Some("eu"))),
        ("de/sources.json", src("mbo")),
        ("de/vocab.json", json!({"room_kinds": {"bedroom": ["schlafzimmer"]}})),
        (
            "de/rules/a.json",
            json!({"rules": [{"id": "g.stack", "disabled": true},
                rule("de.daylight", "mbo", json!({"kind":"ratio","per":"element","cat":"room","num":"window_area","den":"area","min":0.125}), "ratio")]}),
        ),
        (
            "de/dims.json",
            json!({"items": [{"key": "room.height_min", "value": 2.4, "unit": "m", "source": "mbo", "clause": "§47", "verified": true}]}),
        ),
        ("de/drawing.json", json!({"drawing": {"folding": "din824"}})),
        ("de-by/pack.json", meta("de-by", Some("de"))),
        (
            "de-by/dims.json",
            json!({"items": [{"key": "door.clear_min", "value": 0.9, "unit": "m", "source": "mbo", "verified": false}]}),
        ),
    ]
}

#[test]
fn inheritance_merges_generic_eu_de_region() {
    let set = PackSet::from_files(&files(&tiny()));
    assert!(set.problems.is_empty(), "{:?}", set.problems);
    let p = set.resolve("de-by").unwrap();
    assert_eq!(p.chain, ["generic", "eu", "de", "de-by"]);
    let ids: Vec<&str> = p.rules.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["g.formula", "g.dist", "de.daylight"], "override in place, stack disabled");
    let dist = p.rule("g.dist").unwrap();
    assert_eq!(dist.pack, "eu");
    assert_eq!(dist.check, PackCheck::Kind(NormKind::Distance { max: 35.0, exit_kinds: vec![] }));
    assert_eq!(p.dim("room.height_min").unwrap().pack, "de");
    assert!(p.dim("room.height_min").unwrap().verified);
    assert_eq!(p.dim("door.clear_min").unwrap().value, json!(0.9));
    assert_eq!(p.drawing, json!({"titleblock": "iso7200", "scales": ["1:100"], "folding": "din824"}));
    let srcs: Vec<&str> = p.sources.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(srcs, ["iso", "mbo"]);
    assert_eq!(p.room_kinds["bedroom"], ["bedroom", "schlafzimmer"]);
    assert_eq!(p.kind_of_name("Schlafzimmer 1"), Some("bedroom"));
    // the parent stays as it was
    assert_eq!(set.resolve("eu").unwrap().rules.len(), 3);
}

#[test]
fn chain_errors_are_reported() {
    let mut f = tiny();
    f.push(("x/pack.json", meta("x", Some("missing"))));
    f.push(("a/pack.json", meta("a", Some("b"))));
    f.push(("b/pack.json", meta("b", Some("a"))));
    let set = PackSet::from_files(&files(&f));
    assert!(err_of(&set, "x").contains("pack missing not found"));
    assert!(err_of(&set, "a").contains("cycle"));
    assert!(err_of(&set, "nope").contains("not found"));
}

fn with(path: &'static str, v: serde_json::Value) -> PackSet {
    let mut f = tiny();
    f.retain(|(p, _)| *p != path);
    f.push((path, v));
    PackSet::from_files(&files(&f))
}

#[test]
fn licence_is_required_on_every_source() {
    let set = with("de/sources.json", json!({"sources": [{"id": "mbo", "title": "MBO"}]}));
    assert!(err_of(&set, "de").contains("`licence` is required"), "{}", err_of(&set, "de"));
    assert!(set.resolve("generic").is_ok(), "other chains are untouched");
    let set = with(
        "de/sources.json",
        json!({"sources": [{"id": "mbo", "title": "MBO", "licence": "free-ish"}]}),
    );
    assert!(err_of(&set, "de").contains("not one of"));
}

#[test]
fn schema_and_unit_checks_refuse_bad_packs() {
    let cases: Vec<(&'static str, serde_json::Value, &str)> = vec![
        ("de/pack.json", json!({"id": "de", "version": "1", "parent": "eu", "units": {"length": "mm"}, "status": "draft"}), "SI"),
        ("de/pack.json", json!({"id": "dx", "version": "1", "parent": "eu", "units": {"length": "m"}, "status": "draft"}), "`id` must be"),
        ("de/pack.json", json!({"id": "de", "version": "1", "parent": "eu", "units": {"length": "m"}, "status": "final"}), "status"),
        ("de/rules/a.json", json!({"rules": [rule("r", "mbo", json!({"kind":"teleport"}), "")]}), "unknown check kind"),
        ("de/rules/a.json", json!({"rules": [{"id": "r", "text": {"en": "x"}, "source": {"doc": "mbo", "clause": "1"}, "severity": "error", "check": {"kind":"distance","max":30}, "unit": "m"}]}), "`verified`"),
        ("de/rules/a.json", json!({"rules": [rule("r", "nope", json!({"kind":"distance","max":30}), "m")]}), "not in sources"),
        ("de/rules/a.json", json!({"rules": [rule("r", "mbo", json!({"kind":"distance","max":30}), "ratio")]}), "does not fit"),
        ("de/rules/a.json", json!({"rules": [rule("r", "mbo", json!({"kind":"distance","max":30}), "ft")]}), "unknown unit"),
        ("de/rules/a.json", json!({"rules": [{"id": "r", "text": {"en": "x"}, "source": {"doc": "mbo", "clause": "1"}, "severity": "error", "check": {"kind":"distance","max":30}, "unit": "m", "value": 2400, "verified": true}]}), "outside"),
        ("de/rules/a.json", json!({"rules": [rule("r", "mbo", json!({"kind":"ratio","per":"element","num":"window_area","den":"area","min":1.5}), "ratio"), rule("r", "mbo", json!({"kind":"stack","room_kind":["bath"]}), "")]}), "duplicate id"),
        ("de/rules/a.json", json!({"rules": [rule("r", "mbo", json!({"kind":"stack","room_kind":["sauna"]}), "")]}), "unknown room kind"),
        ("de/rules/a.json", json!({"rules": [rule("r", "mbo", json!({"kind":"formula","cat":"stair","expr":"2*rise+tread","min":0.6}), "m")]}), "unknown stair field"),
        ("de/rules/a.json", json!({"rules": [rule("r", "mbo", json!({"kind":"formula","cat":"stair","expr":"2*riser+","min":0.6}), "m")]}), "ends early"),
        ("de/rules/a.json", json!({"rules": [rule("r", "mbo", json!({"kind":"param_range","param":"width"}), "")]}), "rule definition"),
        ("de/dims.json", json!({"items": [{"key": "room.height_min", "value": 2400, "unit": "m", "source": "mbo", "verified": true}]}), "outside"),
        ("de/dims.json", json!({"items": [{"key": "room.height_min", "value": 2.4, "unit": "m", "source": "mbo"}]}), "`verified`"),
        ("de/dims.json", json!({"items": [{"key": "fire.compartment_area_max", "value": 2500, "unlimited": true, "unit": "m2", "source": "mbo", "verified": true}]}), "null value"),
        ("de/dims.json", json!({"items": [{"key": "fire.compartment_area_max", "value": null, "unit": "m2", "source": "mbo", "verified": true}]}), "not a number"),
        ("de/programme/house.json", json!({"typology": "house", "source": "mbo", "variants": [{"code": "1b", "verified": false, "rooms": [{"kind": "sauna"}]}]}), "unknown room kind"),
        ("de/programme/house.json", json!({"typology": "house", "source": "mbo", "variants": [{"code": "1b", "rooms": []}]}), "`verified` is required"),
        ("de/programme/house.json", json!({"typology": "house", "source": "mbo", "variants": [{"code": "1b", "verified": true, "storage_max": 5}]}), "storage_max"),
        ("de/assemblies.json", json!({"assemblies": [{"id": "w", "u_value": 28, "source": "mbo", "verified": false}]}), "u_value"),
        ("de/oops.json", json!({}), "unknown pack file"),
    ];
    for (path, v, want) in cases {
        let set = with(path, v.clone());
        let e = err_of(&set, "de-by");
        assert!(e.contains(want), "{path} {v}: {e}");
    }
    let mut f = files(&tiny());
    f.push(("de/rules/b.json".into(), "{not json".into()));
    assert!(err_of(&PackSet::from_files(&f), "de").contains("de/rules/b.json"));
}

#[test]
fn rules_by_depth_and_discipline() {
    let ru = PackSet::embedded().resolve("ru").unwrap();
    let d1: Vec<&str> = ru.rules(1, "*").iter().map(|r| r.id.as_str()).collect();
    assert!(d1.contains(&"ru.apartment.area") && !d1.contains(&"generic.stair.step"));
    assert!(ru.rules(3, "*").iter().any(|r| r.id == "generic.stair.step"));
    let fire = ru.rules(5, "fire");
    assert!(!fire.is_empty() && fire.iter().all(|r| r.discipline == "fire"));
    assert!(fire.iter().any(|r| r.id == "generic.egress.distance"));
    assert_eq!(ru.rules(5, "*").len(), ru.rules.len());
}

#[test]
fn expressions_parse_and_evaluate() {
    let e = Expr::parse("2*riser + tread").unwrap();
    let get = |n: &str| match n {
        "riser" => Some(0.17),
        "tread" => Some(0.28),
        _ => None,
    };
    assert!((e.eval(&get).unwrap() - 0.62).abs() < 1e-12);
    assert_eq!(Expr::parse("-(1+2)*3/4").unwrap().eval(&|_| None), Some(-2.25));
    assert_eq!(Expr::parse("1/0").unwrap().eval(&|_| None), None);
    assert_eq!(Expr::parse("x").unwrap().eval(&|_| None), None);
    assert!(Expr::parse("2**3").is_err());
    assert!(Expr::parse("(1").is_err());
    assert!(Expr::parse("a $ b").is_err());
}

fn room(id: i64, kind: &str, storey: usize, area: f64, bbox: [f64; 4]) -> FactRoom {
    FactRoom {
        key: ("room".into(), id),
        level: storey as i64 + 1,
        storey,
        name: kind.into(),
        kind: kind.into(),
        unit: None,
        area,
        height: 2.7,
        width: None,
        window_area: 0.0,
        centroid: ((bbox[0] + bbox[2]) / 2.0, (bbox[1] + bbox[3]) / 2.0),
        bbox,
    }
}

fn door(id: i64, at: (f64, f64), rooms: &[usize]) -> FactDoor {
    FactDoor { key: ("opening".into(), id), at, rooms: rooms.to_vec() }
}

fn tiny_pack() -> NormPack {
    PackSet::from_files(&files(&tiny())).resolve("generic").unwrap()
}

fn kind(c: serde_json::Value) -> NormKind {
    parse_kind(c["kind"].as_str().unwrap(), &c).unwrap()
}

#[test]
fn formula_checks_two_risers_plus_tread() {
    let k = kind(json!({"kind":"formula","cat":"stair","expr":"2*riser+tread","min":0.6,"max":0.65}));
    let stair = |id: i64, r: f64, t: f64| FactStair {
        key: ("stair".into(), id),
        vals: [("riser".to_string(), r), ("tread".to_string(), t)].into_iter().collect(),
    };
    let mut f = PlanFacts::default();
    f.stairs = vec![stair(1, 0.17, 0.28), stair(2, 0.2, 0.3), stair(3, 0.15, 0.25)];
    f.stairs.push(FactStair { key: ("stair".into(), 4), vals: Default::default() });
    let h = eval_kind(&k, &f, &tiny_pack(), &[]);
    assert_eq!(h.len(), 3, "{h:?}");
    assert_eq!((h[0].elements[0].1, h[0].what), (2, "max"));
    assert!((h[0].measured.unwrap() - 0.70).abs() < 1e-12 && h[0].required == Some(0.65));
    assert_eq!((h[1].elements[0].1, h[1].what), (3, "min"));
    assert!((h[1].measured.unwrap() - 0.55).abs() < 1e-12 && h[1].required == Some(0.6));
    assert_eq!((h[2].elements[0].1, h[2].what, h[2].verified), (4, "no_value", false));
}

#[test]
fn ratio_per_room_and_per_group() {
    let pack = tiny_pack();
    let mut f = PlanFacts::default();
    f.rooms = vec![
        room(1, "living", 0, 16.0, [0.0, 0.0, 4.0, 4.0]),
        room(2, "bedroom", 0, 12.0, [4.0, 0.0, 7.0, 4.0]),
        room(3, "hall", 0, 4.0, [0.0, 4.0, 2.0, 6.0]),
        room(4, "corridor", 0, 8.0, [2.0, 4.0, 6.0, 6.0]),
    ];
    f.rooms[0].window_area = 1.5; // 1.5 / 16 = 0.09375
    f.rooms[1].window_area = 1.5; // 1.5 / 12 = 0.125, on the limit
    let daylight = kind(json!({"kind":"ratio","per":"element","cat":"room","room_kind":["living","bedroom"],"num":"window_area","den":"area","min":0.125}));
    let h = eval_kind(&daylight, &f, &pack, &[]);
    assert_eq!(h.len(), 1, "{h:?}");
    assert_eq!(h[0].elements, vec![("room".to_string(), 1)]);
    assert!((h[0].measured.unwrap() - 0.09375).abs() < 1e-12);
    // corridors and halls: 12 of 40 m2 = 0.3
    let circ = kind(json!({"kind":"ratio","per":"total","num":{"room_kind":["corridor","hall"]},"den":{},"max":0.18}));
    let h = eval_kind(&circ, &f, &pack, &[]);
    assert_eq!(h.len(), 1);
    assert!((h[0].measured.unwrap() - 0.3).abs() < 1e-12 && h[0].required == Some(0.18));
    assert_eq!(h[0].elements, vec![("room".to_string(), 3), ("room".to_string(), 4)]);
    // two dwellings: only the one over the limit is named
    f.rooms[0].unit = Some("A".into());
    f.rooms[1].unit = Some("B".into());
    f.rooms[2].unit = Some("B".into());
    f.rooms[3].unit = None; // common corridor: not part of a dwelling
    let by_unit = kind(json!({"kind":"ratio","per":"total","group_by":"unit","num":{"room_kind":["corridor","hall"]},"den":{},"max":0.18}));
    let h = eval_kind(&by_unit, &f, &pack, &[]);
    assert_eq!(h.len(), 1, "{h:?}");
    assert!((h[0].measured.unwrap() - 0.25).abs() < 1e-12, "4 of 16");
    // share of low rooms: 12 m2 below 2.5 of 40 = 0.3 passes 0.5; with 28 m2 low: 0.7 fails
    let low = kind(json!({"kind":"ratio","per":"total","num":{"where":[{"field":"height","op":"<","value":2.5}]},"den":{},"max":0.5}));
    f.rooms.iter_mut().for_each(|r| r.unit = None);
    f.rooms[1].height = 2.4;
    assert!(eval_kind(&low, &f, &pack, &[]).is_empty());
    f.rooms[0].height = 2.4;
    let h = eval_kind(&low, &f, &pack, &[]);
    assert!((h[0].measured.unwrap() - 0.7).abs() < 1e-12);
}

#[test]
fn adjacency_requires_and_forbids_links() {
    let pack = tiny_pack();
    let mut f = PlanFacts::default();
    f.rooms = vec![
        room(1, "kitchen", 0, 8.0, [0.0, 0.0, 3.0, 3.0]),
        room(2, "living", 0, 16.0, [3.0, 0.0, 7.0, 4.0]),
        room(3, "wc", 0, 2.0, [0.0, 3.0, 1.0, 5.0]),
        room(4, "kitchen", 0, 8.0, [7.0, 0.0, 10.0, 3.0]),
    ];
    f.doors = vec![door(10, (3.0, 1.0), &[0, 1]), door(11, (0.5, 3.0), &[2, 0]), door(12, (8.0, 0.0), &[3])];
    let k = kind(json!({"kind":"adjacency","require":[{"a":"kitchen","b":["living","hall"]}],"forbid":[["wc","kitchen"]]}));
    let h = eval_kind(&k, &f, &pack, &[]);
    assert_eq!(h.len(), 2, "{h:?}");
    assert_eq!((h[0].what, h[0].elements.clone()), ("adjacency_missing", vec![("room".to_string(), 4)]));
    assert_eq!(h[1].what, "adjacency_forbidden");
    assert_eq!(h[1].elements[0], ("opening".to_string(), 11));
    // with no living room or hall on the level, nothing is required
    f.rooms[1].kind = "office".into();
    let h = eval_kind(&k, &f, &pack, &[]);
    assert_eq!(h.iter().filter(|h| h.what == "adjacency_missing").count(), 0);
}

#[test]
fn stack_wants_wet_rooms_over_wet_rooms() {
    let pack = tiny_pack();
    let mut f = PlanFacts::default();
    f.rooms = vec![
        room(1, "bath", 0, 4.0, [0.0, 0.0, 2.0, 2.0]),
        room(2, "bath", 1, 4.0, [1.0, 1.0, 3.0, 3.0]), // overlaps 1 m2
        room(3, "bath", 1, 4.0, [5.0, 5.0, 7.0, 7.0]), // over nothing wet
        room(4, "living", 0, 4.0, [5.0, 5.0, 7.0, 7.0]),
        room(5, "wc", 2, 1.0, [1.5, 1.5, 2.5, 2.5]),   // over room 2
    ];
    let k = kind(json!({"kind":"stack","room_kind":["bath","wc"]}));
    let h = eval_kind(&k, &f, &pack, &[]);
    assert_eq!(h.len(), 1, "{h:?}");
    assert_eq!((h[0].what, h[0].elements[0].1), ("not_stacked", 3));
}

#[test]
fn distance_walks_through_doors_to_an_exit() {
    let pack = tiny_pack();
    let mut f = PlanFacts::default();
    // A (centroid 0,0) -door (5,0)- B (centroid 10,0) -outer door (15,0); C has no door
    f.rooms = vec![
        room(1, "living", 0, 1.0, [-1.0, -1.0, 1.0, 1.0]),
        room(2, "hall", 0, 1.0, [9.0, -1.0, 11.0, 1.0]),
        room(3, "storage", 0, 1.0, [20.0, 20.0, 22.0, 22.0]),
    ];
    f.doors = vec![door(10, (5.0, 0.0), &[0, 1]), door(11, (15.0, 0.0), &[1])];
    let d = travel_distances(&f, &[]);
    assert_eq!(d, vec![Some(15.0), Some(5.0), None], "5 to the door, 10 across B");
    let k = kind(json!({"kind":"distance","max":12}));
    let h = eval_kind(&k, &f, &pack, &[]);
    assert_eq!(h.len(), 2, "{h:?}");
    assert_eq!((h[0].what, h[0].elements[0].1, h[0].measured), ("max", 1, Some(15.0)));
    assert_eq!((h[1].what, h[1].elements[0].1), ("no_route", 3));
    // a stair room is a safe exit: from A through door (5,0) into the stair
    f.rooms[1].kind = "stair".into();
    f.doors.pop();
    assert_eq!(travel_distances(&f, &["stair".into()])[0], Some(5.0));
}

#[test]
fn programme_picks_the_variant_and_lists_gaps() {
    let ru = PackSet::embedded().resolve("ru").unwrap();
    let mut f = PlanFacts::default();
    // two living rooms (living + bedroom): variant 2k, area_min 44
    f.rooms = vec![
        room(1, "living", 0, 16.0, [0.0, 0.0, 4.0, 4.0]),
        room(2, "bedroom", 0, 7.0, [4.0, 0.0, 6.0, 3.5]),
        room(3, "kitchen", 0, 9.0, [0.0, 4.0, 3.0, 7.0]),
        room(4, "hall", 0, 4.0, [3.0, 4.0, 5.0, 6.0]),
        room(5, "office", 0, 3.0, [5.0, 4.0, 6.0, 7.0]),
    ];
    let k = kind(json!({"kind":"programme","typology":"apartment"}));
    let h = eval_kind(&k, &f, &ru, &[]);
    let what: Vec<(&str, bool)> = h.iter().map(|h| (h.what, h.verified)).collect();
    assert_eq!(
        what,
        [("area_min", true), ("missing_room", false), ("room_area", false), ("foreign_room", false)],
        "{h:?}"
    );
    assert_eq!((h[0].measured, h[0].required, h[0].detail.as_str()), (Some(39.0), Some(44.0), "2k"));
    assert_eq!(h[1].detail, "bath");
    assert_eq!((h[2].elements[0].1, h[2].measured, h[2].required), (2, Some(7.0), Some(8.0)));
    assert_eq!(h[3].elements[0].1, 5);
    // seven living rooms: no variant
    let mut g = PlanFacts::default();
    g.rooms = (0..7).map(|i| room(i, "bedroom", 0, 10.0, [0.0; 4])).collect();
    let h = eval_kind(&k, &g, &ru, &[]);
    assert_eq!((h.len(), h[0].what, h[0].measured), (1, "no_variant", Some(7.0)));
    // a house: storage over 5 % is advice
    let gen = PackSet::embedded().resolve("generic").unwrap();
    let mut hs = PlanFacts::default();
    hs.rooms = vec![
        room(1, "living", 0, 20.0, [0.0; 4]),
        room(2, "kitchen", 0, 10.0, [0.0; 4]),
        room(3, "bedroom", 0, 14.0, [0.0; 4]),
        room(4, "bath", 0, 4.0, [0.0; 4]),
        room(5, "hall", 0, 4.0, [0.0; 4]),
        room(6, "storage", 0, 8.0, [0.0; 4]),
    ];
    let h = eval_kind(&kind(json!({"kind":"programme","typology":"house"})), &hs, &gen, &[]);
    assert_eq!(h.len(), 1, "{h:?}");
    assert_eq!((h[0].what, h[0].verified), ("storage_share", false));
    assert!((h[0].measured.unwrap() - 8.0 / 60.0).abs() < 1e-12);
}

struct B {
    m: Model,
    level: i64,
    top: i64,
    wt: i64,
}

fn base() -> B {
    let mut m = Model::open(":memory:").unwrap();
    let level = m.add_level("L1", 0.0).unwrap();
    let top = m.add_level("L2", 3.0).unwrap();
    let wt = m
        .add_wall_type("W200", &[LayerSpec { material: "Brick".into(), thickness: 0.2 }])
        .unwrap();
    B { m, level, top, wt }
}

impl B {
    /// Four walls round (0,0)-(4,4), a room named `name` inside; returns the walls.
    fn box_room(&mut self, name: &str) -> Vec<i64> {
        let p = [(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)];
        let walls = (0..4)
            .map(|i| {
                self.m
                    .add_wall(&WallSpec {
                        a: p[i],
                        b: p[(i + 1) % 4],
                        level: self.level,
                        base_offset: 0.0,
                        height: 3.0,
                        wall_type: self.wt,
                        offset0: None,
                    })
                    .unwrap()
            })
            .collect();
        self.m
            .add_room(&RoomSpec {
                level: self.level,
                name: name.into(),
                number: "1".into(),
                point: (2.0, 2.0),
                height: 2.7,
            })
            .unwrap();
        walls
    }
}

#[test]
fn model_facts_and_unverified_rules_stay_advice() {
    let mut b = base();
    let walls = b.box_room(crate::test_ru::ru("room_kitchen"));
    let op = |kind: &str, along: f64, width: f64, sill: f64, height: f64| OpeningSpec {
        along,
        width,
        sill,
        height,
        kind: kind.into(),
    };
    b.m.add_opening(walls[0], &op("window", 1.0, 1.0, 0.9, 1.2)).unwrap();
    let door = b.m.add_opening(walls[1], &op("door", 1.0, 0.9, 0.0, 2.1)).unwrap();
    let mut spec = StairSpec::straight(b.level, b.top, (1.0, 2.0), 0.0);
    spec.max_riser = 0.22;
    spec.width = 0.9;
    let stair = b.m.add_stair(&spec).unwrap();
    let ru = PackSet::embedded().resolve("ru").unwrap();
    let f = PlanFacts::from_model(&b.m, &ru).unwrap();
    assert_eq!(f.rooms.len(), 1);
    let r = &f.rooms[0];
    assert_eq!((r.kind.as_str(), r.storey, r.window_area), ("kitchen", 0, 1.2));
    assert!((r.area - 3.8 * 3.8).abs() < 1e-6, "{}", r.area);
    assert!((r.centroid.0 - 2.0).abs() < 1e-9 && (r.centroid.1 - 2.0).abs() < 1e-9);
    assert_eq!(f.doors, vec![FactDoor { key: ("opening".into(), door), at: (4.0, 1.45), rooms: vec![0] }]);
    assert_eq!(f.stairs[0].key, ("stair".to_string(), stair));
    assert!(f.stairs[0].vals["riser"] > 0.0 && f.stairs[0].vals["tread"] > 0.0);

    let ctx = PackCtx { typology: Some("apartment".into()), ..PackCtx::default() };
    let rep = run_pack(&b.m, &ru, &CheckScope::All, &ctx).unwrap();
    let day = rep.findings.iter().find(|f| f.rule_id == "generic.room.daylight").unwrap();
    assert_eq!((day.severity, day.hard), (Severity::Info, false), "unverified: advice only");
    assert!((day.measured.unwrap() - 1.2 / 14.44).abs() < 1e-6);
    // 14.44 m2 with one living-type room is short of 28 m2: verified, so a hard error
    let area = rep
        .findings
        .iter()
        .find(|f| f.rule_id == "ru.apartment.area" && f.what == "area_min");
    assert!(area.is_none(), "a kitchen alone has no living room: no variant, not an area verdict");
    assert!(rep.findings.iter().any(|f| f.rule_id == "ru.apartment.area" && f.what == "no_variant" && !f.hard));
    assert!(rep.hard().all(|f| ru.rule(&f.rule_id).unwrap().verified));
    assert!(rep.skipped.iter().any(|(id, why)| id == "generic.circulation.office" && why == "typology"));
    // the room becomes a living room: 14.44 < 28 is a hard error citing table 5.1
    let rid = f.rooms[0].key.1;
    b.m.set_prop("room", rid, "RoomKind", Some((rusqlite::types::Value::Text("living".into()), ""))).unwrap();
    let rep = run_pack(&b.m, &ru, &CheckScope::All, &ctx).unwrap();
    let hard: Vec<&PackFinding> = rep.hard().collect();
    let a = hard.iter().find(|f| f.what == "area_min").unwrap();
    assert_eq!((a.severity, a.clause.as_str(), a.source.as_str()), (Severity::Error, "table 5.1", "SP 54.13330.2022"));
    assert_eq!((a.required, a.unit.as_str()), (Some(28.0), "m2"));
    assert_eq!(rep, run_pack(&b.m, &ru, &CheckScope::All, &ctx).unwrap(), "repeatable");
    // the level scope keeps room findings of that level only
    let other = run_pack(&b.m, &ru, &CheckScope::Level(b.top), &ctx).unwrap();
    assert!(other.findings.iter().all(|f| !f.elements.iter().any(|e| e.0 == "room")));
}

#[test]
fn locale_picks_a_pack_and_the_choice_is_stored() {
    assert_eq!(country_for_locale("ru", None), "ru");
    assert_eq!(country_for_locale("pt-BR", None), "br");
    assert_eq!(country_for_locale("es", Some("MX")), "mx");
    assert_eq!(country_for_locale("es", None), "es");
    assert_eq!(country_for_locale("en-GB", None), "uk");
    assert_eq!(country_for_locale("en", None), "us");
    assert_eq!(country_for_locale("uk", None), "ua");
    assert_eq!(country_for_locale("de_AT", None), "at");
    assert_eq!(country_for_locale("ru", Some("KZ")), "ru", "a single-market language keeps its country");
    let set = PackSet::embedded();
    assert_eq!(nearest_pack(&set, "ru-mow"), "ru");
    assert_eq!(nearest_pack(&set, "de-by"), "de");
    assert_eq!(nearest_pack(&set, "at"), "eu");
    assert_eq!(nearest_pack(&set, "mx"), "mx");
    assert_eq!(nearest_pack(&set, "ng"), "generic");
    assert_eq!(nearest_pack(&set, "ae-du"), "ae-du");
    assert_eq!(nearest_pack(&set, "ae-sh"), "ae");
    assert_eq!(nearest_pack(&set, "pt"), "eu");
    let tiny = PackSet::from_files(&files(&tiny()));
    assert_eq!(nearest_pack(&tiny, "at"), "eu");
    assert_eq!(nearest_pack(&tiny, "de-by"), "de-by");
    assert_eq!(nearest_pack(&tiny, "de-nw"), "de");

    let mut b = base();
    assert_eq!(b.m.effective_norm_pack(&set, "ru", None).unwrap(), "ru");
    assert_eq!(b.m.effective_norm_pack(&set, "fr", None).unwrap(), "fr");
    assert_eq!(b.m.effective_norm_pack(&set, "it", None).unwrap(), "it");
    assert_eq!(b.m.effective_norm_pack(&set, "tr", None).unwrap(), "tr");
    assert_eq!(b.m.effective_norm_pack(&set, "th", None).unwrap(), "generic");
    let top = b.m.op_top().unwrap();
    let p = NormLocale { pack: "ru".into(), region: "MOW".into(), climate_zone: "IIB".into(), typology: "apartment".into() };
    assert!(b.m.set_norm_locale(&p).unwrap());
    assert!(!b.m.set_norm_locale(&p).unwrap(), "same values are no operation");
    assert_eq!(b.m.op_top().unwrap(), top + 1);
    assert_eq!(b.m.norm_locale().unwrap(), p);
    assert_eq!(b.m.effective_norm_pack(&set, "fr", None).unwrap(), "ru", "the stored choice wins");
    assert_eq!(b.m.effective_norm_pack(&set, "en-GB", None).unwrap(), "ru");
    assert!(b.m.set_norm_locale(&NormLocale { pack: "RU!".into(), ..NormLocale::default() }).is_err());
    b.m.undo().unwrap();
    assert_eq!(b.m.norm_locale().unwrap(), NormLocale::default());
    assert_eq!(b.m.norm_project().unwrap(), super::NormProject::default(), "the fire inputs are separate");
}

/// A shipped country pack: parses, inherits along `chain`, keeps SI units and cites its sources.
fn country_pack(id: &str, chain: &[&str], country: &str, min_own: usize) -> NormPack {
    let set = PackSet::embedded();
    assert!(set.problems.is_empty(), "{:?}", set.problems);
    let p = set.resolve(id).unwrap();
    assert_eq!(p.chain, chain, "{id}: inheritance chain");
    assert_eq!(p.meta["country"].as_str().unwrap_or(""), country);
    assert_eq!(p.meta["units"]["length"].as_str(), Some("m"), "{id}: SI");
    let own: Vec<&PackRule> = p.rules.iter().filter(|r| r.pack == id).collect();
    assert!(own.len() >= min_own, "{id}: {} own rules", own.len());
    let kind_of = |r: &PackRule| r.raw["check"]["kind"].as_str().unwrap_or("").to_string();
    for k in ["formula", "ratio", "distance"] {
        assert!(p.rules.iter().any(|r| kind_of(r) == k), "{id}: a {k} rule");
    }
    assert!(p.programme("apartment").is_some_and(|g| g.pack == id), "{id}: own flat programme");
    assert!(p.programme("office").is_some_and(|g| g.pack == id), "{id}: own office programme");
    for r in &own {
        assert!(!r.clause.is_empty() && p.source(&r.source).is_some(), "{id}: {} cites document and clause", r.id);
        if !r.raw["value"].is_null() {
            assert_eq!(unit_problem(&r.unit, &r.raw["value"]), None, "{id}: {} unit", r.id);
        }
        if let Some(v) = r.value.filter(|_| r.unit == "m") {
            assert!(v > 0.0 && v < 100.0, "{id}: {} is metres, got {v}", r.id);
        }
        if !r.verified {
            assert_ne!(r.severity, Severity::Error, "{id}: {} unverified: advice only", r.id);
        }
    }
    for d in p.dims.iter().filter(|d| d.pack == id) {
        if !d.value.is_null() {
            assert_eq!(unit_problem(&d.unit, &d.value), None, "{id}: dim {}", d.key);
        }
        assert!(p.source(&d.source).is_some(), "{id}: dim {} source", d.key);
    }
    for s in p.sources.iter().filter(|s| s.pack == id) {
        assert!(LICENCES.contains(&s.licence.as_str()), "{id}: {} licence", s.id);
        if s.verified.is_some() {
            assert!(
                own.iter().any(|r| r.verified && r.source == s.id) || p.dims.iter().any(|d| d.verified && d.source == s.id),
                "{id}: source {} is dated checked but backs no verified number",
                s.id
            );
        }
    }
    // an inherited rule never runs twice: same check, typology and room kinds only once
    let mut seen = std::collections::BTreeSet::new();
    for r in &p.rules {
        let key = format!(
            "{}/{}/{}/{:?}/{:?}",
            kind_of(r),
            r.raw["check"]["cat"],
            r.raw["check"]["expr"],
            r.applies.typology,
            r.applies.room_kind
        );
        if matches!(kind_of(r).as_str(), "formula" | "distance") {
            assert!(seen.insert(key.clone()), "{id}: {} repeats {key}", r.id);
        }
    }
    p
}

#[test]
fn eu_pack_is_the_base_of_the_member_packs() {
    let p = country_pack("eu", &["generic", "eu"], "", 19);
    assert!(p.rule("generic.stair.step").is_none() && p.rule("eu.step").is_some());
    assert!(p.rules.iter().all(|r| !r.verified), "the envelope is advice only");
    assert!(p.rule("generic.programme.apartment").is_some(), "the shared programme rule stays");
}

#[test]
fn de_pack_inherits_eu_and_overrides_by_id() {
    let p = country_pack("de", &["generic", "eu", "de"], "DE", 14);
    assert!(p.rule("eu.res.h").is_none(), "replaced");
    let h = p.rule("de.res.h").unwrap();
    assert_eq!((h.verified, h.value, h.clause.as_str()), (true, Some(2.4), "par. 47(1) clear height of habitable rooms"));
    assert!(p.rule("eu.res.corr").is_some(), "inherited where de says nothing");
    assert_eq!(p.dim("room.height_min").unwrap().pack, "de");
}

#[test]
fn fr_pack_inherits_eu() {
    let p = country_pack("fr", &["generic", "eu", "fr"], "FR", 14);
    assert_eq!(p.rule("fr.res.h").unwrap().value, Some(2.2));
    assert!(p.rule("fr.res.h").unwrap().verified);
    assert_eq!(p.dim("dwelling.main_room_area_min").unwrap().value, serde_json::json!(9.0));
    assert!(p.rule("eu.egr.res").is_some() && p.rule("eu.egr.off").is_none());
}

#[test]
fn es_pack_has_the_verified_stair_numbers() {
    let p = country_pack("es", &["generic", "eu", "es"], "ES", 16);
    for (id, v) in [("es.res.riser", 0.185), ("es.res.tread", 0.28), ("es.off.tread", 0.28)] {
        let r = p.rule(id).unwrap();
        assert!(r.verified && r.value == Some(v), "{id}");
    }
    assert_eq!(p.rule("es.step").unwrap().raw["value"], serde_json::json!([0.54, 0.7]));
    let egr = p.rule("es.egr.res").unwrap();
    assert!(egr.verified && egr.value == Some(35.0), "second verification pass: 35 m");
}

#[test]
fn pl_pack_splits_stair_width_by_typology() {
    let p = country_pack("pl", &["generic", "eu", "pl"], "PL", 18);
    assert_eq!(p.rule("pl.res.sw").unwrap().applies.typology, ["apartment", "apartment_section"]);
    assert_eq!(p.rule("pl.stair.sw_house").unwrap().applies.typology, ["house"]);
    let sw = p.rule("pl.res.sw").unwrap();
    assert!(sw.verified && sw.value == Some(1.2), "pl.res.sw verified against the 2022 text");
    assert!(!p.rule("pl.liv.a").is_some_and(|r| r.verified), "absent from the 2022 text");
}

#[test]
fn uk_pack_follows_the_space_standard() {
    let p = country_pack("uk", &["generic", "uk"], "GB", 12);
    let low = p.rule("uk.low.share").unwrap();
    assert!(low.verified && low.value == Some(0.25));
    assert_eq!(low.raw["check"]["num"]["where"][0]["value"], serde_json::json!(2.3));
    assert!(p.rule("generic.egress.distance").is_none() && p.rule("uk.egr.off").is_some());
    let g = p.programme("apartment").unwrap();
    let areas: Vec<Option<f64>> = g.variants.iter().map(|v| v.area_min).collect();
    assert_eq!(areas, [Some(50.0), Some(70.0), Some(74.0)]);
    assert_eq!(p.dim("building.second_stair_height").unwrap().value, serde_json::json!(18.0));
}

#[test]
fn us_pack_is_si_after_conversion() {
    let p = country_pack("us", &["generic", "us"], "US", 19);
    let near = |id: &str, v: f64| {
        let x = p.rule(id).unwrap().value.unwrap();
        assert!((x - v).abs() < 0.002, "{id}: {x} vs {v}");
    };
    near("us.res.riser", 0.19685);
    near("us.res.tread", 0.254);
    near("us.off.riser", 0.1778);
    near("us.res.h", 2.1336);
    near("us.egr.off", 91.44);
    near("us.res.corr", 0.9144);
    // adopted editions differ: only the accessible-route width is verified
    let verified: Vec<&str> = p.rules.iter().filter(|r| r.pack == "us" && r.verified).map(|r| r.id.as_str()).collect();
    assert_eq!(verified, ["us.acc.corr"]);
    assert_eq!(p.meta["units"]["display"].as_str(), Some("imperial"));
}

#[test]
fn ua_and_kz_packs_are_standalone_hints() {
    for (id, c, h) in [("ua", "UA", 2.5), ("kz", "KZ", 2.7)] {
        let p = country_pack(id, &["generic", id], c, 19);
        assert!(p.rules.iter().all(|r| !r.verified), "{id}: no number is checked yet");
        assert_eq!(p.rule(&format!("{id}.res.h")).unwrap().value, Some(h));
        assert!(p.rule("generic.stair.step").is_none());
    }
    let kz = PackSet::embedded().resolve("kz").unwrap();
    assert_eq!(kz.kind_of_name(crate::test_ru::ru("room_bedroom")), Some("bedroom"));
    assert_eq!(kz.kind_of_name(crate::test_ru::ru("room_bedroom_kk")), Some("bedroom"));
    let (general, ct) = (kz.dim("corridor.width_min").unwrap(), kz.dim("corridor.width_min.corridor_type").unwrap());
    assert_eq!((general.value.as_f64(), ct.value.as_f64()), (Some(1.2), Some(1.4)));
    assert!(ct.verified && ct.pack == "kz");
}

/// A pack without programmes: parses, inherits `generic`, cites a dated source per rule.
fn wave_pack(id: &str, country: &str, lang: &str, min_own: usize) -> NormPack {
    let set = PackSet::embedded();
    assert!(set.problems.is_empty(), "{:?}", set.problems);
    let p = set.resolve(id).unwrap();
    assert_eq!(p.chain, ["generic", id]);
    assert_eq!(p.meta["country"].as_str(), Some(country));
    assert_eq!(p.meta["lang"][0].as_str(), Some(lang));
    assert_eq!(p.meta["units"]["length"].as_str(), Some("m"));
    let own: Vec<&PackRule> = p.rules.iter().filter(|r| r.pack == id).collect();
    assert!(own.len() >= min_own, "{id}: {} own rules", own.len());
    for r in &own {
        assert!(!r.clause.is_empty() && p.source(&r.source).is_some(), "{id}: {} cites document and clause", r.id);
        // the pack's research date, or a later one that added the rule
        assert!(r.raw["source"]["accessed"].as_str().is_some_and(|d| d.len() == 10 && d >= "2026-11-13"), "{id}: {} access date", r.id);
        assert!(r.raw["source"]["url"].as_str().is_some_and(|u| u.starts_with("https://")), "{id}: {} url", r.id);
        assert_eq!(unit_problem(&r.unit, &r.raw["value"]), None, "{id}: {} unit", r.id);
        assert!(r.verified || r.raw["source"]["note"].as_str().is_some_and(|n| !n.is_empty()), "{id}: {} note", r.id);
        if !r.verified {
            assert_ne!(r.severity, Severity::Error, "{id}: {} unverified: advice only", r.id);
        }
    }
    for s in p.sources.iter().filter(|s| s.pack == id && s.verified.is_some()) {
        assert!(own.iter().any(|r| r.verified && r.source == s.id) || p.dims.iter().any(|d| d.verified && d.source == s.id), "{id}: {}", s.id);
    }
    p
}

#[test]
fn br_pack_follows_the_goias_emergency_exit_norm() {
    let p = wave_pack("br", "BR", "pt", 6);
    let r = p.rule("br.egr.riser").unwrap();
    assert!(r.verified && r.value == Some(0.18) && r.clause.contains("5.7.3.1"));
    assert!(p.rule("generic.stair.step").is_none() && p.rule("br.egr.step").is_some());
    assert_eq!(p.rule("br.egr.off").unwrap().value, Some(35.0));
    let d = p.dim("ramp.slope_max").unwrap();
    assert!(d.verified && d.value == serde_json::json!(0.0833), "read in the 2020 text");
    assert_eq!(p.dim("ramp.slope_max.egress").unwrap().value, serde_json::json!(0.10));
}

#[test]
fn cn_pack_is_verified_against_the_standard_text_except_the_door() {
    let p = wave_pack("cn", "CN", "zh", 4);
    let own: Vec<&str> = p.rules.iter().filter(|r| r.pack == "cn").map(|r| r.id.as_str()).collect();
    assert_eq!(
        own,
        ["cn.pub.sw", "cn.pub.corr", "cn.corr.h", "cn.stair.headroom", "cn.res.riser", "cn.res.tread", "cn.res.sw", "cn.res.h", "cn.day"]
    );
    assert!(p.rules.iter().filter(|r| r.pack == "cn").all(|r| r.verified), "read in the 2022 clause text");
    assert!(!p.dim("door.clear_min").unwrap().verified, "the excerpt read has no door clauses");
    assert_eq!(p.rule("cn.pub.corr").unwrap().value, Some(1.3));
    assert_eq!(p.rule("cn.stair.headroom").unwrap().value, Some(2.2));
    assert!(p.rule("generic.stair.headroom").is_none());
    assert_eq!(p.dim("stair.flight_risers_max").unwrap().value, serde_json::json!(18));
}

#[test]
fn in_pack_follows_nbcs_part_f() {
    let p = wave_pack("in", "IN", "hi", 10);
    let r = p.rule("in.apt.sw").unwrap();
    assert!(r.verified && r.value == Some(1.25) && r.applies.typology == ["apartment", "apartment_section"]);
    assert_eq!(p.rule("in.house.riser").unwrap().value, Some(0.175));
    assert_eq!(p.rule("in.off.tread").unwrap().value, Some(0.3));
    assert_eq!(p.rule("in.egr.res").unwrap().value, Some(30.0));
    assert!(p.rule("generic.egress.distance").is_none());
    assert_eq!(p.dim("door.exit_clear_min").unwrap().value, serde_json::json!(1.0));
}

#[test]
fn code_check_doors_guards_headroom_with_fixes() {
    let mut b = base();
    let walls = b.box_room("Room");
    let op = |width: f64| OpeningSpec { along: 1.0, width, sill: 0.0, height: 2.1, kind: "door".into() };
    let narrow = b.m.add_opening(walls[1], &op(0.7)).unwrap();
    b.m.add_opening(walls[2], &op(0.9)).unwrap();
    let stair = b.m.add_stair(&StairSpec::straight(b.level, b.top, (1.0, 1.0), 0.0)).unwrap();
    let mut rs = crate::RailingSpec::new(b.level, crate::RailingHost::Stair { stair, side: 0 });
    rs.height = 0.8;
    let rail = b.m.add_railing(&rs).unwrap();
    let gen = PackSet::embedded().resolve("generic").unwrap();
    let rep = run_pack(&b.m, &gen, &CheckScope::All, &PackCtx::default()).unwrap();
    let of = |id: &str| rep.findings.iter().filter(|f| f.rule_id == id).collect::<Vec<_>>();
    let d = of("generic.door.width");
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].elements, vec![("opening".to_string(), narrow)]);
    assert_eq!(d[0].fix, Some(("width".to_string(), 0.8)));
    let g = of("generic.guard.height");
    assert_eq!((g.len(), &g[0].elements[0]), (1, &("railing".to_string(), rail)));
    assert_eq!(g[0].fix, Some(("height".to_string(), 0.9)));
    assert!(of("generic.stair.headroom").is_empty(), "no slab over the stair: not judged");

    // a slab of the upper level over the whole stair, no hole: the head hits it
    let st = b.m.add_slab_type("S200", &[LayerSpec { material: "Concrete".into(), thickness: 0.2 }]).unwrap();
    let ring = vec![(-1.0, -1.0), (9.0, -1.0), (9.0, 9.0), (-1.0, 9.0)];
    let slab = b.m.add_slab(&crate::SlabSpec { level: b.top, slab_type: st, offset: 0.0, outer: ring.clone(), holes: vec![] }).unwrap();
    let f = PlanFacts::from_model(&b.m, &gen).unwrap();
    let h = f.stairs[0].vals["headroom"];
    assert!(h > 0.0 && h < 2.0, "{h}");
    let rep = run_pack(&b.m, &gen, &CheckScope::All, &PackCtx::default()).unwrap();
    let hr: Vec<_> = rep.findings.iter().filter(|f| f.rule_id == "generic.stair.headroom").collect();
    assert_eq!((hr.len(), hr[0].what.as_str(), hr[0].fix.clone()), (1, "min", None), "headroom is not a field to set");
    // a stairwell hole over the flight: nothing over the treads, no headroom
    b.m.add_slab_opening(slab, &[(0.0, 0.0), (8.0, 0.0), (8.0, 3.0), (0.0, 3.0)]).unwrap();
    let f = PlanFacts::from_model(&b.m, &gen).unwrap();
    assert!(!f.stairs[0].vals.contains_key("headroom"), "{:?}", f.stairs[0].vals);
    assert!(parse_kind("formula", &json!({"cat":"door","expr":"depth","min":1})).is_err());
}

/// Packs of the largest construction markets and UI locales.
const WORLD: [(&str, &str); 16] = [
    ("jp", "generic"), ("kr", "generic"), ("au", "generic"), ("ca", "generic"), ("it", "eu"), ("nl", "eu"),
    ("se", "eu"), ("tr", "generic"), ("ae", "generic"), ("sa", "generic"), ("mx", "generic"), ("ar", "generic"),
    ("za", "generic"), ("id", "generic"), ("vn", "generic"), ("cz", "eu"),
];

#[test]
fn world_packs_cover_the_rule_set_and_say_where_each_number_came_from() {
    let set = PackSet::embedded();
    assert!(set.problems.is_empty(), "{:?}", set.problems);
    for (id, parent) in WORLD {
        let p = set.resolve(id).unwrap_or_else(|e| panic!("{id}: {e}"));
        assert_eq!(p.chain[p.chain.len() - 2], parent, "{id}");
        assert_eq!(p.meta["country"].as_str(), Some(id.to_uppercase().as_str()));
        assert!(p.meta["conventions"]["ground_floor"].is_i64() && p.meta["conventions"]["area_standard"].is_string(), "{id}: own conventions");
        let own: Vec<&DimItem> = p.dims.iter().filter(|d| d.pack == id).collect();
        assert!(own.len() >= 5, "{id}: {} own numbers", own.len());
        for d in &own {
            assert!(!d.clause.is_empty() && p.source(&d.source).is_some(), "{id}: {} cites document and clause", d.key);
            if !d.value.is_null() {
                assert_eq!(unit_problem(&d.unit, &d.value), None, "{id}: {}", d.key);
            }
        }
        // every number either read in its clause or with a note of where we looked
        let file = EMBEDDED.iter().find(|(f, _)| *f == format!("{id}/dims.json")).unwrap().1;
        let items: serde_json::Value = serde_json::from_str(file).unwrap();
        for it in items["items"].as_array().unwrap() {
            let k = it["key"].as_str().unwrap();
            assert!(it["url"].as_str().is_some_and(|u| u.starts_with("http")), "{id}: {k} url");
            // accessed on or after the pack's research date
            assert!(it["accessed"].as_str().is_some_and(|a| a.len() == 10 && a >= "2026-11-14"), "{id}: {k}");
            if it["verified"] != json!(true) {
                assert!(it["note"].as_str().is_some_and(|n| !n.trim().is_empty()), "{id}: unverified {k} says where we looked");
            }
        }
        // the shared rule set resolves, own or inherited
        for k in ["egress.travel_max", "daylight.window_floor_ratio", "corridor.width_min.accessible", "door.clear_min.accessible",
                  "accessible.turning_circle", "fire.compartment_area_max", "stair.width_min", "room.height_min", "parking.per_dwelling"] {
            assert!(p.dim(k).is_some(), "{id}: {k}");
        }
        for r in p.rules.iter().filter(|r| r.pack == id) {
            assert!(p.source(&r.source).is_some() && !r.clause.is_empty(), "{id}: {}", r.id);
            if !r.verified {
                assert_ne!(r.severity, Severity::Error, "{id}: {} unverified: advice only", r.id);
            }
        }
        assert!(p.rules.iter().any(|r| r.pack == id), "{id}: own rules");
    }
    let du = set.resolve("ae-du").unwrap();
    assert_eq!(du.chain, ["generic", "ae", "ae-du"]);
    let h = du.dim("room.height_min").unwrap();
    assert_eq!((h.pack.as_str(), h.verified), ("ae-du", true), "the city overlay sets its own numbers");
    assert_eq!(du.rule("ae.res.h").unwrap().pack, "ae-du", "overlay rules use the country's ids");
}

#[test]
fn every_shipped_pack_validates_and_inherits_from_generic() {
    let set = PackSet::embedded();
    assert!(set.problems.is_empty(), "{:?}", set.problems);
    for id in set.ids() {
        let p = set.resolve(id).unwrap_or_else(|e| panic!("{id}: {e}"));
        assert_eq!(p.chain.first().map(String::as_str), Some("generic"), "{id}: {:?}", p.chain);
        assert_eq!(p.chain.last().map(String::as_str), Some(id));
        if let Some((country, _)) = id.split_once('-') {
            assert_eq!(p.chain[p.chain.len() - 2], country, "{id}: an overlay extends its country");
        }
        let c = p.conventions();
        assert!(matches!(c.ground_floor, 0 | 1), "{id}");
        assert!(AREA_STANDARDS.contains(&c.area_standard.as_str()), "{id}");
        assert!(c.net_share > 0.5 && c.net_share <= 1.0, "{id}");
        assert!(DISPLAY_SYSTEMS.contains(&p.display_system(None)), "{id}");
        for d in p.dims.iter().filter(|d| d.verified) {
            assert!(!d.clause.trim().is_empty(), "{id}: verified {} names its clause", d.key);
        }
    }
}

#[test]
fn the_json_schema_lists_what_the_checker_accepts() {
    let s: serde_json::Value = serde_json::from_str(include_str!("../../data/norms/pack.schema.json")).unwrap();
    let d = &s["$defs"];
    let strs = |v: &serde_json::Value| -> Vec<String> {
        let mut out: Vec<String> = v.as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect();
        out.sort();
        out
    };
    let sorted = |a: &[&str]| -> Vec<String> {
        let mut out: Vec<String> = a.iter().map(|x| x.to_string()).collect();
        out.sort();
        out
    };
    assert_eq!(strs(&d["source"]["properties"]["licence"]["enum"]), sorted(&LICENCES));
    assert_eq!(strs(&d["pack"]["properties"]["status"]["enum"]), sorted(&STATUSES));
    assert_eq!(strs(&d["pack"]["properties"]["conventions"]["properties"]["area_standard"]["enum"]), sorted(&AREA_STANDARDS));
    assert_eq!(strs(&d["pack"]["properties"]["units"]["properties"]["display"]["enum"]), sorted(&DISPLAY_SYSTEMS));
    let mut applies: Vec<String> = d["applies"]["properties"].as_object().unwrap().keys().cloned().collect();
    applies.sort();
    assert_eq!(applies, sorted(&APPLIES_KEYS));
    let kinds: Vec<&str> = LEGACY_KINDS.iter().chain(NEW_KINDS.iter()).copied().collect();
    assert_eq!(strs(&d["rule"]["oneOf"][1]["properties"]["check"]["properties"]["kind"]["enum"]), sorted(&kinds));
    for u in strs(&d["unit"]["enum"]) {
        assert_eq!(unit_problem(&u, &json!(0.5)), None, "unit {u:?}");
    }
    for u in strs(&d["origUnit"]["enum"]) {
        assert!(si_of(&u).is_some(), "orig unit {u:?}");
    }
    assert_eq!(d["paraphrase"]["maxLength"].as_u64(), Some(PARAPHRASE_MAX as u64));
}

#[test]
fn source_units_convert_to_si_and_show_in_the_chosen_system() {
    assert_eq!(si_of("in"), Some(("m", 0.0254)));
    assert_eq!(si_of("furlong"), None);
    let (v, u) = display_value(0.1778, "m", "imperial");
    assert!((v - 7.0).abs() < 1e-9 && u == "in", "{v} {u}");
    let (v, u) = display_value(91.44, "m", "imperial");
    assert!((v - 300.0).abs() < 1e-9 && u == "ft", "{v} {u}");
    let (v, u) = display_value(2.4, "kN/m2", "imperial");
    assert!((v - 50.125).abs() < 0.01 && u == "psf", "{v} {u}");
    assert_eq!(display_value(2.4, "m", "metric"), (2.4, "m"));
    let us = PackSet::embedded().resolve("us").unwrap();
    assert_eq!(us.display_system(None), "imperial");
    assert_eq!(us.display_system(Some("metric")), "metric");
    assert_eq!(us.display_system(Some("cubits")), "imperial");
    assert_eq!(PackSet::embedded().resolve("de").unwrap().display_system(None), "metric");
    // a number with its source unit: the source unit must convert to the stored SI value
    let item = |v: f64, orig: serde_json::Value| {
        json!({"items": [{"key": "stair.riser_max", "value": v, "unit": "m", "orig": orig, "source": "mbo", "clause": "1", "verified": true}]})
    };
    assert!(with("de/dims.json", item(0.1778, json!({"value": 7, "unit": "in"}))).resolve("de").is_ok());
    let bad = with("de/dims.json", item(0.2, json!({"value": 7, "unit": "in"})));
    assert!(err_of(&bad, "de").contains("orig: 7 in"), "{}", err_of(&bad, "de"));
    let bad = with("de/dims.json", item(0.2, json!({"value": 7, "unit": "ft2"})));
    assert!(err_of(&bad, "de").contains("not a m unit"), "{}", err_of(&bad, "de"));
}

#[test]
fn conventions_number_storeys_and_read_stated_areas() {
    let set = PackSet::embedded();
    let us = set.resolve("us").unwrap();
    let uk = set.resolve("uk").unwrap();
    assert_eq!((us.storey_number(0), us.storey_number(2)), (1, 3));
    assert_eq!((uk.storey_number(0), uk.storey_number(2)), (0, 2));
    assert_eq!(set.resolve("ru").unwrap().storey_number(0), 1);
    assert_eq!(set.resolve("pl").unwrap().conventions().area_standard, "usable");
    assert!((us.area_as_net(100.0) - 85.0).abs() < 1e-9);
    assert_eq!(set.resolve("generic").unwrap().area_as_net(70.0), 70.0);
    for (c, msg) in [
        (json!({"ground_floor": 2}), "ground_floor"),
        (json!({"area_standard": "carpet"}), "area_standard"),
        (json!({"area_standard": "net", "net_share": 0.9}), "net_share 1"),
        (json!({"net_share": 1.2}), "net_share"),
    ] {
        let mut m = meta("de", Some("eu"));
        m["conventions"] = c;
        let set = with("de/pack.json", m);
        assert!(err_of(&set, "de").contains(msg), "{msg}: {}", err_of(&set, "de"));
    }
    // a stated gross area asks less of the rooms than the same number stated net
    let mut ru = set.resolve("ru").unwrap();
    let mut f = PlanFacts::default();
    f.rooms = vec![
        room(1, "living", 0, 16.0, [0.0, 0.0, 4.0, 4.0]),
        room(2, "bedroom", 0, 7.0, [4.0, 0.0, 6.0, 3.5]),
        room(3, "kitchen", 0, 9.0, [0.0, 4.0, 3.0, 7.0]),
        room(4, "hall", 0, 7.0, [3.0, 4.0, 5.0, 6.0]),
    ];
    let k = kind(json!({"kind":"programme","typology":"apartment"}));
    let short = eval_kind(&k, &f, &ru, &[]);
    let a = short.iter().find(|h| h.what == "area_min").unwrap();
    assert_eq!((a.measured, a.required), (Some(39.0), Some(44.0)));
    ru.meta["conventions"] = json!({"area_standard": "gross", "net_share": 0.8});
    assert!(!eval_kind(&k, &f, &ru, &[]).iter().any(|h| h.what == "area_min"), "39 m2 of rooms is 48.75 m2 gross");
}

fn applies_rule(id: &str, a: serde_json::Value) -> serde_json::Value {
    let mut x = rule(id, "mbo", json!({"kind":"distance","max":60}), "m");
    x["applies"] = a;
    x
}

#[test]
fn rules_apply_by_sprinklers_and_height_class() {
    let rules = json!({"rules": [
        applies_rule("de.sprk", json!({"sprinklered": true})),
        applies_rule("de.plain", json!({"sprinklered": false})),
        applies_rule("de.tall", json!({"height_min": 22.0})),
        applies_rule("de.low", json!({"height_max": 22.0}))
    ]});
    let set = with("de/rules/a.json", rules);
    let p = set.resolve("de").unwrap();
    assert_eq!(p.rule("de.tall").unwrap().applies.height_min, Some(22.0));
    let b = base();
    let skipped = |ctx: &PackCtx| -> Vec<(String, String)> {
        let rep = run_pack(&b.m, &p, &CheckScope::All, ctx).unwrap();
        rep.skipped.into_iter().filter(|(id, _)| id.starts_with("de.")).collect()
    };
    let s = |a: &str, b: &str| (a.to_string(), b.to_string());
    assert_eq!(skipped(&PackCtx::default()), [s("de.sprk", "sprinklers")], "unknown: the plain limit holds");
    let ctx = PackCtx { sprinklered: Some(true), height: Some(30.0), ..PackCtx::default() };
    assert_eq!(skipped(&ctx), [s("de.plain", "sprinklers"), s("de.low", "height")]);
    let ctx = PackCtx { sprinklered: Some(false), height: Some(10.0), ..PackCtx::default() };
    assert_eq!(skipped(&ctx), [s("de.sprk", "sprinklers"), s("de.tall", "height")]);
    for (a, msg) in [
        (json!({"sprinkler": true}), "unknown applies key"),
        (json!({"sprinklered": "yes"}), "wrong type"),
        (json!({"height_max": -3}), "wrong type"),
    ] {
        let set = with("de/rules/a.json", json!({"rules": [applies_rule("de.x", a)]}));
        assert!(err_of(&set, "de").contains(msg), "{msg}: {}", err_of(&set, "de"));
    }
}

#[test]
fn packs_hold_numbers_and_paraphrases_never_quoted_text() {
    let dim = |extra: serde_json::Value| {
        let mut d = json!({"key": "room.height_min", "value": 2.4, "unit": "m", "source": "mbo", "clause": "1", "verified": true});
        for (k, v) in extra.as_object().unwrap() {
            d[k] = v.clone();
        }
        json!({"items": [d]})
    };
    assert!(with("de/dims.json", dim(json!({"paraphrase": "Habitable rooms need at least this clear height."}))).resolve("de").is_ok());
    for (extra, msg) in [
        (json!({"quote": "copied clause text"}), "`quote` is not allowed"),
        (json!({"note": {"excerpt": "copied"}}), "`excerpt` is not allowed"),
        (json!({"paraphrase": "x".repeat(PARAPHRASE_MAX + 1)}), "paraphrase"),
        (json!({"paraphrase": "two\nlines"}), "paraphrase"),
        (json!({"clause": ""}), "names its clause"),
    ] {
        let set = with("de/dims.json", dim(extra));
        assert!(err_of(&set, "de").contains(msg), "{msg}: {}", err_of(&set, "de"));
    }
    let mut q = rule("de.q", "mbo", json!({"kind":"distance","max":30}), "m");
    q["source"]["quote"] = json!("copied");
    assert!(err_of(&with("de/rules/a.json", json!({"rules": [q]})), "de").contains("`quote` is not allowed"));
    let set = with("de/pack.json", meta("de", None));
    assert!(err_of(&set, "de").contains("only `generic` has no parent"));
}

#[test]
fn residential_corridor_and_regional_overlay_numbers() {
    let set = PackSet::embedded();
    let res = |d: &DimItem| d.applies.typology.iter().any(|t| t == "apartment");
    // the generator sizes the common corridor of an apartment block, not a generic escape path
    for (id, want) in [("ru", 1.4), ("ua", 1.8)] {
        let p = set.resolve(id).unwrap();
        let d = p.dim("corridor.width_min").unwrap();
        assert_eq!((d.pack.as_str(), d.value.as_f64(), d.verified), (id, Some(want), true), "{id}");
        assert!(res(d), "{id}: residential common corridor");
    }
    let ru = set.resolve("ru").unwrap();
    assert_eq!(ru.dim("room.height_min").unwrap().pack, "ru", "clear room height overrides the generic default");
    let es = set.resolve("es").unwrap();
    let ct = set.resolve("es-ct").unwrap();
    assert_eq!(ct.chain, ["generic", "eu", "es", "es-ct"]);
    assert_eq!(nearest_pack(&set, "es-ct"), "es-ct");
    let (n, r) = (es.dim("daylight.window_floor_ratio").unwrap(), ct.dim("daylight.window_floor_ratio").unwrap());
    assert_eq!((n.pack.as_str(), n.source.as_str()), ("es", "es-orden-1944"), "national rule in the national pack");
    assert_eq!((r.pack.as_str(), r.value.as_f64()), ("es-ct", Some(0.125)), "Catalan rule only in its overlay");
    assert!(ct.source(&r.source).is_some() && es.source(&r.source).is_none());
}

#[test]
fn the_main_packs_carry_design_climate_and_outdoor_air_with_their_documents() {
    let set = PackSet::embedded();
    for id in ["generic", "ru", "de", "us", "uk", "ae"] {
        let p = set.resolve(id).unwrap();
        for (k, unit, lo, hi) in [
            ("climate.heating_design_temp", "C", -50.0, 15.0),
            ("climate.cooling_design_temp", "C", 15.0, 50.0),
            ("ventilation.oa_per_person", "m3/h", 5.0, 80.0),
            ("ventilation.oa_per_area", "m3/h m2", 0.0, 5.0),
        ] {
            let d = p.dim(k).unwrap_or_else(|| panic!("{id}: {k}"));
            assert_eq!((d.pack.as_str(), d.unit.as_str()), (id, unit), "{id}: {k} is its own");
            let v = d.value.as_f64().unwrap();
            assert!((lo..=hi).contains(&v), "{id}: {k} = {v}");
            assert!(p.source(&d.source).is_some() && !d.clause.is_empty(), "{id}: {k} cites document and clause");
            assert!(p.dim_cite(k).unwrap().1.contains(&d.clause), "{id}: {k}");
        }
    }
    let uk = set.resolve("uk").unwrap();
    assert!(uk.dim("ventilation.oa_per_person").unwrap().verified, "read in its clause");
    assert_eq!(display_value(36.0, "m3/h", "imperial").1, "cfm");
    assert_eq!(display_value(-40.0, "C", "imperial"), (-40.0, "F"));
}

/// Countries that state every key of the reference pack `de`: a number, a no-limit entry,
/// or `"na": true` with the reason.
const FULL_PACKS: [&str; 5] = ["us", "uk", "fr", "es", "za"];

fn check_signature(r: &PackRule) -> String {
    let c = &r.raw["check"];
    match c["kind"].as_str().unwrap_or("") {
        "ratio" => format!("ratio/{}/{}", c["num"], c["den"]),
        "distance" => "distance".to_string(),
        k => format!("{k}/{}/{}", c["cat"], c["expr"]),
    }
}

#[test]
fn full_packs_state_every_key_of_the_reference_pack() {
    let set = PackSet::embedded();
    assert!(set.problems.is_empty(), "{:?}", set.problems);
    let de = set.resolve("de").unwrap();
    let keys: Vec<&DimItem> = de.dims.iter().filter(|d| d.pack == "de").collect();
    let checks: Vec<&PackRule> = de.rules.iter().filter(|r| r.pack == "de").collect();
    assert!(keys.len() >= 14 && checks.len() >= 16, "reference pack: {} keys, {} rules", keys.len(), checks.len());
    for k in ["stair.riser_max", "stair.going_min", "guard.height_min"] {
        assert!(de.dim(k).is_some_and(|d| d.pack == "de"), "reference pack states {k}");
    }
    let mut gaps = vec![];
    for id in FULL_PACKS {
        let p = set.resolve(id).unwrap_or_else(|e| panic!("{id}: {e}"));
        let own = |pack: &str| pack == id || pack.starts_with(&format!("{id}-"));
        for d in &keys {
            let stated = p.dim(&d.key).is_some_and(|x| own(&x.pack));
            let na = p.na.get(&d.key).is_some_and(|why| !why.trim().is_empty());
            if !stated && !na {
                gaps.push(format!("{id}: dim {}", d.key));
            }
            if let Some(x) = p.dim(&d.key).filter(|x| own(&x.pack)) {
                assert!(p.source(&x.source).is_some() && !x.clause.is_empty(), "{id}: {} cites document and clause", d.key);
            }
        }
        for r in &checks {
            let suffix = r.id.trim_start_matches("de.");
            let overlap = |t: &[String]| t.is_empty() || r.applies.typology.is_empty() || t.iter().any(|x| r.applies.typology.contains(x));
            let covered = p
                .rules
                .iter()
                .any(|o| own(&o.pack) && check_signature(o) == check_signature(r) && overlap(&o.applies.typology));
            let na = p.na.get(&format!("{id}.{suffix}")).is_some_and(|why| !why.trim().is_empty());
            if !covered && !na {
                gaps.push(format!("{id}: rule like {}", r.id));
            }
        }
    }
    assert!(gaps.is_empty(), "{} gaps:\n{}", gaps.len(), gaps.join("\n"));
}

#[test]
fn not_applicable_keys_drop_the_parent_value_and_say_why() {
    let set = with("de/dims.json", json!({"items": [
        {"key": "room.height_min", "na": true, "value": null, "unit": "m", "source": "mbo", "verified": false, "note": "no such limit here"}
    ]}));
    assert!(set.problems.is_empty(), "{:?}", set.problems);
    let p = set.resolve("de").unwrap();
    assert!(p.dim("room.height_min").is_none(), "the parent number does not stand in");
    assert_eq!(p.na.get("room.height_min").map(String::as_str), Some("no such limit here"));
    let bad = with("de/dims.json", json!({"items": [
        {"key": "room.height_min", "na": true, "value": 2.4, "unit": "m", "source": "mbo", "verified": false}
    ]}));
    let e = format!("{:?}", bad.problems);
    assert!(e.contains("null value") && e.contains("note"), "{e}");
    let r = with("de/rules/x.json", json!({"rules": [{"id": "de.res.h", "disabled": true, "na": true}]}));
    assert!(format!("{:?}", r.problems).contains("note"), "an n/a rule says why");
}
