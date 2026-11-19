//! Edit constraints: length/distance lock, equal pitch (EQ), alignment, ∥, ⊥ between
//! walls; alignment/lock of a wall to a grid axis; level-pitch lock and wall top bound to a level.
//! Stored in `constraint_rule` (schema 17). Editing a wall axis or grid axis calls
//! [`propagate`]: the edited element is pinned, the other linked lines are moved by ddc-solver;
//! editing a level elevation calls [`propagate_levels`]. If unsatisfiable (including a solution that collapses
//! a wall), `Error::Conflict` lists the constraints and the whole transaction is rolled back.

use std::collections::{BTreeMap, BTreeSet};

use ddc_solver::{Constraint, Rule};
use rusqlite::{params, Connection, OptionalExtension};

use crate::ops::Inv;
use crate::{Error, Model};

#[path = "constraints_dist.rs"]
pub(crate) mod dist;
pub(crate) use dist::{any_dist, relayout};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConstraintKind {
    /// Length of wall `walls[0]` = value.
    LockLength,
    /// Distance between the axes of parallel walls `walls[0]` → `walls[1]` = value (signed).
    LockDist,
    /// Chain of parallel walls: adjacent pitches are equal.
    Equal,
    /// Axis `walls[1]` lies on the line of axis `walls[0]`.
    Align,
    Parallel,
    Perp,
    /// Axis of wall `walls[0]` on the line of grid axis `walls[1]`.
    AlignGrid,
    /// Distance from the axis of wall `walls[0]` to grid axis `walls[1]` = value (signed).
    LockDistGrid,
    /// Level pitch: elevation of `walls[1]` − elevation of `walls[0]` = value.
    LevelGap,
    /// Top of wall `walls[0]` at the elevation of level `walls[1]` (height follows the level).
    TopToLevel,
}

/// Rule participant: a wall, grid axis or level (id in its own table).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Ref {
    Wall(i64),
    Grid(i64),
    Level(i64),
}

impl ConstraintKind {
    pub fn code(self) -> &'static str {
        match self {
            ConstraintKind::LockLength => "lock_len",
            ConstraintKind::LockDist => "lock_dist",
            ConstraintKind::Equal => "eq",
            ConstraintKind::Align => "align",
            ConstraintKind::Parallel => "parallel",
            ConstraintKind::Perp => "perp",
            ConstraintKind::AlignGrid => "align_grid",
            ConstraintKind::LockDistGrid => "lock_dist_grid",
            ConstraintKind::LevelGap => "level_gap",
            ConstraintKind::TopToLevel => "top_level",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [
            ConstraintKind::LockLength,
            ConstraintKind::LockDist,
            ConstraintKind::Equal,
            ConstraintKind::Align,
            ConstraintKind::Parallel,
            ConstraintKind::Perp,
            ConstraintKind::AlignGrid,
            ConstraintKind::LockDistGrid,
            ConstraintKind::LevelGap,
            ConstraintKind::TopToLevel,
        ]
        .into_iter()
        .find(|k| k.code() == s)
    }

    fn arity(self) -> (usize, usize) {
        match self {
            ConstraintKind::LockLength => (1, 1),
            ConstraintKind::Equal => (3, usize::MAX),
            _ => (2, 2),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConstraintSpec {
    pub kind: ConstraintKind,
    pub walls: Vec<i64>,
    /// For locks: None means take the current value.
    pub value: Option<f64>,
}

impl ConstraintSpec {
    /// Participants by `walls` position (for rules with grid axes and levels, by rule kind).
    pub fn refs(&self) -> Vec<Ref> {
        use ConstraintKind as K;
        self.walls
            .iter()
            .enumerate()
            .map(|(k, &id)| match (self.kind, k) {
                (K::AlignGrid | K::LockDistGrid, 1) => Ref::Grid(id),
                (K::LevelGap, _) | (K::TopToLevel, 1) => Ref::Level(id),
                _ => Ref::Wall(id),
            })
            .collect()
    }

    /// Rule on level elevations (solved without ddc-solver).
    pub fn on_levels(&self) -> bool {
        matches!(
            self.kind,
            ConstraintKind::LevelGap | ConstraintKind::TopToLevel
        )
    }
}

type Ax = ((f64, f64), (f64, f64));

fn axis(db: &Connection, id: i64) -> Result<Option<Ax>, Error> {
    line(db, Ref::Wall(id))
}

fn line(db: &Connection, r: Ref) -> Result<Option<Ax>, Error> {
    let (sql, id) = match r {
        Ref::Wall(id) => ("SELECT ax, ay, bx, by FROM wall WHERE id=?1", id),
        Ref::Grid(id) => ("SELECT ax, ay, bx, by FROM grid WHERE id=?1", id),
        Ref::Level(_) => return Ok(None),
    };
    let mut st = db.prepare_cached(sql)?;
    let mut r = st.query([id])?;
    Ok(match r.next()? {
        Some(r) => Some(((r.get(0)?, r.get(1)?), (r.get(2)?, r.get(3)?))),
        None => None,
    })
}

pub(crate) fn load(db: &Connection) -> Result<Vec<(i64, ConstraintSpec)>, Error> {
    let mut st =
        db.prepare_cached("SELECT id, kind, walls, value FROM constraint_rule WHERE kind NOT IN ('dist','dist_step') ORDER BY id")?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<f64>>(3)?,
        ))
    })?;
    let mut out = vec![];
    for row in rows {
        let (id, k, w, value) = row?;
        if k == "measure" {
            // the twin of a driving measurement: a distance or level-gap rule on its supports
            if let Some(s) = crate::measure::twin_spec(db, &w, value.unwrap_or(0.0))? {
                out.push((id, s));
            }
            continue;
        }
        let kind = ConstraintKind::parse(&k)
            .ok_or_else(|| Error::Invalid(format!("constraint {id}: unknown kind '{k}'")))?;
        let walls = w
            .split_whitespace()
            .map(|t| t.parse::<i64>())
            .collect::<Result<_, _>>()
            .map_err(|_| Error::Invalid(format!("constraint {id}: bad walls '{w}'")))?;
        out.push((id, ConstraintSpec { kind, walls, value }));
    }
    Ok(out)
}

