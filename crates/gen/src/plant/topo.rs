// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The process topology of each kind of plant: units sized from the duty by the rules, their
//! nozzles with the valves and instruments they ask for, and the lines between them.

use std::f64::consts::PI;

use ddc_geom::math::V3;

use crate::project::rules::rule;
use crate::project::why::Why;

use super::brief::{Brief, Kind};
use super::size::{self, ItemKind, Spec};
use super::why::{self as pwhy, s, s1, s2};
use super::{Clear, Dims, End, EqKind, Equip, Level, Line, Nozzle, Row, Spot, Svc, Want};

/// Flue gas and air of the stoichiometric burning of natural gas, m³ per m³ of gas.
const FLUE_PER_GAS: f64 = 10.5;
const AIR_PER_GAS: f64 = 9.5;
/// 0 °C in kelvin.
const T0: f64 = 273.15;
/// Smallest water reserve of an expansion vessel, L.
const MIN_RESERVE_L: f64 = 3.0;
const L_PER_M3: f64 = 1000.0;
const MJ_PER_KWH: f64 = 3.6;
const W_PER_KW: f64 = 1000.0;
/// Boiler shell diameter, m, from the output in kW, and its length: a fit of fire-tube packages.
const BOILER_D_K: f64 = 0.42;
const BOILER_D_EXP: f64 = 0.165;
const BOILER_L_PER_D: f64 = 2.1;
/// Tube outside diameter, pitch ratio and bundle factor of a shell-and-tube exchanger.
const TUBE_OD: f64 = 0.01905;
const TUBE_PITCH: f64 = 1.25;
const BUNDLE_K: f64 = 1.15;
/// Tags count from here: P-101, P-102.
const TAG_BASE: usize = 100;

/// Units that stand together: one function, one row.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub key: &'static str,
    pub members: Vec<usize>,
    /// The row a rule puts it in, else the layout balances the rows.
    pub row: Option<Row>,
}

/// The graph: units, lines, groups, and why it is so.
#[derive(Clone, Debug, PartialEq)]
pub struct Topo {
    pub eqs: Vec<Equip>,
    pub lines: Vec<Line>,
    pub groups: Vec<Group>,
    pub why: Vec<Why>,
    drains: Vec<End>,
    process: bool,
    /// Open air: the rack stands on portals.
    pub outdoor: bool,
}

pub(super) fn of(b: &Brief) -> Result<Topo, String> {
    let mut t = Topo {
        eqs: vec![],
        lines: vec![],
        groups: vec![],
        why: vec![],
        drains: vec![],
        process: b.kind == Kind::ProcessLine,
        outdoor: b.outdoor,
    };
    match b.kind {
        Kind::BoilerHouse => boiler_house(&mut t, b)?,
        Kind::PumpingStation => pumping_station(&mut t, b)?,
        Kind::WaterTreatment => water_treatment(&mut t, b)?,
        Kind::ProcessLine => process_line(&mut t, b)?,
        Kind::StorageTank => return Err("a storage tank is designed on its own, not as a piped plant".into()),
    }
    t.sump();
    t.finish();
    Ok(t)
}

fn vessel_d(d: f64) -> f64 {
    size::round_up(d, rule("plant.vessel.step")).max(rule("plant.vessel.min_d"))
}

impl Topo {
    /// A process line (its tanks hold process liquid and stand in a bund).
    pub fn is_process(&self) -> bool {
        self.process
    }

