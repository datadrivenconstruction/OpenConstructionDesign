// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Connections that follow an edit. A connection is geometric: a run end lies on a device
//! connector, or on another run. When a device moves or turns, or a run is lifted, the ends that
//! touched the old place move to the new one in the same journal record; an end that cannot
//! follow refuses the whole edit with a reason (`mep.err.follow_*`), so nothing is left torn.

use std::cell::{Cell, RefCell};

use ddc_geom::math::{v3, V3};
use ddc_geom::mep::{apply_slope, Domain, SystemKind};

use super::check::min_fall;
use super::manual::same_net;
use super::{check_spec, nearest_on, p3, read_run, MepIssue, RunSpec, WorldPort, PORT_TOL, TOL};
use crate::{CompHost, CompSpec, Error, Model, Target};

/// One connection point: where it was, where it goes, and the network type of the thing moved.
type Move = (V3, V3, Domain);

/// Where a run would go if the edit under the cursor were committed.
#[derive(Clone, Debug, PartialEq)]
pub struct FollowGhost {
    pub run: i64,
    pub points: Vec<(f64, f64, f64)>,
}

struct Planned {
    id: i64,
    spec: RunSpec,
}

thread_local! {
    static FOLLOWED: Cell<bool> = const { Cell::new(false) };
    static PROBLEMS: RefCell<Vec<(i64, MepIssue)>> = const { RefCell::new(Vec::new()) };
}

/// Network problems the last follow introduced (system, finding); empty when none. Taking
/// clears them, so the status line shows each follow once.
pub fn take_follow_problems() -> Vec<(i64, MepIssue)> {
    PROBLEMS.with(|p| std::mem::take(&mut *p.borrow_mut()))
}

fn blocked(key: &str) -> Error {
    Error::Invalid(key.into())
}

fn at(p: (f64, f64, f64)) -> V3 {
    v3(p.0, p.1, p.2)
}

fn comp(v: V3, k: usize) -> f64 {
    [v.x, v.y, v.z][k]
}

fn with(v: V3, k: usize, t: f64) -> V3 {
    match k {
        0 => v3(t, v.y, v.z),
        1 => v3(v.x, t, v.z),
        _ => v3(v.x, v.y, t),
    }
}

/// The world axis a leg runs along, if it runs along one.
fn axis_of(d: V3) -> Option<usize> {
    let a = [d.x.abs(), d.y.abs(), d.z.abs()];
    let k = (0..3).max_by(|&i, &j| a[i].total_cmp(&a[j]))?;
    let rest: f64 = (0..3).filter(|&i| i != k).map(|i| a[i]).sum();
    (a[k] > TOL && rest <= 1e-6).then_some(k)
}

/// Axis-parallel legs from `a` to `b` (excluding `a`); the `last` axis is travelled last.
fn stair(a: V3, b: V3, last: Option<usize>) -> Vec<V3> {
    let mut order: Vec<usize> = (0..3).filter(|k| Some(*k) != last).collect();
    order.extend(last);
    let (mut cur, mut out) = (a, vec![]);
    for k in order {
        if (comp(cur, k) - comp(b, k)).abs() > 1e-9 {
            cur = with(cur, k, comp(b, k));
            out.push(cur);
        }
    }
    out
}

/// Drops repeated points and points in the middle of a straight stretch.
fn tidy(p: Vec<V3>) -> Vec<V3> {
    let mut out: Vec<V3> = Vec::with_capacity(p.len());
    for q in p {
        if out.last().is_some_and(|l| (*l - q).len() < 1e-9) {
            continue;
        }
        while out.len() >= 2 {
            let (a, b) = (out[out.len() - 2], out[out.len() - 1]);
            let (d1, d2) = (b - a, q - b);
            if d1.len() > 1e-9
                && d2.len() > 1e-9
                && d1.normalized().dot(d2.normalized()) > 1.0 - 1e-9
            {
                out.pop();
            } else {
                break;
            }
        }
        out.push(q);
    }
    out
}

