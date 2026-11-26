// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;
use crate::{
    BeamJoin, BeamSpec, CeilingSpec, ColumnSpec, CompSpec, CurtainSpec, Edit, GridLines, Just,
    LayerSpec, MemberShape, MullionSection, OpeningSpec, PipeMaterial, RailingHost, RailingSpec,
    RoofKind, RoofSpec, RoomSpec, RunSpec, SlabSpec, StairSpec, SystemKind, WallSpec,
};

use crate::{ClashHit, ClashStatus};

fn go(_: u64, _: u64) -> bool {
    true
}

fn all() -> Changes {
    Changes {
        head: 0,
        all: true,
        rows: HashMap::new(),
    }
}

fn key(t: &str, id: i64) -> Option<ElKey> {
    Some((t.to_string(), id))
}

/// Contents of all tables except the journal and `ai_*`; rows are sorted, rowid is excluded.
fn hash(m: &Model) -> u64 {
    use std::hash::{Hash, Hasher};
    let db = m.sql();
    let mut tables = table_names(db, "main").unwrap();
    tables.retain(|t| t != "op" && !t.starts_with("ai_") && !t.starts_with("sqlite_"));
    tables.sort();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for t in tables {
        let mut st = db.prepare(&format!("SELECT * FROM {}", quote(&t))).unwrap();
        let n = st.column_count();
        let mut rows: Vec<String> = st
            .query_map([], |r| {
                Ok((0..n)
                    .map(|i| format!("{:?}", r.get::<_, Value>(i).unwrap()))
                    .collect::<Vec<_>>()
                    .join("|"))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        rows.sort();
        t.hash(&mut h);
        rows.hash(&mut h);
    }
    h.finish()
}

fn copies(tag: &str) -> PlanCopies {
    PlanCopies::new(&format!("test-{tag}")).unwrap()
}

fn live_path(c: &PlanCopies) -> PathBuf {
    c.dir.join("live.ddc")
}

fn open_live(c: &PlanCopies) -> Model {
    let p = live_path(c);
    let _ = fs::remove_file(&p);
    remove_sidecars(&p).unwrap();
    Model::open(p.to_str().unwrap()).unwrap()
}

fn open_work(c: &PlanCopies) -> Model {
    Model::open_copy(c.work.to_str().unwrap()).unwrap()
}

fn rows_of<'a>(d: &'a ModelDiff, t: &str, k: RowKind) -> Vec<&'a RowDiff> {
    d.rows
        .iter()
        .filter(|r| r.table == t && r.kind == k)
        .collect()
}

fn has(d: &ModelDiff, t: &str, id: i64, k: RowKind) -> bool {
    d.rows
        .iter()
        .any(|r| r.table == t && r.rowid == id && r.kind == k)
}

fn cell<'a>(r: &'a RowDiff, col: &str) -> &'a CellDiff {
    r.cells
        .iter()
        .find(|c| c.col == col)
        .unwrap_or_else(|| panic!("no cell {col} in {r:?}"))
}

fn layer(material: &str, thickness: f64) -> [LayerSpec; 1] {
    [LayerSpec {
        material: material.into(),
        thickness,
    }]
}

fn ring(x: f64, y: f64, w: f64, h: f64) -> Vec<(f64, f64)> {
    vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)]
}

fn wall_at(level: i64, wall_type: i64, a: (f64, f64), b: (f64, f64)) -> WallSpec {
    WallSpec {
        a,
        b,
        level,
        base_offset: 0.0,
        height: 3.0,
        wall_type,
        offset0: None,
    }
}

fn door(along: f64) -> OpeningSpec {
    OpeningSpec {
        along,
        width: 0.9,
        sill: 0.0,
        height: 2.1,
        kind: "door".into(),
    }
}

fn column(level: i64, member_type: i64, x: f64, y: f64) -> ColumnSpec {
    ColumnSpec {
        level,
        top_level: None,
        member_type,
        structural: true,
        at: (x, y),
        base_offset: 0.0,
        top_offset: 3.0,
        rotation: 0.0,
        top_at: None,
    }
}

struct Rich {
    lv: i64,
    wt: i64,
    st: i64,
    mt: i64,
    ctype: i64,
    wall: i64,
    window: i64,
    slab: i64,
    col: i64,
    beam: i64,
    roof: i64,
    ceiling: i64,
    railing: i64,
    run: i64,
    curtain: i64,
    comp: i64,
}

/// A model with all the main element categories.
fn rich(m: &mut Model) -> Rich {
    let lv = m.add_level("L1", 0.0).unwrap();
    let l2 = m.add_level("L2", 3.0).unwrap();
    let wt = m.add_wall_type("W200", &layer("Brick", 0.2)).unwrap();
    let st = m.add_slab_type("S250", &layer("Concrete", 0.25)).unwrap();
    let mt = m
        .add_member_type("C400", &MemberShape::Rect { b: 0.4, h: 0.4 }, "Concrete")
        .unwrap();
    let wall = m
        .add_wall(&wall_at(lv, wt, (0.0, 0.0), (10.0, 0.0)))
        .unwrap();
    m.add_wall(&wall_at(lv, wt, (10.0, 0.0), (10.0, 8.0)))
        .unwrap();
    m.add_wall(&wall_at(lv, wt, (10.0, 8.0), (0.0, 8.0)))
        .unwrap();
    m.add_wall(&wall_at(lv, wt, (0.0, 8.0), (0.0, 0.0)))
        .unwrap();
    m.add_opening(wall, &door(2.0)).unwrap();
    let window = m
        .add_opening(
            wall,
            &OpeningSpec {
                along: 5.0,
                width: 1.2,
                sill: 0.9,
                height: 1.2,
                kind: "window".into(),
            },
        )
        .unwrap();
    let slab = m
        .add_slab(&SlabSpec {
            level: lv,
            slab_type: st,
            offset: 0.0,
            outer: ring(0.0, 0.0, 10.0, 8.0),
            holes: vec![],
        })
        .unwrap();
    m.add_room(&RoomSpec {
        level: lv,
        name: "R".into(),
        number: "1".into(),
        point: (5.0, 4.0),
        height: 3.0,
    })
    .unwrap();
    let col = m.add_column(&column(lv, mt, 20.0, 20.0)).unwrap();
    let beam = m
        .add_beam(&BeamSpec {
            level: lv,
            member_type: mt,
            a: (30.0, 0.0, 3.0),
            b: (34.0, 0.0, 3.0),
            bulge: 0.0,
            yjust: Just::Center,
            zjust: Just::Pos,
            rotation: 0.0,
            start_ext: 0.0,
            end_ext: 0.0,
            join_start: BeamJoin::Auto,
            join_end: BeamJoin::Auto,
        })
        .unwrap();
    let roof = m
        .add_roof(&RoofSpec {
            level: l2,
            slab_type: st,
            offset: 0.0,
            overhang: 0.3,
            fascia: 0.2,
            kind: RoofKind::Footprint {
                ring: ring(40.0, 0.0, 6.0, 6.0),
                slopes: vec![Some(30.0); 4],
                mansard: None,
            },
        })
        .unwrap();
    let ceiling = m
        .add_ceiling(&CeilingSpec {
            level: lv,
            slab_type: st,
            height: 2.7,
            outer: ring(0.0, 20.0, 4.0, 4.0),
            holes: vec![],
        })
        .unwrap();
    let stair = m
        .add_stair(&StairSpec::straight(lv, l2, (50.0, 0.0), 0.0))
        .unwrap();
    let railing = m
        .add_railing(&RailingSpec::new(lv, RailingHost::Stair { stair, side: 0 }))
        .unwrap();
    let sys = m.add_mep_system("CW", SystemKind::ColdWater).unwrap();
    let run = m
        .add_mep_run(
            &RunSpec::pipe(
                sys,
                lv,
                PipeMaterial::Copper,
                15,
                vec![(60.0, 0.0, 1.0), (62.0, 0.0, 1.0)],
            )
            .unwrap(),
        )
        .unwrap();
    let sec = MullionSection::Rect { w: 0.05, d: 0.15 };
    let curtain = m
        .add_curtain_wall(&CurtainSpec {
            level: lv,
            name: "CW".into(),
            a: (70.0, 0.0),
            b: (76.0, 0.0),
            bulge: 0.0,
            base_offset: 0.0,
            height: 3.0,
            u: GridLines::Spacing(1.5),
            v: GridLines::Spacing(1.0),
            interior: sec,
            border: sec,
            lines: Default::default(),
            top: vec![],
        })
        .unwrap();
    let family = m.load_library_family("Sink").unwrap();
    let ctype = m.comp_types(family).unwrap()[0].0;
    let comp = m
        .add_component(&CompSpec::at(lv, ctype, 80.0, 0.0))
        .unwrap();
    Rich {
        lv,
        wt,
        st,
        mt,
        ctype,
        wall,
        window,
        slab,
        col,
        beam,
        roof,
        ceiling,
        railing,
        run,
        curtain,
        comp,
    }
}

