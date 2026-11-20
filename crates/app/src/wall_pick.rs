// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Walls by picking drawing lines: a click near a visible drawing line of the level makes a
//! wall on it. Location: axis on the line, or face on the line toward the cursor (`F`).
//! `Tab` before the click takes the connected chain (ends within [`JOIN`]). One pick is one
//! `Outcome::Commit`, so one undo step; a curtain-wall session uses the same commit path.

use std::collections::HashMap;
use std::sync::Arc;

use ddc_geom::{v2, V2};

use crate::editor::Editor;
use crate::shell::EditorApp;
use crate::snap::closest_on_segment;
use crate::wall_tool::{Event, Key, Mode, Outcome, WallDraft, WallTool, MIN_LEN};

/// Command id.
pub const CMD: &str = "arch.wall_pick";
/// Line ends closer than this belong to one chain, m.
pub const JOIN: f64 = 0.001;
/// A line this close to an existing wall axis lies under it, m.
pub const ON_AXIS: f64 = 0.001;

pub fn register(app: &mut EditorApp) {
    app.register_command(CMD, start);
}

fn start(app: &mut EditorApp) {
    start_on(&mut app.ed);
}

/// Enters the pick mode; a running curtain-wall session stays a curtain-wall session.
pub fn start_on(ed: &mut Editor) {
    let curtain = ed.tool.curtain;
    ed.set_mode(Mode::PickLine);
    ed.tool.lp.kind = crate::line_pick::LineKind::Wall;
    ed.tool.curtain = curtain;
    refresh(ed);
    ed.status = Some(crate::tr("wall_pick.st.start", &[]));
}

/// Per-frame update from the plan view: visible drawing lines, then pick-mode data.
pub fn sync(ed: &mut Editor, segs: Arc<Vec<(V2, V2)>>) {
    ed.tool.underlay_segs = segs;
    if ed.wall_mode() == Mode::PickLine {
        refresh(ed);
    }
}

/// Half thickness of the wall to place and the ids of the level's walls (`tool.walls` order).
pub fn refresh(ed: &mut Editor) {
    ed.tool.half = if ed.tool.curtain {
        ed.cw_defaults.mullion.1 / 2.0
    } else {
        ed.current_type().map_or(0.0, |t| t.thickness() / 2.0)
    };
    if !ed.walls.iter().map(|w| w.id).eq(ed.tool.wall_ids.iter().copied()) {
        ed.tool.wall_ids = ed.walls.iter().map(|w| w.id).collect();
    }
    crate::line_pick::refresh(ed);
}

/// Status-bar hint step (`hint.wall_pick.<step>`).
pub fn step(t: &WallTool) -> &'static str {
    match (t.pick_face, t.lp.pick_chain) {
        (false, false) => "center",
        (true, false) => "face",
        (false, true) => "center_chain",
        (true, true) => "face_chain",
    }
}

impl WallTool {
    /// Events of [`Mode::PickLine`].
    pub(crate) fn pick_handle(&mut self, e: Event) -> Outcome {
        match e {
            Event::Move(p) | Event::Press(p) | Event::Release(p) => {
                self.track_in(&mut WallDraft::default(), p);
                Outcome::Nothing
            }
            Event::Click(p) => {
                self.track_in(&mut WallDraft::default(), p);
                self.pick_at(p)
            }
            // F, or the same key on the Cyrillic layout
            Event::Char('f' | 'F' | '\u{0430}' | '\u{0410}') => {
                self.pick_face = !self.pick_face;
                Outcome::Nothing
            }
            Event::Key(Key::Tab) => {
                self.lp.pick_chain = !self.lp.pick_chain;
                Outcome::Nothing
            }
            Event::Key(Key::Escape) => {
                self.lp.pick_chain = false;
                Outcome::Nothing
            }
            Event::Char(_) | Event::Key(_) => Outcome::Nothing,
        }
    }

