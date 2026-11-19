//! Overlap-free label layout: greedy in order; each label has a list of candidate offsets
//! (the first is the original position). The first candidate with no overlap with obstacles and already
//! placed labels is chosen, otherwise the one with the smallest overlap area (ties go to the earlier one).

/// Rectangle [x0, y0, x1, y1].
pub type Rect = [f64; 4];

pub struct Label {
    pub rect: Rect,
    pub moves: Vec<[f64; 2]>,
}

fn overlap(a: &Rect, b: &Rect) -> f64 {
    let w = a[2].min(b[2]) - a[0].max(b[0]);
    let h = a[3].min(b[3]) - a[1].max(b[1]);
    if w > 1e-9 && h > 1e-9 {
        w * h
    } else {
        0.0
    }
}

pub fn shifted(r: &Rect, m: [f64; 2]) -> Rect {
    [r[0] + m[0], r[1] + m[1], r[2] + m[0], r[3] + m[1]]
}

/// Index of the chosen offset for each label (a label without candidates gets `usize::MAX` and is not placed).
pub fn place(obstacles: &[Rect], labels: &[Label]) -> Vec<usize> {
    let mut placed: Vec<Rect> = obstacles.to_vec();
    let mut out = Vec::with_capacity(labels.len());
    for l in labels {
        let mut best: Option<(f64, usize)> = None;
        for (k, m) in l.moves.iter().enumerate() {
            let r = shifted(&l.rect, *m);
            let s: f64 = placed.iter().map(|p| overlap(&r, p)).sum();
            if best.is_none_or(|b| s < b.0) {
                best = Some((s, k));
            }
            if s == 0.0 {
                break;
            }
        }
        match best {
            Some((_, k)) => {
                placed.push(shifted(&l.rect, l.moves[k]));
                out.push(k);
            }
            None => out.push(usize::MAX),
        }
    }
    out
}

/// Sum of pairwise overlap areas of rectangles.
pub fn total_overlap(rs: &[Rect]) -> f64 {
    let mut s = 0.0;
    for i in 0..rs.len() {
        for j in i + 1..rs.len() {
            s += overlap(&rs[i], &rs[j]);
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_label_moves_to_first_free_slot() {
        let moves = vec![[0.0, 0.0], [0.0, 2.0], [0.0, -2.0]];
        let ls = [
            Label {
                rect: [0.0, 0.0, 4.0, 1.0],
                moves: moves.clone(),
            },
            Label {
                rect: [1.0, 0.5, 5.0, 1.5],
                moves: moves.clone(),
            },
            Label {
                rect: [10.0, 0.0, 11.0, 1.0],
                moves,
            },
        ];
        // obstacle blocks the upward slot of label 1
        let obs = [[0.0, 2.6, 6.0, 3.0]];
        assert_eq!(place(&obs, &ls), vec![0, 2, 0]);
    }

    #[test]
    fn least_overlap_when_all_blocked() {
        let ls = [Label {
            rect: [0.0, 0.0, 2.0, 1.0],
            moves: vec![[0.0, 0.0], [1.5, 0.0]],
        }];
        let obs = [[-1.0, -1.0, 2.0, 2.0]];
        assert_eq!(place(&obs, &ls), vec![1]);
        assert_eq!(
            total_overlap(&[[0.0, 0.0, 2.0, 1.0], [1.5, 0.0, 3.0, 1.0]]),
            0.5
        );
    }
}
