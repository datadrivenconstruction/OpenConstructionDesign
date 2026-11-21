// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Procedural plants: forms, levels of detail, seeds, the built-in species table.

use ddc_geom::component::{Built, Family, Lod};

const LIBRARY: &str = include_str!("../../model/data/library.ddcf");

fn text(form: &str, seed: i64, leaf: f64, h: f64, crown: f64, clear: f64) -> String {
    format!(
        "family T\ncategory planting\nhost floor\n\
         param Height length type {h}m\nparam Crown length type {crown}m\nparam Trunk length type 400mm\n\
         param Clear length type {clear}m\nparam Leaf number type {leaf}\nparam Seed int type {seed}\n\
         param Bark material type \"Bark\"\nparam Leaves material type \"Foliage\"\ntype \"X\" Seed={seed}\n\
         plant Stem part(wood) form({form}) height(Height) crown(Crown) trunk(Trunk) clear(Clear) leaf(Leaf) seed(Seed) mat(Bark) show(plan, 3d, elev)\n\
         plant Crown part(foliage) form({form}) height(Height) crown(Crown) trunk(Trunk) clear(Clear) leaf(Leaf) seed(Seed) mat(Leaves) show(plan, 3d, elev)\n"
    )
}

fn build(src: &str, lod: Lod) -> Built {
    let f = Family::parse(src).unwrap_or_else(|e| panic!("{e}"));
    let r = f.resolve(&f.family_type("X").unwrap().values, &[]).unwrap();
    f.build(&r, lod).unwrap_or_else(|e| panic!("{e}"))
}

const FORMS: [(&str, f64, f64, f64); 8] = [
    ("round", 12.0, 9.0, 3.0),
    ("oval", 14.0, 7.0, 3.0),
    ("conical", 18.0, 6.0, 1.5),
    ("columnar", 16.0, 3.5, 2.0),
    ("weeping", 12.0, 10.0, 1.0),
    ("palm", 11.0, 5.5, 7.0),
    ("vase", 15.0, 10.0, 4.0),
    ("spreading", 9.0, 12.0, 4.5),
];

#[test]
fn every_form_grows_within_its_size_at_every_level() {
    for (form, h, crown, clear) in FORMS {
        let src = text(form, 5, 1.0, h, crown, clear);
        let mut tris = vec![];
        for lod in [Lod::Coarse, Lod::Medium, Lod::Fine] {
            let b = build(&src, lod);
            let (lo, hi) = (b.bbox[0], b.bbox[1]);
            assert!(hi.z > 0.88 * h && hi.z <= h + 0.05, "{form} {lod:?}: top {}", hi.z);
            assert!(lo.z > -0.3, "{form} {lod:?}: bottom {}", lo.z);
            let wide = (hi.x - lo.x).max(hi.y - lo.y);
            assert!(wide > 0.45 * crown && wide < 1.45 * crown, "{form} {lod:?}: width {wide} of {crown}");
            assert!(b.volume > 0.0 && b.surface > 0.0, "{form} {lod:?}");
            assert!(b.parts.iter().filter(|p| p.visible).count() == 2, "{form} {lod:?}: stem and crown");
            // the trunk ring (8 edges coarse, 12 finer) and at least 20 edges of crown symbol
            let ring = if lod == Lod::Coarse { 8 } else { 12 };
            assert!(b.plan.len() >= ring + 20, "{form} {lod:?}: plan {}", b.plan.len());
            tris.push(b.mesh.triangles.len());
        }
        assert!(tris[0] < tris[1] && tris[1] < tris[2], "{form}: {tris:?}");
        // coarse is the crowd budget, crown lumps <= 120 plus a stub trunk <= 40 (see the
        // thousand-tree test); medium and fine are the near budgets of a full-size tree
        assert!(tris[0] <= 160 && tris[1] <= 2500 && tris[2] <= 10_000, "{form}: {tris:?}");
    }
}

#[test]
fn the_same_seed_gives_the_same_tree_and_another_seed_another() {
    for (form, h, crown, clear) in FORMS {
        let a = build(&text(form, 11, 1.0, h, crown, clear), Lod::Fine);
        let b = build(&text(form, 11, 1.0, h, crown, clear), Lod::Fine);
        let c = build(&text(form, 12, 1.0, h, crown, clear), Lod::Fine);
        assert_eq!(a.mesh.positions, b.mesh.positions, "{form}");
        assert_eq!(a.plan, b.plan, "{form}");
        assert_ne!(a.mesh.positions, c.mesh.positions, "{form}: seeds alike");
        assert_ne!(a.plan, c.plan, "{form}: plan outlines alike");
    }
}

#[test]
fn the_plan_outline_is_uneven_not_a_circle() {
    let b = build(&text("round", 3, 1.0, 12.0, 9.0, 3.0), Lod::Medium);
    let radii: Vec<f64> = b.plan.iter().map(|(p, _)| p.len()).filter(|r| *r > 1.0).collect();
    let (lo, hi) = radii.iter().fold((f64::MAX, f64::MIN), |a, r| (a.0.min(*r), a.1.max(*r)));
    assert!(radii.len() >= 20, "{}", radii.len());
    assert!(hi / lo > 1.12, "outline radii {lo}..{hi}");
    assert!(hi < 4.5 * 1.35 && hi > 4.5 * 0.7, "{hi}");
}

