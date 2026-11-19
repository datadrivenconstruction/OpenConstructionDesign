// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Display shaping for right-to-left text: egui draws glyphs left to right, one per code point, with
//! no joining and no bidi. `display` turns logical text into what egui can draw: Arabic letters
//! replaced by their contextual presentation forms (ar, fa, ur), then the Unicode bidi algorithm
//! (UAX #9, `unicode-bidi`) reorders runs so digits and Latin words stay left to right.
//! Apply once, at draw time (`Theme::rich` does); never to text that is stored, compared or searched.
use std::borrow::Cow;

use crate::rtl_table::{FORMS, LAM_ALEF};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Join {
    /// Joins both neighbours (also ZWJ).
    Dual,
    /// Joins the previous letter only (alef, dal, waw, ...).
    Right,
    /// Combining mark: ignored when looking for neighbours.
    Trans,
    None,
}

fn is_mark(c: char) -> bool {
    matches!(c as u32,
        0x610..=0x61A | 0x64B..=0x65F | 0x670 | 0x6D6..=0x6DC | 0x6DF..=0x6E4 | 0x6E7 | 0x6E8
        | 0x6EA..=0x6ED | 0x8D3..=0x8FF | 0x591..=0x5BD | 0x5BF | 0x5C1 | 0x5C2 | 0x5C4 | 0x5C5 | 0x5C7)
}

fn forms(c: char) -> Option<&'static [u32; 4]> {
    FORMS.binary_search_by_key(&c, |e| e.0).ok().map(|i| &FORMS[i].1)
}

fn join_of(c: char) -> Join {
    if c == '\u{200D}' {
        return Join::Dual;
    }
    if is_mark(c) {
        return Join::Trans;
    }
    match forms(c) {
        Some(f) if f[2] != 0 || f[3] != 0 => Join::Dual,
        Some(f) if f[1] != 0 => Join::Right,
        _ => Join::None,
    }
}

/// Replaces Arabic-script letters by the isolated/final/initial/medial presentation forms and joins
/// lam+alef into its ligature. Logical order is kept; ZWJ/ZWNJ are consumed.
pub fn shape_arabic(s: &str) -> String {
    let ch: Vec<char> = s.chars().collect();
    let kinds: Vec<Join> = ch.iter().map(|c| join_of(*c)).collect();
    let prev_joins = |i: usize| {
        kinds[..i].iter().rev().find(|k| **k != Join::Trans).is_some_and(|k| *k == Join::Dual)
    };
    let next_joins = |i: usize| {
        kinds[i + 1..].iter().find(|k| **k != Join::Trans).is_some_and(|k| matches!(k, Join::Dual | Join::Right))
    };
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < ch.len() {
        let c = ch[i];
        i += 1;
        if c == '\u{200C}' || c == '\u{200D}' {
            continue;
        }
        let Some(f) = forms(c) else {
            out.push(c);
            continue;
        };
        let pj = prev_joins(i - 1);
        if c == '\u{644}' {
            if let Some(l) = ch.get(i).and_then(|a| LAM_ALEF.iter().find(|e| e.0 == *a)) {
                let g = if pj { l.1[1] } else { l.1[0] };
                out.extend(char::from_u32(g));
                i += 1;
                continue;
            }
        }
        let nj = kinds[i - 1] == Join::Dual && next_joins(i - 1);
        // index: 0 isolated, 1 final, 2 initial, 3 medial
        let want = match (pj, nj) {
            (true, true) => 3,
            (true, false) => 1,
            (false, true) => 2,
            (false, false) => 0,
        };
        let g = [f[want], f[if want == 3 { 1 } else { want }], f[0]].into_iter().find(|g| *g != 0);
        out.push(g.and_then(char::from_u32).unwrap_or(c));
    }
    out
}

fn mirror(c: char) -> char {
    match c {
        '(' => ')',
        ')' => '(',
        '[' => ']',
        ']' => '[',
        '{' => '}',
        '}' => '{',
        '<' => '>',
        '>' => '<',
        '«' => '»',
        '»' => '«',
        '‹' => '›',
        '›' => '‹',
        '⟨' => '⟩',
        '⟩' => '⟨',
        c => c,
    }
}

/// Visual (left-to-right drawing) order of one line of logical text; `base_rtl` is the paragraph direction.
pub fn reorder_line(line: &str, base_rtl: bool) -> String {
    use unicode_bidi::{BidiInfo, Level};
    let level = if base_rtl { Level::rtl() } else { Level::ltr() };
    let info = BidiInfo::new(line, Some(level));
    let mut out = String::with_capacity(line.len());
    for para in &info.paragraphs {
        let (levels, runs) = info.visual_runs(para, para.range.clone());
        for run in runs {
            let seg = &line[run.clone()];
            if !levels[run.start].is_rtl() {
                out.push_str(seg);
                continue;
            }
            // reverse clusters, not code points: a mark must stay after its base letter
            let mut clusters: Vec<String> = Vec::new();
            for c in seg.chars() {
                match clusters.last_mut() {
                    Some(last) if is_mark(c) => last.push(c),
                    _ => clusters.push(mirror(c).to_string()),
                }
            }
            for cl in clusters.iter().rev() {
                out.push_str(cl);
            }
        }
    }
    out
}

/// True when `s` has Hebrew or Arabic-script letters, i.e. needs `display`.
pub fn contains_rtl(s: &str) -> bool {
    has_rtl(s)
}

fn has_rtl(s: &str) -> bool {
    s.chars().any(|c| matches!(c as u32, 0x590..=0x8FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF))
}