fn rule(s: &ConstraintSpec, line: impl Fn(Ref) -> (usize, usize)) -> Rule {
    let refs = s.refs();
    let l = |k: usize| line(refs[k]);
    let v = s.value.unwrap_or(0.0);
    match s.kind {
        ConstraintKind::LockLength => Rule::Dist(l(0).0, l(0).1, v),
        ConstraintKind::LockDist => Rule::LineDist(l(0), l(1), v),
        ConstraintKind::Equal => Rule::Equal((0..s.walls.len()).map(l).collect()),
        ConstraintKind::Align => Rule::Collinear(l(0), l(1)),
        ConstraintKind::Parallel => Rule::Parallel(l(0), l(1)),
        ConstraintKind::Perp => Rule::Perp(l(0), l(1)),
        ConstraintKind::AlignGrid => Rule::Collinear(l(1), l(0)),
        ConstraintKind::LockDistGrid => Rule::LineDist(l(0), l(1), v),
        ConstraintKind::LevelGap | ConstraintKind::TopToLevel => {
            unreachable!("level rules are solved by propagate_levels")
        }
    }
}

fn name(r: Ref) -> String {
    match r {
        Ref::Wall(w) => format!("#{w}"),
        Ref::Grid(g) => format!("grid #{g}"),
        Ref::Level(l) => format!("level #{l}"),
    }
}

/// Re-solves the constraints tied to the `edited` walls (they are pinned) and writes the shifts
/// of the other lines. Returns the inverse commands for the journal.
pub(crate) fn propagate(db: &Connection, edited: &[i64]) -> Result<Inv, Error> {
    let e: Vec<Ref> = edited.iter().map(|&w| Ref::Wall(w)).collect();
    propagate_refs(db, &e)
}

/// Same for any lines (walls, grid axes): `edited` are pinned.
pub(crate) fn propagate_refs(db: &Connection, edited: &[Ref]) -> Result<Inv, Error> {
    let rules = load(db)?;
    if rules.is_empty() {
        return Ok(Inv::default());
    }
    // live rules on lines: all participants exist
    let mut axes: BTreeMap<Ref, Ax> = BTreeMap::new();
    let mut live = vec![];
    for (id, s) in rules {
        if s.on_levels() {
            continue;
        }
        let mut ok = true;
        for r in s.refs() {
            if let Ref::Grid(g) = r {
                // the rule is solved on a straight line; an axis that became an arc no longer holds it
                let bulge: Option<f64> = db
                    .query_row("SELECT bulge FROM grid WHERE id=?1", [g], |x| x.get(0))
                    .optional()?;
                if bulge.is_some_and(|b| b != 0.0) {
                    if edited.contains(&r) {
                        return Err(Error::Conflict(format!(
                            "edit breaks constraints: {} on grid #{g} needs a straight grid                              (rule {id})",
                            s.kind.code()
                        )));
                    }
                    ok = false;
                    continue;
                }
            }
            match line(db, r)? {
                Some(a) => {
                    axes.insert(r, a);
                }
                None => ok = false,
            }
        }
        if ok {
            live.push((id, s));
        }
    }
    // connected component reachable from the edited lines
    let mut comp: BTreeSet<Ref> = edited.iter().copied().collect();
    loop {
        let n = comp.len();
        for (_, s) in &live {
            let rs = s.refs();
            if rs.iter().any(|w| comp.contains(w)) {
                comp.extend(rs);
            }
        }
        if comp.len() == n {
            break;
        }
    }
    let used: Vec<&(i64, ConstraintSpec)> = live
        .iter()
        .filter(|(_, s)| s.refs().iter().any(|w| comp.contains(w)))
        .collect();
    if used.is_empty() {
        return Ok(Inv::default());
    }
    let nodes: Vec<Ref> = comp
        .iter()
        .copied()
        .filter(|w| axes.contains_key(w))
        .collect();
    let idx = |w: Ref| {
        let k = nodes
            .iter()
            .position(|&x| x == w)
            .expect("line in component");
        (2 * k, 2 * k + 1)
    };
    let mut x = vec![];
    for w in &nodes {
        let (a, b) = axes[w];
        x.extend([a.0, a.1, b.0, b.1]);
    }
    let cs: Vec<Constraint> = used
        .iter()
        .map(|(id, s)| Constraint {
            tag: *id,
            rule: rule(s, idx),
        })
        .collect();
    let pinned: Vec<usize> = edited
        .iter()
        .filter(|w| nodes.contains(w))
        .flat_map(|&w| {
            let (a, b) = idx(w);
            [a, b]
        })
        .collect();
    let sol = ddc_solver::solve(&cs, &x, &pinned).map_err(|c| {
        let names: Vec<String> = c
            .tags
            .iter()
            .map(|(t, r)| {
                let s = &used.iter().find(|u| u.0 == *t).expect("tag").1;
                let w: Vec<String> = s.refs().into_iter().map(name).collect();
                let who = crate::measure::twin_measure(db, *t)
                    .map_or(String::new(), |m| format!("driving measurement #{m}: "));
                format!("{who}{} {} (off by {:.4} m)", s.kind.code(), w.join(" "), r)
            })
            .collect();
        Error::Conflict(format!("edit breaks constraints: {}", names.join("; ")))
    })?;
    let mut inv = Inv::default();
    for (k, w) in nodes.iter().enumerate() {
        if edited.contains(w) {
            continue;
        }
        // a solution within 1e-9 of the stored value keeps the stored value: no drift from repeated solves
        let old = &x[4 * k..4 * k + 4];
        let v: Vec<f64> = sol[4 * k..4 * k + 4]
            .iter()
            .zip(old)
            .map(|(&s, &o)| if (s - o).abs() <= 1e-9 { o } else { s })
            .collect();
        if v.iter().zip(old).all(|(a, b)| a == b) {
            continue;
        }
        inv = write_line(db, inv, *w, (v[0], v[1]), (v[2], v[3]))?;
    }
    Ok(inv)
}

