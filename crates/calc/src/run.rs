//! The run job (design 1.4-1.6, 7.2): build the input, refuse blocking issues, solve, check
//! equilibrium per case, store the run with its input hash and result blobs. The input blob is
//! the canonical JSON of the solver input; its SHA-256 (with the solver identity) is the input
//! hash, and the blob makes any run re-solvable ([`frame_from_json`]).

use ddc_fem::{Axes, Combo, FemError, Frame, Load as FLoad, LoadCase, Material, Member};
use ddc_fem::{Options, Orient, Restraint, Section, Solution, Support};
use ddc_model::{CalcRun, Model};
use serde_json::{json, Value as J};
use sha2::{Digest, Sha256};

use crate::adapter::{build_input, CalcInput};
use crate::blob::write_blob;
use crate::{CalcError, Issue, Resolver, EQ_TOL, FEM_VERSION};

pub struct RunOptions<'a> {
    /// Provenance of the run: manual | ai:<session> | mcp:<client>.
    pub src: String,
    pub note: String,
    pub resolver: Option<Resolver<'a>>,
    /// Store the run in `calc_run` (and move older unpinned runs to the archive).
    pub store: bool,
    pub fem: Options,
}

impl Default for RunOptions<'_> {
    fn default() -> Self {
        RunOptions {
            src: "manual".into(),
            note: String::new(),
            resolver: None,
            store: true,
            fem: Options::default(),
        }
    }
}

/// Equilibrium of one case (or combination), relative values.
#[derive(Clone, Debug, PartialEq)]
pub struct CaseCheck {
    pub name: String,
    /// Solver's own check: applied + reactions about the origin.
    pub fem_relative: f64,
    /// The adapter's resultant of the loads it built against the solver's applied resultant.
    pub applied_relative: f64,
    /// Force the stored loads stand for against what reached the frame and the walls.
    pub conservation: f64,
    pub residual: [f64; 6],
}

pub struct RunOutcome {
    pub input: CalcInput,
    pub input_hash: String,
    pub solution: Solution,
    /// Cases, then combinations.
    pub checks: Vec<CaseCheck>,
    /// ok | suspect
    pub status: &'static str,
    pub run_id: Option<i64>,
    pub issues: Vec<Issue>,
}

const DOF: [&str; 6] = ["ux", "uy", "uz", "rx", "ry", "rz"];

fn ax(a: Axes) -> &'static str {
    match a {
        Axes::Local => "l",
        Axes::Global => "g",
    }
}

fn rel_mask(r: &[bool; 6]) -> String {
    r.iter().map(|x| if *x { 'P' } else { 'F' }).collect()
}

/// Canonical JSON of the solver input (keys, frame, options); also the `input` blob.
pub fn input_json(inp: &CalcInput, opt: &Options) -> J {
    let fr = &inp.frame;
    let load = |l: &FLoad| match l {
        FLoad::Node { node, f } => json!(["node", node, f]),
        FLoad::Point { member, a, f, m, axes } => json!(["point", member, a, f, m, ax(*axes)]),
        FLoad::Dist { member, a, b, w1, w2, axes } => {
            json!(["dist", member, a, b, w1, w2, ax(*axes)])
        }
        FLoad::SelfWeight { dir, g } => json!(["sw", dir, g]),
    };
    let orient = |o: &Orient| match o {
        Orient::Auto => json!(["auto"]),
        Orient::Vector(v) => json!(["v", v]),
        Orient::Roll(r) => json!(["roll", r]),
    };
    let restr = |r: &Restraint| match r {
        Restraint::Free => json!("P"),
        Restraint::Fixed => json!("F"),
        Restraint::Spring(k) => json!(k),
    };
    json!({
        "format": "ddc-calc-input", "version": 1, "solver": FEM_VERSION,
        "options": { "shear": opt.shear, "stations": opt.stations, "pivot_tol": opt.pivot_tol },
        "node_keys": inp.node_keys, "seg_keys": inp.seg_keys,
        "case_ids": inp.case_ids, "combo_ids": inp.combo_ids,
        "auto_restrained": inp.auto_restrained.iter().map(|(n, r)| json!([n, r]))
            .collect::<Vec<_>>(),
        "nodes": fr.nodes,
        "materials": fr.materials.iter().map(|m| json!([m.e, m.g, m.density]))
            .collect::<Vec<_>>(),
        "sections": fr.sections.iter().map(|s| json!([s.a, s.iy, s.iz, s.j, s.avy, s.avz]))
            .collect::<Vec<_>>(),
        "members": fr.members.iter().map(|m| json!({
            "i": m.i, "j": m.j, "mat": m.material, "sec": m.section, "orient": orient(&m.orient),
            "rel": [rel_mask(&m.release[0]), rel_mask(&m.release[1])], "off": m.offset
        })).collect::<Vec<_>>(),
        "supports": fr.supports.iter().map(|s| json!({
            "n": s.node, "d": s.dof.iter().map(restr).collect::<Vec<_>>()
        })).collect::<Vec<_>>(),
        "cases": fr.cases.iter().map(|c| json!({
            "name": c.name, "loads": c.loads.iter().map(load).collect::<Vec<_>>()
        })).collect::<Vec<_>>(),
        "combos": fr.combos.iter().map(|c| json!({ "name": c.name, "f": c.factors }))
            .collect::<Vec<_>>(),
    })
}

