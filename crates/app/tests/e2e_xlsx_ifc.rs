//! XLSX round trip on an IFC-sourced model: import a file, export the book, edit cells
//! (height, Mark, a user parameter, a wall's level), apply, save the patched `-edited.ifc`
//! and read it back. The edits must be in the file, GlobalIds and untouched elements must
//! not move, an undone Apply must save the original, and a book that lacks rows removes
//! nothing unless delete-missing is on.

mod e2e_common;

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use calamine::{open_workbook, Data, Reader, Xlsx};
use ddc_app::editor::{Editor, OpFn};
use ddc_io::ifc::patch::{check_patched, patch_file, PatchOutput};
use ddc_io::ifc::{export_ifc, import_ifc_file, import_ifc_path};
use ddc_io::xlsx::{self, ImportOptions, Report};
use ddc_model::{
    LayerSpec, Model, OpeningSpec, ParamSpec, ParamType, RoomSpec, SlabSpec, WallSpec,
};
use e2e_common::{assert_clean, count, journal_len, temp_file};
use rust_xlsxwriter::Workbook;

type Grid = Vec<Vec<Data>>;

const MARK: &str = "W-01";
const FIRE: &str = "EI 60";
const NEW_HEIGHT: f64 = 2.9;
const KINDS: [&str; 4] = ["wall", "opening", "slab", "room"];
/// Product and spatial entity types whose counts must not change.
const TYPES: [&str; 7] = [
    "IFCWALL",
    "IFCOPENINGELEMENT",
    "IFCDOOR",
    "IFCWINDOW",
    "IFCSLAB",
    "IFCSPACE",
    "IFCBUILDINGSTOREY",
];

fn layer(material: &str, thickness: f64) -> LayerSpec {
    LayerSpec {
        material: material.into(),
        thickness,
    }
}

/// Two levels; on L1 a 6x4 box, a partition, a door, a window, a slab, a room; one wall on L2.
fn source_model() -> Model {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let up = m.add_level("L2", 3.0).unwrap();
    let ext = m
        .add_wall_type("ext", &[layer("brick", 0.2), layer("insulation", 0.1)])
        .unwrap();
    let int = m.add_wall_type("int", &[layer("gypsum", 0.125)]).unwrap();
    let p = [(0., 0.), (6., 0.), (6., 4.), (0., 4.)];
    let spec = |a, b, level, height, wall_type, offset0| WallSpec {
        a,
        b,
        level,
        base_offset: 0.0,
        height,
        wall_type,
        offset0,
    };
    let ws: Vec<WallSpec> = (0..4)
        .map(|i| spec(p[i], p[(i + 1) % 4], lv, 3.0, ext, None))
        .collect();
    let ids = m.add_walls(&ws).unwrap();
    m.add_wall(&spec((3.0, 0.15), (3.0, 3.85), lv, 2.7, int, Some(0.0)))
        .unwrap();
    m.add_wall(&spec((0.0, 0.0), (6.0, 0.0), up, 3.0, ext, None))
        .unwrap();
    for (w, kind, along, width, sill, height) in [
        (0, "door", 1.5, 0.9, 0.0, 2.1),
        (1, "window", 1.0, 1.2, 0.9, 1.4),
    ] {
        m.add_opening(
            ids[w],
            &OpeningSpec {
                along,
                width,
                sill,
                height,
                kind: kind.into(),
            },
        )
        .unwrap();
    }
    let st = m
        .add_slab_type("floor", &[layer("screed", 0.05), layer("concrete", 0.15)])
        .unwrap();
    m.add_slab(&SlabSpec {
        level: lv,
        slab_type: st,
        offset: 0.0,
        outer: p.to_vec(),
        holes: vec![],
    })
    .unwrap();
    m.add_room(&RoomSpec {
        level: lv,
        name: "Office".into(),
        number: "101".into(),
        point: (1.5, 2.0),
        height: 2.7,
    })
    .unwrap();
    m
}

fn text_param(name: &str) -> ParamSpec {
    ParamSpec {
        name: name.into(),
        ptype: ParamType::Text,
        per_type: false,
        unit: String::new(),
        group: String::new(),
        guid: None,
        categories: vec!["wall".into()],
    }
}

