//! Buildings on any footprint: every shape passes the gates, rooms tile each plate without
//! overlap, every room is reached through doors, and a change on one floor keeps the others.

use ddc_gen::project::{build_project, check_project, parse_spec, GProject, ProjectSpec, RoomKind};
use ddc_gen::GLayout;
use serde_json::{json, Value as J};

fn spec(v: J) -> ProjectSpec {
    parse_spec(&v).unwrap_or_else(|e| panic!("{e}"))
}

fn built(v: J) -> (ProjectSpec, GProject) {
    let s = spec(v.clone());
    let p = build_project(&s).unwrap_or_else(|e| panic!("{v}: {e}"));
    let g = check_project(&s, &p);
    assert!(g.passed(), "{v}: {:?}", g.failures);
    (s, p)
}

fn area(r: &[(f64, f64)]) -> f64 {
    let n = r.len();
    (0..n)
        .map(|i| r[i].0 * r[(i + 1) % n].1 - r[(i + 1) % n].0 * r[i].1)
        .sum::<f64>()
        .abs()
        / 2.0
}

fn on_outline(poly: &[(f64, f64)], p: (f64, f64)) -> bool {
    let n = poly.len();
    (0..n).any(|i| {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
        (a.0 + dx * t - p.0).hypot(a.1 + dy * t - p.1) < 1e-4
    })
}

fn door_centers(l: &GLayout) -> Vec<(f64, f64)> {
    l.openings
        .iter()
        .filter(|o| o.door)
        .map(|o| {
            let w = &l.walls[o.wall];
            let (dx, dy) = (w.b.0 - w.a.0, w.b.1 - w.a.1);
            let t = (o.along + o.width / 2.0) / dx.hypot(dy);
            (w.a.0 + dx * t, w.a.1 + dy * t)
        })
        .collect()
}

/// Rooms reached through doors from the outside (ground) or from the stair room (upper).
fn reached(p: &GProject, k: usize) -> usize {
    let s = &p.storeys[k];
    let l = &s.layout;
    let doors = door_centers(l);
    let n = l.rooms.len();
    let mut seen = vec![false; n];
    let mut stack: Vec<usize> = if k == 0 {
        (0..n)
            .filter(|&i| {
                doors.iter().any(|d| {
                    on_outline(&l.rooms[i].polygon, *d)
                        && (0..n).filter(|&j| on_outline(&l.rooms[j].polygon, *d)).count() == 1
                })
            })
            .collect()
    } else {
        s.stair_room.into_iter().collect()
    };
    assert!(!stack.is_empty(), "floor {}: no way in", k + 1);
    for &i in &stack {
        seen[i] = true;
    }
    while let Some(i) = stack.pop() {
        for d in doors.iter().filter(|d| on_outline(&l.rooms[i].polygon, **d)) {
            for j in 0..n {
                if !seen[j] && on_outline(&l.rooms[j].polygon, *d) {
                    seen[j] = true;
                    stack.push(j);
                }
            }
        }
    }
    seen.iter().filter(|x| **x).count()
}

/// Rooms tile the plate: their areas add up to the outline less courtyards.
fn check_plate(s: &ProjectSpec, p: &GProject, what: &str) {
    for (k, st) in p.storeys.iter().enumerate() {
        let l = &st.layout;
        let rooms: f64 = l.rooms.iter().map(|r| area(&r.polygon)).sum();
        let plate = s.plate.as_ref().unwrap().at(k).unwrap();
        let want = area(&plate.outer) - plate.holes.iter().map(|h| area(h)).sum::<f64>();
        assert!((rooms - want).abs() < 1e-3 * want, "{what} floor {}: rooms {rooms:.2} of plate {want:.2}", k + 1);
        assert_eq!(reached(p, k), l.rooms.len(), "{what} floor {}: rooms out of reach", k + 1);
        assert!(st.rooms.iter().any(|r| r.kind == RoomKind::Corridor), "{what}: no corridor");
    }
    if s.floors > 1 {
        assert!(p.storeys.iter().all(|st| st.stair_room.is_some()), "{what}: a floor without its stair");
        let n = p.storeys.len();
        assert_eq!(p.storeys.iter().map(|st| st.layout.stairs.len()).sum::<usize>(), n - 1, "{what}");
    }
}

fn office(fp: J, floors: u32) -> J {
    json!({"typology": "office", "floors": floors, "footprint": fp})
}

#[test]
fn offices_build_on_every_shape() {
    let cases = [
        ("triangle", json!({"shape": "triangle", "width": 48, "depth": 42})),
        ("round tower", json!({"shape": "circle", "width": 34})),
        ("crescent", json!({"shape": "crescent", "width": 70, "depth": 30, "thickness": 16})),
        ("L 30 deg", json!({"shape": "l", "width": 56, "depth": 56, "angle": 30, "arm": 16})),
        ("trapezoid plot", json!({"plot": [[0, 0], [62, 0], [50, 34], [8, 30]], "plot_setback": 3})),
        ("hexagon", json!({"shape": "regular", "sides": 6, "width": 40})),
        ("courtyard", json!({"shape": "courtyard", "width": 52})),
        ("ellipse", json!({"shape": "ellipse", "width": 50, "depth": 32})),
    ];
    for (what, fp) in cases {
        let (s, p) = built(office(fp, 4));
        check_plate(&s, &p, what);
        eprintln!(
            "poly {what}: rooms {} walls {} openings {}",
            p.storeys[1].layout.rooms.len(),
            p.storeys[1].layout.walls.len(),
            p.storeys[1].layout.openings.len()
        );
    }
}

