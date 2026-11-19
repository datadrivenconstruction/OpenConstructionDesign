//! Locks: parts of a project the user pinned, kept whole through any regeneration. A lock
//! targets a rule, a storey, a room or a zone of a storey, and keeps either its geometry or its
//! program (kinds and areas). It carries a seal taken from the build it was set on, so a later
//! build either reproduces the seal or is not accepted.
//!
//! [`regenerate`] applies a brief change around the locks: the changed brief is built first;
//! when a lock breaks, the changes are applied one by one onto the previous brief, each kept
//! only if every lock still holds. What could not be applied is reported with the locks it
//! would break, never dropped silently, and a lock is never released by a regeneration.

use super::rules;
use super::{build_project, check_project, parse_spec, GProject, PRole, PStorey, Pin, Pins, ProjectSpec, RoomKind};
use serde_json::{json, Map, Value as J};

type P = (f64, f64);

/// What a lock pins.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// A rule keeps its value.
    Rule(String),
    /// Storey index above ground, 0 the ground floor.
    Storey(usize),
    /// A room of a storey, by the number it had when locked.
    Room { storey: usize, number: String },
    /// Every room whose centre lies in the outline.
    Zone { storey: usize, polygon: Vec<P> },
}

/// What of the target is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keep {
    /// Outlines, walls and openings to the millimetre.
    Geometry,
    /// Room kinds and areas (within 1 %), wherever the rooms are.
    Program,
}

impl Keep {
    pub fn name(self) -> &'static str {
        match self {
            Keep::Geometry => "geometry",
            Keep::Program => "program",
        }
    }
}

/// A pinned part with the seal it must keep.
#[derive(Clone, Debug, PartialEq)]
pub struct Lock {
    pub target: Target,
    pub keep: Keep,
    /// What the target was when locked: a rule value, or room kinds with outlines or areas.
    pub seal: J,
}

/// FNV-1a: stable across builds and versions, unlike the std hasher.
fn fnv(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

fn mm(x: f64) -> i64 {
    (x * 1000.0).round() as i64
}

fn area(poly: &[P]) -> f64 {
    let n = poly.len();
    let s: f64 = (0..n).map(|i| poly[i].0 * poly[(i + 1) % n].1 - poly[(i + 1) % n].0 * poly[i].1).sum();
    s.abs() / 2.0
}

fn inside(p: P, poly: &[P]) -> bool {
    let mut c = false;
    let n = poly.len();
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < (b.0 - a.0) * (p.1 - a.1) / (b.1 - a.1) + a.0 {
            c = !c;
        }
    }
    c
}

fn centre(poly: &[P]) -> P {
    let n = poly.len().max(1) as f64;
    (poly.iter().map(|p| p.0).sum::<f64>() / n, poly.iter().map(|p| p.1).sum::<f64>() / n)
}

/// A room's outline to the millimetre, from its lowest corner, so a renumbered or re-started
/// outline reads the same.
fn outline(poly: &[P]) -> String {
    let mut pts: Vec<(i64, i64)> = poly.iter().map(|p| (mm(p.0), mm(p.1))).collect();
    pts.dedup();
    // counter-clockwise, without points on a straight run: the same room drawn by another
    // builder or split by a neighbour's wall reads the same
    let n = pts.len();
    let twice: i128 = (0..n)
        .map(|i| {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            i128::from(a.0) * i128::from(b.1) - i128::from(b.0) * i128::from(a.1)
        })
        .sum();
    if twice < 0 {
        pts.reverse();
    }
    let straight = |a: (i64, i64), b: (i64, i64), c: (i64, i64)| {
        i128::from(b.0 - a.0) * i128::from(c.1 - b.1) == i128::from(b.1 - a.1) * i128::from(c.0 - b.0)
    };
    let mut k = 0;
    while pts.len() > 3 && k < pts.len() {
        let n = pts.len();
        if straight(pts[(k + n - 1) % n], pts[k], pts[(k + 1) % n]) {
            pts.remove(k);
            k = k.saturating_sub(1);
        } else {
            k += 1;
        }
    }
    let start = (0..pts.len()).min_by_key(|&i| pts[i]).unwrap_or(0);
    let turned: Vec<(i64, i64)> = pts[start..].iter().chain(&pts[..start]).copied().collect();
    format!("{turned:?}")
}