/// Plan: create, modify and delete per category.
fn plan(m: &mut Model, r: &Rich) -> Result<(), Error> {
    m.add_wall(&wall_at(r.lv, r.wt, (0.0, -5.0), (10.0, -5.0)))?;
    m.add_opening(r.wall, &door(8.0))?;
    m.delete_opening(r.window)?;
    m.add_slab(&SlabSpec {
        level: r.lv,
        slab_type: r.st,
        offset: 0.0,
        outer: ring(0.0, -20.0, 5.0, 5.0),
        holes: vec![],
    })?;
    m.apply_edits(&[
        Edit::Slab {
            id: r.slab,
            level: r.lv,
            slab_type: r.st,
            offset: 0.1,
        },
        Edit::Column {
            id: r.col,
            spec: column(r.lv, r.mt, 21.0, 20.0),
        },
        Edit::DeleteBeam(r.beam),
        Edit::Roof {
            id: r.roof,
            offset: 0.1,
            overhang: 0.5,
            fascia: 0.2,
        },
        Edit::CurtainProps {
            id: r.curtain,
            level: r.lv,
            name: "CW2".into(),
            base_offset: 0.1,
        },
        Edit::NewLevel {
            name: "L3".into(),
            elevation: 6.0,
        },
    ])?;
    m.delete_ceiling(r.ceiling)?;
    m.delete_railing(r.railing)?;
    m.delete_mep_run(r.run)?;
    m.update_component(r.comp, &CompSpec::at(r.lv, r.ctype, 81.0, 0.0))?;
    m.set_prop(
        "wall",
        r.wall,
        "FireRating",
        Some((Value::Text("EI30".into()), "")),
    )?;
    Ok(())
}

#[test]
fn atomic_is_one_journal_record_and_rolls_back_on_error() {
    let mut m = Model::open(":memory:").unwrap();
    m.add_level("L0", 0.0).unwrap();
    let (top, h0) = (m.op_top().unwrap(), hash(&m));
    let ids = m
        .atomic("wall_ai_plan", |m| {
            Ok((m.add_level("L1", 3.0)?, m.add_level("L2", 6.0)?))
        })
        .unwrap();
    assert_ne!(ids.0, ids.1);
    assert_eq!(m.op_top().unwrap(), top + 1, "one journal record");
    m.undo().unwrap();
    assert_eq!(hash(&m), h0, "one undo takes the whole plan");
    assert_eq!(m.op_top().unwrap(), top);

    let err = m.atomic("wall_ai_plan", |m| {
        m.add_level("L1", 3.0)?;
        m.add_level("L2", 6.0)?;
        Err::<(), _>(Error::Invalid("boom".into()))
    });
    assert!(matches!(err, Err(Error::Invalid(_))));
    assert_eq!(hash(&m), h0, "a failed plan leaves no trace");
    assert_eq!(m.op_top().unwrap(), top);
}

#[test]
fn atomic_with_one_operation_is_one_record_and_without_any_is_an_error() {
    let mut m = Model::open(":memory:").unwrap();
    m.add_level("L0", 0.0).unwrap();
    let top = m.op_top().unwrap();
    let none = m.atomic("wall_ai_plan", |_| Ok(()));
    assert!(none.is_err(), "an undo step without a record undoes L0");
    assert_eq!(m.op_top().unwrap(), top);
    m.atomic("wall_ai_plan", |m| m.add_level("L1", 3.0))
        .unwrap();
    assert_eq!(m.op_top().unwrap(), top + 1);
}

#[test]
fn rollback_to_undoes_down_to_the_mark() {
    let mut m = Model::open(":memory:").unwrap();
    assert_eq!(m.op_top().unwrap(), 0, "empty journal");
    m.add_level("L0", 0.0).unwrap();
    let (mark, h0) = (m.op_top().unwrap(), hash(&m));
    m.add_level("L1", 3.0).unwrap();
    m.add_level("L2", 6.0).unwrap();
    assert_eq!(m.rollback_to(mark).unwrap(), 2);
    assert_eq!((m.op_top().unwrap(), hash(&m)), (mark, h0));
    assert_eq!(m.rollback_to(mark).unwrap(), 0, "nothing above the mark");
}

#[test]
fn backup_copies_committed_state_of_an_open_file_and_can_be_cancelled() {
    let c = copies("backup");
    let mut live = open_live(&c);
    rich(&mut live);
    let want = hash(&live);
    let src = live_path(&c);
    let mut seen: Vec<(u64, u64)> = vec![];
    Model::backup_file(&src, &c.base, &mut |d: u64, t: u64| {
        seen.push((d, t));
        true
    })
    .unwrap();
    let last = *seen.last().unwrap();
    assert!(last.1 > 0 && last.0 == last.1, "{seen:?}");
    let copy = Model::open_copy(c.base.to_str().unwrap()).unwrap();
    assert_eq!(
        hash(&copy),
        want,
        "the model is open: pages come from its log"
    );
    drop(copy);

    let dst = c.dir.join("cancelled.db");
    let res = Model::backup_file(&src, &dst, &mut |_: u64, _: u64| false);
    assert!(res.is_err());
    assert!(!dst.exists() && !sidecar(&dst, ".part").exists());
    let missing = c.dir.join("missing.db");
    assert!(Model::backup_file(&missing, &dst, &mut go).is_err());
    assert!(!missing.exists(), "reading a missing file creates nothing");
}

fn lit(ty: &str, n: i64) -> String {
    match ty.to_ascii_uppercase().as_str() {
        "REAL" => format!("{n}.5"),
        "INTEGER" => (7_000_000 + n).to_string(),
        "BLOB" => format!("x'{n:02x}'"),
        _ => format!("'ai2-{n}'"),
    }
}

fn shown(ty: &str, n: i64) -> String {
    match ty.to_ascii_uppercase().as_str() {
        "REAL" => format!("{n}.5"),
        "INTEGER" => (7_000_000 + n).to_string(),
        "BLOB" => "blob(1)".into(),
        _ => format!("ai2-{n}"),
    }
}

/// A row of values by declared types (no rowid alias): fits any table.
fn dummy_insert(db: &Connection, t: &str, n: i64) -> rusqlite::Result<i64> {
    let info = columns(db, "main", t).unwrap();
    let pks = info.iter().filter(|c| c.2 > 0).count();
    let (mut names, mut values) = (vec![], vec![]);
    for (name, ty, pk, _) in &info {
        if *pk > 0 && pks == 1 && ty.eq_ignore_ascii_case("INTEGER") {
            continue;
        }
        names.push(quote(name));
        values.push(lit(ty, n));
    }
    let sql = if names.is_empty() {
        format!("INSERT INTO {} DEFAULT VALUES", quote(t))
    } else {
        format!(
            "INSERT INTO {}({}) VALUES ({})",
            quote(t),
            names.join(","),
            values.join(",")
        )
    };
    db.execute(&sql, [])?;
    Ok(db.last_insert_rowid())
}