/// Writes a solved line; a line collapsed by the solution is a conflict, not a "degenerate edit".
fn write_line(
    db: &Connection,
    inv: Inv,
    w: Ref,
    a: (f64, f64),
    b: (f64, f64),
) -> Result<Inv, Error> {
    let collapse = || {
        Error::Conflict(format!(
            "edit breaks constraints: solving would collapse {}",
            match w {
                Ref::Wall(id) => format!("wall #{id}"),
                r => name(r),
            }
        ))
    };
    match w {
        Ref::Wall(id) => crate::walls::write_axis(db, inv, id, a, b).map_err(|e| match e {
            Error::Degenerate(_) => collapse(),
            e => e,
        }),
        Ref::Grid(id) => {
            if (b.0 - a.0).hypot(b.1 - a.1) <= 1e-6 {
                return Err(collapse());
            }
            let inv = inv.restore(db, "grid", "id", id)?;
            db.execute(
                "UPDATE grid SET ax=?2, ay=?3, bx=?4, by=?5 WHERE id=?1",
                params![id, a.0, a.1, b.0, b.1],
            )?;
            Ok(inv)
        }
        Ref::Level(_) => Ok(inv),
    }
}

fn elevation(db: &Connection, l: i64) -> Result<Option<f64>, Error> {
    let mut st = db.prepare_cached("SELECT elevation FROM level WHERE id=?1")?;
    let mut r = st.query([l])?;
    Ok(match r.next()? {
        Some(r) => Some(r.get(0)?),
        None => None,
    })
}

/// After editing the elevation of level `edited`: level pitches carry linked levels along (walk over
/// `level_gap` rules, a contradiction is a conflict), then wall tops by `top_level` are brought
/// to the elevations. Returns the inverse commands.
pub(crate) fn propagate_levels(db: &Connection, edited: i64) -> Result<Inv, Error> {
    let rules: Vec<(i64, ConstraintSpec)> = load(db)?
        .into_iter()
        .filter(|(_, s)| s.on_levels())
        .collect();
    if rules.is_empty() {
        return Ok(Inv::default());
    }
    let mut inv = Inv::default();
    let mut z: BTreeMap<i64, f64> = BTreeMap::new();
    if let Some(e) = elevation(db, edited)? {
        z.insert(edited, e);
    }
    let mut todo: Vec<i64> = z.keys().copied().collect();
    while let Some(l) = todo.pop() {
        let zl = z[&l];
        for (id, s) in &rules {
            if s.kind != ConstraintKind::LevelGap {
                continue;
            }
            let (a, b, v) = (s.walls[0], s.walls[1], s.value.unwrap_or(0.0));
            let (other, want) = if a == l {
                (b, zl + v)
            } else if b == l {
                (a, zl - v)
            } else {
                continue;
            };
            if elevation(db, other)?.is_none() {
                continue;
            }
            match z.get(&other) {
                Some(&have) if (have - want).abs() > 1e-9 => {
                    return Err(Error::Conflict(format!(
                        "edit breaks constraints: level_gap level #{a} level #{b} \
                         (off by {:.4} m, rule {id})",
                        (have - want).abs()
                    )))
                }
                Some(_) => {}
                None => {
                    z.insert(other, want);
                    todo.push(other);
                }
            }
        }
    }
    for (&l, &e) in &z {
        let old = elevation(db, l)?.expect("level");
        if l == edited || old == e {
            continue;
        }
        inv = inv.sql(
            "UPDATE level SET elevation=?2 WHERE id=?1",
            vec![l.into(), old.into()],
        );
        db.execute("UPDATE level SET elevation=?2 WHERE id=?1", params![l, e])?;
    }
    apply_tops(db, inv)
}

