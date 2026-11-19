// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Area schedule by the measurement standard of a norm pack: the rooms of every storey grouped
//! by use and counted the way the country counts (`net` room areas, `usable` use zones,
//! `gross_internal` inside the outer walls, `gross` to the outside of the outer walls), with
//! totals per storey and for the building. The room kind comes from the room's `Purpose`
//! parameter, else the pack vocabulary, else the name words of `data/area_methods.json`;
//! spaces outside the walls count by the pack's `area.factor.*` numbers.
//!
//! Gross and gross-internal areas are measured on the wall footprints: the outline of the wall
//! groups, less the footprints of the walls that stand on it for the internal one. A storey
//! with no closed outline has neither, and its schedule falls back to the net area.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use ddc_geom::profile::signed_area;
use ddc_geom::{Tol, V2};
use serde_json::Value as J;

use crate::checks::norm_pack::NormPack;
use crate::{Error, Model, PVal};

/// What a schedule counts: one of the pack's `area_standard` words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AreaMethod {
    Net,
    Usable,
    GrossInternal,
    Gross,
}

impl AreaMethod {
    pub const ALL: [AreaMethod; 4] = [Self::Net, Self::Usable, Self::GrossInternal, Self::Gross];
    pub fn key(self) -> &'static str {
        match self {
            Self::Net => "net",
            Self::Usable => "usable",
            Self::GrossInternal => "gross_internal",
            Self::Gross => "gross",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.key() == s)
    }
    fn is_geometry(self) -> bool {
        matches!(self, Self::GrossInternal | Self::Gross)
    }
}

/// One room: its net area (inside the walls) and the area the method counts for it.
#[derive(Clone, Debug, PartialEq)]
pub struct RoomArea {
    pub room: i64,
    pub level: i64,
    pub number: String,
    pub name: String,
    /// Use class, e.g. `living`, `circulation`, `balcony`, `other`.
    pub class: String,
    pub net: f64,
    /// Net area times the class weight of the method (the pack factor for spaces outside).
    pub counted: f64,
}

/// The rooms of one use on a storey.
#[derive(Clone, Debug, PartialEq)]
pub struct UseArea {
    pub class: String,
    pub rooms: usize,
    pub net: f64,
    pub counted: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StoreyArea {
    pub level: i64,
    pub name: String,
    pub elevation: f64,
    pub rooms: usize,
    pub uses: Vec<UseArea>,
    /// Rooms inside the walls, net.
    pub net: f64,
    /// Use zones only (no circulation, no technical rooms), net.
    pub usable: f64,
    /// Spaces outside the walls (balcony, loggia, terrace), net.
    pub external: f64,
    /// To the outside of the outer walls; None: no closed outline.
    pub gross: Option<f64>,
    /// Inside the outer walls, partitions included; None: no closed outline.
    pub gross_internal: Option<f64>,
    /// The storey's area by the method of the report.
    pub total: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AreaReport {
    /// Pack the standard came from.
    pub pack: String,
    pub method: AreaMethod,
    /// Where a geometry method had no outline, the storeys fell back to net.
    pub fallback: bool,
    pub storeys: Vec<StoreyArea>,
    pub rooms: Vec<RoomArea>,
    pub net: f64,
    pub usable: f64,
    pub gross: Option<f64>,
    pub gross_internal: Option<f64>,
    /// Building total by the method.
    pub total: f64,
}

struct Rules {
    classes: BTreeMap<String, (Vec<String>, Vec<String>)>,
    external: Vec<String>,
    other: String,
    methods: BTreeMap<String, Vec<String>>,
}

fn strings(v: &J) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str().map(str::to_lowercase))
        .collect()
}

fn rules() -> &'static Rules {
    static R: OnceLock<Rules> = OnceLock::new();
    R.get_or_init(|| {
        let j: J = serde_json::from_str(include_str!("../data/area_methods.json"))
            .expect("area_methods.json is valid");
        Rules {
            classes: j["classes"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(k, v)| (k.clone(), (strings(&v["kinds"]), strings(&v["words"]))))
                .collect(),
            external: strings(&j["external"]),
            other: j["other"].as_str().unwrap_or("other").to_string(),
            methods: j["methods"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(k, v)| (k.clone(), strings(&v["counts"])))
                .collect(),
        }
    })
}

/// Use class of a room by its kind (`Purpose` or vocabulary) or name.
pub fn class_of(pack: &NormPack, purpose: Option<&str>, name: &str) -> String {
    let r = rules();
    let kind = purpose
        .and_then(|p| pack.purpose.get(p).map(String::as_str))
        .or_else(|| pack.kind_of_name(name));
    if let Some(k) = kind {
        if let Some((c, _)) = r.classes.iter().find(|(_, (kinds, _))| kinds.iter().any(|x| x == k)) {
            return c.clone();
        }
    }
    let n = name.to_lowercase();
    // spaces outside the walls first: "Living room balcony" is a balcony
    let by_word = |c: &String| r.classes[c].1.iter().any(|w| n.contains(w.as_str()));
    r.external
        .iter()
        .find(|c| by_word(*c))
        .or_else(|| r.classes.keys().find(|c| by_word(*c)))
        .cloned()
        .unwrap_or_else(|| r.other.clone())
}

/// Weight of a class in a method: 1 for its counted classes, the pack factor for a space
/// outside the walls, else 0.
fn weight(pack: &NormPack, method: AreaMethod, class: &str) -> f64 {
    let r = rules();
    if r.external.iter().any(|c| c == class) {
        return pack
            .dim(&format!("area.factor.{class}"))
            .and_then(|d| d.value.as_f64())
            .unwrap_or(0.0);
    }
    if r.methods.get(method.key()).is_some_and(|c| c.iter().any(|x| x == class)) {
        1.0
    } else {
        0.0
    }
}

