//! Clipboard and group properties: paste + undo restore the exact previous hash, repeated paste
//! leaves no dangling references, and the integrity check is clean after every step.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use ddc_model::{
    BeamJoin, BeamSpec, CeilingSpec, Clip, ClipText, ColumnSpec, DimKind, DimRef, DimSpec, Edit,
    Form, GridSpec, LayerSpec, MassSpec, Model, OpeningSpec, PasteOpts, PlaceLevel, RoofKind,
    RoofSpec, SlabSpec, Target, WallSpec, CLIP_TEXT_MAX,
};
use rusqlite::types::Value;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn f(&mut self, a: f64, b: f64) -> f64 {
        a + (b - a) * ((self.next() % 10_000) as f64 / 10_000.0)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

struct Base {
    l1: i64,
    l2: i64,
    l3: i64,
    wt: i64,
    st: i64,
    ct: i64,
    bt: i64,
}

fn base() -> (Model, Base) {
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("L1", 0.0).unwrap();
    let l2 = m.add_level("L2", 3.0).unwrap();
    let l3 = m.add_level("L3", 6.0).unwrap();
    let lay = |t| LayerSpec {
        material: "Concrete".into(),
        thickness: t,
    };
    let wt = m.add_wall_type("W", &[lay(0.2)]).unwrap();
    let st = m.add_slab_type("S", &[lay(0.2)]).unwrap();
    let ct = m.add_catalog_member_type("R 400x400").unwrap();
    let bt = m.add_catalog_member_type("IPE 300").unwrap();
    (
        m,
        Base {
            l1,
            l2,
            l3,
            wt,
            st,
            ct,
            bt,
        },
    )
}

fn dump(m: &Model) -> Vec<(String, Vec<String>)> {
    let db = m.sql();
    let mut st = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap();
    // caches and quantities are recomputed from the content, not part of it
    let tables: Vec<String> = st
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|x| x.unwrap())
        .filter(|n| n != "join_cache" && !n.ends_with("_qty"))
        .collect();
    let mut out = vec![];
    for t in tables {
        let mut v = vec![];
        let mut q = db
            .prepare(&format!("SELECT * FROM \"{t}\" ORDER BY rowid"))
            .unwrap();
        let n = q.column_count();
        let mut rows = q.query([]).unwrap();
        while let Some(r) = rows.next().unwrap() {
            let cells: Vec<String> = (0..n)
                .map(|i| format!("{:?}", r.get::<_, Value>(i).unwrap()))
                .collect();
            v.push(cells.join("|"));
        }
        out.push((t, v));
    }
    out
}

fn content_hash(m: &Model) -> u64 {
    let mut h = DefaultHasher::new();
    dump(m).hash(&mut h);
    h.finish()
}

fn clean(m: &Model) {
    let bad = m.check_integrity().unwrap();
    assert!(bad.is_empty(), "{bad:?}");
}

fn count(m: &Model, t: &str) -> i64 {
    m.sql()
        .query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))
        .unwrap()
}

