// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Workflow: tags, dimensions, text notes and detail lines survive an XLSX import and a save.
//! Scene, writer and preview helpers follow `e2e_xlsx.rs`. A failure here is a product finding,
//! the tests assert the contract and nothing is bent to pass.

use crate::e2e_common;

use std::path::Path;
use std::sync::Arc;

use ddc_app::editor::{Editor, OpFn};
use ddc_io::xlsx::{self, ImportOptions, Report, TableCell as C};
use ddc_model::{
    AnnoShape, AnnoView, DimKind, DimRef, DimSpec, Model, OpeningSpec, RoomSpec, TagCat, TextNote,
    WallSpec,
};
use e2e_common::{assert_clean, assert_same, count, dump_data, ids, temp_file};

const TOL: f64 = 1e-9;

fn op(
    ed: &mut Editor,
    f: impl Fn(&mut Model) -> Result<(), ddc_model::Error> + Send + Sync + 'static,
) {
    ed.run_op(OpFn(Arc::new(f))).unwrap();
    assert_clean(&ed.model, "after op");
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < TOL
}

fn near2(a: (f64, f64), b: (f64, f64)) -> bool {
    near(a.0, b.0) && near(a.1, b.1)
}

/// 6x4 box of four walls (ids 1..4) with a door on wall 2, a window on wall 1, a void on
/// wall 4, and one room inside.
fn scene() -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    let (l, t) = (ed.level, ed.types[1].id);
    let corners = [
        ((0.0, 0.0), (6.0, 0.0)),
        ((6.0, 0.0), (6.0, 4.0)),
        ((6.0, 4.0), (0.0, 4.0)),
        ((0.0, 4.0), (0.0, 0.0)),
    ];
    for (a, b) in corners {
        op(&mut ed, move |m| {
            m.add_wall(&WallSpec {
                a,
                b,
                level: l,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .map(drop)
        });
    }
    let w = ids(&ed.model, "wall");
    assert_eq!(w.len(), 4);
    let holes = [
        (w[1], 1.0, 0.9, 0.0, 2.1, "door"),
        (w[0], 2.0, 1.2, 0.9, 1.2, "window"),
        (w[3], 1.0, 1.0, 0.0, 2.0, "void"),
    ];
    for (wall, along, width, sill, height, kind) in holes {
        op(&mut ed, move |m| {
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
            .map(drop)
        });
    }
    op(&mut ed, move |m| {
        m.add_room(&RoomSpec {
            level: l,
            name: "Kitchen".into(),
            number: "101".into(),
            point: (3.0, 2.0),
            height: 2.7,
        })
        .map(drop)
    });
    ed
}

/// `scene()` plus: tags on every wall, the door, the window and the room (7), a linear
/// dimension on wall 1 (length), an angular one between walls 1 and 2, a text note and a
/// detail line on the plan.
fn annotated() -> Editor {
    let mut ed = scene();
    let l = ed.level;
    let w = ids(&ed.model, "wall");
    let (w0, w1) = (w[0], w[1]);
    op(&mut ed, move |m| {
        m.add_dim(&DimSpec {
            level: l,
            kind: DimKind::Linear,
            refs: vec![
                DimRef::AxisEnd { wall: w0, end: 0 },
                DimRef::AxisEnd { wall: w0, end: 1 },
            ],
            dir: (0.0, 0.0),
            side: -1.0,
            offset: 1.0,
            tier: -1,
            auto: false,
        })
        .map(drop)
    });
    op(&mut ed, move |m| {
        m.add_dim(&DimSpec {
            level: l,
            kind: DimKind::Angular,
            refs: vec![DimRef::AxisLine { wall: w0 }, DimRef::AxisLine { wall: w1 }],
            dir: (0.0, 0.0),
            side: 1.0,
            offset: 0.8,
            tier: -1,
            auto: false,
        })
        .map(drop)
    });
    op(&mut ed, move |m| {
        m.tag_all(
            l,
            &[TagCat::Door, TagCat::Window, TagCat::Room, TagCat::Wall],
            false,
        )
        .map(drop)
    });
    op(&mut ed, move |m| {
        m.add_anno(
            AnnoView::Plan(l),
            &AnnoShape::Text(TextNote::new((1.0, 3.0), "Slab edge")),
            None,
            None,
        )
        .map(drop)
    });
    op(&mut ed, move |m| {
        m.add_anno(
            AnnoView::Plan(l),
            &AnnoShape::Line {
                pts: vec![(1.0, 1.0), (2.5, 1.5)],
                closed: false,
            },
            None,
            None,
        )
        .map(drop)
    });
    let m = &ed.model;
    assert_eq!(m.tags(l).unwrap().len(), 7);
    assert_eq!(m.dims(l).unwrap().len(), 2);
    assert_eq!(m.annos(AnnoView::Plan(l)).unwrap().len(), 2);
    ed
}

fn walls_rows(m: &Model) -> Vec<Vec<C>> {
    let mut st = m
        .sql()
        .prepare(
            "SELECT w.id,lv.name,t.name,w.ax,w.ay,w.bx,w.by,w.base_offset,w.height FROM wall w \
             JOIN level lv ON lv.id=w.level JOIN wall_type t ON t.id=w.wtype ORDER BY w.id",
        )
        .unwrap();
    let rows: Vec<Vec<C>> = st
        .query_map([], |r| {
            let mut v = vec![C::Num(r.get::<_, i64>(0)? as f64)];
            for i in 1..9 {
                match r.get::<_, rusqlite::types::Value>(i)? {
                    rusqlite::types::Value::Text(s) => v.push(C::Text(s)),
                    rusqlite::types::Value::Integer(x) => v.push(C::Num(x as f64)),
                    rusqlite::types::Value::Real(x) => v.push(C::Num(x)),
                    _ => v.push(C::Text(String::new())),
                }
            }
            Ok(v)
        })
        .unwrap()
        .map(|x| x.unwrap())
        .collect();
    rows
}

const WALL_HEADS: [&str; 9] = [
    "id",
    "level",
    "type",
    "ax [m]",
    "ay [m]",
    "bx [m]",
    "by [m]",
    "base_offset [m]",
    "height [m]",
];

fn write_walls(path: &Path, rows: &[Vec<C>]) {
    let h: Vec<String> = WALL_HEADS.iter().map(|s| s.to_string()).collect();
    xlsx::export_table(path, "Walls", &h, &[], rows, None).unwrap();
}

/// Walls book whose first row is marked "delete": a hand-made book has no baseline, so a
/// row removed from it is never a deletion; the explicit mark still is.
fn write_walls_deleting_first(path: &Path, rows: &[Vec<C>]) {
    let mut h: Vec<String> = WALL_HEADS.iter().map(|s| s.to_string()).collect();
    h.push("delete".into());
    let rows: Vec<Vec<C>> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let mut r = r.clone();
            r.push(C::Text(if i == 0 { "delete".into() } else { String::new() }));
            r
        })
        .collect();
    xlsx::export_table(path, "Walls", &h, &[], &rows, None).unwrap();
}