/// Height of walls with a `top_level` rule: level elevation − (base level elevation + base offset).
fn apply_tops(db: &Connection, mut inv: Inv) -> Result<Inv, Error> {
    type Row = (i64, f64, f64, (f64, f64), (f64, f64));
    for (id, s) in load(db)? {
        if s.kind != ConstraintKind::TopToLevel {
            continue;
        }
        let (w, l) = (s.walls[0], s.walls[1]);
        let row: Option<Row> = db
            .query_row(
                "SELECT level, base_offset, height, ax, ay, bx, by FROM wall WHERE id=?1",
                [w],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        (r.get(3)?, r.get(4)?),
                        (r.get(5)?, r.get(6)?),
                    ))
                },
            )
            .ok();
        let (Some((base, off, h, a, b)), Some(top)) = (row, elevation(db, l)?) else {
            continue;
        };
        let Some(z0) = elevation(db, base)? else {
            continue;
        };
        let want = top - z0 - off;
        if want <= 0.01 {
            return Err(Error::Conflict(format!(
                "edit breaks constraints: top_level #{w} level #{l} \
                 (wall height would be {want:.4} m, rule {id})"
            )));
        }
        if (want - h).abs() <= 1e-9 {
            continue;
        }
        inv = inv.restore(db, "wall", "id", w)?;
        let tp = crate::walls::remapped_top(db, w, a, b, None, Some(want))?;
        db.execute(
            "UPDATE wall SET height=?2, top_profile=?3 WHERE id=?1",
            params![w, want, tp],
        )?;
        inv = crate::roof_auto::sync(db, inv, Some(&[w]))?;
    }
    Ok(inv)
}

/// Check without solving: all walls are pinned. "Modify" edits (move, rotate, mirror,
/// array, trim) do not move neighbours; a violated rule rejects the whole edit.
pub(crate) fn verify(db: &Connection) -> Result<(), Error> {
    let refs: BTreeSet<Ref> = load(db)?
        .into_iter()
        .filter(|(_, s)| !s.on_levels())
        .flat_map(|(_, s)| s.refs())
        .collect();
    if refs.is_empty() {
        return Ok(());
    }
    let w: Vec<Ref> = refs.into_iter().collect();
    propagate_refs(db, &w).map(|_| ())
}

/// Removes rules that reference a wall (wall deletion); undo restores them.
pub(crate) fn drop_for_wall(db: &Connection, inv: Inv, wall: i64) -> Result<Inv, Error> {
    drop_for_ref(db, inv, Ref::Wall(wall))
}

/// Removes rules with participant `r` (wall, axis or level deletion); undo restores them.
/// Otherwise a new element with the same id (ids lack AUTOINCREMENT) would inherit stale rules.
pub(crate) fn drop_for_ref(db: &Connection, mut inv: Inv, r: Ref) -> Result<Inv, Error> {
    for (id, s) in load(db)? {
        if s.refs().contains(&r) {
            inv = inv.restore(db, "constraint_rule", "id", id)?;
            db.execute("DELETE FROM constraint_rule WHERE id=?1", [id])?;
        }
    }
    Ok(inv)
}

/// Rules referencing a missing axis or level (for integrity checks).
pub(crate) fn dangling(db: &Connection) -> Result<Vec<(i64, String)>, Error> {
    let mut out = vec![];
    for (id, s) in load(db)? {
        for r in s.refs() {
            let gone = match r {
                Ref::Grid(g) => line(db, r)?.is_none().then_some(format!("grid {g}")),
                Ref::Level(l) => elevation(db, l)?.is_none().then_some(format!("level {l}")),
                Ref::Wall(_) => None,
            };
            if let Some(g) = gone {
                out.push((id, format!("{} -> missing {g}", s.kind.code())));
            }
        }
    }
    Ok(out)
}

pub(crate) fn current_value(db: &Connection, s: &ConstraintSpec) -> Result<Option<f64>, Error> {
    let ax = |w: i64| axis(db, w)?.ok_or_else(|| Error::Invalid(format!("no such wall {w}")));
    let el = |l: i64| elevation(db, l)?.ok_or_else(|| Error::Invalid(format!("no such level {l}")));
    Ok(match s.kind {
        ConstraintKind::LockDistGrid => {
            let (a, b) = ax(s.walls[0])?;
            let (p, _) = line(db, Ref::Grid(s.walls[1]))?
                .ok_or_else(|| Error::Invalid(format!("no such grid {}", s.walls[1])))?;
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let l = dx.hypot(dy).max(1e-12);
            Some((dx * (p.1 - a.1) - dy * (p.0 - a.0)) / l)
        }
        ConstraintKind::LevelGap => Some(el(s.walls[1])? - el(s.walls[0])?),
        ConstraintKind::LockLength => {
            let (a, b) = ax(s.walls[0])?;
            Some((b.0 - a.0).hypot(b.1 - a.1))
        }
        ConstraintKind::LockDist => {
            let ((a, b), (p, _)) = (ax(s.walls[0])?, ax(s.walls[1])?);
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let l = dx.hypot(dy).max(1e-12);
            Some((dx * (p.1 - a.1) - dy * (p.0 - a.0)) / l)
        }
        _ => None,
    })
}