    /// Walls on the line nearest to `p` (or its chain) as one commit, or the refusal.
    pub(crate) fn pick_at(&mut self, p: V2) -> Outcome {
        match plan(self, p) {
            Ok(axes) => {
                self.lp.pick_chain = false;
                Outcome::Commit(axes)
            }
            Err(m) => Outcome::Error(m),
        }
    }
}

/// Ghost axes the click at the cursor would create (empty when it would be refused).
pub fn preview(t: &WallTool) -> Vec<(V2, V2)> {
    if t.lp.kind != crate::line_pick::LineKind::Wall {
        return crate::line_pick::preview(t);
    }
    plan(t, t.cursor).unwrap_or_default()
}

/// Wall axes a pick at `p` creates, or the translated reason for refusing it.
pub fn plan(t: &WallTool, p: V2) -> Result<Vec<(V2, V2)>, String> {
    let segs: &[(V2, V2)] = &t.underlay_segs;
    if segs.is_empty() {
        return Err(crate::tr("wall_pick.st.no_lines", &[]));
    }
    let Some(seed) = nearest(segs, p, t.radius) else {
        return Err(crate::tr("wall_pick.st.miss", &[]));
    };
    let (lines, closed, at) = if t.lp.pick_chain {
        chain(segs, seed)
    } else {
        (vec![segs[seed]], false, 0)
    };
    let axes = if t.pick_face && t.half > 0.0 {
        let (a, b) = lines[at];
        let k = if (b - a).cross(p - a) >= 0.0 {
            t.half
        } else {
            -t.half
        };
        offset(&lines, closed, k).ok_or_else(|| crate::tr("wall_pick.st.tight", &[]))?
    } else {
        lines.clone()
    };
    for (&l, &x) in lines.iter().zip(&axes) {
        if let Some(i) = under_wall(t, l).or_else(|| under_wall(t, x)) {
            let id = match t.wall_ids.get(i) {
                Some(id) if t.wall_ids.len() == t.walls.len() => id.to_string(),
                _ => "?".to_string(),
            };
            return Err(crate::tr("wall_pick.st.on_wall", &[("id", id.as_str())]));
        }
    }
    Ok(axes)
}

/// Nearest non-degenerate line within `r` of `p`.
pub(crate) fn nearest(segs: &[(V2, V2)], p: V2, r: f64) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (i, &(a, b)) in segs.iter().enumerate() {
        if (b - a).len() < MIN_LEN {
            continue;
        }
        let d = (closest_on_segment(p, a, b).0 - p).len();
        if d <= r && best.map_or(true, |(_, bd)| d < bd) {
            best = Some((i, d));
        }
    }
    best.map(|(i, _)| i)
}

/// Index of a wall of the level whose axis runs along `s` for more than [`MIN_LEN`].
fn under_wall(t: &WallTool, s: (V2, V2)) -> Option<usize> {
    t.walls.iter().position(|w| along(w.a, w.b, s))
}

/// `s` lies on the line through `a`-`b` (within [`ON_AXIS`]) and shares more than [`MIN_LEN`]
/// of its length with the segment `a`-`b`.
pub(crate) fn along(a: V2, b: V2, s: (V2, V2)) -> bool {
    let d = b - a;
    let l = d.len();
    if l < MIN_LEN {
        return false;
    }
    let u = d * (1.0 / l);
    let off = |q: V2| (q - a).cross(u).abs();
    if off(s.0) > ON_AXIS || off(s.1) > ON_AXIS {
        return false;
    }
    let (t0, t1) = ((s.0 - a).dot(u), (s.1 - a).dot(u));
    t0.max(t1).min(l) - t0.min(t1).max(0.0) > MIN_LEN
}

type Cell = (i64, i64);

fn cell(p: V2) -> Cell {
    ((p.x / JOIN).floor() as i64, (p.y / JOIN).floor() as i64)
}

