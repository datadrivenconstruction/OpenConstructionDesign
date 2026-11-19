//! Network bodies by part for 3D: run and fittings by outer wall, insulation and lining as
//! separate shells (the view draws them semi-transparent); section/elevation primitives.
//! The network is computed per system so the view rebuilds only the systems an edit touched.

use std::collections::HashMap;

use ddc_geom::math::{v2, v3, V2, V3};
use ddc_geom::mep::{self, FittingKind, Piece, Plane, Section, Shape};
use ddc_geom::mesh::Mesh;

use crate::{Error, LineClass, Model, ViewPrim};

/// A part of a network body.
#[derive(Clone, Debug, PartialEq)]
pub enum MepPart {
    Run,
    Fitting(FittingKind),
    Insulation,
    Lining,
}

#[derive(Clone, Debug)]
pub struct MepBody {
    pub run: i64,
    pub part: MepPart,
    pub mesh: Mesh,
}

/// Run for the view: system, level, system color.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RunInfo {
    pub system: i64,
    pub level: i64,
    pub color: [u8; 3],
}

/// Circle segment count: small sections are coarser (fewer triangles on a large model);
/// step ≤ 22.5° so the view does not draw crease edges along the generatrices.
pub fn segments(s: Shape) -> usize {
    let (w, h) = s.wh();
    if w.max(h) < 0.1 {
        16
    } else {
        24
    }
}

fn bodies(out: &mut Vec<MepBody>, run: i64, part: MepPart, s: &Section, p: &Piece) {
    let path = p.samples(8);
    let n = segments(s.envelope());
    out.push(MepBody {
        run,
        part,
        mesh: mep::sweep(s.outer(), &path, n),
    });
    if s.insulation > 1e-6 {
        out.push(MepBody {
            run,
            part: MepPart::Insulation,
            mesh: mep::sweep(s.envelope(), &path, n),
        });
    }
    if s.lining > 1e-6 && s.inner().valid() {
        out.push(MepBody {
            run,
            part: MepPart::Lining,
            mesh: mep::sweep(s.inner(), &path, n),
        });
    }
}

fn key(p: V3) -> (i64, i64, i64) {
    let q = |x: f64| (x * 1e4).round() as i64;
    (q(p.x), q(p.y), q(p.z))
}

impl Model {
    /// System, level and color of each run.
    pub fn mep_run_info(&self) -> Result<HashMap<i64, RunInfo>, Error> {
        let colors: HashMap<i64, [u8; 3]> = self
            .mep_systems()?
            .into_iter()
            .map(|s| (s.id, s.color))
            .collect();
        let mut st = self
            .db
            .prepare_cached("SELECT id, system, level FROM mep_run")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        let mut out = HashMap::new();
        for x in rows {
            let (id, system, level) = x?;
            let color = colors.get(&system).copied().unwrap_or([96, 104, 116]);
            out.insert(
                id,
                RunInfo {
                    system,
                    level,
                    color,
                },
            );
        }
        Ok(out)
    }

    /// Network bodies of a system (`None` means all systems) by part: straights and elbows by
    /// outer wall, tee/cross branches and caps, transitions; insulation and lining as shells.
    pub fn mep_bodies(&self, system: Option<i64>) -> Result<Vec<MepBody>, Error> {
        let net = self.mep_network(system)?;
        let mut out = vec![];
        for (run, p, s) in &net.pieces {
            let part = match p {
                Piece::Elbow { .. } => MepPart::Fitting(FittingKind::Elbow),
                Piece::Straight { .. } => MepPart::Run,
            };
            bodies(&mut out, *run, part, s, p);
        }
        let mut at: HashMap<(i64, i64, i64), Vec<usize>> = HashMap::new();
        for (i, f) in net.fittings.iter().enumerate() {
            at.entry(key(v3(f.at.0, f.at.1, f.at.2)))
                .or_default()
                .push(i);
        }
        for (run, p, s) in &net.fitting_pieces {
            let kind = at
                .get(&key(p.ends().0))
                .into_iter()
                .flatten()
                .map(|&i| &net.fittings[i])
                .find(|f| f.runs.contains(run))
                .map_or(FittingKind::Tee, |f| f.kind.clone());
            bodies(&mut out, *run, MepPart::Fitting(kind), s, p);
        }
        for (i, m) in &net.fitting_meshes {
            let f = &net.fittings[*i];
            if f.kind == FittingKind::Transition {
                out.push(MepBody {
                    run: f.runs[0],
                    part: MepPart::Fitting(FittingKind::Transition),
                    mesh: m.clone(),
                });
            }
        }
        Ok(out)
    }

