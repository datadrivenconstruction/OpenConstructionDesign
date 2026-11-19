//! Mechanical design over the ventilation: room loads, the system the rules choose, VAV boxes
//! on every supply branch, ducts sized within their caps with real fittings, the pressure of
//! every path and the fans on the critical one, the water side, and no clash with the structure.

use ddc_gen::project::mech::{self, ducts::FitKind, equip::Kind, select, Beam, Mech};
use ddc_gen::project::rules::{norms_from, rule};
use ddc_gen::project::{build_project, check_project, parse_spec, GProject, ProjectSpec, RoomKind};
use ddc_geom::mep::duct_sizing::{dynamic, friction_rect, friction_round, pick_round, velocity, ROUND_MM};
use ddc_geom::mep::fittings::elbow_zeta;
use ddc_model::checks::norm_pack::PackSet;
use serde_json::{json, Value as J};

fn office(system: &str) -> J {
    json!({"typology": "office", "floors": 3, "floor_height": 3.6, "rules": {"mep.hvac.system": system}})
}

fn built(v: J) -> (ProjectSpec, GProject) {
    let s = parse_spec(&v).unwrap_or_else(|e| panic!("{e}"));
    let p = build_project(&s).unwrap_or_else(|e| panic!("{e}"));
    let g = check_project(&s, &p);
    assert!(g.passed(), "{:?}", g.failures);
    (s, p)
}

fn designed(v: J) -> (ProjectSpec, GProject, Mech) {
    let (s, p) = built(v);
    let m = mech::design(&s, &p).expect("the rules ask for the design");
    (s, p, m)
}

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

