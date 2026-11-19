//! Distribution along a wall: N openings or wall-hosted components at equal pitch between
//! end margins, or at a fixed pitch. A linked distribution is a rule in `constraint_rule`
//! (kind `dist` is equal pitch, value = margin; `dist_step` has value = pitch, the margin is taken
//! from the first member's position), walls = "wall slot…": > 0 opening, < 0 component, 0 empty slot.
//! [`relayout`] runs after every model edit (`ops::op`): if a wall is lengthened, the members
//! are redistributed; a slot outside the wall or on a foreign opening yields `Error::Conflict` and the edit is rolled back.

use rusqlite::{params, Connection, OptionalExtension};

use crate::ops::Inv;
use crate::{Error, Model, Target};

const EPS: f64 = 1e-9;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DistMode {
    /// Equal pitch; `margin` runs from the wall ends to the outermost edges.
    Equal { margin: f64 },
    /// Fixed pitch between centres; the first edge is `margin` from the wall start.
    Step { margin: f64, step: f64 },
}

impl DistMode {
    fn ok(self) -> bool {
        match self {
            DistMode::Equal { margin } => margin.is_finite() && margin >= 0.0,
            DistMode::Step { margin, step } => {
                margin.is_finite() && margin >= 0.0 && step.is_finite() && step > 0.0
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotConflict {
    /// The slot extends past the wall end.
    OffWall,
    /// Overlaps a foreign opening.
    Opening(i64),
    /// Pitch is smaller than the element width: neighbours overlap.
    Overlap,
}

/// Layout slot: start edge along the wall axis and conflict (None: free).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slot {
    pub along: f64,
    pub conflict: Option<SlotConflict>,
}

/// Distribution member.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Member {
    Empty,
    Opening(i64),
    Comp(i64),
}

impl Member {
    fn code(self) -> i64 {
        match self {
            Member::Empty => 0,
            Member::Opening(i) => i,
            Member::Comp(i) => -i,
        }
    }
    fn from_code(c: i64) -> Self {
        match c {
            0 => Member::Empty,
            c if c > 0 => Member::Opening(c),
            c => Member::Comp(-c),
        }
    }
    pub fn target(self) -> Option<Target> {
        match self {
            Member::Empty => None,
            Member::Opening(i) => Some(Target::Opening(i)),
            Member::Comp(i) => Some(Target::Component(i)),
        }
    }
}

/// Linked distribution (a `constraint_rule` row).
#[derive(Clone, Debug, PartialEq)]
pub struct DistRule {
    pub id: i64,
    pub wall: i64,
    pub members: Vec<Member>,
    pub mode: DistMode,
}

/// Result of `distribute`: the rule (if linked), members by slot, skipped slots.
#[derive(Clone, Debug, PartialEq)]
pub struct Distribution {
    pub rule: Option<i64>,
    pub members: Vec<Member>,
    pub skipped: Vec<(usize, SlotConflict)>,
}

/// Layout of `n` elements of width `width` on a wall of length `len`; `others` are foreign openings
/// (id, start, end) along the axis.
pub fn layout(
    len: f64,
    width: f64,
    n: usize,
    mode: DistMode,
    others: &[(i64, f64, f64)],
) -> Vec<Slot> {
    if n == 0 {
        return vec![];
    }
    let (start, step) = match mode {
        DistMode::Equal { .. } if n == 1 => ((len - width) / 2.0, 0.0),
        DistMode::Equal { margin } => (margin, (len - 2.0 * margin - width) / (n - 1) as f64),
        DistMode::Step { margin, step } => (margin, step),
    };
    let overlap = n > 1 && step < width - EPS;
    (0..n)
        .map(|i| {
            let along = start + step * i as f64;
            let conflict = if along < -EPS || along + width > len + EPS {
                Some(SlotConflict::OffWall)
            } else if overlap {
                Some(SlotConflict::Overlap)
            } else {
                others
                    .iter()
                    .find(|o| along < o.2 - EPS && along + width > o.1 + EPS)
                    .map(|o| SlotConflict::Opening(o.0))
            };
            Slot { along, conflict }
        })
        .collect()
}

/// How many elements at pitch `step` fit between the margins (up to the last one that fits).
pub fn fit_count(len: f64, width: f64, margin: f64, step: f64) -> usize {
    if step.is_nan() || step <= 0.0 || len - 2.0 * margin - width < -EPS {
        return 0;
    }
    ((len - 2.0 * margin - width) / step + EPS).floor() as usize + 1
}

fn wall_len(db: &Connection, wall: i64) -> Result<Option<f64>, Error> {
    let w: Option<(f64, f64, f64, f64, f64)> = db
        .prepare_cached("SELECT ax,ay,bx,by,bulge FROM wall WHERE id=?1")?
        .query_row([wall], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .optional()?;
    Ok(w.map(|(ax, ay, bx, by, bulge)| {
        ddc_geom::arc::axis_length(ddc_geom::v2(ax, ay), ddc_geom::v2(bx, by), bulge)
    }))
}

/// Host wall, start edge and width of a member; None if the member is missing or has no wall.
fn member_geom(db: &Connection, m: Member) -> Result<Option<(i64, f64, f64)>, Error> {
    Ok(match m {
        Member::Empty => None,
        Member::Opening(id) => db
            .prepare_cached("SELECT wall, along, width FROM opening WHERE id=?1")?
            .query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .optional()?,
        Member::Comp(id) => {
            let row: Option<(Option<i64>, f64)> = db
                .prepare_cached("SELECT host_wall, x FROM comp WHERE id=?1")?
                .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?;
            match row {
                Some((Some(w), x)) => {
                    let wd = comp_width(db, id, w)?;
                    Some((w, x - wd / 2.0, wd))
                }
                _ => None,
            }
        }
    })
}

/// Component width along the wall: projection of the `comp_qty` extent onto the axis (0: not built).
fn comp_width(db: &Connection, id: i64, wall: i64) -> Result<f64, Error> {
    let bb: Option<(f64, f64, f64, f64)> = db
        .prepare_cached("SELECT x0, y0, x1, y1 FROM comp_qty WHERE comp=?1")?
        .query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .optional()?;
    let ax: Option<(f64, f64, f64, f64)> = db
        .prepare_cached("SELECT ax, ay, bx, by FROM wall WHERE id=?1")?
        .query_row([wall], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .optional()?;
    Ok(match (bb, ax) {
        (Some((x0, y0, x1, y1)), Some((ax, ay, bx, by))) => {
            let (dx, dy) = (bx - ax, by - ay);
            let l = dx.hypot(dy).max(1e-12);
            ((x1 - x0) * dx / l).abs() + ((y1 - y0) * dy / l).abs()
        }
        _ => 0.0,
    })
}

/// Foreign openings of the wall (not in `skip`): (id, start, end).
fn others(db: &Connection, wall: i64, skip: &[Member]) -> Result<Vec<(i64, f64, f64)>, Error> {
    let mut st = db.prepare_cached("SELECT id, along, width FROM opening WHERE wall=?1")?;
    let rows = st.query_map([wall], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, f64>(1)?,
            r.get::<_, f64>(2)?,
        ))
    })?;
    let mut v = vec![];
    for r in rows {
        let (id, a, w) = r?;
        if !skip.contains(&Member::Opening(id)) {
            v.push((id, a, a + w));
        }
    }
    Ok(v)
}

fn text(wall: i64, ms: &[Member]) -> String {
    std::iter::once(wall)
        .chain(ms.iter().map(|m| m.code()))
        .map(|x| x.to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Distribution rules read built component extents (`comp_qty`) in [`relayout`]: while one
/// exists, an operation recomputes the derived tables itself instead of leaving it to a later
/// one (`Model::hold_derived`).
pub(crate) fn any_dist(db: &Connection) -> Result<bool, Error> {
    Ok(db
        .prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM constraint_rule WHERE kind IN ('dist','dist_step'))",
        )?
        .query_row([], |r| r.get(0))?)
}

pub(crate) fn load_dist(db: &Connection) -> Result<Vec<DistRule>, Error> {
    let mut st = db.prepare_cached(
        "SELECT id, kind, walls, value FROM constraint_rule WHERE kind IN ('dist','dist_step') ORDER BY id",
    )?;
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
        let (id, k, w, v) = row?;
        let ids: Vec<i64> = w
            .split_whitespace()
            .map(|t| t.parse::<i64>())
            .collect::<Result<_, _>>()
            .map_err(|_| Error::Invalid(format!("distribution {id}: bad members '{w}'")))?;
        let Some((&wall, ms)) = ids.split_first() else {
            return Err(Error::Invalid(format!("distribution {id}: no wall")));
        };
        let v = v.unwrap_or(0.0);
        let mode = if k == "dist" {
            DistMode::Equal { margin: v }
        } else {
            // the step rule's margin comes from the first live member's position (see `resolve_mode`)
            DistMode::Step {
                margin: 0.0,
                step: v,
            }
        };
        out.push(DistRule {
            id,
            wall,
            members: ms.iter().map(|&c| Member::from_code(c)).collect(),
            mode,
        });
    }
    Ok(out)
}

