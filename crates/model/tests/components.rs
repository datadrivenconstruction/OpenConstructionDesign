// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Components: library, quantities, hosts, modification, scene with shared meshes.

use std::f64::consts::{FRAC_PI_2, PI};

use ddc_model::{
    library, CompHost, CompSpec, Error, LayerSpec, Lod, Model, ParamValue, Placement, Target,
    WallSpec,
};

fn near(a: f64, b: f64, tol: f64) {
    assert!((a - b).abs() <= tol, "{a} vs {b}");
}

fn near3(a: (f64, f64, f64), b: (f64, f64, f64)) {
    near(a.0, b.0, 1e-9);
    near(a.1, b.1, 1e-9);
    near(a.2, b.2, 1e-9);
}

struct Fx {
    m: Model,
    level: i64,
    wall: i64,
}

fn fx() -> Fx {
    let mut m = Model::open(":memory:").unwrap();
    let level = m.add_level("L1", 3.0).unwrap();
    let wt = m
        .add_wall_type(
            "W200",
            &[LayerSpec {
                material: "Brick".into(),
                thickness: 0.2,
            }],
        )
        .unwrap();
    let wall = m
        .add_wall(&WallSpec {
            a: (0.0, 0.0),
            b: (4.0, 0.0),
            level,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        })
        .unwrap();
    Fx { m, level, wall }
}

fn ctype(m: &mut Model, family: &str, ty: &str) -> i64 {
    let f = m.load_library_family(family).unwrap();
    m.comp_type_id(f, ty).unwrap().unwrap()
}

/// A digit, optional blanks, then `in`, `ft` or `"` not followed by a letter.
fn imperial_name(s: &str) -> bool {
    let c: Vec<char> = s.chars().collect();
    (0..c.len()).any(|i| {
        if !c[i].is_ascii_digit() {
            return false;
        }
        let mut j = i + 1;
        while j < c.len() && c[j] == ' ' {
            j += 1;
        }
        let rest: String = c[j..].iter().collect();
        let unit = ["in", "ft", "\""].into_iter().find(|u| rest.starts_with(u));
        unit.is_some_and(|u| !rest[u.len()..].starts_with(|x: char| x.is_ascii_alphabetic()))
    })
}

#[test]
fn imperial_names_are_units_not_spelling() {
    for s in ["60x30in", "Wide 6 in", "120in", "Queen 60x80in", "2.75x4.5in", "8ft", "36\""] {
        assert!(imperial_name(s), "{s}");
    }
    for s in ["String", "Single", "Spreading 1", "Lilac", "Round 1", "4 inlets"] {
        assert!(!imperial_name(s), "{s}");
    }
}

#[test]
fn library_families_build_at_every_detail_level() {
    let lib = library().unwrap();
    // every `family` head of the catalog parses into one family, none lost or merged
    let heads = include_str!("../data/library.ddcf").lines().filter(|l| l.starts_with("family ")).count();
    assert!(heads > 90, "{heads}");
    assert_eq!(lib.len(), heads);
    let names: Vec<&str> = lib.iter().map(|f| f.name.as_str()).collect();
    for n in [
        "Table",
        "Chair",
        "Desk",
        "Bed",
        "Wardrobe",
        "Sofa",
        "WC",
        "Washbasin",
        "Bathtub",
        "Kitchen Counter",
        "Sink",
        "Ceiling Light",
        "Radiator",
    ] {
        assert!(names.contains(&n), "{n}");
    }
    // at least one type sized in inches or feet (a number then in, ft or ", as in "60x30in" or
    // "Wide 6 in"; "String" or "Lilac" are words); every family without one is named at once
    let metric_only: Vec<&str> = lib
        .iter()
        .filter(|f| f.name != "Generic Box" && !f.types.iter().any(|t| imperial_name(&t.name)))
        .map(|f| f.name.as_str())
        .collect();
    assert!(metric_only.is_empty(), "no imperial type: {metric_only:?}");
    for f in &lib {
        assert!(f.types.len() >= 2 || f.name == "Generic Box", "{}", f.name);
        for t in &f.types {
            let r = f.resolve(&t.values, &[]).unwrap();
            for lod in [Lod::Coarse, Lod::Medium, Lod::Fine] {
                let b = f
                    .build(&r, lod)
                    .unwrap_or_else(|e| panic!("{} {}: {e}", f.name, t.name));
                assert!(
                    b.volume > 0.0 && !b.mesh.triangles.is_empty(),
                    "{} {}",
                    f.name,
                    t.name
                );
                assert!(!b.plan.is_empty(), "{} {} plan symbol", f.name, t.name);
                // the mesh is oriented outward
                assert!(b.mesh.volume() > 0.0, "{} {}", f.name, t.name);
            }
        }
    }
}

