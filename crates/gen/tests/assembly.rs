//! Layered build-ups: every wall, slab and roof of the bench has one, the plan geometry takes
//! its thickness, the U-values of the envelope meet each climate's pack and the sound index of
//! party walls and floors the rule, and a rule the catalogue cannot meet is reported.

#[path = "../bench/suite.rs"]
mod suite;

use ddc_gen::project::assembly::{self, for_rules, uses_of, wall_uses, Use};
use ddc_gen::project::nav::check;
use ddc_gen::project::rules::{self, norms_from, Layer};
use ddc_gen::project::why::explain_project;
use ddc_gen::project::{build_project, check_project, parse_spec, ProjectSpec};
use ddc_gen::WallRole;
use ddc_model::checks::norm_pack::PackSet;
use serde_json::{json, Value as J};

fn spec(v: &J) -> ProjectSpec {
    parse_spec(v).unwrap_or_else(|e| panic!("{v}: {e}"))
}

#[test]
fn every_wall_slab_and_roof_of_the_bench_has_a_layered_build_up() {
    let (mut built, mut walls) = (0, 0);
    for b in suite::load_briefs() {
        let Ok(s) = parse_spec(&suite::spec_of(&b)) else { continue };
        let Ok(p) = build_project(&s) else { continue };
        built += 1;
        for st in p.storeys.iter().chain(&p.basements) {
            let uses = wall_uses(st);
            assert_eq!(uses.len(), st.layout.walls.len(), "{}", b.name);
            for (w, u) in st.layout.walls.iter().zip(&uses) {
                assert!(u.use_.is_wall(), "{}", b.name);
                assert_eq!(u.use_ == Use::ExteriorWall, w.role == WallRole::Exterior, "{}", b.name);
                assert_eq!(u.use_ == Use::RetainingWall, w.role == WallRole::Retaining, "{}", b.name);
                assert!(!u.reversed || matches!(w.role, WallRole::Exterior | WallRole::Retaining), "{}", b.name);
                walls += 1;
            }
        }
        let used = uses_of(&p);
        assert!(used.contains(&Use::GroundSlab), "{}: no ground slab", b.name);
        assert!(used.contains(&assembly::roof_use(p.roof.slope)), "{}: no roof", b.name);
        if p.storeys.len() + p.basements.len() > 1 {
            assert!(used.contains(&Use::IntermediateSlab), "{}: no floor", b.name);
        }
        for u in used {
            let a = for_rules(&s.rules, u);
            assert!(a.layers.len() >= 2 && a.layers.iter().all(|l| l.mm > 0.0), "{}: {u:?} {:?}", b.name, a.layers);
            assert!(a.misses.is_empty(), "{}: {u:?} misses {:?}", b.name, a.misses);
        }
        // the plan geometry takes the build-up's thickness
        rules::scoped(&s.rules, || {
            for role in [WallRole::Exterior, WallRole::Retaining, WallRole::Party, WallRole::Corridor, WallRole::Partition] {
                let a = for_rules(&s.rules, Use::of_role(role));
                assert_eq!(role.thickness(), a.thickness(), "{}: {role:?}", b.name);
            }
            let r = for_rules(&s.rules, assembly::roof_use(p.roof.slope));
            assert_eq!(assembly::roof_thickness(p.roof.slope), r.thickness(), "{}", b.name);
        });
    }
    assert!(built >= 25 && walls >= 1000, "{built} projects, {walls} walls");
}

