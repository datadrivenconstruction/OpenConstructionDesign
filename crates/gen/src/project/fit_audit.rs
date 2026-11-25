// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! An audit of a furnished storey from what it holds: the fixtures read back through the
//! shared transform, walls, door swings, windows and columns from the layout, needs from the
//! room rules. It keeps no state of the placer, so it checks the placer.

use super::fit_rules::{hit, inside, Org, Placed, RoomFacts, Rules, Step, EPS, R};
use super::furnish::{rect_dist, triangle_ok};
use super::{PStorey, RoomKind, RoomTagTable, Typology};
use crate::{GFixture, GLayout};

/// The wall's own rectangle.
fn wall_rect(l: &GLayout, k: usize) -> R {
    let w = &l.walls[k];
    let h = w.role.thickness() / 2.0;
    [w.a.0.min(w.b.0) - h, w.a.1.min(w.b.1) - h, w.a.0.max(w.b.0) + h, w.a.1.max(w.b.1) + h]
}

fn bbox(p: &[(f64, f64)]) -> R {
    p.iter().fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |b, q| {
        [b[0].min(q.0), b[1].min(q.1), b[2].max(q.0), b[3].max(q.1)]
    })
}

/// The room rectangle less the walls standing on its edges.
fn clear_of(l: &GLayout, r: &R) -> R {
    let mut c = *r;
    for k in 0..l.walls.len() {
        let wr = wall_rect(l, k);
        let w = &l.walls[k];
        if (w.a.1 - w.b.1).abs() < EPS && wr[0] < r[2] - EPS && r[0] < wr[2] - EPS {
            if (w.a.1 - r[1]).abs() < EPS {
                c[1] = c[1].max(wr[3]);
            }
            if (w.a.1 - r[3]).abs() < EPS {
                c[3] = c[3].min(wr[1]);
            }
        }
        if (w.a.0 - w.b.0).abs() < EPS && wr[1] < r[3] - EPS && r[1] < wr[3] - EPS {
            if (w.a.0 - r[0]).abs() < EPS {
                c[0] = c[0].max(wr[2]);
            }
            if (w.a.0 - r[2]).abs() < EPS {
                c[2] = c[2].min(wr[0]);
            }
        }
    }
    c
}

/// Openings on the room's edges: the plan strip they take on the room side, `deep` into it.
fn openings(l: &GLayout, r: &R, door: bool, deep: impl Fn(f64) -> f64) -> Vec<R> {
    let mut out = vec![];
    for o in l.openings.iter().filter(|o| o.door == door) {
        let w = &l.walls[o.wall];
        let len = (w.b.0 - w.a.0).hypot(w.b.1 - w.a.1);
        if len < EPS {
            continue;
        }
        let u = ((w.b.0 - w.a.0) / len, (w.b.1 - w.a.1) / len);
        // on an arc wall the ends sit on the arc, over the chord a -> b this strip is cut on
        let (p0, p1) = (w.at(o.along), w.at(o.along + o.width));
        let h = w.role.thickness() / 2.0;
        let d = deep(o.width);
        if u.1.abs() < EPS {
            let (a, b) = (p0.0.min(p1.0).max(r[0]), p0.0.max(p1.0).min(r[2]));
            if b - a < EPS {
                continue;
            }
            if (w.a.1 - r[1]).abs() < EPS {
                out.push([a, r[1] + h, b, r[1] + h + d]);
            } else if (w.a.1 - r[3]).abs() < EPS {
                out.push([a, r[3] - h - d, b, r[3] - h]);
            }
        } else {
            let (a, b) = (p0.1.min(p1.1).max(r[1]), p0.1.max(p1.1).min(r[3]));
            if b - a < EPS {
                continue;
            }
            if (w.a.0 - r[0]).abs() < EPS {
                out.push([r[0] + h, a, r[0] + h + d, b]);
            } else if (w.a.0 - r[2]).abs() < EPS {
                out.push([r[2] - h - d, a, r[2] - h, b]);
            }
        }
    }
    out
}