fn is_shaped(s: &str) -> bool {
    s.chars().any(|c| matches!(c as u32, 0xFB50..=0xFDFF | 0xFE70..=0xFEFF))
}

/// Shapes and reorders `s` for drawing. Text without right-to-left letters is returned as is.
pub fn display_for(s: &str, base_rtl: bool) -> Cow<'_, str> {
    if !has_rtl(s) {
        return Cow::Borrowed(s);
    }
    // already shaped (a caller applied `display` earlier): reversing twice would undo it
    if is_shaped(s) {
        return Cow::Borrowed(s);
    }
    let shaped = shape_arabic(s);
    Cow::Owned(shaped.split('\n').map(|l| reorder_line(l, base_rtl)).collect::<Vec<_>>().join("\n"))
}

/// `display_for` in the direction of the interface language.
pub fn display(s: &str) -> Cow<'_, str> {
    display_for(s, crate::i18n::rtl())
}

/// Wraps logical `s` into lines no wider than `max` (measured on the displayed line by `width`), then
/// orders each line visually, joined by `\n`. For multi-line labels: wrapping already-reordered text
/// would cut the visual line, not the logical one, and scramble the reading order.
pub fn display_wrapped(s: &str, base_rtl: bool, max: f32, mut width: impl FnMut(&str) -> f32) -> String {
    let mut lines: Vec<String> = Vec::new();
    for para in s.split('\n') {
        let mut cur = String::new();
        for word in para.split(' ') {
            let cand = if cur.is_empty() { word.to_owned() } else { format!("{cur} {word}") };
            if !cur.is_empty() && width(&display_for(&cand, base_rtl)) > max {
                lines.push(std::mem::replace(&mut cur, word.to_owned()));
            } else {
                cur = cand;
            }
        }
        lines.push(cur);
    }
    lines.iter().map(|l| display_for(l, base_rtl).into_owned()).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(cps: &[u32]) -> String {
        cps.iter().filter_map(|c| char::from_u32(*c)).collect()
    }

    #[test]
    fn salam_takes_joined_forms() {
        // س initial, lam+alef final ligature, م isolated (alef does not join forward)
        assert_eq!(shape_arabic("سلام"), s(&[0xFEB3, 0xFEFC, 0xFEE1]));
        // no ligature: ب initial, ا final; ك medial between joiners
        assert_eq!(shape_arabic("باب"), s(&[0xFE91, 0xFE8E, 0xFE8F]));
        assert_eq!(shape_arabic("مكتب"), s(&[0xFEE3, 0xFEDC, 0xFE98, 0xFE90]));
    }

    #[test]
    fn single_letters_and_non_arabic_stay() {
        assert_eq!(shape_arabic("ب"), s(&[0xFE8F]));
        assert_eq!(shape_arabic("abc 123 ."), "abc 123 .");
    }

    #[test]
    fn lam_alef_is_one_ligature() {
        assert_eq!(shape_arabic("لا"), s(&[0xFEFB]));
        assert_eq!(shape_arabic("بلا"), s(&[0xFE91, 0xFEFC]));
    }

    #[test]
    fn marks_do_not_break_joining() {
        // ب + fatha + ل: ba is initial through the mark
        assert_eq!(shape_arabic("بَل"), s(&[0xFE91, 0x64E, 0xFEDE]));
    }

    #[test]
    fn persian_and_urdu_letters_join() {
        // گ (U+06AF) initial FB94, ی (U+06CC) final FBFD
        assert_eq!(shape_arabic("گی"), s(&[0xFB94, 0xFBFD]));
        // ے (U+06D2) is right-joining: final after ن
        assert_eq!(shape_arabic("نے"), s(&[0xFEE7, 0xFBAF]));
    }

    #[test]
    fn zwnj_breaks_and_zwj_forces_joining() {
        assert_eq!(shape_arabic("ب\u{200C}ب"), s(&[0xFE8F, 0xFE8F]));
        assert_eq!(shape_arabic("ب\u{200D}"), s(&[0xFE91]));
    }

    #[test]
    fn hebrew_is_reversed_without_shaping() {
        assert_eq!(reorder_line("שלום", true), "םולש");
    }

    #[test]
    fn numbers_and_latin_keep_left_to_right_inside_rtl() {
        let out = reorder_line("שלום 123 abc", true);
        // visual: abc, space, 123, space, reversed Hebrew
        assert_eq!(out, "abc 123 םולש");
        let out = reorder_line("abc שלום 12", false);
        // the number after a Hebrew word belongs to the right-to-left run but keeps its own digit order
        assert_eq!(out, "abc 12 םולש");
    }

    #[test]
    fn brackets_mirror_in_rtl_runs() {
        assert_eq!(reorder_line("(שלום)", true), "(םולש)");
        assert_eq!(reorder_line("א (ב)", true), "(ב) א");
    }

    #[test]
    fn display_orders_shaped_arabic_with_embedded_number() {
        let out = display_for("سلام 42", true);
        // number on the left, then the word reversed: meem, lam-alef, seen
        assert_eq!(out, format!("42 {}", s(&[0xFEE1, 0xFEFC, 0xFEB3])));
    }

    #[test]
    fn plain_and_already_shaped_text_is_untouched() {
        assert!(matches!(display_for("Open file", true), Cow::Borrowed(_)));
        let done = display_for("سلام", true).into_owned();
        assert_eq!(display_for(&done, true), done);
    }

    #[test]
    fn wrapping_cuts_the_logical_line() {
        // width = number of chars; three Hebrew words of 2 letters, max 5 -> two words per line
        let out = display_wrapped("אב גד הו", true, 5.0, |t| t.chars().count() as f32);
        assert_eq!(out, "דג בא\nוה");
    }
}
