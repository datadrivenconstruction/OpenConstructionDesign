// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Freeform blocks: parametric surfaces (`surf.*`), gridshells over any surface, hypar
//! umbrellas, form-found membranes and perforated screens.

use std::f64::consts::PI;

use ddc_ops::{GeoOp, LatticeHost, LatticeKind, LatticePattern, Len, LenUnit, Profile, P2, P3};
use mlua::{Lua, MetaMethod, Table, UserData, UserDataMethods, Value};

use crate::blocks::{block, lerp, turn, v, v3_of, Curve, Em, Opts};
use crate::values::{err, FormV, GroupH, LResult, Shape, V3, UP};

type Out = LResult<(GroupH, Table)>;

/// A surface over u, v in 0..1.
#[derive(Clone)]
pub(crate) enum Surf {
    Bilinear([V3; 4]),
    /// Spherical cap on a base circle of radius `r`; `oculus` is the radius of the top opening.
    Dome { c: V3, r: f64, rise: f64, oculus: f64 },
    Vault { org: V3, span: f64, length: f64, angle: f64, cv: Curve },
    Loft(Vec<Vec<V3>>),
    Revolve { c: V3, prof: Vec<V3>, a0: f64, a1: f64 },
    Wave { org: V3, sx: f64, sy: f64, h: f64, amp: f64, wx: f64, wy: f64, angle: f64 },
    /// Points of a function sampled once, read back bilinearly.
    Grid(Vec<Vec<V3>>),
}

/// Point at `t` (0..1) of a polyline, by arc length.
fn along(p: &[V3], t: f64) -> V3 {
    if p.len() == 1 {
        return p[0];
    }
    let total: f64 = p.windows(2).map(|w| w[1].sub(w[0]).len()).sum();
    let mut left = t.clamp(0.0, 1.0) * total;
    for w in p.windows(2) {
        let l = w[1].sub(w[0]).len();
        if left <= l && l > 0.0 {
            return lerp(w[0], w[1], left / l);
        }
        left -= l;
    }
    p[p.len() - 1]
}

impl Surf {
    pub fn at(&self, u: f64, w: f64) -> V3 {
        match self {
            Surf::Bilinear(c) => lerp(lerp(c[0], c[1], u), lerp(c[3], c[2], u), w),
            Surf::Dome { c, r, rise, oculus } => {
                let big = (r * r + rise * rise) / (2.0 * rise);
                let zc = c.z + rise - big;
                let base = ((big - rise) / big).clamp(-1.0, 1.0).acos();
                let top = (oculus / big).clamp(0.0, 1.0).asin();
                let phi = base + (top - base) * w;
                let th = 2.0 * PI * u;
                v(c.x + big * phi.sin() * th.cos(), c.y + big * phi.sin() * th.sin(), zc + big * phi.cos())
            }
            Surf::Vault { org, span, length, angle, cv } => {
                let p = v(org.x + span * u, org.y + length * w, org.z + cv.y(span * u));
                turn(p, *org, *angle)
            }
            Surf::Loft(cs) => {
                let s = w.clamp(0.0, 1.0) * (cs.len() - 1) as f64;
                let i = (s.floor() as usize).min(cs.len() - 2);
                lerp(along(&cs[i], u), along(&cs[i + 1], u), s - i as f64)
            }
            Surf::Revolve { c, prof, a0, a1 } => {
                let p = along(prof, w);
                let a = a0 + (a1 - a0) * u;
                v(c.x + p.x * a.cos(), c.y + p.x * a.sin(), c.z + p.z)
            }
            Surf::Grid(g) => {
                let (nu, nv) = (g.len() - 1, g[0].len() - 1);
                let (su, sv) = (u.clamp(0.0, 1.0) * nu as f64, w.clamp(0.0, 1.0) * nv as f64);
                let (i, j) = ((su.floor() as usize).min(nu - 1), (sv.floor() as usize).min(nv - 1));
                let (fu, fv) = (su - i as f64, sv - j as f64);
                lerp(lerp(g[i][j], g[i + 1][j], fu), lerp(g[i][j + 1], g[i + 1][j + 1], fu), fv)
            }
            Surf::Wave { org, sx, sy, h, amp, wx, wy, angle } => {
                let z = h + amp * (2.0 * PI * wx * u).sin() * (2.0 * PI * wy * w).cos();
                turn(v(org.x + sx * u, org.y + sy * w, org.z + z), *org, *angle)
            }
        }
    }
}

impl UserData for Surf {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("at", |_, s, (u, w): (f64, f64)| Ok(s.at(u, w)));
        m.add_meta_method(MetaMethod::Call, |_, s, (u, w): (f64, f64)| Ok(s.at(u, w)));
        m.add_meta_method(MetaMethod::ToString, |_, _, ()| Ok("surface".to_string()));
        crate::geo_curve::surf_methods(m);
    }
}