fn same(s: (V2, V2), t: (V2, V2)) -> bool {
    let near = |a: V2, b: V2| (a - b).len() <= JOIN;
    (near(s.0, t.0) && near(s.1, t.1)) || (near(s.0, t.1) && near(s.1, t.0))
}

/// The only unused line leaving node `q` (index and far end); none at a free end or a branch.
/// Copies of the line we came by, and repeated copies of the next one, are not branches.
fn next(
    segs: &[(V2, V2)],
    grid: &HashMap<Cell, Vec<usize>>,
    q: V2,
    used: &[bool],
    came: (V2, V2),
) -> Option<(usize, V2)> {
    let c = cell(q);
    let mut found: Option<(usize, V2)> = None;
    for dx in -1..=1 {
        for dy in -1..=1 {
            let Some(ids) = grid.get(&(c.0 + dx, c.1 + dy)) else {
                continue;
            };
            for &i in ids {
                if used[i] || same(segs[i], came) {
                    continue;
                }
                let (a, b) = segs[i];
                let far = if (a - q).len() <= JOIN {
                    b
                } else if (b - q).len() <= JOIN {
                    a
                } else {
                    continue;
                };
                match found {
                    None => found = Some((i, far)),
                    Some((j, f)) if j == i || (f - far).len() <= JOIN => {}
                    Some(_) => return None,
                }
            }
        }
    }
    found
}

/// Connected chain through `seed`, head to tail with shared ends exact; closed flag and the
/// seed's position in the chain.
pub fn chain(segs: &[(V2, V2)], seed: usize) -> (Vec<(V2, V2)>, bool, usize) {
    let mut grid: HashMap<Cell, Vec<usize>> = HashMap::new();
    for (i, &(a, b)) in segs.iter().enumerate() {
        if (b - a).len() < MIN_LEN {
            continue;
        }
        grid.entry(cell(a)).or_default().push(i);
        if cell(b) != cell(a) {
            grid.entry(cell(b)).or_default().push(i);
        }
    }
    let mut used = vec![false; segs.len()];
    used[seed] = true;
    let (a0, b0) = segs[seed];
    let mut fwd = vec![(a0, b0)];
    let mut closed = false;
    while let Some(&last) = fwd.last() {
        let Some((i, far)) = next(segs, &grid, last.1, &used, last) else {
            break;
        };
        used[i] = true;
        if (far - a0).len() <= JOIN {
            fwd.push((last.1, a0));
            closed = true;
            break;
        }
        fwd.push((last.1, far));
    }
    let mut back: Vec<(V2, V2)> = vec![];
    // an open chain also grows back from the seed; each step uses up one line, so it ends
    if !closed {
        loop {
            let first = back.last().copied().unwrap_or((a0, b0));
            let Some((i, far)) = next(segs, &grid, first.0, &used, first) else {
                break;
            };
            used[i] = true;
            back.push((far, first.0));
        }
    }
    let at = back.len();
    back.reverse();
    back.extend(fwd);
    (back, closed, at)
}

