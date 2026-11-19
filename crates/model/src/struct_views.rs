//! Columns, beams and roofs in views. A level plan at a cut height: the cut is the section
//! outline (Cut), below it is the edge projection (Projection), above it is dashed over the cut
//! plane (Overhead; for a roof above the cut plane: its eaves, ridges, edges). A section along
//! line a→b (looking left): the cut by the plane and the element edges in the view depth (Beyond,
//! without hidden-line removal). Shapes match the solids of the same kind; elements cut by joints
//! are shown as the solid after the cuts.

use ddc_geom::mesh::Mesh;
use ddc_geom::{poly, v2, v3, V2, V3};

use crate::{Error, Model};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StructClass {
    Cut,
    Projection,
    Beyond,
    Overhead,
    /// Not visible below the level floor (foundations): dashed.
    Hidden,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StructPrim {
    /// column | beam | roof | mass | footing | strip_footing | raft | brace | truss | beam_system
    pub kind: &'static str,
    pub id: i64,
    pub class: StructClass,
    /// View coordinates: plan is (x, y); section is (u along a→b, z).
    pub pts: Vec<(f64, f64)>,
    pub closed: bool,
}

const FEATURE_COS: f64 = 0.966; // ≈15°: facets of a round section give no edges

fn line(
    kind: &'static str,
    id: i64,
    class: StructClass,
    a: (f64, f64),
    b: (f64, f64),
) -> StructPrim {
    StructPrim {
        kind,
        id,
        class,
        pts: vec![a, b],
        closed: false,
    }
}

/// A segment split by the plane z = zc: (below, above).
fn split_z(a: V3, b: V3, zc: f64) -> (Option<(V3, V3)>, Option<(V3, V3)>) {
    let (lo, hi) = if a.z <= b.z { (a, b) } else { (b, a) };
    if hi.z <= zc {
        return (Some((a, b)), None);
    }
    if lo.z >= zc {
        return (None, Some((a, b)));
    }
    let m = lo + (hi - lo) * ((zc - lo.z) / (hi.z - lo.z));
    (Some((lo, m)), Some((m, hi)))
}

fn dedup(mut v: Vec<((i64, i64), (i64, i64))>) -> Vec<((i64, i64), (i64, i64))> {
    for s in v.iter_mut() {
        if s.1 < s.0 {
            *s = (s.1, s.0);
        }
    }
    v.sort();
    v.dedup();
    v
}

/// Projection of edges onto the view with duplicates and degenerate ones removed.
fn project(
    out: &mut Vec<StructPrim>,
    kind: &'static str,
    id: i64,
    class: StructClass,
    segs: &[(V3, V3)],
    f: &dyn Fn(V3) -> (f64, f64),
) {
    let q = |p: (f64, f64)| ((p.0 * 1e6).round() as i64, (p.1 * 1e6).round() as i64);
    let keys: Vec<_> = segs
        .iter()
        .map(|s| (q(f(s.0)), q(f(s.1))))
        .filter(|k| k.0 != k.1)
        .collect();
    for (a, b) in dedup(keys) {
        let p = |k: (i64, i64)| (k.0 as f64 / 1e6, k.1 as f64 / 1e6);
        out.push(line(kind, id, class, p(a), p(b)));
    }
}

fn cut_loops(
    out: &mut Vec<StructPrim>,
    kind: &'static str,
    id: i64,
    m: &Mesh,
    n: V3,
    c: f64,
    f: &dyn Fn(V3) -> (f64, f64),
) -> bool {
    let segs: Vec<(V2, V2)> = poly::slice(m, n, c)
        .into_iter()
        .map(|(a, b)| {
            let (pa, pb) = (f(a), f(b));
            (v2(pa.0, pa.1), v2(pb.0, pb.1))
        })
        .collect();
    let loops = poly::chain(&segs, 1e-7);
    let any = !loops.is_empty();
    for r in loops {
        out.push(StructPrim {
            kind,
            id,
            class: StructClass::Cut,
            pts: r.iter().map(|p| (p.x, p.y)).collect(),
            closed: true,
        });
    }
    any
}

fn z_range(m: &Mesh) -> (f64, f64) {
    m.positions
        .iter()
        .fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p.z), b.max(p.z)))
}

