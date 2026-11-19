//! What a built project owes its typology: the clear heights the spaces ask for, the levels
//! below ground and the rooms placed on them, the exits and their width for the people the
//! spaces hold, and the retaining walls against the depth they hold back. A failure means the
//! project must not be offered; a warning is advice beside the draft.

use super::super::nav;
use super::super::{GProject, ProjectSpec};
use super::Level;

/// Failures and warnings of the typology of `spec` on the built project.
pub(crate) fn run(spec: &ProjectSpec, p: &GProject) -> (Vec<String>, Vec<String>) {
    let (mut fail, mut warn) = (vec![], vec![]);
    let Some(m) = spec.model.as_ref() else {
        return (fail, warn);
    };
    let ru = spec.ru;
    let built = super::schedule_of(spec, p).unwrap_or_default();
    // a space with a clear height asked stands on the tallest storey
    let clear = spec.tallest_storey() - spec.rules.get("services.slab");
    for s in &m.spaces {
        if m.storeys_of(s, spec.floors, spec.basements, &spec.rules) == 0 {
            continue;
        }
        if let Some(h) = s.height_min.filter(|h| *h > clear + 1e-9) {
            fail.push(format!(
                "{}: needs {h} m clear height, the storeys give {clear:.2} m; raise massing.floor_height",
                s.name.0
            ));
        }
    }
    for s in m.spaces.iter().filter(|s| s.count_in(Some(&spec.rules)) > 0) {
        let st = m.storeys_of(s, spec.floors, spec.basements, &spec.rules);
        if st == 0 {
            continue;
        }
        let asked = s.count_in(Some(&spec.rules));
        let built: u32 = built_rooms(p, s, ru);
        if built == 0 {
            fail.push(format!("no '{}' built, the typology asks for {asked} on {st} storey(s)", s.name.0));
        } else if built < asked * st {
            warn.push(format!("{built} of {} '{}' built", asked * st, s.name.0));
        }
    }
    let air_below: f64 = built.rows.iter().filter(|r| r.below).map(|r| r.supply_m3h + r.extract_m3h).sum();
    if air_below > 0.0 {
        warn.push(format!(
            "the levels below ground need {air_below:.0} m3/h of air in all; the duct network serves the storeys above ground only"
        ));
    }
    if let Some(b) = &m.below {
        let (have, want) = (p.basements.len(), b.levels.len());
        if have < want {
            warn.push(format!("{have} of the {want} levels below ground of the typology are built; set basements to {want}"));
        } else if have > want {
            warn.push(format!("{have} storeys below ground, the typology defines {want}: the last level's fill repeats"));
        }
        for s in m.spaces.iter().filter(|s| s.count > 0) {
            let Level::Below(n) = s.level else { continue };
            let Some(st) = p.basements.get(n as usize - 1) else { continue };
            let is_fill = b.levels.get(n as usize - 1).is_some_and(|l| l.fill == s.class);
            let built = st.layout.rooms.iter().filter(|r| r.name == s.display(ru)).count() as u32;
            if built < s.count && !is_fill {
                fail.push(format!(
                    "level {n} below ground: {} of {} '{}' built",
                    built,
                    s.count,
                    s.name.0
                ));
            }
        }
        if b.retaining {
            let slender = spec.rules.get("underground.retaining_slenderness");
            let thick = spec.rules.get("underground.retaining_thickness");
            for (i, st) in p.basements.iter().enumerate() {
                let high = st.height;
                if high / thick > slender + 1e-9 {
                    warn.push(format!(
                        "level {} below ground: retaining walls {high} m high and {thick} m thick are slenderer than {slender}; lower the storey height or raise underground.retaining_slenderness",
                        i + 1
                    ));
                }
            }
        }
    }
    let t = built.totals();
    if let Some(ground) = p.storeys.first() {
        let outside = nav::doors(ground).iter().filter(|d| d.exterior).count() as u32;
        if outside < m.egress.exits_min {
            warn.push(format!("{outside} outside door(s) on the ground floor, the typology asks for {} exits", m.egress.exits_min));
        }
    }
    if m.egress.exits_min > 1 {
        for (k, st) in p.storeys.iter().enumerate().skip(1) {
            let n = st.layout.stairs.len() as u32;
            if n < m.egress.exits_min {
                warn.push(format!("storey {k}: {n} stair(s), the typology asks for {} ways out", m.egress.exits_min));
            }
        }
    }
    if t.persons > 0.0 {
        let per100 = spec.rules.get("typology.egress.width_per_100");
        let busiest = built
            .rows
            .iter()
            .map(|r| r.persons)
            .fold(0.0, f64::max);
        let need = busiest * per100 / 100.0;
        let have: f64 = p.storeys.first().map_or(0.0, |s| s.layout.stairs.iter().map(|x| x.width).sum());
        if have + 1e-9 < need {
            warn.push(format!(
                "stairs of {have:.1} m in all carry fewer than the {busiest:.0} people of the largest space class: {need:.1} m asked at {per100} m per 100 people"
            ));
        }
    }
    (fail, warn)
}

fn built_rooms(p: &GProject, s: &super::TSpace, ru: bool) -> u32 {
    p.storeys
        .iter()
        .chain(&p.basements)
        .flat_map(|st| st.rooms.iter().zip(&st.layout.rooms))
        .filter(|(pr, gr)| pr.kind == s.kind && gr.name == s.display(ru))
        .count() as u32
}