#[test]
fn table_1600x800_quantities() {
    let Fx { mut m, level, .. } = fx();
    let t = ctype(&mut m, "Table", "1600x800");
    let mut s = CompSpec::at(level, t, 5.0, 2.0);
    s.rotation = FRAC_PI_2;
    let id = m.add_component(&s).unwrap();
    let q = m.component_quantities(id).unwrap();
    near(q.volume, 1.6 * 0.8 * 0.04 + 4.0 * 0.06 * 0.06 * 0.71, 1e-15);
    near(q.volume, 0.061424, 1e-15);
    // rotated by 90°: extent 0.8 × 1.6, bottom at the level elevation
    near3(q.bbox[0], (4.6, 1.2, 3.0));
    near3(q.bbox[1], (5.4, 2.8, 3.75));
    assert!(m.check_integrity().unwrap().is_empty());
    let sched = m.component_schedule().unwrap();
    assert_eq!(sched.len(), 1);
    let seats = sched[0].params.iter().find(|p| p.0 == "Seats").unwrap();
    assert_eq!(seats.2, ParamValue::Num(4.0));
    assert_eq!(sched[0].category, "furniture");
    // plan: rectangle symbol
    assert_eq!(m.components_plan(level, Lod::Medium).unwrap().len(), 4);
    assert_eq!(m.schema_version().unwrap(), ddc_model::SCHEMA_VERSION);
}

#[test]
fn washbasin_revolve_volume() {
    let Fx {
        mut m, level, wall, ..
    } = fx();
    let t = ctype(&mut m, "Washbasin", "500");
    let mut s = CompSpec::at(level, t, 1.0, 0.1);
    s.host = CompHost::Wall(wall);
    let id = m.add_component(&s).unwrap();
    let (_, _, b) = m.component_built(id, Lod::Fine).unwrap();
    let (r, h, sh) = (0.25f64, 0.18f64, 0.015f64);
    let bowl = PI * (sh * r * r + (h - sh) * (r * r - (r - sh) * (r - sh)));
    near(b.parts[0].volume, bowl, 1e-15);
    let bracket = 0.12 * 0.03 * (0.18 - 0.02);
    near(
        m.component_quantities(id).unwrap().volume,
        bowl + bracket,
        1e-15,
    );
}