/// Every category is every table in the file, including future ones: create, edit, delete.
#[test]
fn diff_sees_created_changed_deleted_in_every_table() {
    struct Seed {
        table: String,
        a: i64,
        b: i64,
        update: Option<(String, String)>,
    }
    let c = copies("every");
    let m = open_live(&c);
    let db = m.sql();
    let tables: Vec<String> = table_names(db, "main")
        .unwrap()
        .into_iter()
        .filter(|t| !skipped(t))
        .collect();
    for want in [
        "level",
        "wall_type",
        "wall",
        "opening",
        "slab",
        "slab_vertex",
        "room",
        "col",
        "beam",
        "roof",
        "ceiling",
        "stair",
        "railing",
        "comp",
        "mep_run",
        "curtain_wall",
        "prop",
        "pparam_val",
        "elem_phase",
        "param",
        "mesh",
    ] {
        assert!(
            tables.iter().any(|t| t.as_str() == want),
            "{want} is not compared"
        );
    }
    let mut seeds = vec![];
    for t in &tables {
        let (a, b) = match (dummy_insert(db, t, 1), dummy_insert(db, t, 2)) {
            (Ok(a), Ok(b)) => (a, b),
            (x, y) => {
                // one-row table: CHECK(id = 1)
                assert_eq!(t.as_str(), "site_info", "{t}: {x:?} {y:?}");
                continue;
            }
        };
        let info = columns(db, "main", t).unwrap();
        let has_pk = info.iter().any(|i| i.2 > 0);
        let update = info
            .iter()
            .find(|i| has_pk && i.2 == 0)
            .map(|i| (i.0.clone(), i.1.clone()));
        seeds.push(Seed {
            table: t.clone(),
            a,
            b,
            update,
        });
    }
    Model::backup_file(&live_path(&c), &c.base, &mut go).unwrap();
    assert_eq!(
        m.diff_from(&c.base, &all(), usize::MAX).unwrap(),
        ModelDiff::default(),
        "an equal copy has no differences"
    );

    let mut created = vec![];
    for s in &seeds {
        created.push(dummy_insert(db, &s.table, 3).unwrap());
        db.execute(
            &format!("DELETE FROM {} WHERE rowid=?1", quote(&s.table)),
            [s.b],
        )
        .unwrap();
        if let Some((col, ty)) = &s.update {
            db.execute(
                &format!(
                    "UPDATE {} SET {}={} WHERE rowid=?1",
                    quote(&s.table),
                    quote(col),
                    lit(ty, 9)
                ),
                [s.a],
            )
            .unwrap();
        }
    }
    let d = m.diff_from(&c.base, &all(), usize::MAX).unwrap();
    assert!(!d.truncated);
    for (s, new_id) in seeds.iter().zip(&created) {
        let t = s.table.as_str();
        assert!(has(&d, t, *new_id, RowKind::Created), "{t} created: {d:?}");
        assert!(has(&d, t, s.b, RowKind::Deleted), "{t} deleted");
        if let Some((col, ty)) = &s.update {
            let row = rows_of(&d, t, RowKind::Changed);
            assert_eq!(row.len(), 1, "{t} changed");
            assert_eq!(row[0].rowid, s.a, "{t}");
            assert_eq!(row[0].cells.len(), 1, "{t}: only the changed column");
            let cl = &row[0].cells[0];
            // a changed setting is named by its key
            let want = if t == "meta" { "ai2-1" } else { col.as_str() };
            assert_eq!(cl.col, want, "{t}");
            // equal-length blobs are told apart by a trailing hash
            let same = |got: &Option<String>, n: i64| {
                let w = shown(ty, n);
                got.as_deref().is_some_and(|g| g == w || (w.starts_with("blob(") && g.starts_with(&w)))
            };
            assert!(same(&cl.before, 1), "{t}: {:?}", cl.before);
            assert!(same(&cl.after, 9), "{t}: {:?}", cl.after);
        }
    }
    let seed = |t: &str| seeds.iter().find(|s| s.table == t).unwrap().a;
    let changed = |t: &str| rows_of(&d, t, RowKind::Changed)[0].owner.clone();
    let id = 7_000_001;
    assert_eq!(changed("wall"), key("wall", seed("wall")));
    assert_eq!(changed("opening"), key("opening", seed("opening")));
    assert_eq!(changed("prop"), key("ai2-1", id));
    assert_eq!(changed("elem_phase"), key("ai2-1", id));
    assert_eq!(changed("slab_vertex"), key("slab", id));
    assert_eq!(changed("param"), key("element", id));
}

#[test]
fn diff_of_a_plan_reports_each_category_with_its_owner() {
    let c = copies("plan");
    let mut live = open_live(&c);
    let r = rich(&mut live);
    c.prepare(&live_path(&c), &mut go).unwrap();
    let mut work = open_work(&c);
    let head = work.change_head();
    work.atomic("wall_ai_plan", |m| plan(m, &r)).unwrap();
    let d = work
        .diff_from(&c.base, &work.changes_since(head), usize::MAX)
        .unwrap();
    assert!(!d.truncated);
    assert!(d.rows.iter().all(|x| !skipped(&x.table)), "{d:?}");
    assert!(is_element(&d.rows[0].table), "elements go first");

    let wall = rows_of(&d, "wall", RowKind::Created);
    assert_eq!(wall.len(), 1);
    assert_eq!(wall[0].owner, key("wall", wall[0].rowid));
    assert_eq!(rows_of(&d, "opening", RowKind::Created).len(), 1);
    assert!(has(&d, "opening", r.window, RowKind::Deleted));
    let slab = rows_of(&d, "slab", RowKind::Created);
    assert_eq!(slab.len(), 1);
    let verts = rows_of(&d, "slab_vertex", RowKind::Created);
    assert_eq!(verts.len(), 4);
    assert!(verts.iter().all(|v| v.owner == key("slab", slab[0].rowid)));
    let edited = d
        .rows
        .iter()
        .find(|x| x.table == "slab" && x.rowid == r.slab && x.kind == RowKind::Changed)
        .unwrap();
    assert_eq!(cell(edited, "offset").after.as_deref(), Some("0.1"));
    let col = d
        .rows
        .iter()
        .find(|x| x.table == "col" && x.rowid == r.col)
        .unwrap();
    assert_eq!(col.kind, RowKind::Changed);
    assert_eq!(cell(col, "x").before.as_deref(), Some("20"));
    assert_eq!(cell(col, "x").after.as_deref(), Some("21"));
    assert!(has(&d, "beam", r.beam, RowKind::Deleted));
    assert!(has(&d, "roof", r.roof, RowKind::Changed));
    assert!(has(&d, "curtain_wall", r.curtain, RowKind::Changed));
    assert_eq!(rows_of(&d, "level", RowKind::Created).len(), 1);
    assert!(has(&d, "ceiling", r.ceiling, RowKind::Deleted));
    let cv = rows_of(&d, "ceiling_vertex", RowKind::Deleted);
    assert_eq!(cv.len(), 4);
    assert_eq!(cv[0].owner, key("ceiling", r.ceiling));
    assert!(has(&d, "railing", r.railing, RowKind::Deleted));
    assert!(has(&d, "mep_run", r.run, RowKind::Deleted));
    assert!(has(&d, "comp", r.comp, RowKind::Changed));
    let prop = rows_of(&d, "prop", RowKind::Created);
    assert_eq!(prop.len(), 1);
    assert_eq!(prop[0].owner, key("wall", r.wall));
    assert_eq!(cell(prop[0], "value").after.as_deref(), Some("EI30"));
}

