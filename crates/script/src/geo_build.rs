//! Elements of the geometry language: freeform surfaces and shells, lattices between any
//! curves or inside any volume, and the classification options of `part`.

use ddc_ops::{
    Angle, Cat, Curve3, CurveFit, GeoOp, IfcClass, Len, Ref, ShellSide, SurfaceBy, P3,
};
use mlua::{FromLua, Lua, Table, Value};

use crate::blocks::{block, v, Em, Opts};
use crate::geo_curve::{path_val, Poly};
use crate::values::{err, GroupH, LResult, PathV, V3};

type Out = LResult<(GroupH, Table)>;

pub(crate) fn install(lua: &Lua, g: &Table) -> LResult<()> {
    g.set("freeform", block(lua, freeform)?)?;
    g.set("lattice_between", block(lua, lattice_between)?)?;
    g.set("lattice_volume", block(lua, lattice_volume)?)?;
    Ok(())
}

// ---- classification of a part ----

/// What `part(name, form, options)` may state besides the form.
pub(crate) struct PartOpts {
    pub material: Option<String>,
    pub ifc: Option<IfcClass>,
    pub level: Option<Ref>,
}

/// The third argument of `part`: a material text, or `{material, ifc, predefined, level}`.
/// The IFC class is checked by the model against its allow list when the ops are applied.
pub(crate) fn part_opts(lua: &Lua, x: Value) -> LResult<PartOpts> {
    match x {
        Value::Nil => Ok(PartOpts { material: None, ifc: None, level: None }),
        Value::String(s) => Ok(PartOpts {
            material: Some(s.to_str()?.to_string()),
            ifc: None,
            level: None,
        }),
        Value::Table(t) => {
            let o = Opts::new("part", Some(t), lua)?;
            let opt = |k: &'static str| -> LResult<Option<String>> {
                if matches!(o.value(k)?, Value::Nil) {
                    Ok(None)
                } else {
                    o.text(k, "").map(Some)
                }
            };
            let material = opt("material")?.or(opt("mat")?);
            let class = opt("ifc")?.or(opt("class")?);
            let pre = opt("predefined")?.or(opt("type")?);
            let level = opt("level")?;
            o.done()?;
            if class.is_none() && pre.is_some() {
                return err("part: predefined needs ifc, e.g. {ifc = \"IfcBeam\", predefined = \"BEAM\"}");
            }
            Ok(PartOpts {
                material,
                ifc: class.map(|c| IfcClass { class: c, predefined: pre }),
                level: level.map(|l| Ref::name(Cat::Level, &l)),
            })
        }
        other => err(format!(
            "part: the third argument is a material text like \"S355\" or a table like \
             {{material = \"S355\", ifc = \"IfcColumn\", level = \"Level 2\"}}, got {}",
            other.type_name()
        )),
    }
}

// ---- freeform surfaces ----

fn r9(x: f64) -> f64 {
    (x * 1e9).round() / 1e9 + 0.0
}

fn p3(p: V3) -> P3 {
    P3::m(r9(p.x), r9(p.y), r9(p.z))
}

/// Points a fitted curve keeps; a denser path is resampled by arc length.
const MAX_FIT: usize = 64;

fn curve3(lua: &Lua, x: Value, f: &str, k: &str, fit: CurveFit, sink: &mut Vec<V3>) -> LResult<Curve3> {
    let pts: Vec<V3> = match &x {
        Value::UserData(u) if u.is::<PathV>() => {
            let poly = Poly::of(&u.borrow::<PathV>()?.clone());
            let keep = if fit == CurveFit::Polyline { 400 } else { MAX_FIT };
            if poly.pts.len() <= keep {
                poly.pts.clone()
            } else {
                (0..keep).map(|i| poly.at(i as f64 / (keep - 1) as f64).0).collect()
            }
        }
        Value::Table(t) => {
            let mut out = vec![];
            for p in t.clone().sequence_values::<Value>() {
                out.push(V3::from_lua(p?, lua)?);
            }
            out
        }
        other => {
            return err(format!(
                "{f}: {k} needs a curve (curve.spline{{...}}, polyline(...)) or a list of points \
                 like {{vec3(0,0,0), vec3(5,0,3)}}, got {}",
                other.type_name()
            ))
        }
    };
    if pts.len() < 2 || pts.iter().any(|p| !p.finite()) {
        return err(format!("{f}: {k} needs 2 or more finite points"));
    }
    sink.extend(pts.iter().copied());
    Ok(Curve3 { pts: pts.into_iter().map(p3).collect(), fit: Some(fit), degree: None })
}

