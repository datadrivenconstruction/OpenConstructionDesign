// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Floor, roof and ceiling types through the book: SlabTypes and SlabLayers sheets.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use calamine::{open_workbook, Data, Reader, Xlsx};
use ddc_model::{CeilingSpec, LayerSpec, Model, RoofKind, RoofSpec, SlabSpec};

use super::baseline::{self, Take};
use super::tests::{edit, set, tmp};
use super::*;

type Grid = Vec<Vec<Data>>;

const RING: [(f64, f64); 4] = [(0.0, 0.0), (4.0, 0.0), (4.0, 3.0), (0.0, 3.0)];

fn layer(material: &str, thickness: f64) -> LayerSpec {
    LayerSpec {
        material: material.into(),
        thickness,
    }
}

struct Fx {
    m: Model,
    floor: i64,
    deck: i64,
    roofing: i64,
    slab: i64,
    roof: i64,
    ceiling: i64,
}

/// Type "floor" carries a slab, a roof and a ceiling; "deck" and "roofing" are spare types.
fn fixture() -> Fx {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let floor = m
        .add_slab_type("floor", &[layer("concrete", 0.2), layer("screed", 0.05)])
        .unwrap();
    let deck = m.add_slab_type("deck", &[layer("timber", 0.1)]).unwrap();
    let roofing = m.add_slab_type("roofing", &[layer("membrane", 0.01)]).unwrap();
    let slab = m
        .add_slab(&SlabSpec {
            level: lv,
            slab_type: floor,
            offset: 0.0,
            outer: RING.to_vec(),
            holes: vec![],
        })
        .unwrap();
    let roof = m
        .add_roof(&RoofSpec {
            level: lv,
            slab_type: floor,
            offset: 3.0,
            overhang: 0.0,
            fascia: 0.0,
            kind: RoofKind::Footprint {
                ring: RING.to_vec(),
                slopes: vec![Some(30.0); 4],
                mansard: None,
            },
        })
        .unwrap();
    let ceiling = m
        .add_ceiling(&CeilingSpec {
            level: lv,
            slab_type: floor,
            height: 2.5,
            outer: RING.to_vec(),
            holes: vec![],
        })
        .unwrap();
    Fx {
        m,
        floor,
        deck,
        roofing,
        slab,
        roof,
        ceiling,
    }
}

fn export(m: &Model, name: &str) -> PathBuf {
    let p = tmp(name);
    export_xlsx(m, &p).unwrap();
    p
}

fn grid(p: &Path, sheet: &str) -> Grid {
    let mut wb: Xlsx<_> = open_workbook(p).unwrap();
    wb.worksheet_range(sheet)
        .unwrap()
        .rows()
        .map(|r| r.to_vec())
        .collect()
}

fn num(d: &Data) -> f64 {
    match d {
        Data::Float(x) => *x,
        Data::Int(i) => *i as f64,
        _ => f64::NAN,
    }
}

fn col_of(g: &Grid, header: &str) -> usize {
    g[0].iter().position(|d| d.to_string() == header).unwrap()
}

/// Grid row (0 = header) whose `header` cell holds `v`; with `pos`, also the layer position.
fn row_of(g: &Grid, header: &str, v: i64, pos: Option<i64>) -> usize {
    let c = col_of(g, header);
    (1..g.len())
        .find(|&r| {
            num(&g[r][c]) == v as f64
                && pos.is_none_or(|p| num(&g[r][col_of(g, "pos")]) == p as f64)
        })
        .unwrap()
}

fn new_row(g: &mut Grid) -> usize {
    g.push(vec![Data::Empty; g[0].len()]);
    g.len() - 1
}

fn s(v: &str) -> Data {
    Data::String(v.into())
}