/// Head-to-tail `lines` moved by `k` to their left with mitred corners; `None` when a
/// segment flips, turns back or gets shorter than [`MIN_LEN`].
pub fn offset(lines: &[(V2, V2)], closed: bool, k: f64) -> Option<Vec<(V2, V2)>> {
    let n = lines.len();
    if n == 0 {
        return Some(vec![]);
    }
    let moved: Vec<(V2, V2)> = lines
        .iter()
        .map(|&(a, b)| {
            let d = b - a;
            let o = v2(-d.y, d.x) * (k / d.len());
            (a + o, b + o)
        })
        .collect();
    // end of moved line i meets the start of moved line j
    let corner = |i: usize, j: usize| -> Option<V2> {
        let ((p1, q1), (p2, q2)) = (moved[i], moved[j]);
        let (d1, d2) = (q1 - p1, q2 - p2);
        let den = d1.cross(d2);
        if den.abs() <= 1e-9 * d1.len() * d2.len() {
            return (d1.dot(d2) > 0.0).then_some(q1);
        }
        Some(p1 + d1 * ((p2 - p1).cross(d2) / den))
    };
    let mut pts = Vec::with_capacity(n + 1);
    let first = if closed {
        corner(n - 1, 0)?
    } else {
        moved[0].0
    };
    pts.push(first);
    for i in 0..n - 1 {
        pts.push(corner(i, i + 1)?);
    }
    let end = if closed { pts[0] } else { moved[n - 1].1 };
    pts.push(end);
    let out: Vec<(V2, V2)> = pts.windows(2).map(|w| (w[0], w[1])).collect();
    for (o, l) in out.iter().zip(lines) {
        let d = o.1 - o.0;
        if d.len() < MIN_LEN || d.dot(l.1 - l.0) <= 0.0 {
            return None;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snap::SnapWall;

    fn close(a: V2, b: V2) {
        assert!((a - b).len() < 1e-9, "{a:?} != {b:?}");
    }

    fn tool(segs: Vec<(V2, V2)>) -> crate::wall_tool::Rig {
        let mut t = crate::wall_tool::Rig::new();
        t.set_mode(Mode::PickLine);
        t.underlay_segs = Arc::new(segs);
        t.half = 0.1;
        t
    }

    fn editor(segs: Vec<(V2, V2)>) -> Editor {
        let mut e = Editor::new_in_memory().unwrap();
        start_on(&mut e);
        sync(&mut e, Arc::new(segs));
        e
    }

    fn rect(x: f64, y: f64) -> Vec<(V2, V2)> {
        let c = [v2(0.0, 0.0), v2(x, 0.0), v2(x, y), v2(0.0, y)];
        (0..4).map(|i| (c[i], c[(i + 1) % 4])).collect()
    }

    #[test]
    fn pick_segment_makes_wall_on_line() {
        let mut e = editor(vec![(v2(0.0, 0.0), v2(5.0, 0.0)), (v2(0.0, 0.3), v2(5.0, 0.3))]);
        assert_eq!(e.wall_mode(), Mode::PickLine);
        assert!(e.tool.half > 0.0, "half thickness from the wall type");
        // nearer to the first line than to the second: exactly that line, not the cursor
        e.handle(Event::Click(v2(2.5, 0.12)));
        assert_eq!(e.walls.len(), 1, "{:?}", e.status);
        close(e.walls[0].a, v2(0.0, 0.0));
        close(e.walls[0].b, v2(5.0, 0.0));
        assert_eq!(e.wall_mode(), Mode::PickLine, "the tool stays for the next pick");
        let segs = e.tool.underlay_segs.clone();
        sync(&mut e, segs);
        e.handle(Event::Click(v2(2.5, 0.2)));
        assert_eq!(e.walls.len(), 2, "{:?}", e.status);
        close(e.walls[1].a, v2(0.0, 0.3));
        close(e.walls[1].b, v2(5.0, 0.3));
        // the ghost shows the axis under the cursor
        let mut t = tool(vec![(v2(0.0, 0.0), v2(5.0, 0.0))]);
        t.handle(Event::Move(v2(1.0, -0.05)));
        assert_eq!(t.preview(), vec![(v2(0.0, 0.0), v2(5.0, 0.0))]);
        t.handle(Event::Move(v2(1.0, 2.0)));
        assert!(t.preview().is_empty());
    }

    #[test]
    fn face_offsets_half_thickness_to_cursor_side() {
        let mut t = tool(vec![(v2(0.0, 0.0), v2(4.0, 0.0))]);
        t.handle(Event::Char('F'));
        assert!(t.pick_face);
        let Outcome::Commit(s) = t.handle(Event::Click(v2(2.0, 0.05))) else {
            panic!("no commit")
        };
        assert_eq!(s.len(), 1);
        close(s[0].0, v2(0.0, 0.1));
        close(s[0].1, v2(4.0, 0.1));
        let Outcome::Commit(s) = t.handle(Event::Click(v2(2.0, -0.05))) else {
            panic!("no commit")
        };
        close(s[0].0, v2(0.0, -0.1));
        close(s[0].1, v2(4.0, -0.1));
        // a chain keeps one side and mitres the corner
        let mut t = tool(vec![(v2(0.0, 0.0), v2(4.0, 0.0)), (v2(4.0, 3.0), v2(4.0, 0.0))]);
        t.pick_face = true;
        t.handle(Event::Key(Key::Tab));
        let Outcome::Commit(s) = t.handle(Event::Click(v2(2.0, 0.05))) else {
            panic!("no commit")
        };
        assert_eq!(s.len(), 2);
        close(s[0].0, v2(0.0, 0.1));
        close(s[0].1, v2(3.9, 0.1));
        close(s[1].0, v2(3.9, 0.1));
        close(s[1].1, v2(3.9, 3.0));
        // back to the axis; the Cyrillic-layout key toggles too
        t.handle(Event::Char('\u{0430}'));
        assert!(!t.pick_face);
        // face offset that does not fit a short leg is refused, nothing written
        let mut t = tool(vec![
            (v2(0.0, 0.0), v2(4.0, 0.0)),
            (v2(4.0, 0.0), v2(4.0, 0.1)),
            (v2(4.0, 0.1), v2(0.0, 0.1)),
        ]);
        t.pick_face = true;
        t.half = 0.2;
        t.handle(Event::Key(Key::Tab));
        assert!(matches!(
            t.handle(Event::Click(v2(2.0, 0.04))),
            Outcome::Error(_)
        ));
        assert!(t.lp.pick_chain, "a refused pick keeps the armed chain");
        t.pick_face = false;
        assert!(
            matches!(t.handle(Event::Click(v2(2.0, 0.04))), Outcome::Commit(s) if s.len() == 3)
        );
        // editor: offset by the current type's half thickness
        let mut e = editor(vec![(v2(0.0, 0.0), v2(4.0, 0.0))]);
        let half = e.current_type().unwrap().thickness() / 2.0;
        e.handle(Event::Char('f'));
        e.handle(Event::Click(v2(2.0, -0.05)));
        assert_eq!(e.walls.len(), 1, "{:?}", e.status);
        close(e.walls[0].a, v2(0.0, -half));
        close(e.walls[0].b, v2(4.0, -half));
    }

    #[test]
    fn tab_chain_one_commit_one_undo() {
        // closed rectangle with ends off by under 1 mm, a duplicate line and a far stray line
        let mut segs = rect(6.0, 4.0);
        segs[1].0 = v2(6.0004, 0.0003);
        segs.push(segs[2]);
        segs.push((v2(10.0, 0.0), v2(12.0, 0.0)));
        let mut t = tool(segs.clone());
        t.handle(Event::Key(Key::Tab));
        assert!(t.lp.pick_chain);
        let Outcome::Commit(s) = t.handle(Event::Click(v2(3.0, 0.05))) else {
            panic!("no commit")
        };
        assert_eq!(s.len(), 4, "{s:?}");
        for i in 0..4 {
            close(s[i].1, s[(i + 1) % 4].0);
        }
        assert!(!t.lp.pick_chain, "Tab arms one pick");
        // a branch stops the chain
        let mut t = tool(vec![
            (v2(0.0, 0.0), v2(2.0, 0.0)),
            (v2(2.0, 0.0), v2(4.0, 0.0)),
            (v2(2.0, 0.0), v2(2.0, 2.0)),
        ]);
        t.handle(Event::Key(Key::Tab));
        assert!(matches!(t.handle(Event::Click(v2(1.0, 0.0))), Outcome::Commit(s) if s.len() == 1));
        // editor: one gesture, one undo step
        let mut e = editor(segs);
        let before = e.can_undo();
        e.handle(Event::Key(Key::Tab));
        e.handle(Event::Click(v2(6.05, 2.0)));
        assert_eq!(e.walls.len(), 4, "{:?}", e.status);
        e.undo();
        assert_eq!(e.walls.len(), 0);
        assert_eq!(e.can_undo(), before, "the chain was one undo step");
        e.redo();
        assert_eq!(e.walls.len(), 4);
    }

    #[test]
    fn miss_writes_nothing() {
        let mut e = editor(vec![(v2(0.0, 0.0), v2(4.0, 0.0))]);
        let undo = e.can_undo();
        e.handle(Event::Click(v2(2.0, 1.0)));
        assert!(e.walls.is_empty());
        assert_eq!(e.can_undo(), undo);
        assert!(e.status.is_some(), "the miss is reported");
        // no drawing lines at all
        let mut t = tool(vec![]);
        assert!(matches!(
            t.handle(Event::Click(v2(0.0, 0.0))),
            Outcome::Error(_)
        ));
        // degenerate lines are not picked
        let mut t = tool(vec![(v2(1.0, 1.0), v2(1.0, 1.0))]);
        assert!(matches!(
            t.handle(Event::Click(v2(1.0, 1.0))),
            Outcome::Error(_)
        ));
    }

    #[test]
    fn existing_wall_axis_refused() {
        let mut t = tool(vec![(v2(0.0, 0.0), v2(4.0, 0.0)), (v2(0.0, 2.0), v2(4.0, 2.0))]);
        t.walls = vec![SnapWall {
            a: v2(-1.0, 0.0),
            b: v2(2.0, 0.0),
            half: 0.1,
        }];
        t.wall_ids = vec![42];
        let Outcome::Error(m) = t.handle(Event::Click(v2(3.0, 0.05))) else {
            panic!("not refused")
        };
        assert!(m.contains("42"), "{m}");
        // a chain with one line under a wall is refused whole
        let mut t = tool(rect(4.0, 2.0));
        t.walls = vec![SnapWall {
            a: v2(4.0, 0.0),
            b: v2(4.0, 2.0),
            half: 0.1,
        }];
        t.wall_ids = vec![7];
        t.handle(Event::Key(Key::Tab));
        assert!(
            matches!(t.handle(Event::Click(v2(2.0, 0.0))), Outcome::Error(m) if m.contains('7'))
        );
        // a parallel wall a few millimetres off does not block
        t.walls[0].a = v2(4.01, 0.0);
        t.walls[0].b = v2(4.01, 2.0);
        assert!(matches!(
            t.handle(Event::Click(v2(4.0, 1.0))),
            Outcome::Commit(_)
        ));
        // editor: picking the same line twice names the wall and writes nothing
        let mut e = editor(vec![(v2(0.0, 0.0), v2(4.0, 0.0))]);
        e.handle(Event::Click(v2(2.0, 0.0)));
        assert_eq!(e.walls.len(), 1, "{:?}", e.status);
        let id = e.walls[0].id;
        let segs = e.tool.underlay_segs.clone();
        sync(&mut e, segs);
        e.handle(Event::Click(v2(2.0, 0.0)));
        assert_eq!(e.walls.len(), 1);
        let m = e.status.clone().unwrap_or_default();
        assert!(m.contains(&id.to_string()), "{m}");
    }

    #[test]
    fn curtain_session_picks_curtain_walls() {
        let mut e = Editor::new_in_memory().unwrap();
        e.start_curtain();
        start_on(&mut e);
        assert!(e.tool.curtain);
        sync(&mut e, Arc::new(vec![(v2(0.0, 0.0), v2(4.0, 0.0))]));
        e.handle(Event::Click(v2(2.0, 0.02)));
        assert_eq!(e.curtains.len(), 1, "{:?}", e.status);
        assert!(e.walls.is_empty());
    }
}