#[test]
fn a_bare_tree_keeps_its_crown_outline_and_shows_branches() {
    let leafy = build(&text("round", 4, 1.0, 12.0, 9.0, 3.0), Lod::Fine);
    let bare = build(&text("round", 4, 0.0, 12.0, 9.0, 3.0), Lod::Fine);
    assert_eq!(bare.parts.iter().filter(|p| p.visible).count(), 1);
    assert!(bare.plan.len() >= 12 + 40, "{}", bare.plan.len());
    assert!(bare.mesh.triangles.len() > 200 && bare.mesh.triangles.len() < leafy.mesh.triangles.len());
    assert!(bare.volume < leafy.volume);
}

#[test]
fn a_form_can_be_chosen_by_a_parameter() {
    let src = text("Form", 2, 1.0, 12.0, 9.0, 3.0).replace("param Seed", "param Form int type 0\nparam Seed");
    let f = Family::parse(&src).unwrap();
    let mut shapes = vec![];
    for code in 0..8 {
        let r = f
            .resolve(&[("Form".to_string(), ddc_geom::component::Value::Num(code as f64)), ("Seed".to_string(), ddc_geom::component::Value::Num(2.0))], &[])
            .unwrap();
        shapes.push(f.build(&r, Lod::Medium).unwrap().mesh.positions.len());
    }
    shapes.sort();
    shapes.dedup();
    assert!(shapes.len() >= 6, "{shapes:?}");
    let bad = f.resolve(&[("Form".to_string(), ddc_geom::component::Value::Num(9.0))], &[]).unwrap();
    assert!(f.build(&bad, Lod::Medium).is_err());
}

#[test]
fn a_malformed_plant_is_refused_with_its_reason() {
    let base = text("round", 1, 1.0, 12.0, 9.0, 3.0);
    let no_part = base.replace("part(wood) ", "");
    assert!(Family::parse(&no_part).unwrap_err().to_string().contains("part"));
    let low = base.replace("clear(Clear)", "clear(11m)");
    let f = Family::parse(&low).unwrap();
    let r = f.resolve(&f.family_type("X").unwrap().values, &[]).unwrap();
    assert!(f.build(&r, Lod::Medium).unwrap_err().to_string().contains("clear"));
}

#[test]
fn the_species_table_builds_every_type_and_covers_every_form() {
    let src = LIBRARY.split("\n---").find(|s| s.lines().any(|l| l.trim() == "family Tree")).unwrap();
    let f = Family::parse(src.trim_start_matches('\n')).unwrap();
    assert!(f.types.len() >= 150, "{}", f.types.len());
    let mut names: Vec<&str> = f.types.iter().map(|t| t.name.as_str()).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), f.types.len(), "type names repeat");
    let mut forms = std::collections::BTreeSet::new();
    let mut seeds = std::collections::BTreeSet::new();
    let mut base = 0;
    for t in &f.types {
        let r = f.resolve(&t.values, &[]).unwrap_or_else(|e| panic!("{}: {e}", t.name));
        forms.insert(r.nums["Form"].v as i64);
        // a season type of a species keeps the seed of its summer type: the same tree in other leaf
        let season = r.nums["Season"].v as i64;
        if season == 0 || season == 4 {
            seeds.insert(r.nums["Seed"].v as i64);
            base += 1;
        }
        let bare = r.nums["Leaf"].v < 0.05;
        for lod in [Lod::Coarse, Lod::Medium, Lod::Fine] {
            let b = f.build(&r, lod).unwrap_or_else(|e| panic!("{} {lod:?}: {e}", t.name));
            // wood, leaves and the tone solids (inner crown, lit lobes, streaks) together
            assert!(b.mesh.triangles.len() <= 10_000, "{} {lod:?}: {}", t.name, b.mesh.triangles.len());
            assert!(b.volume > 0.0, "{}", t.name);
            // every tree has foliage unless the type is bare
            let shown = b.parts.iter().filter(|p| p.visible).count();
            assert!(shown >= if bare { 1 } else { 2 } && shown <= 5, "{} {lod:?}: {shown} parts", t.name);
        }
    }
    assert_eq!(forms.len(), 8, "{forms:?}");
    assert_eq!(seeds.len(), base, "every species type has its own seed");
}

fn tree_family() -> Family {
    let src = LIBRARY.split("\n---").find(|s| s.lines().any(|l| l.trim() == "family Tree")).unwrap();
    Family::parse(src.trim_start_matches('\n')).unwrap()
}

fn tree(f: &Family, name: &str, lod: Lod) -> Built {
    let t = f.family_type(name).unwrap_or_else(|| panic!("no type {name}"));
    f.build(&f.resolve(&t.values, &[]).unwrap(), lod).unwrap()
}

/// Radii of the points of a closed outline, in order.
fn radii(lines: &[(ddc_geom::math::V2, ddc_geom::math::V2)]) -> Vec<f64> {
    lines.iter().map(|(p, _)| p.len()).collect()
}

