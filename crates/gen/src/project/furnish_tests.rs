//! The fit-out rules against the library, and fitted-out projects against the audit.

use super::fit_audit::{audit, connections};
use super::fit_rules::{rules_for, Placed, Rules};
use super::{build_project, parse_spec, GProject, ProjectSpec};
use crate::GFixture;
use ddc_geom::component::{Family, Lod};
use ddc_geom::mep::{Domain, SystemKind};
use serde_json::{json, Value as J};

const LIBRARY: &str = include_str!("../../../model/data/library.ddcf");

fn library() -> Vec<Family> {
    LIBRARY
        .split("\n---")
        .filter(|s| s.lines().any(|l| l.trim_start().starts_with("family ")))
        .map(|s| Family::parse(s.trim_start_matches('\n')).unwrap_or_else(|e| panic!("{e}")))
        .collect()
}

#[test]
fn every_rule_item_is_a_library_type_of_its_size_class_and_drain() {
    let lib = library();
    let rules = rules_for(None).unwrap_or_else(|e| panic!("{e:?}"));
    for it in &rules.items {
        let fam = lib.iter().find(|f| f.name == it.fam).unwrap_or_else(|| panic!("{}: no family {}", it.id, it.fam));
        let ty = fam.family_type(it.ty).unwrap_or_else(|| panic!("{}: no type {} {}", it.id, it.fam, it.ty));
        let r = fam.resolve(&ty.values, &[]).unwrap_or_else(|e| panic!("{}: {e}", it.id));
        let b = fam.build(&r, Lod::Fine).unwrap_or_else(|e| panic!("{}: {e}", it.id)).bbox;
        // the body in the family frame: the fixture at the origin, unturned
        let at0 = GFixture { scale: 1.0, min_clear: 0.0, family: it.fam, ty: it.ty, at: (0.0, 0.0), rotation: 0.0, wall: None, z: 0.0 };
        let body = Placed::of(it, &at0).body;
        let bb = [b[0].x, b[0].y, b[1].x, b[1].y];
        for k in 0..4 {
            let out = if k < 2 { body[k] - bb[k] } else { bb[k] - body[k] };
            assert!(out <= 0.005, "{}: family box {bb:?} outside the body {body:?}", it.id);
            assert!(-out <= 0.1, "{}: body {body:?} more than 0.1 m off the family box {bb:?}", it.id);
        }
        assert!(fam.ifc.0.eq_ignore_ascii_case(&it.ifc), "{}: {} in the library, {} in the rules", it.id, fam.ifc.0, it.ifc);
        if it.drain.is_some() {
            let ports = fam.ports(&r).unwrap_or_else(|e| panic!("{}: {e}", it.id));
            assert!(
                ports.iter().any(|p| p.domain == Domain::Pipe && p.system == Some(SystemKind::Sanitary)),
                "{}: drained but no sanitary connector",
                it.id
            );
        }
    }
    // the classes the exchange needs
    let class = |id: &str| rules.items[rules.item_ix(id).unwrap()].ifc.to_ascii_lowercase();
    for id in ["wc", "wc_acc", "basin", "basin_acc", "urinal", "tub", "shower", "sink"] {
        assert_eq!(class(id), "ifcsanitaryterminal", "{id}");
    }
    for id in ["bed2", "bed1", "bedside", "wardrobe", "sofa", "armchair", "table", "chair", "desk", "school_desk"] {
        assert_eq!(class(id), "ifcfurniture", "{id}");
    }
    assert_eq!(class("floor_drain"), "ifcwasteterminal");
    assert_eq!(class("grab_rail"), "ifcrailing");
}

fn project(v: &J) -> (ProjectSpec, GProject) {
    // no wall-time cap on the layout search: the plan, and so the fit-out, must not depend on load
    let mut pinned = v.clone();
    pinned["rules"]["layout.search_ms"] = json!(0);
    let s = parse_spec(&pinned).unwrap_or_else(|e| panic!("{v}: {e}"));
    let p = build_project(&s).unwrap_or_else(|e| panic!("{v}: {e}"));
    (s, p)
}

