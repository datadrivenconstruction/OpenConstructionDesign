//! Shape oracles on hand-built kernels: a braced tower, a plant line, a data hall, a shell.

use super::case::Case;
use super::facts::{Comp, Facts, Member, Run, P3};
use super::oracle::{evaluate, EvalCx, OracleResult};
use super::tests::editor;

fn check(f: &Facts, toml: &str) -> Vec<OracleResult> {
    let ed = editor();
    let c = Case::parse(&format!("id=\"t\"\n[[turn]]\nsay=\"x\"\n{toml}")).unwrap();
    let cx = EvalCx { ed: &ed, facts: f, before: None, answer: "", ifc: None, errors_before: 0, receipts: &[] };
    evaluate(&c, &cx)
}

fn geom(kind: &str, params: &str) -> String {
    format!("[[oracle.geom]]\nkind=\"{kind}\"\n{params}\n")
}

fn member(kind: &'static str, a: P3, b: P3) -> Member {
    Member { kind, a, b, mtype: None, bulge: 0.0 }
}

const CORNERS: [[f64; 2]; 4] = [[-5.0, -5.0], [5.0, -5.0], [5.0, 5.0], [-5.0, 5.0]];

/// Four legs 60 m high; every 10 m each face is braced: one diagonal, or an X with both.
fn tower(both: bool) -> Facts {
    let mut f = Facts::default();
    for c in CORNERS {
        f.members.push(member("col", [c[0], c[1], 0.0], [c[0], c[1], 60.0]));
    }
    for band in 0..6 {
        let (z0, z1) = (band as f64 * 10.0, band as f64 * 10.0 + 10.0);
        for i in 0..4 {
            let (p, q) = (CORNERS[i], CORNERS[(i + 1) % 4]);
            f.members.push(member("brace", [p[0], p[1], z0], [q[0], q[1], z1]));
            if both {
                f.members.push(member("brace", [q[0], q[1], z0], [p[0], p[1], z1]));
            }
        }
    }
    f.points = f.members.iter().flat_map(|m| [m.a, m.b]).collect();
    f
}

#[test]
fn height_must_reach_the_target_less_the_tolerance() {
    let mut f = Facts::default();
    f.points = vec![[0.0, 0.0, 0.0], [0.0, 0.0, 294.5]];
    let r = check(&f, &geom("height_target", "target=300.0"));
    assert!(r[0].pass, "{r:?}");
    f.points[1][2] = 293.0;
    let r = check(&f, &geom("height_target", "target=300.0"));
    assert!(!r[0].pass && r[0].measured == "293.00 m" && r[0].expected.starts_with(">= 294.0 m"), "{r:?}");
    assert!(check(&Facts::default(), &geom("height_target", "target=300.0"))[0].measured.contains("nothing built"));
}

#[test]
fn a_tower_that_hangs_together_is_one_part_and_a_stray_member_is_named() {
    let mut f = tower(true);
    let r = check(&f, &geom("members_connected", ""));
    assert!(r[0].pass && r[0].measured == "1 part", "{r:?}");
    f.members.push(member("beam", [40.0, 40.0, 5.0], [50.0, 40.0, 5.0]));
    let stray = f.members.len() - 1;
    let r = check(&f, &geom("members_connected", ""));
    assert!(!r[0].pass && r[0].measured == format!("2 separate parts, smallest holds member {stray}"), "{r:?}");
    assert!(check(&Facts::default(), &geom("members_connected", ""))[0].measured == "no members");
}

#[test]
fn crossed_bracing_passes_one_lean_and_none_fail() {
    let r = check(&tower(true), &geom("bracing", ""));
    assert!(r[0].pass, "{r:?}");
    assert_eq!(r[0].measured, "48 diagonals, 24 of 24 braced faces cross (1.00)");
    let r = check(&tower(false), &geom("bracing", ""));
    assert!(!r[0].pass && r[0].measured == "24 diagonals, 0 of 24 braced faces cross (0.00)", "{r:?}");
    let mut bare = tower(true);
    bare.members.retain(|m| m.kind != "brace");
    let r = check(&bare, &geom("bracing", ""));
    assert!(!r[0].pass && r[0].measured.starts_with("0 diagonals"), "{r:?}");
}

fn comp(id: i64, family: &str, at: P3) -> Comp {
    Comp { id, level: 1, at, family: family.into(), category: "equipment".into() }
}

