// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Lowering of the freeform ops (`geometry.freeform`, `edit_point`, `thicken`, `trim` and the
//! freeform case of `convert`, `rebuild`, `panelize`) onto `freeform.rs` and `panel_system.rs`.
//! Op coordinates are absolute; the patch is stored with z above its level, so every point
//! (and every panel row height) is shifted by the level elevation here.

use ddc_geom::nurbs::{self, Curve, NurbsError, Surface, Trim};
use ddc_geom::panelize::{PanelLimits, Pattern, Rows, Spacing};
use ddc_geom::{v3, V3};
use ddc_ops::{
    Cat, ConvertTo, Curve3, CurveFit, GeoOp, PanelDiv, PanelPattern, Ref,
    ShellKind as OpShellKind, ShellSide as OpSide, SurfaceBy, UvTrim, P3,
};

use super::{bad_value, level_row, resolve_as, unsupported, Fail, Run};
use crate::{FreeformSpec, Model, PanelLayout, PanelSystemSpec, ShellSide};

const MAX_DEGREE: u32 = 5;

fn geo(e: NurbsError) -> Fail {
    bad_value(format!("freeform: {e}"))
}

fn side_of(s: OpSide) -> ShellSide {
    match s {
        OpSide::Normal => ShellSide::Normal,
        OpSide::Reverse => ShellSide::Reverse,
        OpSide::Center => ShellSide::Center,
    }
}

fn trim_of(t: &UvTrim) -> Trim {
    Trim { outer: t.outer.clone(), holes: t.holes.clone() }
}

fn pt(p: &P3, elev: f64) -> V3 {
    let [x, y, z] = p.to_m();
    v3(x, y, z - elev)
}

fn degree(d: Option<u32>) -> Result<usize, Fail> {
    match d.unwrap_or(3) {
        k @ 1..=MAX_DEGREE => Ok(k as usize),
        k => Err(bad_value(format!("degree {k} is outside 1..{MAX_DEGREE}"))),
    }
}

use nurbs::clamped_knots as clamped;

fn curve(c: &Curve3, elev: f64) -> Result<Curve, Fail> {
    let pts: Vec<V3> = c.pts.iter().map(|p| pt(p, elev)).collect();
    if pts.len() < 2 {
        return Err(bad_value("a curve needs at least two points"));
    }
    let p = degree(c.degree)?;
    match c.fit.unwrap_or(CurveFit::Through) {
        CurveFit::Through => Curve::interpolate(&pts, p).map_err(geo),
        CurveFit::Polyline => Curve::polyline(&pts).map_err(geo),
        CurveFit::Control => {
            let p = p.min(pts.len() - 1);
            Curve::bspline(p, clamped(pts.len(), p), pts).map_err(geo)
        }
    }
}

/// The exact patch (level coordinates) and how it was made.
fn build(by: &SurfaceBy, elev: f64) -> Result<(Surface, &'static str), Fail> {
    match by {
        SurfaceBy::Loft { curves, degree: d, params } => {
            if curves.len() < 2 {
                return Err(bad_value("a loft needs at least two curves"));
            }
            let cs = curves
                .iter()
                .enumerate()
                .map(|(k, c)| curve(c, elev).map_err(|e| e.at(format!("/curves/{k}"))))
                .collect::<Result<Vec<_>, _>>()?;
            let lf = match params {
                Some(p) => nurbs::loft_through(&cs, degree(*d)?, p),
                None => nurbs::loft(&cs, degree(*d)?),
            };
            Ok((lf.map_err(geo)?.surface, "loft"))
        }
        SurfaceBy::Ruled { a, b } => {
            let (a, b) = (curve(a, elev)?, curve(b, elev)?);
            Ok((nurbs::ruled(&a, &b).map_err(geo)?, "ruled"))
        }
        SurfaceBy::Revolve { profile, axis_a, axis_b, angle } => {
            let c = curve(profile, elev)?;
            let (o, b) = (pt(axis_a, elev), pt(axis_b, elev));
            let axis = b - o;
            if !(axis.len() > 1e-9) {
                return Err(bad_value("revolve axis has zero length"));
            }
            Ok((nurbs::revolve(&c, o, axis, angle.to_rad()).map_err(geo)?, "revolve"))
        }
        SurfaceBy::Points { rows, degree_u, degree_v, weights } => {
            let nu = rows.len();
            let nv = rows.first().map_or(0, Vec::len);
            if nu < 2 || nv < 2 || rows.iter().any(|r| r.len() != nv) {
                return Err(bad_value("points need at least 2 rows of the same length (2 or more)"));
            }
            let w: Vec<f64> = match weights {
                None => vec![1.0; nu * nv],
                Some(ws) if ws.len() == nu && ws.iter().all(|r| r.len() == nv) => {
                    ws.iter().flatten().copied().collect()
                }
                Some(_) => return Err(bad_value("weights must have the shape of rows")),
            };
            let (pu, pv) = (degree(*degree_u)?.min(nu - 1), degree(*degree_v)?.min(nv - 1));
            let ctrl = rows.iter().flatten().map(|p| pt(p, elev)).collect();
            let s = Surface::new(pu, pv, clamped(nu, pu), clamped(nv, pv), nu, nv, ctrl, w)
                .map_err(geo)?;
            Ok((s, "points"))
        }
        SurfaceBy::Network { u_curves, v_curves, degree: d, tolerance } => {
            let family = |cs: &[Curve3], key: &str| {
                cs.iter()
                    .enumerate()
                    .map(|(k, c)| curve(c, elev).map_err(|e| e.at(format!("/{key}/{k}"))))
                    .collect::<Result<Vec<_>, _>>()
            };
            let (us, vs) = (family(u_curves, "u_curves")?, family(v_curves, "v_curves")?);
            let tol = tolerance.map_or(DEFAULT_TOL, |t| t.to_m());
            if !(tol > 0.0) {
                return Err(bad_value("network tolerance must be > 0"));
            }
            let n = nurbs::network(&us, &vs, degree(*d)?, tol)
                .map_err(|e| bad_value(format!("freeform: {e}")))?;
            Ok((n.surface, "network"))
        }
        SurfaceBy::Dome { center, radius, rise } => {
            let s = nurbs::dome(pt(center, elev), radius.to_m(), rise.to_m())
                .map_err(|_| bad_value("dome: radius and rise must be > 0 with the rise at most the radius"))?;
            Ok((s, "dome"))
        }
        SurfaceBy::Vault { a, b, span, rise } => {
            let s = nurbs::vault(pt(a, elev), pt(b, elev), span.to_m(), rise.to_m()).map_err(|_| {
                bad_value("vault: a level axis of non-zero length, and a rise of at most half the span")
            })?;
            Ok((s, "vault"))
        }
    }
}

