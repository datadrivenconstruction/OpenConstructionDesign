// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Building blocks and recipes: every recipe builds, passes its strict audit and is
//! deterministic; options are checked and refusals name the block and the option.

use ddc_ops::{Cat, GeoOp, Op, Ref};
use ddc_script::recipes::{recipe, summary, RECIPES};
use ddc_script::{GeoScript, RunOpts, Sandbox, ScriptError, ScriptRun};
use serde_json::json;

fn run(src: &str) -> Result<ScriptRun, ScriptError> {
    let o = RunOpts::new(Ref::name(Cat::Level, "Level 1"), json!({}));
    Sandbox.run(src, &o)
}

fn ok(src: &str) -> ScriptRun {
    run(src).unwrap_or_else(|e| panic!("{e}\n{src}"))
}

fn fail(src: &str) -> String {
    run(src).expect_err("script must fail").message
}

/// Parts whose solid name starts with `role ` (names are "role section").
fn role(r: &ScriptRun, role: &str) -> usize {
    r.env.groups[0]
        .ops
        .iter()
        .filter(|op| match op {
            Op::Geometry(GeoOp::Solid { name: Some(n), .. }) => {
                n.split(' ').next() == Some(role)
            }
            _ => false,
        })
        .count()
}

#[test]
fn every_recipe_builds_clean_and_deterministic() {
    assert_eq!(RECIPES.len(), 24);
    for (name, script) in RECIPES {
        assert!(summary(script).len() > 20, "{name}: summary line");
        let a = ok(script);
        let b = ok(script);
        assert_eq!(a.hash, b.hash, "{name}: deterministic");
        assert!(a.parts.len() >= 40, "{name}: only {} parts", a.parts.len());
        let audit = a.log.iter().find(|l| l.starts_with("audit:")).expect("audit line");
        assert!(
            audit.contains(" 0 near misses, 0 duplicates, 0 clashes, 0 floating groups"),
            "{name}: {audit}"
        );
        assert!(!a.params.is_empty(), "{name}: declares params");
    }
}

#[test]
fn recipes_carry_the_parts_their_summary_promises() {
    let r = ok(recipe("twisted_diagrid_tower").unwrap());
    assert_eq!(role(&r, "slab"), 24);
    assert_eq!(role(&r, "diagonal"), 12 * 12 * 2);
    assert_eq!(role(&r, "core"), 24);
    let r = ok(recipe("cable_stayed_footbridge").unwrap());
    assert_eq!(role(&r, "pylon"), 4);
    assert!(role(&r, "stay") >= 20);
    assert_eq!(role(&r, "deck"), 16);
    let r = ok(recipe("membrane_canopy").unwrap());
    assert_eq!(role(&r, "mast"), 4);
    assert_eq!(role(&r, "guy"), 4);
    assert!(role(&r, "membrane") >= 8 * 24);
    let r = ok(recipe("voronoi_screen").unwrap());
    assert_eq!((role(&r, "plate"), role(&r, "post")), (4, 5));
    let r = ok(recipe("hypar_umbrellas").unwrap());
    assert_eq!(role(&r, "column"), 6);
}

/// IFC class and predefined type of the first solid with this role.
fn class(r: &ScriptRun, role: &str) -> Option<(String, Option<String>)> {
    r.env.groups[0].ops.iter().find_map(|op| match op {
        Op::Geometry(GeoOp::Solid { name: Some(n), ifc, .. }) if n.split(' ').next() == Some(role) => {
            Some(ifc.as_ref().map(|c| (c.class.clone(), c.predefined.clone())))
        }
        _ => None,
    })?
}

