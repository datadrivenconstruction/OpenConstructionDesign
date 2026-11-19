// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Network oracles on hand-made systems: a connected one passes, the same with one gap fails
//! and names the run.

use super::case::Case;
use super::facts::{Comp, Facts, Member, Room, Run, P3};
use super::oracle::{evaluate, EvalCx, OracleResult};
use super::tests::{editor, level, sql};
use std::time::Instant;

fn run(id: i64, system: &str, level: i64, pts: &[P3]) -> Run {
    Run {
        id,
        system: system.into(),
        level,
        domain: "duct".into(),
        shape: "rect".into(),
        w: 0.4,
        h: 0.4,
        dn: None,
        slope: 0.0,
        pts: pts.to_vec(),
        cap: [false, false],
    }
}

fn comp(id: i64, level: i64, family: &str, category: &str, at: P3) -> Comp {
    Comp { id, level, at, family: family.into(), category: category.into() }
}

fn facts(runs: Vec<Run>, comps: Vec<Comp>, ported: &[i64]) -> Facts {
    let mut f = Facts::default();
    f.tables.insert("mep_run".into());
    f.runs = runs;
    f.comps = comps;
    f.port_comps = ported.iter().copied().collect();
    f
}

/// One system: unit with a port, three ducts in a row, a diffuser at the end.
fn network() -> Facts {
    facts(
        vec![
            run(1, "supply_air", 1, &[[0.0, 0.0, 3.0], [10.0, 0.0, 3.0]]),
            run(2, "supply_air", 1, &[[10.0, 0.0, 3.0], [10.0, 10.0, 3.0]]),
            run(3, "supply_air", 1, &[[10.0, 10.0, 3.0], [20.0, 10.0, 3.0]]),
        ],
        vec![
            comp(7, 1, "AHU 1", "mechanical", [0.0, 0.0, 3.0]),
            comp(8, 1, "Diffuser", "air_terminal", [20.0, 10.0, 3.0]),
        ],
        &[7],
    )
}

fn check(f: &Facts, before: Option<&Facts>, kinds: &str) -> Vec<OracleResult> {
    let ed = editor();
    let c = Case::parse(&format!("id=\"t\"\n[[turn]]\nsay=\"x\"\n{kinds}")).unwrap();
    let cx = EvalCx { ed: &ed, facts: f, before, answer: "", ifc: None, errors_before: 0, receipts: &[] };
    evaluate(&c, &cx)
}

fn geom(kind: &str, params: &str) -> String {
    format!("[[oracle.geom]]\nkind=\"{kind}\"\n{params}\n")
}

#[test]
fn oracle_connected_network_passes_and_a_gap_names_the_run() {
    let ok = network();
    let r = check(&ok, None, &(geom("mep_connected", "components_max=1.0\nends_min=1.0") + &geom("mep_free_ends", "max=0.0")));
    assert!(r.iter().all(|x| x.pass), "{r:?}");
    assert_eq!(r[0].measured, "1 system(s), 3 run(s), ends connected 1.000");
    assert_eq!(r[1].measured, "0 free of 6 ends");

    // the same network, the last duct starts half a metre from where the second one ends
    let mut gap = network();
    gap.runs[2].pts[0] = [10.5, 10.0, 3.0];
    let r = check(&gap, None, &(geom("mep_connected", "components_max=1.0") + &geom("mep_free_ends", "max=0.0")));
    assert!(!r[0].pass && !r[1].pass, "{r:?}");
    assert!(r[0].measured.contains("supply_air: 2 parts, cut off: run 3"), "{}", r[0].measured);
    assert_eq!(
        r[1].measured,
        "2 free of 6 ends: run 2 end 1 at (10.0, 10.0, 3.0); run 3 end 0 at (10.5, 10.0, 3.0)"
    );
    // a network cut into two parts is allowed when the case says two
    let r = check(&gap, None, &geom("mep_connected", "components_max=2.0"));
    assert!(r[0].pass, "{r:?}");
}

#[test]
fn oracle_systems_do_not_join_across_each_other() {
    let mut f = network();
    // a return duct ending exactly on the supply duct is still another system
    f.runs.push(run(4, "return_air", 1, &[[5.0, 0.0, 3.0], [5.0, -8.0, 3.0]]));
    f.runs[3].cap = [false, true];
    let r = check(&f, None, &geom("mep_connected", "components_max=1.0\nsystem_min=2.0"));
    assert!(r[0].pass, "{r:?}");
    assert!(r[0].measured.starts_with("2 system(s), 4 run(s)"), "{}", r[0].measured);
}

