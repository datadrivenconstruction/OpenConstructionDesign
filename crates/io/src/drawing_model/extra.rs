//! Recognition beyond walls: axes (long lines on axis layers, names from tags), columns (small
//! closed rectangles and circles), rooms from closed outlines on area layers and
//! openings from block inserts via "block → family" rules.

use std::f64::consts::PI;

use super::source::{UPoly, Underlay};
use super::propose::{area_number, drawing_mark};
use super::{hash_key, q, Memory, OpeningCand, RoomCand, WallCand};
use crate::num_file::{numeric_like, parse_num, Mark, NumErr};

type P = (f64, f64);

/// Area text (`12,5 m2`, or a bare decimal `12.5`): kept out of room names. An ambiguous
/// number (`1,250 m2`) is still area text; its value is never read here.
fn area_like(t: &str, mark: Option<Mark>) -> bool {
    let (n, unit) = match area_number(t) {
        Some(n) => (n, true),
        None => (t.to_string(), false),
    };
    numeric_like(&n)
        && !matches!(parse_num(&n, mark), Err(NumErr::Invalid))
        && (unit || n.contains(['.', ',']))
}

#[derive(Clone, Debug, PartialEq)]
pub struct GridCand {
    pub id: u64,
    pub name: String,
    pub a: P,
    pub b: P,
    /// The name was taken from a tag on the drawing (otherwise — by order).
    pub named: bool,
    pub confidence: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ColumnCand {
    pub id: u64,
    pub at: P,
    /// Radians, direction of side `b`.
    pub rotation: f64,
    /// `None` — a `b × h` rectangle; `Some(d)` — a circle.
    pub diameter: Option<f64>,
    pub b: f64,
    pub h: f64,
    pub confidence: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleKind {
    Door,
    Window,
    /// The block is not an opening (furniture, symbols): do not propose by the name heuristic.
    Ignore,
}

impl RuleKind {
    pub fn text(self) -> &'static str {
        match self {
            RuleKind::Door => "door",
            RuleKind::Window => "window",
            RuleKind::Ignore => "ignore",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "door" => Some(RuleKind::Door),
            "window" => Some(RuleKind::Window),
            "ignore" => Some(RuleKind::Ignore),
            _ => None,
        }
    }
}

/// Rule "block name → opening": `pattern` — a case-insensitive mask with `*`.
#[derive(Clone, Debug, PartialEq)]
pub struct BlockRule {
    pub pattern: String,
    pub kind: RuleKind,
    /// Opening family (type), stored in the `family` property.
    pub family: String,
    /// Width, m; `None` — the WIDTH attribute (or its Russian equivalent) or the block's X extent.
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub sill: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BlockRules(pub Vec<BlockRule>);

impl Default for BlockRules {
    fn default() -> Self {
        let r = |p: &str, k, f: &str| BlockRule {
            pattern: p.into(),
            kind: k,
            family: f.into(),
            width: None,
            height: None,
            sill: None,
        };
        BlockRules(vec![
            r("*door*", RuleKind::Door, "\u{414}\u{432}\u{435}\u{440}\u{44c}"),
            r("*\u{434}\u{432}\u{435}\u{440}*", RuleKind::Door, "\u{414}\u{432}\u{435}\u{440}\u{44c}"),
            r("*window*", RuleKind::Window, "\u{41e}\u{43a}\u{43d}\u{43e}"),
            // short names like WIN_1500 / A-WIN-12 (prefix forms only: "*win*" hits "winder")
            r("win*", RuleKind::Window, "\u{41e}\u{43a}\u{43d}\u{43e}"),
            r("*_win*", RuleKind::Window, "\u{41e}\u{43a}\u{43d}\u{43e}"),
            r("*-win*", RuleKind::Window, "\u{41e}\u{43a}\u{43d}\u{43e}"),
            r("*\u{43e}\u{43a}\u{43d}*", RuleKind::Window, "\u{41e}\u{43a}\u{43d}\u{43e}"),
        ])
    }
}

/// `*` — any substring; comparison is case-insensitive.
pub fn glob(pattern: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (
        pattern.to_lowercase().chars().collect(),
        name.to_lowercase().chars().collect(),
    );
    let (mut i, mut j, mut star, mut mark) = (0, 0, None, 0);
    while j < n.len() {
        if i < p.len() && p[i] != '*' && p[i] == n[j] {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == '*' {
            star = Some(i);
            mark = j;
            i += 1;
        } else if let Some(s) = star {
            i = s + 1;
            mark += 1;
            j = mark;
        } else {
            return false;
        }
    }
    while i < p.len() && p[i] == '*' {
        i += 1;
    }
    i == p.len()
}

impl BlockRules {
    /// The first matching rule.
    pub fn find(&self, block: &str) -> Option<&BlockRule> {
        self.0.iter().find(|r| glob(&r.pattern, block))
    }

    /// Lines `mask\tkind\tfamily\twidth\theight\tsill` (empty — from the drawing).
    pub fn to_text(&self) -> String {
        let o = |v: Option<f64>| v.map_or(String::new(), |x| format!("{x}"));
        self.0
            .iter()
            .map(|r| {
                format!(
                    "{}\t{}\t{}\t{}\t{}\t{}\n",
                    r.pattern,
                    r.kind.text(),
                    r.family,
                    o(r.width),
                    o(r.height),
                    o(r.sill)
                )
            })
            .collect()
    }

    /// Unreadable lines and comment lines (`#`) are skipped.
    pub fn from_text(t: &str) -> Self {
        let num = |s: Option<&str>| {
            s.and_then(|v| v.trim().parse::<f64>().ok())
                .filter(|v| v.is_finite() && *v > 0.0)
        };
        BlockRules(
            t.lines()
                .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
                .filter_map(|l| {
                    let mut f = l.split('\t');
                    let pattern = f.next()?.trim().to_string();
                    let kind = RuleKind::parse(f.next()?)?;
                    let family = f.next().unwrap_or("").trim().to_string();
                    (!pattern.is_empty()).then(|| BlockRule {
                        pattern,
                        kind,
                        family,
                        width: num(f.next()),
                        height: num(f.next()),
                        sill: num(f.next()),
                    })
                })
                .collect(),
        )
    }
}

/// A word in a layer name: the start of a token (tokens are letters and digits separated by other
/// characters and by a letter-to-digit change); a word of up to 3 characters matches only a whole token
/// ("col" is not found in "COLOR" or "protocol").
pub(crate) fn has_word(layer: &str, words: &[&str]) -> bool {
    let l = layer.to_lowercase();
    let toks = layer_tokens(&l);
    words.iter().any(|w| {
        let short = w.chars().count() <= 3;
        toks.iter().any(|t| {
            if short {
                t.as_str() == *w
            } else {
                t.starts_with(*w)
            }
        })
    })
}

fn layer_tokens(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut prev_digit: Option<bool> = None;
    for ch in s.chars() {
        if !ch.is_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_digit = None;
            continue;
        }
        let d = ch.is_ascii_digit();
        if prev_digit.is_some_and(|p| p != d) && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        cur.push(ch);
        prev_digit = Some(d);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub(crate) const WALL_WORDS: [&str; 3] = ["wall", "\u{441}\u{442}\u{435}\u{43d}", "mur"];
const GRID_WORDS: [&str; 6] = ["axis", "grid", "\u{43e}\u{441}\u{44c}", "\u{43e}\u{441}\u{438}", "\u{441}\u{435}\u{442}\u{43a}", "axes"];
const COLUMN_WORDS: [&str; 5] = ["colu", "\u{43a}\u{43e}\u{43b}\u{43e}\u{43d}", "col", "cols", "\u{43f}\u{438}\u{43b}\u{43e}\u{43d}"];
const ROOM_WORDS: [&str; 7] = ["room", "area", "\u{43f}\u{43e}\u{43c}\u{435}\u{449}", "\u{43f}\u{43b}\u{43e}\u{449}\u{430}\u{434}", "\u{437}\u{43e}\u{43d}\u{430}", "space", "\u{44d}\u{43a}\u{441}\u{43f}\u{43b}"];

fn dist(a: P, b: P) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

/// Axes: long (≥ 3 m) lines on axis layers, collinear pieces merged; the name is the text in
/// the tag circle at the end, otherwise digits left to right for vertical axes and letters bottom to top.
pub fn propose_grids(u: &Underlay, mem: &Memory) -> Vec<GridCand> {
    let mut segs: Vec<(P, P)> = u
        .lines
        .iter()
        .filter(|l| has_word(&l.layer, &GRID_WORDS) && dist(l.a, l.b) >= 3.0)
        .map(|l| (l.a, l.b))
        .collect();
    // merge collinear pieces (a dash-dot line may arrive as separate segments)
    let mut merged: Vec<(P, P)> = vec![];
    segs.sort_by(|a, b| dist(b.0, b.1).total_cmp(&dist(a.0, a.1)));
    'next: for (a, b) in segs {
        for m in merged.iter_mut() {
            let (d, len) = ((m.1 .0 - m.0 .0, m.1 .1 - m.0 .1), dist(m.0, m.1));
            let u = (d.0 / len, d.1 / len);
            let off = |p: P| ((p.0 - m.0 .0) * -u.1 + (p.1 - m.0 .1) * u.0).abs();
            let t = |p: P| (p.0 - m.0 .0) * u.0 + (p.1 - m.0 .1) * u.1;
            if off(a) < 0.02 && off(b) < 0.02 {
                let (ta, tb) = (t(a), t(b));
                let (lo, hi) = (ta.min(tb), ta.max(tb));
                if lo <= len + 0.5 && hi >= -0.5 {
                    let (n0, n1) = (lo.min(0.0), hi.max(len));
                    *m = (
                        (m.0 .0 + u.0 * n0, m.0 .1 + u.1 * n0),
                        (m.0 .0 + u.0 * n1, m.0 .1 + u.1 * n1),
                    );
                    continue 'next;
                }
            }
        }
        merged.push((a, b));
    }
    // direction: vertical ones bottom to top, the rest left to right
    for m in merged.iter_mut() {
        let d = (m.1 .0 - m.0 .0, m.1 .1 - m.0 .1);
        let vertical = d.1.abs() > d.0.abs();
        if (vertical && d.1 < 0.0) || (!vertical && d.0 < 0.0) {
            *m = (m.1, m.0);
        }
    }
    // tags: circles 0.15..1.5 m at the axis end, text inside
    let bubbles: Vec<(P, f64)> = u
        .arcs
        .iter()
        .filter(|a| a.a1 - a.a0 >= 2.0 * PI - 1e-6 && a.r >= 0.15 && a.r <= 1.5)
        .map(|a| (a.c, a.r))
        .collect();
    let bubble_name = |end: P, dir: P| -> Option<String> {
        let (c, r) = bubbles.iter().copied().find(|(c, r)| {
            let along = dist(*c, end);
            along <= r * 1.2 + 0.05 || {
                let p = (end.0 + dir.0 * r, end.1 + dir.1 * r);
                dist(p, *c) <= r * 0.25 + 0.02
            }
        })?;
        u.texts
            .iter()
            .filter(|t| dist(t.pos, c) <= r * 1.1 && !t.text.trim().is_empty())
            .min_by(|a, b| dist(a.pos, c).total_cmp(&dist(b.pos, c)))
            .map(|t| t.text.trim().to_string())
    };
    let mut out: Vec<(GridCand, bool)> = vec![];
    for (a, b) in &merged {
        let len = dist(*a, *b);
        let dir = ((b.0 - a.0) / len, (b.1 - a.1) / len);
        let name = bubble_name(*b, dir).or_else(|| bubble_name(*a, (-dir.0, -dir.1)));
        let vertical = dir.1.abs() > dir.0.abs();
        out.push((
            GridCand {
                id: 0,
                name: name.clone().unwrap_or_default(),
                a: *a,
                b: *b,
                named: name.is_some(),
                confidence: if name.is_some() { 0.95 } else { 0.8 },
            },
            vertical,
        ));
    }
    const LETTERS: [&str; 22] = [
        "\u{410}", "\u{411}", "\u{412}", "\u{413}", "\u{414}", "\u{415}", "\u{416}", "\u{418}", "\u{41a}", "\u{41b}", "\u{41c}", "\u{41d}", "\u{41f}", "\u{420}", "\u{421}", "\u{422}", "\u{423}", "\u{424}",
        "\u{428}", "\u{42d}", "\u{42e}", "\u{42f}",
    ];
    let mut vert: Vec<usize> = (0..out.len()).filter(|&k| out[k].1).collect();
    vert.sort_by(|&x, &y| out[x].0.a.0.total_cmp(&out[y].0.a.0));
    let mut hor: Vec<usize> = (0..out.len()).filter(|&k| !out[k].1).collect();
    hor.sort_by(|&x, &y| out[x].0.a.1.total_cmp(&out[y].0.a.1));
    let mut taken: Vec<String> = out
        .iter()
        .filter(|g| g.0.named)
        .map(|g| g.0.name.clone())
        .collect();
    let mut fresh = |cands: &mut dyn Iterator<Item = String>| -> String {
        for c in cands {
            if !taken.contains(&c) {
                taken.push(c.clone());
                return c;
            }
        }
        String::from("?")
    };
    for &k in &vert {
        if !out[k].0.named {
            out[k].0.name = fresh(&mut (1..1000).map(|n| n.to_string()));
        }
    }
    for &k in &hor {
        if !out[k].0.named {
            out[k].0.name = fresh(&mut (0..LETTERS.len() * 10).map(|n| {
                let base = LETTERS[n % LETTERS.len()];
                if n < LETTERS.len() {
                    base.to_string()
                } else {
                    format!("{base}{}", n / LETTERS.len())
                }
            }));
        }
    }
    let mut res: Vec<GridCand> = vert
        .iter()
        .chain(hor.iter())
        .map(|&k| {
            let mut g = out[k].0.clone();
            // geometry only: the auto name depends on neighbours, the id must not
            g.id = hash_key(4, &[q(g.a.0), q(g.a.1), q(g.b.0), q(g.b.1)], "");
            g
        })
        .filter(|g| !mem.contains(g.id))
        .collect();
    res.dedup_by_key(|g| g.id);
    res
}

/// A rectangle from 4 vertices with right angles: centre, rotation, sides.
fn as_rect(pts: &[P]) -> Option<(P, f64, f64, f64)> {
    let mut v: Vec<P> = pts.to_vec();
    if v.len() == 5 && dist(v[0], v[4]) < 1e-6 {
        v.pop();
    }
    if v.len() != 4 {
        return None;
    }
    for k in 0..4 {
        let (a, b, c) = (v[k], v[(k + 1) % 4], v[(k + 2) % 4]);
        let (e1, e2) = ((b.0 - a.0, b.1 - a.1), (c.0 - b.0, c.1 - b.1));
        let (l1, l2) = (e1.0.hypot(e1.1), e2.0.hypot(e2.1));
        if l1 < 1e-6 || l2 < 1e-6 || (e1.0 * e2.0 + e1.1 * e2.1).abs() > 0.02 * l1 * l2 {
            return None;
        }
    }
    let c = (
        v.iter().map(|p| p.0).sum::<f64>() / 4.0,
        v.iter().map(|p| p.1).sum::<f64>() / 4.0,
    );
    let e = (v[1].0 - v[0].0, v[1].1 - v[0].1);
    let mut rot = e.1.atan2(e.0);
    let (mut b, mut h) = (dist(v[0], v[1]), dist(v[1], v[2]));
    // normalise rotation to (-45°, 45°]
    while rot > PI / 4.0 + 1e-9 {
        rot -= PI / 2.0;
        std::mem::swap(&mut b, &mut h);
    }
    while rot <= -PI / 4.0 + 1e-9 {
        rot += PI / 2.0;
        std::mem::swap(&mut b, &mut h);
    }
    if rot.abs() < 1e-9 {
        rot = 0.0;
    }
    Some((c, rot, b, h))
}

fn ring_area(r: &[P]) -> f64 {
    let n = r.len();
    ((0..n)
        .map(|k| r[k].0 * r[(k + 1) % n].1 - r[(k + 1) % n].0 * r[k].1)
        .sum::<f64>()
        / 2.0)
        .abs()
}

fn inside(p: P, r: &[P]) -> bool {
    let mut c = false;
    let n = r.len();
    for k in 0..n {
        let (a, b) = (r[k], r[(k + 1) % n]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < a.0 + (p.1 - a.1) * (b.0 - a.0) / (b.1 - a.1) {
            c = !c;
        }
    }
    c
}

fn centroid(r: &[P]) -> P {
    let n = r.len() as f64;
    (
        r.iter().map(|p| p.0).sum::<f64>() / n,
        r.iter().map(|p| p.1).sum::<f64>() / n,
    )
}

/// Columns: small (0.15–1.2 m) closed rectangles and circles — on a column layer or at an
/// axis intersection. A solid fill raises confidence but by itself does not make a column
/// (piers between openings are filled the same way), and a fill touching a wall is ignored.
pub fn propose_columns(
    u: &Underlay,
    grids: &[GridCand],
    walls: &[WallCand],
    mem: &Memory,
) -> Vec<ColumnCand> {
    let crossings: Vec<P> = {
        let mut v = vec![];
        for (i, g) in grids.iter().enumerate() {
            for h in &grids[i + 1..] {
                if let Some(p) = seg_cross(g.a, g.b, h.a, h.b) {
                    v.push(p);
                }
            }
        }
        v
    };
    let solid: Vec<(&str, &Vec<P>)> = u
        .fills
        .iter()
        .filter(|f| f.solid && f.loops.len() == 1)
        .map(|f| (f.layer.as_str(), &f.loops[0]))
        .collect();
    let solid_loops: Vec<&Vec<P>> = solid.iter().map(|s| s.1).collect();
    let mut shapes: Vec<(UPoly, bool)> = u.closed.iter().map(|c| (c.clone(), false)).collect();
    for (layer, l) in &solid {
        shapes.push((
            UPoly {
                layer: layer.to_string(),
                pts: (*l).clone(),
                circle: None,
            },
            true,
        ));
    }
    let mut out: Vec<ColumnCand> = vec![];
    for (s, from_fill) in &shapes {
        let (at, rot, b, h, d) = if let Some(r) = s.circle {
            (centroid(&s.pts), 0.0, 2.0 * r, 2.0 * r, Some(2.0 * r))
        } else if let Some((c, rot, b, h)) = as_rect(&s.pts) {
            (c, rot, b, h, None)
        } else {
            continue;
        };
        if !(0.15..=1.2).contains(&b.max(h)) || b.min(h) < 0.1 {
            continue;
        }
        let area = ring_area(&s.pts);
        let touch = touches_wall(at, b.max(h), walls);
        let filled = !touch
            && (*from_fill
                || solid_loops.iter().any(|l| {
                    inside(at, l) && {
                        let a = ring_area(l);
                        a > 0.7 * area && a < 1.4 * area
                    }
                }));
        let on_layer = has_word(&s.layer, &COLUMN_WORDS);
        let at_grid = crossings.iter().any(|c| dist(*c, at) <= 0.3);
        let conf = match (on_layer, filled, at_grid) {
            (true, _, true) | (_, true, true) => 0.95,
            (true, _, _) => 0.85,
            (false, false, true) if !from_fill && !touch => 0.7,
            _ => continue,
        };
        if let Some(prev) = out.iter_mut().find(|c| dist(c.at, at) < 0.05) {
            prev.confidence = prev.confidence.max(conf);
            continue;
        }
        let id = hash_key(
            5,
            &[q(at.0), q(at.1), q(b), q(h)],
            if d.is_some() { "c" } else { "r" },
        );
        if mem.contains(id) {
            continue;
        }
        out.push(ColumnCand {
            id,
            at,
            rotation: rot,
            diameter: d,
            b,
            h,
            confidence: conf,
        });
    }
    out
}

/// A shape of size `size` centred at `at` lies within a wall strip or abuts its face.
fn touches_wall(at: P, size: f64, walls: &[WallCand]) -> bool {
    walls.iter().any(|w| {
        let d = (w.b.0 - w.a.0, w.b.1 - w.a.1);
        let len = d.0.hypot(d.1);
        if !(len > 1e-9) {
            return false;
        }
        let u = (d.0 / len, d.1 / len);
        let rel = (at.0 - w.a.0, at.1 - w.a.1);
        let t = rel.0 * u.0 + rel.1 * u.1;
        let lat = (rel.0 * -u.1 + rel.1 * u.0).abs();
        let reach = w.thickness / 2.0 + size / 2.0 + 0.02;
        lat <= reach && t >= -size / 2.0 && t <= len + size / 2.0
    })
}

fn seg_cross(a: P, b: P, c: P, d: P) -> Option<P> {
    let r = (b.0 - a.0, b.1 - a.1);
    let s = (d.0 - c.0, d.1 - c.1);
    let den = r.0 * s.1 - r.1 * s.0;
    if den.abs() < 1e-12 {
        return None;
    }
    let t = ((c.0 - a.0) * s.1 - (c.1 - a.1) * s.0) / den;
    let w = ((c.0 - a.0) * r.1 - (c.1 - a.1) * r.0) / den;
    ((-0.01..=1.01).contains(&t) && (-0.01..=1.01).contains(&w))
        .then(|| (a.0 + r.0 * t, a.1 + r.1 * t))
}

/// Rooms from closed outlines on area layers: the outline is the boundary, labels are inside.
/// A room from regions between walls whose point is inside an outline gets that boundary.
/// Outlines go from smaller to larger: a room gets the smallest enclosing outline, and an
/// outline containing points of several rooms (an apartment, a floor) is skipped.
pub fn rooms_from_outlines(u: &Underlay, rooms: &mut Vec<RoomCand>, mem: &Memory) {
    let mark = drawing_mark(u);
    let mut polys: Vec<(&UPoly, f64)> = u
        .closed
        .iter()
        .filter(|p| p.circle.is_none() && has_word(&p.layer, &ROOM_WORDS))
        .filter(|p| p.pts.iter().all(|q| q.0.is_finite() && q.1.is_finite()))
        .map(|p| (p, ring_area(&p.pts)))
        .filter(|(_, a)| *a >= 1.0)
        .collect();
    polys.sort_by(|a, b| a.1.total_cmp(&b.1));
    for (poly, area) in polys {
        let mut hits = rooms
            .iter()
            .enumerate()
            .filter(|(_, r)| inside(r.point, &poly.pts))
            .map(|(k, _)| k);
        if let Some(k) = hits.next() {
            if hits.next().is_none() {
                let r = &mut rooms[k];
                if r.boundary.is_empty() {
                    r.boundary = poly.pts.clone();
                    r.confidence = r.confidence.max(0.9);
                }
            }
            continue;
        }
        let texts: Vec<&str> = u
            .texts
            .iter()
            .filter(|t| inside(t.pos, &poly.pts))
            .map(|t| t.text.trim())
            .filter(|t| !t.is_empty())
            .collect();
        let is_num = |t: &str| {
            t.chars()
                .all(|c| c.is_ascii_digit() || c == '.' || c == '-')
        };
        let is_area = |t: &str| area_like(t, mark);
        let number = texts
            .iter()
            .find(|t| is_num(t) && !t.contains('.'))
            .map_or(String::new(), |t| t.to_string());
        let name = texts
            .iter()
            .find(|t| !is_num(t) && !is_area(t))
            .map_or("\u{41f}\u{43e}\u{43c}\u{435}\u{449}\u{435}\u{43d}\u{438}\u{435}".to_string(), |t| t.to_string());
        let mut pt = centroid(&poly.pts);
        if !inside(pt, &poly.pts) {
            match u.texts.iter().find(|t| inside(t.pos, &poly.pts)) {
                Some(t) => pt = t.pos,
                None => continue,
            }
        }
        let (lo, hi) = poly.pts.iter().fold(
            ((f64::MAX, f64::MAX), (f64::MIN, f64::MIN)),
            |(lo, hi), p| {
                (
                    (lo.0.min(p.0), lo.1.min(p.1)),
                    (hi.0.max(p.0), hi.1.max(p.1)),
                )
            },
        );
        let id = hash_key(
            3,
            &[q(lo.0), q(lo.1), q(hi.0), q(hi.1)],
            &format!("{name}\0{number}"),
        );
        if mem.contains(id) {
            continue;
        }
        rooms.push(RoomCand {
            id,
            name,
            number,
            point: pt,
            area,
            confidence: 0.85,
            boundary: poly.pts.clone(),
        });
    }
}

/// Openings from block inserts: a rule sets the kind and family; the wall is the nearest one parallel to the block
/// axis passing through the block centre. An opening at the place of one found by a gap is refined.
pub fn openings_from_blocks(
    u: &Underlay,
    rules: &BlockRules,
    walls: &[WallCand],
    openings: &mut Vec<OpeningCand>,
    defaults: (f64, f64, f64),
    mem: &Memory,
) -> usize {
    openings_from_blocks_checked(u, rules, walls, openings, defaults, mem).added
}

/// Block openings plus the width attributes that could not be read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BlockOpenings {
    pub added: usize,
    /// Width attributes whose number reads two ways or not at all (`1,200` with no decimal
    /// mark in the drawing); never used as a value.
    pub unreadable_widths: usize,
    /// Openings proposed from such blocks: geometric size or gap width, left for review.
    pub review: Vec<u64>,
}

/// Confidence of an opening whose width attribute is unreadable: below the auto-accept bar.
pub const UNREADABLE_WIDTH_CONFIDENCE: f64 = 0.45;

/// As [`openings_from_blocks`], also counting unreadable width attributes. Such a block still
/// proposes an opening (block size or the matched gap), flagged and not auto-accepted.
pub fn openings_from_blocks_checked(
    u: &Underlay,
    rules: &BlockRules,
    walls: &[WallCand],
    openings: &mut Vec<OpeningCand>,
    defaults: (f64, f64, f64),
    mem: &Memory,
) -> BlockOpenings {
    let (door_h, win_sill, win_h) = defaults;
    let mut out = BlockOpenings::default();
    let mark = drawing_mark(u);
    for ins in &u.inserts {
        let Some(rule) = rules.find(&ins.name) else {
            continue;
        };
        let kind = match rule.kind {
            RuleKind::Door => "door",
            RuleKind::Window => "window",
            RuleKind::Ignore => continue,
        };
        let attr = ins
            .attribs
            .iter()
            .find(|(t, _)| {
                let t = t.to_uppercase();
                t == "W" || t == "B" || t == "\u{428}" || t.contains("WIDTH") || t.contains("\u{428}\u{418}\u{420}")
            })
            .map(|(_, v)| v.as_str());
        let attr_w = attr
            .and_then(|v| parse_num(v, mark).ok().map(|n| n.value))
            .map(|v| if v > 10.0 { v / 1000.0 } else { v });
        // a rule width wins, so the attribute is not read at all then
        let unreadable = rule.width.is_none()
            && attr.is_some_and(|v| parse_num(v, mark).is_err() && numeric_like(v));
        if unreadable {
            out.unreadable_widths += 1;
        }
        let width = rule.width.or(attr_w).unwrap_or(ins.size.0);
        if !(width > 0.2 && width < 6.0) {
            continue;
        }
        let dir = (ins.rotation.cos(), ins.rotation.sin());
        // opening centre along the block axis: half a width from the insertion point
        let probe = ins.center;
        let mut best: Option<(f64, usize, f64)> = None;
        for (k, w) in walls.iter().enumerate() {
            let d = (w.b.0 - w.a.0, w.b.1 - w.a.1);
            let len = d.0.hypot(d.1);
            if len < 1e-6 {
                continue;
            }
            let wd = (d.0 / len, d.1 / len);
            // an opening wider than the wall does not fit in it
            if (wd.0 * dir.1 - wd.1 * dir.0).abs() > 0.26 || width > len {
                continue;
            }
            let rel = (probe.0 - w.a.0, probe.1 - w.a.1);
            let t = rel.0 * wd.0 + rel.1 * wd.1;
            let lat = (rel.0 * -wd.1 + rel.1 * wd.0).abs();
            // the centre of a door block with a swing arc lies outside the wall — tolerance for the leaf width
            let tol = w.thickness / 2.0 + if kind == "door" { width * 0.6 } else { 0.15 };
            if lat > tol || t < -0.05 || t > len + 0.05 {
                continue;
            }
            // the opening midpoint is the projection of the block extents centre (a leaf with an arc occupies the width)
            let tc = t.clamp(width / 2.0, (len - width / 2.0).max(width / 2.0));
            if best.is_none_or(|b| lat < b.0) {
                best = Some((lat, k, tc));
            }
        }
        let Some((_, k, tc)) = best else { continue };
        let wall = &walls[k];
        let along = (tc - width / 2.0).max(0.0);
        let (sill, height) = match rule.kind {
            RuleKind::Door => (rule.sill.unwrap_or(0.0), rule.height.unwrap_or(door_h)),
            _ => (rule.sill.unwrap_or(win_sill), rule.height.unwrap_or(win_h)),
        };
        if let Some(o) = openings
            .iter_mut()
            .find(|o| o.wall == wall.id && o.along < along + width && along < o.along + o.width)
        {
            o.kind = kind.into();
            o.family = rule.family.clone();
            o.confidence = if unreadable {
                out.review.push(o.id);
                o.confidence.min(UNREADABLE_WIDTH_CONFIDENCE)
            } else {
                o.confidence.max(0.97)
            };
            if rule.width.is_some() || attr_w.is_some() {
                o.width = width;
                o.along = along;
            }
            if rule.height.is_some() || rule.sill.is_some() || o.sill != sill {
                o.sill = sill;
                o.height = height;
            }
            continue;
        }
        let id = hash_key(2, &[wall.id as i64, q(along), q(width)], kind);
        if mem.contains(id) {
            continue;
        }
        openings.push(OpeningCand {
            id,
            wall: wall.id,
            kind: kind.into(),
            along,
            width,
            sill,
            height,
            swing_side: false,
            swing_hinge: false,
            confidence: if unreadable {
                UNREADABLE_WIDTH_CONFIDENCE
            } else {
                0.9
            },
            family: rule.family.clone(),
        });
        if unreadable {
            out.review.push(id);
        }
        out.added += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_and_rules_text() {
        assert!(glob("*door*", "M_Door_900"));
        assert!(glob(crate::ru_fixtures::ru("dm_glob_pat"), crate::ru_fixtures::ru("dm_glob_name")));
        assert!(!glob("win*", "door"));
        assert!(glob("*", ""));
        let mut r = BlockRules::default();
        r.0.insert(
            0,
            BlockRule {
                pattern: crate::ru_fixtures::ru("dm_rule_pat").into(),
                kind: RuleKind::Door,
                family: crate::ru_fixtures::ru("dm_family").into(),
                width: Some(0.9),
                height: Some(2.1),
                sill: None,
            },
        );
        let back = BlockRules::from_text(&r.to_text());
        assert_eq!(back, r);
        assert_eq!(
            back.find(crate::ru_fixtures::ru("dm_rule_find")).unwrap().family,
            crate::ru_fixtures::ru("dm_family")
        );
        assert_eq!(back.find("chair"), None);
    }

    #[test]
    fn rect_normalized() {
        let (c, rot, b, h) = as_rect(&[(0.0, 0.0), (0.0, 0.4), (-0.6, 0.4), (-0.6, 0.0)]).unwrap();
        assert!((c.0 + 0.3).abs() < 1e-12 && (c.1 - 0.2).abs() < 1e-12);
        assert_eq!(rot, 0.0);
        assert!(
            ((b - 0.6).abs() < 1e-12) && ((h - 0.4).abs() < 1e-12),
            "{b} {h}"
        );
        assert!(as_rect(&[(0.0, 0.0), (1.0, 0.0), (1.2, 1.0), (0.0, 1.0)]).is_none());
    }

    #[test]
    fn area_text_table() {
        for t in ["1,5", "1,250 m2", "1,234.5 m2", "1.234,5", "1 250,5 m2", "12.5"] {
            assert!(area_like(t, None), "{t:?}");
        }
        for t in ["12", "101", "Office", "1,2,3", "Hall m2"] {
            assert!(!area_like(t, None), "{t:?}");
        }
        // with the drawing's comma mark `1.5` is not a number, so not area text
        assert!(!area_like("1.5", Some(Mark::Comma)));
        // widths: never `1.234.5`, an ambiguous width falls back to the block size
        assert_eq!(parse_num("1 200", None).map(|n| n.value), Ok(1200.0));
        assert_eq!(parse_num("0,9", None).map(|n| n.value), Ok(0.9));
        assert!(parse_num("1,200", None).is_err());
    }
}