#[test]
fn an_office_gets_loads_and_a_vav_system() {
    let (s0, p0) = built(office("off"));
    assert!(mech::design(&s0, &p0).is_none(), "off designs nothing");
    let (s, p, m) = designed(office("vav"));
    assert_eq!(m.choice.air, select::AirSide::Vav);
    // loads: every office room loses heat, gains heat, needs outdoor air by its people
    let offices: Vec<_> = m.loads.iter().filter(|l| l.kind == RoomKind::Office).collect();
    assert!(offices.len() >= 3, "{} offices", offices.len());
    for l in &offices {
        assert!(l.heating() > 0.0 && l.sensible() > 0.0, "{l:?}");
        let people = (l.area / rule("mep.hvac.occ_office")).ceil();
        assert_eq!(l.people, people);
        assert!(close(l.oa, rule("mep.hvac.oa_person") * people + rule("mep.hvac.oa_area") * l.area, 1e-9));
        assert!(close(l.cool[4], people * rule("mep.hvac.person_sens"), 1e-9));
        if l.facades.iter().any(|f| f.glass > 0.0) {
            assert!(l.cool[2] > 0.0 && l.heat[1] > 0.0, "glazed room gains sun and loses through glass");
        }
    }
    assert!(m.loads.iter().any(|l| l.roof > 0.0) && m.loads.iter().any(|l| l.ground > 0.0));
    // the air carries the cooling: more supply than the ventilation alone
    assert!(p.hvac.flow.0 > p0.hvac.flow.0, "{:?} vs {:?}", p.hvac.flow, p0.hvac.flow);
    // every supply diffuser is on the network, with its VAV box
    let diffusers: usize = p.storeys.iter().map(|st| st.layout.fixtures.iter().filter(|f| f.family == "Supply Diffuser").count()).sum();
    let supply: Vec<_> = m.air.terms.iter().filter(|t| t.sys == ddc_gen::project::AirSys::Supply).collect();
    assert_eq!(supply.len(), diffusers);
    assert_eq!(mech::unconnected(&m), 0);
    assert!(supply.iter().all(|t| t.run.is_some() && t.vav.is_some()), "every diffuser connected through a box");
    assert_eq!(m.count(Kind::VavBox), diffusers);
    for b in m.equip.iter().filter(|e| e.kind == Kind::VavBox) {
        assert!(b.port("Inlet").is_some() && b.port("Outlet").is_some() && b.duty("reheat_kw") >= 0.0);
    }
    // sized ducts with fittings, every velocity within its cap
    for r in &m.air.runs {
        assert!(r.v > 0.0 && r.v <= r.v_max + 1e-9, "{:?} {} {} m/s over {}", r.sys, r.class.name(), r.v, r.v_max);
        assert!(r.r <= rule("services.hvac.friction") + 1e-9 || r.class == mech::ducts::Class::Plant, "{} Pa/m", r.r);
    }
    let kinds = |k: FitKind| m.air.fittings.iter().filter(|f| f.kind == k).count();
    assert!(kinds(FitKind::Elbow) > 0 && kinds(FitKind::Tee) + kinds(FitKind::Takeoff) > 0, "elbows and tees");
    for f in m.air.fittings.iter().filter(|f| f.kind == FitKind::Elbow) {
        assert!(f.radius > 0.0 && f.zeta > 0.0 && f.dp > 0.0);
    }
    // the critical path, summed again from its pieces
    for (i, c) in m.air.critical.iter().enumerate() {
        assert!(c.dp > 0.0, "system {i} has a critical path");
        let t = &m.air.terms[c.term.expect("a terminal")];
        let sum: f64 = c.runs.iter().map(|&r| m.air.runs[r].friction + m.air.runs[r].fittings).sum::<f64>() + t.dp;
        assert!(close(sum, c.dp, 1e-6), "{sum} vs {}", c.dp);
        assert!(m.air.terms.iter().filter(|u| u.sys == t.sys).all(|u| u.path <= c.dp + 1e-9));
    }
    assert!(close(m.fans[0].dp_ext, m.air.critical[0].dp, 1e-9) && m.fans[0].dp_unit > 0.0);
    assert!(m.fans[0].motor_kw >= m.fans[0].shaft_kw && m.sfp > 0.0);
    // water to every reheat coil and the unit's coils, pumps on the index circuits
    assert!(m.cons.len() > m.count(Kind::VavBox));
    for (ci, c) in m.cons.iter().enumerate() {
        for s in 0..4 {
            if c.ports[s].is_some() && c.kw[s / 2] > 0.0 {
                assert!(m.water.leaf[ci][s].is_some(), "consumer {ci} slot {s} connected");
            }
        }
    }
    assert_eq!(m.water.pumps.len(), 2);
    assert!(m.water.index.iter().all(|x| *x > 0.0));
    assert!(!m.water.sleeves.is_empty() && !m.water.valves.is_empty());
    assert!(m.faults().is_empty(), "{:#?}", m.faults());
    let j = mech::to_json(&m);
    assert!(j["schematic"].as_str().is_some_and(|x| x.starts_with("<svg")));
    assert!(j["equipment"].as_array().is_some_and(|a| a.len() > m.equip.len()));
    let why = ddc_gen::project::why::explain_project(&s, &p);
    for key in ["hvac_loads", "hvac_sys_named", "hvac_supply", "hvac_fan", "hvac_pipes"] {
        assert!(why.iter().any(|w| w.key == key), "why {key}");
    }
    eprintln!(
        "office vav: supply {} vs {} m3/h, {} boxes, {} fittings, critical {:.0}/{:.0} Pa, fan {} kW, sfp {:.2}, {} pipes, pumps {:.0}/{:.0} kPa",
        p.hvac.flow.0,
        p0.hvac.flow.0,
        m.count(Kind::VavBox),
        m.air.fittings.len(),
        m.air.critical[0].dp,
        m.air.critical[1].dp,
        m.fans[0].motor_kw,
        m.sfp,
        m.water.pipes.len(),
        m.water.index[0] / 1000.0,
        m.water.index[1] / 1000.0
    );
}