/// Path `p` (moved end first) with its first point sent to `nw`. The leg next to the end keeps
/// its direction: the following corner shifts sideways with the end. When both that leg and the
/// next one run along world axes, the next leg stays on an axis too: the sideways shift is
/// undone by axis-parallel legs (elbows) back to the old corner.
pub(super) fn move_end(p: &[V3], nw: V3, elbows: bool) -> Result<Vec<V3>, Error> {
    let mut out = p.to_vec();
    out[0] = nw;
    if p.len() < 3 {
        return Ok(out);
    }
    let d0 = p[1] - p[0];
    if d0.len() <= TOL {
        return Ok(out);
    }
    let u = d0.normalized();
    let delta = nw - p[0];
    let perp = delta - u * delta.dot(u);
    out[1] = p[1] + perp;
    if (out[1] - out[0]).dot(u) <= TOL {
        return Err(blocked("mep.err.follow_blocked"));
    }
    if !elbows || perp.len() <= TOL || axis_of(d0).is_none() {
        return Ok(out);
    }
    let d1 = p[2] - p[1];
    let Some(kb) = axis_of(d1) else {
        return Ok(out);
    };
    let b = d1.normalized();
    if (perp - b * perp.dot(b)).len() <= TOL {
        // the shift runs along the next leg: it only gets longer or shorter
        if (p[2] - out[1]).dot(b) <= TOL {
            return Err(blocked("mep.err.follow_blocked"));
        }
        return Ok(out);
    }
    // keep the shift along the next leg: returning to the old corner would fold the path back
    let corner = p[1] + b * perp.dot(b);
    if (p[2] - corner).dot(b) <= TOL {
        return Err(blocked("mep.err.follow_blocked"));
    }
    let mut w = vec![out[0], out[1]];
    w.extend(stair(out[1], corner, Some(kb)));
    w.extend_from_slice(&p[2..]);
    Ok(tidy(w))
}

fn structural(i: &MepIssue) -> bool {
    matches!(
        i,
        MepIssue::Network { .. }
            | MepIssue::Broken { .. }
            | MepIssue::OpenEnd { .. }
            | MepIssue::NotConnected { .. }
            | MepIssue::WrongSystem { .. }
            | MepIssue::SlopeLow { .. }
            | MepIssue::SlopeReverse { .. }
            | MepIssue::PortApproach { .. }
    )
}

