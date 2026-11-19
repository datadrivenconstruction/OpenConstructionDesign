//! Exact search. End rooms first, in the order of the corridor they leave (the bound: no
//! leftover can make a plan cheaper than its corridor); for each pair every remaining item
//! goes into a run by depth-first branch-and-bound on the leftover cost, symmetric runs tried
//! once. A node budget keeps the time bounded; the neighbourhood rebuild then improves.

use super::{run_cost, Cap, Plan, Problem, Run, Side, EPS};

/// Nodes between two reads of the clock.
const CLOCK_EVERY: usize = 512;

/// The cheapest plan the budget finds, improved by the neighbourhood rebuild; `None` when no
/// end-room pair leaves runs that hold the program.
pub(crate) fn solve(p: &Problem) -> Option<Plan> {
    let (we, ee) = (ends(p, 0), ends(p, 1));
    let mut pairs: Vec<(f64, [Cap; 2])> = vec![];
    for &a in &we {
        for &b in &ee {
            if let (Cap::Item(i), Cap::Item(j)) = (a, b) {
                if i == j {
                    continue;
                }
            }
            pairs.push((p.spine_cost(p.w - p.cap_w(a) - p.cap_w(b)), [a, b]));
        }
    }
    pairs.sort_by(|x, y| x.0.total_cmp(&y.0));
    let share = (p.nodes / 8).max(1);
    let mut budget = p.nodes;
    let mut best: Option<Plan> = None;
    for (bound, caps) in pairs {
        if budget == 0 || best.as_ref().is_some_and(|b| bound >= b.cost - EPS) {
            break;
        }
        let Some((xa, xb, runs)) = p.runs(caps) else { continue };
        let rest: Vec<usize> = (0..p.items.len()).filter(|&i| !caps.contains(&Cap::Item(i))).collect();
        let limit = best.as_ref().map_or(f64::INFINITY, |b| b.cost - bound);
        let mut mine = share.min(budget);
        let before = mine;
        let found = pack(p, runs, &rest, limit, &mut mine);
        budget -= before - mine;
        if let Some((c, runs)) = found {
            best = Some(Plan { caps, runs, spine: (xa, xb), cost: bound + c });
        }
    }
    best.map(|b| super::lns::improve(p, b))
}

/// End-room choices at end `k`: the pinned one, else open and every item that may close it,
/// one of each set of equal items.
fn ends(p: &Problem, k: usize) -> Vec<Cap> {
    if let Some(c) = p.caps[k] {
        return vec![c];
    }
    let mut v = vec![Cap::Open];
    for i in 0..p.items.len() {
        let same = (0..i).any(|j| p.items[j].same(&p.items[i]));
        if p.cap_ok(i) && !same {
            v.push(Cap::Item(i));
        }
    }
    v
}

struct State {
    runs: Vec<Run>,
    used: Vec<f64>,
    limit: f64,
    best: Option<(f64, Vec<Run>)>,
}

/// Puts `items` into `runs` (which may already hold items) at the least leftover cost below
/// `limit`; `None` when nothing under the limit is found within the budget.
pub(crate) fn pack(p: &Problem, runs: Vec<Run>, items: &[usize], limit: f64, budget: &mut usize) -> Option<(f64, Vec<Run>)> {
    let key = |i: usize| [Side::South, Side::North].iter().filter(|&&s| p.allowed(i, s)).map(|&s| p.len(i, s)).fold(0.0, f64::max);
    let mut order = items.to_vec();
    order.sort_by(|&a, &b| key(b).total_cmp(&key(a)).then(a.cmp(&b)));
    let used = runs.iter().map(|r| r.items.iter().map(|&i| p.len(i, r.side)).sum()).collect();
    let mut st = State { runs, used, limit, best: None };
    dfs(p, &order, 0, &mut st, budget);
    st.best
}

fn dfs(p: &Problem, order: &[usize], k: usize, st: &mut State, budget: &mut usize) {
    if *budget == 0 {
        return;
    }
    *budget -= 1;
    // the clock is read every so many nodes: past the time cap, once some plan is in hand (a
    // finite limit), the budget is spent
    if *budget % CLOCK_EVERY == 0 && st.limit.is_finite() && super::late() {
        *budget = 0;
        return;
    }
    if k == order.len() {
        let mut c = 0.0;
        for r in &st.runs {
            match run_cost(p, r) {
                Some(x) => c += x,
                None => return,
            }
        }
        if c < st.limit - EPS {
            st.limit = c;
            st.best = Some((c, st.runs.clone()));
        }
        return;
    }
    // what is left must fit the free length somewhere
    let free: f64 = st.runs.iter().zip(&st.used).map(|(r, u)| r.len() - u).sum();
    let need: f64 = order[k..]
        .iter()
        .map(|&i| [Side::South, Side::North].iter().filter(|&&s| p.allowed(i, s)).map(|&s| p.len(i, s)).fold(f64::MAX, f64::min))
        .sum();
    if need > free + EPS {
        return;
    }
    let i = order[k];
    let mut tried: Vec<(Side, i64, i64, usize)> = vec![];
    for r in 0..st.runs.len() {
        let side = st.runs[r].side;
        if !p.allowed(i, side) {
            continue;
        }
        let l = p.len(i, side);
        if st.used[r] + l > st.runs[r].len() + EPS {
            continue;
        }
        let hosts = st.runs[r].items.iter().filter(|&&j| !p.tucked(j, side)).count();
        let sym = (side, (st.runs[r].len() * 1e6).round() as i64, (st.used[r] * 1e6).round() as i64, hosts);
        if tried.contains(&sym) {
            continue;
        }
        tried.push(sym);
        st.runs[r].items.push(i);
        st.used[r] += l;
        dfs(p, order, k + 1, st, budget);
        st.runs[r].items.pop();
        st.used[r] -= l;
        if *budget == 0 {
            return;
        }
    }
}
