// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::norms::*;
use super::{CheckScope, Cond, NormProject, Severity};
use crate::{LayerSpec, Model, OpeningSpec, PVal, ParamSet, RoomSpec, StairSpec, WallSpec};
use rusqlite::types::Value;

struct B {
    m: Model,
    level: i64,
    top: i64,
    wall: i64,
    wt: i64,
}

fn base() -> B {
    let mut m = Model::open(":memory:").unwrap();
    let level = m.add_level("L1", 0.0).unwrap();
    let top = m.add_level("L2", 3.0).unwrap();
    let layer = [LayerSpec {
        material: "Brick".into(),
        thickness: 0.2,
    }];
    let wt = m.add_wall_type("W200", &layer).unwrap();
    let wall = m
        .add_wall(&WallSpec {
            a: (0.0, 0.0),
            b: (10.0, 0.0),
            level,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        })
        .unwrap();
    B {
        m,
        level,
        top,
        wall,
        wt,
    }
}

impl B {
    fn door(&mut self, along: f64, width: f64) -> i64 {
        let spec = OpeningSpec {
            along,
            width,
            sill: 0.0,
            height: 2.1,
            kind: "door".into(),
        };
        self.m.add_opening(self.wall, &spec).unwrap()
    }
    fn evac(&mut self, door: i64, v: &str) {
        self.flag(door, "EvacuationExit", v);
    }
    /// Sets a norm parameter the way the properties panel does: a real project parameter.
    fn param(&mut self, kind: &str, id: i64, name: &str, v: PVal) {
        self.m.ensure_norm_params().unwrap();
        let param = self.m.param_by_name(name).unwrap().unwrap().id;
        let set = ParamSet {
            param,
            kind: kind.into(),
            id,
            value: Some(v),
        };
        self.m.set_param_values(&[set]).unwrap();
    }
    fn flag(&mut self, door: i64, name: &str, v: &str) {
        self.param(
            "opening",
            door,
            name,
            PVal::Bool(v.trim().eq_ignore_ascii_case("yes")),
        );
    }
    /// A room closed by four walls, its point in the middle.
    fn room_box(&mut self, a: (f64, f64), c: (f64, f64), height: f64) -> i64 {
        self.room_at(self.level, a, c, height)
    }
    fn room_at(&mut self, level: i64, a: (f64, f64), c: (f64, f64), height: f64) -> i64 {
        let p = [a, (c.0, a.1), c, (a.0, c.1)];
        for i in 0..4 {
            self.m
                .add_wall(&WallSpec {
                    a: p[i],
                    b: p[(i + 1) % 4],
                    level,
                    base_offset: 0.0,
                    height,
                    wall_type: self.wt,
                    offset0: None,
                })
                .unwrap();
        }
        let point = ((a.0 + c.0) / 2.0, (a.1 + c.1) / 2.0);
        self.m
            .add_room(&RoomSpec {
                level,
                name: "Hall".into(),
                number: "7".into(),
                point,
                height,
            })
            .unwrap()
    }
    fn mark(&mut self, door: i64, v: &str) {
        self.m
            .set_prop("opening", door, "Mark", Some((Value::Text(v.into()), "")))
            .unwrap();
    }
}

/// A one-rule pack in the shipped format.
fn pack(check: &str, cat: &str, severity: &str) -> Vec<NormRule> {
    let json = format!(
        r#"{{"rules":[{{"id":"t.r","title":{{"en":"T","ru":"T-ru"}},
        "source":{{"doc":"Doc 1","clause":"p.2"}},"severity":"{severity}",
        "scope":{{"cats":["{cat}"]}},"check":{check}}}]}}"#
    );
    load_norm_pack(&json).unwrap()
}

fn run(b: &B, rules: &[NormRule]) -> Vec<NormFinding> {
    run_norms(&b.m, rules, &CheckScope::All).unwrap()
}

#[test]
fn starter_pack_is_a_valid_marked_example() {
    let rules = starter_pack().unwrap();
    assert!(rules.len() >= 15, "{}", rules.len());
    let mut ids: Vec<&str> = rules.iter().map(NormRule::id).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), rules.len(), "ids are unique");
    for r in &rules {
        assert!(r.example, "{} is not marked as an example", r.id());
        assert!(
            !r.def.title.is_empty() && !r.title_ru.is_empty(),
            "{}",
            r.id()
        );
        assert!(!r.doc.is_empty() && !r.clause.is_empty(), "{}", r.id());
        assert_eq!(r.citation(), format!("{} {}", r.doc, r.clause));
        assert_eq!(r.title(true), r.title_ru);
        assert_eq!(r.title(false), r.def.title);
    }
    // an empty model breaks nothing
    let b = base();
    assert!(run(&b, &rules).is_empty());
}

