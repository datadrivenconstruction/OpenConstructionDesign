// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Structural blocks: towers, arches and vaults, helical stairs and ramps, facade fins.
//! Bridges, stadium roofs and space frames are in `span_blocks`. Every node is computed once
//! and shared by the members meeting there.

use std::f64::consts::PI;

use ddc_ops::Profile;
use mlua::{Lua, Table};

use crate::blocks::{block, lerp, ngon, turn, v, Curve, Em, Opts};
use crate::values::{err, GroupH, LResult, V3, UP};

type Out = LResult<(GroupH, Table)>;

pub(crate) fn install(lua: &Lua, g: &Table) -> LResult<()> {
    g.set("tower", block(lua, tower)?)?;
    g.set("arch", block(lua, arch)?)?;
    g.set("vault", block(lua, vault)?)?;
    g.set("helix_stair", block(lua, helix_stair)?)?;
    g.set("helix_ramp", block(lua, helix_ramp)?)?;
    g.set("fins", block(lua, fins)?)?;
    crate::span_blocks::install(lua, g)?;
    g.set("pagoda", block(lua, pagoda)?)?;
    Ok(())
}

/// Floors with slabs and ring beams around a core; perimeter columns or a diagrid; twist and
/// taper over the height.
fn tower(lua: &Lua, t: Option<Table>) -> Out {
    let o = Opts::new("tower", t, lua)?;
    let c = o.point("center", v(0.0, 0.0, 0.0))?;
    let floors = o.int("floors", 40, 1, 200)? as usize;
    let fh = o.len_in("floor_h", 4.0, 2.5, 12.0)?;
    let r0 = o.len_in("radius", 18.0, 3.0, 150.0)?;
    let sides = o.int("sides", 16, 3, 96)? as usize;
    let twist = o.ang_in("twist", 0.0, -720.0, 720.0)?;
    let taper = o.num("taper", 1.0, 0.2, 3.0)?;
    let system = o.choice("system", "diagrid", &["diagrid", "columns"])?;
    let module = o.int("module", 2, 1, 20)? as usize;
    let slab_t = o.len_in("slab_t", 0.25, 0.1, 1.0)?;
    let core_r = o.len("core_r", r0 * 0.3)?;
    let core_t = o.len_in("core_t", 0.4, 0.15, 2.0)?;
    let diag = o.prof("diagonal", "CHS 508x25")?;
    let col = o.prof("column", "CHS 610x30")?;
    let beam = o.prof("beam", "I 500x200")?;
    let steel = o.mat("material", "S355")?;
    let conc = o.mat("slab_material", "C30/37")?;
    let core_m = o.mat("core_material", "C40/50")?;
    let name = o.text("name", "tower")?;
    o.done()?;
    let diagrid = system == "diagrid";
    if diagrid && floors % module != 0 {
        return err(format!(
            "tower: floors ({floors}) must be a multiple of module ({module}) for a diagrid"
        ));
    }
    let h = floors as f64 * fh;
    let rad = |z: f64| r0 * (1.0 + (taper - 1.0) * z / h);
    let rmin = r0.min(rad(h)) * (PI / sides as f64).cos();
    if core_r != 0.0 && !(core_r > core_t + 0.5 && core_r < 0.8 * rmin) {
        return err(format!(
            "tower: core_r {core_r:.2} m must be 0 (no core) or between core_t + 0.5 m and \
             0.8 of the smallest floor radius ({:.2} m)",
            0.8 * rmin
        ));
    }
    let at = |z: f64, a: f64| {
        let (r, th) = (rad(z), twist * z / h + a);
        v(c.x + r * th.cos(), c.y + r * th.sin(), c.z + z)
    };
    let step = 2.0 * PI / sides as f64;
    let mut em = Em::new(lua, "tower");
    let core_hole =
        |z: f64| if core_r > 0.0 { vec![ngon(v(c.x, c.y, z), core_r, 48, 0.0)] } else { vec![] };
    // one outline per floor, its points lying on the members below it
    let mut outlines: Vec<Vec<V3>> = vec![];
    if diagrid {
        let rows = floors / module;
        let node = |i: usize, j: i64| {
            let off = if i % 2 == 1 { 0.5 } else { 0.0 };
            at(i as f64 * module as f64 * fh, (j as f64 + off) * step)
        };
        for i in 0..rows {
            for j in 0..sides as i64 {
                let (l, r) = if i % 2 == 0 { (j - 1, j) } else { (j, j + 1) };
                em.member("diagonal", &diag, node(i, j), node(i + 1, l), &steel)?;
                em.member("diagonal", &diag, node(i, j), node(i + 1, r), &steel)?;
            }
        }
        for k in 1..=floors {
            let (i, rem) = (k / module, k % module);
            let ring: Vec<V3> = if rem == 0 {
                (0..sides as i64).map(|j| node(i, j)).collect()
            } else {
                let f = rem as f64 / module as f64;
                let mut pts = vec![];
                for j in 0..sides as i64 {
                    let (l, r) = if i % 2 == 0 { (j - 1, j) } else { (j, j + 1) };
                    pts.push(lerp(node(i, j), node(i + 1, l), f));
                    pts.push(lerp(node(i, j), node(i + 1, r), f));
                }
                pts
            };
            outlines.push(ring);
        }
    } else {
        for k in 0..floors {
            for j in 0..sides {
                let a = j as f64 * step;
                em.member("column", &col, at(k as f64 * fh, a), at((k + 1) as f64 * fh, a), &steel)?;
            }
        }
        for k in 1..=floors {
            outlines.push((0..sides).map(|j| at(k as f64 * fh, j as f64 * step)).collect());
        }
    }
    for (k, ring) in outlines.iter().enumerate() {
        let z = c.z + (k + 1) as f64 * fh;
        for j in 0..ring.len() {
            em.member("ring_beam", &beam, ring[j], ring[(j + 1) % ring.len()], &steel)?;
        }
        em.slab("slab", ring, &core_hole(z), z, slab_t, &conc)?;
    }
    if core_r > 0.0 {
        let ring = Profile::Ring { r_out: ddc_ops::Len::m(core_r), r_in: ddc_ops::Len::m(core_r - core_t) };
        for k in 0..floors {
            let (a, b) = (c.add(UP.scale(k as f64 * fh)), c.add(UP.scale((k + 1) as f64 * fh)));
            em.member("core", &ring, a, b, &core_m)?;
        }
    }
    em.finish(&name)
}

