//! Grids and beams by picking drawing lines (the same pick mode as walls, `Mode::PickLine`).
//! Grid: a click on a line makes a grid exactly between its ends, named after the series
//! (letters for horizontal, numbers for vertical); `Tab` before the click takes every parallel
//! line of the same drawing layer at once. Beam: a beam of the current type on the active level
//! along the line; `Tab` takes the connected chain. One pick or window is one `apply_ext`,
//! so one undo step; a refused pick writes nothing and says why.

use std::sync::Arc;

use ddc_geom::{v2, V2};
use ddc_model::Model;
use ddc_ui::i18n::Lang;

use crate::editor::Editor;
use crate::shell::EditorApp;
use crate::tools::{self, Kind};
use crate::wall_pick::{along, chain, nearest, JOIN, ON_AXIS};
use crate::wall_tool::{Event, Key, Mode, WallTool, MIN_LEN};

/// Command ids.
pub const GRID_CMD: &str = "arch.grid_pick";
pub const BEAM_CMD: &str = "struct.beam_pick";
/// Largest sine of the angle between two lines of one parallel set.
pub const PARALLEL: f64 = 1e-4;

/// What the pick mode creates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LineKind {
    #[default]
    Wall,
    Grid,
    Beam,
}

/// Pick data of grids and beams, kept in the wall tool.
#[derive(Clone, Debug, Default)]
pub struct State {
    pub kind: LineKind,
    /// Drawing layer of each line of `underlay_segs` (same order).
    pub layers: Arc<Vec<u32>>,
    /// Axes that already exist (name or id, ends): grids, or beams of the level.
    pub taken: Vec<(String, V2, V2)>,
    /// Letter series of grids in the Cyrillic alphabet.
    pub cyr: bool,
    /// Member type of the beams to place.
    pub beam_type: String,
    /// The next pick takes the connected chain (`Tab`).
    pub pick_chain: bool,
    key: Option<(u64, i64, LineKind)>,
}

pub fn register(app: &mut EditorApp) {
    app.register_command(GRID_CMD, start_grid);
    app.register_command(BEAM_CMD, start_beam);
}

fn start_grid(app: &mut EditorApp) {
    let cyr = app.sh.cat.lang == Lang::Ru;
    start_on(&mut app.ed, LineKind::Grid, cyr);
}

fn start_beam(app: &mut EditorApp) {
    start_on(&mut app.ed, LineKind::Beam, false);
}

/// Enters the pick mode for `kind` (`Grid` or `Beam`).
pub fn start_on(ed: &mut Editor, kind: LineKind, cyr: bool) {
    let layers = ed.tool.lp.layers.clone();
    ed.set_mode(Mode::PickLine);
    ed.tool.lp = State {
        kind,
        layers,
        cyr,
        ..Default::default()
    };
    refresh(ed);
    ed.status = Some(match kind {
        LineKind::Beam => crate::tr("line_pick.st.start_beam", &[("type", &ed.tool.lp.beam_type)]),
        _ => crate::tr("line_pick.st.start_grid", &[]),
    });
}

/// Rereads the existing axes and the beam type when the model or the level changed.
pub fn refresh(ed: &mut Editor) {
    let kind = ed.tool.lp.kind;
    if kind == LineKind::Wall {
        return;
    }
    let key = Some((ed.revision, ed.level, kind));
    if ed.tool.lp.key == key {
        return;
    }
    let taken: Vec<(String, V2, V2)> = if kind == LineKind::Grid {
        ed.model
            .grids()
            .unwrap_or_default()
            .into_iter()
            .filter(|g| g.spec.bulge == 0.0)
            .map(|g| {
                let (a, b) = (g.spec.a, g.spec.b);
                (g.name, v2(a.0, a.1), v2(b.0, b.1))
            })
            .collect()
    } else {
        beam_ids(&ed.model, ed.level)
            .into_iter()
            .filter_map(|id| ed.model.beam(id).ok().map(|b| (id, b)))
            .filter(|(_, b)| b.bulge == 0.0)
            .map(|(id, b)| (id.to_string(), v2(b.a.0, b.a.1), v2(b.b.0, b.b.1)))
            .collect()
    };
    let ty = (kind == LineKind::Beam).then(|| beam_type(&ed.model));
    let st = &mut ed.tool.lp;
    st.key = key;
    st.taken = taken;
    if let Some(ty) = ty {
        st.beam_type = ty;
    }
}

