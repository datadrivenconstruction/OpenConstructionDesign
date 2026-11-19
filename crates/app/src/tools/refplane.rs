//! Reference plane (RP): two clicks; named "<localized tool name> N".

use super::{tup, Active, Rec};
use ddc_geom::V2;

pub fn rec(a: &Active, p: V2, q: V2) -> Result<Rec, String> {
    let (p, q) = (tup(p), tup(q));
    let base = a.t("tool.refplane");
    Ok(Rec::new("refplane", move |m| {
        let n = m.ref_planes()?.len() + 1;
        m.add_ref_plane(&format!("{base} {n}"), p, q)?;
        Ok(1)
    }))
}