#[test]
fn wall_hosted_component_follows_wall() {
    let Fx {
        mut m, level, wall, ..
    } = fx();
    let t = ctype(&mut m, "Washbasin", "500");
    let mut s = CompSpec::at(level, t, 1.0, 0.1);
    s.host = CompHost::Wall(wall);
    s.values = vec![("Rim".into(), ParamValue::Num(0.8))];
    let id = m.add_component(&s).unwrap();
    let p0 = m.component_quantities(id).unwrap().placement;
    near3(p0.origin, (1.0, 0.1, 3.0));
    m.move_elements(&[Target::Wall(wall)], (1.0, 2.0)).unwrap();
    let p = m.component_quantities(id).unwrap().placement;
    near3(p.origin, (2.0, 2.1, 3.0));
    near(p.rotation, 0.0, 1e-12);
    m.rotate_elements(&[Target::Wall(wall)], (0.0, 0.0), FRAC_PI_2, false)
        .unwrap();
    let p = m.component_quantities(id).unwrap().placement;
    near3(p.origin, (-2.1, 2.0, 3.0));
    near(p.rotation, FRAC_PI_2, 1e-12);
    m.undo().unwrap();
    m.undo().unwrap();
    near3(
        m.component_quantities(id).unwrap().placement.origin,
        p0.origin,
    );
    // the component itself runs along the wall
    m.move_elements(&[Target::Component(id)], (0.5, 0.3))
        .unwrap();
    let sp = m.component(id).unwrap();
    near(sp.at.0, 1.5, 1e-12);
    near(sp.at.1, 0.4, 1e-12);
    assert!(m
        .move_elements(&[Target::Component(id)], (10.0, 0.0))
        .is_err());
    assert!(m
        .rotate_elements(&[Target::Component(id)], (0.0, 0.0), 1.0, false)
        .is_err());
    // a wall copy carries the component; deleting the wall removes it, undo restores it
    let copies = m
        .copy_elements(&[Target::Wall(wall)], &[(0.0, 5.0)])
        .unwrap();
    assert_eq!(m.components(Some(level)).unwrap().len(), 2);
    let Target::Wall(w2) = copies[0] else {
        panic!()
    };
    m.delete_wall(w2).unwrap();
    assert_eq!(m.components(None).unwrap(), vec![id]);
    m.undo().unwrap();
    assert_eq!(m.components(None).unwrap().len(), 2);
    assert!(m.check_integrity().unwrap().is_empty());
    // a host is required for a wall-mounted family and not allowed for floor furniture
    let free = CompSpec::at(level, t, 0.0, 0.0);
    assert!(m.add_component(&free).is_err());
    let table = ctype(&mut m, "Table", "1200x800");
    let mut bad = CompSpec::at(level, table, 0.0, 0.0);
    bad.host = CompHost::Wall(wall);
    assert!(m.add_component(&bad).is_err());
    let mut off = s.clone();
    off.at.0 = 4.5;
    assert!(m.add_component(&off).is_err());
}

fn reflect_x(p: (f64, f64, f64)) -> (f64, f64, f64) {
    (-p.0, p.1, p.2)
}

#[test]
fn mirror_reflects_geometry_exactly() {
    let Fx {
        mut m, level, wall, ..
    } = fx();
    let t = ctype(&mut m, "Chair", "450x500");
    let mut s = CompSpec::at(level, t, 2.0, 1.0);
    s.rotation = PI / 6.0;
    let id = m.add_component(&s).unwrap();
    let wb = ctype(&mut m, "Washbasin", "500");
    let mut h = CompSpec::at(level, wb, 1.0, 0.1);
    h.host = CompHost::Wall(wall);
    h.rotation = 0.2;
    let hid = m.add_component(&h).unwrap();
    let pts = [(0.1, 0.2, 0.0), (-0.3, 0.05, 0.5), (0.0, 0.4, 0.9)];
    let before: Vec<Placement> = [id, hid]
        .iter()
        .map(|&i| m.component_quantities(i).unwrap().placement)
        .collect();
    m.mirror_elements(
        &[Target::Component(id), Target::Wall(wall)],
        (0.0, 0.0),
        (0.0, 1.0),
        false,
    )
    .unwrap();
    for (k, &i) in [id, hid].iter().enumerate() {
        let after = m.component_quantities(i).unwrap().placement;
        assert!(after.mirror, "{i}");
        for p in pts {
            near3(after.apply(p), reflect_x(before[k].apply(p)));
        }
    }
    let q = m.component_quantities(id).unwrap();
    near(
        q.volume,
        m.component_built(id, Lod::Medium).unwrap().2.volume,
        1e-15,
    );
    // a second mirror restores the original orientation
    m.mirror_elements(&[Target::Component(id)], (0.0, 0.0), (0.0, 1.0), false)
        .unwrap();
    let p = m.component_quantities(id).unwrap().placement;
    assert!(!p.mirror);
    for q in pts {
        near3(p.apply(q), before[0].apply(q));
    }
    assert!(m.check_integrity().unwrap().is_empty());
}

