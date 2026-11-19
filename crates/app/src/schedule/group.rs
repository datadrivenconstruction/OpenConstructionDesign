//! Schedule grouping: rows ordered by the group columns (user sort kept within a group),
//! a group header with the row count, and subtotals after each group.

use super::{cmp_val, fmt, sum_col, Table};
use crate::units::Units;

#[derive(Clone, Debug, PartialEq)]
pub enum Line {
    Head {
        depth: u8,
        text: String,
        n: usize,
    },
    /// Position in `ord`.
    Row(usize),
    Sub {
        depth: u8,
        tot: Vec<Option<f64>>,
        n: usize,
    },
}

/// Stably reorders `ord` by the group columns (the sort order is preserved within groups).
pub fn sort(tb: &Table, ord: &mut [u32], by: &[usize]) {
    let by: Vec<usize> = by.iter().copied().filter(|&j| j < tb.cols.len()).collect();
    if by.is_empty() {
        return;
    }
    ord.sort_by(|&a, &b| {
        let (ra, rb) = (&tb.rows[a as usize], &tb.rows[b as usize]);
        by.iter()
            .map(|&j| cmp_val(&ra.vals[j], &rb.vals[j]))
            .find(|o| o.is_ne())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// One group identity shared by `sort` and `lines`: values equal under the sort order
/// (case-insensitive, leading zeros ignored) or numbers that display identically.
fn same_group(tb: &Table, j: usize, a: &super::Val, b: &super::Val, u: Units) -> bool {
    match (a, b) {
        (super::Val::N(_), super::Val::N(_)) => {
            cmp_val(a, b).is_eq() || fmt(a, tb.cols[j].kind, u) == fmt(b, tb.cols[j].kind, u)
        }
        _ => cmp_val(a, b).is_eq(),
    }
}

fn sums(tb: &Table, ord: &[u32], span: std::ops::Range<usize>) -> Vec<Option<f64>> {
    (0..tb.cols.len())
        .map(|j| sum_col(tb, &ord[span.clone()], j))
        .collect()
}

/// Display rows for the ordered `ord` (after `sort`); empty means no grouping.
pub fn lines(tb: &Table, ord: &[u32], by: &[usize], subtotals: bool, u: Units) -> Vec<Line> {
    let by: Vec<usize> = by.iter().copied().filter(|&j| j < tb.cols.len()).collect();
    if by.is_empty() {
        return vec![];
    }
    let key = |pos: usize, d: usize| -> String {
        let j = by[d];
        let v = &tb.rows[ord[pos] as usize].vals[j];
        match fmt(v, tb.cols[j].kind, u) {
            s if s.trim().is_empty() => crate::tr("sch.group.empty", &[]),
            s => s,
        }
    };
    let eq = |p: usize, q: usize, d: usize| -> bool {
        let j = by[d];
        same_group(
            tb,
            j,
            &tb.rows[ord[p] as usize].vals[j],
            &tb.rows[ord[q] as usize].vals[j],
            u,
        )
    };
    let mut out = vec![];
    // start of the open group at each level
    let mut open: Vec<usize> = vec![];
    let close = |out: &mut Vec<Line>, open: &mut Vec<usize>, to: usize, end: usize| {
        while open.len() > to {
            let st = open.pop().expect("non-empty");
            if subtotals {
                out.push(Line::Sub {
                    depth: open.len() as u8,
                    tot: sums(tb, ord, st..end),
                    n: end - st,
                });
            }
        }
    };
    for pos in 0..ord.len() {
        // first level where the key changed
        let level = if pos == 0 {
            0
        } else {
            (0..by.len())
                .find(|&d| !eq(pos, pos - 1, d))
                .unwrap_or(by.len())
        };
        if level < by.len() {
            close(&mut out, &mut open, level, pos);
            for d in level..by.len() {
                let n = (pos..ord.len())
                    .take_while(|&q| (0..=d).all(|e| eq(q, pos, e)))
                    .count();
                out.push(Line::Head {
                    depth: d as u8,
                    text: format!("{}: {}", tb.cols[by[d]].title, key(pos, d)),
                    n,
                });
                open.push(pos);
            }
        }
        out.push(Line::Row(pos));
    }
    close(&mut out, &mut open, 0, ord.len());
    out
}

#[cfg(test)]
mod tests {
    use super::super::{col, Cat, Edit, Kind, Row, Val};
    use super::*;

    fn tb() -> Table {
        let cols = vec![
            col("type", "Type", Kind::Text, Edit::No),
            col("len", "Length", Kind::Len, Edit::No),
        ];
        let rows = [("B", 1.0), ("A", 2.0), ("B", 3.0), ("A", 4.0), ("C", 5.0)]
            .iter()
            .enumerate()
            .map(|(i, (t, l))| Row {
                sel: None,
                id: i as i64,
                sub: 0,
                vals: vec![Val::T((*t).into()), Val::N(*l)],
            })
            .collect();
        Table {
            cat: Cat::Walls,
            cols,
            rows,
            row_phase: vec![None; 5],
            phases: vec![],
        }
    }

    #[test]
    fn groups_with_subtotals() {
        let t = tb();
        let mut ord: Vec<u32> = vec![4, 3, 2, 1, 0]; // sorted by length, descending
        sort(&t, &mut ord, &[0]);
        assert_eq!(ord, vec![3, 1, 2, 0, 4], "sort order inside a group");
        let ls = lines(&t, &ord, &[0], true, Units::Metric);
        assert_eq!(
            ls[0],
            Line::Head {
                depth: 0,
                text: "Type: A".into(),
                n: 2
            }
        );
        assert_eq!(ls[1], Line::Row(0));
        match &ls[3] {
            Line::Sub { tot, n, .. } => {
                assert_eq!(*n, 2);
                assert_eq!(tot[1], Some(6.0));
                assert_eq!(tot[0], None);
            }
            l => panic!("{l:?}"),
        }
        assert_eq!(ls.len(), 3 * 2 + 5);
        assert!(lines(&t, &ord, &[], true, Units::Metric).is_empty());
        let no_sub = lines(&t, &ord, &[0], false, Units::Metric);
        assert_eq!(no_sub.len(), 3 + 5);
    }

    #[test]
    fn subtotals_sum_only_additive_and_flag_errors() {
        let mut t = tb();
        t.cols.push(col("id", "№", Kind::Int, Edit::No));
        t.cols.push(col("k", "Coef.", Kind::Num, Edit::No));
        t.cols.push(col("ops", "Openings", Kind::Int, Edit::No));
        for (i, r) in t.rows.iter_mut().enumerate() {
            r.vals.extend([Val::N(i as f64), Val::N(1.5), Val::N(2.0)]);
        }
        // formula error in the second group (rows 0 and 2)
        t.rows[0].vals[1] = Val::T("#division by 0".into());
        let mut ord: Vec<u32> = (0..5).collect();
        sort(&t, &mut ord, &[0]);
        let subs: Vec<Vec<Option<f64>>> = lines(&t, &ord, &[0], true, Units::Metric)
            .into_iter()
            .filter_map(|l| match l {
                Line::Sub { tot, .. } => Some(tot),
                _ => None,
            })
            .collect();
        // first group: length 6, number and coefficient are not summed, 4 openings
        assert_eq!(subs[0], vec![None, Some(6.0), None, None, Some(4.0)]);
        assert!(subs[1][1].unwrap().is_nan());
        assert_eq!(
            fmt(&Val::N(subs[1][1].unwrap()), Kind::Len, Units::Metric),
            "#"
        );
    }

    #[test]
    fn nested_groups() {
        let mut t = tb();
        for r in t.rows.iter_mut() {
            let big = matches!(r.vals[1], Val::N(x) if x > 2.5);
            r.vals.push(Val::T(if big { "yes" } else { "no" }.into()));
        }
        t.cols.push(col("big", "Long", Kind::Text, Edit::No));
        let mut ord: Vec<u32> = (0..5).collect();
        sort(&t, &mut ord, &[0, 2]);
        let ls = lines(&t, &ord, &[0, 2], true, Units::Metric);
        let heads = ls.iter().filter(|l| matches!(l, Line::Head { .. })).count();
        // first group: yes, no · second: yes, no · third: yes
        assert_eq!(heads, 3 + 5);
        assert_eq!(ls.iter().filter(|l| matches!(l, Line::Row(_))).count(), 5);
    }

    fn text_tb(rows: &[(&str, f64)]) -> Table {
        let mut t = tb();
        t.rows = rows
            .iter()
            .enumerate()
            .map(|(i, (x, l))| Row {
                sel: None,
                id: i as i64,
                sub: 0,
                vals: vec![Val::T((*x).into()), Val::N(*l)],
            })
            .collect();
        t.row_phase = vec![None; rows.len()];
        t
    }

    fn heads(ls: &[Line]) -> Vec<(String, usize)> {
        ls.iter()
            .filter_map(|l| match l {
                Line::Head { text, n, .. } => Some((text.clone(), *n)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn group_case_variants_form_one_group_with_one_subtotal() {
        let t = text_tb(&[("Room A", 1.0), ("room a", 2.0), ("Room B", 7.0), ("Room A", 3.0)]);
        let mut ord: Vec<u32> = (0..4).collect();
        sort(&t, &mut ord, &[0]);
        let ls = lines(&t, &ord, &[0], true, Units::Metric);
        let h = heads(&ls);
        assert_eq!(h.len(), 2, "{h:?}");
        assert_eq!(h[0], ("Type: Room A".to_string(), 3));
        let subs: Vec<_> = ls
            .iter()
            .filter_map(|l| match l {
                Line::Sub { tot, n, .. } => Some((*n, tot[1])),
                _ => None,
            })
            .collect();
        assert_eq!(subs, vec![(3, Some(6.0)), (1, Some(7.0))]);
    }

    #[test]
    fn group_leading_zero_numbers_one_head() {
        let t = text_tb(&[("1", 1.0), ("01", 2.0), ("2", 3.0), ("001", 4.0)]);
        let mut ord: Vec<u32> = (0..4).collect();
        sort(&t, &mut ord, &[0]);
        let h = heads(&lines(&t, &ord, &[0], true, Units::Metric));
        assert_eq!(h.len(), 2, "{h:?}");
        assert_eq!(h[0].1, 3);
    }

    #[test]
    fn group_counts_sum_to_row_count() {
        let t = text_tb(&[("a", 1.0), ("A", 1.0), ("b", 1.0), ("B", 1.0), ("c", 1.0)]);
        let mut ord: Vec<u32> = (0..5).collect();
        sort(&t, &mut ord, &[0]);
        let ls = lines(&t, &ord, &[0], true, Units::Metric);
        assert_eq!(heads(&ls).iter().map(|h| h.1).sum::<usize>(), 5);
        let sub_n: usize = ls
            .iter()
            .map(|l| if let Line::Sub { n, .. } = l { *n } else { 0 })
            .sum();
        assert_eq!(sub_n, 5);
    }
}