/// One faceted arch from `from` to `to`.
fn arch(lua: &Lua, t: Option<Table>) -> Out {
    let o = Opts::new("arch", t, lua)?;
    let a = o.point("from", v(0.0, 0.0, 0.0))?;
    let b = o.point("to", v(20.0, 0.0, 0.0))?;
    let span = v(b.x - a.x, b.y - a.y, 0.0).len();
    let kind = o.text("kind", "catenary")?;
    let rise = o.len("rise", span * 0.4)?;
    let segs = o.int("segments", 16, 2, 400)? as usize;
    let prof = o.prof("profile", "CHS 323.9x12")?;
    let role = o.text("role", "rib")?;
    let mat = o.mat("material", "S355")?;
    let name = o.text("name", "arch")?;
    o.done()?;
    let cv = Curve::new("arch", &kind, span, rise)?;
    let segs = if kind == "pointed" { segs + segs % 2 } else { segs };
    let pts = cv.points(a, b, segs);
    let mut em = Em::new(lua, "arch");
    for w in pts.windows(2) {
        em.member(&role, &prof, w[0], w[1], &mat)?;
    }
    em.finish(&name)
}

/// A vault: arches along its length, purlins between them, optional panels.
fn vault(lua: &Lua, t: Option<Table>) -> Out {
    let o = Opts::new("vault", t, lua)?;
    let org = o.point("origin", v(0.0, 0.0, 0.0))?;
    let span = o.len_in("span", 24.0, 2.0, 300.0)?;
    let length = o.len_in("length", 48.0, 1.0, 1000.0)?;
    let kind = o.text("kind", "catenary")?;
    let rise = o.len("rise", if kind == "circular" || kind == "barrel" { span / 2.0 } else { span * 0.6 })?;
    let arches = o.int("arches", ((length / 6.0).ceil() as i64 + 1).max(2), 2, 400)? as usize;
    let segs = o.int("segments", 16, 4, 200)? as usize;
    let angle = o.ang("angle", 0.0)?;
    let rib = o.prof("rib", "CHS 273x10")?;
    let purlin = o.prof("purlin", "CHS 139.7x6")?;
    let panels = o.len_in("panels", 0.0, 0.0, 0.3)?;
    let mat = o.mat("material", "S355")?;
    let pmat = o.mat("panel_material", "glass")?;
    let name = o.text("name", "vault")?;
    o.done()?;
    let cv = Curve::new("vault", &kind, span, rise)?;
    let segs = if kind == "pointed" { segs + segs % 2 } else { segs };
    let rows: Vec<Vec<V3>> = (0..arches)
        .map(|i| {
            let y = length * i as f64 / (arches - 1) as f64;
            let a = turn(v(org.x, org.y + y, org.z), org, angle);
            let b = turn(v(org.x + span, org.y + y, org.z), org, angle);
            cv.points(a, b, segs)
        })
        .collect();
    let mut em = Em::new(lua, "vault");
    for r in &rows {
        for w in r.windows(2) {
            em.member("rib", &rib, w[0], w[1], &mat)?;
        }
    }
    for i in 0..arches - 1 {
        for k in 0..=segs {
            let role = if k == 0 || k == segs { "sill" } else { "purlin" };
            em.member(role, &purlin, rows[i][k], rows[i + 1][k], &mat)?;
            if panels > 0.0 && k < segs {
                let q = [rows[i][k], rows[i][k + 1], rows[i + 1][k + 1], rows[i + 1][k]];
                em.panel("panel", &q, panels, &pmat)?;
            }
        }
    }
    em.finish(&name)
}