#[test]
fn apply_equals_scratch_and_undo_restores_the_exact_hash() {
    let c = copies("apply");
    let mut live = open_live(&c);
    let r = rich(&mut live);
    let (h0, seq, head) = (hash(&live), live.op_top().unwrap(), live.change_head());
    c.prepare(&live_path(&c), &mut go).unwrap();

    let mut work = open_work(&c);
    assert_eq!(hash(&work), h0, "the work copy equals the model");
    let wh = work.change_head();
    work.atomic("wall_ai_plan", |m| plan(m, &r)).unwrap();
    let scratch = hash(&work);
    assert_ne!(scratch, h0);
    assert_eq!(hash(&live), h0, "the dry run does not touch the model");
    assert!(!live.plan_stale(head), "nothing happened on the live model");
    let dry = work
        .diff_from(&c.base, &work.changes_since(wh), usize::MAX)
        .unwrap();

    live.atomic("wall_ai_plan", |m| plan(m, &r)).unwrap();
    assert_eq!(live.op_top().unwrap(), seq + 1, "exactly one undo step");
    assert_eq!(hash(&live), scratch, "live equals scratch after apply");
    assert_eq!(
        live.diff_from(&c.base, &all(), usize::MAX).unwrap(),
        dry,
        "same rows, same ids"
    );

    live.undo().unwrap();
    assert_eq!(hash(&live), h0, "one undo restores the exact content");
    assert_eq!(live.op_top().unwrap(), seq);
    assert_eq!(
        live.diff_from(&c.base, &all(), usize::MAX).unwrap(),
        ModelDiff::default()
    );

    live.atomic("wall_ai_plan", |m| plan(m, &r)).unwrap();
    assert_eq!(hash(&live), scratch, "redo gets the same ids");

    assert_eq!(work.rollback_to(seq).unwrap(), 1);
    assert_eq!(hash(&work), h0);
    assert_eq!(
        work.diff_from(&c.base, &all(), usize::MAX).unwrap(),
        ModelDiff::default(),
        "after a rollback the copy needs no reset"
    );
}

#[test]
fn reset_promote_and_adopt_replace_the_copies() {
    let c = copies("reset");
    let mut live = open_live(&c);
    live.add_level("L0", 0.0).unwrap();
    c.prepare(&live_path(&c), &mut go).unwrap();
    let h0 = hash(&live);
    {
        let mut w = open_work(&c);
        w.add_level("L1", 3.0).unwrap();
        assert_ne!(hash(&w), h0);
    }
    c.reset_work().unwrap();
    let mut w = open_work(&c);
    assert_eq!(hash(&w), h0, "the reset drops the edits of the closed copy");
    w.add_level("L2", 6.0).unwrap();
    let h1 = hash(&w);
    c.promote_work(&w).unwrap();
    drop(w);
    let b = Model::open(c.base.to_str().unwrap()).unwrap();
    assert_eq!(hash(&b), h1, "pages still in the log reach the new base");
    drop(b);

    let snap = c.dir.join("snapshot.db");
    live.snapshot_to(&snap).unwrap();
    c.adopt(&snap).unwrap();
    let w = open_work(&c);
    assert_eq!(
        hash(&w),
        h0,
        "a snapshot of a project in memory becomes the base"
    );
}

/// The same steps on copies held in memory: the base diffs like a file, and no file appears.
#[test]
fn memory_copies_reset_promote_adopt_and_diff_like_the_files() {
    let c = MemCopies::new("test-mem").unwrap();
    let store: &dyn CopyStore = &c;
    assert!(store.dir().is_none() && is_memory_copy(store.base()) && is_memory_copy(store.work()));
    let mut live = Model::open(":memory:").unwrap();
    live.add_level("L0", 0.0).unwrap();
    store.adopt_bytes(&live.to_bytes().unwrap()).unwrap();
    let h0 = hash(&live);
    let open = |p: &Path| Model::open_copy(p.to_str().unwrap()).unwrap();
    {
        let mut w = open(store.work());
        assert_eq!(hash(&w), h0, "the bytes became base and work");
        w.add_level("L1", 3.0).unwrap();
        let d = w.diff_from(store.base(), &all(), 100).unwrap();
        assert!(d.rows.iter().any(|r| r.table == "level" && r.kind == RowKind::Created));
    }
    store.reset_work().unwrap();
    let mut w = open(store.work());
    assert_eq!(hash(&w), h0, "the reset drops the edits of the closed copy");
    w.add_level("L2", 6.0).unwrap();
    let h1 = hash(&w);
    store.promote_work(&w).unwrap();
    drop(w);
    assert_eq!(hash(&open(store.base())), h1, "the work copy became the base");

    // a second session's copies are its own
    let other = MemCopies::new("test-mem").unwrap();
    assert_ne!(other.base(), store.base());
    other.adopt_bytes(&live.to_bytes().unwrap()).unwrap();
    assert_eq!(hash(&open(other.base())), h0);
    assert_eq!(hash(&open(store.base())), h1);

    assert!(!store.base().exists() && !Path::new("ddc-ai-test-mem").exists(), "no file written");
    let base = store.base().to_path_buf();
    drop(c);
    let gone = Connection::open(&base).unwrap();
    let tables: i64 = gone.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0)).unwrap();
    assert_eq!(tables, 0, "dropped copies are gone from memory");
}

#[test]
fn stale_when_the_live_model_changed_and_only_then() {
    let c = copies("stale");
    let mut live = open_live(&c);
    live.add_level("L0", 0.0).unwrap();
    let head = live.change_head();
    assert!(!live.plan_stale(head));
    live.audit_append(&crate::AuditEntry::default()).unwrap();
    live.set_ai_policy("k", "v").unwrap();
    dummy_insert(live.sql(), "comp_thumb", 1).unwrap();
    assert!(
        !live.plan_stale(head),
        "log, settings and caches do not count"
    );
    assert!(live.plan_stale(0), "a lost window means stale");

    live.add_level("USER", 3.0).unwrap();
    assert!(live.plan_stale(head), "a user edit");
    let after = live.change_head();
    assert!(!live.plan_stale(after));
    live.undo().unwrap();
    assert!(live.plan_stale(after), "an undo changes the model too");

    live.add_level("USER", 3.0).unwrap();
    let rebased = live.change_head();
    c.prepare(&live_path(&c), &mut go).unwrap();
    assert!(!live.plan_stale(rebased), "a new copy has a new head");
    let w = open_work(&c);
    assert_eq!(hash(&w), hash(&live), "the new copy holds the user edit");
}

#[test]
fn only_new_integrity_issues_block() {
    let mut m = Model::open(":memory:").unwrap();
    rich(&mut m);
    let dangling = "INSERT INTO opening(wall,along,width,sill,height,kind) \
                    VALUES (?1,0,1,0,2,'door')";
    m.sql().execute(dangling, [90_001]).unwrap();
    let base = m.check_integrity().unwrap();
    assert!(!base.is_empty());
    assert!(m.integrity_regressions(&base).unwrap().is_empty());
    m.sql().execute(dangling, [90_002]).unwrap();
    let new = m.integrity_regressions(&base).unwrap();
    assert!(new.iter().any(|i| i.problem.contains("90002")), "{new:?}");
    assert!(!new.iter().any(|i| i.problem.contains("90001")), "{new:?}");
    assert!(m.integrity_regressions(&[]).unwrap().len() > new.len());
}

#[test]
fn measured_values_do_not_make_an_old_issue_new() {
    assert_eq!(
        problem_key("opening above wall top (3.200 > 3.000): cut"),
        problem_key("opening above wall top (3.200 > 2.950): cut")
    );
    assert_ne!(
        problem_key("wall -> missing level 12"),
        problem_key("wall -> missing level 13"),
        "an integer points at a row"
    );
    assert_eq!(problem_key("v 1.2.3 end"), "v #.3 end");
    assert_eq!(problem_key(""), "");
    let mut m = Model::open(":memory:").unwrap();
    let r = rich(&mut m);
    m.set_wall_top_profile(r.wall, Some(&[(0.0, 1.0), (10.0, 1.0)]))
        .unwrap();
    let base = m.check_integrity().unwrap();
    assert!(
        base.iter().any(|i| i.problem.contains("above wall top")),
        "{base:?}"
    );
    m.set_wall_top_profile(r.wall, Some(&[(0.0, 0.9), (10.0, 0.9)]))
        .unwrap();
    let now = m.integrity_regressions(&base).unwrap();
    assert!(now.is_empty(), "the same issue with other numbers: {now:?}");
}

