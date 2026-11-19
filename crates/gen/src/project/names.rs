// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Default names of rooms and levels in the project language.

use super::RoomKind;

pub(crate) fn kind_name(k: RoomKind, ru: bool) -> &'static str {
    use RoomKind::*;
    if ru {
        return match k {
            Living => "\u{413}\u{43e}\u{441}\u{442}\u{438}\u{43d}\u{430}\u{44f}",
            Bedroom => "\u{421}\u{43f}\u{430}\u{43b}\u{44c}\u{43d}\u{44f}",
            Kitchen => "\u{41a}\u{443}\u{445}\u{43d}\u{44f}",
            Bath => "\u{412}\u{430}\u{43d}\u{43d}\u{430}\u{44f}",
            Wc => "\u{422}\u{443}\u{430}\u{43b}\u{435}\u{442}",
            Hall => "\u{41f}\u{440}\u{438}\u{445}\u{43e}\u{436}\u{430}\u{44f}",
            Storage => "\u{41a}\u{43b}\u{430}\u{434}\u{43e}\u{432}\u{430}\u{44f}",
            Office => "\u{41a}\u{430}\u{431}\u{438}\u{43d}\u{435}\u{442}",
            Meeting => concat!(
                "\u{41f}\u{435}\u{440}\u{435}\u{433}\u{43e}",
                "\u{432}\u{43e}\u{440}\u{43d}\u{430}\u{44f}"
            ),
            Classroom => "\u{41a}\u{43b}\u{430}\u{441}\u{441}",
            Lab => "\u{41b}\u{430}\u{431}\u{43e}\u{440}\u{430}\u{442}\u{43e}\u{440}\u{438}\u{44f}",
            Staff => concat!(
                "\u{423}\u{447}\u{438}\u{442}\u{435}\u{43b}",
                "\u{44c}\u{441}\u{43a}\u{430}\u{44f}"
            ),
            Lobby => "\u{412}\u{435}\u{441}\u{442}\u{438}\u{431}\u{44e}\u{43b}\u{44c}",
            Technical => {
                "\u{422}\u{435}\u{445}\u{43d}\u{438}\u{447}\u{435}\u{441}\u{43a}\u{43e}\u{435}"
            }
            Corridor => "\u{41a}\u{43e}\u{440}\u{438}\u{434}\u{43e}\u{440}",
            Stair => "\u{41b}\u{435}\u{441}\u{442}\u{43d}\u{438}\u{446}\u{430}",
            Lift => "\u{41b}\u{438}\u{444}\u{442}",
            Parking => "\u{410}\u{432}\u{442}\u{43e}\u{441}\u{442}\u{43e}\u{44f}\u{43d}\u{43a}\u{430}",
            Retail => "\u{422}\u{43e}\u{440}\u{433}\u{43e}\u{432}\u{44b}\u{439} \u{437}\u{430}\u{43b}",
            Dining => concat!(
                "\u{41e}\u{431}\u{435}\u{434}\u{435}\u{43d}\u{43d}\u{44b}\u{439} ",
                "\u{437}\u{430}\u{43b}"
            ),
            Workshop => "\u{426}\u{435}\u{445}",
            Ward => "\u{41f}\u{430}\u{43b}\u{430}\u{442}\u{430}",
            Gym => "\u{421}\u{43f}\u{43e}\u{440}\u{442}\u{437}\u{430}\u{43b}",
        };
    }
    match k {
        Living => "Living",
        Bedroom => "Bedroom",
        Kitchen => "Kitchen",
        Bath => "Bath",
        Wc => "WC",
        Hall => "Hall",
        Storage => "Storage",
        Office => "Office",
        Meeting => "Meeting",
        Classroom => "Classroom",
        Lab => "Lab",
        Staff => "Staff",
        Lobby => "Lobby",
        Technical => "Technical",
        Corridor => "Corridor",
        Stair => "Stair",
        Lift => "Lift",
        Parking => "Parking",
        Retail => "Sales floor",
        Dining => "Dining hall",
        Workshop => "Workshop",
        Ward => "Ward",
        Gym => "Sports hall",
    }
}

pub(crate) fn level_name(storey: usize, ru: bool) -> String {
    if ru {
        format!(
            "\u{423}\u{440}\u{43e}\u{432}\u{435}\u{43d}\u{44c} {}",
            storey + 1
        )
    } else {
        format!("Level {}", storey + 1)
    }
}

