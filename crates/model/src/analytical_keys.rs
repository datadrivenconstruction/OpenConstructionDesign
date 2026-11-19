// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Stable keys and the engineer override layer of the analytical model
//! (docs/design/STRUCTURAL_ANALYSIS.md 1.3, 2.1). Keys come from physical ids, never from vector
//! indices. Overrides (`an_override`) are applied after `analytical_adj`; for the same field the
//! override wins. A target that no longer exists is reported (`orphan_override`, or
//! `locked_conflict` for a locked row) and the row is kept.

use std::collections::{BTreeMap, HashMap};

use ddc_geom::analytical::{Release, Tolerances};
use ddc_geom::member::Shape;
use ddc_geom::section_props::{self, SectionProps};
use ddc_geom::{v3, V3};
use rusqlite::Connection;
use serde_json::{json, Value as J};

use crate::analytical::{AnIssue, AnSection, AnSupport, Analytical, ADJ_KINDS};
use crate::calc_store::{key_kind, AnOverride};
use crate::Error;

/// Released DOF mask `FFFFPP` of a pinned end (ry, rz free).
pub const PINNED: [bool; 6] = [false, false, false, false, true, true];
pub const FIXED: [bool; 6] = [false; 6];

/// How one override was used by the last build.
#[derive(Clone, Debug, PartialEq)]
pub struct OverrideUse {
    pub id: i64,
    pub target: String,
    pub field: String,
    /// applied | re_anchored | orphaned | conflicting | invalid | deferred
    pub status: &'static str,
}

pub fn member_key(kind: &str, id: i64) -> String {
    format!("m:{kind}:{id}")
}

pub fn panel_key(kind: &str, id: i64) -> String {
    format!("p:{kind}:{id}")
}

/// Station text: metres, 3 decimals.
pub fn station(x: f64) -> String {
    format!("{:.3}", x.max(0.0))
}

/// Node owner priority (design 1.3): lower first; then the kind order of `ADJ_KINDS`, then id.
fn prio(kind: &str) -> u8 {
    match kind {
        "column" | "wall" => 0,
        "beam" => 1,
        "slab" => 2,
        "brace" => 3,
        _ => 9,
    }
}

fn kind_ord(kind: &str) -> usize {
    ADJ_KINDS.iter().position(|k| k.0 == kind).unwrap_or(ADJ_KINDS.len())
}

type Rank = (u8, usize, i64);

fn offer(best: &mut [Option<(Rank, String)>], n: usize, r: Rank, key: String) {
    if best[n].as_ref().is_none_or(|(b, _)| r < *b) {
        best[n] = Some((r, key));
    }
}

/// Candidate key of node `n` seen from member `mi` (None when not incident).
fn member_end_key(a: &Analytical, mi: usize, n: usize) -> Option<String> {
    let m = &a.members[mi];
    let k = m.nodes.iter().position(|&x| x == n)?;
    let wh = if k == 0 {
        "start".to_string()
    } else if k + 1 == m.nodes.len() {
        "end".to_string()
    } else {
        station((a.nodes[n] - a.nodes[m.nodes[0]]).len())
    };
    Some(format!("n:{}:{wh}", m.key))
}

/// Keys of members, panels and nodes from the current topology; duplicates get `~2`, `~3`.
pub(crate) fn assign(a: &mut Analytical) {
    for m in &mut a.members {
        m.key = member_key(m.kind, m.id);
    }
    for p in &mut a.panels {
        p.key = panel_key(p.kind, p.id);
    }
    let mut best: Vec<Option<(Rank, String)>> = vec![None; a.nodes.len()];
    for mi in 0..a.members.len() {
        let m = &a.members[mi];
        let r = (prio(m.kind), kind_ord(m.kind), m.id);
        for &n in &m.nodes {
            if let Some(k) = member_end_key(a, mi, n) {
                offer(&mut best, n, r, k);
            }
        }
    }
    for p in &a.panels {
        let r = (prio(p.kind), kind_ord(p.kind), p.id);
        for (k, &n) in p.nodes.iter().enumerate() {
            offer(&mut best, n, r, format!("n:{}:v{k}", p.key));
        }
        for (k, &n) in p.inner.iter().enumerate() {
            offer(&mut best, n, r, format!("n:{}:i{k}", p.key));
        }
    }
    let keys: Vec<String> = best
        .into_iter()
        .enumerate()
        .map(|(i, b)| b.map_or_else(|| format!("n:node:{}", i + 1), |b| b.1))
        .collect();
    a.node_keys = dedupe(keys);
}