#[test]
fn diff_sees_rows_removed_by_replace() {
    let c = copies("replace");
    let m = open_live(&c);
    m.sql()
        .execute(
            "INSERT INTO level(id,name,elevation) VALUES (1,'L1',0.0)",
            [],
        )
        .unwrap();
    Model::backup_file(&live_path(&c), &c.base, &mut go).unwrap();
    let head = m.change_head();
    m.sql()
        .execute(
            "INSERT OR REPLACE INTO level(name,elevation) VALUES ('L1',5.0)",
            [],
        )
        .unwrap();
    let d = m.diff_from(&c.base, &m.changes_since(head), 10).unwrap();
    assert!(has(&d, "level", 1, RowKind::Deleted), "{d:?}");
    assert_eq!(rows_of(&d, "level", RowKind::Created).len(), 1);
}

#[test]
fn diff_covers_a_whole_table_mark_and_limits() {
    let c = copies("whole");
    let m = open_live(&c);
    Model::backup_file(&live_path(&c), &c.base, &mut go).unwrap();
    let head = m.change_head();
    m.sql()
        .execute_batch(
            "BEGIN; WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<5000)
             INSERT INTO level(name,elevation) SELECT 'x'||i, i FROM n; COMMIT;",
        )
        .unwrap();
    let ch = m.changes_since(head);
    assert!(ch.whole("level"));
    let d = m.diff_from(&c.base, &ch, usize::MAX).unwrap();
    assert_eq!(d.rows.len(), 5000);
    assert!(!d.truncated);
    assert!(d.rows.windows(2).all(|w| w[0].rowid < w[1].rowid), "sorted");

    let cut = m.diff_from(&c.base, &ch, 3).unwrap();
    assert_eq!(cut.rows.len(), 3);
    assert!(cut.truncated);
    let exact = m.diff_from(&c.base, &ch, 5000).unwrap();
    assert!(!exact.truncated, "the limit is not exceeded");
    assert!(m.diff_from(&c.base, &ch, 0).unwrap().truncated);
    let untouched = m.diff_from(&c.base, &Changes::default(), 10).unwrap();
    assert!(
        untouched.rows.is_empty(),
        "nothing touched, nothing compared"
    );
}

#[test]
fn diff_shows_blobs_by_size_and_nulls_as_none() {
    let c = copies("blob");
    let m = open_live(&c);
    let db = m.sql();
    db.execute(
        "INSERT INTO mesh(id,level,category,name,positions,indices) \
         VALUES (5,NULL,'c','n',x'01020304',x'0506')",
        [],
    )
    .unwrap();
    Model::backup_file(&live_path(&c), &c.base, &mut go).unwrap();
    db.execute(
        "UPDATE mesh SET positions=x'0A0B0C0D', level=3 WHERE id=5",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO mesh(id,level,category,name,positions,indices) \
         VALUES (6,NULL,'c','n',x'01',x'02')",
        [],
    )
    .unwrap();
    let d = m.diff_from(&c.base, &all(), 10).unwrap();
    let changed = rows_of(&d, "mesh", RowKind::Changed);
    assert_eq!(changed[0].rowid, 5);
    assert_eq!(changed[0].cells.len(), 2);
    let pos = cell(changed[0], "positions");
    let (was, now) = (
        pos.before.as_deref().unwrap(),
        pos.after.as_deref().unwrap(),
    );
    assert!(
        was.starts_with("blob(4) ") && now.starts_with("blob(4) "),
        "{was} {now}"
    );
    assert_ne!(was, now, "same size, other bytes: the text shows it");
    let lv = cell(changed[0], "level");
    assert_eq!(
        (lv.before.as_deref(), lv.after.as_deref()),
        (None, Some("3"))
    );
    let new = rows_of(&d, "mesh", RowKind::Created);
    assert_eq!(cell(new[0], "indices").after.as_deref(), Some("blob(1)"));
    assert!(
        new[0]
            .cells
            .iter()
            .all(|x| x.col != "level" && x.col != "id"),
        "NULL and the rowid column are not cells: {:?}",
        new[0]
    );
}

#[test]
fn diff_ignores_new_rowid_of_the_same_keyed_row_and_service_tables() {
    let c = copies("churn");
    let m = open_live(&c);
    let db = m.sql();
    let put = "INSERT INTO prop(kind,ref,key,value,unit) VALUES ('wall',1,?1,'v','')";
    db.execute(put, ["a"]).unwrap();
    Model::backup_file(&live_path(&c), &c.base, &mut go).unwrap();
    let rid = |k: &str| -> i64 {
        db.query_row("SELECT rowid FROM prop WHERE key=?1", [k], |r| r.get(0))
            .unwrap()
    };
    let before = rid("a");
    db.execute(put, ["z"]).unwrap();
    db.execute("DELETE FROM prop WHERE key='a'", []).unwrap();
    db.execute(put, ["a"]).unwrap();
    db.execute("DELETE FROM prop WHERE key='z'", []).unwrap();
    assert_ne!(rid("a"), before, "the row came back under another rowid");
    let d = m.diff_from(&c.base, &all(), 10).unwrap();
    assert_eq!(d, ModelDiff::default(), "an undo looks like this");

    db.execute("UPDATE prop SET value='w' WHERE key='a'", [])
        .unwrap();
    let d = m.diff_from(&c.base, &all(), 10).unwrap();
    assert_eq!(d.rows.len(), 1, "{d:?}");
    assert_eq!(d.rows[0].kind, RowKind::Changed, "paired by the key");
    db.execute("UPDATE prop SET value='v' WHERE key='a'", [])
        .unwrap();

    m.audit_append(&crate::AuditEntry::default()).unwrap();
    m.set_ai_policy("k", "v").unwrap();
    db.execute(
        "UPDATE meta SET value=value+1 WHERE key='schema_version'",
        [],
    )
    .unwrap();
    let d = m.diff_from(&c.base, &all(), 10).unwrap();
    assert_eq!(d, ModelDiff::default(), "service tables are not compared");
}

