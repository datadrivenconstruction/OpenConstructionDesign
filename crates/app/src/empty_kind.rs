// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Which designed empty state a panel shows. One pure `empty_kind` per panel decides it; the
//! panel renders `ui_kit::empty_state_lang` with `Kind::what()` (keys `empty.<what>.*`).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Project browser search without hits.
    BrowserSearch,
    /// XLSX preview: filters hide every entry.
    XlFiltered,
    /// XLSX preview: the book changes nothing.
    XlNoChanges,
    /// Version compare: both models are identical.
    VersionSame,
    /// View template list without templates.
    Templates,
    /// Family editor parameter table without parameters.
    FamilyParams,
}

impl Kind {
    /// Key stem of the strings: `empty.<what>.{title,hint,action}`.
    pub fn what(self) -> &'static str {
        match self {
            Kind::BrowserSearch => "browser_search",
            Kind::XlFiltered => "xl_filtered",
            Kind::XlNoChanges => "xl_nochanges",
            Kind::VersionSame => "version_same",
            Kind::Templates => "templates",
            Kind::FamilyParams => "fe_params",
        }
    }

    /// Whether the state has a one-click way out (`empty.<what>.action`).
    pub fn has_action(self) -> bool {
        !matches!(self, Kind::XlNoChanges | Kind::VersionSame)
    }

    pub const ALL: [Kind; 6] = [
        Kind::BrowserSearch,
        Kind::XlFiltered,
        Kind::XlNoChanges,
        Kind::VersionSame,
        Kind::Templates,
        Kind::FamilyParams,
    ];
}

/// Browser: a search is typed, no section row matched and no tree group has rows left.
pub fn browser_search(searching: bool, hits: usize, more_groups: bool) -> Option<Kind> {
    (searching && hits == 0 && !more_groups).then_some(Kind::BrowserSearch)
}

/// XLSX preview: `total` entries in the report, `shown` after the filters.
pub fn xl_preview(total: usize, shown: usize) -> Option<Kind> {
    if shown > 0 {
        None
    } else if total == 0 {
        Some(Kind::XlNoChanges)
    } else {
        Some(Kind::XlFiltered)
    }
}

/// Version compare: number of differences found.
pub fn version_compare(total: usize) -> Option<Kind> {
    (total == 0).then_some(Kind::VersionSame)
}

/// View template list.
pub fn templates(count: usize) -> Option<Kind> {
    (count == 0).then_some(Kind::Templates)
}

/// Family editor parameter table.
pub fn family_params(count: usize) -> Option<Kind> {
    (count == 0).then_some(Kind::FamilyParams)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ui::i18n::Lang;

    #[test]
    fn browser_search_only_without_any_hit() {
        assert_eq!(browser_search(true, 0, false), Some(Kind::BrowserSearch));
        assert_eq!(browser_search(false, 0, false), None);
        assert_eq!(browser_search(true, 2, false), None);
        assert_eq!(browser_search(true, 0, true), None);
    }

    #[test]
    fn xl_preview_separates_filtered_from_no_changes() {
        assert_eq!(xl_preview(0, 0), Some(Kind::XlNoChanges));
        assert_eq!(xl_preview(5, 0), Some(Kind::XlFiltered));
        assert_eq!(xl_preview(5, 3), None);
    }

    #[test]
    fn version_compare_identical_only_at_zero() {
        assert_eq!(version_compare(0), Some(Kind::VersionSame));
        assert_eq!(version_compare(1), None);
    }

    #[test]
    fn templates_and_params_empty_at_zero() {
        assert_eq!(templates(0), Some(Kind::Templates));
        assert_eq!(templates(2), None);
        assert_eq!(family_params(0), Some(Kind::FamilyParams));
        assert_eq!(family_params(1), None);
    }

    #[test]
    fn every_kind_has_strings_in_both_languages() {
        for k in Kind::ALL {
            let mut keys = vec![
                format!("empty.{}.title", k.what()),
                format!("empty.{}.hint", k.what()),
            ];
            if k.has_action() {
                keys.push(format!("empty.{}.action", k.what()));
            }
            for key in keys {
                for lang in [Lang::En, Lang::Ru] {
                    let s = crate::tr_in(lang, &key, &[]);
                    assert!(!s.is_empty() && s != key, "{key} {lang:?}");
                }
            }
        }
        // the two languages really differ
        let en = crate::tr_in(Lang::En, "empty.templates.title", &[]);
        let ru = crate::tr_in(Lang::Ru, "empty.templates.title", &[]);
        assert_ne!(en, ru);
    }
}
