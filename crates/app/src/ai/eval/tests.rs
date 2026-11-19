//! Oracles on models built by hand with known numbers, in both outcomes (pass and fail).

use super::case::{self, Case};
use super::facts::Facts;
use super::oracle::{build_oracles, evaluate, EvalCx, OracleResult};
use crate::editor::Editor;

pub(super) fn editor() -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.reload().unwrap();
    ed
}

pub(super) fn sql(ed: &Editor, q: &str) {
    ed.model.sql().execute_batch(q).unwrap_or_else(|e| panic!("{q}: {e}"));
}

pub(super) fn level(ed: &Editor) -> i64 {
    ed.model.sql().query_row("SELECT id FROM level ORDER BY elevation LIMIT 1", [], |r| r.get(0)).unwrap()
}

pub(super) fn member_type(ed: &Editor, name: &str) -> i64 {
    sql(ed, &format!("INSERT INTO member_type(name, shape, material) VALUES('{name}', 'pipe', 'steel')"));
    ed.model.sql().query_row("SELECT id FROM member_type WHERE name = ?1", [name], |r| r.get(0)).unwrap()
}

pub(super) fn brace(ed: &Editor, lv: i64, mt: i64, a: [f64; 3], b: [f64; 3]) {
    sql(
        ed,
        &format!(
            "INSERT INTO brace(level, mtype, ax, ay, az, bx, by, bz, rotation) VALUES({lv}, {mt}, {}, {}, {}, {}, {}, {}, 0)",
            a[0], a[1], a[2], b[0], b[1], b[2]
        ),
    );
}

pub(super) fn beam(ed: &Editor, lv: i64, mt: i64, a: [f64; 3], b: [f64; 3]) {
    sql(
        ed,
        &format!(
            "INSERT INTO beam(level, mtype, ax, ay, az, bx, by, bz, bulge, yjust, zjust, rotation, start_ext, end_ext, join_start, join_end) \
             VALUES({lv}, {mt}, {}, {}, {}, {}, {}, {}, 0, 0, 0, 0, 0, 0, '', '')",
            a[0], a[1], a[2], b[0], b[1], b[2]
        ),
    );
}

pub(super) fn column(ed: &Editor, lv: i64, mt: i64, x: f64, y: f64, top: f64) {
    sql(
        ed,
        &format!(
            "INSERT INTO col(level, top_level, mtype, structural, x, y, base_offset, top_offset, rotation) \
             VALUES({lv}, {lv}, {mt}, 1, {x}, {y}, 0, {top}, 0)"
        ),
    );
}

/// Half-width of the tower at height z: 30 m at the foot, 8 m at the top, bending inward.
pub(super) fn tower_r(z: f64) -> f64 {
    8.0 + 22.0 * (1.0 - z / 300.0).powi(2)
}

/// Four legs of 30 segments up to 300 m, a ring beam at every joint: 120 braces, 124 beams.
pub(super) fn tower(ed: &Editor) {
    let lv = level(ed);
    let mt = member_type(ed, "leg");
    let corners = [(1.0, 1.0), (-1.0, 1.0), (-1.0, -1.0), (1.0, -1.0)];
    for k in 0..=30 {
        let z = 10.0 * k as f64;
        let r = tower_r(z);
        for i in 0..4 {
            let (cx, cy) = corners[i];
            let (nx, ny) = corners[(i + 1) % 4];
            beam(ed, lv, mt, [cx * r, cy * r, z], [nx * r, ny * r, z]);
            if k < 30 {
                let r2 = tower_r(z + 10.0);
                brace(ed, lv, mt, [cx * r, cy * r, z], [cx * r2, cy * r2, z + 10.0]);
            }
        }
    }
}

pub(super) fn run_case(ed: &Editor, toml: &str, answer: &str) -> Vec<OracleResult> {
    let c = Case::parse(toml).unwrap();
    let facts = Facts::read(&ed.model).unwrap();
    let cx = EvalCx { ed, facts: &facts, before: None, answer, ifc: None, errors_before: 0, receipts: &[] };
    evaluate(&c, &cx)
}

