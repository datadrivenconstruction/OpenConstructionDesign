//! Schedules as tables (header row first) and a schematic of the systems as SVG: the unit and
//! plant on the roof, the air and water risers, and per storey its terminal units.

use super::equip::Kind;
use super::loads::{COOL_PARTS, HEAT_PARTS};
use super::pipes::SLOTS;
use super::super::AirSys;
use super::Mech;
use std::fmt::Write;

fn f(x: f64, d: usize) -> String {
    format!("{x:.d$}")
}

fn mm(x: f64) -> String {
    format!("{:.0}", x * 1000.0)
}

/// One row per room: area, people, heating and cooling by part, outdoor air.
pub fn loads(m: &Mech) -> Vec<Vec<String>> {
    let mut head: Vec<String> = ["storey", "room", "kind", "area_m2", "people"].map(String::from).to_vec();
    head.extend(HEAT_PARTS.iter().map(|p| format!("heat_{p}_w")));
    head.push("heat_w".into());
    head.extend(COOL_PARTS.iter().map(|p| format!("cool_{p}_w")));
    head.extend(["latent_w", "sensible_w", "oa_m3h"].map(String::from));
    let mut out = vec![head];
    for l in &m.loads {
        let mut r = vec![(l.storey + 1).to_string(), l.number.clone(), format!("{:?}", l.kind), f(l.area, 1), f(l.people, 0)];
        r.extend(l.heat.iter().map(|x| f(*x, 0)));
        r.push(f(l.heating(), 0));
        r.extend(l.cool.iter().map(|x| f(*x, 0)));
        r.extend([f(l.latent, 0), f(l.sensible(), 0), f(l.oa, 0)]);
        out.push(r);
    }
    out
}

/// One row per unit: tag, kind, storey, size, its ratings; the plant and pumps after.
pub fn equipment(m: &Mech) -> Vec<Vec<String>> {
    let mut out = vec![["tag", "kind", "storey", "l_mm", "w_mm", "h_mm", "ports", "duty"].map(String::from).to_vec()];
    let all = m.equip.iter().chain(&m.water.plant).chain(&m.water.pumps).chain(&m.air.dampers);
    for e in all {
        let duty: Vec<String> = e.duty.iter().map(|(k, v)| format!("{k}={}", f(*v, 2))).collect();
        let mut duty = duty.join("; ");
        if !e.parts.is_empty() {
            let parts: Vec<String> = e.parts.iter().map(|q| format!("{} {} Pa", q.name, f(q.dp, 0))).collect();
            duty = format!("{duty}; sections: {}", parts.join(", "));
        }
        out.push(vec![
            e.tag.clone(),
            e.kind.name().into(),
            e.storey.to_string(),
            mm(e.size.0),
            mm(e.size.1),
            mm(e.size.2),
            e.ports.iter().map(|p| p.name).collect::<Vec<_>>().join(" "),
            duty,
        ]);
    }
    for (i, fan) in m.fans.iter().enumerate() {
        let name = if i == 0 { "supply fan" } else { "extract fan" };
        out.push(vec![
            format!("{}-F{}", Kind::Ahu.tag(), i + 1),
            name.into(),
            m.equip.first().map_or(String::new(), |e| e.storey.to_string()),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            format!(
                "flow_m3h={}; dp_ext_pa={}; dp_unit_pa={}; shaft_kw={}; motor_kw={}",
                f(fan.flow, 0),
                f(fan.dp_ext, 0),
                f(fan.dp_unit, 0),
                f(fan.shaft_kw, 2),
                f(fan.motor_kw, 2)
            ),
        ]);
    }
    out
}

/// One row per duct run in tree order with its size, flow, velocity, friction, fittings,
/// insulation; the fittings follow as rows of their own.
pub fn ducts(m: &Mech) -> Vec<Vec<String>> {
    let mut out = vec![["run", "system", "class", "storey", "size_mm", "flow_m3h", "v_ms", "v_max", "r_pam", "len_m", "friction_pa", "fittings_pa", "insulation_mm"]
        .map(String::from)
        .to_vec()];
    for (i, r) in m.air.runs.iter().enumerate() {
        let size = if r.size.1 > 0.0 { format!("{}x{}", mm(r.size.0), mm(r.size.1)) } else { format!("d{}", mm(r.size.0)) };
        out.push(vec![
            i.to_string(),
            format!("{:?}", r.sys).to_lowercase(),
            r.class.name().into(),
            r.storey.to_string(),
            size,
            f(r.flow, 0),
            f(r.v, 2),
            f(r.v_max, 1),
            f(r.r, 2),
            f(r.len, 2),
            f(r.friction, 1),
            f(r.fittings, 1),
            mm(r.insulation),
        ]);
    }
    for ft in &m.air.fittings {
        out.push(vec![
            format!("fit:{}", ft.run),
            format!("{:?}", ft.sys).to_lowercase(),
            ft.kind.name().into(),
            ft.storey.to_string(),
            format!("{}>{}", mm(ft.size.0.max(ft.size.1)), mm(ft.size2.0.max(ft.size2.1))),
            String::new(),
            String::new(),
            String::new(),
            format!("zeta={}", f(ft.zeta, 2)),
            format!("r={}", f(ft.radius, 2)),
            String::new(),
            f(ft.dp, 1),
            String::new(),
        ]);
    }
    out
}