/// Number of local maxima of a closed sequence.
fn peaks(r: &[f64]) -> usize {
    let n = r.len();
    (0..n).filter(|&k| r[k] > r[(k + n - 1) % n] && r[k] > r[(k + 1) % n]).count()
}

#[test]
fn the_plan_symbol_is_species_specific_at_every_level() {
    // trunk circle first, then the symbol of the crown
    let lens = |form: &str, h, crown, clear| -> Vec<usize> {
        let src = text(form, 5, 1.0, h, crown, clear);
        // the trunk circle has 8 edges at the coarse level, 12 at the finer ones
        [Lod::Coarse, Lod::Medium, Lod::Fine].iter().map(|l| build(&src, *l).plan.len() - if *l == Lod::Coarse { 8 } else { 12 }).collect()
    };
    // deciduous: scalloped outline (lobes x segments), branch spokes; finer, the inner edge on every
    // other lobe and the trunk dot (9 lines)
    assert_eq!(lens("round", 12.0, 9.0, 3.0), vec![24 + 6, 56 + 27 + 7 * 3 + 9, 90 + 39 + 9 * 4 + 9]);
    // conifer: star of 2k edges and k rays, finer with the trunk dot
    assert_eq!(lens("conical", 18.0, 6.0, 1.5), vec![21, 27 + 9, 33 + 9]);
    // palm: four edges per frond
    assert_eq!(lens("palm", 11.0, 5.5, 7.0), vec![24, 32, 40]);
}

#[test]
fn a_conifer_plan_is_a_star() {
    for (form, h, crown, clear) in [("conical", 18.0, 6.0, 1.5), ("columnar", 16.0, 3.5, 2.0)] {
        let b = build(&text(form, 5, 1.0, h, crown, clear), Lod::Fine);
        let star = radii(&b.plan[12..12 + 22]);
        assert_eq!(peaks(&star), 11, "{form}: eleven tips");
        let hi = star.iter().cloned().fold(0.0, f64::max);
        assert!(star.iter().step_by(2).all(|t| *t > 0.5 * hi * 0.9), "{form}");
        assert!(star.iter().skip(1).step_by(2).all(|v| *v < 0.7 * hi), "{form}: pinched between tips");
    }
}

#[test]
fn a_deciduous_plan_is_scalloped() {
    let b = build(&text("round", 3, 1.0, 12.0, 9.0, 3.0), Lod::Fine);
    let ring = radii(&b.plan[12..12 + 90]);
    assert!(peaks(&ring) >= 14, "{} bumps", peaks(&ring));
    // the joints between lobes sit lower than their middles
    let (lo, hi) = ring.iter().fold((f64::MAX, 0.0f64), |a, r| (a.0.min(*r), a.1.max(*r)));
    assert!(lo < 0.9 * hi, "{lo}..{hi}");
}

/// The lobes of a broadleaf outline differ in depth, as a hand draws them, and a dot inside the
/// trunk circle marks the stem above the coarse level.
#[test]
fn the_broadleaf_outline_has_uneven_lobes_and_a_trunk_dot() {
    for seed in [3, 8, 21] {
        let b = build(&text("round", seed, 1.0, 12.0, 9.0, 3.0), Lod::Fine);
        let ring = radii(&b.plan[12..12 + 90]);
        let dips: Vec<f64> = ring
            .chunks(5)
            .map(|c| {
                let (lo, hi) = c.iter().fold((f64::MAX, 0.0f64), |a, r| (a.0.min(*r), a.1.max(*r)));
                1.0 - lo / hi
            })
            .collect();
        let (lo, hi) = dips.iter().fold((f64::MAX, 0.0f64), |a, d| (a.0.min(*d), a.1.max(*d)));
        assert!(hi - lo > 0.08, "seed {seed}: lobe depths {lo:.3}..{hi:.3}");
    }
    // the trunk is 0.4 m: its circle at 0.2 m, the dot inside it
    for (form, h, crown, clear) in [("round", 12.0, 9.0, 3.0), ("conical", 18.0, 6.0, 1.5)] {
        let dot = |lod| build(&text(form, 5, 1.0, h, crown, clear), lod).plan.iter().filter(|(a, b)| a.len() < 0.12 && b.len() < 0.12).count();
        assert_eq!(dot(Lod::Coarse), 0, "{form}");
        assert_eq!(dot(Lod::Fine), 9, "{form}");
    }
}

/// The hedge plan shows its clipped top as a band along the length, inside the scalloped edges.
#[test]
fn a_hedge_plan_is_a_band() {
    let f = library_family("Hedge");
    let b = grown(&f, "Clipped 1200", 1.0, Lod::Fine);
    let band = b.plan.iter().filter(|(a, e)| (a.y - e.y).abs() < 1e-9 && (a.x - e.x).abs() > 2.9 && (a.y.abs() - 0.24).abs() < 1e-6).count();
    assert_eq!(band, 2, "{:?}", &b.plan[b.plan.len() - 4..]);
}