pub(crate) fn install(lua: &Lua, g: &Table) -> LResult<()> {
    let s = lua.create_table()?;
    s.set("hypar", lua.create_function(|lua, t: Option<Table>| s_bilinear("surf.hypar", lua, t))?)?;
    s.set("bilinear", lua.create_function(|lua, t: Option<Table>| s_bilinear("surf.bilinear", lua, t))?)?;
    s.set("dome", lua.create_function(s_dome)?)?;
    s.set("vault", lua.create_function(s_vault)?)?;
    s.set("loft", lua.create_function(s_loft)?)?;
    s.set("ruled", lua.create_function(s_ruled)?)?;
    s.set("revolve", lua.create_function(s_revolve)?)?;
    s.set("wave", lua.create_function(s_wave)?)?;
    s.set("sample", lua.create_function(s_sample)?)?;
    g.set("surf", s)?;
    g.set("surface_grid", block(lua, surface_grid)?)?;
    g.set("geodesic", block(lua, geodesic)?)?;
    g.set("hypar_umbrella", block(lua, hypar_umbrella)?)?;
    g.set("membrane", block(lua, membrane)?)?;
    g.set("perforated_panel", block(lua, perforated_panel)?)?;
    Ok(())
}

// ---- surfaces ----

fn s_bilinear(f: &'static str, lua: &Lua, t: Option<Table>) -> LResult<Surf> {
    let o = Opts::new(f, t, lua)?;
    let c = o.points("corners")?;
    o.done()?;
    match c {
        Some(c) if c.len() == 4 => Ok(Surf::Bilinear([c[0], c[1], c[2], c[3]])),
        _ => err(format!("{f}: corners = {{p1, p2, p3, p4}} (four vec3, in order around)")),
    }
}

fn s_dome(lua: &Lua, t: Option<Table>) -> LResult<Surf> {
    let o = Opts::new("surf.dome", t, lua)?;
    let c = o.point("center", v(0.0, 0.0, 0.0))?;
    let r = o.len_in("radius", 15.0, 0.5, 500.0)?;
    let rise = o.len_in("rise", r, 0.2, 1000.0)?;
    let oculus = o.len_in("oculus", 0.0, 0.0, 500.0)?;
    o.done()?;
    let big = (r * r + rise * rise) / (2.0 * rise);
    if oculus >= big.min(if rise < big { r } else { big }) * 0.95 {
        return err("surf.dome: the oculus must be smaller than the dome");
    }
    Ok(Surf::Dome { c, r, rise, oculus })
}

/// A surface from any function (u, v) -> vec3, sampled on an nu x nv grid.
fn s_sample(lua: &Lua, (func, nu, nv): (mlua::Function, i64, i64)) -> LResult<Surf> {
    if !(1..=400).contains(&nu) || !(1..=400).contains(&nv) {
        return err("surf.sample(function(u, v) return vec3(...) end, nu, nv): nu and nv are 1..400, e.g. surf.sample(f, 24, 24)");
    }
    let mut g = Vec::with_capacity(nu as usize + 1);
    for i in 0..=nu {
        let mut row = Vec::with_capacity(nv as usize + 1);
        for j in 0..=nv {
            row.push(v3_of(lua, func.call((i as f64 / nu as f64, j as f64 / nv as f64))?, "surf.sample")?);
        }
        g.push(row);
    }
    Ok(Surf::Grid(g))
}

fn s_vault(lua: &Lua, t: Option<Table>) -> LResult<Surf> {
    let o = Opts::new("surf.vault", t, lua)?;
    let org = o.point("origin", v(0.0, 0.0, 0.0))?;
    let span = o.len_in("span", 20.0, 1.0, 500.0)?;
    let length = o.len_in("length", 40.0, 1.0, 2000.0)?;
    let kind = o.text("kind", "catenary")?;
    let rise = o.len("rise", span * 0.5)?;
    let angle = o.ang("angle", 0.0)?;
    o.done()?;
    let cv = Curve::new("surf.vault", &kind, span, rise)?;
    Ok(Surf::Vault { org, span, length, angle, cv })
}

fn curves(lua: &Lua, o: &Opts, k: &'static str) -> LResult<Vec<Vec<V3>>> {
    let Some(t) = o.table(k)? else {
        return err(format!("{}: {k} = {{{{p, p, ...}}, {{p, p, ...}}}} (two or more polylines)", o.f));
    };
    let mut out = vec![];
    for c in t.sequence_values::<Table>() {
        let mut pts = vec![];
        for p in c?.sequence_values::<Value>() {
            pts.push(v3_of(lua, p?, o.f)?);
        }
        if pts.len() < 2 {
            return err(format!("{}: every curve in {k} needs two or more points", o.f));
        }
        out.push(pts);
    }
    if out.len() < 2 {
        return err(format!("{}: {k} needs two or more curves", o.f));
    }
    Ok(out)
}

fn s_loft(lua: &Lua, t: Option<Table>) -> LResult<Surf> {
    let o = Opts::new("surf.loft", t, lua)?;
    let cs = curves(lua, &o, "curves")?;
    o.done()?;
    Ok(Surf::Loft(cs))
}

fn s_ruled(lua: &Lua, t: Option<Table>) -> LResult<Surf> {
    let o = Opts::new("surf.ruled", t, lua)?;
    let a = o.points("a")?;
    let b = o.points("b")?;
    o.done()?;
    match (a, b) {
        (Some(a), Some(b)) if a.len() >= 2 && b.len() >= 2 => Ok(Surf::Loft(vec![a, b])),
        _ => err("surf.ruled: a = {p, p, ...}, b = {p, p, ...} (two polylines, straight lines between)"),
    }
}