    fn unit(&mut self, kind: EqKind, role: &'static str, dims: Dims, duty: Vec<(&'static str, f64)>, standby: bool) -> usize {
        let n = self.eqs.iter().filter(|e| e.kind == kind).count() + 1;
        let row = match kind {
            EqKind::Terminal => Row::Wall,
            EqKind::Sump => Row::Under,
            _ => Row::North,
        };
        self.eqs.push(Equip {
            tag: format!("{}-{}", kind.prefix(), TAG_BASE + n),
            kind,
            role,
            standby,
            duty,
            dims,
            nozzles: vec![],
            family: String::new(),
            boxes: vec![],
            clear: Clear::default(),
            flue: None,
            at: V3::default(),
            rot: 0,
            row,
        });
        self.eqs.len() - 1
    }

    #[allow(clippy::too_many_arguments)]
    fn noz(&mut self, e: usize, name: &'static str, svc: Svc, flow: f64, spot: Spot, level: Level, want: &[Want]) -> End {
        let n = &mut self.eqs[e].nozzles;
        n.push(Nozzle {
            name,
            svc,
            dn: 0,
            pipe_dn: 0,
            flow,
            spot,
            level,
            want: want.to_vec(),
            items: vec![],
            at: V3::default(),
            dir: V3::default(),
            line: usize::MAX,
        });
        End { eq: e, nz: n.len() - 1 }
    }

    fn drain(&mut self, e: usize, name: &'static str, spot: Spot, level: Level, flow: f64) {
        let d = self.noz(e, name, Svc::Dr, flow, spot, level, &[Want::Iso]);
        self.drains.push(d);
    }

    fn terminal(&mut self, svc: Svc, name: &'static str, flow: f64, want: &[Want]) -> End {
        let e = self.unit(EqKind::Terminal, "terminal", Dims::default(), vec![("flow_m3h", flow)], false);
        self.noz(e, name, svc, flow, Spot::Wall, Level::Mid, want)
    }

    fn line(&mut self, svc: Svc, root: End, ends: Vec<End>, flow: f64) -> usize {
        let n = self.lines.iter().filter(|l| l.svc == svc).count() + 1;
        let li = self.lines.len();
        for e in std::iter::once(root).chain(ends.iter().copied()) {
            self.eqs[e.eq].nozzles[e.nz].line = li;
        }
        self.lines.push(Line {
            seq: format!("{}-{:03}", svc.code(), n),
            svc,
            spec: Spec::C1,
            root,
            ends,
            flow,
            v_rule: "plant.pipe.v_water",
            dn: 0,
            pn: 0,
            t_op: 0.0,
            p_op: 0.0,
            insul: 0.0,
        });
        li
    }

    fn why(&mut self, target: &str, rule_id: &str, key: &'static str, args: Vec<(&'static str, String)>) {
        pwhy::add(&mut self.why, target, rule_id, key, args);
    }

    /// `n_duty` duty and `n_standby` standby pumps sharing `total` m³/h at `head` m; their
    /// suction and discharge ends.
    #[allow(clippy::too_many_arguments)]
    fn pumps(
        &mut self,
        role: &'static str,
        group: &'static str,
        svc_in: Svc,
        svc_out: Svc,
        total: f64,
        n_duty: usize,
        n_standby: usize,
        head: f64,
        row: Option<Row>,
    ) -> (Vec<End>, Vec<End>) {
        let q = total / n_duty as f64;
        let t = svc_in.temp();
        let shaft = size::rho(t) * size::G * q / 3600.0 * head / W_PER_KW / rule("plant.pump.efficiency");
        let kw = size::motor_kw(shaft * rule("plant.pump.motor_margin"));
        let (mut suc, mut dis, mut members) = (vec![], vec![], vec![]);
        for i in 0..n_duty + n_standby {
            let duty = vec![("flow_m3h", q), ("head_m", head), ("shaft_kw", shaft), ("motor_kw", kw)];
            let e = self.unit(EqKind::Pump, role, Dims { kw, ..Dims::default() }, duty, i >= n_duty);
            let pi = Want::Item(ItemKind::Pi);
            suc.push(self.noz(e, "SUC", svc_in, q, Spot::Suction, Level::Mid, &[pi, Want::Item(ItemKind::Strainer), Want::Iso]));
            dis.push(self.noz(e, "DIS", svc_out, q, Spot::Discharge, Level::Mid, &[pi, Want::Item(ItemKind::Check), Want::Iso]));
            members.push(e);
        }
        let first = self.eqs[members[0]].tag.clone();
        let last = self.eqs[members[members.len() - 1]].tag.clone();
        let label = if first == last { first.clone() } else { format!("{first}..{last}") };
        self.why(&first, "plant.pump.standby", "plant_pumps", vec![("group", label.clone()), ("duty", s(n_duty)), ("standby", s(n_standby))]);
        self.why(
            &first,
            "plant.pump.motor_margin",
            "plant_pump_motor",
            vec![("tag", label), ("q", s1(q)), ("head", s1(head)), ("shaft", s2(shaft)), ("kw", s2(kw))],
        );
        self.groups.push(Group { key: group, members, row });
        (suc, dis)
    }

    /// Duty pumps for `total` m³/h by the largest flow per pump (or `n` of them), standby by the
    /// rule.
    #[allow(clippy::too_many_arguments)]
    fn pump_set(
        &mut self,
        role: &'static str,
        group: &'static str,
        svc_in: Svc,
        svc_out: Svc,
        total: f64,
        head: f64,
        n: Option<usize>,
        row: Option<Row>,
    ) -> (Vec<End>, Vec<End>) {
        let max = rule("plant.pump.max_flow");
        let n_duty = n.unwrap_or(((total / max - 1e-9).ceil() as usize).max(1));
        let sb = rule("plant.pump.standby") as usize;
        let out = self.pumps(role, group, svc_in, svc_out, total, n_duty, sb, head, row);
        let first = self.eqs[out.0[0].eq].tag.clone();
        self.why(&first, "plant.pump.max_flow", "plant_pump_split", vec![("tag", first.clone()), ("flow", s1(total)), ("duty", s(n_duty)), ("max", s1(max))]);
        out
    }

    fn tank(&mut self, kind: EqKind, role: &'static str, volume: f64, hold_rule: &str) -> usize {
        let hd = rule("plant.tank.hd");
        let d = vessel_d(size::d_of_volume(volume, hd));
        let e = self.unit(kind, role, Dims { d, h: hd * d, ..Dims::default() }, vec![("volume_m3", volume)], false);
        let tag = self.eqs[e].tag.clone();
        self.why(&tag, hold_rule, "plant_tank", vec![("tag", tag.clone()), ("vol", s2(volume)), ("hold", s2(rule(hold_rule)))]);
        e
    }

    /// The drain sump and the drain line from every drain to it.
    fn sump(&mut self) {
        if self.drains.is_empty() {
            return;
        }
        let flow = self.drains.iter().map(|d| self.eqs[d.eq].nozzles[d.nz].flow).fold(0.0, f64::max);
        let e = self.unit(EqKind::Sump, "sump", Dims::default(), vec![("flow_m3h", flow)], false);
        let root = self.noz(e, "IN", Svc::Dr, flow, Spot::Pit, Level::Low, &[]);
        let ends = std::mem::take(&mut self.drains);
        self.line(Svc::Dr, root, ends, flow);
    }

    /// Spec, sizes, pressure, temperature and insulation of every line; the items of its
    /// nozzles at the pipe size.
    fn finish(&mut self) {
        for li in 0..self.lines.len() {
            let (svc, flow) = (self.lines[li].svc, self.lines[li].flow);
            let ends = self.lines[li].all_ends();
            let suction = ends.iter().any(|e| self.eqs[e.eq].nozzles[e.nz].spot == Spot::Suction);
            let t_op = svc.temp();
            let static_bar = ends
                .iter()
                .map(|e| size::bar_of_head(self.eqs[e.eq].dims.h + self.eqs[e.eq].dims.d, t_op))
                .fold(0.0, f64::max);
            let pump_bar = ends
                .iter()
                .filter(|e| self.eqs[e.eq].nozzles[e.nz].spot == Spot::Discharge)
                .map(|e| {
                    let head = self.eqs[e.eq].duty.iter().find(|d| d.0 == "head_m").map_or(0.0, |d| d.1);
                    size::bar_of_head(head, t_op)
                })
                .fold(0.0, f64::max);
            let p_op = match svc {
                Svc::Hws | Svc::Hwr => rule("plant.heat.p_safety"),
                Svc::Ng => rule("plant.fuel.p"),
                _ => pump_bar + static_bar,
            };
            let p_design = p_op * rule("plant.pipe.design_margin_p");
            let spec = match svc {
                Svc::Ng => Spec::G1,
                Svc::Pr => Spec::S1,
                Svc::Ch if t_op <= rule("plant.pipe.plastic_max_t") && p_design <= size::PN_PLASTIC as f64 => Spec::P1,
                Svc::Ch => Spec::S1,
                Svc::Dr if self.process => Spec::S1,
                _ => Spec::C1,
            };
            let v_rule = match svc {
                Svc::Ng => "plant.pipe.v_gas",
                Svc::Dr => "plant.pipe.v_drain",
                _ if suction => "plant.pipe.v_suction",
                _ => "plant.pipe.v_water",
            };
            let v = rule(v_rule);
            let drain_dn = size::pipe(spec, rule("plant.pipe.drain_dn") as u32).dn;
            let size_of = |q: f64| {
                let dn = size::dn_for(spec, q, v);
                if svc == Svc::Dr {
                    dn.max(drain_dn)
                } else {
                    dn
                }
            };
            let nozzle_v = rule("plant.pump.nozzle_v");
            let mut dn = size_of(flow);
            for e in &ends {
                let nz = &mut self.eqs[e.eq].nozzles[e.nz];
                let pipe_dn = size_of(nz.flow);
                nz.pipe_dn = pipe_dn;
                nz.dn = match nz.spot {
                    Spot::Suction | Spot::Discharge => size::dn_for(spec, nz.flow, nozzle_v).min(pipe_dn),
                    _ => pipe_dn,
                };
                nz.items = nz
                    .want
                    .iter()
                    .map(|w| match w {
                        Want::Iso => ItemKind::isolation(pipe_dn),
                        Want::Item(k) => *k,
                    })
                    .collect();
                dn = dn.max(pipe_dn);
            }
            let insul = if t_op >= rule("plant.pipe.insul_from") { rule("plant.pipe.insul") } else { 0.0 };
            let l = &mut self.lines[li];
            l.spec = spec;
            l.v_rule = v_rule;
            l.dn = dn;
            l.pn = spec.pn(p_design);
            l.t_op = t_op;
            l.p_op = p_op;
            l.insul = insul;
        }
        let rows: Vec<(String, &'static str, Vec<(&'static str, String)>)> = self
            .lines
            .iter()
            .map(|l| {
                let v = size::velocity(l.spec, l.dn, l.flow);
                (l.number(), l.v_rule, vec![("line", l.number()), ("dn", s(l.dn)), ("flow", s1(l.flow)), ("v", s2(v))])
            })
            .collect();
        for (n, r, a) in rows {
            self.why(&n, r, "plant_line_dn", a);
        }
    }
}

