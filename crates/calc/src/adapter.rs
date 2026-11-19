//! Analytical model + stored loads -> `ddc_fem::Frame` (design 1.6). Members are split into
//! segments at their inner nodes (`m:<kind>:<id>#<k>`); only nodes used by members go to the
//! solver, in ascending analytical order. Local axes: x from start to end, the model's section
//! z axis (roll included) is the reference vector, so fem y and z equal the section axes and
//! `Iy` bends about y. A node where no member end and no support restrains a rotation axis
//! that is a global axis orthogonal to all restrained ones (truss and pin joints) gets that
//! rotation fixed: no stiffness there, so the reaction is exactly zero. Anything else that is
//! singular is reported by the solver and mapped to an `unstable` issue on the node key.

use std::collections::BTreeMap;

use ddc_fem::{Axes, Combo, Frame, Load as FLoad, LoadCase as FCase, Material, Member, Orient};
use ddc_fem::{Restraint, Section, Support};
use ddc_model::{Analytical, Model};

use crate::{cross3, dot3, mul3, norm3, sub3, unit3, Issue, Resolver, P3};

/// One solver member: a piece of analytical member `member` between its nodes `k` and `k+1`.
#[derive(Clone, Debug, PartialEq)]
pub struct Seg {
    pub member: usize,
    pub k: usize,
    /// Station of the segment start on the analytical member, m.
    pub x0: f64,
    /// Length between the nodes and of the flexible part (between the offsets).
    pub len: f64,
    pub flex: f64,
    /// Flexible start and end (node + rigid offset), global.
    pub start: P3,
    pub end: P3,
    /// Local axes as the solver builds them.
    pub x: P3,
    pub y: P3,
    pub z: P3,
}

/// Per load case: what the stored loads mean, and where the area loads went.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CaseLoads {
    pub case_id: i64,
    pub name: String,
    /// Resultant (about the origin) of the solver loads as this crate built them.
    pub applied: [f64; 6],
    /// Total force the stored loads stand for (area loads: p times the net area).
    pub intended: [f64; 3],
    /// Force carried by `line_support` walls (outside the frame; their reaction is minus it).
    pub walls: [f64; 6],
    /// Sum of the load magnitudes, the scale of the equilibrium check.
    pub scale: f64,
    /// Expanded solver loads.
    pub count: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CalcInput {
    pub frame: Frame,
    /// Per solver node: stable key and analytical index.
    pub node_keys: Vec<String>,
    pub an_node: Vec<usize>,
    /// Per solver member.
    pub segs: Vec<Seg>,
    pub seg_keys: Vec<String>,
    /// Per analytical member: its solver members (empty when excluded or blocked).
    pub member_segs: Vec<Vec<usize>>,
    pub case_ids: Vec<i64>,
    pub combo_ids: Vec<i64>,
    /// Solver nodes with automatic rotation restraints (rx, ry, rz).
    pub auto_restrained: Vec<(usize, [bool; 3])>,
    pub cases: Vec<CaseLoads>,
    pub issues: Vec<Issue>,
}

impl CalcInput {
    pub fn blocking(&self) -> bool {
        self.issues.iter().any(|i| i.blocking)
    }
    pub fn node_of(&self, key: &str) -> Option<usize> {
        self.node_keys.iter().position(|k| k == key)
    }
    /// Solver members of the analytical member with stable key `key`.
    pub fn segs_of(&self, a: &Analytical, key: &str) -> Vec<usize> {
        a.member_of(key).map(|i| self.member_segs[i].clone()).unwrap_or_default()
    }
}

/// E, G and density of a material: the material row, else the class defaults.
pub fn material_values(model: &Model, name: &str) -> Result<Option<Material>, ddc_model::Error> {
    let mech = model.material_mech(name)?;
    let def = ddc_model::mat_mech::mech_defaults(name);
    let Some(e) = mech.e.or(def.map(|d| d.e)) else {
        return Ok(None);
    };
    let nu = mech.nu.or(def.map(|d| d.nu));
    let g = match (mech.g, mech.e, nu) {
        (Some(g), _, _) => g,
        (None, Some(e), Some(nu)) => e / (2.0 * (1.0 + nu)),
        _ => match def {
            Some(d) => d.g,
            None => return Ok(None),
        },
    };
    let density = model.material(name)?.spec.density.unwrap_or(0.0);
    Ok(Some(Material { e, g, density }))
}

fn p3(v: ddc_geom::V3) -> P3 {
    [v.x, v.y, v.z]
}

/// Local axes of the solver for a member along `x` with reference vector `r`.
pub(crate) fn local_axes(x: P3, r: P3) -> (P3, P3) {
    let z = unit3(sub3(r, mul3(x, dot3(r, x))));
    (cross3(z, x), z)
}

/// Orthonormal basis of the span of `vs`.
fn basis(vs: &[P3]) -> Vec<P3> {
    let mut b: Vec<P3> = vec![];
    for v in vs {
        let mut w = *v;
        for e in &b {
            w = sub3(w, mul3(*e, dot3(w, *e)));
        }
        if norm3(w) > 1e-6 {
            b.push(unit3(w));
        }
        if b.len() == 3 {
            break;
        }
    }
    b
}

