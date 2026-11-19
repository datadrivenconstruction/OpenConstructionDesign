//! Room programme templates as data: the default rooms of each typology, what spare band
//! length becomes, and the names a typology gives its rooms. A norm pack builds its own
//! `Programme` and passes it to `parse_spec_with`.

use super::{RoomKind, RoomReq, Typology};

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

fn l(kind: RoomKind, area: f64, count: u32) -> Line {
    Line {
        kind,
        area,
        count,
        name: None,
    }
}

fn named(kind: RoomKind, area: f64, count: u32, en: &str, ru: &str) -> Line {
    Line {
        name: Some((en.to_string(), ru.to_string())),
        ..l(kind, area, count)
    }
}

fn s(x: &str) -> String {
    x.to_string()
}

const RU_OPEN_SPACE: &str = "\u{41e}\u{442}\u{43a}\u{440}\u{44b}\u{442}\u{43e}\u{435} \
    \u{440}\u{430}\u{431}\u{43e}\u{447}\u{435}\u{435} \u{43f}\u{440}\u{43e}\u{441}\u{442}\
    \u{440}\u{430}\u{43d}\u{441}\u{442}\u{432}\u{43e}";
const RU_HALL: &str = "\u{425}\u{43e}\u{43b}\u{43b}";
const RU_STUDY: &str = "\u{41a}\u{430}\u{431}\u{438}\u{43d}\u{435}\u{442}";
const RU_GARAGE: &str = "\u{413}\u{430}\u{440}\u{430}\u{436}";
const RU_STAFF_OFFICE: &str = "\u{41a}\u{43e}\u{43c}\u{43d}\u{430}\u{442}\u{430} \
    \u{43f}\u{435}\u{440}\u{441}\u{43e}\u{43d}\u{430}\u{43b}\u{430}";
const RU_STAFF_SCHOOL: &str =
    "\u{423}\u{447}\u{438}\u{442}\u{435}\u{43b}\u{44c}\u{441}\u{43a}\u{430}\u{44f}";
const RU_RECREATION: &str = "\u{420}\u{435}\u{43a}\u{440}\u{435}\u{430}\u{446}\u{438}\u{44f}";
const RU_TECH: &str =
    "\u{422}\u{435}\u{445}\u{43d}\u{438}\u{447}\u{435}\u{441}\u{43a}\u{43e}\u{435}";

/// The built-in programme of a typology.
pub fn default_programme(t: Typology) -> Programme {
    use RoomKind::*;
    match t {
        Typology::House => Programme {
            typology: t,
            ground: vec![
                l(Hall, 6.0, 1),
                l(Living, 26.0, 1),
                l(Kitchen, 12.0, 1),
                l(Wc, 3.0, 1),
            ],
            typical: vec![
                l(Bedroom, 14.0, 1),
                l(Bedroom, 12.0, 1),
                l(Bedroom, 10.0, 1),
                l(Bath, 6.0, 1),
            ],
            single: vec![
                l(Hall, 6.0, 1),
                l(Living, 24.0, 1),
                l(Kitchen, 12.0, 1),
                l(Bedroom, 14.0, 1),
                l(Bedroom, 12.0, 1),
                l(Bath, 5.0, 1),
                l(Wc, 2.0, 1),
            ],
            // spare length of a house is a room people use: a study with a window
            spare: Spare {
                grow_share: 0.6,
                kind: Office,
                name: (s("Study"), s(RU_STUDY)),
            },
            core_rest: (Hall, s("Hall"), s(RU_HALL)),
            // a house parks one or two cars: a garage, not a parking deck
            names: vec![(Parking, s("Garage"), s(RU_GARAGE))],
        },
        Typology::Office => {
            let open = |n| named(Office, 72.0, n, "Open space", RU_OPEN_SPACE);
            Programme {
                typology: t,
                ground: vec![
                    l(Lobby, 48.0, 1),
                    open(1),
                    l(Office, 18.0, 2),
                    l(Meeting, 24.0, 1),
                    l(Wc, 6.0, 2),
                    l(Technical, 8.0, 1),
                ],
                typical: vec![
                    open(2),
                    l(Office, 18.0, 2),
                    l(Meeting, 24.0, 1),
                    l(Meeting, 12.0, 1),
                    l(Staff, 12.0, 1),
                    l(Wc, 6.0, 2),
                    l(Storage, 4.0, 1),
                ],
                single: vec![
                    l(Lobby, 24.0, 1),
                    open(1),
                    l(Office, 18.0, 2),
                    l(Meeting, 24.0, 1),
                    l(Staff, 12.0, 1),
                    l(Wc, 6.0, 2),
                ],
                spare: Spare {
                    grow_share: 0.5,
                    kind: Office,
                    name: (s("Open space"), s(RU_OPEN_SPACE)),
                },
                core_rest: (Technical, s("Technical"), s(RU_TECH)),
                names: vec![(Staff, s("Staff room"), s(RU_STAFF_OFFICE))],
            }
        }
        Typology::School => Programme {
            typology: t,
            ground: vec![
                l(Lobby, 72.0, 1),
                l(Classroom, 60.0, 4),
                l(Staff, 24.0, 1),
                l(Wc, 18.0, 2),
                l(Storage, 12.0, 1),
            ],
            typical: vec![
                l(Classroom, 60.0, 6),
                l(Staff, 24.0, 1),
                l(Wc, 18.0, 2),
                l(Storage, 12.0, 1),
            ],
            single: vec![
                l(Lobby, 48.0, 1),
                l(Classroom, 60.0, 4),
                l(Staff, 24.0, 1),
                l(Wc, 18.0, 2),
                l(Storage, 8.0, 1),
            ],
            spare: Spare {
                grow_share: 0.3,
                kind: Lobby,
                name: (s("Recreation"), s(RU_RECREATION)),
            },
            core_rest: (Technical, s("Technical"), s(RU_TECH)),
            names: vec![(Staff, s("Staff room"), s(RU_STAFF_SCHOOL))],
        },
        Typology::ApartmentSection => Programme {
            typology: t,
            ground: vec![],
            typical: vec![],
            single: vec![],
            spare: Spare {
                grow_share: 0.0,
                kind: Technical,
                name: (s("Technical"), s(RU_TECH)),
            },
            core_rest: (Technical, s("Technical"), s(RU_TECH)),
            names: vec![],
        },
    }
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
}