#[test]
fn bad_packs_are_refused_whole() {
    assert!(load_norm_pack("{}").is_err());
    assert!(load_norm_pack(r#"{"rules":[{"id":"a"}]}"#).is_err());
    let one = r#"{"id":"a","title":{"en":"A"},"source":{"doc":"D","clause":"c"},
        "severity":"error","scope":{"cats":["door"]},"check":{"kind":"unique","param":"Mark"}}"#;
    assert!(
        load_norm_pack(&format!("[{one},{one}]")).is_err(),
        "duplicate id"
    );
    let no_clause = one.replace(r#""clause":"c""#, r#""clause":" ""#);
    assert!(
        load_norm_pack(&format!("[{no_clause}]")).is_err(),
        "clause is required"
    );
}

#[test]
fn range_rule_names_clause_and_numbers() {
    let mut b = base();
    let narrow = b.door(2.0, 0.7);
    b.door(6.0, 0.9);
    let rules = pack(
        r#"{"kind":"param_range","param":"width","min":0.8}"#,
        "door",
        "error",
    );
    let f = run(&b, &rules);
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].elements, vec![("opening".to_string(), narrow)]);
    assert_eq!(f[0].rule_id, "t.r");
    assert_eq!(f[0].severity, Severity::Error);
    assert!((f[0].measured.unwrap() - 0.7).abs() < 1e-9);
    assert!((f[0].required.unwrap() - 0.8).abs() < 1e-9);
    assert_eq!(f[0].unit, "m");
    assert_eq!(rules[0].citation(), "Doc 1 p.2");
}

#[test]
fn presence_rule_passes_once_filled() {
    let mut b = base();
    let d = b.door(2.0, 0.9);
    let rules = pack(
        r#"{"kind":"param_present","param":"Mark"}"#,
        "door",
        "warning",
    );
    assert_eq!(run(&b, &rules).len(), 1);
    b.mark(d, "D1");
    assert!(run(&b, &rules).is_empty());
}

#[test]
fn allowed_values_rule() {
    let mut b = base();
    let d = b.door(2.0, 0.9);
    b.mark(d, "X");
    let rules = pack(
        r#"{"kind":"param_in","param":"Mark","allowed":["D1","D2"]}"#,
        "door",
        "warning",
    );
    assert_eq!(run(&b, &rules).len(), 1);
    b.mark(d, "D2");
    assert!(run(&b, &rules).is_empty());
}

#[test]
fn unique_rule_flags_every_holder_of_a_shared_mark() {
    let mut b = base();
    let (d1, d2, d3) = (b.door(1.0, 0.9), b.door(4.0, 0.9), b.door(7.0, 0.9));
    b.mark(d1, "D1");
    b.mark(d2, "D1");
    b.mark(d3, "D3");
    let rules = pack(r#"{"kind":"unique","param":"Mark"}"#, "door", "error");
    let f = run(&b, &rules);
    let ids: Vec<i64> = f.iter().map(|x| x.elements[0].1).collect();
    assert_eq!(ids, vec![d1, d2]);
    assert!(f
        .iter()
        .all(|x| x.measured == Some(2.0) && x.required == Some(1.0)));
    b.mark(d2, "D2");
    assert!(
        run(&b, &rules).is_empty(),
        "blank marks are not duplicates either"
    );
    b.m.set_prop("opening", d1, "Mark", None).unwrap();
    b.m.set_prop("opening", d2, "Mark", None).unwrap();
    assert!(run(&b, &rules).is_empty());
}

#[test]
fn clearance_rule_measures_the_gap() {
    let mut b = base();
    b.door(2.0, 0.9);
    b.door(3.0, 0.9);
    let rules = pack(
        r#"{"kind":"clearance","other":["door"],"min_m":2.0}"#,
        "door",
        "info",
    );
    assert!(!run(&b, &rules).is_empty());
    let rules = pack(
        r#"{"kind":"clearance","other":["door"],"min_m":0.05}"#,
        "door",
        "info",
    );
    assert!(run(&b, &rules).is_empty());
}

#[test]
fn stair_rules_read_riser_tread_and_width() {
    let mut b = base();
    let mut spec = StairSpec::straight(b.level, b.top, (1.0, 2.0), 0.0);
    spec.max_riser = 0.22;
    spec.width = 0.8;
    let s = b.m.add_stair(&spec).unwrap();
    let riser = pack(
        r#"{"kind":"param_range","param":"riser","max":0.19}"#,
        "stair",
        "error",
    );
    let f = run(&b, &riser);
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].elements, vec![("stair".to_string(), s)]);
    assert!(f[0].measured.unwrap() > 0.19 && f[0].required == Some(0.19));
    assert_eq!(f[0].unit, "m");
    let width = pack(
        r#"{"kind":"param_range","param":"width","min":0.9}"#,
        "stair",
        "error",
    );
    assert_eq!(run(&b, &width).len(), 1);
    let tread = pack(
        r#"{"kind":"param_range","param":"tread","min":0.01}"#,
        "stair",
        "error",
    );
    assert!(run(&b, &tread).is_empty());
}