/// A helical stair around a central column: treads, stringer, balusters, handrail.
fn helix_stair(lua: &Lua, t: Option<Table>) -> Out {
    let o = Opts::new("helix_stair", t, lua)?;
    let c = o.point("center", v(0.0, 0.0, 0.0))?;
    let height = o.len_in("height", 4.0, 0.5, 200.0)?;
    let r_out = o.len_in("r_out", 1.2, 0.6, 10.0)?;
    let colp = o.prof("column", "CHS 219.1x10")?;
    let ce = crate::audit::extents(&colp);
    let r_col = (ce[1] - ce[0]) / 2.0;
    let walk = r_col + 0.6 * (r_out - r_col);
    let going = o.len_in("going", 0.26, 0.22, 0.40)?;
    let riser_max = o.len_in("riser_max", 0.19, 0.12, 0.20)?;
    let clockwise = o.flag("clockwise", false)?;
    let tread_t = o.len_in("tread_t", 0.05, 0.02, 0.2)?;
    let mat = o.mat("material", "S355")?;
    let tread_m = o.mat("tread_material", "oak")?;
    let start = o.ang("start", 0.0)?;
    let name = o.text("name", "stair")?;
    o.done()?;
    if r_out - r_col < 0.55 {
        return err(format!("helix_stair: r_out must leave a 0.55 m flight beside the column ({:.2} m)", r_out - r_col));
    }
    let steps = (height / riser_max).ceil().max(2.0) as usize;
    let riser = height / steps as f64;
    let dth = going / walk * if clockwise { -1.0 } else { 1.0 };
    let per_turn = 2.0 * PI / dth.abs();
    if riser * per_turn < 2.05 {
        return err(format!(
            "helix_stair: headroom {:.2} m under the turn above is below 2.05 m; use a larger \
             going or r_out",
            riser * per_turn
        ));
    }
    let r_s = r_out - 0.05;
    let pol = |r: f64, a: f64, z: f64| v(c.x + r * a.cos(), c.y + r * a.sin(), c.z + z);
    let ang = |k: f64| start + k * dth;
    let stringer = Profile::Rect { w: ddc_ops::Len::m(0.02), h: ddc_ops::Len::m(0.2) };
    let post = crate::blocks::prof_text("CHS 42.4x3.2").map_err(mlua::Error::RuntimeError)?;
    let mut em = Em::new(lua, "helix_stair");
    em.member("column", &colp, c, c.add(UP.scale(height + 1.0)), &mat)?;
    let snode = |k: usize| pol(r_s, ang(k as f64 + 0.5), k as f64 * riser - tread_t - 0.1);
    let hnode = |k: usize| pol(r_s, ang(k as f64 + 0.5), k as f64 * riser + 0.9);
    for k in 1..=steps {
        let z = k as f64 * riser - tread_t / 2.0;
        let (a0, a1) = (ang(k as f64), ang(k as f64 + 1.0));
        let am = 0.5 * (a0 + a1);
        let q = [pol(r_col, a0, z), pol(r_out, a0, z), pol(r_out, am, z), pol(r_out, a1, z), pol(r_col, a1, z)];
        em.panel("tread", &q, tread_t, &tread_m)?;
        em.member("baluster", &post, snode(k), hnode(k), &mat)?;
        if k > 1 {
            em.member("stringer", &stringer, snode(k - 1), snode(k), &mat)?;
            em.member("handrail", &post, hnode(k - 1), hnode(k), &mat)?;
        }
    }
    let s1 = snode(1);
    em.member("post", &post, v(s1.x, s1.y, c.z), s1, &mat)?;
    em.finish(&name)
}

