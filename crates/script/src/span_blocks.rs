// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Long-span blocks: bridges (girder, truss, tied arch, cable-stayed) with abutments, piers,
//! bearings, ramps and railings; stadium roofs (cantilever trusses, back stays, rings, a
//! seating bowl); hangar space frames (deep double-layer grid, runway beams, gate leaves).
//! Each block lays out its members once, sizes them by the structural check of
//! `span_check` (a linear frame analysis with the rules named there), then emits parts with
//! the sized sections. Passing a profile option fixes that group; `size = false` skips the
//! check and uses the plain defaults. The check, its reasons and the sections go into the
//! `check` table of the returned info and into the log.

use std::f64::consts::PI;
use std::time::Duration;

use ddc_ops::Profile;
use mlua::{Lua, Table, Value};

use crate::blocks::{block, lerp, prof_text, turn, v, Em, Opts};
use crate::span_check::{self as sc, Crit, Design, Group, Ladder, Lat, Rules};
use crate::values::{err, GroupH, LResult, V3, UP};

type Out = LResult<(GroupH, Table)>;

pub(crate) fn install(lua: &Lua, g: &Table) -> LResult<()> {
    g.set("bridge", block(lua, bridge)?)?;
    g.set("stadium_roof", block(lua, stadium_roof)?)?;
    g.set("space_frame", block(lua, space_frame)?)?;
    Ok(())
}

// ---- shared helpers ----

/// A member of the layout: role of the part, its ends, its section group.
struct El {
    role: &'static str,
    a: V3,
    b: V3,
    g: usize,
    pin: bool,
}

fn el(els: &mut Vec<El>, role: &'static str, a: V3, b: V3, g: usize, pin: bool) {
    els.push(El { role, a, b, g, pin });
}

fn given(o: &Opts, k: &'static str) -> LResult<bool> {
    Ok(!matches!(o.value(k)?, Value::Nil))
}

fn rules_of(o: &Opts) -> LResult<Rules> {
    let mut r = Rules::new();
    if let Some(t) = o.table("rules")? {
        for kv in t.pairs::<String, f64>() {
            let (k, x) = kv?;
            if let Err(e) = r.set(&k, x) {
                return err(format!("{}: rules: {e}", o.f));
            }
        }
    }
    Ok(r)
}