fn beam_ids(m: &Model, level: i64) -> Vec<i64> {
    let Ok(mut st) = m.sql().prepare("SELECT id FROM beam WHERE level=?1") else {
        return vec![];
    };
    st.query_map([level], |r| r.get(0))
        .map(|it| it.flatten().collect())
        .unwrap_or_default()
}

/// Type of the newest beam of the model, else the first catalog beam section.
fn beam_type(m: &Model) -> String {
    m.sql()
        .query_row(
            "SELECT t.name FROM beam b JOIN member_type t ON t.id=b.mtype \
             ORDER BY b.id DESC LIMIT 1",
            [],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .or_else(|| tools::beam_catalog_names(Kind::Beam).into_iter().next())
        .unwrap_or_default()
}

/// Status-bar hint (`hint.<tool>.<step>`).
pub fn hint(t: &WallTool) -> (&'static str, &'static str) {
    let step = if t.lp.pick_chain { "more" } else { "one" };
    match t.lp.kind {
        LineKind::Wall => ("wall_pick", crate::wall_pick::step(t)),
        LineKind::Grid => ("grid_pick", step),
        LineKind::Beam => ("beam_pick", step),
    }
}

/// Command of the running pick mode (ribbon highlight).
pub fn command(t: &WallTool) -> &'static str {
    match t.lp.kind {
        LineKind::Wall => crate::wall_pick::CMD,
        LineKind::Grid => GRID_CMD,
        LineKind::Beam => BEAM_CMD,
    }
}

/// Events of the grid and beam pick modes.
pub(crate) fn handle(ed: &mut Editor, e: Event) {
    refresh(ed);
    match e {
        Event::Move(p) | Event::Press(p) | Event::Release(p) => ed.tool.track_in(ed.ix.draft_mut(), p),
        Event::Click(p) => {
            ed.tool.track_in(ed.ix.draft_mut(), p);
            pick(ed, p);
        }
        Event::Key(Key::Tab) => ed.tool.lp.pick_chain = !ed.tool.lp.pick_chain,
        Event::Key(Key::Escape) if ed.tool.lp.pick_chain => ed.tool.lp.pick_chain = false,
        Event::Key(Key::Escape) => ed.set_mode(Mode::Select),
        Event::Char(_) | Event::Key(_) => {}
    }
}

/// Creates what a click at `p` picks, or reports why not; nothing is written on a refusal.
fn pick(ed: &mut Editor, p: V2) {
    let lines = match plan(&ed.tool, p) {
        Ok(l) => l,
        Err(m) => {
            ed.status = Some(m);
            return;
        }
    };
    let n = lines.len();
    let (rec, done) = match ed.tool.lp.kind {
        LineKind::Beam => {
            let ty = ed.tool.lp.beam_type.clone();
            if ty.is_empty() {
                ed.status = Some(crate::tr("line_pick.st.no_beam_type", &[]));
                return;
            }
            let done = if n == 1 {
                crate::tr("line_pick.st.beam_done", &[("type", &ty)])
            } else {
                crate::tr("line_pick.st.beams_done", &[("n", &n.to_string()), ("type", &ty)])
            };
            (tools::beam_pick_rec(ed.level, tools::beam_tool_z(ed.height), ty, lines), done)
        }
        _ => {
            let have: Vec<String> = ed.tool.lp.taken.iter().map(|t| t.0.clone()).collect();
            let names = tools::grid_pick_names(&have, &lines, ed.tool.lp.cyr);
            let done = match names.as_slice() {
                [one] => crate::tr("line_pick.st.grid_done", &[("name", one)]),
                [first, .., last] => crate::tr(
                    "line_pick.st.grids_done",
                    &[("n", &n.to_string()), ("first", first), ("last", last)],
                ),
                [] => String::new(),
            };
            (tools::grid_pick_rec(lines, ed.tool.lp.cyr), done)
        }
    };
    let top = ed.model.op_top().ok();
    ed.status = Some(match ed.apply_ext(rec) {
        Ok(()) => {
            ed.tool.lp.pick_chain = false;
            done
        }
        // committed (the op and its undo step exist), only the reload failed: not a failure
        Err(e) if top.is_some() && ed.model.op_top().ok() != top => {
            ed.tool.lp.pick_chain = false;
            crate::tr("line_pick.st.reload_warn", &[("err", &e.to_string())])
        }
        Err(e) => crate::tr("line_pick.st.failed", &[("err", &e.to_string())]),
    });
    refresh(ed);
}