/// A helical ramp: edge beams, cross beams, deck panels, posts and an outer railing.
fn helix_ramp(lua: &Lua, t: Option<Table>) -> Out {
    let o = Opts::new("helix_ramp", t, lua)?;
    let c = o.point("center", v(0.0, 0.0, 0.0))?;
    let z0 = o.len("z0", c.z)?;
    let height = o.len_in("height", 12.0, 0.5, 300.0)?;
    let r_in = o.len_in("r_in", 6.0, 1.0, 100.0)?;
    let width = o.len_in("width", 2.0, 1.2, 10.0)?;
    let slope_max = o.num("slope_max", 1.0 / 12.0, 0.01, 0.2)?;
    let per_turn = o.int("segments", 24, 8, 128)? as usize;
    let col_every = o.int("column_every", 4, 1, 64)? as usize;
    let edge = o.prof("edge", "RHS 300x150x10")?;
    let cross = o.prof("cross", "I 200x100")?;
    let colp = o.prof("column", "CHS 219.1x8")?;
    let deck_t = o.len_in("deck", 0.12, 0.05, 0.5)?;
    let rail = o.flag("railing", true)?;
    let mat = o.mat("material", "S355")?;
    let deck_m = o.mat("deck_material", "C30/37")?;
    let start = o.ang("start", 0.0)?;
    let name = o.text("name", "ramp")?;
    o.done()?;
    let r_out = r_in + width;
    let walk = r_in + 0.3;
    let turns_min = height / (slope_max * 2.0 * PI * walk);
    // whole segments per turn, so a column stands on the node one turn below
    let n = ((turns_min.max(0.25) * per_turn as f64).ceil() as usize).max(2);
    let dth = 2.0 * PI / per_turn as f64;
    let dz = height / n as f64;
    let per_turn_rise = dz * per_turn as f64;
    if n > per_turn && per_turn_rise < 2.3 {
        return err(format!(
            "helix_ramp: one turn rises {per_turn_rise:.2} m, below 2.3 m of headroom; use a \
             larger r_in or a steeper slope_max"
        ));
    }
    let pol = |r: f64, k: usize, up: f64| {
        let a = start + k as f64 * dth;
        v(c.x + r * a.cos(), c.y + r * a.sin(), z0 + k as f64 * dz + up)
    };
    let post = crate::blocks::prof_text("CHS 48.3x4").map_err(mlua::Error::RuntimeError)?;
    let mut em = Em::new(lua, "helix_ramp");
    for k in 0..=n {
        em.member("cross_beam", &cross, pol(r_in, k, 0.0), pol(r_out, k, 0.0), &mat)?;
        if k < n {
            em.member("edge_beam", &edge, pol(r_in, k, 0.0), pol(r_in, k + 1, 0.0), &mat)?;
            em.member("edge_beam", &edge, pol(r_out, k, 0.0), pol(r_out, k + 1, 0.0), &mat)?;
            let q = [pol(r_in, k, 0.0), pol(r_out, k, 0.0), pol(r_out, k + 1, 0.0), pol(r_in, k + 1, 0.0)];
            em.panel("deck", &q, deck_t, &deck_m)?;
        }
        if k % col_every == 0 && k > 0 {
            for r in [r_in, r_out] {
                let top = pol(r, k, 0.0);
                let foot = if k >= per_turn { pol(r, k - per_turn, 0.0) } else { v(top.x, top.y, c.z) };
                em.member("column", &colp, foot, top, &mat)?;
            }
        }
        // a column rising from this node stands in for the baluster
        let col_up = k + per_turn <= n && (k + per_turn) % col_every == 0;
        if rail {
            let (b, h) = (pol(r_out, k, 0.0), pol(r_out, k, 1.1));
            if !col_up {
                em.member("baluster", &post, b, h, &mat)?;
            }
            if k > 0 {
                em.member("handrail", &post, pol(r_out, k - 1, 1.1), h, &mat)?;
            }
        }
    }
    em.finish(&name)
}

