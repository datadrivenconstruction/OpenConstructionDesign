//! Plan checks as rules: hand plans with known escape distances, and every rule passing on
//! the defaults and failing when tightened, its finding naming the rule.

use ddc_gen::project::nav::{check, exit_distances, fixes, penalty, strip_width, Finding};
use ddc_gen::project::rules::{self, Layer};
use ddc_gen::project::{build_project, check_project, parse_spec, GProject, PRole, PRoom, PStorey, ProjectSpec, RoomKind};
use ddc_gen::{GLayout, GOpening, GRoom, GWall, WallRole};
use serde_json::{json, Map, Value as J};

type P = (f64, f64);

fn built(v: &J) -> (ProjectSpec, GProject) {
    let s = parse_spec(v).unwrap_or_else(|e| panic!("{v}: {e}"));
    let p = build_project(&s).unwrap_or_else(|e| panic!("{v}: {e}"));
    (s, p)
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<P> {
    vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
}

fn wall(a: P, b: P, role: WallRole) -> GWall {
    GWall::line(a, b, role)
}

fn door(wall: usize, along: f64) -> GOpening {
    GOpening { wall, along, door: true, width: 1.0, height: 2.1, sill: 0.0, ty: None }
}

/// A corridor 20 x 2 m, office A 5 x 6 m off it and office C behind A. On the ground floor
/// the corridor's east end is an outside door; above, a stair room stands there.
fn plan(upper: bool) -> PStorey {
    let mut rooms = vec![
        (RoomKind::Corridor, "01", rect(0.0, 0.0, 20.0, 2.0)),
        (RoomKind::Office, "02", rect(0.0, 2.0, 5.0, 8.0)),
        (RoomKind::Office, "03", rect(0.0, 8.0, 5.0, 12.0)),
    ];
    let end = if upper { WallRole::Partition } else { WallRole::Exterior };
    if upper {
        rooms.push((RoomKind::Stair, "04", rect(20.0, 0.0, 24.0, 4.0)));
    }
    let layout = GLayout {
        walls: vec![
            wall((20.0, 0.0), (20.0, 2.0), end),
            wall((0.0, 2.0), (5.0, 2.0), WallRole::Partition),
            wall((0.0, 8.0), (5.0, 8.0), WallRole::Partition),
        ],
        openings: vec![door(0, 0.5), door(1, 2.0), door(2, 2.0)],
        rooms: rooms
            .iter()
            .map(|r| GRoom { name: r.1.to_string(), apartment: None, point: (0.0, 0.0), polygon: r.2.clone() })
            .collect(),
        ..GLayout::default()
    };
    PStorey {
        name: "L".into(),
        elevation: 0.0,
        height: 3.0,
        layout,
        rooms: rooms
            .iter()
            .map(|r| PRoom { kind: r.0, number: r.1.to_string(), owner: PRole::Public, asked: None, grown: false })
            .collect(),
        stair_room: upper.then_some(3),
        fit: vec![],
        curtains: vec![],
        railings: vec![],
    }
}

fn near(x: Option<f64>, want: f64) -> bool {
    x.is_some_and(|x| (x - want).abs() <= 0.01 * want)
}

#[test]
fn escape_distances_of_a_hand_plan_are_exact() {
    // corridor leg from office A's door (2.5, 2) to the exit door (20, 1)
    let leg = (17.5f64 * 17.5 + 1.0).sqrt();
    let want = [
        (20.0f64 * 20.0 + 1.0).sqrt(),
        6.5 + leg,
        (2.5f64 * 2.5 + 4.0 * 4.0).sqrt() + 6.0 + leg,
    ];
    for (upper, k) in [(false, 0), (true, 1)] {
        let d = exit_distances(&plan(upper), k);
        for (i, w) in want.iter().enumerate() {
            assert!(near(d[i], *w), "upper {upper}: room {i} {:?}, want {w:.3}", d[i]);
        }
        if upper {
            assert_eq!(d[3], None, "the stair itself is the way out");
        }
    }
    // a ground floor without an outside door has no way out to measure
    let d = exit_distances(&plan(true), 0);
    assert!(d.iter().all(Option::is_none), "{d:?}");
}

#[test]
fn route_width_reads_a_strip() {
    assert!((strip_width(&rect(0.0, 0.0, 20.0, 2.0)) - 2.0).abs() < 1e-9);
    let l = [(0.0, 0.0), (10.0, 0.0), (10.0, 1.5), (1.5, 1.5), (1.5, 8.0), (0.0, 8.0)];
    assert!((strip_width(&l) - 1.5).abs() < 1e-9, "{}", strip_width(&l));
}

fn office(rules: J) -> J {
    json!({"typology": "office", "floors": 2,
           "rooms": [{"kind": "office", "area": 30, "count": 10}, {"kind": "wc", "area": 4, "count": 2}],
           "rules": rules})
}

fn one(rule: &str, v: J) -> J {
    let mut m = Map::new();
    m.insert(rule.to_string(), v);
    J::Object(m)
}

fn find<'a>(f: &'a [Finding], rule: &str) -> Option<&'a Finding> {
    f.iter().find(|x| x.rule == rule)
}

