// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Letter names of grid axes. Latin without I and O (read as 1 and 0); Cyrillic without
//! the letters the drawing standard excludes. Past the last letter the letter doubles:
//! A..Z, AA, BB, ... as drawing standards ask, not the spreadsheet AB, AC.

pub const LATIN: [char; 24] = [
    'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'J', 'K', 'L', 'M', 'N', 'P', 'Q', 'R', 'S', 'T', 'U',
    'V', 'W', 'X', 'Y', 'Z',
];
/// The 33 capitals without Yo, Ze, Short I, O, Kha, Tse, Che, Shcha, the hard and soft signs
/// and Yery: 22 letters.
pub const CYRILLIC: [char; 22] = [
    '\u{410}', '\u{411}', '\u{412}', '\u{413}', '\u{414}', '\u{415}', '\u{416}', '\u{418}',
    '\u{41a}', '\u{41b}', '\u{41c}', '\u{41d}', '\u{41f}', '\u{420}', '\u{421}', '\u{422}',
    '\u{423}', '\u{424}', '\u{428}', '\u{42d}', '\u{42e}', '\u{42f}',
];

pub fn alphabet(cyr: bool) -> &'static [char] {
    if cyr {
        &CYRILLIC
    } else {
        &LATIN
    }
}

/// Name of the letter axis `i` (from 0).
pub fn axis_letter(i: usize, cyr: bool) -> String {
    let set = alphabet(cyr);
    std::iter::repeat(set[i % set.len()])
        .take(i / set.len() + 1)
        .collect()
}

/// Inverse of [`axis_letter`]; `None` for a name outside the series (or too long to count).
pub fn axis_index(s: &str, cyr: bool) -> Option<usize> {
    let set = alphabet(cyr);
    let first = s.chars().next()?;
    if !s.chars().all(|c| c == first) {
        return None;
    }
    let pos = set.iter().position(|&c| c == first)?;
    let times = s.chars().count();
    (times - 1).checked_mul(set.len())?.checked_add(pos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_skip_the_confusing_ones_and_double_past_the_end() {
        assert_eq!(axis_letter(0, false), "A");
        assert_eq!(axis_letter(8, false), "J");
        assert_eq!(axis_letter(23, false), "Z");
        assert_eq!(axis_letter(24, false), "AA");
        assert_eq!(axis_letter(25, false), "BB");
        assert_eq!(axis_letter(48, false), "AAA");
        assert_eq!(axis_letter(0, true), "\u{410}");
        assert_eq!(axis_letter(21, true), "\u{42f}");
        assert_eq!(axis_letter(22, true), "\u{410}\u{410}");
        assert_eq!(axis_letter(23, true), "\u{411}\u{411}");
        let excluded =
            "\u{401}\u{417}\u{419}\u{41e}\u{425}\u{426}\u{427}\u{429}\u{42a}\u{42b}\u{42c}";
        assert!(CYRILLIC.iter().all(|c| !excluded.contains(*c)));
        assert!(!LATIN.contains(&'I') && !LATIN.contains(&'O'));
        for cyr in [false, true] {
            for i in 0..500 {
                assert_eq!(axis_index(&axis_letter(i, cyr), cyr), Some(i));
            }
        }
        for bad in ["", "I", "O", "AB", "1", "a"] {
            assert_eq!(axis_index(bad, false), None, "{bad}");
        }
        assert_eq!(axis_index("\u{417}", true), None);
    }
}
