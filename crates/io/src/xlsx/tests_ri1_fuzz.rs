// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Seeded fuzz of by-key row matching in the XLSX import: random books built from a 300-room
//! fixture (blank ids, duplicate and renamed keys, deleted rows, random Accept/Reject/pending
//! decisions) are driven through preview, apply, delete_missing and undo. A failure names the seed.

use std::path::PathBuf;

use calamine::Data;
use ddc_model::{Model, RoomSpec};

use super::tests::{edit, set, tmp};
use super::*;

type Grid = Vec<Vec<Data>>;

const ROOMS_N: usize = 300;
const SEEDS: u64 = 12;
const SEEDS_LONG: u64 = 500;

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng((seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        (self.next() >> 11) % n
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Act {
    Keep,
    Rename,
    Blank,
    BlankNewKey,
    Delete,
    DupCopy,
    BlankAndCopy,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Dec {
    Pending,
    Accept,
    Reject,
}

/// A book row whose id cell is empty.
struct Blank {
    /// Row of the sheet, 1-based.
    row: u32,
    /// Number of elements carrying the number written in the row.
    hits: usize,
    /// The only element carrying that number, if exactly one does.
    element: Option<i64>,
    dec: Dec,
}

struct Case {
    book: PathBuf,
    src: PathBuf,
    dec: KeyDecisions,
    blanks: Vec<Blank>,
}

impl Drop for Case {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.book);
        let _ = std::fs::remove_file(&self.src);
    }
}

/// 300 rooms; every 25th room shares its number with the next one (ambiguous key).
fn fixture() -> (Model, Vec<i64>, Vec<String>) {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let mut ids = vec![];
    let mut numbers = vec![];
    for i in 0..ROOMS_N {
        let number = if i % 25 == 0 && i + 1 < ROOMS_N {
            format!("D{i:03}")
        } else if i % 25 == 1 && i > 0 {
            format!("D{:03}", i - 1)
        } else {
            format!("R{i:03}")
        };
        let id = m
            .add_room(&RoomSpec {
                level: lv,
                name: format!("Room {i}"),
                number: number.clone(),
                point: (i as f64, 1.0),
                height: 3.0,
            })
            .unwrap();
        ids.push(id);
        numbers.push(number);
    }
    (m, ids, numbers)
}

fn text(s: String) -> Data {
    Data::String(s)
}

fn pick_dec(rng: &mut Rng) -> Dec {
    match rng.below(4) {
        0 => Dec::Accept,
        1 => Dec::Reject,
        _ => Dec::Pending,
    }
}

fn setup(seed: u64, tag: &str) -> (Model, Case) {
    let (m, ids, numbers) = fixture();
    let mut rng = Rng::new(seed);
    let acts: Vec<Act> = (0..ROOMS_N)
        .map(|_| match rng.below(100) {
            0..=54 => Act::Keep,
            55..=64 => Act::Rename,
            65..=79 => Act::Blank,
            80..=84 => Act::BlankNewKey,
            85..=89 => Act::Delete,
            90..=95 => Act::DupCopy,
            _ => Act::BlankAndCopy,
        })
        .collect();

    let hits_of = |key: &str| numbers.iter().filter(|n| n.as_str() == key).count();
    let element_of = |key: &str| {
        let at: Vec<usize> = (0..ROOMS_N).filter(|&i| numbers[i] == key).collect();
        (at.len() == 1).then(|| ids[at[0]])
    };

    let mut blanks: Vec<Blank> = vec![];
    let mut dec = KeyDecisions::default();
    let mut add_blank = |row: u32, key: &str, rng: &mut Rng, blanks: &mut Vec<Blank>| {
        let d = pick_dec(rng);
        match d {
            Dec::Accept => {
                dec.accept.insert(("Rooms".into(), row));
            }
            Dec::Reject => {
                dec.reject.insert(("Rooms".into(), row));
            }
            Dec::Pending => {}
        }
        blanks.push(Blank { row, hits: hits_of(key), element: element_of(key), dec: d });
    };

    // Grid index k is sheet row k + 1 (row 0 is the header); deleted rows shift the rest up.
    let mut k = 1usize;
    let mut copies: Vec<usize> = vec![];
    for (i, act) in acts.iter().enumerate() {
        let row = (k + 1) as u32;
        match act {
            Act::Keep | Act::Rename => k += 1,
            Act::Blank | Act::BlankAndCopy => {
                add_blank(row, &numbers[i], &mut rng, &mut blanks);
                if *act == Act::BlankAndCopy {
                    copies.push(i);
                }
                k += 1;
            }
            Act::BlankNewKey => {
                add_blank(row, &format!("N{seed}_{i}"), &mut rng, &mut blanks);
                k += 1;
            }
            Act::Delete => {}
            Act::DupCopy => {
                copies.push(i);
                k += 1;
            }
        }
    }
    // Copies are appended after the last kept row, in order.
    for (j, i) in copies.iter().enumerate() {
        add_blank((k + j + 1) as u32, &numbers[*i], &mut rng, &mut blanks);
    }

    let src = tmp(&format!("kf_{tag}_{seed}_src"));
    export_xlsx(&m, &src).unwrap();
    let book = tmp(&format!("kf_{tag}_{seed}_dst"));
    edit(&src, &book, |sheet, g: &mut Grid| {
        if sheet != "Rooms" {
            return;
        }
        let orig = g.clone();
        let mut out: Grid = vec![orig[0].clone()];
        let mut tail: Grid = vec![];
        for (i, act) in acts.iter().enumerate() {
            let mut h: Grid = vec![orig[0].clone(), orig[i + 1].clone()];
            match act {
                Act::Keep => out.push(h.remove(1)),
                Act::Rename => {
                    set(&mut h, 1, "name", text(format!("Renamed {i}")));
                    out.push(h.remove(1));
                }
                Act::Blank | Act::BlankAndCopy => {
                    set(&mut h, 1, "id", Data::Empty);
                    set(&mut h, 1, "name", text(format!("Blank {i}")));
                    if *act == Act::BlankAndCopy {
                        let mut c = h.clone();
                        set(&mut c, 1, "name", text(format!("Copy {i}")));
                        tail.push(c.remove(1));
                    }
                    out.push(h.remove(1));
                }
                Act::BlankNewKey => {
                    set(&mut h, 1, "id", Data::Empty);
                    set(&mut h, 1, "number", text(format!("N{seed}_{i}")));
                    set(&mut h, 1, "name", text(format!("Fresh {i}")));
                    out.push(h.remove(1));
                }
                Act::Delete => {}
                Act::DupCopy => {
                    let mut c = h.clone();
                    set(&mut c, 1, "id", Data::Empty);
                    set(&mut c, 1, "name", text(format!("Copy {i}")));
                    tail.push(c.remove(1));
                    out.push(h.remove(1));
                }
            }
        }
        out.extend(tail);
        *g = out;
    });
    (m, Case { book, src, dec, blanks })
}

fn opts(apply: bool, delete_missing: bool) -> ImportOptions {
    ImportOptions { apply, delete_missing, new_params: false }
}

fn snapshot(m: &Model) -> Vec<String> {
    let mut st = m
        .sql()
        .prepare("SELECT id,level,name,number,px,py,height FROM room ORDER BY id")
        .unwrap();
    let rows = st
        .query_map([], |r| {
            Ok(format!(
                "{}|{}|{}|{}|{:?}|{:?}|{:?}",
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, f64>(4)?,
                r.get::<_, f64>(5)?,
                r.get::<_, f64>(6)?
            ))
        })
        .unwrap();
    rows.map(|r| r.unwrap()).collect()
}

fn ops(m: &Model) -> i64 {
    m.sql().query_row("SELECT count(*) FROM op", [], |r| r.get(0)).unwrap()
}

fn exists(m: &Model, id: i64) -> bool {
    m.sql().query_row("SELECT count(*) FROM room WHERE id=?1", [id], |r| r.get::<_, i64>(0)).unwrap() > 0
}

fn deleted_ids(rep: &Report) -> Vec<i64> {
    rep.changes.iter().filter(|c| c.kind() == "deleted").filter_map(|c| c.id).collect()
}

fn preview_equals_apply(seed: u64, dm: bool, tag: &str) -> Result<(), String> {
    let bad = |s: String| format!("seed {seed} delete_missing {dm}: {s}");
    let (mut m, case) = setup(seed, tag);
    let before = snapshot(&m);
    let pv = import_xlsx_keyed(&mut m, &case.book, opts(false, dm), &case.dec).map_err(&bad)?;
    if snapshot(&m) != before {
        return Err(bad("the preview changed the model".into()));
    }
    let ap = import_xlsx_keyed(&mut m, &case.book, opts(true, dm), &case.dec).map_err(&bad)?;
    if !pv.rejected.is_empty() || !ap.rejected.is_empty() || !ap.applied {
        return Err(bad(format!("rejected {:?} / {:?}, applied {}", pv.rejected, ap.rejected, ap.applied)));
    }
    if pv.counts() != ap.counts() {
        return Err(bad(format!("counts {:?} != {:?}", pv.counts(), ap.counts())));
    }
    let key = |r: &Report| r.matches.iter().map(|k| (k.row, k.id, k.state, k.changes.len())).collect::<Vec<_>>();
    if key(&pv) != key(&ap) || pv.pending_matches() != ap.pending_matches() {
        return Err(bad("matches differ between preview and apply".into()));
    }
    if (pv.changes.len(), pv.edits.len(), pv.ignored.len()) != (ap.changes.len(), ap.edits.len(), ap.ignored.len()) {
        return Err(bad("changes, edits or ignored differ between preview and apply".into()));
    }
    let c = ap.counts();
    let expect = before.len() + c.added - c.deleted;
    let after = snapshot(&m).len();
    if after != expect {
        return Err(bad(format!("{after} rooms after apply, the counts promise {expect}")));
    }
    Ok(())
}

fn no_matched_element_deleted(seed: u64, dm: bool, tag: &str) -> Result<(), String> {
    let bad = |s: String| format!("seed {seed} delete_missing {dm}: {s}");
    let (mut m, case) = setup(seed, tag);
    let pv = import_xlsx_keyed(&mut m, &case.book, opts(false, dm), &case.dec).map_err(&bad)?;
    let deleted = deleted_ids(&pv);
    for km in &pv.matches {
        if deleted.contains(&km.id) {
            return Err(bad(format!("room {} is matched by row {} and deleted", km.id, km.row)));
        }
    }
    // Rejected row: the user said the row is not that element, so the element must survive.
    for b in case.blanks.iter().filter(|b| b.dec == Dec::Reject) {
        if let Some(id) = b.element {
            if deleted.contains(&id) {
                return Err(bad(format!("room {id} was matched by rejected row {} and is deleted", b.row)));
            }
        }
    }
    // A row whose key hits an element but which is neither matched nor rejected says why.
    for b in &case.blanks {
        let matched = pv.matches.iter().any(|k| k.row == b.row);
        if b.hits == 0 || b.dec == Dec::Reject || matched {
            continue;
        }
        if !pv.ignored.iter().any(|i| i.sheet == "Rooms" && i.row == b.row && !i.reason.is_empty()) {
            return Err(bad(format!("row {} ({} hits) is unmatched and has no reason", b.row, b.hits)));
        }
    }
    let ap = import_xlsx_keyed(&mut m, &case.book, opts(true, dm), &case.dec).map_err(&bad)?;
    if !ap.applied {
        return Err(bad(format!("not applied: {:?}", ap.rejected)));
    }
    for km in &ap.matches {
        if !exists(&m, km.id) {
            return Err(bad(format!("matched room {} is gone after apply", km.id)));
        }
    }
    for b in case.blanks.iter().filter(|b| b.dec == Dec::Reject) {
        if let Some(id) = b.element {
            if !exists(&m, id) {
                return Err(bad(format!("room {id} of rejected row {} is gone after apply", b.row)));
            }
        }
    }
    Ok(())
}

fn undo_restores(seed: u64, dm: bool, tag: &str) -> Result<(), String> {
    let bad = |s: String| format!("seed {seed} delete_missing {dm}: {s}");
    let (mut m, case) = setup(seed, tag);
    let before = snapshot(&m);
    let ops0 = ops(&m);
    let ap = import_xlsx_keyed(&mut m, &case.book, opts(true, dm), &case.dec).map_err(&bad)?;
    if !ap.applied {
        return Err(bad(format!("not applied: {:?}", ap.rejected)));
    }
    let step = ops(&m) - ops0;
    if step != 1 {
        return Err(bad(format!("the import recorded {step} operations, expected one undo step")));
    }
    if snapshot(&m) == before {
        return Err(bad("the import changed nothing".into()));
    }
    m.undo().map_err(|e| bad(e.to_string()))?;
    let after = snapshot(&m);
    if after.len() != before.len() {
        return Err(bad(format!("{} rooms after undo, {} before", after.len(), before.len())));
    }
    if after != before {
        let at = after.iter().zip(&before).position(|(a, b)| a != b).unwrap_or(0);
        return Err(bad(format!("undo left a different value at row {at}: {} vs {}", after[at], before[at])));
    }
    Ok(())
}

fn sweep(seeds: std::ops::Range<u64>, tag: &str, f: fn(u64, bool, &str) -> Result<(), String>) {
    let fails: Vec<String> = seeds
        .flat_map(|s| [false, true].map(move |dm| (s, dm)))
        .filter_map(|(s, dm)| f(s, dm, tag).err())
        .collect();
    assert!(
        fails.is_empty(),
        "{} failing runs, first: {}",
        fails.len(),
        fails.iter().take(5).cloned().collect::<Vec<_>>().join(" ;; ")
    );
}

#[test]
fn key_fuzz_preview_equals_apply() {
    sweep(0..SEEDS, "pe", preview_equals_apply);
}

// A row the user rejected keeps its element even with delete_missing on.
#[test]
fn key_fuzz_no_matched_element_deleted() {
    sweep(0..SEEDS, "nd", no_matched_element_deleted);
}

#[test]
fn key_fuzz_undo_restores() {
    sweep(0..SEEDS, "un", undo_restores);
}

#[test]
#[ignore = "long: 500 seeds"]
fn key_fuzz_500_seeds() {
    sweep(0..SEEDS_LONG, "l1", preview_equals_apply);
    sweep(0..SEEDS_LONG, "l2", no_matched_element_deleted);
    sweep(0..SEEDS_LONG, "l3", undo_restores);
}