const CHECKED: [&str; 9] = [
    "light.depth_ratio",
    "egress.max_travel",
    "access.route_width",
    "access.door_width",
    "access.turning",
    "fire.compartment_area",
    "mep.riser_offset",
    "core.stack",
    "grid.transfer",
];

#[test]
fn every_check_passes_the_defaults_and_fails_a_tightened_rule() {
    let (s, p) = built(&office(json!({})));
    let base = check(&s, &p);
    for rule in CHECKED {
        assert!(find(&base, rule).is_none(), "{rule} breaks on the defaults: {base:?}");
    }
    assert!(check_project(&s, &p).passed());
    let tight = [
        ("light.depth_ratio", json!(1.5)),
        ("egress.max_travel", json!(15)),
        ("access.route_width", json!(2.4)),
        ("access.door_width", json!(1.0)),
        ("access.turning", json!(2.0)),
        ("fire.compartment_area", json!(200)),
    ];
    for (rule, v) in tight {
        // the generator's own cure off (more stairs, fire walls), so the check has work to do
        let mut r = one(rule, v.clone());
        r["egress.stairs_max"] = json!(1);
        r["fire.walls"] = json!(false);
        let (s, p) = built(&office(r));
        let f = check(&s, &p);
        let x = find(&f, rule).unwrap_or_else(|| panic!("{rule} = {v} found nothing: {f:?}"));
        assert!(x.line().contains(&format!(": {rule}: ")), "{}", x.line());
        assert!(x.excess > 0.0 && !x.hard && x.weight > 0.0, "{x:?}");
        if let Some(fix) = &x.fix {
            let d = rules::def(&fix.rule).unwrap();
            assert!(d.parse_value(&fix.value).is_some(), "{rule}: fix {fix:?} outside {}", d.range());
            assert!(!fix.why.is_empty());
        }
        // soft: advice beside the draft, named by its rule
        let g = check_project(&s, &p);
        assert!(g.passed(), "{rule}: {:?}", g.failures);
        assert!(g.warnings.iter().any(|w| w.contains(rule)), "{rule}: {:?}", g.warnings);
        assert!(penalty(&f) > 0.0);
    }
    // the fixes point at the rules that cure them, not at the check itself
    let (s, p) = built(&office(json!({"light.depth_ratio": 1.5, "access.door_width": 1.0})));
    let ids: Vec<String> = fixes(&check(&s, &p)).into_iter().map(|f| f.rule).collect();
    assert!(ids.contains(&"depth.band".to_string()), "{ids:?}");
    assert!(ids.contains(&"room.door_wet".to_string()) || ids.contains(&"room.door".to_string()), "{ids:?}");
}