impl Model {
    /// Adds a constraint. The first wall is the reference (pinned): a non-matching ∥/⊥/alignment
    /// immediately brings the others into line. Unsatisfiable together with existing ones: `Error::Conflict`.
    pub fn add_constraint(&mut self, s: &ConstraintSpec) -> Result<i64, Error> {
        let (lo, hi) = s.kind.arity();
        if s.walls.len() < lo || s.walls.len() > hi {
            return Err(Error::Invalid(format!(
                "{} needs {lo}..{} walls",
                s.kind.code(),
                if hi == usize::MAX {
                    "n".into()
                } else {
                    hi.to_string()
                }
            )));
        }
        let uniq: BTreeSet<_> = s.refs().into_iter().collect();
        if uniq.len() != s.walls.len() {
            return Err(Error::Invalid("constraint walls must differ".into()));
        }
        self.op("constraint_add", |db| {
            let value = match s.value {
                Some(v) => Some(v),
                None => current_value(db, s)?,
            };
            let walls: Vec<String> = s.walls.iter().map(|w| w.to_string()).collect();
            for r in s.refs() {
                let ok = match r {
                    Ref::Level(l) => elevation(db, l)?.is_some(),
                    Ref::Grid(g) => {
                        let arc: Option<f64> = db
                            .query_row("SELECT bulge FROM grid WHERE id=?1", [g], |x| x.get(0))
                            .ok();
                        if arc.is_some_and(|b| b != 0.0) {
                            return Err(Error::Invalid(format!("grid #{g} is an arc")));
                        }
                        arc.is_some()
                    }
                    Ref::Wall(_) => line(db, r)?.is_some(),
                };
                if !ok {
                    return Err(Error::Invalid(format!("no such {}", name(r))));
                }
            }
            db.execute(
                "INSERT INTO constraint_rule(kind, walls, value) VALUES (?1, ?2, ?3)",
                params![s.kind.code(), walls.join(" "), value],
            )?;
            let id = db.last_insert_rowid();
            let inv = if s.kind == ConstraintKind::TopToLevel {
                apply_tops(db, Inv::default())?
            } else if s.on_levels() {
                propagate_levels(db, s.walls[0])?
            } else {
                propagate_refs(db, &s.refs()[..1])?
            };
            let inv = inv.delete("constraint_rule", "id", id);
            Ok((id, id, inv))
        })
    }

    pub fn delete_constraint(&mut self, id: i64) -> Result<(), Error> {
        self.op("constraint_delete", |db| {
            let inv = Inv::default().restore(db, "constraint_rule", "id", id)?;
            if db.execute("DELETE FROM constraint_rule WHERE id=?1", [id])? == 0 {
                return Err(Error::Invalid("no such constraint".into()));
            }
            Ok(((), id, inv))
        })
    }

    /// All constraints (id, spec); rules with deleted walls are included too, the UI flags them.
    pub fn constraints(&self) -> Result<Vec<(i64, ConstraintSpec)>, Error> {
        load(&self.db)
    }

    /// Constraints involving a grid axis or level.
    pub fn constraints_on(&self, r: Ref) -> Result<Vec<(i64, ConstraintSpec)>, Error> {
        Ok(load(&self.db)?
            .into_iter()
            .filter(|(_, s)| s.refs().contains(&r))
            .collect())
    }

