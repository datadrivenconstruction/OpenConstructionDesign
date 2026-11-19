// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Own fuzzy scorer for the command palette: prefix > word prefix > word-initial chain >
//! substring > subsequence; the query is also tried through the other keyboard layout.
use crate::commands::{cyr_to_latin, latin_to_cyr, Command, Registry};
use crate::i18n::{Catalog, Lang};

/// Palette ranking with usage and arguments (`frecency`).
pub use crate::frecency::search as ranked;

fn lower(s: &str) -> Vec<char> {
    s.chars().flat_map(char::to_lowercase).collect()
}

fn words(t: &[char]) -> Vec<&[char]> {
    t.split(|c| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect()
}

fn starts(h: &[char], n: &[char]) -> bool {
    h.len() >= n.len() && h[..n.len()] == *n
}

/// Query consumed as consecutive prefixes of successive words (Russian initials of "New plan").
fn chain(q: &[char], ws: &[&[char]]) -> bool {
    if q.is_empty() {
        return true;
    }
    for (j, w) in ws.iter().enumerate() {
        let max = w.len().min(q.len());
        for k in (1..=max).rev() {
            if w[..k] == q[..k] && chain(&q[k..], &ws[j + 1..]) {
                return true;
            }
        }
    }
    false
}

/// Score `query` against `text`; `None` = no match. Higher is better.
pub fn score(query: &str, text: &str) -> Option<i32> {
    let q: Vec<char> = lower(query)
        .into_iter()
        .filter(|c| !c.is_whitespace())
        .collect();
    if q.is_empty() {
        return Some(0);
    }
    let t = lower(text);
    let ws = words(&t);
    let len_pen = t.len() as i32;
    let qs = lower(query.trim());
    if starts(&t, &qs) {
        return Some(1000 - len_pen);
    }
    if ws.iter().any(|w| starts(w, &q)) {
        return Some(800 - len_pen);
    }
    if q.len() >= 2 && chain(&q, &ws) {
        return Some(650 - len_pen);
    }
    if let Some(pos) = t.windows(qs.len()).position(|w| w == qs.as_slice()) {
        return Some(500 - pos as i32 - len_pen);
    }
    // Subsequence with bonus for adjacent hits.
    let (mut i, mut s, mut last) = (0usize, 0i32, None::<usize>);
    for (j, c) in t.iter().enumerate() {
        if i < q.len() && *c == q[i] {
            s += if last == Some(j.wrapping_sub(1)) {
                8
            } else {
                2
            };
            last = Some(j);
            i += 1;
        }
    }
    (i == q.len() && q.len() >= 2).then(|| 100 + s - len_pen)
}

/// Query typed in the wrong layout; chars outside the tables (digits, space, punctuation) pass through.
pub fn other_layout(query: &str) -> Option<String> {
    let cyr = query.chars().any(|c| cyr_to_latin(c).is_some());
    let lat = query.chars().any(|c| latin_to_cyr(c).is_some());
    if !cyr && !lat {
        return None;
    }
    Some(
        query
            .chars()
            .map(|c| match c {
                '\u{401}' => '~',
                '~' if !cyr => '\u{401}',
                _ if cyr => cyr_to_latin(c).unwrap_or(c),
                _ => latin_to_cyr(c).unwrap_or(c),
            })
            .collect(),
    )
}

/// Best score of a query over several fields of one item, layout-swapped at a penalty.
pub fn best(query: &str, fields: &[&str]) -> Option<i32> {
    let direct = fields.iter().filter_map(|f| score(query, f)).max();
    let swapped = other_layout(query)
        .and_then(|q| fields.iter().filter_map(|f| score(&q, f)).max())
        .map(|s| s - 50);
    direct.max(swapped)
}

/// Rank registry commands for a palette query: UI-language label, EN label, id, mnemonic.
/// An exact mnemonic wins outright; ties break by id for determinism.
pub fn search<'a>(reg: &'a Registry, cat: &Catalog, query: &str) -> Vec<&'a Command> {
    let q = query.trim();
    let mut v: Vec<(i32, &Command)> = reg
        .all()
        .iter()
        .filter_map(|cmd| {
            let key = cmd.label_key();
            let (l, le) = (cat.t(&key), cat.t_in(Lang::En, &key));
            // the id is a fallback: a short id must not outrank the label a person reads
            let by_id = best(q, &[&cmd.id]).map(|s| s - 50);
            let mut s = best(q, &[&l, &le]).max(by_id)?;
            if cmd
                .mnemonic
                .as_deref()
                .is_some_and(|m| m.eq_ignore_ascii_case(q))
            {
                s += 2000;
            }
            Some((s, cmd))
        })
        .collect();
    v.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.id.cmp(&b.1.id)));
    v.into_iter().map(|x| x.1).collect()
}

/// Rank arbitrary rows (views, levels, elements) by their text fields; stable on ties.
pub fn rank<T>(query: &str, items: impl IntoIterator<Item = (T, Vec<String>)>) -> Vec<(i32, T)> {
    let mut v: Vec<(i32, T)> = items
        .into_iter()
        .filter_map(|(t, f)| {
            let r: Vec<&str> = f.iter().map(String::as_str).collect();
            best(query, &r).map(|s| (s, t))
        })
        .collect();
    v.sort_by(|a, b| b.0.cmp(&a.0));
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fx::fx;

    fn rank(lang: Lang, q: &str) -> Vec<String> {
        let (r, c) = (Registry::builtin(), Catalog::new(lang));
        search(&r, &c, q)
            .into_iter()
            .map(|c| c.id.clone())
            .collect()
    }

    #[test]
    fn fuzzy_ranking() {
        assert_eq!(rank(Lang::Ru, fx("wall_pre"))[0], "arch.wall");
        assert_eq!(rank(Lang::Ru, fx("plan_abbr"))[0], "view.plan");
        assert_eq!(rank(Lang::En, fx("wall_keys"))[0], "arch.wall");
        assert_eq!(rank(Lang::En, "wa")[0], "arch.wall");
        assert_eq!(rank(Lang::En, "ifc")[0], "data.export_ifc");
        assert_eq!(rank(Lang::En, "undo")[0], "edit.undo");
        assert_eq!(rank(Lang::Ru, fx("export"))[0..3].len(), 3);
        assert!(rank(Lang::En, "zzzqqq").is_empty());
    }

    #[test]
    fn scorer_order() {
        let p = score("wal", "Wall").unwrap();
        let w = score("wal", "Bearing wall").unwrap();
        let s = score("all", "Wall").unwrap();
        let q = score("wll", "Wall").unwrap();
        assert!(p > w && w > s && s > q, "{p} {w} {s} {q}");
        assert!(score("x", "Wall").is_none());
        assert_eq!(other_layout("ghbdtn").as_deref(), Some(fx("hello")));
        assert_eq!(other_layout(&format!("{}2", fx("wall"))).as_deref(), Some("cntyf2"));
        assert_eq!(other_layout(fx("spruce")).as_deref(), Some("`krf"));
        assert_eq!(other_layout("`krf").as_deref(), Some(fx("spruce")));
        assert_eq!(other_layout("123"), None);
        let lv = fx("level");
        let r = super::rank(
            &format!("{} 2", fx("level_lc")),
            [
                (1, vec![format!("{lv} 1")]),
                (2, vec![format!("{}: {lv} 2", fx("plan")), format!("{lv} 2")]),
                (3, vec![fx("facade").to_string()]),
            ],
        );
        assert_eq!(r[0].1, 2);
        assert!(r.iter().all(|x| x.1 != 3));
    }
}