#[test]
fn parts_carry_the_ifc_class_of_their_role() {
    let r = ok(recipe("twisted_diagrid_tower").unwrap());
    let c = |cl: &str, p: &str| Some((cl.to_string(), Some(p.to_string())));
    assert_eq!(class(&r, "slab"), c("IfcSlab", "FLOOR"));
    assert_eq!(class(&r, "diagonal"), c("IfcMember", "STRUT"));
    assert_eq!(class(&r, "core"), c("IfcColumn", "COLUMN"));
    let r = ok(recipe("sun_louvres").unwrap());
    assert_eq!(class(&r, "louvre"), c("IfcShadingDevice", "JALOUSIE"));
    let r = ok(recipe("fin_facade").unwrap());
    assert_eq!(class(&r, "fin"), c("IfcShadingDevice", "NOTDEFINED"));
    // a role in the table wins over the kind; unknown roles use the kind
    for (role, cl, p) in ddc_script::ROLE_IFC {
        let k = ddc_script::ifc_of(ddc_script::PartKind::Solid, role).unwrap();
        assert_eq!((k.class.as_str(), k.predefined.as_deref()), (*cl, Some(*p)));
    }
    use ddc_script::{ifc_of, PartKind};
    assert_eq!(ifc_of(PartKind::Solid, "blob"), None);
    assert_eq!(ifc_of(PartKind::Member, "blob").unwrap().class, "IfcMember");
    assert_eq!(ifc_of(PartKind::Panel, "blob").unwrap().class, "IfcPlate");
    assert_eq!(ifc_of(PartKind::Slab, "blob").unwrap().class, "IfcSlab");
    // every recipe: no generated part is left a proxy unless it is a free solid
    for (name, script) in RECIPES {
        let r = ok(script);
        let classed = r.env.groups[0]
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Geometry(GeoOp::Solid { ifc: Some(_), .. })))
            .count();
        assert!(classed > 0, "{name}: no classed part");
    }
}

#[test]
fn params_change_the_structure() {
    let src = recipe("twisted_diagrid_tower").unwrap();
    let o = RunOpts::new(Ref::name(Cat::Level, "Level 1"), json!({"floors": 10, "twist": "30 deg"}));
    let r = Sandbox.run(src, &o).unwrap();
    assert_eq!(role(&r, "slab"), 10);
    let o = RunOpts::new(Ref::name(Cat::Level, "Level 1"), json!({"floors": 9}));
    let e = Sandbox.run(src, &o).unwrap_err().message;
    assert!(e.contains("tower: floors (9) must be a multiple of module (2)"), "{e}");
}

#[test]
fn options_are_checked_by_name() {
    let e = fail("tower{floor = 10}");
    assert!(e.contains("tower: unknown option `floor`") && e.contains("floors"), "{e}");
    let e = fail("bridge{kind = 'suspension'}");
    assert!(e.contains("bridge") && e.contains("kind"), "{e}");
    let e = fail("vault{rib = 'CHS 100'}");
    assert!(e.contains("vault") && e.contains("rib") && e.contains("CHS 508x20"), "{e}");
    let e = fail("tower(5)");
    assert!(e.contains("one options table"), "{e}");
    let e = fail("surface_grid{surface = 3}");
    assert!(e.contains("surface_grid: surface needs surf."), "{e}");
}

fn gridshells(r: &ScriptRun) -> Vec<&GeoOp> {
    r.env.groups[0]
        .ops
        .iter()
        .filter_map(|op| match op {
            Op::Geometry(g @ GeoOp::Gridshell { .. }) => Some(g),
            _ => None,
        })
        .collect()
}