/// Facade fins or louvres along a line, turned by an attractor point or the sun.
fn fins(lua: &Lua, t: Option<Table>) -> Out {
    let o = Opts::new("fins", t, lua)?;
    let a = o.point("from", v(0.0, 0.0, 0.0))?;
    let b = o.point("to", v(30.0, 0.0, 0.0))?;
    let height = o.len_in("height", 12.0, 0.5, 400.0)?;
    let dir = o.choice("direction", "vertical", &["vertical", "horizontal"])?;
    let spacing = o.len_in("spacing", 0.6, 0.1, 10.0)?;
    let depth = o.len_in("depth", 0.4, 0.05, 3.0)?;
    // depth swings between depth_min and depth, `waves` periods along the run (sine)
    let depth_min = o.len_in("depth_min", depth, 0.05, 3.0)?;
    let waves = o.num("waves", 1.0, 0.0, 200.0)?;
    let thick = o.len_in("thickness", 0.05, 0.01, 0.5)?;
    let mode = o.choice("mode", "attractor", &["attractor", "sun", "fixed"])?;
    let attractor = o.opt_point("attractor")?;
    let falloff = o.len("falloff", 0.0)?;
    let sun = o.table("sun")?;
    let rot_min = o.ang_in("min_rotation", 0.0, -90.0, 90.0)?;
    let rot_max = o.ang_in("max_rotation", PI / 3.0, -90.0, 90.0)?;
    let frame = o.prof("frame", "RHS 200x100x8")?;
    let mat = o.mat("material", "aluminium")?;
    let fmat = o.mat("frame_material", "S355")?;
    let name = o.text("name", "fins")?;
    o.done()?;
    let along = b.sub(a);
    let l = v(along.x, along.y, 0.0).len();
    let Some(u) = v(along.x, along.y, 0.0).unit() else {
        return err("fins: from and to must differ in plan");
    };
    // outward normal: to the right of from -> to
    let nrm = v(u.y, -u.x, 0.0);
    let (sun_az, sun_alt) = match &sun {
        Some(s) => {
            let so = Opts::new("fins.sun", Some(s.clone()), lua)?;
            let az = so.ang("azimuth", PI)?;
            let alt = so.ang_in("altitude", PI / 4.0, 0.0, 90.0)?;
            so.done()?;
            (az, alt)
        }
        None => (PI, PI / 4.0),
    };
    if mode == "attractor" && attractor.is_none() {
        return err("fins: mode attractor needs attractor = vec3(...)");
    }
    let fall = if falloff > 0.0 { falloff } else { l.max(height) / 2.0 };
    let span_of = if dir == "vertical" { l } else { height };
    let count = (span_of / spacing).floor() as usize;
    if count < 2 {
        return err("fins: fewer than two fins fit; lower the spacing");
    }
    let rot_at = |p: V3| -> f64 {
        let w = match mode.as_str() {
            "attractor" => {
                let d = p.sub(attractor.unwrap_or(p)).len();
                (1.0 - d / fall).clamp(0.0, 1.0)
            }
            "sun" => {
                let rel = if dir == "vertical" {
                    // facade normal azimuth against the sun's (x east, y north, azimuth from north)
                    let n_az = nrm.x.atan2(nrm.y);
                    (sun_az - n_az).sin()
                } else {
                    sun_alt.sin()
                };
                rel.abs().clamp(0.0, 1.0)
            }
            _ => 0.0,
        };
        rot_min + (rot_max - rot_min) * w
    };
    if depth_min > depth {
        return err("fins: depth_min must not exceed depth");
    }
    let depth_at = |k: usize, n: usize| {
        let t = k as f64 / n as f64;
        depth_min + (depth - depth_min) * 0.5 * (1.0 - (2.0 * PI * waves * t).cos())
    };
    let fin_at = |d: f64| ddc_ops::Profile::Rect { w: ddc_ops::Len::m(thick), h: ddc_ops::Len::m(d) };
    // the profile is centred on the axis: move the axis out by half the projected depth so
    // the fin grows from the facade line outward, not both ways
    let out = |p: V3, d: f64, rot: f64| p.add(nrm.scale(d / 2.0 * rot.cos()));
    let mut em = Em::new(lua, "fins");
    let base = |s: f64| a.add(u.scale(s));
    let up = |p: V3, z: f64| v(p.x, p.y, a.z + z);
    if dir == "vertical" {
        let step = l / count as f64;
        for k in 0..=count {
            let p = base(k as f64 * step);
            let mid = up(p, height / 2.0);
            // the roll that points the fin's depth along the facade normal
            let face = (-nrm.x).atan2(nrm.y);
            let (d, rot) = (depth_at(k, count), rot_at(mid));
            let (f0, f1) = (out(up(p, 0.0), d, rot), out(up(p, height), d, rot));
            em.member_roll("fin", &fin_at(d), f0, f1, &mat, face + rot)?;
        }
        em.member("rail", &frame, up(a, 0.0), up(base(l), 0.0), &fmat)?;
        em.member("rail", &frame, up(a, height), up(base(l), height), &fmat)?;
    } else {
        let step = height / count as f64;
        em.member("post", &frame, up(a, 0.0), up(a, height), &fmat)?;
        em.member("post", &frame, up(base(l), 0.0), up(base(l), height), &fmat)?;
        for k in 1..count {
            let z = k as f64 * step;
            let mid = up(base(l / 2.0), z);
            let (d, rot) = (depth_at(k, count), rot_at(mid));
            // blade ends stay on the posts: only the vertical fins move out
            let (f0, f1) = (up(a, z), up(base(l), z));
            em.member_roll("louvre", &fin_at(d), f0, f1, &mat, PI / 2.0 - rot)?;
        }
    }
    em.finish(&name)
}