#[test]
fn types_formulas_and_errors() {
    let Fx { mut m, level, .. } = fx();
    let cyc = "family Loop\nparam A length type = B\nparam B length type = A\n";
    let e = m.load_family(cyc).unwrap_err().to_string();
    assert!(e.contains("cycle") && e.contains("A -> B -> A"), "{e}");
    let fam = m.load_library_family("Table").unwrap();
    assert!(m.load_library_family("Table").is_ok());
    assert!(m.load_family(ddc_model::library_sources()[0]).is_err());
    let t = m.comp_type_id(fam, "1600x800").unwrap().unwrap();
    let id = m.add_component(&CompSpec::at(level, t, 0.0, 0.0)).unwrap();
    // a type parameter on an instance is an error
    let mut s = CompSpec::at(level, t, 0.0, 0.0);
    s.values = vec![("Width".into(), ParamValue::Num(2.0))];
    assert!(m.add_component(&s).is_err());
    // a new type and a type edit recompute the quantities
    let t2 = m
        .add_comp_type(
            fam,
            "2000x1000",
            &[
                ("Width".into(), ParamValue::Num(2.0)),
                ("Depth".into(), ParamValue::Num(1.0)),
            ],
        )
        .unwrap();
    let id2 = m.add_component(&CompSpec::at(level, t2, 3.0, 0.0)).unwrap();
    near(
        m.component_quantities(id2).unwrap().volume,
        2.0 * 1.0 * 0.04 + 4.0 * 0.06 * 0.06 * 0.71,
        1e-15,
    );
    m.set_comp_type_values(t, &[("Width".into(), ParamValue::Num(1.0))])
        .unwrap();
    near(
        m.component_quantities(id).unwrap().volume,
        1.0 * 0.8 * 0.04 + 4.0 * 0.06 * 0.06 * 0.71,
        1e-15,
    );
    // width smaller than two legs: the solid cannot be built, so the edit is rejected and state is unchanged
    let bad = m.set_comp_type_values(t, &[("Width".into(), ParamValue::Num(0.0))]);
    assert!(bad.is_err());
    near(
        m.component_quantities(id).unwrap().volume,
        1.0 * 0.8 * 0.04 + 4.0 * 0.06 * 0.06 * 0.71,
        1e-15,
    );
    m.undo().unwrap();
    near(m.component_quantities(id).unwrap().volume, 0.061424, 1e-15);
    // deleting a type or family that is in use is forbidden
    assert!(matches!(
        m.delete_comp_type(t),
        Err(Error::Referenced { .. })
    ));
    assert!(matches!(
        m.delete_family(fam),
        Err(Error::Referenced { .. })
    ));
    let counts = m.component_counts().unwrap();
    assert_eq!(counts.len(), 2);
    assert!(m.check_integrity().unwrap().is_empty());
}