/// One row per pipe: medium, class, DN, flow, velocity, friction, length, insulation.
pub fn pipes(m: &Mech) -> Vec<Vec<String>> {
    let mut out = vec![["pipe", "medium", "class", "storey", "dn", "flow_m3h", "v_ms", "r_pam", "len_m", "fittings_pa", "insulation_mm"].map(String::from).to_vec()];
    for (i, p) in m.water.pipes.iter().enumerate() {
        out.push(vec![
            i.to_string(),
            SLOTS[p.slot].name().into(),
            p.class.name().into(),
            p.storey.to_string(),
            p.dn.to_string(),
            f(p.q, 2),
            f(p.v, 2),
            f(p.r, 0),
            f(p.len, 2),
            f(p.fittings, 0),
            mm(p.ins),
        ]);
    }
    for v in &m.water.valves {
        out.push(vec![
            format!("valve:{}", v.kind.name()),
            SLOTS[v.slot].name().into(),
            String::new(),
            String::new(),
            v.dn.to_string(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            f(v.dp, 0),
            String::new(),
        ]);
    }
    out
}

/// The systems as a riser diagram: the roof row with the unit and plant, a row per storey
/// with its terminal units, the supply and extract risers and each water riser between them.
pub fn schematic(m: &Mech) -> String {
    let storeys = m.loads.iter().map(|l| l.storey + 1).max().unwrap_or(0);
    let (row, w) = (60.0, 760.0);
    let h = row * (storeys as f64 + 2.0);
    let y_of = |k: usize| h - row * (k as f64 + 1.0);
    let mut s = String::new();
    let _ = write!(s, r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" font-family="sans-serif" font-size="11">"#);
    let lines = [("supply", "#2b6cb0", 120.0), ("extract", "#c05621", 150.0), ("chw", "#2c7a7b", 180.0), ("hw", "#c53030", 210.0)];
    for k in 0..storeys {
        let y = y_of(k);
        let _ = write!(s, r##"<line x1="20" y1="{y}" x2="{}" y2="{y}" stroke="#999" stroke-dasharray="4 3"/><text x="24" y="{}">L{}</text>"##, w - 20.0, y - 4.0, k + 1);
    }
    let roof = y_of(storeys);
    let _ = write!(s, r##"<line x1="20" y1="{roof}" x2="{}" y2="{roof}" stroke="#333"/>"##, w - 20.0);
    let has_water = [m.water.pipes.iter().any(|p| p.slot < 2),m.water.pipes.iter().any(|p| p.slot >= 2)];
    for (i, (name, color, x)) in lines.iter().enumerate() {
        if i >= 2 && !has_water[i - 2] {
            continue;
        }
        let _ = write!(s, r#"<line x1="{x}" y1="{}" x2="{x}" y2="{}" stroke="{color}" stroke-width="2"/><text x="{}" y="{}" fill="{color}" transform="rotate(-90 {} {})">{name}</text>"#, roof - 20.0, y_of(0) - 10.0, x - 3.0, roof - 24.0, x - 3.0, roof - 24.0);
    }
    if let Some(ahu) = m.equip.first().filter(|e| e.kind == Kind::Ahu) {
        let _ = write!(
            s,
            r##"<rect x="100" y="{}" width="130" height="34" fill="none" stroke="#333"/><text x="106" y="{}">{} {} m3/h</text><text x="106" y="{}">{} Pa / {} kW</text>"##,
            roof - 50.0,
            roof - 36.0,
            ahu.tag,
            f(m.supply, 0),
            roof - 22.0,
            f(m.fans[0].dp(), 0),
            f(m.fans[0].motor_kw, 1)
        );
    }
    let mut x = 260.0;
    for e in m.water.plant.iter().chain(&m.water.pumps) {
        let _ = write!(s, r##"<rect x="{x}" y="{}" width="90" height="34" fill="none" stroke="#333"/><text x="{}" y="{}">{}</text><text x="{}" y="{}">{} kW</text>"##, roof - 50.0, x + 6.0, roof - 36.0, e.tag, x + 6.0, roof - 22.0, f(e.duty("kw").max(e.duty("motor_kw")), 1));
        x += 100.0;
    }
    for k in 0..storeys {
        let y = y_of(k);
        let mut counts: Vec<(Kind, usize)> = vec![];
        for e in m.equip.iter().filter(|e| e.storey == k) {
            match counts.iter_mut().find(|c| c.0 == e.kind) {
                Some(c) => c.1 += 1,
                None => counts.push((e.kind, 1)),
            }
        }
        let terms = |sys: AirSys| m.air.terms.iter().filter(|t| t.storey == k && t.sys == sys).count();
        let mut label = format!("{} supply, {} extract terminals", terms(AirSys::Supply), terms(AirSys::Extract));
        for (kind, n) in counts {
            let _ = write!(label, "; {n} x {}", kind.tag());
        }
        let _ = write!(s, r##"<line x1="120" y1="{}" x2="250" y2="{}" stroke="#2b6cb0"/><text x="260" y="{}">{label}</text>"##, y - 20.0, y - 20.0, y - 16.0);
    }
    s.push_str("</svg>");
    s
}