#[test]
fn the_built_in_rules_keep_the_former_thicknesses() {
    let t = |u: Use| assembly::pick(u, &rules::rule).thickness();
    assert_eq!(t(Use::ExteriorWall), 0.3);
    assert_eq!(t(Use::RetainingWall), 0.5, "the former retaining wall");
    assert_eq!(t(Use::PartyWall), 0.2);
    assert_eq!(t(Use::Partition), 0.1);
    assert_eq!(t(Use::WetPartition), t(Use::Partition), "a wet room changes the boards, not the wall line");
    for u in Use::ALL {
        let a = assembly::pick(u, &rules::rule);
        assert!(a.misses.is_empty(), "{u:?}: {:?}", a.misses);
        if let Some(r) = u.u_rule() {
            assert!(a.u.unwrap() <= rules::rule(r), "{u:?}");
        }
        if let Some(r) = u.rw_rule() {
            assert!(a.rw.unwrap() >= rules::rule(r), "{u:?}");
        }
    }
    // combustible insulation is kept off the envelope by the fire class rule: at U 0.15 the
    // better-insulating EPS is 20 mm thinner, but only taken once class E is allowed
    let at = |class: f64| {
        move |id: &str| match id {
            assembly::CLASS_RULE => class,
            "envelope.u_wall" => 0.15,
            _ => rules::rule(id),
        }
    };
    let strict = assembly::pick(Use::ExteriorWall, &at(rules::rule(assembly::CLASS_RULE)));
    assert_eq!(strict.id, "ext_rc_mw");
    let loose = assembly::pick(Use::ExteriorWall, &at(5.0));
    assert_eq!(loose.id, "ext_rc_eps", "with class E allowed the thinner build-up wins");
    assert!((strict.thickness() - loose.thickness() - 0.02).abs() < 1e-9, "{} {}", strict.thickness(), loose.thickness());
    assert!(strict.misses.is_empty() && loose.misses.is_empty());
}

#[test]
fn exterior_u_values_meet_the_pack_of_each_climate() {
    let set = PackSet::embedded();
    assert!(set.problems.is_empty(), "{:?}", set.problems);
    let dir = suite::crate_dir().join("data/bench");
    let mut walls = vec![];
    for id in ["es", "ru", "de", "uk"] {
        let pack = set.resolve(id).unwrap();
        let norms = norms_from(|k| pack.dim_cite(k));
        let limit = norms["envelope.u_wall"]["value"].as_f64().unwrap_or_else(|| panic!("{id}: no wall U-value"));
        let mut b = suite::read_brief(&dir.join("section_5.json"));
        b.spec["norms"] = norms.clone();
        let s = spec(&b.spec);
        assert_eq!(s.rules.resolve("envelope.u_wall"), Some((limit, Layer::Norm)), "{id}");
        for (key, u) in [("envelope.u_wall", Use::ExteriorWall), ("envelope.u_roof", Use::FlatRoof), ("envelope.u_ground", Use::GroundSlab)] {
            let a = for_rules(&s.rules, u);
            let want = s.rules.get(key);
            assert!(a.u.unwrap() <= want + 1e-9, "{id}: {u:?} U {:?} over {want}", a.u);
            if let Some(v) = norms[key]["value"].as_f64() {
                assert_eq!(want, v, "{id}: {key}");
            }
        }
        let p = build_project(&s).unwrap_or_else(|e| panic!("{id}: {e}"));
        let g = check_project(&s, &p);
        assert!(!g.failures.iter().chain(&g.warnings).any(|f| f.contains("envelope.")), "{id}: {:?}", g.warnings);
        let ext = p.storeys[0].layout.walls.iter().find(|w| w.role == WallRole::Exterior).unwrap();
        let t = rules::scoped(&s.rules, || ext.role.thickness());
        walls.push((id, limit, t));
    }
    // a colder climate asks a lower U-value and gets a thicker wall
    walls.sort_by(|a, b| b.1.total_cmp(&a.1));
    assert!(walls.windows(2).all(|w| w[1].2 > w[0].2), "{walls:?}");
}

#[test]
fn party_walls_and_floors_reach_the_sound_rule_by_the_mass_law() {
    let lo = spec(&json!({"typology": "apartment_section", "floors": 3, "rules": {"acoustic.rw_party": 45, "acoustic.rw_floor": 45}}));
    let hi = spec(&json!({"typology": "apartment_section", "floors": 3, "rules": {"acoustic.rw_party": 58, "acoustic.rw_floor": 64}}));
    for u in [Use::PartyWall, Use::IntermediateSlab] {
        let (a, b) = (for_rules(&lo.rules, u), for_rules(&hi.rules, u));
        assert!(a.thickness() < b.thickness(), "{u:?}");
        assert!(b.rw.unwrap() > a.rw.unwrap() && b.mass > a.mass, "{u:?}");
        assert!(b.rw.unwrap() >= hi.rules.get(u.rw_rule().unwrap()), "{u:?}");
    }
}