#[test]
fn same_type_shares_one_mesh() {
    let Fx { mut m, level, .. } = fx();
    let t = ctype(&mut m, "Table", "1600x800");
    let specs: Vec<CompSpec> = (0..1000)
        .map(|k| {
            let mut s = CompSpec::at(level, t, (k % 40) as f64 * 2.0, (k / 40) as f64 * 1.5);
            s.rotation = k as f64 * 0.01;
            s
        })
        .collect();
    let ids = m.add_components(&specs).unwrap();
    assert_eq!(ids.len(), 1000);
    let t0 = std::time::Instant::now();
    let sc = m.component_scene(Lod::Medium, None).unwrap();
    let ms = t0.elapsed().as_secs_f64() * 1e3;
    eprintln!("scene of 1000 instances: {ms:.1} ms");
    if !cfg!(debug_assertions) {
        assert!(ms < 50.0, "{ms} ms");
    }
    assert_eq!(sc.meshes.len(), 1);
    assert_eq!(sc.instances.len(), 1000);
    assert!(sc.instances.iter().all(|i| i.mesh == 0));
    // instance values -> its own mesh; another type with the same values -> the same mesh
    let gb = ctype(&mut m, "Generic Box", "Box");
    let mut a = CompSpec::at(level, gb, 0.0, 0.0);
    m.add_component(&a).unwrap();
    a.values = vec![("Width".into(), ParamValue::Num(0.9))];
    m.add_component(&a).unwrap();
    let fam = m.family_id("Table").unwrap().unwrap();
    let vals = m.comp_type_values(t).unwrap();
    let same = m.add_comp_type(fam, "Copy", &vals).unwrap();
    m.add_component(&CompSpec::at(level, same, 0.0, 9.0))
        .unwrap();
    let sc = m.component_scene(Lod::Medium, None).unwrap();
    assert_eq!(sc.meshes.len(), 3);
    assert_eq!(sc.instances.len(), 1003);
    m.undo().unwrap();
    m.undo().unwrap();
    m.undo().unwrap();
    m.undo().unwrap();
    m.undo().unwrap();
    assert_eq!(m.components(None).unwrap().len(), 1000);
    m.undo().unwrap();
    assert!(m.components(None).unwrap().is_empty());
    assert!(m.check_integrity().unwrap().is_empty());
}

#[test]
fn floor_and_ceiling_hosts() {
    let Fx { mut m, level, .. } = fx();
    let lay = [LayerSpec {
        material: "Concrete".into(),
        thickness: 0.2,
    }];
    let st = m.add_slab_type("S200", &lay).unwrap();
    let ring = vec![(0.0, 0.0), (6.0, 0.0), (6.0, 6.0), (0.0, 6.0)];
    let slab = m
        .add_slab(&ddc_model::SlabSpec {
            level,
            slab_type: st,
            offset: 0.05,
            outer: ring.clone(),
            holes: vec![],
        })
        .unwrap();
    let ceil = m
        .add_ceiling(&ddc_model::CeilingSpec {
            level,
            slab_type: st,
            height: 2.7,
            outer: ring,
            holes: vec![],
        })
        .unwrap();
    let bed = ctype(&mut m, "Bed", "Double 1600x2000");
    let mut s = CompSpec::at(level, bed, 3.0, 3.0);
    s.host = CompHost::Floor(slab);
    let b = m.add_component(&s).unwrap();
    near(m.component_quantities(b).unwrap().bbox[0].2, 3.05, 1e-12);
    let light = ctype(&mut m, "Ceiling Light", "Round 300");
    let mut s = CompSpec::at(level, light, 3.0, 3.0);
    s.host = CompHost::Ceiling(ceil);
    let l = m.add_component(&s).unwrap();
    let q = m.component_quantities(l).unwrap();
    near(q.bbox[1].2, 5.7, 1e-12);
    near(q.bbox[0].2, 5.62, 1e-12);
    near(q.volume, PI * 0.15 * 0.15 * 0.08, 1e-15);
    // ceiling-mounted items are not in the floor plan
    assert!(m
        .components_plan(level, Lod::Medium)
        .unwrap()
        .iter()
        .all(|p| matches!(p, ddc_model::ViewPrim::Edge { element, .. } if *element == b)));
    m.delete_ceiling(ceil).unwrap();
    assert_eq!(m.components(None).unwrap(), vec![b]);
    m.undo().unwrap();
    m.delete_slab(slab).unwrap();
    assert_eq!(m.components(None).unwrap(), vec![l]);
    m.undo().unwrap();
    assert!(m.check_integrity().unwrap().is_empty());
}