/// Basement `b`, 0 right below the ground floor.
pub(crate) fn basement_name(b: usize, ru: bool) -> String {
    if ru {
        format!("\u{41f}\u{43e}\u{434}\u{432}\u{430}\u{43b} {}", b + 1)
    } else {
        format!("Basement {}", b + 1)
    }
}

// LOCALES: every UI locale (a folder of `crates/ui/data/i18n`); NAME_FILES: the locales whose
// folder holds `gen_names.json`, with its text
include!(concat!(env!("OUT_DIR"), "/gen_names.rs"));

/// Every UI locale code; a spec's `lang` is one of them.
pub fn locales() -> &'static [&'static str] {
    LOCALES
}

/// The UI locale of a BCP-47 tag (`pt_BR`, `de-AT`, `iw`): the exact tag, then its language;
/// anything else is `en`.
pub fn locale(tag: &str) -> &'static str {
    let norm = tag.trim().split(['.', '@']).next().unwrap_or("").replace('_', "-");
    let hit = |c: &str| LOCALES.iter().copied().find(|l| l.eq_ignore_ascii_case(c));
    if let Some(l) = hit(&norm) {
        return l;
    }
    match norm.split('-').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "nb" | "nn" => "no",
        "tl" => "fil",
        "iw" => "he",
        p => hit(p).unwrap_or("en"),
    }
}

fn tables() -> &'static std::collections::BTreeMap<&'static str, serde_json::Map<String, serde_json::Value>> {
    static T: std::sync::OnceLock<std::collections::BTreeMap<&'static str, serde_json::Map<String, serde_json::Value>>> =
        std::sync::OnceLock::new();
    T.get_or_init(|| {
        NAME_FILES
            .iter()
            .filter_map(|(l, t)| Some((*l, serde_json::from_str::<serde_json::Map<_, _>>(t).ok()?)))
            .collect()
    })
}

/// The i18n text of `key` in `lang`, its base language, then English, `{n}` filled.
pub fn text(lang: &str, key: &str, n: Option<usize>) -> String {
    let base = lang.split('-').next().unwrap_or(lang);
    let s = [lang, base, "en"]
        .iter()
        .find_map(|l| tables().get(l)?.get(key)?.as_str())
        .unwrap_or(key);
    match n {
        Some(n) => s.replace("{n}", &n.to_string()),
        None => s.to_string(),
    }
}

/// Default names in the project's locale: rooms, storeys, basements and the roof the builders
/// named by the English default take the locale's `gen.name.*`. en and ru are built so.
pub(crate) fn localize(p: &mut super::GProject, lang: &str) {
    if lang == "en" || lang == "ru" {
        return;
    }
    for st in p.storeys.iter_mut().chain(p.basements.iter_mut()) {
        for (g, r) in st.layout.rooms.iter_mut().zip(&st.rooms) {
            if g.name == kind_name(r.kind, false) {
                g.name = text(lang, &format!("gen.name.room.{}", r.kind.key()), None);
            }
        }
    }
    for (k, st) in p.storeys.iter_mut().enumerate() {
        if st.name == level_name(k, false) {
            st.name = text(lang, "gen.name.level", Some(k + 1));
        }
    }
    for (b, st) in p.basements.iter_mut().enumerate() {
        if st.name == basement_name(b, false) {
            st.name = text(lang, "gen.name.basement", Some(b + 1));
        }
    }
    if p.roof.name == roof_name(false) {
        p.roof.name = text(lang, "gen.name.roof", None);
    }
}

pub(crate) fn roof_name(ru: bool) -> &'static str {
    if ru {
        "\u{41a}\u{440}\u{43e}\u{432}\u{43b}\u{44f}"
    } else {
        "Roof"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generators_and_projects_name_rooms_from_one_table() {
        for (lang, ru) in [("en", false), ("ru", true)] {
            let n = crate::layout::names(lang);
            let pairs = [
                (n.living, RoomKind::Living),
                (n.bedroom, RoomKind::Bedroom),
                (n.bath, RoomKind::Bath),
                (n.hall, RoomKind::Hall),
                (n.corridor, RoomKind::Corridor),
                (n.stair, RoomKind::Stair),
                (n.service, RoomKind::Technical),
            ];
            for (name, k) in pairs {
                assert_eq!(name, kind_name(k, ru), "{lang} {k:?}");
            }
        }
    }
}