/// A placed piece of a room.
struct Piece<'a> {
    id: &'a str,
    item: usize,
    pl: Placed,
    f: &'a GFixture,
}

/// Every problem of the storey's fit-out; empty when it is sound. `who`: the typology and
/// whether the building is one household.
pub(crate) fn audit(st: &PStorey, who: (Typology, bool), rules: &Rules, tags: &RoomTagTable) -> Vec<String> {
    let l = &st.layout;
    let mut bad = vec![];
    let mut pieces: Vec<Piece<'_>> = vec![];
    // diffusers, grilles and ceiling lights belong to the services; item_of skips the rest
    for f in l.fixtures.iter().filter(|f| !matches!(f.family, "Supply Diffuser" | "Exhaust Grille" | "Ceiling Light")) {
        match rules.item_of(f) {
            Some(it) => {
                let item = rules.item_ix(&it.id).unwrap_or(usize::MAX);
                pieces.push(Piece { id: &it.id, item, pl: Placed::of(it, f), f });
            }
            None => {}
        }
    }
    let rects: Vec<R> = l.rooms.iter().map(|r| bbox(&r.polygon)).collect();
    let walls: Vec<R> = (0..l.walls.len()).map(|k| wall_rect(l, k)).collect();
    let columns: Vec<R> = l
        .columns
        .iter()
        .map(|c| [c.at.0 - c.section.0 / 2.0, c.at.1 - c.section.1 / 2.0, c.at.0 + c.section.0 / 2.0, c.at.1 + c.section.1 / 2.0])
        .collect();
    let room_of = |b: &R| {
        let c = ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0);
        rects.iter().position(|r| c.0 > r[0] && c.0 < r[2] && c.1 > r[1] && c.1 < r[3])
    };
    let floor = |p: &Piece<'_>| rules.items[p.item].on.is_empty() && !rules.items[p.item].raised;
    let has_body = |p: &Piece<'_>| rules.items[p.item].on.is_empty();
    for (i, p) in pieces.iter().enumerate() {
        let it = &rules.items[p.item];
        let name = format!("{} at ({:.2}, {:.2})", p.id, p.f.at.0, p.f.at.1);
        let Some(ri) = room_of(&p.pl.body) else {
            bad.push(format!("{name}: in no room"));
            continue;
        };
        let room = rects[ri];
        let clear = clear_of(l, &room);
        let small_wet = matches!(st.rooms[ri].kind, RoomKind::Wc | RoomKind::Bath)
            && (room[2] - room[0]) * (room[3] - room[1]) < rules.outward_below;
        let doors = openings(l, &room, true, |w| if small_wet { 0.3 } else { w.min(1.2) });
        let body = p.pl.body;
        if has_body(p) {
            if !inside(&body, &clear) || walls.iter().any(|w| hit(w, &body)) {
                bad.push(format!("{name}: body out of the room"));
            }
            if columns.iter().any(|c| hit(c, &body)) {
                bad.push(format!("{name}: body on a column"));
            }
            if doors.iter().any(|d| hit(d, &body)) {
                bad.push(format!("{name}: body in a door swing"));
            }
            for q in &pieces[i + 1..] {
                if has_body(q) && hit(&q.pl.body, &body) {
                    bad.push(format!("{name}: body on {}", q.id));
                }
            }
        } else if !pieces.iter().any(|q| rules.items[q.item].on.is_empty() && it.on.contains(&q.item) && inside(&body, &q.pl.body)) {
            let hosts: Vec<&str> = it.on.iter().map(|&h| rules.items[h].id.as_str()).collect();
            bad.push(format!("{name}: not on its {}", hosts.join(" or ")));
        }
        // free space: in the room, no floor body but its own (or its host's) in it
        let free = |r: &R| {
            inside(r, &clear)
                && !walls.iter().chain(&columns).any(|w| hit(w, r))
                && !pieces.iter().any(|q| floor(q) && !hit(&q.pl.body, &body) && hit(&q.pl.body, r))
        };
        if let Some(fb) = p.pl.front_band(it) {
            if !free(&fb) {
                bad.push(format!("{name}: front space taken"));
            }
        }
        let sides = p.pl.side_bands(it);
        if !sides.is_empty() && sides.iter().filter(|b| free(b)).count() < it.side.2 as usize {
            bad.push(format!("{name}: side space taken"));
        }
        if it.tall {
            for w in openings(l, &room, false, |_| 1.0) {
                if hit(&w, &body) {
                    bad.push(format!("{name}: tall piece before a window"));
                }
            }
        }
        if it.org == Org::Wall {
            let on_face = p.f.wall.is_some_and(|k| {
                let w = &l.walls[k];
                let h = w.role.thickness() / 2.0;
                let back = (p.f.at.0 - p.pl.front.0 * h, p.f.at.1 - p.pl.front.1 * h);
                let wr = walls[k];
                back.0 >= wr[0] + h - 1e-6 && back.0 <= wr[2] - h + 1e-6 && back.1 >= wr[1] + h - 1e-6 && back.1 <= wr[3] - h + 1e-6
            });
            if !on_face {
                bad.push(format!("{name}: not on its host wall"));
            }
        }
    }
    // every room a rule set matches is fitted out
    for i in 0..l.rooms.len().min(st.rooms.len()) {
        // a smoke lobby stays empty: nothing may burn or block it
        if l.rooms[i].name == super::fire::SMOKE_LOBBY {
            continue;
        }
        let (facts, clear) = facts_of(st, who, tags, &rects, i);
        let roomy = clear[2] - clear[0] >= 0.6 && clear[3] - clear[1] >= 0.6;
        if roomy && rules.rooms.iter().any(|r| r.matches(&facts)) && !st.fit.iter().any(|f| f.room == i) {
            bad.push(format!("room {i} {}: not fitted out", l.rooms[i].name));
        }
    }
    for fit in &st.fit {
        bad.extend(room_audit(st, (who, tags), rules, fit, &pieces, &rects, &walls, &columns).into_iter().map(|e| {
            format!("room {} {}: {e}", fit.room, l.rooms.get(fit.room).map_or("", |r| r.name.as_str()))
        }));
    }
    bad
}

