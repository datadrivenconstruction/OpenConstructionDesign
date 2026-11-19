// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Grid line (GR): two clicks; horizontal lines are lettered A, B, C… (Cyrillic letters in a
//! Russian project), vertical lines are numbered 1, 2, 3…

use super::{tup, Active, Rec};
use ddc_geom::V2;
use ddc_model::GridSpec;
use ddc_ui::i18n::Lang;
use std::collections::HashSet;

/// The line runs along X (horizontal): it gets a letter.
pub fn is_horizontal(a: (f64, f64), b: (f64, f64)) -> bool {
    (b.0 - a.0).abs() >= (b.1 - a.1).abs()
}

pub fn rec(a: &Active, p: V2, q: V2) -> Result<Rec, String> {
    let (p, q) = (tup(p), tup(q));
    let horizontal = is_horizontal(p, q);
    let cyr = a.cat.lang == Lang::Ru;
    let spec = GridSpec::line("", p, q);
    Ok(Rec::new("grid", move |m| {
        let have: Vec<String> = m.grids()?.into_iter().map(|g| g.name).collect();
        let mut s = spec.clone();
        s.name = Some(next_name(&have, horizontal, cyr));
        m.add_grid(&s)?;
        Ok(1)
    }))
}

/// Names for grids picked on `lines` (in this order), each continuing its own series.
pub(crate) fn pick_names(existing: &[String], lines: &[(V2, V2)], cyr: bool) -> Vec<String> {
    let mut have = existing.to_vec();
    let mut out = vec![];
    for &(a, b) in lines {
        let n = next_name(&have, is_horizontal(tup(a), tup(b)), cyr);
        have.push(n.clone());
        out.push(n);
    }
    out
}

/// One record that adds a grid on each of `lines`, exactly between their ends: one undo step.
pub(crate) fn pick_rec(lines: Vec<(V2, V2)>, cyr: bool) -> Rec {
    Rec::new("grid", move |m| {
        let have: Vec<String> = m.grids()?.into_iter().map(|g| g.name).collect();
        let names = pick_names(&have, &lines, cyr);
        for (k, (&(a, b), name)) in lines.iter().zip(&names).enumerate() {
            if let Err(e) = m.add_grid(&GridSpec::line(name, tup(a), tup(b))) {
                // a refused grid takes back the ones before it: no op is left without its undo step
                for _ in 0..k {
                    m.undo()?;
                }
                return Err(e);
            }
        }
        Ok(lines.len())
    })
}

/// Axis `n` counted from 1: 1 is A, 24 is Z, 25 is AA, 26 is BB (one series for the app).
pub fn letters(n: usize, cyr: bool) -> String {
    match n {
        0 => String::new(),
        n => ddc_geom::axes::axis_letter(n - 1, cyr),
    }
}

/// Inverse of [`letters`]; `None` for a name outside the series.
pub fn letters_index(s: &str, cyr: bool) -> Option<usize> {
    ddc_geom::axes::axis_index(s, cyr).map(|i| i + 1)
}

/// Cyrillic look-alikes of Latin letters count as the same name (Cyrillic capital A == Latin "A").
fn canon(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{410}' => 'A',
            '\u{412}' => 'B',
            '\u{415}' => 'E',
            '\u{41a}' => 'K',
            '\u{41c}' => 'M',
            '\u{41d}' => 'H',
            '\u{420}' => 'P',
            '\u{421}' => 'C',
            '\u{422}' => 'T',
            '\u{425}' => 'X',
            c => c,
        })
        .collect()
}

/// Name of the next line in its series: after the highest used number/letter.
/// `cyr` — Russian project: Cyrillic letters.
pub fn next_name(existing: &[String], horizontal: bool, cyr: bool) -> String {
    let used: HashSet<String> = existing.iter().map(|e| canon(e)).collect();
    let top = existing
        .iter()
        .filter_map(|n| {
            if horizontal {
                letters_index(n, cyr)
            } else {
                n.parse::<usize>().ok()
            }
        })
        .max()
        .unwrap_or(0);
    let mut k = top.saturating_add(1);
    loop {
        let name = if horizontal {
            letters(k, cyr)
        } else {
            k.to_string()
        };
        if !used.contains(&canon(&name)) {
            return name;
        }
        k += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn letters_skip_i_o_and_roll_over() {
        assert_eq!(letters(1, false), "A");
        assert_eq!(letters(8, false), "H");
        assert_eq!(letters(9, false), "J");
        assert_eq!(letters(13, false), "N");
        assert_eq!(letters(14, false), "P");
        assert_eq!(letters(24, false), "Z");
        assert_eq!(letters(25, false), "AA");
        assert_eq!(letters(26, false), "BB");
        assert_eq!(letters(48, false), "ZZ");
        assert_eq!(letters(49, false), "AAA");
        for cyr in [false, true] {
            for n in 1..2000 {
                assert_eq!(letters_index(&letters(n, cyr), cyr), Some(n));
            }
        }
        assert_eq!(letters_index("I", false), None);
        assert_eq!(letters_index("1", false), None);
        // overflow is None, not a panic
        assert_eq!(letters_index("STRUCTURALAXESSTRUCTURALAXES", false), None);
    }

    #[test]
    fn cyrillic_series_skips_hard_letters() {
        // 22 capitals: no Yo, Ze, Short I, O, Kha, Tse, Che, Shcha, hard and soft signs, Yery
        let want = concat!(
            "\u{410}\u{411}\u{412}\u{413}\u{414}\u{415}\u{416}\u{418}\u{41a}\u{41b}\u{41c}",
            "\u{41d}\u{41f}\u{420}\u{421}\u{422}\u{423}\u{424}\u{428}\u{42d}\u{42e}\u{42f}"
        );
        for (i, c) in want.chars().enumerate() {
            assert_eq!(letters(i + 1, true), c.to_string());
        }
        let (a, b, v_, ye, zhe, ya) = ("\u{410}", "\u{411}", "\u{412}", "\u{415}", "\u{416}", "\u{42f}");
        assert_eq!(letters(23, true), format!("{a}{a}"));
        assert_eq!(next_name(&[], true, true), a);
        assert_eq!(next_name(&v(&[a, b]), true, true), v_);
        assert_eq!(next_name(&v(&[ye]), true, true), zhe);
        assert_eq!(next_name(&v(&[ya]), true, true), format!("{a}{a}"));
        assert_eq!(next_name(&v(&["1", "2"]), false, true), "3");
        // Latin look-alike counts as used
        assert_eq!(next_name(&v(&["A"]), true, true), b);
        assert_eq!(next_name(&v(&[a]), true, false), "B");
    }

    #[test]
    fn series_are_independent_by_direction() {
        assert_eq!(next_name(&[], true, false), "A");
        assert_eq!(next_name(&[], false, false), "1");
        let have = v(&["1", "2", "3", "A"]);
        assert_eq!(next_name(&have, true, false), "B");
        assert_eq!(next_name(&have, false, false), "4");
        assert_eq!(next_name(&v(&["H"]), true, false), "J");
        assert_eq!(next_name(&v(&["Z"]), true, false), "AA");
        // gaps are not refilled, names never collide
        assert_eq!(next_name(&v(&["1", "5"]), false, false), "6");
        assert_eq!(next_name(&v(&["\u{416}"]), true, false), "A");
    }

    #[test]
    fn direction() {
        assert!(is_horizontal((0.0, 0.0), (8.0, 1.0)));
        assert!(!is_horizontal((0.0, 0.0), (1.0, 8.0)));
    }
}