/// Step rule: margin = start edge of the first live member − k·pitch.
fn resolve_mode(mode: DistMode, geo: &[Option<(i64, f64, f64)>]) -> DistMode {
    match mode {
        DistMode::Step { step, .. } => {
            let m = geo
                .iter()
                .enumerate()
                .find_map(|(k, g)| g.map(|g| g.1 - step * k as f64))
                .unwrap_or(0.0);
            DistMode::Step { margin: m, step }
        }
        m => m,
    }
}

/// Places live members into slots; a conflict on an occupied slot is an error.
fn place(
    db: &Connection,
    mut inv: Inv,
    rule: i64,
    wall: i64,
    members: &[Member],
    mode: DistMode,
    width: f64,
) -> Result<Inv, Error> {
    let len = wall_len(db, wall)?.ok_or_else(|| Error::Invalid(format!("no such wall {wall}")))?;
    let oth = others(db, wall, members)?;
    let slots = layout(len, width, members.len(), mode, &oth);
    let mut prev: Option<f64> = None;
    for (i, (m, s)) in members.iter().zip(&slots).enumerate() {
        if *m == Member::Empty {
            continue;
        }
        // overlap is checked only between live neighbours: empty slots do not interfere
        let conflict = match s.conflict {
            Some(SlotConflict::Overlap) if prev.is_none_or(|p| s.along - p >= width - EPS) => oth
                .iter()
                .find(|o| s.along < o.2 - EPS && s.along + width > o.1 + EPS)
                .map(|o| SlotConflict::Opening(o.0)),
            c => c,
        };
        prev = Some(s.along);
        if let Some(c) = conflict {
            let why = match c {
                SlotConflict::OffWall => "off the wall".to_string(),
                SlotConflict::Overlap => "members overlap".to_string(),
                SlotConflict::Opening(o) => format!("hits opening #{o}"),
            };
            return Err(Error::Conflict(format!(
                "edit breaks distribution #{rule} on wall #{wall}: slot {} {why}",
                i + 1
            )));
        }
        match *m {
            Member::Opening(id) => {
                let cur: f64 =
                    db.query_row("SELECT along FROM opening WHERE id=?1", [id], |r| r.get(0))?;
                if (cur - s.along).abs() > EPS {
                    inv = inv.restore(db, "opening", "id", id)?;
                    db.execute(
                        "UPDATE opening SET along=?2 WHERE id=?1",
                        params![id, s.along],
                    )?;
                }
            }
            Member::Comp(id) => {
                let cur: f64 =
                    db.query_row("SELECT x FROM comp WHERE id=?1", [id], |r| r.get(0))?;
                let x = s.along + width / 2.0;
                if (cur - x).abs() > EPS {
                    inv = inv.restore(db, "comp", "id", id)?;
                    db.execute("UPDATE comp SET x=?2 WHERE id=?1", params![id, x])?;
                }
            }
            Member::Empty => {}
        }
    }
    Ok(inv)
}

