//! QA: MEP runs, sizes, checks and clash inputs.

use crate::qa_methods_tests::fx;
use crate::types::MepResize;
use crate::{ClashParams, ClashSide, MepHit, RouteEnd, RunSpec};
use ddc_geom::mep::{Domain, Section, Shape, SystemKind};

fn duct(f: &mut crate::qa_methods_tests::Fx, sys: i64, d: f64, y: f64) -> i64 {
    let s = Section::new(Domain::Duct, Shape::Round { d });
    let spec = RunSpec::new(sys, f.l1, s, vec![(0.0, y, 2.5), (5.0, y, 2.5)]);
    f.m.add_mep_run(&spec).unwrap()
}

#[test]
fn run_sizes_and_checks() {
    let mut f = fx();
    let sys = f.m.add_mep_system("SA", SystemKind::SupplyAir).unwrap();
    let a = duct(&mut f, sys, 0.2, 1.0);
    let b = duct(&mut f, sys, 0.2, 2.0);
    let c = duct(&mut f, sys, 0.3, 3.0);
    let rs = |d: f64, nw: f64| MepResize {
        domain: "duct".into(),
        shape: "round".into(),
        w: d,
        h: d,
        nw,
        nh: nw,
    };
    let n = f.m.set_mep_sizes(sys, &[rs(0.2, 0.25)]).unwrap();
    assert_eq!(n, 2, "both 0.2 runs are resized, the 0.3 one is not");
    let w = |m: &crate::Model, id: i64| match m.mep_run(id).unwrap().section.shape {
        Shape::Round { d } => d,
        _ => unreachable!(),
    };
    assert_eq!((w(&f.m, a), w(&f.m, b), w(&f.m, c)), (0.25, 0.25, 0.3));
    f.m.undo().unwrap();
    assert_eq!(w(&f.m, a), 0.2);
    assert!(f.m.set_mep_sizes(sys, &[rs(0.99, 0.25)]).is_err(), "no runs of this size");
    assert!(f.m.set_mep_sizes(sys, &[rs(0.2, 0.0)]).is_err());
    assert!(f.m.set_mep_sizes(sys, &[rs(0.2, f64::NAN)]).is_err());
    assert!(f.m.set_mep_sizes(sys, &[]).is_err());
    // report per system
    let all = f.m.mep_check_all().unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].system, sys);
    assert!(all[0].length > 14.0, "three 5 m runs: {}", all[0].length);
    // no systems: empty report
    let empty = crate::Model::open(":memory:").unwrap();
    assert!(empty.mep_check_all().unwrap().is_empty());
}

#[test]
fn clash_inputs_bounds_and_clear() {
    let mut f = fx();
    let sys = f.m.add_mep_system("SA", SystemKind::SupplyAir).unwrap();
    let a = duct(&mut f, sys, 0.3, 0.0);
    let p = ClashParams {
        a: ClashSide::cats(&["duct"]),
        b: ClashSide::cats(&["wall"]),
        tolerance: 0.0,
        clearance: false,
    };
    let inp = f.m.clash_input(&p).unwrap();
    assert!(inp.len() >= 1);
    assert!(!inp.is_empty());
    let none = ClashParams { a: ClashSide::cats(&["pipe"]), ..p.clone() };
    let e = f.m.clash_input(&none).unwrap();
    assert_eq!(e.len(), 0);
    assert!(e.is_empty());
    let neg = ClashParams { tolerance: -1.0, ..p.clone() };
    assert!(f.m.clash_input(&neg).is_err());
    let nan = ClashParams { tolerance: f64::NAN, ..p.clone() };
    assert!(f.m.clash_input(&nan).is_err());
    let cl = ClashParams {
        a: ClashSide::cats(&["wall"]),
        b: ClashSide::cats(&["slab"]),
        tolerance: 0.1,
        clearance: true,
    };
    assert!(f.m.clash_input(&cl).is_err(), "clearance needs a MEP set");
    // bounds
    let bb = f.m.clash_element_bounds(&[("duct".to_string(), a), ("duct".to_string(), 9999)]).unwrap();
    assert_eq!(bb.len(), 2);
    assert!(bb[0].is_some());
    assert!(bb[1].is_none());
    assert!(f.m.clash_element_bounds(&[]).unwrap().is_empty());
    f.m.clash_clear().unwrap();
    f.m.clash_clear().unwrap();
}

#[test]
fn run_min_leg_and_route_end() {
    let s = Section::new(Domain::Duct, Shape::Round { d: 0.2 });
    let mut r = RunSpec::new(1, 1, s, vec![(0.0, 0.0, 0.0), (1.0, 0.0, 0.0)]);
    r.bend = Some(0.4);
    assert_eq!(RunSpec::min_leg(&r), 2.5 * 0.4);
    r.bend = Some(0.0);
    assert_eq!(RunSpec::min_leg(&r), 0.0);
    let hit = MepHit::RunEnd { run: 7, at: (1.0, 2.0, 3.0), system: 1, end: 0 };
    assert_eq!(MepHit::route_end(&hit), RouteEnd::Run { run: 7, at: (1.0, 2.0, 3.0) });
    let port = crate::WorldPort {
        comp: 4,
        name: "out".into(),
        domain: Domain::Duct,
        system: None,
        at: (0.0, 0.0, 0.0),
        dir: (1.0, 0.0, 0.0),
        size: 0.2,
    };
    assert_eq!(
        MepHit::route_end(&MepHit::Port(port)),
        RouteEnd::Port { comp: 4, connector: "out".into() }
    );
}