fn run(id: i64, pts: &[P3]) -> Run {
    Run {
        id,
        system: "process".into(),
        level: 1,
        domain: "pipe".into(),
        shape: "round".into(),
        w: 0.1,
        h: 0.1,
        dn: Some(100),
        slope: 0.0,
        pts: pts.to_vec(),
        cap: [false, false],
    }
}

#[test]
fn equipment_is_counted_by_kind_and_the_short_kind_is_named() {
    let mut f = Facts::default();
    f.comps = vec![comp(1, "Vessel V-1", [0.0; 3]), comp(2, "Vessel V-2", [5.0, 0.0, 0.0]), comp(3, "Pump P-1", [9.0, 0.0, 0.0])];
    let kinds = |k: &str| geom("equipment_by_kind", &format!("kinds=\"{k}\""));
    let r = check(&f, &kinds("vessel|tank:2;pump:1"));
    assert!(r[0].pass && r[0].measured == "vessel|tank 2/2, pump 1/1", "{r:?}");
    let r = check(&f, &kinds("vessel:2;pump:2"));
    assert!(!r[0].pass && r[0].measured == "vessel 2/2, pump 1/2", "{r:?}");
    assert!(!check(&f, &kinds("vessel"))[0].pass);
}

#[test]
fn a_pipe_line_must_join_two_nozzles() {
    let mut f = Facts::default();
    f.tables.insert("mep_run".into());
    f.runs = vec![run(1, &[[0.0, 0.0, 3.0], [10.0, 0.0, 3.0]]), run(2, &[[10.0, 0.0, 3.0], [10.0, 8.0, 3.0]])];
    f.comps = vec![comp(1, "Nozzle N1", [0.0, 0.0, 3.0]), comp(2, "Nozzle N2", [10.0, 8.0, 3.0])];
    let q = geom("pipe_nozzles", "");
    let r = check(&f, &q);
    assert!(r[0].pass && r[0].measured == "1 of 1 pipe lines join two nozzles (1.00)", "{r:?}");
    f.comps.pop();
    let r = check(&f, &q);
    assert!(!r[0].pass && r[0].measured.ends_with("line of run 1 reaches 1 nozzle(s)"), "{r:?}");
    // a gap in the line leaves two lines, neither joining two nozzles
    f.comps.push(comp(2, "Nozzle N2", [10.0, 8.0, 3.0]));
    f.runs[1].pts[0] = [11.0, 0.0, 3.0];
    let r = check(&f, &q);
    assert!(!r[0].pass && r[0].measured.starts_with("0 of 2 pipe lines"), "{r:?}");
    assert_eq!(check(&Facts::default(), &q)[0].measured, "no pipe runs");
}

/// Rows of five racks at the given y; each has a rack comp.
fn hall(ys: &[f64], containment: bool) -> Facts {
    let mut f = Facts::default();
    let mut id = 0;
    for y in ys {
        for k in 0..5 {
            id += 1;
            f.comps.push(comp(id, "Rack 42U", [k as f64, *y, 0.0]));
        }
    }
    if containment {
        f.comps.push(comp(900, "Aisle containment door", [2.0, 2.0, 0.0]));
    }
    f
}

#[test]
fn racks_back_to_back_with_contained_aisles_pass() {
    let q = geom("aisle_containment", "");
    let ok = hall(&[0.0, 1.2, 4.2, 5.4, 8.4, 9.6], true);
    let r = check(&ok, &q);
    assert!(r[0].pass, "{r:?}");
    assert_eq!(r[0].measured, "30 racks in 6 rows, 2 aisle(s) between back-to-back pairs, 1 containment element(s)");
    let r = check(&hall(&[0.0, 1.2, 4.2, 5.4, 8.4, 9.6], false), &q);
    assert!(!r[0].pass && r[0].measured.ends_with("0 containment element(s)"), "{r:?}");
    let r = check(&hall(&[0.0, 3.0, 6.0, 9.0], true), &q);
    assert!(!r[0].pass && r[0].measured.contains("do not alternate"), "{r:?}");
}

fn shell(z: impl Fn(f64, f64) -> f64) -> Facts {
    let mut f = Facts::default();
    f.tables.insert("gridshell_node".into());
    f.rows.insert("gridshell_node".into(), 441);
    for i in 0..=20 {
        for j in 0..=20 {
            let (x, y) = (-10.0 + i as f64, -10.0 + j as f64);
            f.shell_pts.push([x, y, z(x, y)]);
        }
    }
    f
}