#[test]
fn changing_the_current_phase_makes_the_plan_stale_and_shows_in_the_diff() {
    let c = copies("phase");
    let mut live = open_live(&c);
    live.add_level("L0", 0.0).unwrap();
    Model::backup_file(&live_path(&c), &c.base, &mut go).unwrap();
    let head = live.change_head();
    let cur = live.current_phase().unwrap();
    let other: i64 = live
        .sql()
        .query_row("SELECT id FROM phase WHERE id<>?1 LIMIT 1", [cur], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(!live.plan_stale(head));

    live.set_current_phase(other).unwrap();
    assert!(live.changes_since(head).is_empty(), "views need no rebuild");
    assert!(
        live.plan_stale(head),
        "new elements would get another phase"
    );
    assert!(
        !live.plan_stale(live.change_head()),
        "a new copy has a new head"
    );
    let d = live
        .diff_from(&c.base, &live.changes_since(head), 10)
        .unwrap();
    let meta = rows_of(&d, "meta", RowKind::Changed);
    assert_eq!(meta.len(), 1, "{d:?}");
    let was = cell(meta[0], "phase_current");
    assert_eq!(was.before, Some(cur.to_string()));
    assert_eq!(was.after, Some(other.to_string()));

    live.set_current_phase(cur).unwrap();
    assert_eq!(
        live.diff_from(&c.base, &all(), 10).unwrap(),
        ModelDiff::default(),
        "a REPLACE back gives another rowid, not a difference"
    );
    live.sql()
        .execute("INSERT INTO meta(key,value) VALUES ('ai2_probe',1)", [])
        .unwrap();
    let d = live.diff_from(&c.base, &Changes::default(), 10).unwrap();
    assert_eq!(
        rows_of(&d, "meta", RowKind::Created).len(),
        1,
        "any setting counts"
    );
}

#[test]
fn atomic_leaves_the_file_untouched_until_commit_and_survives_a_panic() {
    let c = copies("atomic");
    let mut m = open_live(&c);
    m.add_level("L0", 0.0).unwrap();
    let (top, h0, head) = (m.op_top().unwrap(), hash(&m), m.change_head());
    let path = live_path(&c);
    let on_disk = || -> i64 {
        Connection::open(&path)
            .unwrap()
            .query_row("SELECT count(*) FROM level", [], |r| r.get(0))
            .unwrap()
    };
    let before = on_disk();
    m.atomic("wall_ai_plan", |m| {
        m.add_level("L1", 3.0)?;
        assert_eq!(
            on_disk(),
            before,
            "a crash here loses the whole plan: not committed"
        );
        assert_eq!(
            m.change_head(),
            head,
            "nothing is published before the commit"
        );
        assert!(
            m.add_level("L1", 9.0).is_err(),
            "a failed step undoes only itself"
        );
        m.add_level("L2", 6.0)?;
        assert_eq!(on_disk(), before);
        Ok(())
    })
    .unwrap();
    assert_eq!(on_disk(), before + 2);
    assert_eq!(m.op_top().unwrap(), top + 1, "one journal record");
    assert!(m.change_head() > head);
    m.undo().unwrap();
    assert_eq!(hash(&m), h0);
    assert_eq!(on_disk(), before);

    let head = m.change_head();
    let hit = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let _ = m.atomic("wall_ai_plan", |m| -> Result<(), Error> {
            m.add_level("L1", 3.0)?;
            panic!("exec failed");
        });
    }));
    assert!(hit.is_err(), "the panic goes on to the caller");
    assert!(m.sql().is_autocommit(), "no transaction is left open");
    assert_eq!((hash(&m), m.op_top().unwrap()), (h0, top));
    assert_eq!(on_disk(), before);
    assert_eq!(m.change_head(), head, "a rollback publishes nothing");
    m.add_level("L9", 9.0).unwrap();
    assert_eq!(m.op_top().unwrap(), top + 1, "the model still works");

    m.sql().execute_batch("BEGIN").unwrap();
    assert!(m.atomic("wall_ai_plan", |m| m.add_level("X", 1.0)).is_err());
    m.sql().execute_batch("ROLLBACK").unwrap();
}

/// BEGIN inside an open `atomic` transaction would fail: model writes go through `ops::scope`.
#[test]
fn model_writes_can_run_inside_atomic() {
    fn scan(dir: &Path, bad: &mut Vec<String>) {
        let needle = concat!("self.db", ".transaction()");
        for e in fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                scan(&p, bad);
            } else if p.extension().is_some_and(|x| x == "rs")
                && fs::read_to_string(&p).unwrap().contains(needle)
            {
                bad.push(p.display().to_string());
            }
        }
    }
    let mut bad = vec![];
    scan(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut bad);
    assert!(bad.is_empty(), "use ops::scope: {bad:?}");
}

#[test]
fn reset_work_reports_a_log_it_cannot_remove_and_keeps_the_old_copy() {
    let c = copies("wal");
    let mut live = open_live(&c);
    live.add_level("L0", 0.0).unwrap();
    c.prepare(&live_path(&c), &mut go).unwrap();
    let h0 = hash(&live);
    {
        let mut w = open_work(&c);
        w.add_level("L1", 3.0).unwrap();
    }
    let h1 = hash(&open_work(&c));
    let log = sidecar(&c.work, "-wal");
    let _ = fs::remove_file(&log);
    // a directory stands for a file that cannot be removed
    fs::create_dir(&log).unwrap();
    assert!(c.reset_work().is_err());
    assert!(!sidecar(&c.work, ".part").exists());
    fs::remove_dir(&log).unwrap();
    assert_eq!(hash(&open_work(&c)), h1, "the old copy is intact");

    c.reset_work().unwrap();
    assert_eq!(hash(&open_work(&c)), h0);
}

#[cfg(windows)]
#[test]
fn reset_work_refuses_an_open_work_copy() {
    let c = copies("open");
    let mut live = open_live(&c);
    live.add_level("L0", 0.0).unwrap();
    c.prepare(&live_path(&c), &mut go).unwrap();
    let mut w = open_work(&c);
    w.add_level("L1", 3.0).unwrap();
    let h1 = hash(&w);
    assert!(
        c.reset_work().is_err(),
        "the log and the file are held open"
    );
    assert_eq!(hash(&w), h1, "the open copy goes on working");
    drop(w);
    c.reset_work().unwrap();
}

#[test]
fn promote_work_refuses_a_busy_log_and_keeps_the_old_base() {
    let c = copies("busy");
    let mut live = open_live(&c);
    live.add_level("L0", 0.0).unwrap();
    c.prepare(&live_path(&c), &mut go).unwrap();
    let h0 = hash(&live);
    let mut w = open_work(&c);
    w.sql().busy_timeout(Duration::from_millis(10)).unwrap();
    w.add_level("L1", 3.0).unwrap();
    let reader = Connection::open(&c.work).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    reader
        .query_row("SELECT count(*) FROM level", [], |_| Ok(()))
        .unwrap();
    w.add_level("L2", 6.0).unwrap();
    let h2 = hash(&w);
    assert!(c.promote_work(&w).is_err(), "the reader holds the log");
    assert!(!sidecar(&c.base, ".part").exists());
    {
        let b = Model::open(c.base.to_str().unwrap()).unwrap();
        assert_eq!(hash(&b), h0, "the base did not change");
    }
    reader.execute_batch("COMMIT").unwrap();
    drop(reader);
    c.promote_work(&w).unwrap();
    let b = Model::open(c.base.to_str().unwrap()).unwrap();
    assert_eq!(hash(&b), h2);
}

/// 20 MiB with pages up to 4 KiB: the copy takes several steps.
const BIG: &str = "CREATE TABLE zz_big(b BLOB); CREATE TABLE zz_probe(n INTEGER);
    WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<20)
    INSERT INTO zz_big(b) SELECT zeroblob(1048576) FROM n;";

