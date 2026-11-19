//! Exchange safety, IFC: every element family is exported, validated, read back and compared
//! (count, type, key properties, positions, materials), or it is listed here with the reason it
//! is not. A family that silently stops round-tripping fails with its name and the probe.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use ddc_io::ifc::{check_step, export_ifc_file, import_ifc};
use ddc_model::{
    BeamJoin, BeamSpec, CeilingSpec, ColumnSpec, CurtainSpec, GridLines, GridSpec, Just,
    LayerSpec, MemberShape, Model, MullionSection, OpeningSpec, RailingHost, RailingSpec,
    RampSpec, RoofKind, RoofSpec, RoomSpec, SlabSpec, StairSpec, WallSpec,
};
use rusqlite::types::ValueRef;

struct Ids {
    l1: i64,
    l2: i64,
    wall_type: i64,
    slab_type: i64,
}

type Build = fn(&mut Model, &Ids);

#[allow(dead_code)]
enum Outcome {
    /// Written as its own class and read back as the same native family; all probes equal.
    Native,
    /// Written, but read back as a generic mesh (reason); the element is never lost.
    Mesh(&'static str),
    /// Not written at all; the export lists every row as not written (reason).
    NotWritten(&'static str),
}

struct Family {
    name: &'static str,
    /// Model table that holds the family's rows.
    table: &'static str,
    /// IFC class the rows are written as (upper case, without the parenthesis).
    class: &'static str,
    build: Build,
    /// (label, SQL without ids): rows compared between the source and the re-imported model.
    probes: &'static [(&'static str, &'static str)],
    outcome: Outcome,
}

/// Tables the exporter tracks that this test has no fixture for yet, with the reason. The rows
/// are still reported by `ExportStats::not_written` when they are dropped.
const NOT_COVERED: &[(&str, &str)] = &[
    ("comp", "covered by tests/ifc_components.rs"),
    ("mep_run", "covered by tests/ifc_mep.rs"),
    ("zone", "covered by tests/ifc_ramp_zone.rs"),
    ("mesh", "generic meshes, covered by the import tests in src/ifc/import/tests_any.rs"),
    ("rebar_set", "covered by the export tests in src/ifc/export/rebar.rs"),
    ("solid", "written as a proxy; no fixture here yet"),
    ("mass", "no fixture here yet"),
    ("toposolid", "no fixture here yet"),
    ("topo_pad", "no fixture here yet"),
    ("skylight", "no fixture here yet"),
    ("soffit", "no fixture here yet"),
    ("sweep", "no fixture here yet"),
    ("iso_footing", "no fixture here yet"),
    ("strip_footing", "no fixture here yet"),
    ("raft", "no fixture here yet"),
    ("brace", "no fixture here yet"),
    ("truss", "no fixture here yet"),
    ("beam_system", "no fixture here yet"),
];

fn lay(material: &str, thickness: f64) -> LayerSpec {
    LayerSpec {
        material: material.into(),
        thickness,
    }
}

fn base() -> (Model, Ids) {
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("L1", 0.0).unwrap();
    let l2 = m.add_level("L2", 3.5).unwrap();
    let wall_type = m
        .add_wall_type("W200", &[lay("brick", 0.1), lay("insulation", 0.1)])
        .unwrap();
    let slab_type = m.add_slab_type("S250", &[lay("concrete", 0.25)]).unwrap();
    (
        m,
        Ids {
            l1,
            l2,
            wall_type,
            slab_type,
        },
    )
}

/// A closed 5 x 4 box of walls; returns the wall ids.
fn walls(m: &mut Model, i: &Ids) -> Vec<i64> {
    let p = [(0.0, 0.0), (5.0, 0.0), (5.0, 4.0), (0.0, 4.0)];
    (0..4)
        .map(|k| {
            m.add_wall(&WallSpec {
                a: p[k],
                b: p[(k + 1) % 4],
                level: i.l1,
                base_offset: 0.0,
                height: 3.0,
                wall_type: i.wall_type,
                offset0: None,
            })
            .unwrap()
        })
        .collect()
}

fn opening(kind: &str) -> OpeningSpec {
    OpeningSpec {
        along: 1.0,
        width: 0.9,
        sill: if kind == "door" { 0.0 } else { 0.9 },
        height: if kind == "door" { 2.1 } else { 1.2 },
        kind: kind.into(),
    }
}

fn b_wall(m: &mut Model, i: &Ids) {
    walls(m, i);
}

fn b_door(m: &mut Model, i: &Ids) {
    let w = walls(m, i);
    m.add_opening(w[0], &opening("door")).unwrap();
}

fn b_window(m: &mut Model, i: &Ids) {
    let w = walls(m, i);
    m.add_opening(w[1], &opening("window")).unwrap();
}

fn b_slab(m: &mut Model, i: &Ids) {
    m.add_slab(&SlabSpec {
        level: i.l1,
        slab_type: i.slab_type,
        offset: 0.0,
        outer: vec![(0.0, 0.0), (6.0, 0.0), (6.0, 5.0), (0.0, 5.0)],
        holes: vec![vec![(1.0, 1.0), (1.0, 2.0), (2.0, 2.0), (2.0, 1.0)]],
    })
    .unwrap();
}

fn b_room(m: &mut Model, i: &Ids) {
    walls(m, i);
    m.add_room(&RoomSpec {
        level: i.l1,
        name: "Hall".into(),
        number: "101".into(),
        point: (2.5, 2.0),
        height: 3.0,
    })
    .unwrap();
}

fn b_column(m: &mut Model, i: &Ids) {
    let t = m
        .add_member_type("C30", &MemberShape::Rect { b: 0.3, h: 0.4 }, "concrete")
        .unwrap();
    m.add_column(&ColumnSpec {
        level: i.l1,
        top_level: Some(i.l2),
        member_type: t,
        structural: true,
        at: (1.0, 1.0),
        base_offset: 0.0,
        top_offset: -0.4,
        rotation: 0.3,
        top_at: None,
    })
    .unwrap();
}

fn b_beam(m: &mut Model, i: &Ids) {
    let t = m
        .add_member_type(
            "I300",
            &MemberShape::I {
                h: 0.3,
                b: 0.15,
                tw: 0.007,
                tf: 0.01,
            },
            "steel",
        )
        .unwrap();
    m.add_beam(&BeamSpec {
        level: i.l1,
        member_type: t,
        a: (0.0, 0.0, 3.0),
        b: (4.0, 0.5, 3.0),
        bulge: 0.0,
        yjust: Just::Center,
        zjust: Just::Pos,
        rotation: 0.0,
        start_ext: 0.0,
        end_ext: 0.1,
        join_start: BeamJoin::Auto,
        join_end: BeamJoin::Butt,
    })
    .unwrap();
}

fn ring() -> Vec<(f64, f64)> {
    vec![(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)]
}

fn b_roof(m: &mut Model, i: &Ids) {
    m.add_roof(&RoofSpec {
        level: i.l1,
        slab_type: i.slab_type,
        offset: 3.0,
        overhang: 0.3,
        fascia: 0.0,
        kind: RoofKind::Footprint {
            ring: ring(),
            slopes: vec![Some(30.0), None, Some(30.0), None],
            mansard: None,
        },
    })
    .unwrap();
}

fn b_ceiling(m: &mut Model, i: &Ids) {
    m.add_ceiling(&CeilingSpec {
        level: i.l1,
        slab_type: i.slab_type,
        height: 2.7,
        outer: ring(),
        holes: vec![vec![(1.0, 1.0), (1.0, 2.0), (2.0, 2.0), (2.0, 1.0)]],
    })
    .unwrap();
}

fn b_curtain(m: &mut Model, i: &Ids) {
    let sec = MullionSection::Rect { w: 0.05, d: 0.1 };
    m.add_curtain_wall(&CurtainSpec {
        level: i.l1,
        name: "CW".into(),
        a: (0.0, 6.0),
        b: (4.0, 6.0),
        bulge: 0.0,
        base_offset: 0.0,
        height: 3.0,
        u: GridLines::Spacing(1.0),
        v: GridLines::Spacing(1.5),
        interior: sec,
        border: sec,
        lines: Default::default(),
        top: vec![],
    })
    .unwrap();
}

fn b_grid(m: &mut Model, _: &Ids) {
    m.add_grid(&GridSpec {
        name: Some("A".into()),
        a: (0.0, 0.0),
        b: (10.0, 0.0),
        bulge: 0.0,
        base_level: None,
        top_level: None,
        bubble_start: true,
        bubble_end: false,
    })
    .unwrap();
}

fn b_ramp(m: &mut Model, i: &Ids) {
    m.add_ramp(&RampSpec::new(i.l1, Some(i.l2), vec![(0.0, 0.0), (6.0, 0.0)]))
        .unwrap();
}

fn b_stair(m: &mut Model, i: &Ids) {
    m.add_stair(&StairSpec::straight(i.l1, i.l2, (0.0, 0.0), 0.0))
        .unwrap();
}

fn b_railing(m: &mut Model, i: &Ids) {
    m.add_railing(&RailingSpec {
        level: i.l1,
        host: RailingHost::Path(vec![(0.0, 0.0, 0.0), (3.0, 0.0, 0.0)]),
        height: 1.0,
        post_spacing: 1.2,
        baluster_spacing: 0.12,
        post: 0.05,
        baluster: 0.02,
        rail: (0.05, 0.04),
        material: "Steel".into(),
    })
    .unwrap();
}

const MEMBER: (&str, &str) = (
    "member types and materials",
    "SELECT name, shape, material FROM member_type",
);

const FAMILIES: &[Family] = &[
    Family {
        name: "wall",
        table: "wall",
        class: "IFCWALL",
        build: b_wall,
        probes: &[
            (
                "position, height, level, type",
                "SELECT l.name, t.name, w.ax, w.ay, w.bx, w.by, w.base_offset, w.height, w.bulge \
                 FROM wall w JOIN level l ON l.id=w.level JOIN wall_type t ON t.id=w.wtype",
            ),
            (
                "layer materials and thickness",
                "SELECT t.name, y.pos, y.material, y.thickness \
                 FROM wall_type_layer y JOIN wall_type t ON t.id=y.wtype",
            ),
        ],
        outcome: Outcome::Native,
    },
    Family {
        name: "door",
        table: "opening",
        class: "IFCDOOR",
        build: b_door,
        probes: &[(
            "kind, along, width, sill, height",
            "SELECT kind, along, width, sill, height FROM opening",
        )],
        outcome: Outcome::Native,
    },
    Family {
        name: "window",
        table: "opening",
        class: "IFCWINDOW",
        build: b_window,
        probes: &[(
            "kind, along, width, sill, height",
            "SELECT kind, along, width, sill, height FROM opening",
        )],
        outcome: Outcome::Native,
    },
    Family {
        name: "slab",
        table: "slab",
        class: "IFCSLAB",
        build: b_slab,
        probes: &[
            (
                "level, type, offset",
                "SELECT l.name, t.name, s.offset FROM slab s \
                 JOIN level l ON l.id=s.level JOIN slab_type t ON t.id=s.stype",
            ),
            ("outline and hole points", "SELECT ring, x, y FROM slab_vertex"),
            (
                "layer materials and thickness",
                "SELECT t.name, y.pos, y.material, y.thickness \
                 FROM slab_type_layer y JOIN slab_type t ON t.id=y.stype",
            ),
        ],
        outcome: Outcome::Native,
    },
    Family {
        name: "room",
        table: "room",
        class: "IFCSPACE",
        build: b_room,
        probes: &[
            (
                "level, name, number, height",
                "SELECT l.name, r.name, r.number, r.height FROM room r JOIN level l ON l.id=r.level",
            ),
            ("area and perimeter", "SELECT area, perimeter, volume FROM room_qty"),
        ],
        outcome: Outcome::Native,
    },
    Family {
        name: "column",
        table: "col",
        class: "IFCCOLUMN",
        build: b_column,
        probes: &[
            (
                "level, type, position, offsets, rotation",
                "SELECT l.name, t.name, c.structural, c.x, c.y, c.base_offset, c.top_offset, c.rotation \
                 FROM col c JOIN level l ON l.id=c.level JOIN member_type t ON t.id=c.mtype",
            ),
            MEMBER,
        ],
        outcome: Outcome::Native,
    },
    Family {
        name: "beam",
        table: "beam",
        class: "IFCBEAM",
        build: b_beam,
        probes: &[
            (
                "level, type, end points, justification, extensions",
                "SELECT l.name, t.name, b.ax, b.ay, b.az, b.bx, b.by, b.bz, b.bulge, b.yjust, \
                 b.zjust, b.rotation, b.start_ext, b.end_ext, b.join_start, b.join_end \
                 FROM beam b JOIN level l ON l.id=b.level JOIN member_type t ON t.id=b.mtype",
            ),
            MEMBER,
        ],
        outcome: Outcome::Native,
    },
    Family {
        name: "roof",
        table: "roof",
        class: "IFCROOF",
        build: b_roof,
        probes: &[
            (
                "level, kind, offset, overhang",
                "SELECT l.name, r.kind, r.offset, r.overhang FROM roof r JOIN level l ON l.id=r.level",
            ),
            ("outline and slopes", "SELECT x, y, slope FROM roof_vertex"),
            (
                "layer materials and thickness",
                "SELECT y.pos, y.material, y.thickness FROM slab_type_layer y \
                 JOIN roof r ON r.stype=y.stype",
            ),
        ],
        outcome: Outcome::Native,
    },
    Family {
        name: "ceiling",
        table: "ceiling",
        class: "IFCCOVERING",
        build: b_ceiling,
        probes: &[
            (
                "level, height",
                "SELECT l.name, c.height FROM ceiling c JOIN level l ON l.id=c.level",
            ),
            ("outline and hole points", "SELECT ring, x, y FROM ceiling_vertex"),
        ],
        outcome: Outcome::Native,
    },
    Family {
        name: "curtain wall",
        table: "curtain_wall",
        class: "IFCCURTAINWALL",
        build: b_curtain,
        probes: &[(
            "level, name, end points, height",
            "SELECT l.name, c.name, c.ax, c.ay, c.bx, c.by, c.base_offset, c.height \
             FROM curtain_wall c JOIN level l ON l.id=c.level",
        )],
        outcome: Outcome::Native,
    },
    Family {
        name: "grid axis",
        table: "grid",
        class: "IFCGRID",
        build: b_grid,
        probes: &[("name and end points", "SELECT name, ax, ay, bx, by FROM grid")],
        outcome: Outcome::Native,
    },
    Family {
        name: "ramp",
        table: "ramp",
        class: "IFCRAMP",
        build: b_ramp,
        probes: &[
            (
                "levels and spec",
                "SELECT l.name, t.name, r.base_offset, r.top_offset, r.width, r.slope, r.thickness,                  r.landing, r.max_run, r.path, r.material                  FROM ramp r JOIN level l ON l.id=r.level LEFT JOIN level t ON t.id=r.top_level",
            ),
        ],
        outcome: Outcome::Native,
    },
    Family {
        name: "stair",
        table: "stair",
        class: "IFCSTAIR",
        build: b_stair,
        probes: &[
            (
                "kind, position, width, material",
                "SELECT kind, round(x,3), round(y,3), round(width,3), material FROM stair",
            ),
            (
                "levels, shape and spec",
                "SELECT l.name, t.name, s.kind, s.turn, s.x, s.y, s.angle, s.width, s.max_riser, s.tread,              s.first, s.gap, s.landing, s.thickness, s.stringer_w, s.stringer_h, s.material,              s.base_offset, s.top_offset              FROM stair s JOIN level l ON l.id=s.level JOIN level t ON t.id=s.top_level",
            ),
        ],
        outcome: Outcome::Native,
    },
    Family {
        name: "railing",
        table: "railing",
        class: "IFCRAILING",
        build: b_railing,
        probes: &[
            ("height and material", "SELECT round(height,3), material FROM railing"),
            (
                "level, sizes, material",
                "SELECT l.name, r.stair, r.side, r.height, r.post_spacing, r.baluster_spacing, r.post,                  r.baluster, r.rail_w, r.rail_h, r.material FROM railing r JOIN level l ON l.id=r.level",
            ),
            ("path", "SELECT idx, x, y, z FROM railing_vertex"),
        ],
        outcome: Outcome::Native,
    },
];

fn rows(m: &Model, table: &str) -> i64 {
    m.sql()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap_or_else(|e| panic!("count of {table}: {e}"))
}

fn num(v: f64) -> String {
    let v = if v.abs() < 5e-5 { 0.0 } else { v };
    format!("{v:.4}")
}

/// Rows of a query as sorted text, numbers rounded to 0.1 mm.
fn probe_rows(m: &Model, sql: &str) -> Result<Vec<String>, String> {
    let mut st = m.sql().prepare(sql).map_err(|e| format!("{e}"))?;
    let n = st.column_count();
    let rows = st
        .query_map([], |r| {
            let mut cells = vec![];
            for i in 0..n {
                cells.push(match r.get_ref(i)? {
                    ValueRef::Null => "null".to_string(),
                    ValueRef::Integer(v) => v.to_string(),
                    ValueRef::Real(v) => num(v),
                    ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
                    ValueRef::Blob(b) => format!("blob({})", b.len()),
                });
            }
            Ok(cells.join(" | "))
        })
        .map_err(|e| format!("{e}"))?;
    let mut out = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("{e}"))?;
    out.sort();
    Ok(out)
}

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ddc-exchange-ifc-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// The external validator, when the repository has one and an interpreter is installed.
fn validate_external(file: &Path, family: &str) {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/validate_ifc.py");
    if !script.exists() {
        return;
    }
    let py = if cfg!(windows) { "python" } else { "python3" };
    if let Ok(o) = Command::new(py).arg("-I").arg(&script).arg(file).output() {
        assert!(
            o.status.success(),
            "{family}: validate_ifc.py rejected the export\n{}\n{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
    }
}

fn check_family(f: &Family) {
    let name = f.name;
    let (mut m, ids) = base();
    (f.build)(&mut m, &ids);
    let n = rows(&m, f.table);
    assert!(n > 0, "{name}: the fixture built no row in {}", f.table);

    let dir = scratch(&name.replace(' ', "_"));
    let path = dir.join("out.ifc");
    let stats = export_ifc_file(&m, &path, "out.ifc", None)
        .unwrap_or_else(|e| panic!("{name}: export failed: {e}"));
    let text = fs::read_to_string(&path).unwrap();
    check_step(&text).unwrap_or_else(|e| panic!("{name}: exported file is not valid STEP: {e}"));
    validate_external(&path, name);

    let written = stats.written.get(f.table).copied().unwrap_or(0) as i64;
    let dropped = stats
        .not_written
        .iter()
        .filter(|x| x.table == f.table)
        .count() as i64;
    let class_count = text.matches(&format!("={}(", f.class)).count() as i64;

    let mut back = Model::open(":memory:").unwrap();
    let rep = import_ifc(&mut back, text.as_bytes())
        .unwrap_or_else(|e| panic!("{name}: re-import failed: {e:?}"));
    let native = rows(&back, f.table);
    assert!(
        !rep.skipped.keys().any(|k| k.contains(f.class)),
        "{name}: re-import skipped {} {:?}",
        f.class,
        rep.skipped
    );

    match &f.outcome {
        Outcome::NotWritten(why) => {
            assert_eq!(
                (written, dropped),
                (0, n),
                "{name}: documented as not written ({why}), but the export says written={written} \
                 not_written={dropped} of {n} rows; update the table if it is now exported"
            );
            assert_eq!(
                class_count, 0,
                "{name}: {} appears in the file although the family is listed as not written",
                f.class
            );
        }
        Outcome::Mesh(why) => {
            assert_eq!(
                (written, dropped),
                (n, 0),
                "{name}: expected all {n} rows written; written={written} not_written={dropped}"
            );
            assert!(
                class_count >= n,
                "{name}: file has {class_count} {} for {n} rows",
                f.class
            );
            assert!(
                native + rep.meshes as i64 >= n,
                "{name}: {n} rows exported ({why}) but only {native} native + {} meshes came back",
                rep.meshes
            );
        }
        Outcome::Native => {
            assert_eq!(
                (written, dropped),
                (n, 0),
                "{name}: expected all {n} rows written; written={written} not_written={dropped}"
            );
            assert!(
                class_count >= n,
                "{name}: file has {class_count} {} for {n} rows",
                f.class
            );
            assert_eq!(
                native, n,
                "{name}: {n} rows exported, {native} came back in {}; meshes={} warnings={:?}",
                f.table, rep.meshes, rep.warnings
            );
            for (label, sql) in f.probes {
                let a = probe_rows(&m, sql).unwrap_or_else(|e| panic!("{name}/{label}: {e}"));
                let b =
                    probe_rows(&back, sql).unwrap_or_else(|e| panic!("{name}/{label}: {e}"));
                assert!(!a.is_empty(), "{name}/{label}: the probe sees no source row");
                assert_eq!(
                    a, b,
                    "{name}/{label}: differs after the round trip\n source: {a:#?}\n   back: {b:#?}"
                );
            }
        }
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn every_family_round_trips_or_is_listed_with_its_reason() {
    let mut failed = vec![];
    for f in FAMILIES {
        let r = std::panic::catch_unwind(|| check_family(f));
        if let Err(e) = r {
            let msg = e
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "panic".into());
            failed.push(msg);
        }
    }
    assert!(
        failed.is_empty(),
        "{} of {} families lose data:\n{}",
        failed.len(),
        FAMILIES.len(),
        failed.join("\n---\n")
    );
}

#[test]
fn covered_and_uncovered_lists_do_not_overlap_and_give_reasons() {
    for (t, why) in NOT_COVERED {
        assert!(!why.is_empty(), "{t}: a reason is required");
        assert!(
            !FAMILIES.iter().any(|f| f.table == *t),
            "{t} is both covered and listed as not covered"
        );
    }
    let mut names: Vec<&str> = FAMILIES.iter().map(|f| f.name).collect();
    names.sort_unstable();
    let n = names.len();
    names.dedup();
    assert_eq!(n, names.len(), "two families share a name");
}

#[test]
fn export_is_deterministic_for_the_same_model() {
    let (mut m, ids) = base();
    b_door(&mut m, &ids);
    b_slab(&mut m, &ids);
    let dir = scratch("determinism");
    let (a, b) = (dir.join("a.ifc"), dir.join("b.ifc"));
    export_ifc_file(&m, &a, "x.ifc", None).unwrap();
    export_ifc_file(&m, &b, "x.ifc", None).unwrap();
    let (ta, tb) = (fs::read_to_string(&a).unwrap(), fs::read_to_string(&b).unwrap());
    // the header carries a timestamp; the data section must not change
    let data = |t: &str| t.split("DATA;").nth(1).unwrap_or("").to_string();
    assert_eq!(data(&ta), data(&tb), "two exports of one model differ");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn material_written_once() {
    let (mut m, ids) = base();
    for x in [0.0, 6.0] {
        m.add_stair(&StairSpec::straight(ids.l1, ids.l2, (x, 0.0), 0.0))
            .unwrap();
    }
    let dir = scratch("mat_once");
    let path = dir.join("out.ifc");
    export_ifc_file(&m, &path, "out.ifc", None).unwrap();
    let text = fs::read_to_string(&path).unwrap();
    check_step(&text).unwrap();
    assert_eq!(text.matches("=IFCMATERIAL('Concrete'").count(), 1);
    assert_eq!(text.matches("=IFCRELASSOCIATESMATERIAL(").count(), 2);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn stair_exports_parts_material_and_properties_and_its_railing_follows_it() {
    let (mut m, ids) = base();
    let st = m
        .add_stair(&StairSpec::straight(ids.l1, ids.l2, (0.0, 0.0), 0.0))
        .unwrap();
    m.add_stair_railings(st, &RailingSpec::new(ids.l1, RailingHost::Stair { stair: st, side: 0 }))
        .unwrap();
    let dir = scratch("stair_parts");
    let path = dir.join("out.ifc");
    export_ifc_file(&m, &path, "out.ifc", None).unwrap();
    let text = fs::read_to_string(&path).unwrap();
    check_step(&text).unwrap();
    validate_external(&path, "stair parts");
    for needle in [
        "=IFCSTAIR(",
        "=IFCSTAIRFLIGHT(",
        "=IFCRAILING(",
        "=IFCMATERIAL('Concrete'",
        "'Pset_StairCommon'",
        "'NumberOfRiser'",
        "'RiserHeight'",
        "'TreadLength'",
    ] {
        assert!(text.contains(needle), "missing {needle}");
    }
    let mut back = Model::open(":memory:").unwrap();
    import_ifc(&mut back, text.as_bytes()).unwrap();
    assert_eq!(rows(&back, "stair"), 1);
    assert_eq!(rows(&back, "railing"), 2);
    let hosted: i64 = back
        .sql()
        .query_row(
            "SELECT count(*) FROM railing WHERE stair IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(hosted, 2, "railings lost their stair host");
    let _ = fs::remove_dir_all(&dir);
}