/// Worked by hand (equal friction, 1.2 kg/m3, 1.5e-5 m2/s, 0.15 mm roughness): 600 m3/h in
/// d250 is 3.395 m/s, Re 56 588, lambda 0.0227, 0.627 Pa/m; d200 would be 5.3 m/s, over the
/// 4 m/s branch cap. A 90 degree elbow at r/D 1.5 is 0.15 of the 6.92 Pa velocity pressure.
#[test]
fn a_duct_sized_by_hand() {
    assert!(close(velocity(600.0, std::f64::consts::PI * 0.0625 / 4.0), 3.3953, 1e-4));
    assert!(close(friction_round(600.0, 0.25), 0.6270, 1e-4));
    assert!(close(friction_rect(1000.0, 0.4, 0.2), 0.6027, 1e-4));
    assert!(close(friction_round(600.0, 0.2), 1.8891, 1e-4));
    assert_eq!(pick_round(600.0, 1.0, 4.0, 0.0, &ROUND_MM), 0.25);
    assert!(close(dynamic(3.3953), 6.9168, 1e-3));
    assert!(close(elbow_zeta(1.5, 90.0) * dynamic(3.3953), 1.0375, 1e-3));
}

/// Beams under the slab across the corridor: the ducts drop under them with their clearance,
/// and no duct, box or pipe enters the structure.
#[test]
fn no_duct_clashes_with_structure() {
    let (s, p, m) = designed(office("vav"));
    assert!(m.clashes.is_empty(), "{:#?}", m.clashes);
    let depth = 0.2;
    let mut beams = vec![];
    for st in &p.storeys {
        let soffit = st.elevation + st.height - rule("services.slab");
        let xs: Vec<f64> = st.layout.grids.iter().filter(|g| (g.a.0 - g.b.0).abs() < 1e-6).map(|g| g.a.0).collect();
        let (y0, y1) = st.layout.slab.iter().fold((f64::MAX, f64::MIN), |a, q| (a.0.min(q.1), a.1.max(q.1)));
        for x in xs {
            beams.push(Beam { a: (x, y0), b: (x, y1), bottom: soffit - depth, top: soffit, width: 0.3 });
        }
    }
    assert!(!beams.is_empty(), "the grid gives beam lines");
    let b = mech::design_with(&s, &p, &beams).expect("design");
    assert!(b.air.lowered.iter().all(|d| *d > 0.0), "{:?}", b.air.lowered);
    for r in &b.air.runs {
        for x in r.boxes() {
            for beam in &beams {
                let bb = beam.bbox();
                let hit = x[0] < bb[3] && bb[0] < x[3] && x[1] < bb[4] && bb[1] < x[4] && x[2] < bb[5] && bb[2] < x[5];
                assert!(!hit, "{:?} duct {} under beam at x {}", r.sys, r.duct, beam.a.0);
            }
        }
    }
    assert!(b.faults().is_empty(), "{:#?}", b.faults());
    eprintln!("beams {depth} m deep: ducts lowered {:?} m, critical {:.0} Pa", b.air.lowered, b.air.critical[0].dp);
}

#[test]
fn the_rules_choose_the_system() {
    use select::{pick_air, pick_plant, AirSide, PlantKind};
    assert_eq!(pick_air(false, 50.0, 0.6, 1).0, AirSide::Vav);
    assert_eq!(pick_air(false, 120.0, 0.6, 1).0, AirSide::FcuDoas);
    assert_eq!(pick_air(false, 50.0, 0.3, 1).0, AirSide::RadiantDoas);
    assert_eq!(pick_air(false, 70.0, 0.3, 1).0, AirSide::FcuDoas);
    assert_eq!(pick_air(false, 10.0, 0.6, 1).0, AirSide::Radiators);
    assert_eq!(pick_air(true, 50.0, 0.6, 1).0, AirSide::Radiators);
    assert_eq!(pick_air(true, 50.0, 0.6, 2).0, AirSide::Vav);
    assert_eq!(pick_plant(100.0, 0), PlantKind::HeatPump);
    assert_eq!(pick_plant(1000.0, 0), PlantKind::ChillerBoiler);
    assert_eq!(pick_plant(100.0, 1), PlantKind::ChillerBoiler);
}

