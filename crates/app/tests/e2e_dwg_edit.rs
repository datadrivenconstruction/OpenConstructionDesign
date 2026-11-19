//! Drawing edit round trip: an opened .dxf/.dwg drawing is edited (move, rotate and copy picked
//! primitives, delete one), every gesture undone and redone, then written with `export_into`
//! and read back with `read_drawing`. The light tests run on the built-in sample plan plus one
//! block with attributes; the file-level checks are the same ones a corpus drawing would face.

mod e2e_common;

use ddc_app::drawing::{read_drawing, Drawing, Drawings, Origin};
use ddc_app::editor::Editor;
use ddc_geom::{v2, V2};
use ddc_io::drawing_model::{Pick, Underlay};
use ddc_io::dxf::{
    self, AttDef, Attrib, Block, Circle, Common, DxfDocument, Entity, Insert, Layer, Pair,
};

const TOL: f64 = 1e-6;
const SHIFT_X: f64 = 1.5; // metres, the move gesture
const SHIFT_Y: f64 = 2.0; // metres, the copy gesture

type Seg = [f64; 4];
type Snap = Vec<(u64, Underlay, V2, f64)>;

/// The sample plan plus a block with attribute values, all in file units (mm).
fn source_doc() -> DxfDocument {
    let mut d = ddc_io::drawing_model::fixture::sample_plan(0);
    d.layers.push(Layer {
        name: "A-TAG".into(),
        flags: 0,
        color: 5,
        linetype: "CONTINUOUS".into(),
        extra: vec![Pair::new(370, "35")],
    });
    let mut tag = Block::new("TAG");
    for (k, t) in ["NO", "NAME", "HIDDEN"].iter().enumerate() {
        tag.attdefs.push(AttDef {
            common: Common::on_layer("0"),
            tag: (*t).into(),
            prompt: (*t).into(),
            default: "-".into(),
            pos: [0.0, -100.0 * (k as f64 + 1.0), 0.0],
            height: 50.0,
            flags: i32::from(k == 2),
        });
    }
    tag.entities = vec![Entity::Circle(Circle {
        common: Common::on_layer("0"),
        center: [0.0; 3],
        radius: 80.0,
    })];
    d.blocks.push(tag);
    let attrib = |t: &str, v: &str, pos: [f64; 3], flags: i32, extra: Vec<Pair>| {
        let mut common = Common::on_layer("A-TAG");
        common.extra = extra;
        Attrib {
            common,
            tag: t.into(),
            value: v.into(),
            pos,
            height: 50.0,
            flags,
        }
    };
    d.entities.push(Entity::Insert(Insert {
        common: Common::on_layer("A-TAG"),
        block: "TAG".into(),
        pos: [14500.0, 3000.0, 0.0],
        scale: [1.0, 1.0, 1.0],
        rotation: 10.0,
        attribs: vec![
            attrib("NO", "101", [14517.4, 2901.5, 0.0], 0, vec![]),
            attrib(
                "NAME",
                "Office 2",
                [14400.0, 2800.0, 0.0],
                0,
                vec![Pair::new(50, "15.0")],
            ),
            attrib("HIDDEN", "x", [14500.0, 2700.0, 0.0], 1, vec![]),
        ],
    }));
    d
}

fn snap(ds: &Drawings) -> Snap {
    ds.list
        .iter()
        .map(|d| (d.id, d.u.clone(), d.offset, d.rot))
        .collect()
}

/// File units of a model point (the drawing is placed without a turn).
fn file(d: &Drawing, q: (f64, f64)) -> (f64, f64) {
    ((q.0 - d.offset.x) / d.unit, (q.1 - d.offset.y) / d.unit)
}

fn seg(d: &Drawing, i: usize) -> Seg {
    let l = &d.u.lines[i];
    let (a, b) = (file(d, l.a), file(d, l.b));
    [a.0, a.1, b.0, b.1]
}

fn same_seg(x: &Seg, y: &Seg) -> bool {
    let n = |a: f64, b: f64| (a - b).abs() < TOL;
    (n(x[0], y[0]) && n(x[1], y[1]) && n(x[2], y[2]) && n(x[3], y[3]))
        || (n(x[0], y[2]) && n(x[1], y[3]) && n(x[2], y[0]) && n(x[3], y[1]))
}

fn shifted(s: Seg, dx: f64, dy: f64) -> Seg {
    [s[0] + dx, s[1] + dy, s[2] + dx, s[3] + dy]
}

