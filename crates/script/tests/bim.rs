// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Building elements, plates, fasteners and shells from Lua.

use ddc_ops::{Cat, GeoOp, Len, NewEl, Op, Ref};
use ddc_script::{GeoScript, RunOpts, Sandbox, ScriptRun};
use serde_json::json;

fn run(src: &str) -> ScriptRun {
    let o = RunOpts::new(Ref::name(Cat::Level, "Level 1"), json!({}));
    Sandbox.run(src, &o).unwrap_or_else(|e| panic!("{e}\n{src}"))
}

fn ops(r: &ScriptRun) -> &[Op] {
    &r.env.groups[0].ops
}

#[test]
fn walls_openings_rooms_and_roofs_are_elements_not_solids() {
    let r = run(r#"
local w = wall{from = vec3(0, 0, 0), to = vec3(6, 0, 0), height = 3}
door{wall = w, at = 1.5}
window{wall = w, at = 4, width = 1.4}
room{at = vec3(3, 2, 0), name = "Hall"}
roof{outline = {vec3(0, 0, 0), vec3(6, 0, 0), vec3(6, 4, 0), vec3(0, 4, 0)}, slope = "35 deg", gables = {1, 3}}
"#);
    let kinds: Vec<&str> = ops(&r)
        .iter()
        .map(|o| match o {
            Op::Create { el: NewEl::Wall { .. }, .. } => "wall",
            Op::Create { el: NewEl::Door { .. }, .. } => "door",
            Op::Create { el: NewEl::Window { .. }, .. } => "window",
            Op::Create { el: NewEl::Room { .. }, .. } => "room",
            Op::Create { el: NewEl::Roof { .. }, .. } => "roof",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["wall", "door", "window", "room", "roof"]);
    // the door names the wall by its slot
    let Op::Create { el: NewEl::Door { host, .. }, .. } = &ops(&r)[1] else { panic!() };
    assert_eq!(*host, Ref::slot(0));
}

#[test]
fn an_opening_needs_a_wall() {
    let e = Sandbox
        .run("door{at = 1}", &RunOpts::new(Ref::name(Cat::Level, "Level 1"), json!({})))
        .expect_err("no wall");
    assert!(e.to_string().contains("wall"), "{e}");
}

#[test]
fn plates_bolts_and_rivets_are_parts_with_their_class() {
    let r = run(r#"
plate{outline = {vec3(0, 0, 0), vec3(0.3, 0, 0), vec3(0.3, 0.2, 0), vec3(0, 0.2, 0)}, thickness = "12 mm", name = "gusset"}
bolt{at = vec3(0.1, 0.1, 0), dir = vec3(0, 0, 1), grip = "12 mm", diameter = "16 mm"}
rivet{at = vec3(0.2, 0.1, 0), grip = "12 mm", diameter = "12 mm"}
"#);
    let classes: Vec<String> = ops(&r)
        .iter()
        .filter_map(|o| match o {
            Op::Geometry(GeoOp::Solid { ifc, .. }) => ifc.as_ref().map(|c| c.class.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(classes.len(), 3, "{classes:?}");
    assert_eq!(classes[0], "IfcPlate");
    assert_eq!(classes[1], "IfcMechanicalFastener");
}

#[test]
fn a_shell_is_a_skin_of_the_given_thickness() {
    let r = run(r#"
shell{surface = surf.dome{radius = 10, rise = 5}, nu = 12, nv = 4, thickness = "24 mm", material = "glass"}
"#);
    let n = ops(&r).iter().filter(|o| matches!(o, Op::Geometry(GeoOp::Solid { .. }))).count();
    assert!(n >= 40, "{n} panels");
}

#[test]
fn a_lattice_girder_has_chords_posts_and_diagonals() {
    let r = run(r#"
local g, info = lattice_girder{from = vec3(0, 0, 6), to = vec3(12, 0, 6), depth = 1.2, panels = 6}
assert(info.counts.chord == 12 and info.counts.post == 7 and info.counts.diagonal == 6, "counts " .. tostring(info.counts.chord) .. " " .. tostring(info.counts.post) .. " " .. tostring(info.counts.diagonal))
"#);
    assert!(ops(&r).len() >= 25);
}

#[test]
fn a_double_angle_is_a_profile_text() {
    let r = run(r#"
local f = extrude(profile.rect(0.1, 0.1), vec3(0, 0, 0), vec3(0, 0, 1))
member{from = vec3(0, 0, 0), to = vec3(3, 0, 0), profile = "2L 100x65x10", role = "brace"}
"#);
    assert!(!ops(&r).is_empty());
}

#[test]
fn furniture_and_pipes_are_elements_too() {
    let r = run(r#"
local w = wall{from = vec3(0, 0, 0), to = vec3(6, 0, 0), height = 3}
place_family{family = "Chair", at = vec3(1, 2, 0), rotation = "90 deg"}
pipe{system = "Cold water", path = {vec3(0, 0, 0.3), vec3(3, 0, 0.3), vec3(3, 2, 0.3)}, size = "20 mm"}
"#);
    let Op::Create { el: NewEl::Component { family, .. }, .. } = &ops(&r)[1] else { panic!("{:?}", ops(&r)[1]) };
    assert_eq!(family, &Ref::name(Cat::Family, "Chair"));
    let Op::Create { el: NewEl::MepRun { system, path, size, .. }, .. } = &ops(&r)[2] else { panic!() };
    assert_eq!((system.as_str(), path.len(), *size), ("Cold water", 3, Len::m(0.02)));
}

#[test]
fn furniture_and_pipes_say_what_is_missing() {
    let o = RunOpts::new(Ref::name(Cat::Level, "Level 1"), json!({}));
    for (src, want) in [
        (r#"place_family{at = vec3(0, 0, 0)}"#, "needs family and at"),
        (r#"pipe{path = {vec3(0, 0, 0), vec3(1, 0, 0)}}"#, "needs the system"),
        (r#"pipe{system = "x", path = {vec3(0, 0, 0)}}"#, "two or more points"),
    ] {
        let e = Sandbox.run(src, &o).err().unwrap_or_else(|| panic!("{src} ran")).to_string();
        assert!(e.contains(want), "{src}: {e}");
    }
}

#[test]
fn a_part_name_limit_counts_characters_not_bytes() {
    let o = RunOpts::new(Ref::name(Cat::Level, "Level 1"), json!({}));
    let ok = "ж".repeat(80);
    let too_long = "ж".repeat(81);
    let src = |n: &str| format!("part(\"{n}\", extrude(profile.rect(0.1, 0.1), vec3(0,0,0), vec3(1,0,0)))");
    if let Err(e) = Sandbox.run(&src(&ok), &o) {
        panic!("80 characters of 2 bytes each: {e}");
    }
    let e = Sandbox.run(&src(&too_long), &o).err().expect("81 characters").to_string();
    assert!(e.contains("80 characters"), "{e}");
}