fn layer_of(m: &Model, st: i64, pos: i64) -> (String, f64) {
    m.sql()
        .query_row(
            "SELECT material,thickness FROM slab_type_layer WHERE stype=?1 AND pos=?2",
            [st, pos],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
}

fn type_name(m: &Model, id: i64) -> String {
    m.sql()
        .query_row("SELECT name FROM slab_type WHERE id=?1", [id], |r| r.get(0))
        .unwrap()
}

fn count(m: &Model, sql: &str) -> i64 {
    m.sql().query_row(sql, [], |r| r.get(0)).unwrap()
}

fn ops(m: &Model) -> i64 {
    count(m, "SELECT count(*) FROM op")
}

fn sheet_xml(p: &Path, n: usize) -> String {
    let mut z = zip::ZipArchive::new(std::fs::File::open(p).unwrap()).unwrap();
    let mut out = String::new();
    z.by_name(&format!("xl/worksheets/sheet{n}.xml"))
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    out
}

#[test]
fn slab_layer_thickness_roundtrip() {
    let Fx {
        mut m,
        floor,
        deck,
        slab,
        roof,
        ceiling,
        ..
    } = fixture();
    let p = export(&m, "st_round_src");
    // both sheets, their baseline (layers keyed by type and position) and a material dropdown
    let mut wb: Xlsx<_> = open_workbook(&p).unwrap();
    let base = baseline::read(&mut wb).expect("baseline");
    assert!(base.has_id(SLAB_TYPES, floor) && base.has_id(SLAB_TYPES, deck));
    assert!(base.has_id(SLAB_LAYERS, layer_key(floor, 1)));
    let lg = grid(&p, SLAB_LAYERS);
    assert_eq!(lg.len(), 5, "a row per layer of the three types");
    let styled = tmp("st_round_styled");
    export_xlsx_styled(&m, &styled, &styled, Units::Metric, &[], &Style::professional(HashMap::new()))
        .unwrap();
    let names = super::book::sheet_names(&styled).unwrap();
    let at = names.iter().position(|n| n == SLAB_LAYERS).unwrap() + 1;
    let xml = sheet_xml(&styled, at);
    assert!(xml.contains("dataValidation") && xml.contains("Lists!"), "material dropdown");

    // unchanged book: nothing to do
    let before = ops(&m);
    let rep = import_xlsx(&mut m, &p).unwrap();
    assert!(rep.applied && rep.changes.is_empty() && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(ops(&m), before);

    let slab_v = m.slab_quantities(slab).unwrap()[0].volume;
    let roof_v = m.roof_quantities(roof).unwrap().layers[0].3;
    let ceil_v = m.ceiling_quantities(ceiling).unwrap()[0].2;
    let q = tmp("st_round_dst");
    edit(&p, &q, |sh, g| {
        if sh == SLAB_LAYERS {
            let r = row_of(g, "type_id", floor, Some(0));
            set(g, r, "thickness [m]", Data::Float(0.25));
            let r = row_of(g, "type_id", floor, Some(1));
            set(g, r, "material", s("anhydrite"));
            // out of range: only this cell is refused
            let r = row_of(g, "type_id", deck, Some(0));
            set(g, r, "thickness [m]", Data::Float(-0.1));
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert_eq!(rep.changes.len(), 2, "{:?}", rep.changes);
    assert_eq!(rep.rejected.len(), 1, "{:?}", rep.rejected);
    let bad = &rep.rejected[0];
    assert_eq!((bad.sheet.as_str(), bad.column.as_str()), (SLAB_LAYERS, "thickness [m]"));
    assert!(bad.reason.starts_with("thickness must be > 0"), "{}", bad.reason);
    assert_eq!(rep.blocking(), 0);
    // one batch, one undo step
    assert_eq!(ops(&m), before + 1);
    assert_eq!(layer_of(&m, floor, 0), ("concrete".into(), 0.25));
    assert_eq!(layer_of(&m, floor, 1), ("anhydrite".into(), 0.05));
    assert_eq!(layer_of(&m, deck, 0), ("timber".into(), 0.1), "refused cell keeps the model");
    // the slab, the roof and the ceiling of the type follow
    let sq = &m.slab_quantities(slab).unwrap()[0];
    assert!((sq.thickness - 0.25).abs() < 1e-12 && (sq.volume - sq.area * 0.25).abs() < 1e-9);
    assert!(sq.volume > slab_v);
    let rq = m.roof_quantities(roof).unwrap();
    assert!((rq.layers[0].1 - 0.25).abs() < 1e-12 && rq.layers[0].3 > roof_v);
    let cq = &m.ceiling_quantities(ceiling).unwrap()[0];
    assert!((cq.2 - cq.1 * 0.25).abs() < 1e-9 && cq.2 > ceil_v);
    // the reloaded book shows the new values
    let r = export(&m, "st_round_again");
    let g = grid(&r, SLAB_LAYERS);
    let i = row_of(&g, "type_id", floor, Some(0));
    assert!((num(&g[i][col_of(&g, "thickness [m]")]) - 0.25).abs() < 1e-12);
    let rep = import_xlsx(&mut m, &r).unwrap();
    assert!(rep.applied && rep.changes.is_empty() && rep.rejected.is_empty(), "{rep:?}");
    // undo takes back the whole book edit
    m.undo().unwrap();
    assert_eq!(layer_of(&m, floor, 0), ("concrete".into(), 0.2));
    assert_eq!(layer_of(&m, floor, 1), ("screed".into(), 0.05));
    let cq = &m.ceiling_quantities(ceiling).unwrap()[0];
    assert!((cq.2 - ceil_v).abs() < 1e-9);
}

#[test]
fn slab_type_rename_and_dup_refused() {
    let Fx {
        mut m,
        floor,
        deck,
        roofing,
        slab,
        ..
    } = fixture();
    let p = export(&m, "st_ren_src");
    let q = tmp("st_ren_dst");
    edit(&p, &q, |sh, g| {
        if sh == SLAB_TYPES {
            let r = row_of(g, "id", floor, None);
            set(g, r, "name", s("Floor A"));
            let r = row_of(g, "id", deck, None);
            set(g, r, "name", s("roofing"));
        }
    });
    let before = ops(&m);
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.blocking() == 0, "{rep:?}");
    assert_eq!(ops(&m), before + 1);
    assert_eq!(type_name(&m, floor), "Floor A");
    assert_eq!(type_name(&m, deck), "deck", "a taken name is refused");
    assert_eq!(type_name(&m, roofing), "roofing");
    let g = grid(&q, SLAB_TYPES);
    let deck_row = row_of(&g, "id", deck, None) as u32 + 1;
    let name_col = col_of(&g, "name") as u32 + 1;
    let dup = rep
        .rejected
        .iter()
        .find(|r| r.sheet == SLAB_TYPES && r.row == deck_row)
        .expect("refusal on the deck row");
    assert_eq!(dup.col, name_col);
    assert_eq!(dup.reason, "duplicate slab type name 'roofing'");
    assert!(
        !rep.changes.iter().any(|c| c.sheet == SLAB_TYPES && c.row == deck_row),
        "{:?}",
        rep.changes
    );
    // the slab keeps its type; its row in the next book shows the new name
    let st: i64 = m
        .sql()
        .query_row("SELECT stype FROM slab WHERE id=?1", [slab], |r| r.get(0))
        .unwrap();
    assert_eq!(st, floor);
    let r = export(&m, "st_ren_again");
    let g = grid(&r, SLABS);
    let i = row_of(&g, "id", slab, None);
    assert_eq!(g[i][col_of(&g, "type")].to_string(), "Floor A");

    // two rows renamed to one new name: both refused, nothing changes
    let q2 = tmp("st_ren_dst2");
    edit(&r, &q2, |sh, g| {
        if sh == SLAB_TYPES {
            for id in [deck, roofing] {
                let r = row_of(g, "id", id, None);
                set(g, r, "name", s("Same"));
            }
        }
    });
    let before = ops(&m);
    let rep = import_xlsx(&mut m, &q2).unwrap();
    let dups = rep
        .rejected
        .iter()
        .filter(|r| r.sheet == SLAB_TYPES && r.reason == "duplicate slab type name 'Same'")
        .count();
    assert_eq!(dups, 2, "{:?}", rep.rejected);
    assert_eq!(ops(&m), before, "no edit left to apply");
    assert_eq!((type_name(&m, deck), type_name(&m, roofing)), ("deck".into(), "roofing".into()));
}

#[test]
fn slab_layer_create_refused_with_reason() {
    let Fx { mut m, floor, .. } = fixture();
    let p = export(&m, "st_new_src");
    let q = tmp("st_new_dst");
    edit(&p, &q, |sh, g| {
        if sh == SLAB_LAYERS {
            let r = new_row(g);
            set(g, r, "type_id", Data::Float(floor as f64));
            set(g, r, "pos", Data::Float(2.0));
            set(g, r, "material", s("tiles"));
            set(g, r, "thickness [m]", Data::Float(0.01));
            let r = new_row(g);
            set(g, r, "material", s("tiles"));
            set(g, r, "thickness [m]", Data::Float(0.01));
            // the rest of the book still applies
            let r = row_of(g, "type_id", floor, Some(1));
            set(g, r, "thickness [m]", Data::Float(0.06));
        }
        if sh == SLAB_TYPES {
            let r = new_row(g);
            set(g, r, "name", s("new type"));
        }
    });
    let types = count(&m, "SELECT count(*) FROM slab_type");
    let layers = count(&m, "SELECT count(*) FROM slab_type_layer");
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.blocking() == 0, "{rep:?}");
    let g = grid(&q, SLAB_LAYERS);
    let why = |sheet: &str, row: usize| {
        rep.rejected
            .iter()
            .find(|r| r.sheet == sheet && r.row == row as u32 + 1)
            .map(|r| (r.col, r.reason.clone()))
            .unwrap_or_else(|| panic!("no refusal on {sheet} row {row}: {:?}", rep.rejected))
    };
    let (c, reason) = why(SLAB_LAYERS, g.len() - 2);
    assert_eq!(c, col_of(&g, "pos") as u32 + 1);
    assert_eq!(
        reason,
        format!("slab type {floor} has no layer 2; adding layers from a workbook is not supported")
    );
    let (_, reason) = why(SLAB_LAYERS, g.len() - 1);
    assert_eq!(reason, "creating slab layers from a workbook is not supported");
    let tg = grid(&q, SLAB_TYPES);
    let (_, reason) = why(SLAB_TYPES, tg.len() - 1);
    assert_eq!(reason, "creating slab types from a workbook is not supported");
    assert_eq!(count(&m, "SELECT count(*) FROM slab_type"), types);
    assert_eq!(count(&m, "SELECT count(*) FROM slab_type_layer"), layers);
    assert_eq!(layer_of(&m, floor, 1), ("screed".into(), 0.06));
}

#[test]
fn slab_type_conflict_per_cell() {
    let Fx { mut m, floor, .. } = fixture();
    let p = export(&m, "st_conf_src");
    let q = tmp("st_conf_dst");
    edit(&p, &q, |sh, g| {
        if sh == SLAB_LAYERS {
            let r = row_of(g, "type_id", floor, Some(0));
            set(g, r, "thickness [m]", Data::Float(0.25));
            let r = row_of(g, "type_id", floor, Some(1));
            set(g, r, "material", s("anhydrite"));
        }
        if sh == SLAB_TYPES {
            let r = row_of(g, "id", floor, None);
            set(g, r, "name", s("Floor B"));
        }
    });
    // after the export the model changes the name and the first layer, not the second
    m.set_slab_type(floor, "Floor M", &[layer("concrete", 0.22), layer("screed", 0.05)])
        .unwrap();
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied, "{rep:?}");
    let g = grid(&q, SLAB_LAYERS);
    let tg = grid(&q, SLAB_TYPES);
    let mut cells: Vec<(String, u32, u32)> =
        rep.conflicts.iter().map(|c| (c.sheet.clone(), c.row, c.col)).collect();
    cells.sort();
    let want = vec![
        (
            SLAB_LAYERS.to_string(),
            row_of(&g, "type_id", floor, Some(0)) as u32 + 1,
            col_of(&g, "thickness [m]") as u32 + 1,
        ),
        (
            SLAB_TYPES.to_string(),
            row_of(&tg, "id", floor, None) as u32 + 1,
            col_of(&tg, "name") as u32 + 1,
        ),
    ];
    assert_eq!(cells, want, "{:?}", rep.conflicts);
    assert!(rep.conflicts.iter().all(|c| !c.take_book));
    // conflicting cells keep the model, the other cell of the type applies
    assert_eq!(type_name(&m, floor), "Floor M");
    assert_eq!(layer_of(&m, floor, 0), ("concrete".into(), 0.22));
    assert_eq!(layer_of(&m, floor, 1), ("anhydrite".into(), 0.05));

    // a re-export keeps the book values of the kept cells, so the conflict is found again
    let r = tmp("st_conf_again");
    baseline::with_kept(&rep.kept, || export_xlsx(&m, &r)).unwrap();
    let g2 = grid(&r, SLAB_LAYERS);
    let i = row_of(&g2, "type_id", floor, Some(0));
    assert!((num(&g2[i][col_of(&g2, "thickness [m]")]) - 0.25).abs() < 1e-12);
    let rep = import_xlsx_with(&mut m, &r, ImportOptions::default()).unwrap();
    assert_eq!(rep.conflicts.len(), 2, "{:?}", rep.conflicts);

    // taking the book values applies them in one step
    let before = ops(&m);
    let all = Take {
        all: true,
        ..Take::default()
    };
    let rep = baseline::with_take(&all, || import_xlsx(&mut m, &r)).unwrap();
    assert!(rep.applied && rep.conflicts.iter().all(|c| c.take_book), "{rep:?}");
    assert_eq!(ops(&m), before + 1);
    assert_eq!(type_name(&m, floor), "Floor B");
    assert_eq!(layer_of(&m, floor, 0), ("concrete".into(), 0.25));
}

/// Adds a column `head` to the grid with `cells` (grid row, text) filled in.
fn user_col(g: &mut Grid, head: &str, cells: &[(usize, &str)]) {
    g[0].push(s(head));
    let c = g[0].len() - 1;
    for (r, t) in cells {
        while g[*r].len() <= c {
            g[*r].push(Data::Empty);
        }
        g[*r][c] = s(t);
    }
}

fn text_at(g: &Grid, r: usize, head: &str) -> String {
    g[r].get(col_of(g, head)).map_or(String::new(), |d| d.to_string())
}

fn wall_layer_of(m: &Model, wt: i64, pos: i64) -> (String, f64) {
    m.sql()
        .query_row(
            "SELECT material,thickness FROM wall_type_layer WHERE wtype=?1 AND pos=?2",
            [wt, pos],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
}

#[test]
fn user_column_on_layers_survives_reexport() {
    let Fx { mut m, .. } = fixture();
    let wt = m
        .add_wall_type("ext", &[layer("brick", 0.2), layer("wool", 0.1)])
        .unwrap();
    let p = export(&m, "ul_src");
    let q = tmp("ul_dst");
    edit(&p, &q, |sh, g| {
        if sh == LAYERS {
            let (a, b) = (
                row_of(g, "type_id", wt, Some(0)),
                row_of(g, "type_id", wt, Some(1)),
            );
            user_col(g, "my note", &[(a, "outer"), (b, "inner")]);
        }
    });
    export_xlsx(&m, &q).unwrap();
    let g = grid(&q, LAYERS);
    assert_eq!(text_at(&g, row_of(&g, "type_id", wt, Some(0)), "my note"), "outer");
    assert_eq!(text_at(&g, row_of(&g, "type_id", wt, Some(1)), "my note"), "inner");

    // layer 1 disappears: its cell is dropped with a note, layer 0 keeps its own
    m.sql()
        .execute("DELETE FROM wall_type_layer WHERE wtype=?1 AND pos=1", [wt])
        .unwrap();
    export_xlsx(&m, &q).unwrap();
    let notes = lost_user_cells();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains(LAYERS) && notes[0].contains("layer 1"), "{notes:?}");
    let g = grid(&q, LAYERS);
    assert_eq!(g.len(), 2, "header and one layer row left");
    assert_eq!(text_at(&g, 1, "my note"), "outer");
}

#[test]
fn user_column_on_slab_layers_survives_reexport() {
    let Fx { mut m, floor, deck, .. } = fixture();
    let p = export(&m, "usl_src");
    let q = tmp("usl_dst");
    edit(&p, &q, |sh, g| {
        if sh == SLAB_LAYERS {
            let (a, b, c) = (
                row_of(g, "type_id", floor, Some(0)),
                row_of(g, "type_id", floor, Some(1)),
                row_of(g, "type_id", deck, Some(0)),
            );
            user_col(g, "my note", &[(a, "slab"), (b, "top"), (c, "deck")]);
        }
    });
    export_xlsx(&m, &q).unwrap();
    assert!(lost_user_cells().is_empty());
    let g = grid(&q, SLAB_LAYERS);
    assert_eq!(text_at(&g, row_of(&g, "type_id", floor, Some(0)), "my note"), "slab");
    assert_eq!(text_at(&g, row_of(&g, "type_id", floor, Some(1)), "my note"), "top");
    assert_eq!(text_at(&g, row_of(&g, "type_id", deck, Some(0)), "my note"), "deck");

    // the floor loses its second layer: that cell is dropped and named, never moved to deck
    m.set_slab_type(floor, "floor", &[layer("concrete", 0.2)]).unwrap();
    export_xlsx(&m, &q).unwrap();
    let notes = lost_user_cells();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains(SLAB_LAYERS), "{notes:?}");
    let g = grid(&q, SLAB_LAYERS);
    assert_eq!(text_at(&g, row_of(&g, "type_id", floor, Some(0)), "my note"), "slab");
    assert_eq!(text_at(&g, row_of(&g, "type_id", deck, Some(0)), "my note"), "deck");
    let texts: Vec<String> = (1..g.len()).map(|r| text_at(&g, r, "my note")).collect();
    assert!(!texts.contains(&"top".to_string()), "{texts:?}");
}

#[test]
fn user_column_on_levels_survives_reexport() {
    let Fx { mut m, .. } = fixture();
    let l2 = m.add_level("L2", 3.0).unwrap();
    let p = export(&m, "ulv_src");
    let q = tmp("ulv_dst");
    edit(&p, &q, |sh, g| {
        if sh == LEVELS {
            let r = row_of(g, "id", l2, None);
            user_col(g, "my note", &[(r, "roof level")]);
        }
    });
    export_xlsx(&m, &q).unwrap();
    let g = grid(&q, LEVELS);
    assert_eq!(text_at(&g, row_of(&g, "id", l2, None), "my note"), "roof level");
    // an edit of the level in the book still imports with the extra column present
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.rejected.is_empty(), "{:?}", rep.rejected);
}

