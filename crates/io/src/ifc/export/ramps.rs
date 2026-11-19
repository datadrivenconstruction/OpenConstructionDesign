//! Level ramps (IfcRamp from IfcRampFlight and IfcSlab LANDING via IfcRelAggregates) and zones
//! (IfcZone with spaces via IfcRelAssignsToGroup). A ramp that cannot be built is skipped
//! with a warning.

use std::collections::BTreeMap;

use ddc_geom::ramp::Ramp;
use ddc_model::Model;

use super::mep::{body, pset};
use super::stairs::material;
use super::{list, r, real, s, text, Ctx, Geo, NONE};

fn predefined(l: &Ramp) -> &'static str {
    let straight = l
        .flights
        .windows(2)
        .all(|w| w[0].dir.dot(w[1].dir) > 1.0 - 1e-9);
    match (straight, l.flights.len()) {
        (true, 1) => ".STRAIGHT_RUN_RAMP.",
        (true, 2) => ".TWO_STRAIGHT_RUN_RAMP.",
        _ => ".NOTDEFINED.",
    }
}

pub(super) fn items(
    c: &mut Ctx,
    model: &Model,
    level: i64,
    elev: f64,
    g: &Geo,
    elems: &mut Vec<u32>,
) -> Result<(), String> {
    let num = |v: f64| format!("IFCREAL({})", real(v));
    for id in model.ramps(level).map_err(|e| e.to_string())? {
        let Ok(l) = model.ramp_layout(id) else {
            c.g.warnings
                .push(format!("ramp {id} cannot be built: not exported"));
            continue;
        };
        let parts = l.meshes();
        let m = ddc_geom::mep::merge(&parts);
        if m.triangles.is_empty() {
            continue;
        }
        let o = c.point3(0.0, 0.0, 0.0);
        let ax = c.axis3(o, None);
        let pl = c.placement(Some(g.storey_pl), ax);
        // meshes go as flights, then landings; a failure of one mesh would shift the part classes
        let flights = l.flights.len();
        let split = parts.len() == flights + l.landings.len();
        let own = if split {
            s(NONE)
        } else {
            r(body(c, g, &m, elev))
        };
        let q = l.qty();
        let ent = c.rooted(
            "IFCRAMP",
            "ramp",
            id,
            &format!("Ramp {id}"),
            &[s(NONE), r(pl), own, s(NONE), s(predefined(&l))],
        );
        let mut all = vec![ent];
        if split {
            let mut kids = Vec::with_capacity(parts.len());
            for (k, part) in parts.iter().enumerate() {
                let pds = body(c, g, part, elev);
                let ppl = super::stairs::part_placement(c, pl);
                let (class, kind, pre, label) = if k < flights {
                    (
                        "IFCRAMPFLIGHT",
                        "ramp-flight",
                        ".STRAIGHT.",
                        format!("Flight {}", k + 1),
                    )
                } else {
                    (
                        "IFCSLAB",
                        "ramp-landing",
                        ".LANDING.",
                        format!("Landing {}", k - flights + 1),
                    )
                };
                kids.push(c.rooted(
                    class,
                    &format!("{kind}/{k}"),
                    id,
                    &label,
                    &[s(NONE), r(ppl), r(pds), s(NONE), s(pre)],
                ));
            }
            c.rel("IFCRELAGGREGATES", "ramp-parts", id, &[r(ent), list(&kids)]);
            all.extend(kids);
        }
        let f = vec![
            ("Width", num(l.def.width)),
            ("Slope", num(l.def.slope)),
            ("Rise", num(q.rise)),
            ("Run", num(q.run)),
            ("Volume", num(q.volume)),
            ("Landings", format!("IFCINTEGER({})", q.landings)),
        ];
        let sp = model.ramp(id).map_err(|e| e.to_string())?;
        let top = sp.top_level.map_or(String::new(), |t| {
            model
                .sql()
                .query_row("SELECT name FROM level WHERE id=?1", [t], |r| r.get(0))
                .unwrap_or_default()
        });
        let ex = |v: f64| format!("{v:?}");
        let spec = [
            sp.base_offset,
            sp.top_offset,
            sp.width,
            sp.slope,
            sp.thickness,
            sp.landing,
            sp.max_run,
        ]
        .map(ex)
        .join(" ");
        let path: Vec<String> = sp
            .path
            .iter()
            .map(|p| format!("{},{}", ex(p.0), ex(p.1)))
            .collect();
        let txt = |v: &str| format!("IFCTEXT({})", text(v));
        let mut f = f;
        f.push(("Spec", txt(&spec)));
        f.push(("Path", txt(&path.join(";"))));
        f.push(("TopLevel", txt(&top)));
        f.push(("Material", txt(&sp.material)));
        pset(c, "ramp-pset", id, "DDC_Ramp", ent, f);
        material(c, "ramp", id, &sp.material, &all);
        elems.push(ent);
    }
    Ok(())
}

/// Project zones; `spaces` — room → IfcSpace. A zone without exported rooms is skipped.
pub(super) fn zones(c: &mut Ctx, model: &Model, spaces: &BTreeMap<i64, u32>) -> Result<(), String> {
    for (id, z) in model.zones().map_err(|e| e.to_string())? {
        let members: Vec<u32> = z
            .rooms
            .iter()
            .filter_map(|x| spaces.get(x).copied())
            .collect();
        if members.is_empty() {
            continue;
        }
        let ent = c.rooted("IFCZONE", "zone", id, &z.name, &[s(NONE), text(&z.name)]);
        c.rel(
            "IFCRELASSIGNSTOGROUP",
            "zone-group",
            id,
            &[list(&members), s(NONE), r(ent)],
        );
    }
    Ok(())
}
