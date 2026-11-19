// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! One more storey on top of a built tower: every storey and basement already there keeps its
//! slot block, so a revision keeps their element references, and the new storey takes a block
//! no element of the old model held.

use ddc_gen::project::slots::{self, Floor};
use ddc_gen::project::{build_project, merge_patch, parse_spec, GProject};
use serde_json::{json, Value as J};
use std::collections::{BTreeMap, BTreeSet};

fn built(v: &J) -> GProject {
    let s = parse_spec(v).unwrap_or_else(|e| panic!("{e}"));
    build_project(&s).unwrap_or_else(|e| panic!("{e}"))
}

/// Block of every storey (`s<k>`) and basement (`b<k>`).
fn floor_blocks(p: &GProject) -> BTreeMap<String, u32> {
    let n = p.storeys.len();
    p.blocks()
        .enumerate()
        .map(|(i, (k, _))| (if i < n { format!("s{i}") } else { format!("b{}", i - n) }, k))
        .collect()
}

fn add_one(v: &J) -> (BTreeMap<String, u32>, BTreeMap<String, u32>) {
    let old = built(v);
    let mut w = v.clone();
    merge_patch(&mut w, &json!({"floors": old.storeys.len() + 1}));
    let new = built(&w);
    assert_eq!(new.storeys.len(), old.storeys.len() + 1);
    (floor_blocks(&old), floor_blocks(&new))
}

#[test]
fn a_storey_on_top_keeps_every_block_of_the_floors_and_basements_below() {
    // a tower whose storeys reach the basements' low blocks, and one past the original layout
    for (floors, basements) in [(37, 2), (38, 2), (39, 1), (40, 0)] {
        let v = json!({"typology": "office", "floors": floors, "basements": basements});
        let (old, new) = add_one(&v);
        for (f, b) in &old {
            assert_eq!(new.get(f), Some(b), "{floors}+{basements}: {f} moved from block {b}");
        }
        let top = &format!("s{floors}");
        let held: BTreeSet<u32> = old.values().copied().collect();
        assert!(!held.contains(&new[top]), "{floors}+{basements}: the new storey reuses block {}", new[top]);
        eprintln!("add storey {floors}+{basements}: {} floors kept, new block {}", old.len(), new[top]);
    }
}

#[test]
fn storey_and_basement_blocks_never_meet_and_name_their_floor() {
    for basements in 0..=3usize {
        for floors in [1usize, 36, 37, 38, 39, 40, 41, 120, slots::MAX_STOREYS as usize - basements] {
            let mut seen = BTreeSet::new();
            for k in 0..floors {
                let blk = slots::storey_block(k, basements);
                assert!(blk < slots::MAX_STOREYS && seen.insert(blk), "{floors}+{basements}: storey {k}");
                assert_eq!(slots::floor_of(blk, basements), Some(Floor::Storey(k)));
            }
            for b in 0..basements {
                let blk = slots::basement_block(b);
                assert!(seen.insert(blk), "{floors}+{basements}: basement {b} meets a storey");
                assert_eq!(slots::floor_of(blk, basements), Some(Floor::Basement(b)));
            }
        }
    }
    // the roof slot is not in any storey's block
    let roof = slots::ROOF;
    assert!((0..slots::MAX_STOREYS).all(|k| !(slots::block_base(k)..slots::block_base(k) + slots::PER_STOREY).contains(&roof)));
}