fn dedupe(keys: Vec<String>) -> Vec<String> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    keys.into_iter()
        .map(|k| {
            let c = seen.entry(k.clone()).or_insert(0);
            *c += 1;
            if *c == 1 {
                k
            } else {
                format!("{k}~{c}")
            }
        })
        .collect()
}

/// Support keys: `s:<table>:<id>` for a footing with one support node, else
/// `s:<table>:<id>@<node key>`; user supports `s:<node key>`.
pub(crate) fn support_keys(a: &mut Analytical) {
    let mut count: BTreeMap<(&str, i64), usize> = BTreeMap::new();
    for s in &a.supports {
        *count.entry(s.from).or_insert(0) += 1;
    }
    let keys: Vec<String> = a
        .supports
        .iter()
        .map(|s| {
            let nk = &a.node_keys[s.node];
            if s.from.0 == "user" {
                format!("s:{nk}")
            } else if count[&s.from] == 1 {
                format!("s:{}:{}", s.from.0, s.from.1)
            } else {
                format!("s:{}:{}@{nk}", s.from.0, s.from.1)
            }
        })
        .collect();
    for (s, k) in a.supports.iter_mut().zip(keys) {
        s.key = k;
    }
}

/// Node anchor for an override on node `n`: position and incident member ends.
pub fn node_anchor(a: &Analytical, n: usize) -> J {
    let p = a.nodes[n];
    let mut ends = vec![];
    for m in &a.members {
        if m.nodes.first() == Some(&n) {
            ends.push(format!("{}:start", m.key));
        }
        if m.nodes.last() == Some(&n) {
            ends.push(format!("{}:end", m.key));
        }
    }
    json!({ "at": [p.x, p.y, p.z], "ends": ends })
}