/// Default gap and deviation of a curve network, m.
const DEFAULT_TOL: f64 = 1e-3;

fn elev_of(m: &Model, level: i64) -> Result<f64, Fail> {
    Ok(level_row(m, level)?.1)
}

pub(super) fn lower(m: &mut Model, run: &mut Run, g: &GeoOp) -> Result<(), Fail> {
    match g {
        GeoOp::Freeform { slot, level, surface, thickness, side, trim, material, name } => {
            let level = match level {
                Some(r) => resolve_as(m, run, r, Cat::Level).map_err(|e| e.at("/level"))?,
                None => return Err(bad_value("geometry.freeform needs `level`")),
            };
            let (s, origin) = build(surface, elev_of(m, level)?).map_err(|e| e.at("/surface"))?;
            let spec = FreeformSpec {
                level,
                name: name.clone().unwrap_or_else(|| "Freeform".into()),
                material: material.clone().filter(|m| !m.trim().is_empty()).unwrap_or_else(|| {
                    crate::material_look::default_material("freeform", name.as_deref().unwrap_or("")).into()
                }),
                surface: s,
                thickness: thickness.map_or(0.0, |t| t.to_m()),
                side: side.map_or(ShellSide::Normal, side_of),
                trim: trim.as_ref().map(trim_of).unwrap_or_default(),
                origin: origin.into(),
            };
            let id = m.add_freeform(&spec)?;
            run.made(*slot, Cat::Freeform, id);
        }
        GeoOp::EditPoint { target, i, j, to } => {
            let id = resolve_as(m, run, target, Cat::Freeform)?;
            let elev = elev_of(m, m.freeform(id)?.level)?;
            m.move_freeform_point(id, *i as usize, *j as usize, pt(to, elev))?;
            run.receipt.changed.push(Ref::id(Cat::Freeform, id));
        }
        GeoOp::Thicken { target, thickness, side } => {
            let id = resolve_as(m, run, target, Cat::Freeform)?;
            let keep = m.freeform(id)?.side;
            m.set_freeform_shell(id, thickness.to_m(), side.map_or(keep, side_of))?;
            run.receipt.changed.push(Ref::id(Cat::Freeform, id));
        }
        GeoOp::Trim { target, trim } => {
            let id = resolve_as(m, run, target, Cat::Freeform)?;
            m.set_freeform_trim(id, trim.as_ref().map(trim_of).unwrap_or_default())?;
            run.receipt.changed.push(Ref::id(Cat::Freeform, id));
        }
        GeoOp::Rebuild { target, count_u, count_v, degree_u, degree_v } => {
            let id = resolve_as(m, run, target, Cat::Freeform)?;
            let mut spec = m.freeform(id)?;
            let rational = spec.surface.weights().iter().any(|w| (w - 1.0).abs() > 1e-12);
            let (du, dv) = (degree(*degree_u)?, degree(*degree_v)?);
            let r = nurbs::rebuild(&spec.surface, *count_u as usize, *count_v as usize, du, dv)
                .map_err(|e| bad_value(format!("freeform: {e}")))?;
            spec.surface = r.surface;
            m.update_freeform(id, &spec)?;
            run.receipt.changed.push(Ref::id(Cat::Freeform, id));
            run.receipt.loss.push(format!(
                "rebuild: new {count_u} x {count_v} control net deviates up to {:.3} mm",
                r.deviation * 1000.0
            ));
            if rational {
                run.receipt.loss.push("rebuild: weights dropped (the new net is polynomial)".into());
            }
        }
        GeoOp::Panelize {
            slot,
            host,
            columns,
            rows,
            pattern,
            planarize,
            mullion,
            glass,
            flat,
            cold,
            material,
            name,
        } => {
            let hid = resolve_as(m, run, host, Cat::Freeform).map_err(|e| e.at("/host"))?;
            let elev = elev_of(m, m.freeform(hid)?.level)?;
            let cols = spacing(columns).map_err(|e| e.at("/columns"))?;
            let rows = match rows {
                PanelDiv::Heights { heights } => Rows::Levels(heights.iter().map(|h| h.to_m() - elev).collect()),
                d => Rows::Spacing(spacing(d).map_err(|e| e.at("/rows"))?),
            };
            let def = PanelLimits::default();
            let limits = PanelLimits {
                flat: flat.map_or(def.flat, |x| x.to_m()),
                cold: cold.map_or(def.cold, |x| x.to_m()),
                cluster: def.cluster,
            };
            let pattern = match pattern.unwrap_or(PanelPattern::Quad) {
                PanelPattern::Quad => Pattern::Quad,
                PanelPattern::Triangle => Pattern::Triangle,
                PanelPattern::Diamond => Pattern::Diamond,
            };
            let spec = PanelSystemSpec {
                host: hid,
                name: name.clone().unwrap_or_else(|| "Curtain system".into()),
                // the frames: the panes are glass whatever the material
                material: material.clone().filter(|m| !m.trim().is_empty()).unwrap_or_else(|| {
                    crate::material_look::default_material("mullion", name.as_deref().unwrap_or("")).into()
                }),
                layout: PanelLayout { cols, rows, pattern, limits, planarize: planarize.unwrap_or(0) },
                mullion: mullion.as_ref().map_or(DEFAULT_MULLION, |x| [x.width.to_m(), x.depth.to_m()]),
                glass: glass.map_or(DEFAULT_GLASS, |g| g.to_m()),
            };
            let id = m.add_panel_system(&spec)?;
            run.made(*slot, Cat::PanelSystem, id);
            run.receipt.loss.extend(m.panel_system_geom(id)?.loss);
        }
        GeoOp::SurfaceShell { slot, host, kind, layers, side, openings, name } => {
            let h = resolve_as(m, run, host, Cat::Freeform).map_err(|e| e.at("/host"))?;
            let layers = layers
                .iter()
                .enumerate()
                .map(|(k, l)| crate::ShellLayer {
                    name: l.name.clone().unwrap_or_else(|| format!("Layer {}", k + 1)),
                    material: l.material.clone().unwrap_or_default(),
                    thickness: l.thickness.to_m(),
                })
                .collect();
            let openings = openings
                .iter()
                .flatten()
                .map(|o| o.iter().map(|p| { let [x, y] = p.to_m(); [x, y] }).collect())
                .collect();
            let kind = match kind {
                OpShellKind::Roof => crate::ShellKind::Roof,
                OpShellKind::Slab => crate::ShellKind::Slab,
            };
            let label = name.clone().unwrap_or_default();
            let spec = m.shell_on_host(h, kind, layers, side.map_or(ShellSide::Normal, side_of), openings, &label)?;
            let id = m.add_shell_elem(&spec)?;
            run.made(*slot, Cat::SurfaceShell, id);
        }
        _ => return Err(unsupported("not a freeform op")),
    }
    Ok(())
}

/// Mullion width and depth and glass thickness when the op leaves them out, m.
const DEFAULT_MULLION: [f64; 2] = [0.06, 0.15];
const DEFAULT_GLASS: f64 = 0.024;

fn spacing(d: &PanelDiv) -> Result<Spacing, Fail> {
    Ok(match d {
        PanelDiv::Count { n } => Spacing::Params(*n as usize),
        PanelDiv::Equal { n } => Spacing::Equal(*n as usize),
        PanelDiv::Width { width } => Spacing::Width(width.to_m()),
        PanelDiv::Heights { .. } => return Err(bad_value("heights divide rows only")),
    })
}

/// `geometry.convert` of a freeform patch: to a plain mesh only, with the loss listed.
pub(super) fn to_mesh(m: &mut Model, run: &mut Run, id: i64, to: ConvertTo) -> Result<(), Fail> {
    if to != ConvertTo::Mesh {
        return Err(unsupported("a freeform surface converts to a mesh only"));
    }
    let (new, loss) = m.freeform_to_mesh(id)?;
    run.receipt.changed.push(Ref::id(Cat::Mesh, new));
    run.receipt.deleted.push(Ref::id(Cat::Freeform, id));
    run.receipt.loss.extend(loss);
    Ok(())
}