fn wall(b: &Base, level: i64, a: (f64, f64), to: (f64, f64)) -> WallSpec {
    WallSpec {
        a,
        b: to,
        level,
        base_offset: 0.0,
        height: 3.0,
        wall_type: b.wt,
        offset0: Some(0.0),
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

fn square(x: f64, y: f64, w: f64) -> Vec<(f64, f64)> {
    vec![(x, y), (x + w, y), (x + w, y + w), (x, y + w)]
}

fn column(b: &Base, at: (f64, f64)) -> ColumnSpec {
    ColumnSpec {
        level: b.l1,
        top_level: Some(b.l2),
        member_type: b.ct,
        structural: true,
        at,
        base_offset: 0.0,
        top_offset: 0.0,
        rotation: 0.0,
        top_at: None,
    }
}

fn beam(b: &Base, a: (f64, f64), to: (f64, f64)) -> BeamSpec {
    BeamSpec {
        level: b.l2,
        member_type: b.bt,
        a: (a.0, a.1, 0.0),
        b: (to.0, to.1, 0.0),
        bulge: 0.0,
        yjust: Default::default(),
        zjust: Default::default(),
        rotation: 0.0,
        start_ext: 0.0,
        end_ext: 0.0,
        join_start: BeamJoin::Auto,
        join_end: BeamJoin::Auto,
    }
}

/// A random model and a random selection from it.
fn random_model(r: &mut Rng) -> (Model, Base, Vec<(&'static str, i64)>) {
    let (mut m, b) = base();
    let mut sel = vec![];
    for i in 0..(2 + r.below(4)) {
        let y = i as f64 * 6.0;
        let len = r.f(4.0, 9.0);
        let w = m.add_wall(&wall(&b, b.l1, (0.0, y), (len, y))).unwrap();
        for k in 0..r.below(3) {
            m.add_opening(w, &door(0.3 + k as f64 * 1.3)).unwrap();
        }
        if r.below(2) == 0 {
            sel.push(("wall", w));
        }
    }
    for i in 0..r.below(3) {
        let s = m
            .add_slab(&SlabSpec {
                level: b.l1,
                slab_type: b.st,
                offset: 0.0,
                outer: square(30.0 + i as f64 * 8.0, 0.0, r.f(3.0, 6.0)),
                holes: vec![],
            })
            .unwrap();
        if r.below(2) == 0 {
            sel.push(("slab", s));
        }
    }
    for i in 0..r.below(3) {
        let c = m.add_column(&column(&b, (-5.0, i as f64 * 3.0))).unwrap();
        if r.below(2) == 0 {
            sel.push(("col", c));
        }
    }
    for i in 0..r.below(3) {
        let y = -5.0 - i as f64 * 2.0;
        let x = m.add_beam(&beam(&b, (0.0, y), (r.f(2.0, 6.0), y))).unwrap();
        if r.below(2) == 0 {
            sel.push(("beam", x));
        }
    }
    if r.below(2) == 0 {
        let c = m
            .add_ceiling(&CeilingSpec {
                level: b.l1,
                slab_type: b.st,
                height: 2.5,
                outer: square(70.0, 0.0, r.f(3.0, 6.0)),
                holes: vec![],
            })
            .unwrap();
        sel.push(("ceiling", c));
    }
    if r.below(2) == 0 {
        let x = m
            .add_roof(&RoofSpec {
                level: b.l2,
                slab_type: b.st,
                offset: 0.0,
                overhang: r.f(0.0, 0.5),
                fascia: 0.0,
                kind: RoofKind::Footprint {
                    ring: square(90.0, 0.0, r.f(4.0, 7.0)),
                    slopes: vec![Some(0.5), None, Some(0.5), None],
                    mansard: None,
                },
            })
            .unwrap();
        sel.push(("roof", x));
    }
    if sel.is_empty() {
        let w = m
            .add_wall(&wall(&b, b.l1, (-20.0, 0.0), (-15.0, 0.0)))
            .unwrap();
        sel.push(("wall", w));
    }
    (m, b, sel)
}

#[test]
fn paste_then_undo_restores_the_exact_hash() {
    for seed in 1..=24u64 {
        let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let (mut m, b, sel) = random_model(&mut r);
        let clip = m.clip_copy(&sel).unwrap();
        let before = content_hash(&m);
        let o = PasteOpts {
            dx: r.f(-40.0, 40.0),
            dy: r.f(40.0, 80.0),
            level: PlaceLevel::Base(if r.below(2) == 0 { b.l1 } else { b.l2 }),
        };
        let rep = m.clip_paste(&clip, &o).unwrap();
        assert!(!rep.created.is_empty(), "seed {seed}");
        clean(&m);
        m.undo().unwrap();
        assert_eq!(before, content_hash(&m), "seed {seed}");
        clean(&m);
    }
}

#[test]
fn copy_writes_nothing_to_the_model() {
    let (m, b) = {
        let (mut m, b) = base();
        m.add_wall(&wall(&b, b.l1, (0.0, 0.0), (5.0, 0.0))).unwrap();
        (m, b)
    };
    let _ = b;
    let before = content_hash(&m);
    m.clip_copy(&[("wall", 1)]).unwrap();
    assert_eq!(before, content_hash(&m));
}

#[test]
fn paste_twice_leaves_no_dangling_references() {
    for seed in 1..=12u64 {
        let mut r = Rng(seed.wrapping_mul(0xD134_2543_DE82_EF95) | 1);
        let (mut m, b, sel) = random_model(&mut r);
        let clip = m.clip_copy(&sel).unwrap();
        for k in 1..=2 {
            let o = PasteOpts {
                dx: 0.0,
                dy: 100.0 * k as f64,
                level: PlaceLevel::Base(b.l1),
            };
            m.clip_paste(&clip, &o).unwrap();
            clean(&m);
        }
        let dangling: i64 = m
            .sql()
            .query_row(
                "SELECT count(*) FROM opening WHERE wall NOT IN (SELECT id FROM wall)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(dangling, 0);
    }
}

#[test]
fn host_and_its_openings_travel_together() {
    let (mut m, b) = base();
    let w = m.add_wall(&wall(&b, b.l1, (0.0, 0.0), (8.0, 0.0))).unwrap();
    m.add_opening(w, &door(1.0)).unwrap();
    m.add_opening(w, &door(4.0)).unwrap();
    let clip = m.clip_copy(&[("wall", w)]).unwrap();
    let rep = m
        .clip_paste(
            &clip,
            &PasteOpts {
                dx: 0.0,
                dy: 5.0,
                level: PlaceLevel::Base(b.l1),
            },
        )
        .unwrap();
    let new = rep.created[0].1;
    assert_ne!(new, w);
    let n: i64 = m
        .sql()
        .query_row("SELECT count(*) FROM opening WHERE wall=?1", [new], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(n, 2);
    let y: f64 = m
        .sql()
        .query_row("SELECT ay FROM wall WHERE id=?1", [new], |r| r.get(0))
        .unwrap();
    assert!((y - 5.0).abs() < 1e-9);
}

#[test]
fn lone_opening_finds_a_new_host() {
    let (mut m, b) = base();
    let w = m.add_wall(&wall(&b, b.l1, (0.0, 0.0), (8.0, 0.0))).unwrap();
    let other = m.add_wall(&wall(&b, b.l1, (0.0, 5.0), (8.0, 5.0))).unwrap();
    let o = m.add_opening(w, &door(1.0)).unwrap();
    let clip = m.clip_copy(&[("opening", o)]).unwrap();
    m.clip_paste(
        &clip,
        &PasteOpts {
            dx: 0.0,
            dy: 5.0,
            level: PlaceLevel::Base(b.l1),
        },
    )
    .unwrap();
    let n: i64 = m
        .sql()
        .query_row("SELECT count(*) FROM opening WHERE wall=?1", [other], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(n, 1);
    clean(&m);
    // an opening is not placed onto itself at the same position
    let same = PasteOpts {
        dx: 0.0,
        dy: 0.0,
        level: PlaceLevel::Same,
    };
    assert!(m.clip_paste(&clip, &same).is_err());
    clean(&m);
}

#[test]
fn paste_to_levels_is_one_undo_step() {
    let (mut m, b) = base();
    let w = m.add_wall(&wall(&b, b.l1, (0.0, 0.0), (6.0, 0.0))).unwrap();
    m.add_opening(w, &door(1.0)).unwrap();
    m.add_slab(&SlabSpec {
        level: b.l1,
        slab_type: b.st,
        offset: 0.0,
        outer: square(0.0, 0.0, 6.0),
        holes: vec![],
    })
    .unwrap();
    let clip = m.clip_copy(&[("wall", w), ("slab", 1)]).unwrap();
    let before = content_hash(&m);
    m.clip_paste_levels(&clip, &[b.l2, b.l3]).unwrap();
    assert_eq!(count(&m, "wall"), 3);
    assert_eq!(count(&m, "opening"), 3);
    assert_eq!(count(&m, "slab"), 3);
    let at_top: i64 = m
        .sql()
        .query_row("SELECT count(*) FROM wall WHERE level=?1", [b.l3], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(at_top, 1);
    clean(&m);
    m.undo().unwrap();
    assert_eq!(before, content_hash(&m));
}

#[test]
fn paste_aligned_keeps_coordinates() {
    let (mut m, b) = base();
    let c = m.add_column(&column(&b, (3.0, 4.0))).unwrap();
    let clip = m.clip_copy(&[("col", c)]).unwrap();
    let rep = m
        .clip_paste(
            &clip,
            &PasteOpts {
                dx: 0.0,
                dy: 0.0,
                level: PlaceLevel::Same,
            },
        )
        .unwrap();
    let id = rep.created[0].1;
    let (x, y, l): (f64, f64, i64) = m
        .sql()
        .query_row("SELECT x,y,level FROM col WHERE id=?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    assert_eq!((x, y, l), (3.0, 4.0, b.l1));
    clean(&m);
}

#[test]
fn clipboard_text_round_trips_between_projects() {
    let (mut a, ba) = base();
    let w = a
        .add_wall(&wall(&ba, ba.l1, (0.0, 0.0), (6.0, 0.0)))
        .unwrap();
    a.add_opening(w, &door(1.0)).unwrap();
    let lay = LayerSpec {
        material: "Brick".into(),
        thickness: 0.3,
    };
    let special = a.add_wall_type("Only in A", &[lay]).unwrap();
    let w2 = a
        .add_wall(&WallSpec {
            wall_type: special,
            ..wall(&ba, ba.l2, (0.0, 3.0), (6.0, 3.0))
        })
        .unwrap();
    let clip = a.clip_copy(&[("wall", w), ("wall", w2)]).unwrap();
    let text = clip.to_text();
    let back = ddc_model::Clip::from_text(&text).unwrap();
    assert_eq!(back, clip);

    // a different model: different ids, no type and no second level
    let mut b = Model::open(":memory:").unwrap();
    b.add_level("Ground", 0.0).unwrap();
    let before = content_hash(&b);
    let o = PasteOpts {
        dx: 0.0,
        dy: 0.0,
        level: PlaceLevel::Same,
    };
    b.clip_paste(&back, &o).unwrap();
    clean(&b);
    assert_eq!(count(&b, "wall"), 2);
    assert_eq!(count(&b, "opening"), 1);
    let (tn, lv): (String, f64) = b
        .sql()
        .query_row(
            "SELECT t.name,l.elevation FROM wall w JOIN wall_type t ON t.id=w.wtype JOIN level l ON l.id=w.level \
             ORDER BY w.id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(tn, "Only in A");
    assert!((lv - 3.0).abs() < 1e-9);
    b.undo().unwrap();
    assert_eq!(before, content_hash(&b));
    assert!(ddc_model::Clip::from_text("just text").is_none());
}

#[test]
fn cut_is_one_undo_step() {
    let (mut m, b) = base();
    let w = m.add_wall(&wall(&b, b.l1, (0.0, 0.0), (6.0, 0.0))).unwrap();
    m.add_opening(w, &door(1.0)).unwrap();
    let c = m.add_column(&column(&b, (10.0, 0.0))).unwrap();
    let before = content_hash(&m);
    let clip = m.clip_cut(&[("wall", w), ("col", c)]).unwrap();
    assert_eq!(count(&m, "wall"), 0);
    assert_eq!(count(&m, "col"), 0);
    clean(&m);
    m.undo().unwrap();
    assert_eq!(before, content_hash(&m));
    assert!(!clip.is_empty());
}

#[test]
fn match_properties_copies_type_not_geometry() {
    let (mut m, b) = base();
    let lay = LayerSpec {
        material: "Brick".into(),
        thickness: 0.4,
    };
    let thick = m.add_wall_type("Thick", &[lay]).unwrap();
    let sample = m
        .add_wall(&WallSpec {
            wall_type: thick,
            height: 4.0,
            ..wall(&b, b.l1, (0.0, 0.0), (5.0, 0.0))
        })
        .unwrap();
    let t = m.add_wall(&wall(&b, b.l1, (0.0, 3.0), (5.0, 3.0))).unwrap();
    let before = content_hash(&m);
    assert_eq!(
        m.match_properties(("wall", sample), &[("wall", t)])
            .unwrap(),
        1
    );
    let (ty, h, ay): (i64, f64, f64) = m
        .sql()
        .query_row("SELECT wtype,height,ay FROM wall WHERE id=?1", [t], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    assert_eq!((ty, h, ay), (thick, 4.0, 3.0));
    clean(&m);
    m.undo().unwrap();
    assert_eq!(before, content_hash(&m));
    assert!(m.match_properties(("wall", sample), &[("col", 1)]).is_err());
}

#[test]
fn pin_blocks_moves_and_deletes_until_unpinned() {
    let (mut m, b) = base();
    let w = m.add_wall(&wall(&b, b.l1, (0.0, 0.0), (5.0, 0.0))).unwrap();
    let s = m
        .add_slab(&SlabSpec {
            level: b.l1,
            slab_type: b.st,
            offset: 0.0,
            outer: square(0.0, 5.0, 4.0),
            holes: vec![],
        })
        .unwrap();
    m.pin(&[("wall", w), ("slab", s)]).unwrap();
    assert!(m.is_pinned("wall", w).unwrap());
    let pinned = content_hash(&m);
    assert!(m.move_elements(&[Target::Wall(w)], (1.0, 0.0)).is_err());
    assert!(m.move_elements(&[Target::Slab(s)], (1.0, 0.0)).is_err());
    assert!(m.delete_walls(&[w]).is_err());
    assert_eq!(pinned, content_hash(&m));
    clean(&m);
    m.unpin(&[("wall", w), ("slab", s)]).unwrap();
    m.move_elements(&[Target::Wall(w)], (1.0, 0.0)).unwrap();
    m.undo().unwrap();
    m.undo().unwrap();
    assert_eq!(pinned, content_hash(&m));
    m.undo().unwrap();
    assert!(!m.is_pinned("wall", w).unwrap());
    clean(&m);
}

fn apartment(m: &mut Model, b: &Base) -> Vec<(&'static str, i64)> {
    let w = m.add_wall(&wall(b, b.l1, (0.0, 0.0), (6.0, 0.0))).unwrap();
    m.add_opening(w, &door(1.0)).unwrap();
    let c = m.add_column(&column(b, (6.0, 2.0))).unwrap();
    vec![("wall", w), ("col", c)]
}

#[test]
fn group_edit_updates_every_instance_and_keeps_attached_detail() {
    let (mut m, b) = base();
    let items = apartment(&mut m, &b);
    let (g, first) = m.group_create("Apartment", &items).unwrap();
    assert_eq!(m.groups().unwrap()[0].instances, 1);
    let second = m.group_place(g, b.l1, 3.0, 21.0).unwrap();
    clean(&m);
    let second_walls: Vec<i64> = m
        .group_members(second)
        .unwrap()
        .into_iter()
        .filter(|x| x.0 == "wall")
        .map(|x| x.1)
        .collect();
    assert_eq!(second_walls.len(), 1);
    let sw = second_walls[0];
    // size of the second instance: an attached detail, not part of the definition
    let dim = m
        .add_dim(&DimSpec {
            level: b.l1,
            kind: DimKind::Linear,
            refs: vec![
                DimRef::WallEnd {
                    wall: sw,
                    face: 0,
                    end: 0,
                },
                DimRef::WallEnd {
                    wall: sw,
                    face: 0,
                    end: 1,
                },
            ],
            dir: (1.0, 0.0),
            side: 1.0,
            offset: 1.0,
            tier: 0,
            auto: false,
        })
        .unwrap();
    // edit of the first instance: a new opening and a taller wall
    let fw = items[0].1;
    let new_op = m.add_opening(fw, &door(3.0)).unwrap();
    m.sql()
        .execute("UPDATE wall SET height=4.5 WHERE id=?1", [fw])
        .unwrap();
    let before = content_hash(&m);
    let before_dump = dump(&m);
    m.group_finish_edit(first, &[("opening", new_op)], &[])
        .unwrap();
    clean(&m);
    let (h, n): (f64, i64) = m
        .sql()
        .query_row(
            "SELECT height,(SELECT count(*) FROM opening WHERE wall=wall.id) FROM wall WHERE id=?1",
            [sw],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(h, 4.5, "same wall id, new height");
    assert_eq!(n, 2, "new opening reached the other instance");
    let d: i64 = m
        .sql()
        .query_row("SELECT count(*) FROM dim WHERE id=?1", [dim], |r| r.get(0))
        .unwrap();
    assert_eq!(d, 1, "attached dimension stays");
    m.undo().unwrap();
    if before != content_hash(&m) {
        let now = dump(&m);
        for (a, b) in before_dump.iter().zip(&now) {
            if a != b {
                eprintln!(
                    "DIFF table {}: before {:?} now {:?}",
                    a.0,
                    a.1.iter().filter(|x| !b.1.contains(x)).collect::<Vec<_>>(),
                    b.1.iter().filter(|x| !a.1.contains(x)).collect::<Vec<_>>()
                );
            }
        }
        panic!("hash");
    }
    clean(&m);
}

#[test]
fn group_keeps_a_moved_instance_in_place_on_edit() {
    let (mut m, b) = base();
    let items = apartment(&mut m, &b);
    let (g, first) = m.group_create("Flat", &items).unwrap();
    let second = m.group_place(g, b.l1, 0.0, 30.0).unwrap();
    let targets: Vec<Target> = m
        .group_members(second)
        .unwrap()
        .into_iter()
        .filter_map(|x| match x.0.as_str() {
            "wall" => Some(Target::Wall(x.1)),
            "col" => Some(Target::Column(x.1)),
            _ => None,
        })
        .collect();
    let wall_id = m
        .group_members(second)
        .unwrap()
        .into_iter()
        .find(|x| x.0 == "wall")
        .unwrap()
        .1;
    m.move_elements(&targets, (2.0, 1.0)).unwrap();
    let was: f64 = m
        .sql()
        .query_row("SELECT ax FROM wall WHERE id=?1", [wall_id], |r| r.get(0))
        .unwrap();
    m.group_finish_edit(first, &[], &[]).unwrap();
    let now: f64 = m
        .sql()
        .query_row("SELECT ax FROM wall WHERE id=?1", [wall_id], |r| r.get(0))
        .unwrap();
    assert!((was - now).abs() < 1e-6, "{was} {now}");
    clean(&m);
}

#[test]
fn group_place_undo_ungroup_and_delete() {
    let (mut m, b) = base();
    let items = apartment(&mut m, &b);
    let (g, first) = m.group_create("Unit", &items).unwrap();
    let before = content_hash(&m);
    let inst = m.group_place(g, b.l2, 0.0, 40.0).unwrap();
    clean(&m);
    m.undo().unwrap();
    assert_eq!(before, content_hash(&m));
    m.group_ungroup(first).unwrap();
    assert_eq!(count(&m, "group_inst"), 0);
    assert_eq!(count(&m, "wall"), 1, "elements stay");
    m.undo().unwrap();
    assert_eq!(before, content_hash(&m));
    // deleting a member removes it from the instance in the same journal entry
    let wall_id = items[0].1;
    let col_id = items[1].1;
    m.delete_items("wall_del", &[("wall", wall_id), ("col", col_id)])
        .unwrap();
    assert_eq!(count(&m, "group_inst"), 0);
    clean(&m);
    m.undo().unwrap();
    assert_eq!(before, content_hash(&m));
    assert!(m.group_delete(g).is_err());
    let _ = inst;
}

#[test]
fn copying_a_whole_instance_creates_a_new_instance() {
    let (mut m, b) = base();
    let items = apartment(&mut m, &b);
    let (_, first) = m.group_create("Copied", &items).unwrap();
    let members = m.group_members(first).unwrap();
    let list: Vec<(&str, i64)> = members.iter().map(|x| (x.0.as_str(), x.1)).collect();
    let clip = m.clip_copy(&list).unwrap();
    assert_eq!(clip.instances.len(), 1);
    m.clip_paste(
        &clip,
        &PasteOpts {
            dx: 0.0,
            dy: 25.0,
            level: PlaceLevel::Base(b.l1),
        },
    )
    .unwrap();
    assert_eq!(count(&m, "group_inst"), 2);
    assert_eq!(count(&m, "el_group"), 1);
    clean(&m);
}

#[test]
fn group_members_refuse_rotation() {
    let (mut m, b) = base();
    let items = apartment(&mut m, &b);
    m.group_create("Turn", &items).unwrap();
    assert!(m
        .rotate_elements(&[Target::Wall(items[0].1)], (0.0, 0.0), 1.0, false)
        .is_err());
    assert!(m
        .rotate_elements(&[Target::Wall(items[0].1)], (0.0, 0.0), 1.0, true)
        .is_ok());
    clean(&m);
}

#[test]
fn join_selected_is_one_step_and_respects_priority() {
    let (mut m, b) = base();
    let w = m.add_wall(&wall(&b, b.l1, (0.0, 0.0), (6.0, 0.0))).unwrap();
    let c = m.add_column(&column(&b, (3.0, 0.0))).unwrap();
    let far = m.add_column(&column(&b, (50.0, 50.0))).unwrap();
    let before = content_hash(&m);
    let before_dump = dump(&m);
    let n = m
        .join_selected(&[("wall", w), ("col", c), ("col", far)])
        .unwrap();
    assert_eq!(n, 1, "only the intersecting pair");
    let joins = m.joins().unwrap();
    assert_eq!(joins[0].0, ("col".to_string(), c), "columns cut walls");
    clean(&m);
    m.switch_joins(&[("wall", w), ("col", c)]).unwrap();
    assert_eq!(m.joins().unwrap()[0].0, ("wall".to_string(), w));
    m.undo().unwrap();
    m.unjoin_selected(&[("wall", w), ("col", c)]).unwrap();
    assert!(m.joins().unwrap().is_empty());
    m.undo().unwrap();
    m.undo().unwrap();
    if before != content_hash(&m) {
        let now = dump(&m);
        for (a, b) in before_dump.iter().zip(&now) {
            if a != b {
                eprintln!(
                    "DIFF table {}: before {:?} now {:?}",
                    a.0,
                    a.1.iter().filter(|x| !b.1.contains(x)).collect::<Vec<_>>(),
                    b.1.iter().filter(|x| !a.1.contains(x)).collect::<Vec<_>>()
                );
            }
        }
        panic!("hash");
    }
}

#[test]
fn dim_between_copied_walls_goes_along() {
    let (mut m, b) = base();
    let w = m.add_wall(&wall(&b, b.l1, (0.0, 0.0), (6.0, 0.0))).unwrap();
    let d = m
        .add_dim(&DimSpec {
            level: b.l1,
            kind: DimKind::Linear,
            refs: vec![
                DimRef::WallEnd {
                    wall: w,
                    face: 0,
                    end: 0,
                },
                DimRef::WallEnd {
                    wall: w,
                    face: 0,
                    end: 1,
                },
            ],
            dir: (1.0, 0.0),
            side: 1.0,
            offset: 1.0,
            tier: 0,
            auto: false,
        })
        .unwrap();
    let clip = m.clip_copy(&[("wall", w)]).unwrap();
    let rep = m
        .clip_paste(
            &clip,
            &PasteOpts {
                dx: 0.0,
                dy: 9.0,
                level: PlaceLevel::Base(b.l1),
            },
        )
        .unwrap();
    let new = rep.created[0].1;
    let refs: String = m
        .sql()
        .query_row("SELECT refs FROM dim WHERE id<>?1", [d], |r| r.get(0))
        .unwrap();
    assert!(refs.contains(&format!("W {new} ")), "{refs}");
    clean(&m);
}

/// A wall and two columns: most members stay in place when one column is edited.
fn flat(m: &mut Model, b: &Base) -> (i64, i64) {
    let w = m.add_wall(&wall(b, b.l1, (0.0, 0.0), (6.0, 0.0))).unwrap();
    let c1 = m.add_column(&column(b, (3.0, 2.0))).unwrap();
    let c2 = m.add_column(&column(b, (6.0, 2.0))).unwrap();
    m.group_create("Flat", &[("wall", w), ("col", c1), ("col", c2)])
        .unwrap()
}

fn member(m: &Model, inst: i64, kind: &str) -> Vec<i64> {
    let list = m.group_members(inst).unwrap();
    list.into_iter()
        .filter(|x| x.0 == kind)
        .map(|x| x.1)
        .collect()
}

fn anchor(m: &Model, inst: i64) -> (f64, f64) {
    let w = member(m, inst, "wall")[0];
    m.sql()
        .query_row("SELECT ax,ay FROM wall WHERE id=?1", [w], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap()
}

/// The instance's columns relative to its wall, in order.
fn local_cols(m: &Model, inst: i64) -> Vec<(i64, i64)> {
    let a = anchor(m, inst);
    let mut out: Vec<(i64, i64)> = member(m, inst, "col")
        .into_iter()
        .map(|c| {
            let (x, y): (f64, f64) = m
                .sql()
                .query_row("SELECT x,y FROM col WHERE id=?1", [c], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .unwrap();
            (
                ((x - a.0) * 1e3).round() as i64,
                ((y - a.1) * 1e3).round() as i64,
            )
        })
        .collect();
    out.sort_unstable();
    out
}

fn far_col(m: &Model, inst: i64) -> i64 {
    let a = anchor(m, inst);
    let at_x = |c: &i64| -> i64 {
        let x: f64 = m
            .sql()
            .query_row("SELECT x FROM col WHERE id=?1", [*c], |r| r.get(0))
            .unwrap();
        ((x - a.0) * 1e3).round() as i64
    };
    member(m, inst, "col")
        .into_iter()
        .max_by_key(|c| at_x(c))
        .unwrap()
}

fn near(a: (f64, f64), b: (f64, f64)) -> bool {
    (a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6
}

#[test]
fn editing_a_later_instance_updates_the_others_where_they_stand() {
    let (mut m, b) = base();
    let (g, first) = flat(&mut m, &b);
    let second = m.group_place(g, b.l1, 0.0, 30.0).unwrap();
    let third = m.group_place(g, b.l1, 40.0, 0.0).unwrap();
    let (a1, a2, a3) = (anchor(&m, first), anchor(&m, second), anchor(&m, third));
    m.move_elements(&[Target::Column(far_col(&m, second))], (1.0, 0.0))
        .unwrap();
    m.group_finish_edit(second, &[], &[]).unwrap();
    assert!(near(anchor(&m, first), a1), "the first instance stays put");
    assert!(near(anchor(&m, second), a2));
    assert!(near(anchor(&m, third), a3));
    let want = vec![(3000, 2000), (7000, 2000)];
    for i in [first, second, third] {
        assert_eq!(local_cols(&m, i), want, "instance {i}");
    }
    clean(&m);
    m.undo().unwrap();
    assert_eq!(local_cols(&m, third), vec![(3000, 2000), (6000, 2000)]);
    clean(&m);
}

#[test]
fn editing_after_the_author_moved_keeps_the_author_moved() {
    let (mut m, b) = base();
    let (g, first) = flat(&mut m, &b);
    let second = m.group_place(g, b.l1, 0.0, 30.0).unwrap();
    let mut all: Vec<Target> = member(&m, first, "col")
        .into_iter()
        .map(Target::Column)
        .collect();
    all.push(Target::Wall(member(&m, first, "wall")[0]));
    m.move_elements(&all, (5.0, 1.0)).unwrap();
    let (a1, a2) = (anchor(&m, first), anchor(&m, second));
    m.move_elements(&[Target::Column(far_col(&m, second))], (0.0, 1.0))
        .unwrap();
    m.group_finish_edit(second, &[], &[]).unwrap();
    assert!(near(anchor(&m, first), a1));
    assert!(near(anchor(&m, second), a2));
    assert_eq!(local_cols(&m, first), vec![(3000, 2000), (6000, 3000)]);
    // once more, now from the new definition and with a different instance
    m.group_finish_edit(first, &[], &[]).unwrap();
    assert!(near(anchor(&m, second), a2));
    assert_eq!(local_cols(&m, second), vec![(3000, 2000), (6000, 3000)]);
    clean(&m);
}

#[test]
fn editing_an_instance_on_another_level_leaves_each_on_its_level() {
    let (mut m, b) = base();
    let (g, first) = flat(&mut m, &b);
    let second = m.group_place(g, b.l2, 0.0, 30.0).unwrap();
    m.move_elements(&[Target::Column(far_col(&m, second))], (1.0, 0.0))
        .unwrap();
    m.group_finish_edit(second, &[], &[]).unwrap();
    let level = |m: &Model, i: i64| -> i64 {
        let w = member(m, i, "wall")[0];
        m.sql()
            .query_row("SELECT level FROM wall WHERE id=?1", [w], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(level(&m, first), b.l1);
    assert_eq!(level(&m, second), b.l2);
    assert_eq!(local_cols(&m, first), vec![(3000, 2000), (7000, 2000)]);
    // definition captured on L2: the next edit does not move the first instance to this level
    m.group_finish_edit(first, &[], &[]).unwrap();
    assert_eq!(level(&m, first), b.l1);
    assert_eq!(level(&m, second), b.l2);
    clean(&m);
}

#[test]
fn a_door_removed_from_a_group_stays_out_on_the_next_edit() {
    let (mut m, b) = base();
    let w = m.add_wall(&wall(&b, b.l1, (0.0, 0.0), (6.0, 0.0))).unwrap();
    let o = m.add_opening(w, &door(1.0)).unwrap();
    let c = m.add_column(&column(&b, (6.0, 2.0))).unwrap();
    let (g, first) = m.group_create("Flat", &[("wall", w), ("col", c)]).unwrap();
    let second = m.group_place(g, b.l1, 0.0, 30.0).unwrap();
    assert_eq!(member(&m, second, "opening").len(), 1);
    m.group_finish_edit(first, &[], &[("opening", o)]).unwrap();
    assert_eq!(member(&m, second, "opening").len(), 0);
    assert_eq!(member(&m, first, "opening").len(), 0);
    m.group_finish_edit(first, &[], &[]).unwrap();
    assert_eq!(member(&m, second, "opening").len(), 0);
    assert_eq!(member(&m, first, "opening").len(), 0);
    assert_eq!(count(&m, "opening"), 1, "the door itself stays in its wall");
    clean(&m);
}

#[test]
fn a_member_deleted_in_one_instance_is_not_recreated() {
    let (mut m, b) = base();
    let (g, first) = flat(&mut m, &b);
    let second = m.group_place(g, b.l1, 0.0, 30.0).unwrap();
    let c = far_col(&m, second);
    m.delete_items("wall_cut", &[("col", c)]).unwrap();
    m.group_finish_edit(first, &[], &[]).unwrap();
    assert_eq!(member(&m, second, "col").len(), 1);
    assert_eq!(member(&m, first, "col").len(), 2);
    clean(&m);
}

#[test]
fn a_grid_in_a_group_keeps_its_name_on_edit() {
    let (mut m, b) = base();
    let w = m.add_wall(&wall(&b, b.l1, (0.0, 0.0), (6.0, 0.0))).unwrap();
    let gr = m
        .add_grid(&GridSpec::line("A", (0.0, -5.0), (0.0, 5.0)))
        .unwrap();
    let (g, first) = m
        .group_create("Flat", &[("wall", w), ("grid", gr)])
        .unwrap();
    let second = m.group_place(g, b.l1, 0.0, 30.0).unwrap();
    let name = |m: &Model, i: i64| -> String {
        let id = member(m, i, "grid")[0];
        m.sql()
            .query_row("SELECT name FROM grid WHERE id=?1", [id], |r| r.get(0))
            .unwrap()
    };
    let (n1, n2) = (name(&m, first), name(&m, second));
    assert_ne!(n1, n2);
    m.group_finish_edit(first, &[], &[]).unwrap();
    m.group_finish_edit(first, &[], &[]).unwrap();
    assert_eq!((name(&m, first), name(&m, second)), (n1, n2));
    clean(&m);
}

#[test]
fn another_instances_link_to_an_outside_mass_survives_an_edit() {
    let (mut m, b) = base();
    let (g, first) = flat(&mut m, &b);
    let second = m.group_place(g, b.l1, 0.0, 30.0).unwrap();
    let w = member(&m, second, "wall")[0];
    let tower = m
        .add_mass(&MassSpec {
            level: b.l1,
            name: "Tower".into(),
            void: false,
            inplace: false,
            form: Form::Extrude {
                ring: vec![(0.0, 0.0), (10.0, 0.0), (10.0, 6.0), (0.0, 6.0)],
                z0: 0.0,
                h: 9.0,
                ids: vec![],
            },
        })
        .unwrap();
    m.sql()
        .execute(
            "INSERT INTO mass_host(kind,ref,mass,face) VALUES ('wall',?1,?2,0)",
            rusqlite::params![w, tower],
        )
        .unwrap();
    m.group_finish_edit(first, &[], &[]).unwrap();
    let n: i64 = m
        .sql()
        .query_row(
            "SELECT count(*) FROM mass_host WHERE kind='wall' AND ref=?1",
            [w],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn a_door_pasted_to_another_level_lands_in_the_wall_of_that_level() {
    let (mut m, b) = base();
    let w1 = m.add_wall(&wall(&b, b.l1, (0.0, 0.0), (8.0, 0.0))).unwrap();
    let w2 = m.add_wall(&wall(&b, b.l2, (0.0, 0.0), (8.0, 0.0))).unwrap();
    let o = m.add_opening(w1, &door(1.0)).unwrap();
    let clip = m.clip_copy(&[("opening", o)]).unwrap();
    let rep = m
        .clip_paste(
            &clip,
            &PasteOpts {
                dx: 0.0,
                dy: 0.0,
                level: PlaceLevel::Base(b.l2),
            },
        )
        .unwrap();
    assert_eq!(rep.created.len(), 1, "{:?}", rep.skipped);
    let on = |m: &Model, w: i64| -> i64 {
        m.sql()
            .query_row("SELECT count(*) FROM opening WHERE wall=?1", [w], |r| {
                r.get(0)
            })
            .unwrap()
    };
    assert_eq!((on(&m, w1), on(&m, w2)), (1, 1));
    clean(&m);
    m.undo().unwrap();
    assert_eq!((on(&m, w1), on(&m, w2)), (1, 0));
}

#[test]
fn a_door_pasted_into_another_project_finds_a_wall_there() {
    let (mut m, b) = base();
    let w = m.add_wall(&wall(&b, b.l1, (0.0, 0.0), (8.0, 0.0))).unwrap();
    let o = m.add_opening(w, &door(1.0)).unwrap();
    let text = m.clip_copy(&[("opening", o)]).unwrap().to_text();
    let (mut other, ob) = base();
    let there = other
        .add_wall(&wall(&ob, ob.l2, (0.0, 0.0), (8.0, 0.0)))
        .unwrap();
    let clip = Clip::from_text(&text).unwrap();
    let rep = other
        .clip_paste(
            &clip,
            &PasteOpts {
                dx: 0.0,
                dy: 0.0,
                level: PlaceLevel::Base(ob.l2),
            },
        )
        .unwrap();
    assert_eq!(rep.created.len(), 1, "{:?}", rep.skipped);
    let n: i64 = other
        .sql()
        .query_row("SELECT count(*) FROM opening WHERE wall=?1", [there], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(n, 1);
    clean(&other);
}

fn tamper(text: &str, f: impl FnOnce(&mut serde_json::Value)) -> String {
    let (head, body) = text.split_once('\n').unwrap();
    let mut v: serde_json::Value = serde_json::from_str(body).unwrap();
    f(&mut v);
    format!("{head}\n{v}")
}

#[test]
fn a_malformed_clipboard_text_is_refused_without_a_panic() {
    let (mut m, b) = base();
    let w = m.add_wall(&wall(&b, b.l1, (0.0, 0.0), (8.0, 0.0))).unwrap();
    m.add_opening(w, &door(1.0)).unwrap();
    let good = m.clip_copy(&[("wall", w)]).unwrap().to_text();
    assert!(matches!(Clip::parse(&good).unwrap(), ClipText::Clip(_)));
    assert_eq!(Clip::parse("plain text").unwrap(), ClipText::Foreign);
    assert_eq!(
        Clip::parse(&Clip::marker("7-1")).unwrap(),
        ClipText::Marker("7-1".to_string())
    );

    let bad = [
        "DDC-CLIP/1\n{".to_string(),
        "DDC-CLIP/1\n[]".to_string(),
        "DDC-CLIP/1\n{\"tables\":7}".to_string(),
        tamper(&good, |v| {
            v["tables"][0][1]["rows"][0] = serde_json::json!([1])
        }),
        tamper(&good, |v| {
            v["tables"][0][1]["rows"] = serde_json::json!("x")
        }),
        tamper(&good, |v| {
            v["tables"][0][1]["cols"][0] = serde_json::json!(5)
        }),
        tamper(&good, |v| v["hosts"] = serde_json::json!([[1, 2]])),
        tamper(&good, |v| v["roots"] = serde_json::json!([["wall"]])),
        tamper(&good, |v| v["schema"] = serde_json::json!(i64::MAX)),
    ];
    for t in &bad {
        assert!(Clip::parse(t).is_err(), "{}", &t[..t.len().min(120)]);
        assert!(Clip::from_text(t).is_none());
    }
    let huge = format!("DDC-CLIP/1\n{}", "x".repeat(CLIP_TEXT_MAX));
    assert!(Clip::parse(&huge).is_err());
    assert_eq!(count(&m, "wall"), 1);
}

#[test]
fn a_clipboard_text_from_before_the_schema_field_still_pastes() {
    let (mut m, b) = base();
    let w = m.add_wall(&wall(&b, b.l1, (0.0, 0.0), (8.0, 0.0))).unwrap();
    let text = m.clip_copy(&[("wall", w)]).unwrap().to_text();
    let old = tamper(&text, |v| {
        let o = v.as_object_mut().unwrap();
        for k in ["schema", "unsupported", "except"] {
            o.remove(k);
        }
    });
    let clip = Clip::from_text(&old).unwrap();
    m.clip_paste(
        &clip,
        &PasteOpts {
            dx: 0.0,
            dy: 5.0,
            level: PlaceLevel::Base(b.l1),
        },
    )
    .unwrap();
    assert_eq!(count(&m, "wall"), 2);
}

/// Tables of other modules with their columns; in a real project they come from migrations.
fn module_tables(m: &Model) {
    m.sql()
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS ramp(id INTEGER PRIMARY KEY, level INTEGER NOT NULL, top_level INTEGER, path TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS zone(id INTEGER PRIMARY KEY, name TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS zone_room(room INTEGER PRIMARY KEY, zone INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS anno_text_style(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);
             CREATE TABLE IF NOT EXISTS anno_line_style(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);
             CREATE TABLE IF NOT EXISTS anno(id INTEGER PRIMARY KEY, view_kind TEXT NOT NULL, view_id INTEGER NOT NULL,
               kind TEXT NOT NULL, text_style INTEGER, line_style INTEGER, data TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS rebar_type(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);
             CREATE TABLE IF NOT EXISTS rebar_set(id INTEGER PRIMARY KEY, host_kind TEXT NOT NULL, host INTEGER NOT NULL,
               rtype INTEGER NOT NULL, rtype2 INTEGER);
             CREATE TABLE IF NOT EXISTS rebar_cover(id INTEGER PRIMARY KEY, host_kind TEXT NOT NULL, host INTEGER NOT NULL,
               cover REAL NOT NULL);
             CREATE TABLE IF NOT EXISTS analytical_adj(id INTEGER PRIMARY KEY, kind TEXT NOT NULL, ref INTEGER NOT NULL,
               dz REAL NOT NULL);",
        )
        .unwrap();
}

#[test]
fn tables_of_other_modules_are_copied_undone_and_deleted_through_the_registry() {
    let (mut m, b) = base();
    module_tables(&m);
    let w = m.add_wall(&wall(&b, b.l1, (0.0, 0.0), (8.0, 0.0))).unwrap();
    let db = m.sql();
    db.execute(
        "INSERT INTO ramp(id,level,top_level,base_offset,top_offset,width,slope,thickness,landing,max_run,path,material) VALUES (1,?1,?2,0,0,1.2,0.083,0.2,1.2,9,'0,0;4,0','')",
        [b.l1, b.l2],
    )
    .unwrap();
    db.execute_batch("INSERT INTO zone(id,name) VALUES (1,'North');")
        .unwrap();
    db.execute_batch("INSERT INTO anno_text_style(id,name,height_mm,bold,underline,frame) VALUES (55,'T',2.5,0,0,0);")
        .unwrap();
    db.execute(
        "INSERT INTO anno(id,view_kind,view_id,kind,text_style,data) \
         VALUES (1,'plan',?1,'text',55,'{\"at\":[1.0,1.0],\"text\":\"note\"}')",
        [b.l1],
    )
    .unwrap();
    db.execute_batch("INSERT INTO rebar_type(id,name,d,grade) VALUES (333,'D12x',0.012,'B500');")
        .unwrap();
    db.execute(
        "INSERT INTO rebar_set(id,host_kind,host,layout,rtype) VALUES (1,'wall',?1,'{}',333)",
        [w],
    )
    .unwrap();
    db.execute(
        "INSERT INTO rebar_cover(id,host_kind,host,face,cover) VALUES (1,'wall',?1,'all',0.03)",
        [w],
    )
    .unwrap();
    db.execute(
        "INSERT INTO analytical_adj(id,kind,ref,daz) VALUES (1,'wall',?1,0.1)",
        [w],
    )
    .unwrap();

    let clip = m
        .clip_copy(&[("ramp", 1), ("zone", 1), ("anno", 1), ("wall", w)])
        .unwrap();
    assert!(clip.unsupported.is_empty(), "{:?}", clip.unsupported);
    let before = content_hash(&m);
    let rep = m
        .clip_paste(
            &clip,
            &PasteOpts {
                dx: 0.0,
                dy: 5.0,
                level: PlaceLevel::Base(b.l2),
            },
        )
        .unwrap();
    assert_eq!(rep.created.len(), 4, "{:?}", rep.skipped);
    for (t, n) in [
        ("ramp", 2),
        ("zone", 2),
        ("anno", 2),
        ("rebar_set", 2),
        ("rebar_cover", 2),
        ("analytical_adj", 2),
    ] {
        assert_eq!(count(&m, t), n, "{t}");
    }
    let new_wall = rep.created.iter().find(|x| x.0 == "wall").unwrap().1;
    let db = m.sql();
    let path: String = db
        .query_row("SELECT path FROM ramp ORDER BY id DESC", [], |r| r.get(0))
        .unwrap();
    assert_eq!(path, "0,5;4,5");
    let (view, data): (i64, String) = db
        .query_row("SELECT view_id,data FROM anno ORDER BY id DESC", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(view, b.l2);
    let v: serde_json::Value = serde_json::from_str(&data).unwrap();
    assert_eq!(
        (v["at"][1].as_f64(), v["text"].as_str()),
        (Some(6.0), Some("note"))
    );
    let host: i64 = db
        .query_row("SELECT host FROM rebar_set ORDER BY id DESC", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        host, new_wall,
        "rebar follows its wall by the alias-free host kind"
    );
    m.undo().unwrap();
    assert_eq!(before, content_hash(&m));

    m.delete_items("wall_cut", &[("zone", 1), ("ramp", 1)])
        .unwrap();
    assert_eq!((count(&m, "zone"), count(&m, "ramp")), (0, 0));
    m.undo().unwrap();
    assert_eq!(before, content_hash(&m));
}

#[test]
fn an_element_kind_nobody_can_copy_is_reported_not_fatal() {
    let (mut m, b) = base();
    let w = m.add_wall(&wall(&b, b.l1, (0.0, 0.0), (8.0, 0.0))).unwrap();
    let clip = m.clip_copy(&[("wall", w), ("no_such_table", 3)]).unwrap();
    assert_eq!(clip.unsupported, vec![("no_such_table".to_string(), 3)]);
    assert_eq!(clip.roots.len(), 1);
    assert!(m.clip_copy(&[("no_such_table", 3)]).is_err());
}

#[test]
fn pasting_to_levels_skips_the_source_level() {
    let (mut m, b) = base();
    let w = m.add_wall(&wall(&b, b.l1, (0.0, 0.0), (6.0, 0.0))).unwrap();
    let clip = m.clip_copy(&[("wall", w)]).unwrap();
    assert!(m.clip_paste_levels(&clip, &[b.l1]).is_err());
    assert_eq!(count(&m, "wall"), 1);
    m.clip_paste_levels(&clip, &[b.l1, b.l2]).unwrap();
    assert_eq!(count(&m, "wall"), 2);
    let on_source: i64 = m
        .sql()
        .query_row("SELECT count(*) FROM wall WHERE level=?1", [b.l1], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(on_source, 1);
    clean(&m);
}

#[test]
fn pin_covers_rotation_and_a_pin_can_be_undone() {
    let (mut m, b) = base();
    let c = m.add_column(&column(&b, (1.0, 1.0))).unwrap();
    assert!(m.can_pin("col"));
    assert!(!m.can_pin("no_such_table"));
    m.pin(&[("col", c)]).unwrap();
    assert!(m
        .sql()
        .execute("UPDATE col SET rotation=1.0 WHERE id=?1", [c])
        .is_err());
    m.undo().unwrap();
    m.move_elements(&[Target::Column(c)], (1.0, 0.0)).unwrap();
    clean(&m);
}

#[test]
fn a_pinned_slab_does_not_block_what_leaves_it_alone() {
    let (mut m, b) = base();
    let s = m
        .add_slab(&SlabSpec {
            level: b.l1,
            slab_type: b.st,
            offset: 0.0,
            outer: square(0.0, 0.0, 6.0),
            holes: vec![],
        })
        .unwrap();
    m.pin(&[("slab", s)]).unwrap();
    m.update_level(b.l1, "L1", 0.5).unwrap();
    let same = Edit::SlabBoundary {
        id: s,
        outer: square(0.0, 0.0, 6.0),
        holes: vec![],
    };
    m.apply_edits(&[same]).unwrap();
    let moved = Edit::SlabBoundary {
        id: s,
        outer: square(1.0, 0.0, 6.0),
        holes: vec![],
    };
    assert!(m.apply_edits(&[moved]).is_err());
}

#[test]
fn a_batch_keeps_going_without_the_pinned_rows() {
    let (mut m, b) = base();
    let pinned = m.add_wall(&wall(&b, b.l1, (0.0, 0.0), (8.0, 0.0))).unwrap();
    let hosts_pin = m.add_wall(&wall(&b, b.l1, (0.0, 4.0), (8.0, 4.0))).unwrap();
    let free = m.add_wall(&wall(&b, b.l1, (0.0, 8.0), (8.0, 8.0))).unwrap();
    let o = m.add_opening(hosts_pin, &door(1.0)).unwrap();
    m.pin(&[("wall", pinned), ("opening", o)]).unwrap();
    let edits = [
        Edit::Delete { table: "wall", id: pinned },
        Edit::Delete { table: "wall", id: hosts_pin },
        Edit::Delete { table: "wall", id: free },
    ];
    let (keep, skipped) = m.split_pinned(&edits).unwrap();
    assert_eq!(keep.len(), 1);
    assert_eq!(
        skipped,
        vec![
            ("wall".to_string(), pinned),
            ("wall".to_string(), hosts_pin)
        ]
    );
    m.apply_edits(&keep).unwrap();
    assert_eq!(count(&m, "wall"), 2);
}

#[test]
fn rotating_a_group_member_is_refused_on_every_path() {
    let (mut m, b) = base();
    let (_, first) = flat(&mut m, &b);
    let c = member(&m, first, "col")[0];
    assert!(m.reject_group_turn(&[("col", c)]).is_err());
    let free = m.add_column(&column(&b, (20.0, 20.0))).unwrap();
    assert!(m.reject_group_turn(&[("col", free)]).is_ok());
}

#[test]
fn schema_step_is_applied() {
    let m = Model::open(":memory:").unwrap();
    let v: i64 = m
        .sql()
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM meta WHERE key='schema_version'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(v, ddc_model::SCHEMA_VERSION);
    for t in ["el_group", "group_inst", "group_member", "el_pin"] {
        assert_eq!(count(&m, t), 0);
    }
}

fn push_mesh(m: &mut Model, level: i64) -> i64 {
    let pos: Vec<u8> = [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect();
    let idx: Vec<u8> = [0u32, 1, 2].iter().flat_map(|i| i.to_le_bytes()).collect();
    m.sql()
        .execute(
            "INSERT INTO mesh(level,category,name,positions,indices) VALUES (?1,'Doors','n',?2,?3)",
            rusqlite::params![level, pos, idx],
        )
        .unwrap();
    m.sql().last_insert_rowid()
}

fn cherry(note: &str) -> ddc_model::MaterialSpec {
    ddc_model::MaterialSpec {
        name: "Cherry".into(),
        color: Some(0x336699),
        hatch: "cross".into(),
        density: Some(700.0),
        conductivity: Some(0.15),
        specific_heat: Some(1600.0),
        note: note.into(),
    }
}

/// Mesh with the material "Cherry" (own properties, embedded image) in a fresh project.
fn mesh_source() -> (Model, i64, Clip) {
    let (mut m, b) = base();
    let mesh = push_mesh(&mut m, b.l1);
    m.set_mesh_material(ddc_model::MeshScope::Element, &[mesh.to_string()], Some("Cherry"))
        .unwrap();
    m.set_material("Cherry", &cherry("grain")).unwrap();
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    png.extend_from_slice(&[1, 2, 3, 4]);
    let tex = ddc_model::MaterialTexture {
        source: ddc_model::TextureSource::Embedded,
        scale_m: 0.5,
        rotation_deg: 0.0,
    };
    m.set_material_texture("Cherry", Some((tex, Some(png)))).unwrap();
    let clip = Clip::from_text(&m.clip_copy(&[("mesh", mesh)]).unwrap().to_text()).unwrap();
    (m, mesh, clip)
}

fn here(l: i64) -> PasteOpts {
    PasteOpts {
        dx: 0.0,
        dy: 0.0,
        level: PlaceLevel::Base(l),
    }
}

fn material_row(m: &Model, name: &str) -> Option<ddc_model::Material> {
    m.material_rows()
        .unwrap()
        .into_iter()
        .find(|x| x.spec.name == name)
}

#[test]
fn paste_to_other_project_keeps_material_props() {
    let (_src, _mesh, clip) = mesh_source();
    let (mut other, ob) = base();
    assert!(material_row(&other, "Cherry").is_none());
    let rep = other.clip_paste(&clip, &here(ob.l1)).unwrap();
    assert_eq!(rep.materials_created, vec!["Cherry".to_string()]);
    assert!(rep.materials_differ.is_empty());
    assert_eq!(material_row(&other, "Cherry").unwrap().spec, cherry("grain"));
    assert_eq!(count(&other, "material_image"), 1);
    assert!(other.material_texture_image("Cherry").unwrap().is_some());
    let new = rep.created.iter().find(|c| c.0 == "mesh").unwrap().1;
    let (own, _) = other.mesh_material_of(new).unwrap();
    assert_eq!(own.as_deref(), Some("Cherry"));
    clean(&other);
}

#[test]
fn paste_keeps_existing_target_material_and_reports_diff() {
    let (_src, _mesh, clip) = mesh_source();
    let (mut other, ob) = base();
    other.add_material(&cherry("mine")).unwrap();
    let rep = other.clip_paste(&clip, &here(ob.l1)).unwrap();
    assert_eq!(rep.materials_differ, vec!["Cherry".to_string()]);
    assert!(rep.materials_created.is_empty());
    assert_eq!(material_row(&other, "Cherry").unwrap().spec.note, "mine");
    clean(&other);
}

#[test]
fn paste_undo_removes_created_materials() {
    let (_src, _mesh, clip) = mesh_source();
    let (mut other, ob) = base();
    let (mats, imgs, links) = (
        count(&other, "material"),
        count(&other, "material_image"),
        count(&other, "mesh_material"),
    );
    other.clip_paste(&clip, &here(ob.l1)).unwrap();
    assert_eq!(count(&other, "material"), mats + 1);
    other.undo().unwrap();
    assert_eq!(count(&other, "material"), mats);
    assert_eq!(count(&other, "material_image"), imgs);
    assert_eq!(count(&other, "mesh_material"), links);
    assert_eq!(count(&other, "mesh"), 0);
    clean(&other);
}

#[test]
fn same_project_paste_unchanged() {
    let (mut src, mesh, _clip) = mesh_source();
    let clip = src.clip_copy(&[("mesh", mesh)]).unwrap();
    let before = (count(&src, "material"), count(&src, "material_image"));
    let l: i64 = src
        .sql()
        .query_row("SELECT min(id) FROM level", [], |r| r.get(0))
        .unwrap();
    let rep = src.clip_paste(&clip, &here(l)).unwrap();
    assert!(rep.materials_created.is_empty() && rep.materials_differ.is_empty());
    assert_eq!(before, (count(&src, "material"), count(&src, "material_image")));
    clean(&src);
}

#[test]
fn clip_images_stay_within_the_clipboard_budget() {
    let (mut m, b) = base();
    let mut meshes = vec![];
    for (i, &name) in ["Oak", "Ash", "Elm"].iter().enumerate() {
        let mesh = push_mesh(&mut m, b.l1);
        m.set_mesh_material(ddc_model::MeshScope::Element, &[mesh.to_string()], Some(name))
            .unwrap();
        m.set_material(name, &ddc_model::MaterialSpec { name: name.into(), ..cherry("") })
            .unwrap();
        // 3 MB each and distinct: three of them are over the 8 MB budget
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, i as u8];
        png.resize(3 * 1024 * 1024, i as u8 + 1);
        let tex = ddc_model::MaterialTexture {
            source: ddc_model::TextureSource::Embedded,
            scale_m: 0.5,
            rotation_deg: 0.0,
        };
        m.set_material_texture(name, Some((tex, Some(png)))).unwrap();
        meshes.push(("mesh", mesh));
    }
    let clip = m.clip_copy(&meshes).unwrap();
    let rows = |t: &str| clip.materials.iter().find(|(n, _)| n == t).map(|(_, r)| r);
    let images = rows("material_image").unwrap();
    assert_eq!(images.rows.len(), 2);
    let mats = rows("material").unwrap();
    assert_eq!(mats.rows.len(), 3);
    let at = mats.cols.iter().position(|c| c == "tex_ref").unwrap();
    let bare = mats.rows.iter().filter(|r| r[at] == Value::Null).count();
    assert_eq!(bare, 1);
}