/// A pagoda: tiers of shrinking square plan, eight columns and a ring beam on each, a hip roof
/// per tier with its eaves curled up at the corners, rafters, and a mast finial on the top.
fn pagoda(lua: &Lua, t: Option<Table>) -> Out {
    let o = Opts::new("pagoda", t, lua)?;
    let org = o.point("origin", v(0.0, 0.0, 0.0))?;
    let tiers = o.int("tiers", 5, 1, 15)? as usize;
    let base = o.len_in("base_width", 10.0, 3.0, 60.0)?;
    let th = o.len_in("tier_height", 4.0, 2.5, 12.0)?;
    let taper = o.num("taper", 0.8, 0.55, 0.95)?;
    let over = o.len_in("overhang", 1.5, 0.3, 6.0)?;
    let curl = o.len_in("eave_curl", 0.6, 0.0, 2.5)?;
    let slope = o.num("roof_slope", 0.5, 0.15, 1.2)?;
    let roof_t = o.len_in("roof_t", 0.1, 0.02, 0.4)?;
    let floor_t = o.len_in("floor", 0.2, 0.0, 0.6)?;
    let finial = o.len_in("finial", 3.0, 0.5, 12.0)?;
    let colp = o.prof("column", "CHS 323.9x10")?;
    let beam = o.prof("beam", "RHS 250x150x8")?;
    let raf = o.prof("rafter", "RHS 250x150x8")?;
    let mastp = o.prof("mast", "CHS 88.9x5")?;
    let mat = o.mat("material", "S355")?;
    let rmat = o.mat("roof_material", "Clay tile")?;
    let name = o.text("name", "pagoda")?;
    o.done()?;
    let top_w = base * taper.powi(tiers as i32 - 1);
    if top_w < 1.5 {
        return err("pagoda: tiers and taper leave the top tier narrower than 1.5 m; raise base_width or taper, or lower tiers");
    }
    let corners = [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)];
    // point `p` (0..4) around a square of half-width `h` at height `z`
    let at = |h: f64, z: f64, p: f64| {
        let k = (p.floor() as usize).min(3);
        let s = p - k as f64;
        let (a, b) = (corners[k], corners[(k + 1) % 4]);
        v(org.x + h * (a.0 + (b.0 - a.0) * s), org.y + h * (a.1 + (b.1 - a.1) * s), z)
    };
    // the eight column nodes: corners and side middles
    let nodes8 = |h: f64, z: f64| -> Vec<V3> { (0..8).map(|j| at(h, z, j as f64 / 2.0)).collect() };
    let seg = 4usize;
    let rings = 2usize;
    let mut em = Em::new(lua, "pagoda");
    let (mut z0, mut half) = (org.z, base / 2.0);
    for i in 0..tiers {
        let last = i + 1 == tiers;
        let half_in = if last { 0.0 } else { half * taper };
        let z_top = z0 + th;
        let z_in = z_top + slope * (half - half_in);
        let (bot, top) = (nodes8(half, z0), nodes8(half, z_top));
        for j in 0..8 {
            em.member("column", &colp, bot[j], top[j], &mat)?;
            em.member("ring_beam", &beam, top[j], top[(j + 1) % 8], &mat)?;
        }
        let inner = if last { vec![v(org.x, org.y, z_in); 8] } else { nodes8(half_in, z_in) };
        if !last {
            for j in 0..8 {
                em.member("ring_beam", &beam, inner[j], inner[(j + 1) % 8], &mat)?;
            }
        }
        let half_e = half + over;
        let z_e = z_top - slope * over;
        for k in 0..4 {
            let tip = at(half_e, z_e + curl, k as f64);
            em.member("rafter", &raf, tip, top[2 * k], &mat)?;
            em.member("rafter", &raf, top[2 * k], inner[2 * k], &mat)?;
        }
        if floor_t > 0.0 {
            let pts: Vec<V3> = (0..4).map(|k| at(half, z0, k as f64)).collect();
            em.slab("deck", &pts, &[], z0 + floor_t, floor_t, &mat)?;
        }
        // the roof: a grid from the curled eave to the inner ring
        let eave = |p: f64| {
            let s = p - p.floor();
            let c = curl * (2.0 * s - 1.0).powi(2);
            at(half_e, z_e + c, p)
        };
        let ridge = |p: f64| at(half_in, z_in, p);
        let grid = |a: usize, b: usize| {
            let p = a as f64 * 4.0 / (4 * seg) as f64;
            lerp(eave(p), ridge(p), b as f64 / rings as f64)
        };
        for a in 0..4 * seg {
            for b in 0..rings {
                let q = [grid(a, b), grid(a + 1, b), grid(a + 1, b + 1), grid(a, b + 1)];
                em.panel("shell", &q, roof_t, &rmat)?;
            }
        }
        if last {
            em.member("mast", &mastp, v(org.x, org.y, z_in), v(org.x, org.y, z_in + finial), &mat)?;
        }
        z0 = z_in;
        half = half_in;
    }
    em.finish(&name)
}
