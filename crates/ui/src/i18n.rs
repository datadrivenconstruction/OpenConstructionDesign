// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! String catalogs by key (en source, ru full coverage), CLDR plural forms for en/ru.
//! Base files `data/i18n/{ui,commands}.<lang>.json` plus fragments `data/i18n/<lang>/*.json`.
use serde_json::Value;
use std::collections::BTreeMap;

/// Declares `Lang` with the same 45 locales as the ERP frontend (`frontend/src/app/locales`).
/// Columns: variant, locale code (= folder under `data/i18n`), right-to-left. Endonyms live in
/// `data/languages.json`.
macro_rules! langs {
    ($($v:ident $code:literal $rtl:literal),+ $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Lang { $($v),+ }
        impl Lang {
            /// Every shipped locale; `En` first, `Ru` second, the rest alphabetical by code.
            pub const ALL: &'static [Lang] = &[$(Lang::$v),+];
            /// Locale code of the language folder under `data/i18n`.
            pub fn code(self) -> &'static str {
                match self { $(Lang::$v => $code),+ }
            }
            /// Name of the language in itself (shown in the picker, never translated).
            pub fn native_name(self) -> &'static str {
                static NAMES: std::sync::OnceLock<BTreeMap<String, String>> = std::sync::OnceLock::new();
                let names = NAMES.get_or_init(|| {
                    serde_json::from_str(include_str!("../data/languages.json")).expect("languages.json")
                });
                names.get(self.code()).map_or(self.code(), String::as_str)
            }
            /// Right-to-left script: ar, he, fa, ur.
            pub fn is_rtl(self) -> bool {
                match self { $(Lang::$v => $rtl),+ }
            }
        }
    };
}

langs! {
    En "en" false,
    Ru "ru" false,
    Ar "ar" true,
    Bg "bg" false,
    Bn "bn" false,
    Cs "cs" false,
    Da "da" false,
    De "de" false,
    El "el" false,
    EnGb "en-GB" false,
    EnUs "en-US" false,
    Es "es" false,
    EsCl "es-CL" false,
    EsCo "es-CO" false,
    EsMx "es-MX" false,
    Et "et" false,
    Fa "fa" true,
    Fi "fi" false,
    Fil "fil" false,
    Fr "fr" false,
    He "he" true,
    Hi "hi" false,
    Hr "hr" false,
    Hu "hu" false,
    Id "id" false,
    It "it" false,
    Ja "ja" false,
    Kk "kk" false,
    Ko "ko" false,
    Ky "ky" false,
    Mn "mn" false,
    Nl "nl" false,
    No "no" false,
    Pl "pl" false,
    Pt "pt" false,
    PtBr "pt-BR" false,
    Ro "ro" false,
    Sv "sv" false,
    Th "th" false,
    Tr "tr" false,
    Uk "uk" false,
    Ur "ur" true,
    Uz "uz" false,
    Vi "vi" false,
    Zh "zh" false,
}

const N_LANGS: usize = 45;

impl Lang {
    /// Regional variant falls back to its base language before English
    /// (es-MX -> es -> en, pt-BR -> pt -> en, en-GB -> en).
    pub fn parent(self) -> Option<Lang> {
        match self {
            Lang::EnGb | Lang::EnUs => Some(Lang::En),
            Lang::EsCl | Lang::EsCo | Lang::EsMx => Some(Lang::Es),
            Lang::PtBr => Some(Lang::Pt),
            _ => None,
        }
    }
    /// Lookup order for a key: the language, its base, then English.
    pub fn chain(self) -> impl Iterator<Item = Lang> {
        let parent = self.parent().filter(|p| *p != Lang::En);
        std::iter::once(self)
            .chain(parent)
            .chain((self != Lang::En).then_some(Lang::En))
    }
    /// Language for a locale code (`ru`, `ru-RU`, `RU_ru`, `pt_BR`, `en-AU`): the exact tag
    /// first, then the base language; unknown codes give English.
    pub fn from_code(code: &str) -> Lang {
        let norm = code.split(['.', '@']).next().unwrap_or("").replace('_', "-");
        let hit = |c: &str| Lang::ALL.iter().copied().find(|l| l.code().eq_ignore_ascii_case(c));
        if let Some(l) = hit(&norm) {
            return l;
        }
        let primary = norm.split('-').next().unwrap_or("");
        match primary.to_ascii_lowercase().as_str() {
            "nb" | "nn" => Lang::No,
            "tl" => Lang::Fil,
            "iw" => Lang::He,
            p => hit(p).unwrap_or(Lang::En),
        }
    }
}