type Sig = (i64, &'static str, Option<i64>, Option<i64>);

fn sig(sys: i64, i: &MepIssue) -> Sig {
    (sys, i.key(), i.run(), i.comp())
}

impl Model {
    fn port_systems(&self, ports: &[WorldPort]) -> Vec<i64> {
        let pts: Vec<V3> = ports.iter().map(|p| at(p.at)).collect();
        self.touched_systems(&pts, &[], None)
    }

    fn ports_before(&self, comps: &[i64]) -> Vec<WorldPort> {
        comps
            .iter()
            .filter_map(|c| self.component_ports(Some(*c)).ok())
            .flatten()
            .collect()
    }

    /// Systems of the runs with an end on one of `pts` (any network type), or on the path
    /// `along`; plus `own`. These are the only networks an edit can touch.
    fn touched_systems(&self, pts: &[V3], along: &[V3], own: Option<i64>) -> Vec<i64> {
        let mut out: Vec<i64> = own.into_iter().collect();
        for id in self.mep_runs(None).unwrap_or_default() {
            let Ok(r) = read_run(&self.db, id) else {
                continue;
            };
            let p = r.pts();
            if p.len() < 2 || out.contains(&r.system) {
                continue;
            }
            let hit = [p[0], p[p.len() - 1]].into_iter().any(|e| {
                pts.iter().any(|q| (e - *q).len() < PORT_TOL)
                    || (along.len() >= 2 && nearest_on(along, e).2 < PORT_TOL)
            });
            if hit {
                out.push(r.system);
            }
        }
        out
    }

    /// Findings that mean a broken network, for the `systems` only.
    fn follow_snapshot(&self, systems: &[i64]) -> Vec<(i64, MepIssue)> {
        let Ok(ports) = self.component_ports(None) else {
            return vec![];
        };
        systems
            .iter()
            .filter_map(|&s| self.mep_check_with_ports(s, &ports).ok())
            .flat_map(|c| {
                let sys = c.system;
                c.issues
                    .into_iter()
                    .filter(structural)
                    .map(move |i| (sys, i))
            })
            .collect()
    }

    fn follow_begin(&self, systems: &[i64]) -> Vec<(i64, MepIssue)> {
        FOLLOWED.with(|f| f.set(false));
        self.follow_snapshot(systems)
    }

    /// After a follow: findings that were not there before wait for `take_follow_problems`.
    fn follow_end(&self, systems: &[i64], pre: Vec<(i64, MepIssue)>, ok: bool) {
        if !ok || !FOLLOWED.with(|f| f.replace(false)) {
            return;
        }
        let new: Vec<(i64, MepIssue)> = self
            .follow_snapshot(systems)
            .into_iter()
            .filter(|(s, i)| !pre.iter().any(|(ps, pi)| sig(*ps, pi) == sig(*s, i)))
            .collect();
        PROBLEMS.with(|p| *p.borrow_mut() = new);
    }

    /// An end that stays put is held by a connector or by another run of the network.
    fn end_pinned(
        &self,
        id: i64,
        dom: Domain,
        e: V3,
        skip: &[i64],
        ports: &mut Option<Vec<WorldPort>>,
    ) -> Result<bool, Error> {
        if ports.is_none() {
            *ports = Some(self.component_ports(None)?);
        }
        if ports
            .iter()
            .flatten()
            .any(|p| same_net(p.domain, dom) && (at(p.at) - e).len() < PORT_TOL)
        {
            return Ok(true);
        }
        for oid in self.mep_runs(None)? {
            if oid == id || skip.contains(&oid) {
                continue;
            }
            let o = read_run(&self.db, oid)?;
            if !same_net(o.section.domain, dom) || o.points.len() < 2 {
                continue;
            }
            if nearest_on(&o.pts(), e).2 < PORT_TOL {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn fall_min(
        &self,
        kinds: &mut Option<Vec<(i64, SystemKind)>>,
        s: &RunSpec,
    ) -> Result<f64, Error> {
        if kinds.is_none() {
            *kinds = Some(
                self.mep_systems()?
                    .into_iter()
                    .map(|x| (x.id, x.kind))
                    .collect(),
            );
        }
        Ok(kinds
            .iter()
            .flatten()
            .find(|k| k.0 == s.system)
            .map_or(0.0, |k| min_fall(k.1, s)))
    }

    /// New paths of the runs whose ends touched `moves[..].0`, without writing anything. A run
    /// with a fall keeps it: with a free far end the far end takes the height change; with a far
    /// end held by something else the fall is refitted between the two fixed ends, and the edit
    /// is refused (`follow_slope`) only when that fall drops below the system's least.
    fn plan_follow(&self, moves: &[Move], skip: &[i64]) -> Result<Vec<Planned>, Error> {
        let mut out = vec![];
        if moves.is_empty() {
            return Ok(out);
        }
        let mut ports: Option<Vec<WorldPort>> = None;
        let mut kinds: Option<Vec<(i64, SystemKind)>> = None;
        for id in self.mep_runs(None)? {
            if skip.contains(&id) {
                continue;
            }
            let mut s = read_run(&self.db, id)?;
            let orig: Vec<V3> = s.points.iter().map(|p| at(*p)).collect();
            let n = orig.len();
            if n < 2 {
                continue;
            }
            // (is the start, where it goes)
            let mut hits: Vec<(bool, V3)> = vec![];
            for (o, nw, dom) in moves {
                if !same_net(s.section.domain, *dom) {
                    continue;
                }
                for start in [true, false] {
                    let e = if start { orig[0] } else { orig[n - 1] };
                    if (e - *o).len() >= PORT_TOL || hits.iter().any(|h| h.0 == start) {
                        continue;
                    }
                    hits.push((start, *nw));
                }
            }
            if hits.is_empty() {
                continue;
            }
            let both = hits.len() == 2;
            let mut np = orig.clone();
            for &(start, to) in &hits {
                if !start {
                    np.reverse();
                }
                np = move_end(&np, to, !(both && n < 4))?;
                if !start {
                    np.reverse();
                }
            }
            if s.slope != 0.0 {
                let last = np.len() - 1;
                let h0 = hits.iter().any(|h| h.0);
                let h1 = hits.iter().any(|h| !h.0);
                let dom = s.section.domain;
                let mut pinned = false;
                if !h0 {
                    pinned |= self.end_pinned(id, dom, orig[0], skip, &mut ports)?;
                }
                if !h1 {
                    pinned |= self.end_pinned(id, dom, orig[n - 1], skip, &mut ports)?;
                }
                // last height = start height + c0 - slope% of h metres of level run
                let flat = apply_slope(&np, 0.0);
                let c0 = flat[last].z - np[0].z;
                let h = flat[last].z - apply_slope(&np, 100.0)[last].z;
                if h > 1e-9 {
                    if (h0 && h1) || pinned {
                        let pct = 100.0 * (np[0].z + c0 - np[last].z) / h;
                        let min = self.fall_min(&mut kinds, &s)?;
                        if !pct.is_finite() || pct + 1e-9 < min {
                            return Err(blocked("mep.err.follow_slope"));
                        }
                        if pct < 1e-9 {
                            s.slope = 0.0;
                            np = flat;
                        } else {
                            s.slope = pct;
                            np = apply_slope(&np, pct);
                        }
                    } else {
                        if !h0 {
                            let z0 = np[last].z - c0 + s.slope / 100.0 * h;
                            np[0] = v3(np[0].x, np[0].y, z0);
                        }
                        np = apply_slope(&np, s.slope);
                    }
                }
            }
            s.points = np.into_iter().map(p3).collect();
            check_spec(&self.db, &s).map_err(|_| blocked("mep.err.follow_blocked"))?;
            let got = s.pts();
            let last = got.len() - 1;
            for &(start, to) in &hits {
                let e = if start { got[0] } else { got[last] };
                if (e - to).len() > PORT_TOL {
                    return Err(blocked("mep.err.follow_slope"));
                }
            }
            out.push(Planned { id, spec: s });
        }
        Ok(out)
    }

    /// Ends of other runs that touched `moves[..].0` go to `.1`, in the caller's journal record.
    fn follow_moves(&mut self, moves: &[Move], skip: &[i64]) -> Result<(), Error> {
        let plan = self.plan_follow(moves, skip)?;
        if plan.is_empty() {
            return Ok(());
        }
        FOLLOWED.with(|f| f.set(true));
        for p in plan {
            self.update_mep_run(p.id, &p.spec)
                .map_err(|_| blocked("mep.err.follow_blocked"))?;
        }
        Ok(())
    }

    fn ghosts(&self, moves: &[Move], skip: &[i64]) -> Result<Vec<FollowGhost>, Error> {
        Ok(self
            .plan_follow(moves, skip)?
            .into_iter()
            .map(|p| FollowGhost {
                run: p.id,
                points: p.spec.pts().into_iter().map(p3).collect(),
            })
            .collect())
    }

    /// Connectors of `before` that moved drag the runs that touched them.
    fn follow_ports(&mut self, before: &[WorldPort]) -> Result<(), Error> {
        let mut mv: Vec<Move> = vec![];
        for b in before {
            let Ok(a) = self.world_port(b.comp, &b.name) else {
                // the connector is gone (type change): a run that sat on it would be torn off
                if !self.touched_systems(&[at(b.at)], &[], None).is_empty() {
                    return Err(blocked("mep.err.follow_port_lost"));
                }
                continue;
            };
            if (at(a.at) - at(b.at)).len() > TOL {
                mv.push((at(b.at), at(a.at), b.domain));
            }
        }
        self.follow_moves(&mv, &[])
    }

    /// Edits a device; runs connected to its connectors follow. One undo step.
    pub fn mep_update_device(&mut self, id: i64, s: &CompSpec) -> Result<(), Error> {
        let before = self.ports_before(&[id]);
        if before.is_empty() {
            return self.update_component(id, s);
        }
        let sys = self.port_systems(&before);
        let pre = self.follow_begin(&sys);
        let r = self.mep_group("comp_edit", |m| {
            m.update_component(id, s)?;
            m.follow_ports(&before)
        });
        self.follow_end(&sys, pre, r.is_ok());
        r
    }

    /// Runs `f` (a move, turn or mirror of `ts`); devices among `ts`, or hosted on walls in
    /// `ts`, take their connected runs along. With `copy` nothing follows: copies start free.
    pub(crate) fn mep_follow<T>(
        &mut self,
        kind: &str,
        ts: &[Target],
        copy: bool,
        f: impl FnOnce(&mut Model) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let mut comps: Vec<i64> = vec![];
        if !copy {
            for t in ts {
                match *t {
                    Target::Component(c) => comps.push(c),
                    Target::Wall(w) => {
                        let mut st = self.db.prepare("SELECT id FROM comp WHERE host_wall=?1")?;
                        let r = st.query_map([w], |r| r.get::<_, i64>(0))?;
                        for c in r {
                            comps.push(c?);
                        }
                    }
                    _ => {}
                }
            }
        }
        let before = self.ports_before(&comps);
        if before.is_empty() {
            return f(self);
        }
        let sys = self.port_systems(&before);
        let pre = self.follow_begin(&sys);
        let r = self.mep_group(kind, |m| {
            let r = f(m)?;
            m.follow_ports(&before)?;
            Ok(r)
        });
        self.follow_end(&sys, pre, r.is_ok());
        r
    }

    /// Where the ends of the runs on run `id` go when the run takes the path of `new`; refuses
    /// when the run ends on a device connector and moves that end.
    fn run_moves(&self, id: i64, dom: Domain, old: &[V3], new: &[V3]) -> Result<Vec<Move>, Error> {
        if old.len() != new.len() || old.iter().zip(new).all(|(a, b)| (*a - *b).len() <= TOL) {
            return Ok(vec![]);
        }
        let last = old.len() - 1;
        let ends_moved: Vec<V3> = [0, last]
            .into_iter()
            .filter(|&e| (old[e] - new[e]).len() > TOL)
            .map(|e| old[e])
            .collect();
        if !ends_moved.is_empty() {
            let ports = self.component_ports(None)?;
            if ports.iter().any(|p| {
                same_net(p.domain, dom)
                    && ends_moved.iter().any(|e| (at(p.at) - *e).len() < PORT_TOL)
            }) {
                return Err(blocked("mep.err.follow_device"));
            }
        }
        let mut mv: Vec<Move> = vec![];
        for oid in self.mep_runs(None)? {
            if oid == id {
                continue;
            }
            let o = read_run(&self.db, oid)?;
            if !same_net(o.section.domain, dom) || o.points.len() < 2 {
                continue;
            }
            let op = o.pts();
            for e in [op[0], op[op.len() - 1]] {
                let (_, k, dist) = nearest_on(old, e);
                if dist >= PORT_TOL {
                    continue;
                }
                let d = old[k + 1] - old[k];
                let t = ((e - old[k]).dot(d) / d.dot(d)).clamp(0.0, 1.0);
                let to = new[k] + (new[k + 1] - new[k]) * t;
                if (to - e).len() > TOL {
                    mv.push((e, to, o.section.domain));
                }
            }
        }
        Ok(mv)
    }

    /// Edits a run; runs that join it follow its new path. A run on a device connector cannot
    /// leave it: that edit is refused, the device moves instead.
    pub fn mep_edit_run(&mut self, id: i64, s: &RunSpec) -> Result<(), Error> {
        let cur = read_run(&self.db, id)?;
        let old = cur.pts();
        let sys = self.touched_systems(&[], &old, Some(cur.system));
        let pre = self.follow_begin(&sys);
        let r = self.mep_group("mep_run_edit", |m| {
            m.update_mep_run(id, s)?;
            let new = read_run(&m.db, id)?.pts();
            let mv = m.run_moves(id, s.section.domain, &old, &new)?;
            m.follow_moves(&mv, &[id])
        });
        self.follow_end(&sys, pre, r.is_ok());
        r
    }

    /// Live preview, nothing written: the runs that would follow run `id` taking the path of
    /// `new`. An error is the refusal the edit would give.
    pub fn mep_follow_ghosts_run(&self, id: i64, new: &RunSpec) -> Result<Vec<FollowGhost>, Error> {
        let old = read_run(&self.db, id)?.pts();
        let new_pts = new.pts();
        let mv = self.run_moves(id, new.section.domain, &old, &new_pts)?;
        self.ghosts(&mv, &[id])
    }

    /// Live preview, nothing written: the runs that would follow device `id` placed as `new`.
    /// Free-standing devices only (a rigid move and turn); others show no ghost.
    pub fn mep_follow_ghosts_device(
        &self,
        id: i64,
        new: &CompSpec,
    ) -> Result<Vec<FollowGhost>, Error> {
        let old = self.component(id)?;
        if !matches!(old.host, CompHost::None)
            || !matches!(new.host, CompHost::None)
            || old.ctype != new.ctype
            || old.level != new.level
            || old.mirror != new.mirror
        {
            return Ok(vec![]);
        }
        let o = self.component_quantities(id)?.placement.origin;
        let dr = new.rotation - old.rotation;
        let (sn, cs) = dr.sin_cos();
        let shift = v3(new.at.0 - old.at.0, new.at.1 - old.at.1, new.at.2 - old.at.2);
        let mut mv: Vec<Move> = vec![];
        for b in self.ports_before(&[id]) {
            let r = at(b.at) - at(o);
            let to = at(o) + v3(cs * r.x - sn * r.y, sn * r.x + cs * r.y, r.z) + shift;
            if (to - at(b.at)).len() > TOL {
                mv.push((at(b.at), to, b.domain));
            }
        }
        self.ghosts(&mv, &[])
    }
}
