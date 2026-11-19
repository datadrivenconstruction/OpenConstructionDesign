//! Rooms that lose their enclosure while walls or room separators are edited. One check is
//! shared by the editor (status, badge, warnings list) and the AI plan validator.
//! Also: a status note when an edit puts a second room into a region another room already
//! holds; the regenerate counts the region once, in the lower room, and the note names both.
use crate::editor::Editor;
use crate::status::Severity;
use ddc_model::{Changes, ElementWarning, Error, Model};
use std::collections::{BTreeMap, BTreeSet};

/// Tables whose edits can change a room's boundary.
const WATCHED: [&str; 3] = ["wall", "room_sep", "room"];

/// Prefix of the model-side message; `shell::warn::warning_text` localizes it.
pub const MSG: &str = "room open: ";

#[derive(Clone, Debug, PartialEq)]
pub struct RoomState {
    pub id: i64,
    pub level: i64,
    pub name: String,
    pub number: String,
    pub enclosed: bool,
}

impl RoomState {
    /// The number, else the name, else "#id".
    pub fn label(&self) -> String {
        if !self.number.is_empty() {
            self.number.clone()
        } else if !self.name.is_empty() {
            self.name.clone()
        } else {
            format!("#{}", self.id)
        }
    }
}

/// A room is enclosed when the walls and separators of its level give it a measured area.
pub fn is_enclosed(m: &Model, room: i64) -> bool {
    m.sql()
        .query_row("SELECT area FROM room_qty WHERE room=?1", [room], |r| {
            r.get::<_, Option<f64>>(0)
        })
        .ok()
        .flatten()
        .unwrap_or(0.0)
        > 0.0
}

/// Rooms of the given levels (`None` = every level) with their enclosure.
pub fn rooms_on(m: &Model, levels: Option<&[i64]>) -> Result<Vec<RoomState>, Error> {
    let mut st = m.sql().prepare(
        "SELECT r.id, r.level, r.name, r.number, COALESCE(q.area, 0) FROM room r \
         LEFT JOIN room_qty q ON q.room=r.id ORDER BY r.id",
    )?;
    let rows = st.query_map([], |r| {
        Ok(RoomState {
            id: r.get(0)?,
            level: r.get(1)?,
            name: r.get(2)?,
            number: r.get(3)?,
            enclosed: r.get::<_, f64>(4)? > 0.0,
        })
    })?;
    let mut out = vec![];
    for r in rows {
        let r = r?;
        if levels.map_or(true, |l| l.contains(&r.level)) {
            out.push(r);
        }
    }
    Ok(out)
}

/// Levels with changed walls, separators or rooms; `None` when it cannot be told (deleted rows,
/// lost journal window), which means every level.
pub fn touched_levels(m: &Model, ch: &Changes) -> Option<Vec<i64>> {
    if ch.all {
        return None;
    }
    let mut out = BTreeSet::new();
    for t in WATCHED {
        if ch.whole(t) {
            return None;
        }
        for id in ch.ids(t) {
            let lv: Option<i64> = m
                .sql()
                .query_row(&format!("SELECT level FROM {t} WHERE id=?1"), [id], |r| {
                    r.get(0)
                })
                .ok();
            out.insert(lv?);
        }
    }
    Some(out.into_iter().collect())
}

/// Per-editor state: rooms seen enclosed, and rooms that lost it since.
#[derive(Default)]
pub struct Watch {
    head: Option<u64>,
    seen_closed: BTreeSet<i64>,
    /// Room id -> label of every room flagged as no longer enclosed.
    open: BTreeMap<i64, String>,
    /// Labels of rooms opened by the last reload, until the step's status is settled.
    pending: Option<String>,
    /// Labels the last settle folded into the status: a status the caller sets afterwards
    /// gets the note once more on the next settle; the next reload forgets it.
    written: Option<String>,
}

impl Watch {
    #[cfg(test)]
    pub(crate) fn set_pending_for_test(&mut self, labels: &str) {
        self.pending = Some(labels.into());
    }

    /// The pending labels, whatever step they belong to; the step's end calls this.
    pub fn take_pending(&mut self) -> Option<String> {
        self.pending.take()
    }