/// The geometry seal of a storey: walls, openings and rooms.
fn storey_geometry(st: &PStorey) -> String {
    let l = &st.layout;
    let walls: Vec<_> = l.walls.iter().map(|w| (mm(w.a.0), mm(w.a.1), mm(w.b.0), mm(w.b.1))).collect();
    let ops: Vec<_> = l.openings.iter().map(|o| (o.wall, mm(o.along), mm(o.width), mm(o.height), o.door)).collect();
    let mut rooms: Vec<String> = st.layout.rooms.iter().zip(&st.rooms).map(|(g, r)| format!("{}{}", r.kind.key(), outline(&g.polygon))).collect();
    rooms.sort();
    fnv(&format!("{walls:?}|{ops:?}|{rooms:?}|{}|{}", mm(st.elevation), mm(st.height)))
}

/// `[kind, outline or area]` of the given rooms of a storey, sorted.
fn room_seals(st: &PStorey, keep: Keep, pick: &dyn Fn(usize) -> bool) -> Vec<J> {
    let mut v: Vec<(String, J)> = st
        .layout
        .rooms
        .iter()
        .zip(&st.rooms)
        .enumerate()
        .filter(|(i, _)| pick(*i))
        .map(|(_, (g, r))| match keep {
            Keep::Geometry => (r.kind.key().to_string(), json!(outline(&g.polygon))),
            Keep::Program => (r.kind.key().to_string(), json!((area(&g.polygon) * 100.0).round() / 100.0)),
        })
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.to_string().cmp(&b.1.to_string())));
    v.into_iter().map(|(k, x)| json!([k, x])).collect()
}

/// Whether `want` (sealed rooms) is found among `have`: equal outlines, or equal kinds with
/// areas within 1 %, one room for each.
fn rooms_found(want: &[J], have: &[J], keep: Keep) -> bool {
    let mut used = vec![false; have.len()];
    want.iter().all(|w| {
        let hit = have.iter().enumerate().position(|(i, h)| {
            !used[i]
                && h[0] == w[0]
                && match keep {
                    Keep::Geometry => h[1] == w[1],
                    Keep::Program => {
                        let (a, b) = (h[1].as_f64().unwrap_or(-1.0), w[1].as_f64().unwrap_or(0.0));
                        (a - b).abs() <= 0.01 * b.abs().max(1.0)
                    }
                }
        });
        if let Some(i) = hit {
            used[i] = true;
        }
        hit.is_some()
    })
}

fn storey(p: &GProject, k: usize) -> Result<&PStorey, String> {
    p.storeys.get(k).ok_or(format!("there is no storey {k}"))
}

