// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Final circuits and feeders of the boards from what the power design serves: devices grouped
//! by use and room up to the points and load a circuit takes, each sized for its breaker, cable
//! and voltage drop along the carrier route, the residual current devices grouped per board;
//! circuit lines for the plans and cable traces in 3D derived from the circuits.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::OnceLock;

use ddc_geom::math::{v3, V3};

use super::rules::rule;
use super::svc::{self, Room};
use super::{CircuitUse, GBoard, GCircuit, GProject, GRcd, GRun, GTrace, GWire, PRole, ProjectSpec, SvcSys};

/// Percent in a ratio.
const PCT: f64 = 100.0;
const USES: [CircuitUse; 8] = [
    CircuitUse::Lighting,
    CircuitUse::Sockets,
    CircuitUse::Worktop,
    CircuitUse::Wet,
    CircuitUse::Appliance,
    CircuitUse::Feeder,
    CircuitUse::Emergency,
    CircuitUse::Essential,
];
/// Circuit name prefix per use, in `USES` order.
const CODES: [&str; 8] = ["L", "S", "K", "W", "A", "F", "E", "N"];

/// A device the power design served: storey, fixture, use, load W, room tree index, room
/// index in the storey layout, family.
pub(crate) type Served = (usize, usize, CircuitUse, f64, usize, usize, &'static str);

struct Table {
    sections: Vec<f64>,
    iz2: Vec<f64>,
    iz3: Vec<f64>,
    breakers: Vec<f64>,
    types: Vec<(f64, String)>,
}

fn table() -> &'static Table {
    static T: OnceLock<Table> = OnceLock::new();
    T.get_or_init(|| {
        let v: serde_json::Value = serde_json::from_str(include_str!("../../data/cable_table.json")).expect("cable_table.json is valid");
        let nums = |k: &str| -> Vec<f64> { v[k].as_array().expect("cable table list").iter().filter_map(|x| x.as_f64()).collect() };
        let types = v["types"]
            .as_array()
            .expect("cable types")
            .iter()
            .filter_map(|t| Some((t[0].as_f64()?, t[1].as_str()?.to_string())))
            .collect();
        Table { sections: nums("sections"), iz2: nums("iz2"), iz3: nums("iz3"), breakers: nums("breakers"), types }
    })
}

fn volts() -> f64 {
    rule("services.elec.voltage")
}

fn housing(spec: &ProjectSpec) -> bool {
    spec.traits.residential
}

fn num(x: f64) -> String {
    if x.fract().abs() < 1e-9 {
        format!("{x:.0}")
    } else {
        format!("{x}")
    }
}

/// Cable designation, as NYM-J 3x1.5.
pub(crate) fn cable_name(cores: u32, section: f64) -> String {
    let t = table();
    let ty = t.types.iter().find(|(max, _)| section <= *max).or(t.types.last()).map_or("", |x| x.1.as_str());
    format!("{ty} {cores}x{}", num(section))
}

/// Protective device, as B16 or 3xB16.
pub(crate) fn breaker_name(poles: u32, breaker: f64) -> String {
    if poles > 1 {
        format!("{poles}xB{}", num(breaker))
    } else {
        format!("B{}", num(breaker))
    }
}

pub(crate) fn rcd_name(r: &GRcd) -> String {
    format!("RCD{} {}P {}A {}mA A", r.number, r.poles, num(r.rating), num(r.ma))
}

/// Current-carrying capacity of a section, A: two loaded conductors single phase, three in
/// three phases.
pub(crate) fn iz(section: f64, poles: u32) -> Option<f64> {
    let t = table();
    let i = t.sections.iter().position(|s| (s - section).abs() < 1e-9)?;
    Some(if poles > 1 { t.iz3[i] } else { t.iz2[i] })
}

/// Voltage drop, %, of `current` A over `length` m of a copper cable of `section` mm2: out and
/// back on one phase, or a balanced three-phase load; resistance only.
pub fn voltage_drop(poles: u32, length: f64, current: f64, section: f64) -> f64 {
    let kappa = rule("services.elec.kappa_cu");
    if poles > 1 {
        3f64.sqrt() * length * current / (kappa * section) / rule("services.elec.voltage_ll") * PCT
    } else {
        2.0 * length * current / (kappa * section) / volts() * PCT
    }
}