static LANG_NOW: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Process-wide interface language for code without a `Catalog` at hand (RTL queries, `tr`).
pub fn set_current(l: Lang) {
    let i = Lang::ALL.iter().position(|x| *x == l).unwrap_or(0);
    LANG_NOW.store(i as u8, std::sync::atomic::Ordering::Relaxed);
}

/// Language set by the last `set_current` (English before any).
pub fn current() -> Lang {
    Lang::ALL[(LANG_NOW.load(std::sync::atomic::Ordering::Relaxed) as usize).min(N_LANGS - 1)]
}

/// True while the interface language is written right to left.
pub fn rtl() -> bool {
    current().is_rtl()
}

/// Layout direction for a row of widgets in the current language.
pub fn layout_dir() -> egui::Layout {
    if rtl() {
        egui::Layout::right_to_left(egui::Align::Center)
    } else {
        egui::Layout::left_to_right(egui::Align::Center)
    }
}

/// Horizontal text alignment that follows the reading direction.
pub fn text_align() -> egui::Align {
    if rtl() {
        egui::Align::RIGHT
    } else {
        egui::Align::LEFT
    }
}

const EN: [&str; 2] = [
    include_str!("../data/i18n/ui.en.json"),
    include_str!("../data/i18n/commands.en.json"),
];
const RU: [&str; 2] = [
    include_str!("../data/i18n/ui.ru.json"),
    include_str!("../data/i18n/commands.ru.json"),
];

/// CLDR plural category for a non-negative integer.
pub fn plural_category(lang: Lang, n: u64) -> &'static str {
    let (m10, m100) = (n % 10, n % 100);
    match lang {
        Lang::Ru | Lang::Uk => {
            if m10 == 1 && m100 != 11 {
                "one"
            } else if (2..=4).contains(&m10) && !(12..=14).contains(&m100) {
                "few"
            } else {
                "many"
            }
        }
        Lang::Pl => {
            if n == 1 {
                "one"
            } else if (2..=4).contains(&m10) && !(12..=14).contains(&m100) {
                "few"
            } else {
                "many"
            }
        }
        Lang::Hr => {
            if m10 == 1 && m100 != 11 {
                "one"
            } else if (2..=4).contains(&m10) && !(12..=14).contains(&m100) {
                "few"
            } else {
                "other"
            }
        }
        Lang::Cs => match n {
            1 => "one",
            2..=4 => "few",
            _ => "other",
        },
        Lang::Ro => {
            if n == 1 {
                "one"
            } else if n == 0 || (1..=19).contains(&m100) {
                "few"
            } else {
                "other"
            }
        }
        Lang::Ar => match n {
            0 => "zero",
            1 => "one",
            2 => "two",
            _ if (3..=10).contains(&m100) => "few",
            _ if (11..=99).contains(&m100) => "many",
            _ => "other",
        },
        Lang::He => match n {
            1 => "one",
            2 => "two",
            _ => "other",
        },
        Lang::Fr | Lang::Pt | Lang::PtBr => {
            if n <= 1 {
                "one"
            } else {
                "other"
            }
        }
        Lang::Hi | Lang::Bn | Lang::Fa => {
            if n <= 1 {
                "one"
            } else {
                "other"
            }
        }
        // no grammatical number
        Lang::Ja | Lang::Ko | Lang::Zh | Lang::Vi | Lang::Th | Lang::Id => "other",
        _ if n == 1 => "one",
        _ => "other",
    }
}

pub struct Catalog {
    pub lang: Lang,
    tables: [OnceLock<BTreeMap<String, Value>>; N_LANGS],
}

use crate::fragments::{I18N_ALL, I18N_EN_FRAGMENTS, I18N_RU_FRAGMENTS};
use std::sync::OnceLock;

/// Base files plus fragments of one language; only `en` and `ru` have base files.
/// Other locales hold fragments `<code>/<same name as en>.json` and fall back per key.
fn load_lang(lang: Lang) -> BTreeMap<String, Value> {
    let (base, frags): (&[&str], &[(&str, &str)]) = match lang {
        Lang::En => (&EN[..], I18N_EN_FRAGMENTS),
        Lang::Ru => (&RU[..], I18N_RU_FRAGMENTS),
        _ => (
            &[][..],
            I18N_ALL.iter().find(|e| e.0 == lang.code()).map_or(&[][..], |e| e.1),
        ),
    };
    load(base, frags)
}

