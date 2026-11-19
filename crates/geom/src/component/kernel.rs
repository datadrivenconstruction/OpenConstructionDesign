//! Solid-kernel adapter for the family editor: an operation is a `gsolid::Form` body in a work plane
//! `gsolid::Frame` (extrude, blend, revolve, sweep along a path, blended sweep;
//! profiles with arcs and holes). The preview is built by the solid kernel itself; the family
//! definition string is a translation of the same operation. Anything the definition language cannot express (holes, blended
//! sweep, tilted work plane, revolve not starting at zero azimuth) is an explicit
//! "unsupported" error rather than a silently different body.
//! Profile as JSON: `{"closed": true, "pts": [[x, y, bulge], …], "holes": [[[x, y, bulge], …]]}`
//! (bulge = tan(arc angle / 4) for the segment from a point to the next one).

use super::{expr, Family, Lod};
use crate::arc::BulgeRing;
use crate::gsolid::{self, Form, Frame, Path3, Section};
use crate::math::{v2, v3, V2, V3};
use crate::mesh::Mesh;
use crate::profile::signed_area;

#[derive(Clone, Debug, PartialEq)]
pub struct BulgeProfile {
    /// (x, y, bulge).
    pub pts: Vec<[f64; 3]>,
    pub closed: bool,
    /// Holes (each is a ring of (x, y, bulge)).
    pub holes: Vec<Vec<[f64; 3]>>,
}

/// Editor operation: a kernel body in a work plane.
#[derive(Clone, Debug, PartialEq)]
pub struct SolidOp {
    pub form: Form,
    pub frame: Frame,
}

pub trait Kernel {
    /// Mesh of the operation body (for the editor preview).
    fn mesh(&self, op: &SolidOp, lod: Lod) -> Result<Mesh, String>;
    /// Family definition string that builds the same body (or "unsupported").
    fn statement(&self, name: &str, op: &SolidOp, lod: Lod) -> Result<String, String>;
}

/// Built-in implementation on top of `gsolid`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Own;

/// Mini-JSON node: a number or an array.
enum J {
    N(f64),
    A(Vec<J>),
}

fn parse_j(s: &str, depth: usize) -> Result<(J, &str), String> {
    if depth > 8 {
        return Err("profile: nesting too deep".into());
    }
    let s = s.trim_start();
    if let Some(mut r) = s.strip_prefix('[') {
        let mut v = vec![];
        loop {
            r = r.trim_start();
            if let Some(rest) = r.strip_prefix(']') {
                return Ok((J::A(v), rest));
            }
            if !v.is_empty() {
                r = r.strip_prefix(',').ok_or("profile: expected ',' or ']'")?;
            }
            let (x, rest) = parse_j(r, depth + 1)?;
            v.push(x);
            r = rest;
        }
    }
    let end = s
        .find(|c: char| c == ',' || c == ']' || c.is_whitespace())
        .unwrap_or(s.len());
    let t = &s[..end];
    let x = t.parse::<f64>().map_err(|_| format!("bad number '{t}'"))?;
    Ok((J::N(x), &s[end..]))
}

fn ring_of(j: &J) -> Result<Vec<[f64; 3]>, String> {
    let J::A(pts) = j else {
        return Err("profile: expected an array of points".into());
    };
    pts.iter()
        .map(|p| match p {
            J::A(n) => {
                let v: Vec<f64> = n
                    .iter()
                    .map(|x| match x {
                        J::N(x) => Ok(*x),
                        J::A(_) => Err("point coordinates must be numbers".to_string()),
                    })
                    .collect::<Result<_, _>>()?;
                match v.len() {
                    2 => Ok([v[0], v[1], 0.0]),
                    3 => Ok([v[0], v[1], v[2]]),
                    k => Err(format!("point needs 2 or 3 numbers, got {k}")),
                }
            }
            J::N(_) => Err("profile: expected [x, y] or [x, y, bulge]".into()),
        })
        .collect()
}

fn array_after<'a>(t: &'a str, key: &str) -> Option<&'a str> {
    let k = t.find(key)?;
    let r = &t[k + key.len()..];
    r.find('[').map(|a| &r[a..])
}

