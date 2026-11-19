//! Materials sheet: name, colour, hatch and properties of existing materials through the book.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use calamine::{open_workbook, Data, Reader, Xlsx};
use ddc_model::{LayerSpec, MaterialSpec, Model};

use super::baseline::{self, Take};
use super::tests::{col, edit, set, tmp};
use super::*;

type Grid = Vec<Vec<Data>>;

const H_COLOR: &str = "color (#RRGGBB)";
const H_DENSITY: &str = "density [kg/m3]";
const H_COND: &str = "conductivity [W/(m.K)]";
const H_HEAT: &str = "specific heat [J/(kg.K)]";

fn layer(material: &str, thickness: f64) -> LayerSpec {
    LayerSpec {
        material: material.into(),
        thickness,
    }
}

/// A wall type and a slab type that both carry a "brick" layer.
fn fixture() -> Model {
    let mut m = Model::open(":memory:").unwrap();
    m.add_wall_type("W", &[layer("brick", 0.2), layer("plaster", 0.02)]).unwrap();
    m.add_slab_type("S", &[layer("brick", 0.1)]).unwrap();
    m
}

fn export(m: &Model, name: &str) -> PathBuf {
    let p = tmp(name);
    export_xlsx(m, &p).unwrap();
    p
}

fn grid(p: &Path, sheet: &str) -> Grid {
    let mut wb: Xlsx<_> = open_workbook(p).unwrap();
    wb.worksheet_range(sheet).unwrap().rows().map(|r| r.to_vec()).collect()
}

fn num(d: &Data) -> f64 {
    match d {
        Data::Float(x) => *x,
        Data::Int(i) => *i as f64,
        _ => f64::NAN,
    }
}

fn s(v: &str) -> Data {
    Data::String(v.into())
}

fn spec(m: &Model, name: &str) -> MaterialSpec {
    m.material(name).unwrap().spec
}

fn id_of(m: &Model, name: &str) -> i64 {
    let id = m.material(name).unwrap().id;
    assert!(id > 0, "{name} has a row");
    id
}

/// Grid row (0 = header) of the material called `name`.
fn row_named(g: &Grid, name: &str) -> usize {
    let c = col(g, "name");
    (1..g.len()).find(|&r| g[r][c].to_string() == name).unwrap()
}

fn cell(g: &Grid, name: &str, header: &str) -> Data {
    g[row_named(g, name)][col(g, header)].clone()
}

fn ops(m: &Model) -> i64 {
    m.sql().query_row("SELECT count(*) FROM op", [], |r| r.get(0)).unwrap()
}

fn texts(m: &Model, sql: &str) -> Vec<String> {
    let mut st = m.sql().prepare(sql).unwrap();
    let v = st.query_map([], |r| r.get(0)).unwrap().map(|x| x.unwrap()).collect();
    v
}