/// `cargo test --release -p ddc-model --test components -- --ignored`
#[test]
#[ignore]
fn perf_100k_instances() {
    let Fx { mut m, level, .. } = fx();
    let t = ctype(&mut m, "Chair", "450x500");
    let specs: Vec<CompSpec> = (0..100_000)
        .map(|k| CompSpec::at(level, t, (k % 300) as f64, (k / 300) as f64))
        .collect();
    let t0 = std::time::Instant::now();
    m.add_components(&specs).unwrap();
    eprintln!("add 100k: {:.0} ms", t0.elapsed().as_secs_f64() * 1e3);
    let t0 = std::time::Instant::now();
    assert!(m.check_integrity().unwrap().is_empty());
    let ms = t0.elapsed().as_secs_f64() * 1e3;
    eprintln!("check_integrity 100k: {ms:.0} ms");
    assert!(ms < 1000.0, "{ms} ms");
    let t0 = std::time::Instant::now();
    let sc = m.component_scene(Lod::Medium, None).unwrap();
    eprintln!("scene 100k: {:.0} ms", t0.elapsed().as_secs_f64() * 1e3);
    assert_eq!(sc.meshes.len(), 1);
}

/// A tree reaches the scene with coarser pictures that are lighter, and every part names the
/// material the renderer colours it by.
#[test]
fn a_tree_scene_carries_lighter_levels_and_part_materials() {
    let Fx { mut m, level, .. } = fx();
    let t = ctype(&mut m, "Tree", "Oak A");
    m.add_components(&[CompSpec::at(level, t, 2.0, 2.0)]).unwrap();
    let sc = m.component_scene(Lod::Fine, None).unwrap();
    let k = sc.instances[0].mesh;
    let full = sc.meshes[k].triangles.len();
    let lv = &sc.lods[k];
    assert_eq!(lv.len(), 2, "medium and coarse below the fine body");
    let tris: Vec<usize> = lv.iter().map(|l| l.0.triangles.len()).collect();
    assert!(tris[0] < full && tris[1] < tris[0], "{full} {tris:?}");
    assert!(lv[0].2 < lv[1].2, "the coarser the larger the error");
    // Oak A declares its own bark (Bark Dark); its leaves keep the family default
    assert!(sc.materials[k].iter().any(|x| x == "Bark Dark") && sc.materials[k].iter().any(|x| x == "Foliage Green"), "{:?}", sc.materials[k]);
    for l in lv {
        assert!(l.1.iter().any(|x| x == "Foliage Green"));
    }
    // a family that is not a plant has no levels
    let tf = ctype(&mut m, "Table", "1600x800");
    m.add_components(&[CompSpec::at(level, tf, 6.0, 2.0)]).unwrap();
    let sc = m.component_scene(Lod::Fine, None).unwrap();
    let kt = sc.instances.iter().find(|i| i.mesh != k).unwrap().mesh;
    assert!(sc.lods[kt].is_empty());
}

/// Families the hard scenarios need (plant): present, with a connector set or plan symbol that
/// builds at every detail level (the loop above covers the levels, the imperial type and the plan).
#[test]
fn library_has_plant_families() {
    let lib = library().unwrap();
    for n in ["Pressure Vessel", "Heat Exchanger", "Centrifugal Pump", "Gate Valve"] {
        let f = lib.iter().find(|f| f.name == n).unwrap_or_else(|| panic!("{n}"));
        assert!(f.types.len() >= 3, "{n}");
        assert!(!f.solids.is_empty() && !f.symbols.is_empty(), "{n}");
    }
}

/// Families the hard scenarios need (dc): present, with a connector set or plan symbol that
/// builds at every detail level (the loop above covers the levels, the imperial type and the plan).
#[test]
fn library_has_dc_families() {
    let lib = library().unwrap();
    for n in ["Power Distribution Unit", "Aisle Containment Door"] {
        let f = lib.iter().find(|f| f.name == n).unwrap_or_else(|| panic!("{n}"));
        assert!(f.types.len() >= 3, "{n}");
        assert!(!f.solids.is_empty() && !f.symbols.is_empty(), "{n}");
    }
}

