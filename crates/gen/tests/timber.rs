//! The timber structure: a glulam post-and-beam frame by the span rules, CLT floor panels cut
//! at the panel width, the concrete core kept, and the share of each material in the types.

use ddc_gen::project::rules;
use ddc_gen::project::structure::{self, TypeDef};
use ddc_gen::project::timber::{self, GTimber};
use ddc_gen::project::{build_project, parse_spec, GProject, ProjectSpec, Structure};
use ddc_gen::WallRole;
use serde_json::json;

fn build(structure: &str) -> (ProjectSpec, GProject) {
    let v = json!({"typology": "apartment_section", "floors": 6, "design": {"structure": structure}, "features": []});
    let s = parse_spec(&v).unwrap_or_else(|e| panic!("{structure}: {e}"));
    let p = build_project(&s).unwrap_or_else(|e| panic!("{structure}: {e}"));
    (s, p)
}

fn rule(s: &ProjectSpec, id: &str) -> f64 {
    rules::scoped(&s.rules, || rules::rule(id))
}

fn share(t: &TypeDef, word: &str) -> f64 {
    t.layers.iter().filter(|l| l.0.to_lowercase().contains(word)).map(|l| l.1).sum()
}

fn frame() -> (ProjectSpec, GProject, GTimber) {
    let (s, p) = build("timber_frame");
    let t = timber::design(&s, &p);
    (s, p, t)
}

#[test]
fn timber_is_mostly_timber_round_a_concrete_core() {
    for id in [Structure::Timber, Structure::TimberFrame] {
        let sys = structure::system(id);
        assert!(share(&sys.floor, "timber") >= 0.7, "{:?} floor", id);
        assert!(share(&sys.roof, "timber") >= 0.7, "{:?} roof", id);
        assert!(share(sys.wall(WallRole::Party), "timber") >= 0.7, "{:?} party wall", id);
        assert!(share(sys.wall(WallRole::Exterior), "timber") > 0.0, "{:?} exterior wall", id);
        // the core spine is the only concrete wall
        assert!((share(sys.wall(WallRole::Corridor), "concrete") - 1.0).abs() < 1e-9, "{:?} core", id);
        for r in [WallRole::Exterior, WallRole::Party, WallRole::Partition] {
            assert!(share(sys.wall(r), "concrete") < 1e-9, "{:?} {r:?} has concrete", id);
        }
        assert!(sys.column_material.to_lowercase().contains("glulam"));
    }
    for id in [Structure::Concrete, Structure::Masonry, Structure::Frame] {
        let sys = structure::system(id);
        assert!(share(&sys.floor, "timber") < 1e-9, "{:?}", id);
    }
    let t = timber::design(&build("timber_frame").0, &build("timber_frame").1);
    assert!(t.material.to_lowercase().contains("glulam"), "{}", t.material);
}

#[test]
fn only_the_timber_frame_gets_beams_and_every_span_is_within_the_rule() {
    for other in ["concrete", "masonry", "frame"] {
        let (s, p) = build(other);
        assert!(timber::design(&s, &p).is_empty(), "{other}");
    }
    let (s, p) = build("timber");
    let t = timber::design(&s, &p);
    assert!(t.beams.is_empty() && !t.panels.is_empty(), "bearing timber: panels only");
    let (s, p, t) = frame();
    assert!(t.beams.len() >= p.storeys.len() * 2, "{} beams", t.beams.len());
    let max = rule(&s, "timber.beam_span_max");
    let ratio = rule(&s, "timber.span_depth");
    assert!(t.beams.iter().any(|b| b.primary) && t.beams.iter().any(|b| !b.primary));
    for b in &t.beams {
        assert!(b.span() <= max + 1e-9, "beam of {:.2} m over the rule {max}", b.span());
        assert!(b.section.1 + 1e-9 >= b.span() / ratio || (b.section.1 - 0.88).abs() < 1e-9, "{b:?}");
        let h = p.storeys[b.storey].height;
        assert!(b.z > 0.0 && b.z < h, "beam height {} in a storey of {h}", b.z);
    }
    // a beam joins two columns of its own storey
    for b in &t.beams {
        let cols = &p.storeys[b.storey].layout.columns;
        let on = |q: (f64, f64)| cols.iter().any(|c| (c.at.0 - q.0).abs() < 1e-6 && (c.at.1 - q.1).abs() < 1e-6);
        assert!(on(b.a) && on(b.b), "{b:?}");
    }
}