pub(super) fn one<'a>(rs: &'a [OracleResult], name: &str) -> &'a OracleResult {
    rs.iter().find(|r| r.name == name).unwrap_or_else(|| panic!("no oracle {name}: {rs:?}"))
}

#[test]
fn case_toml_roundtrip() {
    let cases = case::load_dir(&case::evals_dir().join("cases")).unwrap();
    assert!(cases.len() >= 60, "{} cases", cases.len());
    for c in &cases {
        let again = Case::parse(&c.to_toml()).unwrap_or_else(|e| panic!("{}: {e}", c.id));
        assert_eq!(&again, c, "{}", c.id);
        assert!(!c.turns.is_empty(), "{} has no turn", c.id);
    }
    let mut ids: Vec<_> = cases.iter().map(|c| c.id.clone()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), cases.len(), "case ids are unique");
}

#[test]
fn evals_order_hard_first() {
    let mk = |id: &str, tier: &str| Case::parse(&format!("id = \"{id}\"\ntier = \"{tier}\"\n[[turn]]\nsay = \"x\"\n")).unwrap();
    let v = case::ordered(vec![mk("S01", "smoke"), mk("H02", "hard"), mk("E1", "edit"), mk("H01", "hard")]);
    let ids: Vec<_> = v.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["H01", "H02", "E1", "S01"]);
    let all = case::load_dir(&case::evals_dir().join("cases")).unwrap();
    let first_smoke = all.iter().position(|c| c.tier == "smoke").unwrap();
    assert!(all[..first_smoke].iter().all(|c| c.tier != "smoke"));
    assert!(all.iter().position(|c| c.id.starts_with('H')).unwrap() < all.iter().position(|c| c.id.starts_with('S')).unwrap());
}

#[test]
fn oracle_tower_numbers_are_exact() {
    let ed = editor();
    tower(&ed);
    let toml = r#"
id = "t"
tier = "hard"
[[turn]]
say = "tower"
[[oracle.geom]]
kind = "height"
min = 270.0
max = 330.0
[[oracle.geom]]
kind = "taper"
min = 3.0
[[oracle.geom]]
kind = "curve_dev"
min = 0.03
[[oracle.geom]]
kind = "legs"
min = 4.0
[[oracle.geom]]
kind = "no_dup_members"
[[oracle.geom]]
kind = "free_ends"
max = 0.0
[[oracle.count]]
spec = "brace"
min = 120.0
max = 120.0
[[oracle.count]]
spec = "beam"
min = 124.0
max = 124.0
"#;
    let rs = run_case(&ed, toml, "");
    assert!(rs.iter().all(|r| r.pass), "{rs:?}");
    assert_eq!(one(&rs, "geom height").measured, "300.00 m");
    assert_eq!(one(&rs, "geom legs").measured, "4 legs");
    // 30 / r(250) with r(250) = 8 + 22/36
    let facts = Facts::read(&ed.model).unwrap();
    let (r, taper, dev) = super::geom_oracles::profile(&facts).unwrap();
    assert!((r[0] - 30.0).abs() < 1e-9 && (r[5] - tower_r(250.0)).abs() < 1e-9, "{r:?}");
    assert!((taper - 30.0 / tower_r(250.0)).abs() < 1e-9, "{taper}");
    assert!((dev - 11.0 / 3.0 / 30.0).abs() < 1e-3, "{dev}");
}