impl BulgeProfile {
    /// JSON parsing: an object with `pts` (and `closed`, `holes`) or a bare array of points.
    pub fn from_json(s: &str) -> Result<Self, String> {
        let t = s.trim();
        let closed = !t.contains("\"closed\"") || {
            let k = t.find("\"closed\"").unwrap_or(0);
            let rest = t[k + 8..].trim_start().trim_start_matches(':').trim_start();
            rest.starts_with("true")
        };
        let arr = match array_after(t, "\"pts\"") {
            Some(a) => a,
            None if t.starts_with('[') => t,
            None => return Err("profile: expected {\"pts\": [...]} or [...]".into()),
        };
        let pts = ring_of(&parse_j(arr, 0)?.0)?;
        let holes = match array_after(t, "\"holes\"") {
            Some(a) => match parse_j(a, 0)?.0 {
                J::A(hs) => hs.iter().map(ring_of).collect::<Result<_, _>>()?,
                J::N(_) => return Err("holes: expected an array of rings".into()),
            },
            None => vec![],
        };
        let p = BulgeProfile { pts, closed, holes };
        p.check()?;
        Ok(p)
    }

    pub fn to_json(&self) -> String {
        let ring = |r: &[[f64; 3]]| {
            let v: Vec<String> = r
                .iter()
                .map(|p| format!("[{:?}, {:?}, {:?}]", p[0], p[1], p[2]))
                .collect();
            format!("[{}]", v.join(", "))
        };
        let mut s = format!(
            "{{\"closed\": {}, \"pts\": {}",
            self.closed,
            ring(self.pts.as_slice())
        );
        if !self.holes.is_empty() {
            let h: Vec<String> = self.holes.iter().map(|r| ring(r.as_slice())).collect();
            s += &format!(", \"holes\": [{}]", h.join(", "));
        }
        s + "}"
    }

    pub fn check(&self) -> Result<(), String> {
        let few = |r: &[[f64; 3]]| r.len() < 2 || (r.len() < 3 && r.iter().all(|p| p[2] == 0.0));
        if few(self.pts.as_slice()) || self.holes.iter().any(|h| few(h.as_slice())) {
            return Err("profile needs at least 3 points (or 2 with an arc)".into());
        }
        let all = || self.pts.iter().chain(self.holes.iter().flatten());
        if all().flatten().any(|v| !v.is_finite()) {
            return Err("profile: coordinates must be finite".into());
        }
        if all().any(|p| p[2].abs() > 1e3) {
            return Err("profile: bulge out of range".into());
        }
        Ok(())
    }

    fn ring(r: &[[f64; 3]]) -> BulgeRing {
        BulgeRing {
            pts: r.iter().map(|p| v2(p[0], p[1])).collect(),
            bulges: r.iter().map(|p| p[2]).collect(),
        }
    }

    /// Outer ring for the solid kernel (the body outline must be closed).
    pub fn to_ring(&self) -> Result<BulgeRing, String> {
        if !self.closed {
            return Err("solid profiles must be closed".into());
        }
        Ok(Self::ring(&self.pts))
    }

    /// Solid-kernel profile: outer ring and holes.
    pub fn section(&self) -> Result<Section, String> {
        Ok(Section {
            outer: self.to_ring()?,
            holes: self.holes.iter().map(|h| Self::ring(h)).collect(),
        })
    }
}

fn lit(v: f64) -> String {
    expr::literal(expr::q(v, expr::LENGTH))
}

/// Chord sagitta by level of detail (the medium one matches the view).
fn sag(lod: Lod) -> f64 {
    gsolid::VIEW_SAG * 24.0 / lod.segments() as f64
}

fn poly(pts: &[V2]) -> String {
    let v: Vec<String> = pts
        .iter()
        .map(|q| format!("{}, {}", lit(q.x), lit(q.y)))
        .collect();
    format!("poly({})", v.join(", "))
}

fn unsupported(what: &str) -> String {
    format!("unsupported in the family definition: {what}")
}

/// World axis parallel to `v`: (index 0..3, sign).
fn world_axis(v: V3) -> Option<(usize, f64)> {
    [v.x, v.y, v.z]
        .iter()
        .position(|c| (c.abs() - 1.0).abs() < 1e-9)
        .map(|k| (k, [v.x, v.y, v.z][k].signum()))
}

fn comp(v: V3, k: usize) -> f64 {
    [v.x, v.y, v.z][k]
}

fn ccw(r: &BulgeRing, s: f64) -> Vec<V2> {
    let mut v = gsolid::tess(r, s);
    if signed_area(&v) < 0.0 {
        v.reverse();
    }
    v
}

