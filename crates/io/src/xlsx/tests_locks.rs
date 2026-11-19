//! Lock matrix of the XLSX round trip: every column of every sheet has a lock class; an edit
//! of a locked or read-only cell gives a visible refused row with a localized reason, is never
//! applied (model and journal unchanged), and a re-export keeps the lock style and the sheet
//! protection. A column without a class fails `every_catalog_column_has_a_lock_class`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use calamine::{open_workbook, Data, Reader, Xlsx};
use ddc_model::{
    BeamJoin, BeamSpec, CeilingSpec, ColumnSpec, CurtainSpec, GridLines, Just, LayerSpec,
    MemberShape, Model, MullionSection, OpeningSpec, PVal, ParamSet, ParamSpec, ParamType,
    RoofKind, RoofSpec, RoomSpec, SlabSpec, WallSpec,
};

use super::tests::{col, edit, set, tmp};
use super::*;

type Grid = Vec<Vec<Data>>;
type Locale = BTreeMap<String, String>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    /// Row key (id, layer position, summary key): not a value.
    Key,
    /// Editable and applied.
    Edit,
    /// Editable column of the format, locked for an existing element (edited in the editor).
    Editor,
    /// Read-only column of an editable sheet, recomputed from the model.
    Computed,
    /// Value column of a read-only sheet.
    Sheet,
}
use Class::{Computed, Edit, Editor, Key, Sheet};