#[test]
fn oracle_tower_too_straight_fails() {
    let ed = editor();
    let (lv, mt) = (level(&ed), member_type(&ed, "leg"));
    // four straight legs, 30 m wide all the way: no taper, no curvature
    for k in 0..30 {
        let z = 10.0 * k as f64;
        for (cx, cy) in [(1.0, 1.0), (-1.0, 1.0), (-1.0, -1.0), (1.0, -1.0)] {
            brace(&ed, lv, mt, [cx * 30.0, cy * 30.0, z], [cx * 30.0, cy * 30.0, z + 10.0]);
        }
    }
    let rs = run_case(
        &ed,
        "id=\"t\"\n[[turn]]\nsay=\"x\"\n[[oracle.geom]]\nkind=\"taper\"\nmin=3.0\n[[oracle.geom]]\nkind=\"curve_dev\"\nmin=0.03\n",
        "",
    );
    assert!(!one(&rs, "geom taper").pass, "{rs:?}");
    assert_eq!(one(&rs, "geom taper").measured, "1.000");
    assert!(!one(&rs, "geom curve_dev").pass);
}

#[test]
fn oracle_free_end_and_duplicate_are_named() {
    let ed = editor();
    let (lv, mt) = (level(&ed), member_type(&ed, "m"));
    column(&ed, lv, mt, 0.0, 0.0, 5.0);
    column(&ed, lv, mt, 10.0, 0.0, 5.0);
    beam(&ed, lv, mt, [0.0, 0.0, 5.0], [10.0, 0.0, 5.0]);
    beam(&ed, lv, mt, [10.0, 0.0, 5.0], [10.0, 8.0, 5.0]);
    let toml = "id=\"t\"\n[[turn]]\nsay=\"x\"\n[[oracle.geom]]\nkind=\"free_ends\"\nmax=0.0\n[[oracle.geom]]\nkind=\"no_dup_members\"\n";
    let rs = run_case(&ed, toml, "");
    assert_eq!(one(&rs, "geom free_ends").measured, "1 free of 8 ends");
    assert!(!one(&rs, "geom free_ends").pass);
    assert!(one(&rs, "geom no_dup_members").pass);
    // the same beam again, drawn the other way round
    beam(&ed, lv, mt, [10.0, 0.0, 5.0], [0.0, 0.0, 5.0]);
    let rs = run_case(&ed, toml, "");
    assert_eq!(one(&rs, "geom no_dup_members").measured, "1 duplicated");
    assert!(!one(&rs, "geom no_dup_members").pass);
    let ok = run_case(&ed, &toml.replace("max=0.0", "max=1.0"), "");
    assert!(one(&ok, "geom free_ends").pass);
}

#[test]
fn missing_capability_fails() {
    let ed = editor();
    let toml = r#"
id = "t"
[[turn]]
say = "x"
[[oracle.count]]
spec = "gridshell_nonexistent"
min = 1.0
[[oracle.geom]]
kind = "double_curve"
min = 0.05
[[oracle.geom]]
kind = "no_such_oracle"
[[oracle.claim]]
name = "plates"
say = "plates"
have = "no_such_table"
min = 20.0
"#;
    let rs = run_case(&ed, toml, "bolts and plates");
    assert_eq!(rs.len(), 4);
    for r in &rs {
        assert!(!r.pass, "{r:?}");
    }
    assert!(one(&rs, "count gridshell_nonexistent").measured.contains("no capability"));
    assert!(one(&rs, "geom double_curve").measured.contains("no shell nodes"));
    assert!(one(&rs, "geom no_such_oracle").measured.contains("unknown oracle kind"));
}