#[test]
fn backup_ends_under_a_commit_at_every_step_and_holds_its_snapshot() {
    let c = copies("commits");
    let m = open_live(&c);
    m.sql().execute_batch(BIG).unwrap();
    let mut seen: Vec<(u64, u64)> = vec![];
    Model::backup_file(&live_path(&c), &c.base, &mut |d: u64, t: u64| {
        seen.push((d, t));
        m.sql()
            .execute("INSERT INTO zz_probe(n) VALUES (1)", [])
            .unwrap();
        seen.len() < 1000
    })
    .unwrap();
    assert!(seen.len() >= 2, "more than one step: {seen:?}");
    assert!(
        seen.windows(2).all(|w| w[0].0 <= w[1].0),
        "no restart: {seen:?}"
    );
    let count = |p: &Path, t: &str| -> i64 {
        Connection::open(p)
            .unwrap()
            .query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(
        count(&c.base, "zz_probe"),
        0,
        "the state at the start of the copy"
    );
    assert_eq!(count(&c.base, "zz_big"), 20);
    assert_eq!(
        count(&live_path(&c), "zz_probe"),
        seen.len() as i64,
        "commits went through"
    );
}

#[test]
fn backup_of_a_rollback_journal_file_is_bounded_under_commits() {
    let c = copies("rollback");
    let src = c.dir.join("plain.db");
    let w = Connection::open(&src).unwrap();
    w.execute_batch(BIG).unwrap();
    let mut calls = 0;
    let _ = Model::backup_file(&src, &c.base, &mut |_: u64, _: u64| {
        calls += 1;
        w.execute("INSERT INTO zz_probe(n) VALUES (1)", []).unwrap();
        calls < 1000
    });
    assert!(
        calls < 1000,
        "restarts are counted, the copy gives up: {calls}"
    );
}

#[test]
fn prepare_reports_one_scale_and_can_be_cancelled_at_the_work_copy() {
    let c = copies("scale");
    let mut live = open_live(&c);
    live.add_level("L0", 0.0).unwrap();
    let src = live_path(&c);
    let mut seen: Vec<(u64, u64)> = vec![];
    c.prepare(&src, &mut |d: u64, t: u64| {
        seen.push((d, t));
        true
    })
    .unwrap();
    assert!(seen.windows(2).all(|w| w[0].0 <= w[1].0), "{seen:?}");
    let last = *seen.last().unwrap();
    assert_eq!(last.0, last.1, "{seen:?}");
    assert_eq!(hash(&open_work(&c)), hash(&live));

    fs::remove_file(&c.work).unwrap();
    let res = c.prepare(&src, &mut |d: u64, t: u64| d * 2 <= t);
    assert!(res.is_err(), "cancelled past the base");
    assert!(c.base.exists());
    assert!(!c.work.exists() && !sidecar(&c.work, ".part").exists());

    // elsewhere the file may be sparse and the check says nothing
    if cfg!(windows) {
        assert!(
            reserve(&c.dir, i64::MAX as u64).is_err(),
            "no disk holds this"
        );
    }
    assert!(!c.dir.join("reserve.tmp").exists());
}

#[test]
fn copies_are_private_to_their_owner_and_abandoned_ones_are_swept() {
    let (a, b) = (copies("same"), copies("same"));
    assert_ne!(a.dir, b.dir, "one session name does not share a folder");
    let kept = b.dir.clone();
    drop(a);
    assert!(kept.is_dir(), "dropping one keeps the other");

    let root = b.dir.join("root");
    let dir = |n: &str| {
        let d = root.join(n);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("base.db"), b"x").unwrap();
        d
    };
    let dead = dir("ddc-ai-1-dead-0");
    drop(hold_lock(&dead).unwrap());
    let alive = dir("ddc-ai-2-alive-0");
    let held = hold_lock(&alive).unwrap();
    let foreign = dir("ddc-ai-audit-3");
    let other = dir("something-else");
    fs::write(other.join(LOCK_FILE), b"").unwrap();

    sweep_abandoned(&root, Duration::from_secs(3600));
    assert!(dead.exists(), "too young: the owner may be starting");
    sweep_abandoned(&root, Duration::ZERO);
    assert!(!dead.exists(), "the owner is gone");
    assert!(alive.exists(), "the owner holds the lock");
    assert!(foreign.exists(), "no lock file: not ours");
    assert!(other.exists(), "not our prefix");
    drop(held);
}

#[test]
fn clash_comment_shows_in_the_diff_and_comes_back_on_undo() {
    let c = copies("clash");
    let mut live = open_live(&c);
    let r = rich(&mut live);
    let (a, b) = (("wall".to_string(), r.wall), ("column".to_string(), r.col));
    let hit = ClashHit {
        a: a.clone(),
        b: b.clone(),
        volume: 1.0,
        lo: [0.0; 3],
        hi: [1.0; 3],
        level: None,
    };
    live.clash_merge("t", &[hit]).unwrap();
    live.clash_set_status("t", &a, &b, ClashStatus::Reviewed, "keep me")
        .unwrap();
    Model::backup_file(&live_path(&c), &c.base, &mut go).unwrap();
    let pairs = |d: &ModelDiff| d.rows.iter().filter(|x| x.table == "clash_item").count();

    let head = live.change_head();
    assert!(!live.plan_stale(head));
    live.clash_set_status("t", &a, &b, ClashStatus::Reviewed, "keep me too")
        .unwrap();
    assert!(
        live.plan_stale(head),
        "a plan may remove what the user just wrote"
    );
    live.clash_set_status("t", &a, &b, ClashStatus::Reviewed, "keep me")
        .unwrap();

    let head = live.change_head();
    live.delete_wall(r.wall).unwrap();
    assert!(live.clash_items().unwrap().is_empty());
    let d = live
        .diff_from(&c.base, &live.changes_since(head), usize::MAX)
        .unwrap();
    let gone = rows_of(&d, "clash_item", RowKind::Deleted);
    assert_eq!(gone.len(), 1, "{d:?}");
    assert!(gone[0].secondary());
    assert!(d
        .rows
        .iter()
        .filter(|x| x.table == "wall")
        .all(|x| !x.secondary()));
    assert_eq!(cell(gone[0], "comment").before.as_deref(), Some("keep me"));
    assert!(gone[0].owner.is_some());
    assert!(
        gone[0].cells.iter().all(|x| x.col != "volume"),
        "recalculated by a run"
    );

    live.undo().unwrap();
    let it = live.clash_items().unwrap().remove(0);
    assert_eq!(
        (it.status, it.comment.as_str()),
        (ClashStatus::Reviewed, "keep me")
    );
    let d = live.diff_from(&c.base, &all(), usize::MAX).unwrap();
    assert_eq!(pairs(&d), 0, "one undo brings the pair back: {d:?}");

    // a run of the check moves only the recalculated columns
    let hit = ClashHit {
        a: a.clone(),
        b: b.clone(),
        volume: 2.0,
        lo: [0.0; 3],
        hi: [2.0; 3],
        level: None,
    };
    live.clash_merge("t", &[hit]).unwrap();
    let d = live.diff_from(&c.base, &all(), usize::MAX).unwrap();
    assert_eq!(pairs(&d), 0, "{d:?}");

    // a reopened file has the snapshot trigger too
    let p = live_path(&c);
    drop(live);
    let mut again = Model::open(p.to_str().unwrap()).unwrap();
    again.delete_wall(r.wall).unwrap();
    again.undo().unwrap();
    assert_eq!(again.clash_items().unwrap()[0].comment, "keep me");
}

#[test]
fn a_column_of_one_side_is_compared_against_its_default() {
    let c = copies("column");
    let mut m = open_live(&c);
    let (l1, l2) = (
        m.add_level("L1", 0.0).unwrap(),
        m.add_level("L2", 3.0).unwrap(),
    );
    m.add_level("L3", 6.0).unwrap();
    Model::backup_file(&live_path(&c), &c.base, &mut go).unwrap();
    let db = m.sql();
    db.execute_batch(
        "ALTER TABLE level ADD COLUMN zz_plain TEXT;
         ALTER TABLE level ADD COLUMN zz_num INTEGER NOT NULL DEFAULT 7;",
    )
    .unwrap();
    assert_eq!(
        m.diff_from(&c.base, &all(), 10).unwrap(),
        ModelDiff::default(),
        "old rows read what ALTER gives them"
    );
    db.execute("UPDATE level SET zz_plain='x' WHERE id=?1", [l1])
        .unwrap();
    db.execute("UPDATE level SET zz_num=8 WHERE id=?1", [l2])
        .unwrap();
    let d = m.diff_from(&c.base, &all(), 10).unwrap();
    let ch = rows_of(&d, "level", RowKind::Changed);
    assert_eq!(ch.len(), 2, "{d:?}");
    let of = |id: i64| *ch.iter().find(|x| x.rowid == id).unwrap();
    let plain = cell(of(l1), "zz_plain");
    assert_eq!(
        (plain.before.as_deref(), plain.after.as_deref()),
        (None, Some("x"))
    );
    let num = cell(of(l2), "zz_num");
    assert_eq!(
        (num.before.as_deref(), num.after.as_deref()),
        (Some("7"), Some("8"))
    );
}