#[test]
fn gaussian_curvature_is_not_zero_on_a_dome_or_a_saddle_only() {
    let q = geom("gaussian_curve", "");
    assert!(check(&shell(|x, y| 30.0 - (x * x + y * y) / 4.0), &q)[0].pass);
    assert!(check(&shell(|x, y| (x * x - y * y) / 4.0), &q)[0].pass);
    let cyl = check(&shell(|x, _| 30.0 - x * x / 4.0), &q);
    assert!(!cyl[0].pass && cyl[0].measured.starts_with("0 of 25 samples curved"), "{cyl:?}");
    assert!(!check(&shell(|x, _| 0.2 * x), &q)[0].pass);
    let none = check(&Facts::default(), &q);
    assert!(!none[0].pass && none[0].measured.contains("no gridshell nodes"), "{none:?}");
}

#[test]
fn hard_cases_carry_the_shape_checks() {
    let dir = super::case::evals_dir().join("cases");
    let kinds = |id: &str| {
        let c = Case::load(&dir.join(format!("{id}.toml"))).unwrap_or_else(|e| panic!("{e}"));
        c.oracle.geom.iter().map(|g| g.kind.clone()).collect::<Vec<_>>()
    };
    for (id, want) in [
        ("H01_lattice_tower_300", vec!["height_target", "members_connected", "bracing"]),
        ("H03_process_plant", vec!["equipment_by_kind", "pipe_nozzles", "mep_free_ends"]),
        ("H07_datacenter", vec!["aisle_containment"]),
        ("H06_museum_freeform", vec!["gaussian_curve"]),
    ] {
        let have = kinds(id);
        for k in want {
            assert!(have.contains(&k.to_string()), "{id} lacks {k}");
        }
    }
}

#[test]
fn one_braced_band_cannot_pass_the_bracing_check() {
    let mut f = tower(true);
    // keep a single band of braces: 8 diagonals, every face crossed, but only 4 faces braced
    f.members.retain(|m| m.kind != "brace" || m.a[2].min(m.b[2]) < 1.0);
    let r = check(&f, &geom("bracing", ""));
    assert!(!r[0].pass && r[0].measured.contains("of 4 braced faces cross"), "{r:?}");
    assert!(r[0].expected.contains(">= 24 faces"), "{r:?}");
}

#[test]
fn pipe_ends_count_once_and_ports_count_as_nozzles() {
    let mut f = Facts::default();
    f.runs = vec![run(1, &[[0.0, 0.0, 3.0], [10.0, 0.0, 3.0]])];
    // vessels carry ports; their family names say nothing about nozzles
    f.comps = vec![comp(1, "Vessel V-1", [0.0, 0.0, 3.0]), comp(2, "Pump P-1", [10.0, 0.0, 3.0])];
    f.ports = vec![[0.0, 0.0, 3.0], [10.0, 0.0, 3.0]];
    let r = check(&f, &geom("pipe_nozzles", ""));
    assert!(r[0].pass && r[0].measured == "1 of 1 pipe lines join two nozzles (1.00)", "{r:?}");
    // three connectors at one end and none at the other: one end, not two
    f.comps = vec![comp(1, "Vessel V-1", [0.0, 0.0, 3.0]), comp(3, "Nozzle N1", [0.1, 0.0, 3.0]), comp(4, "Nozzle N2", [0.0, 0.2, 3.0])];
    f.ports = vec![[0.0, 0.0, 3.0]];
    let r = check(&f, &geom("pipe_nozzles", ""));
    assert!(!r[0].pass && r[0].measured.ends_with("line of run 1 reaches 1 nozzle(s)"), "{r:?}");
    // a joint between two runs is not an end: the T has one loose end on equipment
    f.runs.push(run(2, &[[5.0, 0.0, 3.0], [5.0, 6.0, 3.0]]));
    f.comps.push(comp(5, "Pump P-2", [5.0, 6.0, 3.0]));
    f.ports.push([5.0, 6.0, 3.0]);
    let r = check(&f, &geom("pipe_nozzles", ""));
    assert!(r[0].pass && r[0].measured.starts_with("1 of 1"), "{r:?}");
}