/// Ghost lines the click at the cursor would create (empty when it would be refused).
pub fn preview(t: &WallTool) -> Vec<(V2, V2)> {
    plan(t, t.cursor).unwrap_or_default()
}

/// Lines a pick at `p` creates, in creation order, or the translated reason for refusing it.
pub fn plan(t: &WallTool, p: V2) -> Result<Vec<(V2, V2)>, String> {
    let segs: &[(V2, V2)] = &t.underlay_segs;
    if segs.is_empty() {
        return Err(crate::tr("wall_pick.st.no_lines", &[]));
    }
    let Some(seed) = nearest(segs, p, t.radius) else {
        return Err(crate::tr("wall_pick.st.miss", &[]));
    };
    match t.lp.kind {
        LineKind::Grid => plan_grid(t, seed),
        LineKind::Beam => plan_beam(t, seed),
        LineKind::Wall => Ok(vec![]),
    }
}

/// The existing axis lying along `l`, if any.
fn taken(list: &[(String, V2, V2)], l: (V2, V2)) -> Option<&str> {
    list.iter()
        .find(|(_, a, b)| along(*a, *b, l))
        .map(|(n, _, _)| n.as_str())
}

/// Ends ordered left to right for horizontal grids, bottom to top for vertical ones.
fn orient((a, b): (V2, V2), horizontal: bool) -> (V2, V2) {
    let flip = if horizontal { b.x < a.x } else { b.y < a.y };
    if flip {
        (b, a)
    } else {
        (a, b)
    }
}

fn plan_beam(t: &WallTool, seed: usize) -> Result<Vec<(V2, V2)>, String> {
    let segs = &t.underlay_segs;
    let lines = if t.lp.pick_chain {
        chain(segs, seed).0
    } else {
        vec![segs[seed]]
    };
    for &l in &lines {
        if let Some(id) = taken(&t.lp.taken, l) {
            return Err(crate::tr("line_pick.st.on_beam", &[("id", id)]));
        }
    }
    Ok(lines)
}

/// One axis of a parallel set: offset across, extent along (start, end) and the end points.
struct Axis {
    off: f64,
    t0: f64,
    t1: f64,
    p0: V2,
    p1: V2,
    clicked: bool,
}