fn has(set: &[Seg], s: &Seg) -> bool {
    set.iter().any(|x| same_seg(x, s))
}

/// Same segments, any order, each used once.
fn same_set(a: &[Seg], b: &[Seg]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut left: Vec<Seg> = b.to_vec();
    for x in a {
        match left.iter().position(|y| same_seg(x, y)) {
            Some(i) => {
                left.remove(i);
            }
            None => return false,
        }
    }
    true
}

/// Quarter turn about the middle: same middle and length, perpendicular.
fn is_quarter_turn(orig: &Seg, got: &Seg) -> bool {
    let mid = |s: &Seg| ((s[0] + s[2]) / 2.0, (s[1] + s[3]) / 2.0);
    let (m0, m1) = (mid(orig), mid(got));
    let d0 = (orig[2] - orig[0], orig[3] - orig[1]);
    let d1 = (got[2] - got[0], got[3] - got[1]);
    (m0.0 - m1.0).abs() < TOL
        && (m0.1 - m1.1).abs() < TOL
        && (d0.0.hypot(d0.1) - d1.0.hypot(d1.1)).abs() < TOL
        && (d0.0 * d1.0 + d0.1 * d1.1).abs() < TOL
}

// ---- entity keys: a document as a sorted multiset of strings ----

fn r(v: f64) -> String {
    format!("{:.6}", (v * 1e6).round() / 1e6 + 0.0)
}

fn ang(v: f64) -> String {
    let a = v.rem_euclid(360.0);
    r(if 360.0 - a < 1e-7 { 0.0 } else { a })
}

fn pen_full(c: &Common) -> String {
    format!(
        "{}|{:?}|{:?}|{:?}|{:?}",
        c.layer, c.linetype, c.color, c.true_color, c.lineweight
    )
}

fn pen_layer(c: &Common) -> String {
    c.layer.clone()
}

fn lkey(pen: &str, a: (f64, f64), b: (f64, f64)) -> String {
    let (mut p, mut q) = ((r(a.0), r(a.1)), (r(b.0), r(b.1)));
    if q < p {
        std::mem::swap(&mut p, &mut q);
    }
    format!("L|{pen}|{},{}|{},{}", p.0, p.1, q.0, q.1)
}

/// `(layer, key)` of an entity kind that has a stable form; `None` for the rest.
fn describe(e: &Entity, pen: &dyn Fn(&Common) -> String) -> Option<(String, String)> {
    match e {
        Entity::Line(l) => Some((
            l.common.layer.clone(),
            lkey(
                &pen(&l.common),
                (l.start[0], l.start[1]),
                (l.end[0], l.end[1]),
            ),
        )),
        Entity::Circle(c) => Some((
            c.common.layer.clone(),
            format!(
                "C|{}|{},{}|{}",
                pen(&c.common),
                r(c.center[0]),
                r(c.center[1]),
                r(c.radius)
            ),
        )),
        Entity::Arc(a) => Some((
            a.common.layer.clone(),
            format!(
                "A|{}|{},{}|{}|{}|{}",
                pen(&a.common),
                r(a.center[0]),
                r(a.center[1]),
                r(a.radius),
                ang(a.start_angle),
                ang(a.end_angle)
            ),
        )),
        Entity::LwPolyline(p) => {
            let mut v: Vec<String> = p
                .vertices
                .iter()
                .map(|q| format!("{},{}", r(q.x), r(q.y)))
                .collect();
            v.sort();
            Some((
                p.common.layer.clone(),
                format!("P|{}|{}|{}", pen(&p.common), p.closed(), v.join(";")),
            ))
        }
        Entity::Insert(i) => {
            let at: Vec<String> = i
                .attribs
                .iter()
                .map(|a| {
                    format!(
                        "{}={}/{}/{},{}/{:?}",
                        a.tag,
                        a.value,
                        a.flags,
                        r(a.pos[0]),
                        r(a.pos[1]),
                        a.common.extra
                    )
                })
                .collect();
            Some((
                i.common.layer.clone(),
                format!(
                    "I|{}|{}|{},{}|{},{},{}|{}|{:?}|{}",
                    pen(&i.common),
                    i.block,
                    r(i.pos[0]),
                    r(i.pos[1]),
                    r(i.scale[0]),
                    r(i.scale[1]),
                    r(i.scale[2]),
                    ang(i.rotation),
                    i.common.extra,
                    at.join(";")
                ),
            ))
        }
        Entity::Text(t) => Some((
            t.common.layer.clone(),
            format!(
                "T|{}|{},{}|{}|{}",
                pen(&t.common),
                r(t.pos[0]),
                r(t.pos[1]),
                r(t.height),
                t.text
            ),
        )),
        Entity::MText(t) => Some((
            t.common.layer.clone(),
            format!(
                "M|{}|{},{}|{}|{}",
                pen(&t.common),
                r(t.pos[0]),
                r(t.pos[1]),
                r(t.height),
                t.text
            ),
        )),
        _ => None,
    }
}