    /// Labels written into the status by the last settle, once.
    pub fn take_written(&mut self) -> Option<String> {
        self.written.take()
    }

    pub fn remember_written(&mut self, labels: String) {
        self.written = Some(labels);
    }

    pub fn is_flagged(&self, room: i64) -> bool {
        self.open.contains_key(&room)
    }

    pub fn flagged(&self) -> impl Iterator<Item = (i64, &str)> {
        self.open.iter().map(|(k, v)| (*k, v.as_str()))
    }

    /// Element warnings for the warnings list and the properties badge.
    pub fn warnings(&self) -> Vec<ElementWarning> {
        self.open
            .iter()
            .map(|(id, label)| ElementWarning {
                table: "room",
                id: *id,
                message: format!("{MSG}{label}"),
            })
            .collect()
    }
}

/// Called after every model reload: re-checks only the levels touched since the last call.
pub fn after_reload(ed: &mut Editor) {
    let head = ed.model.change_head();
    let (levels, silent) = match ed.room_watch.head {
        None => (None, true),
        Some(h) => {
            let ch = ed.model.changes_since(h);
            if ch.all {
                // journal window lost or another model: start over without noise
                ed.room_watch = Watch::default();
                (None, true)
            } else if !WATCHED.iter().any(|t| ch.touched(t)) {
                ed.room_watch.head = Some(head);
                return;
            } else {
                (touched_levels(&ed.model, &ch), false)
            }
        }
    };
    let Ok(rooms) = rooms_on(&ed.model, levels.as_deref()) else {
        return;
    };
    let alive: BTreeSet<i64> = ed
        .model
        .sql()
        .prepare("SELECT id FROM room")
        .and_then(|mut s| s.query_map([], |r| r.get(0))?.collect())
        .unwrap_or_default();
    let w = &mut ed.room_watch;
    w.head = Some(head);
    w.written = None;
    w.pending = None;
    w.seen_closed.retain(|id| alive.contains(id));
    w.open.retain(|id, _| alive.contains(id));
    let mut fresh = vec![];
    for r in rooms {
        if r.enclosed {
            w.seen_closed.insert(r.id);
            w.open.remove(&r.id);
        } else if w.seen_closed.contains(&r.id) && !w.open.contains_key(&r.id) {
            w.open.insert(r.id, r.label());
            fresh.push(r.label());
        }
    }
    if !silent && !fresh.is_empty() {
        let labels = fresh.join(", ");
        let text = crate::tr("room_status.open", &[("room", &labels)]);
        ed.say(Severity::Warning, text);
        // a status the caller assigns later replaces the line; the step's end puts it back
        ed.room_watch.pending = Some(labels);
    }
}

/// (room, room that keeps the quantities) pairs before the edit.
pub type Dups = Vec<(i64, i64)>;

pub fn snapshot(m: &Model) -> Dups {
    m.room_duplicates().unwrap_or_default()
}

/// Number and name of the room, or its id when both are empty.
fn label(m: &Model, id: i64) -> String {
    let s = m
        .sql()
        .query_row(
            "SELECT trim(number || ' ' || name) FROM room WHERE id=?1",
            [id],
            |r| r.get::<_, String>(0),
        )
        .unwrap_or_default();
    if s.is_empty() {
        id.to_string()
    } else {
        s
    }
}

/// Text for the first pair the edit created; `None` when there is none.
pub fn text(m: &Model, before: &Dups) -> Option<String> {
    let (room, other) = snapshot(m).into_iter().find(|p| !before.contains(p))?;
    let (room, other) = (label(m, room), label(m, other));
    Some(crate::tr(
        "room_watch.room.dup",
        &[("room", &room), ("other", &other)],
    ))
}