#[test]
fn user_column_on_slab_types_survives_reexport() {
    let Fx { m, floor, deck, .. } = fixture();
    let p = export(&m, "ust_src");
    let q = tmp("ust_dst");
    edit(&p, &q, |sh, g| {
        if sh == SLAB_TYPES {
            let r = row_of(g, "id", deck, None);
            user_col(g, "my note", &[(r, "spare")]);
        }
    });
    export_xlsx(&m, &q).unwrap();
    let g = grid(&q, SLAB_TYPES);
    assert_eq!(text_at(&g, row_of(&g, "id", deck, None), "my note"), "spare");
    assert_eq!(text_at(&g, row_of(&g, "id", floor, None), "my note"), "");
}

#[test]
fn layers_conflict_cell_kept_like_slab_layers() {
    let Fx { mut m, .. } = fixture();
    let wt = m
        .add_wall_type("ext", &[layer("brick", 0.2), layer("wool", 0.1)])
        .unwrap();
    let p = export(&m, "lc_src");
    let q = tmp("lc_dst");
    edit(&p, &q, |sh, g| {
        if sh == LAYERS {
            let r = row_of(g, "type_id", wt, Some(0));
            set(g, r, "thickness [m]", Data::Float(0.25));
            let r = row_of(g, "type_id", wt, Some(1));
            set(g, r, "material", s("glass wool"));
        }
    });
    // after the export the model changes the first layer, not the second
    m.set_layer_thickness(wt, 0, 0.22).unwrap();
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied, "{rep:?}");
    let g = grid(&q, LAYERS);
    let want = vec![(
        LAYERS.to_string(),
        row_of(&g, "type_id", wt, Some(0)) as u32 + 1,
        col_of(&g, "thickness [m]") as u32 + 1,
    )];
    let cells: Vec<(String, u32, u32)> =
        rep.conflicts.iter().map(|c| (c.sheet.clone(), c.row, c.col)).collect();
    assert_eq!(cells, want, "{:?}", rep.conflicts);
    assert_eq!(wall_layer_of(&m, wt, 0), ("brick".into(), 0.22));
    assert_eq!(wall_layer_of(&m, wt, 1), ("glass wool".into(), 0.1));

    // a re-export keeps the book value of the kept cell, so the conflict is found again
    let r = tmp("lc_again");
    baseline::with_kept(&rep.kept, || export_xlsx(&m, &r)).unwrap();
    let g2 = grid(&r, LAYERS);
    let i = row_of(&g2, "type_id", wt, Some(0));
    assert!((num(&g2[i][col_of(&g2, "thickness [m]")]) - 0.25).abs() < 1e-12);
    let rep = import_xlsx_with(&mut m, &r, ImportOptions::default()).unwrap();
    assert_eq!(rep.conflicts.len(), 1, "{:?}", rep.conflicts);

    // taking the book value applies it in one step
    let before = ops(&m);
    let all = Take {
        all: true,
        ..Take::default()
    };
    let rep = baseline::with_take(&all, || import_xlsx(&mut m, &r)).unwrap();
    assert!(rep.applied && rep.conflicts.iter().all(|c| c.take_book), "{rep:?}");
    assert_eq!(ops(&m), before + 1);
    assert_eq!(wall_layer_of(&m, wt, 0), ("brick".into(), 0.25));
}