/// (sheet, column key, class) of every column the export writes.
const CLASSES: &[(&str, &str, Class)] = &[
    (LEVELS, "id", Key),
    (LEVELS, "name", Edit),
    (LEVELS, "elevation", Edit),
    (TYPES, "id", Key),
    (TYPES, "name", Edit),
    (TYPES, "layers", Computed),
    (TYPES, "total", Computed),
    (LAYERS, "wtype", Key),
    (LAYERS, "pos", Key),
    (LAYERS, "type_name", Computed),
    (LAYERS, "material", Edit),
    (LAYERS, "thickness", Edit),
    (SLAB_LAYERS, "stype", Key),
    (SLAB_LAYERS, "pos", Key),
    (SLAB_LAYERS, "type_name", Computed),
    (SLAB_LAYERS, "material", Edit),
    (SLAB_LAYERS, "thickness", Edit),
    (MATERIALS, "id", Key),
    (MATERIALS, "name", Edit),
    (MATERIALS, "color", Edit),
    (MATERIALS, "hatch", Edit),
    (MATERIALS, "density", Edit),
    (MATERIALS, "conductivity", Edit),
    (MATERIALS, "specific_heat", Edit),
    (MATERIALS, "note", Edit),
    (WALLS, "id", Key),
    (WALLS, "level", Edit),
    (WALLS, "type", Edit),
    (WALLS, "ax", Edit),
    (WALLS, "ay", Edit),
    (WALLS, "bx", Edit),
    (WALLS, "by", Edit),
    (WALLS, "base_offset", Edit),
    (WALLS, "height", Edit),
    (WALLS, "offset0", Edit),
    (WALLS, "bulge", Edit),
    (WALLS, "length", Computed),
    (WALLS, "delete", Edit),
    (OPENINGS, "id", Key),
    (OPENINGS, "wall", Edit),
    (OPENINGS, "along", Edit),
    (OPENINGS, "width", Edit),
    (OPENINGS, "sill", Edit),
    (OPENINGS, "height", Edit),
    (OPENINGS, "kind", Edit),
    (OPENINGS, "swing", Edit),
    (OPENINGS, "hinge", Edit),
    (OPENINGS, "delete", Edit),
    (QUANTITIES, "wall", Key),
    (QUANTITIES, "pos", Key),
    (QUANTITIES, "material", Sheet),
    (QUANTITIES, "area", Sheet),
    (QUANTITIES, "volume", Sheet),
    (SUMMARY, "category", Key),
    (SUMMARY, "type", Key),
    (SUMMARY, "level", Key),
    (SUMMARY, "count", Sheet),
    (SUMMARY, "area", Sheet),
    (SUMMARY, "volume", Sheet),
    (ROOMS, "id", Key),
    (ROOMS, "level", Edit),
    (ROOMS, "name", Edit),
    (ROOMS, "number", Edit),
    (ROOMS, "px", Edit),
    (ROOMS, "py", Edit),
    (ROOMS, "height", Edit),
    (ROOMS, "area", Computed),
    (ROOMS, "perimeter", Computed),
    (ROOMS, "volume", Computed),
    (ROOMS, "delete", Edit),
    (SLABS, "id", Key),
    (SLABS, "level", Edit),
    (SLABS, "type", Edit),
    (SLABS, "offset", Edit),
    (SLABS, "outline", Edit),
    (SLABS, "area", Computed),
    (SLABS, "surface_area", Computed),
    (SLABS, "volume", Computed),
    (SLABS, "delete", Edit),
    (CURTAINS, "id", Key),
    (CURTAINS, "level", Edit),
    (CURTAINS, "name", Edit),
    (CURTAINS, "ax", Editor),
    (CURTAINS, "ay", Editor),
    (CURTAINS, "bx", Editor),
    (CURTAINS, "by", Editor),
    (CURTAINS, "bulge", Editor),
    (CURTAINS, "base_offset", Edit),
    (CURTAINS, "height", Editor),
    (CURTAINS, "ugrid", Editor),
    (CURTAINS, "vgrid", Editor),
    (CURTAINS, "area", Computed),
    (CURTAINS, "glass", Computed),
    (CURTAINS, "panels", Computed),
    (CURTAINS, "mullions", Computed),
    (CURTAINS, "delete", Edit),
    (COLUMNS, "id", Key),
    (COLUMNS, "level", Computed),
    (COLUMNS, "type", Computed),
    (COLUMNS, "x", Computed),
    (COLUMNS, "y", Computed),
    (COLUMNS, "base_offset", Computed),
    (COLUMNS, "top_offset", Computed),
    (COLUMNS, "volume", Computed),
    (BEAMS, "id", Key),
    (BEAMS, "level", Computed),
    (BEAMS, "type", Computed),
    (BEAMS, "ax", Computed),
    (BEAMS, "ay", Computed),
    (BEAMS, "bx", Computed),
    (BEAMS, "by", Computed),
    (BEAMS, "length", Computed),
    (BEAMS, "volume", Computed),
    (ROOFS, "id", Key),
    (ROOFS, "level", Computed),
    (ROOFS, "type", Computed),
    (ROOFS, "kind", Computed),
    (ROOFS, "offset", Computed),
    (ROOFS, "area", Computed),
    (ROOFS, "volume", Computed),
    (CEILINGS, "id", Key),
    (CEILINGS, "level", Computed),
    (CEILINGS, "type", Computed),
    (CEILINGS, "height", Computed),
    (CEILINGS, "area", Computed),
    (CEILINGS, "volume", Computed),
    (SLAB_TYPES, "id", Key),
    (SLAB_TYPES, "name", Edit),
    (SLAB_TYPES, "layers", Computed),
    (SLAB_TYPES, "total", Computed),
    (MEMBER_TYPES, "id", Key),
    (MEMBER_TYPES, "name", Computed),
    (MEMBER_TYPES, "shape", Computed),
    (MEMBER_TYPES, "material", Computed),
];

/// Every sheet the format writes with its columns.
fn catalog() -> Vec<(&'static str, &'static [Col])> {
    let mut v: Vec<(&'static str, &'static [Col])> = vec![
        (LEVELS, LEVELS_COLS),
        (TYPES, TYPES_COLS),
        (LAYERS, LAYERS_COLS),
        (SLAB_LAYERS, SLAB_LAYERS_COLS),
        (MATERIALS, MATERIALS_COLS),
        (WALLS, WALLS_COLS),
        (OPENINGS, OPENINGS_COLS),
        (QUANTITIES, QUANTITIES_COLS),
        (SUMMARY, SUMMARY_COLS),
        (ROOMS, ROOMS_COLS),
        (SLABS, SLABS_COLS),
        (CURTAINS, CURTAINS_COLS),
    ];
    v.extend(PARAM_SHEETS.iter().map(|p| (p.sheet, p.cols)));
    v
}