/// Preview only: the model is not touched.
fn preview(m: &mut Model, p: &Path, delete_missing: bool) -> Report {
    let o = ImportOptions {
        apply: false,
        delete_missing,
        new_params: false,
    };
    xlsx::import_xlsx_with(m, p, o).unwrap()
}

/// Walls book where wall 1 is 7 m long (bx 6 -> 7) and wall 2 is moved and tilted
/// (start 6,0 -> 7,0; end 6,4 -> 8,4).
fn edited_book(m: &Model, name: &str) -> std::path::PathBuf {
    let mut rows = walls_rows(m);
    rows[0][5] = C::Num(7.0);
    rows[1][3] = C::Num(7.0);
    rows[1][5] = C::Num(8.0);
    let p = temp_file(name);
    write_walls(&p, &rows);
    p
}

fn apply_edit_book(ed: &mut Editor, name: &str) {
    let p = edited_book(&ed.model, name);
    let rep = preview(&mut ed.model, &p, false);
    assert!(rep.rejected.is_empty(), "{:?}", rep.rejected);
    assert_eq!(rep.counts().changed, 2, "{:?}", rep.changes);
    ed.apply_xlsx(rep.batch(), "walls").unwrap();
    assert_clean(&ed.model, "xlsx applied");
}

/// Splits a debug text into its shape (numbers replaced by `#`) and the numbers in order,
/// so two dumps compare exactly in shape and to 1e-9 in value.
fn split_nums(s: &str) -> (String, Vec<f64>) {
    let b: Vec<char> = s.chars().collect();
    let (mut out, mut nums, mut i) = (String::new(), vec![], 0);
    while i < b.len() {
        let c = b[i];
        let after_word = i > 0 && (b[i - 1].is_alphanumeric() || b[i - 1] == '_');
        let starts = !after_word
            && (c.is_ascii_digit() || (c == '-' && i + 1 < b.len() && b[i + 1].is_ascii_digit()));
        if starts {
            let mut j = i + 1;
            while j < b.len()
                && (b[j].is_ascii_digit()
                    || b[j] == '.'
                    || b[j] == 'e'
                    || b[j] == 'E'
                    || ((b[j] == '-' || b[j] == '+') && (b[j - 1] == 'e' || b[j - 1] == 'E')))
            {
                j += 1;
            }
            let tok: String = b[i..j].iter().collect();
            if let Ok(v) = tok.parse::<f64>() {
                nums.push(v);
                out.push('#');
                i = j;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    (out, nums)
}

type Fp = (String, Vec<f64>);

fn fp_tags(m: &Model, l: i64) -> Fp {
    split_nums(&format!("{:?}", m.tags(l).unwrap()))
}

fn fp_dims(m: &Model, l: i64) -> Fp {
    split_nums(&format!("{:?}", m.dims(l).unwrap()))
}

fn fp_notes(m: &Model, l: i64) -> Fp {
    split_nums(&format!("{:?}", m.annos(AnnoView::Plan(l)).unwrap()))
}

fn same_fp(a: &Fp, b: &Fp, ctx: &str) {
    assert_eq!(a.0, b.0, "{ctx}: shape differs");
    assert_eq!(a.1.len(), b.1.len(), "{ctx}: number count differs");
    for (k, (x, y)) in a.1.iter().zip(&b.1).enumerate() {
        assert!(near(*x, *y), "{ctx}: number {k}: {x} vs {y}");
    }
}

/// Tag rows whose host row no longer exists.
fn stray_tags(m: &Model) -> Vec<i64> {
    let mut st = m
        .sql()
        .prepare("SELECT id,refs FROM dim WHERE kind='tag' ORDER BY id")
        .unwrap();
    let rows: Vec<(i64, String)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(|x| x.unwrap())
        .collect();
    let mut out = vec![];
    for (id, refs) in rows {
        let p: Vec<&str> = refs.split(' ').collect();
        let table = match p.get(1).copied() {
            Some("w") => "wall",
            Some("o") => "opening",
            Some("r") => "room",
            _ => "comp",
        };
        let host: i64 = p.get(2).and_then(|s| s.parse().ok()).unwrap_or(-1);
        let n: i64 = m
            .sql()
            .query_row(
                &format!("SELECT count(*) FROM {table} WHERE id={host}"),
                [],
                |r| r.get(0),
            )
            .unwrap();
        if n == 0 {
            out.push(id);
        }
    }
    out
}

/// (dimension id, orphan flag) of every non-tag dimension.
fn dim_flags(m: &Model) -> Vec<(i64, i64)> {
    let mut st = m
        .sql()
        .prepare(
            "SELECT d.id,COALESCE(g.orphan,-1) FROM dim d LEFT JOIN dim_geo g ON g.dim=d.id \
             WHERE d.kind<>'tag' ORDER BY d.id",
        )
        .unwrap();
    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(|x| x.unwrap())
        .collect()
}

#[test]
fn xlsx_edit_tags_follow_dims_update() {
    let mut ed = annotated();
    let l = ed.level;
    let w = ids(&ed.model, "wall");
    let tag_ids: Vec<i64> = ed.model.tags(l).unwrap().iter().map(|t| t.id).collect();
    let dim_ids: Vec<i64> = ed.model.dims(l).unwrap().iter().map(|d| d.id).collect();
    let tags0 = fp_tags(&ed.model, l);
    let notes0 = fp_notes(&ed.model, l);

    apply_edit_book(&mut ed, "annot_edit.xlsx");

    // the same rows still exist, none is orphaned or strays
    let m = &ed.model;
    let tags = m.tags(l).unwrap();
    assert_eq!(tags.iter().map(|t| t.id).collect::<Vec<_>>(), tag_ids);
    assert!(stray_tags(m).is_empty(), "{:?}", stray_tags(m));
    let dims = m.dims(l).unwrap();
    assert_eq!(dims.iter().map(|d| d.id).collect::<Vec<_>>(), dim_ids);
    assert!(dims.iter().all(|d| !d.orphan));
    assert_eq!(dim_flags(m), dim_ids.iter().map(|i| (*i, 0)).collect::<Vec<_>>());
    assert_eq!(count(m, "dim"), (tag_ids.len() + dim_ids.len()) as i64);

    // dimensions read the new geometry
    assert!(near(dims[0].geom.value(), 7.0), "{:?}", dims[0].geom);
    let want_angle = 4f64.atan2(1.0).to_degrees();
    assert!(
        (dims[1].geom.value() - want_angle).abs() < 1e-7,
        "{:?}",
        dims[1].geom
    );

    // tags follow their hosts and keep their offsets
    for t in &tags {
        let head = (t.anchor.0 + t.spec.offset.0, t.anchor.1 + t.spec.offset.1);
        assert!(near2(t.head, head), "tag {} head", t.id);
    }
    let anchor_of = |host| tags.iter().find(|t| t.spec.host == host).unwrap().anchor;
    assert!(near2(anchor_of(ddc_model::TagHost::Wall(w[0])), (3.5, 0.0)));
    assert!(near2(anchor_of(ddc_model::TagHost::Wall(w[1])), (7.5, 2.0)));
    let window = ids(m, "opening")[1];
    assert!(near2(anchor_of(ddc_model::TagHost::Opening(window)), (2.6, 0.0)));

    // the same edit made directly on a twin model gives the same annotations
    let mut twin = annotated();
    let (w0, w1) = (w[0], w[1]);
    twin.model.set_wall_axis(w0, (0.0, 0.0), (7.0, 0.0)).unwrap();
    twin.model.set_wall_axis(w1, (7.0, 0.0), (8.0, 4.0)).unwrap();
    same_fp(&fp_tags(m, l), &fp_tags(&twin.model, l), "tags vs twin");
    same_fp(&fp_dims(m, l), &fp_dims(&twin.model, l), "dims vs twin");

    // the room row is untouched, so is its tag: same place, number and name. Its area is
    // measured from the walls, and the moved wall 2 no longer meets wall 3, so the area is
    // whatever the same walls give on the twin
    let room_tags = |m: &Model| -> Vec<ddc_model::Tag> {
        m.tags(l).unwrap().into_iter().filter(|t| t.cat == TagCat::Room).collect()
    };
    let no_area = |m: &Model| {
        let t: Vec<_> = room_tags(m)
            .into_iter()
            .map(|mut t| {
                t.text.area = None;
                t
            })
            .collect();
        split_nums(&format!("{t:?}"))
    };
    let before = annotated();
    assert_eq!(room_tags(m).len(), 1);
    same_fp(&no_area(m), &no_area(&before.model), "room tag");
    let area = |m: &Model| room_tags(m)[0].text.area;
    assert_eq!(area(m), area(&twin.model), "room tag area vs twin");
    same_fp(&fp_notes(m, l), &notes0, "notes");
    assert_ne!(fp_tags(m, l).1, tags0.1, "tags did move");
}

/// Number of report rows (changes plus ignored edits) that speak about tags, dimensions or
/// annotations.
fn annotation_notes(rep: &Report) -> usize {
    let hit = |s: &str| {
        let s = s.to_lowercase();
        s.contains("tag") || s.contains("dimension") || s.contains("annotation")
    };
    rep.changes
        .iter()
        .filter(|c| hit(&c.sheet) || hit(&c.column) || hit(&c.new))
        .count()
        + rep
            .ignored
            .iter()
            .filter(|i| hit(&i.sheet) || hit(&i.column) || hit(&i.reason))
            .count()
}

#[test]
fn xlsx_delete_missing_reports_annotations() {
    let mut ed = annotated();
    let l = ed.level;
    let w = ids(&ed.model, "wall");
    let notes0 = fp_notes(&ed.model, l);
    let state0 = dump_data(&ed.model);

    // wall 1 carries a wall tag, the window tag and both dimensions
    let rows = walls_rows(&ed.model);
    let p = temp_file("annot_delete_missing.xlsx");
    write_walls_deleting_first(&p, &rows);
    let rep = preview(&mut ed.model, &p, true);
    assert!(rep.rejected.is_empty(), "{:?}", rep.rejected);
    assert_eq!(rep.counts().deleted, 1, "{:?}", rep.changes);
    assert_same(&dump_data(&ed.model), &state0, "preview changes nothing");
    // the report counts every annotation the deletion takes or orphans: 2 tags + 2 dimensions
    assert_eq!(
        annotation_notes(&rep),
        4,
        "changes {:?} ignored {:?}",
        rep.changes,
        rep.ignored
    );

    ed.apply_xlsx(rep.batch(), "walls").unwrap();
    assert_clean(&ed.model, "delete applied");
    let m = &ed.model;
    assert_eq!(count(m, "wall"), 3);

    // no tag row of the deleted wall or its window survives, none is left pointing nowhere
    assert!(stray_tags(m).is_empty(), "stray tags {:?}", stray_tags(m));
    let tags = m.tags(l).unwrap();
    assert_eq!(tags.len(), 5);
    assert!(tags
        .iter()
        .all(|t| t.spec.host != ddc_model::TagHost::Wall(w[0])));
    // both dimensions refer to the deleted wall: removed or marked orphan, never live
    let flags = dim_flags(m);
    assert!(
        flags.iter().all(|(_, o)| *o == 1),
        "dimensions of a deleted wall stay live: {flags:?}"
    );
    assert!(m.dims(l).unwrap().iter().all(|d| d.orphan));

    // text note and detail line are not tied to walls
    same_fp(&fp_notes(m, l), &notes0, "notes");
    assert_eq!(m.annos(AnnoView::Plan(l)).unwrap().len(), 2);
}

#[test]
fn xlsx_apply_undo_restores_annotations() {
    let mut ed = annotated();
    let l = ed.level;
    let state0 = dump_data(&ed.model);
    let (tags0, dims0, notes0) = (
        fp_tags(&ed.model, l),
        fp_dims(&ed.model, l),
        fp_notes(&ed.model, l),
    );

    let check_back = |ed: &Editor, ctx: &str| {
        assert_clean(&ed.model, ctx);
        assert_same(&dump_data(&ed.model), &state0, ctx);
        same_fp(&fp_tags(&ed.model, l), &tags0, ctx);
        same_fp(&fp_dims(&ed.model, l), &dims0, ctx);
        same_fp(&fp_notes(&ed.model, l), &notes0, ctx);
    };

    // edit book: one undo step
    apply_edit_book(&mut ed, "annot_undo_edit.xlsx");
    let after_edit = dump_data(&ed.model);
    assert_ne!(fp_dims(&ed.model, l).1, dims0.1);
    ed.undo();
    check_back(&ed, "undo of the edit book");
    ed.redo();
    assert_same(&dump_data(&ed.model), &after_edit, "redo of the edit book");
    ed.undo();
    check_back(&ed, "second undo of the edit book");

    // delete-missing book: the orphan geometry of the dimensions comes back too
    let rows = walls_rows(&ed.model);
    let p = temp_file("annot_undo_delete.xlsx");
    write_walls_deleting_first(&p, &rows);
    let rep = preview(&mut ed.model, &p, true);
    ed.apply_xlsx(rep.batch(), "walls").unwrap();
    let after_delete = dump_data(&ed.model);
    assert_eq!(count(&ed.model, "wall"), 3);
    ed.undo();
    check_back(&ed, "undo of the delete book");
    assert!(dim_flags(&ed.model).iter().all(|(_, o)| *o == 0));
    assert!(stray_tags(&ed.model).is_empty());
    ed.redo();
    assert_same(&dump_data(&ed.model), &after_delete, "redo of the delete book");
}

fn cleanup(p: &Path) {
    for ext in ["", "-wal", "-shm"] {
        let mut s = p.as_os_str().to_owned();
        s.push(ext);
        let _ = std::fs::remove_file(s);
    }
}

#[test]
fn save_reopen_keeps_annotations() {
    let mut ed = annotated();
    let l = ed.level;

    // as built, then after an XLSX edit
    for (round, edit) in [(0, false), (1, true)] {
        if edit {
            apply_edit_book(&mut ed, "annot_save_edit.xlsx");
        }
        let want = dump_data(&ed.model);
        let (tags, dims, notes) = (
            fp_tags(&ed.model, l),
            fp_dims(&ed.model, l),
            fp_notes(&ed.model, l),
        );
        let p = temp_file(&format!("annot_reopen_{round}.ddc"));
        ed.model.snapshot_to(&p).unwrap();
        let r = Model::open(p.to_str().unwrap()).unwrap();

        assert_same(&dump_data(&r), &want, "reopened data");
        assert_clean(&r, "reopened");
        assert_eq!(r.tags(l).unwrap().len(), 7);
        assert_eq!(r.dims(l).unwrap().len(), 2);
        assert_eq!(r.annos(AnnoView::Plan(l)).unwrap().len(), 2);
        assert_eq!(count(&r, "anno"), 2);
        same_fp(&fp_tags(&r, l), &tags, "tags");
        same_fp(&fp_dims(&r, l), &dims, "dims");
        same_fp(&fp_notes(&r, l), &notes, "notes");

        // the text and the positions, read back one by one (annos come in draw order, text
        // last, so each is picked by its shape)
        let annos = r.annos(AnnoView::Plan(l)).unwrap();
        let texts: Vec<_> = annos
            .iter()
            .filter_map(|a| match &a.shape {
                AnnoShape::Text(n) => Some(n),
                _ => None,
            })
            .collect();
        assert_eq!(texts.len(), 1, "{annos:?}");
        assert_eq!(texts[0].text, "Slab edge");
        assert!(near2(texts[0].pos, (1.0, 3.0)));
        let lines: Vec<_> = annos
            .iter()
            .filter_map(|a| match &a.shape {
                AnnoShape::Line { pts, closed } => Some((pts, *closed)),
                _ => None,
            })
            .collect();
        assert_eq!(lines.len(), 1, "{annos:?}");
        let (pts, closed) = lines[0];
        assert!(!closed);
        assert_eq!(pts.len(), 2);
        assert!(near2(pts[0], (1.0, 1.0)) && near2(pts[1], (2.5, 1.5)));
        let room = r
            .tags(l)
            .unwrap()
            .into_iter()
            .find(|t| t.cat == TagCat::Room)
            .unwrap();
        assert_eq!(room.text.name, "Kitchen");
        drop(r);
        cleanup(&p);
    }
}