/// Design current, A, of `load` W.
pub fn design_current(poles: u32, load: f64) -> f64 {
    if poles > 1 {
        load / (3f64.sqrt() * rule("services.elec.voltage_ll"))
    } else {
        load / volts()
    }
}

fn breaker_at_least(a: f64) -> f64 {
    let t = table();
    t.breakers.iter().copied().find(|b| *b + 1e-9 >= a).unwrap_or(*t.breakers.last().unwrap_or(&a))
}

/// Least section from `min` whose capacity holds the breaker.
fn section_holding(min: f64, breaker: f64, poles: u32) -> f64 {
    let t = table();
    t.sections.iter().copied().filter(|s| *s + 1e-9 >= min).find(|s| iz(*s, poles).is_some_and(|i| i + 1e-9 >= breaker)).unwrap_or(*t.sections.last().unwrap_or(&min))
}

/// The next section up, if the table has one.
fn section_up(s: f64) -> Option<f64> {
    table().sections.iter().copied().find(|x| *x > s + 1e-9)
}

/// Points counted against a circuit's limit: every device but a switch.
fn point(family: &str) -> usize {
    usize::from(family != "Light Switch")
}

fn diversity(u: CircuitUse) -> f64 {
    match u {
        CircuitUse::Lighting | CircuitUse::Emergency => rule("services.elec.g_lighting"),
        CircuitUse::Appliance | CircuitUse::Feeder => 1.0,
        _ => rule("services.elec.g_sockets"),
    }
}

/// Most points and design load, W, a circuit of this use takes.
fn limits(spec: &ProjectSpec, u: CircuitUse) -> (usize, f64) {
    let margin = rule("services.elec.load_margin") * volts();
    match u {
        CircuitUse::Lighting | CircuitUse::Emergency => (rule("services.elec.points_light") as usize, rule("services.elec.light_circuit_w").min(margin * rule("services.elec.breaker_light"))),
        CircuitUse::Appliance | CircuitUse::Feeder => (usize::MAX, f64::INFINITY),
        _ => {
            let pts = if housing(spec) { rule("services.elec.points_sockets_home") } else { rule("services.elec.points_sockets_work") };
            (pts as usize, margin * rule("services.elec.breaker_other"))
        }
    }
}

/// Whether a circuit is behind a 30 mA residual current device: every final circuit of a
/// dwelling, sockets and their appliances elsewhere; never safety or essential supplies.
fn needs_rcd(spec: &ProjectSpec, u: CircuitUse) -> bool {
    match u {
        CircuitUse::Feeder | CircuitUse::Emergency | CircuitUse::Essential => false,
        CircuitUse::Lighting => housing(spec),
        _ => true,
    }
}

/// Shortest routes along the power runs from every board.
struct Routes {
    edges: Vec<Vec<V3>>,
    ends: Vec<[usize; 2]>,
    /// Network node of every power connector, by storey and fixture.
    ports: HashMap<(usize, usize), usize>,
    /// Per board: distance and (previous node, edge) per node.
    trees: HashMap<(usize, usize), (Vec<f64>, Vec<Option<(usize, usize)>>)>,
}