/// The app's import path: one `run_op`, one undo step.
fn import_op(ed: &mut Editor, path: &Path) {
    let (p, done) = (path.to_path_buf(), Arc::new(Mutex::new(false)));
    let d = done.clone();
    let j = journal_len(&ed.model);
    ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
        import_ifc_path(m, &p).map_err(|e| ddc_model::Error::Invalid(e.to_string()))?;
        *d.lock().unwrap() = true;
        Ok(())
    })))
    .unwrap_or_else(|e| panic!("{}: import failed: {e}", path.display()));
    assert!(*done.lock().unwrap());
    assert_eq!(journal_len(&ed.model), j + 1, "import: one journal op");
}

struct Fx {
    ed: Editor,
    src: PathBuf,
    out: PathBuf,
    key: String,
    /// The partition (height edit), the door wall (Mark, Fire), the north wall (level move).
    part: i64,
    door_wall: i64,
    north: i64,
}

fn wall_at(m: &Model, ax: f64, ay: f64, bx: f64, by: f64, level: &str) -> i64 {
    m.sql()
        .query_row(
            "SELECT w.id FROM wall w JOIN level l ON l.id=w.level WHERE l.name=?5 \
             AND abs(w.ax-?1)<1e-6 AND abs(w.ay-?2)<1e-6 AND abs(w.bx-?3)<1e-6 AND abs(w.by-?4)<1e-6",
            rusqlite::params![ax, ay, bx, by, level],
            |r| r.get(0),
        )
        .unwrap_or_else(|e| panic!("no wall ({ax},{ay})-({bx},{by}) on {level}: {e}"))
}

/// IFC file from the source model, imported into an empty editor, with the two user
/// parameters defined (as one more undo step, before the book is exported).
fn fixture(tag: &str) -> Fx {
    let src = temp_file(&format!("tst16_{tag}.ifc"));
    std::fs::write(&src, export_ifc(&source_model()).unwrap()).unwrap();
    let out = temp_file(&format!("tst16_{tag}-edited.ifc"));
    let mut ed = Editor::new_in_memory().unwrap();
    ed.model = Model::open(":memory:").unwrap();
    import_op(&mut ed, &src);
    assert_clean(&ed.model, "import");
    for n in ["Mark", "Fire"] {
        ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
            m.add_param(&text_param(n)).map(drop)
        })))
        .unwrap();
    }
    let key = ed.model.sources().unwrap().remove(0).source;
    let m = &ed.model;
    assert_eq!((count(m, "level"), count(m, "wall")), (2, 6));
    let (part, door_wall, north) = (
        wall_at(m, 3.0, 0.15, 3.0, 3.85, "L1"),
        wall_at(m, 0.0, 0.0, 6.0, 0.0, "L1"),
        wall_at(m, 6.0, 4.0, 0.0, 4.0, "L1"),
    );
    Fx {
        ed,
        src,
        out,
        key,
        part,
        door_wall,
        north,
    }
}

fn row_id(d: &Data) -> Option<i64> {
    match d {
        Data::Float(x) => Some(*x as i64),
        Data::Int(i) => Some(*i),
        _ => None,
    }
}

fn col(g: &Grid, header: &str) -> usize {
    g[0].iter()
        .position(|d| d.to_string() == header)
        .unwrap_or_else(|| panic!("no column {header}: {:?}", g[0]))
}

fn set_by_id(g: &mut Grid, id: i64, header: &str, v: Data) {
    let c = col(g, header);
    let r = (1..g.len())
        .find(|&r| g[r].first().and_then(row_id) == Some(id))
        .unwrap_or_else(|| panic!("no row with id {id}"));
    while g[r].len() <= c {
        g[r].push(Data::Empty);
    }
    g[r][c] = v;
}

/// The book rewritten with every sheet's grid passed through `f`.
fn edit_book(src: &Path, dst: &Path, f: impl Fn(&str, &mut Grid)) {
    let mut wb: Xlsx<_> = open_workbook(src).unwrap();
    let mut out = Workbook::new();
    for name in wb.sheet_names() {
        let mut grid: Grid = wb
            .worksheet_range(&name)
            .unwrap()
            .rows()
            .map(|r| r.to_vec())
            .collect();
        f(&name, &mut grid);
        let ws = out.add_worksheet();
        ws.set_name(&name).unwrap();
        for (i, row) in grid.iter().enumerate() {
            for (j, d) in row.iter().enumerate() {
                let (i, j) = (i as u32, j as u16);
                match d {
                    Data::Float(x) => ws.write_number(i, j, *x).map(drop),
                    Data::Int(x) => ws.write_number(i, j, *x as f64).map(drop),
                    Data::String(s) => ws.write_string(i, j, s).map(drop),
                    _ => Ok(()),
                }
                .unwrap();
            }
        }
    }
    out.save(dst).unwrap();
}