fn audited(v: &J, rules: &Rules) -> GProject {
    let (s, p) = project(v);
    let mut bad = vec![];
    for (k, st) in p.storeys.iter().enumerate() {
        bad.extend(audit(st, s.typology, rules, &s.room_tags).into_iter().map(|e| format!("floor {k}: {e}")));
    }
    assert!(bad.is_empty(), "{v}:\n{}", bad.join("\n"));
    p
}

fn briefs() -> Vec<J> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let mut out: Vec<J> = ["apartment_block", "hotel", "house", "mixed_use", "office", "school"]
        .iter()
        .map(|n| {
            let t = std::fs::read_to_string(dir.join(format!("{n}.json"))).unwrap_or_else(|e| panic!("{n}: {e}"));
            serde_json::from_str(&t).unwrap_or_else(|e| panic!("{n}: {e}"))
        })
        .collect();
    // full-depth rooms (wide enough for the band) and two small WCs sharing one strip
    out.push(json!({"typology": "office", "floors": 1, "rooms": [
        {"kind": "bedroom", "area": 20, "count": 3, "name": "Hotel room"},
        {"kind": "bath", "area": 20, "name": "Accessible bath"},
        {"kind": "wc", "area": 6, "count": 2}, {"kind": "wc", "area": 20},
        {"kind": "kitchen", "area": 14}, {"kind": "ward", "area": 22, "count": 2},
        {"kind": "classroom", "area": 50}, {"kind": "meeting", "area": 24}]}));
    out.push(json!({"typology": "office", "floors": 1, "footprint": {"width": 40, "depth": 30},
        "rooms": [{"kind": "parking", "area": 800}, {"kind": "storage", "area": 60}]}));
    out
}

#[test]
fn every_brief_is_fitted_out_without_collisions_and_with_every_clearance() {
    let rules = rules_for(None).unwrap();
    let mut rules_seen = std::collections::BTreeSet::new();
    for v in briefs() {
        let p = audited(&v, &rules);
        for st in &p.storeys {
            for f in &st.fit {
                rules_seen.insert(f.rule.clone());
            }
        }
    }
    for id in ["hotel_room", "bath_accessible", "wc_accessible", "wc_block", "kitchen", "ward", "classroom", "parking", "guest_room"] {
        assert!(rules_seen.contains(id), "rule {id} never used: {rules_seen:?}");
    }
}

#[test]
fn accessible_rooms_keep_a_turning_circle_and_drains_reach_the_stack() {
    let rules = rules_for(None).unwrap();
    let v = &briefs()[6];
    let p = audited(v, &rules);
    let fits: Vec<_> = p.storeys.iter().flat_map(|s| &s.fit).collect();
    let acc: Vec<_> = fits.iter().filter(|f| f.rule == "bath_accessible" || f.rule == "wc_accessible").collect();
    assert!(acc.len() >= 3, "{acc:?}");
    for f in &acc {
        assert!(f.turning.iter().any(|t| (t.1 - 1.5).abs() < 1e-9), "{f:?}");
        assert!(f.stack.is_some() && !f.drains.is_empty(), "{f:?}");
    }
    let bath = acc.iter().find(|f| f.rule == "bath_accessible").unwrap();
    assert!(!bath.falls.is_empty() && bath.falls.iter().all(|x| *x <= rules.drainage.max_floor_drop + 1e-9), "{bath:?}");
    let kitchen = fits.iter().find(|f| f.rule == "kitchen").unwrap();
    let legs = kitchen.triangle.unwrap_or_else(|| panic!("{kitchen:?}"));
    assert!(super::furnish::triangle_ok(&legs, (1.2, 2.7), (4.0, 7.9)), "{legs:?}");
}