#[test]
fn a_rule_the_catalogue_cannot_meet_is_reported_and_explained() {
    let v = json!({"typology": "apartment_section", "floors": 2, "rules": {"envelope.u_wall": 0.08, "acoustic.rw_party": 70}});
    let s = spec(&v);
    let p = build_project(&s).unwrap();
    let f = check(&s, &p);
    for rule in ["envelope.u_wall", "acoustic.rw_party"] {
        let hit = f.iter().find(|f| f.rule == rule).unwrap_or_else(|| panic!("{rule}: {f:?}"));
        assert!(hit.excess > 0.0 && !hit.hard, "{rule}");
    }
    let g = check_project(&s, &p);
    assert!(g.warnings.iter().any(|w| w.contains("envelope.u_wall")), "{:?}", g.warnings);
    let why = explain_project(&s, &p);
    for key in ["asm_wall", "asm_ground", "asm_party", "asm_floor"] {
        assert!(why.iter().any(|w| w.target == "assembly" && w.key == key), "{key}");
    }
    let wall = why.iter().find(|w| w.key == "asm_wall").unwrap();
    assert_eq!(wall.rule, "envelope.u_wall");
    assert!(wall.effect.contains("0.08"), "{}", wall.effect);
}

#[test]
fn a_bearing_build_up_below_the_project_fire_rating_is_reported() {
    let mut s = spec(&json!({"typology": "apartment_section", "floors": 2}));
    s.fire_rating = "REI 120".into();
    let p = build_project(&s).unwrap();
    let f = check(&s, &p);
    assert!(f.iter().any(|f| f.rule == assembly::FIRE_RATING && f.msg.contains("REI 120")), "{f:?}");
    s.fire_rating = "REI 60".into();
    assert!(!check(&s, &p).iter().any(|f| f.rule == assembly::FIRE_RATING));
}

#[test]
fn metro_and_basement_retaining_walls_are_waterproofed_concrete_of_the_rule_thickness() {
    for (v, thick) in [
        (json!({"typology": "underground_station"}), 0.8),
        (json!({"typology": "underground_station", "rules": {"underground.retaining_thickness": 1.0}}), 1.0),
        (json!({"typology": "mixed_use_podium"}), 0.5),
    ] {
        let s = parse_spec(&v).unwrap();
        let p = build_project(&s).unwrap();
        let mut held = 0;
        for st in &p.basements {
            let uses = wall_uses(st);
            for (w, u) in st.layout.walls.iter().zip(&uses) {
                if w.role == WallRole::Retaining {
                    assert_eq!(u.use_, Use::RetainingWall, "{v}");
                    held += 1;
                }
            }
            // no outside wall of a basement is built as an exterior wall
            assert!(!uses.iter().any(|u| u.use_ == Use::ExteriorWall), "{v}: an exterior wall below ground");
        }
        assert!(held > 0, "{v}: no retaining wall");
        assert!(uses_of(&p).contains(&Use::RetainingWall));
        let a = for_rules(&s.rules, Use::RetainingWall);
        assert!(a.misses.is_empty(), "{:?}", a.misses);
        let names: Vec<&str> = a.layers.iter().map(|l| l.material.id.as_str()).collect();
        assert_eq!(names, ["waterproofing", "rc"], "{v}");
        assert!((a.thickness() - thick).abs() < 1e-9, "{v}: {}", a.thickness());
        rules::scoped(&s.rules, || assert_eq!(WallRole::Retaining.thickness(), a.thickness()));
        assert!(a.fire_minutes >= 120);
    }
}

