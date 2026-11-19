//! Rebar and the analytical model -> IFC. Each bar is an IfcReinforcingBar (MAIN / LIGATURE)
//! with an IfcSweptDiskSolid along an IfcIndexedPolyCurve whose bends and hooks are exact arc
//! segments, placed in the storey frame; Tag carries the bar mark. The analytical model is an
//! IfcStructuralAnalysisModel grouping point connections (nodes, supports as boundary
//! conditions), curve members (one per node-to-node segment) and shell surface members.

use ddc_geom::rebar::Prim2;
use ddc_geom::{v2, v3, V3};
use ddc_model::{AnRelease, Model};

use super::{list, r, real, reals, s, text, Ctx, Geo, NONE};

/// Bars of one storey.
pub(super) fn bars(
    c: &mut Ctx,
    model: &Model,
    level: i64,
    elev: f64,
    g: &Geo,
    elems: &mut Vec<u32>,
) -> Result<(), String> {
    let built = model.rebar_built().map_err(|e| format!("rebar: {e}"))?;
    let mut ax: Option<u32> = None;
    let mut k_in_set: (i64, i64) = (-1, 0);
    for b in built.bars.iter().filter(|b| b.level == level) {
        if k_in_set.0 != b.set {
            k_in_set = (b.set, 0);
        }
        k_in_set.1 += 1;
        let at = |p| {
            let q = b.bar.at(p);
            v3(q.x, q.y, q.z - elev)
        };
        let mut pts: Vec<V3> = vec![];
        let push = |p: V3, pts: &mut Vec<V3>| -> usize {
            if pts.last().is_none_or(|l| (*l - p).len() > 1e-9) {
                pts.push(p);
            }
            pts.len()
        };
        let mut segs: Vec<String> = vec![];
        for prim in &b.bar.path {
            match *prim {
                Prim2::Line(p, q) => {
                    let i = push(at(p), &mut pts);
                    let j = push(at(q), &mut pts);
                    if i != j {
                        segs.push(format!("IFCLINEINDEX(({i},{j}))"));
                    }
                }
                Prim2::Arc {
                    c: cc,
                    r: rr,
                    a0,
                    sweep,
                } => {
                    let i = push(at(prim.start()), &mut pts);
                    // arc too short for three distinct indexed points
                    if sweep.abs() * rr < 1e-4 {
                        let j = push(at(prim.end()), &mut pts);
                        if i != j {
                            segs.push(format!("IFCLINEINDEX(({i},{j}))"));
                        }
                        continue;
                    }
                    let m = a0 + sweep / 2.0;
                    let k = push(at(cc + v2(m.cos(), m.sin()) * rr), &mut pts);
                    let j = push(at(prim.end()), &mut pts);
                    segs.push(format!("IFCARCINDEX(({i},{k},{j}))"));
                }
            }
        }
        if pts.len() < 2 || segs.is_empty() {
            continue;
        }
        let coords = pts
            .iter()
            .map(|p| reals(&[p.x, p.y, p.z]))
            .collect::<Vec<_>>()
            .join(",");
        let pl3 =
            c.s.add("IFCCARTESIANPOINTLIST3D", &[format!("({coords})"), s(NONE)]);
        let curve = c.s.add(
            "IFCINDEXEDPOLYCURVE",
            &[r(pl3), format!("({})", segs.join(",")), s(".F.")],
        );
        let disk = c.s.add(
            "IFCSWEPTDISKSOLID",
            &[r(curve), real(b.d / 2.0), s(NONE), s(NONE), s(NONE)],
        );
        let rep = c.s.add(
            "IFCSHAPEREPRESENTATION",
            &[
                r(g.body_ctx),
                text("Body"),
                text("AdvancedSweptSolid"),
                list(&[disk]),
            ],
        );
        let pds = c.s.add(
            "IFCPRODUCTDEFINITIONSHAPE",
            &[s(NONE), s(NONE), list(&[rep])],
        );
        let axis = match ax {
            Some(a) => a,
            None => {
                let o = c.point3(0.0, 0.0, 0.0);
                let a = c.axis3(o, None);
                ax = Some(a);
                a
            }
        };
        let pl = c.placement(Some(g.storey_pl), axis);
        let area = std::f64::consts::PI * b.d * b.d / 4.0;
        let e = c.rooted(
            "IFCREINFORCINGBAR",
            &format!("rebar-{}", b.set),
            k_in_set.1,
            &format!("{} {}", b.mark, b.shape.code()),
            &[
                s(NONE),
                r(pl),
                r(pds),
                text(&b.mark),
                text(&b.grade),
                real(b.d),
                real(area),
                real(b.length),
                s(if b.ligature { ".LIGATURE." } else { ".MAIN." }),
                s(NONE),
            ],
        );
        elems.push(e);
    }
    Ok(())
}