#[test]
fn a_brief_patch_changes_the_fit_out() {
    let mut v = json!({"typology": "office", "floors": 1, "rooms": [
        {"kind": "wc", "area": 3, "count": 2}, {"kind": "bedroom", "area": 14}]});
    let (_, before) = project(&v);
    let has = |p: &GProject, fam: &str| p.storeys.iter().any(|s| s.layout.fixtures.iter().any(|f| f.family == fam));
    assert!(has(&before, "Washbasin") && has(&before, "Bed"));
    v["furnish"] = json!({"rooms": [{"id": "wc_only", "kind": "wc", "steps": [{"place": "wc", "need": 1}]}]});
    let (s, after) = project(&v);
    let rules = rules_for(s.furnish.as_ref()).unwrap();
    assert!(!has(&after, "Washbasin") && !has(&after, "Bed") && has(&after, "WC"));
    for st in &after.storeys {
        assert!(st.fit.iter().all(|f| f.rule == "wc_only"));
        assert!(audit(st, s.typology, &rules, &s.room_tags).is_empty());
    }
}

/// Fit-out follows the spec's canonical tags, not the room's name: rooms named in Arabic,
/// Japanese or Hindi with tags take the accessible and guest rule sets; a name hint never
/// overrides tags the spec gave.
#[test]
fn tags_pick_the_rule_set_whatever_the_name() {
    let rules = rules_for(None).unwrap();
    let rule_of = |v: &J| -> std::collections::BTreeSet<String> {
        audited(v, &rules).storeys.iter().flat_map(|s| s.fit.iter().map(|f| f.rule.clone())).collect()
    };
    let rooms = |bed: &str, bath: &str, tags: bool| {
        let t = |x: &str| if tags { json!([x]) } else { J::Null };
        json!({"typology": "office", "floors": 1, "rooms": [
            {"kind": "bedroom", "area": 20, "count": 2, "name": bed, "tags": t("guest")},
            {"kind": "bath", "area": 20, "name": bath, "tags": t("accessible")},
            {"kind": "kitchen", "area": 14}]})
    };
    for (bed, bath) in [("غرفة ضيوف", "حمام"), ("客室", "浴室"), ("अतिथि कक्ष", "स्नानघर"), ("Room", "Bath")] {
        let got = rule_of(&rooms(bed, bath, true));
        assert!(got.contains("hotel_room") && got.contains("bath_accessible"), "{bed}: {got:?}");
        let plain = rule_of(&rooms(bed, bath, false));
        assert!(!plain.contains("hotel_room") && !plain.contains("bath_accessible"), "{bed}: {plain:?}");
    }
    // the English and Russian words stay a hint for a room the spec gave no tags
    let hinted = rule_of(&rooms("Hotel room", "Accessible bath", false));
    assert!(hinted.contains("hotel_room") && hinted.contains("bath_accessible"), "{hinted:?}");
    assert!(parse_spec(&json!({"typology": "house", "rooms": [{"kind": "bath", "area": 5, "tags": ["wide"]}]})).is_err());
}

fn fam(rules: &Rules, id: &str) -> &'static str {
    rules.items[rules.item_ix(id).unwrap_or_else(|| panic!("no item {id}"))].fam
}

fn n(fx: &[&GFixture], family: &str) -> usize {
    fx.iter().filter(|f| f.family == family).count()
}

fn metric(p: &GProject, k: &str) -> f64 {
    p.metrics.iter().find(|m| m.0 == k).map_or(-1.0, |m| m.1)
}

/// The rooms one rule fitted out: (room area, the fixtures within the room).
fn rooms_by<'a>(p: &'a GProject, rule: &str) -> Vec<(f64, Vec<&'a GFixture>)> {
    let mut out = vec![];
    for st in &p.storeys {
        for f in st.fit.iter().filter(|f| f.rule == rule) {
            let poly = &st.layout.rooms[f.room].polygon;
            let (xs, ys): (Vec<f64>, Vec<f64>) = poly.iter().copied().unzip();
            let lo = (xs.iter().copied().fold(f64::MAX, f64::min) - 1e-6, ys.iter().copied().fold(f64::MAX, f64::min) - 1e-6);
            let hi = (xs.iter().copied().fold(f64::MIN, f64::max) + 1e-6, ys.iter().copied().fold(f64::MIN, f64::max) + 1e-6);
            let area = poly.iter().zip(poly.iter().cycle().skip(1)).map(|(a, b)| a.0 * b.1 - b.0 * a.1).sum::<f64>().abs() / 2.0;
            let fx = st.layout.fixtures.iter().filter(|x| x.at.0 >= lo.0 && x.at.0 <= hi.0 && x.at.1 >= lo.1 && x.at.1 <= hi.1).collect();
            out.push((area, fx));
        }
    }
    out
}