impl Routes {
    fn new(p: &GProject) -> Routes {
        let s = &p.services;
        let runs: Vec<&GRun> = s.elec.iter().collect();
        let mut r = Routes { edges: vec![], ends: vec![], ports: HashMap::new(), trees: HashMap::new() };
        if runs.is_empty() {
            return r;
        }
        let net = svc::network(&runs);
        let mut adj: Vec<Vec<(usize, f64, usize)>> = vec![vec![]; net.nodes.len()];
        for (e, (_, pts, nd, _)) in net.edges.iter().enumerate() {
            let l = svc::len(pts);
            adj[nd[0]].push((nd[1], l, e));
            adj[nd[1]].push((nd[0], l, e));
            r.edges.push(pts.clone());
            r.ends.push(*nd);
        }
        let node_of = |q: (f64, f64, f64)| net.nodes.iter().position(|n| (*n - v3(q.0, q.1, q.2)).len() < 5e-3);
        for l in s.links.iter().filter(|l| l.sys == SvcSys::Power) {
            if let Some(n) = svc::port_world(p, l.storey, l.fixture, l.connector).and_then(|c| node_of(c.at)) {
                r.ports.insert((l.storey, l.fixture), n);
            }
        }
        let boards: Vec<((usize, usize), usize)> = r.ports.iter().map(|(f, n)| (*f, *n)).filter(|(f, _)| p.storeys[f.0].layout.fixtures[f.1].family == "Distribution Board").collect();
        for (b, root) in boards {
            r.trees.insert(b, dijkstra(&adj, root));
        }
        r
    }

    fn dist(&self, board: (usize, usize), to: (usize, usize)) -> Option<f64> {
        let (d, _) = self.trees.get(&board)?;
        let n = *self.ports.get(&to)?;
        d[n].is_finite().then_some(d[n])
    }

    /// Axis points from the board to a connector along the route.
    fn path(&self, board: (usize, usize), to: (usize, usize)) -> Option<Vec<V3>> {
        let (d, prev) = self.trees.get(&board)?;
        let mut n = *self.ports.get(&to)?;
        if !d[n].is_finite() {
            return None;
        }
        let mut pieces: Vec<Vec<V3>> = vec![];
        while let Some((m, e)) = prev[n] {
            let mut pts = self.edges[e].clone();
            if self.ends[e][0] == n {
                pts.reverse();
            }
            pieces.push(pts);
            n = m;
        }
        let mut out: Vec<V3> = vec![];
        for piece in pieces.into_iter().rev() {
            for q in piece {
                if out.last().is_none_or(|l: &V3| (*l - q).len() > 1e-6) {
                    out.push(q);
                }
            }
        }
        Some(out)
    }
}

/// Shortest-path tree from `root`: distance and (previous node, edge) per node.
fn dijkstra(adj: &[Vec<(usize, f64, usize)>], root: usize) -> (Vec<f64>, Vec<Option<(usize, usize)>>) {
    let mut dist = vec![f64::INFINITY; adj.len()];
    let mut prev = vec![None; adj.len()];
    let mut heap = BinaryHeap::new();
    dist[root] = 0.0;
    // non-negative floats order as their bit patterns
    heap.push(Reverse((0f64.to_bits(), root)));
    while let Some(Reverse((bits, u))) = heap.pop() {
        if f64::from_bits(bits) > dist[u] {
            continue;
        }
        for &(w, l, e) in &adj[u] {
            let nd = dist[u] + l;
            if nd < dist[w] {
                dist[w] = nd;
                prev[w] = Some((u, e));
                heap.push(Reverse((nd.to_bits(), w)));
            }
        }
    }
    (dist, prev)
}