fn curves3(lua: &Lua, x: Value, f: &str, k: &str, fit: CurveFit, sink: &mut Vec<V3>) -> LResult<Vec<Curve3>> {
    let Value::Table(t) = x else {
        return err(format!("{f}: {k} needs a list of curves, e.g. {k} = {{curve1, curve2, curve3}}"));
    };
    let mut out = vec![];
    for c in t.sequence_values::<Value>() {
        out.push(curve3(lua, c?, f, k, fit, sink)?);
    }
    if out.len() < 2 {
        return err(format!("{f}: {k} needs 2 or more curves"));
    }
    Ok(out)
}

/// A NURBS surface as one element: `by` = loft | ruled | revolve | net | network | dome |
/// vault; `thickness` makes it a shell.
fn freeform(lua: &Lua, t: Option<Table>) -> Out {
    let f = "freeform";
    let o = Opts::new(f, t, lua)?;
    let by = o.text("by", "")?;
    let thick = o.len("thickness", 0.0)?;
    let side = match o.choice("side", "normal", &["normal", "reverse", "center"])?.as_str() {
        "reverse" => ShellSide::Reverse,
        "center" => ShellSide::Center,
        _ => ShellSide::Normal,
    };
    let fit = match o.choice("fit", "through", &["through", "control", "polyline"])?.as_str() {
        "control" => CurveFit::Control,
        "polyline" => CurveFit::Polyline,
        _ => CurveFit::Through,
    };
    let name = o.text("name", "freeform")?;
    let mat = o.value("material")?;
    let material = match mat {
        Value::Nil => None,
        Value::String(s) => Some(s.to_str()?.to_string()),
        other => return err(format!("freeform: material needs text, got {}", other.type_name())),
    };
    if thick < 0.0 || thick > 5.0 {
        return err("freeform: thickness is 0 (a bare surface) to 5 m, e.g. thickness = 0.12");
    }
    let mut pts: Vec<V3> = vec![];
    let degree = |k: &'static str| -> LResult<Option<u32>> {
        if matches!(o.value(k)?, Value::Nil) {
            Ok(None)
        } else {
            Ok(Some(o.int(k, 3, 1, 5)? as u32))
        }
    };
    let surface = match by.as_str() {
        "loft" => {
            let curves = curves3(lua, o.value("curves")?, f, "curves", fit, &mut pts)?;
            SurfaceBy::Loft { curves, degree: degree("degree")?, params: None }
        }
        "ruled" => {
            let a = curve3(lua, o.value("a")?, f, "a", fit, &mut pts)?;
            let b = curve3(lua, o.value("b")?, f, "b", fit, &mut pts)?;
            SurfaceBy::Ruled { a, b }
        }
        "revolve" => {
            let profile = curve3(lua, o.value("profile")?, f, "profile", fit, &mut pts)?;
            let a = o.point("axis_a", v(0.0, 0.0, 0.0))?;
            let b = o.point("axis_b", v(0.0, 0.0, 1.0))?;
            let ang = o.ang("angle", 2.0 * std::f64::consts::PI)?;
            if a.sub(b).len() < 1e-6 || ang.abs() < 1e-6 {
                return err("freeform: revolve needs two different axis points and a non-zero angle");
            }
            SurfaceBy::Revolve { profile, axis_a: p3(a), axis_b: p3(b), angle: Angle::rad(ang) }
        }
        "net" => {
            let rows = match o.value("rows")? {
                Value::Table(t) => t,
                _ => {
                    return err(
                        "freeform: net needs rows = {{p00, p01, ...}, {p10, p11, ...}, ...}: lists of \
                         points, every row the same length",
                    )
                }
            };
            let mut grid: Vec<Vec<P3>> = vec![];
            for r in rows.sequence_values::<Value>() {
                let Value::Table(rt) = r? else {
                    return err("freeform: every row of a net is a list of points");
                };
                let mut row = vec![];
                for p in rt.sequence_values::<Value>() {
                    let p = V3::from_lua(p?, lua)?;
                    pts.push(p);
                    row.push(p3(p));
                }
                grid.push(row);
            }
            if grid.len() < 2 || grid.iter().any(|r| r.len() != grid[0].len() || r.len() < 2) {
                return err("freeform: a net needs 2 or more rows of the same length (2 or more points)");
            }
            let weights = match o.table("weights")? {
                None => None,
                Some(wt) => {
                    let mut w = vec![];
                    for r in wt.sequence_values::<Table>() {
                        w.push(r?.sequence_values::<f64>().collect::<Result<Vec<f64>, mlua::Error>>()?);
                    }
                    Some(w)
                }
            };
            SurfaceBy::Points { rows: grid, degree_u: degree("degree_u")?, degree_v: degree("degree_v")?, weights }
        }
        "network" => {
            let u_curves = curves3(lua, o.value("u_curves")?, f, "u_curves", fit, &mut pts)?;
            let v_curves = curves3(lua, o.value("v_curves")?, f, "v_curves", fit, &mut pts)?;
            let tol = o.len("tolerance", 0.0)?;
            SurfaceBy::Network {
                u_curves,
                v_curves,
                degree: degree("degree")?,
                tolerance: (tol > 0.0).then(|| Len::m(tol)),
            }
        }
        "dome" => {
            let c = o.point("center", v(0.0, 0.0, 0.0))?;
            let r = o.len_in("radius", 10.0, 0.5, 500.0)?;
            let rise = o.len_in("rise", r / 2.0, 0.1, r)?;
            pts.extend([v(c.x - r, c.y - r, c.z), v(c.x + r, c.y + r, c.z + rise)]);
            SurfaceBy::Dome { center: p3(c), radius: Len::m(r), rise: Len::m(rise) }
        }
        "vault" => {
            let a = o.point("a", v(0.0, 0.0, 0.0))?;
            let b = o.point("b", v(0.0, 20.0, 0.0))?;
            let span = o.len_in("span", 10.0, 0.5, 500.0)?;
            let rise = o.len_in("rise", span / 4.0, 0.1, span / 2.0)?;
            pts.extend([a, b, v(a.x + span, a.y, a.z + rise), v(b.x + span, b.y, b.z + rise)]);
            SurfaceBy::Vault { a: p3(a), b: p3(b), span: Len::m(span), rise: Len::m(rise) }
        }
        _ => {
            return err(
                "freeform: by is one of loft, ruled, revolve, net, network, dome, vault, e.g. \
                 freeform{by = \"loft\", curves = {c1, c2, c3}, thickness = 0.1}",
            )
        }
    };
    o.done()?;
    let thickness = (thick > 0.0).then(|| Len::m(thick));
    let shell_side = thickness.is_some().then_some(side);
    let mut em = Em::new(lua, "freeform");
    let nm = name.clone();
    em.lattice(
        &name,
        move |slot, level| GeoOp::Freeform {
            slot,
            level,
            surface,
            thickness,
            side: shell_side,
            trim: None,
            material,
            name: Some(nm),
        },
        &pts,
        thick.max(0.05),
    )?;
    em.finish(&name)
}