fn options(apply: bool, delete_missing: bool) -> ImportOptions {
    ImportOptions {
        apply,
        delete_missing,
        new_params: false,
    }
}

fn preview(m: &mut Model, book: &Path, delete_missing: bool) -> Report {
    let rep = xlsx::import_xlsx_with(m, book, options(false, delete_missing)).unwrap();
    assert!(rep.rejected.is_empty(), "{:?}", rep.rejected);
    rep
}

fn export_book(fx: &Fx, tag: &str) -> PathBuf {
    let p = temp_file(&format!("tst16_{tag}.xlsx"));
    xlsx::export_xlsx(&fx.ed.model, &p).unwrap();
    p
}

impl Fx {
    /// Export the styled book, edit height, Mark, Fire and a level, preview and Apply.
    fn xlsx_edit(&mut self, tag: &str) {
        let book = export_book(self, &format!("{tag}_src"));
        let edited = temp_file(&format!("tst16_{tag}_edited.xlsx"));
        let (part, door, north) = (self.part, self.door_wall, self.north);
        edit_book(&book, &edited, |sheet, g| {
            if sheet != "Walls" {
                return;
            }
            set_by_id(g, part, "height [m]", Data::Float(NEW_HEIGHT));
            set_by_id(g, door, "Mark", Data::String(MARK.into()));
            set_by_id(g, door, "Fire", Data::String(FIRE.into()));
            set_by_id(g, north, "level", Data::String("L2".into()));
        });
        let rep = preview(&mut self.ed.model, &edited, false);
        assert!(
            rep.counts().changed >= 3 && rep.removed.is_empty(),
            "{:?}",
            rep.changes
        );
        let ops = journal_len(&self.ed.model);
        self.ed.apply_xlsx(rep.batch(), tag).unwrap();
        assert_clean(&self.ed.model, "xlsx applied");
        assert!(journal_len(&self.ed.model) > ops);
        let m = &self.ed.model;
        assert!(near(wall_height(m, part), NEW_HEIGHT));
        assert_eq!(wall_level(m, north), "L2");
    }

    /// Save as the app does for an imported file: the patch writer, then its own read-back.
    fn save(&self) -> PatchOutput {
        let p = patch_file(&self.ed.model, &self.key, &self.src, &self.out, None)
            .unwrap_or_else(|e| panic!("patch refused: {e}"));
        if let Some(e) = &p.expect {
            check_patched(&self.out, e, None).unwrap();
        }
        p
    }

    fn guid_of(&self, id: i64) -> String {
        guid_of(&self.ed.model, "wall", id)
    }
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-4
}

fn wall_height(m: &Model, id: i64) -> f64 {
    m.sql()
        .query_row("SELECT height FROM wall WHERE id=?1", [id], |r| r.get(0))
        .unwrap()
}

fn wall_level(m: &Model, id: i64) -> String {
    m.sql()
        .query_row(
            "SELECT l.name FROM wall w JOIN level l ON l.id=w.level WHERE w.id=?1",
            [id],
            |r| r.get(0),
        )
        .unwrap()
}

fn guid_of(m: &Model, kind: &str, id: i64) -> String {
    m.sql()
        .query_row(
            "SELECT guid FROM ext_ref WHERE kind=?1 AND ref=?2",
            rusqlite::params![kind, id],
            |r| r.get(0),
        )
        .unwrap_or_else(|e| panic!("no GlobalId for {kind} {id}: {e}"))
}

fn all_guids(m: &Model) -> HashSet<String> {
    let mut st = m.sql().prepare("SELECT guid FROM ext_ref").unwrap();
    let v = st
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    v
}

/// The saved file as a fresh model, and its source key.
fn reopen(fx: &Fx, tag: &str) -> (Model, String) {
    let back = temp_file(&format!("tst16_{tag}_back.ifc"));
    std::fs::copy(&fx.out, &back).unwrap();
    let mut m = Model::open(":memory:").unwrap();
    import_ifc_file(&mut m, &back).unwrap();
    let key = m.sources().unwrap().remove(0).source;
    (m, key)
}

/// Row of the element with this GlobalId in the reopened model.
fn row_of(m: &Model, key: &str, guid: &str) -> i64 {
    m.element_by_guid(key, guid)
        .unwrap()
        .unwrap_or_else(|| panic!("GlobalId {guid} is gone from the saved file"))
        .1
}