const AXES_G: [P3; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// Builds the solver input of the current model; blocking issues are listed, not raised.
pub fn build_input(
    model: &Model,
    resolver: Option<Resolver>,
) -> Result<CalcInput, ddc_model::Error> {
    let a = model.analytical()?;
    let mut issues: Vec<Issue> = vec![];
    for q in &a.issues {
        let key = match q.kind {
            "override" => format!("ov:{}", q.id),
            "node" => q.node.map(|n| a.node_keys[n].clone()).unwrap_or_default(),
            k @ ("wall" | "slab") => ddc_model::panel_key(k, q.id),
            k => ddc_model::member_key(k, q.id),
        };
        let blocking = matches!(q.code, "no_section" | "no_material");
        issues.push(Issue { code: q.code, key, blocking, note: String::new() });
    }
    // materials and sections in first-use order
    let mut mats: Vec<Material> = vec![];
    let mut mat_of: BTreeMap<String, Option<usize>> = BTreeMap::new();
    let mut secs: Vec<Section> = vec![];
    let mut sec_of: BTreeMap<usize, usize> = BTreeMap::new();
    let mut used = vec![false; a.nodes.len()];
    let mut plan: Vec<(usize, usize, usize)> = vec![];
    for (mi, m) in a.members.iter().enumerate() {
        if m.role == "excluded" || m.nodes.len() < 2 {
            continue;
        }
        if m.nodes.iter().all(|n| *n == m.nodes[0]) {
            // both ends snapped to one node: no element; loads on it are reported as lost
            issues.push(Issue::warn("short_member", m.key.clone(), "both ends on one node"));
            continue;
        }
        let Some(si) = m.section else { continue };
        let mat = match mat_of.get(&m.material) {
            Some(x) => *x,
            None => {
                let v = material_values(model, &m.material)?.map(|v| {
                    mats.push(v);
                    mats.len() - 1
                });
                mat_of.insert(m.material.clone(), v);
                v
            }
        };
        let Some(mat) = mat else {
            if !issues.iter().any(|i| i.code == "no_material" && i.key == m.key) {
                let note = format!("no E and G for '{}'", m.material);
                issues.push(Issue::block("no_material", m.key.clone(), note));
            }
            continue;
        };
        let n = secs.len();
        let fs = *sec_of.entry(si).or_insert(n);
        if fs == n {
            let p = &a.sections[si].props;
            let pos = |x: f64| (x > 0.0).then_some(x);
            let base = Section::new(p.area, p.iy, p.iz, p.j);
            secs.push(Section { avy: pos(p.avy), avz: pos(p.avz), ..base });
        }
        for &nd in &m.nodes {
            used[nd] = true;
        }
        plan.push((mi, mat, fs));
    }
    let an_node: Vec<usize> = (0..a.nodes.len()).filter(|&n| used[n]).collect();
    let mut fem_of = vec![usize::MAX; a.nodes.len()];
    for (i, &n) in an_node.iter().enumerate() {
        fem_of[n] = i;
    }
    let mut frame = Frame {
        nodes: an_node.iter().map(|&n| p3(a.nodes[n])).collect(),
        materials: mats,
        sections: secs,
        ..Frame::default()
    };
    let node_keys: Vec<String> = an_node.iter().map(|&n| a.node_keys[n].clone()).collect();
    let mut segs: Vec<Seg> = vec![];
    let mut seg_keys: Vec<String> = vec![];
    let mut member_segs: Vec<Vec<usize>> = vec![vec![]; a.members.len()];
    for &(mi, mat, sec) in &plan {
        let m = &a.members[mi];
        let r = p3(m.axes.1);
        let last = m.nodes.len() - 2;
        let mut x0 = 0.0;
        for (k, w) in m.nodes.windows(2).enumerate() {
            let (pi, pj) = (p3(a.nodes[w[0]]), p3(a.nodes[w[1]]));
            let oi = if k == 0 { p3(m.ecc[0]) } else { [0.0; 3] };
            let oj = if k == last { p3(m.ecc[1]) } else { [0.0; 3] };
            let (s, e) = (crate::add3(pi, oi), crate::add3(pj, oj));
            let x = unit3(sub3(e, s));
            let (y, z) = local_axes(x, r);
            let mut rel = [[false; 6]; 2];
            if k == 0 {
                rel[0] = m.releases[0];
            }
            if k == last {
                rel[1] = m.releases[1];
            }
            frame.members.push(Member {
                i: fem_of[w[0]],
                j: fem_of[w[1]],
                material: mat,
                section: sec,
                orient: Orient::Vector(r),
                release: rel,
                offset: [oi, oj],
            });
            let len = norm3(sub3(pj, pi));
            member_segs[mi].push(segs.len());
            seg_keys.push(format!("{}#{k}", m.key));
            segs.push(Seg {
                member: mi,
                k,
                x0,
                len,
                flex: norm3(sub3(e, s)),
                start: s,
                end: e,
                x,
                y,
                z,
            });
            x0 += len;
        }
    }
    // supports on solver nodes
    let mut sup: BTreeMap<usize, [Restraint; 6]> = BTreeMap::new();
    for s in &a.supports {
        let n = fem_of[s.node];
        if n == usize::MAX {
            issues.push(Issue::warn("support_unused", s.key.clone(), "no member at the node"));
            continue;
        }
        let mut d = [Restraint::Free; 6];
        for k in 0..6 {
            d[k] = if s.fixed[k] {
                Restraint::Fixed
            } else if s.springs[k] > 0.0 {
                Restraint::Spring(s.springs[k])
            } else {
                Restraint::Free
            };
        }
        sup.insert(n, d);
    }
    // rotation stiffness directions per node
    let mut dirs: Vec<Vec<P3>> = vec![vec![]; frame.nodes.len()];
    for (fm, sg) in frame.members.iter().zip(&segs) {
        let ax = [sg.x, sg.y, sg.z];
        for (end, node) in [(0, fm.i), (1, fm.j)] {
            for r in 0..3 {
                if !fm.release[end][3 + r] {
                    dirs[node].push(ax[r]);
                }
            }
        }
    }
    for (n, d) in &sup {
        for r in 0..3 {
            if !matches!(d[3 + r], Restraint::Free) {
                dirs[*n].push(AXES_G[r]);
            }
        }
    }
    let mut auto_restrained = vec![];
    for (n, ds) in dirs.iter().enumerate() {
        let mut b = basis(ds);
        if b.len() == 3 {
            continue;
        }
        let mut add = [false; 3];
        for (r, e) in AXES_G.iter().enumerate() {
            if b.iter().all(|v| dot3(*v, *e).abs() < 1e-9) {
                add[r] = true;
                b.push(*e);
            }
        }
        if b.len() < 3 || !add.iter().any(|x| *x) {
            continue;
        }
        let d = sup.entry(n).or_insert([Restraint::Free; 6]);
        for r in 0..3 {
            if add[r] {
                d[3 + r] = Restraint::Fixed;
            }
        }
        auto_restrained.push((n, add));
    }
    frame.supports = sup.into_iter().map(|(n, d)| Support::new(n, d)).collect();
    let mut inp = CalcInput {
        frame,
        node_keys,
        an_node,
        segs,
        seg_keys,
        member_segs,
        case_ids: vec![],
        combo_ids: vec![],
        auto_restrained,
        cases: vec![],
        issues,
    };
    // load cases and combinations, ordered by id
    let mut cases = model.load_cases()?;
    cases.sort_by_key(|c| c.id);
    let all = model.loads(None)?;
    let mut pden = vec![];
    for p in &a.panels {
        pden.push(model.material(&p.material)?.spec.density.unwrap_or(0.0));
    }
    for c in &cases {
        let mut ls: Vec<&ddc_model::Load> = all.iter().filter(|l| l.lcase == c.id).collect();
        ls.sort_by_key(|l| l.id);
        let (loads, info, iss) = crate::loads::expand_case(&a, &inp, &pden, c, &ls, resolver);
        inp.frame.cases.push(FCase { name: c.name.clone(), loads });
        inp.cases.push(info);
        inp.issues.extend(iss);
        inp.case_ids.push(c.id);
    }
    check_auto_moments(&mut inp);
    let mut combos = model.load_combos()?;
    combos.sort_by_key(|c| c.id);
    for c in &combos {
        let factors = c
            .terms
            .iter()
            .filter_map(|(lc, f)| inp.case_ids.iter().position(|x| x == lc).map(|i| (i, *f)))
            .collect();
        inp.frame.combos.push(Combo { name: c.name.clone(), factors });
        inp.combo_ids.push(c.id);
    }
    if inp.frame.members.is_empty() {
        inp.issues.push(Issue::block("no_members", "global", "no member can be analysed"));
    }
    Ok(inp)
}

/// A nodal moment on an automatically restrained rotation has no stiffness to go to.
fn check_auto_moments(inp: &mut CalcInput) {
    let mut bad = vec![];
    for c in &inp.frame.cases {
        for l in &c.loads {
            if let FLoad::Node { node, f } = l {
                if let Some((_, r)) = inp.auto_restrained.iter().find(|x| x.0 == *node) {
                    if (0..3).any(|k| r[k] && f[3 + k] != 0.0) {
                        bad.push(inp.node_keys[*node].clone());
                    }
                }
            }
        }
    }
    for k in bad {
        let note = "a moment acts on a rotation no member restrains";
        inp.issues.push(Issue::block("unstable", k, note));
    }
}

/// Load in global axes from a local load of segment `s`.
pub(crate) fn to_global(s: &Seg, v: P3, axes: Axes) -> P3 {
    match axes {
        Axes::Global => v,
        Axes::Local => {
            let mut o = [0.0; 3];
            for k in 0..3 {
                o[k] = s.x[k] * v[0] + s.y[k] * v[1] + s.z[k] * v[2];
            }
            o
        }
    }
}
