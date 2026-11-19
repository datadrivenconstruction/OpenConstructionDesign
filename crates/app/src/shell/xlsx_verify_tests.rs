//! Seeded books with locked, refused, conflicting, new and deleted rows: an honest Apply
//! passes the check; a batch changed between the plan and the write (test hook) is taken
//! back whole and the message names the first difference. A failure names its seed.
use std::path::{Path, PathBuf};

use ddc_io::xlsx::{self, TableCell as C};
use ddc_model::{Edit, Model, OpeningSpec, PVal, ParamSpec, ParamType, Swing, WallSpec};

use super::super::xlsx_win::{apply_begin, XlsxPreview};
use super::{set_tamper, Diff, Plan};
use crate::editor::Editor;
use crate::xl_text::Lang;

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n.max(1) as u64) as usize
    }
}

/// What the book does to one wall.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Row {
    Keep,
    Height,
    Offset,
    Param,
    /// The parameter cell is edited, the value is locked in the model after the export.
    Locked,
    /// An invalid height: the cell is refused, the rest applies.
    Refused,
    /// Height edited in the book and in the model since the export: the model value stays.
    Conflict,
}

const KINDS: [Row; 7] = [Row::Keep, Row::Height, Row::Offset, Row::Param, Row::Locked, Row::Refused, Row::Conflict];

struct World {
    ed: Editor,
    dir: PathBuf,
    book: PathBuf,
    walls: Vec<i64>,
    rows: Vec<Row>,
    /// Walls added after the export: the book never had them, so "delete missing" keeps them.
    gone: Vec<i64>,
    news: usize,
}

fn next(dir: &Path, gen: &mut usize) -> PathBuf {
    *gen += 1;
    dir.join(format!("gen{gen}.xlsx"))
}