/// Polyline path in world space: link arcs are expanded in the xy projection, z linearly.
fn path_points(p: &Path3, fr: &Frame, s: f64) -> Vec<V3> {
    let mut out = vec![];
    for (i, w) in p.pts.windows(2).enumerate() {
        let (a, b) = (w[0], w[1]);
        let bulge = p.bulges.get(i).copied().unwrap_or(0.0);
        let ring = BulgeRing {
            pts: vec![v2(a.x, a.y), v2(b.x, b.y)],
            bulges: vec![bulge, 0.0],
        };
        // [a, arc…, b]: b is the start of the next link
        let xy = gsolid::tess(&ring, s);
        let m = xy.len().max(2) - 1;
        for (j, q) in xy.iter().take(m).enumerate() {
            let z = a.z + (b.z - a.z) * j as f64 / m as f64;
            out.push(fr.at(v3(q.x, q.y, z)));
        }
    }
    if let Some(l) = p.pts.last() {
        out.push(fr.at(*l));
    }
    out
}

impl Own {
    fn translate(name: &str, op: &SolidOp, s: f64) -> Result<String, String> {
        let fr = &op.frame;
        let at = |p: V2, z: f64| fr.at(v3(p.x, p.y, z));
        Ok(match &op.form {
            Form::Extrusion {
                profile,
                start,
                end,
            } => {
                if !profile.holes.is_empty() {
                    return Err(unsupported("extrusion with holes (add a void solid)"));
                }
                let (k, sg) = world_axis(fr.z)
                    .ok_or_else(|| unsupported("work plane not aligned with a world axis"))?;
                let pts: Vec<V2> = gsolid::tess(&profile.outer, s)
                    .iter()
                    .map(|p| {
                        let w = at(*p, 0.0);
                        match k {
                            0 => v2(w.y, w.z),
                            1 => v2(w.x, w.z),
                            _ => v2(w.x, w.y),
                        }
                    })
                    .collect();
                let base = comp(fr.o, k);
                let (a, b) = (base + sg * start, base + sg * end);
                let dir = ["dir(x) ", "dir(y) ", ""][k];
                format!(
                    "extrude {name} {} {dir}z({}, {})",
                    poly(&pts),
                    lit(a.min(b)),
                    lit(a.max(b))
                )
            }
            Form::Blend {
                bottom,
                top,
                height,
            } => {
                let Some((2, sg)) = world_axis(fr.z) else {
                    return Err(unsupported("blend on a non-horizontal work plane"));
                };
                let (a, b) = gsolid::correspond(&ccw(bottom, s), &ccw(top, s));
                let lift = |v: &[V2], z: f64| -> Vec<V2> {
                    v.iter()
                        .map(|p| {
                            let w = at(*p, z);
                            v2(w.x, w.y)
                        })
                        .collect()
                };
                let (mut lo, mut hi) = (lift(&a, 0.0), lift(&b, *height));
                let (mut z0, mut z1) = (fr.o.z, fr.o.z + sg * height);
                if z1 < z0 {
                    std::mem::swap(&mut lo, &mut hi);
                    std::mem::swap(&mut z0, &mut z1);
                }
                format!(
                    "blend {name} {} {} z({}, {})",
                    poly(&lo),
                    poly(&hi),
                    lit(z0),
                    lit(z1)
                )
            }
            Form::Revolve {
                profile,
                axis_o,
                axis_d,
                start,
                end,
            } => {
                if !profile.holes.is_empty() {
                    return Err(unsupported("revolve with holes"));
                }
                let ad = fr.dir(v3(axis_d.x, axis_d.y, 0.0)).normalized();
                let Some((2, sa)) = world_axis(ad) else {
                    return Err(unsupported("revolve about a non-vertical axis"));
                };
                let w0 = at(*axis_o, 0.0);
                let world: Vec<V3> = gsolid::tess(&profile.outer, s)
                    .iter()
                    .map(|p| at(*p, 0.0))
                    .collect();
                let off = |p: &V3| v2(p.x - w0.x, p.y - w0.y);
                let far = world
                    .iter()
                    .map(off)
                    .max_by(|a, b| a.len().total_cmp(&b.len()))
                    .filter(|h| h.len() > 1e-12)
                    .ok_or("revolve: profile lies on the axis")?;
                let u = far * (1.0 / far.len());
                let phi0 = u.y.atan2(u.x);
                // right-handed rotation about the local axis; reversed for a mirrored plane
                let ccw_world = sa * fr.det().signum() > 0.0;
                let from = if ccw_world { phi0 + start } else { phi0 - end };
                let th = end - start;
                let full = th >= std::f64::consts::TAU - 1e-9;
                if !full && ddc_solver::wrap_angle(from).abs() > 1e-9 {
                    return Err(unsupported(
                        "revolve must start at azimuth 0 (+x from the axis); rotate the work plane",
                    ));
                }
                let rz: Vec<V2> = world
                    .iter()
                    .map(|p| {
                        let h = off(p);
                        v2(h.x * u.x + h.y * u.y, p.z)
                    })
                    .collect();
                format!(
                    "revolve {name} {} axis({}, {}) angle({})",
                    poly(&rz),
                    lit(w0.x),
                    lit(w0.y),
                    expr::literal(expr::q(th.min(std::f64::consts::TAU), expr::ANGLE))
                )
            }
            Form::Sweep { profile, path } => {
                if !profile.holes.is_empty() {
                    return Err(unsupported("sweep with holes"));
                }
                if world_axis(fr.z) != Some((2, 1.0)) || fr.det() < 0.0 {
                    return Err(unsupported("sweep on a work plane other than a level one"));
                }
                let p: Vec<String> = path_points(path, fr, s)
                    .iter()
                    .map(|q| format!("{}, {}, {}", lit(q.x), lit(q.y), lit(q.z)))
                    .collect();
                let prof = gsolid::tess(&profile.outer, s);
                format!("sweep {name} {} path({})", poly(&prof), p.join(", "))
            }
            Form::SweptBlend { .. } => return Err(unsupported("swept blend")),
        })
    }
}