fn fire_boards(a: &assembly::Assembly) -> usize {
    a.layers.iter().filter(|l| l.material.id == "gypsum_fire").count()
}

#[test]
fn fire_board_brings_a_build_up_to_the_rating_it_names() {
    let s = spec(&json!({"typology": "apartment_section", "floors": 3}));
    let sys = s.design.structure;
    let base = assembly::for_rules_in(&s.rules, Use::Partition, sys);
    assert_eq!((base.fire_minutes, fire_boards(&base)), (30, 0));
    // rated as it is: nothing added
    assert_eq!(assembly::rated_in(&s.rules, Use::Partition, sys, 30), base);
    // 90 minutes from 30: three boards of 20 minutes, 45 mm thicker, and the name says EI 90
    let a = assembly::rated_in(&s.rules, Use::Partition, sys, 90);
    assert_eq!((a.fire_minutes, fire_boards(&a)), (90, 3));
    assert!((a.thickness() - base.thickness() - 0.045).abs() < 1e-9);
    assert!(a.name.contains("EI90"), "{}", a.name);
    assert_ne!(a.type_name(), base.type_name());
    // the board sits on the inside face, after the core
    assert_eq!(a.layers.last().unwrap().material.id, "gypsum_fire");
    // more than the board limit allows: the most it can, said by its own number
    let top = assembly::rated_in(&s.rules, Use::Partition, sys, 1000);
    assert!(top.fire_minutes < 1000 && fire_boards(&top) == 8, "{} {}", top.fire_minutes, fire_boards(&top));
    // a bearing build-up follows the project's rating: REI 120 on a 90-minute floor adds board
    let hi = spec(&json!({"typology": "apartment_section", "floors": 3, "params": {"fire_rating": "REI 120"}}));
    let f = assembly::chosen(&hi, Use::IntermediateSlab);
    assert!(f.fire_minutes >= 120 && fire_boards(&f) == 2, "{} {}", f.fire_minutes, fire_boards(&f));
    assert_eq!(fire_boards(&assembly::chosen(&hi, Use::Partition)), 0, "a partition does not bear");
}

fn compartment_uses(p: &ddc_gen::project::GProject) -> Vec<(Use, u32)> {
    let mut out = vec![];
    for s in p.storeys.iter().chain(&p.basements) {
        let uses = wall_uses(s);
        for (k, w) in s.layout.walls.iter().enumerate() {
            if let Some(f) = w.fire.filter(|f| f.sep == ddc_gen::FireSep::Compartment) {
                out.push((uses[k].use_, f.minutes));
            }
        }
    }
    out
}

fn long_office(rules: J) -> J {
    json!({"typology": "office", "floors": 3,
        "rooms": [{"kind": "office", "area": 30, "count": 26}, {"kind": "wc", "area": 4, "count": 2}],
        "ground_rooms": [{"kind": "lobby", "area": 40}, {"kind": "office", "area": 30, "count": 24}],
        "norms": {"fire.compartment_area": {"value": 500, "cite": "xx fire.compartment_area_max: Test code, art. 2"}},
        "rules": rules})
}

#[test]
fn a_fire_wall_the_layers_cannot_rate_is_flagged_and_one_they_can_is_not() {
    for (want, flagged) in [(120.0, false), (240.0, true)] {
        let s = spec(&long_office(json!({"fire.wall_rating": want})));
        let p = build_project(&s).unwrap();
        let walls = compartment_uses(&p);
        assert!(!walls.is_empty(), "no fire wall");
        let short = walls.iter().any(|(u, m)| assembly::rated_in(&s.rules, *u, s.design.structure, *m).fire_minutes < *m);
        assert_eq!(short, flagged, "{walls:?}");
        let found = check(&s, &p);
        assert_eq!(found.iter().any(|f| f.rule == "fire.wall_rating"), flagged, "{found:?}");
    }
}