#[test]
fn claim_and_text_and_ifc_oracles() {
    let ed = editor();
    let toml = r#"
id = "t"
[[turn]]
say = "x"
[[oracle.text]]
name = "sway"
re = "(sway|drift)[^\\n]{0,40}\\d"
[[oracle.claim]]
name = "plates"
say = "\\b(bolts?|plates?)\\b"
have = "steel_conn_part"
min = 20.0
[oracle.ifc.classes_min]
"IfcColumn" = 2
"IfcDuctSegment|IfcPipeSegment" = 1
"#;
    let c = Case::parse(toml).unwrap();
    assert_eq!(build_oracles(&c).len(), 3);
    let facts = Facts::read(&ed.model).unwrap();
    let step = "#1=IFCCOLUMN('a');#2=IFCCOLUMN('b');#3=IFCPIPESEGMENT('c');";
    let at = |answer: &str, ifc: Option<&str>| {
        let cx = EvalCx { ed: &ed, facts: &facts, before: None, answer, ifc, errors_before: 0, receipts: &[] };
        evaluate(&c, &cx)
    };
    let rs = at("Top sway is 0.8 m. We use plates.", Some(step));
    assert!(one(&rs, "text sway").pass);
    assert!(!one(&rs, "claim plates").pass, "claims plates, the model has none: {rs:?}");
    assert!(one(&rs, "ifc classes").pass, "{rs:?}");
    assert_eq!(one(&rs, "ifc classes").measured, "IfcColumn=2, IfcDuctSegment|IfcPipeSegment=1");
    let rs = at("no number here", Some("#1=IFCCOLUMN('a');"));
    assert!(!one(&rs, "text sway").pass);
    assert!(one(&rs, "claim plates").pass, "nothing claimed");
    assert!(!one(&rs, "ifc classes").pass);
    assert!(!at("", None).iter().find(|r| r.name == "ifc classes").unwrap().pass);
}

#[test]
fn edit_oracles_see_only_new_members() {
    let ed = editor();
    let (lv, mt) = (level(&ed), member_type(&ed, "m"));
    for k in 0..4 {
        brace(&ed, lv, mt, [0.0, 0.0, 10.0 * k as f64], [0.0, 0.0, 10.0 * (k + 1) as f64]);
    }
    let before = Facts::read(&ed.model).unwrap();
    brace(&ed, lv, mt, [0.0, 0.0, 195.0], [0.0, 0.0, 205.0]);
    let toml = "id=\"t\"\n[[turn]]\nsay=\"x\"\n[[oracle.geom]]\nkind=\"edit_kept\"\nmin=1.0\n[[oracle.geom]]\nkind=\"new_within\"\nzmin=195.0\nzmax=205.0\n[[oracle.grow]]\nspec=\"brace\"\nmin=1.0\nmax=1.0\n";
    let c = Case::parse(toml).unwrap();
    let after = Facts::read(&ed.model).unwrap();
    let cx = EvalCx { ed: &ed, facts: &after, before: Some(&before), answer: "", ifc: None, errors_before: 0, receipts: &[] };
    let rs = evaluate(&c, &cx);
    assert!(rs.iter().all(|r| r.pass), "{rs:?}");
    assert_eq!(one(&rs, "geom new_within").measured, "1 new, 0 outside the band");
    // one more new member at the wrong height
    brace(&ed, lv, mt, [0.0, 0.0, 50.0], [0.0, 0.0, 60.0]);
    let after = Facts::read(&ed.model).unwrap();
    let cx = EvalCx { ed: &ed, facts: &after, before: Some(&before), answer: "", ifc: None, errors_before: 0, receipts: &[] };
    let rs = evaluate(&c, &cx);
    assert!(!one(&rs, "geom new_within").pass);
    assert_eq!(one(&rs, "geom new_within").measured, "2 new, 1 outside the band");
}

fn shell_facts(z: impl Fn(f64, f64) -> f64) -> Facts {
    let mut f = Facts::default();
    f.tables.insert("gridshell_node".into());
    f.rows.insert("gridshell_node".into(), 121);
    for i in 0..=10 {
        for j in 0..=10 {
            let (x, y) = (-10.0 + 2.0 * i as f64, -10.0 + 2.0 * j as f64);
            f.shell_pts.push([x, y, z(x, y)]);
        }
    }
    f
}

#[test]
fn oracle_double_curve_needs_both_directions() {
    let ed = editor();
    let c = Case::parse("id=\"t\"\n[[turn]]\nsay=\"x\"\n[[oracle.geom]]\nkind=\"double_curve\"\nmin=0.05\n").unwrap();
    let check = |f: &Facts| {
        let cx = EvalCx { ed: &ed, facts: f, before: None, answer: "", ifc: None, errors_before: 0, receipts: &[] };
        evaluate(&c, &cx).remove(0)
    };
    let saddle = check(&shell_facts(|x, y| (x * x - y * y) / 10.0));
    assert!(saddle.pass, "{saddle:?}");
    let barrel = check(&shell_facts(|x, _| x * x / 10.0));
    assert!(!barrel.pass, "bent in one direction only: {barrel:?}");
    let flat = check(&shell_facts(|_, _| 3.0));
    assert!(!flat.pass, "{flat:?}");
    assert!(flat.measured.starts_with("along x 0.000, along y 0.000"), "{}", flat.measured);
}