#[test]
fn only_shell_nodes_are_measured_for_curvature_not_columns_or_pipes() {
    let mut f = shell(|_, _| 3.0);
    let flat = check(&f, &geom("gaussian_curve", ""));
    assert!(!flat[0].pass, "{flat:?}");
    // tall columns and a pipe far above the flat shell change nothing
    f.points.extend([[-10.0, -10.0, 40.0], [10.0, 10.0, -5.0], [0.0, 0.0, 90.0]]);
    f.members.push(member("col", [0.0, 0.0, 0.0], [0.0, 0.0, 50.0]));
    let again = check(&f, &geom("gaussian_curve", ""));
    assert_eq!(again[0].measured, flat[0].measured);
}

#[test]
fn the_model_reader_keeps_pipes_out_of_the_height_and_the_shell() {
    let ed = editor();
    let lv = super::tests::level(&ed);
    super::tests::sql(
        &ed,
        &format!(
            "INSERT INTO mep_system(name, kind, color) VALUES('CW 1', 'cold_water', 255); \
             INSERT INTO mep_run(system, level, domain, shape, w, h, wall, insulation, lining, slope, points, cap0, cap1) \
             VALUES((SELECT id FROM mep_system WHERE name = 'CW 1'), {lv}, 'pipe', 'round', 0.1, 0.1, 0.001, 0, 0, 0, '0 0 0;0 0 400', 0, 0)"
        ),
    );
    let f = Facts::read(&ed.model).unwrap_or_default();
    assert!(f.points.iter().all(|p| p[2] < 100.0) && f.shell_pts.is_empty(), "{:?}", f.points);
}

#[test]
fn bases_and_platforms_belong_to_the_connected_structure() {
    let mut f = tower(true);
    f.bases = CORNERS.iter().map(|c| [c[0], c[1], 0.0]).collect();
    f.comps.push(comp(9, "Platform deck", [5.0, 5.0, 30.0]));
    let r = check(&f, &geom("members_connected", ""));
    assert!(r[0].pass && r[0].measured == "1 part", "{r:?}");
    // a footing that touches no member, and a platform floating off the tower, are parts of their own
    f.bases.push([40.0, 40.0, 0.0]);
    let r = check(&f, &geom("members_connected", ""));
    assert!(!r[0].pass && r[0].measured == "2 separate parts, base 4 touches no member", "{r:?}");
    f.bases.pop();
    f.comps[0].at = [50.0, 50.0, 30.0];
    let r = check(&f, &geom("members_connected", ""));
    assert!(!r[0].pass && r[0].measured.contains("separate parts, base 4"), "{r:?}");
}

#[test]
fn a_nozzle_on_a_large_vessel_is_found_at_its_port_not_at_the_origin() {
    // a 4 m vessel centred at x = 2: its origin is 2 m from the nozzle face at x = 0
    let mut f = Facts::default();
    f.runs = vec![run(1, &[[0.0, 0.0, 3.0], [10.0, 0.0, 3.0]])];
    f.comps = vec![comp(1, "Reactor R-1", [2.0, 0.0, 3.0]), comp(2, "Pump P-1", [10.0, 0.0, 3.0])];
    f.port_comps = [1, 2].into_iter().collect();
    f.ports = vec![[10.0, 0.0, 3.0]];
    let r = check(&f, &geom("pipe_nozzles", ""));
    assert!(!r[0].pass && r[0].measured.ends_with("line of run 1 reaches 1 nozzle(s)"), "{r:?}");
    f.ports.push([0.0, 0.0, 3.0]);
    let r = check(&f, &geom("pipe_nozzles", ""));
    assert!(r[0].pass, "{r:?}");
    // a port that is not at either end of the line does not count
    f.ports[1] = [2.0, 0.0, 3.0];
    assert!(!check(&f, &geom("pipe_nozzles", ""))[0].pass);
}

#[test]
fn a_hypar_is_a_saddle_a_dome_and_a_flat_sheet_are_not() {
    let hypar = shell(|x, y| (x * x - y * y) / 10.0);
    let r = check(&hypar, &geom("saddle_curve", ""));
    assert!(r[0].pass && r[0].measured.contains("saddle-shaped"), "{r:?}");
    let dome = shell(|x, y| -(x * x + y * y) / 10.0);
    let r = check(&dome, &geom("saddle_curve", ""));
    assert!(!r[0].pass, "{r:?}");
    assert!(!check(&shell(|_, _| 2.0), &geom("saddle_curve", ""))[0].pass);
    assert!(check(&Facts::default(), &geom("saddle_curve", ""))[0].measured.contains("no capability"));
}