/// Re-lays out all linked distributions after an edit. A deleted wall removes the rule,
/// a deleted member frees its slot; members moved to another wall (split) drop out, trailing
/// empty slots are removed; if all moved to one wall, the rule moves to that wall.
pub(crate) fn relayout(db: &Connection) -> Result<Inv, Error> {
    let mut inv = Inv::default();
    for mut r in load_dist(db)? {
        if wall_len(db, r.wall)?.is_none() {
            inv = inv.restore(db, "constraint_rule", "id", r.id)?;
            db.execute("DELETE FROM constraint_rule WHERE id=?1", [r.id])?;
            continue;
        }
        let raw: Vec<_> = r
            .members
            .iter()
            .map(|&m| member_geom(db, m))
            .collect::<Result<_, _>>()?;
        let mut changed = false;
        // a wall split moved all live members to one new wall: the rule follows them
        let hosts: Vec<i64> = raw.iter().flatten().map(|g| g.0).collect();
        if !hosts.is_empty() && !hosts.contains(&r.wall) && hosts.iter().all(|&h| h == hosts[0]) {
            r.wall = hosts[0];
            changed = true;
        }
        let mut geo = vec![];
        let mut moved = false;
        for (m, g) in r.members.iter_mut().zip(raw) {
            if *m == Member::Empty {
                geo.push(None);
                continue;
            }
            match g {
                Some(g) if g.0 == r.wall => geo.push(Some(g)),
                g => {
                    moved |= g.is_some();
                    *m = Member::Empty;
                    changed = true;
                    geo.push(None);
                }
            }
        }
        // members that moved to another wall (split) leave no empty slots at the edges
        if moved {
            let a = r
                .members
                .iter()
                .position(|m| *m != Member::Empty)
                .unwrap_or(0);
            let b = r
                .members
                .iter()
                .rposition(|m| *m != Member::Empty)
                .map_or(0, |b| b + 1);
            if a < b {
                r.members = r.members[a..b].to_vec();
                geo = geo[a..b].to_vec();
            }
        }
        // flipping a wall reverses the members: slot order follows them
        let live: Vec<f64> = geo.iter().flatten().map(|g| g.1).collect();
        if live.len() > 1 && live.windows(2).all(|w| w[1] < w[0]) {
            r.members.reverse();
            geo.reverse();
            changed = true;
        }
        if r.members.iter().all(|m| *m == Member::Empty) {
            inv = inv.restore(db, "constraint_rule", "id", r.id)?;
            db.execute("DELETE FROM constraint_rule WHERE id=?1", [r.id])?;
            continue;
        }
        if changed {
            inv = inv.restore(db, "constraint_rule", "id", r.id)?;
            db.execute(
                "UPDATE constraint_rule SET walls=?2 WHERE id=?1",
                params![r.id, text(r.wall, &r.members)],
            )?;
        }
        let width = geo.iter().flatten().next().map_or(0.0, |g| g.2);
        let mode = resolve_mode(r.mode, &geo);
        inv = place(db, inv, r.id, r.wall, &r.members, mode, width)?;
    }
    Ok(inv)
}

