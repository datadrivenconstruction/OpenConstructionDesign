// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Room programme templates as data: the default rooms of each typology, what spare band
//! length becomes, and the names a typology gives its rooms, read with the typology's traits
//! from the `base` table of `data/typology_programs.json`. A norm pack builds its own
//! `Programme` and passes it to `parse_spec_with`.

use super::{RoomKind, RoomReq, Traits, Typology};
use serde_json::Value as J;
use std::sync::OnceLock;

/// One programme line: kind, net area m2, count, and an optional (en, ru) name.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub kind: RoomKind,
    pub area: f64,
    pub count: u32,
    pub name: Option<(String, String)>,
}

/// What a stretch of band no asked room fills becomes.
#[derive(Clone, Debug, PartialEq)]
pub struct Spare {
    /// The neighbouring room grows by up to this share of its own length.
    pub grow_share: f64,
    /// Otherwise a room of this kind and (en, ru) name; small leftovers stay storage.
    pub kind: RoomKind,
    pub name: (String, String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Programme {
    pub typology: Typology,
    /// Ground floor of a building with more storeys.
    pub ground: Vec<Line>,
    /// Typical upper floor.
    pub typical: Vec<Line>,
    /// The only floor of a single-storey building.
    pub single: Vec<Line>,
    pub spare: Spare,
    /// The core half opposite the stair when there is no lift room.
    pub core_rest: (RoomKind, String, String),
    /// Room names this typology uses instead of the generic ones: (kind, en, ru).
    pub names: Vec<(RoomKind, String, String)>,
}

/// The base entry of a typology in the data.
fn base(t: Typology) -> &'static J {
    &super::typo_program::data()["base"][t.name()]
}

fn kind_of(t: Typology, v: &J) -> RoomKind {
    let k = v.as_str().unwrap_or_default();
    RoomKind::parse(k).unwrap_or_else(|| panic!("typology_programs.json base.{}: unknown kind '{k}'", t.name()))
}

fn en_ru(v: &J) -> (String, String) {
    let en = v["en"].as_str().unwrap_or_default().to_string();
    let ru = v["ru"].as_str().map_or_else(|| en.clone(), str::to_string);
    (en, ru)
}

fn programme_of(t: Typology) -> Programme {
    let b = base(t);
    let lines = |f: &str| lines_from_json(&b[f]).unwrap_or_else(|e| panic!("typology_programs.json base.{}.{f}: {e}", t.name()));
    let named = |v: &J| {
        let (en, ru) = en_ru(&v["name"]);
        (kind_of(t, &v["kind"]), en, ru)
    };
    let sp = &b["spare"];
    Programme {
        typology: t,
        ground: lines("ground"),
        typical: lines("typical"),
        single: lines("single"),
        spare: Spare {
            grow_share: sp["grow_share"].as_f64().unwrap_or_else(|| panic!("typology_programs.json base.{}.spare.grow_share", t.name())),
            kind: kind_of(t, &sp["kind"]),
            name: en_ru(&sp["name"]),
        },
        core_rest: named(&b["core_rest"]),
        names: b["names"].as_array().into_iter().flatten().map(named).collect(),
    }
}

/// The built-in programme of a typology.
pub fn default_programme(t: Typology) -> Programme {
    static ALL: OnceLock<Vec<Programme>> = OnceLock::new();
    let all = ALL.get_or_init(|| Typology::ALL.into_iter().map(programme_of).collect());
    all.iter().find(|p| p.typology == t).expect("every typology has a base programme").clone()
}

/// The traits a typology's base entry gives it.
pub fn traits(t: Typology) -> Traits {
    Traits::default()
        .patched(&base(t)["traits"])
        .unwrap_or_else(|e| panic!("typology_programs.json base.{}.traits: {e}", t.name()))
}

/// Lines from a norm-pack programme list `[{kind, min_area|area, count?, name?}]`; the
/// name may be a string or `{en, ru}`. Unknown kinds are reported, not dropped silently.
pub fn lines_from_json(v: &serde_json::Value) -> Result<Vec<Line>, String> {
    let list = v.as_array().ok_or("programme rooms must be a list")?;
    list.iter()
        .enumerate()
        .map(|(i, r)| {
            let k = r["kind"].as_str().unwrap_or_default();
            let kind = super::kinds::parse(k).map(|x| x.0).ok_or(format!("rooms[{i}].kind: unknown '{k}'"))?;
            let area = r["area"]
                .as_f64()
                .or_else(|| r["min_area"].as_f64())
                .ok_or(format!("rooms[{i}]: area or min_area required"))?;
            let count = r["count"].as_u64().map_or(1, |c| c as u32);
            let name = match &r["name"] {
                serde_json::Value::String(x) => Some((x.clone(), x.clone())),
                n if n.is_object() => {
                    let en = n["en"].as_str().unwrap_or(k).to_string();
                    Some((en.clone(), n["ru"].as_str().map_or(en, str::to_string)))
                }
                _ => None,
            };
            Ok(Line {
                kind,
                area,
                count,
                name,
            })
        })
        .collect()
}

impl Programme {
    /// The name this typology gives a kind.
    pub fn room_name(&self, k: RoomKind, ru: bool) -> String {
        match self.names.iter().find(|x| x.0 == k) {
            Some((_, en, r)) => if ru { r } else { en }.clone(),
            None => super::names::kind_name(k, ru).to_string(),
        }
    }