    /// Constraints involving a wall.
    pub fn constraints_of(&self, wall: i64) -> Result<Vec<(i64, ConstraintSpec)>, Error> {
        Ok(load(&self.db)?
            .into_iter()
            .filter(|(_, s)| s.refs().contains(&Ref::Wall(wall)))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LayerSpec, WallSpec};

    fn model() -> (Model, i64, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m
            .add_wall_type(
                "W",
                &[LayerSpec {
                    material: "c".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        (m, lv, t)
    }

    fn wall(m: &mut Model, lv: i64, t: i64, a: (f64, f64), b: (f64, f64)) -> i64 {
        m.add_wall(&WallSpec {
            a,
            b,
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        })
        .unwrap()
    }

    fn ax(m: &Model, w: i64) -> Ax {
        axis(m.sql(), w).unwrap().unwrap()
    }

    #[test]
    fn eq_chain_stays_equal_after_moving_a_wall_and_undo_restores() {
        let (mut m, lv, t) = model();
        let ws: Vec<i64> = [0.0, 3.0, 6.0]
            .iter()
            .map(|&x| wall(&mut m, lv, t, (x, 0.0), (x, 5.0)))
            .collect();
        let eq = ConstraintSpec {
            kind: ConstraintKind::Equal,
            walls: ws.clone(),
            value: None,
        };
        m.add_constraint(&eq).unwrap();
        m.set_wall_axis(ws[2], (8.0, 0.0), (8.0, 5.0)).unwrap();
        let x: Vec<f64> = ws.iter().map(|&w| ax(&m, w).0 .0).collect();
        assert!(((x[1] - x[0]) - (x[2] - x[1])).abs() < 1e-6, "{x:?}");
        assert_eq!(x[2], 8.0, "edited wall is exact");
        // one operation = one undo: the middle wall is restored too
        m.undo().unwrap();
        assert_eq!(ax(&m, ws[1]).0 .0, 3.0);
        assert_eq!(ax(&m, ws[2]).0 .0, 6.0);
    }

    #[test]
    fn lock_conflict_rolls_back_and_names_rule() {
        let (mut m, lv, t) = model();
        let w = wall(&mut m, lv, t, (0.0, 0.0), (4.0, 0.0));
        let id = m
            .add_constraint(&ConstraintSpec {
                kind: ConstraintKind::LockLength,
                walls: vec![w],
                value: None,
            })
            .unwrap();
        let e = m.set_wall_axis(w, (0.0, 0.0), (5.0, 0.0)).unwrap_err();
        assert!(matches!(e, Error::Conflict(_)));
        assert!(e.to_string().contains("lock_len"), "{e}");
        assert_eq!(ax(&m, w).1, (4.0, 0.0), "state unchanged");
        // a move that keeps the length is allowed
        m.set_wall_axis(w, (1.0, 1.0), (5.0, 1.0)).unwrap();
        m.delete_constraint(id).unwrap();
        m.set_wall_axis(w, (0.0, 0.0), (5.0, 0.0)).unwrap();
    }

    #[test]
    fn perp_parallel_align_and_lock_dist_follow() {
        let (mut m, lv, t) = model();
        let a = wall(&mut m, lv, t, (0.0, 0.0), (10.0, 0.0));
        let b = wall(&mut m, lv, t, (0.0, 0.0), (0.5, 4.0));
        let c = wall(&mut m, lv, t, (0.0, 3.0), (10.0, 3.0));
        let d = wall(&mut m, lv, t, (12.0, 0.0), (15.0, 0.0));
        let add = |m: &mut Model, kind, walls: Vec<i64>| {
            m.add_constraint(&ConstraintSpec {
                kind,
                walls,
                value: None,
            })
            .unwrap()
        };
        add(&mut m, ConstraintKind::Perp, vec![a, b]);
        let (p, q) = ax(&m, b);
        assert!(((q.0 - p.0) * 10.0).abs() < 1e-6, "made perpendicular");
        add(&mut m, ConstraintKind::LockDist, vec![a, c]);
        add(&mut m, ConstraintKind::Align, vec![a, d]);
        // rotating the reference wall: c stays parallel at 3 m, d stays on its line
        m.set_wall_axis(a, (0.0, 0.0), (10.0, 1.0)).unwrap();
        let x = |w| {
            let (p, q) = ax(&m, w);
            vec![p.0, p.1, q.0, q.1]
        };
        let la = (0, 1);
        let mut pts = x(a);
        pts.extend(x(c));
        pts.extend(x(d));
        pts.extend(x(b));
        assert!((ddc_solver::sdist(&pts, la, 2) - 3.0).abs() < 1e-6);
        assert!((ddc_solver::sdist(&pts, la, 3) - 3.0).abs() < 1e-6);
        assert!(ddc_solver::sdist(&pts, la, 4).abs() < 1e-6);
        assert!(ddc_solver::sdist(&pts, la, 5).abs() < 1e-6);
        // persistence: rules are read back from the file
        assert_eq!(m.constraints().unwrap().len(), 3);
        assert_eq!(m.constraints_of(a).unwrap().len(), 3);
    }

    #[test]
    fn zero_length_axis_is_rejected_without_write() {
        let (mut m, lv, t) = model();
        let w = wall(&mut m, lv, t, (0.0, 0.0), (4.0, 0.0));
        let e = m
            .set_wall_axis(w, (1.0, 1.0), (1.0, 1.0 + 1e-6))
            .unwrap_err();
        assert!(matches!(e, Error::Degenerate(_)), "{e}");
        assert_eq!(ax(&m, w), ((0.0, 0.0), (4.0, 0.0)));
    }

    #[test]
    fn deleting_wall_drops_its_rules_and_undo_restores() {
        let (mut m, lv, t) = model();
        let a = wall(&mut m, lv, t, (0.0, 0.0), (10.0, 0.0));
        let b = wall(&mut m, lv, t, (0.0, 3.0), (10.0, 3.0));
        m.add_constraint(&ConstraintSpec {
            kind: ConstraintKind::Parallel,
            walls: vec![a, b],
            value: None,
        })
        .unwrap();
        m.delete_wall(b).unwrap();
        assert!(m.constraints().unwrap().is_empty());
        // a new wall with the same id does not inherit a stale rule
        let c = wall(&mut m, lv, t, (0.0, 5.0), (3.0, 9.0));
        assert!(m.constraints_of(c).unwrap().is_empty());
        m.undo().unwrap();
        m.undo().unwrap();
        assert_eq!(m.constraints().unwrap().len(), 1);
        m.delete_walls(&[a]).unwrap();
        assert!(m.constraints().unwrap().is_empty());
        m.undo().unwrap();
        assert_eq!(m.constraints().unwrap().len(), 1);
    }

    #[test]
    fn redundant_consistent_rules_are_fine_contradictory_conflict() {
        let (mut m, lv, t) = model();
        let a = wall(&mut m, lv, t, (0.0, 0.0), (10.0, 0.0));
        let b = wall(&mut m, lv, t, (0.0, 3.0), (10.0, 3.0));
        let add = |m: &mut Model, kind| {
            m.add_constraint(&ConstraintSpec {
                kind,
                walls: vec![a, b],
                value: None,
            })
        };
        add(&mut m, ConstraintKind::Parallel).unwrap();
        add(&mut m, ConstraintKind::Parallel).unwrap();
        m.set_wall_axis(a, (0.0, 0.0), (10.0, 2.0)).unwrap();
        let e = add(&mut m, ConstraintKind::Perp).unwrap_err();
        assert!(matches!(e, Error::Conflict(_)), "{e}");
    }

    #[test]
    fn repeated_solves_do_not_drift() {
        let (mut m, lv, t) = model();
        let ws: Vec<i64> = [0.0, 3.0, 6.0]
            .iter()
            .map(|&x| wall(&mut m, lv, t, (x, 0.0), (x, 5.0)))
            .collect();
        m.add_constraint(&ConstraintSpec {
            kind: ConstraintKind::Equal,
            walls: ws.clone(),
            value: None,
        })
        .unwrap();
        let mid = ax(&m, ws[1]);
        for _ in 0..20 {
            m.set_wall_axis(ws[0], (0.0, 0.0), (0.0, 5.0)).unwrap();
        }
        assert_eq!(ax(&m, ws[1]), mid, "bit-exact");
    }

    #[test]
    fn fresh_schema_equals_migrated_from_16() {
        fn shape(db: &rusqlite::Connection) -> Vec<String> {
            let mut st = db
                .prepare(
                    "SELECT m.name, p.name, p.type, p.\"notnull\", p.pk FROM sqlite_master m,                      pragma_table_info(m.name) p WHERE m.type='table' ORDER BY m.name, p.cid",
                )
                .unwrap();
            st.query_map([], |r| {
                Ok(format!(
                    "{}.{} {} {} {}",
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?
                ))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
        }
        let fresh = Model::open(":memory:").unwrap();
        let dir = std::env::temp_dir().join(format!("ddc_mig17_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("m.ddc");
        let ps = p.to_str().unwrap();
        {
            let m = Model::open(ps).unwrap();
            m.sql()
                .execute_batch(
                    "DROP TABLE constraint_rule;                      UPDATE meta SET value=16 WHERE key='schema_version';",
                )
                .unwrap();
        }
        let mig = Model::open(ps).unwrap();
        assert_eq!(shape(fresh.sql()), shape(mig.sql()));
        let v: i64 = mig
            .sql()
            .query_row(
                "SELECT value FROM meta WHERE key='schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v, crate::schema::VERSION);
        drop(mig);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn modify_move_that_breaks_a_lock_is_rejected() {
        let (mut m, lv, t) = model();
        let a = wall(&mut m, lv, t, (0.0, 0.0), (10.0, 0.0));
        let c = wall(&mut m, lv, t, (0.0, 3.0), (10.0, 3.0));
        m.add_constraint(&ConstraintSpec {
            kind: ConstraintKind::LockDist,
            walls: vec![a, c],
            value: None,
        })
        .unwrap();
        let e = m
            .move_elements(&[crate::Target::Wall(c)], (0.0, 1.0))
            .unwrap_err();
        assert!(e.to_string().contains("lock_dist"), "{e}");
        assert_eq!(ax(&m, c), ((0.0, 3.0), (10.0, 3.0)));
        // together, the rule holds
        m.move_elements(
            &[crate::Target::Wall(a), crate::Target::Wall(c)],
            (0.0, 1.0),
        )
        .unwrap();
    }

    fn spec(kind: ConstraintKind, walls: Vec<i64>) -> ConstraintSpec {
        ConstraintSpec {
            kind,
            walls,
            value: None,
        }
    }

    fn grid_ax(m: &Model, g: i64) -> Ax {
        line(m.sql(), Ref::Grid(g)).unwrap().unwrap()
    }

    #[test]
    fn wall_aligned_and_locked_to_grids_follows_grid_moves() {
        let (mut m, lv, t) = model();
        let g1 = m
            .add_grid(&crate::GridSpec::line("1", (0.0, -1.0), (0.0, 9.0)))
            .unwrap();
        let g2 = m
            .add_grid(&crate::GridSpec::line("2", (6.0, -1.0), (6.0, 9.0)))
            .unwrap();
        let w = wall(&mut m, lv, t, (0.5, 0.0), (0.5, 5.0));
        let v = wall(&mut m, lv, t, (5.0, 0.0), (5.0, 5.0));
        m.add_constraint(&spec(ConstraintKind::AlignGrid, vec![w, g1]))
            .unwrap();
        // alignment: the wall snaps onto the axis (the axis is the reference, the wall is pinned first… the axis follows)
        let (a, b) = (ax(&m, w), grid_ax(&m, g1));
        assert!(
            a.0 .0.abs() < 1e-9 || (b.0 .0 - 0.5).abs() < 1e-9,
            "{a:?} {b:?}"
        );
        assert!((a.0 .0 - b.0 .0).abs() < 1e-9 && (a.1 .0 - b.1 .0).abs() < 1e-9);
        let id = m
            .add_constraint(&spec(ConstraintKind::LockDistGrid, vec![v, g2]))
            .unwrap();
        let val = m
            .constraints()
            .unwrap()
            .into_iter()
            .find(|c| c.0 == id)
            .unwrap()
            .1
            .value;
        assert!((val.unwrap().abs() - 1.0).abs() < 1e-12, "{val:?}");
        // axis 2 shifts by +2: wall v shifts by +2, one operation
        let mut s2 = m
            .grids()
            .unwrap()
            .into_iter()
            .find(|g| g.id == g2)
            .unwrap()
            .spec;
        s2.a = (8.0, -1.0);
        s2.b = (8.0, 9.0);
        m.update_grid(g2, &s2).unwrap();
        let x = ax(&m, v);
        assert!(
            (x.0 .0 - 7.0).abs() < 1e-6 && (x.1 .0 - 7.0).abs() < 1e-6,
            "{x:?}"
        );
        m.undo().unwrap();
        assert_eq!(ax(&m, v), ((5.0, 0.0), (5.0, 5.0)));
        assert_eq!(grid_ax(&m, g2), ((6.0, -1.0), (6.0, 9.0)));
        // editing a wall aligned to an axis: the axis follows the wall (the link is two-way)
        let a0 = ax(&m, w);
        m.set_wall_axis(w, (1.0, a0.0 .1), (1.0, a0.1 .1)).unwrap();
        let g = grid_ax(&m, g1);
        assert!(
            (g.0 .0 - 1.0).abs() < 1e-6 && (g.1 .0 - 1.0).abs() < 1e-6,
            "{g:?}"
        );
        assert_eq!(m.constraints_on(Ref::Grid(g1)).unwrap().len(), 1);
    }

    #[test]
    fn grid_edit_regenerates_rooms_bounded_by_walls_locked_to_it() {
        let (mut m, lv, t) = model();
        wall(&mut m, lv, t, (-1.0, 0.0), (10.0, 0.0));
        wall(&mut m, lv, t, (-1.0, 5.0), (10.0, 5.0));
        wall(&mut m, lv, t, (0.0, -1.0), (0.0, 6.0));
        let east = wall(&mut m, lv, t, (6.0, -1.0), (6.0, 6.0));
        let g = m
            .add_grid(&crate::GridSpec::line("A", (6.0, -2.0), (6.0, 7.0)))
            .unwrap();
        m.add_constraint(&spec(ConstraintKind::AlignGrid, vec![east, g]))
            .unwrap();
        let room = m
            .add_room(&crate::RoomSpec {
                level: lv,
                name: "R".into(),
                number: "1".into(),
                point: (3.0, 2.5),
                height: 3.0,
            })
            .unwrap();
        let area = |m: &Model| -> f64 {
            m.sql()
                .query_row("SELECT area FROM room_qty WHERE room=?1", [room], |r| {
                    r.get(0)
                })
                .unwrap()
        };
        let a0 = area(&m);
        m.update_grid(g, &crate::GridSpec::line("A", (7.0, -2.0), (7.0, 7.0)))
            .unwrap();
        assert!(
            (ax(&m, east).0 .0 - 7.0).abs() < 1e-6,
            "wall follows the grid"
        );
        let a1 = area(&m);
        assert!(
            a1 > a0 + 4.0,
            "room re-derived in the same op: {a0} -> {a1}"
        );
        crate::regenerate(m.sql()).unwrap();
        assert!(
            (area(&m) - a1).abs() < 1e-9,
            "nothing stale after the grid edit"
        );
    }

    #[test]
    fn level_gap_and_wall_top_follow_level_edit_with_one_undo() {
        let (mut m, lv, t) = model();
        let l2 = m.add_level("L2", 3.0).unwrap();
        let l3 = m.add_level("L3", 6.5).unwrap();
        let w = wall(&mut m, lv, t, (0.0, 0.0), (4.0, 0.0));
        m.add_constraint(&spec(ConstraintKind::LevelGap, vec![l2, l3]))
            .unwrap();
        m.add_constraint(&spec(ConstraintKind::TopToLevel, vec![w, l3]))
            .unwrap();
        let h = |m: &Model| -> f64 {
            m.sql()
                .query_row("SELECT height FROM wall WHERE id=?1", [w], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(h(&m), 6.5, "top snapped to L3 on add");
        m.update_level(l2, "L2", 3.5).unwrap();
        let z = |m: &Model, l: i64| {
            m.level_list()
                .unwrap()
                .into_iter()
                .find(|x| x.0 == l)
                .unwrap()
                .2
        };
        assert_eq!(z(&m, l3), 7.0);
        assert_eq!(h(&m), 7.0);
        m.undo().unwrap();
        assert_eq!((z(&m, l2), z(&m, l3), h(&m)), (3.0, 6.5, 6.5));
        // top level below the wall base is a conflict, nothing is written
        let e = m.update_level(l2, "L2", -4.0).unwrap_err();
        assert!(matches!(e, Error::Conflict(_)), "{e}");
        assert!(e.to_string().contains("top_level"), "{e}");
        assert_eq!((z(&m, l2), z(&m, l3), h(&m)), (3.0, 6.5, 6.5));
        // contradictory cycle of pitches
        m.add_constraint(&ConstraintSpec {
            kind: ConstraintKind::LevelGap,
            walls: vec![lv, l2],
            value: None,
        })
        .unwrap();
        let e = m
            .add_constraint(&ConstraintSpec {
                kind: ConstraintKind::LevelGap,
                walls: vec![lv, l3],
                value: Some(1.0),
            })
            .unwrap_err();
        assert!(e.to_string().contains("level_gap"), "{e}");
        assert_eq!(m.constraints().unwrap().len(), 3);
    }

    #[test]
    fn solver_collapsing_a_wall_is_reported_as_conflict() {
        let (mut m, lv, t) = model();
        let a = wall(&mut m, lv, t, (0.0, 0.0), (4.0, 0.0));
        let b = wall(&mut m, lv, t, (0.0, 2.0), (4.0, 2.0));
        let c = wall(&mut m, lv, t, (6.0, 0.0), (6.0, 3.0));
        m.add_constraint(&spec(ConstraintKind::Parallel, vec![a, b]))
            .unwrap();
        m.add_constraint(&spec(ConstraintKind::Perp, vec![c, b]))
            .unwrap();
        // b ∥ a (pinned), c ∥ a, b ⊥ c: satisfiable only by collapsing b or c
        let before: Vec<Ax> = [a, b, c].iter().map(|&w| ax(&m, w)).collect();
        let e = m
            .add_constraint(&spec(ConstraintKind::Parallel, vec![a, c]))
            .unwrap_err();
        assert!(matches!(e, Error::Conflict(_)), "{e}");
        assert!(e.to_string().contains("edit breaks constraints"), "{e}");
        let after: Vec<Ax> = [a, b, c].iter().map(|&w| ax(&m, w)).collect();
        assert_eq!(before, after, "rolled back");
        assert_eq!(m.constraints().unwrap().len(), 2);
    }

    #[test]
    fn collapse_mapping_names_the_wall() {
        let (mut m, lv, t) = model();
        let w = wall(&mut m, lv, t, (0.0, 0.0), (4.0, 0.0));
        let e = m
            .op("probe", |db| {
                write_line(db, Inv::default(), Ref::Wall(w), (1.0, 1.0), (1.0, 1.0))?;
                Ok(((), w, Inv::default()))
            })
            .unwrap_err();
        assert!(matches!(e, Error::Conflict(_)), "{e}");
        assert!(e.to_string().contains("collapse wall #"), "{e}");
        assert_eq!(ax(&m, w), ((0.0, 0.0), (4.0, 0.0)));
    }
}