fn s_revolve(lua: &Lua, t: Option<Table>) -> LResult<Surf> {
    let o = Opts::new("surf.revolve", t, lua)?;
    let c = o.point("center", v(0.0, 0.0, 0.0))?;
    let prof = o.points("profile")?;
    let a0 = o.ang("from", 0.0)?;
    let a1 = o.ang("to", 2.0 * PI)?;
    o.done()?;
    match prof {
        Some(p) if p.len() >= 2 && p.iter().all(|q| q.x >= 0.0) => Ok(Surf::Revolve { c, prof: p, a0, a1 }),
        _ => err("surf.revolve: profile = {vec3(r, 0, z), ...}: two or more points, radius r >= 0"),
    }
}

fn s_wave(lua: &Lua, t: Option<Table>) -> LResult<Surf> {
    let o = Opts::new("surf.wave", t, lua)?;
    let org = o.point("origin", v(0.0, 0.0, 0.0))?;
    let sx = o.len_in("size_x", 40.0, 1.0, 2000.0)?;
    let sy = o.len_in("size_y", 20.0, 1.0, 2000.0)?;
    let h = o.len_in("height", 8.0, 0.5, 300.0)?;
    let amp = o.len("amplitude", 2.0)?;
    let wx = o.num("waves_x", 1.0, 0.0, 20.0)?;
    let wy = o.num("waves_y", 0.5, 0.0, 20.0)?;
    let angle = o.ang("angle", 0.0)?;
    o.done()?;
    if amp.abs() >= h {
        return err("surf.wave: amplitude must stay below height (the surface would reach the ground)");
    }
    Ok(Surf::Wave { org, sx, sy, h, amp, wx, wy, angle })
}

/// A surface value: `surf.*{...}` or a Lua function (u, v) -> vec3.
enum SurfIn {
    S(Surf),
    F(mlua::Function),
}

fn surf_in(o: &Opts, k: &'static str) -> LResult<SurfIn> {
    match o.value(k)? {
        Value::UserData(u) if u.is::<Surf>() => Ok(SurfIn::S(u.borrow::<Surf>()?.clone())),
        Value::Function(f) => Ok(SurfIn::F(f)),
        other => err(format!(
            "{}: {k} needs surf.hypar/dome/vault/loft/ruled/revolve/wave{{...}} or a function(u, v) \
             returning vec3, got {}",
            o.f,
            other.type_name()
        )),
    }
}

impl SurfIn {
    fn at(&self, lua: &Lua, u: f64, w: f64, f: &str) -> LResult<V3> {
        match self {
            SurfIn::S(s) => {
                let p = s.at(u, w);
                if p.finite() {
                    Ok(p)
                } else {
                    err(format!("{f}: the surface is not finite at u {u:.3}, v {w:.3}"))
                }
            }
            SurfIn::F(func) => v3_of(lua, func.call((u, w))?, f),
        }
    }
}

// ---- gridshell ----