/// The value of a rule a spec builds with: a rule set value, else the brief field it aliases.
fn rule_value(brief: &J, spec: &ProjectSpec, id: &str) -> Option<J> {
    if let Some(x) = brief["rules"].get(id).filter(|x| !x.is_null()) {
        return Some(x.clone());
    }
    let d = rules::def(id)?;
    if let Some(path) = d.alias.as_deref() {
        let mut x = brief;
        let mut ok = true;
        for k in path.split('.') {
            match x.get(k) {
                Some(y) => x = y,
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok && !x.is_null() {
            return Some(x.clone());
        }
    }
    // values the typology decides live on the spec
    let field = match id {
        "massing.floor_height" => Some(spec.floor_height),
        "depth.band" => Some(spec.band_depth),
        "corridor.width" => Some(spec.corridor),
        "core.stair_width" => Some(spec.stair_width),
        "grid.span" => Some(spec.grid),
        "basement.count" => Some(f64::from(spec.basements)),
        "massing.form" => super::Massing::NAMES.iter().position(|n| *n == spec.design.massing.name()).map(|i| i as f64),
        _ => None,
    };
    field.map(|x| d.value_json(x)).or_else(|| spec.rules.value_json(id))
}

impl Lock {
    /// Seals `target` as it is in a built project.
    pub fn take(target: Target, keep: Keep, brief: &J, spec: &ProjectSpec, p: &GProject) -> Result<Lock, String> {
        let seal = match &target {
            Target::Rule(id) => {
                rules::def(id).ok_or(format!("no rule {id}"))?;
                rule_value(brief, spec, id).ok_or(format!("{id} has no value to keep"))?
            }
            Target::Storey(k) => {
                let st = storey(p, *k)?;
                match keep {
                    Keep::Geometry => json!(storey_geometry(st)),
                    Keep::Program => J::Array(room_seals(st, keep, &|_| true)),
                }
            }
            Target::Room { storey: k, number } => {
                let st = storey(p, *k)?;
                let i = st.rooms.iter().position(|r| r.number == *number).ok_or(format!("storey {k} has no room {number}"))?;
                J::Array(room_seals(st, keep, &|j| j == i))
            }
            Target::Zone { storey: k, polygon } => {
                let st = storey(p, *k)?;
                let pick = |j: usize| inside(centre(&st.layout.rooms[j].polygon), polygon);
                let s = room_seals(st, keep, &pick);
                if s.is_empty() {
                    return Err(format!("the zone holds no room of storey {k}"));
                }
                J::Array(s)
            }
        };
        Ok(Lock { target, keep, seal })
    }

    /// Whether a build keeps the sealed target.
    pub fn holds(&self, brief: &J, spec: &ProjectSpec, p: &GProject) -> bool {
        match &self.target {
            Target::Rule(id) => rule_value(brief, spec, id).as_ref() == Some(&self.seal),
            Target::Storey(k) => match (p.storeys.get(*k), self.keep) {
                (None, _) => false,
                (Some(st), Keep::Geometry) => json!(storey_geometry(st)) == self.seal,
                (Some(st), Keep::Program) => {
                    let have = room_seals(st, self.keep, &|_| true);
                    let want = self.seal.as_array().cloned().unwrap_or_default();
                    have.len() == want.len() && rooms_found(&want, &have, self.keep)
                }
            },
            Target::Room { storey: k, .. } | Target::Zone { storey: k, .. } => match p.storeys.get(*k) {
                None => false,
                Some(st) => {
                    let have = room_seals(st, self.keep, &|_| true);
                    rooms_found(self.seal.as_array().map(Vec::as_slice).unwrap_or(&[]), &have, self.keep)
                }
            },
        }
    }

    /// A short name for messages: `storey 1 geometry`, `rule grid.span`.
    pub fn label(&self) -> String {
        match &self.target {
            Target::Rule(id) => format!("rule {id}"),
            Target::Storey(k) => format!("storey {k} {}", self.keep.name()),
            Target::Room { storey, number } => format!("room {number} of storey {storey} {}", self.keep.name()),
            Target::Zone { storey, .. } => format!("zone of storey {storey} {}", self.keep.name()),
        }
    }

    pub fn to_json(&self) -> J {
        let mut o = Map::new();
        match &self.target {
            Target::Rule(id) => {
                o.insert("rule".into(), json!(id));
            }
            Target::Storey(k) => {
                o.insert("storey".into(), json!(k));
            }
            Target::Room { storey, number } => {
                o.insert("storey".into(), json!(storey));
                o.insert("room".into(), json!(number));
            }
            Target::Zone { storey, polygon } => {
                o.insert("storey".into(), json!(storey));
                o.insert("zone".into(), json!(polygon.iter().map(|p| [p.0, p.1]).collect::<Vec<_>>()));
            }
        }
        o.insert("keep".into(), json!(self.keep.name()));
        o.insert("seal".into(), self.seal.clone());
        J::Object(o)
    }

    pub fn from_json(v: &J) -> Result<Lock, String> {
        let keep = match v["keep"].as_str().unwrap_or("geometry") {
            "geometry" => Keep::Geometry,
            "program" => Keep::Program,
            k => return Err(format!("keep must be geometry or program, not {k}")),
        };
        let storey = || v["storey"].as_u64().map(|k| k as usize).ok_or("storey must be a storey index".to_string());
        let target = if let Some(id) = v["rule"].as_str() {
            Target::Rule(id.to_string())
        } else if let Some(n) = v["room"].as_str() {
            Target::Room { storey: storey()?, number: n.to_string() }
        } else if let Some(z) = v["zone"].as_array() {
            let polygon: Option<Vec<P>> = z.iter().map(|p| Some((p[0].as_f64()?, p[1].as_f64()?))).collect();
            let polygon = polygon.filter(|p| p.len() >= 3).ok_or("zone must be three or more [x, y] points")?;
            Target::Zone { storey: storey()?, polygon }
        } else {
            Target::Storey(storey()?)
        };
        if v["seal"].is_null() {
            return Err(format!("{}: a lock needs the seal it was taken with", Lock { target, keep, seal: J::Null }.label()));
        }
        Ok(Lock { target, keep, seal: v["seal"].clone() })
    }
}

/// The locks a brief carries under `locks`.
pub fn of_brief(v: &J) -> Result<Vec<Lock>, String> {
    v["locks"].as_array().into_iter().flatten().enumerate().map(|(i, l)| Lock::from_json(l).map_err(|e| format!("locks[{i}]: {e}"))).collect()
}

/// A brief change that was not applied: where, what was asked, what stays, and why.
#[derive(Clone, Debug, PartialEq)]
pub struct Refusal {
    /// Keys from the brief root; a rule id is one key.
    pub path: Vec<String>,
    pub asked: J,
    pub kept: J,
    /// The locks it would break, or the build error it caused.
    pub because: Vec<String>,
}

impl Refusal {
    pub fn to_json(&self) -> J {
        json!({"path": self.path.join("/"), "asked": self.asked, "kept": self.kept, "because": self.because})
    }
}

/// A regeneration around locks.
#[derive(Clone, Debug)]
pub struct Regen {
    /// The brief built, the locks kept in it.
    pub brief: J,
    pub spec: ProjectSpec,
    pub project: GProject,
    pub applied: Vec<Vec<String>>,
    pub refused: Vec<Refusal>,
}

fn get_at<'a>(v: &'a J, path: &[String]) -> &'a J {
    path.iter().fold(v, |x, k| &x[k.as_str()])
}