fn rules_used(p: &GProject) -> std::collections::BTreeSet<String> {
    p.storeys.iter().flat_map(|s| s.fit.iter().map(|f| f.rule.clone())).collect()
}

/// A whole apartment section with every service: furnished without a collision, every water
/// connector on its riser, every cooker hood ducted to the kitchen's extract.
#[test]
fn a_whole_apartment_is_furnished_plumbed_and_ducted() {
    let rules = rules_for(None).unwrap();
    let p = audited(&json!({"typology": "apartment_section", "floors": 2, "services": true}), &rules);
    let open = connections(&p);
    assert!(open.is_empty(), "{}", open.join("\n"));
    assert_eq!(metric(&p, "water_unserved"), 0.0);
    let kitchens = rooms_by(&p, "kitchen");
    assert!(!kitchens.is_empty(), "{:?}", rules_used(&p));
    for (_, fx) in &kitchens {
        assert!(n(fx, fam(&rules, "cooker")) >= 1);
        assert_eq!(n(fx, fam(&rules, "hood")), n(fx, fam(&rules, "cooker")), "a hood over every cooker");
    }
    assert!(kitchens.iter().any(|(_, fx)| n(fx, fam(&rules, "wall_cab_600")) >= 1), "wall cabinets over the counters");
    let hoods: usize = p.storeys.iter().map(|s| n(&s.layout.fixtures.iter().collect::<Vec<_>>(), fam(&rules, "hood"))).sum();
    if !p.hvac.ducts.is_empty() {
        assert_eq!(p.hvac.links.iter().filter(|l| l.connector == "Exhaust").count(), hoods);
    }
    let all: Vec<&GFixture> = p.storeys.iter().flat_map(|s| &s.layout.fixtures).collect();
    for id in ["mirror", "towel_rail", "paper_holder", "coat_rack"] {
        assert!(n(&all, fam(&rules, id)) >= 1, "{id}");
    }
    assert!(n(&all, fam(&rules, "tv_unit")) + n(&all, fam(&rules, "coffee_table")) >= 1);
    assert_eq!(n(&all, fam(&rules, "heater")), 0, "central hot water in a block of flats");
}

#[test]
fn a_house_gets_one_water_heater_per_dwelling_and_the_rule_turns_it_off() {
    let rules = rules_for(None).unwrap();
    let heaters = |p: &GProject| p.storeys.iter().map(|s| n(&s.layout.fixtures.iter().collect::<Vec<_>>(), fam(&rules, "heater"))).sum::<usize>();
    let p = audited(&json!({"typology": "house", "floors": 2, "services": true}), &rules);
    assert_eq!(heaters(&p), 1);
    assert!(connections(&p).is_empty(), "{:?}", connections(&p));
    let off = audited(&json!({"typology": "house", "floors": 2, "rules": {"fit.heater.local": false}}), &rules);
    assert_eq!(heaters(&off), 0);
}

#[test]
fn baths_and_wcs_get_their_accessories_and_the_tag_picks_the_accessible_wc() {
    let rules = rules_for(None).unwrap();
    let p = audited(&json!({"typology": "office", "floors": 1, "rooms": [
        {"kind": "wc", "area": 6, "name": "WC", "tags": ["accessible"]},
        {"kind": "wc", "area": 3, "count": 2}, {"kind": "bath", "area": 8}]}), &rules);
    let used = rules_used(&p);
    let acc: Vec<_> = ["wc_accessible", "wc_accessible_tag"].iter().flat_map(|r| rooms_by(&p, r)).collect();
    assert!(!acc.is_empty(), "{used:?}");
    for (_, fx) in &acc {
        for id in ["grab_rail", "paper_holder", "mirror_l"] {
            assert!(n(fx, fam(&rules, id)) >= 1, "{id}");
        }
    }
    for (_, fx) in rooms_by(&p, "wc") {
        assert_eq!(n(&fx, fam(&rules, "paper_holder")), n(&fx, fam(&rules, "wc")));
    }
}

