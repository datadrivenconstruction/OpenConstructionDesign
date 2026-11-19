//! STEP scan and receipt oracles.

use super::case::Case;
use super::facts::Facts;
use super::oracle::{evaluate, EvalCx};
use super::query_oracles::step_problem;
use super::tests::editor;
use crate::ai::plan::QtyDelta;
use crate::ai::receipt::WriteReceipt;

#[test]
fn step_scan_finds_the_stray_comma_and_passes_clean_text() {
    let ok = "ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\n#1=IFCWALL('a''b,,c',$,(#2,#3));\n#2=IFCCARTESIANPOINT((0.,1.));\n#3=IFCCARTESIANPOINT((1.,1.));\nENDSEC;\n";
    assert_eq!(step_problem(ok), Ok(3));
    let dangling = ok.replace("#3=IFCCARTESIANPOINT((1.,1.));\n", "");
    assert_eq!(step_problem(&dangling), Err("line 5: dangling reference #3".to_string()));
    let bad = "DATA;\n#1=IFCWALL($,#2);\n#2=IFCX((#3,,#4));\n";
    assert_eq!(step_problem(bad), Err("line 3: unexpected ','".to_string()));
    assert_eq!(step_problem("DATA;\n#1=IFCX((1,2),);\n").unwrap_err(), "line 2: unexpected ',' before ')'");
    assert_eq!(step_problem("DATA;\n#1=IFCX((1,2);\n").unwrap_err(), "line 2: statement ends inside a list");
    assert!(step_problem("no data here").is_err());
}

fn receipt() -> WriteReceipt {
    WriteReceipt {
        token: 0,
        applied: true,
        undo_label: "x".into(),
        n_created: 3,
        n_changed: 1,
        n_deleted: 0,
        created: vec![],
        changed: vec![],
        deleted: vec![],
        qty: vec![QtyDelta { key: "slab_area".into(), level: None, before: 100.0, after: 160.0 }],
        new_findings: vec![],
        ifc: vec![("IfcSlab".into(), 2)],
        truncated: 0,
        blocked: None,
    }
}

#[test]
fn receipt_oracle_sums_what_the_applied_plans_reported() {
    let ed = editor();
    let f = Facts::read(&ed.model).unwrap();
    let c = Case::parse(
        "id=\"t\"\n[[turn]]\nsay=\"x\"\n[[oracle.receipt]]\nfield=\"created\"\nmin=3.0\nmax=3.0\n[[oracle.receipt]]\nfield=\"deleted\"\nmax=0.0\n\
         [[oracle.receipt]]\nfield=\"ifc:IfcSlab\"\nmin=2.0\n[[oracle.receipt]]\nfield=\"qty:slab_area\"\nmin=60.0\nmax=60.0\n\
         [[oracle.receipt]]\nfield=\"blocked\"\nmax=0.0\n[[oracle.receipt]]\nfield=\"created\"\nmax=2.0\n[[oracle.receipt]]\nfield=\"nonsense\"\nmin=1.0\n",
    )
    .unwrap();
    let rs = [receipt()];
    let cx = EvalCx { ed: &ed, facts: &f, before: None, answer: "", ifc: None, errors_before: 0, receipts: &rs };
    let got: Vec<bool> = evaluate(&c, &cx).iter().map(|o| o.pass).collect();
    assert_eq!(got, [true, true, true, true, true, false, false]);
    let cx = EvalCx { receipts: &[], ..cx };
    let none = evaluate(&c, &cx);
    assert!(none.iter().all(|o| !o.pass && o.measured.contains("no receipt")), "{none:?}");
}

#[test]
fn ifc_wellformed_needs_an_export_and_names_the_line() {
    let ed = editor();
    let f = Facts::read(&ed.model).unwrap();
    let c = Case::parse("id=\"t\"\n[[turn]]\nsay=\"x\"\n[[oracle.geom]]\nkind=\"ifc_wellformed\"\n").unwrap();
    let at = |ifc: Option<&str>| {
        let cx = EvalCx { ed: &ed, facts: &f, before: None, answer: "", ifc, errors_before: 0, receipts: &[] };
        evaluate(&c, &cx).remove(0)
    };
    assert!(!at(None).pass);
    assert!(at(Some("DATA;\n#1=IFCX((1,2));\n")).pass);
    let bad = at(Some("DATA;\n#1=IFCX((1,2));\n#2=IFCY(#1,,);\n"));
    assert!(!bad.pass && bad.measured == "line 3: unexpected ','", "{bad:?}");
}