fn set_at(v: &mut J, path: &[String], x: J) {
    let Some((last, head)) = path.split_last() else {
        *v = x;
        return;
    };
    let mut at = v;
    for k in head {
        if !at[k.as_str()].is_object() {
            at[k.as_str()] = J::Object(Map::new());
        }
        at = &mut at[k.as_str()];
    }
    match (at.as_object_mut(), x.is_null()) {
        (Some(o), true) => {
            o.remove(last);
        }
        (Some(o), false) => {
            o.insert(last.clone(), x);
        }
        (None, _) => {}
    }
}

/// Leaf differences from `a` to `b`: objects are walked, anything else is one leaf. `locks`
/// is not a change.
fn diff(a: &J, b: &J, at: &mut Vec<String>, out: &mut Vec<(Vec<String>, J)>) {
    match (a, b) {
        (J::Object(x), J::Object(y)) => {
            let mut keys: Vec<&String> = x.keys().chain(y.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                // what a regeneration kept is its own output, never a change asked
                if at.is_empty() && (k == "locks" || k == "pins") {
                    continue;
                }
                at.push(k.clone());
                diff(x.get(k).unwrap_or(&J::Null), y.get(k).unwrap_or(&J::Null), at, out);
                at.pop();
            }
        }
        _ if a == b => {}
        (_, J::Object(y)) if a.is_null() => {
            // a new object: each of its leaves is a change of its own
            for (k, v) in y {
                at.push(k.clone());
                diff(&J::Null, v, at, out);
                at.pop();
            }
        }
        _ => out.push((at.clone(), b.clone())),
    }
}

/// The brief with every rule lock written in; `refused` gets the asked values it overrides.
fn with_rule_locks(v: &J, locks: &[Lock], refused: &mut Vec<Refusal>) -> J {
    let mut b = v.clone();
    for l in locks {
        let Target::Rule(id) = &l.target else { continue };
        let asked = b["rules"].get(id.as_str()).cloned().or_else(|| {
            let path = rules::def(id)?.alias.clone()?;
            let x = get_at(&b, &path.split('.').map(str::to_string).collect::<Vec<_>>()).clone();
            (!x.is_null()).then_some(x)
        });
        if let Some(a) = asked.filter(|a| *a != l.seal) {
            refused.push(Refusal { path: vec!["rules".into(), id.clone()], asked: a, kept: l.seal.clone(), because: vec![l.label()] });
        }
        if let Some(path) = rules::def(id).and_then(|d| d.alias.clone()) {
            if !path.contains("[]") {
                set_at(&mut b, &path.split('.').map(str::to_string).collect::<Vec<_>>(), J::Null);
            }
        }
        if !b["rules"].is_object() {
            b["rules"] = J::Object(Map::new());
        }
        b["rules"][id.as_str()] = l.seal.clone();
    }
    b
}