/// Families the hard scenarios need (tower): present, with a connector set or plan symbol that
/// builds at every detail level (the loop above covers the levels, the imperial type and the plan).
#[test]
fn library_has_tower_families() {
    let lib = library().unwrap();
    for n in ["Antenna Mount", "Tower Platform", "Cage Ladder"] {
        let f = lib.iter().find(|f| f.name == n).unwrap_or_else(|| panic!("{n}"));
        assert!(f.types.len() >= 3, "{n}");
        assert!(!f.solids.is_empty() && !f.symbols.is_empty(), "{n}");
    }
}


#[test]
fn a_crowd_of_plants_is_drawn_at_a_coarser_level_with_hysteresis() {
    use ddc_model::{plant_level, Crowd};
    // the plan limit is the symbol segments
    let plan = |lod, n, last| plant_level(lod, n, last, Crowd::Plan);
    assert_eq!(plan(Lod::Fine, 10, 0), (Lod::Fine, 0));
    assert_eq!(plan(Lod::Fine, 150, 0), (Lod::Fine, 0));
    assert_eq!(plan(Lod::Fine, 151, 0), (Lod::Medium, 1));
    assert_eq!(plan(Lod::Medium, 300, 0), (Lod::Medium, 1));
    assert_eq!(plan(Lod::Fine, 1000, 0), (Lod::Coarse, 2));
    assert_eq!(plan(Lod::Fine, 140, 1), (Lod::Medium, 1));
    assert_eq!(plan(Lod::Fine, 134, 1), (Lod::Fine, 0));
    assert_eq!(plan(Lod::Fine, 560, 2), (Lod::Coarse, 2));
    assert_eq!(plan(Lod::Fine, 539, 2), (Lod::Medium, 1));
    // the scene keeps a thousand trees fine and leaves the far ones to the renderer's levels
    let scene = |lod, n, last| plant_level(lod, n, last, Crowd::Scene);
    assert_eq!(scene(Lod::Fine, 1000, 0), (Lod::Fine, 0));
    assert_eq!(scene(Lod::Fine, 1001, 0), (Lod::Medium, 1));
    assert_eq!(scene(Lod::Fine, 950, 1), (Lod::Medium, 1));
    assert_eq!(scene(Lod::Fine, 4001, 0), (Lod::Coarse, 2));
}

#[test]
fn seven_hundred_trees_are_coarse_in_the_plan_and_fine_with_levels_in_the_scene() {
    let Fx { mut m, level, .. } = fx();
    let few = ctype(&mut m, "Tree", "Oak A");
    let one = m.add_component(&CompSpec::at(level, few, 0.0, 0.0)).unwrap();
    let fine = m.component_scene(Lod::Fine, None).unwrap();
    let fine_tris = fine.meshes[0].triangles.len();
    let fine_plan = m.components_plan(level, Lod::Fine).unwrap().len();
    m.delete_component(one).unwrap();
    let specs: Vec<CompSpec> = (0..700)
        .map(|k| CompSpec::at(level, few, (k % 30) as f64 * 8.0, (k / 30) as f64 * 8.0))
        .collect();
    m.add_components(&specs).unwrap();
    // scene: the full mesh with two coarser levels for the distance, each lighter than the last
    let sc = m.component_scene(Lod::Fine, None).unwrap();
    assert_eq!(sc.instances.len(), 700);
    assert_eq!(sc.meshes[0].triangles.len(), fine_tris);
    let tris: Vec<usize> = sc.lods[0].iter().map(|l| l.0.triangles.len()).collect();
    assert!(tris.len() == 2 && tris[0] < fine_tris && tris[1] < tris[0], "{fine_tris} {tris:?}");
    // plan: coarse symbols
    let plan = m.components_plan(level, Lod::Fine).unwrap().len();
    assert!(plan <= 700 * 40 && plan * 2 < 700 * fine_plan, "plan {plan} segments vs {fine_plan} per fine tree");
}