/// Members and panels over a surface: quad, triangulated or diagrid; columns where asked.
fn surface_grid(lua: &Lua, t: Option<Table>) -> Out {
    let o = Opts::new("surface_grid", t, lua)?;
    let s = surf_in(&o, "surface")?;
    let nu = o.int("nu", 12, 1, 400)? as usize;
    let nv = o.int("nv", 8, 1, 400)? as usize;
    let pattern = o.choice("pattern", "quad", &["quad", "tri", "diagrid"])?;
    let closed = o.flag("closed_u", false)?;
    // the seam joins row 0 mirrored in v: a half twist (Moebius band)
    let flip = o.flag("flip_seam", false)?;
    let memb_name = section_name(&o, "member", "CHS 168.3x8")?;
    let edge_name = section_name(&o, "edge", "CHS 219.1x10")?;
    let memb = o.prof("member", "CHS 168.3x8")?;
    let edge = o.prof("edge", "CHS 219.1x10")?;
    let panels = o.len_in("panels", 0.0, 0.0, 0.5)?;
    let cols = o.table("columns")?;
    let edge_cols = o.int("edge_columns", 0, 0, 400)? as usize;
    let colp = o.prof("column", "CHS 406.4x12")?;
    let ground = o.len("ground", 0.0)?;
    let mat = o.mat("material", "S355")?;
    let pmat = o.mat("panel_material", "glass")?;
    let name = o.text("name", "gridshell")?;
    o.done()?;
    let diag = pattern == "diagrid";
    if flip && !closed {
        return err("surface_grid: flip_seam needs closed_u = true");
    }
    if diag && (nv % 2 == 1 || (closed && nu % 2 == 1)) {
        return err("surface_grid: a diagrid needs an even nv (and an even nu when closed_u)");
    }
    if (nu + 1) * (nv + 1) > 40_000 {
        return err("surface_grid: at most 40000 grid points");
    }
    let mut p: Vec<Vec<V3>> = Vec::with_capacity(nu + 1);
    for i in 0..=nu {
        if closed && i == nu {
            let mut seam = p[0].clone();
            if flip {
                seam.reverse();
            }
            p.push(seam);
            continue;
        }
        let mut row = Vec::with_capacity(nv + 1);
        for j in 0..=nv {
            row.push(s.at(lua, i as f64 / nu as f64, j as f64 / nv as f64, "surface_grid")?);
        }
        p.push(row);
    }
    let node = |i: usize, j: usize| !diag || (i + j) % 2 == 0;
    let mut em = Em::new(lua, "surface_grid");
    // a dome or circular vault without panels is one grid structure the model builds on its
    // analytic host (one element, IFC structural members and nodes); other surfaces stay members
    let steel = |n: &Option<String>| n.as_deref().is_some_and(|n| ddc_geom::steel::sections::find(n).is_some());
    let host = match (&s, panels > 0.0) {
        (SurfIn::S(sf), false) if steel(&memb_name) && steel(&edge_name) => lattice_host(sf),
        _ => None,
    };
    if let (Some(host), Some(member), Some(edge_sec)) = (host, memb_name, edge_name) {
        if diag && nu % 2 == 1 && matches!(host, LatticeHost::Dome { .. }) {
            return err("surface_grid: a diagrid dome needs an even nu");
        }
        let kind = if diag { LatticeKind::Diagrid } else { LatticeKind::Gridshell };
        let pat = match pattern.as_str() {
            "diagrid" => LatticePattern::Diagrid,
            "tri" => LatticePattern::Triangular,
            _ => LatticePattern::Quad,
        };
        let pts: Vec<V3> = p.iter().flatten().copied().collect();
        let (cu, cv) = (nu as u32, nv as u32);
        em.lattice(
            "grid",
            move |slot, level| GeoOp::Gridshell {
                slot,
                level,
                host,
                kind: Some(kind),
                pattern: Some(pat),
                count_u: cu,
                count_v: cv,
                supports: None,
                member,
                edge: Some(edge_sec),
                node: None,
            },
            &pts,
            0.15,
        )?;
    } else {
        grid_members(&mut em, &p, nu, nv, closed, diag, &pattern, &memb, &edge, &mat, panels, &pmat)?;
    }
    let column = |em: &mut Em, i: usize, j: usize| -> LResult<()> {
        let top = p[i][j];
        if top.z - ground < 0.3 {
            return Ok(());
        }
        em.member("column", &colp, v(top.x, top.y, ground), top, &mat)?;
        Ok(())
    };
    if edge_cols > 0 {
        for j in [0, nv] {
            for i in (0..=nu).step_by(edge_cols) {
                if node(i, j) && !(closed && i == nu) {
                    column(&mut em, i, j)?;
                }
            }
        }
    }
    if let Some(cs) = cols {
        for c in cs.sequence_values::<Table>() {
            let c = c.map_err(|_| mlua::Error::RuntimeError("surface_grid: columns = {{u, v}, ...} with u, v in 0..1".into()))?;
            let (u, w): (f64, f64) = (c.get(1)?, c.get(2)?);
            if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&w) {
                return err("surface_grid: column u and v must be in 0..1");
            }
            let mut i = (u * nu as f64).round() as usize;
            let mut j = (w * nv as f64).round() as usize;
            if !node(i, j) {
                i = if i < nu { i + 1 } else { i - 1 };
            }
            if closed && i == nu {
                i = 0;
                if flip {
                    j = nv - j;
                }
            }
            if p[i][j].z - ground < 0.3 {
                return err(format!(
                    "surface_grid: the column at u {u}, v {w} would be {:.2} m tall; put it where \
                     the surface stands above the ground",
                    p[i][j].z - ground
                ));
            }
            column(&mut em, i, j)?;
        }
    }
    em.finish(&name)
}

/// Catalogue name of a section option given as text (or its default); None for a profile value.
fn section_name(o: &Opts, k: &'static str, d: &str) -> LResult<Option<String>> {
    Ok(match o.value(k)? {
        Value::Nil => Some(d.to_string()),
        Value::String(s) => Some(s.to_str()?.trim().to_string()),
        _ => None,
    })
}

fn p3m(p: V3) -> P3 {
    P3 { x: p.x, y: p.y, z: p.z, unit: LenUnit::M }
}