fn boiler_house(t: &mut Topo, b: &Brief) -> Result<(), String> {
    let duty = b.duty_kw.unwrap_or_default();
    let margin = rule("plant.boiler.margin");
    let standby = rule("plant.boiler.standby") as usize;
    let unit_max = rule("plant.boiler.unit_max");
    let need = duty * (1.0 + margin);
    let total = match b.units {
        Some(n) => n,
        None => {
            let d = ((need / unit_max - 1e-9).ceil() as usize).max(1);
            (d + standby).max(rule("plant.boiler.min_count") as usize)
        }
    };
    if total <= standby {
        return Err(format!("{total} boilers leave none in service with {standby} on standby (plant.boiler.standby)"));
    }
    let n_duty = total - standby;
    let unit = size::round_up(need / n_duty as f64, rule("plant.boiler.step"));
    let (ts, tr) = (rule("plant.heat.t_supply"), rule("plant.heat.t_return"));
    if ts <= tr {
        return Err("plant.heat.t_supply must be above plant.heat.t_return".into());
    }
    let dt = ts - tr;
    let q_b = size::heat_flow(unit, dt, tr);
    let q_prim = q_b * n_duty as f64;
    let q_net = size::heat_flow(duty, dt, tr);
    let gas = unit * MJ_PER_KWH / (rule("plant.boiler.efficiency") * rule("plant.fuel.lhv"));
    let fg = gas * (FLUE_PER_GAS + (rule("plant.flue.excess_air") - 1.0) * AIR_PER_GAS) * (T0 + rule("plant.flue.temp")) / T0;
    let d_flue = size::round_up(size::d_of_area(fg / 3600.0 / rule("plant.flue.velocity")), rule("plant.flue.step"));
    let bd = BOILER_D_K * unit.powf(BOILER_D_EXP);
    let (mut boilers, mut sup, mut ret, mut gasn) = (vec![], vec![], vec![], vec![]);
    for i in 0..total {
        let dims = Dims { d: bd, l: BOILER_L_PER_D * bd, kw: unit, h: 0.0 };
        let e = t.unit(EqKind::Boiler, "boiler", dims, vec![("kw", unit), ("gas_m3h", gas), ("flow_m3h", q_b)], i >= n_duty);
        t.eqs[e].flue = Some(d_flue);
        let iso = Want::Iso;
        sup.push(t.noz(e, "SUP", Svc::Hws, q_b, Spot::Top, Level::High, &[Want::Item(ItemKind::Ti), Want::Item(ItemKind::Psv), iso]));
        ret.push(t.noz(e, "RET", Svc::Hwr, q_b, Spot::Top, Level::Low, &[Want::Item(ItemKind::Ti), iso]));
        let train = [Want::Item(ItemKind::ShutOff), Want::Item(ItemKind::Regulator), Want::Item(ItemKind::GasFilter), iso];
        gasn.push(t.noz(e, "GAS", Svc::Ng, gas, Spot::Gas, Level::High, &train));
        t.drain(e, "DRN", Spot::Right, Level::Low, 0.0);
        boilers.push(e);
    }
    let b0 = t.eqs[boilers[0]].tag.clone();
    t.why(&b0, "plant.boiler.unit_max", "plant_boiler_count", vec![("n", s(n_duty)), ("max", s1(unit_max))]);
    t.why(
        &b0,
        "plant.boiler.margin",
        "plant_boilers",
        vec![("units", s(total)), ("kw", s1(unit)), ("duty", s1(duty)), ("margin", s1(margin * 100.0)), ("duty_n", s(n_duty))],
    );
    t.why(&b0, "plant.boiler.standby", "plant_boiler_standby", vec![("n", s(standby)), ("duty_n", s(n_duty))]);
    t.why(&b0, "plant.flue.velocity", "plant_flue", vec![("tag", b0.clone()), ("d", s2(d_flue)), ("v", s1(rule("plant.flue.velocity"))), ("gas", s1(gas))]);
    t.groups.push(Group { key: "boilers", members: boilers.clone(), row: Some(Row::North) });

    // hydraulic separator between the boiler loop and the network
    let q_hs = q_prim.max(q_net);
    let v_hs = rule("plant.heat.hs_v");
    let d_hs = vessel_d(size::d_of_area(q_hs / 3600.0 / v_hs));
    let hs = t.unit(EqKind::Separator, "separator", Dims { d: d_hs, h: rule("plant.vessel.hd") * d_hs, ..Dims::default() }, vec![("flow_m3h", q_hs)], false);
    let pin = t.noz(hs, "PIN", Svc::Hws, q_prim, Spot::Left, Level::High, &[]);
    let pout = t.noz(hs, "POUT", Svc::Hwr, q_prim, Spot::Left, Level::Low, &[]);
    let sout = t.noz(hs, "SOUT", Svc::Hws, q_net, Spot::Right, Level::High, &[]);
    let sin = t.noz(hs, "SIN", Svc::Hwr, q_net, Spot::Right, Level::Low, &[]);
    t.drain(hs, "DRN", Spot::Back, Level::Low, 0.0);
    let hs_tag = t.eqs[hs].tag.clone();
    t.why(&hs_tag, "plant.heat.hs_v", "plant_separator", vec![("d", s2(d_hs)), ("flow", s1(q_hs)), ("v", s2(v_hs))]);

    // one pump per boiler, in the boiler return
    let (bs, bdis) = t.pumps("boiler_pump", "boiler_pumps", Svc::Hwr, Svc::Hwr, q_prim, n_duty, standby, rule("plant.pump.head_boiler"), None);
    let bp = t.eqs[bs[0].eq].tag.clone();
    t.why(&bp, "plant.pump.head_boiler", "plant_boiler_pumps", vec![("n", s(total)), ("q", s1(q_b)), ("head", s1(rule("plant.pump.head_boiler")))]);
    t.line(Svc::Hws, pin, sup, q_prim);
    t.line(Svc::Hwr, pout, bs, q_prim);
    for (d, r) in bdis.into_iter().zip(ret) {
        t.line(Svc::Hwr, d, vec![r], q_b);
    }

    // network pumps after the separator
    let (ns, nd) = t.pump_set("network_pump", "network_pumps", Svc::Hws, Svc::Hws, q_net, rule("plant.pump.head_network"), None, None);
    t.line(Svc::Hws, sout, ns, q_net);
    let ths = t.terminal(Svc::Hws, "HWS", q_net, &[Want::Iso, Want::Item(ItemKind::FlowMeter)]);
    t.line(Svc::Hws, ths, nd, q_net);

    // make-up: softened water pumped into the return
    let v_sys = rule("plant.heat.volume_per_kw") * duty;
    let rate = rule("plant.heat.makeup_rate");
    let q_mu = v_sys / L_PER_M3 * rate;
    let sf_rate = rule("plant.wt.softener_rate");
    let d_sf = vessel_d(size::d_of_area(q_mu / sf_rate));
    let sf = t.unit(EqKind::Softener, "softener", Dims { d: d_sf, h: rule("plant.vessel.hd") * d_sf, ..Dims::default() }, vec![("flow_m3h", q_mu)], false);
    let sf_in = t.noz(sf, "IN", Svc::Mw, q_mu, Spot::Top, Level::High, &[Want::Iso]);
    let sf_out = t.noz(sf, "OUT", Svc::Tw, q_mu, Spot::Back, Level::Low, &[Want::Iso]);
    t.drain(sf, "DRN", Spot::Back, Level::Low, 0.0);
    let sf_tag = t.eqs[sf].tag.clone();
    t.why(&sf_tag, "plant.wt.softener_rate", "plant_softener", vec![("tag", sf_tag.clone()), ("d", s2(d_sf)), ("q", s2(q_mu)), ("rate", s1(sf_rate))]);
    t.why(&sf_tag, "plant.heat.makeup_rate", "plant_makeup", vec![("q", s2(q_mu)), ("sys", s1(v_sys / L_PER_M3)), ("rate", s2(rate))]);
    t.groups.push(Group { key: "softener", members: vec![sf], row: None });
    let tmw = t.terminal(Svc::Mw, "MW", q_mu, &[Want::Iso, Want::Item(ItemKind::Check), Want::Item(ItemKind::FlowMeter)]);
    t.line(Svc::Mw, tmw, vec![sf_in], q_mu);
    let sb = rule("plant.pump.standby") as usize;
    let (ms, md) = t.pumps("makeup_pump", "makeup_pumps", Svc::Tw, Svc::Hwr, q_mu, 1, sb, rule("plant.pump.head_makeup"), None);
    t.line(Svc::Tw, sf_out, ms, q_mu);

    // expansion vessel on the return, sized for the expansion from fill to supply temperature
    let e = size::rho(rule("plant.heat.t_fill")) / size::rho(ts) - 1.0;
    let ve = e * v_sys;
    let vwr = (rule("plant.heat.reserve") * v_sys).max(MIN_RESERVE_L);
    let pe = rule("plant.heat.p_safety") - rule("plant.heat.psv_margin");
    let p0 = rule("plant.heat.p_fill");
    if pe <= p0 {
        return Err(format!(
            "the fill pressure {p0} bar must stay below the relief pressure less its margin, {pe} bar (plant.heat.p_fill, plant.heat.p_safety)"
        ));
    }
    let vn = (ve + vwr) * (pe + 1.0) / (pe - p0);
    let hd = rule("plant.vessel.hd");
    let d_ev = vessel_d(size::d_of_volume(vn / L_PER_M3, hd));
    let ev = t.unit(EqKind::Expansion, "expansion", Dims { d: d_ev, h: hd * d_ev, ..Dims::default() }, vec![("volume_l", vn)], false);
    let con = t.noz(ev, "CON", Svc::Hwr, q_mu, Spot::Back, Level::Low, &[Want::Iso]);
    t.drain(ev, "DRN", Spot::Back, Level::Low, 0.0);
    let ev_tag = t.eqs[ev].tag.clone();
    t.why(
        &ev_tag,
        "plant.heat.p_safety",
        "plant_expansion",
        vec![("vol", s1(vn)), ("sys", s1(v_sys)), ("fill", s1(p0)), ("safety", s1(rule("plant.heat.p_safety")))],
    );
    t.groups.push(Group { key: "separator", members: vec![hs, ev], row: None });
    let thr = t.terminal(Svc::Hwr, "HWR", q_net, &[Want::Iso, Want::Item(ItemKind::Strainer)]);
    let mut ends = vec![sin, con];
    ends.extend(md);
    t.line(Svc::Hwr, thr, ends, q_net + q_mu);

    // gas to every burner
    let tng = t.terminal(Svc::Ng, "NG", gas * n_duty as f64, &[Want::Iso, Want::Item(ItemKind::ShutOff), Want::Item(ItemKind::FlowMeter)]);
    t.line(Svc::Ng, tng, gasn, gas * n_duty as f64);
    Ok(())
}

