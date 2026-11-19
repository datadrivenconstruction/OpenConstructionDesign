// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Zones (schema 32): named groups of rooms (fire compartment, climate zone, etc.).
//! A room belongs to at most one zone; deleting a room removes it from its zone, and deleting
//! the last room deletes the zone too.

use rusqlite::{params, Connection, OptionalExtension};

use crate::ops::Inv;
use crate::{Error, Model};

const DDL: &str = "\
CREATE TABLE IF NOT EXISTS zone(id INTEGER PRIMARY KEY, name TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS zone_room(room INTEGER PRIMARY KEY, zone INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS zone_room_zone ON zone_room(zone);";

pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    db.execute_batch(DDL)?;
    Ok(())
}

#[derive(Clone, Debug, PartialEq)]
pub struct ZoneSpec {
    pub name: String,
    /// Rooms in ascending id order.
    pub rooms: Vec<i64>,
}

fn invalid<T>(m: String) -> Result<T, Error> {
    Err(Error::Invalid(m))
}

fn check(db: &Connection, s: &ZoneSpec, id: Option<i64>) -> Result<(), Error> {
    if s.name.trim().is_empty() {
        return invalid("zone: empty name".into());
    }
    if s.rooms.is_empty() {
        return invalid("zone: no rooms".into());
    }
    for r in &s.rooms {
        let n: i64 = db.query_row("SELECT count(*) FROM room WHERE id=?1", [r], |x| x.get(0))?;
        if n == 0 {
            return invalid(format!("zone: no such room {r}"));
        }
        let other: Option<i64> = db
            .query_row("SELECT zone FROM zone_room WHERE room=?1", [r], |x| {
                x.get(0)
            })
            .optional()?;
        if other.is_some_and(|z| Some(z) != id) {
            return invalid(format!("zone: room {r} is already in a zone"));
        }
    }
    Ok(())
}

fn write_rooms(db: &Connection, id: i64, rooms: &[i64]) -> Result<(), Error> {
    db.execute("DELETE FROM zone_room WHERE zone=?1", [id])?;
    for r in rooms {
        db.execute(
            "INSERT OR REPLACE INTO zone_room(room,zone) VALUES (?1,?2)",
            params![r, id],
        )?;
    }
    Ok(())
}