fn f(v: &J) -> Option<f64> {
    v.as_f64()
}
fn u(v: &J) -> Option<usize> {
    v.as_u64().map(|x| x as usize)
}
fn a3(v: &J) -> Option<[f64; 3]> {
    let a = v.as_array().filter(|a| a.len() == 3)?;
    Some([f(&a[0])?, f(&a[1])?, f(&a[2])?])
}
fn a6(v: &J) -> Option<[f64; 6]> {
    let a = v.as_array().filter(|a| a.len() == 6)?;
    let mut o = [0.0; 6];
    for (x, y) in o.iter_mut().zip(a) {
        *x = f(y)?;
    }
    Some(o)
}
fn axes_of(v: &J) -> Option<Axes> {
    match v.as_str()? {
        "l" => Some(Axes::Local),
        "g" => Some(Axes::Global),
        _ => None,
    }
}
fn mask(v: &J) -> Option<[bool; 6]> {
    let s = v.as_str().filter(|s| s.len() == 6)?;
    let mut o = [false; 6];
    for (x, c) in o.iter_mut().zip(s.chars()) {
        *x = c == 'P';
    }
    Some(o)
}

/// Decodes the frame of an `input` blob (the inverse of [`input_json`]).
pub fn frame_from_json(j: &J) -> Option<Frame> {
    let arr = |k: &str| j.get(k).and_then(J::as_array).cloned().unwrap_or_default();
    let mut fr = Frame::default();
    for n in arr("nodes") {
        fr.nodes.push(a3(&n)?);
    }
    for m in arr("materials") {
        let a = m.as_array()?;
        fr.materials.push(Material { e: f(&a[0])?, g: f(&a[1])?, density: f(&a[2])? });
    }
    for s in arr("sections") {
        let a = s.as_array()?;
        let mut sec = Section::new(f(&a[0])?, f(&a[1])?, f(&a[2])?, f(&a[3])?);
        sec.avy = a.get(4).and_then(f);
        sec.avz = a.get(5).and_then(f);
        fr.sections.push(sec);
    }
    for m in arr("members") {
        let o = m.get("orient")?.as_array()?;
        let orient = match o.first()?.as_str()? {
            "auto" => Orient::Auto,
            "v" => Orient::Vector(a3(o.get(1)?)?),
            "roll" => Orient::Roll(f(o.get(1)?)?),
            _ => return None,
        };
        let rel = m.get("rel")?.as_array()?;
        let off = m.get("off")?.as_array()?;
        let mut mm = Member::new(u(m.get("i")?)?, u(m.get("j")?)?, u(m.get("mat")?)?, 0);
        mm.section = u(m.get("sec")?)?;
        mm.orient = orient;
        mm.release = [mask(rel.first()?)?, mask(rel.get(1)?)?];
        mm.offset = [a3(off.first()?)?, a3(off.get(1)?)?];
        fr.members.push(mm);
    }
    for s in arr("supports") {
        let d = s.get("d")?.as_array()?;
        let mut dof = [Restraint::Free; 6];
        for (x, v) in dof.iter_mut().zip(d) {
            *x = match v.as_str() {
                Some("F") => Restraint::Fixed,
                Some(_) => Restraint::Free,
                None => Restraint::Spring(f(v)?),
            };
        }
        fr.supports.push(Support::new(u(s.get("n")?)?, dof));
    }
    for c in arr("cases") {
        let mut lc = LoadCase { name: c.get("name")?.as_str()?.to_string(), loads: vec![] };
        for l in c.get("loads")?.as_array()? {
            let a = l.as_array()?;
            lc.loads.push(match a.first()?.as_str()? {
                "node" => FLoad::Node { node: u(&a[1])?, f: a6(&a[2])? },
                "point" => FLoad::Point {
                    member: u(&a[1])?,
                    a: f(&a[2])?,
                    f: a3(&a[3])?,
                    m: a3(&a[4])?,
                    axes: axes_of(&a[5])?,
                },
                "dist" => FLoad::Dist {
                    member: u(&a[1])?,
                    a: f(&a[2])?,
                    b: f(&a[3])?,
                    w1: a3(&a[4])?,
                    w2: a3(&a[5])?,
                    axes: axes_of(&a[6])?,
                },
                "sw" => FLoad::SelfWeight { dir: a3(&a[1])?, g: f(&a[2])? },
                _ => return None,
            });
        }
        fr.cases.push(lc);
    }
    for c in arr("combos") {
        let mut factors = vec![];
        for p in c.get("f")?.as_array()? {
            let p = p.as_array()?;
            factors.push((u(&p[0])?, f(&p[1])?));
        }
        fr.combos.push(Combo { name: c.get("name")?.as_str()?.to_string(), factors });
    }
    Some(fr)
}