/// The country pack's assemblies set the envelope: the rules cite the assembly, and the
/// design under the pack's numbers loses more heat through its weaker envelope.
#[test]
fn the_pack_envelope_reaches_the_loads() {
    let set = PackSet::embedded();
    let generic = norms_from(|k| set.resolve("generic").unwrap().dim_cite(k));
    assert_eq!(generic["mep.hvac.u_wall"]["value"], json!(0.3));
    assert!(generic["mep.hvac.u_wall"]["cite"].as_str().unwrap().contains("assembly"));
    let ru = norms_from(|k| set.resolve("ru").unwrap().dim_cite(k));
    assert_eq!(ru["mep.hvac.u_window"]["value"], json!(1.67));
    let mut a = office("vav");
    a["norms"] = generic;
    let mut b = office("vav");
    b["norms"] = ru;
    let (_, _, ma) = designed(a);
    let (_, _, mb) = designed(b);
    let heat = |m: &Mech| m.loads.iter().map(|l| l.heat[0] + l.heat[1]).sum::<f64>();
    assert!(heat(&mb) > heat(&ma), "{} vs {}", heat(&mb), heat(&ma));
}

/// Fan coils with dedicated outdoor air: every cooled room gets its units, each piped.
#[test]
fn fan_coils_serve_every_cooled_room() {
    let (_, _, m) = designed(office("fcu_doas"));
    assert_eq!(m.choice.air, select::AirSide::FcuDoas);
    for l in m.loads.iter().filter(|l| l.kind.habitable() && l.sensible() > 0.0) {
        let cap: f64 = m.equip.iter().filter(|e| e.kind == Kind::FanCoil && e.storey == l.storey && e.room == Some(l.room)).map(|e| e.duty("cap_kw")).sum();
        assert!(cap * 1000.0 + 1e-6 >= l.sensible(), "room {} on {}: {cap} kW for {} W", l.number, l.storey, l.sensible());
    }
    assert_eq!(m.count(Kind::VavBox), 0);
    for (ci, c) in m.cons.iter().enumerate() {
        assert!(c.kw[0] <= 0.0 || (m.water.leaf[ci][0].is_some() && m.water.leaf[ci][1].is_some()), "consumer {ci} on chilled water");
    }
    assert!(m.faults().is_empty(), "{:#?}", m.faults());
}

/// A house heats by radiators at the facade, fed along the floor.
#[test]
fn a_house_heats_by_radiators() {
    let (_, _, m) = designed(json!({"typology": "house", "floors": 2,
        "rooms": [{"kind": "living", "area": 28}, {"kind": "kitchen", "area": 14},
            {"kind": "bedroom", "area": 14, "count": 3}, {"kind": "bath", "area": 6}],
        "rules": {"mep.hvac.system": "auto"}}));
    assert_eq!(m.choice.air, select::AirSide::Radiators);
    assert!(m.count(Kind::Radiator) >= 4);
    assert!(m.water.pipes.iter().all(|p| p.slot >= 2), "heating water only");
    assert!(m.faults().is_empty(), "{:#?}", m.faults());
}

/// Each unit's family, built with its type values and placed, puts every port where the design
/// has it, and a run of the network ends there.
fn ports_land(p: &GProject, m: &Mech) -> mech::model::Content {
    use ddc_geom::component::{Family, Value};
    use mech::model::Net;
    let c = mech::model::content(p, m);
    assert!(c.loose.is_empty(), "ports no run reaches: {:?}", c.loose);
    let fams: std::collections::HashMap<&str, Family> =
        c.families.iter().map(|f| (f.name.as_str(), Family::parse(&f.def).unwrap_or_else(|e| panic!("{}: {e:?}", f.name)))).collect();
    for u in &c.units {
        let f = &fams[u.family.as_str()];
        let vals: Vec<(String, Value)> = u.values.iter().map(|(k, v)| (k.clone(), Value::Num(*v))).collect();
        let r = f.resolve(&vals, &[]).unwrap_or_else(|e| panic!("{}: {e:?}", u.tag));
        let ports = f.ports(&r).unwrap();
        let (cs, sn) = (u.rot.cos(), u.rot.sin());
        for (name, net) in &u.links {
            let q = ports.iter().find(|x| x.name == *name).unwrap_or_else(|| panic!("{} has no {name}", u.family));
            let at = (u.at.0 + cs * q.at.x - sn * q.at.y, u.at.1 + sn * q.at.x + cs * q.at.y, u.at.2 + q.at.z);
            let runs = if matches!(net, Net::Air(_)) { &c.ducts } else { &c.pipes };
            let near = |a: (f64, f64, f64)| (a.0 - at.0).hypot(a.1 - at.1).hypot(a.2 - at.2) < 2e-3;
            assert!(
                runs.iter().any(|r| r.net == *net && (near(r.points[0]) || near(*r.points.last().unwrap()))),
                "{} {name} at {at:?}: no run ends there",
                u.tag
            );
        }
    }
    c
}