fn world(tag: &str, seed: u64) -> World {
    let mut rng = Rng::new(seed);
    let dir = std::env::temp_dir().join(format!("ddc_xs1_{tag}_{seed}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let m = Model::open(dir.join("m.ddc").to_str().unwrap()).unwrap();
    let mut ed = Editor::seeded(m).unwrap();
    let (lv, ty) = (ed.level, ed.types[1].id);
    let wall = |x: f64, y: f64| WallSpec {
        a: (x, y),
        b: (x + 4.0, y),
        level: lv,
        base_offset: 0.0,
        height: 3.0,
        wall_type: ty,
        offset0: None,
    };
    let n = 4 + rng.below(5);
    let walls: Vec<i64> = (0..n).map(|i| ed.model.add_wall(&wall(6.0 * i as f64, 0.0)).unwrap()).collect();
    let spec = ParamSpec {
        name: "Load".into(),
        ptype: ParamType::Number,
        per_type: false,
        unit: String::new(),
        group: String::new(),
        guid: None,
        categories: vec!["wall".into()],
    };
    let mut edits = vec![Edit::NewParam(spec)];
    for (i, id) in walls.iter().enumerate() {
        let value = Some(PVal::Num(1.5 + i as f64));
        edits.push(Edit::ParamValue { param: "Load".into(), kind: "wall".into(), id: *id, value });
    }
    ed.model.apply_edits(&edits).unwrap();
    ed.reload().unwrap();
    let src = dir.join("src.xlsx");
    xlsx::export_xlsx_families(&ed.model, &src, xlsx::Units::Metric, &crate::elem::sheets::families()).unwrap();
    // wall 0 stays as exported (one hook deletes it), wall 1 always gets a new base offset
    let rows: Vec<Row> = (0..n)
        .map(|i| match i {
            0 => Row::Keep,
            1 => Row::Offset,
            _ => KINDS[rng.below(KINDS.len())],
        })
        .collect();
    let (mut cur, mut gen) = (src, 0);
    for (i, (id, row)) in walls.iter().zip(&rows).enumerate() {
        let k = i as f64;
        let cell = match row {
            Row::Keep => None,
            Row::Height => Some(("height [m]", 3.4 + 0.1 * k)),
            Row::Offset => Some(("base_offset [m]", 0.5)),
            Row::Param => Some(("Load", 10.5 + k)),
            Row::Locked => Some(("Load", 20.5 + k)),
            Row::Refused => Some(("height [m]", -1.0)),
            Row::Conflict => Some(("height [m]", 4.0)),
        };
        if let Some((head, v)) = cell {
            let to = next(&dir, &mut gen);
            xlsx::patch_cell(&cur, &to, "Walls", *id, head, v).unwrap_or_else(|e| panic!("seed {seed}: {head}: {e}"));
            cur = to;
        }
    }
    let news = rng.below(3);
    for k in 0..news {
        let f = 6.0 * k as f64;
        let tweaks = [
            ("ax [m]", C::Num(f)),
            ("ay [m]", C::Num(20.0)),
            ("bx [m]", C::Num(f + 4.0)),
            ("by [m]", C::Num(20.0)),
        ];
        let to = next(&dir, &mut gen);
        xlsx::clone_row_as_new(&cur, &to, "Walls", walls[0], &tweaks).unwrap();
        cur = to;
    }
    let book = dir.join("book.xlsx");
    std::fs::copy(&cur, &book).unwrap();
    for (id, row) in walls.iter().zip(&rows) {
        match row {
            Row::Locked => {
                ed.model.set_param_lock(&[("wall", *id, "Load")]).unwrap();
            }
            Row::Conflict => {
                ed.model.sql().execute("UPDATE wall SET height=3.5 WHERE id=?1", [id]).unwrap();
            }
            _ => {}
        }
    }
    let gone: Vec<i64> = (0..rng.below(3))
        .map(|k| ed.model.add_wall(&wall(6.0 * k as f64, -10.0)).unwrap())
        .collect();
    ed.reload().unwrap();
    World { ed, dir, book, walls, rows, gone, news }
}

impl World {
    /// The preview as the window shows it, with deletions on and confirmed.
    fn preview(&mut self) -> XlsxPreview {
        let mut pv = XlsxPreview::load(&mut self.ed.model, &self.book, true).unwrap();
        pv.rev = self.ed.content_snap();
        pv.confirm_mass = true;
        pv
    }

    fn done(self) {
        let World { ed, dir, .. } = self;
        drop(ed);
        let _ = std::fs::remove_dir_all(dir);
    }
}

fn op_count(m: &Model) -> i64 {
    m.sql().query_row("SELECT count(*) FROM op", [], |r| r.get(0)).unwrap()
}

fn wall_count(m: &Model) -> usize {
    m.sql().query_row("SELECT count(*) FROM wall", [], |r| r.get::<_, i64>(0)).unwrap() as usize
}

fn exists(m: &Model, id: i64) -> bool {
    m.sql().query_row("SELECT count(*) FROM wall WHERE id=?1", [id], |r| r.get::<_, i64>(0)).unwrap() > 0
}

fn wall_f(m: &Model, id: i64, col: &str) -> f64 {
    m.sql().query_row(&format!("SELECT {col} FROM wall WHERE id=?1"), [id], |r| r.get(0)).unwrap()
}

fn load(m: &Model, id: i64) -> Option<f64> {
    let d = m.param_by_name("Load").unwrap().unwrap();
    m.param_value(d.id, "wall", id).unwrap().and_then(|v| v.as_f64())
}

/// Every row of the tables the Apply writes, sorted (undo may put rows back in another order).
fn dump(m: &Model) -> Vec<String> {
    let mut out = vec![];
    for t in ["level", "wall", "wall_type", "pparam", "pparam_val", "param_lock"] {
        let mut st = m.sql().prepare(&format!("SELECT * FROM {t}")).unwrap();
        let n = st.column_count();
        let rows = st
            .query_map([], |r| {
                let mut s = format!("{t}:");
                for i in 0..n {
                    let v: rusqlite::types::Value = r.get(i)?;
                    s.push_str(&format!("{v:?}|"));
                }
                Ok(s)
            })
            .unwrap();
        out.extend(rows.map(|r| r.unwrap()));
    }
    out.sort();
    out
}

#[test]
fn honest_apply_passes_the_check_over_random_books() {
    for seed in 1..=16u64 {
        let mut w = world("ok", seed);
        let mut pv = w.preview();
        let ctx = format!("seed {seed}, rows {:?}, new {}, gone {:?}", w.rows, w.news, w.gone);
        assert_eq!(pv.report.blocking(), 0, "{ctx}: {:?}", pv.report.rejected);
        let ops = op_count(&w.ed.model);
        if let Err(e) = apply_begin(&mut w.ed, &mut pv, Lang::En) {
            panic!("{ctx}: honest Apply refused: {e}");
        }
        let m = &w.ed.model;
        assert_eq!(op_count(m), ops + 1, "{ctx}: one undo step");
        let walls = w.walls.len() + w.news + w.gone.len();
        assert_eq!(wall_count(m), walls, "{ctx}: new rows made, newer walls kept");
        for g in &w.gone {
            assert!(exists(m, *g), "{ctx}: wall {g} is newer than the export, so it stays");
        }
        for (i, (id, row)) in w.walls.iter().zip(&w.rows).enumerate() {
            let k = i as f64;
            let near = |a: f64, b: f64| (a - b).abs() < 1e-9;
            let ok = match row {
                Row::Keep => near(wall_f(m, *id, "height"), 3.0) && load(m, *id).is_some_and(|v| near(v, 1.5 + k)),
                Row::Height => near(wall_f(m, *id, "height"), 3.4 + 0.1 * k),
                Row::Offset => near(wall_f(m, *id, "base_offset"), 0.5),
                Row::Param => load(m, *id).is_some_and(|v| near(v, 10.5 + k)),
                Row::Locked => load(m, *id).is_some_and(|v| near(v, 1.5 + k)),
                Row::Refused => near(wall_f(m, *id, "height"), 3.0),
                Row::Conflict => near(wall_f(m, *id, "height"), 3.5),
            };
            assert!(ok, "{ctx}: wall {id} ({row:?}) is not as the preview showed");
        }
        let iss = m.check_integrity().unwrap();
        assert!(iss.is_empty(), "{ctx}: integrity {iss:?}");
        w.done();
    }
}

/// Clears the hook even when an assertion fails.
struct Hook;

impl Drop for Hook {
    fn drop(&mut self) {
        set_tamper(None);
    }
}

/// `how`: 0 a value written other than previewed, 1 a previewed row not written, 2 a row
/// deleted that the preview keeps.
fn diverged(seed: u64, how: usize) {
    let mut w = world("bad", seed);
    let mut pv = w.preview();
    let (w0, w1) = (w.walls[0], w.walls[1]);
    let before = dump(&w.ed.model);
    let ops = op_count(&w.ed.model);
    let top = w.ed.model.op_top().unwrap();
    let _hook = Hook;
    let want = match how {
        0 => {
            set_tamper(Some(Box::new(move |b: &mut Vec<Edit>| {
                for e in b.iter_mut() {
                    if let Edit::Wall { id, spec, .. } = e {
                        if *id == w1 {
                            spec.base_offset += 0.25;
                        }
                    }
                }
            })));
            format!("#{w1}, base_offset")
        }
        1 => {
            set_tamper(Some(Box::new(move |b: &mut Vec<Edit>| {
                b.retain(|e| !matches!(e, Edit::Wall { id, .. } if *id == w1));
            })));
            format!("#{w1}")
        }
        _ => {
            set_tamper(Some(Box::new(move |b: &mut Vec<Edit>| b.push(Edit::Delete { table: "wall", id: w0 }))));
            format!("#{w0}")
        }
    };
    let ctx = format!("seed {seed}, case {how}, rows {:?}", w.rows);
    let msg = match apply_begin(&mut w.ed, &mut pv, Lang::En) {
        Err(m) => m,
        Ok(_) => panic!("{ctx}: an Apply that differs from its preview went through"),
    };
    assert!(msg.contains(&want), "{ctx}: the message names {want}: {msg}");
    assert!(w.ed.status.as_deref() == Some(msg.as_str()), "{ctx}: the user is told: {:?}", w.ed.status);
    assert_eq!(dump(&w.ed.model), before, "{ctx}: the model is as before the Apply");
    assert_eq!((op_count(&w.ed.model), w.ed.model.op_top().unwrap()), (ops, top), "{ctx}: no step left");
    assert!(!pv.report.changes.is_empty(), "{ctx}: the preview stays whole for another try");
    w.ed.redo();
    assert_eq!(dump(&w.ed.model), before, "{ctx}: the taken-back step is not offered for Redo");
    w.done();
}

#[test]
fn diverged_apply_is_taken_back_and_named() {
    for seed in 1..=6u64 {
        for how in 0..3 {
            diverged(seed, how);
        }
    }
}

#[test]
fn plan_checks_values_and_creates() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("A", 0.0).unwrap();
    let e = [
        Edit::Level { id: l, name: "B".into(), elevation: 3.0 },
        Edit::NewLevel { name: "N".into(), elevation: 6.0 },
    ];
    let p = Plan::of(&m, &e, &[]).unwrap();
    m.apply_edits(&e).unwrap();
    assert_eq!(p.check(&m).unwrap(), None, "honest write");
    let p2 = Plan::of(&m, &[Edit::Level { id: l, name: "C".into(), elevation: 3.0 }], &[]).unwrap();
    m.apply_edits(&[Edit::Level { id: l, name: "D".into(), elevation: 3.0 }]).unwrap();
    match p2.check(&m).unwrap() {
        Some(Diff::Value { table, field, old, want, got, .. }) => {
            assert_eq!([table, field, old, want, got], ["level", "name", "'B'", "'C'", "'D'"].map(String::from));
        }
        d => panic!("{d:?}"),
    }
    let p3 = Plan::of(&m, &[Edit::NewLevel { name: "M".into(), elevation: 9.0 }], &[]).unwrap();
    assert_eq!(p3.check(&m).unwrap(), Some(Diff::Created { table: "level".into(), want: 1, got: 0 }));
}

#[test]
fn type_parameter_is_one_change_of_the_type_row() {
    let mut ed = Editor::seeded(Model::open(":memory:").unwrap()).unwrap();
    let spec = ParamSpec {
        name: "Rating".into(),
        ptype: ParamType::Text,
        per_type: true,
        unit: String::new(),
        group: String::new(),
        guid: None,
        categories: vec!["wall".into()],
    };
    ed.model.apply_edits(&[Edit::NewParam(spec)]).unwrap();
    let t = ed.types[0].id;
    let e = [Edit::ParamValue { param: "Rating".into(), kind: "wall_type".into(), id: t, value: Some(PVal::Text("EI60".into())) }];
    let p = Plan::of(&ed.model, &e, &[]).unwrap();
    ed.model.apply_edits(&e).unwrap();
    assert_eq!(p.check(&ed.model).unwrap(), None, "no element of the type is expected in the journal");
}

#[test]
fn opening_reusing_the_id_of_one_deleted_with_its_wall_is_no_difference() {
    let mut ed = Editor::seeded(Model::open(":memory:").unwrap()).unwrap();
    let (lv, ty) = (ed.level, ed.types[1].id);
    let w = |x: f64| WallSpec {
        a: (x, 0.0),
        b: (x + 4.0, 0.0),
        level: lv,
        base_offset: 0.0,
        height: 3.0,
        wall_type: ty,
        offset0: None,
    };
    let keep = ed.model.add_wall(&w(0.0)).unwrap();
    let gone = ed.model.add_wall(&w(10.0)).unwrap();
    let spec = || OpeningSpec { along: 1.0, width: 0.9, sill: 0.0, height: 2.1, kind: "door".into() };
    ed.model.add_opening(keep, &spec()).unwrap();
    let last = ed.model.add_opening(gone, &spec()).unwrap();
    fn no_set(_: &mut Model, _: i64, _: f64) -> Result<(), ddc_model::Error> {
        Ok(())
    }
    let del = xlsx::PropEdit {
        sheet: "Walls".into(),
        table: "wall",
        id: gone,
        key: xlsx::DELETE_HEAD,
        value: 1.0,
        set: no_set,
        delete: true,
    };
    let e = [Edit::NewOpening { wall: keep, spec: spec(), swing: Swing::default() }];
    let p = Plan::of(&ed.model, &e, std::slice::from_ref(&del)).unwrap();
    ed.model.delete_wall(gone).unwrap();
    ed.model.apply_edits(&e).unwrap();
    let max: i64 = ed.model.sql().query_row("SELECT max(id) FROM opening", [], |r| r.get(0)).unwrap();
    assert_eq!(max, last, "the new opening took the freed id");
    assert_eq!(p.check(&ed.model).unwrap(), None, "honest write with a reused id");
}

#[test]
fn difference_text_in_both_languages() {
    let d = Diff::ExtraDeleted { table: "wall".into(), id: 7 };
    let (en, ru) = (d.text(false), d.text(true));
    assert!(en.contains("#7") && ru.contains("#7") && en != ru, "{en} | {ru}");
    assert!(!en.contains("xlsx_verify.") && !ru.contains("xlsx_verify."), "{en} | {ru}");
}
