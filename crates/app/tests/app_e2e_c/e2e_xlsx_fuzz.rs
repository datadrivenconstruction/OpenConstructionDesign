// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Whole-book XLSX round trip with seeded random edits: export the full model (levels, wall
//! types and layers, slab types and layers, 200 walls with openings, rooms, slabs, curtain walls,
//! column and beam family sheets), edit cells of every editable sheet in the exported book, read
//! the book back through the preview, apply it as one undo step and re-export.
//!
//! A failing seed is printed at the head of every panic message. `DDC_E2E_SEQS` sets the number
//! of seeds of the ignored sweep (default 50). Failures are product findings, not test fixes.

use crate::e2e_common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use ddc_app::editor::Editor;
use ddc_app::elem::sheets::families;
use ddc_io::xlsx::{
    self, FamDim, FamilySheet, ImportOptions, Report, TableCell as C, Units,
};
use ddc_model::{
    BeamJoin, BeamSpec, ColumnSpec, CurtainSpec, GridLines, Just, LayerSpec, MullionSection,
    OpeningSpec, RoomSpec, SlabSpec, WallSpec,
};
use e2e_common::{self as ec, assert_clean, assert_same, data_hash, dump_data, ids, Rng};

const SEEDS: [u64; 3] = [1, 2, 3];

type Addr = (String, u32, u32);
type Key = (String, usize, usize);

/// Sheets edited by the planner; each must get at least one valid edit per seed.
const EDITED: [&str; 12] = [
    "Levels",
    "WallTypes",
    "Layers",
    "SlabTypes",
    "SlabLayers",
    "Walls",
    "Openings",
    "Rooms",
    "Slabs",
    "Curtain walls",
    "Columns",
    "Beams",
];

/// Sheets with their own column set; every column but the read-only ones is compared.
const BESPOKE: [&str; 10] = [
    "Levels",
    "WallTypes",
    "Layers",
    "SlabTypes",
    "SlabLayers",
    "Walls",
    "Openings",
    "Rooms",
    "Slabs",
    "Curtain walls",
];

const MATERIALS: [&str; 6] = ["concrete", "brick", "timber", "steel", "gypsum", "glass"];

// ---------------------------------------------------------------- fixture

fn layer(material: &str, thickness: f64) -> LayerSpec {
    LayerSpec {
        material: material.into(),
        thickness,
    }
}