pub(crate) fn read(db: &Connection) -> Result<Vec<AnOverride>, Error> {
    if !crate::rebar::has_table(db, "an_override")? {
        return Ok(vec![]);
    }
    let mut st = db.prepare(
        "SELECT id, target, field, value, anchor, locked, src, why FROM an_override ORDER BY id",
    )?;
    let js = |s: String| serde_json::from_str(&s).unwrap_or(J::String(s));
    let rows = st.query_map([], |r| {
        Ok(AnOverride {
            id: r.get(0)?,
            target: r.get(1)?,
            field: r.get(2)?,
            value: js(r.get(3)?),
            anchor: r.get::<_, Option<String>>(4)?.map(js),
            locked: r.get::<_, i64>(5)? != 0,
            src: r.get(6)?,
            why: r.get(7)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Tolerances with the `global` overrides `merge` and `snap` applied.
pub(crate) fn tolerances(ovs: &[AnOverride], mut tol: Tolerances) -> Tolerances {
    for o in ovs.iter().filter(|o| o.target == "global") {
        let v = o.value.as_f64().filter(|x| x.is_finite() && *x > 0.0 && *x <= 1.0);
        match (o.field.as_str(), v) {
            ("merge", Some(x)) => tol.merge = x,
            ("snap", Some(x)) => tol.snap = x,
            _ => {}
        }
    }
    tol
}

fn f3(v: Option<&J>) -> Option<V3> {
    let a = v?.as_array()?;
    if a.len() != 3 {
        return None;
    }
    let x: Vec<f64> = a.iter().filter_map(J::as_f64).filter(|x| x.is_finite()).collect();
    (x.len() == 3).then(|| v3(x[0], x[1], x[2]))
}

/// Released DOFs of a mask `FFFFPP` (P and S are released).
fn mask_of(v: &J) -> Option<([bool; 6], bool)> {
    let s = v.get("mask")?.as_str()?;
    if s.len() != 6 || !s.chars().all(|c| "FPS".contains(c)) {
        return None;
    }
    let mut r = [false; 6];
    for (o, c) in r.iter_mut().zip(s.chars()) {
        *o = c != 'F';
    }
    Some((r, s.contains('S')))
}

/// Rotates the section axes (y, z) about x by `rad`.
fn roll(y: V3, z: V3, rad: f64) -> (V3, V3) {
    let (c, s) = (rad.cos(), rad.sin());
    (y * c + z * s, z * c - y * s)
}

/// Resolves a node key: exact, else by the anchor (incident ends, then position); the flag
/// tells that the anchor was used.
fn find_node(
    a: &Analytical,
    key: &str,
    anchor: Option<&J>,
    tol: Tolerances,
) -> Option<(usize, bool)> {
    if let Some(n) = a.node_keys.iter().position(|k| k == key) {
        return Some((n, false));
    }
    let an = anchor?;
    let ends: Vec<&str> = an
        .get("ends")
        .and_then(J::as_array)
        .map(|v| v.iter().filter_map(J::as_str).collect())
        .unwrap_or_default();
    let mut hit: Option<usize> = None;
    let mut agree = !ends.is_empty();
    for e in &ends {
        let Some((mk, wh)) = e.rsplit_once(':') else {
            agree = false;
            break;
        };
        let n = a.members.iter().find(|m| m.key == mk).and_then(|m| match wh {
            "start" => m.nodes.first().copied(),
            "end" => m.nodes.last().copied(),
            _ => None,
        });
        match (n, hit) {
            (Some(n), None) => hit = Some(n),
            (Some(n), Some(h)) if n == h => {}
            _ => {
                agree = false;
                break;
            }
        }
    }
    if agree {
        if let Some(h) = hit {
            return Some((h, true));
        }
    }
    let at = f3(an.get("at"))?;
    let mut best: Option<(f64, usize)> = None;
    for (i, p) in a.nodes.iter().enumerate() {
        let d = (*p - at).len();
        if d <= tol.merge && best.is_none_or(|b| d < b.0) {
            best = Some((d, i));
        }
    }
    best.map(|b| (b.1, true))
}

/// Section of member type `t` (shared with the builder's sections by name and shape).
fn type_section(db: &Connection, a: &mut Analytical, t: i64) -> Result<Option<usize>, Error> {
    let row: Option<(String, String)> = rusqlite::OptionalExtension::optional(db.query_row(
        "SELECT name, shape FROM member_type WHERE id=?1",
        [t],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ))?;
    let Some((name, shape)) = row else {
        return Ok(None);
    };
    if let Some(i) = a.sections.iter().position(|s| s.name == name && s.shape == shape) {
        return Ok(Some(i));
    }
    let Some(sh) = Shape::parse(&shape) else {
        return Ok(None);
    };
    let props = section_props::of_shape(&sh);
    a.sections.push(AnSection {
        name,
        shape,
        area: props.area,
        iy: props.iy,
        iz: props.iz,
        props,
    });
    Ok(Some(a.sections.len() - 1))
}

fn inline_section(a: &mut Analytical, v: &J) -> Option<usize> {
    let g = |k: &str| v.get(k).and_then(J::as_f64).filter(|x| x.is_finite() && *x > 0.0);
    let (area, iy, iz, j) = (g("A")?, g("Iy")?, g("Iz")?, g("J")?);
    let props = SectionProps {
        area,
        iy,
        iz,
        i1: iy.max(iz),
        i2: iy.min(iz),
        j,
        avy: g("Avy").unwrap_or(0.0),
        avz: g("Avz").unwrap_or(0.0),
        ry: (iy / area).sqrt(),
        rz: (iz / area).sqrt(),
        ..SectionProps::default()
    };
    a.sections.push(AnSection {
        name: "engineer-defined".into(),
        shape: v.get("shape").and_then(J::as_str).unwrap_or("inline").to_string(),
        area,
        iy,
        iz,
        props,
    });
    Some(a.sections.len() - 1)
}

fn issue(a: &mut Analytical, code: &'static str, kind: &'static str, id: i64, node: Option<usize>) {
    a.issues.push(AnIssue { code, kind, id, node });
}

/// Applies the override layer to a freshly built model; fills `a.overrides`.
pub(crate) fn apply(
    db: &Connection,
    a: &mut Analytical,
    ovs: &[AnOverride],
    tol: Tolerances,
) -> Result<(), Error> {
    let mut used: Vec<OverrideUse> = vec![];
    let mut mark = |o: &AnOverride, status: &'static str| {
        used.push(OverrideUse {
            id: o.id,
            target: o.target.clone(),
            field: o.field.clone(),
            status,
        })
    };
    // 1: node topology (move, merge, split) on the keys of the base build
    let mut late: Vec<&AnOverride> = vec![];
    for o in ovs {
        match key_kind(&o.target) {
            Some('g') => mark(o, "applied"),
            Some('n') => {
                let Some((n, re)) = find_node(a, &o.target, o.anchor.as_ref(), tol) else {
                    mark(o, if o.locked { "conflicting" } else { "orphaned" });
                    continue;
                };
                let ok = match o.field.as_str() {
                    "move" => match f3(o.value.get("d")) {
                        Some(d) => {
                            a.nodes[n] = a.nodes[n] + d;
                            true
                        }
                        None => false,
                    },
                    "merge_with" => match o.value.as_str().and_then(|k| {
                        find_node(a, k, None, tol)
                    }) {
                        Some((b, _)) if b != n => {
                            merge(a, n, b);
                            true
                        }
                        _ => false,
                    },
                    "split" => {
                        split(a, n);
                        true
                    }
                    _ => false,
                };
                mark(o, if !ok { "invalid" } else if re { "re_anchored" } else { "applied" });
            }
            _ => late.push(o),
        }
    }
    // keys of nodes created by split; others keep the base keys
    if a.node_keys.len() < a.nodes.len() {
        let mut fresh = a.node_keys.clone();
        for n in fresh.len()..a.nodes.len() {
            let k = (0..a.members.len())
                .find_map(|mi| member_end_key(a, mi, n))
                .unwrap_or_else(|| format!("n:node:{}", n + 1));
            fresh.push(k);
        }
        a.node_keys = dedupe(fresh);
    }
    support_keys(a);
    // 2: members, supports, panels
    for o in late {
        let st = match key_kind(&o.target) {
            Some('m') => member(db, a, o, tol)?,
            Some('s') => support(a, o, tol),
            Some('p') => panel(a, o),
            _ => "invalid",
        };
        let st = if st == "orphaned" && o.locked { "conflicting" } else { st };
        mark(o, st);
    }
    for u in &used {
        match u.status {
            "orphaned" => issue(a, "orphan_override", "override", u.id, None),
            "conflicting" => issue(a, "locked_conflict", "override", u.id, None),
            _ => {}
        }
    }
    a.overrides = used;
    finish(db, a, tol)
}

fn merge(a: &mut Analytical, from: usize, to: usize) {
    for m in &mut a.members {
        for x in m.nodes.iter_mut() {
            if *x == from {
                *x = to;
            }
        }
        m.nodes.dedup();
    }
    for p in &mut a.panels {
        for x in p.nodes.iter_mut().chain(p.inner.iter_mut()) {
            if *x == from {
                *x = to;
            }
        }
        p.nodes.dedup();
    }
    let has_to = a.supports.iter().any(|s| s.node == to);
    if has_to {
        a.supports.retain(|s| s.node != from);
    } else {
        for s in &mut a.supports {
            if s.node == from {
                s.node = to;
            }
        }
    }
}

/// Every incident member except the node owner gets its own node at the same place.
fn split(a: &mut Analytical, n: usize) {
    let owner = a.node_keys[n]
        .strip_prefix("n:")
        .and_then(|r| r.rsplit_once(':'))
        .map(|(o, _)| o.to_string())
        .unwrap_or_default();
    let p = a.nodes[n];
    for mi in 0..a.members.len() {
        if a.members[mi].key == owner || !a.members[mi].nodes.contains(&n) {
            continue;
        }
        a.nodes.push(p);
        let nn = a.nodes.len() - 1;
        for x in a.members[mi].nodes.iter_mut() {
            if *x == n {
                *x = nn;
            }
        }
    }
}

fn member(
    db: &Connection,
    a: &mut Analytical,
    o: &AnOverride,
    tol: Tolerances,
) -> Result<&'static str, Error> {
    let Some(i) = a.members.iter().position(|m| m.key == o.target) else {
        return Ok("orphaned");
    };
    let v = &o.value;
    let ok = match o.field.as_str() {
        f @ ("ecc_start" | "ecc_end") => {
            let e = usize::from(f == "ecc_end");
            match f3(v.get("d")) {
                Some(d) => {
                    let m = &a.members[i];
                    let g = if v.get("frame").and_then(J::as_str) == Some("local") {
                        let x = a.axis(i);
                        x * d.x + m.axes.0 * d.y + m.axes.1 * d.z
                    } else {
                        d
                    };
                    a.members[i].ecc[e] = g;
                    if g.len() > tol.snap {
                        let (k, id) = (a.members[i].kind, a.members[i].id);
                        issue(a, "ecc_far", k, id, None);
                    }
                    true
                }
                None => false,
            }
        }
        f @ ("release_start" | "release_end") => match mask_of(v) {
            Some((r, spring)) => {
                a.members[i].releases[usize::from(f == "release_end")] = r;
                if spring {
                    let (k, id) = (a.members[i].kind, a.members[i].id);
                    issue(a, "release_spring", k, id, None);
                }
                true
            }
            None => false,
        },
        "section" => {
            let s = match v.get("type").and_then(J::as_i64) {
                Some(t) => type_section(db, a, t)?,
                None => inline_section(a, v),
            };
            if s.is_some() {
                a.members[i].section = s;
            }
            s.is_some()
        }
        "material" => match v.get("name").and_then(J::as_str) {
            Some(n) if !n.is_empty() => {
                a.members[i].material = n.to_string();
                true
            }
            _ => false,
        },
        "role" => match v.as_str() {
            Some("frame") => {
                a.members[i].role = "frame";
                true
            }
            Some("truss") => {
                a.members[i].role = "truss";
                true
            }
            Some("excluded") => {
                a.members[i].role = "excluded";
                true
            }
            _ => false,
        },
        "roll" => match v.get("deg").and_then(J::as_f64).filter(|x| x.is_finite()) {
            Some(deg) => {
                let m = &mut a.members[i];
                m.axes = roll(m.axes.0, m.axes.1, deg.to_radians());
                true
            }
            None => false,
        },
        "buckling" => {
            let g = |k: &str| v.get(k).and_then(J::as_f64).filter(|x| x.is_finite() && *x > 0.0);
            a.members[i].buckling = [g("ky"), g("kz"), g("Ly"), g("Lz")];
            true
        }
        _ => false,
    };
    Ok(if ok { "applied" } else { "invalid" })
}

fn dofs(v: &J) -> Option<[bool; 6]> {
    let s = v.as_str()?;
    if s.len() != 6 || !s.chars().all(|c| c == 'F' || c == 'P') {
        return None;
    }
    let mut r = [false; 6];
    for (o, c) in r.iter_mut().zip(s.chars()) {
        *o = c == 'F';
    }
    Some(r)
}

fn springs6(v: &J) -> Option<[f64; 6]> {
    let a = v.as_array().filter(|a| a.len() == 6)?;
    let mut r = [0.0; 6];
    for (o, x) in r.iter_mut().zip(a) {
        *o = x.as_f64().filter(|x| x.is_finite() && *x >= 0.0)?;
    }
    Some(r)
}

fn support(a: &mut Analytical, o: &AnOverride, tol: Tolerances) -> &'static str {
    let group = format!("{}@", o.target);
    let mut hits: Vec<usize> = (0..a.supports.len())
        .filter(|&i| a.supports[i].key == o.target || a.supports[i].key.starts_with(&group))
        .collect();
    let mut re = false;
    if hits.is_empty() {
        // a user support on a node: `s:<node key>`
        let Some(nk) = o.target.strip_prefix("s:").filter(|k| k.starts_with("n:")) else {
            return "orphaned";
        };
        let Some((n, r)) = find_node(a, nk, o.anchor.as_ref(), tol) else {
            return "orphaned";
        };
        re = r;
        match a.supports.iter().position(|s| s.node == n) {
            Some(i) => hits.push(i),
            None => {
                if o.field == "enabled" {
                    return "applied";
                }
                a.supports.push(AnSupport {
                    node: n,
                    kind: Release::Fixed,
                    from: ("user", 0),
                    key: o.target.clone(),
                    fixed: [true; 6],
                    springs: [0.0; 6],
                });
                hits.push(a.supports.len() - 1);
            }
        }
    }
    let ok = match o.field.as_str() {
        "dofs" => match dofs(&o.value) {
            Some(d) => {
                for &i in &hits {
                    a.supports[i].fixed = d;
                }
                true
            }
            None => false,
        },
        "springs" => match springs6(&o.value) {
            Some(k) => {
                for &i in &hits {
                    a.supports[i].springs = k;
                }
                true
            }
            None => false,
        },
        "enabled" => {
            let mut k = 0;
            a.supports.retain(|_| {
                k += 1;
                !hits.contains(&(k - 1))
            });
            true
        }
        // springs from footing geometry and subgrade modulus: deferred
        "subgrade" => return "deferred",
        _ => false,
    };
    match (ok, re) {
        (false, _) => "invalid",
        (true, true) => "re_anchored",
        (true, false) => "applied",
    }
}

fn panel(a: &mut Analytical, o: &AnOverride) -> &'static str {
    let Some(j) = a.panels.iter().position(|p| p.key == o.target) else {
        return "orphaned";
    };
    let v = &o.value;
    let ok = match o.field.as_str() {
        "role" => {
            const ROLES: [&str; 6] =
                ["load_only", "line_support", "shell", "plate", "membrane", "excluded"];
            match v.as_str().and_then(|s| ROLES.iter().find(|r| **r == s)) {
                Some(r) => {
                    a.panels[j].role = r;
                    true
                }
                None => false,
            }
        }
        "span" => match v.get("way").and_then(J::as_i64) {
            Some(w @ (1 | 2)) => {
                let deg = v.get("deg").and_then(J::as_f64).filter(|x| x.is_finite());
                a.panels[j].span = Some((w as u8, deg.unwrap_or(0.0)));
                true
            }
            _ => false,
        },
        "mesh" => match v.get("h").and_then(J::as_f64).filter(|h| *h > 0.0) {
            Some(h) => {
                a.panels[j].mesh = Some(h);
                true
            }
            None => false,
        },
        _ => false,
    };
    if ok {
        "applied"
    } else {
        "invalid"
    }
}

/// E of a material: row value, else the class default.
pub(crate) fn has_modulus(db: &Connection, name: &str) -> Result<bool, Error> {
    if name.is_empty() {
        return Ok(false);
    }
    let e: Option<Option<f64>> = rusqlite::OptionalExtension::optional(db.query_row(
        "SELECT e_mod FROM material WHERE name=?1",
        [name],
        |r| r.get(0),
    ))
    .unwrap_or(None);
    Ok(e.flatten().is_some() || crate::mat_mech::mech_defaults(name).is_some())
}

/// Role rules, the legacy release view, and the blocking issues `no_section`, `no_material`.
fn finish(db: &Connection, a: &mut Analytical, _tol: Tolerances) -> Result<(), Error> {
    let mut extra = vec![];
    for m in &mut a.members {
        if m.role == "truss" {
            // axial only: bending free at both ends, torsion free at the end
            m.releases[0][4] = true;
            m.releases[0][5] = true;
            m.releases[1][3] = true;
            m.releases[1][4] = true;
            m.releases[1][5] = true;
        }
        let rel = |r: &[bool; 6]| if r[4] && r[5] { Release::Pinned } else { Release::Fixed };
        m.release = (rel(&m.releases[0]), rel(&m.releases[1]));
        if m.role == "excluded" {
            continue;
        }
        if m.section.is_none() {
            extra.push(AnIssue { code: "no_section", kind: m.kind, id: m.id, node: None });
        }
        if !has_modulus(db, &m.material)? {
            extra.push(AnIssue { code: "no_material", kind: m.kind, id: m.id, node: None });
        }
    }
    for s in &mut a.supports {
        s.kind = if s.fixed.iter().all(|f| *f) { Release::Fixed } else { Release::Pinned };
    }
    a.issues.extend(extra);
    Ok(())
}

#[cfg(test)]
#[path = "analytical_keys_tests.rs"]
mod tests;