#[test]
fn a_palm_plan_is_a_rosette_of_fronds() {
    let b = build(&text("palm", 5, 1.0, 11.0, 5.5, 7.0), Lod::Fine);
    let fronds = &b.plan[12..];
    assert_eq!(fronds.len(), 40);
    let tips: Vec<f64> = fronds.chunks(4).map(|c| c[1].1.len()).collect();
    assert_eq!(tips.len(), 10);
    // each frond is a leaf: root, a side point at about half its length, the tip, the other side
    for c in fronds.chunks(4) {
        assert!(c[0].0.len() < 0.4 && c[1].1.len() > 2.0 && c[0].1.len() < c[1].1.len(), "{c:?}");
    }
    assert!(tips.iter().all(|t| *t > 0.6 * 2.75 && *t < 1.35 * 2.75), "{tips:?}");
}

#[test]
fn the_plan_symbol_follows_the_crown_size() {
    let edge = |crown: f64| radii(&build(&text("conical", 5, 1.0, 18.0, crown, 1.5), Lod::Fine).plan[12..12 + 22]).iter().cloned().fold(0.0, f64::max);
    let ratio = edge(12.0) / edge(6.0);
    assert!(ratio > 1.7 && ratio < 2.3, "{ratio}");
}

#[test]
fn the_coarse_crown_is_lobed_not_a_cylinder_or_barrel() {
    use ddc_geom::component::plant::{grow, Form, Organ, Spec};
    for (form, h, crown, clear) in [("round", 12.0, 9.0, 3.0), ("oval", 14.0, 7.0, 3.0), ("weeping", 12.0, 10.0, 1.0), ("vase", 15.0, 10.0, 4.0), ("spreading", 9.0, 12.0, 4.5), ("columnar", 16.0, 3.5, 2.0)] {
        let f = Form::from_name(form).unwrap();
        let spec = Spec { form: f, height: h, crown, trunk: 0.4, clear, leaf: 1.0, seed: 5 };
        let g = grow(&spec, Organ::Foliage, Lod::Coarse);
        assert!(g.tris.len() <= 120 && g.tris.len() >= 60, "{form}: {}", g.tris.len());
        // a lathed barrel has whole rings of vertices at one height; lumps do not
        let mut level = std::collections::HashMap::new();
        for p in &g.pos {
            *level.entry((p.z * 1e6).round() as i64).or_insert(0) += 1;
        }
        let ring = level.values().copied().max().unwrap();
        assert!(ring <= 6, "{form}: {ring} vertices on one level");
        // the crown reaches the top and is wider than the spindle alone
        let top = g.pos.iter().map(|p| p.z).fold(0.0, f64::max);
        assert!(top > clear + 0.9 * (h - clear), "{form}: top {top}");
    }
}

#[test]
fn a_species_in_every_season_is_the_same_tree_with_other_leaf() {
    let f = tree_family();
    for sp in ["Oak A", "Birch B", "Cherry A", "Magnolia A"] {
        let summer = tree(&f, sp, Lod::Fine);
        let autumn = tree(&f, &format!("{sp} Autumn"), Lod::Fine);
        let bare = tree(&f, &format!("{sp} Bare"), Lod::Fine);
        let spring = tree(&f, &format!("{sp} Spring"), Lod::Fine);
        let wood = |b: &Built| b.parts.iter().find(|p| p.name == "Stem").unwrap().volume;
        // the wood of autumn is the same seed, so the same trunk and branches (a bare tree grows more twigs)
        assert!((wood(&summer) - wood(&autumn)).abs() < 1e-9, "{sp}");
        let colour = |b: &Built| b.parts.iter().find(|p| p.name == "Crown").map(|p| p.material.clone());
        assert_ne!(colour(&summer), colour(&autumn), "{sp}: autumn leaf colour");
        assert_ne!(colour(&autumn), colour(&spring), "{sp}: spring leaf colour");
        // winter is bare: no leaf part shows; the wood and its bark streaks stay (see the streak test)
        let shown: Vec<&str> = bare.parts.iter().filter(|p| p.visible).map(|p| p.name.as_str()).collect();
        assert_eq!(shown, ["Stem", "Streak"], "{sp}: winter is bare");
        // young leaf is thinner than summer's
        assert!(spring.mesh.triangles.len() <= summer.mesh.triangles.len(), "{sp}");
        assert!(bare.mesh.triangles.len() < summer.mesh.triangles.len(), "{sp}");
    }
    // evergreens stand in all seasons: one type, no autumn or bare variants
    assert!(f.family_type("Spruce A").is_some() && f.family_type("Spruce A Bare").is_none());
}

#[test]
fn species_vary_in_size_and_leaf_colour_by_data() {
    let f = tree_family();
    let mut mats = std::collections::BTreeSet::new();
    let mut sizes = std::collections::BTreeSet::new();
    for t in &f.types {
        let r = f.resolve(&t.values, &[]).unwrap();
        mats.insert(r.texts["Leaves"].clone());
        sizes.insert((r.nums["Height"].v * 10.0).round() as i64);
    }
    assert!(mats.len() >= 8, "{mats:?}");
    assert!(sizes.len() >= 40, "{}", sizes.len());
}