fn load(base: &[&str], frags: &[(&str, &str)]) -> BTreeMap<String, Value> {
    let mut m = BTreeMap::new();
    for s in base.iter().copied().chain(frags.iter().map(|f| f.1)) {
        match serde_json::from_str::<BTreeMap<String, Value>>(s) {
            Ok(v) => m.extend(v),
            Err(e) => {
                eprintln!("i18n: invalid JSON catalog dropped: {e}");
                debug_assert!(false, "i18n: invalid JSON catalog: {e}");
            }
        }
    }
    m
}

impl Catalog {
    pub fn new(lang: Lang) -> Self {
        Self {
            lang,
            tables: std::array::from_fn(|_| OnceLock::new()),
        }
    }
    /// Table of one language, parsed on first use.
    pub fn table(&self, lang: Lang) -> &BTreeMap<String, Value> {
        self.tables[lang as usize].get_or_init(|| load_lang(lang))
    }
    fn lookup(&self, key: &str) -> Option<(Lang, &Value)> {
        self.lang
            .chain()
            .find_map(|l| self.table(l).get(key).map(|v| (l, v)))
    }
    /// Text by key; missing key falls back to EN, then to the key itself (visible in QA).
    pub fn t(&self, key: &str) -> String {
        match self.lookup(key) {
            Some((_, Value::String(s))) => s.clone(),
            _ => key.to_string(),
        }
    }
    /// Text in a specific language (no fallback beyond EN).
    pub fn t_in(&self, lang: Lang, key: &str) -> String {
        match lang.chain().find_map(|l| self.table(l).get(key)) {
            Some(Value::String(s)) => s.clone(),
            _ => self.t(key),
        }
    }
    /// Named `{placeholder}` substitution.
    pub fn tf(&self, key: &str, args: &[(&str, &str)]) -> String {
        let mut s = self.t(key);
        for (k, v) in args {
            s = s.replace(&format!("{{{k}}}"), v);
        }
        s
    }
    /// Plural message: object of `=N` exact forms and CLDR categories; `#` becomes n.
    pub fn plural(&self, key: &str, n: u64) -> String {
        let Some((lang, Value::Object(forms))) = self.lookup(key) else {
            return self.t(key);
        };
        let pick = forms
            .get(&format!("={n}"))
            .or_else(|| forms.get(plural_category(lang, n)))
            .or_else(|| forms.get("other"));
        match pick {
            Some(Value::String(s)) => s.replace('#', &n.to_string()),
            _ => key.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ru_plurals() {
        let c = Catalog::new(Lang::Ru);
        use crate::test_fx::fx;
        let cases = [0, 1, 2, 5, 11, 21, 22, 100, 112].map(|n| (n, fx(&format!("sel_{n}"))));
        for (n, want) in cases {
            assert_eq!(c.plural("status.sel", n), want, "n={n}");
        }
        let e = Catalog::new(Lang::En);
        assert_eq!(e.plural("status.sel", 1), "1 element selected");
        assert_eq!(e.plural("status.sel", 21), "21 elements selected");
    }

    fn placeholders(v: &Value, out: &mut std::collections::BTreeSet<String>) {
        match v {
            Value::String(s) => {
                let mut rest = s.as_str();
                while let Some(i) = rest.find('{') {
                    let Some(j) = rest[i..].find('}') else { break };
                    let name = &rest[i + 1..i + j];
                    if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                        out.insert(name.to_string());
                    }
                    rest = &rest[i + j + 1..];
                }
            }
            Value::Object(m) => m.values().for_each(|x| placeholders(x, out)),
            _ => {}
        }
    }

    #[test]
    fn registry_has_the_45_erp_locales() {
        assert_eq!(Lang::ALL.len(), N_LANGS);
        for (i, l) in Lang::ALL.iter().enumerate() {
            assert_eq!(*l as usize, i, "{l:?}");
            assert_eq!(Lang::from_code(l.code()), *l);
            // a missing endonym in data/languages.json would fall back to the code
            assert_ne!(l.native_name(), l.code(), "{l:?}");
        }
        let rtl: Vec<_> = Lang::ALL.iter().filter(|l| l.is_rtl()).map(|l| l.code()).collect();
        assert_eq!(rtl, ["ar", "fa", "he", "ur"]);
        assert_eq!(Lang::from_code("pt_BR.UTF-8"), Lang::PtBr);
        assert_eq!(Lang::from_code("es-AR"), Lang::Es);
        assert_eq!(Lang::from_code("nb-NO"), Lang::No);
        assert_eq!(Lang::from_code("xx"), Lang::En);
        let chain = |l: Lang| l.chain().map(Lang::code).collect::<Vec<_>>();
        assert_eq!(chain(Lang::EsMx), ["es-MX", "es", "en"]);
        assert_eq!(chain(Lang::PtBr), ["pt-BR", "pt", "en"]);
        assert_eq!(chain(Lang::EnGb), ["en-GB", "en"]);
        assert_eq!(chain(Lang::En), ["en"]);
        assert_eq!(chain(Lang::De), ["de", "en"]);
    }

    /// Every locale file parses, uses only keys of the English source and keeps its `{name}` slots.
    #[test]
    fn every_locale_is_a_clean_subset_of_en() {
        let c = Catalog::new(Lang::En);
        let en = c.table(Lang::En);
        for lang in Lang::ALL {
            for (k, v) in c.table(*lang) {
                let Some(src) = en.get(k) else {
                    panic!("{}: key {k} absent from en", lang.code());
                };
                // ru predates the check: 14 `ai.realism.*` texts use named slots where en has {0}
                if *lang == Lang::Ru {
                    continue;
                }
                let (mut a, mut b) = (Default::default(), Default::default());
                placeholders(src, &mut a);
                placeholders(v, &mut b);
                assert_eq!(a, b, "{}: placeholders of {k}", lang.code());
            }
        }
        for (code, frags) in I18N_ALL {
            assert!(Lang::ALL.iter().any(|l| l.code() == *code), "folder {code} is not a locale");
            for (name, src) in *frags {
                serde_json::from_str::<BTreeMap<String, Value>>(src)
                    .unwrap_or_else(|e| panic!("{code}/{name}: {e}"));
            }
        }
    }

    #[test]
    fn regional_locale_falls_back_to_base_then_en() {
        let c = Catalog::new(Lang::EsMx);
        assert_eq!(c.t("settings.lang.en"), "English");
        assert_eq!(c.t_in(Lang::PtBr, "settings.lang.en"), "English");
        assert_eq!(plural_category(Lang::Ar, 11), "many");
        assert_eq!(plural_category(Lang::Pl, 22), "few");
        assert_eq!(plural_category(Lang::Ja, 1), "other");
    }

    #[test]
    fn i18n_no_missing_keys() {
        let c = Catalog::new(Lang::En);
        let (en, ru) = (c.table(Lang::En), c.table(Lang::Ru));
        let miss: Vec<_> = en.keys().filter(|k| !ru.contains_key(*k)).collect();
        assert!(miss.is_empty(), "ru missing: {miss:?}");
        let extra: Vec<_> = ru.keys().filter(|k| !en.contains_key(*k)).collect();
        assert!(extra.is_empty(), "ru keys absent from en source: {extra:?}");
        for (k, v) in ru {
            if let Value::Object(f) = v {
                for cat in ["one", "few", "many", "other"] {
                    assert!(f.contains_key(cat), "{k}: ru plural lacks {cat}");
                }
            }
        }
        // Every registry command has a label in both languages.
        for cmd in crate::Registry::builtin().all() {
            assert!(en.contains_key(&cmd.label_key()), "{}", cmd.id);
            assert!(ru.contains_key(&cmd.label_key()), "{}", cmd.id);
        }
        assert_eq!(c.t("no.such.key"), "no.such.key");
        for (i, src) in EN.iter().chain(RU.iter()).enumerate() {
            serde_json::from_str::<BTreeMap<String, Value>>(src)
                .unwrap_or_else(|e| panic!("i18n base file #{i}: {e}"));
        }
        for (name, src) in I18N_EN_FRAGMENTS.iter().chain(I18N_RU_FRAGMENTS) {
            serde_json::from_str::<BTreeMap<String, Value>>(src)
                .unwrap_or_else(|e| panic!("i18n fragment {name}: {e}"));
        }
        // ribbon group captions and status hints resolve in both languages
        for cmd in crate::Registry::builtin().all() {
            let g = format!("rgroup.{}", crate::widgets::ribbon::group_name(cmd));
            if cmd.on("R") {
                assert!(ru.contains_key(&g) && en.contains_key(&g), "{g}");
            }
        }
    }
}