/// Every entity as a key (kinds without a stable form count as "O"), sorted.
fn keys(doc: &DxfDocument, pen: &dyn Fn(&Common) -> String) -> Vec<String> {
    let mut v: Vec<String> = doc
        .entities
        .iter()
        .map(|e| describe(e, pen).map_or_else(|| "O".to_string(), |d| d.1))
        .collect();
    v.sort();
    v
}

/// Keys of the kinds an expansion or a rewrite cannot change: lines, circles and INSERTs
/// outside the layers that dimension blocks land on.
fn core_keys(doc: &DxfDocument) -> Vec<String> {
    let mut v: Vec<String> = doc
        .entities
        .iter()
        .filter_map(|e| describe(e, &pen_layer))
        .filter(|(layer, k)| {
            layer != "0"
                && layer != "A-DIMS"
                && (k.starts_with("L|") || k.starts_with("C|") || k.starts_with("I|"))
        })
        .map(|d| d.1)
        .collect();
    v.sort();
    v
}

fn insert_keys(doc: &DxfDocument) -> Vec<String> {
    let mut v: Vec<String> = keys(doc, &pen_layer)
        .into_iter()
        .filter(|k| k.starts_with("I|"))
        .collect();
    v.sort();
    v
}

fn remove_one(v: &mut Vec<String>, k: &str) -> bool {
    match v.iter().position(|x| x == k) {
        Some(i) => {
            v.remove(i);
            true
        }
        None => false,
    }
}

fn grid_lines(doc: &DxfDocument) -> Vec<Seg> {
    doc.entities
        .iter()
        .filter_map(|e| match e {
            Entity::Line(l) if l.common.layer == "S-GRID" => {
                Some([l.start[0], l.start[1], l.end[0], l.end[1]])
            }
            _ => None,
        })
        .collect()
}