fn weld(g: &ddc_geom::component::plant::Grown) -> (Vec<[usize; 3]>, f64) {
    let mut ids = std::collections::HashMap::new();
    let key = |p: &ddc_geom::V3| [(p.x * 1e5).round() as i64, (p.y * 1e5).round() as i64, (p.z * 1e5).round() as i64];
    let id: Vec<usize> = g
        .pos
        .iter()
        .map(|p| {
            let n = ids.len();
            *ids.entry(key(p)).or_insert(n)
        })
        .collect();
    let tris: Vec<[usize; 3]> = g.tris.iter().map(|t| [id[t[0] as usize], id[t[1] as usize], id[t[2] as usize]]).collect();
    let vol = g
        .tris
        .iter()
        .map(|t| {
            let (a, b, c) = (g.pos[t[0] as usize], g.pos[t[1] as usize], g.pos[t[2] as usize]);
            (a.x * (b.y * c.z - b.z * c.y) - a.y * (b.x * c.z - b.z * c.x) + a.z * (b.x * c.y - b.y * c.x)) / 6.0
        })
        .sum();
    (tris, vol)
}

#[test]
fn every_organ_is_a_closed_mesh_with_outward_normals_at_every_level() {
    use ddc_geom::component::plant::{grow, Form, Organ, Spec};
    for (form, h, crown, clear) in FORMS {
        for leaf in [1.0, 0.4] {
            let spec = Spec { form: Form::from_name(form).unwrap(), height: h, crown, trunk: 0.4, clear, leaf, seed: 7 };
            for organ in [Organ::Wood, Organ::Foliage, Organ::Shade, Organ::Light, Organ::Streak] {
                for lod in [Lod::Coarse, Lod::Medium, Lod::Fine] {
                    let g = grow(&spec, organ, lod);
                    // a tone organ a level or form leaves out is empty: no mesh to close
                    // (its absence is asserted by the inner crown and lit lobe tests)
                    if g.tris.is_empty() && matches!(organ, Organ::Shade | Organ::Light | Organ::Streak) {
                        continue;
                    }
                    let (tris, vol) = weld(&g);
                    // closed: each directed edge has its opposite, so no hole and no flipped face
                    let mut edges: std::collections::HashMap<(usize, usize), i32> = Default::default();
                    for t in &tris {
                        for k in 0..3 {
                            let (a, b) = (t[k], t[(k + 1) % 3]);
                            if a != b {
                                *edges.entry((a.min(b), a.max(b))).or_insert(0) += if a < b { 1 } else { -1 };
                            }
                        }
                    }
                    let open = edges.values().filter(|v| **v != 0).count();
                    assert_eq!(open, 0, "{form} leaf {leaf} {organ:?} {lod:?}: {open} open or flipped edges");
                    // outward: a positive signed volume that matches the organ's volume
                    assert!(vol > 0.0, "{form} {organ:?} {lod:?}: inward normals, volume {vol}");
                }
            }
        }
    }
}

#[test]
fn the_coarse_crown_follows_the_foliage_share() {
    use ddc_geom::component::plant::{grow, Form, Organ, Spec};
    let vol = |leaf: f64| {
        let spec = Spec { form: Form::from_name("round").unwrap(), height: 12.0, crown: 9.0, trunk: 0.4, clear: 3.0, leaf, seed: 5 };
        weld(&grow(&spec, Organ::Foliage, Lod::Coarse)).1
    };
    let (full, spring, bare) = (vol(1.0), vol(0.5), vol(0.25));
    assert!(spring < 0.8 * full && bare < 0.8 * spring, "full {full:.1} spring {spring:.1} bare {bare:.1}");
}

#[test]
fn a_thousand_trees_are_built_coarse_and_stay_within_the_triangle_and_segment_budget() {
    // a thousand plants resolve to coarse (model test `a_crowd_of_plants...`), so coarse is measured
    let lod = Lod::Coarse;
    let f = tree_family();
    // coarse budget per tree: crown lumps <= 120 triangles, a stub trunk <= 40, trunk circle + outline + spokes
    let (mut tris, mut segs) = (0, 0);
    for n in ["Oak A", "Linden A", "Spruce A", "Birch A", "Palm A"] {
        if f.family_type(n).is_none() {
            continue;
        }
        let b = tree(&f, n, lod);
        let (t, s) = (b.mesh.triangles.len(), b.plan.len());
        assert!(t <= 160 && s <= 40, "{n}: {t} triangles, {s} segments");
        tris = tris.max(t);
        segs = segs.max(s);
    }
    assert!(tris > 0);
    assert!(tris * 1000 <= 160_000 && segs * 1000 <= 40_000);
}

