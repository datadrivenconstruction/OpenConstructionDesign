//! Count words of the XLSX window (chips, notes) with proper plural forms per language.
use ddc_ui::i18n::{Catalog, Lang};

/// Catalog plural message in an explicit language; never reads the global UI language.
pub(super) fn plural(ru: bool, key: &str, n: usize) -> String {
    static RU: std::sync::OnceLock<Catalog> = std::sync::OnceLock::new();
    static EN: std::sync::OnceLock<Catalog> = std::sync::OnceLock::new();
    let c = if ru {
        RU.get_or_init(|| Catalog::new(Lang::Ru))
    } else {
        EN.get_or_init(|| Catalog::new(Lang::En))
    };
    c.plural(key, n as u64)
}

/// Chip kinds whose label agrees with the count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Chip {
    NewColumns,
    Conflicts,
    Removed,
    Skipped,
    Errors,
}

impl Chip {
    fn key(self) -> &'static str {
        match self {
            Chip::NewColumns => "xlw.pl_new_columns",
            Chip::Conflicts => "xlw.pl_conflicts",
            Chip::Removed => "xlw.pl_removed",
            Chip::Skipped => "xlw.pl_skipped",
            Chip::Errors => "xlw.pl_errors",
        }
    }
}

/// Label that follows the bold number of a chip.
pub(crate) fn chip_label(ru: bool, chip: Chip, n: usize) -> String {
    plural(ru, chip.key(), n)
}

/// «3 values»: number and noun in one string.
pub(super) fn values(ru: bool, n: usize) -> String {
    plural(ru, "xlw.pl_values", n)
}

/// «4 elements not in the workbook».
pub(super) fn not_in_book(ru: bool, n: usize) -> String {
    plural(ru, "xlw.pl_not_in_book", n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn en_forms() {
        assert_eq!(chip_label(false, Chip::Removed, 1), "row removed");
        assert_eq!(chip_label(false, Chip::Removed, 2), "rows removed");
        assert_eq!(not_in_book(false, 1), "1 element not in the workbook");
        assert_eq!(not_in_book(false, 2), "2 elements not in the workbook");
        assert_eq!(values(false, 1), "1 value");
        assert_eq!(values(false, 2), "2 values");
        assert_eq!(chip_label(false, Chip::Errors, 1), "error");
        assert_eq!(chip_label(false, Chip::Errors, 2), "errors");
    }

    #[test]
    fn ru_forms_follow_one_few_many() {
        for n in [1, 2, 5, 11, 21, 22, 25] {
            let want = crate::app2_fx::fx(&format!("xc_removed_{n}"));
            assert_eq!(chip_label(true, Chip::Removed, n), want, "n={n}");
            let want = crate::app2_fx::fx(&format!("xc_not_in_book_{n}"));
            assert_eq!(not_in_book(true, n), want, "n={n}");
        }
    }

    #[test]
    fn every_chip_has_a_distinct_form_per_language_category() {
        for ru in [false, true] {
            for chip in [Chip::NewColumns, Chip::Conflicts, Chip::Removed, Chip::Skipped, Chip::Errors] {
                let ns: &[usize] = if ru { &[1, 2, 5, 11, 21, 22, 25] } else { &[1, 2] };
                for &n in ns {
                    let t = chip_label(ru, chip, n);
                    assert!(!t.is_empty() && !t.starts_with("xlw."), "{ru} {chip:?} {n}: {t}");
                    assert!(!t.contains('#') && !t.contains('{'), "{t}");
                }
                assert_ne!(chip_label(ru, chip, 1), chip_label(ru, chip, 5), "{ru} {chip:?}");
            }
        }
    }
}