/// SHA-256 (hex) of the canonical input bytes.
pub fn input_hash(bytes: &[u8]) -> String {
    let d = Sha256::digest(bytes);
    d.iter().map(|b| format!("{b:02x}")).collect()
}

fn map_fem(inp: &CalcInput, e: FemError) -> Issue {
    match e {
        FemError::Mechanism { node, dof } => {
            let key = inp.node_keys.get(node).cloned().unwrap_or_else(|| format!("#{node}"));
            Issue::block("unstable", key, format!("{} has no stiffness", DOF[dof % 6]))
        }
        FemError::Release { member, dof } => {
            let key = inp.seg_keys.get(member).cloned().unwrap_or_else(|| format!("#{member}"));
            let end = if dof < 6 { "start" } else { "end" };
            let note = format!("released {} at the {end} is loaded", DOF[dof % 6]);
            Issue::block("unstable", key, note)
        }
        FemError::Input(s) => Issue::block("solver_input", "global", s),
    }
}

fn rel(res: &[f64; 6], fscale: f64, mscale: f64) -> f64 {
    let fr = (0..3).map(|k| res[k].abs()).fold(0.0, f64::max);
    let mr = (3..6).map(|k| res[k].abs()).fold(0.0, f64::max);
    if fscale <= 0.0 {
        return 0.0;
    }
    (fr / fscale).max(mr / mscale)
}