#[test]
fn the_design_becomes_model_content() {
    let (_, p, m) = designed(office("vav"));
    let c = ports_land(&p, &m);
    let n = m.equip.len() + m.water.plant.len() + m.water.pumps.len();
    assert_eq!(c.units.len(), n);
    assert_eq!(c.units[0].kind, Kind::Ahu);
    // every VAV box sits inline: the branch is cut and both its ends join the box
    let boxes: Vec<_> = c.units.iter().filter(|u| u.kind == Kind::VavBox).collect();
    assert_eq!(boxes.len(), m.count(Kind::VavBox));
    for b in &boxes {
        for port in ["Inlet", "Outlet", "hw_flow", "hw_return"] {
            assert!(b.links.iter().any(|l| l.0 == port), "{} {port} not joined", b.tag);
        }
    }
    assert_eq!(c.ducts.len(), p.hvac.ducts.len() + boxes.len());
    let ahu = &c.units[0];
    for port in ["Supply", "Return", "chw_flow", "chw_return", "hw_flow", "hw_return"] {
        assert!(ahu.links.iter().any(|l| l.0 == port), "AHU {port} not joined");
    }
    for pump in c.units.iter().filter(|u| u.kind == Kind::Pump) {
        assert_eq!(pump.links.len(), 2, "pump {} inline", pump.tag);
    }
    assert_eq!(c.pipes.len(), m.water.pipes.len() + m.water.pumps.len());
    assert!(c.families.iter().any(|f| f.name == "HVAC VAV Box HW"), "{:?}", c.families.iter().map(|f| &f.name).collect::<Vec<_>>());
    // fan coils and radiators land the same way
    for sys in ["fcu_doas", "radiant_doas"] {
        let (_, p, m) = designed(office(sys));
        ports_land(&p, &m);
    }
    let (_, p, m) = designed(json!({"typology": "house", "floors": 2, "rules": {"mep.hvac.system": "auto"}}));
    ports_land(&p, &m);
}

/// A hospital wing ventilates with terminals in its rooms; its ducts get a fire damper wherever
/// they cross a rated wall (and at every shaft), and a volume damper on every supply and
/// extract branch takeoff unless the rule turns the supply ones off.
#[test]
fn a_hospital_gets_fire_dampers_at_rated_walls_and_volume_dampers_on_branches() {
    let spec = |rules: J| json!({"typology": "hospital_wing", "rules": rules});
    let (s, p, m) = designed(spec(json!({"mep.hvac.system": "vav"})));
    let terminals = m.air.terms.len();
    assert!(terminals >= 40, "{terminals} air terminals");
    let count = |m: &Mech, k: Kind| m.air.dampers.iter().filter(|d| d.kind == k).count();
    let branches = m.air.runs.iter().filter(|r| r.class == ddc_gen::project::mech::ducts::Class::Branch && r.term.is_some()).count();
    assert!(count(&m, Kind::VolumeDamper) >= branches, "{} volume dampers on {branches} branches", count(&m, Kind::VolumeDamper));
    assert!(count(&m, Kind::FireDamper) >= 1);
    let rated = p.storeys.iter().flat_map(|st| &st.layout.walls).filter(|w| w.fire.is_some()).count();
    assert!(rated > 0, "the hospital has rated walls");
    // the rules steer them: no supply takeoff dampers, and rating limit above every wall
    let (_, _, none) = designed(spec(json!({"mep.hvac.system": "vav", "mep.hvac.volume_damper_supply": 0, "mep.hvac.fire_damper_min": 240})));
    assert!(count(&none, Kind::VolumeDamper) < count(&m, Kind::VolumeDamper));
    assert!(count(&none, Kind::FireDamper) <= count(&m, Kind::FireDamper));
    let _ = s;
}
