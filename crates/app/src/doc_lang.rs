// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The language documents are written in: sheet words, title blocks, schedules on sheets, tags.
//! One project setting ([`ddc_model::Model::document_lang`]); a project without one writes in the
//! interface language. Independent of the drawing standard (GOST, ISO), which is a country choice.
use ddc_ui::i18n::Lang;

/// A language argument: a [`Lang`], or the en/ru flag older call sites pass (`true` — Russian).
pub trait AsLang: Copy {
    fn lang(self) -> Lang;
}

impl AsLang for Lang {
    fn lang(self) -> Lang {
        self
    }
}

impl AsLang for bool {
    fn lang(self) -> Lang {
        if self {
            Lang::Ru
        } else {
            Lang::En
        }
    }
}

/// The project's document language: the stored locale, else the interface language.
pub fn doc_lang(m: &ddc_model::Model) -> Lang {
    match m.document_lang() {
        Ok(Some(code)) => Lang::from_code(&code),
        _ => crate::ui_lang(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_locale_wins_over_the_interface_and_clears_back_to_it() {
        let mut m = ddc_model::Model::open(":memory:").unwrap();
        crate::with_test_lang(true, || {
            assert_eq!(doc_lang(&m), Lang::Ru);
            m.set_document_lang(Some("ja")).unwrap();
            assert_eq!(doc_lang(&m), Lang::Ja);
            m.set_document_lang(Some("pt-BR")).unwrap();
            assert_eq!(doc_lang(&m), Lang::PtBr);
            m.set_document_lang(None).unwrap();
            assert_eq!(doc_lang(&m), Lang::Ru);
        });
    }
}