impl Model {
    /// Bar and roof solids of a level (or of all levels when `level = None`): (kind, id, mesh, roof lines).
    #[allow(clippy::type_complexity)]
    pub(crate) fn struct_bodies(
        &self,
        level: Option<i64>,
    ) -> Result<Vec<(&'static str, i64, Mesh, Option<Vec<(V3, V3)>>)>, Error> {
        let mut out = vec![];
        let cut = self.cut_elements()?;
        let is_cut = |k: &str, id: i64| cut.iter().any(|c| c.0 == k && c.1 == id);
        let built = self.struct_build()?;
        for b in &built.members {
            if level.is_none_or(|l| l == b.level) {
                let k = if b.kind == "column" { "col" } else { b.kind };
                // a cut that fails (broken cutter history) shows the element uncut
                let mesh = match is_cut(k, b.id).then(|| self.joined_body(k, b.id)) {
                    Some(Ok(j)) => j.to_mesh(),
                    _ => b.m.mesh(),
                };
                out.push((b.kind, b.id, mesh, None));
            }
        }
        // one mesh per part: edges without seams between step meshes; a strip has its own lines
        for p in &built.parts {
            if level.is_none_or(|l| l == p.level) {
                out.push((
                    p.kind,
                    p.id,
                    ddc_geom::mep::merge(&p.meshes),
                    p.lines.clone(),
                ));
            }
        }
        let mut st = self.db.prepare("SELECT id, level FROM roof ORDER BY id")?;
        let roofs: Vec<(i64, i64)> = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        for (id, lv) in roofs {
            if level.is_some_and(|l| l != lv) {
                continue;
            }
            if is_cut("roof", id) {
                if let Ok(j) = self.joined_body("roof", id) {
                    out.push(("roof", id, j.to_mesh(), None));
                    continue;
                }
            }
            let g = self.roof_geometry(id)?;
            let lines = g.edges().into_iter().map(|e| (e.0, e.1)).collect();
            out.push(("roof", id, self.roof_mesh(id)?, Some(lines)));
        }
        // a form belongs to every storey it spans, not only to its own level
        let band = match level {
            Some(l) => {
                let lv: Vec<(i64, f64)> = self
                    .db
                    .prepare("SELECT id,elevation FROM level ORDER BY elevation,id")?
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<Result<_, _>>()?;
                let i = lv.iter().position(|x| x.0 == l);
                i.map(|i| (lv[i].1, lv.get(i + 1).map_or(f64::INFINITY, |x| x.1)))
            }
            None => None,
        };
        for id in self.masses()? {
            let s = self.mass(id)?;
            if s.void {
                continue;
            }
            let b = match is_cut("mass", id) {
                true => self.joined_body("mass", id).or_else(|_| self.mass_body(id)),
                false => self.mass_body(id),
            };
            // a broken history is reported by warnings, not by failing every view
            let Ok(b) = b else {
                continue;
            };
            if let (Some(l), Some((lo, hi))) = (level, band) {
                let (z0, z1) = b
                    .polys
                    .iter()
                    .flat_map(|p| p.pts.iter())
                    .fold((f64::MAX, f64::MIN), |(a, c), p| (a.min(p.z), c.max(p.z)));
                if s.level != l && !(z1 > lo + 1e-9 && z0 < hi - 1e-9) {
                    continue;
                }
            } else if level.is_some_and(|l| l != s.level) {
                continue;
            }
            let lines = b.feature_edges(1e-7, FEATURE_COS);
            out.push(("mass", id, b.to_mesh(), Some(lines)));
        }
        out.extend(self.roof_edge_bodies(level)?);
        Ok(out)
    }

    /// Level plan: cut plane at `cut_height` above the level elevation.
    pub fn struct_plan(&self, level: i64, cut_height: f64) -> Result<Vec<StructPrim>, Error> {
        let elev: f64 = self
            .db
            .query_row("SELECT elevation FROM level WHERE id=?1", [level], |r| {
                r.get(0)
            })
            .map_err(|_| Error::Invalid(format!("no such level {level}")))?;
        let zc = elev + cut_height;
        let f = |p: V3| (p.x, p.y);
        let mut out = vec![];
        for (kind, id, mesh, lines) in self.struct_bodies(Some(level))? {
            let (z0, z1) = z_range(&mesh);
            if z0 < zc && z1 > zc {
                cut_loops(&mut out, kind, id, &mesh, v3(0.0, 0.0, 1.0), zc, &f);
            }
            let edges = match lines {
                Some(l) => l,
                None => poly::feature_edges(&mesh, FEATURE_COS),
            };
            let (mut below, mut above) = (vec![], vec![]);
            for (a, b) in edges {
                let (lo, hi) = split_z(a, b, zc);
                below.extend(lo);
                above.extend(hi);
            }
            // a foundation below the floor gives hidden lines; for a slab only the top is visible
            let found = matches!(kind, "footing" | "strip_footing" | "raft");
            let (hidden, below): (Vec<_>, Vec<_>) = below
                .into_iter()
                .partition(|(a, b)| found && (kind != "raft" || a.z.max(b.z) < elev - 1e-6));
            project(&mut out, kind, id, StructClass::Hidden, &hidden, &f);
            // a column in section: the cut only
            if !(kind == "column" && z0 < zc && z1 > zc) {
                project(&mut out, kind, id, StructClass::Projection, &below, &f);
                project(&mut out, kind, id, StructClass::Overhead, &above, &f);
            }
        }
        Ok(out)
    }