/// Devices of one room for one use, in the order served.
struct Group {
    room: (usize, usize),
    devs: Vec<(usize, usize, f64, &'static str)>,
}

/// Packs the room groups of one board and use into circuits: a room whole while it fits,
/// else device by device.
fn pack(groups: &[Group], u: CircuitUse, max_pts: usize, max_w: f64) -> Vec<Vec<(usize, usize, f64, &'static str, (usize, usize))>> {
    let g = diversity(u);
    let mut out: Vec<Vec<(usize, usize, f64, &'static str, (usize, usize))>> = vec![];
    let measure = |d: &[(usize, usize, f64, &'static str, (usize, usize))]| -> (usize, f64) { (d.iter().map(|x| point(x.3)).sum(), d.iter().map(|x| x.2 * g).sum()) };
    let fits = |cur: &[(usize, usize, f64, &'static str, (usize, usize))], add: (usize, f64)| {
        let (p, w) = measure(cur);
        p + add.0 <= max_pts && w + add.1 <= max_w + 1e-9
    };
    for grp in groups {
        let items: Vec<_> = grp.devs.iter().map(|d| (d.0, d.1, d.2, d.3, grp.room)).collect();
        let whole = measure(&items);
        match out.last_mut() {
            Some(cur) if fits(cur, whole) => cur.extend(items),
            _ if whole.0 <= max_pts && whole.1 <= max_w + 1e-9 => out.push(items),
            _ => {
                for it in items {
                    let one = measure(std::slice::from_ref(&it));
                    match out.last_mut() {
                        Some(cur) if fits(cur, one) => cur.push(it),
                        _ => out.push(vec![it]),
                    }
                }
            }
        }
    }
    out
}

fn word(u: CircuitUse, lang: &str) -> String {
    let key = match u {
        CircuitUse::Lighting => "lighting",
        CircuitUse::Sockets => "sockets",
        CircuitUse::Worktop => "worktop",
        CircuitUse::Wet => "wet",
        CircuitUse::Appliance => "appliance",
        CircuitUse::Feeder => "feeder",
        CircuitUse::Emergency => "emergency",
        CircuitUse::Essential => "essential",
    };
    svc::text(&["circuits", key], lang)
}

fn code(u: CircuitUse) -> usize {
    USES.iter().position(|x| *x == u).unwrap_or(0)
}

/// The rooms a circuit serves, by name: two, then a count of the rest.
fn room_names(p: &GProject, rooms: &[(usize, usize)]) -> String {
    let mut names: Vec<&str> = vec![];
    for &(k, i) in rooms {
        if let Some(r) = p.storeys[k].layout.rooms.get(i) {
            if !r.name.is_empty() && !names.contains(&r.name.as_str()) {
                names.push(&r.name);
            }
        }
    }
    let shown = names.iter().take(2).copied().collect::<Vec<_>>().join(", ");
    match names.len() {
        0..=2 => shown,
        n => format!("{shown} +{}", n - 2),
    }
}

/// Designs the circuits of the boards: floor boards per storey, unit boards per apartment.
pub(crate) fn design(spec: &ProjectSpec, p: &mut GProject, floor: &[Option<(usize, usize)>], units: &HashMap<(usize, u32), (usize, usize)>, zones: &HashMap<(usize, usize), (usize, usize)>, served: &[Served]) {
    let lang = spec.lang;
    let routes = Routes::new(p);
    let rooms: Vec<Vec<Room>> = p.storeys.iter().map(svc::rooms).collect();
    let board_of = |k: usize, room_idx: usize| -> Option<(usize, usize)> {
        let owner = rooms[k].iter().find(|r| r.idx == room_idx).map(|r| r.owner);
        if let Some(PRole::Apartment(a)) = owner {
            if let Some(b) = units.get(&(k, a)) {
                return Some(*b);
            }
        }
        zones.get(&(k, room_idx)).copied().or(floor[k])
    };
    // boards: the main one, the floor boards it feeds, the unit boards they feed
    let main = floor.first().copied().flatten();
    let mut boards: Vec<GBoard> = vec![];
    for (k, f) in floor.iter().enumerate() {
        if let Some(b) = f {
            let (label, parent) = if Some(*b) == main { (svc::text(&["boards", "main"], lang), None) } else { (format!("{}{k}", svc::text(&["boards", "floor"], lang)), main) };
            boards.push(GBoard { at: *b, label, parent, rcds: vec![] });
        }
    }
    let mut unit_list: Vec<((usize, u32), (usize, usize))> = units.iter().map(|(a, b)| (*a, *b)).collect();
    unit_list.sort();
    for ((k, a), b) in unit_list {
        boards.push(GBoard { at: b, label: format!("{}{k}.{a}", svc::text(&["boards", "unit"], lang)), parent: floor[k], rcds: vec![] });
    }
    let mut zone_list: Vec<(usize, usize)> = zones.values().copied().collect();
    zone_list.sort();
    zone_list.dedup();
    for (n, b) in zone_list.iter().enumerate() {
        let nth = zone_list[..n].iter().filter(|z| z.0 == b.0).count() + 1;
        boards.push(GBoard { at: *b, label: format!("{}{}.{nth}", svc::text(&["boards", "zone"], lang), b.0), parent: floor[b.0], rcds: vec![] });
    }
    // devices by board, use, and per room for a worktop, per fixture for an appliance
    let mut keys: Vec<((usize, usize), usize, (usize, usize))> = vec![];
    let mut groups: HashMap<((usize, usize), usize, (usize, usize)), Vec<Group>> = HashMap::new();
    for &(k, fx, use_, load, _, room_idx, fam) in served {
        let Some(b) = board_of(k, room_idx) else { continue };
        let c = code(use_);
        let split = match use_ {
            CircuitUse::Worktop => (k, room_idx),
            CircuitUse::Appliance => (k, fx),
            _ => (usize::MAX, usize::MAX),
        };
        let key = (b, c, split);
        let gs = groups.entry(key).or_insert_with(|| {
            keys.push(key);
            vec![]
        });
        match gs.last_mut() {
            Some(g) if g.room == (k, room_idx) => g.devs.push((k, fx, load, fam)),
            _ => gs.push(Group { room: (k, room_idx), devs: vec![(k, fx, load, fam)] }),
        }
    }
    let mut out: Vec<GCircuit> = vec![];
    let mut count: HashMap<((usize, usize), usize), usize> = HashMap::new();
    for key in keys {
        let u = USES[key.1];
        let (max_pts, max_w) = limits(spec, u);
        for pk in pack(&groups[&key], u, max_pts, max_w) {
            let c = count.entry((key.0, key.1)).or_insert(0);
            *c += 1;
            let load: f64 = pk.iter().map(|x| x.2).sum();
            let length = pk.iter().filter_map(|x| routes.dist(key.0, (x.0, x.1))).fold(0.0, f64::max);
            let what = if u == CircuitUse::Appliance { pk[0].3.to_string() } else { word(u, lang) };
            let rooms_of: Vec<(usize, usize)> = pk.iter().map(|x| x.4).collect();
            let where_ = room_names(p, &rooms_of);
            let name = if where_.is_empty() { format!("{}{c} {what}", CODES[key.1]) } else { format!("{}{c} {what} {where_}", CODES[key.1]) };
            let three = u == CircuitUse::Appliance && (pk[0].3 == "Cooker" || load > rule("services.elec.three_phase_w"));
            out.push(GCircuit {
                board: key.0,
                name,
                use_: u,
                devices: pk.iter().map(|x| (x.0, x.1, "Power")).collect(),
                load,
                length: length + 2.0 * rule("services.elec.cable_slack"),
                number: 0,
                breaker: 0.0,
                poles: if three { 3 } else { 1 },
                section: 0.0,
                cores: 0,
                rcd: None,
                points: pk.iter().map(|x| point(x.3)).sum(),
                demand: pk.iter().map(|x| x.2).sum::<f64>() * diversity(u),
                drop: 0.0,
                drop_total: 0.0,
            });
        }
    }
    // feeders: the main board to each floor board, a floor board to each unit board
    let mut feeders: Vec<((usize, usize), (usize, usize))> = boards.iter().filter_map(|b| Some((b.parent?, b.at))).collect();
    feeders.sort();
    let below = |b: (usize, usize)| -> Vec<(usize, usize)> {
        let mut all = vec![b];
        let mut i = 0;
        while i < all.len() {
            let x = all[i];
            all.extend(boards.iter().filter(|c| c.parent == Some(x)).map(|c| c.at));
            i += 1;
        }
        all
    };
    let g_feed = rule("services.elec.g_feeder");
    for (n, (from, to)) in feeders.iter().enumerate() {
        let behind = below(*to);
        let load: f64 = out.iter().filter(|c| behind.contains(&c.board)).map(|c| c.load).sum();
        let demand: f64 = out.iter().filter(|c| behind.contains(&c.board)).map(|c| c.demand).sum::<f64>() * g_feed;
        out.push(GCircuit {
            board: *from,
            name: format!("F{} {}", n + 1, word(CircuitUse::Feeder, lang)),
            use_: CircuitUse::Feeder,
            devices: vec![(to.0, to.1, "Main")],
            load,
            length: routes.dist(*from, *to).unwrap_or(0.0) + 2.0 * rule("services.elec.cable_slack"),
            number: 0,
            breaker: 0.0,
            poles: 3,
            section: 0.0,
            cores: 0,
            rcd: None,
            points: 0,
            demand,
            drop: 0.0,
            drop_total: 0.0,
        });
    }
    // feeders first, from the main board down, so every board knows the drop up to it
    let mut up: HashMap<(usize, usize), f64> = HashMap::new();
    if let Some(m) = main {
        up.insert(m, 0.0);
    }
    let mut issues = vec![];
    let depth = |b: (usize, usize)| -> usize {
        let mut d = 0;
        let mut x = b;
        while let Some(pa) = boards.iter().find(|c| c.at == x).and_then(|c| c.parent) {
            d += 1;
            x = pa;
        }
        d
    };
    let mut order: Vec<usize> = (0..out.len()).collect();
    order.sort_by_key(|&i| (out[i].use_ != CircuitUse::Feeder, depth(out[i].board)));
    for i in order {
        let c = &mut out[i];
        let upstream = up.get(&c.board).copied().unwrap_or(0.0);
        let home = housing(spec);
        size(c, home, upstream);
        if c.use_ == CircuitUse::Feeder {
            let to = (c.devices[0].0, c.devices[0].1);
            up.insert(to, c.drop_total);
            if c.drop > drop_limit(home, true, 0.0) + 1e-9 {
                issues.push(format!("feeder {} drops {:.2} % over {:.0} m", c.name, c.drop, c.length));
            }
        } else if c.drop > drop_limit(home, false, upstream) + 1e-9 {
            issues.push(format!("circuit {} drops {:.2} % over {:.0} m", c.name, c.drop_total, c.length));
        }
    }
    // ways per board: final circuits in order, then the feeders
    let mut ways: HashMap<(usize, usize), u32> = HashMap::new();
    let mut by_way: Vec<usize> = (0..out.len()).collect();
    by_way.sort_by_key(|&i| out[i].use_ == CircuitUse::Feeder);
    for i in by_way {
        let w = ways.entry(out[i].board).or_insert(0);
        *w += 1;
        out[i].number = *w;
    }
    for b in &mut boards {
        b.rcds = rcd_groups(spec, p, &mut out, b.at);
    }
    p.services.issues.extend(issues);
    p.services.wires = if spec.services.wires { wires(p, &out, &boards, &routes) } else { vec![] };
    p.services.traces = if spec.services.traces { traces(&out, &routes) } else { vec![] };
    p.services.circuits = out;
    p.services.boards = boards;
}

/// Highest drop, %, a final circuit may add `upstream` % down from the origin: in a dwelling 3 %
/// from the meter to the last outlet (DIN 18015-1 5.2.1), elsewhere 5 % in all (DIN VDE 0100-520
/// G.52); a feeder its own share.
fn drop_limit(home: bool, feeder: bool, upstream: f64) -> f64 {
    if feeder {
        rule("services.elec.max_drop_feeder")
    } else if home {
        rule("services.elec.max_drop") - upstream
    } else {
        rule("services.elec.max_drop_total") - upstream
    }
}

/// Breaker, section and voltage drop of a circuit `upstream` % down from the origin: the breaker
/// holds the design current, the cable the breaker (Iz >= In, DIN VDE 0298-4 laying method C),
/// the section steps up while the design current drops more than the limit allows.
fn size(c: &mut GCircuit, home: bool, upstream: f64) {
    let ib = design_current(c.poles, c.demand);
    let feeder = c.use_ == CircuitUse::Feeder;
    let light = matches!(c.use_, CircuitUse::Lighting | CircuitUse::Emergency);
    let base = if light { rule("services.elec.breaker_light") } else if feeder { rule("services.elec.feeder_min_a") } else { rule("services.elec.breaker_other") };
    c.breaker = breaker_at_least(base.max(ib / rule("services.elec.load_margin")));
    let min = if light { rule("services.elec.section_light") } else { rule("services.elec.section_other") };
    c.section = section_holding(min, c.breaker, c.poles);
    c.cores = c.poles + 2;
    let limit = drop_limit(home, feeder, upstream);
    let cap = if feeder { f64::INFINITY } else { rule("services.elec.section_max") };
    c.drop = voltage_drop(c.poles, c.length, ib, c.section);
    while c.drop > limit + 1e-9 {
        match section_up(c.section).filter(|s| *s <= cap + 1e-9) {
            Some(s) => c.section = s,
            None => break,
        }
        c.drop = voltage_drop(c.poles, c.length, ib, c.section);
    }
    c.drop_total = upstream + c.drop;
}

/// Residual current devices of a board: the bathroom circuits on their own group, each
/// refrigerator on its own, the rest in groups of the most a device takes.
fn rcd_groups(spec: &ProjectSpec, p: &GProject, out: &mut [GCircuit], board: (usize, usize)) -> Vec<GRcd> {
    let fam = |c: &GCircuit| p.storeys[c.devices[0].0].layout.fixtures[c.devices[0].1].family;
    let mine: Vec<usize> = (0..out.len()).filter(|&i| out[i].board == board && needs_rcd(spec, out[i].use_)).collect();
    let wet: Vec<usize> = mine.iter().copied().filter(|&i| out[i].use_ == CircuitUse::Wet).collect();
    let cold: Vec<usize> = mine.iter().copied().filter(|&i| out[i].use_ == CircuitUse::Appliance && fam(&out[i]) == "Refrigerator").collect();
    let rest: Vec<usize> = mine.iter().copied().filter(|i| !wet.contains(i) && !cold.contains(i)).collect();
    let (two, four) = (rule("services.elec.rcd_circuits_2p") as usize, rule("services.elec.rcd_circuits_4p") as usize);
    let mut sets: Vec<Vec<usize>> = vec![];
    for pool in [wet, rest] {
        sets.extend(pool.chunks(four.max(1)).map(<[usize]>::to_vec));
    }
    sets.extend(cold.into_iter().map(|i| vec![i]));
    let mut rcds = vec![];
    for (n, set) in sets.into_iter().enumerate() {
        let number = n as u32 + 1;
        let three = set.iter().any(|&i| out[i].poles > 1);
        for &i in &set {
            out[i].rcd = Some(number);
        }
        rcds.push(GRcd {
            number,
            poles: if three || set.len() > two { 4 } else { 2 },
            ma: rule("services.elec.rcd_ma"),
            rating: rule("services.elec.rcd_a"),
        });
    }
    rcds
}

/// Short code of a circuit on its board, as L3.
fn tag(c: &GCircuit) -> &str {
    c.name.split(' ').next().unwrap_or("")
}

/// A line between two plan points bowed to its left by a share of its length.
fn arc(a: (f64, f64), b: (f64, f64)) -> Vec<(f64, f64)> {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let bow = rule("services.elec.wire_bow");
    // a quadratic curve reaches half way to its control point
    let ctl = ((a.0 + b.0) / 2.0 - dy * bow * 2.0, (a.1 + b.1) / 2.0 + dx * bow * 2.0);
    let n = rule("services.elec.wire_steps").max(1.0) as usize;
    (0..=n)
        .map(|i| {
            let t = i as f64 / n as f64;
            let (u, w, v) = ((1.0 - t) * (1.0 - t), 2.0 * t * (1.0 - t), t * t);
            (u * a.0 + w * ctl.0 + v * b.0, u * a.1 + w * ctl.1 + v * b.1)
        })
        .collect()
}

/// Circuit lines per storey: the devices of a circuit chained nearest first from the one the
/// cable reaches first, and a home run from it towards the board with the circuit label.
fn wires(p: &GProject, out: &[GCircuit], boards: &[GBoard], routes: &Routes) -> Vec<GWire> {
    let at = |k: usize, fx: usize, conn: &str| svc::port_world(p, k, fx, conn).map(|c| (c.at.0, c.at.1));
    let mut w = vec![];
    for (ci, c) in out.iter().enumerate() {
        if c.use_ == CircuitUse::Feeder {
            continue;
        }
        let Some(board_xy) = at(c.board.0, c.board.1, "Main") else { continue };
        let label = boards.iter().find(|b| b.at == c.board).map_or(String::new(), |b| format!("{} / {}", b.label, tag(c)));
        let mut storeys: Vec<usize> = c.devices.iter().map(|d| d.0).collect();
        storeys.dedup();
        for k in storeys {
            let mut left: Vec<((f64, f64), f64)> = c.devices.iter().filter(|d| d.0 == k).filter_map(|d| Some((at(d.0, d.1, d.2)?, routes.dist(c.board, (d.0, d.1)).unwrap_or(f64::INFINITY)))).collect();
            let Some(first) = (0..left.len()).min_by(|&i, &j| left[i].1.total_cmp(&left[j].1)) else { continue };
            let mut cur = left.swap_remove(first).0;
            let (dx, dy) = (board_xy.0 - cur.0, board_xy.1 - cur.1);
            let d = (dx * dx + dy * dy).sqrt();
            if d > 1e-6 {
                let l = rule("services.elec.home_run").min(d);
                w.push(GWire { storey: k, circuit: ci, points: vec![cur, (cur.0 + dx / d * l, cur.1 + dy / d * l)], home: true, ticks: c.cores, label: label.clone() });
            }
            while !left.is_empty() {
                let dist = |q: (f64, f64)| (q.0 - cur.0).hypot(q.1 - cur.1);
                let next = (0..left.len()).min_by(|&i, &j| dist(left[i].0).total_cmp(&dist(left[j].0))).unwrap_or(0);
                let q = left.swap_remove(next).0;
                if dist(q) > 1e-6 {
                    w.push(GWire { storey: k, circuit: ci, points: arc(cur, q), home: false, ticks: 0, label: String::new() });
                }
                cur = q;
            }
        }
    }
    w
}

/// Cable traces: from each board along the carriers to the device of each circuit the cable
/// reaches first.
fn traces(out: &[GCircuit], routes: &Routes) -> Vec<GTrace> {
    let mut t = vec![];
    for (ci, c) in out.iter().enumerate() {
        let first = c.devices.iter().filter_map(|d| Some(((d.0, d.1), routes.dist(c.board, (d.0, d.1))?))).min_by(|a, b| a.1.total_cmp(&b.1));
        let Some((to, _)) = first else { continue };
        if let Some(pts) = routes.path(c.board, to).filter(|x| x.len() >= 2) {
            t.push(GTrace { circuit: ci, storey: c.board.0, points: pts.iter().map(|q| (q.x, q.y, q.z)).collect() });
        }
    }
    t
}

/// The circuit schedule of every board, one row per circuit, as semicolon separated text.
pub fn schedule_csv(p: &GProject) -> String {
    let sv = &p.services;
    let mut s = String::from("board;way;circuit;use;protection;rcd;cable;points;load_w;demand_w;length_m;drop_pct;drop_total_pct\n");
    for b in &sv.boards {
        let mut rows: Vec<&GCircuit> = sv.circuits.iter().filter(|c| c.board == b.at).collect();
        rows.sort_by_key(|c| c.number);
        for c in rows {
            let rcd = c.rcd.and_then(|n| b.rcds.iter().find(|r| r.number == n)).map_or(String::new(), GRcd::label);
            s += &format!(
                "{};{};{};{:?};{};{};{};{};{:.0};{:.0};{:.1};{:.2};{:.2}\n",
                b.label,
                c.number,
                c.name,
                c.use_,
                c.protection(),
                rcd,
                c.cable(),
                c.points,
                c.load,
                c.demand,
                c.length,
                c.drop,
                c.drop_total
            );
        }
    }
    s
}