#[test]
fn oracle_clash_counts_members_through_a_duct() {
    let mut f = network();
    let beam = |z: f64| Member { kind: "beam", a: [5.0, -2.0, z], b: [5.0, 2.0, z], mtype: None, bulge: 0.0 };
    f.members = vec![beam(3.0)];
    let r = check(&f, None, &geom("mep_clashes", "max=0.0"));
    assert!(!r[0].pass);
    assert_eq!(r[0].measured, "1 clashing pair(s), first: run 1 with member 0");
    f.members = vec![beam(4.0)];
    let r = check(&f, None, &geom("mep_clashes", "max=0.0"));
    assert!(r[0].pass, "{r:?}");
    assert_eq!(r[0].measured, "0 clashing pair(s)");
}

#[test]
fn oracle_served_rooms_by_name_and_reach() {
    let mut f = network();
    f.rooms = vec![
        Room { id: 1, level: 1, name: "Ward 1".into(), at: [18.0, 8.0] },
        Room { id: 2, level: 1, name: "Ward 2".into(), at: [60.0, 60.0] },
        Room { id: 3, level: 1, name: "Shaft".into(), at: [90.0, 90.0] },
    ];
    let q = |extra: &str| geom("served_rooms", &format!("terminal=\"diffuser|grille\"\nexclude=\"shaft\"\n{extra}"));
    let r = check(&f, None, &q("share_min=1.0"));
    assert!(!r[0].pass);
    assert_eq!(r[0].measured, "1 of 2 rooms served (0.500); without: Ward 2");
    assert!(check(&f, None, &q("share_min=0.5"))[0].pass);
    // the theatre category alone
    let r = check(&f, None, &geom("served_rooms", "terminal=\"diffuser\"\nrooms=\"ward 1\"\nshare_min=1.0"));
    assert!(r[0].pass, "{r:?}");
    // a diffuser on another level serves nobody
    f.comps[1].level = 2;
    assert!(!check(&f, None, &q("share_min=0.5"))[0].pass);
}

#[test]
fn oracle_edit_scope_one_system_one_level() {
    let before = network();
    let mut after = network();
    after.runs.push(run(9, "extract_air", 3, &[[0.0, 20.0, 9.0], [8.0, 20.0, 9.0]]));
    let kinds = geom("mep_kept", "min=1.0") + &geom("mep_new_scope", "systems_max=1.0\nlevels_max=1.0");
    let r = check(&after, Some(&before), &kinds);
    assert!(r.iter().all(|x| x.pass), "{r:?}");
    assert_eq!(r[1].measured, "1 new run(s) in 1 system(s) on 1 level(s)");
    // a second new run in another system and on another level, and an old run moved
    after.runs.push(run(10, "supply_air", 2, &[[0.0, 30.0, 6.0], [8.0, 30.0, 6.0]]));
    after.runs[0].pts[1] = [10.0, 0.5, 3.0];
    let r = check(&after, Some(&before), &kinds);
    assert!(!r[0].pass && !r[1].pass, "{r:?}");
    assert_eq!(r[0].measured, "2 of 3 runs unchanged (0.667)");
    assert!(check(&after, None, &kinds).iter().all(|x| !x.pass), "no base: no capability");
}

#[test]
fn no_network_table_fails_with_the_reason() {
    let f = Facts::default();
    let r = check(&f, None, &geom("mep_connected", "components_max=1.0"));
    assert!(!r[0].pass);
    assert!(r[0].measured.contains("no table mep_run"), "{}", r[0].measured);
}

#[test]
fn facts_read_parses_runs_and_systems_from_the_model() {
    let ed = editor();
    let lv = level(&ed);
    sql(&ed, "INSERT INTO mep_system(name, kind, color) VALUES('Supply 1', 'supply_air', 255)");
    let sys: i64 = ed.model.sql().query_row("SELECT id FROM mep_system", [], |r| r.get(0)).unwrap();
    for (i, pts) in ["0 0 3;10 0 3", "10 0 3;10 10 3;20 10 3"].iter().enumerate() {
        sql(
            &ed,
            &format!(
                "INSERT INTO mep_run(system, level, domain, shape, w, h, wall, insulation, lining, slope, points, cap0, cap1) \
                 VALUES({sys}, {lv}, 'duct', 'rect', 0.4, 0.3, 0.001, 0, 0, 0, '{pts}', {}, 0)",
                i
            ),
        );
    }
    let f = Facts::read(&ed.model).unwrap();
    assert_eq!(f.runs.len(), 2);
    assert_eq!(f.runs[0].system, "supply_air");
    assert_eq!(f.runs[1].pts.len(), 3);
    assert_eq!(f.runs[1].pts[2], [20.0, 10.0, 3.0]);
    assert_eq!(f.runs[1].cap, [true, false]);
    assert_eq!(f.count("mep:duct").unwrap(), 2);
    assert_eq!(f.count("sysk:supply").unwrap(), 1);
    // the two runs share the point (10, 0, 3): one part, one free end at each far end
    let r = check(&f, None, &(geom("mep_connected", "components_max=1.0") + &geom("mep_free_ends", "max=3.0")));
    assert!(r.iter().all(|x| x.pass), "{r:?}");
}

