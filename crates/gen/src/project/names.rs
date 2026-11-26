// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Default names of rooms and levels in the project language.

use super::RoomKind;

/// The English name of a room kind, the source the bundles translate.
pub(crate) fn kind_name_en(k: RoomKind) -> &'static str {
    use RoomKind::*;
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

/// The default name of a room kind in `lang`.
pub(crate) fn kind_name(k: RoomKind, lang: &str) -> &'static str {
    look(lang, &format!("gen.name.room.{}", k.key())).unwrap_or(kind_name_en(k))
}

pub(crate) fn level_name(storey: usize, lang: &str) -> String {
    look(lang, "gen.name.level").unwrap_or("Level {n}").replace("{n}", &(storey + 1).to_string())
}

/// Basement `b`, 0 right below the ground floor.
pub(crate) fn basement_name(b: usize, lang: &str) -> String {
    look(lang, "gen.name.basement").unwrap_or("Basement {n}").replace("{n}", &(b + 1).to_string())
}

pub(crate) fn roof_name(lang: &str) -> &'static str {
    look(lang, "gen.name.roof").unwrap_or("Roof")
}

/// Rooms still named by their kind's English default take the locale's default.
pub(crate) fn default_rooms(p: &mut super::GProject, lang: &str) {
    if lang == "en" {
        return;
    }
    for st in p.storeys.iter_mut().chain(p.basements.iter_mut()) {
        for (g, r) in st.layout.rooms.iter_mut().zip(&st.rooms) {
            if g.name == kind_name_en(r.kind) {
                g.name = kind_name(r.kind, lang).to_string();
            }
        }
    }
}

/// Grid axes take Cyrillic letters where the locale's `gen.grid.axes` says `cyrillic`.
pub(crate) fn cyrillic_axes(lang: &str) -> bool {
    look(lang, "gen.grid.axes") == Some("cyrillic")
}

// LOCALES: every UI locale (a folder of `crates/ui/data/i18n`); NAME_FILES: the locales whose
// folder holds `gen_names.json`, with its text
include!(concat!(env!("OUT_DIR"), "/gen_names.rs"));

/// The word lists of `data/lang`: (locale, list, text).
pub(crate) fn word_files() -> &'static [(&'static str, &'static str, &'static str)] {
    WORD_FILES
}

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

/// The translation of `key` in `lang` or its base language; `None` in English or when no
/// bundle has it, so the caller keeps its English source.
pub(crate) fn look(lang: &str, key: &str) -> Option<&'static str> {
    if lang == "en" {
        return None;
    }
    let base = lang.split('-').next().unwrap_or(lang);
    [lang, base].iter().find_map(|l| tables().get(*l)?.get(key)?.as_str())
}

/// Lowercase ASCII words of an English text joined by `_`: the key part of a name.
pub(crate) fn slug(en: &str) -> String {
    let mut out = String::new();
    for w in en.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()) {
        if !out.is_empty() {
            out.push('_');
        }
        out.push_str(&w.to_ascii_lowercase());
    }
    out
}

/// An English name of generator data in `lang`: `gen.n.<scope>.<slug>`, then `gen.n.<slug>`.
pub fn tr(lang: &str, scope: &str, en: &str) -> String {
    if lang == "en" || en.is_empty() {
        return en.to_string();
    }
    let s = slug(en);
    let own = (!scope.is_empty()).then(|| look(lang, &format!("gen.n.{scope}.{s}"))).flatten();
    own.or_else(|| look(lang, &format!("gen.n.{s}"))).unwrap_or(en).to_string()
}

/// A display name: English source the bundles translate, or the user's own words. A plain
/// string is the same in every language; `{en, <locale>: ..}` keeps the locales it gives.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Text {
    pub en: String,
    scope: String,
    own: Vec<(String, String)>,
    fixed: bool,
}

impl Text {
    /// A name of the generator's own data, translated under `scope`.
    pub fn src(en: &str, scope: &str) -> Text {
        Text { en: en.to_string(), scope: scope.to_string(), own: vec![], fixed: false }
    }

    /// Words that stay as written in every language.
    pub fn fixed(s: &str) -> Text {
        Text { en: s.to_string(), scope: String::new(), own: vec![], fixed: true }
    }

    /// A string or `{en, <locale>..}`; `None` without English words.
    pub fn from_json(v: &serde_json::Value, scope: &str) -> Option<Text> {
        match v {
            serde_json::Value::String(s) if !s.trim().is_empty() => Some(Text::fixed(s)),
            serde_json::Value::Object(o) => {
                let en = o.get("en")?.as_str().filter(|s| !s.trim().is_empty())?;
                let own = o.iter().filter(|(k, _)| *k != "en").filter_map(|(k, x)| Some((k.clone(), x.as_str()?.to_string()))).collect();
                Some(Text { own, ..Text::src(en, scope) })
            }
            _ => None,
        }
    }

    /// The scope the bundles translate it under, for data read before its scope is known.
    pub fn in_scope(mut self, scope: &str) -> Text {
        if self.scope.is_empty() {
            self.scope = scope.to_string();
        }
        self
    }

    pub fn get(&self, lang: &str) -> String {
        let base = lang.split('-').next().unwrap_or(lang);
        let own = [lang, base].iter().find_map(|l| self.own.iter().find(|(k, _)| k == l));
        match own {
            Some((_, v)) => v.clone(),
            None if self.fixed => self.en.clone(),
            None => tr(lang, &self.scope, &self.en),
        }
    }

    /// True when `name` is this name in some language.
    pub fn names(&self, name: &str) -> bool {
        if name == self.en || self.own.iter().any(|(_, v)| v == name) {
            return true;
        }
        if self.fixed {
            return false;
        }
        let s = slug(&self.en);
        let keys = [format!("gen.n.{}.{s}", self.scope), format!("gen.n.{s}")];
        tables().values().any(|t| keys.iter().any(|k| t.get(k).and_then(|v| v.as_str()) == Some(name)))
    }

    pub fn to_json(&self) -> serde_json::Value {
        if self.fixed {
            return serde_json::Value::String(self.en.clone());
        }
        let mut o = serde_json::Map::new();
        o.insert("en".into(), self.en.clone().into());
        for (k, v) in &self.own {
            o.insert(k.clone(), v.clone().into());
        }
        serde_json::Value::Object(o)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generators_and_projects_name_rooms_from_one_table() {
        for lang in ["en", "ru", "de"] {
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
                assert_eq!(name, kind_name(k, lang), "{lang} {k:?}");
            }
        }
    }
}