fn plan_grid(t: &WallTool, seed: usize) -> Result<Vec<(V2, V2)>, String> {
    let segs = &t.underlay_segs;
    let (s0, s1) = segs[seed];
    let horizontal = tools::is_horizontal((s0.x, s0.y), (s1.x, s1.y));
    let on_grid = |name: &str| crate::tr("line_pick.st.on_grid", &[("name", name)]);
    if !t.lp.pick_chain {
        let l = orient(segs[seed], horizontal);
        return match taken(&t.lp.taken, l) {
            Some(n) => Err(on_grid(n)),
            None => Ok(vec![l]),
        };
    }
    if t.lp.layers.len() != segs.len() {
        return Err(crate::tr("line_pick.st.no_layer", &[]));
    }
    let layer = t.lp.layers[seed];
    let (a0, b0) = orient(segs[seed], horizontal);
    let u = (b0 - a0) * (1.0 / (b0 - a0).len());
    // across: up for horizontal axes, right for vertical ones, so names ascend
    let n = if horizontal {
        v2(-u.y, u.x)
    } else {
        v2(u.y, -u.x)
    };
    let mut cand: Vec<Axis> = vec![];
    for (i, &(a, b)) in segs.iter().enumerate() {
        let d = b - a;
        let len = d.len();
        if t.lp.layers[i] != layer || len < MIN_LEN || u.cross(d).abs() > PARALLEL * len {
            continue;
        }
        let (ta, tb) = (a.dot(u), b.dot(u));
        let (p0, p1, t0, t1) = if ta <= tb {
            (a, b, ta, tb)
        } else {
            (b, a, tb, ta)
        };
        cand.push(Axis {
            off: (a.dot(n) + b.dot(n)) / 2.0,
            t0,
            t1,
            p0,
            p1,
            clicked: i == seed,
        });
    }
    cand.sort_by(|x, y| x.off.total_cmp(&y.off).then(x.t0.total_cmp(&y.t0)));
    // collinear lines that overlap or touch are one axis (duplicates, split lines)
    let mut axes: Vec<Axis> = vec![];
    for c in cand {
        match axes.last_mut() {
            Some(g) if (c.off - g.off).abs() <= ON_AXIS && c.t0 <= g.t1 + JOIN => {
                if c.t0 < g.t0 {
                    (g.t0, g.p0) = (c.t0, c.p0);
                }
                if c.t1 > g.t1 {
                    (g.t1, g.p1) = (c.t1, c.p1);
                }
                g.clicked |= c.clicked;
            }
            _ => axes.push(c),
        }
    }
    let mut out = vec![];
    for g in axes {
        match taken(&t.lp.taken, (g.p0, g.p1)) {
            Some(name) if g.clicked => return Err(on_grid(name)),
            Some(_) => {}
            None => out.push((g.p0, g.p1)),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    type Line = ((f64, f64), (f64, f64), u32);

    fn close(a: V2, b: V2) {
        assert!((a - b).len() < 1e-12, "{a:?} != {b:?}");
    }

    fn set_lines(e: &mut Editor, lines: &[Line]) {
        let seg = |l: &Line| (v2(l.0 .0, l.0 .1), v2(l.1 .0, l.1 .1));
        e.tool.underlay_segs = Arc::new(lines.iter().map(seg).collect());
        e.tool.lp.layers = Arc::new(lines.iter().map(|l| l.2).collect());
    }

    /// Editor in the pick mode of `kind` over `lines` (each with a drawing layer id).
    fn editor(kind: LineKind, lines: &[Line]) -> Editor {
        let mut e = Editor::new_in_memory().unwrap();
        start_on(&mut e, kind, false);
        set_lines(&mut e, lines);
        e
    }

    fn grids(e: &Editor) -> Vec<(String, V2, V2)> {
        let mut g: Vec<_> = e
            .model
            .grids()
            .unwrap()
            .into_iter()
            .map(|g| {
                let (a, b) = (g.spec.a, g.spec.b);
                (g.name, v2(a.0, a.1), v2(b.0, b.1))
            })
            .collect();
        g.sort_by(|x, y| x.0.cmp(&y.0));
        g
    }

    /// (level, start, end, z) of each beam, oldest first.
    fn beams(e: &Editor) -> Vec<(i64, V2, V2, f64)> {
        let mut st = e
            .model
            .sql()
            .prepare("SELECT level,ax,ay,bx,by,az FROM beam ORDER BY id")
            .unwrap();
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    v2(r.get(1)?, r.get(2)?),
                    v2(r.get(3)?, r.get(4)?),
                    r.get(5)?,
                ))
            })
            .unwrap();
        rows.map(|x| x.unwrap()).collect()
    }

    fn first_beam_id(e: &Editor) -> String {
        let id: i64 = e
            .model
            .sql()
            .query_row("SELECT min(id) FROM beam", [], |r| r.get(0))
            .unwrap();
        id.to_string()
    }

    #[test]
    fn grid_pick_follows_line_and_names() {
        let mut e = editor(
            LineKind::Grid,
            &[
                ((0.0, 0.0), (12.0, 0.0), 1),
                ((0.0, 6.0), (12.0, 6.0), 1),
                ((3.0, -1.0), (3.0, 7.0), 1),
                ((9.0, 7.0), (9.0, -1.0), 1),
            ],
        );
        let before = e.can_undo();
        // off the line by a few centimetres: the grid still runs exactly along the line
        e.handle(Event::Click(v2(5.0, 0.05)));
        assert_eq!(grids(&e), vec![("A".to_string(), v2(0.0, 0.0), v2(12.0, 0.0))]);
        e.handle(Event::Click(v2(3.04, 3.0)));
        // a vertical line drawn top to bottom is stored bottom to top
        e.handle(Event::Click(v2(9.0, 3.0)));
        e.handle(Event::Click(v2(5.0, 6.02)));
        let g = grids(&e);
        let names: Vec<&str> = g.iter().map(|x| x.0.as_str()).collect();
        assert_eq!(names, ["1", "2", "A", "B"], "{:?}", e.status);
        close(g[0].1, v2(3.0, -1.0));
        close(g[0].2, v2(3.0, 7.0));
        close(g[1].1, v2(9.0, -1.0));
        close(g[1].2, v2(9.0, 7.0));
        close(g[3].1, v2(0.0, 6.0));
        close(g[3].2, v2(12.0, 6.0));
        assert_eq!(e.wall_mode(), Mode::PickLine, "the tool stays for the next pick");
        // each click is one undo step
        e.undo();
        assert_eq!(grids(&e).len(), 3);
        for _ in 0..3 {
            e.undo();
        }
        assert!(grids(&e).is_empty());
        assert_eq!(e.can_undo(), before);
        // the ghost is the line under the cursor
        e.handle(Event::Move(v2(5.0, 0.05)));
        assert_eq!(e.tool.preview_in(e.wall_draft()), vec![(v2(0.0, 0.0), v2(12.0, 0.0))]);
        e.handle(Event::Move(v2(5.0, 3.0)));
        assert!(e.tool.preview_in(e.wall_draft()).is_empty());
        // the Cyrillic series starts with its own first letter
        let mut r = editor(LineKind::Grid, &[((0.0, 0.0), (5.0, 0.0), 1)]);
        start_on(&mut r, LineKind::Grid, true);
        r.handle(Event::Click(v2(1.0, 0.0)));
        assert_eq!(grids(&r)[0].0, "\u{410}");
    }

    #[test]
    fn grid_pick_parallel_set_one_undo() {
        // four vertical axes in shuffled order (one drawn twice, one split in two), a
        // horizontal line and a vertical line of another layer
        let mut e = editor(
            LineKind::Grid,
            &[
                ((9.0, 8.0), (9.0, 0.0), 5),
                ((0.0, 0.0), (0.0, 8.0), 5),
                ((6.0, 0.0), (6.0, 4.0), 5),
                ((6.0, 4.0), (6.0, 8.0), 5),
                ((3.0, 0.0), (3.0, 8.0), 5),
                ((3.0, 0.0), (3.0, 8.0), 5),
                ((0.0, 9.0), (9.0, 9.0), 5),
                ((4.5, 0.0), (4.5, 8.0), 6),
            ],
        );
        let before = e.can_undo();
        e.handle(Event::Key(Key::Tab));
        assert!(e.tool.lp.pick_chain);
        e.handle(Event::Move(v2(3.0, 4.0)));
        assert_eq!(e.tool.preview_in(e.wall_draft()).len(), 4, "the ghost shows the whole set");
        e.handle(Event::Click(v2(3.0, 4.0)));
        let g = grids(&e);
        assert_eq!(g.len(), 4, "{:?}", e.status);
        let at = |n: &str| g.iter().find(|x| x.0 == n).unwrap().clone();
        // numbered left to right, ends exactly those of the drawing lines
        close(at("1").1, v2(0.0, 0.0));
        close(at("1").2, v2(0.0, 8.0));
        close(at("2").1, v2(3.0, 0.0));
        close(at("2").2, v2(3.0, 8.0));
        close(at("3").1, v2(6.0, 0.0));
        close(at("3").2, v2(6.0, 8.0));
        close(at("4").1, v2(9.0, 0.0));
        close(at("4").2, v2(9.0, 8.0));
        assert!(!e.tool.lp.pick_chain, "Tab arms one pick");
        // one window, one undo step
        e.undo();
        assert!(grids(&e).is_empty());
        assert_eq!(e.can_undo(), before);
        e.redo();
        assert_eq!(grids(&e).len(), 4);
        // picking the set again: the clicked line has its grid, nothing is added
        e.handle(Event::Key(Key::Tab));
        e.handle(Event::Click(v2(0.0, 4.0)));
        assert_eq!(grids(&e).len(), 4);
        // a set without layer data is refused, not guessed
        let mut t = WallTool::new();
        t.set_mode(Mode::PickLine);
        t.lp.kind = LineKind::Grid;
        t.lp.pick_chain = true;
        t.underlay_segs = Arc::new(vec![(v2(0.0, 0.0), v2(0.0, 5.0))]);
        assert!(plan(&t, v2(0.0, 1.0)).is_err());
    }

    #[test]
    fn grid_pick_set_skips_existing_axes() {
        let mut e = editor(
            LineKind::Grid,
            &[
                ((0.0, 0.0), (0.0, 8.0), 1),
                ((4.0, 0.0), (4.0, 8.0), 1),
                ((8.0, 0.0), (8.0, 8.0), 1),
            ],
        );
        e.handle(Event::Click(v2(4.0, 3.0)));
        assert_eq!(grids(&e).len(), 1);
        // the clicked line has no grid: the set adds the other two, not a second one at x=4
        e.handle(Event::Key(Key::Tab));
        e.handle(Event::Click(v2(0.0, 3.0)));
        let g = grids(&e);
        assert_eq!(g.len(), 3, "{:?}", e.status);
        assert_eq!(g.iter().filter(|x| x.1.x == 4.0).count(), 1);
    }

    #[test]
    fn pick_on_existing_grid_refused() {
        let mut e = editor(
            LineKind::Grid,
            &[((0.0, 0.0), (6.0, 0.0), 1), ((0.0, 3.0), (6.0, 3.0), 1)],
        );
        e.handle(Event::Click(v2(3.0, 0.0)));
        assert_eq!(grids(&e).len(), 1);
        let undo = e.can_undo();
        e.handle(Event::Click(v2(3.0, 0.02)));
        assert_eq!(grids(&e).len(), 1);
        assert_eq!(e.can_undo(), undo, "a refusal is not an undo step");
        let m = e.status.clone().unwrap_or_default();
        assert!(m.contains('A'), "the refusal names the grid: {m}");
        // the whole set is refused when the clicked line already has its grid
        e.handle(Event::Key(Key::Tab));
        e.handle(Event::Click(v2(3.0, 0.0)));
        assert_eq!(grids(&e).len(), 1);
        assert!(e.tool.lp.pick_chain, "a refused pick keeps the armed set");
        // a miss and an empty drawing report and write nothing
        e.status = None;
        e.handle(Event::Click(v2(3.0, 1.5)));
        assert!(e.status.is_some());
        assert_eq!(e.can_undo(), undo);
        let mut n = editor(LineKind::Grid, &[]);
        n.status = None;
        n.handle(Event::Click(v2(0.0, 0.0)));
        assert!(n.status.is_some());
        assert!(grids(&n).is_empty());
        // Esc disarms the set first, then leaves the tool
        e.handle(Event::Key(Key::Escape));
        assert!(!e.tool.lp.pick_chain);
        assert_eq!(e.wall_mode(), Mode::PickLine);
        e.handle(Event::Key(Key::Escape));
        assert_eq!(e.wall_mode(), Mode::Select);
    }

    #[test]
    fn beam_pick_on_active_level() {
        let lines: [Line; 3] = [
            ((0.0, 0.0), (6.0, 0.0), 1),
            ((6.0, 0.0), (6.0, 4.0), 1),
            ((6.0, 4.0), (0.0, 4.0), 1),
        ];
        let mut e = editor(LineKind::Beam, &lines);
        let l2 = e.add_level("Second", 3.0).unwrap();
        assert_eq!(e.level, l2);
        set_lines(&mut e, &lines);
        e.handle(Event::Move(v2(3.0, 0.0)));
        assert!(!e.tool.lp.beam_type.is_empty(), "the first catalog section is the default");
        let before = e.can_undo();
        e.handle(Event::Click(v2(3.0, 0.05)));
        let b = beams(&e);
        assert_eq!(b.len(), 1, "{:?}", e.status);
        assert_eq!(b[0].0, l2, "on the active level");
        close(b[0].1, v2(0.0, 0.0));
        close(b[0].2, v2(6.0, 0.0));
        assert!((b[0].3 - tools::beam_tool_z(e.height)).abs() < 1e-12);
        assert!(e.can_undo());
        e.undo();
        assert!(beams(&e).is_empty());
        assert_eq!(e.can_undo(), before);
        // the chain is one commit, one undo step
        e.handle(Event::Key(Key::Tab));
        e.handle(Event::Click(v2(6.0, 2.0)));
        assert_eq!(beams(&e).len(), 3, "{:?}", e.status);
        assert!(beams(&e).iter().all(|b| b.0 == l2));
        e.undo();
        assert!(beams(&e).is_empty());
        assert_eq!(e.can_undo(), before);
        e.redo();
        assert_eq!(beams(&e).len(), 3);
    }

    #[test]
    fn beam_pick_uses_beam_tool_z() {
        let lines: [Line; 1] = [((0.0, 0.0), (6.0, 0.0), 1)];
        let mut e = editor(LineKind::Beam, &lines);
        e.height = 3.4;
        e.handle(Event::Move(v2(3.0, 0.0)));
        e.handle(Event::Click(v2(3.0, 0.05)));
        let b = beams(&e);
        assert_eq!(b.len(), 1, "{:?}", e.status);
        assert!((b[0].3 - tools::beam_tool_z(3.4)).abs() < 1e-12);
    }

    #[test]
    fn line_pick_reload_fail_is_warning() {
        let lines: [Line; 1] = [((0.0, 0.0), (6.0, 0.0), 1)];
        let mut e = editor(LineKind::Beam, &lines);
        e.handle(Event::Move(v2(3.0, 0.0)));
        // the reload reads this table: the op commits, the reload fails
        e.model
            .sql()
            .execute_batch("ALTER TABLE wall_type_layer RENAME TO zz_layer")
            .unwrap();
        e.handle(Event::Click(v2(3.0, 0.05)));
        assert_eq!(beams(&e).len(), 1, "{:?}", e.status);
        let st = e.status.clone().unwrap_or_default();
        // the status follows the shared UI flag, which other tests switch: accept either language
        let head = |k: &str, ru: bool| crate::tr_lang(ru, k, &[("err", "")]).trim_end().to_string();
        assert!([false, true].iter().any(|&ru| st.starts_with(&head("line_pick.st.reload_warn", ru))), "{st}");
        assert!(![false, true].iter().any(|&ru| st.starts_with(&head("line_pick.st.failed", ru))), "{st}");
        assert!(e.can_undo(), "the committed op keeps its undo step");
    }

    #[test]
    fn pick_on_existing_beam_refused() {
        let mut e = editor(
            LineKind::Beam,
            &[
                ((0.0, 0.0), (6.0, 0.0), 1),
                ((0.0, 3.0), (6.0, 3.0), 1),
                ((6.0, 0.0), (6.0, 3.0), 1),
            ],
        );
        e.handle(Event::Click(v2(3.0, 0.0)));
        assert_eq!(beams(&e).len(), 1);
        let undo = e.can_undo();
        e.handle(Event::Click(v2(3.0, 0.02)));
        assert_eq!(beams(&e).len(), 1);
        assert_eq!(e.can_undo(), undo, "a refusal is not an undo step");
        let m = e.status.clone().unwrap_or_default();
        assert!(m.contains(&first_beam_id(&e)), "the refusal names the beam: {m}");
        // a chain through a taken line is refused whole
        e.handle(Event::Key(Key::Tab));
        e.handle(Event::Click(v2(6.0, 1.5)));
        assert_eq!(beams(&e).len(), 1);
        assert!(e.tool.lp.pick_chain);
        // the next beam keeps the type of the last one and a miss writes nothing
        e.handle(Event::Key(Key::Tab));
        e.handle(Event::Click(v2(3.0, 3.0)));
        assert_eq!(beams(&e).len(), 2);
        e.handle(Event::Click(v2(3.0, 1.5)));
        assert_eq!(beams(&e).len(), 2);
    }

    #[test]
    fn wall_pick_still_walls_after_grid_pick() {
        let mut e = editor(LineKind::Grid, &[((0.0, 0.0), (4.0, 0.0), 1)]);
        crate::wall_pick::start_on(&mut e);
        assert_eq!(e.tool.lp.kind, LineKind::Wall);
        let segs = e.tool.underlay_segs.clone();
        crate::wall_pick::sync(&mut e, segs);
        e.handle(Event::Click(v2(2.0, 0.0)));
        assert_eq!(e.walls.len(), 1, "{:?}", e.status);
        assert!(grids(&e).is_empty());
    }
}