#[test]
fn a_hard_rule_fails_the_project_with_its_name() {
    let (s, p) = built(&office(json!({"egress.max_travel": {"value": 15, "hard": true}, "egress.stairs_max": 1})));
    assert!(s.rules.hard("egress.max_travel"));
    let g = check_project(&s, &p);
    assert!(g.failures.iter().any(|x| x.contains("egress.max_travel")), "{:?}", g.failures);
    let mut s2 = s.clone();
    s2.rules.set_goal(Layer::Brief, "egress.max_travel", None, Some(false)).unwrap();
    assert!(check_project(&s2, &p).passed());
}

#[test]
fn the_glazing_gate_reads_its_rule() {
    let (s, p) = built(&office(json!({"light.glazing_floor": 0.5})));
    let g = check_project(&s, &p);
    assert!(g.warnings.iter().any(|w| w.contains("light.glazing_floor") && w.contains("glazing")), "{:?}", g.warnings);
    let (s, p) = built(&office(json!({})));
    assert!(!check_project(&s, &p).warnings.iter().any(|w| w.contains("light.glazing_floor")));
}

#[test]
fn a_norm_value_cites_its_clause_until_the_brief_overrides_it() {
    let cite = "xx egress.travel_max: Test code, par. 9(1), unverified";
    // one stair: the generator does not cure the travel with more
    let mut v = office(json!({"egress.stairs_max": 1}));
    v["norms"] = json!({"egress.max_travel": {"value": 15, "cite": cite}, "light.glazing_floor": {"value": 0.5, "cite": "xx light"}});
    let (s, p) = built(&v);
    assert_eq!((s.rules.get("egress.max_travel"), s.rules.source("egress.max_travel")), (15.0, Some(Layer::Norm)));
    assert_eq!(s.rules.cite("egress.max_travel"), Some(cite));
    let f = check(&s, &p);
    let x = find(&f, "egress.max_travel").unwrap_or_else(|| panic!("{f:?}"));
    assert!(x.line().contains(&format!("[norm {cite}]")), "{}", x.line());
    let g = check_project(&s, &p);
    assert!(g.warnings.iter().any(|w| w.contains("light.glazing_floor") && w.contains("[norm xx light]")), "{:?}", g.warnings);
    // the brief sits above the norm: its value wins and no clause is claimed for it
    v["rules"] = json!({"egress.max_travel": 16, "egress.stairs_max": 1});
    let (s, p) = built(&v);
    assert_eq!((s.rules.get("egress.max_travel"), s.rules.cite("egress.max_travel")), (16.0, None));
    assert!(find(&check(&s, &p), "egress.max_travel").is_some_and(|x| x.cite.is_none()));
    // a norm number outside the rule's range is refused with its path
    let mut bad = office(json!({}));
    bad["norms"] = json!({"egress.max_travel": {"value": 500, "cite": cite}});
    assert!(parse_spec(&bad).unwrap_err().to_string().contains("norms.egress.max_travel"));
}

#[test]
fn every_norm_key_names_a_numeric_rule() {
    let keyed: Vec<&str> = rules::defs().iter().filter(|d| !d.norm.is_empty()).map(|d| d.id.as_str()).collect();
    for id in ["egress.max_travel", "light.glazing_floor", "light.depth_ratio", "access.route_width", "access.door_width", "access.turning", "fire.compartment_area", "mep.riser_offset"] {
        assert!(keyed.contains(&id), "{id} has no norm key");
    }
}

#[test]
fn the_check_rules_explain_themselves() {
    for id in CHECKED.iter().chain(&["light.glazing_floor", "mep.wet_stack_fill"]) {
        let d = rules::def(id).unwrap_or_else(|| panic!("{id} is not in rules.json"));
        assert!(d.default.is_some(), "{id}");
        assert!(d.why.len() > 20, "{id}: why '{}'", d.why);
    }
}