#[test]
fn the_floor_is_cut_into_equal_panels_no_wider_than_the_rule() {
    let (s, p, t) = frame();
    let w = rule(&s, "timber.panel_w");
    for k in 0..p.storeys.len() {
        let ps: Vec<_> = t.panels.iter().filter(|x| x.storey == k).collect();
        assert!(ps.len() >= 2, "storey {k}: {} panels", ps.len());
        let widths: Vec<f64> = ps
            .iter()
            .map(|x| {
                let (dx, dy) = (x.rect[2] - x.rect[0], x.rect[3] - x.rect[1]);
                dx.min(dy)
            })
            .collect();
        // the strip width is the cut width, the other side the span across the floor
        let cut = widths.iter().cloned().fold(f64::MAX, f64::min);
        assert!(cut <= w + 1e-9, "storey {k}: panel {cut} m over the rule {w}");
        assert!(widths.iter().all(|x| (x - cut).abs() < 1e-6), "storey {k}: unequal panels {widths:?}");
    }
}

#[test]
fn the_panels_make_up_the_floor_and_its_holes() {
    let (s, p, t) = frame();
    let w = rule(&s, "timber.panel_w");
    for (k, st) in p.storeys.iter().enumerate() {
        let ps: Vec<_> = t.panels.iter().filter(|x| x.storey == k).collect();
        let slab = &st.layout.slab;
        let (x0, x1) = slab.iter().fold((f64::MAX, f64::MIN), |a, q| (a.0.min(q.0), a.1.max(q.0)));
        let (y0, y1) = slab.iter().fold((f64::MAX, f64::MIN), |a, q| (a.0.min(q.1), a.1.max(q.1)));
        let long = (x1 - x0).max(y1 - y0);
        // panels x their width = the floor across its longer side, each within the rule
        let each = ps.iter().map(|x| (x.rect[2] - x.rect[0]).min(x.rect[3] - x.rect[1])).fold(f64::MAX, f64::min);
        assert!(each <= w + 1e-9 && ps.len() as f64 >= long / w - 1e-9, "storey {k}: {} panels of {each} m for {long} m", ps.len());
        assert!((ps.len() as f64 * each - long).abs() < 1e-6, "storey {k}: {} x {each} is not {long}", ps.len());
        // and they cover the slab exactly, holes included
        let floor = timber::area(slab) - st.layout.slab_holes.iter().map(|h| timber::area(h)).sum::<f64>();
        let made: f64 = ps.iter().map(|x| timber::area(&x.outer) - x.holes.iter().map(|h| timber::area(h)).sum::<f64>()).sum();
        assert!((made - floor).abs() < 1e-6, "storey {k}: panels {made} m2, floor {floor} m2");
    }
}

#[test]
fn a_wider_panel_rule_makes_fewer_panels() {
    let base = json!({"typology": "apartment_section", "floors": 3, "design": {"structure": "timber_frame"}, "features": []});
    let count = |w: f64| {
        let mut v = base.clone();
        v["rules"] = json!({"timber.panel_w": w});
        let s = parse_spec(&v).unwrap_or_else(|e| panic!("{e}"));
        let p = build_project(&s).unwrap_or_else(|e| panic!("{e}"));
        timber::design(&s, &p).panels.len()
    };
    assert!(count(1.2) > count(3.0));
}

#[test]
fn beam_sections_grow_with_the_span() {
    let (a, b) = (timber::section_for(3.0), timber::section_for(8.0));
    assert!(a.1 < b.1, "{a:?} {b:?}");
    assert!(timber::section_for(1000.0).1 >= b.1);
}