#[test]
fn a_four_corner_hypar_is_told_by_its_warp() {
    let mut f = Facts::default();
    // low corners on one diagonal, high on the other: rise 5 m
    f.shell_pts = vec![[0.0, 0.0, 0.0], [20.0, 20.0, 0.0], [20.0, 0.0, 5.0], [0.0, 20.0, 5.0]];
    let r = check(&f, &geom("saddle_curve", ""));
    assert!(r[0].pass && r[0].measured == "corner warp 5.00 m", "{r:?}");
    // a tilted plane has the same corner spread but no warp
    f.shell_pts = vec![[0.0, 0.0, 0.0], [20.0, 20.0, 10.0], [20.0, 0.0, 5.0], [0.0, 20.0, 5.0]];
    assert!(!check(&f, &geom("saddle_curve", ""))[0].pass);
}

fn sweep_facts(def: serde_json::Value) -> Facts {
    let mut f = Facts::default();
    f.tables.insert("solid".into());
    f.sweeps.extend(super::facts::sweep_of(7, &def));
    f
}

#[test]
fn a_sculpture_must_turn_and_scale_along_its_path() {
    let q = geom("sweep_law", "turns_min=1.3\nscale_min=3.0\nrise_min=4.0");
    let ring = serde_json::json!([[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]);
    let helix = |angle: f64, scale: serde_json::Value| {
        serde_json::json!({
            "kind": "sweep", "profile": {"outer": ring, "holes": []}, "path": [],
            "smooth": {"start": [3.0, 0.0, 0.0], "segs": [{"kind": "helix", "center": [0.0, 0.0, 0.0], "axis": [0.0, 0.0, 1.0], "angle": angle, "rise": 5.0}],
                       "frame": "rmf", "roll": 0.0, "twist": 0.0, "scale": scale, "role": "generic"}
        })
    };
    // one and a half turns, 0.3 -> 1.2, rising 5 m
    let ok = sweep_facts(helix(3.0 * std::f64::consts::PI, serde_json::json!({"kind": "linear", "a": 0.3, "b": 1.2})));
    let r = check(&ok, &q);
    assert!(r[0].pass && r[0].measured == "sweep 7: 1.50 turns, scale x4.00, rise 5.0 m", "{r:?}");
    // constant section: fails on scale; half a turn: fails on turns
    assert!(!check(&sweep_facts(helix(3.0 * std::f64::consts::PI, serde_json::json!(1.0))), &q)[0].pass);
    let r = check(&sweep_facts(helix(std::f64::consts::PI, serde_json::json!({"kind": "linear", "a": 0.3, "b": 1.2}))), &q);
    assert!(!r[0].pass && r[0].measured.contains("0.50 turns"), "{r:?}");
    // twist of the profile counts as turning
    let mut d = helix(0.0, serde_json::json!({"kind": "ease", "a": 0.3, "b": 1.2}));
    d["smooth"]["twist"] = serde_json::json!({"kind": "linear", "a": 0.0, "b": 3.0 * std::f64::consts::PI});
    assert!(check(&sweep_facts(d), &q)[0].pass);
    // a straight extrusion is no sweep; no solids at all is a no capability
    assert!(check(&sweep_facts(serde_json::json!({"kind": "extrusion"})), &q)[0].measured == "no swept solid");
    assert!(check(&Facts::default(), &q)[0].measured.contains("no capability"));
}

/// G28: the two legs of a V-column share their base; read with their top points they are two
/// members, not one duplicated.
#[test]
fn v_column_legs_are_two_members() {
    let mut ed = editor();
    let mt = ed.model.add_member_type("V-C600", &ddc_model::MemberShape::Rect { b: 0.6, h: 0.6 }, "Concrete").unwrap();
    let leg = |top: (f64, f64)| ddc_model::ColumnSpec {
        level: ed.level,
        top_level: None,
        member_type: mt,
        structural: true,
        at: (45.0, 0.0),
        base_offset: 0.0,
        top_offset: 12.0,
        rotation: 0.0,
        top_at: Some(top),
    };
    let specs = [leg((42.0, 3.0)), leg((42.0, -3.0))];
    ed.model.add_columns(&specs).unwrap();
    let f = Facts::read(&ed.model).unwrap();
    let cols: Vec<&Member> = f.members.iter().filter(|m| m.kind == "col").collect();
    assert_eq!(cols.len(), 2);
    assert_eq!((cols[0].b[0], cols[0].b[1]), (42.0, 3.0), "the leg ends at its top point");
    assert_eq!(super::geom_oracles::duplicates(&f.members), 0);
}

#[test]
fn swept_solids_are_measured_for_the_height_and_voids_are_not() {
    let ed = editor();
    let lv = super::tests::level(&ed);
    super::tests::sql(
        &ed,
        &format!(
            "INSERT INTO solid(id, level, kind, void, plane, def) VALUES \
             (9001, {lv}, 'sweep', 0, '{{}}', '{{}}'), (9002, {lv}, 'sweep', 0, '{{}}', '{{}}'), (9003, {lv}, 'extrusion', 1, '{{}}', '{{}}'); \
             INSERT INTO solid_qty VALUES (9001, 1, 1, 1, -1, -1, 0, 1, 1, 6, NULL), (9002, 1, 1, 1, 0, 0, 6, 2, 2, 12, NULL), \
             (9003, 1, 1, 1, 0, 0, 0, 1, 1, 40, NULL);"
        ),
    );
    let f = Facts::read(&ed.model).unwrap();
    let r = check(&f, &geom("height", "min=10.0\nmax=15.0"));
    assert!(r[0].pass && r[0].measured == "12.00 m", "{r:?}");
}

/// G18: one trunk, 4 generations of 3, ratio 0.7, 12 m, crown ring under the deck.
#[test]
fn a_tree_canopy_from_the_steel_generator_meets_the_fractal_tree_case() {
    let mut ed = editor();
    let lv = super::tests::level(&ed);
    let p = serde_json::json!({"kind": "tree", "height": 12.0, "generations": 4, "branches": 3, "ratio": 0.7});
    let (frame, _) = ddc_gen::steel::build_frame(&p, 1).unwrap();
    ed.model.add_steel_frame(lv, &frame).unwrap();
    let f = Facts::read(&ed.model).unwrap();
    assert!(f.count("col").unwrap() == 1 && f.count("brace").unwrap() == 120, "{}", f.count("col+brace").unwrap());
    assert!(f.count("col+beam+brace+solid").unwrap() >= 80);
    let r = check(&f, &format!("{}{}", geom("height", "min=10.0\nmax=15.0"), geom("no_dup_members", "")));
    let geo: Vec<&OracleResult> = r.iter().filter(|x| x.name.starts_with("geom")).collect();
    assert!(geo.len() == 2 && geo.iter().all(|x| x.pass), "{r:?}");
}

/// G19: the bowl and roof cover from a script are solids; the roof ring on masts from the
/// steel generator gives the structural members.
#[test]
fn a_stadium_ring_on_masts_meets_the_stadium_bowl_case() {
    use ddc_script::GeoScript;
    let mut ed = editor();
    let lv = super::tests::level(&ed);
    let src = ddc_script::recipes::recipe("stadium_roof").unwrap();
    let opts = ddc_script::RunOpts::new(ddc_ops::Ref::id(ddc_ops::Cat::Level, lv), serde_json::json!({}));
    let run = ddc_script::Sandbox.run(src, &opts).unwrap_or_else(|e| panic!("{e}"));
    assert!(ed.model.apply_envelope(&run.env).is_ok());
    let p = serde_json::json!({"kind": "stadium_ring", "length": 150.0, "width": 110.0, "roof_depth": 30.0, "height": 25.0, "masts": 16});
    let (frame, _) = ddc_gen::steel::build_frame(&p, 1).unwrap();
    ed.model.add_steel_frame(lv, &frame).unwrap();
    let f = Facts::read(&ed.model).unwrap();
    assert!(f.count("solid").unwrap() >= 40, "{}", f.count("solid").unwrap());
    assert_eq!(f.count("col").unwrap(), 32);
    assert!(f.count("col+beam+brace").unwrap() >= 50, "{}", f.count("col+beam+brace").unwrap());
    let r = check(&f, &geom("no_dup_members", ""));
    let geo: Vec<&OracleResult> = r.iter().filter(|x| x.name.starts_with("geom")).collect();
    assert!(geo.len() == 1 && geo[0].pass, "{r:?}");
}