/// The model host of a dome (at most a hemisphere) or a circular vault; None for surfaces the
/// model has no host for. A dome without an oculus gets the model's top ring (a tenth of its
/// radius): bars cannot meet in the pole.
fn lattice_host(s: &Surf) -> Option<LatticeHost> {
    use ddc_geom::host::Host;
    let g = |p: V3| ddc_geom::v3(p.x, p.y, p.z);
    match s {
        Surf::Dome { c, r, rise, oculus } => {
            Host::dome(g(*c), *r, *rise, *oculus).ok()?;
            Some(LatticeHost::Dome {
                center: p3m(*c),
                radius: Len::m(*r),
                rise: Len::m(*rise),
                oculus: (*oculus > 0.0).then(|| Len::m(*oculus)),
            })
        }
        Surf::Vault { org, span, length, angle, cv } => {
            let rise = cv.circular_rise()?;
            let a = turn(v(org.x + span / 2.0, org.y, org.z), *org, *angle);
            let b = turn(v(org.x + span / 2.0, org.y + length, org.z), *org, *angle);
            Host::vault(g(a), g(b), *span, rise).ok()?;
            Some(LatticeHost::Vault { a: p3m(a), b: p3m(b), span: Len::m(*span), rise: Len::m(rise) })
        }
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn grid_members(
    em: &mut Em,
    p: &[Vec<V3>],
    nu: usize,
    nv: usize,
    closed: bool,
    diag: bool,
    pattern: &str,
    memb: &Profile,
    edge: &Profile,
    mat: &str,
    panels: f64,
    pmat: &str,
) -> LResult<()> {
    let u_edge = |i: usize| !closed && (i == 0 || i == nu);
    // boundary members follow every grid point; inner ones depend on the pattern
    for i in 0..nu {
        for j in [0, nv] {
            em.member("edge", edge, p[i][j], p[i + 1][j], &mat)?;
        }
    }
    for i in 0..=nu {
        if u_edge(i) {
            for j in 0..nv {
                em.member("edge", edge, p[i][j], p[i][j + 1], &mat)?;
            }
        }
    }
    for i in 0..nu {
        for j in 0..nv {
            let (a, b, c, d) = (p[i][j], p[i + 1][j], p[i + 1][j + 1], p[i][j + 1]);
            if diag {
                if (i + j) % 2 == 0 {
                    em.member("diagonal", memb, a, c, &mat)?;
                } else {
                    em.member("diagonal", memb, b, d, &mat)?;
                }
            } else {
                if j > 0 {
                    em.member("u_rib", memb, a, b, &mat)?;
                }
                if !u_edge(i) {
                    em.member("v_rib", memb, a, d, &mat)?;
                }
                if pattern == "tri" {
                    em.member("diagonal", memb, a, c, &mat)?;
                }
            }
            if panels > 0.0 {
                let even = (i + j) % 2 == 0;
                if pattern == "quad" {
                    em.panel("panel", &[a, b, c, d], panels, &pmat)?;
                } else if !diag || even {
                    em.panel("panel", &[a, b, c], panels, &pmat)?;
                    em.panel("panel", &[a, c, d], panels, &pmat)?;
                } else {
                    em.panel("panel", &[b, c, d], panels, &pmat)?;
                    em.panel("panel", &[b, d, a], panels, &pmat)?;
                }
            }
        }
    }
    Ok(())
}

// ---- geodesic sphere ----

/// Unit vertices and faces of an icosahedron with a vertex at each pole.
fn icosahedron() -> (Vec<V3>, Vec<[usize; 3]>) {
    let z = 1.0 / 5f64.sqrt();
    let rho = (1.0 - z * z).sqrt();
    let ring = |k: usize, z: f64, off: f64| {
        let a = off + 2.0 * PI * k as f64 / 5.0;
        v(rho * a.cos(), rho * a.sin(), z)
    };
    let mut p = vec![v(0.0, 0.0, 1.0), v(0.0, 0.0, -1.0)];
    p.extend((0..5).map(|k| ring(k, z, 0.0)));
    p.extend((0..5).map(|k| ring(k, -z, PI / 5.0)));
    let (r, b) = (|k: usize| 2 + k % 5, |k: usize| 7 + k % 5);
    let mut f = vec![];
    for k in 0..5 {
        f.push([0, r(k), r(k + 1)]);
        f.push([r(k), b(k), r(k + 1)]);
        f.push([b(k), b(k + 1), r(k + 1)]);
        f.push([1, b(k + 1), b(k)]);
    }
    (p, f)
}

/// A geodesic sphere or dome: class I frequency `frequency` subdivision of the icosahedron,
/// struts between shared nodes, optional triangular panels; `cut` keeps the triangles above
/// that height (-1 whole sphere, 0 hemisphere), the open rim gets the `edge` section. With an
/// odd frequency no ring of nodes lies on the equator, so a cut at 0 leaves a zigzag rim.
fn geodesic(lua: &Lua, t: Option<Table>) -> Out {
    use std::collections::HashMap;
    let o = Opts::new("geodesic", t, lua)?;
    let c = o.point("center", v(0.0, 0.0, 0.0))?;
    let r = o.len_in("radius", 10.0, 0.5, 500.0)?;
    let n = o.int("frequency", 4, 1, 8)? as usize;
    let cut = o.num("cut", -1.0, -1.0, 0.99)?;
    let memb = o.prof("member", "CHS 88.9x5")?;
    let edge = o.prof("edge", "CHS 168.3x8")?;
    let panels = o.len_in("panels", 0.0, 0.0, 0.5)?;
    let mat = o.mat("material", "S355")?;
    let pmat = o.mat("panel_material", "glass")?;
    let name = o.text("name", "geodesic")?;
    o.done()?;
    let (ico, faces) = icosahedron();
    let mut ids: HashMap<(i64, i64, i64), usize> = HashMap::new();
    let mut unit: Vec<V3> = vec![];
    let mut tris: Vec<[usize; 3]> = vec![];
    for f in &faces {
        let (a, b, d) = (ico[f[0]], ico[f[1]], ico[f[2]]);
        // grid point (i, j) of the face, pushed out to the sphere and shared across faces
        let mut at = |i: usize, j: usize| -> usize {
            let p = a.add(b.sub(a).scale(i as f64 / n as f64)).add(d.sub(a).scale(j as f64 / n as f64));
            let u = p.scale(1.0 / p.len());
            let key = ((u.x * 1e6).round() as i64, (u.y * 1e6).round() as i64, (u.z * 1e6).round() as i64);
            *ids.entry(key).or_insert_with(|| {
                unit.push(u);
                unit.len() - 1
            })
        };
        for i in 0..n {
            for j in 0..n - i {
                tris.push([at(i, j), at(i + 1, j), at(i, j + 1)]);
                if i + j + 1 < n {
                    tris.push([at(i + 1, j), at(i + 1, j + 1), at(i, j + 1)]);
                }
            }
        }
    }
    tris.retain(|t| t.iter().all(|&k| unit[k].z >= cut - 1e-9));
    if tris.is_empty() {
        return err("geodesic: cut leaves no triangle");
    }
    let pos = |k: usize| c.add(unit[k].scale(r));
    let mut uses: HashMap<(usize, usize), u32> = HashMap::new();
    for t in &tris {
        for e in 0..3 {
            let (a, b) = (t[e], t[(e + 1) % 3]);
            *uses.entry((a.min(b), a.max(b))).or_insert(0) += 1;
        }
    }
    let mut em = Em::new(lua, "geodesic");
    let mut keys: Vec<_> = uses.iter().collect();
    keys.sort();
    for (&(a, b), &cnt) in keys {
        let (role, prof) = if cnt == 1 { ("edge", &edge) } else { ("strut", &memb) };
        em.member(role, prof, pos(a), pos(b), &mat)?;
    }
    if panels > 0.0 {
        for t in &tris {
            em.panel("panel", &[pos(t[0]), pos(t[1]), pos(t[2])], panels, &pmat)?;
        }
    }
    em.finish(&name)
}

// ---- hypar umbrellas ----

/// Inverted umbrellas: four hyperbolic-paraboloid quadrants on one column each, in a grid.
fn hypar_umbrella(lua: &Lua, t: Option<Table>) -> Out {
    let o = Opts::new("hypar_umbrella", t, lua)?;
    let org = o.point("origin", v(0.0, 0.0, 0.0))?;
    let size = o.len_in("size", 12.0, 2.0, 60.0)?;
    let height = o.len_in("height", 6.0, 2.5, 40.0)?;
    let rise = o.len_in("rise", 2.0, 0.2, 20.0)?;
    let nx = o.int("nx", 2, 1, 40)? as usize;
    let ny = o.int("ny", 1, 1, 40)? as usize;
    let n = o.int("divisions", 6, 1, 40)? as usize;
    let rib = o.prof("rib", "RHS 250x150x8")?;
    let grid = o.prof("member", "CHS 88.9x5")?;
    let colp = o.prof("column", "CHS 457x16")?;
    let shell = o.len_in("shell", 0.06, 0.0, 0.3)?;
    let mat = o.mat("material", "S355")?;
    let smat = o.mat("shell_material", "C40/50")?;
    let name = o.text("name", "umbrellas")?;
    o.done()?;
    let h2 = size / 2.0;
    let mut em = Em::new(lua, "hypar_umbrella");
    for ix in 0..nx {
        for iy in 0..ny {
            let c = v(org.x + ix as f64 * size, org.y + iy as f64 * size, org.z + height);
            em.member("column", &colp, v(c.x, c.y, org.z), c, &mat)?;
            for (sx, sy) in [(1.0, 1.0), (-1.0, 1.0), (-1.0, -1.0), (1.0, -1.0)] {
                // centre low, the two edge midpoints high, the outer corner low: a saddle
                let q = [
                    c,
                    c.add(v(sx * h2, 0.0, rise)),
                    c.add(v(sx * h2, sy * h2, 0.0)),
                    c.add(v(0.0, sy * h2, rise)),
                ];
                let s = Surf::Bilinear(q);
                let at = |i: usize, j: usize| s.at(i as f64 / n as f64, j as f64 / n as f64);
                // straight rulings both ways; the four boundary lines are ribs
                for i in 0..=n {
                    let (role, prof) = if i == 0 || i == n { ("rib", &rib) } else { ("member", &grid) };
                    for j in 0..n {
                        em.member(role, prof, at(i, j), at(i, j + 1), &mat)?;
                        em.member(role, prof, at(j, i), at(j + 1, i), &mat)?;
                    }
                }
                if shell > 0.0 {
                    for i in 0..n {
                        for j in 0..n {
                            let quad = [at(i, j), at(i + 1, j), at(i + 1, j + 1), at(i, j + 1)];
                            em.panel("shell", &quad, shell, &smat)?;
                        }
                    }
                }
            }
        }
    }
    em.finish(&name)
}

// ---- membranes ----

/// A tensile membrane over corner points: force-density form finding on a radial net,
/// edge cables, masts with guys under raised corners, an optional central mast.
fn membrane(lua: &Lua, t: Option<Table>) -> Out {
    let o = Opts::new("membrane", t, lua)?;
    let corners = o.points("corners")?;
    let peak = o.opt_point("peak")?;
    let rings = o.int("rings", 8, 2, 60)? as usize;
    let per_edge = o.int("divisions", 6, 1, 60)? as usize;
    let iters = o.int("iterations", 300, 10, 5000)? as usize;
    let ground = o.len("ground", 0.0)?;
    let t_m = o.len_in("thickness", 0.01, 0.005, 0.05)?;
    let mast = o.prof("mast", "CHS 323.9x10")?;
    let cable = o.prof("cable", "round 40")?;
    let guys = o.flag("guys", true)?;
    let mat = o.mat("material", "S355")?;
    let mmat = o.mat("membrane_material", "PTFE fabric")?;
    let name = o.text("name", "membrane")?;
    o.done()?;
    let Some(cs) = corners.filter(|c| c.len() >= 3) else {
        return err("membrane: corners = {vec3, vec3, vec3, ...} (three or more, in order around)");
    };
    // boundary ring: every edge split into `per_edge` straight cable segments
    let mut bnd: Vec<V3> = vec![];
    for k in 0..cs.len() {
        for s in 0..per_edge {
            bnd.push(lerp(cs[k], cs[(k + 1) % cs.len()], s as f64 / per_edge as f64));
        }
    }
    let m = bnd.len();
    let avg = bnd.iter().fold(v(0.0, 0.0, 0.0), |a, b| a.add(*b)).scale(1.0 / m as f64);
    let centre = peak.unwrap_or(avg);
    // net[k][s]: ring k (0 = centre, rings = boundary)
    let mut net: Vec<Vec<V3>> = (0..=rings)
        .map(|k| (0..m).map(|s| lerp(centre, bnd[s], k as f64 / rings as f64)).collect())
        .collect();
    let mut c = centre;
    for _ in 0..iters {
        for k in 1..rings {
            for s in 0..m {
                let inner = if k == 1 { c } else { net[k - 1][s] };
                let sum = inner
                    .add(net[k + 1][s])
                    .add(net[k][(s + m - 1) % m])
                    .add(net[k][(s + 1) % m]);
                net[k][s] = sum.scale(0.25);
            }
        }
        if peak.is_none() {
            c = net[1].iter().fold(v(0.0, 0.0, 0.0), |a, b| a.add(*b)).scale(1.0 / m as f64);
        }
        for s in 0..m {
            net[0][s] = c;
        }
    }
    if net.iter().flatten().any(|q| q.z < ground + 0.3) {
        return err("membrane: the form-found surface comes within 0.3 m of the ground; raise the corners or add a peak");
    }
    let mut em = Em::new(lua, "membrane");
    for k in 0..rings {
        for s in 0..m {
            let s1 = (s + 1) % m;
            em.panel("membrane", &[net[k][s], net[k + 1][s], net[k + 1][s1], net[k][s1]], t_m, &mmat)?;
        }
    }
    for s in 0..m {
        em.member("edge_cable", &cable, bnd[s], bnd[(s + 1) % m], &mat)?;
    }
    for &corner in &cs {
        if corner.z - ground < 0.5 {
            continue;
        }
        em.member("mast", &mast, v(corner.x, corner.y, ground), corner, &mat)?;
        if guys {
            let Some(out) = v(corner.x - avg.x, corner.y - avg.y, 0.0).unit() else { continue };
            let foot = v(corner.x, corner.y, ground).add(out.scale(0.6 * (corner.z - ground)));
            em.member("guy", &cable, corner, foot, &mat)?;
        }
    }
    if let Some(pk) = peak {
        em.member("mast", &mast, v(pk.x, pk.y, ground), pk, &mat)?;
    }
    em.finish(&name)
}

// ---- perforated screens ----

/// Deterministic generator (64-bit LCG), so a seed always gives the same pattern.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Keeps the part of polygon `p` where `(x - m) . d <= -off`.
fn clip(p: &[[f64; 2]], m: [f64; 2], d: [f64; 2], off: f64) -> Vec<[f64; 2]> {
    let f = |q: [f64; 2]| (q[0] - m[0]) * d[0] + (q[1] - m[1]) * d[1] + off;
    let mut out = vec![];
    for i in 0..p.len() {
        let (a, b) = (p[i], p[(i + 1) % p.len()]);
        let (fa, fb) = (f(a), f(b));
        if fa <= 0.0 {
            out.push(a);
        }
        if (fa < 0.0) != (fb < 0.0) && (fa - fb).abs() > 1e-15 {
            let t = fa / (fa - fb);
            out.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
        }
    }
    out
}

fn area(p: &[[f64; 2]]) -> f64 {
    let mut s = 0.0;
    for i in 0..p.len() {
        let (a, b) = (p[i], p[(i + 1) % p.len()]);
        s += a[0] * b[1] - b[0] * a[1];
    }
    s / 2.0
}

/// Plates with Voronoi openings between posts; openings grow towards an attractor.
fn perforated_panel(lua: &Lua, t: Option<Table>) -> Out {
    let o = Opts::new("perforated_panel", t, lua)?;
    let a = o.point("from", v(0.0, 0.0, 0.0))?;
    let b = o.point("to", v(12.0, 0.0, 0.0))?;
    let count = o.int("count", 4, 1, 200)? as usize;
    let height = o.len_in("height", 3.0, 0.3, 30.0)?;
    let base = o.len_in("base", 0.0, 0.0, 200.0)?;
    let cells = o.int("cells", 40, 2, 400)? as usize;
    let seed = o.int("seed", 1, 0, i64::MAX)? as u64;
    let rib = o.len_in("rib", 0.06, 0.01, 1.0)?;
    let border = o.len_in("border", 0.1, 0.02, 1.0)?;
    let thick = o.len_in("thickness", 0.008, 0.005, 0.1)?;
    let attractor = o.opt_point("attractor")?;
    let open_min = o.num("open_min", 0.35, 0.05, 1.0)?;
    let post = o.prof("post", "SHS 120x8")?;
    let mat = o.mat("material", "weathering steel")?;
    let pmat = o.mat("post_material", "S355")?;
    let name = o.text("name", "screen")?;
    o.done()?;
    let along = v(b.x - a.x, b.y - a.y, 0.0);
    let Some(x) = along.unit() else {
        return err("perforated_panel: from and to must differ in plan");
    };
    let e = crate::audit::extents(&post);
    let pw = e[1] - e[0];
    let w = (along.len() - (count + 1) as f64 * pw) / count as f64;
    if w < 0.3 {
        return err("perforated_panel: panels narrower than 0.3 m; use fewer panels or a longer line");
    }
    let z = x.cross(UP); // plate normal; local y is up
    let mut rng = Lcg(seed.wrapping_mul(0x9E3779B97F4A7C15) ^ 0x5DEECE66D);
    let mut em = Em::new(lua, "perforated_panel");
    let at = |s: f64, h: f64| a.add(x.scale(s)).add(UP.scale(h));
    for k in 0..=count {
        let s = k as f64 * (w + pw) + pw / 2.0;
        em.member("post", &post, at(s, 0.0), at(s, base + height), &pmat)?;
    }
    let (x0, x1, y0, y1) = (border, w - border, border, height - border);
    if x1 - x0 < 0.1 || y1 - y0 < 0.1 {
        return err("perforated_panel: the border leaves no room for openings");
    }
    for k in 0..count {
        let s0 = k as f64 * (w + pw) + pw;
        // jittered grid of seeds keeps the cells even in size
        let cols = ((cells as f64 * (x1 - x0) / (y1 - y0)).sqrt().round() as usize).max(1);
        let rows = cells.div_ceil(cols).max(1);
        let mut seeds = vec![];
        for r in 0..rows {
            for c in 0..cols {
                let jx = 0.15 + 0.7 * rng.next();
                let jy = 0.15 + 0.7 * rng.next();
                seeds.push([
                    x0 + (x1 - x0) * (c as f64 + jx) / cols as f64,
                    y0 + (y1 - y0) * (r as f64 + jy) / rows as f64,
                ]);
            }
        }
        let rect = vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]];
        let mut holes: Vec<Vec<P2>> = vec![];
        for (i, si) in seeds.iter().enumerate() {
            let mut poly = rect.clone();
            for (j, sj) in seeds.iter().enumerate() {
                if i == j {
                    continue;
                }
                let d = [sj[0] - si[0], sj[1] - si[1]];
                let l = d[0].hypot(d[1]);
                if l < 1e-9 {
                    continue;
                }
                let m = [(si[0] + sj[0]) / 2.0, (si[1] + sj[1]) / 2.0];
                poly = clip(&poly, m, [d[0] / l, d[1] / l], rib / 2.0);
                if poly.len() < 3 {
                    break;
                }
            }
            if poly.len() < 3 || area(&poly) < 0.002 {
                continue;
            }
            let n = poly.len() as f64;
            let cx = poly.iter().map(|q| q[0]).sum::<f64>() / n;
            let cy = poly.iter().map(|q| q[1]).sum::<f64>() / n;
            let scale = match attractor {
                Some(p) => {
                    let world = at(s0 + cx, base + cy);
                    let d = world.sub(p).len();
                    let reach = (along.len().max(height)) / 2.0;
                    open_min + (1.0 - open_min) * (1.0 - d / reach).clamp(0.0, 1.0)
                }
                None => 1.0,
            };
            let mut hole: Vec<P2> = vec![];
            for q in &poly {
                let (hx, hy) = (cx + (q[0] - cx) * scale, cy + (q[1] - cy) * scale);
                if hole.last().is_none_or(|l| {
                    let [lx, ly] = l.to_m();
                    (lx - hx).hypot(ly - hy) > 0.002
                }) {
                    hole.push(P2::m(hx, hy));
                }
            }
            if hole.len() >= 3 && area(&hole.iter().map(|p| p.to_m()).collect::<Vec<_>>()).abs() >= 0.0005 {
                holes.push(hole);
            }
        }
        let outer = vec![P2::m(0.0, 0.0), P2::m(w, 0.0), P2::m(w, height), P2::m(0.0, height)];
        let f = FormV(Shape::Extrude {
            prof: Profile::Rings { outer, holes },
            o: at(s0, base).sub(z.scale(thick / 2.0)),
            x,
            z,
            len: thick,
        });
        em.solid("plate", &f, &mat)?;
    }
    em.finish(&name)
}