fn class_of(sheet: &str, key: &str) -> Option<Class> {
    CLASSES.iter().find(|c| c.0 == sheet && c.1 == key).map(|c| c.2)
}

fn fixture() -> Model {
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("L1", 0.0).unwrap();
    let layer = |n: &str, t: f64| LayerSpec { material: n.into(), thickness: t };
    let ext = m.add_wall_type("ext", &[layer("brick", 0.2)]).unwrap();
    let p = [(0., 0.), (5., 0.), (5., 4.), (0., 4.), (0., 0.)];
    for i in 0..4 {
        let w = WallSpec {
            a: p[i],
            b: p[i + 1],
            level: l1,
            base_offset: 0.0,
            height: 3.0,
            wall_type: ext,
            offset0: None,
        };
        m.add_wall(&w).unwrap();
    }
    let door = OpeningSpec { along: 1.0, width: 0.9, sill: 0.0, height: 2.1, kind: "door".into() };
    m.add_opening(1, &door).unwrap();
    m.add_room(&RoomSpec {
        level: l1,
        name: "Hall".into(),
        number: "101".into(),
        point: (2.5, 2.0),
        height: 3.0,
    })
    .unwrap();
    let floor = m.add_slab_type("floor", &[layer("concrete", 0.2)]).unwrap();
    m.add_slab(&SlabSpec {
        level: l1,
        slab_type: floor,
        offset: 0.0,
        outer: vec![(0., 0.), (5., 0.), (5., 4.), (0., 4.)],
        holes: vec![],
    })
    .unwrap();
    let sec = MullionSection::Rect { w: 0.05, d: 0.1 };
    m.add_curtain_wall(&CurtainSpec {
        level: l1,
        name: "CW1".into(),
        a: (0.0, 6.0),
        b: (4.0, 6.0),
        bulge: 0.0,
        base_offset: 0.0,
        height: 3.0,
        u: GridLines::Spacing(1.0),
        v: GridLines::At(vec![1.0, 2.0]),
        interior: sec,
        border: sec,
        lines: Default::default(),
        top: vec![],
    })
    .unwrap();
    let mt = m
        .add_member_type("C400", &MemberShape::Rect { b: 0.4, h: 0.4 }, "concrete")
        .unwrap();
    m.add_column(&ColumnSpec {
        level: l1,
        top_level: None,
        member_type: mt,
        structural: true,
        at: (1.0, 2.0),
        base_offset: 0.0,
        top_offset: 3.0,
        rotation: 0.0,
        top_at: None,
    })
    .unwrap();
    m.add_beam(&BeamSpec {
        level: l1,
        member_type: mt,
        a: (0.0, 0.0, 3.0),
        b: (5.0, 0.0, 3.0),
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
    let ring = vec![(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)];
    m.add_roof(&RoofSpec {
        level: l1,
        slab_type: floor,
        offset: 3.0,
        overhang: 0.3,
        fascia: 0.2,
        kind: RoofKind::Footprint { ring: ring.clone(), slopes: vec![Some(30.0); 4], mansard: None },
    })
    .unwrap();
    m.add_ceiling(&CeilingSpec {
        level: l1,
        slab_type: floor,
        height: 2.7,
        outer: ring,
        holes: vec![],
    })
    .unwrap();
    m
}

fn preview() -> ImportOptions {
    ImportOptions { apply: false, delete_missing: false, new_params: false }
}

fn ops(m: &Model) -> i64 {
    m.sql().query_row("SELECT COUNT(*) FROM op", [], |r| r.get(0)).unwrap()
}

fn export(m: &Model, name: &str) -> PathBuf {
    let p = tmp(&format!("lk_{name}"));
    export_xlsx(m, &p).unwrap();
    p
}

fn read_sheet(p: &Path, sheet: &str) -> Vec<Vec<String>> {
    let mut wb: Xlsx<_> = open_workbook(p).unwrap();
    wb.worksheet_range(sheet)
        .unwrap_or_else(|e| panic!("sheet {sheet}: {e}"))
        .rows()
        .map(|r| r.iter().map(|d| d.to_string()).collect())
        .collect()
}

fn sheet_names(p: &Path) -> Vec<String> {
    let wb: Xlsx<_> = open_workbook(p).unwrap();
    wb.sheet_names()
}

fn locale(src: &str) -> Locale {
    serde_json::from_str(src).expect("locale file is valid JSON")
}

fn locales() -> (Locale, Locale) {
    (
        locale(include_str!("../../../ui/data/i18n/en/xl.json")),
        locale(include_str!("../../../ui/data/i18n/ru/xl.json")),
    )
}

/// The reason has a template among the English entries and a Russian translation.
fn assert_localized(why: &str, ctx: &str) {
    let (en, ru) = locales();
    let key = en
        .iter()
        .filter(|(k, _)| k.starts_with("xl.msg."))
        .find(|(_, t)| match_template(t, why).is_some())
        .map(|(k, _)| k.clone())
        .unwrap_or_else(|| panic!("{ctx}: reason {why:?} has no xl.msg template"));
    let tr = ru.get(&key).unwrap_or_else(|| panic!("{ctx}: {key} is missing in ru"));
    assert!(
        tr.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c)),
        "{ctx}: {key} is not translated: {tr:?}"
    );
}