#[test]
fn room_rule_and_unknown_category() {
    let mut b = base();
    b.m.add_room(&RoomSpec {
        level: b.level,
        name: "R".into(),
        number: "1".into(),
        point: (1.0, 1.0),
        height: 2.2,
    })
    .unwrap();
    let rules = pack(
        r#"{"kind":"param_range","param":"height","min":2.5}"#,
        "room",
        "warning",
    );
    let f = run(&b, &rules);
    assert_eq!(f.len(), 1);
    assert_eq!((f[0].measured, f[0].required), (Some(2.2), Some(2.5)));
    // a category the engine does not know is reported, never silently passed
    let rules = pack(
        r#"{"kind":"param_present","param":"x"}"#,
        "spaceship",
        "error",
    );
    let f = run(&b, &rules);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].severity, Severity::Info);
    assert!(f[0].elements.is_empty());
}

#[test]
fn identical_model_gives_identical_ordered_findings() {
    let mut b = base();
    for i in 0..6 {
        let d = b.door(1.0 + i as f64 * 1.4, 0.6 + 0.05 * i as f64);
        b.mark(d, if i % 2 == 0 { "A" } else { "B" });
    }
    let rules = starter_pack().unwrap();
    let first = run(&b, &rules);
    assert!(first.len() > 6, "{}", first.len());
    for _ in 0..3 {
        assert_eq!(run(&b, &rules), first);
    }
    // errors first, then pack order
    let keys: Vec<(Severity, usize)> = first.iter().map(|f| (f.severity, f.rule_index)).collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
    // a rebuilt model with the same steps gives the same findings
    let mut c = base();
    for i in 0..6 {
        let d = c.door(1.0 + i as f64 * 1.4, 0.6 + 0.05 * i as f64);
        c.mark(d, if i % 2 == 0 { "A" } else { "B" });
    }
    assert_eq!(run(&c, &rules), first);
    assert_eq!(
        findings_json(&rules, &first),
        findings_json(&rules, &run(&c, &rules))
    );
}

#[test]
fn json_export_carries_rule_clause_elements_and_numbers() {
    let mut b = base();
    let d = b.door(2.0, 0.7);
    let rules = pack(
        r#"{"kind":"param_range","param":"width","min":0.8}"#,
        "door",
        "error",
    );
    let f = run(&b, &rules);
    let v: serde_json::Value = serde_json::from_str(&findings_json(&rules, &f)).unwrap();
    assert_eq!(v["format"], "ddc-norm-findings");
    let t = &v["topics"][0];
    assert_eq!(t["rule"]["document"], "Doc 1");
    assert_eq!(t["rule"]["clause"], "p.2");
    assert_eq!(t["priority"], "High");
    assert_eq!(t["components"][0]["type"], "opening");
    assert_eq!(t["components"][0]["id"], d);
    assert_eq!(t["measured"], 0.7);
    assert_eq!(t["required"], 0.8);
    assert_eq!(t["title_ru"], "T-ru");
}

fn rooms_with(b: &mut B, name: &str, number: &str, at: (f64, f64), height: f64) {
    b.m.add_room(&RoomSpec {
        level: b.level,
        name: name.into(),
        number: number.into(),
        point: at,
        height,
    })
    .unwrap();
}