#[test]
fn material_props_roundtrip() {
    let mut m = fixture();
    let mut c = spec(&m, "concrete");
    c.color = Some(0x336699);
    c.hatch = "cross".into();
    c.density = Some(2500.0);
    c.conductivity = Some(1.5);
    c.specific_heat = Some(900.0);
    c.note = "C30/37".into();
    m.set_material("concrete", &c).unwrap();
    let mut w = spec(&m, "wood");
    w.color = None;
    m.set_material("wood", &w).unwrap();

    let p = export(&m, "mat_round_src");
    let g = grid(&p, MATERIALS);
    assert_eq!(cell(&g, "concrete", H_COLOR).to_string(), "#336699");
    assert_eq!(cell(&g, "concrete", "hatch").to_string(), "cross");
    assert_eq!(num(&cell(&g, "concrete", H_DENSITY)), 2500.0);
    assert_eq!(num(&cell(&g, "concrete", H_COND)), 1.5);
    assert_eq!(num(&cell(&g, "concrete", H_HEAT)), 900.0);
    assert_eq!(cell(&g, "concrete", "note").to_string(), "C30/37");
    assert!(matches!(cell(&g, "wood", H_COLOR), Data::Empty), "no colour = empty cell");
    let mut wb: Xlsx<_> = open_workbook(&p).unwrap();
    let base = baseline::read(&mut wb).expect("baseline");
    assert!(base.has_id(MATERIALS, id_of(&m, "concrete")));

    // an untouched book is no edit and no undo step
    let before = ops(&m);
    let rep = import_xlsx(&mut m, &p).unwrap();
    assert!(rep.applied && rep.changes.is_empty() && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(ops(&m), before);

    // every value of one row and the colour of another change in one undo step
    let q = tmp("mat_round_dst");
    edit(&p, &q, |sh, g| {
        if sh == MATERIALS {
            let r = row_named(g, "concrete");
            set(g, r, H_COLOR, s("a0b0c0"));
            set(g, r, "hatch", s("Dots"));
            set(g, r, H_DENSITY, Data::Float(2300.0));
            set(g, r, H_COND, Data::Float(1.2));
            set(g, r, H_HEAT, Data::Float(880.0));
            set(g, r, "note", s("C25/30"));
            let r = row_named(g, "wood");
            set(g, r, H_COLOR, s("#00ff00"));
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(ops(&m), before + 1, "one undo step for the whole sheet");
    let c2 = spec(&m, "concrete");
    assert_eq!(c2.color, Some(0xA0B0C0));
    assert_eq!(c2.hatch, "dots");
    assert_eq!((c2.density, c2.conductivity, c2.specific_heat), (Some(2300.0), Some(1.2), Some(880.0)));
    assert_eq!(c2.note, "C25/30");
    assert_eq!(spec(&m, "wood").color, Some(0x00FF00));

    // the values come back in the next book
    let r = export(&m, "mat_round_again");
    let g = grid(&r, MATERIALS);
    assert_eq!(cell(&g, "concrete", H_COLOR).to_string(), "#A0B0C0");
    assert_eq!(cell(&g, "wood", H_COLOR).to_string(), "#00FF00");

    // an emptied colour cell clears the colour; undo takes the whole edit back
    let q2 = tmp("mat_round_clear");
    edit(&r, &q2, |sh, g| {
        if sh == MATERIALS {
            let r = row_named(g, "concrete");
            set(g, r, H_COLOR, Data::Empty);
        }
    });
    let rep = import_xlsx(&mut m, &q2).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert_eq!(spec(&m, "concrete").color, None);
    m.undo().unwrap();
    assert_eq!(spec(&m, "concrete").color, Some(0xA0B0C0));
    m.undo().unwrap();
    assert_eq!(spec(&m, "concrete"), c, "undo restores every property");
    assert_eq!(spec(&m, "wood").color, None);
}

#[test]
fn material_rename_updates_layers() {
    let mut m = fixture();
    let brick = id_of(&m, "brick");
    let p = export(&m, "mat_ren_src");
    let q = tmp("mat_ren_dst");
    edit(&p, &q, |sh, g| {
        if sh == MATERIALS {
            let r = row_named(g, "brick");
            set(g, r, "name", s("clinker"));
        }
    });
    let before = ops(&m);
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.blocking() == 0 && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(ops(&m), before + 1, "rename and every reference in one step");
    assert_eq!(m.material("clinker").unwrap().id, brick, "the row keeps its id");
    for sql in [
        "SELECT material FROM wall_type_layer WHERE pos=0",
        "SELECT material FROM slab_type_layer WHERE pos=0",
    ] {
        assert_eq!(texts(&m, sql), vec!["clinker".to_string()], "{sql}");
    }
    assert_eq!(texts(&m, "SELECT material FROM wall_type_layer WHERE pos=1"), vec!["plaster".to_string()]);

    // the next book shows the new name on the layer sheets and in the dropdown list
    let styled = tmp("mat_ren_styled");
    export_xlsx_styled(&m, &styled, &styled, Units::Metric, &[], &Style::professional(HashMap::new()))
        .unwrap();
    for sh in [LAYERS, SLAB_LAYERS] {
        let g = grid(&styled, sh);
        // captions of a styled book follow the key: "Material"
        let c = col(&g, "Material");
        assert!(g.iter().skip(1).any(|r| r[c].to_string() == "clinker"), "{sh}");
        assert!(!g.iter().skip(1).any(|r| r[c].to_string() == "brick"), "{sh}");
    }
    let lists = grid(&styled, LISTS);
    let mats = lists[0].iter().position(|d| d.to_string() == "materials").unwrap();
    let names: Vec<String> = lists.iter().skip(1).map(|r| r[mats].to_string()).collect();
    assert!(names.iter().any(|n| n == "clinker") && !names.iter().any(|n| n == "brick"), "{names:?}");
    let hatches = lists[0].iter().position(|d| d.to_string() == "hatches").unwrap();
    assert_eq!(lists[1][hatches].to_string(), "none");

    // a layer cell edited on the Layers sheet to the new name goes through the same batch
    let q2 = tmp("mat_ren_both");
    let p2 = export(&m, "mat_ren_src2");
    edit(&p2, &q2, |sh, g| {
        if sh == MATERIALS {
            let r = row_named(g, "plaster");
            set(g, r, "name", s("lime plaster"));
        }
        if sh == LAYERS {
            let c = col(g, "material");
            for row in g.iter_mut().skip(1) {
                if row[c].to_string() == "clinker" {
                    row[c] = s("lime plaster");
                }
            }
        }
    });
    let rep = import_xlsx(&mut m, &q2).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(
        texts(&m, "SELECT material FROM wall_type_layer ORDER BY pos"),
        vec!["lime plaster".to_string(), "lime plaster".to_string()]
    );
    // one undo brings the layer cell and the rename back together
    m.undo().unwrap();
    assert_eq!(
        texts(&m, "SELECT material FROM wall_type_layer ORDER BY pos"),
        vec!["clinker".to_string(), "plaster".to_string()]
    );
    assert!(m.material("plaster").unwrap().id > 0 && m.material("lime plaster").unwrap().id == 0);
}

#[test]
fn material_bad_color_refused_per_cell() {
    let mut m = fixture();
    let before_spec = spec(&m, "concrete");
    let p = export(&m, "mat_bad_src");
    let q = tmp("mat_bad_dst");
    edit(&p, &q, |sh, g| {
        if sh == MATERIALS {
            let r = row_named(g, "concrete");
            set(g, r, H_COLOR, s("red"));
            set(g, r, "hatch", s("wavy"));
            set(g, r, H_DENSITY, Data::Float(-5.0));
            set(g, r, H_COND, s("abc"));
            set(g, r, "note", s("kept"));
            let r = row_named(g, "glass");
            set(g, r, H_COLOR, s("#12345"));
            // a new row is refused whole, with its reason
            g.push(vec![Data::Empty; g[0].len()]);
            let n = g.len() - 1;
            set(g, n, "name", s("Fresh"));
            set(g, n, H_COLOR, s("#FFFFFF"));
        }
    });
    let before = ops(&m);
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.blocking() == 0, "{rep:?}");
    let g = grid(&q, MATERIALS);
    let r = row_named(&g, "concrete") as u32 + 1;
    let why = |header: &str| {
        let c = col(&g, header) as u32 + 1;
        rep.rejected
            .iter()
            .find(|x| x.sheet == MATERIALS && x.row == r && x.col == c)
            .map(|x| x.reason.clone())
            .unwrap_or_else(|| panic!("no refusal of {header}: {:?}", rep.rejected))
    };
    assert_eq!(why(H_COLOR), "not a colour: 'red' (use #RRGGBB)");
    assert_eq!(why("hatch"), "unknown hatch 'wavy' (use none, diagonal, back_diagonal, cross, dots)");
    assert_eq!(why(H_DENSITY), "value must be 0 or more, got -5");
    assert!(why(H_COND).starts_with("not a number"));
    let glass = row_named(&g, "glass") as u32 + 1;
    assert!(rep.rejected.iter().any(|x| x.row == glass && x.reason.starts_with("not a colour")));
    let fresh = row_named(&g, "Fresh") as u32 + 1;
    let f = rep.rejected.iter().find(|x| x.row == fresh).expect("new row refused");
    assert_eq!(f.reason, "creating materials from a workbook is not supported");
    assert_eq!(m.material("Fresh").unwrap().id, 0, "no material was made");
    // refused cells keep the model value, the good cell of the same row applies
    let now = spec(&m, "concrete");
    assert_eq!(
        (now.color, now.hatch.as_str(), now.density, now.conductivity),
        (before_spec.color, before_spec.hatch.as_str(), before_spec.density, before_spec.conductivity)
    );
    assert_eq!(now.note, "kept");
    assert_eq!(spec(&m, "glass").color, None);
    assert_eq!(ops(&m), before + 1);
    assert!(
        !rep.changes.iter().any(|c| c.sheet == MATERIALS && c.column == H_COLOR),
        "{:?}",
        rep.changes
    );
}

#[test]
fn material_dup_name_refused() {
    let mut m = fixture();
    let p = export(&m, "mat_dup_src");
    let q = tmp("mat_dup_dst");
    edit(&p, &q, |sh, g| {
        if sh == MATERIALS {
            let r = row_named(g, "brick");
            set(g, r, "name", s("concrete"));
            let r = row_named(g, "wood");
            set(g, r, "name", s("Same"));
            let r = row_named(g, "steel");
            set(g, r, "name", s("Same"));
            let r = row_named(g, "glass");
            set(g, r, "name", s(""));
        }
    });
    let before = ops(&m);
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.blocking() == 0, "{rep:?}");
    // rows by the names of the exported book (the edited one renames them)
    let g = grid(&p, MATERIALS);
    let name_col = col(&g, "name") as u32 + 1;
    let refusal = |name: &str| {
        let r = row_named(&g, name) as u32 + 1;
        rep.rejected
            .iter()
            .find(|x| x.sheet == MATERIALS && x.row == r)
            .unwrap_or_else(|| panic!("no refusal on {name}: {:?}", rep.rejected))
            .clone()
    };
    let b = refusal("brick");
    assert_eq!((b.col, b.reason.as_str()), (name_col, "duplicate material name 'concrete'"));
    for n in ["wood", "steel"] {
        let x = refusal(n);
        assert_eq!((x.col, x.reason.as_str()), (name_col, "duplicate material name 'Same'"), "{n}");
    }
    assert_eq!(refusal("glass").reason, "value required");
    assert_eq!(ops(&m), before, "nothing is left to apply");
    for n in ["brick", "wood", "steel", "glass"] {
        assert!(m.material(n).unwrap().id > 0, "{n} keeps its name");
    }
    assert!(!rep.changes.iter().any(|c| c.sheet == MATERIALS), "{:?}", rep.changes);
    assert_eq!(texts(&m, "SELECT material FROM wall_type_layer WHERE pos=0"), vec!["brick".to_string()]);

    // a refused name does not block the other cells of its row
    let q2 = tmp("mat_dup_dst2");
    edit(&p, &q2, |sh, g| {
        if sh == MATERIALS {
            let r = row_named(g, "brick");
            set(g, r, "name", s("concrete"));
            set(g, r, "note", s("hand made"));
        }
    });
    let rep = import_xlsx(&mut m, &q2).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert_eq!(spec(&m, "brick").note, "hand made");
    assert_eq!(rep.rejected.len(), 1, "{:?}", rep.rejected);
}

#[test]
fn material_conflict_per_cell() {
    let mut m = fixture();
    let p = export(&m, "mat_conf_src");
    let q = tmp("mat_conf_dst");
    edit(&p, &q, |sh, g| {
        if sh == MATERIALS {
            let r = row_named(g, "concrete");
            set(g, r, H_DENSITY, Data::Float(2600.0));
            set(g, r, "note", s("book note"));
        }
    });
    // after the export the model changes the density, not the note
    let mut c = spec(&m, "concrete");
    c.density = Some(2500.0);
    m.set_material("concrete", &c).unwrap();
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied, "{rep:?}");
    let g = grid(&q, MATERIALS);
    let want = vec![(MATERIALS.to_string(), row_named(&g, "concrete") as u32 + 1, col(&g, H_DENSITY) as u32 + 1)];
    let got: Vec<(String, u32, u32)> =
        rep.conflicts.iter().map(|c| (c.sheet.clone(), c.row, c.col)).collect();
    assert_eq!(got, want, "{:?}", rep.conflicts);
    assert!(rep.conflicts.iter().all(|c| !c.take_book));
    let now = spec(&m, "concrete");
    assert_eq!((now.density, now.note.as_str()), (Some(2500.0), "book note"));

    // a re-export keeps the book value of the kept cell, so the conflict is found again
    let r = tmp("mat_conf_again");
    baseline::with_kept(&rep.kept, || export_xlsx(&m, &r)).unwrap();
    let g2 = grid(&r, MATERIALS);
    assert_eq!(num(&cell(&g2, "concrete", H_DENSITY)), 2600.0);
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
    assert_eq!(spec(&m, "concrete").density, Some(2600.0));
}

/// Book with the first wall layer and the slab layer both set to `name`.
fn layer_typo_book(m: &Model, name: &str, tag: &str) -> PathBuf {
    let p = export(m, &format!("{tag}_src"));
    let q = tmp(&format!("{tag}_dst"));
    edit(&p, &q, |sh, g| {
        if sh == LAYERS || sh == SLAB_LAYERS {
            let c = col(g, "material");
            g[1][c] = s(name);
        }
    });
    q
}

#[test]
fn unknown_layer_material_warned_not_refused() {
    let mut m = fixture();
    let q = layer_typo_book(&m, "Concret", "mat_typo");
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.rejected.is_empty() && rep.blocking() == 0, "{rep:?}");
    // still accepted as before
    assert_eq!(texts(&m, "SELECT material FROM wall_type_layer WHERE pos=0"), vec!["Concret".to_string()]);
    assert_eq!(texts(&m, "SELECT material FROM slab_type_layer WHERE pos=0"), vec!["Concret".to_string()]);
    let sheets: Vec<&str> = rep.warnings.iter().map(|w| w.sheet.as_str()).collect();
    assert_eq!(sheets, vec![LAYERS, SLAB_LAYERS], "{:?}", rep.warnings);
    for w in &rep.warnings {
        assert_eq!(w.reason, "material Concret is not in Materials: a new material will be used");
        assert!(w.col > 0 && !w.column.is_empty(), "{w:?}");
    }
}