#[test]
fn every_structural_system_builds_its_own_walls_and_the_geometry_follows() {
    let ext = [("concrete", "ext_rc_mw"), ("timber", "ext_clt"), ("timber_frame", "ext_timber_frame"), ("masonry", "ext_brick"), ("frame", "ext_aac")];
    let mut thick = std::collections::BTreeSet::new();
    for (name, id) in ext {
        let s = spec(&json!({"typology": "apartment_section", "floors": 4, "design": {"structure": name}, "features": []}));
        let sys = s.design.structure;
        assert_eq!(assembly::for_rules_in(&s.rules, Use::ExteriorWall, sys).id, id, "{name}");
        let p = build_project(&s).unwrap_or_else(|e| panic!("{name}: {e}"));
        for u in uses_of(&p) {
            let a = assembly::chosen(&s, u);
            assert!(a.misses.is_empty(), "{name} {u:?} {}: {:?}", a.id, a.misses);
            if u.bearing() {
                assert!(a.fire_minutes >= assembly::project_rating(&s), "{name} {u:?} {}", a.id);
            }
        }
        // timber floors and roofs are CLT; the corridor wall is the concrete core, never CLT
        let timber = sys.is_timber();
        let id = |u| assembly::for_rules_in(&s.rules, u, sys).id;
        assert_eq!(id(Use::IntermediateSlab) == "floor_clt", timber, "{name}");
        assert_eq!(id(Use::FlatRoof) == "roof_flat_clt", timber, "{name}");
        assert_eq!(id(Use::CoreWall), if timber { "core_rc" } else if name == "masonry" { "core_brick" } else { "core_cs" }, "{name}");
        assert_eq!(id(Use::PartyWall) == "party_clt", timber, "{name}");
        // walls, slabs and roofs a system has no build-up for take the concrete ones
        assert_eq!(assembly::chosen(&s, Use::GroundSlab).id, "ground_rc", "{name}");
        // the plan geometry takes the thickness of the build-up the project is made of
        assembly::with_structure(sys, assembly::project_rating(&s), || {
            rules::scoped(&s.rules, || {
                for role in [WallRole::Exterior, WallRole::Party, WallRole::Corridor, WallRole::Partition] {
                    let a = assembly::chosen(&s, Use::of_role(role));
                    assert_eq!(role.thickness(), a.thickness(), "{name} {role:?}");
                }
            })
        });
        thick.insert(ddc_geom::to_mm(assembly::chosen(&s, Use::ExteriorWall).thickness()));
    }
    assert!(thick.len() >= 4, "the systems differ in their walls: {thick:?}");
}

#[test]
fn the_clt_floor_holds_the_floor_sound_rule_and_misses_a_stricter_one_without_panic() {
    let v = |rw: f64| json!({"typology": "apartment_section", "floors": 4, "design": {"structure": "timber"}, "rules": {"acoustic.rw_floor": rw}});
    let s = spec(&v(54.0));
    let a = assembly::for_rules_in(&s.rules, Use::IntermediateSlab, s.design.structure);
    assert_eq!(a.id, "floor_clt");
    let clt = a.layers.iter().find(|l| l.material.function == "structure").unwrap();
    assert_eq!(clt.mm, 240.0, "{:?}", a.layers.iter().map(|l| l.mm).collect::<Vec<_>>());
    assert!((a.rw.unwrap() - 54.4).abs() < 0.1, "{:?}", a.rw);
    assert!(a.misses.is_empty(), "{:?}", a.misses);
    // a stricter rule than the thickest panel reaches: a miss, reported as an issue
    let s = spec(&v(60.0));
    let a = assembly::for_rules_in(&s.rules, Use::IntermediateSlab, s.design.structure);
    assert!(a.misses.iter().any(|m| m.rule == "acoustic.rw_floor" && m.value < m.limit), "{:?}", a.misses);
    let p = build_project(&s).unwrap();
    let f = check(&s, &p);
    let hit = f.iter().find(|f| f.rule == "acoustic.rw_floor").unwrap_or_else(|| panic!("{f:?}"));
    assert!(hit.excess > 0.0 && !hit.hard);
}