#[test]
fn the_inner_crown_is_a_dark_mass_inside_the_foliage_of_broadleaf_trees_only() {
    use ddc_geom::component::plant::{grow, Form, Organ, Spec};
    for (form, h, crown, clear) in FORMS {
        let spec = Spec { form: Form::from_name(form).unwrap(), height: h, crown, trunk: 0.4, clear, leaf: 1.0, seed: 3 };
        let tris = |lod| grow(&spec, Organ::Shade, lod).tris.len();
        let broadleaf = !matches!(form, "conical" | "palm");
        assert_eq!(tris(Lod::Coarse), 0, "{form}");
        assert_eq!(tris(Lod::Medium) > 0, broadleaf, "{form}");
        assert_eq!(tris(Lod::Fine) > tris(Lod::Medium), broadleaf, "{form}");
        // it lies within the foliage box, so it never grows the tree
        let (fo, sh) = (grow(&spec, Organ::Foliage, Lod::Fine), grow(&spec, Organ::Shade, Lod::Fine));
        let top = |g: &ddc_geom::component::plant::Grown| g.pos.iter().map(|p| p.z).fold(f64::MIN, f64::max);
        let wide = |g: &ddc_geom::component::plant::Grown| g.pos.iter().map(|p| p.x.abs().max(p.y.abs())).fold(0.0, f64::max);
        if broadleaf {
            assert!(top(&sh) < top(&fo) && wide(&sh) < wide(&fo), "{form}");
        }
    }
    // a bare tree has no inner crown
    let bare = Spec { form: Form::from_name("round").unwrap(), height: 12.0, crown: 9.0, trunk: 0.4, clear: 3.0, leaf: 0.0, seed: 3 };
    assert_eq!(grow(&bare, Organ::Shade, Lod::Fine).tris.len(), 0);
}

#[test]
fn bark_and_inner_crown_colours_follow_the_species_and_the_season() {
    let f = tree_family();
    let mats = |name: &str| {
        let b = tree(&f, name, Lod::Fine);
        let m = &b.parts;
        (m[0].material.clone(), m[1].material.clone(), m.get(2).map(|p| p.material.clone()))
    };
    let (birch, oak, spruce, palm) = (mats("Birch A"), mats("Oak A"), mats("Spruce A"), mats("Date Palm A"));
    let barks: std::collections::BTreeSet<_> = [&birch.0, &oak.0, &spruce.0, &palm.0].iter().map(|s| s.to_string()).collect();
    assert_eq!(barks.len(), 4, "{barks:?}");
    assert_eq!(birch.0, "Bark Birch");
    // the inner crown is the darker twin of the leaf colour, in every season
    assert_eq!(oak.2.as_deref(), Some("Foliage Shade"));
    let autumn = mats("Maple A Autumn");
    assert_eq!((autumn.1.as_str(), autumn.2.as_deref()), ("Foliage Red", Some("Foliage Red Shade")));
    // the bark of a species is the same in every season
    assert_eq!(mats("Oak A Autumn").0, oak.0);
}

#[test]
fn lit_lobes_are_the_outer_and_top_ones_and_streaks_stay_on_the_clear_trunk() {
    use ddc_geom::component::plant::{grow, Form, Grown, Organ, Spec};
    let top = |g: &Grown| g.pos.iter().map(|p| p.z).fold(f64::MIN, f64::max);
    let wide = |g: &Grown| g.pos.iter().map(|p| p.x.hypot(p.y)).fold(0.0, f64::max);
    for (form, h, crown, clear) in FORMS {
        let spec = Spec { form: Form::from_name(form).unwrap(), height: h, crown, trunk: 0.4, clear, leaf: 1.0, seed: 3 };
        let broadleaf = !matches!(form, "conical" | "palm");
        let n = |o, lod| grow(&spec, o, lod).tris.len();
        // lit lobes: a share of the foliage, none at the coarse level or on a conifer or palm
        assert_eq!(n(Organ::Light, Lod::Coarse), 0, "{form}");
        assert_eq!(n(Organ::Light, Lod::Fine) > 0, broadleaf, "{form}");
        if broadleaf {
            let (fo, li) = (grow(&spec, Organ::Foliage, Lod::Fine), grow(&spec, Organ::Light, Lod::Fine));
            assert!(li.tris.len() < fo.tris.len(), "{form}: every lobe is lit");
            // a hair larger than the foliage, never a new silhouette
            assert!(top(&li) <= top(&fo) * 1.06 && wide(&li) <= wide(&fo) * 1.06, "{form}");
            // the lit lobes sit high or outside: their mean height or reach beats the foliage's
            let mean = |g: &Grown, f: &dyn Fn(&ddc_geom::V3) -> f64| g.pos.iter().map(|p| f(p)).sum::<f64>() / g.pos.len() as f64;
            let (hz, hr) = (mean(&li, &|p| p.z) - mean(&fo, &|p| p.z), mean(&li, &|p| p.x.hypot(p.y)) - mean(&fo, &|p| p.x.hypot(p.y)));
            assert!(hz > 0.0 || hr > 0.0, "{form}: lit lobes are neither higher nor wider ({hz:.2} {hr:.2})");
        }
        // streaks: none at coarse, more at fine, all below the trunk top
        assert_eq!(n(Organ::Streak, Lod::Coarse), 0, "{form}");
        assert!(n(Organ::Streak, Lod::Fine) > n(Organ::Streak, Lod::Medium) && n(Organ::Streak, Lod::Medium) > 0, "{form}");
        let (st, wd) = (grow(&spec, Organ::Streak, Lod::Fine), grow(&spec, Organ::Wood, Lod::Fine));
        assert!(top(&st) <= top(&wd), "{form}: a streak above the trunk");
        // measured from the trunk's own axis: a palm trunk leans by design
        let off = st
            .pos
            .iter()
            .map(|p| {
                let a = ddc_geom::component::plant::trunk_axis(&spec, p.z);
                (p.x - a.x).hypot(p.y - a.y)
            })
            .fold(0.0, f64::max);
        assert!(off <= 0.4, "{form}: a streak {off} m off the axis");
    }
    // a bare tree has no lit lobes but keeps its bark streaks
    let bare = Spec { form: Form::from_name("round").unwrap(), height: 12.0, crown: 9.0, trunk: 0.4, clear: 3.0, leaf: 0.0, seed: 3 };
    assert_eq!(grow(&bare, Organ::Light, Lod::Fine).tris.len(), 0);
    assert!(grow(&bare, Organ::Streak, Lod::Fine).tris.len() > 0);
}