fn boolean(v: bool) -> String {
    format!("IFCBOOLEAN({})", if v { ".T." } else { ".F." })
}

fn condition(c: &mut Ctx, rel: AnRelease) -> u32 {
    let rot = rel == AnRelease::Fixed;
    c.s.add(
        "IFCBOUNDARYNODECONDITION",
        &[
            text(rel.name()),
            boolean(true),
            boolean(true),
            boolean(true),
            boolean(rot),
            boolean(rot),
            boolean(rot),
        ],
    )
}

fn topo(c: &mut Ctx, ctx: u32, kind: &str, item: u32) -> u32 {
    let rep = c.s.add(
        "IFCTOPOLOGYREPRESENTATION",
        &[r(ctx), text("Reference"), text(kind), list(&[item])],
    );
    c.s.add(
        "IFCPRODUCTDEFINITIONSHAPE",
        &[s(NONE), s(NONE), list(&[rep])],
    )
}

fn dir(c: &mut Ctx, d: V3) -> u32 {
    c.s.add("IFCDIRECTION", &[reals(&[d.x, d.y, d.z])])
}

/// The analytical model as one IfcStructuralAnalysisModel (world coordinates) in a
/// 'Reference' subcontext of `parent`; skipped when the model has no structural members
/// (walls and slabs alone are not a structural model).
pub(super) fn analysis(
    c: &mut Ctx,
    model: &Model,
    parent: u32,
    building: u32,
) -> Result<(), String> {
    let a = model.analytical().map_err(|e| format!("analytical: {e}"))?;
    if a.members.is_empty() {
        return Ok(());
    }
    let ref_ctx = c.s.add(
        "IFCGEOMETRICREPRESENTATIONSUBCONTEXT",
        &[
            text("Reference"),
            text("Model"),
            s("*"),
            s("*"),
            s("*"),
            s("*"),
            r(parent),
            s(NONE),
            s(".GRAPH_VIEW."),
            s(NONE),
        ],
    );
    let wax = c.axis3(c.origin, Some(c.dir_x));
    let shared = c.placement(None, wax);
    let iax = c.axis3(c.origin, None);
    let fixed = condition(c, AnRelease::Fixed);
    let pinned = condition(c, AnRelease::Pinned);
    let cond = |k: AnRelease| if k == AnRelease::Fixed { fixed } else { pinned };
    let mut items: Vec<u32> = vec![];
    let mut pts = vec![];
    let mut verts = vec![];
    let mut conns = vec![];
    for (i, p) in a.nodes.iter().enumerate() {
        let pt = c.point3(p.x, p.y, p.z);
        let vp = c.s.add("IFCVERTEXPOINT", &[r(pt)]);
        let pds = topo(c, ref_ctx, "Vertex", vp);
        let pl = c.placement(Some(shared), iax);
        let applied = a
            .supports
            .iter()
            .find(|s| s.node == i)
            .map_or(s(NONE), |s| r(cond(s.kind)));
        let e = c.rooted(
            "IFCSTRUCTURALPOINTCONNECTION",
            "an-node",
            i as i64,
            &format!("N{}", i + 1),
            &[s(NONE), r(pl), r(pds), applied, s(NONE)],
        );
        pts.push(pt);
        verts.push(vp);
        conns.push(e);
        items.push(e);
    }
    let mut rels = 0i64;
    let mut connect = |c: &mut Ctx, member: u32, node: usize, applied: Option<u32>| {
        rels += 1;
        c.rel(
            "IFCRELCONNECTSSTRUCTURALMEMBER",
            "an-rel",
            rels,
            &[
                r(member),
                r(conns[node]),
                applied.map_or(s(NONE), r),
                s(NONE),
                s(NONE),
                s(NONE),
            ],
        );
    };
    for (k, m) in a.members.iter().enumerate() {
        let n = m.nodes.len();
        for (j, w) in m.nodes.windows(2).enumerate() {
            let (p, q) = (a.nodes[w[0]], a.nodes[w[1]]);
            let d = q - p;
            if d.len() < 1e-9 {
                continue;
            }
            let u = d * (1.0 / d.len());
            let up = v3(0.0, 0.0, 1.0);
            let z = up - u * up.dot(u);
            let z = if z.len() < 1e-6 {
                v3(0.0, 1.0, 0.0)
            } else {
                z.normalized()
            };
            let axis = dir(c, z);
            let edge = c.s.add("IFCEDGE", &[r(verts[w[0]]), r(verts[w[1]])]);
            let pds = topo(c, ref_ctx, "Edge", edge);
            let pl = c.placement(Some(shared), iax);
            // pin-joined means pinned at both ends; a single release is an end condition below
            let pin = m.release == (AnRelease::Pinned, AnRelease::Pinned);
            let e = c.rooted(
                "IFCSTRUCTURALCURVEMEMBER",
                &format!("an-{}-{}", m.kind, m.id),
                j as i64,
                &format!("{} {} M{}", m.kind, m.id, k + 1),
                &[
                    s(NONE),
                    r(pl),
                    r(pds),
                    s(if pin {
                        ".PIN_JOINED_MEMBER."
                    } else {
                        ".RIGID_JOINED_MEMBER."
                    }),
                    r(axis),
                ],
            );
            items.push(e);
            let start = (j == 0).then(|| cond(m.release.0));
            let end = (j + 2 == n).then(|| cond(m.release.1));
            connect(c, e, w[0], start);
            connect(c, e, w[1], end);
        }
    }
    for (k, p) in a.panels.iter().enumerate() {
        if p.nodes.len() < 3 {
            continue;
        }
        let ring: Vec<V3> = p.nodes.iter().map(|n| a.nodes[*n]).collect();
        let mut nrm = v3(0.0, 0.0, 0.0);
        for i in 0..ring.len() {
            let (a0, b0) = (ring[i], ring[(i + 1) % ring.len()]);
            nrm.x += (a0.y - b0.y) * (a0.z + b0.z);
            nrm.y += (a0.z - b0.z) * (a0.x + b0.x);
            nrm.z += (a0.x - b0.x) * (a0.y + b0.y);
        }
        if nrm.len() < 1e-12 {
            continue;
        }
        let nrm = nrm.normalized();
        let e0 = ring[1] - ring[0];
        let xd = e0 - nrm * e0.dot(nrm);
        if xd.len() < 1e-12 {
            continue;
        }
        let (dz, dx) = (dir(c, nrm), dir(c, xd.normalized()));
        let pax =
            c.s.add("IFCAXIS2PLACEMENT3D", &[r(pts[p.nodes[0]]), r(dz), r(dx)]);
        let plane = c.s.add("IFCPLANE", &[r(pax)]);
        let ids: Vec<u32> = p.nodes.iter().map(|n| pts[*n]).collect();
        let lp = c.s.add("IFCPOLYLOOP", &[list(&ids)]);
        let bound = c.s.add("IFCFACEOUTERBOUND", &[r(lp), s(".T.")]);
        let face =
            c.s.add("IFCFACESURFACE", &[list(&[bound]), r(plane), s(".T.")]);
        let pds = topo(c, ref_ctx, "Face", face);
        let pl = c.placement(Some(shared), iax);
        let e = c.rooted(
            "IFCSTRUCTURALSURFACEMEMBER",
            &format!("an-{}-{}", p.kind, p.id),
            0,
            &format!("{} {} P{}", p.kind, p.id, k + 1),
            &[s(NONE), r(pl), r(pds), s(".SHELL."), real(p.thickness)],
        );
        items.push(e);
        for &n in p.nodes.iter().chain(&p.inner) {
            connect(c, e, n, None);
        }
    }
    let m = c.rooted(
        "IFCSTRUCTURALANALYSISMODEL",
        "an-model",
        0,
        "Analytical model",
        &[
            s(NONE),
            s(".LOADING_3D."),
            s(NONE),
            s(NONE),
            s(NONE),
            r(shared),
        ],
    );
    c.rel(
        "IFCRELASSIGNSTOGROUP",
        "an-group",
        0,
        &[list(&items), s(NONE), r(m)],
    );
    c.rel(
        "IFCRELSERVICESBUILDINGS",
        "an-bld",
        0,
        &[r(m), list(&[building])],
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::ifc::export::export_ifc;
    use ddc_model::{
        AnRelease, AnalyticalAdj, BeamJoin, BeamSpec, ColumnSpec, Just, MemberShape, Model,
        RebarHook, RebarLayout, RebarPath, RebarSetSpec,
    };

    #[test]
    fn bars_and_analysis_model_are_written() {
        let mut m = Model::open(":memory:").unwrap();
        let l0 = m.add_level("L0", 0.0).unwrap();
        let l1 = m.add_level("L1", 3.0).unwrap();
        let ct = m
            .add_member_type("RC 400", &MemberShape::Rect { b: 0.4, h: 0.4 }, "Concrete")
            .unwrap();
        let bt = m
            .add_member_type(
                "RC 300x500",
                &MemberShape::Rect { b: 0.3, h: 0.5 },
                "Concrete",
            )
            .unwrap();
        for x in [0.0, 6.0] {
            m.add_column(&ColumnSpec {
                level: l0,
                top_level: Some(l1),
                member_type: ct,
                structural: true,
                at: (x, 0.0),
                base_offset: 0.0,
                top_offset: 0.0,
                rotation: 0.0,
                top_at: None,
            })
            .unwrap();
        }
        let b = m
            .add_beam(&BeamSpec {
                level: l0,
                member_type: bt,
                a: (0.0, 0.0, 3.0),
                b: (6.0, 0.0, 3.0),
                bulge: 0.0,
                yjust: Just::Center,
                zjust: Just::Center,
                rotation: 0.0,
                start_ext: 0.0,
                end_ext: 0.0,
                join_start: BeamJoin::Auto,
                join_end: BeamJoin::Auto,
            })
            .unwrap();
        let t8 = m
            .rebar_types()
            .unwrap()
            .into_iter()
            .find(|t| (t.d - 0.008).abs() < 1e-9)
            .unwrap();
        m.add_rebar_sets(&[RebarSetSpec {
            host_kind: "beam".into(),
            host: b,
            rtype: t8.id,
            layout: RebarLayout::Stirrup {
                rule: RebarPath::Fixed(4),
                hook: RebarHook::H135,
            },
            mark: Some("S1".into()),
        }])
        .unwrap();
        let ifc = export_ifc(&m).unwrap();
        assert_eq!(ifc.matches("=IFCREINFORCINGBAR(").count(), 4);
        assert!(ifc.contains(".LIGATURE."));
        assert!(ifc.contains("IFCARCINDEX(("));
        assert!(ifc.contains("'AdvancedSweptSolid'"));
        assert_eq!(ifc.matches("=IFCSTRUCTURALANALYSISMODEL(").count(), 1);
        assert_eq!(ifc.matches("=IFCSTRUCTURALCURVEMEMBER(").count(), 3);
        assert_eq!(ifc.matches("=IFCSTRUCTURALPOINTCONNECTION(").count(), 4);
        assert!(ifc.contains("IFCRELASSIGNSTOGROUP"));
        assert_eq!(ifc.matches("=IFCRELSERVICESBUILDINGS(").count(), 1);
        assert!(!ifc.contains(".PIN_JOINED_MEMBER."));

        // one released end: still a rigid-joined member, the pin is that end's condition
        let adj = |a, b| AnalyticalAdj {
            start: (0.0, 0.0, 0.0),
            end: (0.0, 0.0, 0.0),
            release_start: Some(a),
            release_end: Some(b),
        };
        // how many member ends carry the pinned condition
        let pinned_ends = |ifc: &str| {
            let cond = ifc
                .lines()
                .find(|l| l.contains("=IFCBOUNDARYNODECONDITION('pinned'"))
                .and_then(|l| l.split('=').next())
                .unwrap()
                .to_string();
            ifc.lines()
                .filter(|l| l.contains("=IFCRELCONNECTSSTRUCTURALMEMBER("))
                .filter(|l| l.contains(&format!(",{cond},")))
                .count()
        };
        assert_eq!(pinned_ends(&ifc), 0);
        m.set_analytical_adj("beam", &[b], adj(AnRelease::Pinned, AnRelease::Fixed))
            .unwrap();
        let one = export_ifc(&m).unwrap();
        assert!(!one.contains(".PIN_JOINED_MEMBER."));
        assert_eq!(pinned_ends(&one), 1);
        // both released: a pin-joined member
        m.set_analytical_adj("beam", &[b], adj(AnRelease::Pinned, AnRelease::Pinned))
            .unwrap();
        let both = export_ifc(&m).unwrap();
        assert_eq!(both.matches(".PIN_JOINED_MEMBER.").count(), 1);
        assert_eq!(pinned_ends(&both), 2);
    }
}