/// Section groups of a block; the ones the script gave a profile for are fixed.
fn groups_of(o: &Opts, specs: &[(&'static str, Ladder, bool)]) -> LResult<(Vec<Group>, Vec<Option<Profile>>)> {
    let mut gs = vec![];
    let mut us = vec![];
    for &(name, lad, tension) in specs {
        let mut g = Group::new(name, lad, tension);
        if given(o, name)? {
            g.fixed = true;
            let p = o.prof(name, "CHS 100x5")?;
            g.custom = sc::sec_of_profile(&p);
            us.push(Some(p));
        } else {
            us.push(None);
        }
        gs.push(g);
    }
    Ok((gs, us))
}

fn final_profile(g: &Group, user: &Option<Profile>, default: &str, sized: bool) -> LResult<Profile> {
    if let Some(p) = user {
        return Ok(p.clone());
    }
    let t = if sized { sc::text_of(g) } else { default.to_string() };
    prof_text(&t).map_err(mlua::Error::RuntimeError)
}

/// Half of the largest section dimension, m.
fn half_depth(p: &Profile) -> f64 {
    let e = crate::audit::extents(p);
    ((e[1] - e[0]).max(e[3] - e[2])) / 2.0
}

fn rz_at(lat: &Lat, d: &Design, p: V3) -> Option<[f64; 3]> {
    let n = lat.find(p)?;
    d.react.iter().find(|r| r.0 == n).map(|r| r.1)
}

/// Runs the check unless `size` is off or there is nothing to hold the lattice up.
fn run_design(lua: &Lua, lat: &Lat, groups: &mut [Group], crit: &Crit, r: &Rules, size: bool) -> LResult<Option<Design>> {
    if !size {
        return Ok(None);
    }
    let spent = crate::stdlib::builder(lua)?.cpu_credit.as_secs_f64();
    let left = sc::RUN_CAP - spent;
    if left <= 0.0 {
        return Err(mlua::Error::RuntimeError(format!(
            "the structural checks of this script used their {} s; size the remaining blocks with size = false or fix their sections",
            sc::RUN_CAP
        )));
    }
    let d = sc::design(lat, groups, crit, r, left.min(sc::BUDGET));
    crate::stdlib::builder(lua)?.cpu_credit += Duration::from_secs_f64(d.secs);
    Ok(Some(d))
}

struct Typical {
    lo: f64,
    hi: f64,
    what: &'static str,
}

/// The `check` table of the info and the log line: what was checked, against which rule,
/// and the sections.
#[allow(clippy::too_many_arguments)]
fn report(
    lua: &Lua,
    info: &Table,
    tag: &str,
    d: &Option<Design>,
    groups: &[Group],
    r: &Rules,
    defl_rule: &str,
    ratio: f64,
    typ: Typical,
    vib: bool,
    extra: &[String],
) -> LResult<()> {
    let t = lua.create_table()?;
    let mut why: Vec<String> = vec![];
    let mut notes: Vec<String> = vec![];
    let sized = d.as_ref().is_some_and(|d| d.failed.is_none());
    match d {
        None => why.push("check not run (size = false, or nothing in this block holds the lattice up): the default sections are used".into()),
        Some(d) => {
            if let Some(e) = &d.failed {
                why.push(format!("check skipped ({e}); the default sections are used"));
            } else {
                let um = r.get("util_max");
                why.push(format!(
                    "strength [util_max = {um}]: {}; largest member ratio {:.2}",
                    sc::why_of("util_max"),
                    d.util_max
                ));
                why.push(format!(
                    "stiffness [{defl_rule} = {}]: {}; deflection {:.0} mm against {:.0} mm",
                    r.get(defl_rule),
                    sc::why_of(defl_rule),
                    d.defl * 1e3,
                    d.limit * 1e3
                ));
                if vib {
                    why.push(format!(
                        "vibration [f_min = {} Hz]: {}; first vertical frequency about {:.1} Hz{}",
                        r.get("f_min"),
                        sc::why_of("f_min"),
                        d.f1,
                        if d.vib_ok { "" } else { ", under the requirement: a dynamic check is needed" }
                    ));
                }
                t.set("ok", d.ok)?;
                t.set("members", d.members)?;
                t.set("free_ends", d.free_ends)?;
                let pg = lua.create_table()?;
                for (g, n) in groups.iter().zip(&d.per_group) {
                    pg.set(g.name, *n)?;
                }
                t.set("group_members", pg)?;
                t.set("vibration_ok", d.vib_ok)?;
                t.set("util_max", d.util_max)?;
                t.set("deflection_mm", d.defl * 1e3)?;
                t.set("limit_mm", d.limit * 1e3)?;
                t.set("f1_hz", d.f1)?;
                t.set("mass_t", d.mass / 1e3)?;
                t.set("solves", d.solves)?;
                notes.extend(d.notes.iter().cloned());
            }
        }
    }
    why.push(format!("proportions: span/depth {ratio:.1}; usual {}-{} for {}", typ.lo, typ.hi, typ.what));
    if ratio < typ.lo || ratio > typ.hi {
        notes.push(format!("span/depth {ratio:.1} is outside the usual {}-{} for {}", typ.lo, typ.hi, typ.what));
    }
    let sections = lua.create_table()?;
    for (i, g) in groups.iter().enumerate() {
        let used = d.as_ref().is_some_and(|d| d.util.get(i).is_some_and(|u| *u > 0.0));
        if !used {
            continue;
        }
        let txt = if g.fixed {
            "given by the script".to_string()
        } else if sized {
            sc::text_of(g)
        } else {
            "default".to_string()
        };
        let u = d.as_ref().map_or(0.0, |d| d.util[i]);
        why.push(format!("{}: {txt} (ratio {u:.2})", g.name));
        sections.set(g.name, txt)?;
    }
    why.extend(extra.iter().cloned());
    t.set("span_depth", ratio)?;
    t.set("sections", sections)?;
    t.set("why", lua.create_sequence_from(why.clone())?)?;
    t.set("notes", lua.create_sequence_from(notes.clone())?)?;
    info.set("check", t)?;
    let mut b = crate::stdlib::builder(lua)?;
    let head = match d {
        Some(d) if d.failed.is_none() => format!(
            "{tag} check: ratio {:.2}, deflection {:.0} of {:.0} mm, span/depth {ratio:.1}{}, {:.1} t steel, {}",
            d.util_max,
            d.defl * 1e3,
            d.limit * 1e3,
            if vib { format!(", {:.1} Hz", d.f1) } else { String::new() },
            d.mass / 1e3,
            if d.ok { "passes" } else { "does not pass" }
        ),
        _ => format!("{tag} check: {}", why[0]),
    };
    b.print(head);
    for n in notes {
        b.print(format!("{tag} note: {n}"));
    }
    Ok(())
}

/// A box (plan rectangle x0..x1, y0..y1 in block axes) with its top at `top`, `t` thick,
/// as a slab part.
#[allow(clippy::too_many_arguments)]
fn slab_box(
    em: &mut Em,
    lw: &dyn Fn(V3) -> V3,
    role: &str,
    r: [f64; 4],
    top: f64,
    t: f64,
    mat: &str,
) -> LResult<()> {
    let at = |x: f64, y: f64| lw(v(x, y, 0.0));
    let outer = [at(r[0], r[2]), at(r[1], r[2]), at(r[1], r[3]), at(r[0], r[3])];
    em.slab(role, &outer, &[], lw(v(0.0, 0.0, top)).z, t, mat)
}

/// Balusters standing on `pts` (cross-beam nodes inboard of the girder line, so stays,
/// hangers and truss webs rising from the girder pass clear) and handrails between their tops.
fn rail(em: &mut Em, pts: &[V3], post: &Profile, mat: &str) -> LResult<()> {
    let mut prev: Option<V3> = None;
    for m in pts {
        let top = m.add(UP.scale(1.1));
        em.member("baluster", post, *m, top, mat)?;
        if let Some(p) = prev {
            em.member("handrail", post, p, top, mat)?;
        }
        prev = Some(top);
    }
    Ok(())
}

/// Gusset plates at both ends of every rod (hangers, stays): bolts and plate sized for the
/// largest axial force of the group. Returns the schedule lines for the check.
#[allow(clippy::too_many_arguments)]
fn gussets(
    em: &mut Em,
    els: &[El],
    groups: &[Group],
    des: &Option<Design>,
    r: &Rules,
    lw: &dyn Fn(V3) -> V3,
    mat: &str,
) -> LResult<Vec<String>> {
    let mut lines = vec![];
    let Some(d) = des.as_ref().filter(|d| d.failed.is_none()) else { return Ok(lines) };
    for (g, gr) in groups.iter().enumerate() {
        if !gr.tension || d.per_group.get(g).is_none_or(|n| *n == 0) {
            continue;
        }
        let n = d.force[g];
        let j = sc::joint(n, r);
        let mut plates = 0;
        let mut bolts = 0;
        for e in els.iter().filter(|e| e.g == g) {
            for (p, q) in [(e.a, e.b), (e.b, e.a)] {
                let Some(u) = q.sub(p).unit() else { continue };
                let w = u.cross(UP).unit().unwrap_or(v(1.0, 0.0, 0.0));
                let (hw, far) = (w.scale(j.w / 2.0), p.add(u.scale(j.l)));
                let pts: Vec<V3> = [p.sub(hw), p.add(hw), far.add(hw), far.sub(hw)].iter().map(|x| lw(*x)).collect();
                plates += em.panel("plate", &pts, j.t, mat)?;
                // bolts in two columns beside the rod axis, one bar from face to face of the plate
                let nrm = u.cross(w);
                let bolt = Profile::Circle { r: ddc_ops::Len::m(j.d / 2.0) };
                for k in 0..j.bolts {
                    let side = if k % 2 == 0 { -1.0 } else { 1.0 };
                    let at = p.add(u.scale(2.0 * j.pitch / 3.0 * 1.5 + (k / 2) as f64 * j.pitch)).add(w.scale(side * j.w / 4.0));
                    // head and nut stand half a diameter proud of the plate faces
                    let half = j.t / 2.0 + 0.5 * j.d;
                    let (a, b) = (at.sub(nrm.scale(half)), at.add(nrm.scale(half)));
                    if em.member("bolt", &bolt, lw(a), lw(b), mat)? {
                        bolts += 1;
                    }
                }
            }
        }
        lines.push(format!(
            "joints {}: {} at each end for {:.0} kN, {plates} plates and {bolts} bolts [bolt shear 0.6 f_ub A_s / 1.25, plate net section at fy]",
            gr.name,
            j.text(),
            n / 1e3
        ));
    }
    Ok(lines)
}

// ---- bridge ----

const B_GIRDER: usize = 0;
const B_CROSS: usize = 1;
const B_CHORD: usize = 2;
const B_WEB: usize = 3;
const B_ARCH: usize = 4;
const B_HANGER: usize = 5;
const B_STAY: usize = 6;
const B_PYLON: usize = 7;
const B_WIND: usize = 8;

const B_DEFAULT: [&str; 9] = [
    "I 600x300", "I 300x150", "SHS 300x16", "SHS 200x10", "CHS 610x25", "round 60", "round 80", "CHS 1016x40",
    "CHS 139.7x6",
];

/// Ramp geometry in its own axes: x from the abutment along the ramp, deck centre lines
/// at y = +-w2, z the deck height; `nb` bays.
struct Ramp {
    xs: Vec<f64>,
    zs: Vec<f64>,
}

/// A bridge. Kinds: girder (two deep plate girders), truss (Pratt side trusses), arch
/// (tied arch with hangers), cable_stayed (pylon and fan stays). Common: deck, cross beams,
/// wind bracing in the deck plane, bearings on abutments (and on `piers` interior piers),
/// optional approach ramps (`ramp_slope`), railings. Sections come from the structural
/// check; `rules = {...}` steers it.
fn bridge(lua: &Lua, t: Option<Table>) -> Out {
    let o = Opts::new("bridge", t, lua)?;
    let org = o.point("origin", v(0.0, 0.0, 0.0))?;
    let kind = o.choice("kind", "truss", &["truss", "arch", "cable_stayed", "girder"])?;
    let span = o.len_in("span", 60.0, 6.0, 1000.0)?;
    let width = o.len_in("width", 4.0, 1.5, 40.0)?;
    let deck_z = o.len_in("deck_z", 6.0, 0.5, 200.0)?;
    let bays = o.int("bays", 12, 2, 400)? as usize;
    let depth = o.len_in("depth", span / 12.0, 0.5, 60.0)?;
    let rise = o.len_in("rise", span / 5.0, 1.0, 300.0)?;
    let pylon_h = o.len_in("pylon_h", span * 0.4, 3.0, 400.0)?;
    let pylon_at = o.num("pylon_at", 0.35, 0.1, 0.9)?;
    let angle = o.ang("angle", 0.0)?;
    let piers = o.int("piers", 0, 0, 8)? as usize;
    let slope = o.num("ramp_slope", 0.0, 0.0, 0.12)?;
    let ramp_bay = o.len_in("ramp_bay", 10.0, 3.0, 30.0)?;
    let deck_t = o.len_in("deck", 0.2, 0.05, 1.0)?;
    let railing = o.flag("railing", true)?;
    let wind_bracing = o.flag("wind_bracing", true)?;
    let size = o.flag("size", true)?;
    let footbridge = o.flag("footbridge", width <= 6.0)?;
    let mat = o.mat("material", "S355")?;
    let deck_m = o.mat("deck_material", "C35/45")?;
    let conc = o.mat("abutment_material", "C30/37")?;
    let name = o.text("name", "bridge")?;
    let r = rules_of(&o)?;
    let specs = [
        ("girder", Ladder::Beam, false),
        ("cross", Ladder::Beam, false),
        ("chord", Ladder::Chs, false),
        ("web", Ladder::Chs, false),
        ("arch", Ladder::Chs, false),
        ("hanger", Ladder::Round, true),
        ("stay", Ladder::Round, true),
        ("pylon", Ladder::Chs, false),
        ("wind_brace", Ladder::Chs, false),
    ];
    let (mut groups, user) = groups_of(&o, &specs)?;
    let pier_user = if given(&o, "pier")? { Some(o.prof("pier", "CHS 813x25")?) } else { None };
    o.done()?;
    if piers > 0 && kind != "truss" && kind != "girder" {
        return err("bridge: piers apply to the truss and girder kinds; an arch or cable-stayed span has none inside");
    }
    if piers >= bays {
        return err("bridge: piers must be fewer than bays");
    }

    // ---- layout in block axes: x along the span, y across, z up; origin at the start abutment
    let w2 = width / 2.0;
    let xs: Vec<f64> = (0..=bays).map(|i| span * i as f64 / bays as f64).collect();
    let d = |i: usize, s: f64| v(xs[i], s * w2, deck_z);
    let lw = |p: V3| turn(v(org.x + p.x, org.y + p.y, org.z + p.z), org, angle);
    let mut kp: Vec<usize> = (1..=piers).map(|j| bays * j / (piers + 1)).filter(|k| *k > 0 && *k < bays).collect();
    kp.dedup();
    let pyl: Option<usize> = (kind == "cable_stayed").then(|| ((pylon_at * bays as f64).round() as usize).clamp(1, bays - 1));
    let off = w2 + 0.8;
    let leg = |s: f64, z: f64| v(xs[pyl.unwrap_or(1)], s * off, z);
    let top_z = deck_z + pylon_h;
    let mut els: Vec<El> = vec![];
    for i in 0..=bays {
        for s in [-1.0, 1.0] {
            if i < bays {
                el(&mut els, "girder", d(i, s), d(i + 1, s), B_GIRDER, false);
            }
        }
        el(&mut els, "cross_beam", d(i, -1.0), d(i, 1.0), B_CROSS, false);
        if wind_bracing && i < bays {
            let (a, b) = if i % 2 == 0 { (d(i, -1.0), d(i + 1, 1.0)) } else { (d(i, 1.0), d(i + 1, -1.0)) };
            el(&mut els, "wind_brace", a, b, B_WIND, false);
        }
    }
    match kind.as_str() {
        "truss" => {
            let top = |i: usize, s: f64| v(xs[i], s * w2, deck_z + depth);
            for s in [-1.0, 1.0] {
                for i in 0..=bays {
                    el(&mut els, "vertical", d(i, s), top(i, s), B_WEB, false);
                    if i < bays {
                        el(&mut els, "top_chord", top(i, s), top(i + 1, s), B_CHORD, false);
                        // Pratt: diagonals fall towards mid-span
                        if (i as f64) < bays as f64 / 2.0 {
                            el(&mut els, "diagonal", top(i, s), d(i + 1, s), B_WEB, false);
                        } else {
                            el(&mut els, "diagonal", d(i, s), top(i + 1, s), B_WEB, false);
                        }
                    }
                }
            }
            if depth >= 2.6 {
                for i in 0..=bays {
                    el(&mut els, "top_brace", top(i, -1.0), top(i, 1.0), B_WEB, false);
                }
            }
        }
        "arch" => {
            let cv = crate::blocks::Curve::new("bridge", "parabolic", span, rise)?;
            let a = |i: usize, s: f64| v(xs[i], s * w2, deck_z + cv.y(xs[i]));
            for s in [-1.0, 1.0] {
                for i in 0..bays {
                    el(&mut els, "arch", a(i, s), a(i + 1, s), B_ARCH, false);
                }
                for i in 1..bays {
                    el(&mut els, "hanger", d(i, s), a(i, s), B_HANGER, true);
                }
            }
            for i in 1..bays {
                if cv.y(xs[i]) >= 4.0 {
                    el(&mut els, "arch_brace", a(i, -1.0), a(i, 1.0), B_WEB, false);
                }
            }
        }
        "cable_stayed" => {
            let k = pyl.unwrap_or(1);
            let fan = (pylon_h * 0.35).max(1.0);
            let far = k.max(bays - k) as f64;
            let stay_z = |i: usize| top_z - 0.3 - fan * (1.0 - (i as f64 - k as f64).abs() / far);
            // a stay ends on a pylon node: the legs are split where the stays meet them
            let mut zs: Vec<f64> = vec![0.0, deck_z, top_z];
            zs.extend((1..bays).filter(|i| *i != k).map(stay_z));
            zs.sort_by(f64::total_cmp);
            zs.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
            for s in [-1.0, 1.0] {
                for w in zs.windows(2) {
                    el(&mut els, "pylon", leg(s, w[0]), leg(s, w[1]), B_PYLON, false);
                }
                el(&mut els, "pylon_beam", leg(s, deck_z), d(k, s), B_CROSS, false);
            }
            el(&mut els, "pylon_beam", leg(-1.0, top_z), leg(1.0, top_z), B_CROSS, false);
            for s in [-1.0, 1.0] {
                for i in 1..bays {
                    if i != k {
                        el(&mut els, "stay", d(i, s), leg(s, stay_z(i)), B_STAY, true);
                    }
                }
            }
        }
        _ => {}
    }

    // ---- check of the main span
    let mut lat = Lat::default();
    for e in &els {
        lat.member(e.a, e.b, e.g, e.pin);
    }
    for s in [-1.0, 1.0] {
        lat.support(d(0, s), [true, true, true], false);
        lat.support(d(bays, s), [false, true, true], false);
        for &k in &kp {
            lat.support(d(k, s), [false, true, true], false);
        }
        if pyl.is_some() {
            lat.support(leg(s, 0.0), [true, true, true], true);
        }
    }
    let h_exp = 1.1 + deck_t + if kind == "truss" { depth } else { 0.0 };
    let (q_dead, q_live) = ((deck_t * 25.0 + r.get("surfacing")) * 1e3, r.get("live_deck") * 1e3);
    let q_wind = r.get("wind") * 1e3 * h_exp;
    let mut crit_nodes = vec![];
    for i in 0..=bays {
        let trib = (xs[(i + 1).min(bays)] - xs[i.saturating_sub(1)]) / 2.0;
        let area = trib * w2;
        for s in [-1.0, 1.0] {
            lat.load(0, d(i, s), [0.0, 0.0, -q_dead * area]);
            lat.load(1, d(i, s), [0.0, 0.0, -q_live * area]);
            lat.load(2, d(i, s), [0.0, q_wind * trib / 2.0, 0.0]);
            if i > 0 && i < bays {
                crit_nodes.push(d(i, s));
            }
        }
    }
    let sub_span = span / (kp.len() + 1) as f64;
    let defl_rule = "defl_bridge";
    let crit = Crit {
        nodes: crit_nodes,
        limit: sub_span / r.get(defl_rule),
        total: false,
        stiff: vec![B_GIRDER, B_CHORD, B_ARCH, B_STAY, B_PYLON],
        f_min: if footbridge { r.get("f_min") } else { 0.0 },
    };
    let des = run_design(lua, &lat, &mut groups, &crit, &r, size)?;
    let sized = des.as_ref().is_some_and(|d| d.failed.is_none());
    let mut profs: Vec<Profile> = vec![];
    for (g, grp) in groups.iter().enumerate() {
        profs.push(final_profile(grp, &user[g], B_DEFAULT[g], sized)?);
    }
    let hh = half_depth(&profs[B_GIRDER]);
    let bh = 0.15;
    // bearings sit under the girder; the seat is below them
    let bear_top = deck_z - hh + 0.02;
    let seat = bear_top - bh;
    if seat < 0.3 {
        return err("bridge: deck_z leaves less than 0.3 m under the bearings; raise deck_z or use a shallower girder");
    }

    // ---- ramps: simple spans on piers, own check
    let ramp: Option<Ramp> = if slope > 0.0 {
        let len = deck_z / slope;
        let nb = ((len / ramp_bay).ceil() as usize).max(2);
        let xs: Vec<f64> = (0..=nb).map(|t| len * t as f64 / nb as f64).collect();
        let zs: Vec<f64> = (0..=nb).map(|t| deck_z * (1.0 - t as f64 / nb as f64)).collect();
        Some(Ramp { xs, zs })
    } else {
        None
    };
    let rspecs = [("girder", Ladder::Beam, false), ("cross", Ladder::Beam, false)];
    let mut rgroups: Vec<Group> = rspecs
        .iter()
        .enumerate()
        .map(|(i, &(n, l, t))| {
            let mut g = Group::new(n, l, t);
            g.fixed = user[i].is_some();
            g
        })
        .collect();
    let mut rprofs: Vec<Profile> = vec![];
    let mut rdes: Option<Design> = None;
    let mut rlat = Lat::default();
    let rpier_at = |ra: &Ramp, t: usize| t > 0 && t + 1 < ra.xs.len() && ra.zs[t] >= 2.0;
    if let Some(ra) = &ramp {
        let nb = ra.xs.len() - 1;
        let rp = |t: usize, s: f64| v(ra.xs[t], s * w2, ra.zs[t]);
        for t in 0..=nb {
            for s in [-1.0, 1.0] {
                if t < nb {
                    rlat.member(rp(t, s), rp(t + 1, s), 0, false);
                }
            }
            rlat.member(rp(t, -1.0), rp(t, 1.0), 1, false);
        }
        let mut cn = vec![];
        for s in [-1.0, 1.0] {
            rlat.support(rp(0, s), [true, true, true], false);
            rlat.support(rp(nb, s), [false, true, true], false);
            for t in 1..nb {
                if rpier_at(ra, t) {
                    rlat.support(rp(t, s), [false, true, true], false);
                }
            }
        }
        for t in 0..=nb {
            let trib = (ra.xs[(t + 1).min(nb)] - ra.xs[t.saturating_sub(1)]) / 2.0;
            for s in [-1.0, 1.0] {
                rlat.load(0, rp(t, s), [0.0, 0.0, -q_dead * trib * w2]);
                rlat.load(1, rp(t, s), [0.0, 0.0, -q_live * trib * w2]);
                rlat.load(2, rp(t, s), [0.0, q_wind * trib / 2.0 * 0.5, 0.0]);
                if t > 0 && t < nb {
                    cn.push(rp(t, s));
                }
            }
        }
        let rc = Crit { nodes: cn, limit: 2.0 * ramp_bay / r.get(defl_rule), total: false, stiff: vec![0], f_min: 0.0 };
        rdes = run_design(lua, &rlat, &mut rgroups, &rc, &r, size)?;
        let rsized = rdes.as_ref().is_some_and(|d| d.failed.is_none());
        for (g, grp) in rgroups.iter().enumerate() {
            rprofs.push(final_profile(grp, &user[g], B_DEFAULT[g], rsized)?);
        }
    }

    // railings stand inboard of the girder line, clear of every section rising in its plane
    let rail_p = prof_text("CHS 48.3x4").map_err(mlua::Error::RuntimeError)?;
    let reach = [B_GIRDER, B_CHORD, B_WEB, B_ARCH, B_HANGER, B_STAY]
        .iter()
        .filter(|g| els.iter().any(|e| e.g == **g))
        .map(|g| half_depth(&profs[*g]))
        .chain(rprofs.first().map(half_depth))
        .fold(0.0, f64::max);
    let inset = reach + half_depth(&rail_p) + 0.05;
    if railing && inset >= w2 {
        return err("bridge: the girder sections leave no walkway between the railings; widen the deck or set railing = false");
    }

    // ---- emit
    let mut em = Em::new(lua, "bridge");
    for e in &els {
        em.member(e.role, &profs[e.g], lw(e.a), lw(e.b), &mat)?;
    }
    let joint_lines = gussets(&mut em, &els, &groups, &des, &r, &lw, &mat)?;
    for i in 0..bays {
        let q = [lw(d(i, -1.0)), lw(d(i + 1, -1.0)), lw(d(i + 1, 1.0)), lw(d(i, 1.0))];
        em.panel("deck", &q, deck_t, &deck_m)?;
    }
    // supports: bearing pad sized by the reaction, abutment or pier below it
    let pad = |rz: Option<[f64; 3]>| -> f64 {
        let n = rz.map_or(0.0, |f| f[2].abs());
        (n / (r.get("bearing_stress") * 1e6)).sqrt().clamp(0.3, 1.5)
    };
    let foot = |rz: Option<[f64; 3]>| -> f64 {
        let n = rz.map_or(0.0, |f| f[2].abs()) / 1.4;
        (n / (r.get("soil") * 1e3)).sqrt().clamp(1.6, 9.0)
    };
    let pad_of = |x: f64, y: f64, side: f64| [x - side / 2.0, x + side / 2.0, y - side / 2.0, y + side / 2.0];
    let nodes_rz = |lt: &Lat, ds: &Option<Design>, p: V3| ds.as_ref().and_then(|dd| rz_at(lt, dd, p));
    for (x_end, dir) in [(0.0, -1.0), (span, 1.0)] {
        let (x0, x1) = if dir < 0.0 { (x_end - 3.0, x_end + 0.6) } else { (x_end - 0.6, x_end + 3.0) };
        slab_box(&mut em, &lw, "abutment", [x0, x1, -(w2 + 1.0), w2 + 1.0], seat, seat, &conc)?;
        let (f0, f1) = if dir < 0.0 { (x_end - 4.0, x_end + 1.6) } else { (x_end - 1.6, x_end + 4.0) };
        slab_box(&mut em, &lw, "footing", [f0, f1, -(w2 + 2.0), w2 + 2.0], 0.0, 1.0, &conc)?;
        let i = if dir < 0.0 { 0 } else { bays };
        for s in [-1.0, 1.0] {
            let side = pad(nodes_rz(&lat, &des, d(i, s)));
            slab_box(&mut em, &lw, "bearing", pad_of(x_end, s * w2, side), bear_top, bh, &mat)?;
        }
    }
    let height = seat;
    let mut ppr: Option<Profile> = pier_user.clone();
    for &k in &kp {
        for s in [-1.0, 1.0] {
            let rz = nodes_rz(&lat, &des, d(k, s));
            if ppr.is_none() {
                let (n, hz) = rz.map_or((2e6, 1e5), |f| (f[2].abs(), f[0].hypot(f[1])));
                ppr = Some(prof_text(&sc::ladder(Ladder::Chs)[sc::size_column(Ladder::Chs, n, hz, height, &r)].text).map_err(mlua::Error::RuntimeError)?);
            }
            let pp = ppr.clone().unwrap_or(profs[B_WEB].clone());
            em.member("pier", &pp, lw(v(xs[k], s * w2, 0.0)), lw(v(xs[k], s * w2, seat)), &mat)?;
            let side = pad(rz);
            slab_box(&mut em, &lw, "bearing", pad_of(xs[k], s * w2, side), bear_top, bh, &mat)?;
            let fs = foot(rz);
            slab_box(&mut em, &lw, "footing", pad_of(xs[k], s * w2, fs), 0.0, 0.8, &conc)?;
        }
    }
    if let Some(ra) = &ramp {
        let nb = ra.xs.len() - 1;
        let hr = half_depth(&rprofs[0]);
        let mut cpr: Option<Profile> = pier_user.clone();
        // one column section for all ramp piers, from the largest reaction
        if cpr.is_none() {
            let (mut n, mut hz) = (0.0f64, 0.0f64);
            for t in 1..nb {
                for s in [-1.0, 1.0] {
                    if let Some(f) = nodes_rz(&rlat, &rdes, v(ra.xs[t], s * w2, ra.zs[t])) {
                        n = n.max(f[2].abs());
                        hz = hz.max(f[0].hypot(f[1]));
                    }
                }
            }
            let n = if n > 0.0 { n } else { 1.5e6 };
            let hmax = ra.zs[1].max(2.0);
            cpr = Some(prof_text(&sc::ladder(Ladder::Chs)[sc::size_column(Ladder::Chs, n, hz.max(5e4), hmax, &r)].text).map_err(mlua::Error::RuntimeError)?);
        }
        let cp = cpr.unwrap_or(profs[B_WEB].clone());
        for end in [false, true] {
            let m = |t: usize, s: f64, z: f64| v(if end { span + ra.xs[t] } else { -ra.xs[t] }, s * w2, z);
            for t in 0..nb {
                for s in [-1.0, 1.0] {
                    em.member("girder", &rprofs[0], lw(m(t, s, ra.zs[t])), lw(m(t + 1, s, ra.zs[t + 1])), &mat)?;
                }
            }
            for t in 0..=nb {
                em.member("cross_beam", &rprofs[1], lw(m(t, -1.0, ra.zs[t])), lw(m(t, 1.0, ra.zs[t])), &mat)?;
                if t < nb {
                    let q = [
                        lw(m(t, -1.0, ra.zs[t])),
                        lw(m(t + 1, -1.0, ra.zs[t + 1])),
                        lw(m(t + 1, 1.0, ra.zs[t + 1])),
                        lw(m(t, 1.0, ra.zs[t])),
                    ];
                    em.panel("deck", &q, deck_t, &deck_m)?;
                }
            }
            for t in 1..nb {
                if !rpier_at(ra, t) {
                    continue;
                }
                let btop = ra.zs[t] - hr + 0.02;
                for s in [-1.0, 1.0] {
                    let p0 = m(t, s, 0.0);
                    let rz = nodes_rz(&rlat, &rdes, v(ra.xs[t], s * w2, ra.zs[t]));
                    em.member("pier", &cp, lw(p0), lw(v(p0.x, p0.y, btop - bh)), &mat)?;
                    slab_box(&mut em, &lw, "bearing", pad_of(p0.x, p0.y, pad(rz)), btop, bh, &mat)?;
                    slab_box(&mut em, &lw, "footing", pad_of(p0.x, p0.y, foot(rz)), 0.0, 0.8, &conc)?;
                }
            }
            if railing {
                for s in [-1.0, 1.0] {
                    let pts: Vec<V3> = (0..=nb).map(|t| lw(m(t, s * (w2 - inset) / w2, ra.zs[t]))).collect();
                    rail(&mut em, &pts, &rail_p, &mat)?;
                }
            }
        }
    }
    if railing {
        for s in [-1.0, 1.0] {
            let pts: Vec<V3> = (0..=bays).map(|i| lw(v(xs[i], s * (w2 - inset), deck_z))).collect();
            rail(&mut em, &pts, &rail_p, &mat)?;
        }
    }
    let (g, info) = em.finish(&name)?;

    // ---- report
    let (ratio, typ) = match kind.as_str() {
        "arch" => (span / rise, Typical { lo: 4.0, hi: 10.0, what: "a tied arch (span/rise)" }),
        "cable_stayed" => (span / pylon_h, Typical { lo: 2.0, hi: 5.0, what: "a cable-stayed span (span/pylon height above deck)" }),
        "girder" => (span / (2.0 * hh), Typical { lo: 15.0, hi: 30.0, what: "a plate girder bridge" }),
        _ => (span / depth, Typical { lo: 8.0, hi: 16.0, what: "a truss bridge" }),
    };
    let mut extra: Vec<String> = joint_lines;
    extra.push(format!(
        "bearings: pads sized so the reaction stays under {} MPa [bearing_stress]; {}",
        r.get("bearing_stress"),
        sc::why_of("bearing_stress")
    ));
    extra.push(format!("footings: plan from the service reaction over {} kPa [soil]; {}", r.get("soil"), sc::why_of("soil")));
    if let Some(rd) = &rdes {
        if rd.failed.is_none() {
            extra.push(format!(
                "ramps: slope {:.1} %, {} bays of {:.1} m on piers where the deck is above 2 m; girder ratio {:.2}, deflection {:.0} of {:.0} mm",
                slope * 100.0,
                ramp.as_ref().map_or(0, |x| x.xs.len() - 1),
                ramp_bay,
                rd.util_max,
                rd.defl * 1e3,
                rd.limit * 1e3
            ));
        }
    }
    let mut all = groups.clone();
    for (i, g) in all.iter_mut().enumerate() {
        g.fixed = user[i].is_some();
    }
    report(lua, &info, "bridge", &des, &all, &r, defl_rule, ratio, typ, footbridge, &extra)?;
    Ok((g, info))
}

// ---- stadium roof ----

const S_CHORD: usize = 0;
const S_WEB: usize = 1;
const S_RING: usize = 2;
const S_PURLIN: usize = 3;
const S_COLUMN: usize = 4;
const S_STAY: usize = 5;

const S_DEFAULT: [&str; 6] = ["CHS 457x16", "CHS 219.1x10", "CHS 610x20", "RHS 300x200x10", "CHS 914x30", "round 100"];

/// A stadium roof: cantilever trusses round an elliptic opening, two columns each (the front one
/// `backspan` ahead of the back one), purlins,
/// front compression ring, roof panels, back stays to ground anchors (`backstay`), and with
/// `bowl` three raked seating tiers under it on posts.
fn stadium_roof(lua: &Lua, t: Option<Table>) -> Out {
    let o = Opts::new("stadium_roof", t, lua)?;
    let c = o.point("center", v(0.0, 0.0, 0.0))?;
    let ra = o.len_in("a", 70.0, 10.0, 400.0)?;
    let rb = o.len_in("b", 50.0, 10.0, 400.0)?;
    let depth = o.len_in("depth", 30.0, 4.0, 120.0)?;
    let bays = o.int("bays", 36, 8, 400)? as usize;
    let m = o.int("divisions", 6, 2, 40)? as usize;
    let height = o.len_in("height", 32.0, 6.0, 200.0)?;
    let back_d = o.len_in("truss_depth", 6.0, 1.0, 30.0)?;
    let front_d = o.len_in("front_depth", 1.5, 0.5, 30.0)?;
    let rake = o.num("rake", 0.08, -0.5, 0.5)?;
    // back column to front column; 0 puts the front column on the first panel node
    let backspan = o.len_in("backspan", 0.0, 0.0, 120.0)?;
    let cover = o.len_in("cover", 0.05, 0.0, 0.3)?;
    let backstay = o.flag("backstay", true)?;
    let stay_every = o.int("stay_every", 2, 1, 40)? as usize;
    let anchor = o.len_in("anchor", height * 0.8, 3.0, 200.0)?;
    let bowl = o.flag("bowl", false)?;
    let inset = o.len_in("inset", 8.0, 0.0, 40.0)?;
    let size = o.flag("size", true)?;
    let mat = o.mat("material", "S355")?;
    let cmat = o.mat("cover_material", "polycarbonate")?;
    let conc = o.mat("bowl_material", "C35/45")?;
    let name = o.text("name", "stadium roof")?;
    let r = rules_of(&o)?;
    let specs = [
        ("chord", Ladder::Chs, false),
        ("web", Ladder::Chs, false),
        ("ring", Ladder::Chs, false),
        ("purlin", Ladder::Chs, false),
        ("column", Ladder::Chs, false),
        ("stay", Ladder::Round, true),
    ];
    let (mut groups, user) = groups_of(&o, &specs)?;
    o.done()?;
    if back_d > height - 3.0 {
        return err("stadium_roof: truss_depth leaves less than 3 m under the back of the roof");
    }
    // the front column stands on the panel node nearest the asked back-span
    let kf = if backspan > 0.0 { ((backspan * m as f64 / depth).round() as usize).clamp(1, m - 1) } else { 1 };
    if backspan > 0.0 {
        let (back, reach) = (depth * kf as f64 / m as f64, depth * (m - kf) as f64 / m as f64);
        if back < reach / 4.0 {
            return err(format!(
                "stadium_roof: back-span {back:.1} m is under a quarter of the {reach:.1} m cantilever; raise backspan or divisions"
            ));
        }
    }
    let normal = |th: f64| v(th.cos() / ra, th.sin() / rb, 0.0).unit().unwrap_or(v(1.0, 0.0, 0.0));
    let at = |i: usize, k: usize, bottom: bool| {
        let th = 2.0 * PI * (i % bays) as f64 / bays as f64;
        let inner = v(c.x + ra * th.cos(), c.y + rb * th.sin(), 0.0);
        let f = k as f64 / m as f64; // 0 back, 1 front
        let p = inner.add(normal(th).scale(depth * (1.0 - f)));
        let top = height + rake * depth * f;
        let dd = back_d + (front_d - back_d) * f;
        v(p.x, p.y, c.z + if bottom { top - dd } else { top })
    };
    let anchor_at = |i: usize| {
        let th = 2.0 * PI * (i % bays) as f64 / bays as f64;
        let inner = v(c.x + ra * th.cos(), c.y + rb * th.sin(), c.z);
        inner.add(normal(th).scale(depth + anchor))
    };
    let mut els: Vec<El> = vec![];
    for i in 0..bays {
        for k in 0..=m {
            el(&mut els, "vertical", at(i, k, true), at(i, k, false), S_WEB, false);
            if k < m {
                el(&mut els, "top_chord", at(i, k, false), at(i, k + 1, false), S_CHORD, false);
                el(&mut els, "bottom_chord", at(i, k, true), at(i, k + 1, true), S_CHORD, false);
                el(&mut els, "diagonal", at(i, k, false), at(i, k + 1, true), S_WEB, false);
            }
            if k == m {
                el(&mut els, "front_ring", at(i, k, false), at(i + 1, k, false), S_RING, false);
            } else {
                el(&mut els, "purlin", at(i, k, false), at(i + 1, k, false), S_PURLIN, false);
            }
            if k == 0 {
                el(&mut els, "back_ring", at(i, 0, true), at(i + 1, 0, true), S_RING, false);
            }
        }
        for k in [0usize, kf] {
            let b = at(i, k, true);
            el(&mut els, "column", v(b.x, b.y, c.z), b, S_COLUMN, false);
        }
        if backstay && i % stay_every == 0 {
            el(&mut els, "stay", at(i, 0, false), anchor_at(i), S_STAY, true);
        }
    }
    let mut lat = Lat::default();
    for e in &els {
        lat.member(e.a, e.b, e.g, e.pin);
    }
    for e in &els {
        if e.g == S_COLUMN {
            lat.support(e.a, [true, true, true], true);
        }
        if e.g == S_STAY {
            lat.support(e.b, [true, true, true], true);
        }
    }
    let (q_dead, q_live, q_up) = (r.get("dead_roof") * 1e3, r.get("live_roof") * 1e3, r.get("uplift") * 1e3);
    for i in 0..bays {
        for k in 0..=m {
            let circ = at(i + 1, k, false).sub(at(i + bays - 1, k, false)).len() / 2.0;
            let rad = if k == 0 {
                at(i, 1, false).sub(at(i, 0, false)).len() / 2.0
            } else if k == m {
                at(i, m, false).sub(at(i, m - 1, false)).len() / 2.0
            } else {
                at(i, k + 1, false).sub(at(i, k - 1, false)).len() / 2.0
            };
            let area = circ * rad;
            let p = at(i, k, false);
            lat.load(0, p, [0.0, 0.0, -q_dead * area]);
            lat.load(1, p, [0.0, 0.0, -q_live * area]);
            lat.load(2, p, [0.0, 0.0, q_up * area]);
        }
    }
    let crit = Crit {
        nodes: (0..bays).map(|i| at(i, m, false)).collect(),
        limit: depth / r.get("defl_cantilever"),
        total: false,
        stiff: vec![S_CHORD, S_WEB],
        f_min: 0.0,
    };
    let des = run_design(lua, &lat, &mut groups, &crit, &r, size)?;
    let sized = des.as_ref().is_some_and(|d| d.failed.is_none());
    let mut profs: Vec<Profile> = vec![];
    for (g, grp) in groups.iter().enumerate() {
        profs.push(final_profile(grp, &user[g], S_DEFAULT[g], sized)?);
    }

    let mut em = Em::new(lua, "stadium_roof");
    for e in &els {
        em.member(e.role, &profs[e.g], e.a, e.b, &mat)?;
    }
    let joint_lines = gussets(&mut em, &els, &groups, &des, &r, &|p| p, &mat)?;
    if cover > 0.0 {
        for i in 0..bays {
            for k in 0..m {
                let q = [at(i, k, false), at(i, k + 1, false), at(i + 1, k + 1, false), at(i + 1, k, false)];
                em.panel("cover", &q, cover, &cmat)?;
            }
        }
    }
    if backstay {
        for i in (0..bays).filter(|i| i % stay_every == 0) {
            let a = anchor_at(i);
            let outer = [v(a.x - 1.5, a.y - 1.5, 0.0), v(a.x + 1.5, a.y - 1.5, 0.0), v(a.x + 1.5, a.y + 1.5, 0.0), v(a.x - 1.5, a.y + 1.5, 0.0)];
            em.slab("footing", &outer, &[], c.z, 1.2, &conc)?;
        }
    }
    let mut tiers = 0;
    if bowl {
        // three raked tiers between the pitch edge (inset inside the opening) and the columns,
        // aisles between them; every tier panel stands on posts to the ground
        let end = depth * (1.0 - 1.0 / m as f64) - 2.5;
        if end <= 6.0 {
            return err("stadium_roof: the roof is too shallow for a seating bowl (depth must exceed about 9 m plus one division)");
        }
        let lo = -inset;
        let third = (end - lo - 5.0) / 3.0;
        let bands: Vec<(f64, f64)> = (0..3).map(|j| (lo + j as f64 * (third + 2.5), lo + j as f64 * (third + 2.5) + third)).collect();
        let rise_per_m = 0.45;
        let outward = |i: usize, off: f64, j: usize| {
            let th = 2.0 * PI * (i % bays) as f64 / bays as f64;
            let inner = v(c.x + ra * th.cos(), c.y + rb * th.sin(), 0.0);
            // off measured from the opening edge outwards (negative = inside the opening)
            let p = inner.add(normal(th).scale(off));
            v(p.x, p.y, c.z + 0.6 + rise_per_m * (off - lo) + 1.2 * j as f64)
        };
        let post = prof_text("CHS 193.7x8").map_err(mlua::Error::RuntimeError)?;
        for (j, (o0, o1)) in bands.iter().enumerate() {
            for i in 0..bays {
                let q = [outward(i, *o0, j), outward(i, *o1, j), outward(i + 1, *o1, j), outward(i + 1, *o0, j)];
                if em.panel("slab", &q, 0.3, &conc)? > 0 {
                    tiers += 1;
                }
                for p in [q[0], q[1]] {
                    let base = v(p.x, p.y, c.z);
                    em.member("post", &post, base, v(p.x, p.y, p.z - 0.15), &mat)?;
                }
            }
        }
    }
    let (g, info) = em.finish(&name)?;
    if bowl {
        info.set("tiers", tiers)?;
    }
    let mut all = groups.clone();
    for (i, g) in all.iter_mut().enumerate() {
        g.fixed = user[i].is_some();
    }
    let ratio = depth / back_d;
    let mut extra = vec![format!(
        "loads: roof dead {} kN/m2 [dead_roof], imposed {} [live_roof], suction {} [uplift]; the tip deflection limit is the overhang over {} [defl_cantilever]",
        r.get("dead_roof"),
        r.get("live_roof"),
        r.get("uplift"),
        r.get("defl_cantilever")
    )];
    extra.extend(joint_lines);
    let typ = Typical { lo: 3.0, hi: 8.0, what: "a cantilever roof truss (overhang/depth at the root)" };
    report(lua, &info, "stadium_roof", &des, &all, &r, "defl_cantilever", ratio, typ, false, &extra)?;
    Ok((g, info))
}

// ---- space frame / hangar ----

const H_CHORD: usize = 0;
const H_BOTTOM: usize = 1;
const H_DIAG: usize = 2;
const H_COLUMN: usize = 3;
const H_RUNWAY: usize = 4;

const H_DEFAULT: [&str; 5] = ["CHS 168.3x8", "CHS 168.3x8", "CHS 114.3x6", "CHS 406.4x12", "I 600x300"];

/// A double-layer space frame on columns. Hangar options: `crane_z` (runway beams between
/// the columns of the two long sides at that height, a crane girder across at mid-length)
/// and `gates` (sliding leaves with a header along one long side, up to `gate_h`).
fn space_frame(lua: &Lua, t: Option<Table>) -> Out {
    let o = Opts::new("space_frame", t, lua)?;
    let org = o.point("origin", v(0.0, 0.0, 0.0))?;
    let nx = o.int("nx", 10, 1, 200)? as usize;
    let ny = o.int("ny", 8, 1, 200)? as usize;
    let bay = o.len_in("bay", 3.0, 0.5, 20.0)?;
    let depth = o.len_in("depth", bay / std::f64::consts::SQRT_2, 0.3, 20.0)?;
    let z = o.len_in("height", 8.0, 1.0, 200.0)?;
    let angle = o.ang("angle", 0.0)?;
    let cols = o.choice("columns", "corners", &["corners", "perimeter", "none"])?;
    let col_every = o.int("column_every", 3, 1, 100)? as usize;
    let deck = o.len_in("deck", 0.0, 0.0, 0.5)?;
    let crane_z = o.len_in("crane_z", 0.0, 0.0, 100.0)?;
    let gates = o.int("gates", 0, 0, 20)? as usize;
    let gate_h = o.len_in("gate_h", 0.0, 0.0, 100.0)?;
    let size = o.flag("size", true)?;
    let steel = o.mat("material", "S355")?;
    let deck_m = o.mat("deck_material", "steel deck")?;
    let name = o.text("name", "space frame")?;
    let r = rules_of(&o)?;
    let specs = [
        ("chord", Ladder::Chs, false),
        ("bottom", Ladder::Chs, false),
        ("diagonal", Ladder::Chs, false),
        ("column", Ladder::Chs, false),
        ("runway", Ladder::Beam, false),
    ];
    let (mut groups, mut user) = groups_of(&o, &specs)?;
    let header = o.prof("header", "I 500x200")?;
    o.done()?;
    // one chord profile fixes both layers unless the bottom layer is given too
    if user[H_BOTTOM].is_none() && user[H_CHORD].is_some() {
        user[H_BOTTOM] = user[H_CHORD].clone();
        groups[H_BOTTOM].fixed = true;
    }
    if z - depth < 0.5 {
        return err("space_frame: height must leave 0.5 m under the bottom chords");
    }
    let p = |x: f64, y: f64, zz: f64| v(x, y, zz);
    let lw = |q: V3| turn(v(org.x + q.x, org.y + q.y, org.z + q.z), org, angle);
    let top = |i: usize, j: usize| p(i as f64 * bay, j as f64 * bay, z);
    let bot = |i: usize, j: usize| p((i as f64 + 0.5) * bay, (j as f64 + 0.5) * bay, z - depth);
    let under = |i: usize, j: usize| {
        let b = bot(i, j);
        v(b.x, b.y, 0.0)
    };
    let mut els: Vec<El> = vec![];
    for i in 0..=nx {
        for j in 0..=ny {
            if i < nx {
                el(&mut els, "top_chord", top(i, j), top(i + 1, j), H_CHORD, false);
            }
            if j < ny {
                el(&mut els, "top_chord", top(i, j), top(i, j + 1), H_CHORD, false);
            }
        }
    }
    for i in 0..nx {
        for j in 0..ny {
            if i + 1 < nx {
                el(&mut els, "bottom_chord", bot(i, j), bot(i + 1, j), H_BOTTOM, false);
            }
            if j + 1 < ny {
                el(&mut els, "bottom_chord", bot(i, j), bot(i, j + 1), H_BOTTOM, false);
            }
            for (a, b) in [(i, j), (i + 1, j), (i, j + 1), (i + 1, j + 1)] {
                el(&mut els, "diagonal", bot(i, j), top(a, b), H_DIAG, false);
            }
        }
    }
    let edge = |i: usize, j: usize| i == 0 || j == 0 || i + 1 == nx || j + 1 == ny;
    let mut col_at: Vec<(usize, usize)> = vec![];
    for i in 0..nx {
        for j in 0..ny {
            let corner = (i == 0 || i + 1 == nx) && (j == 0 || j + 1 == ny);
            let along = (i % col_every == 0 || i + 1 == nx) && (j % col_every == 0 || j + 1 == ny);
            let put = match cols.as_str() {
                "corners" => corner,
                "perimeter" => edge(i, j) && along,
                _ => false,
            };
            if put {
                col_at.push((i, j));
            }
        }
    }
    let split = crane_z > 0.0;
    if split && crane_z > z - depth - 0.5 {
        return err("space_frame: crane_z must be at least 0.5 m under the bottom chords");
    }
    let along_x = nx >= ny;
    // runway rows: columns on the two long sides, ordered along the span axis
    let (row_a, row_b): (Vec<(usize, usize)>, Vec<(usize, usize)>) = {
        let mut a: Vec<(usize, usize)> = vec![];
        let mut b: Vec<(usize, usize)> = vec![];
        for &(i, j) in &col_at {
            if along_x {
                if j == 0 {
                    a.push((i, j));
                } else if j + 1 == ny {
                    b.push((i, j));
                }
            } else if i == 0 {
                a.push((i, j));
            } else if i + 1 == nx {
                b.push((i, j));
            }
        }
        let key = |c: &(usize, usize)| if along_x { c.0 } else { c.1 };
        a.sort_by_key(key);
        b.sort_by_key(key);
        (a, b)
    };
    for &(i, j) in &col_at {
        let (b0, b1) = (under(i, j), bot(i, j));
        if split {
            let mid = v(b0.x, b0.y, crane_z);
            el(&mut els, "column", b0, mid, H_COLUMN, false);
            el(&mut els, "column", mid, b1, H_COLUMN, false);
        } else {
            el(&mut els, "column", b0, b1, H_COLUMN, false);
        }
    }
    let mut crane_nodes: Vec<V3> = vec![];
    let mut runway_mid: Vec<(V3, V3)> = vec![];
    if split {
        let mut mids: [Vec<V3>; 2] = [vec![], vec![]];
        for (n, row) in [&row_a, &row_b].iter().enumerate() {
            for w in row.windows(2) {
                let (a, b) = (under(w[0].0, w[0].1), under(w[1].0, w[1].1));
                let (pa, pb) = (v(a.x, a.y, crane_z), v(b.x, b.y, crane_z));
                let mid = lerp(pa, pb, 0.5);
                el(&mut els, "crane_beam", pa, mid, H_RUNWAY, false);
                el(&mut els, "crane_beam", mid, pb, H_RUNWAY, false);
                crane_nodes.push(mid);
                mids[n].push(mid);
            }
        }
        for (a, b) in mids[0].iter().zip(mids[1].iter()) {
            runway_mid.push((*a, *b));
        }
    }
    let mut lat = Lat::default();
    for e in &els {
        lat.member(e.a, e.b, e.g, e.pin);
    }
    for &(i, j) in &col_at {
        lat.support(under(i, j), [true, true, true], true);
    }
    let (q_dead, q_live, q_up) = (r.get("dead_roof") * 1e3, r.get("live_roof") * 1e3, r.get("uplift") * 1e3);
    let mut tops = vec![];
    for i in 0..=nx {
        for j in 0..=ny {
            let wx = if i == 0 || i == nx { 0.5 } else { 1.0 };
            let wy = if j == 0 || j == ny { 0.5 } else { 1.0 };
            let area = bay * bay * wx * wy;
            lat.load(0, top(i, j), [0.0, 0.0, -q_dead * area]);
            lat.load(1, top(i, j), [0.0, 0.0, -q_live * area]);
            lat.load(2, top(i, j), [0.0, 0.0, q_up * area]);
            tops.push(top(i, j));
        }
    }
    for n in &crane_nodes {
        lat.load(1, *n, [0.0, 0.0, -r.get("crane") * 1e3]);
    }
    let short = nx.min(ny) as f64 * bay;
    let crit = Crit {
        nodes: tops,
        limit: short / r.get("defl_roof"),
        total: false,
        stiff: vec![H_CHORD, H_BOTTOM, H_DIAG],
        f_min: 0.0,
    };
    let des = if col_at.is_empty() { None } else { run_design(lua, &lat, &mut groups, &crit, &r, size)? };
    let sized = des.as_ref().is_some_and(|d| d.failed.is_none());
    let mut profs: Vec<Profile> = vec![];
    for (g, grp) in groups.iter().enumerate() {
        profs.push(final_profile(grp, &user[g], H_DEFAULT[g], sized)?);
    }

    let mut em = Em::new(lua, "space_frame");
    for e in &els {
        em.member(e.role, &profs[e.g], lw(e.a), lw(e.b), &steel)?;
    }
    for i in 0..nx {
        for j in 0..ny {
            if deck > 0.0 {
                let q = [lw(top(i, j)), lw(top(i + 1, j)), lw(top(i + 1, j + 1)), lw(top(i, j + 1))];
                em.panel("deck", &q, deck, &deck_m)?;
            }
        }
    }
    if let Some((a, b)) = runway_mid.get(runway_mid.len() / 2) {
        // the crane's own girder spans the hall: about span/25 deep, from the beam ladder
        let want = b.sub(*a).len() / 25.0;
        let lad = sc::ladder(Ladder::Beam);
        let sec = lad.iter().find(|c| c.h >= want).unwrap_or(&lad[lad.len() - 1]);
        let cg = prof_text(&sec.text).map_err(mlua::Error::RuntimeError)?;
        em.member("crane_girder", &cg, lw(*a), lw(*b), &steel)?;
    }
    if gates > 0 && row_a.len() >= 2 {
        let gh = if gate_h > 0.0 { gate_h } else { (z - depth - 1.0).max(2.0) };
        let (first, last) = (under(row_a[0].0, row_a[0].1), under(row_a[row_a.len() - 1].0, row_a[row_a.len() - 1].1));
        // header between consecutive columns, leaves across the whole row, just outside the line
        for w in row_a.windows(2) {
            let (a, b) = (under(w[0].0, w[0].1), under(w[1].0, w[1].1));
            em.member("gate_header", &header, lw(v(a.x, a.y, gh)), lw(v(b.x, b.y, gh)), &steel)?;
        }
        let out = if along_x { v(0.0, -0.6, 0.0) } else { v(-0.6, 0.0, 0.0) };
        for k in 0..gates {
            let (t0, t1) = (k as f64 / gates as f64, (k + 1) as f64 / gates as f64);
            let (pa, pb) = (lerp(first, last, t0).add(out), lerp(first, last, t1).add(out));
            let q = [lw(pa), lw(pb), lw(v(pb.x, pb.y, gh)), lw(v(pa.x, pa.y, gh))];
            em.panel("gate", &q, 0.12, &steel)?;
        }
    }
    let (g, info) = em.finish(&name)?;
    let mut all = groups.clone();
    for (i, g) in all.iter_mut().enumerate() {
        g.fixed = user[i].is_some();
    }
    let ratio = short / depth;
    let mut extra = vec![format!(
        "loads: roof dead {} kN/m2 [dead_roof], imposed {} [live_roof], suction {} [uplift]; deflection limit span/{} [defl_roof]",
        r.get("dead_roof"),
        r.get("live_roof"),
        r.get("uplift"),
        r.get("defl_roof")
    )];
    if split {
        extra.push(format!("runway: {} kN at mid-span of each beam [crane]; {}", r.get("crane"), sc::why_of("crane")));
    }
    if col_at.is_empty() {
        extra.push("check skipped: columns = none leaves no supports in this block".into());
    }
    let typ = Typical { lo: 12.0, hi: 25.0, what: "a double-layer space frame (short span/depth)" };
    report(lua, &info, "space_frame", &des, &all, &r, "defl_roof", ratio, typ, false, &extra)?;
    Ok((g, info))
}