#[test]
fn hard_cases_carry_the_checks() {
    let dir = super::case::evals_dir().join("cases");
    for id in ["H01_lattice_tower_300", "H03_process_plant", "H05_tower40_core_fire", "H06_museum_freeform", "H12_de_hospital_hvac"] {
        let c = Case::load(&dir.join(format!("{id}.toml"))).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(c.tier, "hard", "{id}");
        assert!(c.oracle.geom.iter().any(|g| g.kind == "ifc_wellformed"), "{id}: no STEP check");
        assert!(c.oracle.ifc.is_some(), "{id}: nothing is exported without [oracle.ifc]");
        assert!(c.oracle.receipt.iter().any(|r| r.field == "created") && c.oracle.receipt.iter().any(|r| r.field == "blocked"), "{id}");
        assert!(c.oracle.checks.is_some(), "{id}");
        assert_eq!(Case::parse(&c.to_toml()).unwrap(), c, "{id}: round trip");
    }
}

#[test]
fn h05_oracles_fail_on_the_recorded_failure() {
    // what the live run left: a model without sprinklers, a STEP file with a stray comma, and
    // a mass deletion that waited for the person
    let dir = super::case::evals_dir().join("cases");
    let c = Case::load(&dir.join("H05_tower40_core_fire.toml")).unwrap();
    let ed = editor();
    let f = Facts::read(&ed.model).unwrap();
    let mut blocked = receipt();
    blocked.applied = false;
    blocked.blocked = Some((900, 1000));
    let rs = [blocked];
    let cx = EvalCx {
        ed: &ed,
        facts: &f,
        before: None,
        answer: "Sprinklers on every floor.",
        ifc: Some("DATA;
#1=IFCX((#2,,#3));
ENDSEC;
"),
        errors_before: 0,
        receipts: &rs,
    };
    let r = evaluate(&c, &cx);
    let get = |n: &str| r.iter().find(|o| o.name == n).unwrap_or_else(|| panic!("no oracle {n}: {:?}", r.iter().map(|o| &o.name).collect::<Vec<_>>()));
    assert!(!get("count comp:sprinkler").pass);
    let step = get("geom ifc_wellformed");
    assert!(!step.pass && step.measured == "line 2: unexpected ','", "{step:?}");
    let b = get("receipt blocked");
    assert!(!b.pass && b.measured.starts_with("1.00 over 1 receipt"), "{b:?}");
    assert!(r.iter().filter(|o| o.pass).count() < r.len());
}

#[test]
fn receipt_qty_can_be_read_per_level() {
    let mut r = receipt();
    r.qty = vec![
        QtyDelta { key: "room_area".into(), level: Some(3), before: 0.0, after: 40.0 },
        QtyDelta { key: "room_area".into(), level: Some(4), before: 0.0, after: 25.0 },
    ];
    let ed = editor();
    let f = Facts::read(&ed.model).unwrap();
    let c = Case::parse(
        "id=\"t\"
[[turn]]
say=\"x\"
[[oracle.receipt]]
field=\"qty:room_area\"
min=65.0
max=65.0
         [[oracle.receipt]]
field=\"qty:room_area@3\"
min=40.0
max=40.0
[[oracle.receipt]]
field=\"qty:room_area@4\"
max=30.0
         [[oracle.receipt]]
field=\"qty:room_area@x\"
min=0.0
",
    )
    .unwrap();
    let rs = [r];
    let cx = EvalCx { ed: &ed, facts: &f, before: None, answer: "", ifc: None, errors_before: 0, receipts: &rs };
    let got: Vec<bool> = evaluate(&c, &cx).iter().map(|o| o.pass).collect();
    assert_eq!(got, [true, true, true, false]);
}