#[test]
fn mep_oracle_budget() {
    // 100 rows of 100 ducts, 3 m each, end to end: 10 000 runs, 100 separate parts
    let mut runs = vec![];
    for row in 0..100 {
        for k in 0..100 {
            let id = (row * 100 + k) as i64;
            let (x, y) = (3.0 * k as f64, 5.0 * row as f64);
            runs.push(run(id, "supply_air", 1, &[[x, y, 3.0], [x + 3.0, y, 3.0]]));
        }
    }
    let f = facts(runs, vec![], &[]);
    let t = Instant::now();
    let r = check(&f, None, &geom("mep_connected", "components_max=100.0"));
    let ms = t.elapsed().as_millis();
    assert!(r[0].pass, "{r:?}");
    // the budget is for the release build; a debug build gets 25 times as much
    let limit = if cfg!(debug_assertions) { 5000 } else { 200 };
    assert!(ms <= limit, "10 000 runs took {ms} ms, budget {limit} ms");
    let r = check(&f, None, &geom("mep_connected", "components_max=99.0"));
    assert!(!r[0].pass && r[0].measured.contains("100 parts"), "{}", r[0].measured);
}

#[test]
fn hard_network_cases_are_complete() {
    let dir = super::case::evals_dir().join("cases");
    let get = |id: &str| Case::load(&dir.join(format!("{id}.toml"))).unwrap_or_else(|e| panic!("{e}"));
    let kinds = |c: &Case| c.oracle.geom.iter().map(|g| g.kind.clone()).collect::<Vec<_>>();
    for id in ["H02_hospital_hvac", "H03_process_plant", "H07_datacenter", "H15_hospital_add_theatre"] {
        let c = get(id);
        assert_eq!(c.tier, "hard", "{id}");
        for k in ["mep_connected", "mep_free_ends", "mep_clashes"] {
            assert!(kinds(&c).contains(&k.to_string()), "{id} lacks {k}");
        }
        assert!(c.oracle.checks.is_some(), "{id}");
    }
    assert!(kinds(&get("H02_hospital_hvac")).contains(&"served_rooms".to_string()));
    let h15 = get("H15_hospital_add_theatre");
    assert_eq!(h15.base, "@H02_hospital_hvac");
    assert!(kinds(&h15).contains(&"mep_kept".to_string()) && kinds(&h15).contains(&"mep_new_scope".to_string()));
    // every kind a case names is one the oracles know: none falls into "unknown oracle kind"
    let ed = editor();
    let f = Facts::default();
    for c in super::case::load_dir(&dir).unwrap() {
        let cx = EvalCx { ed: &ed, facts: &f, before: None, answer: "", ifc: None, errors_before: 0, receipts: &[] };
        for r in evaluate(&c, &cx) {
            assert!(!r.measured.contains("unknown oracle kind"), "{}: {}", c.id, r.name);
        }
    }
}

/// A run end counts as joined on a connector only when the connector is linked to the run's
/// own system and the end lies on it: a feed 2 m above its unit's origin holds its drop, the
/// same end beside a port of another system, or off any port, stays free.
#[test]
fn oracle_free_ends_holds_an_end_only_on_a_port_of_its_own_system() {
    let drop = |sys: &str, port: (&str, P3)| {
        let mut f = facts(vec![run(1, sys, 1, &[[0.0, 0.0, 2.0], [0.0, 0.0, 2.8]])], vec![], &[]);
        f.runs[0].cap = [false, true];
        f.sys_ports = vec![(port.0.to_string(), port.1)];
        f
    };
    let kinds = geom("mep_free_ends", "max=0.0");
    let on = check(&drop("power", ("power", [0.0, 0.0, 2.0])), None, &kinds);
    assert!(on[0].pass, "{on:?}");
    let other = check(&drop("power", ("chilled_water", [0.0, 0.0, 2.0])), None, &kinds);
    assert!(!other[0].pass, "an end on another system's port is free: {other:?}");
    let off = check(&drop("power", ("power", [0.0, 0.5, 2.0])), None, &kinds);
    assert!(!off[0].pass, "an end half a metre off the port is free: {off:?}");
    assert!(super::mep_oracles::on_port(&drop("power", ("power", [0.0, 0.0, 2.0])), "power", [0.05, 0.0, 2.0]));
}