/// Solves a built input without storing it.
pub fn solve_input(inp: CalcInput, opt: &Options) -> Result<RunOutcome, CalcError> {
    if inp.blocking() {
        return Err(CalcError::Blocked(inp.issues.clone()));
    }
    let bytes = input_json(&inp, opt).to_string().into_bytes();
    let hash = input_hash(&bytes);
    let sol = match ddc_fem::solve(&inp.frame, opt) {
        Ok(s) => s,
        Err(e) => {
            let mut v = inp.issues.clone();
            v.push(map_fem(&inp, e));
            return Err(CalcError::Blocked(v));
        }
    };
    let ext = inp
        .frame
        .nodes
        .iter()
        .map(|p| (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt())
        .fold(1.0, f64::max);
    let mut checks = vec![];
    for (c, info) in sol.cases.iter().zip(&inp.cases) {
        let (fs, ms) = (info.scale, info.scale * ext);
        let eq = &c.equilibrium;
        let mut d = [0.0; 6];
        let mut cons = [0.0; 6];
        for k in 0..6 {
            d[k] = info.applied[k] - eq.load[k];
        }
        for k in 0..3 {
            cons[k] = info.intended[k] - info.applied[k] - info.walls[k];
        }
        checks.push(CaseCheck {
            name: c.name.clone(),
            fem_relative: eq.relative,
            applied_relative: rel(&d, fs, ms),
            conservation: rel(&cons, fs, ms),
            residual: eq.residual,
        });
    }
    for c in &sol.combos {
        checks.push(CaseCheck {
            name: c.name.clone(),
            fem_relative: c.equilibrium.relative,
            applied_relative: 0.0,
            conservation: 0.0,
            residual: c.equilibrium.residual,
        });
    }
    let worst = checks
        .iter()
        .map(|c| c.fem_relative.max(c.applied_relative).max(c.conservation))
        .fold(0.0, f64::max);
    let status = if worst <= EQ_TOL { "ok" } else { "suspect" };
    let issues = inp.issues.clone();
    Ok(RunOutcome {
        input: inp,
        input_hash: hash,
        solution: sol,
        checks,
        status,
        run_id: None,
        issues,
    })
}

fn sets(inp: &CalcInput) -> Vec<String> {
    let c = inp.case_ids.iter().map(|i| format!("lc:{i}"));
    c.chain(inp.combo_ids.iter().map(|i| format!("co:{i}"))).collect()
}

/// Result blobs: nodes (displacements), reactions, members (end forces, local).
pub fn result_blobs(o: &RunOutcome) -> Vec<(&'static str, Vec<u8>)> {
    let inp = &o.input;
    let all: Vec<&ddc_fem::CaseResult> =
        o.solution.cases.iter().chain(&o.solution.combos).collect();
    let mut nodes = vec![];
    let mut reac = vec![];
    let mut memb = vec![];
    for r in &all {
        nodes.extend(r.disp.iter().flatten());
        reac.extend(r.reactions.iter().flat_map(|x| x.1));
        memb.extend(r.end_forces.iter().flat_map(|e| e.iter().flatten()));
    }
    let sup_keys: Vec<&String> = o
        .solution
        .cases
        .first()
        .map(|c| c.reactions.iter().map(|r| &inp.node_keys[r.0]).collect())
        .unwrap_or_default();
    let cols_n = ["ux", "uy", "uz", "rx", "ry", "rz"];
    let cols_r = ["Rx", "Ry", "Rz", "Mx", "My", "Mz"];
    let q = ["N", "Vy", "Vz", "T", "My", "Mz"];
    let cols_m: Vec<String> = ["start", "end"]
        .iter()
        .flat_map(|e| q.iter().map(move |x| format!("{x}@{e}")))
        .collect();
    let h = |part: &str, rows: J, cols: J| {
        json!({ "v": 1, "part": part, "layout": "set,row,col", "sets": sets(inp),
            "rows": rows, "cols": cols, "units": "SI" })
    };
    vec![
        ("nodes", write_blob(&h("nodes", json!(inp.node_keys), json!(cols_n)), &nodes)),
        ("reactions", write_blob(&h("reactions", json!(sup_keys), json!(cols_r)), &reac)),
        ("members", write_blob(&h("members", json!(inp.seg_keys), json!(cols_m)), &memb)),
    ]
}

/// Builds, solves, checks and (by default) stores a run of the current model.
pub fn run(model: &Model, o: &RunOptions) -> Result<RunOutcome, CalcError> {
    let inp = build_input(model, o.resolver)?;
    let bytes = input_json(&inp, &o.fem).to_string().into_bytes();
    let mut out = solve_input(inp, &o.fem)?;
    if !o.store {
        return Ok(out);
    }
    let stats = json!({
        "nodes": out.input.frame.nodes.len(), "members": out.input.frame.members.len(),
        "equations": out.solution.equations,
        "cases": out.checks.iter().map(|c| json!({
            "name": c.name, "fem_relative": c.fem_relative,
            "applied_relative": c.applied_relative, "conservation": c.conservation,
            "residual": c.residual
        })).collect::<Vec<_>>(),
        "auto_restrained": out.input.auto_restrained.iter()
            .map(|(n, _)| out.input.node_keys[*n].clone()).collect::<Vec<_>>(),
        "warnings": out.issues.iter().map(|i| json!({ "code": i.code, "key": i.key,
            "note": i.note })).collect::<Vec<_>>(),
    });
    let blobs = result_blobs(&out);
    let mut parts: Vec<(&str, &[u8])> = vec![("input", &bytes)];
    for (p, b) in &blobs {
        parts.push((p, b));
    }
    let row = CalcRun {
        id: 0,
        created: 0,
        input_hash: out.input_hash.clone(),
        fem_version: FEM_VERSION.into(),
        status: out.status.into(),
        stats,
        pinned: false,
        src: o.src.clone(),
        note: o.note.clone(),
    };
    out.run_id = Some(model.add_calc_run(&row, &parts)?);
    model.retain_calc_runs()?;
    Ok(out)
}