/// Copy of a `table` row (all columns except id) with a new id.
fn duplicate(db: &Connection, table: &str, id: i64) -> Result<i64, Error> {
    let cols: Vec<String> = {
        let mut st = db.prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))?;
        let v = st
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        v.into_iter().filter(|c| c != "id").collect()
    };
    let c = cols.join(",");
    let n = db.execute(
        &format!("INSERT INTO {table}({c}) SELECT {c} FROM {table} WHERE id=?1"),
        [id],
    )?;
    if n == 0 {
        return Err(Error::Invalid(format!("no such {table} {id}")));
    }
    Ok(db.last_insert_rowid())
}

fn source(t: Target) -> Result<Member, Error> {
    match t {
        Target::Opening(i) => Ok(Member::Opening(i)),
        Target::Component(i) => Ok(Member::Comp(i)),
        _ => Err(Error::Invalid(
            "distribute: a door, window or wall-hosted component is required".into(),
        )),
    }
}

fn check(n: usize, mode: DistMode) -> Result<(), Error> {
    if n == 0 || n > 500 {
        return Err(Error::Invalid("distribute: count must be 1..500".into()));
    }
    if !mode.ok() {
        return Err(Error::Invalid(
            "distribute: margin must be ≥ 0, step > 0, finite".into(),
        ));
    }
    Ok(())
}

fn rule_of(db: &Connection, m: Member) -> Result<Option<DistRule>, Error> {
    Ok(load_dist(db)?.into_iter().find(|r| r.members.contains(&m)))
}