fn dist_to_ring(p: V2, ring: &[V2]) -> f64 {
    let mut best = f64::INFINITY;
    for i in 0..ring.len() {
        let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let l2 = dx * dx + dy * dy;
        let u = if l2 > 0.0 {
            (((p.x - a.x) * dx + (p.y - a.y) * dy) / l2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        best = best.min((p.x - a.x - u * dx).hypot(p.y - a.y - u * dy));
    }
    best
}

/// Footprint of the storey: (gross, gross_internal), None without a closed outline.
fn footprint(m: &Model, level: i64, elevation: f64) -> Result<Option<(f64, f64)>, Error> {
    const ON: f64 = 1e-3;
    let polys = crate::walls::room_wall_polys(&m.db, level, elevation)?;
    let a = ddc_geom::rooms::arrange(&polys, Tol::default());
    // outermost outlines: not inside another one
    let outer: Vec<&Vec<V2>> = a
        .outlines
        .iter()
        .filter(|o| {
            !a.outlines.iter().any(|q| {
                !std::ptr::eq(*o, q)
                    && signed_area(q).abs() > signed_area(o).abs()
                    && ddc_geom::predicates::point_in_ring(o[0], q) > 0
            })
        })
        .collect();
    if outer.is_empty() {
        return Ok(None);
    }
    let gross: f64 = outer.iter().map(|o| signed_area(o).abs()).sum();
    // a wall stands on the outline when one of its edges lies on it
    let wall: f64 = polys
        .iter()
        .filter(|p| {
            (0..p.len()).any(|i| {
                let (s, e) = (p[i], p[(i + 1) % p.len()]);
                outer
                    .iter()
                    .any(|o| dist_to_ring(s, o) < ON && dist_to_ring(e, o) < ON)
            })
        })
        .map(|p| signed_area(p).abs())
        .sum();
    Ok(Some((gross, (gross - wall).max(0.0))))
}

impl Model {
    /// The area report of the project by the measurement standard of `pack`.
    pub fn area_report(&self, pack: &NormPack) -> Result<AreaReport, Error> {
        let method = AreaMethod::parse(&pack.conventions().area_standard).unwrap_or(AreaMethod::Net);
        let purpose = self.param_by_name("Purpose")?;
        let r = rules();
        let mut report = AreaReport {
            pack: pack.id.clone(),
            method,
            fallback: false,
            storeys: vec![],
            rooms: vec![],
            net: 0.0,
            usable: 0.0,
            gross: None,
            gross_internal: None,
            total: 0.0,
        };
        let (mut gross, mut gia, mut any_geo) = (0.0, 0.0, false);
        for (level, name, elevation) in self.level_list()? {
            let rows: Vec<(i64, String, String, f64)> = {
                let mut st = self.db.prepare(
                    "SELECT r.id, r.number, r.name, q.area FROM room r JOIN room_qty q ON q.room=r.id \
                     WHERE r.level=?1 ORDER BY r.id",
                )?;
                let it = st.query_map([level], |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?, x.get(3)?)))?;
                it.collect::<Result<_, _>>()?
            };
            let fp = footprint(self, level, elevation)?;
            if rows.is_empty() && fp.is_none() {
                continue;
            }
            let mut s = StoreyArea {
                level,
                name,
                elevation,
                rooms: 0,
                uses: vec![],
                net: 0.0,
                usable: 0.0,
                external: 0.0,
                gross: fp.map(|f| f.0),
                gross_internal: fp.map(|f| f.1),
                total: 0.0,
            };
            let mut uses: BTreeMap<String, UseArea> = BTreeMap::new();
            for (id, number, rname, net) in rows {
                let p = match (&purpose, self.param_value_of_def(&purpose, id)?) {
                    (Some(_), Some(PVal::Text(t))) => Some(t),
                    _ => None,
                };
                let class = class_of(pack, p.as_deref(), &rname);
                let counted = net * weight(pack, method, &class);
                let u = uses.entry(class.clone()).or_insert_with(|| UseArea {
                    class: class.clone(),
                    rooms: 0,
                    net: 0.0,
                    counted: 0.0,
                });
                u.rooms += 1;
                u.net += net;
                u.counted += counted;
                if r.external.contains(&class) {
                    s.external += net;
                } else {
                    s.net += net;
                    if weight(pack, AreaMethod::Usable, &class) > 0.0 {
                        s.usable += net;
                    }
                }
                s.rooms += 1;
                report.rooms.push(RoomArea { room: id, level, number, name: rname, class, net, counted });
            }
            s.uses = uses.into_values().collect();
            let counted: f64 = s.uses.iter().map(|u| u.counted).sum();
            s.total = match (method, s.gross, s.gross_internal) {
                (AreaMethod::Gross, Some(g), _) => g,
                (AreaMethod::GrossInternal, _, Some(g)) => g,
                _ => counted,
            };
            if method.is_geometry() && s.gross.is_none() {
                report.fallback = true;
            }
            if let (Some(g), Some(i)) = (s.gross, s.gross_internal) {
                gross += g;
                gia += i;
                any_geo = true;
            }
            report.net += s.net;
            report.usable += s.usable;
            report.total += s.total;
            report.storeys.push(s);
        }
        if any_geo {
            report.gross = Some(gross);
            report.gross_internal = Some(gia);
        }
        Ok(report)
    }

    /// Value of the room parameter `def` for room `id`, when the project defines it.
    fn param_value_of_def(
        &self,
        def: &Option<crate::ParamDef>,
        id: i64,
    ) -> Result<Option<PVal>, Error> {
        match def {
            Some(d) => self.param_value_of(d, "room", id),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
#[path = "area_std_tests.rs"]
mod tests;