    pub fn reqs(&self, lines: &[Line], ru: bool) -> Vec<RoomReq> {
        lines
            .iter()
            .filter(|x| x.count > 0)
            .map(|x| RoomReq {
                name: match &x.name {
                    Some((en, r)) => if ru { r } else { en }.clone(),
                    None => self.room_name(x.kind, ru),
                },
                kind: x.kind,
                area: x.area,
                count: x.count,
                tags: Default::default(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn office_names_carry_no_school_rooms() {
        let p = default_programme(Typology::Office);
        let school = default_programme(Typology::School).room_name(RoomKind::Staff, true);
        for lines in [&p.ground, &p.typical, &p.single] {
            for r in p.reqs(lines, true) {
                assert_ne!(r.name, school);
                assert_ne!(r.kind, RoomKind::Classroom);
            }
        }
    }

    /// The programmes that moved from code into the data, line for line.
    #[test]
    fn the_base_programmes_read_from_the_data() {
        let h = default_programme(Typology::House);
        let areas = |ls: &[Line]| ls.iter().map(|l| (l.kind, l.area, l.count)).collect::<Vec<_>>();
        use RoomKind::*;
        assert_eq!(areas(&h.ground), [(Hall, 6.0, 1), (Living, 26.0, 1), (Kitchen, 12.0, 1), (Wc, 3.0, 1)]);
        assert_eq!(h.spare.grow_share, 0.6);
        assert_eq!(h.spare.kind, Office);
        assert_eq!(h.core_rest.0, Hall);
        assert_eq!(h.room_name(Parking, false), "Garage");
        let o = default_programme(Typology::Office);
        assert_eq!(o.ground[1].name.as_ref().map(|n| n.0.as_str()), Some("Open space"));
        assert_eq!(areas(&o.typical).len(), 7);
        assert_eq!(default_programme(Typology::School).spare.name.0, "Recreation");
        let a = default_programme(Typology::ApartmentSection);
        assert!(a.ground.is_empty() && a.typical.is_empty() && a.single.is_empty());
        assert_eq!(a.core_rest.0, Technical);
        let t = traits;
        assert!(t(Typology::House).residential && t(Typology::House).one_household && !t(Typology::House).flats);
        assert!(t(Typology::ApartmentSection).residential && t(Typology::ApartmentSection).flats);
        assert_eq!(t(Typology::Office), Traits::default());
        assert_eq!(t(Typology::School), Traits::default());
    }
}