#[test]
fn apartments_build_on_a_round_tower_and_an_angled_l() {
    for fp in [
        json!({"shape": "circle", "width": 36}),
        json!({"shape": "l", "width": 60, "depth": 48, "angle": 60, "arm": 15}),
    ] {
        let (s, p) = built(json!({"typology": "apartment_section", "floors": 5, "footprint": fp}));
        check_plate(&s, &p, "apartments");
        assert!(p.apartments >= 5, "{}", p.apartments);
        for st in &p.storeys {
            for (r, m) in st.layout.rooms.iter().zip(&st.rooms) {
                if let ddc_gen::project::PRole::Apartment(a) = m.owner {
                    assert_eq!(r.apartment, Some(a));
                }
            }
        }
    }
}

#[test]
fn twisted_tapered_and_stepped_floors_keep_the_core() {
    let v = json!({
        "typology": "office", "floors": 6,
        "footprint": {"shape": "regular", "sides": 8, "width": 44},
        "design": {"twist": 3, "taper": 0.3, "levels": [{"from": 4, "setback": 2}]},
        "roof": {"kind": "hip", "slope": 25}
    });
    let (s, p) = built(v);
    check_plate(&s, &p, "twisted");
    let st: Vec<_> = p.storeys[..5].iter().map(|x| x.layout.stairs[0].origin).collect();
    assert!(st.windows(2).all(|w| w[0] == w[1]), "the stair moves: {st:?}");
    assert_eq!(p.roof.edges.len(), p.roof.ring.len());
    assert!(p.roof.edges.iter().all(Option::is_some));
}

#[test]
fn courtyard_roofs_split_round_the_court_with_gable_cuts() {
    let (_, p) = built(json!({
        "typology": "office", "floors": 3,
        "footprint": {"shape": "courtyard", "width": 54},
        "roof": {"kind": "gable"}
    }));
    assert!(!p.extra_roofs.is_empty(), "a courtyard roof comes in pieces");
    let court = p.court.as_ref().expect("the court");
    for r in std::iter::once(&p.roof).chain(&p.extra_roofs) {
        assert_eq!(r.edges.len(), r.ring.len());
        let c = court.iter().fold((0.0, 0.0), |a, q| (a.0 + q.0, a.1 + q.1));
        let c = (c.0 / court.len() as f64, c.1 / court.len() as f64);
        let n = r.ring.len();
        let crossings = (0..n)
            .filter(|&i| {
                let (a, b) = (r.ring[i], r.ring[(i + 1) % n]);
                (a.1 > c.1) != (b.1 > c.1) && c.0 < a.0 + (c.1 - a.1) * (b.0 - a.0) / (b.1 - a.1)
            })
            .count();
        assert!(crossings % 2 == 0, "a roof covers the court");
    }
}

#[test]
fn a_change_on_one_floor_keeps_the_other_floors() {
    let base = |setback: f64| {
        json!({
            "typology": "office", "floors": 6,
            "footprint": {"shape": "circle", "width": 36},
            "design": {"levels": [{"from": 5, "setback": setback}]}
        })
    };
    let (_, a) = built(base(1.0));
    let (_, b) = built(base(2.5));
    let same = a.storeys.iter().zip(&b.storeys).filter(|(x, y)| x.layout == y.layout && x.rooms == y.rooms).count();
    let elems = |p: &GProject| p.storeys.iter().map(|s| s.layout.walls.len() + s.layout.openings.len() + s.layout.rooms.len()).sum::<usize>();
    let kept: usize = a
        .storeys
        .iter()
        .zip(&b.storeys)
        .filter(|(x, y)| x.layout == y.layout)
        .map(|(x, _)| x.layout.walls.len() + x.layout.openings.len() + x.layout.rooms.len())
        .sum();
    assert_eq!(same, 5, "only the top floor changes");
    assert!(kept as f64 >= 0.8 * elems(&a) as f64, "kept {kept} of {}", elems(&a));
}

#[test]
fn bad_plates_name_the_fix() {
    let s = spec(office(json!({"shape": "crescent", "width": 60, "depth": 30, "thickness": 9}), 3));
    let e = build_project(&s).unwrap_err().to_string();
    assert!(e.contains("too thin") || e.contains("across"), "{e}");
    let e = parse_spec(&json!({
        "typology": "office",
        "footprint": {"shape": "circle"},
        "design": {"massing": "l"}
    }))
    .unwrap_err()
    .to_string();
    assert!(e.contains("design.massing"), "{e}");
    let e = parse_spec(&json!({"typology": "office", "design": {"twist": 2}})).unwrap_err().to_string();
    assert!(e.contains("design.twist"), "{e}");
}