/// Reason of the refused row for this cell (skipped or rejected), if the report has one.
fn refused(rep: &Report, sheet: &str, header: &str) -> Option<String> {
    rep.ignored
        .iter()
        .find(|i| i.sheet == sheet && i.column == header)
        .map(|i| i.reason.clone())
        .or_else(|| {
            rep.rejected
                .iter()
                .find(|r| r.sheet == sheet && r.column == header)
                .map(|r| r.reason.clone())
        })
}

/// A value that differs from the cell: number + 7.5, text "edited", grids in their syntax.
fn edited(key: &str, cur: Option<&Data>) -> Data {
    match (key, cur) {
        ("ugrid", _) => Data::String("s 0.25".into()),
        ("vgrid", _) => Data::String("x 0.5".into()),
        (_, Some(Data::Float(x))) => Data::Float(x + 7.5),
        (_, Some(Data::Int(x))) => Data::Float(*x as f64 + 7.5),
        (_, Some(Data::String(_))) => Data::String("edited".into()),
        _ => Data::Float(7.5),
    }
}

fn attack(g: &mut Grid, key: &str, header: &str) {
    let c = col(g, header);
    let v = edited(key, g[1].get(c));
    set(g, 1, header, v);
}

fn first_id(m: &Model, kind: &str) -> i64 {
    m.sql()
        .query_row(&format!("SELECT MIN(id) FROM {kind}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn every_catalog_column_has_a_lock_class() {
    let cat = catalog();
    for (sheet, cols) in &cat {
        for c in *cols {
            let class = class_of(sheet, c.key)
                .unwrap_or_else(|| panic!("{sheet}.{} has no lock class: add it to CLASSES", c.key));
            let editable = matches!(class, Edit | Editor);
            assert_eq!(editable, c.edit, "{sheet}.{}: class {class:?} against edit={}", c.key, c.edit);
        }
    }
    for (i, (sheet, key, _)) in CLASSES.iter().enumerate() {
        let known = cat.iter().any(|(s, cols)| s == sheet && cols.iter().any(|c| c.key == *key));
        assert!(known, "CLASSES lists {sheet}.{key}, which is not in the catalog");
        let again = CLASSES.iter().skip(i + 1).any(|c| c.0 == *sheet && c.1 == *key);
        assert!(!again, "CLASSES lists {sheet}.{key} twice");
    }
}

#[test]
fn every_exported_sheet_is_in_the_catalog() {
    let p = export(&fixture(), "sheets");
    let cat = catalog();
    for name in sheet_names(&p) {
        if [INFO, BASELINE, KEYS, LISTS].contains(&name.as_str()) {
            continue;
        }
        assert!(cat.iter().any(|c| c.0 == name), "sheet {name} has no lock classes");
    }
    for (sheet, _) in &cat {
        assert!(sheet_names(&p).iter().any(|n| n == sheet), "catalog sheet {sheet} is not exported");
    }
}

/// One case of the matrix: the cell is edited in the book; the report names it with a
/// localized reason, the model and journal stay as they were, the re-export restores the cell.
fn refuse_case(sheet: &str, c: &Col, class: Class) {
    let ctx = format!("{sheet}.{}", c.key);
    let tag = format!("{}_{}", sheet.replace(' ', "_"), c.key);
    let mut m = fixture();
    let p = export(&m, &format!("a_{tag}"));
    let before = read_sheet(&p, sheet);
    let q = tmp(&format!("lk_b_{tag}"));
    edit(&p, &q, |sh, g| {
        if sh == sheet {
            attack(g, c.key, c.header);
        }
    });
    let (n, head) = (ops(&m), m.change_head());
    let pre = import_xlsx_with(&mut m, &q, preview()).unwrap();
    let rep = import_xlsx(&mut m, &q).unwrap();
    for (what, r) in [("preview", &pre), ("apply", &rep)] {
        let why = refused(r, sheet, c.header).unwrap_or_else(|| {
            panic!("{ctx} {what}: no refused row; ignored {:?}; rejected {:?}", r.ignored, r.rejected)
        });
        assert_localized(&why, &format!("{ctx} {what}"));
        assert!(
            r.changes.iter().all(|x| !(x.sheet == sheet && x.column == c.header)),
            "{ctx} {what}: the cell is listed as a change: {:?}",
            r.changes
        );
        let _ = class;
    }
    assert_eq!((ops(&m), m.change_head()), (n, head), "{ctx}: the journal moved");
    assert_eq!(read_sheet(&export(&m, &format!("c_{tag}")), sheet), before, "{ctx}: the model moved");
    // a re-export over the edited book gives the model value back
    export_xlsx_units(&m, &q, Units::Metric).unwrap();
    assert_eq!(read_sheet(&q, sheet), before, "{ctx}: the re-export kept the book value");
}

#[test]
fn locked_and_read_only_cells_are_refused_not_applied() {
    let mut cases = 0;
    for (sheet, cols) in catalog() {
        for c in cols {
            let class = class_of(sheet, c.key).expect("classified by the guard test");
            if matches!(class, Computed | Sheet | Editor) {
                refuse_case(sheet, c, class);
                cases += 1;
            }
        }
    }
    assert!(cases >= 60, "the matrix shrank to {cases} cases");
}

fn spec(name: &str, per_type: bool, cats: &[&str]) -> ParamSpec {
    ParamSpec {
        name: name.into(),
        ptype: ParamType::Text,
        per_type,
        unit: String::new(),
        group: String::new(),
        guid: None,
        categories: cats.iter().map(|c| c.to_string()).collect(),
    }
}

fn row_of(g: &Grid, id: i64) -> usize {
    g.iter()
        .position(|r| matches!(r.first(), Some(Data::Float(x)) if *x as i64 == id))
        .unwrap_or_else(|| panic!("no row {id}"))
}

/// User lock on a parameter value, on every sheet that carries parameters: instance sheets
/// (own value) and type sheets (the value of the type).
#[test]
fn user_locked_param_value_is_refused_on_every_parameter_sheet() {
    for sc in super::extra::SHEET_CATS.iter() {
        let ctx = format!("{} ({})", sc.sheet, if sc.per_type { "type" } else { "instance" });
        let mut m = fixture();
        let id = first_id(&m, sc.kind);
        let pid = m.add_param(&spec("LockP", sc.per_type, sc.cats)).unwrap();
        m.set_param_values(&[ParamSet {
            param: pid,
            kind: sc.kind.into(),
            id,
            value: Some(PVal::Text("v0".into())),
        }])
        .unwrap();
        m.set_param_lock(&[(sc.kind, id, "LockP")]).unwrap();
        let tag = sc.sheet.replace(' ', "_");
        let p = export(&m, &format!("u_{tag}"));
        let before = read_sheet(&p, sc.sheet);
        let q = tmp(&format!("lk_ub_{tag}"));
        edit(&p, &q, |sh, g| {
            if sh == sc.sheet {
                let r = row_of(g, id);
                set(g, r, "LockP", Data::String("v1".into()));
            }
        });
        let (n, head) = (ops(&m), m.change_head());
        let pre = import_xlsx_with(&mut m, &q, preview()).unwrap();
        let rep = import_xlsx(&mut m, &q).unwrap();
        for (what, r) in [("preview", &pre), ("apply", &rep)] {
            let hit = r.ignored.iter().find(|i| i.sheet == sc.sheet && i.column == "LockP");
            let hit = hit.unwrap_or_else(|| panic!("{ctx} {what}: no refused row: {:?}", r.ignored));
            assert_eq!(hit.id, Some(id), "{ctx} {what}");
            assert_eq!(hit.reason, "value of 'LockP' is locked: skipped", "{ctx} {what}");
            assert_localized(&hit.reason, &ctx);
            assert!(
                r.changes.iter().all(|c| !(c.id == Some(id) && c.column == "LockP")),
                "{ctx} {what}: {:?}",
                r.changes
            );
        }
        assert_eq!(m.param_value(pid, sc.kind, id).unwrap(), Some(PVal::Text("v0".into())), "{ctx}");
        assert_eq!((ops(&m), m.change_head()), (n, head), "{ctx}: the journal moved");
        export_xlsx_units(&m, &q, Units::Metric).unwrap();
        assert_eq!(read_sheet(&q, sc.sheet), before, "{ctx}: the re-export kept the book value");
    }
}

/// Built-in lock of an element: a pinned wall takes no edit from the book.
#[test]
fn pinned_wall_edit_is_refused_not_applied() {
    let mut m = fixture();
    m.pin(&[("wall", 1)]).unwrap();
    let p = export(&m, "pin_a");
    let before = read_sheet(&p, WALLS);
    let q = tmp("lk_pin_b");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            set(g, 1, "height [m]", Data::Float(3.6));
        }
    });
    let (n, head) = (ops(&m), m.change_head());
    let pre = import_xlsx_with(&mut m, &q, preview()).unwrap();
    let rep = import_xlsx(&mut m, &q).unwrap();
    for (what, r) in [("preview", &pre), ("apply", &rep)] {
        let hit = r.ignored.iter().find(|i| i.id == Some(1) && i.reason.contains("is pinned"));
        let hit = hit.unwrap_or_else(|| panic!("{what}: no refused row: {:?}", r.ignored));
        assert_eq!(hit.reason, "wall 1 is pinned: skipped");
        assert_localized(&hit.reason, what);
        assert!(r.changes.iter().all(|c| c.id != Some(1)), "{what}: {:?}", r.changes);
    }
    assert_eq!((ops(&m), m.change_head()), (n, head));
    let h: f64 = m.sql().query_row("SELECT height FROM wall WHERE id=1", [], |r| r.get(0)).unwrap();
    assert!((h - 3.0).abs() < 1e-12);
    export_xlsx_units(&m, &q, Units::Metric).unwrap();
    assert_eq!(read_sheet(&q, WALLS), before);
}