#[test]
fn oracle_slab_at_reads_the_height_of_slabs() {
    let ed = editor();
    let c = Case::parse("id=\"t\"\n[[turn]]\nsay=\"x\"\n[[oracle.geom]]\nkind=\"slab_at\"\nzmin=195.0\nzmax=205.0\nmin=1.0\n").unwrap();
    let mut f = Facts::default();
    f.slab_z = vec![57.0, 115.0, 276.0];
    let at = |f: &Facts| {
        let cx = EvalCx { ed: &ed, facts: f, before: None, answer: "", ifc: None, errors_before: 0, receipts: &[] };
        evaluate(&c, &cx).remove(0)
    };
    let r = at(&f);
    assert!(!r.pass);
    assert_eq!(r.measured, "0 slab(s) at z 195..205");
    f.slab_z.push(200.0);
    assert!(at(&f).pass);
}

#[test]
fn hard_structure_cases_are_complete() {
    let dir = case::evals_dir().join("cases");
    let get = |id: &str| Case::load(&dir.join(format!("{id}.toml"))).unwrap_or_else(|e| panic!("{e}"));
    let kinds = |c: &Case| c.oracle.geom.iter().map(|g| g.kind.clone()).collect::<Vec<_>>();
    let h01 = get("H01_lattice_tower_300");
    for k in ["height", "taper", "curve_dev", "legs", "no_dup_members", "free_ends"] {
        assert!(kinds(&h01).contains(&k.to_string()), "H01 lacks {k}");
    }
    assert!(h01.oracle.checks.is_some() && h01.oracle.ifc.is_some());
    assert!(!h01.oracle.claim.is_empty() && !h01.oracle.text.is_empty());
    let h06 = get("H06_museum_freeform");
    assert!(kinds(&h06).contains(&"double_curve".to_string()));
    let h14 = get("H14_tower_add_floor");
    assert_eq!(h14.base, "@H01_lattice_tower_300");
    for k in ["slab_at", "edit_kept", "new_within"] {
        assert!(kinds(&h14).contains(&k.to_string()), "H14 lacks {k}");
    }
    let h16 = get("H16_hangar_space_frame");
    assert!(kinds(&h16).contains(&"free_ends".to_string()));
    for c in [&h01, &h06, &h14, &h16] {
        assert_eq!(c.tier, "hard", "{}", c.id);
    }
}

#[test]
fn an_inclined_column_ends_at_its_top_point() {
    let ed = editor();
    let lv = level(&ed);
    let mt = member_type(&ed, "SHS 300x300x12.5");
    sql(&ed, &format!("INSERT INTO col(level, mtype, structural, x, y, base_offset, top_offset, rotation, tx, ty) VALUES({lv}, {mt}, 1, 10, 10, 0, 4, 0, 8, 9)"));
    sql(&ed, &format!("INSERT INTO col(level, mtype, structural, x, y, base_offset, top_offset, rotation) VALUES({lv}, {mt}, 1, 0, 0, 0, 4, 0)"));
    let f = Facts::read(&ed.model).unwrap();
    let ends: Vec<([f64; 2], [f64; 2])> = f.members.iter().filter(|m| m.kind == "col").map(|m| ([m.a[0], m.a[1]], [m.b[0], m.b[1]])).collect();
    assert!(ends.contains(&([10.0, 10.0], [8.0, 9.0])), "{ends:?}");
    assert!(ends.contains(&([0.0, 0.0], [0.0, 0.0])), "{ends:?}");
}