/// A tree is in the section and the elevation: the cut through its trunk and crown is filled up
/// to its height, and the elevation shows its silhouette edges, not nothing and not a box.
#[test]
fn a_tree_shows_in_sections_and_elevations_by_its_mesh() {
    use ddc_model::{SectionSpec, ViewKind, ViewPrim};
    let Fx { mut m, level, .. } = fx();
    let t = ctype(&mut m, "Tree", "Oak A");
    let id = m.add_component(&CompSpec::at(level, t, 20.0, 20.0)).unwrap();
    // the views cut world-space bodies: the built (local) top lifted by the placement, whose
    // bottom the quantities give in world coordinates (the level elevation)
    let built = m.component_built(id, Lod::Medium).unwrap().2.bbox;
    let lift = m.component_quantities(id).unwrap().bbox[0].2 - built[0].z;
    let top = built[1].z + lift;
    let spec = |kind| SectionSpec { kind, name: "T".into(), a: (10.0, 20.0), b: (30.0, 20.0), depth: 30.0, far_clip: false };
    let cut = m.section_view(&spec(ViewKind::Section), &[]).unwrap();
    let hi = cut
        .prims
        .iter()
        .filter_map(|p| match p {
            ViewPrim::SectionCut { table: "comp", rings, .. } => Some(rings.iter().map(|q| q.1).fold(f64::MIN, f64::max)),
            _ => None,
        })
        .fold(f64::MIN, f64::max);
    assert!(hi > 0.8 * top && hi <= top + 0.1, "the cut reaches {hi} of {top}");
    let el = m.section_view(&spec(ViewKind::Elevation), &[]).unwrap();
    let edges: Vec<f64> = el
        .prims
        .iter()
        .filter_map(|p| match p {
            ViewPrim::Edge { table: "comp", a, b, .. } => Some(a.1.max(b.1)),
            _ => None,
        })
        .collect();
    assert!(edges.len() > 20, "silhouette edges: {}", edges.len());
    assert!(edges.iter().cloned().fold(f64::MIN, f64::max) > 0.8 * top);
}

/// Trees of one type at different sizes share one mesh: the size goes into the placement, and
/// the placed unit mesh is the tree built at full values (an active clear-trunk floor included).
#[test]
fn trees_of_one_type_share_a_mesh_and_carry_their_scale() {
    let Fx { mut m, level, .. } = fx();
    let t = ctype(&mut m, "Tree", "Linden A");
    let at = |x: f64, s: f64, floor: f64| {
        let mut c = CompSpec::at(level, t, x, 0.0);
        c.values = vec![
            ("Scale".into(), ParamValue::Num(s)),
            ("MinClear".into(), ParamValue::Num(floor)),
        ];
        c
    };
    let specs = [
        at(0.0, 0.8, 0.0),
        at(20.0, 1.0, 0.0),
        at(40.0, 1.17, 0.0),
        at(60.0, 0.9, 6.0),
    ];
    let ids: Vec<i64> = specs.iter().map(|s| m.add_component(s).unwrap()).collect();
    let sc = m.component_scene(Lod::Medium, None).unwrap();
    assert_eq!(sc.instances.len(), 4);
    let meshes: std::collections::HashSet<usize> = sc.instances.iter().map(|i| i.mesh).collect();
    // the clear-trunk floor of the last tree changes its shape, the sizes alone do not
    assert_eq!(
        meshes.len(),
        2,
        "{:?}",
        sc.instances
            .iter()
            .map(|i| (i.mesh, i.scale))
            .collect::<Vec<_>>()
    );
    for (i, id) in sc.instances.iter().zip(&ids) {
        let full = m.component_built(*id, Lod::Medium).unwrap().2.bbox;
        let unit = sc.meshes[i.mesh]
            .positions
            .iter()
            .fold([f64::MAX, f64::MIN], |b, p| [b[0].min(p.z), b[1].max(p.z)]);
        near(unit[1] * i.scale, full[1].z, 1e-6);
        let ext = sc.meshes[i.mesh]
            .positions
            .iter()
            .fold(f64::MIN, |b, p| b.max(p.x));
        near(ext * i.scale, full[1].x, 1e-6);
    }
}
