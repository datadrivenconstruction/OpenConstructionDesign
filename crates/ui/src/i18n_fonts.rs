// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Fallback fonts for scripts DDC Sans does not cover (Noto, SIL OFL 1.1; assets/fonts/i18n/, built by
//! tools/build_i18n_fonts.py). They join egui's font lists only while the interface language needs
//! them, so a Latin or Cyrillic session carries just the symbol, math, block and emoji faces.
use crate::i18n::Lang;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Script {
    Arabic,
    Hebrew,
    Devanagari,
    Bengali,
    Thai,
    /// Vietnamese and Uzbek letters outside DDC Sans.
    LatinVi,
    ChineseSc,
    Japanese,
    Korean,
    /// Check marks, ballot boxes, geometric shapes, misc symbols, dingbats; always loaded.
    Symbols,
    /// Arrows, math operators and technical signs; always loaded.
    Math,
    /// Box drawing and block elements (the text caret); always loaded.
    Blocks,
    /// Monochrome emoji in user text; always loaded.
    Emoji,
}

impl Script {
    pub const ALL: [Script; 13] = [
        Script::Arabic,
        Script::Hebrew,
        Script::Devanagari,
        Script::Bengali,
        Script::Thai,
        Script::LatinVi,
        Script::ChineseSc,
        Script::Japanese,
        Script::Korean,
        Script::Symbols,
        Script::Math,
        Script::Blocks,
        Script::Emoji,
    ];

    /// egui font name.
    pub fn name(self) -> &'static str {
        match self {
            Script::Arabic => "ddc.i18n.arabic",
            Script::Hebrew => "ddc.i18n.hebrew",
            Script::Devanagari => "ddc.i18n.devanagari",
            Script::Bengali => "ddc.i18n.bengali",
            Script::Thai => "ddc.i18n.thai",
            Script::LatinVi => "ddc.i18n.latin-vi",
            Script::ChineseSc => "ddc.i18n.cjk-sc",
            Script::Japanese => "ddc.i18n.cjk-jp",
            Script::Korean => "ddc.i18n.cjk-kr",
            Script::Symbols => "ddc.i18n.symbols",
            Script::Math => "ddc.i18n.math",
            Script::Blocks => "ddc.i18n.blocks",
            Script::Emoji => "ddc.i18n.emoji",
        }
    }

    pub fn bytes(self) -> &'static [u8] {
        match self {
            Script::Arabic => include_bytes!("../assets/fonts/i18n/NotoSansArabic.ttf"),
            Script::Hebrew => include_bytes!("../assets/fonts/i18n/NotoSansHebrew.ttf"),
            Script::Devanagari => include_bytes!("../assets/fonts/i18n/NotoSansDevanagari.ttf"),
            Script::Bengali => include_bytes!("../assets/fonts/i18n/NotoSansBengali.ttf"),
            Script::Thai => include_bytes!("../assets/fonts/i18n/NotoSansThai.ttf"),
            Script::LatinVi => include_bytes!("../assets/fonts/i18n/NotoSansLatinVi.ttf"),
            Script::ChineseSc => include_bytes!("../assets/fonts/i18n/NotoSansSC.ttf"),
            Script::Japanese => include_bytes!("../assets/fonts/i18n/NotoSansJP.ttf"),
            Script::Korean => include_bytes!("../assets/fonts/i18n/NotoSansKR.ttf"),
            Script::Symbols => include_bytes!("../assets/fonts/i18n/NotoSansSymbols2.ttf"),
            Script::Math => include_bytes!("../assets/fonts/i18n/NotoSansMath.ttf"),
            Script::Blocks => include_bytes!("../assets/fonts/i18n/NotoSansMono.ttf"),
            Script::Emoji => include_bytes!("../assets/fonts/i18n/NotoEmoji.ttf"),
        }
    }
}

/// Scripts the interface in `lang` needs beyond DDC Sans.
pub fn scripts_for(lang: Lang) -> Vec<Script> {
    let mut v = match lang {
        Lang::Ar | Lang::Fa | Lang::Ur => vec![Script::Arabic],
        Lang::He => vec![Script::Hebrew],
        Lang::Hi => vec![Script::Devanagari],
        Lang::Bn => vec![Script::Bengali],
        Lang::Th => vec![Script::Thai],
        Lang::Vi | Lang::Uz => vec![Script::LatinVi],
        Lang::Zh => vec![Script::ChineseSc],
        Lang::Ja => vec![Script::Japanese],
        Lang::Ko => vec![Script::Korean],
        _ => vec![],
    };
    v.extend([Script::Symbols, Script::Math, Script::Blocks, Script::Emoji]);
    v
}