    /// Section along line a→b, looking left, with depth `depth`.
    pub fn struct_section(
        &self,
        a: (f64, f64),
        b: (f64, f64),
        depth: f64,
    ) -> Result<Vec<StructPrim>, Error> {
        let (pa, pb) = (v2(a.0, a.1), v2(b.0, b.1));
        let l = (pb - pa).len();
        if l < 1e-9 {
            return Err(Error::Invalid("section line has zero length".into()));
        }
        let d = (pb - pa) * (1.0 / l);
        let nrm = v2(-d.y, d.x);
        let n3 = v3(nrm.x, nrm.y, 0.0);
        let c = nrm.dot(pa);
        let f = move |p: V3| ((v2(p.x, p.y) - pa).dot(d), p.z);
        let mut out = vec![];
        for (kind, id, mesh, lines) in self.struct_bodies(None)? {
            let w: Vec<f64> = mesh.positions.iter().map(|p| n3.dot(*p) - c).collect();
            let (w0, w1) = w
                .iter()
                .fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
            if w1 < 0.0 || w0 > depth {
                continue;
            }
            if w0 < 0.0 && w1 > 0.0 {
                cut_loops(&mut out, kind, id, &mesh, n3, c, &f);
            }
            let edges = match lines {
                Some(l) => l,
                None => poly::feature_edges(&mesh, FEATURE_COS),
            };
            let seen: Vec<(V3, V3)> = edges
                .into_iter()
                .filter(|(p, q)| {
                    let (u, v) = (n3.dot(*p) - c, n3.dot(*q) - c);
                    u >= -1e-9 && v >= -1e-9 && u <= depth && v <= depth
                })
                .collect();
            project(&mut out, kind, id, StructClass::Beyond, &seen, &f);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use crate::Model;
    use crate::{BeamJoin, BeamSpec, ColumnSpec, Just, LayerSpec, RoofKind, RoofSpec, StructClass};

    #[test]
    fn plan_cut_projection_overhead_and_section() {
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_level("L0", 0.0).unwrap();
        let ct = m.add_catalog_member_type("R 400x400").unwrap();
        let c = m
            .add_column(&ColumnSpec {
                level: l,
                top_level: None,
                member_type: ct,
                structural: true,
                at: (0.0, 0.0),
                base_offset: 0.0,
                top_offset: 3.0,
                rotation: 0.0,
                top_at: None,
            })
            .unwrap();
        let bt = m.add_catalog_member_type("IPE 300").unwrap();
        let b = m
            .add_beam(&BeamSpec {
                level: l,
                member_type: bt,
                a: (0.0, 0.0, 3.0),
                b: (6.0, 0.0, 3.0),
                bulge: 0.0,
                yjust: Just::Center,
                zjust: Just::Pos,
                rotation: 0.0,
                start_ext: 0.0,
                end_ext: 0.0,
                join_start: BeamJoin::Auto,
                join_end: BeamJoin::Auto,
            })
            .unwrap();
        let st = m
            .add_slab_type(
                "R",
                &[LayerSpec {
                    material: "Tiles".into(),
                    thickness: 0.1,
                }],
            )
            .unwrap();
        let r = m
            .add_roof(&RoofSpec {
                level: l,
                slab_type: st,
                offset: 3.0,
                overhang: 0.0,
                fascia: 0.0,
                kind: RoofKind::Footprint {
                    ring: vec![(-1.0, -3.0), (9.0, -3.0), (9.0, 3.0), (-1.0, 3.0)],
                    slopes: vec![Some(30.0); 4],
                    mansard: None,
                },
            })
            .unwrap();
        let p = m.struct_plan(l, 1.2).unwrap();
        let cut: Vec<_> = p.iter().filter(|x| x.class == StructClass::Cut).collect();
        assert_eq!(cut.len(), 1);
        assert_eq!(cut[0].id, c);
        let area = ddc_geom::poly::area(
            &cut[0]
                .pts
                .iter()
                .map(|q| ddc_geom::v2(q.0, q.1))
                .collect::<Vec<_>>(),
        );
        assert!((area.abs() - 0.16).abs() < 1e-12);
        // a beam above the cut plane is dashed; roof: 4 eaves + ridge + 4 dashed edges
        assert!(p
            .iter()
            .any(|x| x.id == b && x.kind == "beam" && x.class == StructClass::Overhead));
        let roof: Vec<_> = p.iter().filter(|x| x.kind == "roof" && x.id == r).collect();
        assert_eq!(roof.len(), 9);
        assert!(roof.iter().all(|x| x.class == StructClass::Overhead));
        // section across the beam at x = 3: the I-beam and roof cuts
        let s = m.struct_section((3.0, -5.0), (3.0, 5.0), 10.0).unwrap();
        let beam_cut: Vec<_> = s
            .iter()
            .filter(|x| x.kind == "beam" && x.class == StructClass::Cut)
            .collect();
        assert_eq!(beam_cut.len(), 1);
        let a = ddc_geom::poly::area(
            &beam_cut[0]
                .pts
                .iter()
                .map(|q| ddc_geom::v2(q.0, q.1))
                .collect::<Vec<_>>(),
        );
        assert!((a.abs() - 0.00518806).abs() < 1e-12, "{a}");
        assert!(s
            .iter()
            .any(|x| x.kind == "roof" && x.class == StructClass::Cut));
        assert!(s
            .iter()
            .any(|x| x.kind == "column" && x.class == StructClass::Beyond));
    }
}