/// Builds a model that breaks `id` (true) or satisfies it (false).
fn fixture(id: &str, broken: bool) -> B {
    let mut b = base();
    let pick = |bad: f64, good: f64| if broken { bad } else { good };
    match id {
        "ex.wall.thickness" => {
            let t = if broken { 0.05 } else { 0.2 };
            let wt =
                b.m.add_wall_type(
                    "T",
                    &[LayerSpec {
                        material: "Brick".into(),
                        thickness: t,
                    }],
                )
                .unwrap();
            b.m.add_wall(&WallSpec {
                a: (0.0, 5.0),
                b: (4.0, 5.0),
                level: b.level,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wt,
                offset0: None,
            })
            .unwrap();
            if broken {
                return b;
            }
        }
        "ex.door.mark" | "ex.door.fire" | "ex.door.mark_unique" => {
            let d = b.door(2.0, 0.9);
            let e = b.door(6.0, 0.9);
            match id {
                "ex.door.mark" if !broken => {
                    b.mark(d, "D1");
                    b.mark(e, "D2");
                }
                "ex.door.fire" if !broken => {
                    for x in [d, e] {
                        let v = Some((Value::Text("EI30".into()), ""));
                        b.m.set_prop("opening", x, "FireRating", v).unwrap();
                    }
                }
                "ex.door.mark_unique" => {
                    b.mark(d, "D1");
                    b.mark(e, if broken { "D1" } else { "D2" });
                }
                _ => {}
            }
        }
        "ex.window.mark" | "ex.window.mark_unique" => {
            for (i, along) in [2.0, 6.0].into_iter().enumerate() {
                let spec = OpeningSpec {
                    along,
                    width: 1.2,
                    sill: 0.9,
                    height: 1.2,
                    kind: "window".into(),
                };
                let w = b.m.add_opening(b.wall, &spec).unwrap();
                let same = id == "ex.window.mark_unique" && broken;
                let v = if same { "W1".into() } else { format!("W{i}") };
                if id == "ex.window.mark_unique" || !broken {
                    b.mark(w, &v);
                }
            }
        }
        "ex.room.name" => rooms_with(
            &mut b,
            if broken { "" } else { "Hall" },
            "1",
            (1.0, 1.0),
            3.0,
        ),
        "ex.room.number" => rooms_with(
            &mut b,
            "Hall",
            if broken { "" } else { "1" },
            (1.0, 1.0),
            3.0,
        ),
        "ex.room.number_unique" => {
            rooms_with(&mut b, "A", "1", (1.0, 1.0), 3.0);
            rooms_with(&mut b, "B", if broken { "1" } else { "2" }, (3.0, 1.0), 3.0);
        }
        "ex.window.width" => {
            let spec = OpeningSpec {
                along: 2.0,
                width: pick(0.3, 1.2),
                sill: 0.9,
                height: 1.2,
                kind: "window".into(),
            };
            b.m.add_opening(b.wall, &spec).unwrap();
        }
        "ex.door.width" => {
            let d = b.door(2.0, pick(0.7, 0.9));
            b.evac(d, "yes");
        }
        "ex.door.evac" => {
            let d = b.door(2.0, 0.9);
            if !broken {
                b.evac(d, "no");
            }
        }
        "ex.door.height" => {
            let spec = OpeningSpec {
                along: 2.0,
                width: 0.9,
                sill: 0.0,
                height: pick(1.8, 2.1),
                kind: "door".into(),
            };
            let d = b.m.add_opening(b.wall, &spec).unwrap();
            b.evac(d, "Yes ");
        }
        "ex.stair.riser" | "ex.stair.tread" | "ex.stair.risers" => {
            let mut s = StairSpec::straight(b.level, b.top, (1.0, 2.0), 0.0);
            if id == "ex.stair.riser" {
                s.max_riser = pick(0.25, 0.17);
            } else if id == "ex.stair.risers" {
                // 3 m of rise: 0.10 m risers make 30, 0.20 m risers make 15
                s.max_riser = pick(0.10, 0.20);
            } else {
                // 2 * riser + tread must stay in the comfort band the stair builder demands
                s.tread = Some(pick(0.248, 0.28));
            }
            b.m.add_stair(&s).unwrap();
        }
        "ex.door.column" => {
            b.door(2.0, 0.9);
            let mt =
                b.m.add_member_type(
                    "C",
                    &crate::MemberShape::Rect { b: 0.4, h: 0.4 },
                    "Concrete",
                )
                .unwrap();
            b.m.add_column(&crate::ColumnSpec {
                level: b.level,
                top_level: None,
                member_type: mt,
                structural: true,
                at: (2.45, pick(0.1, 3.0)),
                base_offset: 0.0,
                top_offset: 3.0,
                rotation: 0.0,
                top_at: None,
            })
            .unwrap();
        }
        "ex.stair.landing" => {
            let mut s = StairSpec::straight(b.level, b.top, (1.0, 2.0), 0.0);
            s.landing = pick(0.9, 1.2);
            b.m.add_stair(&s).unwrap();
        }
        "ex.stair.slope" => {
            let mut s = StairSpec::straight(b.level, b.top, (1.0, 2.0), 0.0);
            // a 0.25 m riser with a 0.12 m tread is steeper than a riser-high step is deep
            s.max_riser = pick(0.25, 0.15);
            s.tread = Some(pick(0.12, 0.31));
            b.m.add_stair(&s).unwrap();
        }
        "ex.corridor.width" => {
            // clear width 1.1 m: 60 people need 1.2 m, 40 people need 1.0 m
            let r = b.room_box((0.0, 3.0), (10.0, 4.3), 3.0);
            b.param("room", r, "Purpose", PVal::Text("corridor".into()));
            b.param(
                "room",
                r,
                "Occupants",
                PVal::Int(if broken { 60 } else { 40 }),
            );
        }
        "ex.corridor.workplace" => {
            // a 0.6 m passage is below 0.7 m, a 1.1 m one is fine for 3 people
            let r = b.room_box((0.0, 3.0), (10.0, pick(3.6, 4.1)), 3.0);
            b.param("room", r, "Purpose", PVal::Text("workplace_passage".into()));
            b.param("room", r, "Occupants", PVal::Int(3));
        }
        "ex.stair.flight_width" => {
            building(&mut b, "\u{424}1.1", None, 10);
            let mut s = StairSpec::straight(b.level, b.top, (1.0, 2.0), 0.0);
            s.width = pick(1.2, 1.4);
            b.m.add_stair(&s).unwrap();
        }
        "ex.stair.headroom" => {
            let s =
                b.m.add_stair(&StairSpec::straight(b.level, b.top, (1.0, 2.0), 0.0))
                    .unwrap();
            b.param("stair", s, "Headroom", PVal::Num(pick(2.0, 2.3)));
        }
        "ex.room.height_public" | "ex.room.height_service" => {
            let public = id.ends_with("public");
            let (name, good) = if public {
                ("public", 3.2)
            } else {
                ("service", 2.8)
            };
            let r = b.room_box((0.0, 3.0), (6.0, 8.0), pick(2.5, good));
            b.param("room", r, "Purpose", PVal::Text(name.into()));
        }
        "ex.door.accessible_width" => {
            let d = b.door(2.0, pick(0.8, 0.95));
            b.flag(d, "Accessible", "yes");
        }
        "ex.room.purpose" => {
            let r = b.room_box((0.0, 3.0), (6.0, 8.0), 3.0);
            if !broken {
                b.param("room", r, "Purpose", PVal::Text("office".into()));
            }
        }
        other => panic!("no fixture for {other}"),
    }
    b
}

