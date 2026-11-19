// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Storeys of different heights: a tall level under normal ones. Elevations add up the heights
//! below, every stair rises its own storey, and each storey's structure stands at its height.

use ddc_gen::project::{build_project, parse_spec, GProject, ProjectSpec};
use serde_json::{json, Value as J};

fn spec(v: &J) -> ProjectSpec {
    parse_spec(v).unwrap_or_else(|e| panic!("{v}: {e:?}"))
}

fn built(v: &J) -> (ProjectSpec, GProject) {
    let s = spec(v);
    let p = build_project(&s).unwrap_or_else(|e| panic!("{v}: {e}"));
    (s, p)
}

fn tall_ground() -> J {
    json!({"typology": "office", "floors": 4, "storey_heights": [8.0, null, null, 4.5]})
}

#[test]
fn the_elevations_add_up_the_heights_below() {
    let (s, p) = built(&tall_ground());
    let fh = s.floor_height;
    let want = [8.0, fh, fh, 4.5];
    let heights: Vec<f64> = p.storeys.iter().map(|st| st.height).collect();
    assert_eq!(heights, want);
    let mut z = 0.0;
    for (k, st) in p.storeys.iter().enumerate() {
        assert!((st.elevation - z).abs() < 1e-9, "storey {k}: {} vs {z}", st.elevation);
        assert_eq!(st.layout.height, st.height, "storey {k}: the layout stands at its own height");
        z += want[k];
    }
    assert!((p.roof.elevation - z).abs() < 1e-9, "the roof stands on the last storey: {} vs {z}", p.roof.elevation);
    assert!((s.building_height() - z).abs() < 1e-9);
    assert_eq!(s.storey_elevation(2), 8.0 + fh);
}

#[test]
fn without_heights_every_storey_is_the_floor_height() {
    let (s, p) = built(&json!({"typology": "office", "floors": 3}));
    assert!(s.storey_heights.is_empty());
    for (k, st) in p.storeys.iter().enumerate() {
        assert_eq!(st.height, s.floor_height);
        assert!((st.elevation - k as f64 * s.floor_height).abs() < 1e-9);
    }
}

#[test]
fn each_stair_rises_the_height_of_its_own_storey() {
    let (s, p) = built(&tall_ground());
    let mut risers = vec![];
    for (k, st) in p.storeys.iter().enumerate().take(3) {
        assert!(!st.layout.stairs.is_empty(), "storey {k} has no stair");
        for g in &st.layout.stairs {
            assert_eq!(g.height, st.height, "storey {k}: the stair is defined at the storey height");
            let lay = g.def().layout().unwrap_or_else(|e| panic!("storey {k}: {e:?}"));
            assert!((lay.riser * lay.risers as f64 - st.height).abs() < 1e-6, "storey {k}: {} risers of {}", lay.risers, lay.riser);
            assert!(lay.riser <= s.max_riser + 1e-9, "storey {k}: riser {} over {}", lay.riser, s.max_riser);
            assert_eq!(lay.risers, (st.height / s.max_riser - 1e-9).ceil() as usize, "storey {k}");
            risers.push(lay.risers);
        }
    }
    // the 8 m storey needs more risers than a 3.6 m one, the equal storeys the same number
    assert!(risers[0] > risers[1], "{risers:?}");
    assert_eq!(risers[1], risers[2], "{risers:?}");
    // the top storey has no stair above it
    assert!(p.storeys[3].layout.stairs.is_empty());
}

#[test]
fn the_slab_openings_and_columns_follow_each_storey() {
    let (_, p) = built(&tall_ground());
    for k in 1..p.storeys.len() {
        assert!(!p.storeys[k].layout.slab_holes.is_empty(), "storey {k}: the stair opening is cut in its slab");
        assert!(!p.storeys[k].layout.slab.is_empty(), "storey {k}");
    }
    // storeys of one plan stand on the same columns whatever their height
    assert_eq!(p.storeys[1].layout.columns, p.storeys[2].layout.columns);
    assert!(!p.storeys[0].layout.columns.is_empty());
    // the walls of a tall storey stand at its height and openings stay under their head
    for st in &p.storeys {
        for o in &st.layout.openings {
            assert!(o.sill + o.height <= st.height + 1e-9, "{} + {} over {}", o.sill, o.height, st.height);
        }
    }
}

#[test]
fn the_heights_are_checked() {
    let bad = |h: J| parse_spec(&json!({"typology": "office", "floors": 3, "storey_heights": h})).is_err();
    assert!(bad(json!([4.0, 4.0, 4.0, 4.0])), "more heights than storeys");
    assert!(bad(json!([1.0])), "under the least floor height");
    assert!(bad(json!([40.0])), "over typology.tall_space_max");
    assert!(bad(json!("tall")), "not a list");
    assert!(!bad(json!([12.0])), "a tall ground storey under normal ones");
    assert!(!bad(json!([null, 3.0, null])));
    // the rule moves the limit
    let v = json!({"typology": "office", "floors": 3, "storey_heights": [20.0], "rules": {"typology.tall_space_max": 25.0}});
    assert!(parse_spec(&v).is_ok());
}

#[test]
fn a_tall_ground_storey_keeps_the_building_in_one_piece() {
    // a tall level at the bottom of a lifted, stacked building: the lift serves the real height
    let tall = spec(&json!({"typology": "office", "floors": 12, "storey_heights": [10.0]}));
    let plain = spec(&json!({"typology": "office", "floors": 12}));
    assert!(ddc_gen::project::lift::serve_height(&tall) > ddc_gen::project::lift::serve_height(&plain) + 6.0);
    let p = build_project(&tall).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(p.storeys.len(), 12);
    assert!((p.storeys[1].elevation - 10.0).abs() < 1e-9);
}