pub(crate) fn rooms_of(db: &Connection, id: i64) -> Result<Vec<i64>, Error> {
    let mut st = db.prepare_cached("SELECT room FROM zone_room WHERE zone=?1 ORDER BY room")?;
    let v = st
        .query_map([id], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(v)
}

/// Removes a room from its zone within the room-deletion operation; a zone left empty is deleted with it.
pub(crate) fn drop_room(db: &Connection, inv: Inv, room: i64) -> Result<Inv, Error> {
    let zone = zone_of(db, room)?;
    let mut inv = inv.restore(db, "zone_room", "room", room)?;
    db.execute("DELETE FROM zone_room WHERE room=?1", [room])?;
    if let Some(z) = zone.filter(|z| is_empty(db, *z).unwrap_or(false)) {
        inv = inv.restore(db, "zone", "id", z)?;
        db.execute("DELETE FROM zone WHERE id=?1", [z])?;
    }
    Ok(inv)
}

/// The zone of a room.
pub(crate) fn zone_of(db: &Connection, room: i64) -> Result<Option<i64>, Error> {
    Ok(db
        .query_row("SELECT zone FROM zone_room WHERE room=?1", [room], |r| {
            r.get(0)
        })
        .optional()?)
}

/// The zone has no rooms left.
pub(crate) fn is_empty(db: &Connection, zone: i64) -> Result<bool, Error> {
    Ok(rooms_of(db, zone)?.is_empty())
}

impl Model {
    pub fn add_zone(&mut self, s: &ZoneSpec) -> Result<i64, Error> {
        check(&self.db, s, None)?;
        self.op("zone_add", |db| {
            db.execute("INSERT INTO zone(name) VALUES (?1)", [s.name.trim()])?;
            let id = db.last_insert_rowid();
            write_rooms(db, id, &s.rooms)?;
            let inv = Inv::default()
                .delete("zone_room", "zone", id)
                .delete("zone", "id", id);
            Ok((id, id, inv))
        })
    }

    pub fn update_zone(&mut self, id: i64, s: &ZoneSpec) -> Result<(), Error> {
        check(&self.db, s, Some(id))?;
        self.op("zone_edit", |db| {
            // rollback runs in order: first remove the new membership, then restore the old one
            let inv = Inv::default()
                .delete("zone_room", "zone", id)
                .restore(db, "zone_room", "zone", id)?
                .restore(db, "zone", "id", id)?;
            if db.execute(
                "UPDATE zone SET name=?1 WHERE id=?2",
                params![s.name.trim(), id],
            )? == 0
            {
                return invalid(format!("no such zone {id}"));
            }
            write_rooms(db, id, &s.rooms)?;
            Ok(((), id, inv))
        })
    }

    pub fn delete_zone(&mut self, id: i64) -> Result<(), Error> {
        self.delete_ref("zone", "zone_delete", id, &[("zone_room", "zone")])
    }

    pub fn zone(&self, id: i64) -> Result<ZoneSpec, Error> {
        let name: Option<String> = self
            .db
            .query_row("SELECT name FROM zone WHERE id=?1", [id], |r| r.get(0))
            .optional()?;
        let Some(name) = name else {
            return invalid(format!("no such zone {id}"));
        };
        Ok(ZoneSpec {
            name,
            rooms: rooms_of(&self.db, id)?,
        })
    }

    /// All zones in ascending id order.
    pub fn zones(&self) -> Result<Vec<(i64, ZoneSpec)>, Error> {
        let ids: Vec<i64> = {
            let mut st = self.db.prepare("SELECT id FROM zone ORDER BY id")?;
            let v = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
            v
        };
        ids.into_iter().map(|id| Ok((id, self.zone(id)?))).collect()
    }

    /// The zone of a room.
    pub fn zone_of_room(&self, room: i64) -> Result<Option<i64>, Error> {
        zone_of(&self.db, room)
    }

    /// Zones without rooms (old files, external edits): visible only in the manager.
    pub fn zone_warnings(&self) -> Result<Vec<crate::ElementWarning>, Error> {
        Ok(self
            .zones()?
            .into_iter()
            .filter(|(_, z)| z.rooms.is_empty())
            .map(|(id, _)| crate::ElementWarning {
                table: "zone",
                id,
                message: "zone has no rooms".into(),
            })
            .collect())
    }

    /// Zone area: the sum of its room areas.
    pub fn zone_area(&self, id: i64) -> Result<f64, Error> {
        Ok(self.db.query_row(
            "SELECT COALESCE(SUM(q.area),0) FROM zone_room z JOIN room_qty q ON q.room=z.room \
             WHERE z.zone=?1",
            [id],
            |r| r.get(0),
        )?)
    }

    /// Number for the auto-name "`base` N": one more than the zone count; taken names are skipped.
    pub fn next_zone_number(&self, base: &str) -> Result<i64, Error> {
        let names: Vec<String> = self.zones()?.into_iter().map(|(_, z)| z.name).collect();
        let mut n = names.len() as i64 + 1;
        while names.contains(&format!("{base} {n}")) {
            n += 1;
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RoomSpec;

    fn model() -> (Model, i64, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_level("L1", 0.0).unwrap();
        let room = |m: &mut Model, x: f64| {
            m.add_room(&RoomSpec {
                level: l,
                name: "R".into(),
                number: "1".into(),
                point: (x, 0.0),
                height: 2.7,
            })
        };
        let a = room(&mut m, 1.0).unwrap();
        let b = room(&mut m, 2.0).unwrap();
        (m, a, b)
    }

    fn zone(name: &str, rooms: &[i64]) -> ZoneSpec {
        ZoneSpec {
            name: name.into(),
            rooms: rooms.to_vec(),
        }
    }

    #[test]
    fn add_reads_back_and_undo_clears() {
        let (mut m, a, b) = model();
        let id = m.add_zone(&zone("Z1", &[a, b])).unwrap();
        assert_eq!(m.zone(id).unwrap(), zone("Z1", &[a, b]));
        assert_eq!(m.zone_of_room(b).unwrap(), Some(id));
        m.undo().unwrap();
        assert!(m.zone(id).is_err());
        assert_eq!(m.zone_of_room(b).unwrap(), None);
    }

    #[test]
    fn room_belongs_to_one_zone() {
        let (mut m, a, b) = model();
        m.add_zone(&zone("Z1", &[a])).unwrap();
        assert!(m.add_zone(&zone("Z2", &[a, b])).is_err());
        assert_eq!(m.zones().unwrap().len(), 1);
        assert!(m.add_zone(&zone(" ", &[b])).is_err());
        assert!(m.add_zone(&zone("Z2", &[])).is_err());
        assert!(m.add_zone(&zone("Z2", &[9999])).is_err());
    }

    #[test]
    fn edit_undo_restores_old_rooms() {
        let (mut m, a, b) = model();
        let id = m.add_zone(&zone("Z1", &[a])).unwrap();
        m.update_zone(id, &zone("Z2", &[b])).unwrap();
        assert_eq!(m.zone(id).unwrap(), zone("Z2", &[b]));
        assert_eq!(m.zone_of_room(a).unwrap(), None);
        m.undo().unwrap();
        assert_eq!(m.zone(id).unwrap(), zone("Z1", &[a]));
        assert_eq!(m.zone_of_room(b).unwrap(), None);
    }

    #[test]
    fn delete_zone_keeps_rooms_and_undo_restores() {
        let (mut m, a, b) = model();
        let id = m.add_zone(&zone("Z1", &[a, b])).unwrap();
        m.delete_zone(id).unwrap();
        assert!(m.zone(id).is_err());
        assert_eq!(m.zone_of_room(a).unwrap(), None);
        assert!(m.check_integrity().unwrap().is_empty());
        m.undo().unwrap();
        assert_eq!(m.zone(id).unwrap(), zone("Z1", &[a, b]));
    }

    #[test]
    fn deleting_room_leaves_zone_clean_and_undo_returns_it() {
        let (mut m, a, b) = model();
        let id = m.add_zone(&zone("Z1", &[a, b])).unwrap();
        m.delete_room(a).unwrap();
        assert_eq!(m.zone(id).unwrap().rooms, vec![b]);
        assert!(m.check_integrity().unwrap().is_empty());
        m.undo().unwrap();
        assert_eq!(m.zone(id).unwrap().rooms, vec![a, b]);
    }

    #[test]
    fn next_number_skips_taken_names() {
        let (mut m, a, b) = model();
        assert_eq!(m.next_zone_number("Z").unwrap(), 1);
        m.add_zone(&zone("Z 2", &[a])).unwrap();
        assert_eq!(m.next_zone_number("Z").unwrap(), 3, "2 is taken");
        let id = m.add_zone(&zone("Z 3", &[b])).unwrap();
        assert_eq!(m.next_zone_number("Z").unwrap(), 4);
        m.delete_zone(id).unwrap();
        assert_eq!(m.next_zone_number("Z").unwrap(), 3);
    }

    #[test]
    fn deleting_last_room_deletes_the_zone_and_undo_returns_both() {
        let (mut m, a, b) = model();
        let id = m.add_zone(&zone("Z1", &[a, b])).unwrap();
        m.delete_room(a).unwrap();
        m.delete_room(b).unwrap();
        assert!(m.zone(id).is_err(), "no undeletable empty zone");
        assert!(m.zones().unwrap().is_empty());
        assert!(m.zone_warnings().unwrap().is_empty());
        assert!(m.check_integrity().unwrap().is_empty());
        m.undo().unwrap();
        assert_eq!(m.zone(id).unwrap(), zone("Z1", &[b]));
        m.undo().unwrap();
        assert_eq!(m.zone(id).unwrap(), zone("Z1", &[a, b]));
    }

    #[test]
    fn bulk_room_delete_also_removes_the_emptied_zone() {
        let (mut m, a, b) = model();
        let id = m.add_zone(&zone("Z1", &[a, b])).unwrap();
        m.apply_edits(&[crate::Edit::DeleteRoom(a), crate::Edit::DeleteRoom(b)])
            .unwrap();
        assert!(m.zone(id).is_err());
        assert!(m.check_integrity().unwrap().is_empty());
        m.undo().unwrap();
        assert_eq!(m.zone(id).unwrap(), zone("Z1", &[a, b]));
    }

    #[test]
    fn empty_zone_from_old_data_is_reported_and_deletable() {
        let (mut m, a, _) = model();
        let id = m.add_zone(&zone("Z1", &[a])).unwrap();
        m.sql()
            .execute("DELETE FROM zone_room WHERE zone=?1", [id])
            .unwrap();
        let w = m.zone_warnings().unwrap();
        assert_eq!((w.len(), w[0].table, w[0].id), (1, "zone", id));
        m.delete_zone(id).unwrap();
        assert!(m.zone(id).is_err());
    }

    fn point(m: &Model, room: i64) -> (f64, f64) {
        m.sql()
            .query_row("SELECT px,py FROM room WHERE id=?1", [room], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap()
    }

    fn rooms_total(m: &Model) -> i64 {
        m.sql()
            .query_row("SELECT count(*) FROM room", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn zone_transforms_go_through_its_rooms_in_one_step() {
        use crate::Target;
        let (mut m, a, b) = model();
        let id = m.add_zone(&zone("Z1", &[a, b])).unwrap();
        let (pa, pb) = (point(&m, a), point(&m, b));
        m.move_elements(&[Target::Zone(id)], (0.5, 0.25)).unwrap();
        assert_eq!(point(&m, a), (pa.0 + 0.5, pa.1 + 0.25));
        assert_eq!(point(&m, b), (pb.0 + 0.5, pb.1 + 0.25));
        m.undo().unwrap();
        assert_eq!((point(&m, a), point(&m, b)), (pa, pb));

        m.rotate_elements(&[Target::Zone(id)], (0.0, 0.0), std::f64::consts::FRAC_PI_2, false)
            .unwrap();
        let q = point(&m, a);
        assert!((q.0 + pa.1).abs() < 1e-9 && (q.1 - pa.0).abs() < 1e-9, "{q:?}");
        m.undo().unwrap();
        m.mirror_elements(&[Target::Zone(id)], (-1.0, 0.0), (-1.0, 1.0), false)
            .unwrap();
        let q = point(&m, b);
        assert!((q.0 - (-2.0 - pb.0)).abs() < 1e-9 && (q.1 - pb.1).abs() < 1e-9, "{q:?}");
        m.undo().unwrap();
        // the group stays whole and valid
        assert_eq!(m.zone(id).unwrap(), zone("Z1", &[a, b]));
        assert!(m.check_integrity().unwrap().is_empty());
    }

    #[test]
    fn zone_copy_and_array_copy_the_rooms_and_undo_whole() {
        use crate::Target;
        let (mut m, a, b) = model();
        let id = m.add_zone(&zone("Z1", &[a, b])).unwrap();
        let made = m
            .copy_elements(&[Target::Zone(id)], &[(0.0, 10.0)])
            .unwrap();
        let Target::Zone(n) = made[0] else { panic!() };
        assert_eq!(rooms_total(&m), 4);
        let copy = m.zone(n).unwrap();
        assert_eq!((copy.name.as_str(), copy.rooms.len()), ("Z1", 2));
        assert!(copy.rooms.iter().all(|r| *r != a && *r != b));
        assert_eq!(m.zone(id).unwrap(), zone("Z1", &[a, b]), "the original is untouched");
        assert_eq!(point(&m, copy.rooms[0]).1, point(&m, a).1 + 10.0);
        assert!(m.check_integrity().unwrap().is_empty());
        m.undo().unwrap();
        assert_eq!((rooms_total(&m), m.zones().unwrap().len()), (2, 1));

        let row = m
            .array_linear(&[Target::Zone(id)], 3, crate::Spacing::Step((0.0, 10.0)))
            .unwrap();
        assert_eq!((row.len(), rooms_total(&m)), (2, 6));
        m.undo().unwrap();
        assert_eq!((rooms_total(&m), m.zones().unwrap().len()), (2, 1));
    }

    #[test]
    fn a_room_selected_with_its_zone_moves_once() {
        use crate::Target;
        let (mut m, a, b) = model();
        let id = m.add_zone(&zone("Z1", &[a, b])).unwrap();
        let pa = point(&m, a);
        m.move_elements(&[Target::Zone(id), Target::Room(a)], (1.0, 0.0))
            .unwrap();
        assert_eq!(point(&m, a), (pa.0 + 1.0, pa.1));
        let made = m
            .copy_elements(&[Target::Room(a), Target::Zone(id)], &[(0.0, 10.0)])
            .unwrap();
        assert_eq!((made.len(), rooms_total(&m)), (1, 4));
    }

    #[test]
    fn deleting_a_zone_leaves_its_rooms() {
        let (mut m, a, b) = model();
        let id = m.add_zone(&zone("Z1", &[a, b])).unwrap();
        m.delete_items("t", &[("zone", id)]).unwrap();
        assert!(m.zone(id).is_err());
        assert_eq!(rooms_total(&m), 2);
        m.undo().unwrap();
        assert_eq!(m.zone(id).unwrap(), zone("Z1", &[a, b]));
    }
}