#[test]
fn living_rooms_and_halls_are_furnished() {
    let rules = rules_for(None).unwrap();
    let p = audited(&json!({"typology": "house", "floors": 1}), &rules);
    let living = rooms_by(&p, "living");
    assert!(!living.is_empty(), "{:?}", rules_used(&p));
    for (_, fx) in &living {
        assert!(n(fx, fam(&rules, "sofa")) >= 1);
    }
    let hall: Vec<&GFixture> = rooms_by(&p, "hall").into_iter().flat_map(|r| r.1).collect();
    assert!(n(&hall, fam(&rules, "coat_rack")) + n(&hall, fam(&rules, "shoe_cab")) + n(&hall, fam(&rules, "wardrobe")) >= 1);
}

#[test]
fn hotel_rooms_get_a_bed_desk_wardrobe_and_tv() {
    let rules = rules_for(None).unwrap();
    let p = audited(&json!({"typology": "office", "floors": 1, "rooms": [
        {"kind": "bedroom", "area": 22, "count": 3, "tags": ["guest"]}]}), &rules);
    let rooms = rooms_by(&p, "hotel_room");
    assert_eq!(rooms.len(), 3, "{:?}", rules_used(&p));
    for (_, fx) in &rooms {
        assert!(n(fx, "Bed") >= 1 && n(fx, fam(&rules, "desk")) >= 1 && n(fx, fam(&rules, "wardrobe")) >= 1);
    }
    assert!(rooms.iter().any(|(_, fx)| n(fx, fam(&rules, "tv_unit")) >= 1));
}

#[test]
fn office_desks_follow_the_occupancy_rule() {
    let rules = rules_for(None).unwrap();
    let v = json!({"typology": "office", "floors": 1, "rooms": [{"kind": "office", "area": 60}]});
    let desks = |p: &GProject| rooms_by(p, "office").iter().map(|(a, fx)| (*a, n(fx, fam(&rules, "desk")))).collect::<Vec<_>>();
    let base = desks(&audited(&v, &rules));
    let per = super::rules::def("fit.office.area_per_desk").and_then(|d| d.default).unwrap();
    assert!(!base.is_empty());
    for (a, k) in &base {
        assert!(*k >= 1 && *k as f64 <= (a / per).floor() + 1e-9, "{k} desks in {a} m2");
    }
    let mut sparse = v.clone();
    sparse["rules"] = json!({"fit.office.area_per_desk": 20.0});
    let fewer = desks(&audited(&sparse, &rules));
    let total = |d: &[(f64, usize)]| d.iter().map(|x| x.1).sum::<usize>();
    assert!(total(&fewer) < total(&base), "{fewer:?} vs {base:?}");
}

#[test]
fn classroom_pairs_face_the_board_within_the_occupancy() {
    let rules = rules_for(None).unwrap();
    let p = audited(&json!({"typology": "school", "floors": 1, "rooms": [{"kind": "classroom", "area": 56}]}), &rules);
    let per = super::rules::def("fit.classroom.area_per_pupil").and_then(|d| d.default).unwrap();
    let rooms = rooms_by(&p, "classroom");
    assert!(!rooms.is_empty(), "{:?}", rules_used(&p));
    for (a, fx) in &rooms {
        let desks: Vec<_> = fx.iter().filter(|f| f.family == fam(&rules, "school_desk") && f.ty == rules.items[rules.item_ix("school_desk").unwrap()].ty).collect();
        assert!(!desks.is_empty() && desks.len() as f64 <= (a / per / 2.0).floor() + 1e-9, "{} pairs in {a} m2", desks.len());
        let board = fx.iter().find(|f| f.family == fam(&rules, "board")).expect("a board");
        // from each chair the pupil looks over the desk to the board
        for c in fx.iter().filter(|f| f.family == fam(&rules, "chair")) {
            let d2 = |f: &GFixture| (f.at.0 - c.at.0).hypot(f.at.1 - c.at.1);
            // the teacher's chair sits by no pupil desk
            let Some(&&d) = desks.iter().min_by(|x, y| d2(***x).total_cmp(&d2(***y))) else { continue };
            if d2(d) >= 1.0 {
                continue;
            }
            let to_desk = (d.at.0 - c.at.0, d.at.1 - c.at.1);
            let to_board = (board.at.0 - c.at.0, board.at.1 - c.at.1);
            assert!(to_desk.0 * to_board.0 + to_desk.1 * to_board.1 > 0.0, "chair at {:?} turned from the board", c.at);
        }
    }
}

