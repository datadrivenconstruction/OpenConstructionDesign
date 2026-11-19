//! Text codes (`007`, `1.10`, `1E3`, `1,000`, 19-digit ids) that a spreadsheet turned into
//! numbers: the cell now reads as a number whose canonical text differs from the model text only
//! by that coercion. Such a cell must not overwrite the model text.

#[cfg(test)]
use calamine::Data;

/// Report note for a kept cell (the English text is the locale key source).
pub(super) const REASON: &str = super::msg::NUM_SAME.1;
/// Report reason for a refused cell whose digits the spreadsheet dropped.
pub(super) const LOSSY: &str = super::msg::NUM_LOSSY.1;

/// Column keys of model sheets whose cells are text and get the Text number format.
pub(super) fn is_text_key(key: &str) -> bool {
    matches!(key, "name" | "number" | "mark" | "comments" | "note")
}

/// `true` if `cell` is a number that reads as the model text `model` (leading zeros, trailing
/// zeros, exponent form, thousands separators, digits beyond 15) but is not its canonical text.
/// A cell equal to the model text, or a different number, is not a coercion.
#[cfg(test)]
pub(super) fn coerced(model: &str, cell: &Data) -> bool {
    use super::units::{num_cell_text, num_text, NumText};
    let n = match cell {
        Data::Float(x) => *x,
        Data::Int(i) => *i as f64,
        _ => return false,
    };
    let m = model.trim();
    if !n.is_finite() || m.is_empty() || m == num_cell_text(n) {
        return false;
    }
    matches!(num_text(cell, m, None), NumText::Same | NumText::Lossy)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(x: f64) -> Data {
        Data::Float(x)
    }

    #[test]
    fn leading_zeros_trailing_zeros_exponent_groups() {
        assert!(coerced("007", &f(7.0)));
        assert!(coerced("00123", &f(123.0)));
        assert!(coerced("1.10", &f(1.1)));
        assert!(coerced("1E3", &f(1000.0)));
        assert!(coerced("1e3", &Data::Int(1000)));
        assert!(coerced("1,000", &f(1000.0)));
        assert!(coerced("1 000", &f(1000.0)));
        assert!(coerced("1,5", &f(1.5)));
        assert!(coerced("+7", &f(7.0)));
    }

    #[test]
    fn long_digit_code_rounded_by_the_spreadsheet() {
        assert!(coerced("1234567890123456789", &f(1.23456789012346e18)));
    }

    #[test]
    fn equal_or_different_values_are_not_coercion() {
        assert!(!coerced("12", &f(12.0)));
        assert!(!coerced("12", &Data::Int(12)));
        assert!(!coerced("7", &f(8.0)));
        assert!(!coerced("007", &f(8.0)));
        assert!(!coerced("A-7", &f(7.0)));
        assert!(!coerced("inf", &f(f64::INFINITY)));
        assert!(!coerced("", &f(7.0)));
        assert!(!coerced("007", &Data::String("7".into())));
    }
}