type Built = (ProjectSpec, GProject);

fn build(v: &J) -> Result<Built, String> {
    let s = parse_spec(v).map_err(|e| e.to_string())?;
    let p = build_project(&s).map_err(|e| e.to_string())?;
    let g = check_project(&s, &p);
    if !g.passed() {
        return Err(format!("gates: {}", g.failures.join("; ")));
    }
    Ok((s, p))
}

/// The rooms whose outline a lock keeps, as pins on the plan `p` was built on; `None` when no
/// lock keeps a room's outline.
fn pins_of(locks: &[Lock], spec: &ProjectSpec, p: &GProject) -> Option<Pins> {
    let o = spec.origin;
    let mut rooms: Vec<Pin> = vec![];
    for l in locks.iter().filter(|l| l.keep == Keep::Geometry) {
        let (k, number, zone) = match &l.target {
            Target::Room { storey, number } => (*storey, Some(number), None),
            Target::Zone { storey, polygon } => (*storey, None, Some(polygon)),
            _ => continue,
        };
        let st = p.storeys.get(k)?;
        for (g, r) in st.layout.rooms.iter().zip(&st.rooms) {
            let hit = number.is_some_and(|n| r.number == *n) || zone.is_some_and(|z| inside(centre(&g.polygon), z));
            let polygon: Vec<P> = g.polygon.iter().map(|q| (q.0 - o.0, q.1 - o.1)).collect();
            if hit && !rooms.iter().any(|x| x.storey == k && x.polygon == polygon) {
                rooms.push(Pin { storey: k, kind: r.kind, name: g.name.clone(), owner: r.owner, polygon });
            }
        }
    }
    if rooms.is_empty() {
        return None;
    }
    let stair = p
        .storeys
        .iter()
        .flat_map(|st| st.layout.rooms.iter().zip(&st.rooms))
        .find(|(_, r)| r.kind == RoomKind::Stair)
        .map(|(g, _)| g.polygon.iter().map(|q| q.0 - o.0).fold(f64::MAX, f64::min));
    // an apartment with a pinned room keeps its zone: the outline round all its rooms
    let mut flats: Vec<Pin> = vec![];
    for r in &rooms {
        let PRole::Apartment(a) = r.owner else { continue };
        if flats.iter().any(|f| f.storey == r.storey && f.owner == r.owner) {
            continue;
        }
        let st = &p.storeys[r.storey];
        let pts: Vec<P> = st
            .layout
            .rooms
            .iter()
            .zip(&st.rooms)
            .filter(|(_, x)| x.owner == PRole::Apartment(a))
            .flat_map(|(g, _)| g.polygon.iter().map(|q| (q.0 - o.0, q.1 - o.1)))
            .collect();
        let lo = pts.iter().fold((f64::MAX, f64::MAX), |m, q| (m.0.min(q.0), m.1.min(q.1)));
        let hi = pts.iter().fold((f64::MIN, f64::MIN), |m, q| (m.0.max(q.0), m.1.max(q.1)));
        let polygon = vec![lo, (hi.0, lo.1), hi, (lo.0, hi.1)];
        flats.push(Pin { storey: r.storey, kind: RoomKind::Hall, name: String::new(), owner: r.owner, polygon });
    }
    let had = spec.pins.as_ref();
    Some(Pins {
        width: p.footprint.0,
        depth: p.footprint.1,
        south: had.map_or(spec.band_depth, |x| x.south),
        core_x: had.and_then(|x| x.core_x).or(stair),
        rooms,
        flats,
    })
}

/// `v` built with the outline-locked rooms of `base` kept in place and the rest placed freely
/// around them. `Err(None)`: no lock keeps an outline.
/// `v` with the locked rooms written into its `pins`, and its build: the brief alone builds
/// the same plan again.
fn pinned(v: &J, locks: &[Lock], base: &Built) -> Result<(J, Built), Option<String>> {
    // a storey kept to its walls and openings is not drawn again by another layout
    if locks.iter().any(|l| l.keep == Keep::Geometry && matches!(l.target, Target::Storey(_))) {
        return Err(None);
    }
    let pins = pins_of(locks, &base.0, &base.1).ok_or(None)?;
    let mut w = v.clone();
    w["pins"] = pins.to_json();
    let s = parse_spec(&w).map_err(|e| Some(e.to_string()))?;
    let p = build_project(&s).map_err(|e| Some(e.to_string()))?;
    let g = check_project(&s, &p);
    if !g.passed() {
        return Err(Some(format!("gates: {}", g.failures.join("; "))));
    }
    Ok((w, (s, p)))
}