fn write_rule(
    db: &Connection,
    id: Option<i64>,
    wall: i64,
    ms: &[Member],
    mode: DistMode,
) -> Result<i64, Error> {
    let (k, v) = match mode {
        DistMode::Equal { margin } => ("dist", margin),
        DistMode::Step { step, .. } => ("dist_step", step),
    };
    match id {
        Some(id) => {
            db.execute(
                "UPDATE constraint_rule SET kind=?2, walls=?3, value=?4 WHERE id=?1",
                params![id, k, text(wall, ms), v],
            )?;
            Ok(id)
        }
        None => {
            db.execute(
                "INSERT INTO constraint_rule(kind, walls, value) VALUES (?1, ?2, ?3)",
                params![k, text(wall, ms), v],
            )?;
            Ok(db.last_insert_rowid())
        }
    }
}

/// Members, skips and the inverse after [`fill`].
type Filled = (Vec<Member>, Vec<(usize, SlotConflict)>, Inv);

/// Fills slots: existing members stay in their slots, free conflict-free slots
/// get copies of the template. Returns the members and skips.
fn fill(
    db: &Connection,
    keep: &[Member],
    template: Member,
    slots: &[Slot],
) -> Result<Filled, Error> {
    let (mut ms, mut skipped, mut inv) = (vec![], vec![], Inv::default());
    for (i, s) in slots.iter().enumerate() {
        let have = keep.get(i).copied().unwrap_or(Member::Empty);
        if have != Member::Empty {
            ms.push(have);
            continue;
        }
        if let Some(c) = s.conflict {
            skipped.push((i, c));
            ms.push(Member::Empty);
            continue;
        }
        let m = match template {
            Member::Opening(id) => {
                let n = duplicate(db, "opening", id)?;
                inv = inv.delete("opening", "id", n);
                Member::Opening(n)
            }
            Member::Comp(id) => {
                let n = duplicate(db, "comp", id)?;
                inv = inv.delete("comp", "id", n);
                Member::Comp(n)
            }
            Member::Empty => Member::Empty,
        };
        ms.push(m);
    }
    Ok((ms, skipped, inv))
}

impl Model {
    /// Preview: host wall, element width and slots (no write to the model).
    pub fn distribution_preview(
        &self,
        src: Target,
        n: usize,
        mode: DistMode,
    ) -> Result<(i64, f64, Vec<Slot>), Error> {
        check(n, mode)?;
        let m = source(src)?;
        let (wall, _, width) = member_geom(&self.db, m)?
            .ok_or_else(|| Error::Invalid("distribute: element has no host wall".into()))?;
        let len = wall_len(&self.db, wall)?.unwrap_or(0.0);
        let skip = match rule_of(&self.db, m)? {
            Some(r) => r.members,
            None => vec![m],
        };
        Ok((
            wall,
            width,
            layout(len, width, n, mode, &others(&self.db, wall, &skip)?),
        ))
    }

    /// Distributes `src` along its wall: `n` slots, `src` goes into the first free one, the other
    /// free ones get copies; conflicting slots are skipped. `link` is the linked rule (EQ).
    /// One operation (one undo step).
    pub fn distribute(
        &mut self,
        src: Target,
        n: usize,
        mode: DistMode,
        link: bool,
    ) -> Result<Distribution, Error> {
        check(n, mode)?;
        let m = source(src)?;
        if rule_of(&self.db, m)?.is_some() {
            return Err(Error::Invalid(
                "element is already distributed: edit the distribution instead".into(),
            ));
        }
        let (wall, width, slots) = self.distribution_preview(src, n, mode)?;
        let Some(first) = slots.iter().position(|s| s.conflict.is_none()) else {
            return Err(Error::Conflict(
                "distribute: no free slot on the wall".into(),
            ));
        };
        self.op("wall_distribute", |db| {
            let mut inv = match m {
                Member::Opening(id) => Inv::default().restore(db, "opening", "id", id)?,
                Member::Comp(id) => Inv::default().restore(db, "comp", "id", id)?,
                Member::Empty => Inv::default(),
            };
            let mut keep = vec![Member::Empty; n];
            keep[first] = m;
            let (ms, skipped, i2) = fill(db, &keep, m, &slots)?;
            inv = i2.then(inv);
            let rule = if link {
                let id = write_rule(db, None, wall, &ms, mode)?;
                inv = inv.delete("constraint_rule", "id", id);
                Some(id)
            } else {
                None
            };
            // placement snapshots go first: undo restores the intermediate state, then the original
            let inv = place(
                db,
                Inv::default(),
                rule.unwrap_or(0),
                wall,
                &ms,
                mode,
                width,
            )?
            .then(inv);
            Ok((
                Distribution {
                    rule,
                    members: ms,
                    skipped,
                },
                wall,
                inv,
            ))
        })
    }