#[test]
fn known_material_no_warning() {
    let mut m = fixture();
    // a Materials row, a material in use, and a name the same book gives a Materials row
    for (name, tag) in [("concrete", "mat_known_row"), ("plaster", "mat_known_use")] {
        let q = layer_typo_book(&m, name, tag);
        let rep = import_xlsx(&mut m, &q).unwrap();
        assert!(rep.warnings.is_empty(), "{name}: {:?}", rep.warnings);
    }
    let p = export(&m, "mat_known_ren_src");
    let q = tmp("mat_known_ren_dst");
    edit(&p, &q, |sh, g| {
        if sh == MATERIALS {
            let r = row_named(g, "wood");
            set(g, r, "name", s("timber"));
        }
        if sh == LAYERS {
            let c = col(g, "material");
            g[1][c] = s("timber");
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.warnings.is_empty(), "{:?}", rep.warnings);
}

#[test]
fn swap_refusal_names_swap() {
    let mut m = fixture();
    let p = export(&m, "mat_swap_src");
    let q = tmp("mat_swap_dst");
    edit(&p, &q, |sh, g| {
        if sh == MATERIALS {
            let (a, b) = (row_named(g, "wood"), row_named(g, "steel"));
            set(g, a, "name", s("steel"));
            set(g, b, "name", s("wood"));
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    let swap = "swapping two names is not supported: rename one to a free name first";
    let g = grid(&p, MATERIALS);
    for n in ["wood", "steel"] {
        let r = row_named(&g, n) as u32 + 1;
        let x = rep.rejected.iter().find(|x| x.sheet == MATERIALS && x.row == r).unwrap();
        assert_eq!(x.reason, swap, "{n}");
    }
    assert!(m.material("wood").unwrap().id > 0 && m.material("steel").unwrap().id > 0);

    // a plain clash keeps the old text
    let q2 = tmp("mat_swap_dup");
    edit(&p, &q2, |sh, g| {
        if sh == MATERIALS {
            let a = row_named(g, "wood");
            set(g, a, "name", s("steel"));
        }
    });
    let rep = import_xlsx(&mut m, &q2).unwrap();
    assert!(rep.rejected.iter().any(|x| x.reason == "duplicate material name 'steel'"), "{:?}", rep.rejected);
}

#[test]
fn warning_counted_in_preview() {
    let mut m = fixture();
    let q = layer_typo_book(&m, "Concret", "mat_prev");
    let before = ops(&m);
    let ask = ImportOptions { apply: false, delete_missing: false, new_params: false };
    let rep = import_xlsx_with(&mut m, &q, ask).unwrap();
    assert!(!rep.applied && ops(&m) == before);
    assert_eq!(rep.warnings.len(), 2, "{:?}", rep.warnings);
    assert_eq!(rep.warned(), 2);
    assert!(!rep.changes.is_empty() && rep.rejected.is_empty());
}

#[test]
fn library_hatch_in_use_is_in_the_dropdown() {
    let mut m = fixture();
    let mut b = spec(&m, "brick");
    b.hatch = "lib:brick".into();
    m.set_material("brick", &b).unwrap();
    let styled = tmp("mat_lib_hatch");
    export_xlsx_styled(&m, &styled, &styled, Units::Metric, &[], &Style::professional(HashMap::new()))
        .unwrap();
    let lists = grid(&styled, LISTS);
    let h = lists[0].iter().position(|d| d.to_string() == "hatches").unwrap();
    let names: Vec<String> = lists.iter().skip(1).map(|r| r[h].to_string()).collect();
    assert!(names.iter().any(|n| n == "lib:brick"), "{names:?}");
    assert!(names.iter().any(|n| n == "none"), "built-ins stay");
}

/// A material text in use without a row is listed with a blank id and the values the model
/// reads for it; the untouched book changes nothing, an edited cell gives the material a row.
#[test]
fn rowless_material_is_listed_and_round_trips() {
    let mut m = fixture();
    m.add_wall_type("T", &[layer("Tuff", 0.3)]).unwrap();
    assert_eq!(m.material("Tuff").unwrap().id, 0, "fixture: no row");
    let want = spec(&m, "Tuff");
    let p = export(&m, "mat_rowless_src");
    let g = grid(&p, MATERIALS);
    assert!(matches!(cell(&g, "Tuff", "id"), Data::Empty), "blank id");
    assert_eq!(cell(&g, "Tuff", "hatch").to_string(), want.hatch);
    for name in m.materials().unwrap().into_iter().map(|x| x.spec.name) {
        row_named(&g, &name);
    }

    // the untouched book: no change, no refusal, no note, no undo step, still no row
    let before = ops(&m);
    let rep = import_xlsx(&mut m, &p).unwrap();
    assert!(rep.applied && rep.changes.is_empty(), "{rep:?}");
    assert!(rep.rejected.is_empty(), "{rep:?}");
    assert!(rep.ignored.iter().all(|x| x.sheet != MATERIALS), "{rep:?}");
    assert_eq!(ops(&m), before);
    assert_eq!(m.material("Tuff").unwrap().id, 0, "the untouched book makes no row");

    // an edited value gives it a row, in one undo step; the layer text stays
    let q = tmp("mat_rowless_dst");
    edit(&p, &q, |sh, g| {
        if sh == MATERIALS {
            let r = row_named(g, "Tuff");
            set(g, r, H_DENSITY, Data::Float(1400.0));
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(ops(&m), before + 1);
    let t = m.material("Tuff").unwrap();
    assert!(t.id > 0, "row made by the edit");
    assert_eq!(t.spec.density, Some(1400.0));
    assert_eq!(t.spec.hatch, want.hatch);
    m.undo().unwrap();
    assert_eq!(m.material("Tuff").unwrap().id, 0, "undo takes the row back");
}

/// A blank-id row whose material got a table row after the export is skipped with a reason:
/// its values are older than the row, nothing is written.
#[test]
fn rowless_material_with_a_row_since_export_is_skipped() {
    let mut m = fixture();
    m.add_wall_type("T", &[layer("Tuff", 0.3)]).unwrap();
    let p = export(&m, "mat_rowed_src");
    let mut now = spec(&m, "Tuff");
    now.density = Some(1700.0);
    assert!(m.set_material("Tuff", &now).unwrap() > 0, "row made after the export");
    let q = tmp("mat_rowed_dst");
    edit(&p, &q, |sh, g| {
        if sh == MATERIALS {
            let r = row_named(g, "Tuff");
            set(g, r, H_DENSITY, Data::Float(1400.0));
        }
    });
    let before = ops(&m);
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.changes.iter().all(|c| c.sheet != MATERIALS), "{rep:?}");
    assert!(rep.rejected.iter().all(|x| x.sheet != MATERIALS), "{rep:?}");
    let why: Vec<&str> = rep
        .ignored
        .iter()
        .filter(|x| x.sheet == MATERIALS)
        .map(|x| x.reason.as_str())
        .collect();
    assert_eq!(why.len(), 1, "{rep:?}");
    assert!(why[0].contains("Tuff") && why[0].contains("has an id now"), "{why:?}");
    assert_eq!(ops(&m), before, "nothing written");
    assert_eq!(m.material("Tuff").unwrap().spec.density, Some(1700.0));
}

/// A row-less material renamed in the book: no material is matched or made, and the reason
/// says a blank-id row is matched by its name and which name is missing (en and ru).
#[test]
fn rowless_material_renamed_says_why() {
    let mut m = fixture();
    m.add_wall_type("T", &[layer("Tuff", 0.3)]).unwrap();
    let want = spec(&m, "Tuff");
    let p = export(&m, "mat_renamed_src");
    let q = tmp("mat_renamed_dst");
    edit(&p, &q, |sh, g| {
        if sh == MATERIALS {
            let r = row_named(g, "Tuff");
            set(g, r, "name", Data::String("Tuff stone".into()));
        }
    });
    let before = ops(&m);
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert_eq!(rep.blocking(), 0, "{rep:?}");
    let why: Vec<&str> = rep
        .rejected
        .iter()
        .filter(|x| x.sheet == MATERIALS)
        .map(|x| x.reason.as_str())
        .collect();
    assert_eq!(why.len(), 1, "{rep:?}");
    let args = super::msg::match_template(super::msg::MAT_RENAMED.1, why[0]).expect("template");
    assert_eq!(args, ["Tuff stone", "Tuff"], "{why:?}");
    assert!(!why[0].contains("not supported"), "{why:?}");
    assert_eq!(ops(&m), before, "nothing written");
    assert_eq!(m.material("Tuff").unwrap().spec, want, "the material is untouched");
    let names: Vec<String> = m.materials().unwrap().into_iter().map(|x| x.spec.name).collect();
    assert!(!names.iter().any(|n| n == "Tuff stone"), "no material was made: {names:?}");
    // the reason reads in both languages with the same substitutions
    let cat = |src: &str| -> HashMap<String, String> { serde_json::from_str(src).unwrap() };
    let en = cat(include_str!("../../../ui/data/i18n/en/xl.json"));
    let ru = cat(include_str!("../../../ui/data/i18n/ru/xl.json"));
    let (key, tpl) = super::msg::MAT_RENAMED;
    assert_eq!(en.get(key).map(String::as_str), Some(tpl), "en text is the template");
    let r = &ru[key];
    assert!(r.chars().any(|c| ('\u{400}'..='\u{4FF}').contains(&c)), "{r}");
    assert_eq!(tpl.matches("{}").count(), r.matches("{}").count(), "{r}");
}