/// Every fixture connector the services leave open: with water runs, each water and drain
/// connector has a link; with ducts, each air outlet (a cooker hood) has one. Empty when sound.
pub(crate) fn connections(p: &super::GProject) -> Vec<String> {
    use super::SvcSys;
    let water = !p.services.water.is_empty();
    let air = !p.hvac.ducts.is_empty();
    let mut bad = vec![];
    for (k, st) in p.storeys.iter().enumerate() {
        for (i, f) in st.layout.fixtures.iter().enumerate() {
            let linked = |c: &str| p.services.links.iter().any(|l| l.storey == k && l.fixture == i && l.connector == c);
            for c in super::svc_ports(f).iter().filter(|c| water && c.sys != SvcSys::Power && !c.source) {
                if !linked(c.name) {
                    bad.push(format!("floor {}: {} {} {} not connected", k + 1, f.family, f.ty, c.name));
                }
            }
            for (c, ..) in super::svc::air_outlets(f).into_iter().filter(|_| air) {
                if !p.hvac.links.iter().any(|l| l.storey == k && l.fixture == i && l.connector == c) {
                    bad.push(format!("floor {}: {} {} {c} not ducted", k + 1, f.family, f.ty));
                }
            }
        }
    }
    bad
}

/// What the rules match a room by, and its clear rectangle.
fn facts_of<'a>(st: &'a PStorey, (typology, one_household): (Typology, bool), tags: &RoomTagTable, rects: &[R], i: usize) -> (RoomFacts<'a>, R) {
    let clear = clear_of(&st.layout, &rects[i]);
    let pr = &st.rooms[i];
    let facts = RoomFacts {
        kind: pr.kind,
        dwelling: super::furnish::dwelling_of(pr.owner, one_household).is_some(),
        typology,
        area: (clear[2] - clear[0]) * (clear[3] - clear[1]),
        width: (clear[2] - clear[0]).min(clear[3] - clear[1]),
        height: st.height,
        name: &st.layout.rooms[i].name,
        tags: tags.of(pr.kind, &st.layout.rooms[i].name),
    };
    (facts, clear)
}