/// Appends the note to the status line.
pub fn note(status: &mut Option<String>, m: &Model, before: &Dups) {
    if let Some(t) = text(m, before) {
        *status = Some(match status.take() {
            Some(s) if !s.is_empty() => format!("{s} \u{b7} {t}"),
            _ => t,
        });
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::editor::Sel;
    use ddc_model::{RoomSpec, WallSpec};

    pub(crate) fn square_room() -> (Editor, Vec<i64>, i64) {
        let mut e = Editor::new_in_memory().unwrap();
        let t = e.types[1].id;
        let mut walls = vec![];
        for (a, b) in [
            ((0.0, 0.0), (6.0, 0.0)),
            ((6.0, 0.0), (6.0, 4.0)),
            ((6.0, 4.0), (0.0, 4.0)),
            ((0.0, 4.0), (0.0, 0.0)),
        ] {
            walls.push(
                e.model
                    .add_wall(&WallSpec {
                        a,
                        b,
                        level: e.level,
                        base_offset: 0.0,
                        height: 3.0,
                        wall_type: t,
                        offset0: None,
                    })
                    .unwrap(),
            );
        }
        let room = e
            .model
            .add_room(&RoomSpec {
                level: e.level,
                name: "Hall".into(),
                number: "101".into(),
                point: (3.0, 2.0),
                height: 2.7,
            })
            .unwrap();
        e.reload().unwrap();
        (e, walls, room)
    }

    #[test]
    fn delete_one_of_four_walls_flags_room() {
        let (mut e, walls, room) = square_room();
        assert!(!e.room_watch.is_flagged(room));
        e.selection = vec![Sel::Wall(walls[0])];
        e.delete_selection();
        assert!(e.room_watch.is_flagged(room));
        let ws = e.room_watch.warnings();
        assert_eq!((ws[0].table, ws[0].id), ("room", room));
        assert!(ws[0].message.ends_with("101"));
    }

    #[test]
    fn wall_move_keeping_closed_no_flag() {
        let (mut e, walls, room) = square_room();
        e.edit_wall(walls[1], None, None, Some(3.5));
        assert!(!e.room_watch.is_flagged(room));
        assert_eq!(e.room_watch.flagged().count(), 0);
    }

    #[test]
    fn undo_clears_flag() {
        let (mut e, walls, room) = square_room();
        e.selection = vec![Sel::Wall(walls[2])];
        e.delete_selection();
        assert!(e.room_watch.is_flagged(room));
        e.undo();
        assert!(!e.room_watch.is_flagged(room));
        e.redo();
        assert!(e.room_watch.is_flagged(room));
    }

    #[test]
    fn opened_model_does_not_inherit_flags() {
        let (mut e, _, room) = square_room();
        // the other model: same room id, open, with a longer change journal
        let (mut other, walls, _) = square_room();
        other.selection = vec![Sel::Wall(walls[0])];
        other.delete_selection();
        e.replace_model(other.model).unwrap();
        assert!(!e.room_watch.is_flagged(room));
    }
}

#[cfg(test)]
mod dup_tests {
    use super::*;
    use ddc_model::{LayerSpec, RoomSpec, WallSpec};

    fn room(m: &mut Model, lv: i64, number: &str, p: (f64, f64)) -> i64 {
        m.add_room(&RoomSpec {
            level: lv,
            name: "Office".into(),
            number: number.into(),
            point: p,
            height: 3.0,
        })
        .unwrap()
    }

    #[test]
    fn second_room_in_a_held_region_names_both_rooms() {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m
            .add_wall_type(
                "w",
                &[LayerSpec {
                    material: "brick".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let p = [(0., 0.), (6., 0.), (6., 4.), (0., 4.)];
        let specs: Vec<WallSpec> = (0..4)
            .map(|i| WallSpec {
                a: p[i],
                b: p[(i + 1) % 4],
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .collect();
        m.add_walls(&specs).unwrap();
        room(&mut m, lv, "101", (3., 2.));
        let before = snapshot(&m);
        assert!(text(&m, &before).is_none());
        room(&mut m, lv, "102", (1., 1.));
        let mut st = Some("done".to_string());
        note(&mut st, &m, &before);
        let st = st.unwrap();
        assert!(st.starts_with("done"), "{st}");
        assert!(st.contains("102 Office") && st.contains("101 Office"), "{st}");
        // a known pair is not told again
        let now = snapshot(&m);
        assert!(text(&m, &now).is_none());
    }
}

#[cfg(test)]
mod fuzz;
