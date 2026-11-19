//! Drawing edit round trip on real files: open a .dwg/.dxf, move lines, edit a text, move
//! primitives to a new layer, delete some, export to DXF, read the file back and compare with
//! the export of the unedited drawing. Undo of every edit returns the drawing to its read state.
//! The synthetic tests run everywhere; the corpus tests are `--ignored`. A failure is a product
//! finding: the expectations are not loosened to fit.

mod e2e_common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use ddc_app::drawing::{read_drawing, Drawings, Origin};
use ddc_app::editor::Editor;
use ddc_geom::v2;
use ddc_io::drawing_model::{fixture, Pick, Underlay, WriteReport};
use ddc_io::dxf::DxfDocument;

type P = (f64, f64);

const TOL: f64 = 1e-5;
const NEW_LAYER: &str = "TST24-NEW";
const NEW_TEXT: &str = "TST24 edited text";
const SHIFT: P = (0.5, 0.25);

fn near(a: P, b: P) -> bool {
    (a.0 - b.0).abs() <= TOL && (a.1 - b.1).abs() <= TOL
}

fn same_seg(a: P, b: P, c: P, d: P) -> bool {
    (near(a, c) && near(b, d)) || (near(a, d) && near(b, c))
}

fn seg_count(u: &Underlay, layer: &str, a: P, b: P) -> usize {
    u.lines
        .iter()
        .filter(|l| l.layer == layer && same_seg(l.a, l.b, a, b))
        .count()
}