// ---- lattices ----

/// Point of a leg at station fraction `t`: by arc length, or by height.
fn station(p: &Poly, t: f64, by_height: bool) -> V3 {
    if !by_height {
        return p.at(t).0;
    }
    let (z0, z1) = (p.pts[0].z, p.pts[p.pts.len() - 1].z);
    let want = z0 + (z1 - z0) * t;
    let (mut lo, mut hi) = (0.0, p.total());
    for _ in 0..50 {
        let mid = (lo + hi) / 2.0;
        if p.at_len(mid).0.z < want {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    p.at_len((lo + hi) / 2.0).0
}

/// Members between any number of leg curves: the legs, a ring at every station, bracing
/// in each bay between neighbouring legs and optional platforms.
fn lattice_between(lua: &Lua, t: Option<Table>) -> Out {
    let f = "lattice_between";
    let o = Opts::new(f, t, lua)?;
    let legs = o.value("legs")?;
    let n = o.int("stations", 12, 1, 400)? as usize;
    let by_height = o.choice("spacing", "length", &["length", "height"])? == "height";
    let ratio = o.num("ratio", 1.0, 0.5, 2.0)?;
    let ring = o.flag("ring", true)?;
    let bracing = o.choice("bracing", "x", &["x", "warren", "none"])?;
    let leg_p = o.prof("leg", "CHS 323.9x12.5")?;
    let ring_p = o.prof("ring_member", "CHS 168.3x8")?;
    let brace_p = o.prof("brace", "CHS 114.3x6")?;
    let plat_every = o.int("platform_every", 0, 0, 400)? as usize;
    let plat_t = o.len_in("platform_thickness", 0.2, 0.02, 2.0)?;
    let mat = o.mat("material", "S355")?;
    let deck_mat = o.mat("platform_material", "concrete")?;
    let name = o.text("name", "lattice")?;
    o.done()?;
    let Value::Table(lt) = legs else {
        return err(
            "lattice_between: legs needs a list of 2 or more curves, e.g. \
             lattice_between{legs = {leg1, leg2, leg3}, stations = 20}",
        );
    };
    let mut polys = vec![];
    for c in lt.sequence_values::<Value>() {
        let p = Poly::of(&path_val(lua, c?, f, "legs")?);
        if !p.usable() {
            return err("lattice_between: a leg has no length");
        }
        if by_height && p.pts[p.pts.len() - 1].z <= p.pts[0].z + 1e-6 {
            return err("lattice_between: spacing = \"height\" needs legs that rise (each leg ends higher than it starts)");
        }
        polys.push(p);
    }
    let legs_n = polys.len();
    if legs_n < 2 {
        return err("lattice_between: legs needs 2 or more curves");
    }
    let fracs: Vec<f64> = (0..=n)
        .map(|k| if (ratio - 1.0).abs() < 1e-9 { k as f64 / n as f64 } else { (ratio.powi(k as i32) - 1.0) / (ratio.powi(n as i32) - 1.0) })
        .collect();
    let g: Vec<Vec<V3>> = polys.iter().map(|p| fracs.iter().map(|t| station(p, *t, by_height)).collect()).collect();
    let mut em = Em::new(lua, "lattice_between");
    for l in 0..legs_n {
        for k in 0..n {
            em.member("chord", &leg_p, g[l][k], g[l][k + 1], &mat)?;
        }
    }
    let pairs: Vec<(usize, usize)> = if legs_n == 2 {
        vec![(0, 1)]
    } else {
        (0..legs_n).map(|l| (l, (l + 1) % legs_n)).collect()
    };
    for &(a, b) in &pairs {
        if ring {
            for k in 0..=n {
                em.member("brace", &ring_p, g[a][k], g[b][k], &mat)?;
            }
        }
        for k in 0..n {
            match bracing.as_str() {
                "x" => {
                    em.member("diagonal", &brace_p, g[a][k], g[b][k + 1], &mat)?;
                    em.member("diagonal", &brace_p, g[b][k], g[a][k + 1], &mat)?;
                }
                "warren" => {
                    if (k + a) % 2 == 0 {
                        em.member("diagonal", &brace_p, g[a][k], g[b][k + 1], &mat)?;
                    } else {
                        em.member("diagonal", &brace_p, g[b][k], g[a][k + 1], &mat)?;
                    }
                }
                _ => {}
            }
        }
    }
    if plat_every > 0 && legs_n >= 3 {
        for k in (plat_every..=n).step_by(plat_every) {
            let outline: Vec<V3> = (0..legs_n).map(|l| g[l][k]).collect();
            let top = outline.iter().map(|p| p.z).sum::<f64>() / legs_n as f64;
            em.slab("deck", &outline, &[], top, plat_t, &deck_mat)?;
        }
    }
    em.finish(&name)
}

/// Members between the nodes of a 3D grid that lie inside a region.
fn lattice_volume(lua: &Lua, t: Option<Table>) -> Out {
    let f = "lattice_volume";
    let o = Opts::new(f, t, lua)?;
    let lo = o.point("min", v(0.0, 0.0, 0.0))?;
    let hi = o.point("max", v(10.0, 10.0, 10.0))?;
    let cell = o.len_in("cell", 1.0, 0.05, 200.0)?;
    let pattern = o.choice("pattern", "cubic", &["cubic", "diagonal", "braced", "bcc"])?;
    let inside = o.value("inside")?;
    let prof = o.prof("member", "CHS 60.3x4")?;
    let role = o.text("role", "strut")?;
    let mat = o.mat("material", "S355")?;
    let name = o.text("name", "lattice volume")?;
    o.done()?;
    let inside = match inside {
        Value::Nil => None,
        Value::Function(func) => Some(func),
        other => {
            return err(format!(
                "lattice_volume: inside is function(p) return true end (p is a vec3), got {}",
                other.type_name()
            ))
        }
    };
    let size = hi.sub(lo);
    if !(size.x > cell && size.y > cell && size.z > cell) {
        return err("lattice_volume: max must exceed min by more than one cell in x, y and z, e.g. {min = vec3(0,0,0), max = vec3(10,10,6), cell = 1}");
    }
    let (nx, ny, nz) = (
        (size.x / cell + 1e-9).floor() as usize,
        (size.y / cell + 1e-9).floor() as usize,
        (size.z / cell + 1e-9).floor() as usize,
    );
    if (nx + 1) * (ny + 1) * (nz + 1) > 60_000 {
        return err(format!(
            "lattice_volume: {} nodes; at most 60000, use a larger cell",
            (nx + 1) * (ny + 1) * (nz + 1)
        ));
    }
    let at = |i: usize, j: usize, k: usize| v(lo.x + i as f64 * cell, lo.y + j as f64 * cell, lo.z + k as f64 * cell);
    let test = |p: V3| -> LResult<bool> {
        match &inside {
            None => Ok(true),
            Some(func) => {
                let r: Value = func.call(p)?;
                Ok(!matches!(r, Value::Nil | Value::Boolean(false)))
            }
        }
    };
    let idx = |i: usize, j: usize, k: usize| (i * (ny + 1) + j) * (nz + 1) + k;
    let mut ok = vec![false; (nx + 1) * (ny + 1) * (nz + 1)];
    for i in 0..=nx {
        for j in 0..=ny {
            for k in 0..=nz {
                ok[idx(i, j, k)] = test(at(i, j, k))?;
            }
        }
    }
    let mut em = Em::new(lua, "lattice_volume");
    let made = std::cell::Cell::new(0usize);
    let link = |em: &mut Em, a: (usize, usize, usize), b: (usize, usize, usize)| -> LResult<()> {
        if ok[idx(a.0, a.1, a.2)] && ok[idx(b.0, b.1, b.2)] {
            made.set(made.get() + 1);
            if made.get() > 30_000 {
                return err("lattice_volume: more than 30000 members; use a larger cell");
            }
            em.member(&role, &prof, at(a.0, a.1, a.2), at(b.0, b.1, b.2), &mat)?;
        }
        Ok(())
    };
    for i in 0..=nx {
        for j in 0..=ny {
            for k in 0..=nz {
                if pattern != "bcc" {
                    if i < nx {
                        link(&mut em, (i, j, k), (i + 1, j, k))?;
                    }
                    if j < ny {
                        link(&mut em, (i, j, k), (i, j + 1, k))?;
                    }
                    if k < nz {
                        link(&mut em, (i, j, k), (i, j, k + 1))?;
                    }
                }
                if i < nx && j < ny && k < nz {
                    match pattern.as_str() {
                        "diagonal" => link(&mut em, (i, j, k), (i + 1, j + 1, k + 1))?,
                        "braced" => {
                            link(&mut em, (i, j, k), (i + 1, j + 1, k))?;
                            link(&mut em, (i, j, k), (i + 1, j, k + 1))?;
                            link(&mut em, (i, j, k), (i, j + 1, k + 1))?;
                        }
                        "bcc" => {
                            let c = at(i, j, k).add(v(cell / 2.0, cell / 2.0, cell / 2.0));
                            if test(c)? {
                                for (di, dj, dk) in [(0, 0, 0), (1, 0, 0), (0, 1, 0), (1, 1, 0), (0, 0, 1), (1, 0, 1), (0, 1, 1), (1, 1, 1)] {
                                    let (a, b2, d) = (i + di, j + dj, k + dk);
                                    if ok[idx(a, b2, d)] {
                                        made.set(made.get() + 1);
                                        if made.get() > 30_000 {
                                            return err("lattice_volume: more than 30000 members; use a larger cell");
                                        }
                                        em.member(&role, &prof, c, at(a, b2, d), &mat)?;
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    em.finish(&name)
}