/// Identity of the font set for `lang`; equal values need no font reinstall.
pub fn set_key(lang: Lang) -> u16 {
    scripts_for(lang).iter().fold(0, |m, s| m | 1 << Script::ALL.iter().position(|x| x == s).unwrap_or(0))
}

/// Appends the fonts of `lang` to the end of every family, after the Latin faces. The toolkit's
/// default fonts are not built in (feature `default_fonts` off): these are the only fallbacks.
pub fn add_to(fd: &mut egui::FontDefinitions, lang: Lang) {
    for s in scripts_for(lang) {
        fd.font_data.insert(s.name().to_owned(), std::sync::Arc::new(egui::FontData::from_static(s.bytes())));
        for list in fd.families.values_mut() {
            list.push(s.name().to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{font_definitions_for_lang, UiFont, W_REGULAR};
    use egui::FontFamily;

    /// One typical string per locale: endonym, letters with diacritics, digits, UI punctuation, marks.
    fn sample(l: Lang) -> &'static str {
        match l.code() {
            "ar" => "العربية: مرحبا بالعالم ٠١٢٣ ✓ ✗ ﻻ",
            "fa" => "فارسی: سلام دنیا پچژگ ۱۲۳ ✓",
            "ur" => "اردو: ٹڈڑ ںے ھ ہ ی کھیل ✓",
            "he" => "עברית: שלום עולם ✓",
            "hi" => "हिन्दी: नमस्ते दुनिया ✓",
            "bn" => "বাংলা: হ্যালো বিশ্ব ✓",
            "th" => "ไทย: สวัสดีชาวโลก ✓",
            "zh" => "中文：你好，世界。文件 编辑 视图 ✓",
            "ja" => "日本語：こんにちは世界、ファイル 編集 表示 ✓",
            "ko" => "한국어: 안녕하세요 세계 파일 편집 ✓",
            "vi" => "Tiếng Việt: Xin chào thế giới ạảấầ ơư đ ✓",
            "uz" => "Oʻzbekcha: Gʻ gʻ oʻ ✓",
            "pl" => "Polski: Zażółć gęślą jaźń ✓",
            "cs" => "Čeština: Příliš žluťoučký kůň ✓",
            "tr" => "Türkçe: İıĞğŞş Çç Öö Üü ✓",
            "ro" => "Română: ăâîșț ĂÂÎȘȚ ţ ✓",
            "hu" => "Magyar: árvíztűrő tükörfúrógép ŐŰ ✓",
            "et" => "Eesti: õäöü šž ✓",
            "fi" | "sv" => "Suomi Svenska: åäö ÅÄÖ ✓",
            "da" | "no" => "Dansk Norsk: æøå ÆØÅ ✓",
            "hr" => "Hrvatski: čćžšđ ČĆŽŠĐ ✓",
            "de" => "Deutsch: äöüß ÄÖÜ ✓",
            "fr" => "Français: àâçéèêëîïôùûüÿœæ ✓",
            "es" | "es-CL" | "es-CO" | "es-MX" => "Español: ñ¿¡áéíóú ✓",
            "pt" | "pt-BR" => "Português: ãõçáâêô ✓",
            "it" => "Italiano: àèéìòù ✓",
            "nl" => "Nederlands: ëï ✓",
            "el" => "Ελληνικά: άέήίόύώ ΐΰ ς ✓",
            "bg" => crate::test_fx::fx("font_bg"),
            "ru" => crate::test_fx::fx("font_ru"),
            "uk" => crate::test_fx::fx("font_uk"),
            "kk" => crate::test_fx::fx("font_kk"),
            "ky" => crate::test_fx::fx("font_ky"),
            "mn" => crate::test_fx::fx("font_mn"),
            _ => "English Filipino Indonesia: “quoted” — … ’ → ✓ ✗ ☐",
        }
    }

    fn chars_of(v: &serde_json::Value, out: &mut String) {
        match v {
            serde_json::Value::String(s) => out.push_str(s),
            serde_json::Value::Array(a) => a.iter().for_each(|x| chars_of(x, out)),
            serde_json::Value::Object(o) => o.values().for_each(|x| chars_of(x, out)),
            _ => {}
        }
    }

    /// Characters of the translations already shipped for `lang`.
    fn shipped_text(lang: Lang) -> String {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data/i18n").join(lang.code());
        let mut out = String::new();
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            if e.path().extension().is_some_and(|x| x == "json") {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(e.path()).unwrap_or_default()) {
                    chars_of(&v, &mut out);
                }
            }
        }
        out
    }

    fn covered(fd: &egui::FontDefinitions, family: &FontFamily, c: char) -> bool {
        fd.families[family].iter().any(|name| {
            let d = &fd.font_data[name];
            ttf_parser::Face::parse(&d.font, d.index).is_ok_and(|f| f.glyph_index(c).is_some())
        })
    }

    #[test]
    fn every_locale_sample_has_glyphs_in_its_font_chain() {
        for &l in Lang::ALL {
            let fd = font_definitions_for_lang(UiFont::Sans, l);
            let mut text = sample(l).to_owned();
            text.push_str(l.native_name());
            // what the shaping step emits for right-to-left locales
            if l.is_rtl() {
                text = crate::rtl::display_for(&text, true).into_owned();
            }
            for fam in [FontFamily::Proportional, crate::theme::family(crate::theme::W_REGULAR)] {
                let miss: Vec<char> = text
                    .chars()
                    .filter(|c| !c.is_whitespace() && !c.is_control() && !covered(&fd, &fam, *c))
                    .collect();
                assert!(miss.is_empty(), "{} lacks glyphs for {miss:?}", l.code());
            }
        }
    }

    #[test]
    fn shipped_translations_have_glyphs() {
        for &l in Lang::ALL {
            let fd = font_definitions_for_lang(UiFont::Sans, l);
            let text = shipped_text(l);
            let text = if l.is_rtl() { crate::rtl::display_for(&text, true).into_owned() } else { text };
            let mut miss: Vec<char> = text
                .chars()
                .filter(|c| !c.is_whitespace() && !c.is_control() && !covered(&fd, &FontFamily::Proportional, *c))
                .collect();
            miss.sort_unstable();
            miss.dedup();
            assert!(miss.is_empty(), "{}: no glyph for {miss:?}", l.code());
        }
    }

    #[test]
    fn check_marks_resolve_for_every_locale() {
        for &l in Lang::ALL {
            let fd = font_definitions_for_lang(UiFont::Sans, l);
            for c in ['\u{2713}', '\u{2717}'] {
                assert!(covered(&fd, &FontFamily::Proportional, c), "{} {c}", l.code());
            }
        }
    }

    /// Warning, close, dropdown, swap, diameter, angle, text caret, lock.
    const UI_MARKS: [char; 8] = [
        '\u{26a0}', '\u{2715}', '\u{25be}', '\u{21c5}', '\u{2300}', '\u{2220}', '\u{258f}', '\u{1f512}',
    ];

    /// Symbols the interface draws as text, and emoji in user text, resolve without the toolkit's
    /// default fonts; only our own faces are registered.
    #[test]
    fn ui_symbols_resolve_from_our_own_fonts() {
        for &l in Lang::ALL {
            for face in UiFont::ALL {
                let fd = font_definitions_for_lang(face, l);
                for name in fd.font_data.keys() {
                    let ours = name.starts_with("ddc.") || name.starts_with(W_REGULAR);
                    assert!(ours, "{} {face:?}: {name}", l.code());
                }
                for c in UI_MARKS {
                    for fam in [FontFamily::Proportional, FontFamily::Monospace] {
                        let ok = covered(&fd, &fam, c);
                        assert!(ok, "{} {face:?} {fam:?} U+{:04X}", l.code(), c as u32);
                    }
                }
            }
        }
    }

    #[test]
    fn fonts_load_only_where_the_script_is_needed() {
        let has = |l: Lang, s: Script| font_definitions_for_lang(UiFont::Sans, l).font_data.contains_key(s.name());
        assert!(has(Lang::Ar, Script::Arabic) && has(Lang::Fa, Script::Arabic) && has(Lang::Ur, Script::Arabic));
        assert!(has(Lang::He, Script::Hebrew) && has(Lang::Ko, Script::Korean) && has(Lang::Ja, Script::Japanese));
        assert!(!has(Lang::De, Script::Arabic) && !has(Lang::Ru, Script::ChineseSc) && !has(Lang::En, Script::Thai));
        assert!(has(Lang::En, Script::Symbols));
        assert_eq!(set_key(Lang::De), set_key(Lang::Fr));
        assert_ne!(set_key(Lang::Ar), set_key(Lang::De));
    }

    #[test]
    fn subsets_stay_small() {
        for s in [Script::ChineseSc, Script::Japanese, Script::Korean] {
            assert!(s.bytes().len() <= 3 * 1024 * 1024, "{s:?} is {} bytes", s.bytes().len());
        }
    }
}