/// Pen of the S-GRID line of `doc` lying on `s`.
fn grid_pen(doc: &DxfDocument, s: &Seg) -> String {
    doc.entities
        .iter()
        .find_map(|e| match e {
            Entity::Line(l)
                if l.common.layer == "S-GRID"
                    && same_seg(&[l.start[0], l.start[1], l.end[0], l.end[1]], s) =>
            {
                Some(pen_full(&l.common))
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no S-GRID line on {s:?}"))
}

fn seg_key(pen: &str, s: &Seg) -> String {
    lkey(pen, (s[0], s[1]), (s[2], s[3]))
}

fn lineweight(l: &Layer) -> Option<i32> {
    l.extra
        .iter()
        .find(|p| p.code == 370)
        .and_then(|p| p.value.trim().parse().ok())
}

fn check_layers(src: &DxfDocument, out: &DxfDocument, ctx: &str) {
    assert!(!src.layers.is_empty());
    for l in &src.layers {
        let m = out
            .layers
            .iter()
            .find(|x| x.name.eq_ignore_ascii_case(&l.name))
            .unwrap_or_else(|| panic!("{ctx}: layer {} lost", l.name));
        assert_eq!(m.color, l.color, "{ctx}: {} color", l.name);
        assert_eq!(m.flags, l.flags, "{ctx}: {} flags", l.name);
        assert!(
            m.linetype.eq_ignore_ascii_case(&l.linetype),
            "{ctx}: {} line type {} vs {}",
            l.name,
            m.linetype,
            l.linetype
        );
        if let Some(w) = lineweight(l) {
            assert_eq!(lineweight(m), Some(w), "{ctx}: {} weight", l.name);
        }
    }
}

fn check_blocks(src: &DxfDocument, out: &DxfDocument, ctx: &str) {
    for name in ["DOOR_900", "WIN_1500", "TAG"] {
        let a = src
            .blocks
            .iter()
            .find(|b| b.name == name)
            .unwrap_or_else(|| panic!("{ctx}: source has no {name}"));
        let b = out
            .blocks
            .iter()
            .find(|b| b.name == name)
            .unwrap_or_else(|| panic!("{ctx}: block {name} lost"));
        assert_eq!(a.entities.len(), b.entities.len(), "{ctx}: {name} content");
        let tags = |x: &Block| -> Vec<String> { x.attdefs.iter().map(|d| d.tag.clone()).collect() };
        assert_eq!(tags(a), tags(b), "{ctx}: {name} attribute tags");
    }
}

// ---- the session under test ----

struct Session {
    ed: Editor,
    ds: Drawings,
    /// The source file as `read_drawing` gives it.
    src: DxfDocument,
    /// The four S-GRID lines the gestures use, in file units, before any edit.
    orig: [Seg; 4],
    grid: [usize; 4],
    /// Export of the untouched drawing, read back.
    base: DxfDocument,
    start: Snap,
}

impl Session {
    fn open() -> Session {
        let path = e2e_common::temp_file("dwg_edit_src.dxf");
        dxf::write_file(&source_doc(), &path).unwrap();
        let src = read_drawing(&path).unwrap_or_else(|e| panic!("read_drawing: {e}"));
        let _ = std::fs::remove_file(&path);

        let ed = Editor::new_in_memory().unwrap();
        let mut ds = Drawings::default();
        let _ = ds.add(
            &src,
            "plan",
            None,
            ed.level,
            None,
            Origin::MinToZero,
            v2(0.0, 0.0),
        );
        assert_eq!(ds.list.len(), 1, "drawing placed");
        assert_eq!(ds.list[0].rot, 0.0);
        let d = &ds.list[0];
        let idx: Vec<usize> = d
            .u
            .lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.layer == "S-GRID")
            .map(|(i, _)| i)
            .collect();
        assert!(idx.len() >= 4, "sample plan has grid lines");
        let grid = [idx[0], idx[1], idx[2], idx[3]];
        let orig = [
            seg(d, grid[0]),
            seg(d, grid[1]),
            seg(d, grid[2]),
            seg(d, grid[3]),
        ];
        let src_grid = grid_lines(&src);
        for o in &orig {
            assert!(has(&src_grid, o), "session line {o:?} is a file line");
        }
        let start = snap(&ds);
        let mut s = Session {
            ed,
            ds,
            src,
            orig,
            grid,
            base: DxfDocument::default(),
            start,
        };
        s.base = s.export();
        s
    }

    /// `export_into` a new document, write it, read it back.
    fn export(&self) -> DxfDocument {
        let mut doc = DxfDocument::default();
        let rep = self.ds.export_into(&mut doc);
        assert!(rep.total() > 0, "something written");
        let path = e2e_common::temp_file("dwg_edit_out.dxf");
        dxf::write_file(&doc, &path).unwrap();
        let back = read_drawing(&path).unwrap_or_else(|e| panic!("re-read: {e}"));
        let _ = std::fs::remove_file(&path);
        back
    }

    fn undo(&mut self) {
        self.ed.undo();
        self.ds.sync_undo(&mut self.ed);
    }

    fn redo(&mut self) {
        self.ed.redo();
        self.ds.sync_undo(&mut self.ed);
    }

    /// One gesture is one undo step; undo and redo return it exactly, twice over.
    fn gesture(&mut self, act: impl FnOnce(&mut Drawings)) {
        let before = snap(&self.ds);
        let steps = self.ed.undo_len();
        act(&mut self.ds);
        self.ds.sync_undo(&mut self.ed);
        let after = snap(&self.ds);
        assert!(before != after, "the gesture changed nothing");
        assert_eq!(self.ed.undo_len(), steps + 1, "one gesture, one step");
        for _ in 0..2 {
            self.undo();
            assert!(snap(&self.ds) == before, "undo returns the drawing");
            assert!(self.ed.can_redo());
            self.redo();
            assert!(snap(&self.ds) == after, "redo returns the edit");
        }
        assert_eq!(self.ed.undo_len(), steps + 1);
    }

    fn pick_line(ds: &mut Drawings, i: usize) {
        ds.pick = Some((
            0,
            Pick {
                lines: vec![i],
                ..Default::default()
            },
        ));
    }

    fn do_move(&mut self) {
        let i = self.grid[0];
        self.gesture(|ds| {
            Self::pick_line(ds, i);
            ds.xf = (SHIFT_X, 0.0, 0.0);
            ds.apply_xf(false, false);
        });
        let got = seg(&self.ds.list[0], i);
        assert!(
            same_seg(&got, &shifted(self.orig[0], SHIFT_X / self.ds.list[0].unit, 0.0)),
            "moved in the session: {got:?}"
        );
    }

    fn do_rotate(&mut self) {
        let i = self.grid[1];
        self.gesture(|ds| {
            Self::pick_line(ds, i);
            ds.xf = (0.0, 0.0, 90.0);
            ds.apply_xf(false, false);
        });
        let got = seg(&self.ds.list[0], i);
        assert!(is_quarter_turn(&self.orig[1], &got), "rotated in the session: {got:?}");
    }

    fn do_copy(&mut self) {
        let i = self.grid[2];
        let n = self.ds.list[0].u.lines.len();
        self.gesture(|ds| {
            Self::pick_line(ds, i);
            ds.xf = (0.0, SHIFT_Y, 0.0);
            ds.apply_xf(false, true);
        });
        let d = &self.ds.list[0];
        assert_eq!(d.u.lines.len(), n + 1, "one copy");
        assert!(same_seg(&seg(d, i), &self.orig[2]), "the source of a copy stays");
        let got = seg(d, n);
        assert!(
            same_seg(&got, &shifted(self.orig[2], 0.0, SHIFT_Y / d.unit)),
            "copied in the session: {got:?}"
        );
    }

    fn do_delete(&mut self) {
        let i = self.grid[3];
        let n = self.ds.list[0].u.lines.len();
        self.gesture(|ds| {
            Self::pick_line(ds, i);
            ds.delete_pick();
        });
        assert_eq!(self.ds.list[0].u.lines.len(), n - 1, "one line deleted");
    }

    /// S-GRID lines of the session in file units.
    fn session_grid(&self) -> Vec<Seg> {
        let d = &self.ds.list[0];
        (0..d.u.lines.len())
            .filter(|&i| d.u.lines[i].layer == "S-GRID")
            .map(|i| seg(d, i))
            .collect()
    }

    fn undo_all(&mut self) {
        let mut guard = 0;
        while self.ed.can_undo() {
            self.undo();
            guard += 1;
            assert!(guard < 1000, "undo does not drain");
        }
    }
}

#[test]
fn move_rotate_copy_reach_reopened_dxf() {
    let mut s = Session::open();
    let src_grid = grid_lines(&s.src);
    let base_grid = grid_lines(&s.base);
    assert_eq!(base_grid.len(), src_grid.len(), "untouched export keeps the grid");
    s.do_move();
    s.do_rotate();
    s.do_copy();

    let out = s.export();
    let ex = grid_lines(&out);
    // original + copies - deleted
    assert_eq!(ex.len(), src_grid.len() + 1, "one copy added");
    assert_eq!(out.entities.len(), s.base.entities.len() + 1);
    let ux = s.ds.list[0].unit;
    let moved = shifted(s.orig[0], SHIFT_X / ux, 0.0);
    assert!(has(&ex, &moved), "moved line in file coordinates");
    assert!(!has(&ex, &s.orig[0]), "no line left at the old place");
    assert!(
        ex.iter().any(|x| is_quarter_turn(&s.orig[1], x)),
        "rotated line about its middle"
    );
    assert!(!has(&ex, &s.orig[1]), "no line left at the unrotated place");
    assert!(has(&ex, &s.orig[2]), "the copy source stays");
    assert!(
        has(&ex, &shifted(s.orig[2], 0.0, SHIFT_Y / ux)),
        "the copy at its place"
    );
    // the file says what the session shows
    assert!(same_set(&ex, &s.session_grid()), "file equals session");
    // gestures leave the source untouched
    assert!(has(&ex, &s.orig[3]));
}

#[test]
fn delete_then_undo_roundtrip() {
    let mut s = Session::open();
    let before = keys(&s.base, &pen_full);
    let gone = s.orig[3];
    assert!(has(&grid_lines(&s.base), &gone));
    s.do_delete();

    let after = s.export();
    assert_eq!(
        after.entities.len(),
        s.base.entities.len() - 1,
        "original - deleted"
    );
    assert!(!has(&grid_lines(&after), &gone), "the deleted line is gone");
    let after_keys = keys(&after, &pen_full);

    s.undo();
    assert!(snap(&s.ds) == s.start, "undo returns the drawing");
    assert_eq!(keys(&s.export(), &pen_full), before, "undo returns the file content");
    s.redo();
    assert_eq!(keys(&s.export(), &pen_full), after_keys, "redo deletes again");
    assert!(!has(&grid_lines(&s.export()), &gone));
    s.undo();
    assert_eq!(keys(&s.export(), &pen_full), before);
}

#[test]
fn layers_and_blocks_survive_export() {
    let mut s = Session::open();
    s.do_move();
    s.do_rotate();
    s.do_copy();
    s.do_delete();
    let out = s.export();
    check_layers(&s.src, &out, "edited");
    check_layers(&s.src, &s.base, "untouched");
    check_blocks(&s.src, &out, "edited");

    // every INSERT of the file, attribute values and flags included, as read
    let want = insert_keys(&s.src);
    assert!(want.len() >= 5, "doors, windows and the tagged block: {}", want.len());
    assert_eq!(insert_keys(&out), want, "INSERTs after the edits");
    let tagged = out
        .entities
        .iter()
        .find_map(|e| match e {
            Entity::Insert(i) if i.block == "TAG" => Some(i),
            _ => None,
        })
        .expect("tagged INSERT kept");
    let values: Vec<(&str, &str, i32)> = tagged
        .attribs
        .iter()
        .map(|a| (a.tag.as_str(), a.value.as_str(), a.flags))
        .collect();
    assert_eq!(
        values,
        vec![("NO", "101", 0), ("NAME", "Office 2", 0), ("HIDDEN", "x", 1)]
    );
}

#[test]
fn untouched_entities_equal_after_edit() {
    let mut s = Session::open();
    s.do_move();
    s.do_rotate();
    s.do_copy();
    s.do_delete();
    let out = s.export();

    // pens as written for the four lines, before they change
    let pens: Vec<String> = s.orig.iter().map(|o| grid_pen(&s.base, o)).collect();
    let mut rest = keys(&out, &pen_full);
    let mut untouched = keys(&s.base, &pen_full);
    // the moved, the rotated and the deleted line leave the untouched set (the copy source stays)
    for k in [0, 1, 3] {
        let key = seg_key(&pens[k], &s.orig[k]);
        assert!(remove_one(&mut untouched, &key), "base has {:?}", s.orig[k]);
    }
    // everything else is in the edited file exactly once more
    for k in &untouched {
        assert!(remove_one(&mut rest, k), "untouched entity changed or lost: {k}");
    }
    // what remains is the moved, the rotated and the copied line, with their pens
    let ux = s.ds.list[0].unit;
    let d = &s.ds.list[0];
    let rotated = seg(d, s.grid[1]);
    assert!(is_quarter_turn(&s.orig[1], &rotated));
    let mut want = vec![
        seg_key(&pens[0], &shifted(s.orig[0], SHIFT_X / ux, 0.0)),
        seg_key(&pens[1], &rotated),
        seg_key(&pens[2], &shifted(s.orig[2], 0.0, SHIFT_Y / ux)),
    ];
    want.sort();
    rest.sort();
    assert_eq!(rest, want, "only the edited lines differ");
}

#[test]
fn undo_all_export_equals_source() {
    let mut s = Session::open();
    let model_start = e2e_common::dump(&s.ed.model);
    s.do_move();
    s.do_rotate();
    s.do_copy();
    s.do_delete();
    let edited = snap(&s.ds);
    let edited_keys = keys(&s.export(), &pen_full);
    assert_ne!(edited_keys, keys(&s.base, &pen_full), "the edits are in the file");

    s.undo_all();
    assert!(snap(&s.ds) == s.start, "undo all returns the drawing");
    assert!(!s.ed.can_undo());
    let out = s.export();
    assert_eq!(keys(&out, &pen_full), keys(&s.base, &pen_full), "same entity set as untouched");
    assert_eq!(
        core_keys(&out),
        core_keys(&s.src),
        "lines, circles and INSERTs equal the source file"
    );
    assert!(same_set(&grid_lines(&out), &grid_lines(&s.src)));
    assert_eq!(out.entities.len(), s.base.entities.len());
    check_layers(&s.src, &out, "undone");
    check_blocks(&s.src, &out, "undone");
    e2e_common::assert_clean(&s.ed.model, "after undo all");
    e2e_common::assert_same(&e2e_common::dump(&s.ed.model), &model_start, "model untouched");

    // redo all brings the edits back, byte for byte
    let mut guard = 0;
    while s.ed.can_redo() {
        s.redo();
        guard += 1;
        assert!(guard < 1000, "redo does not drain");
    }
    assert!(snap(&s.ds) == edited, "redo all returns the edits");
    assert_eq!(keys(&s.export(), &pen_full), edited_keys);
}