#[allow(clippy::too_many_arguments)]
fn room_audit(
    st: &PStorey,
    (who, tags): ((Typology, bool), &RoomTagTable),
    rules: &Rules,
    fit: &super::RoomFit,
    pieces: &[Piece<'_>],
    rects: &[R],
    walls: &[R],
    columns: &[R],
) -> Vec<String> {
    let mut bad = fit.issues.clone();
    let Some(room) = rects.get(fit.room).filter(|_| fit.room < st.rooms.len()) else {
        return vec!["no such room".into()];
    };
    let typology = who.0;
    let (facts, clear) = facts_of(st, who, tags, rects, fit.room);
    let area = facts.area;
    let Some(rule) = rules.rule(&fit.rule) else {
        return vec![format!("unknown rule {}", fit.rule)];
    };
    if !rule.matches(&facts) {
        bad.push(format!("rule {} does not match the room", rule.id));
    }
    let mine: Vec<&Piece<'_>> = pieces
        .iter()
        .filter(|p| {
            let b = p.pl.body;
            let c = ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0);
            c.0 > room[0] && c.0 < room[2] && c.1 > room[1] && c.1 < room[3]
        })
        .collect();
    let count = |items: &[usize]| mine.iter().filter(|p| items.contains(&p.item)).count();
    let keys = |sets: &[usize]| sets.iter().map(|&s| rules.sets[s].key()).collect::<Vec<_>>();
    let dwelling = facts.dwelling;
    needs(rules, &rule.steps, area, (dwelling, typology), fit, &count, &keys, &mut bad);
    // turning circles: free of floor bodies, walls and columns
    for (c, d) in &fit.turning {
        let r = d / 2.0;
        let blocked = mine
            .iter()
            .filter(|p| rules.items[p.item].on.is_empty() && !rules.items[p.item].raised)
            .map(|p| &p.pl.body)
            .chain(walls)
            .chain(columns)
            .any(|b| rect_dist(b, *c) < r - 1e-6);
        if blocked || c.0 - r < clear[0] - EPS || c.0 + r > clear[2] + EPS || c.1 - r < clear[1] - EPS || c.1 + r > clear[3] + EPS {
            bad.push(format!("turning circle {d} m at ({:.2}, {:.2}) not free", c.0, c.1));
        }
    }
    // drains: every drained piece has its branch, within the rules from the stack
    let dr = rules.drainage;
    let drained: Vec<&&Piece<'_>> = mine.iter().filter(|p| rules.items[p.item].drain.is_some()).collect();
    if !drained.is_empty() {
        match fit.stack {
            None => bad.push("drained pieces without a stack".into()),
            Some(s) => {
                if s.0 < clear[0] - EPS || s.0 > clear[2] + EPS || s.1 < clear[1] - EPS || s.1 > clear[3] + EPS {
                    bad.push("stack outside the room".into());
                }
                for p in &drained {
                    let b = p.pl.body;
                    let run = ((b[0] + b[2]) / 2.0 - s.0).abs() + ((b[1] + b[3]) / 2.0 - s.1).abs();
                    let max = rules.items[p.item].drain.map_or(0.0, |d| d.max_run);
                    if run > max + EPS || run * dr.fall > dr.max_drop + EPS {
                        bad.push(format!("{}: branch {run:.2} m to the stack", p.id));
                    }
                }
            }
        }
        if fit.drains.len() != drained.len() {
            bad.push(format!("{} drains reported for {} drained pieces", fit.drains.len(), drained.len()));
        }
    }
    for f in &fit.falls {
        if *f > dr.max_floor_drop + EPS {
            bad.push(format!("floor fall drop {f:.3} m"));
        }
    }
    // the work triangle, from the pieces themselves
    for s in all_steps(&rule.steps) {
        let Step::Run { run, need, .. } = s else {
            continue;
        };
        let Some(t) = &rules.runs[*run].triangle else {
            continue;
        };
        if *need == 0 || area + EPS < t.from_area {
            continue;
        }
        let at = |i: usize| -> Vec<(f64, f64)> {
            mine.iter()
                .filter(|p| p.item == i)
                .map(|p| ((p.pl.body[0] + p.pl.body[2]) / 2.0, (p.pl.body[1] + p.pl.body[3]) / 2.0))
                .collect()
        };
        let d = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).hypot(a.1 - b.1);
        let (a, b, c) = (at(t.items[0]), at(t.items[1]), at(t.items[2]));
        let ok = a.iter().any(|&p| b.iter().any(|&q| c.iter().any(|&r| triangle_ok(&[d(p, q), d(q, r), d(r, p)], t.leg, t.sum))));
        if !ok {
            bad.push(format!("work triangle of {} not met", rules.runs[*run].id));
        }
    }
    bad
}