fn pumping_station(t: &mut Topo, b: &Brief) -> Result<(), String> {
    let q = b.flow_m3h.unwrap_or_default();
    let head = b.head_m.unwrap_or_else(|| rule("plant.pump.head_station"));
    let (suc, dis) = t.pump_set("station_pump", "station_pumps", Svc::Rw, Svc::Rw, q, head, b.units, Some(Row::South));
    let tin = t.terminal(Svc::Rw, "RWI", q, &[Want::Iso, Want::Item(ItemKind::Strainer)]);
    t.line(Svc::Rw, tin, suc, q);
    let hold = rule("plant.station.surge_s");
    let v = q / 3600.0 * hold;
    let hd = rule("plant.vessel.hd");
    let d = vessel_d(size::d_of_volume(v, hd));
    let sv = t.unit(EqKind::Surge, "surge", Dims { d, h: hd * d, ..Dims::default() }, vec![("volume_m3", v)], false);
    let con = t.noz(sv, "CON", Svc::Rw, q, Spot::Back, Level::Low, &[Want::Iso]);
    t.drain(sv, "DRN", Spot::Back, Level::Low, 0.0);
    let tag = t.eqs[sv].tag.clone();
    t.why(&tag, "plant.station.surge_s", "plant_surge", vec![("tag", tag.clone()), ("vol", s2(v)), ("s", s1(hold))]);
    t.groups.push(Group { key: "surge", members: vec![sv], row: Some(Row::North) });
    let tout = t.terminal(Svc::Rw, "RWO", q, &[Want::Iso, Want::Item(ItemKind::FlowMeter)]);
    let mut ends = dis;
    ends.push(con);
    t.line(Svc::Rw, tout, ends, q);
    Ok(())
}