/// Whether some property of the element holds this value.
fn has_value(m: &Model, kind: &str, id: i64, v: &str) -> bool {
    m.sql()
        .query_row(
            "SELECT count(*) FROM prop WHERE kind=?1 AND ref=?2 AND value=?3",
            rusqlite::params![kind, id, v],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
        > 0
}

fn hist(step: &str) -> BTreeMap<String, usize> {
    let mut h = BTreeMap::new();
    for l in step.lines() {
        let Some(rest) = l.strip_prefix('#') else {
            continue;
        };
        let Some((_, body)) = rest.split_once('=') else {
            continue;
        };
        let name: String = body
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect::<String>()
            .to_ascii_uppercase();
        *h.entry(name).or_default() += 1;
    }
    h
}

/// Product count of a type family: `IFCWALL` also counts `IFCWALLSTANDARDCASE`.
fn typed(h: &BTreeMap<String, usize>, prefix: &str) -> usize {
    h.iter()
        .filter(|(k, _)| {
            k.as_str() == prefix
                || (prefix == "IFCWALL" && k.starts_with("IFCWALLSTANDARDCASE"))
                || (prefix == "IFCDOOR" && k.starts_with("IFCDOORSTANDARDCASE"))
                || (prefix == "IFCWINDOW" && k.starts_with("IFCWINDOWSTANDARDCASE"))
        })
        .map(|(_, n)| n)
        .sum()
}

#[test]
fn xlsx_edits_reach_saved_ifc() {
    let mut fx = fixture("reach");
    let (part, door, north) = (fx.part, fx.door_wall, fx.north);
    let guids = (fx.guid_of(part), fx.guid_of(door), fx.guid_of(north));
    fx.xlsx_edit("reach");
    let p = fx.save();
    assert!(!p.no_changes && p.edited > 0, "{p:?}");

    let (m, key) = reopen(&fx, "reach");
    assert_clean(&m, "reopened");
    let (rp, rd, rn) = (
        row_of(&m, &key, &guids.0),
        row_of(&m, &key, &guids.1),
        row_of(&m, &key, &guids.2),
    );
    assert!(near(wall_height(&m, rp), NEW_HEIGHT), "height edit");
    assert_eq!(wall_level(&m, rn), "L2", "level edit");
    assert_eq!(wall_level(&m, rp), "L1");
    assert!(has_value(&m, "wall", rd, MARK), "Mark edit");
    assert!(has_value(&m, "wall", rd, FIRE), "user parameter edit");
    // the other walls kept their heights
    assert!(near(wall_height(&m, rd), 3.0) && near(wall_height(&m, rn), 3.0));
}

#[test]
fn xlsx_keeps_guids_and_untouched() {
    let mut fx = fixture("guids");
    let before = all_guids(&fx.ed.model);
    let edited: HashSet<String> = [fx.part, fx.door_wall, fx.north]
        .iter()
        .map(|&i| fx.guid_of(i))
        .collect();
    let src_text = std::fs::read_to_string(&fx.src).unwrap();
    fx.xlsx_edit("guids");
    assert_eq!(all_guids(&fx.ed.model), before, "Apply keeps every GlobalId");
    fx.save();

    let (m, key) = reopen(&fx, "guids");
    assert_eq!(all_guids(&m), before, "saved file keeps every GlobalId");
    for k in KINDS {
        assert_eq!(count(&m, k), count(&fx.ed.model, k), "{k} count");
    }
    assert_eq!(count(&m, "level"), 2);

    // entity counts of products and storeys are the file's own
    let out_text = std::fs::read_to_string(&fx.out).unwrap();
    let (hs, ho) = (hist(&src_text), hist(&out_text));
    for t in TYPES {
        assert_eq!(typed(&hs, t), typed(&ho, t), "{t} entity count");
    }
    assert!(typed(&hs, "IFCWALL") >= 6 && typed(&hs, "IFCBUILDINGSTOREY") == 2);

    // every element the book did not touch is the same statement, byte for byte
    let mut same = 0;
    for g in before.difference(&edited) {
        let needle = format!("'{g}'");
        let line = |t: &str| {
            t.lines()
                .find(|l| l.starts_with('#') && l.contains(&needle) && l.contains("IFC"))
                .map(str::to_string)
        };
        let (a, b) = (line(&src_text), line(&out_text));
        if a.is_some() {
            assert_eq!(a, b, "untouched element {g} was rewritten");
            same += 1;
        }
        // and it is still the same element after the reopen
        row_of(&m, &key, g);
    }
    assert!(same >= 5, "only {same} untouched statements found by GlobalId");
}

#[test]
fn xlsx_apply_undo_then_save_original() {
    let mut fx = fixture("undo");
    let (part, door, north) = (fx.part, fx.door_wall, fx.north);
    let guids = (fx.guid_of(part), fx.guid_of(door), fx.guid_of(north));
    let h0 = wall_height(&fx.ed.model, part);
    fx.xlsx_edit("undo");
    fx.ed.undo();
    assert_clean(&fx.ed.model, "undo");
    assert!(near(wall_height(&fx.ed.model, part), h0));
    assert_eq!(wall_level(&fx.ed.model, north), "L1");

    let p = fx.save();
    assert!(p.no_changes, "an undone Apply leaves nothing to write: {p:?}");
    assert_eq!(
        std::fs::read(&fx.out).unwrap(),
        std::fs::read(&fx.src).unwrap(),
        "the saved file is the source"
    );
    let (m, key) = reopen(&fx, "undo");
    let (rp, rd, rn) = (
        row_of(&m, &key, &guids.0),
        row_of(&m, &key, &guids.1),
        row_of(&m, &key, &guids.2),
    );
    assert!(near(wall_height(&m, rp), h0));
    assert_eq!((wall_level(&m, rn).as_str(), wall_level(&m, rd).as_str()), ("L1", "L1"));
    assert!(!has_value(&m, "wall", rd, MARK) && !has_value(&m, "wall", rd, FIRE));

    // redo brings the edits back into the saved file
    fx.ed.redo();
    fx.save();
    let (m, key) = reopen(&fx, "undo_redo");
    assert!(has_value(&m, "wall", row_of(&m, &key, &guids.1), MARK));
}

#[test]
fn xlsx_no_delete_missing_keeps_all() {
    let mut fx = fixture("keep");
    let src_text = std::fs::read_to_string(&fx.src).unwrap();
    let before = all_guids(&fx.ed.model);
    let counts: Vec<i64> = ["level", "wall", "opening", "slab", "room"]
        .iter()
        .map(|t| count(&fx.ed.model, t))
        .collect();
    let opening = fx
        .ed
        .model
        .sql()
        .query_row("SELECT min(id) FROM opening", [], |r| r.get::<_, i64>(0))
        .unwrap();

    // the book drops two wall rows and an opening row; one remaining wall gets a new height
    let book = export_book(&fx, "keep_src");
    let edited = temp_file("tst16_keep_edited.xlsx");
    let (north, door, part) = (fx.north, fx.door_wall, fx.part);
    edit_book(&book, &edited, |sheet, g| match sheet {
        "Walls" => {
            set_by_id(g, part, "height [m]", Data::Float(NEW_HEIGHT));
            g.retain(|r| !matches!(r.first().and_then(row_id), Some(i) if i == north || i == door));
        }
        "Openings" => g.retain(|r| r.first().and_then(row_id) != Some(opening)),
        _ => {}
    });
    let rep = preview(&mut fx.ed.model, &edited, false);
    assert!(
        rep.removed.len() >= 3,
        "the missing rows are reported as kept: {:?}",
        rep.removed
    );
    assert_eq!(rep.counts().deleted, 0, "{:?}", rep.changes);
    fx.ed.apply_xlsx(rep.batch(), "keep").unwrap();
    assert_clean(&fx.ed.model, "applied");
    let now: Vec<i64> = ["level", "wall", "opening", "slab", "room"]
        .iter()
        .map(|t| count(&fx.ed.model, t))
        .collect();
    assert_eq!(now, counts, "no element removed from the model");
    assert!(near(wall_height(&fx.ed.model, part), NEW_HEIGHT));

    fx.save();
    let (m, key) = reopen(&fx, "keep");
    let now: Vec<i64> = ["level", "wall", "opening", "slab", "room"]
        .iter()
        .map(|t| count(&m, t))
        .collect();
    assert_eq!(now, counts, "no element removed from the saved file");
    assert_eq!(all_guids(&m), before);
    let out_text = std::fs::read_to_string(&fx.out).unwrap();
    let (hs, ho) = (hist(&src_text), hist(&out_text));
    for t in TYPES {
        assert_eq!(typed(&hs, t), typed(&ho, t), "{t} entity count");
    }
    let g = guid_of(&fx.ed.model, "wall", part);
    assert!(near(wall_height(&m, row_of(&m, &key, &g)), NEW_HEIGHT));
}