#[test]
fn every_species_has_three_leaf_tones_and_a_bark_streak_from_a_shared_material_set() {
    let f = tree_family();
    let mut used = std::collections::BTreeSet::new();
    let mut checked = 0;
    let twin = |m: &str, tone: &str| if m == "Foliage Green" { format!("Foliage {tone}") } else { format!("{m} {tone}") };
    for t in &f.types {
        let b = tree(&f, &t.name, Lod::Fine);
        let m: Vec<&str> = b.parts.iter().map(|p| p.material.as_str()).collect();
        used.extend(m.iter().map(|s| s.to_string()));
        // wood, leaf, inner, lit, streak: the other tones are twins of the leaf and bark
        assert_eq!(m.len(), 5, "{}: {m:?}", t.name);
        assert_eq!(m[2], twin(m[1], "Shade"), "{}", t.name);
        assert_eq!(m[3], twin(m[1], "Light"), "{}", t.name);
        assert_eq!(m[4], format!("{} Streak", m[0]), "{}", t.name);
        checked += 1;
    }
    assert!(checked > 100, "{checked} types");
    // the draw-call set stays small: the materials of the whole table are shared by species
    assert!(used.len() <= 60, "{} materials", used.len());
}

#[test]
fn every_lobe_meets_the_core_or_a_branch_tip_at_every_level_and_form() {
    use ddc_geom::component::plant::{anatomy, Form, Spec};
    for (form, h, crown, clear) in FORMS {
        let broadleaf = !matches!(form, "conical" | "palm");
        for leaf in [1.0, 0.5, 0.1] {
            let spec = Spec { form: Form::from_name(form).unwrap(), height: h, crown, trunk: 0.4, clear, leaf, seed: 9 };
            let mut counts = vec![];
            for lod in [Lod::Coarse, Lod::Medium, Lod::Fine] {
                let a = anatomy(&spec, lod);
                counts.push(a.lobes.len());
                assert_eq!(!a.lobes.is_empty(), broadleaf, "{form} {lod:?}");
                for (i, l) in a.lobes.iter().enumerate() {
                    let ok = match (&a.core, lod) {
                        (Some(core), Lod::Coarse) => core.meets(l),
                        _ => a.tips.iter().any(|t| l.contains(*t)),
                    };
                    assert!(ok, "{form} leaf {leaf} {lod:?}: lobe {i} at {:?} floats", l.c);
                }
            }
            if broadleaf {
                // fewer drawn lobes the coarser the level
                assert!(counts[0] <= counts[1] && counts[1] <= counts[2], "{form} {counts:?}");
            }
        }
    }
}

fn library_family(name: &str) -> Family {
    let head = format!("family {name}");
    let src = LIBRARY
        .split("\n---")
        .map(|s| s.trim_start_matches('\n'))
        .find(|s| s.lines().any(|l| l.trim() == head))
        .unwrap_or_else(|| panic!("no {name}"));
    Family::parse(src).unwrap_or_else(|e| panic!("{e}"))
}