#[test]
fn a_clinic_room_gets_the_treatment_layout_by_tag_or_name() {
    let rules = rules_for(None).unwrap();
    for room in [json!({"kind": "office", "area": 18, "name": "Consulting room"}), json!({"kind": "office", "area": 18, "name": "診察室", "tags": ["clinical"]})] {
        let p = audited(&json!({"typology": "office", "floors": 1, "rooms": [room]}), &rules);
        let rooms = rooms_by(&p, "treatment");
        assert_eq!(rooms.len(), 1, "{:?}", rules_used(&p));
        let fx = &rooms[0].1;
        for id in ["couch", "desk", "basin"] {
            assert!(n(fx, fam(&rules, id)) >= 1, "{id}");
        }
        assert!(n(fx, fam(&rules, "chair")) >= 2, "doctor and patient");
    }
}

#[test]
fn a_technical_room_gets_a_floor_drain_with_a_fall() {
    let rules = rules_for(None).unwrap();
    let p = audited(&json!({"typology": "office", "floors": 1, "rooms": [{"kind": "technical", "area": 8}]}), &rules);
    let tech: Vec<_> = p.storeys.iter().flat_map(|s| &s.fit).filter(|f| f.rule == "technical").collect();
    assert!(!tech.is_empty(), "{:?}", rules_used(&p));
    for f in tech {
        assert!(!f.drains.is_empty() && !f.falls.is_empty(), "{f:?}");
    }
}

/// Every fit-out number is a rule with a reason, and the rules file reads each one.
#[test]
fn every_fit_rule_has_a_reason_and_is_read() {
    let text = include_str!("../../data/furnish_rules.json");
    let fit: Vec<_> = super::rules::defs().iter().filter(|d| d.group == "fit").collect();
    assert!(fit.len() >= 100, "{}", fit.len());
    for d in fit {
        assert!(!d.why.trim().is_empty(), "{} has no why", d.id);
        assert!(text.contains(&format!("\"{}\"", d.id)), "{} is never read", d.id);
    }
}

#[test]
fn a_broken_patch_is_a_spec_issue_at_its_path() {
    let issues = |p: J| match parse_spec(&json!({"typology": "house", "furnish": p})) {
        Ok(_) => vec![],
        Err(e) => e.0.into_iter().map(|i| i.path).collect::<Vec<_>>(),
    };
    assert_eq!(issues(json!(5)), ["furnish"]);
    assert!(issues(json!({"items": {"wc": {"body": [-1, 0.5]}}})).contains(&"furnish.items.wc.body[0]".to_string()));
    let e = issues(json!({"rooms": [{"id": "a", "kind": "wc", "steps": [{"place": "nothing"}]}]}));
    assert!(e.iter().any(|p| p.starts_with("furnish.rooms[0].steps[0]")), "{e:?}");
}

#[test]
fn smoke_lobbies_pass_the_fit_audit_and_stay_empty() {
    let (s, p) = project(&json!({"typology": "office", "floors": 4, "basements": 1,
        "rules": {"egress.protected_stairs": "smokeproof"}}));
    let rules = rules_for(s.furnish.as_ref()).unwrap();
    assert!(!super::fire::smoke_lobbies(&p).is_empty());
    for (k, st) in p.storeys.iter().chain(&p.basements).enumerate() {
        let bad = audit(st, s.typology, &rules, &s.room_tags);
        assert!(bad.is_empty(), "level {k}: {bad:?}");
    }
}