fn tall_theatre() -> J {
    json!({"typology": "office", "floors": 3, "storey_heights": [14.0, 4.0, 4.0],
           "floor_height": 4.0, "design": {"facade": "curtain"}})
}

#[test]
fn walls_curtains_and_columns_stand_at_their_own_level_height() {
    let (_, p) = built(&tall_theatre());
    let want = [14.0, 4.0, 4.0];
    let z = [0.0, 14.0, 18.0];
    for (k, st) in p.storeys.iter().enumerate() {
        assert_eq!(st.height, want[k], "storey {k}");
        assert_eq!(st.layout.height, want[k], "storey {k}: the walls");
        assert!((st.elevation - z[k]).abs() < 1e-9, "storey {k}");
        assert!(!st.curtains.is_empty(), "storey {k}: curtain wall");
        for c in &st.curtains {
            assert!((c.height - want[k]).abs() < 1e-9, "storey {k}: curtain {} of {}", c.height, want[k]);
            assert!(c.spandrel < c.height, "storey {k}: spandrel {} under the wall {}", c.spandrel, c.height);
        }
        // a window head stays under its own storey's ceiling
        for o in st.layout.openings.iter().filter(|o| !o.door) {
            assert!(o.sill + o.height <= want[k] + 1e-9, "storey {k}");
        }
    }
    // the spandrel band is one thing at every level while the vision rows grow with the height
    let sp = |k: usize| p.storeys[k].curtains.iter().map(|c| c.spandrel).fold(0.0, f64::max);
    assert!((sp(1) - sp(2)).abs() < 1e-9);
    // a 14 m column is thicker than a 4 m one (slenderness), the 4 m storeys share a section
    let sect = |k: usize| p.storeys[k].layout.columns.iter().map(|c| c.section.0).fold(0.0, f64::max);
    assert!(sect(0) > sect(1), "{} vs {}", sect(0), sect(1));
    assert_eq!(sect(1), sect(2));
}

#[test]
fn horizontal_runs_and_risers_follow_the_levels() {
    let (_, p) = built(&json!({"typology": "office", "floors": 3, "storey_heights": [14.0, 4.0, 4.0],
                               "floor_height": 4.0, "services": {"on": true}}));
    let top = p.roof.elevation;
    assert!((top - 22.0).abs() < 1e-9);
    // every run lies inside the building and a level-bound run inside its own level's band
    let runs = p.services.elec.iter().chain(&p.services.water);
    for r in runs {
        let (lo, hi) = (p.storeys[r.storey].elevation, p.storeys[r.storey].elevation + p.storeys[r.storey].height);
        let flat = r.points.windows(2).all(|w| (w[0].2 - w[1].2).abs() < 1e-9);
        for q in &r.points {
            assert!(q.2 > -1.0 && q.2 < top + 1.0, "z {} outside the building", q.2);
            if flat {
                assert!(q.2 >= lo - 0.5 && q.2 <= hi + 0.5, "storey {}: z {} outside {lo}..{hi}", r.storey, q.2);
            }
        }
    }
    // the fire risers reach the top level, the stairs rise the top-level height only
    for r in &p.fire.runs {
        assert!(r.run.points.iter().all(|q| q.2 < top + 1.0), "a fire run above the roof");
    }
    assert_eq!(p.storeys[0].layout.stairs[0].height, 14.0);
    assert_eq!(p.storeys[1].layout.stairs[0].height, 4.0);
}

#[test]
fn the_levels_reach_the_model_with_their_elevations() {
    let (_, p) = built(&tall_theatre());
    let z: Vec<f64> = p.storeys.iter().map(|s| s.elevation).chain([p.roof.elevation]).collect();
    assert_eq!(z, vec![0.0, 14.0, 18.0, 22.0], "the plan's storey and roof levels, the base of the IFC storeys");
    let gaps: Vec<f64> = z.windows(2).map(|w| w[1] - w[0]).collect();
    assert_eq!(gaps, vec![14.0, 4.0, 4.0]);
}

#[test]
fn the_building_height_is_held_to_the_height_rule() {
    let v = json!({"typology": "office", "floors": 3, "storey_heights": [14.0, 4.0, 4.0],
                   "rules": {"massing.height_max": 20.0}});
    let e = parse_spec(&v).unwrap_err();
    assert!(format!("{e:?}").contains("massing.height_max"), "{e:?}");
    let ok = json!({"typology": "office", "floors": 3, "storey_heights": [14.0, 4.0, 4.0],
                    "rules": {"massing.height_max": 22.0}});
    assert!(parse_spec(&ok).is_ok());
}

#[test]
fn the_duct_band_of_a_tall_level_hangs_under_its_own_ceiling() {
    let v = json!({"typology": "office", "floors": 3, "storey_heights": [10.0, 4.0, 4.0], "floor_height": 4.0,
                   "services": {"hvac": true}});
    let (_, p) = built(&v);
    let mut seen = false;
    for r in p.hvac.ducts.iter().filter(|r| r.storey < 3) {
        let st = &p.storeys[r.storey];
        for q in r.points.iter().filter(|_| r.points.windows(2).all(|w| (w[0].2 - w[1].2).abs() < 1e-9)) {
            assert!(q.2 > st.elevation && q.2 < st.elevation + st.height, "storey {}: z {} outside {}", r.storey, q.2, st.elevation);
            if r.storey == 0 {
                seen |= q.2 > 5.0;
            }
        }
    }
    assert!(seen || p.hvac.ducts.is_empty(), "the 10 m level's ducts hang near its own ceiling");
}