    /// The linked distribution that contains the element.
    pub fn distribution_of(&self, t: Target) -> Result<Option<DistRule>, Error> {
        let Ok(m) = source(t) else { return Ok(None) };
        let Some(mut r) = rule_of(&self.db, m)? else {
            return Ok(None);
        };
        let geo: Vec<_> = r
            .members
            .iter()
            .map(|&m| member_geom(&self.db, m))
            .collect::<Result<_, _>>()?;
        r.mode = resolve_mode(r.mode, &geo);
        Ok(Some(r))
    }

    /// Linked distribution by id (None: removed).
    pub fn distribution(&self, rule: i64) -> Result<Option<DistRule>, Error> {
        Ok(load_dist(&self.db)?.into_iter().find(|r| r.id == rule))
    }

    /// New slot count and mode of a linked distribution: extra members are removed from the end,
    /// new slots get copies of the first member. One operation.
    pub fn set_distribution(&mut self, rule: i64, n: usize, mode: DistMode) -> Result<(), Error> {
        check(n, mode)?;
        let r = load_dist(&self.db)?
            .into_iter()
            .find(|r| r.id == rule)
            .ok_or_else(|| Error::Invalid(format!("no such distribution {rule}")))?;
        let mut keep = r.members.clone();
        let mut tail: Vec<Member> = keep.drain(n.min(keep.len())..).collect();
        // live members only in the discarded tail: the first of them stays as the template in slot 1
        if keep.iter().all(|m| *m == Member::Empty) {
            if let Some(k) = tail.iter().position(|m| *m != Member::Empty) {
                keep[0] = tail.remove(k);
            }
        }
        let template = *keep
            .iter()
            .find(|m| **m != Member::Empty)
            .ok_or_else(|| Error::Invalid("distribution has no members".into()))?;
        let (_, width, _) = member_geom(&self.db, template)?
            .ok_or_else(|| Error::Invalid("distribution member lost its wall".into()))?;
        let len = wall_len(&self.db, r.wall)?.unwrap_or(0.0);
        self.op("wall_distribute_edit", |db| {
            let mut inv = Inv::default().restore(db, "constraint_rule", "id", rule)?;
            for &m in &tail {
                match m {
                    Member::Opening(id) => {
                        inv = inv.restore(db, "opening", "id", id)?;
                        db.execute("DELETE FROM opening WHERE id=?1", [id])?;
                    }
                    Member::Comp(id) => {
                        inv = inv.restore(db, "comp", "id", id)?;
                        db.execute("DELETE FROM comp WHERE id=?1", [id])?;
                        db.execute("DELETE FROM comp_qty WHERE comp=?1", [id])?;
                    }
                    Member::Empty => {}
                }
            }
            let mut slots = layout(len, width, n, mode, &others(db, r.wall, &keep)?);
            for (i, s) in slots.iter_mut().enumerate() {
                // own members do not block their own slots: `place` checks conflicts
                if keep.get(i).is_some_and(|m| *m != Member::Empty) {
                    s.conflict = None;
                }
            }
            let (ms, _, i2) = fill(db, &keep, template, &slots)?;
            let inv = i2.then(inv);
            write_rule(db, Some(rule), r.wall, &ms, mode)?;
            let inv = place(db, Inv::default(), rule, r.wall, &ms, mode, width)?.then(inv);
            Ok(((), r.wall, inv))
        })
    }

    /// Break the link: members stay in place, the rule is removed.
    pub fn release_distribution(&mut self, rule: i64) -> Result<(), Error> {
        self.op("constraint_delete", |db| {
            let inv = Inv::default().restore(db, "constraint_rule", "id", rule)?;
            let n = db.execute(
                "DELETE FROM constraint_rule WHERE id=?1 AND kind IN ('dist','dist_step')",
                [rule],
            )?;
            if n == 0 {
                return Err(Error::Invalid(format!("no such distribution {rule}")));
            }
            Ok(((), rule, inv))
        })
    }
}

#[cfg(test)]
#[path = "constraints_dist_tests.rs"]
mod tests;