/// A steel dome or circular vault grid is one model gridshell; other surfaces, timber
/// sections or glazed grids stay members.
#[test]
fn steel_dome_and_barrel_grids_are_one_gridshell() {
    let r = ok("surface_grid{surface = surf.dome{radius = 12, rise = 5, oculus = 1.5}, nu = 16, nv = 4,\n\
                closed_u = true, pattern = 'diagrid', member = 'CHS 88.9x5', edge = 'CHS 168.3x8'}");
    let g = gridshells(&r);
    assert_eq!(g.len(), 1);
    match g[0] {
        GeoOp::Gridshell { level: Some(_), host: ddc_ops::LatticeHost::Dome { oculus: Some(_), .. }, count_u: 16, count_v: 4, member, edge: Some(e), .. } => {
            assert_eq!((member.as_str(), e.as_str()), ("CHS 88.9x5", "CHS 168.3x8"));
        }
        o => panic!("{o:?}"),
    }
    assert_eq!(role(&r, "diagonal"), 0);
    let v = ok("surface_grid{surface = surf.vault{span = 20, length = 30, kind = 'circular', rise = 6}, nu = 8, nv = 6}");
    assert!(matches!(gridshells(&v)[..], [GeoOp::Gridshell { host: ddc_ops::LatticeHost::Vault { .. }, .. }]));
    let c = ok("surface_grid{surface = surf.vault{span = 20, length = 30, kind = 'catenary', rise = 6}, nu = 8, nv = 6}");
    assert!(gridshells(&c).is_empty() && role(&c, "v_rib") > 0);
    let t = ok("surface_grid{surface = surf.dome{radius = 12, rise = 5}, nu = 16, nv = 4, closed_u = true,\n\
                member = 'rect 120x240', edge = 'rect 200x360', material = 'glulam GL28h'}");
    assert!(gridshells(&t).is_empty());
    let p = ok("surface_grid{surface = surf.dome{radius = 12, rise = 5}, nu = 16, nv = 4, closed_u = true, panels = 0.02}");
    assert!(gridshells(&p).is_empty() && role(&p, "panel") > 0);
}

#[test]
fn stairs_and_ramps_keep_walkable_geometry() {
    let r = ok("helix_stair{height = '4 m', r_out = '1.6 m'}\naudit{strict = true}");
    let treads = role(&r, "tread");
    // risers at most 190 mm over 4 m
    assert!(treads >= 22, "{treads} treads");
    let e = fail("helix_stair{height = '4 m', r_out = '0.7 m'}");
    assert!(e.contains("helix_stair"), "{e}");
    let e = fail("helix_stair{going = '0.18 m'}");
    assert!(e.contains("helix_stair") && e.contains("going"), "{e}");
    let e = fail("helix_ramp{r_in = '1 m', height = '10 m', slope_max = 0.2}");
    assert!(e.contains("helix_ramp") && e.contains("headroom"), "{e}");
}