fn water_treatment(t: &mut Topo, b: &Brief) -> Result<(), String> {
    let q = b.flow_m3h.unwrap_or_default();
    let hold = q * rule("plant.wt.tank_hold");
    let rt = t.tank(EqKind::Tank, "raw_tank", hold, "plant.wt.tank_hold");
    let rt_in = t.noz(rt, "IN", Svc::Rw, q, Spot::Top, Level::High, &[]);
    let q_dos = rule("plant.wt.dosing_flow") / L_PER_M3;
    let rt_dos = t.noz(rt, "DOS", Svc::Ch, q_dos, Spot::Top, Level::High, &[]);
    let rt_out = t.noz(rt, "OUT", Svc::Rw, q, Spot::Back, Level::Low, &[Want::Iso]);
    t.drain(rt, "DRN", Spot::Back, Level::Low, 0.0);
    let ct = t.tank(EqKind::Tank, "clean_tank", hold, "plant.wt.tank_hold");
    let ct_in = t.noz(ct, "IN", Svc::Tw, q, Spot::Top, Level::High, &[]);
    let ct_out = t.noz(ct, "OUT", Svc::Tw, q, Spot::Back, Level::Low, &[Want::Iso]);
    t.drain(ct, "DRN", Spot::Back, Level::Low, 0.0);
    t.groups.push(Group { key: "tanks", members: vec![rt, ct], row: Some(Row::North) });

    // pressure filters by the filtration rate, the largest diameter and standby
    let rate = rule("plant.wt.filter_rate");
    let dmax = rule("plant.wt.filter_dmax");
    let area = q / rate;
    let n_duty = b.units.unwrap_or(((area / (PI / 4.0 * dmax * dmax) - 1e-9).ceil() as usize).max(1));
    let d_f = vessel_d(size::d_of_area(area / n_duty as f64));
    let sb = rule("plant.wt.filter_standby") as usize;
    let q_f = q / n_duty as f64;
    let q_bw = rule("plant.wt.backwash_rate") * PI / 4.0 * d_f * d_f;
    let (mut f_in, mut f_out, mut f_bwi, mut filters) = (vec![], vec![], vec![], vec![]);
    for i in 0..n_duty + sb {
        let dims = Dims { d: d_f, h: rule("plant.wt.filter_h"), ..Dims::default() };
        let e = t.unit(EqKind::Filter, "filter", dims, vec![("flow_m3h", q_f), ("backwash_m3h", q_bw)], i >= n_duty);
        f_in.push(t.noz(e, "IN", Svc::Rw, q_f, Spot::Top, Level::High, &[Want::Iso]));
        f_out.push(t.noz(e, "OUT", Svc::Tw, q_f, Spot::Back, Level::Low, &[Want::Iso]));
        f_bwi.push(t.noz(e, "BWI", Svc::Bw, q_bw, Spot::Back, Level::Low, &[Want::Iso]));
        t.drain(e, "BWO", Spot::Back, Level::High, q_bw);
        t.drain(e, "DRN", Spot::Back, Level::Low, 0.0);
        filters.push(e);
    }
    let f0 = t.eqs[filters[0]].tag.clone();
    t.why(&f0, "plant.wt.filter_rate", "plant_filters", vec![("n", s(n_duty)), ("d", s2(d_f)), ("flow", s1(q)), ("rate", s1(rate)), ("standby", s(sb))]);
    t.groups.push(Group { key: "filters", members: filters, row: Some(Row::North) });

    let (fs, fd) = t.pump_set("feed_pump", "feed_pumps", Svc::Rw, Svc::Rw, q, rule("plant.pump.head_transfer"), None, None);
    let (ds, dd) = t.pump_set("delivery_pump", "delivery_pumps", Svc::Tw, Svc::Tw, q, rule("plant.pump.head_station"), None, None);
    let psb = rule("plant.pump.standby") as usize;
    let (bs, bd) = t.pumps("backwash_pump", "backwash_pumps", Svc::Tw, Svc::Bw, q_bw, 1, psb, rule("plant.pump.head_transfer"), None);

    let vol = rule("plant.wt.dosing_l") / L_PER_M3;
    let hd = rule("plant.tank.hd");
    let d_dt = vessel_d(size::d_of_volume(vol, hd));
    let dt = t.unit(EqKind::Dosing, "dosing_tank", Dims { d: d_dt, h: hd * d_dt, ..Dims::default() }, vec![("volume_m3", vol)], false);
    let dt_out = t.noz(dt, "OUT", Svc::Ch, q_dos, Spot::Back, Level::Low, &[Want::Iso]);
    t.drain(dt, "DRN", Spot::Back, Level::Low, 0.0);
    let dt_tag = t.eqs[dt].tag.clone();
    t.why(&dt_tag, "plant.wt.dosing_l", "plant_dosing", vec![("tag", dt_tag.clone()), ("vol", s1(vol * L_PER_M3)), ("q", s1(q_dos * L_PER_M3))]);
    t.groups.push(Group { key: "dosing_tank", members: vec![dt], row: None });
    let (xs, xd) = t.pumps("dosing_pump", "dosing_pumps", Svc::Ch, Svc::Ch, q_dos, 1, psb, rule("plant.pump.head_dosing"), None);

    let tin = t.terminal(Svc::Rw, "RWI", q, &[Want::Iso, Want::Item(ItemKind::FlowMeter)]);
    t.line(Svc::Rw, tin, vec![rt_in], q);
    t.line(Svc::Rw, rt_out, fs, q);
    let mut feed = f_in[1..].to_vec();
    feed.extend(fd);
    t.line(Svc::Rw, f_in[0], feed, q);
    t.line(Svc::Tw, ct_in, f_out, q);
    let mut draw = ds;
    draw.extend(bs);
    t.line(Svc::Tw, ct_out, draw, q + q_bw);
    let tout = t.terminal(Svc::Tw, "TWO", q, &[Want::Iso, Want::Item(ItemKind::FlowMeter)]);
    t.line(Svc::Tw, tout, dd, q);
    let mut bw = f_bwi[1..].to_vec();
    bw.extend(bd);
    t.line(Svc::Bw, f_bwi[0], bw, q_bw);
    t.line(Svc::Ch, dt_out, xs, q_dos);
    t.line(Svc::Ch, rt_dos, xd, q_dos);
    Ok(())
}