fn broken(locks: &[Lock], v: &J, s: &ProjectSpec, p: &GProject) -> Vec<String> {
    locks.iter().filter(|l| !l.holds(v, s, p)).map(Lock::label).collect()
}

/// Builds `next` with every lock of `locks` kept. `prev` is the brief the locks were sealed
/// on. Errors only when the locks fail on `prev` itself (a seal from another build).
pub fn regenerate(prev: &J, next: &J, locks: &[Lock]) -> Result<Regen, String> {
    let mut refused = vec![];
    let next = with_rule_locks(next, locks, &mut refused);
    let prev = with_rule_locks(prev, locks, &mut vec![]);
    let carry = |mut v: J| {
        if !locks.is_empty() {
            v["locks"] = J::Array(locks.iter().map(Lock::to_json).collect());
        }
        v
    };
    if let Ok((spec, project)) = build(&next) {
        if broken(locks, &next, &spec, &project).is_empty() {
            let mut changes = vec![];
            diff(&prev, &next, &mut vec![], &mut changes);
            return Ok(Regen { brief: carry(next), spec, project, applied: changes.into_iter().map(|c| c.0).collect(), refused });
        }
    }
    let base = build(&prev).map_err(|e| format!("the previous brief no longer builds: {e}"))?;
    let off = broken(locks, &prev, &base.0, &base.1);
    if !off.is_empty() {
        return Err(format!("the locks do not hold on the brief they were taken on: {}", off.join(", ")));
    }
    let mut changes = vec![];
    diff(&prev, &next, &mut vec![], &mut changes);
    // the whole change with the locked rooms kept in place and the rest placed around them
    if let Ok((w, b)) = pinned(&next, locks, &base) {
        if broken(locks, &w, &b.0, &b.1).is_empty() {
            return Ok(Regen { brief: carry(w), spec: b.0, project: b.1, applied: changes.into_iter().map(|c| c.0).collect(), refused });
        }
    }
    let (mut cur, mut built) = (prev.clone(), base);
    let mut applied = vec![];
    for (path, x) in changes {
        let mut cand = cur.clone();
        set_at(&mut cand, &path, x.clone());
        let mut because = match build(&cand) {
            Ok(b) => {
                let off = broken(locks, &cand, &b.0, &b.1);
                if off.is_empty() {
                    cur = cand;
                    built = b;
                    applied.push(path);
                    continue;
                }
                off
            }
            Err(e) => vec![e],
        };
        // worked around: the locked rooms stay, the change is placed around them
        match pinned(&cand, locks, &built) {
            Ok((w, b)) => {
                let off = broken(locks, &w, &b.0, &b.1);
                if off.is_empty() {
                    cur = w;
                    built = b;
                    applied.push(path);
                    continue;
                }
            }
            Err(Some(e)) => because.push(format!("placed around the locks: {e}")),
            Err(None) => {}
        }
        refused.push(Refusal { kept: get_at(&cur, &path).clone(), path, asked: x, because });
    }
    let (spec, project) = built;
    Ok(Regen { brief: carry(cur), spec, project, applied, refused })
}

/// `brief` with every storey except `free` locked to its built geometry, so a change to the
/// free storeys is placed there alone. Locks already in the brief are kept.
pub fn lock_all_but(brief: &J, free: &[usize]) -> Result<J, String> {
    let (spec, p) = build(brief)?;
    let mut all = of_brief(brief)?;
    for k in (0..p.storeys.len()).filter(|k| !free.contains(k)) {
        if all.iter().any(|l| l.target == Target::Storey(k) && l.keep == Keep::Geometry) {
            continue;
        }
        all.push(Lock::take(Target::Storey(k), Keep::Geometry, brief, &spec, &p)?);
    }
    let mut out = brief.clone();
    out["locks"] = J::Array(all.iter().map(Lock::to_json).collect());
    Ok(out)
}