fn zip_text(p: &Path, name: &str) -> String {
    use std::io::Read;
    let mut z = zip::ZipArchive::new(std::fs::File::open(p).unwrap()).unwrap();
    let mut t = String::new();
    z.by_name(name).unwrap().read_to_string(&mut t).unwrap();
    t
}

fn col_letters(mut n: usize) -> String {
    let mut s = String::new();
    loop {
        s.insert(0, (b'A' + (n % 26) as u8) as char);
        if n < 26 {
            return s;
        }
        n = n / 26 - 1;
    }
}

/// Whether the cell's style is unlocked (editable); None if the sheet has no such cell.
fn unlocked(sheet_xml: &str, styles: &str, cell: &str) -> Option<bool> {
    let at = sheet_xml.find(&format!("<c r=\"{cell}\""))?;
    let end = at + sheet_xml[at..].find('>')?;
    let head = &sheet_xml[at..end];
    let s: usize = head
        .split(" s=\"")
        .nth(1)
        .and_then(|r| r.split('"').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    let xfs = styles.split("<cellXfs").nth(1)?.split("</cellXfs>").next()?;
    let xf = xfs.split("<xf ").nth(s + 1)?;
    Some(xf.contains("locked=\"0\""))
}

/// (protected, per column (sheet, key) the unlocked state of its first data cell)
fn lock_style(p: &Path) -> (Vec<String>, Vec<(String, String, Option<bool>)>) {
    let wb = zip_text(p, "xl/workbook.xml");
    let names: Vec<String> = wb
        .split("<sheet name=\"")
        .skip(1)
        .map(|s| s.split('"').next().unwrap().to_string())
        .collect();
    let styles = zip_text(p, "xl/styles.xml");
    let (mut protected, mut cells) = (vec![], vec![]);
    for (sheet, cols) in catalog() {
        let i = names.iter().position(|n| n == sheet).unwrap_or_else(|| panic!("sheet {sheet}"));
        let xml = zip_text(p, &format!("xl/worksheets/sheet{}.xml", i + 1));
        if xml.contains("<sheetProtection") {
            protected.push(sheet.to_string());
        }
        for (j, c) in cols.iter().enumerate() {
            let cell = format!("{}2", col_letters(j));
            cells.push((sheet.to_string(), c.key.to_string(), unlocked(&xml, &styles, &cell)));
        }
    }
    (protected, cells)
}

/// Every locked and read-only cell is edited at once; the import applies nothing and the
/// re-export over the edited book restores values, lock styles and sheet protection.
#[test]
fn reexport_keeps_lock_style_protection_and_values() {
    let mut m = fixture();
    let p = export(&m, "style_a");
    let (prot0, cells0) = lock_style(&p);
    let all: Vec<&str> = catalog().iter().map(|c| c.0).collect();
    assert_eq!(prot0.len(), all.len(), "every catalog sheet is protected: {prot0:?}");
    for (sheet, key, state) in &cells0 {
        let class = class_of(sheet, key).unwrap();
        match (class, state) {
            (Edit, Some(false)) => panic!("{sheet}.{key}: an editable cell is locked"),
            (Key | Computed | Sheet, Some(true)) => panic!("{sheet}.{key}: a locked cell is open"),
            _ => {}
        }
    }
    let before: Vec<(&str, Vec<Vec<String>>)> =
        all.iter().map(|s| (*s, read_sheet(&p, s))).collect();
    let q = tmp("lk_style_b");
    edit(&p, &q, |sh, g| {
        for (sheet, cols) in catalog() {
            if sheet != sh {
                continue;
            }
            for c in cols {
                if matches!(class_of(sheet, c.key), Some(Computed | Sheet | Editor)) {
                    attack(g, c.key, c.header);
                }
            }
        }
    });
    let (n, head) = (ops(&m), m.change_head());
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.changes.is_empty(), "{:?}", rep.changes);
    assert_eq!((ops(&m), m.change_head()), (n, head), "the journal moved");
    export_xlsx_units(&m, &q, Units::Metric).unwrap();
    let (prot1, cells1) = lock_style(&q);
    assert_eq!(prot1, prot0, "sheet protection changed");
    assert_eq!(cells1, cells0, "lock styles changed");
    for (sheet, grid) in &before {
        assert_eq!(&read_sheet(&q, sheet), grid, "{sheet}: values changed");
    }
}