/// A distillation column on the product: its nozzles, the reboiler on hot water and the
/// condenser on cooling water, with the process ends the lines join.
struct Distil {
    feed: End,
    refl: End,
    btm: End,
    ret: End,
    ovh: End,
    rb_in: End,
    rb_out: End,
    rb_hi: End,
    rb_ho: End,
    cd_in: End,
    cd_out: End,
    cd_wi: End,
    cd_wo: End,
    /// Flows, m³/h: reflux, bottoms, hot water to the reboiler, cooling water to the condenser.
    q_refl: f64,
    q_btm: f64,
    q_hot: f64,
    q_cw: f64,
}

/// Column, reboiler and condenser for `q` m³/h of feed with `q_d` m³/h drawn as distillate.
fn distillation(t: &mut Topo, q: f64, q_d: f64) -> Result<Distil, String> {
    let (ts, tr) = (rule("plant.heat.t_supply"), rule("plant.heat.t_return"));
    let (cw_in, cw_dt) = (rule("plant.process.cw_in"), rule("plant.process.cw_dt"));
    let t_boil = rule("plant.column.t_boil");
    if tr <= t_boil + 1.0 || cw_in + cw_dt >= t_boil - 1.0 {
        return Err("plant.column.t_boil must lie above the cooling water outlet and below the heating water return".into());
    }
    let reflux = rule("plant.column.reflux");
    let q_refl = q_d * reflux;
    let q_v = q_d + q_refl;
    let q_btm = q - q_d;
    let kw = q_v * rule("plant.column.latent");
    let u = rule("plant.hx.u");
    let margin = 1.0 + rule("plant.hx.margin");
    let tube_l = rule("plant.hx.tube_l");
    let shell = |area: f64| {
        let tubes = area / (PI * TUBE_OD * tube_l);
        vessel_d(BUNDLE_K * TUBE_PITCH * TUBE_OD * tubes.sqrt())
    };
    let lmtd = |d1: f64, d2: f64| if (d1 - d2).abs() < 1e-6 { d1 } else { (d1 - d2) / (d1 / d2).ln() };

    let vol = q * rule("plant.column.hold") / 60.0;
    let hd = rule("plant.column.hd");
    let d_c = vessel_d(size::d_of_volume(vol, hd));
    let col = t.unit(EqKind::Column, "column", Dims { d: d_c, h: hd * d_c, ..Dims::default() }, vec![("volume_m3", vol), ("boilup_m3h", q_v)], false);
    let feed = t.noz(col, "FEED", Svc::Pr, q, Spot::Left, Level::Mid, &[Want::Iso]);
    let ovh = t.noz(col, "OVH", Svc::Pr, q_v, Spot::Top, Level::High, &[Want::Iso]);
    let refl = t.noz(col, "REFLUX", Svc::Pr, q_refl, Spot::Right, Level::High, &[Want::Iso, Want::Item(ItemKind::Ti)]);
    let ret = t.noz(col, "RETURN", Svc::Pr, q_v, Spot::Right, Level::Low, &[Want::Iso]);
    let btm = t.noz(col, "BTM", Svc::Pr, q_btm + q_v, Spot::Back, Level::Low, &[Want::Iso]);
    t.drain(col, "DRN", Spot::Back, Level::Low, 0.0);
    let c_tag = t.eqs[col].tag.clone();
    t.why(&c_tag, "plant.column.hold", "plant_column", vec![("tag", c_tag.clone()), ("vol", s2(vol)), ("hold", s1(rule("plant.column.hold"))), ("d", s2(d_c)), ("h", s1(hd * d_c))]);
    t.why(&c_tag, "plant.column.reflux", "plant_reflux", vec![("tag", c_tag.clone()), ("r", s1(reflux)), ("q", s2(q_refl)), ("v", s2(q_v))]);
    t.groups.push(Group { key: "column", members: vec![col], row: Some(Row::North) });

    // reboiler: hot water on the shell boils the bottoms in the tubes
    let q_hot = size::heat_flow(kw, ts - tr, tr);
    let area_r = kw * W_PER_KW / (u * lmtd(ts - t_boil, tr - t_boil)) * margin;
    let rb = t.unit(EqKind::Exchanger, "reboiler", Dims { d: shell(area_r), l: tube_l, kw, ..Dims::default() }, vec![("area_m2", area_r), ("heat_kw", kw)], false);
    let rb_hi = t.noz(rb, "SHI", Svc::Hws, q_hot, Spot::Top, Level::High, &[Want::Iso]);
    let rb_ho = t.noz(rb, "SHO", Svc::Hwr, q_hot, Spot::Top, Level::High, &[Want::Iso, Want::Item(ItemKind::Ti)]);
    let rb_in = t.noz(rb, "TUI", Svc::Pr, q_v, Spot::Back, Level::Mid, &[Want::Iso]);
    let rb_out = t.noz(rb, "TUO", Svc::Pr, q_v, Spot::Top, Level::High, &[Want::Iso, Want::Item(ItemKind::Ti)]);
    t.drain(rb, "DRN", Spot::Back, Level::Low, 0.0);
    let rb_tag = t.eqs[rb].tag.clone();
    t.why(&rb_tag, "plant.column.latent", "plant_exchanger", vec![("tag", rb_tag.clone()), ("area", s1(area_r)), ("kw", s1(kw)), ("u", s1(u))]);
    t.groups.push(Group { key: "reboiler", members: vec![rb], row: None });

    // condenser: the overhead vapour gives its heat to cooling water
    let q_cw = size::heat_flow(kw, cw_dt, cw_in);
    let area_c = kw * W_PER_KW / (u * lmtd(t_boil - (cw_in + cw_dt), t_boil - cw_in)) * margin;
    let cd = t.unit(EqKind::Exchanger, "condenser", Dims { d: shell(area_c), l: tube_l, kw, ..Dims::default() }, vec![("area_m2", area_c), ("heat_kw", kw)], false);
    let cd_wi = t.noz(cd, "SHI", Svc::Cws, q_cw, Spot::Top, Level::High, &[Want::Iso]);
    let cd_wo = t.noz(cd, "SHO", Svc::Cwr, q_cw, Spot::Top, Level::High, &[Want::Iso, Want::Item(ItemKind::Ti)]);
    let cd_in = t.noz(cd, "TUI", Svc::Pr, q_v, Spot::Back, Level::Mid, &[Want::Iso]);
    let cd_out = t.noz(cd, "TUO", Svc::Pr, q_v, Spot::Top, Level::High, &[Want::Iso, Want::Item(ItemKind::Ti)]);
    t.drain(cd, "DRN", Spot::Back, Level::Low, 0.0);
    let cd_tag = t.eqs[cd].tag.clone();
    t.why(&cd_tag, "plant.column.latent", "plant_exchanger", vec![("tag", cd_tag.clone()), ("area", s1(area_c)), ("kw", s1(kw)), ("u", s1(u))]);
    t.groups.push(Group { key: "condenser", members: vec![cd], row: None });
    Ok(Distil { feed, refl, btm, ret, ovh, rb_in, rb_out, rb_hi, rb_ho, cd_in, cd_out, cd_wi, cd_wo, q_refl, q_btm, q_hot, q_cw })
}