fn kind_counts(u: &Underlay) -> [(&'static str, usize); 5] {
    [
        ("lines", u.lines.len()),
        ("arcs", u.arcs.len()),
        ("texts", u.texts.len()),
        ("closed", u.closed.len()),
        ("fills", u.fills.len()),
    ]
}

/// Kinds that came back fewer than they were read.
fn dropped(a: &Underlay, b: &Underlay) -> Vec<String> {
    kind_counts(a)
        .iter()
        .zip(kind_counts(b).iter())
        .filter(|(x, y)| y.1 < x.1)
        .map(|(x, y)| format!("{}: {} -> {}", x.0, x.1, y.1))
        .collect()
}

type ByLayer = BTreeMap<(&'static str, String), i64>;

fn by_layer(u: &Underlay) -> ByLayer {
    let mut m: ByLayer = BTreeMap::new();
    for l in &u.lines {
        *m.entry(("line", l.layer.clone())).or_default() += 1;
    }
    for l in &u.arcs {
        *m.entry(("arc", l.layer.clone())).or_default() += 1;
    }
    for l in &u.texts {
        *m.entry(("text", l.layer.clone())).or_default() += 1;
    }
    for l in &u.closed {
        *m.entry(("closed", l.layer.clone())).or_default() += 1;
    }
    for l in &u.fills {
        *m.entry(("fill", l.layer.clone())).or_default() += 1;
    }
    m
}

fn nonzero(mut m: ByLayer) -> ByLayer {
    m.retain(|_, v| *v != 0);
    m
}

/// Export the drawings the way the app saves (model document plus the underlays), write the
/// DXF file, read it back like a reopen. Returns the write report and the reread underlay.
fn export_reread(ed: &Editor, ds: &Drawings, tag: &str) -> (WriteReport, Underlay) {
    let mut doc =
        ddc_io::dxf::export_from(&ed.model).unwrap_or_else(|e| panic!("export_from: {e}"));
    let rep = ds.export_into(&mut doc);
    let path = e2e_common::temp_file(&format!("{tag}.dxf"));
    ddc_io::dxf::write_file(&doc, &path).unwrap_or_else(|e| panic!("{tag}: write: {e}"));
    let back = read_drawing(&path).unwrap_or_else(|e| panic!("{tag}: reread: {e}"));
    let _ = std::fs::remove_file(&path);
    let mut rd = Drawings::default();
    rd.add(&back, tag, None, ed.level, None, Origin::AsIs, v2(0.0, 0.0));
    assert_eq!(rd.list.len(), 1, "{tag}: reread drawing placed");
    let d = rd.list.remove(0);
    assert!(
        (d.unit - ds.list[0].unit).abs() < 1e-12,
        "{tag}: file unit {} read back as {}",
        ds.list[0].unit,
        d.unit
    );
    (rep, d.u)
}

struct Sess {
    ed: Editor,
    ds: Drawings,
    u0: Underlay,
    /// State before edit k.
    snaps: Vec<Underlay>,
    e0: Underlay,
    rep0: WriteReport,
    /// (layer, a, b) of the lines moved by SHIFT and then to NEW_LAYER.
    mv: Vec<(String, P, P)>,
    /// (layer, a, b) of the deleted lines.
    del: Vec<(String, P, P)>,
    /// (old text, layer, position) of the edited text.
    text: Option<(String, String, P)>,
    base_undo: usize,
    start: Vec<e2e_common::TableRows>,
}

fn step(s: &mut Sess, what: &str) {
    let n = s.ed.undo_len();
    s.ds.sync_undo(&mut s.ed);
    assert_eq!(s.ed.undo_len(), n + 1, "{what}: one edit, one undo step");
    assert!(s.ds.list[0].dirty, "{what}: drawing marked edited");
}

/// Open the drawing, export the unedited one as the baseline, then edit: move lines, move them
/// to a new layer, edit a text, delete lines.
fn edit(doc: &DxfDocument, name: &str) -> Sess {
    let ed = Editor::new_in_memory().unwrap();
    let start = e2e_common::dump(&ed.model);
    let mut ds = Drawings::default();
    ds.add(doc, name, None, ed.level, None, Origin::AsIs, v2(0.0, 0.0));
    assert_eq!(ds.list.len(), 1, "{name}: drawing placed");
    let u0 = ds.list[0].u.clone();
    assert!(!ds.list[0].dirty, "{name}: fresh drawing is not edited");
    let (rep0, e0) = export_reread(&ed, &ds, "tst24_base");
    eprintln!("{name}: baseline write report {rep0:?}");
    eprintln!(
        "{name}: dropped by write and reread: {:?}; unknown kinds {:?}; skipped {}",
        dropped(&u0, &e0),
        u0.unknown,
        rep0.skipped
    );
    let base_undo = ed.undo_len();
    let mut s = Sess {
        ed,
        ds,
        u0,
        snaps: vec![],
        e0,
        rep0,
        mv: vec![],
        del: vec![],
        text: None,
        base_undo,
        start,
    };

    // lines worth moving: finite and not a point
    let cand: Vec<usize> = (0..s.u0.lines.len())
        .filter(|&i| {
            let l = &s.u0.lines[i];
            let len = ((l.b.0 - l.a.0).powi(2) + (l.b.1 - l.a.1).powi(2)).sqrt();
            len > 1e-3 && len < 1e6
        })
        .collect();
    let stride = (cand.len() / 3).max(1);
    let mv_ix: Vec<usize> = cand.iter().copied().step_by(stride).take(3).collect();
    let del_ix: Vec<usize> = cand
        .iter()
        .rev()
        .copied()
        .filter(|i| !mv_ix.contains(i))
        .take(2)
        .collect();
    let key = |u: &Underlay, i: usize| (u.lines[i].layer.clone(), u.lines[i].a, u.lines[i].b);

    // 1. move
    if !mv_ix.is_empty() {
        s.mv = mv_ix.iter().map(|&i| key(&s.u0, i)).collect();
        s.snaps.push(s.ds.list[0].u.clone());
        s.ds.pick = Some((
            0,
            Pick {
                lines: mv_ix.clone(),
                ..Default::default()
            },
        ));
        s.ds.xf = (SHIFT.0, SHIFT.1, 0.0);
        s.ds.apply_xf(false, false);
        step(&mut s, "move");
        for (k, &i) in mv_ix.iter().enumerate() {
            let l = &s.ds.list[0].u.lines[i];
            let (_, a, b) = &s.mv[k];
            assert!(near(l.a, (a.0 + SHIFT.0, a.1 + SHIFT.1)), "{name}: moved a");
            assert!(near(l.b, (b.0 + SHIFT.0, b.1 + SHIFT.1)), "{name}: moved b");
        }

        // 2. the same primitives to a new layer
        s.snaps.push(s.ds.list[0].u.clone());
        s.ds.move_pick_to_layer(NEW_LAYER)
            .unwrap_or_else(|e| panic!("{name}: move to layer: {e}"));
        step(&mut s, "layer");
        for &i in &mv_ix {
            assert_eq!(s.ds.list[0].u.lines[i].layer, NEW_LAYER, "{name}: on the new layer");
        }
    } else {
        eprintln!("{name}: no movable lines");
    }

    // 3. text: the first one that survives a write and reread as the same string
    let cand_t = s.u0.texts.iter().position(|t| {
        !t.text.trim().is_empty()
            && t.text != NEW_TEXT
            && s.e0.texts.iter().any(|x| x.text == t.text)
    });
    if let Some(ti) = cand_t {
        let t = s.u0.texts[ti].clone();
        s.snaps.push(s.ds.list[0].u.clone());
        s.ds.pick = Some((
            0,
            Pick {
                texts: vec![ti],
                ..Default::default()
            },
        ));
        s.ds.edit_text(NEW_TEXT)
            .unwrap_or_else(|e| panic!("{name}: edit_text: {e}"));
        step(&mut s, "text");
        assert_eq!(s.ds.list[0].u.texts[ti].text, NEW_TEXT);
        s.text = Some((t.text, t.layer, t.pos));
    } else {
        eprintln!("{name}: no text that survives the file");
    }

    // 4. delete
    if !del_ix.is_empty() {
        s.del = del_ix.iter().map(|&i| key(&s.u0, i)).collect();
        let n = s.ds.list[0].u.lines.len();
        s.snaps.push(s.ds.list[0].u.clone());
        s.ds.pick = Some((
            0,
            Pick {
                lines: del_ix.clone(),
                ..Default::default()
            },
        ));
        s.ds.delete_pick();
        step(&mut s, "delete");
        assert_eq!(s.ds.list[0].u.lines.len(), n - del_ix.len(), "{name}: deleted");
    } else {
        eprintln!("{name}: no lines to delete");
    }
    assert!(!s.snaps.is_empty(), "{name}: nothing could be edited");
    s
}

/// Export the edited drawing, reread it and compare with the baseline reread.
fn check_export(s: &Sess, name: &str) {
    let (rep1, e1) = export_reread(&s.ed, &s.ds, "tst24_edited");
    eprintln!("{name}: edited write report {rep1:?}");
    assert_eq!(rep1.skipped, s.rep0.skipped, "{name}: no new skipped primitives");
    let e0 = &s.e0;

    // counts per kind and layer: the baseline with the edits applied, nothing else changes
    let mut want = by_layer(e0);
    for (layer, _, _) in s.mv.iter().chain(s.del.iter()) {
        *want.entry(("line", layer.clone())).or_default() -= 1;
    }
    if !s.mv.is_empty() {
        *want.entry(("line", NEW_LAYER.to_string())).or_default() += s.mv.len() as i64;
    }
    assert_eq!(
        nonzero(by_layer(&e1)),
        nonzero(want),
        "{name}: primitive counts per kind and layer"
    );
    for (a, b) in kind_counts(e0).iter().zip(kind_counts(&e1).iter()) {
        let d = if a.0 == "lines" { s.del.len() } else { 0 };
        assert_eq!(b.1 + d, a.1, "{name}: {} count", a.0);
    }

    // moved lines: on the new layer at the shifted spot, gone from the old spot
    for (_, a, b) in &s.mv {
        let (a2, b2) = ((a.0 + SHIFT.0, a.1 + SHIFT.1), (b.0 + SHIFT.0, b.1 + SHIFT.1));
        assert!(seg_count(&e1, NEW_LAYER, a2, b2) >= 1, "{name}: moved line {a2:?} {b2:?}");
    }
    let mut keys: Vec<&(String, P, P)> = vec![];
    for k in s.mv.iter().chain(s.del.iter()) {
        if !keys.iter().any(|q| q.0 == k.0 && same_seg(q.1, q.2, k.1, k.2)) {
            keys.push(k);
        }
    }
    for (layer, a, b) in keys {
        let gone = s
            .mv
            .iter()
            .chain(s.del.iter())
            .filter(|q| q.0 == *layer && same_seg(q.1, q.2, *a, *b))
            .count();
        assert_eq!(
            seg_count(&e1, layer, *a, *b) + gone,
            seg_count(e0, layer, *a, *b),
            "{name}: old spot of {layer} {a:?} {b:?}"
        );
    }

    // text value
    if let Some((old, layer, pos)) = &s.text {
        let count = |u: &Underlay, t: &str| u.texts.iter().filter(|x| x.text == t).count();
        assert_eq!(count(&e1, NEW_TEXT), 1, "{name}: new text value");
        assert_eq!(count(&e1, old) + 1, count(e0, old), "{name}: old text value gone once");
        let t = e1.texts.iter().find(|x| x.text == NEW_TEXT).unwrap();
        assert_eq!(&t.layer, layer, "{name}: text keeps its layer");
        assert!(near(t.pos, *pos), "{name}: text keeps its position");
    }

    // layer table: the baseline plus the new layer
    let names = |u: &Underlay| u.layers.iter().map(|l| l.name.clone()).collect::<BTreeSet<_>>();
    let mut want_names = names(e0);
    if !s.mv.is_empty() {
        want_names.insert(NEW_LAYER.to_string());
    }
    assert_eq!(names(&e1), want_names, "{name}: layer table");
    for l0 in &e0.layers {
        let l1 = e1.layers.iter().find(|l| l.name == l0.name).unwrap();
        assert_eq!(
            (l1.color, l1.off, l1.frozen, &l1.linetype, l1.weight_mm),
            (l0.color, l0.off, l0.frozen, &l0.linetype, l0.weight_mm),
            "{name}: layer {} unchanged",
            l0.name
        );
    }
    if let Some((src, _, _)) = s.mv.first() {
        let n = e1.layers.iter().find(|l| l.name == NEW_LAYER).unwrap();
        let o = e1.layers.iter().find(|l| &l.name == src).unwrap();
        assert_eq!(
            (n.color, &n.linetype, n.weight_mm, n.off, n.frozen),
            (o.color, &o.linetype, o.weight_mm, false, false),
            "{name}: new layer looks like its source"
        );
    }
}

/// Undo every edit (the drawing equals the state before it each time), then redo them all.
fn check_undo(s: &mut Sess, name: &str) {
    let n = s.snaps.len();
    let end = s.ds.list[0].u.clone();
    assert_eq!(s.ed.undo_len(), s.base_undo + n, "{name}: edits on the stack");
    for j in 0..n {
        s.ed.undo();
        s.ds.sync_undo(&mut s.ed);
        assert!(
            s.ds.list[0].u == s.snaps[n - 1 - j],
            "{name}: undo {} returns the state before that edit",
            n - j
        );
    }
    let d = &s.ds.list[0];
    assert!(d.u == s.u0, "{name}: all undone equals the state as read");
    assert_eq!(d.offset, v2(0.0, 0.0), "{name}: placement back");
    assert!(!d.dirty, "{name}: undone drawing is not edited");
    assert_eq!(s.ed.undo_len(), s.base_undo, "{name}: stack back");
    e2e_common::assert_same(&e2e_common::dump(&s.ed.model), &s.start, "model after undo all");
    for j in 0..n {
        s.ed.redo();
        s.ds.sync_undo(&mut s.ed);
        let want = if j + 1 < n { &s.snaps[j + 1] } else { &end };
        assert!(s.ds.list[0].u == *want, "{name}: redo {} returns the edited state", j + 1);
    }
    assert!(s.ds.list[0].dirty, "{name}: redone drawing is edited");
}

fn full(doc: &DxfDocument, name: &str) {
    let mut s = edit(doc, name);
    check_export(&s, name);
    check_undo(&mut s, name);
}

fn sample_through_file() -> DxfDocument {
    let path = e2e_common::temp_file("tst24_sample.dxf");
    ddc_io::dxf::write_file(&fixture::sample_plan(0), &path).unwrap();
    let doc = read_drawing(&path).unwrap_or_else(|e| panic!("read_drawing: {e}"));
    let _ = std::fs::remove_file(&path);
    doc
}

#[test]
fn dwg_edit_export_reread_synthetic() {
    let doc = sample_through_file();
    let s = edit(&doc, "sample_plan");
    assert!(!s.mv.is_empty() && !s.del.is_empty() && s.text.is_some(), "all four edits ran");
    assert_eq!(s.snaps.len(), 4);
    check_export(&s, "sample_plan");
}

#[test]
fn dwg_edit_undo_all_restores() {
    let doc = sample_through_file();
    let mut s = edit(&doc, "sample_plan");
    assert_eq!(s.snaps.len(), 4);
    check_undo(&mut s, "sample_plan");
}

fn run_file(path: &Path) {
    let is_dwg = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("dwg"));
    if is_dwg && !ddc_io::dxf::DWG_SUPPORTED {
        eprintln!("e2e: native .dwg reader not in this build, skipped");
        return;
    }
    let doc = read_drawing(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    full(&doc, &path.display().to_string());
}

#[test]
#[ignore = "corpus"]
fn dwg_edit_corpus_dwg() {
    // the drawing lives in the private corpus, which is not part of this repository
    let Some(path) = e2e_common::corpus("DDC_E2E_DWG", "") else {
        return;
    };
    run_file(&path);
}

// no .dxf sample in the corpus list: without DDC_E2E_DXF the second floor of the built-in
// sample plan stands in, so the test still runs the whole chain
#[test]
#[ignore = "corpus"]
fn dwg_edit_corpus_dxf() {
    match e2e_common::corpus("DDC_E2E_DXF", "") {
        Some(path) => run_file(&path),
        None => {
            let path = e2e_common::temp_file("tst24_sample_v1.dxf");
            ddc_io::dxf::write_file(&fixture::sample_plan(1), &path).unwrap();
            run_file(&path);
            let _ = std::fs::remove_file(&path);
        }
    }
}