    /// Network in section/elevation along line `a`→`u` (looking left, depth `depth`): plane
    /// sections of straight runs (wall in system color, insulation hatched) and projection outlines.
    pub fn mep_section_prims(
        &self,
        a: V2,
        u: V2,
        depth: Option<f64>,
        cut: bool,
    ) -> Result<Vec<ViewPrim>, Error> {
        if self.mep_runs(None)?.is_empty() {
            return Ok(vec![]);
        }
        let info = self.mep_run_info()?;
        let net = self.mep_network(None)?;
        let n = v2(-u.y, u.x);
        let plane = Plane {
            n: v3(n.x, n.y, 0.0),
            c: n.dot(a),
        };
        let sz = |p: V3| ((v2(p.x, p.y) - a).dot(u), p.z);
        let d = |p: V3| (v2(p.x, p.y) - a).dot(n);
        let far = depth.unwrap_or(f64::INFINITY);
        let mut out = vec![];
        let mut cut_runs = vec![];
        for (run, p, s) in net.pieces.iter().chain(&net.fitting_pieces) {
            let rgb = info.get(run).map_or([96, 104, 116], |i| i.color);
            let ns = segments(s.envelope());
            if cut {
                if let Some(r) = mep::section_cut(s.outer(), p, plane, ns) {
                    let hole = mep::section_cut(s.inner(), p, plane, ns)
                        .filter(|_| s.inner().valid())
                        .map(|h| h.into_iter().rev().map(sz).collect::<Vec<_>>());
                    out.push(ViewPrim::SectionCut {
                        table: "mep_run",
                        element: *run,
                        material: format!("mep#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]),
                        rings: r.iter().map(|&q| sz(q)).collect(),
                        holes: hole.into_iter().collect(),
                    });
                    if s.insulation > 1e-6 {
                        if let Some(e) = mep::section_cut(s.envelope(), p, plane, ns) {
                            out.push(ViewPrim::SectionCut {
                                table: "mep_run",
                                element: *run,
                                material: "insulation".into(),
                                rings: e.iter().map(|&q| sz(q)).collect(),
                                holes: vec![r.iter().rev().map(|&q| sz(q)).collect()],
                            });
                        }
                    }
                    cut_runs.push(*run);
                    continue;
                }
            }
            let path = p.samples(8);
            if path.iter().any(|(q, _)| d(*q) < -1e-9 || d(*q) > far) {
                continue;
            }
            let class = if cut {
                LineClass::Beyond
            } else {
                LineClass::Projection
            };
            out.extend(
                silhouette(s.envelope(), &path, ns, &sz)
                    .into_iter()
                    .map(|(a, b)| ViewPrim::Edge {
                        table: "mep_run",
                        element: *run,
                        class,
                        a,
                        b,
                    }),
            );
        }
        // a run with a section: its projection outlines are also section lines
        for q in &mut out {
            if let ViewPrim::Edge { element, class, .. } = q {
                if cut_runs.contains(element) {
                    *class = LineClass::Projection;
                }
            }
        }
        Ok(out)
    }
}

/// Projection outline of a piece onto the view plane: two extent lines along the axis; a piece
/// along the view direction yields its section outline.
fn silhouette(
    s: Shape,
    path: &[(V3, mep::Frame)],
    n: usize,
    sz: &dyn Fn(V3) -> (f64, f64),
) -> Vec<((f64, f64), (f64, f64))> {
    let ring = s.outline(n);
    let proj = |(p, f): &(V3, mep::Frame)| -> Vec<V2> {
        ring.iter()
            .map(|q| {
                let w = sz(*p + f.side * q.x + f.up * q.y);
                v2(w.0, w.1)
            })
            .collect()
    };
    let c: Vec<V2> = path
        .iter()
        .map(|(p, _)| {
            let w = sz(*p);
            v2(w.0, w.1)
        })
        .collect();
    let span = (c[c.len() - 1] - c[0]).len();
    let (wd, ht) = s.wh();
    if span < 0.25 * wd.max(ht) && path.len() == 2 {
        let r = proj(&path[0]);
        let t = |p: V2| (p.x, p.y);
        return (0..r.len())
            .map(|i| (t(r[i]), t(r[(i + 1) % r.len()])))
            .collect();
    }
    let mut lo = vec![];
    let mut hi = vec![];
    for (i, smp) in path.iter().enumerate() {
        let dir = if i + 1 < c.len() {
            c[i + 1] - c[i]
        } else {
            c[i] - c[i - 1]
        };
        let l = dir.len();
        let perp = if l > 1e-9 {
            v2(-dir.y / l, dir.x / l)
        } else {
            v2(0.0, 1.0)
        };
        let ts: Vec<f64> = proj(smp).iter().map(|q| (*q - c[i]).dot(perp)).collect();
        let (mn, mx) = ts.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |a, &t| {
            (a.0.min(t), a.1.max(t))
        });
        lo.push(c[i] + perp * mn);
        hi.push(c[i] + perp * mx);
    }
    let t = |p: V2| (p.x, p.y);
    let mut out = vec![];
    for side in [&lo, &hi] {
        for w in side.windows(2) {
            out.push((t(w[0]), t(w[1])));
        }
    }
    out.push((t(lo[0]), t(hi[0])));
    out.push((t(lo[lo.len() - 1]), t(hi[hi.len() - 1])));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RunSpec;
    use ddc_geom::mep::{Domain, SystemKind};