impl Kernel for Own {
    fn statement(&self, name: &str, op: &SolidOp, lod: Lod) -> Result<String, String> {
        if !name.chars().all(|c| c.is_alphanumeric() || c == '_') || name.is_empty() {
            return Err(format!("bad solid name '{name}'"));
        }
        if !op.frame.is_valid() {
            return Err("work plane axes must be orthonormal".into());
        }
        gsolid::validate(&op.form).map_err(|e| e.to_string())?;
        Self::translate(name, op, sag(lod))
    }

    fn mesh(&self, op: &SolidOp, lod: Lod) -> Result<Mesh, String> {
        if !op.frame.is_valid() {
            return Err("work plane axes must be orthonormal".into());
        }
        gsolid::mesh(&op.form, &op.frame, sag(lod)).map_err(|e| e.to_string())
    }
}

/// Body of the definition string (translation check, text preview).
pub fn statement_mesh(st: &str, lod: Lod) -> Result<Mesh, String> {
    let f = Family::parse(&format!("family Preview\n{st}\n")).map_err(|e| e.to_string())?;
    let r = f.resolve(&[], &[]).map_err(|e| e.to_string())?;
    Ok(f.build(&r, lod).map_err(|e| e.to_string())?.mesh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Section {
        Section::polygon(vec![v2(x0, y0), v2(x1, y0), v2(x1, y1), v2(x0, y1)])
    }

    /// The kernel mesh volume and the volume of the body from the definition string agree.
    fn same_volume(op: &SolidOp, want: f64) {
        let a = Own.mesh(op, Lod::Fine).unwrap().volume().abs();
        let st = Own.statement("S", op, Lod::Fine).unwrap();
        let b = statement_mesh(&st, Lod::Fine).unwrap().volume().abs();
        assert!((a - want).abs() < 0.01 * want, "kernel {a} vs {want}: {st}");
        assert!(
            (b - want).abs() < 0.01 * want,
            "definition {b} vs {want}: {st}"
        );
        let ka = crate::gsolid::bbox(&Own.mesh(op, Lod::Fine).unwrap());
        let kb = crate::gsolid::bbox(&statement_mesh(&st, Lod::Fine).unwrap());
        assert!(
            (ka.0 - kb.0).len() < 5e-3 && (ka.1 - kb.1).len() < 5e-3,
            "{ka:?} vs {kb:?}: {st}"
        );
    }

    #[test]
    fn bulge_profile_json_round_trip_with_holes() {
        // 1×1 square with a semicircular top side (bulge 1 is a semicircle)
        let p = BulgeProfile::from_json(
            r#"{"closed": true, "pts": [[0, 0, 0], [1, 0, 0], [1, 1, 1], [0, 1, 0]],
               "holes": [[[0.4, 0.4], [0.6, 0.4], [0.6, 0.6], [0.4, 0.6]]]}"#,
        )
        .unwrap();
        assert_eq!((p.pts.len(), p.holes.len()), (4, 1));
        assert_eq!(BulgeProfile::from_json(&p.to_json()).unwrap(), p);
        let sec = p.section().unwrap();
        assert_eq!(sec.holes[0].pts.len(), 4);
        let tess = crate::gsolid::tess(&sec.outer, crate::gsolid::VIEW_SAG);
        assert!(tess.iter().any(|q| q.y > 1.4), "arc bulges outward");
        assert!(BulgeProfile::from_json("[[0,0],[1,0]]").is_err());
        assert!(BulgeProfile::from_json("[[0,0],[1,0],[x,1]]").is_err());
    }

    #[test]
    fn translations_build_the_kernel_body() {
        // extrusion in a vertical plane along x: dir(x)
        let ext = SolidOp {
            form: Form::Extrusion {
                profile: rect(0.0, 0.0, 1.0, 0.5),
                start: 0.0,
                end: 2.0,
            },
            frame: Frame::vertical(v3(0.0, 0.0, 0.0), v2(0.0, 1.0)),
        };
        assert!(Own
            .statement("S", &ext, Lod::Fine)
            .unwrap()
            .contains("dir(x)"));
        same_volume(&ext, 1.0);
        // blend square → square of half the size (prismatoid)
        let bl = SolidOp {
            form: Form::Blend {
                bottom: rect(0.0, 0.0, 1.0, 1.0).outer,
                top: rect(0.25, 0.25, 0.75, 0.75).outer,
                height: 1.0,
            },
            frame: Frame::level(0.1),
        };
        same_volume(&bl, (1.0 + 4.0 * 0.5625 + 0.25) / 6.0);
        // half-turn of an annular profile about the vertical through (1, 2)
        let rv = SolidOp {
            form: Form::Revolve {
                profile: rect(0.2, 0.0, 0.5, 1.0),
                axis_o: v2(0.0, 0.0),
                axis_d: v2(0.0, 1.0),
                start: 0.0,
                end: PI,
            },
            frame: Frame::vertical(v3(1.0, 2.0, 0.0), v2(1.0, 0.0)),
        };
        same_volume(&rv, 0.5 * PI * (0.25 - 0.04));
        // sweep of a 0.1 square along a polyline with a right angle
        let sw = SolidOp {
            form: Form::Sweep {
                profile: rect(-0.05, -0.05, 0.05, 0.05),
                path: Path3::polyline(vec![
                    v3(0.0, 0.0, 0.0),
                    v3(1.0, 0.0, 0.0),
                    v3(1.0, 1.0, 0.0),
                ]),
            },
            frame: Frame::default(),
        };
        same_volume(&sw, 0.02);
    }

    #[test]
    fn untranslatable_forms_are_explicit_errors() {
        let mut holed = rect(0.0, 0.0, 1.0, 1.0);
        holed.holes.push(rect(0.4, 0.4, 0.6, 0.6).outer);
        let op = SolidOp {
            form: Form::Extrusion {
                profile: holed,
                start: 0.0,
                end: 1.0,
            },
            frame: Frame::default(),
        };
        // the kernel builds a body with a hole; the translation honestly refuses
        let v = Own.mesh(&op, Lod::Fine).unwrap().volume().abs();
        assert!((v - 0.96).abs() < 1e-6, "{v}");
        assert!(Own
            .statement("S", &op, Lod::Fine)
            .unwrap_err()
            .contains("unsupported"));
        let sb = SolidOp {
            form: Form::SweptBlend {
                bottom: rect(-0.1, -0.1, 0.1, 0.1).outer,
                top: rect(-0.05, -0.05, 0.05, 0.05).outer,
                path: Path3::polyline(vec![v3(0.0, 0.0, 0.0), v3(1.0, 0.0, 0.0)]),
            },
            frame: Frame::default(),
        };
        assert!(Own
            .statement("S", &sb, Lod::Fine)
            .unwrap_err()
            .contains("swept blend"));
        let c = 0.5f64.sqrt();
        let tilted = SolidOp {
            form: Form::Extrusion {
                profile: rect(0.0, 0.0, 1.0, 1.0),
                start: 0.0,
                end: 1.0,
            },
            frame: Frame {
                o: v3(0.0, 0.0, 0.0),
                x: v3(1.0, 0.0, 0.0),
                y: v3(0.0, c, c),
                z: v3(0.0, -c, c),
            },
        };
        assert!(Own
            .statement("S", &tilted, Lod::Fine)
            .unwrap_err()
            .contains("unsupported"));
        let rv = SolidOp {
            form: Form::Revolve {
                profile: rect(0.2, 0.0, 0.5, 1.0),
                axis_o: v2(0.0, 0.0),
                axis_d: v2(0.0, 1.0),
                start: 0.5,
                end: PI,
            },
            frame: Frame::vertical(v3(0.0, 0.0, 0.0), v2(1.0, 0.0)),
        };
        assert!(Own
            .statement("S", &rv, Lod::Fine)
            .unwrap_err()
            .contains("azimuth"));
        assert!(Own.statement("bad name", &sb, Lod::Fine).is_err());
    }
}