/// Steps with those inside `choose` options.
fn all_steps(steps: &[Step]) -> Vec<&Step> {
    let mut out = vec![];
    for s in steps {
        out.push(s);
        if let Step::Choose(opts) = s {
            for o in opts {
                out.extend(all_steps(o));
            }
        }
    }
    out
}

/// What the steps ask for, counted in the room; for `choose`, one option must be met.
#[allow(clippy::too_many_arguments)]
fn needs(
    rules: &Rules,
    steps: &[Step],
    area: f64,
    (dwelling, typology): (bool, Typology),
    fit: &super::RoomFit,
    count: &dyn Fn(&[usize]) -> usize,
    keys: &dyn Fn(&[usize]) -> Vec<usize>,
    bad: &mut Vec<String>,
) {
    for s in steps {
        match s {
            Step::Place { what, need, once, min_area, typologies, when, .. } => {
                let here = *when && (typologies.is_empty() || typologies.contains(&typology));
                if area + EPS < *min_area || (*once && dwelling) || *need == 0 || !here {
                    continue;
                }
                let k = keys(what);
                if count(&k) < *need as usize {
                    bad.push(format!("{}: {} of {need}", rules.items[k[0]].id, count(&k)));
                }
            }
            Step::Grid { set, need, min_area, .. } => {
                let k = keys(&[*set]);
                if area + EPS >= *min_area && count(&k) < *need as usize {
                    bad.push(format!("{}: {} of {need}", rules.items[k[0]].id, count(&k)));
                }
            }
            Step::Attach { item, to, max, need, .. } if *need > 0 => {
                let want = count(to.as_slice()).min(*max as usize);
                if count(&[*item]) < want {
                    bad.push(format!("{}: {} of {want}", rules.items[*item].id, count(&[*item])));
                }
            }
            Step::Bays { set, need, .. } => {
                let k = keys(&[*set]);
                if count(&k) < *need as usize {
                    bad.push(format!("{}: {} of {need} bays", rules.items[k[0]].id, count(&k)));
                }
            }
            Step::Run { run, need, .. } if *need > 0 => {
                // one of the sequences with every module it cannot do without
                let run = &rules.runs[*run];
                let whole = run.seqs.iter().any(|seq| {
                    seq.iter().all(|slot| match slot {
                        super::fit_rules::Slot::Module { set, optional: false } => count(&keys(&[*set])) > 0,
                        _ => true,
                    })
                });
                if !whole {
                    bad.push(format!("run {} incomplete", run.id));
                }
            }
            Step::Turning { d, need } => {
                if fit.turning.iter().filter(|t| (t.1 - d).abs() < EPS).count() < *need as usize {
                    bad.push(format!("turning circle {d} m missing"));
                }
            }
            Step::Choose(opts) => {
                let met = opts.iter().any(|o| {
                    let mut b = vec![];
                    needs(rules, o, area, (dwelling, typology), fit, count, keys, &mut b);
                    b.is_empty()
                });
                if !met {
                    bad.push("no option of a choice is met".into());
                }
            }
            _ => {}
        }
    }
}