#[test]
fn every_starter_rule_fails_on_a_bad_model_and_passes_on_a_good_one() {
    let rules = starter_pack().unwrap();
    for r in &rules {
        let one = std::slice::from_ref(r);
        let bad = run(&fixture(r.id(), true), one);
        assert!(
            !bad.is_empty(),
            "{} found nothing on a breaking model",
            r.id()
        );
        assert!(bad.iter().all(|f| f.rule_id == r.id()));
        assert!(
            bad.iter()
                .all(|f| f.severity == r.def.severity || f.elements.is_empty()),
            "{}",
            r.id()
        );
        let good = run(&fixture(r.id(), false), one);
        assert!(good.is_empty(), "{} flagged a good model: {good:?}", r.id());
    }
}

#[test]
fn exit_rules_apply_only_to_marked_evacuation_exits() {
    let mut b = base();
    let sanitary = b.door(1.0, 0.7);
    let exit = b.door(4.0, 0.7);
    let unset = b.door(7.0, 0.7);
    b.evac(sanitary, "no");
    b.evac(exit, " YES");
    let rules: Vec<NormRule> = starter_pack()
        .unwrap()
        .into_iter()
        .filter(|r| matches!(r.id(), "ex.door.width" | "ex.door.evac"))
        .collect();
    let f = run(&b, &rules);
    let hit = |rule: &str| -> Vec<i64> {
        f.iter()
            .filter(|x| x.rule_id == rule)
            .map(|x| x.elements[0].1)
            .collect()
    };
    assert_eq!(
        hit("ex.door.width"),
        vec![exit],
        "only the exit is held to 0.8 m"
    );
    assert_eq!(
        hit("ex.door.evac"),
        vec![unset],
        "an unset door is reported as a note"
    );
    let note = f.iter().find(|x| x.rule_id == "ex.door.evac").unwrap();
    assert_eq!(note.severity, Severity::Info);
}

/// A second level with its own wall and door.
fn upper(b: &mut B) -> i64 {
    let wt =
        b.m.add_wall_type(
            "U",
            &[LayerSpec {
                material: "Brick".into(),
                thickness: 0.2,
            }],
        )
        .unwrap();
    let w =
        b.m.add_wall(&WallSpec {
            a: (0.0, 0.0),
            b: (10.0, 0.0),
            level: b.top,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        })
        .unwrap();
    let spec = OpeningSpec {
        along: 2.0,
        width: 0.9,
        sill: 0.0,
        height: 2.1,
        kind: "door".into(),
    };
    b.m.add_opening(w, &spec).unwrap()
}