    fn model() -> (Model, i64, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_level("L1", 0.0).unwrap();
        let s = m.add_mep_system("Supply", SystemKind::SupplyAir).unwrap();
        (m, l, s)
    }

    #[test]
    fn duct_with_bend_insulation_and_tee_parts() {
        let (mut m, l, s) = model();
        let mut sec = Section::new(Domain::Duct, Shape::Rect { w: 0.4, h: 0.3 });
        sec.wall = 0.001;
        sec.insulation = 0.05;
        sec.lining = 0.02;
        let r = m
            .add_mep_run(&RunSpec::new(
                s,
                l,
                sec,
                vec![(0., 0., 3.), (5., 0., 3.), (5., 4., 3.)],
            ))
            .unwrap();
        let b = m
            .add_mep_run(&RunSpec::new(
                s,
                l,
                Section::new(Domain::Duct, Shape::Round { d: 0.2 }),
                vec![(2.0, 0.0, 3.0), (2.0, -3.0, 3.0)],
            ))
            .unwrap();
        let parts = m.mep_bodies(None).unwrap();
        let has = |run: i64, p: MepPart| parts.iter().any(|x| x.run == run && x.part == p);
        assert!(has(r, MepPart::Run) && has(r, MepPart::Insulation) && has(r, MepPart::Lining));
        assert!(has(r, MepPart::Fitting(FittingKind::Elbow)));
        assert!(
            parts
                .iter()
                .any(|x| x.part == MepPart::Fitting(FittingKind::Tee)),
            "tee legs"
        );
        assert!(has(b, MepPart::Run));
        for x in &parts {
            assert!(x.mesh.volume() > 0.0, "{:?} closed outward", x.part);
        }
        let info = m.mep_run_info().unwrap();
        assert_eq!(info[&r].color, SystemKind::SupplyAir.color());
        // section across the first leg: wall with a bore and insulation
        let pr = m
            .mep_section_prims(v2(1.0, 5.0), v2(0.0, -1.0), Some(20.0), true)
            .unwrap();
        let cuts = pr
            .iter()
            .filter(|p| matches!(p, ViewPrim::SectionCut { element, .. } if *element == r))
            .count();
        assert_eq!(cuts, 2, "wall ring + insulation ring");
        assert!(pr.iter().any(|p| matches!(p, ViewPrim::Edge { .. })));
    }

    #[test]
    fn run_moves_copies_and_undoes_as_target() {
        use crate::Target;
        let (mut m, l, _) = model();
        // a pipe needs a piped system (the air one from model() only carries ducts)
        let s = m.add_mep_system("CW", SystemKind::ColdWater).unwrap();
        let sec = Section::new(Domain::Pipe, Shape::Round { d: 0.05 });
        let r = m
            .add_mep_run(&RunSpec::new(s, l, sec, vec![(0., 0., 1.), (2., 0., 1.5)]))
            .unwrap();
        m.move_elements(&[Target::MepRun(r)], (1.0, 2.0)).unwrap();
        assert_eq!(
            m.mep_run(r).unwrap().points,
            vec![(1., 2., 1.), (3., 2., 1.5)]
        );
        let c = m
            .copy_elements(&[Target::MepRun(r)], &[(0.0, 1.0)])
            .unwrap();
        let Target::MepRun(n) = c[0] else { panic!() };
        assert_eq!(m.mep_run(n).unwrap().points[0], (1., 3., 1.));
        m.undo().unwrap();
        assert!(m.mep_run(n).is_err(), "copy undone");
        m.undo().unwrap();
        assert_eq!(m.mep_run(r).unwrap().points[0], (0., 0., 1.));
    }
}