/// 3 levels, 200 walls (188 loose, 3 boxes of 4), openings on about half of the loose walls,
/// one room per box, 2 slab types and 6 slabs, 2 curtain walls, 4 columns and 4 beams of 2
/// member types.
fn fixture(seed: u64) -> Editor {
    let mut rng = Rng::new(seed.wrapping_mul(31).wrapping_add(7));
    let mut ed = Editor::new_in_memory().unwrap();
    let l1 = ed.level;
    let wtypes = ids(&ed.model, "wall_type");
    {
        let m = &mut ed.model;
        let l2 = m.add_level("L2", 3.0).unwrap();
        let l3 = m.add_level("L3", 6.0).unwrap();
        let levels = [l1, l2, l3];
        let floor = m
            .add_slab_type("Floor A", &[layer("concrete", 0.2), layer("screed", 0.05)])
            .unwrap();
        let deck = m.add_slab_type("Deck B", &[layer("timber", 0.1)]).unwrap();

        for n in 0..188usize {
            let (x, y) = ((n % 20) as f64 * 10.0, (n / 20) as f64 * 10.0);
            let height = 3.0 + 0.1 * rng.below(3) as f64;
            let wall_type = wtypes[rng.below(wtypes.len())];
            let wall = m
                .add_wall(&WallSpec {
                    a: (x, y),
                    b: (x + 6.0, y),
                    level: levels[n % 3],
                    base_offset: 0.0,
                    height,
                    wall_type,
                    offset0: None,
                })
                .unwrap();
            let hole = match n % 6 {
                0 => Some((1.0, 0.9, 0.0, 2.1, "door")),
                1 => Some((3.0, 1.2, 0.9, 1.2, "window")),
                3 => Some((2.0, 1.0, 0.0, 2.0, "void")),
                _ => None,
            };
            if let Some((along, width, sill, height, kind)) = hole {
                m.add_opening(
                    wall,
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
        }

        for k in 0..3usize {
            let (x0, y0) = (k as f64 * 20.0, 300.0);
            let corners = [
                ((x0, y0), (x0 + 6.0, y0)),
                ((x0 + 6.0, y0), (x0 + 6.0, y0 + 4.0)),
                ((x0 + 6.0, y0 + 4.0), (x0, y0 + 4.0)),
                ((x0, y0 + 4.0), (x0, y0)),
            ];
            let mut ws = vec![];
            for (a, b) in corners {
                let id = m
                    .add_wall(&WallSpec {
                        a,
                        b,
                        level: levels[k],
                        base_offset: 0.0,
                        height: 3.0,
                        wall_type: wtypes[1],
                        offset0: None,
                    })
                    .unwrap();
                ws.push(id);
            }
            let holes = [
                (ws[1], 1.0, 0.9, 0.0, 2.1, "door"),
                (ws[0], 2.0, 1.2, 0.9, 1.2, "window"),
                (ws[3], 1.0, 1.0, 0.0, 2.0, "void"),
            ];
            for (wall, along, width, sill, height, kind) in holes {
                m.add_opening(
                    wall,
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
            m.add_room(&RoomSpec {
                level: levels[k],
                name: format!("Room {}", k + 1),
                number: format!("{}", 101 + k),
                point: (x0 + 3.0, y0 + 2.0),
                height: 2.7,
            })
            .unwrap();
        }

        for i in 0..6usize {
            let x = i as f64 * 10.0;
            m.add_slab(&SlabSpec {
                level: levels[i % 3],
                slab_type: if i % 2 == 0 { floor } else { deck },
                offset: 0.0,
                outer: vec![(x, 400.0), (x + 4.0, 400.0), (x + 4.0, 404.0), (x, 404.0)],
                holes: vec![],
            })
            .unwrap();
        }

        let sec = MullionSection::Rect { w: 0.05, d: 0.1 };
        for (i, level) in [l1, l2].into_iter().enumerate() {
            let x = i as f64 * 20.0;
            m.add_curtain_wall(&CurtainSpec {
                level,
                name: format!("CW {}", i + 1),
                a: (x, 500.0),
                b: (x + 6.0, 500.0),
                bulge: 0.0,
                base_offset: 0.0,
                height: 3.0,
                u: GridLines::Spacing(1.5),
                v: GridLines::Spacing(1.5),
                interior: sec,
                border: sec,
                lines: Default::default(),
                top: vec![],
            })
            .unwrap();
        }

        let rect = m.add_catalog_member_type("R 400x400").unwrap();
        let ipe = m.add_catalog_member_type("IPE 200").unwrap();
        for i in 0..4usize {
            let x = i as f64 * 8.0;
            m.add_column(&ColumnSpec {
                level: l1,
                top_level: None,
                member_type: if i % 2 == 0 { rect } else { ipe },
                structural: true,
                at: (x, 600.0),
                base_offset: 0.0,
                top_offset: 3.0,
                rotation: 0.0,
                top_at: None,
            })
            .unwrap();
            m.add_beam(&BeamSpec {
                level: l1,
                member_type: if i % 2 == 0 { ipe } else { rect },
                a: (x, 700.0, 2.75),
                b: (x + 6.0, 700.0, 2.75),
                bulge: 0.0,
                yjust: Just::Center,
                zjust: Just::Center,
                rotation: 0.0,
                start_ext: 0.0,
                end_ext: 0.0,
                join_start: BeamJoin::Auto,
                join_end: BeamJoin::Auto,
            })
            .unwrap();
        }
    }
    ed.reload().unwrap();
    assert_clean(&ed.model, "fixture");
    ed
}

// ---------------------------------------------------------------- cells

fn text_of(c: &C) -> String {
    match c {
        C::Num(x) => format!("{x}"),
        C::Text(t) => t.clone(),
        other => format!("{other:?}"),
    }
}

fn num_of(c: &C) -> Option<f64> {
    match c {
        C::Num(x) => Some(*x),
        C::Text(t) => t.trim().replace(',', ".").parse().ok(),
        _ => None,
    }
}

/// One sheet as a grid; `origin` is the (row, column) of the first stored cell.
#[derive(Clone, Debug, PartialEq)]
struct SheetGrid {
    name: String,
    origin: (u32, u32),
    rows: Vec<Vec<C>>,
}

/// Every sheet of the book in order; an empty cell is `Text("")`.
fn read_grids(path: &Path) -> Result<Vec<SheetGrid>, String> {
    use calamine::{open_workbook, Data, Reader, Xlsx};
    let mut wb: Xlsx<std::io::BufReader<std::fs::File>> =
        open_workbook(path).map_err(|e: calamine::XlsxError| e.to_string())?;
    let mut out = vec![];
    for name in wb.sheet_names() {
        let range = wb.worksheet_range(&name).map_err(|e| e.to_string())?;
        let origin = range.start().unwrap_or((0, 0));
        let rows = range
            .rows()
            .map(|r| {
                r.iter()
                    .map(|d| match d {
                        Data::Float(x) => C::Num(*x),
                        Data::Int(x) => C::Num(*x as f64),
                        Data::Bool(b) => C::Num(f64::from(u8::from(*b))),
                        Data::Empty => C::Text(String::new()),
                        Data::String(s) => C::Text(s.clone()),
                        other => C::Text(other.to_string()),
                    })
                    .collect()
            })
            .collect();
        out.push(SheetGrid { name, origin, rows });
    }
    Ok(out)
}

/// Writes the grids as a new book with the same sheet names and order.
fn write_grids(dst: &Path, sheets: &[SheetGrid]) -> Result<(), String> {
    let mut out = rust_xlsxwriter::Workbook::new();
    for s in sheets {
        let ws = out.add_worksheet();
        ws.set_name(&s.name).map_err(|e| e.to_string())?;
        for (i, row) in s.rows.iter().enumerate() {
            for (j, cell) in row.iter().enumerate() {
                let (i, j) = (s.origin.0 + i as u32, (s.origin.1 + j as u32) as u16);
                match cell {
                    C::Num(x) => ws.write_number(i, j, *x).map(|_| ()),
                    C::Text(t) if t.is_empty() => Ok(()),
                    C::Text(t) => ws.write_string(i, j, t).map(|_| ()),
                    _ => Ok(()),
                }
                .map_err(|e| e.to_string())?;
            }
        }
    }
    out.save(dst).map_err(|e| e.to_string())
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * (1.0 + a.abs().max(b.abs()))
}

fn pts_of(s: &str) -> Option<Vec<(f64, f64)>> {
    let mut v = vec![];
    for p in s.split(';').map(str::trim).filter(|p| !p.is_empty()) {
        let xy: Vec<&str> = p.split_whitespace().collect();
        match xy.as_slice() {
            [x, y] => v.push((x.parse().ok()?, y.parse().ok()?)),
            _ => return None,
        }
    }
    (!v.is_empty()).then_some(v)
}

/// Same point set (a re-exported outline may start elsewhere or run the other way).
fn same_ring(a: &[(f64, f64)], b: &[(f64, f64)]) -> bool {
    a.len() == b.len()
        && a.iter()
            .all(|p| b.iter().any(|q| near(p.0, q.0) && near(p.1, q.1)))
}

fn cell_eq(a: &C, b: &C) -> bool {
    if let (C::Num(x), C::Num(y)) = (a, b) {
        return near(*x, *y);
    }
    let (x, y) = (text_of(a), text_of(b));
    if x.trim() == y.trim() {
        return true;
    }
    if let (Some(p), Some(q)) = (num_of(a), num_of(b)) {
        return near(p, q);
    }
    match (pts_of(&x), pts_of(&y)) {
        (Some(p), Some(q)) => same_ring(&p, &q),
        _ => false,
    }
}

fn included(sheet: &str, h: &str, fams: &[FamilySheet]) -> bool {
    let h = h.trim();
    if BESPOKE.contains(&sheet) {
        return !h.contains("(read-only)") && h != "delete";
    }
    match fams.iter().find(|f| f.sheet == sheet) {
        Some(f) => h == "id" || f.cols.iter().any(|c| c.header(Units::Metric) == h),
        None => false,
    }
}

fn row_key(g: &SheetGrid, r: usize) -> String {
    let n = |c: usize| {
        g.rows[r]
            .get(c)
            .and_then(num_of)
            .map_or(String::from("?"), |v| format!("{v}"))
    };
    if g.name == "Layers" || g.name == "SlabLayers" {
        format!("{}/{}", n(0), n(1))
    } else {
        n(0)
    }
}

// ---------------------------------------------------------------- plan

/// The edited book: `valid` cells carry the cell as written and the value the model must show
/// after the import; `bad_cell` cells are refused alone, `bad_row` cells refuse their row.
struct Plan {
    orig: Vec<SheetGrid>,
    book: Vec<SheetGrid>,
    valid: BTreeMap<Key, (C, C)>,
    bad_cell: BTreeSet<Key>,
    bad_row: BTreeSet<Key>,
    reserved: BTreeSet<(String, usize)>,
    lv_ren: BTreeMap<String, String>,
    wt_ren: BTreeMap<String, String>,
    st_ren: BTreeMap<String, String>,
    seq: u64,
    rng: Rng,
}

impl Plan {
    fn new(seed: u64, orig: Vec<SheetGrid>) -> Plan {
        Plan {
            book: orig.clone(),
            orig,
            valid: BTreeMap::new(),
            bad_cell: BTreeSet::new(),
            bad_row: BTreeSet::new(),
            reserved: BTreeSet::new(),
            lv_ren: BTreeMap::new(),
            wt_ren: BTreeMap::new(),
            st_ren: BTreeMap::new(),
            seq: 0,
            rng: Rng::new(seed.wrapping_mul(977).wrapping_add(13)),
        }
    }

    fn si(&self, sheet: &str) -> usize {
        self.orig
            .iter()
            .position(|s| s.name == sheet)
            .unwrap_or_else(|| panic!("the exported book has no sheet '{sheet}'"))
    }

    fn n(&self, sheet: &str) -> usize {
        self.orig[self.si(sheet)].rows.len()
    }

    fn col(&self, sheet: &str, header: &str) -> usize {
        let s = &self.orig[self.si(sheet)];
        s.rows[0]
            .iter()
            .position(|c| text_of(c).trim() == header)
            .unwrap_or_else(|| panic!("no column '{header}' in {sheet}: {:?}", s.rows[0]))
    }

    fn id_of(&self, sheet: &str, r: usize) -> i64 {
        num_of(&self.orig[self.si(sheet)].rows[r][0]).unwrap_or(-1.0) as i64
    }

    /// The cell as the model shows it before the import (names follow the renames).
    fn cur_cell(&self, sheet: &str, r: usize, c: usize) -> C {
        let s = &self.orig[self.si(sheet)];
        let o = s.rows[r][c].clone();
        let h = text_of(&s.rows[0][c]);
        let map = match (sheet, h.trim()) {
            ("Walls" | "Rooms" | "Slabs" | "Curtain walls", "level") => &self.lv_ren,
            ("Walls", "type") => &self.wt_ren,
            ("Slabs", "type") => &self.st_ren,
            _ => return o,
        };
        match map.get(text_of(&o).trim()) {
            Some(n) => C::Text(n.clone()),
            None => o,
        }
    }

    /// The value the cell must show after the import.
    fn want_at(&self, sheet: &str, r: usize, c: usize) -> C {
        match self.valid.get(&(sheet.to_string(), r, c)) {
            Some(v) => v.1.clone(),
            None => self.cur_cell(sheet, r, c),
        }
    }

    fn want(&self, sheet: &str, r: usize, header: &str) -> C {
        self.want_at(sheet, r, self.col(sheet, header))
    }

    fn numv(&self, sheet: &str, r: usize, header: &str) -> f64 {
        num_of(&self.want(sheet, r, header)).unwrap_or(0.0)
    }

    fn texts(&self, sheet: &str, header: &str) -> Vec<String> {
        let c = self.col(sheet, header);
        (1..self.n(sheet))
            .map(|r| text_of(&self.want_at(sheet, r, c)).trim().to_string())
            .collect()
    }

    fn count(&self, sheet: &str) -> usize {
        self.valid.keys().filter(|k| k.0 == sheet).count()
    }

    fn chance(&mut self, den: usize) -> bool {
        self.rng.below(den.max(1)) == 0
    }

    /// 0.1 .. 0.1 * k
    fn step(&mut self, k: usize) -> f64 {
        0.1 * (1 + self.rng.below(k)) as f64
    }

    fn next(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    fn used_cell(&self, key: &Key) -> bool {
        self.valid.contains_key(key) || self.bad_cell.contains(key) || self.bad_row.contains(key)
    }

    fn row_used(&self, sheet: &str, r: usize) -> bool {
        self.valid.keys().any(|k| k.0 == sheet && k.1 == r)
            || self.bad_cell.iter().any(|k| k.0 == sheet && k.1 == r)
            || self.bad_row.iter().any(|k| k.0 == sheet && k.1 == r)
    }

    fn put_book(&mut self, sheet: &str, r: usize, c: usize, v: C) {
        let s = self.si(sheet);
        self.book[s].rows[r][c] = v;
    }

    /// A valid edit; false when the cell is taken or the value is the one the model has.
    fn try_put(&mut self, sheet: &str, r: usize, c: usize, put: C, want: C) -> bool {
        let key = (sheet.to_string(), r, c);
        if self.used_cell(&key) || self.reserved.contains(&(sheet.to_string(), r)) {
            return false;
        }
        if cell_eq(&self.cur_cell(sheet, r, c), &want) {
            return false;
        }
        self.put_book(sheet, r, c, put.clone());
        self.valid.insert(key, (put, want));
        true
    }

    fn put_num(&mut self, sheet: &str, r: usize, c: usize, v: f64) -> bool {
        self.try_put(sheet, r, c, C::Num(v), C::Num(v))
    }

    fn put_text(&mut self, sheet: &str, r: usize, c: usize, t: &str) -> bool {
        self.try_put(sheet, r, c, C::Text(t.into()), C::Text(t.into()))
    }

    fn pick(&mut self, v: &[String]) -> String {
        v[self.rng.below(v.len())].clone()
    }

    fn rename(&mut self, sheet: &str, den: usize, prefix: &str) -> BTreeMap<String, String> {
        let c = self.col(sheet, "name");
        let mut out = BTreeMap::new();
        for r in 1..self.n(sheet) {
            if !self.chance(den) {
                continue;
            }
            let old = text_of(&self.cur_cell(sheet, r, c)).trim().to_string();
            let new = format!("{prefix}{}", self.id_of(sheet, r));
            if self.put_text(sheet, r, c, &new) {
                out.insert(old, new);
            }
        }
        out
    }

    fn edit_levels(&mut self, den: usize) {
        let m = self.rename("Levels", den, "Lvl");
        self.lv_ren.extend(m);
        let c = self.col("Levels", "elevation [m]");
        for r in 1..self.n("Levels") {
            if self.chance(den) {
                let v = self.numv("Levels", r, "elevation [m]") + self.step(3);
                self.put_num("Levels", r, c, v);
            }
        }
    }

    fn edit_layers(&mut self, sheet: &str, den: usize) {
        let (cm, ct) = (self.col(sheet, "material"), self.col(sheet, "thickness [m]"));
        for r in 1..self.n(sheet) {
            if self.chance(den) {
                let m = MATERIALS[self.rng.below(MATERIALS.len())];
                self.put_text(sheet, r, cm, m);
            }
            if self.chance(den) {
                let v = self.numv(sheet, r, "thickness [m]") + 0.01 * (1 + self.rng.below(5)) as f64;
                self.put_num(sheet, r, ct, v);
            }
        }
    }

    fn edit_walls(&mut self, den: usize) {
        let s = "Walls";
        let (cl, ct, cbo, ch) = (
            self.col(s, "level"),
            self.col(s, "type"),
            self.col(s, "base_offset [m]"),
            self.col(s, "height [m]"),
        );
        let ends = [
            self.col(s, "ax [m]"),
            self.col(s, "ay [m]"),
            self.col(s, "bx [m]"),
            self.col(s, "by [m]"),
        ];
        let with_op: BTreeSet<i64> = (1..self.n("Openings"))
            .map(|r| self.numv("Openings", r, "wall_id") as i64)
            .collect();
        let types = self.texts("WallTypes", "name");
        let lvls = self.texts("Levels", "name");
        for r in 1..self.n(s) {
            let id = self.id_of(s, r);
            let boxed = self.numv(s, r, "ay [m]") >= 299.0;
            if self.chance(den) {
                let h = 2.6 + 0.1 * self.rng.below(13) as f64;
                if self.rng.below(2) == 0 {
                    self.put_num(s, r, ch, h);
                } else {
                    let put = C::Text(format!("{} mm", (h * 1000.0).round()));
                    self.try_put(s, r, ch, put, C::Num(h));
                }
            }
            if self.chance(den) {
                let t = self.pick(&types);
                self.put_text(s, r, ct, &t);
            }
            if self.chance(den) {
                let v = 0.05 * (1 + self.rng.below(4)) as f64;
                self.put_num(s, r, cbo, v);
            }
            if !boxed && !with_op.contains(&id) && self.chance(den) {
                for c in ends {
                    if self.chance(2) {
                        let v = num_of(&self.want_at(s, r, c)).unwrap_or(0.0) + self.step(5);
                        self.put_num(s, r, c, v);
                    }
                }
            }
            if !boxed && self.chance(den * 2) {
                let l = self.pick(&lvls);
                self.put_text(s, r, cl, &l);
            }
        }
    }

    fn edit_openings(&mut self, den: usize) {
        let s = "Openings";
        let mut len: BTreeMap<i64, f64> = BTreeMap::new();
        for r in 1..self.n("Walls") {
            let g = |h: &str| self.numv("Walls", r, h);
            let l = (g("bx [m]") - g("ax [m]")).hypot(g("by [m]") - g("ay [m]"));
            len.insert(self.id_of("Walls", r), l);
        }
        let (ca, cw, cs, ch) = (
            self.col(s, "along [m]"),
            self.col(s, "width [m]"),
            self.col(s, "sill [m]"),
            self.col(s, "height [m]"),
        );
        for r in 1..self.n(s) {
            let wall = self.numv(s, r, "wall_id") as i64;
            let l = len.get(&wall).copied().unwrap_or(4.0);
            let kind = text_of(&self.want(s, r, "kind")).trim().to_ascii_lowercase();
            if self.chance(den) {
                let w = 0.5 + 0.1 * self.rng.below(8) as f64;
                if self.numv(s, r, "along [m]") + w <= l - 0.1 {
                    self.put_num(s, r, cw, w);
                }
            }
            if self.chance(den) {
                let a = 0.2 + 0.1 * self.rng.below(10) as f64;
                if a + self.numv(s, r, "width [m]") <= l - 0.1 {
                    self.put_num(s, r, ca, a);
                }
            }
            if kind == "window" && self.chance(den) {
                let room = ((2.2 - self.numv(s, r, "height [m]")) / 0.1).floor();
                if room >= 0.0 {
                    let v = 0.1 * self.rng.below(room as usize + 1) as f64;
                    self.put_num(s, r, cs, v);
                }
            }
            if self.chance(den) {
                let h = 1.0 + 0.1 * self.rng.below(13) as f64;
                if self.numv(s, r, "sill [m]") + h <= 2.2 + 1e-9 {
                    self.put_num(s, r, ch, h);
                }
            }
            for (h, a, b) in [
                ("swing side (left/right)", "left", "right"),
                ("hinge (start/end)", "start", "end"),
            ] {
                if !self.chance(den) {
                    continue;
                }
                let cur = text_of(&self.want(s, r, h)).trim().to_ascii_lowercase();
                let new = if cur == a {
                    b
                } else if cur == b {
                    a
                } else {
                    continue;
                };
                let c = self.col(s, h);
                self.put_text(s, r, c, new);
            }
        }
    }

    fn edit_rooms(&mut self, den: usize) {
        let s = "Rooms";
        let (cn, cu, ch, cx, cy) = (
            self.col(s, "name"),
            self.col(s, "number"),
            self.col(s, "height [m]"),
            self.col(s, "point x [m]"),
            self.col(s, "point y [m]"),
        );
        for r in 1..self.n(s) {
            if self.chance(den) {
                let t = format!("Edited {}", self.next());
                self.put_text(s, r, cn, &t);
            }
            if self.chance(den) {
                let k = self.next();
                if k % 2 == 0 {
                    self.try_put(s, r, cu, C::Num((200 + k) as f64), C::Text(format!("{}", 200 + k)));
                } else {
                    self.put_text(s, r, cu, &format!("R{k}"));
                }
            }
            if self.chance(den) {
                let v = 2.4 + 0.1 * self.rng.below(10) as f64;
                self.put_num(s, r, ch, v);
            }
            if self.chance(den) {
                let v = self.numv(s, r, "point x [m]") + self.step(3);
                self.put_num(s, r, cx, v);
            }
            if self.chance(den) {
                let v = self.numv(s, r, "point y [m]") + self.step(3);
                self.put_num(s, r, cy, v);
            }
        }
    }

    fn edit_slabs(&mut self, den: usize) {
        let s = "Slabs";
        let outline = "outline [m] (x y; x y; ...)";
        let (cl, ct, co, cp) = (
            self.col(s, "level"),
            self.col(s, "type"),
            self.col(s, "offset [m]"),
            self.col(s, outline),
        );
        let types = self.texts("SlabTypes", "name");
        let lvls = self.texts("Levels", "name");
        for r in 1..self.n(s) {
            if self.chance(den) {
                let l = self.pick(&lvls);
                self.put_text(s, r, cl, &l);
            }
            if self.chance(den) {
                let t = self.pick(&types);
                self.put_text(s, r, ct, &t);
            }
            if self.chance(den) {
                let v = 0.05 * (1 + self.rng.below(4)) as f64;
                self.put_num(s, r, co, v);
            }
            if self.chance(den) {
                let cur = text_of(&self.want(s, r, outline));
                let (dx, dy) = (self.step(5), self.step(5));
                if let Some(p) = pts_of(&cur) {
                    let t: Vec<String> = p.iter().map(|q| format!("{} {}", q.0 + dx, q.1 + dy)).collect();
                    self.put_text(s, r, cp, &t.join("; "));
                }
            }
        }
    }

    fn edit_curtains(&mut self, den: usize) {
        let s = "Curtain walls";
        let (cl, cn, cb) = (
            self.col(s, "level"),
            self.col(s, "name"),
            self.col(s, "base_offset [m]"),
        );
        let lvls = self.texts("Levels", "name");
        for r in 1..self.n(s) {
            if self.chance(den) {
                let t = format!("CW edited {}", self.next());
                self.put_text(s, r, cn, &t);
            }
            if self.chance(den) {
                let l = self.pick(&lvls);
                self.put_text(s, r, cl, &l);
            }
            if self.chance(den) {
                let v = 0.05 * (1 + self.rng.below(4)) as f64;
                self.put_num(s, r, cb, v);
            }
        }
    }

    fn edit_family(&mut self, fam: &FamilySheet, mts: &[i64], den: usize) {
        let s = fam.sheet;
        for r in 1..self.n(s) {
            for fc in &fam.cols {
                let c = self.col(s, &fc.header(Units::Metric));
                if !self.chance(den) {
                    continue;
                }
                let Some(cur) = num_of(&self.want_at(s, r, c)) else {
                    continue;
                };
                let new = match fc.dim {
                    FamDim::Len => {
                        let v = cur + self.step(3);
                        if v > fc.hi {
                            continue;
                        }
                        v
                    }
                    FamDim::Deg => {
                        let v = cur + 5.0 * (1 + self.rng.below(3)) as f64;
                        if v > fc.hi {
                            continue;
                        }
                        v
                    }
                    FamDim::Pct => {
                        let v = cur + 1.0;
                        if v > fc.hi {
                            continue;
                        }
                        v
                    }
                    FamDim::Type(_) => {
                        let others: Vec<i64> = mts.iter().copied().filter(|&m| m as f64 != cur).collect();
                        match self.rng.pick(&others) {
                            Some(m) => m as f64,
                            None => continue,
                        }
                    }
                    FamDim::Choice(n) => ((cur as usize + 1) % n.max(1)) as f64,
                };
                self.put_num(s, r, c, new);
            }
        }
    }

    /// Random valid edits on every editable sheet; a sheet that got none is edited in full.
    fn valid_edits(&mut self, fams: &[FamilySheet], mts: &[i64]) {
        for den in [2, 1] {
            if self.count("Levels") == 0 {
                self.edit_levels(den);
            }
        }
        for den in [2, 1] {
            if self.count("WallTypes") == 0 {
                self.wt_ren = self.rename("WallTypes", den, "WT");
            }
            if self.count("SlabTypes") == 0 {
                self.st_ren = self.rename("SlabTypes", den, "ST");
            }
        }
        for sheet in ["Layers", "SlabLayers"] {
            for den in [2, 1] {
                if self.count(sheet) == 0 {
                    self.edit_layers(sheet, den);
                }
            }
        }
        for den in [3, 1] {
            if self.count("Walls") == 0 {
                self.edit_walls(den);
            }
            if self.count("Openings") == 0 {
                self.edit_openings(den);
            }
        }
        for den in [2, 1] {
            if self.count("Rooms") == 0 {
                self.edit_rooms(den);
            }
            if self.count("Slabs") == 0 {
                self.edit_slabs(den);
            }
            if self.count("Curtain walls") == 0 {
                self.edit_curtains(den);
            }
        }
        for fam in fams.iter().filter(|f| f.sheet == "Columns" || f.sheet == "Beams") {
            for den in [2, 1] {
                if self.count(fam.sheet) == 0 {
                    self.edit_family(fam, mts, den);
                }
            }
        }
    }

    /// Rows kept free of valid edits so that a whole-row refusal has a row to land on.
    fn reserve(&mut self) {
        for sheet in ["Walls", "Openings", "Rooms", "Slabs", "Curtain walls"] {
            let rows = self.n(sheet) - 1;
            let r = 1 + self.rng.below(rows);
            self.reserved.insert((sheet.to_string(), r));
        }
    }

    fn shuffle(&mut self, v: &mut [usize]) {
        for i in (1..v.len()).rev() {
            let j = self.rng.below(i + 1);
            v.swap(i, j);
        }
    }

    /// A cell the import refuses alone; `bad` builds it from the cell's current value.
    fn bad_cell(&mut self, sheet: &str, header: &str, bad: &dyn Fn(&C) -> C) {
        let c = self.col(sheet, header);
        let mut rows: Vec<usize> = (1..self.n(sheet)).collect();
        self.shuffle(&mut rows);
        rows.sort_by_key(|&r| !self.row_used(sheet, r));
        for r in rows.iter().copied() {
            let key = (sheet.to_string(), r, c);
            if self.used_cell(&key) || self.reserved.contains(&(sheet.to_string(), r)) {
                continue;
            }
            let v = bad(&self.cur_cell(sheet, r, c));
            self.put_book(sheet, r, c, v);
            self.bad_cell.insert(key);
            return;
        }
        // a short sheet (Layers: one row per seeded layer) can have every cell of the column
        // taken by valid edits: one of them gives its cell up, the rest of its row stays edited
        for r in rows {
            let key = (sheet.to_string(), r, c);
            if !self.valid.contains_key(&key) {
                continue;
            }
            self.valid.remove(&key);
            let v = bad(&self.cur_cell(sheet, r, c));
            self.put_book(sheet, r, c, v);
            self.bad_cell.insert(key);
            return;
        }
        panic!("setup: no free cell for a bad value in {sheet}/{header}");
    }

    /// A cell that makes the import refuse its whole row (a reserved row, free of other edits).
    fn bad_row(&mut self, sheet: &str, header: &str, v: &str) {
        let c = self.col(sheet, header);
        let rows: Vec<usize> = self
            .reserved
            .iter()
            .filter(|k| k.0 == sheet)
            .map(|k| k.1)
            .collect();
        for r in rows {
            if self.row_used(sheet, r) {
                continue;
            }
            self.put_book(sheet, r, c, C::Text(v.into()));
            self.bad_row.insert((sheet.to_string(), r, c));
            return;
        }
        panic!("setup: no free row for a refused row in {sheet}");
    }

    fn bad_edits(&mut self, fams: &[FamilySheet]) {
        let text = |t: &'static str| move |_: &C| C::Text(t.into());
        let num = |x: f64| move |_: &C| C::Num(x);
        self.bad_cell("Walls", "height [m]", &text("tall"));
        self.bad_cell("Walls", "height [m]", &num(-1.0));
        self.bad_cell("Walls", "ax [m]", &text("x?"));
        self.bad_cell("Walls", "base_offset [m]", &text("n/a"));
        self.bad_cell("Rooms", "height [m]", &text("high"));
        self.bad_cell("Levels", "elevation [m]", &text("up"));
        self.bad_cell("Layers", "thickness [m]", &num(-0.01));
        self.bad_cell("SlabLayers", "thickness [m]", &num(0.0));
        self.bad_cell("Slabs", "offset [m]", &text("high"));
        self.bad_cell("Openings", "sill [m]", &num(-0.3));
        self.bad_cell("Openings", "along [m]", &text("far"));
        self.bad_cell("Curtain walls", "ax [m]", &|c: &C| C::Num(num_of(c).unwrap_or(0.0) + 1.0));
        for (sheet, bad) in [("Columns", C::Text("far".into())), ("Beams", C::Num(2.0e4))] {
            if let Some(fam) = fams.iter().find(|f| f.sheet == sheet) {
                if let Some(fc) = fam.cols.iter().find(|c| c.dim == FamDim::Len) {
                    self.bad_cell(sheet, &fc.header(Units::Metric), &move |_: &C| bad.clone());
                }
            }
        }
        self.bad_row("Walls", "level", "No such level");
        self.bad_row("Slabs", "outline [m] (x y; x y; ...)", "garbage");
        self.bad_row("Openings", "kind", "archway");
        self.bad_row("Rooms", "level", "Nowhere");
        self.bad_row("Curtain walls", "level", "Nowhere");
    }

    fn addr(&self, k: &Key) -> Addr {
        let s = &self.orig[self.si(&k.0)];
        (k.0.clone(), k.1 as u32 + s.origin.0 + 1, k.2 as u32 + s.origin.1 + 1)
    }

    fn valid_addrs(&self) -> BTreeSet<Addr> {
        self.valid.keys().map(|k| self.addr(k)).collect()
    }

    fn bad_addrs(&self) -> BTreeSet<Addr> {
        self.bad_cell
            .iter()
            .chain(self.bad_row.iter())
            .map(|k| self.addr(k))
            .collect()
    }
}

// ---------------------------------------------------------------- run

struct Run {
    ed: Editor,
    fams: Vec<FamilySheet>,
    plan: Plan,
    book: PathBuf,
}

fn prepare(seed: u64, tag: &str, with_bad: bool) -> Run {
    let ed = fixture(seed);
    let fams = families();
    let src = ec::temp_file(&format!("fz_{tag}_{seed}_src.xlsx"));
    xlsx::export_xlsx_families(&ed.model, &src, Units::Metric, &fams).unwrap();
    let mut plan = Plan::new(seed, read_grids(&src).unwrap());
    let mts = ids(&ed.model, "member_type");
    if with_bad {
        plan.reserve();
    }
    plan.valid_edits(&fams, &mts);
    if with_bad {
        plan.bad_edits(&fams);
    }
    let missing: Vec<&str> = EDITED.iter().copied().filter(|s| plan.count(s) == 0).collect();
    assert!(missing.is_empty(), "seed {seed}: setup made no edit on {missing:?}");
    let book = ec::temp_file(&format!("fz_{tag}_{seed}_book.xlsx"));
    write_grids(&book, &plan.book).unwrap();
    Run { ed, fams, plan, book }
}

fn preview(ed: &mut Editor, book: &Path, fams: &[FamilySheet]) -> Report {
    let o = ImportOptions {
        apply: false,
        delete_missing: false,
        new_params: false,
    };
    xlsx::import_xlsx_families(&mut ed.model, book, o, fams).unwrap()
}

fn apply(ed: &mut Editor, rep: &Report) {
    ed.apply_xlsx_props(rep.batch(), rep.props.clone(), "fuzz").unwrap();
    assert_clean(&ed.model, "after the book is applied");
}

fn reexport(ed: &Editor, fams: &[FamilySheet], seed: u64, tag: &str) -> Vec<SheetGrid> {
    let p = ec::temp_file(&format!("fz_{tag}_{seed}_back.xlsx"));
    xlsx::export_xlsx_families(&ed.model, &p, Units::Metric, fams).unwrap();
    read_grids(&p).unwrap()
}

fn fail(seed: u64, what: &str, problems: &[String]) {
    if problems.is_empty() {
        return;
    }
    let shown: Vec<&String> = problems.iter().take(12).collect();
    panic!(
        "seed {seed}: {what}: {} problems\n{}",
        problems.len(),
        shown.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n")
    );
}

fn check_preview(rep: &Report, plan: &Plan) -> Vec<String> {
    let mut out = vec![];
    if rep.blocking() > 0 {
        out.push(format!("blocking rejections: {:?}", rep.rejected));
    }
    let want_rej = plan.bad_addrs();
    let got_rej: Vec<Addr> = rep
        .rejected
        .iter()
        .map(|r| (r.sheet.clone(), r.row, r.col))
        .collect();
    let got_set: BTreeSet<Addr> = got_rej.iter().cloned().collect();
    if got_set != want_rej || got_rej.len() != want_rej.len() {
        out.push(format!(
            "refused cells differ: want {want_rej:?}, got {:?}",
            rep.rejected
        ));
    }
    if rep.skipped() != want_rej.len() {
        out.push(format!("skipped() is {}, want {}", rep.skipped(), want_rej.len()));
    }
    if !rep.conflicts.is_empty() {
        out.push(format!("conflicts on a book of the current model: {:?}", rep.conflicts));
    }
    let want = plan.valid_addrs();
    let got: Vec<Addr> = rep
        .changes
        .iter()
        .map(|c| (c.sheet.clone(), c.row, c.col))
        .collect();
    let got_set: BTreeSet<Addr> = got.iter().cloned().collect();
    if got_set != want || got.len() != want.len() {
        let missing: Vec<&Addr> = want.difference(&got_set).take(8).collect();
        let extra: Vec<&Addr> = got_set.difference(&want).take(8).collect();
        out.push(format!(
            "changes differ: {} edited, {} reported; unreported {missing:?}; unplanned {extra:?}",
            want.len(),
            got.len()
        ));
    }
    let rows: BTreeSet<(String, u32)> = want.iter().map(|k| (k.0.clone(), k.1)).collect();
    let c = rep.counts();
    if (c.added, c.deleted) != (0, 0) || c.changed != rows.len() {
        out.push(format!(
            "counts {c:?}, want {} changed rows and no added or deleted",
            rows.len()
        ));
    }
    out
}

#[derive(Clone, Copy, PartialEq)]
enum Part {
    Edited,
    Rest,
    All,
}

fn compare(plan: &Plan, got: &[SheetGrid], fams: &[FamilySheet], part: Part) -> Vec<String> {
    let mut out = vec![];
    let names = |g: &[SheetGrid]| g.iter().map(|s| s.name.clone()).collect::<Vec<_>>();
    if names(got) != names(&plan.orig[..]) {
        out.push(format!("sheets differ: {:?} vs {:?}", names(got), names(&plan.orig[..])));
    }
    for o in &plan.orig {
        let Some(g) = got.iter().find(|s| s.name == o.name) else {
            continue;
        };
        let heads: Vec<String> = o.rows[0].iter().map(text_of).collect();
        if !heads.iter().any(|h| included(&o.name, h, fams)) {
            continue;
        }
        let back: Vec<String> = g.rows[0].iter().map(text_of).collect();
        if back != heads {
            out.push(format!("{}: headers differ: {back:?} vs {heads:?}", o.name));
            continue;
        }
        if g.rows.len() != o.rows.len() {
            out.push(format!("{}: {} rows, was {}", o.name, g.rows.len(), o.rows.len()));
        }
        let by_key: BTreeMap<String, usize> = (1..g.rows.len()).map(|r| (row_key(g, r), r)).collect();
        for r in 1..o.rows.len() {
            let Some(&gr) = by_key.get(&row_key(o, r)) else {
                out.push(format!("{}: element {} is gone", o.name, row_key(o, r)));
                continue;
            };
            for (c, h) in heads.iter().enumerate() {
                if !included(&o.name, h, fams) {
                    continue;
                }
                let edited = plan.valid.contains_key(&(o.name.clone(), r, c));
                if (part == Part::Edited && !edited) || (part == Part::Rest && edited) {
                    continue;
                }
                let want = plan.want_at(&o.name, r, c);
                let have = g.rows[gr].get(c).cloned().unwrap_or(C::Text(String::new()));
                if !cell_eq(&want, &have) {
                    out.push(format!(
                        "{}[{}] {h}: want {want:?}, got {have:?} ({})",
                        o.name,
                        row_key(o, r),
                        if edited { "edited cell" } else { "untouched cell" }
                    ));
                }
            }
        }
    }
    out
}

/// Valid edits only: preview, apply, re-export, compare `part`.
fn valid_round(seed: u64, tag: &str, part: Part) {
    let mut run = prepare(seed, tag, false);
    let before = dump_data(&run.ed.model);
    let rep = preview(&mut run.ed, &run.book, &run.fams);
    fail(seed, "preview", &check_preview(&rep, &run.plan));
    assert_same(&dump_data(&run.ed.model), &before, "preview changes nothing");
    apply(&mut run.ed, &rep);
    let back = reexport(&run.ed, &run.fams, seed, tag);
    fail(seed, "re-export", &compare(&run.plan, &back, &run.fams, part));
}

/// Valid and bad cells: bad ones refused alone, the rest applied, bad cells keep the model value.
fn bad_round(seed: u64, tag: &str) {
    let mut run = prepare(seed, tag, true);
    let before = dump_data(&run.ed.model);
    let rep = preview(&mut run.ed, &run.book, &run.fams);
    fail(seed, "preview", &check_preview(&rep, &run.plan));
    assert_same(&dump_data(&run.ed.model), &before, "preview changes nothing");
    apply(&mut run.ed, &rep);
    let back = reexport(&run.ed, &run.fams, seed, tag);
    fail(seed, "re-export", &compare(&run.plan, &back, &run.fams, Part::All));
}

/// One undo restores the whole model; redo brings the edits back; a second undo restores again.
fn undo_round(seed: u64, tag: &str, with_bad: bool) {
    let mut run = prepare(seed, tag, with_bad);
    let before = dump_data(&run.ed.model);
    let h0 = data_hash(&run.ed.model);
    let rep = preview(&mut run.ed, &run.book, &run.fams);
    fail(seed, "preview", &check_preview(&rep, &run.plan));
    apply(&mut run.ed, &rep);
    let after = dump_data(&run.ed.model);
    assert_ne!(data_hash(&run.ed.model), h0, "seed {seed}: the book changed nothing");
    assert!(run.ed.can_undo(), "seed {seed}: the book is not an undo step");
    run.ed.undo();
    assert_clean(&run.ed.model, "after undo");
    assert_eq!(data_hash(&run.ed.model), h0, "seed {seed}: one undo left a different model");
    assert_same(&dump_data(&run.ed.model), &before, "one undo of the whole book");
    assert!(!run.ed.can_undo(), "seed {seed}: the book took more than one undo step");
    assert!(run.ed.can_redo(), "seed {seed}: nothing to redo after undo");
    run.ed.redo();
    assert_clean(&run.ed.model, "after redo");
    assert_same(&dump_data(&run.ed.model), &after, "redo of the whole book");
    run.ed.undo();
    assert_eq!(data_hash(&run.ed.model), h0, "seed {seed}: second undo left a different model");
}

// ---------------------------------------------------------------- tests

#[test]
fn xlsx_fuzz_edits_land_exactly() {
    for seed in SEEDS {
        valid_round(seed, "land", Part::Edited);
    }
}

#[test]
fn xlsx_fuzz_untouched_cells_identical() {
    for seed in SEEDS {
        valid_round(seed, "rest", Part::Rest);
    }
}

#[test]
fn xlsx_fuzz_bad_cells_refused_rest_applied() {
    for seed in SEEDS {
        bad_round(seed, "bad");
    }
}

#[test]
fn xlsx_fuzz_one_undo_restores_hash() {
    for seed in SEEDS {
        undo_round(seed, "undo", false);
    }
}

/// 50 seeds (`DDC_E2E_SEQS`) of all four checks in one pass, bad cells included.
#[test]
#[ignore = "50 seeds; run with --ignored"]
fn xlsx_fuzz_50_seeds() {
    for seed in 1..=ec::seqs(50) {
        valid_round(seed, "s50v", Part::All);
        bad_round(seed, "s50b");
        undo_round(seed, "s50u", true);
    }
}