#[test]
fn duplicates_are_found_across_levels_but_reported_inside_the_area_only() {
    let mut b = base();
    let low = b.door(2.0, 0.9);
    let high = upper(&mut b);
    b.mark(low, "D1");
    b.mark(high, "D1");
    let rules = pack(r#"{"kind":"unique","param":"Mark"}"#, "door", "error");
    let all = run_norms(&b.m, &rules, &CheckScope::All).unwrap();
    assert_eq!(all.len(), 2);
    let only_low = run_norms(&b.m, &rules, &CheckScope::Level(b.level)).unwrap();
    assert_eq!(
        only_low.len(),
        1,
        "the twin on another level still makes it a duplicate"
    );
    assert_eq!(only_low[0].elements, vec![("opening".to_string(), low)]);
    assert_eq!(only_low[0].measured, Some(2.0));
}

#[test]
fn duplicates_ignore_surrounding_blanks_and_case_only_when_asked() {
    let mut b = base();
    let (d1, d2) = (b.door(2.0, 0.9), b.door(6.0, 0.9));
    b.mark(d1, "D1");
    b.mark(d2, "D1 ");
    let loose = pack(r#"{"kind":"unique","param":"Mark"}"#, "door", "error");
    assert_eq!(
        run(&b, &loose).len(),
        2,
        "a trailing blank is the same mark"
    );
    b.mark(d2, "d1");
    assert!(run(&b, &loose).is_empty(), "case counts by default");
    let nocase = pack(
        r#"{"kind":"unique","param":"Mark","case_sensitive":false}"#,
        "door",
        "error",
    );
    assert_eq!(run(&b, &nocase).len(), 2);
}

#[test]
fn scope_filter_round_trips_and_a_damaged_pack_is_an_error() {
    let json = r#"[{"id":"a","title":{"en":"A"},"source":{"doc":"D","clause":"c"},
        "severity":"error","scope":{"cats":["door"],"when":{"param":"P","eq":"yes"}},
        "check":{"kind":"param_present","param":"Mark"}}]"#;
    let r = load_norm_pack(json).unwrap();
    assert_eq!(
        r[0].def.scope.when,
        Some(Cond::Eq("P".to_string(), "yes".to_string()))
    );
    assert!(load_norm_pack("[{").is_err());
    // a scope filter is an equality; any other condition is refused
    let bad_when = json.replace(r#""eq":"yes""#, r#""in":["yes"]"#);
    assert!(load_norm_pack(&bad_when).is_err());
}

#[test]
fn corridor_width_tiers_by_people_and_reports_missing_data() {
    let rules: Vec<NormRule> = starter_pack()
        .unwrap()
        .into_iter()
        .filter(|r| r.id() == "ex.corridor.width")
        .collect();
    let mut b = base();
    let r = b.room_box((0.0, 3.0), (10.0, 4.3), 3.0);
    // a room with no purpose is not a corridor: silent here (a separate note covers it)
    assert!(run(&b, &rules).is_empty());
    b.param("room", r, "Purpose", PVal::Text(" Corridor ".into()));
    let f = run(&b, &rules);
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].severity, Severity::Info, "people not set is a note");
    b.param("room", r, "Occupants", PVal::Int(40));
    assert!(run(&b, &rules).is_empty(), "1.1 m clears the 1.0 m tier");
    b.param("room", r, "Occupants", PVal::Int(60));
    let f = run(&b, &rules);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].severity, Severity::Error);
    let (v, lim) = (f[0].measured.unwrap(), f[0].required.unwrap());
    assert!(
        (v - 1.1).abs() < 1e-6 && (lim - 1.2).abs() < 1e-9,
        "{v} {lim}"
    );
    assert_eq!(f[0].unit, "m");
}

#[test]
fn ring_width_is_the_narrowest_gap() {
    use super::rule_cases::ring_min_width;
    let rect = [(0.0, 0.0), (10.0, 0.0), (10.0, 1.0), (0.0, 1.0)];
    assert!((ring_min_width(&rect).unwrap() - 1.0).abs() < 1e-9);
    let cw: Vec<_> = rect.iter().rev().copied().collect();
    assert!(
        (ring_min_width(&cw).unwrap() - 1.0).abs() < 1e-9,
        "orientation does not matter"
    );
    // an L of two arms, 1.0 and 1.5 wide: the narrow arm decides
    let l = [
        (0.0, 0.0),
        (8.0, 0.0),
        (8.0, 1.0),
        (1.5, 1.0),
        (1.5, 6.0),
        (0.0, 6.0),
    ];
    assert!((ring_min_width(&l).unwrap() - 1.0).abs() < 1e-9);
    assert!(ring_min_width(&rect[..2]).is_none());
}

