use super::*;
use crate::views::tests::room_6x4;
use crate::{DimGeom, GridSpec, Target};

fn ids(m: &Model, sql: &str) -> Vec<i64> {
    let mut st = m.sql().prepare(sql).unwrap();
    st.query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

fn near(a: P, b: P) {
    assert!(
        (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9,
        "{a:?} != {b:?}"
    );
}

#[test]
fn door_tag_follows_host_on_move_and_keeps_manual_offset() {
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let door = ids(&m, "SELECT id FROM opening WHERE kind='door'")[0];
    let t = m.tag_element(lv, TagHost::Opening(door), false).unwrap();
    let tag = |m: &Model| m.tags(lv).unwrap().into_iter().find(|x| x.id == t).unwrap();
    let a = tag(&m);
    assert_eq!(a.cat, TagCat::Door);
    // door centre on the south wall axis (offset0 = 0 puts the axis on the lower face)
    near(a.anchor, (1.45, 0.0));
    assert_eq!(a.text.mark, "");
    m.move_tag(t, (0.3, -0.8)).unwrap();
    m.move_elements(&[Target::Opening(door)], (1.0, 0.0))
        .unwrap();
    let b = tag(&m);
    near(b.anchor, (2.45, 0.0));
    near(b.head, (2.75, -0.8));
    m.undo().unwrap();
    near(tag(&m).head, (1.75, -0.8));
    m.undo().unwrap();
    near(tag(&m).spec.offset, a.spec.offset);
    m.assert_integrity();
}

#[test]
fn tag_follows_wall_move_and_cascades_on_host_delete() {
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let walls = ids(&m, "SELECT id FROM wall ORDER BY id");
    let door = ids(&m, "SELECT id FROM opening WHERE kind='door'")[0];
    let td = m.tag_element(lv, TagHost::Opening(door), true).unwrap();
    let tw = m.tag_element(lv, TagHost::Wall(walls[0]), false).unwrap();
    m.set_mark(TagHost::Opening(door), "D1").unwrap();
    let before = m.tags(lv).unwrap();
    m.set_wall_axis(walls[0], (0.0, -1.0), (6.0, -1.0)).unwrap();
    let after = m.tags(lv).unwrap();
    for (x, y) in before.iter().zip(&after) {
        assert!((x.head.1 - 1.0 - y.head.1).abs() < 1e-9, "{x:?} → {y:?}");
    }
    assert_eq!(after.iter().find(|t| t.id == td).unwrap().text.mark, "D1");
    m.undo().unwrap();
    // deleting a wall: the opening, both tags and the number go in one operation; undo restores everything
    m.delete_wall(walls[0]).unwrap();
    assert!(m.tags(lv).unwrap().iter().all(|t| t.id != td && t.id != tw));
    assert!(ids(&m, "SELECT id FROM dim WHERE kind='tag'").is_empty());
    assert!(ids(&m, "SELECT ref FROM prop WHERE key='mark'").is_empty());
    m.undo().unwrap();
    assert_eq!(m.tags(lv).unwrap().len(), 2);
    assert_eq!(m.mark(TagHost::Opening(door)).unwrap(), "D1");
    m.assert_integrity();
}

#[test]
fn tag_all_places_without_overlap_one_undo_and_skips_tagged() {
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let cats = [TagCat::Door, TagCat::Window, TagCat::Room];
    assert_eq!(m.untagged(lv, &cats).unwrap().len(), 3);
    let made = m.tag_all(lv, &cats, false).unwrap();
    assert_eq!(made.len(), 3);
    assert!(m.untagged(lv, &cats).unwrap().is_empty());
    assert!(m.tag_all(lv, &cats, false).unwrap().is_empty());
    let ts = m.tags(lv).unwrap();
    let rect = |t: &Tag| {
        let (w, h) = tag_size(t.cat, &t.text);
        (
            t.head.0 - w / 2.0,
            t.head.1 - h / 2.0,
            t.head.0 + w / 2.0,
            t.head.1 + h / 2.0,
        )
    };
    for (i, a) in ts.iter().enumerate() {
        for b in &ts[i + 1..] {
            let (p, q) = (rect(a), rect(b));
            assert!(
                p.2 <= q.0 || q.2 <= p.0 || p.3 <= q.1 || q.3 <= p.1,
                "{a:?} overlaps {b:?}"
            );
        }
        // the tag did not land on a wall (axes 0/6 in x, 0/4 in y, thickness 0.2 inward)
        let r = rect(a);
        assert!(
            !(r.1 < 0.2 && r.3 > 0.0 && r.0 < 6.0 && r.2 > 0.0),
            "{a:?} on south wall"
        );
    }
    let room = ts.iter().find(|t| t.cat == TagCat::Room).unwrap();
    assert_eq!(room.text.mark, "101");
    assert!(room.text.area.unwrap() > 20.0);
    m.undo().unwrap();
    assert!(m.tags(lv).unwrap().is_empty());
}

#[test]
fn numbering_increments_and_room_number_is_one_undo() {
    assert_eq!(next_mark("101"), "102");
    assert_eq!(next_mark("009"), "010");
    assert_eq!(next_mark(crate::test_ru::ru("mark_d9")), crate::test_ru::ru("mark_d10"));
    assert_eq!(next_mark("101a"), "101b");
    assert_eq!(next_mark("A"), "B");
    assert_eq!(next_mark(""), "1");
    assert_eq!(next_mark(crate::test_ru::ru("mark_d")), crate::test_ru::ru("mark_d1"));
    let (mut m, _) = room_6x4(&[("Brick", 0.2)]);
    let room = ids(&m, "SELECT id FROM room")[0];
    let win = ids(&m, "SELECT id FROM opening WHERE kind='window'")[0];
    m.set_mark(TagHost::Room(room), "201").unwrap();
    m.set_mark(TagHost::Opening(win), "W1").unwrap();
    m.set_mark(TagHost::Opening(win), "W2").unwrap();
    assert_eq!(m.mark(TagHost::Room(room)).unwrap(), "201");
    m.undo().unwrap();
    assert_eq!(m.mark(TagHost::Opening(win)).unwrap(), "W1");
    m.undo().unwrap();
    assert_eq!(m.mark(TagHost::Opening(win)).unwrap(), "");
    m.undo().unwrap();
    assert_eq!(m.mark(TagHost::Room(room)).unwrap(), "101");
    assert!(m.set_mark(TagHost::Wall(1), "x").is_err());
}

#[test]
fn dim_auto_walls_chains_openings_grids_overall_and_no_duplicates() {
    let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
    let south = ids(&m, "SELECT id FROM wall ORDER BY id")[0];
    m.add_grid(&GridSpec::line("A", (2.0, -3.0), (2.0, 6.0)))
        .unwrap();
    let made = m.auto_dimension_walls(lv, &[south]).unwrap();
    let mut v: Vec<f64> = m
        .dims(lv)
        .unwrap()
        .iter()
        .map(|d| (d.geom.value() * 1e9).round() / 1e9)
        .collect();
    v.sort_by(f64::total_cmp);
    // openings: 1.0 | 0.9 | 4.1; axes: 2 | 4; overall 6
    assert_eq!(v, vec![0.9, 1.0, 2.0, 4.0, 4.1, 6.0]);
    assert_eq!(made.len(), 6);
    // a repeat replaces rather than duplicates; one undo
    m.auto_dimension_walls(lv, &[south]).unwrap();
    assert_eq!(m.dims(lv).unwrap().len(), 6);
    m.undo().unwrap();
    assert_eq!(m.dims(lv).unwrap().len(), 6);
    m.undo().unwrap();
    assert!(m.dims(lv).unwrap().is_empty());
    // outward: the dimension line is below the south wall (the interior side is north)
    m.auto_dimension_walls(lv, &[south]).unwrap();
    for d in m.dims(lv).unwrap() {
        if let DimGeom::Linear(l) = d.geom {
            assert!(l.base.y < -0.5, "{:?}", l.base);
        }
    }
    // the grid axis is linked: moving the wall changes the dimension to the axis
    m.move_elements(&[Target::Wall(south)], (0.5, 0.0)).unwrap();
    m.assert_integrity();
}

#[test]
fn set_tag_text_writes_number_and_name_in_one_undo_and_refuses_pinned() {
    let (mut m, _) = room_6x4(&[("Brick", 0.2)]);
    let room = ids(&m, "SELECT id FROM room")[0];
    let name = |m: &Model| -> String {
        m.sql()
            .query_row("SELECT name FROM room WHERE id=?1", [room], |r| r.get(0))
            .unwrap()
    };
    let (n0, name0) = (m.mark(TagHost::Room(room)).unwrap(), name(&m));
    m.set_tag_text(TagHost::Room(room), " 305 ", Some("Hall"))
        .unwrap();
    assert_eq!(m.mark(TagHost::Room(room)).unwrap(), "305");
    assert_eq!(name(&m), "Hall");
    m.undo().unwrap();
    assert_eq!(
        (m.mark(TagHost::Room(room)).unwrap(), name(&m)),
        (n0, name0)
    );
    m.pin(&[("room", room)]).unwrap();
    let e = m
        .set_tag_text(TagHost::Room(room), "9", Some("X"))
        .unwrap_err();
    assert!(e.to_string().contains("element is pinned"), "{e}");
    assert!(m.set_tag_text(TagHost::Wall(1), "x", None).is_err());
}

#[test]
fn other_marks_counts_same_kind_openings_and_rooms() {
    let (mut m, _) = room_6x4(&[("Brick", 0.2)]);
    let door = ids(&m, "SELECT id FROM opening WHERE kind='door'")[0];
    let win = ids(&m, "SELECT id FROM opening WHERE kind='window'")[0];
    let room = ids(&m, "SELECT id FROM room")[0];
    m.set_mark(TagHost::Opening(win), "D12").unwrap();
    // a window with the same text is not a door duplicate
    assert_eq!(m.other_marks(TagHost::Opening(door), "D12").unwrap(), 0);
    m.set_tag_text(TagHost::Opening(door), "D12", None).unwrap();
    assert_eq!(m.other_marks(TagHost::Opening(win), "D12").unwrap(), 0);
    assert_eq!(m.other_marks(TagHost::Opening(door), "").unwrap(), 0);
    m.set_tag_text(TagHost::Room(room), "D12", None).unwrap();
    assert_eq!(m.other_marks(TagHost::Opening(door), "D12").unwrap(), 0);
    assert_eq!(m.other_marks(TagHost::Room(room), "D12").unwrap(), 0);
}

/// 2000 windows on one level are tagged in one pass: every tag clear of the others, and the
/// placement stays near linear (each candidate reads only its grid cells).
#[test]
fn two_thousand_openings_are_tagged_fast_and_clear() {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let layer = crate::LayerSpec {
        material: "Brick".into(),
        thickness: 0.25,
    };
    let wt = m.add_wall_type("W250", &[layer]).unwrap();
    let specs: Vec<crate::WallSpec> = (0..20)
        .map(|k| crate::WallSpec {
            a: (0.0, k as f64 * 6.0),
            b: (101.0, k as f64 * 6.0),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        })
        .collect();
    let walls = m.add_walls(&specs).unwrap();
    for &w in &walls {
        for k in 0..100 {
            let o = crate::OpeningSpec {
                along: 0.6 + k as f64 * 1.0,
                width: 0.6,
                sill: 0.9,
                height: 1.2,
                kind: "window".into(),
            };
            m.add_opening_with(w, &o, crate::Swing::default()).unwrap();
        }
    }
    let t0 = std::time::Instant::now();
    let made = m.tag_all(lv, &[TagCat::Window], false).unwrap();
    let took = t0.elapsed();
    eprintln!("tag_all 2000 windows: {took:?}");
    assert_eq!(made.len(), 2000);
    assert!(took.as_secs_f64() < 10.0, "{took:?}");
    let mut r: Vec<(f64, f64, f64, f64)> = m
        .tags(lv)
        .unwrap()
        .iter()
        .map(|t| {
            let (w, h) = tag_size(t.cat, &t.text);
            let (x, y) = t.head;
            (x - w / 2.0, y - h / 2.0, x + w / 2.0, y + h / 2.0)
        })
        .collect();
    r.sort_by(|a, b| a.0.total_cmp(&b.0));
    for i in 0..r.len() {
        for j in i + 1..r.len() {
            if r[j].0 >= r[i].2 - 1e-9 {
                break;
            }
            let (p, q) = (r[i], r[j]);
            let apart =
                p.2 <= q.0 + 1e-9 || q.2 <= p.0 + 1e-9 || p.3 <= q.1 + 1e-9 || q.3 <= p.1 + 1e-9;
            assert!(apart, "{p:?} overlaps {q:?}");
        }
    }
}
