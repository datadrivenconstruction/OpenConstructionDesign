//! Air of hospital rooms by care class: operating theatres at positive pressure, airborne
//! isolation rooms at negative pressure, and clinical wards with a floor of air changes. The
//! class is a room tag, never a word of the room's name; every number is a rule.

use super::rules::rule;
use super::{RoomKind, RoomTags};

/// What a room's tags make of its air.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Care {
    Operating,
    Isolation,
    Ward,
}

impl Care {
    pub fn name(self) -> &'static str {
        match self {
            Care::Operating => "operating",
            Care::Isolation => "isolation",
            Care::Ward => "ward",
        }
    }
}

/// The care class of a room: operating before isolation before a clinical ward.
pub fn class_of(kind: RoomKind, tags: RoomTags) -> Option<Care> {
    let has = |id: &str| RoomTags::parse(id).is_some_and(|t| tags.meets(t));
    if has("operating") {
        Some(Care::Operating)
    } else if has("isolation") {
        Some(Care::Isolation)
    } else if kind == RoomKind::Ward && has("clinical") {
        Some(Care::Ward)
    } else {
        None
    }
}

/// Supply and extract in m3/h of a room of a care class, `None` for any other room. `clear` is
/// the clear height under the ceiling void.
pub fn air(kind: RoomKind, tags: RoomTags, area: f64, clear: f64) -> Option<(f64, f64)> {
    let vol = area * clear;
    Some(match class_of(kind, tags)? {
        Care::Operating => {
            let s = rule("services.hvac.ach_operating") * vol;
            (s, s * rule("services.hvac.operating_extract_ratio"))
        }
        Care::Isolation => {
            let s = rule("services.hvac.ach_isolation") * vol;
            (s, s * rule("services.hvac.isolation_extract_ratio"))
        }
        Care::Ward => {
            let q = (rule("services.hvac.rate_ward") * area).max(rule("services.hvac.ach_ward") * vol);
            (q, q)
        }
    })
}