fn process_line(t: &mut Topo, b: &Brief) -> Result<(), String> {
    let q = b.flow_m3h.unwrap_or_default();
    let (t_feed, t_hot, t_out) = (rule("plant.process.t_feed"), rule("plant.process.t_hot"), rule("plant.process.t_out"));
    let (cw_in, cw_dt) = (rule("plant.process.cw_in"), rule("plant.process.cw_dt"));
    let (ts, tr) = (rule("plant.heat.t_supply"), rule("plant.heat.t_return"));
    if t_hot <= t_feed || t_hot <= t_out || ts <= tr || ts <= t_hot {
        return Err("plant.process.t_hot must lie above the feed and outlet temperatures and below plant.heat.t_supply".into());
    }
    let dt1 = t_hot - (cw_in + cw_dt);
    let dt2 = t_out - cw_in;
    if dt1 <= 0.0 || dt2 <= 0.0 {
        return Err("the cooling water (plant.process.cw_in + cw_dt) must stay below the product temperatures".into());
    }
    // what leaves as product: all of the flow, or the distillate when a column splits it
    let q_p = if b.distill { q * (1.0 - rule("plant.column.bottoms")) } else { q };
    let hold = q * rule("plant.process.tank_hold");
    let nft = b.feed_tanks.max(1);
    let mut feeds = vec![];
    for _ in 0..nft {
        let rt = t.tank(EqKind::Tank, "raw_tank", hold / nft as f64, "plant.process.tank_hold");
        let rt_in = t.noz(rt, "IN", Svc::Pr, q / nft as f64, Spot::Top, Level::High, &[]);
        let rt_out = t.noz(rt, "OUT", Svc::Pr, q / nft as f64, Spot::Back, Level::Low, &[Want::Iso]);
        t.drain(rt, "DRN", Spot::Back, Level::Low, 0.0);
        feeds.push((rt, rt_in, rt_out));
    }
    let pt = t.tank(EqKind::Tank, "product_tank", q_p * rule("plant.process.tank_hold"), "plant.process.tank_hold");
    let pt_in = t.noz(pt, "IN", Svc::Pr, q_p, Spot::Top, Level::High, &[]);
    let pt_out = t.noz(pt, "OUT", Svc::Pr, q_p, Spot::Back, Level::Low, &[Want::Iso]);
    t.drain(pt, "DRN", Spot::Back, Level::Low, 0.0);
    let mut tanks: Vec<usize> = feeds.iter().map(|f| f.0).collect();
    tanks.push(pt);
    t.groups.push(Group { key: "tanks", members: tanks, row: Some(Row::North) });

    // jacketed batch reactor heated by hot water
    let batch = rule("plant.process.batch_h");
    let fill = rule("plant.process.fill");
    let vol = q * batch / fill;
    let hd = rule("plant.vessel.hd");
    let d_r = vessel_d(size::d_of_volume(vol, hd));
    let kw_h = size::heat_of(q, t_hot - t_feed, t_feed);
    let q_hm = size::heat_flow(kw_h, ts - tr, tr);
    let re = t.unit(EqKind::Reactor, "reactor", Dims { d: d_r, h: hd * d_r, kw: kw_h, ..Dims::default() }, vec![("volume_m3", vol), ("heat_kw", kw_h)], false);
    let re_feed = t.noz(re, "FEED", Svc::Pr, q, Spot::Top, Level::High, &[]);
    let re_out = t.noz(re, "OUT", Svc::Pr, q, Spot::Back, Level::Low, &[Want::Iso]);
    let re_jin = t.noz(re, "JIN", Svc::Hws, q_hm, Spot::Left, Level::Low, &[Want::Iso, Want::Item(ItemKind::Ti)]);
    let re_jout = t.noz(re, "JOUT", Svc::Hwr, q_hm, Spot::Right, Level::High, &[Want::Iso]);
    t.drain(re, "DRN", Spot::Back, Level::Low, 0.0);
    let re_tag = t.eqs[re].tag.clone();
    t.why(&re_tag, "plant.process.batch_h", "plant_reactor", vec![("tag", re_tag.clone()), ("vol", s2(vol)), ("batch", s1(batch)), ("fill", s1(fill * 100.0))]);
    t.why(&re_tag, "plant.process.t_hot", "plant_reactor_heat", vec![("tag", re_tag.clone()), ("kw", s1(kw_h)), ("q", s2(q_hm))]);
    t.groups.push(Group { key: "reactor", members: vec![re], row: Some(Row::North) });

    // product cooler: counterflow shell-and-tube on cooling water
    let kw_c = size::heat_of(q_p, t_hot - t_out, t_out);
    let cw = size::heat_flow(kw_c, cw_dt, cw_in);
    let lmtd = if (dt1 - dt2).abs() < 1e-6 { dt1 } else { (dt1 - dt2) / (dt1 / dt2).ln() };
    let u = rule("plant.hx.u");
    let area = kw_c * W_PER_KW / (u * lmtd) * (1.0 + rule("plant.hx.margin"));
    let tube_l = rule("plant.hx.tube_l");
    let tubes = area / (PI * TUBE_OD * tube_l);
    let d_hx = vessel_d(BUNDLE_K * TUBE_PITCH * TUBE_OD * tubes.sqrt());
    let hx = t.unit(EqKind::Exchanger, "cooler", Dims { d: d_hx, l: tube_l, kw: kw_c, ..Dims::default() }, vec![("area_m2", area), ("heat_kw", kw_c)], false);
    let shi = t.noz(hx, "SHI", Svc::Cws, cw, Spot::Top, Level::High, &[Want::Iso]);
    let sho = t.noz(hx, "SHO", Svc::Cwr, cw, Spot::Top, Level::High, &[Want::Iso, Want::Item(ItemKind::Ti)]);
    let tui = t.noz(hx, "TUI", Svc::Pr, q_p, Spot::Back, Level::Mid, &[Want::Iso]);
    let tuo = t.noz(hx, "TUO", Svc::Pr, q_p, Spot::Top, Level::High, &[Want::Iso, Want::Item(ItemKind::Ti)]);
    t.drain(hx, "DRN", Spot::Back, Level::Low, 0.0);
    let hx_tag = t.eqs[hx].tag.clone();
    t.why(&hx_tag, "plant.hx.u", "plant_exchanger", vec![("tag", hx_tag.clone()), ("area", s1(area)), ("kw", s1(kw_c)), ("u", s1(u))]);
    t.groups.push(Group { key: "exchanger", members: vec![hx], row: None });

    let dist = if b.distill { Some(distillation(t, q, q_p)?) } else { None };

    let head = rule("plant.pump.head_transfer");
    let (ts_, td) = t.pump_set("transfer_pump", "transfer_pumps", Svc::Pr, Svc::Pr, q, head, None, None);
    let (ps, pd) = t.pump_set("product_pump", "product_pumps", Svc::Pr, Svc::Pr, q, head, None, None);
    let (xs, xd) = t.pump_set("dispatch_pump", "dispatch_pumps", Svc::Pr, Svc::Pr, q_p, head, None, None);

    let first_in: Vec<End> = feeds.iter().map(|f| f.1).collect();
    let tin = t.terminal(Svc::Pr, "PRI", q, &[Want::Iso, Want::Item(ItemKind::FlowMeter)]);
    t.line(Svc::Pr, tin, first_in, q);
    let mut draw: Vec<End> = feeds[1..].iter().map(|f| f.2).collect();
    draw.extend(ts_);
    t.line(Svc::Pr, feeds[0].2, draw, q);
    t.line(Svc::Pr, re_feed, td, q);
    t.line(Svc::Pr, re_out, ps, q);
    t.line(Svc::Pr, pt_in, vec![tuo], q_p);
    t.line(Svc::Pr, pt_out, xs, q_p);
    let tout = t.terminal(Svc::Pr, "PRO", q_p, &[Want::Iso, Want::Item(ItemKind::FlowMeter)]);
    t.line(Svc::Pr, tout, xd, q_p);
    let mut hot_ends = vec![re_jin];
    let mut hot_back = vec![re_jout];
    let mut cw_ends = vec![shi];
    let mut cw_back = vec![sho];
    let (mut q_hot, mut q_cw) = (q_hm, cw);
    match dist {
        None => {
            t.line(Svc::Pr, tui, pd, q);
        }
        Some(d) => {
            let (rs, rd) = t.pump_set("reflux_pump", "reflux_pumps", Svc::Pr, Svc::Pr, d.q_refl + q_p, head, None, None);
            let (bs, bd) = t.pump_set("bottoms_pump", "bottoms_pumps", Svc::Pr, Svc::Pr, d.q_btm, head, None, None);
            t.line(Svc::Pr, d.feed, pd, q);
            t.line(Svc::Pr, d.ovh, vec![d.cd_in], d.q_refl + q_p);
            t.line(Svc::Pr, d.cd_out, rs, d.q_refl + q_p);
            let mut back = rd;
            back.push(tui);
            t.line(Svc::Pr, d.refl, back, d.q_refl + q_p);
            let mut bot = vec![d.rb_in];
            bot.extend(bs);
            t.line(Svc::Pr, d.btm, bot, d.q_btm + d.q_refl + q_p);
            t.line(Svc::Pr, d.rb_out, vec![d.ret], d.q_refl + q_p);
            let bout = t.terminal(Svc::Pr, "BOT", d.q_btm, &[Want::Iso, Want::Item(ItemKind::FlowMeter)]);
            t.line(Svc::Pr, bout, bd, d.q_btm);
            hot_ends.push(d.rb_hi);
            hot_back.push(d.rb_ho);
            cw_ends.push(d.cd_wi);
            cw_back.push(d.cd_wo);
            q_hot += d.q_hot;
            q_cw += d.q_cw;
        }
    }
    let ths = t.terminal(Svc::Hws, "HWS", q_hot, &[Want::Iso, Want::Item(ItemKind::Strainer)]);
    t.line(Svc::Hws, ths, hot_ends, q_hot);
    let thr = t.terminal(Svc::Hwr, "HWR", q_hot, &[Want::Iso]);
    t.line(Svc::Hwr, thr, hot_back, q_hot);
    let tcs = t.terminal(Svc::Cws, "CWS", q_cw, &[Want::Iso, Want::Item(ItemKind::Strainer)]);
    t.line(Svc::Cws, tcs, cw_ends, q_cw);
    let tcr = t.terminal(Svc::Cwr, "CWR", q_cw, &[Want::Iso]);
    t.line(Svc::Cwr, tcr, cw_back, q_cw);
    Ok(())
}