#[test]
fn derived_tables_are_not_shown_as_edits() {
    let c = copies("derived");
    let m = open_live(&c);
    Model::backup_file(&live_path(&c), &c.base, &mut go).unwrap();
    let db = m.sql();
    db.execute("INSERT INTO mass_cache(mass,key) VALUES (1,'k')", [])
        .unwrap();
    db.execute(
        "INSERT INTO join_cache(kind,ref,key,ratios) VALUES ('wall',1,'k','r')",
        [],
    )
    .unwrap();
    db.execute("INSERT INTO dim_geo(dim,orphan,geom) VALUES (1,0,'g')", [])
        .unwrap();
    db.execute(
        "INSERT INTO struct_issue(kind,ref,problem) VALUES ('raft',1,'p')",
        [],
    )
    .unwrap();
    assert_eq!(
        m.diff_from(&c.base, &all(), 10).unwrap(),
        ModelDiff::default()
    );
}

#[test]
fn numbers_compare_exactly_like_except() {
    let big = (1_i64 << 53) + 1;
    let real = (1_i64 << 53) as f64;
    assert!(!same(&Value::Integer(big), &Value::Real(real)));
    assert!(same(&Value::Integer(1 << 53), &Value::Real(real)));
    assert!(same(&Value::Real(1.0), &Value::Integer(1)));
    assert!(!same(&Value::Integer(1), &Value::Real(1.5)));
    assert!(!same(
        &Value::Integer(i64::MAX),
        &Value::Real(9_223_372_036_854_775_808.0)
    ));
}

#[test]
fn diff_handles_a_table_of_one_side_and_a_missing_base() {
    let c = copies("sides");
    let m = open_live(&c);
    Model::backup_file(&live_path(&c), &c.base, &mut go).unwrap();
    m.sql()
        .execute_batch(
            "CREATE TABLE zz_new(id INTEGER PRIMARY KEY, v TEXT);
             INSERT INTO zz_new(id,v) VALUES (4,'a');",
        )
        .unwrap();
    let d = m.diff_from(&c.base, &all(), 10).unwrap();
    assert!(has(&d, "zz_new", 4, RowKind::Created), "{d:?}");
    assert_eq!(rows_of(&d, "zz_new", RowKind::Created)[0].cells.len(), 1);

    let nope = c.dir.join("nope.db");
    assert!(m.diff_from(&nope, &all(), 10).is_err());
    assert!(!nope.exists(), "ATTACH must not create an empty base");
    assert!(
        m.diff_from(&c.base, &all(), 10).is_ok(),
        "the alias is released"
    );
}

fn find_qty<'a>(all: &'a [MaterialQty], material: &str, level: i64) -> &'a MaterialQty {
    all.iter()
        .find(|q| q.material == material && q.level == level)
        .unwrap_or_else(|| panic!("no {material} on {level}: {all:?}"))
}

#[test]
fn material_totals_match_manual_counts() {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let l2 = m.add_level("L2", 3.0).unwrap();
    let wt = m.add_wall_type("W", &layer("Brick", 0.2)).unwrap();
    let st = m.add_slab_type("S", &layer("Concrete", 0.25)).unwrap();
    let mt = m
        .add_member_type("C", &MemberShape::Rect { b: 0.4, h: 0.4 }, "Concrete")
        .unwrap();
    m.add_wall(&wall_at(lv, wt, (100.0, 100.0), (110.0, 100.0)))
        .unwrap();
    m.add_slab(&SlabSpec {
        level: lv,
        slab_type: st,
        offset: 0.0,
        outer: ring(0.0, 0.0, 10.0, 8.0),
        holes: vec![],
    })
    .unwrap();
    m.add_column(&column(lv, mt, 50.0, 50.0)).unwrap();
    m.add_slab(&SlabSpec {
        level: l2,
        slab_type: st,
        offset: 0.0,
        outer: ring(0.0, 0.0, 4.0, 5.0),
        holes: vec![],
    })
    .unwrap();

    let close = |a: f64, b: f64| (a - b).abs() < 1e-6;
    let totals = m.material_totals(None).unwrap();
    let brick = find_qty(&totals, "Brick", lv);
    assert!(
        close(brick.volume, 6.0) && close(brick.area, 2.0),
        "{brick:?}"
    );
    let concrete = find_qty(&totals, "Concrete", lv);
    assert!(close(concrete.volume, 20.0 + 0.48), "{concrete:?}");
    let upper = find_qty(&totals, "Concrete", l2);
    assert!(
        close(upper.volume, 5.0) && close(upper.area, 20.0),
        "{upper:?}"
    );

    let only = m.material_totals(Some(l2)).unwrap();
    assert_eq!(only.len(), 1);
    assert_eq!(only[0], *upper);
    let empty = m.add_level("L3", 6.0).unwrap();
    assert!(m.material_totals(Some(empty)).unwrap().is_empty());
}

#[test]
fn audit_and_policy_tables_exist_at_current_version() {
    let m = Model::open(":memory:").unwrap();
    assert_eq!(m.schema_version().unwrap(), crate::schema::VERSION);
    let names = table_names(m.sql(), "main").unwrap();
    assert!(names.iter().any(|t| t == "ai_audit"));
    assert!(names.iter().any(|t| t == "ai_policy"));
}

/// Budgets for 100k rows:`cargo test --release -p ddc-model bench_100k -- --ignored --nocapture`.
#[test]
#[ignore = "bench on 100k rows"]
fn bench_100k_changeset() {
    const N: i64 = 100_000;
    let c = copies("bench");
    let mut m = open_live(&c);
    let lv = m.add_level("L1", 0.0).unwrap();
    let wt = m.add_wall_type("W", &layer("Brick", 0.2)).unwrap();
    let t = Instant::now();
    m.batch_walls("wall_seed", |db| {
        db.execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<?3)
             INSERT INTO wall(level,wtype,ax,ay,bx,by,base_offset,height,offset0)
             SELECT ?1,?2,i*3.0,0.0,i*3.0+1.0,0.0,0.0,3.0,NULL FROM n",
            params![lv, wt, N],
        )?;
        Ok(())
    })
    .unwrap();
    let seed = t.elapsed();
    let ids: Vec<i64> = {
        let mut st = m.sql().prepare("SELECT id FROM wall").unwrap();
        let v = st
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        v
    };
    assert_eq!(ids.len() as i64, N);

    let t = Instant::now();
    c.prepare(&live_path(&c), &mut go).unwrap();
    let copy = t.elapsed();

    let (h0, seq, head) = (hash(&m), m.op_top().unwrap(), m.change_head());
    let t = Instant::now();
    m.atomic("wall_ai_plan", |m| {
        m.batch_walls_edit("wall_bench", |db, snaps| {
            for id in &ids {
                snaps.save(db, "wall", "id", *id)?;
            }
            db.execute("UPDATE wall SET height=height+1", [])?;
            Ok(())
        })
    })
    .unwrap();
    let apply = t.elapsed();
    assert_eq!(m.op_top().unwrap(), seq + 1);

    let ch = m.changes_since(head);
    assert!(ch.whole("wall"));
    let t = Instant::now();
    let d = m.diff_from(&c.base, &ch, (2 * N) as usize).unwrap();
    let diff = t.elapsed();
    let walls = rows_of(&d, "wall", RowKind::Changed);
    assert_eq!(walls.len() as i64, N);
    assert!(walls.iter().all(|r| r.cells.len() == 1));

    let t = Instant::now();
    let first = m.diff_from(&c.base, &ch, 1000).unwrap();
    let cut = t.elapsed();
    assert_eq!(first.rows.len(), 1000);
    assert!(first.truncated);

    let t = Instant::now();
    m.undo().unwrap();
    let undo = t.elapsed();
    assert_eq!(hash(&m), h0);
    assert_eq!(
        m.diff_from(&c.base, &all(), usize::MAX).unwrap(),
        ModelDiff::default()
    );
    eprintln!(
        "100k walls: seed {seed:?}, copy {copy:?}, apply {apply:?}, diff {diff:?}, \
         diff limit 1000 {cut:?}, undo {undo:?}"
    );
    if !cfg!(debug_assertions) {
        assert!(copy < Duration::from_secs(5), "copy {copy:?}");
        assert!(diff < Duration::from_secs(5), "diff {diff:?}");
    }
}