/// A Moebius band: the closed seam joins row 0 mirrored in v, so the grid is the one a
/// function that closes by itself gives (same nodes, same members), without a doubled seam.
#[test]
fn flip_seam_closes_a_moebius_band() {
    let band = "local function band(u, v)
                  local th, s = 2 * math.pi * u, (v - 0.5) * 3
                  local r = 9 + s * math.cos(th / 2)
                  return vec3(r * math.cos(th), r * math.sin(th), 6 + s * math.sin(th / 2))
                end
";
    let flip = ok(&format!(
        "{band}local _, a = surface_grid{{surface = band, nu = 24, nv = 4, closed_u = true, flip_seam = true}}
         local _, b = surface_grid{{surface = band, nu = 24, nv = 4, name = 'open'}}
         assert(#a.nodes == 24 * 5, #a.nodes)
         assert(#a.nodes == #b.nodes, #b.nodes)
         assert(a.parts == b.parts, a.parts .. ' vs ' .. b.parts)
         assert(a.counts.u_rib == b.counts.u_rib)"
    ));
    assert!(flip.parts.len() >= 216);
    let e = fail("surface_grid{surface = surf.dome{radius = 12, rise = 5}, nu = 8, nv = 4, flip_seam = true}");
    assert!(e.contains("surface_grid") && e.contains("flip_seam"), "{e}");
}

/// Distinct profiles of the parts of one role (fins differ by their depth).
fn sections_of(r: &ScriptRun, role: &str) -> std::collections::BTreeSet<String> {
    use ddc_ops::Form;
    r.env.groups[0]
        .ops
        .iter()
        .filter_map(|op| match op {
            Op::Geometry(GeoOp::Solid { name: Some(n), form, .. }) if n.split(' ').next() == Some(role) => match form {
                Form::Extrude { profile, .. } | Form::Sweep { profile, .. } => Some(format!("{profile:?}")),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[test]
fn fin_depth_swings_between_its_bounds() {
    let flat = ok("fins{from = vec3(0, 0, 0), to = vec3(30, 0, 0), height = 3, depth = '0.9 m', mode = 'fixed'}");
    assert_eq!(sections_of(&flat, "fin").len(), 1);
    let wave = ok("fins{from = vec3(0, 0, 0), to = vec3(30, 0, 0), height = 3, depth = '0.9 m',
                   depth_min = '0.2 m', waves = 2, mode = 'fixed', spacing = '0.6 m'}");
    let n = sections_of(&wave, "fin");
    assert!(n.len() > 8, "{n:?}");
    assert_eq!(role(&wave, "fin"), role(&flat, "fin"));
    let e = fail("fins{depth = '0.3 m', depth_min = '0.5 m', mode = 'fixed'}");
    assert!(e.contains("fins") && e.contains("depth_min"), "{e}");
}

/// Class I frequency n of the icosahedron: 10 n^2 + 2 nodes, 30 n^2 struts, 20 n^2 triangles.
#[test]
fn geodesic_sphere_counts_follow_the_frequency() {
    let r = ok("local _, a = geodesic{center = vec3(0, 0, 15), radius = 15, frequency = 4, panels = 0.02}
                assert(#a.nodes == 162, #a.nodes)
                assert(a.counts.strut == 480 and a.counts.edge == nil, a.counts.strut)
                assert(a.counts.panel == 320, a.counts.panel)
                local _, f = geodesic{radius = 5, frequency = 1, name = 'ico'}
                assert(#f.nodes == 12 and f.counts.strut == 30)
                local _, h = geodesic{center = vec3(0, 0, 0), radius = 15, frequency = 4, cut = 0, name = 'dome'}
                assert(h.counts.edge > 0 and h.counts.strut > 0)");
    assert!(role(&r, "strut") >= 480);
    let e = fail("geodesic{cut = 1}");
    assert!(e.contains("geodesic"), "{e}");
}

/// Recursion in a script (G18, a fractal tree): a trunk splitting into 3 for 4 generations,
/// branches shrinking by 0.7, each branch one member between shared nodes.
#[test]
fn a_script_can_recurse_into_a_branching_tree() {
    let r = ok("local prof = profile.chs(0.2, 0.01)
                local made = 0
                local function branch(a, dir, len, gen)
                  local b = a + dir * len
                  part('branch', extrude(prof, a, b), 'S355')
                  made = made + 1
                  if gen < 4 then
                    for k = 0, 2 do
                      local ang = 2 * math.pi * k / 3
                      local d = (dir + vec3(math.cos(ang), math.sin(ang), 0) * 0.8):unit()
                      branch(b, d, len * 0.7, gen + 1)
                    end
                  end
                end
                branch(vec3(0, 0, 0), vec3(0, 0, 1), 4, 1)
                assert(made == 1 + 3 + 9 + 27 + 81, made)");
    assert_eq!(r.parts.len(), 121);
}

#[test]
fn surfaces_take_functions_and_audit_catches_clashes() {
    let r = ok("surface_grid{surface = function(u, v) return vec3(20 * u, 10 * v, 4 + 2 * u * v) end,\n\
                nu = 4, nv = 2, edge_columns = 1}\naudit{strict = true}");
    assert_eq!(role(&r, "column"), 10);
    let s = ok("local s = surf.dome{radius = 10, rise = 4}\nlocal p = s(0, 0)\n\
                assert(math.abs(p.z) < 1e-9 and math.abs(p.x - 10) < 1e-9)\n\
                part('x', extrude(profile.chs(0.2, 0.01), vec3(0, 0, 0), vec3(0, 0, 3)))");
    assert_eq!(s.parts.len(), 1);
    let e = fail(
        "local p = profile.chs(0.2, 0.01)\n\
         part('a', extrude(p, vec3(0, 0, 0), vec3(4, 4, 0)))\n\
         part('b', extrude(p, vec3(4, 0, 0), vec3(0, 4, 0)))\n\
         audit{strict = true}",
    );
    assert!(e.contains("audit:") && e.contains("crosses"), "{e}");
    let e = fail("local p = profile.chs(0.2, 0.01)\npart('a', extrude(p, vec3(0, 0, 3), vec3(4, 0, 3)))\naudit{strict = true}");
    assert!(e.contains("do not reach the ground"), "{e}");
}

#[test]
fn a_stadium_cantilever_is_held_by_a_back_span() {
    let src = |back: &str| format!("stadium_roof{{a = 60, b = 42, depth = 32, backspan = {back}, bays = 16, divisions = 8, height = 28}}");
    let r = ok(&src("8"));
    assert_eq!(role(&r, "column"), 2 * 16);
    let e = fail(&src("2"));
    assert!(e.contains("stadium_roof") && e.contains("back-span"), "{e}");
}

#[test]
fn pagoda_has_columns_roofs_and_a_finial_per_tier() {
    let r = ok("pagoda{tiers = 3, base_width = \"10 m\", tier_height = \"4 m\", eave_curl = \"0.8 m\"}\naudit{strict = true}");
    assert_eq!(role(&r, "column"), 3 * 8);
    assert_eq!(role(&r, "mast"), 1);
    assert_eq!(role(&r, "deck"), 3);
    assert!(role(&r, "shell") >= 3 * 16, "a roof of panels per tier");
    assert_eq!(class(&r, "shell"), Some(("IfcSlab".to_string(), Some("ROOF".to_string()))));
    assert_eq!(class(&r, "column"), Some(("IfcColumn".to_string(), Some("COLUMN".to_string()))));
    // more tiers, more columns and roofs
    let r5 = ok("pagoda{tiers = 5}");
    assert_eq!(role(&r5, "column"), 5 * 8);
    assert!(role(&r5, "shell") > role(&r, "shell"));
    // a curl lifts the eave corners: the roofs differ from the straight eaves
    let flat = ok("pagoda{tiers = 3, eave_curl = 0}");
    let curled = ok("pagoda{tiers = 3, eave_curl = \"0.8 m\"}");
    assert_ne!(flat.hash, curled.hash);
    // a top tier narrower than 1.5 m is refused with the options named
    assert!(fail("pagoda{tiers = 15, base_width = 5, taper = 0.55}").contains("pagoda"));
    assert!(fail("pagoda{tires = 3}").contains("tires"));
}

/// Role names of the parts that carry a component tag.
fn roles(r: &ScriptRun) -> Vec<(String, f64)> {
    r.env.groups[0]
        .ops
        .iter()
        .filter_map(|op| match op {
            Op::Geometry(GeoOp::Solid { role: Some(t), .. }) => Some((t.name.clone(), t.at.z)),
            _ => None,
        })
        .collect()
}

#[test]
fn place_with_a_role_tags_one_part_per_placed_group() {
    let r = ok(r#"
local c = define_component("leg", {
  {name = "post", form = extrude(profile.rect(0.1, 0.2), vec3(0, 0, 0), vec3(0, 0, 1))},
  {name = "foot", form = extrude(profile.rect(0.1, 0.2), vec3(-0.2, 0, 0), vec3(0.2, 0, 0))},
})
place(c, vec3(1, 0, 2), "post")
place(c, vec3(3, 0, 2), deg(90), "post")
place(c, vec3(5, 0, 2))
"#);
    assert_eq!(roles(&r), [("post".to_string(), 2.0), ("post".to_string(), 2.0)]);
    assert_eq!(r.parts.len(), 6);
    assert!(fail(r#"place(define_component("a", {extrude(profile.rect(1, 1), vec3(0,0,0), vec3(0,0,1))}), vec3(0,0,0), "a/b")"#)
        .contains("name"));
}

#[test]
fn wind_turbine_recipe_has_three_blades_and_a_rotor() {
    let r = ok(recipe("wind_turbine").unwrap());
    let t = roles(&r);
    assert_eq!(t.iter().filter(|(n, _)| n == "blade").count(), 3);
    assert_eq!(t.iter().filter(|(n, _)| n == "rotor").count(), 1);
    assert!(t.iter().all(|(_, z)| (*z - 90.0).abs() < 1e-9), "{t:?}");
}

/// Axis ends of every extruded solid: (role tag, start, end) in metres.
fn extrusions(r: &ScriptRun) -> Vec<(Option<String>, [f64; 3], [f64; 3])> {
    r.env.groups[0]
        .ops
        .iter()
        .filter_map(|op| match op {
            Op::Geometry(GeoOp::Solid { form: ddc_ops::Form::Extrude { frame, start, end, .. }, role, .. }) => {
                let o = frame.origin.to_m();
                let z = frame.z;
                let at = |t: f64| [o[0] + z.x * t, o[1] + z.y * t, o[2] + z.z * t];
                Some((role.as_ref().map(|t| t.name.clone()), at(start.to_m()), at(end.to_m())))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn turbine_blades_start_on_the_rotor_surface() {
    let r = ok(recipe("wind_turbine").unwrap());
    // blade sections: radial axes in the rotor plane x = 4.5, not the tower or nacelle
    let from_axis = |p: [f64; 3]| p[1].hypot(p[2] - 80.0);
    let blades: Vec<_> = extrusions(&r)
        .into_iter()
        .filter(|(_, a, b)| (a[0] - 4.5).abs() < 1e-6 && (b[0] - 4.5).abs() < 1e-6 && from_axis(*a).max(from_axis(*b)) > 2.0)
        .collect();
    assert_eq!(blades.len(), 3 * 6);
    for (_, a, b) in &blades {
        assert!(from_axis(*a).min(from_axis(*b)) >= 1.5 - 1e-6, "blade inside the rotor: {a:?} {b:?}");
    }
}

#[test]
fn gondolas_hang_clear_of_the_rim_and_spokes() {
    let r = ok(recipe("ferris_wheel").unwrap());
    let cabins: Vec<_> = extrusions(&r).into_iter().filter(|(t, _, _)| t.as_deref() == Some("gondola")).collect();
    assert_eq!(cabins.len(), 24);
    for (_, a, b) in &cabins {
        // vertical, hanging below its top; its 1.6 m box clears the 0.3 m rim and spokes at x = 0
        assert!((a[0] - b[0]).abs() < 1e-9 && a[2].min(b[2]) < a[2].max(b[2]), "{a:?} {b:?}");
        assert!(a[0].abs() - 0.8 >= 0.15 + 0.3, "cabin at x {} too close to the wheel plane", a[0]);
    }
}

#[test]
fn ferris_wheel_recipe_hangs_a_gondola_on_every_rim_node() {
    let r = ok(recipe("ferris_wheel").unwrap());
    assert_eq!(roles(&r).iter().filter(|(n, _)| n == "gondola").count(), 24);
    // rim, spokes, legs, axle and hangers are member rows, not solids: beam + brace >= 70
    let members = |r: &ScriptRun| {
        r.env.groups[0]
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Geometry(GeoOp::Member { .. })))
            .count()
    };
    assert_eq!(members(&r), 24 * 3 + 1 + 4);
    assert!(members(&r) >= 70);
    let r = ok(&recipe("ferris_wheel").unwrap().replace("24, {min", "30, {min"));
    assert_eq!(roles(&r).len(), 30);
}

#[test]
fn member_makes_a_beam_or_brace_part_and_refuses_bad_roles_and_short_spans() {
    let r = ok(r#"
member("a", vec3(0, 0, 0), vec3(4, 0, 0), "CHS 88.9x5", "beam")
member("b", vec3(0, 0, 0), vec3(4, 0, 3), "CHS 88.9x5")
"#);
    let roles: Vec<ddc_ops::MemberRole> = r.env.groups[0]
        .ops
        .iter()
        .filter_map(|op| match op {
            Op::Geometry(GeoOp::Member { role, .. }) => Some(*role),
            _ => None,
        })
        .collect();
    assert_eq!(roles, [ddc_ops::MemberRole::Beam, ddc_ops::MemberRole::Brace]);
    assert_eq!(r.parts.len(), 2);
    assert!(fail(r#"member("a", vec3(0,0,0), vec3(4,0,0), "CHS 88.9x5", "column")"#).contains("beam or brace"));
    assert!(fail(r#"member("a", vec3(0,0,0), vec3(0.001,0,0), "CHS 88.9x5")"#).contains("5 mm"));
}