#[test]
fn compare_rule_checks_both_sides_and_refuses_bad_definitions() {
    let json = r#"[{"id":"c","title":{"en":"C"},"source":{"doc":"D","clause":"c"},
        "severity":"error","scope":{"cats":["stair"]},
        "check":{"kind":"compare","a":"landing","op":">=","b":"width","factor":1.0}}]"#;
    let rules = load_norm_pack(json).unwrap();
    let mut b = base();
    let mut s = StairSpec::straight(b.level, b.top, (1.0, 2.0), 0.0);
    s.landing = 1.0;
    s.width = 1.2;
    b.m.add_stair(&s).unwrap();
    let f = run(&b, &rules);
    assert_eq!((f[0].measured, f[0].required), (Some(1.0), Some(1.2)));
    let scaled = json.replace(r#""factor":1.0"#, r#""factor":0.8"#);
    assert!(
        run(&b, &load_norm_pack(&scaled).unwrap()).is_empty(),
        "1.0 >= 1.2 x 0.8"
    );
    assert!(
        load_norm_pack(&json.replace("\">=\"", "\"=\"")).is_err(),
        "operator"
    );
    assert!(load_norm_pack(&json.replace(r#""factor":1.0"#, r#""factor":0"#)).is_err());
}

#[test]
fn norm_parameters_are_added_once_in_one_undo_step() {
    let mut b = base();
    assert_eq!(b.m.norm_params_missing().unwrap().len(), 5);
    let top = b.m.op_top().unwrap();
    assert_eq!(b.m.ensure_norm_params().unwrap(), 5);
    assert_eq!(b.m.op_top().unwrap(), top + 1);
    assert!(b.m.norm_params_missing().unwrap().is_empty());
    assert_eq!(b.m.ensure_norm_params().unwrap(), 0);
    assert_eq!(
        b.m.op_top().unwrap(),
        top + 1,
        "nothing to add is no operation"
    );
    let door = b.m.param_by_name("Accessible").unwrap().unwrap();
    assert_eq!(door.spec.categories, vec!["door".to_string()]);
}

/// Fire class, typed height and `people` on a room of the upper floor (negative: no room).
fn building(b: &mut B, class: &str, height: Option<f64>, people: i64) {
    let p = NormProject {
        fire_class: class.into(),
        height_m: height,
    };
    b.m.set_norm_project(&p).unwrap();
    if people >= 0 {
        let r = b.room_at(b.top, (20.0, 3.0), (26.0, 8.0), 3.0);
        b.param("room", r, "Occupants", PVal::Int(people));
    }
}

fn flight_rule() -> Vec<NormRule> {
    let all = starter_pack().unwrap();
    all.into_iter()
        .filter(|r| r.id() == "ex.stair.flight_width")
        .collect()
}

/// Findings of the flight-width rule for a stair of `width` in a building.
fn flight(class: &str, height: Option<f64>, people: i64, width: f64) -> Vec<NormFinding> {
    let mut b = base();
    building(&mut b, class, height, people);
    let mut s = StairSpec::straight(b.level, b.top, (1.0, 2.0), 0.0);
    s.width = width;
    b.m.add_stair(&s).unwrap();
    run(&b, &flight_rule())
}

/// One sub-item: 5 cm under the limit is an error citing it, exactly the limit passes.
fn pair(class: &str, height: Option<f64>, people: i64, limit: f64, clause: &str) {
    let bad = flight(class, height, people, limit - 0.05);
    assert_eq!(bad.len(), 1, "{clause}: {bad:?}");
    assert_eq!(bad[0].severity, Severity::Error);
    assert_eq!(bad[0].clause.as_deref(), Some(clause));
    assert!(
        (bad[0].required.unwrap() - limit).abs() < 1e-9,
        "{clause}: {bad:?}"
    );
    assert!((bad[0].measured.unwrap() - (limit - 0.05)).abs() < 1e-9);
    let good = flight(class, height, people, limit);
    assert!(good.is_empty(), "{clause}: {good:?}");
}

#[test]
fn flight_width_follows_each_sub_item_of_the_clause() {
    pair("\u{424}1.2", None, 700, 1.6, "p.4.4.1 \u{431})");
    pair("\u{424}2.1", None, 10, 1.35, "p.4.4.1 \u{430})");
    pair("\u{424}1.2", None, 250, 1.35, "p.4.4.1 \u{430})");
    pair("\u{424}3.1", Some(60.0), 10, 1.35, "p.4.4.1 \u{430})");
    pair("\u{424}1.3", Some(60.0), 10, 1.05, "p.4.4.1 \u{433})");
    pair("\u{424}1.2", None, 10, 1.2, "p.4.4.1 \u{432})");
    pair("\u{424}1.3", Some(80.0), 3, 0.7, "p.4.4.1 \u{434})");
    pair("\u{424}1.3", Some(80.0), 10, 0.9, "p.4.4.1 \u{435})");
}

#[test]
fn flight_width_priority_and_edges() {
    // 700 people on a floor: b) wins over the class of a)
    let f = flight("\u{424}1.1", None, 700, 1.5);
    assert_eq!(f[0].clause.as_deref(), Some("p.4.4.1 \u{431})"));
    // a public building of exactly 50 m is not "more than 50 m": falls to c)
    let f = flight("\u{424}3.1", Some(50.0), 10, 1.1);
    assert_eq!(f[0].clause.as_deref(), Some("p.4.4.1 \u{432})"));
    // 200 people is not "more than 200"
    assert!(flight("\u{424}1.2", None, 200, 1.2).is_empty());
}

#[test]
fn flight_width_says_when_the_norm_is_silent_or_the_data_missing() {
    for (class, height) in [("\u{424}1.3", 75.0), ("\u{424}1.4", 3.0), ("\u{424}5.1", 3.0)] {
        let f = flight(class, Some(height), 10, 0.5);
        assert_eq!(f.len(), 1, "{class}: {f:?}");
        assert_eq!(f[0].severity, Severity::Info, "{class}");
        assert!(f[0].measured.is_none(), "{class}: the norm gives no number");
    }
    // no class, no people count: notes, not verdicts
    let f = flight("", None, 10, 0.5);
    assert_eq!((f.len(), f[0].severity), (1, Severity::Info));
    let f = flight("\u{424}1.2", None, -1, 0.5);
    assert_eq!((f.len(), f[0].severity), (1, Severity::Info));
}

#[test]
fn building_inputs_are_stored_validated_and_computed() {
    let mut b = base();
    assert_eq!(b.m.norm_project().unwrap(), NormProject::default());
    assert_eq!(
        b.m.building_height().unwrap(),
        Some(3.0),
        "span of the levels"
    );
    assert_eq!(
        b.m.people_upper_floor().unwrap(),
        None,
        "nobody counted yet"
    );
    let top = b.m.op_top().unwrap();
    let p = NormProject {
        fire_class: "\u{424}2.1".into(),
        height_m: Some(12.5),
    };
    assert!(b.m.set_norm_project(&p).unwrap());
    assert!(
        !b.m.set_norm_project(&p).unwrap(),
        "same values are no operation"
    );
    assert_eq!(b.m.op_top().unwrap(), top + 1);
    assert_eq!(b.m.norm_project().unwrap(), p);
    assert_eq!(
        b.m.building_height().unwrap(),
        Some(12.5),
        "typed height wins"
    );
    let bad = NormProject {
        fire_class: "\u{424}9.9".into(),
        height_m: None,
    };
    assert!(b.m.set_norm_project(&bad).is_err());
    let negative = NormProject {
        fire_class: String::new(),
        height_m: Some(-1.0),
    };
    assert!(b.m.set_norm_project(&negative).is_err());
    b.m.undo().unwrap();
    assert_eq!(b.m.norm_project().unwrap(), NormProject::default());
    // people: ground floor does not count, the busiest upper floor is the sum of its rooms
    let ground = b.room_box((0.0, 3.0), (6.0, 8.0), 3.0);
    b.param("room", ground, "Occupants", PVal::Int(900));
    let a = b.room_at(b.top, (20.0, 3.0), (26.0, 8.0), 3.0);
    let c = b.room_at(b.top, (30.0, 3.0), (36.0, 8.0), 3.0);
    b.param("room", a, "Occupants", PVal::Int(30));
    b.param("room", c, "Occupants", PVal::Int(12));
    assert_eq!(b.m.people_upper_floor().unwrap(), Some(42));
}

#[test]
fn small_corridor_to_workplaces_and_headroom_have_their_own_limits() {
    let rules = starter_pack().unwrap();
    let only =
        |id: &str| -> Vec<NormRule> { rules.iter().filter(|r| r.id() == id).cloned().collect() };
    let mut b = base();
    let r = b.room_box((0.0, 3.0), (10.0, 4.0), 3.0);
    b.param("room", r, "Purpose", PVal::Text("workplace_passage".into()));
    b.param("room", r, "Occupants", PVal::Int(60));
    let f = run(&b, &only("ex.corridor.workplace"));
    assert_eq!(f.len(), 1, "more than 50 people need 1.2 m: {f:?}");
    assert!((f[0].required.unwrap() - 1.2).abs() < 1e-9);
    b.param("room", r, "Occupants", PVal::Int(3));
    assert!(
        run(&b, &only("ex.corridor.workplace")).is_empty(),
        "0.8 m clears 0.7 m"
    );
    let s =
        b.m.add_stair(&StairSpec::straight(b.level, b.top, (1.0, 2.0), 0.0))
            .unwrap();
    assert!(run(&b, &only("ex.stair.headroom"))
        .iter()
        .all(|f| f.severity == Severity::Info));
    b.param("stair", s, "Headroom", PVal::Num(2.2));
    assert!(
        run(&b, &only("ex.stair.headroom")).is_empty(),
        "exactly 2.2 m passes"
    );
}