fn grown(f: &Family, ty: &str, scale: f64, lod: Lod) -> Built {
    let mut v = f.family_type(ty).unwrap().values.clone();
    v.push(("Scale".to_string(), ddc_geom::component::Value::Num(scale)));
    let r = f.resolve(&v, &[]).unwrap_or_else(|e| panic!("{e}"));
    f.build(&r, lod).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn a_hedge_is_trimmed_with_an_irregular_top_and_a_scalloped_plan() {
    let f = library_family("Hedge");
    let b = grown(&f, "Clipped 1200", 1.0, Lod::Fine);
    // six cuts, each a base and a narrower top: not one box
    // a box is 12 triangles: 12 prisms give at most 144 (shared faces may be merged), well over a box
    let tris = b.mesh.triangles.len();
    assert!(tris >= 6 * 12 && tris <= 12 * 12, "{tris}");
    assert!(b.plan.len() > 40, "scalloped edges and ends: {}", b.plan.len());
    // the tops stand at different heights along the length
    let mut tops: Vec<i64> = vec![];
    for k in 0..6 {
        let x = -1.5 + 0.5 * k as f64 + 0.25;
        // a cut's corners lie on its ends, shared in x with the neighbour: pick its faces by centroid
        let p = &b.mesh.positions;
        let z = b
            .mesh
            .triangles
            .iter()
            .filter(|t| ((p[t[0] as usize].x + p[t[1] as usize].x + p[t[2] as usize].x) / 3.0 - x).abs() < 0.24)
            .flat_map(|t| t.iter().map(|&i| p[i as usize].z))
            .fold(0.0, f64::max);
        tops.push((z * 1e4).round() as i64);
    }
    tops.sort();
    tops.dedup();
    assert!(tops.len() >= 4, "{tops:?}");
    let hi = *tops.last().unwrap() as f64 / 1e4;
    let lo = *tops.first().unwrap() as f64 / 1e4;
    // the clipped height is 1.2 m: the tallest cut within 8 % above it, the shortest below the tallest
    assert!(hi > 1.2 * 0.95 && hi < 1.2 * 1.12 && lo < hi, "{lo}..{hi}");
    // the scale sizes it and reshuffles the tops
    let big = grown(&f, "Clipped 1200", 1.2, Lod::Fine);
    assert!(big.bbox[1].z > b.bbox[1].z * 1.05);
    assert_ne!(big.mesh.positions.len(), 0);
    assert_ne!(b.mesh.positions, big.mesh.positions);
}

#[test]
fn shrubs_of_one_type_differ_by_scale_and_are_clustered_not_boxes() {
    let f = library_family("Shrub");
    let a = grown(&f, "Round 1", 0.9, Lod::Medium);
    let b = grown(&f, "Round 1", 1.1, Lod::Medium);
    assert!(a.mesh.triangles.len() > 100, "{}", a.mesh.triangles.len());
    assert!(b.bbox[1].z > a.bbox[1].z * 1.1);
    // another scale reseeds the lumps: not the same shape stretched
    let norm = |m: &Built, s: f64| m.mesh.positions.iter().map(|p| ((p.x / s * 1e3).round() as i64, (p.y / s * 1e3).round() as i64)).collect::<std::collections::BTreeSet<_>>();
    assert_ne!(norm(&a, 0.9), norm(&b, 1.1));
    assert!(a.plan.len() > 30, "{}", a.plan.len());
}

/// A park of 500 trees, the eight forms in equal shares, stays inside the render budget: a
/// chunk holds 400 000 triangles (`CHUNK_TRIS`). Everywhere coarse is a small fraction of it,
/// everywhere medium fits two chunks, and the way the renderer mixes the levels by distance (a
/// tenth of the trees near and full, three tenths medium, the rest coarse; beyond that boxes)
/// fits two chunks and weighs under a third of a park drawn full everywhere.
#[test]
fn a_park_of_500_trees_stays_inside_the_triangle_budget_at_every_level() {
    const CHUNK_TRIS: usize = 400_000;
    let per_form: Vec<[usize; 3]> = FORMS
        .iter()
        .map(|&(form, h, crown, clear)| {
            let src = text(form, 5, 1.0, h, crown, clear);
            [Lod::Coarse, Lod::Medium, Lod::Fine].map(|l| build(&src, l).mesh.triangles.len())
        })
        .collect();
    let trees = 500;
    let each = |k: usize| per_form.iter().map(|t| t[k]).sum::<usize>() * trees / per_form.len();
    let (coarse, medium, fine) = (each(0), each(1), each(2));
    assert!(coarse * 4 <= CHUNK_TRIS, "coarse park {coarse}");
    assert!(medium <= 2 * CHUNK_TRIS, "medium park {medium}");
    assert!(coarse < medium && medium < fine, "{coarse} {medium} {fine}");
    let mixed = fine / 10 + medium * 3 / 10 + coarse * 6 / 10;
    assert!(mixed <= 2 * CHUNK_TRIS, "mixed park {mixed}");
    assert!(mixed * 3 <= fine, "mixed {mixed} against full {fine}");
}


/// Trees of the species table in full leaf are built in detail near the eye: fine 6..10k
/// triangles, medium 1.5..2.5k; coarse stays the crowd mesh.
#[test]
fn near_trees_reach_their_triangle_budgets() {
    use ddc_geom::component::Value;
    let f = tree_family();
    let num = |t: &ddc_geom::component::FamilyType, k: &str| match t.values.iter().find(|(n, _)| n == k) {
        Some((_, Value::Num(v))) => *v,
        _ => 0.0,
    };
    let (mut fine, mut medium, mut n) = ((usize::MAX, 0), (usize::MAX, 0), 0);
    for t in &f.types {
        // palms and small trees keep the light build; a bare tree has no leaf patches
        if num(t, "Leaf") < 0.99 || num(t, "Form") == 5.0 || num(t, "Height") < 6.0 || num(t, "Crown") < 3.0 {
            continue;
        }
        let c = tree(&f, &t.name, Lod::Coarse).mesh.triangles.len();
        let m = tree(&f, &t.name, Lod::Medium).mesh.triangles.len();
        let x = tree(&f, &t.name, Lod::Fine).mesh.triangles.len();
        assert!(c <= 160, "{} coarse {c}", t.name);
        eprintln!("{}: {x}/{m}/{c}", t.name);
        fine = (fine.0.min(x), fine.1.max(x));
        medium = (medium.0.min(m), medium.1.max(m));
        n += 1;
    }
    assert!(n > 20, "{n} trees");
    assert!(fine.0 >= 6000 && fine.1 <= 10_000, "fine {fine:?}");
    assert!(medium.0 >= 1500 && medium.1 <= 2500, "medium {medium:?}");
}
