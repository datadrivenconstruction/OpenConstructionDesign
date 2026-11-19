// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Find and replace text across the model: text notes, room names and numbers, the mark and
//! comments of any element, and every text parameter value. A search is read-only; a replace is
//! one `atomic` step (one undo). Pinned elements and locked parameter values
//! ([`Model::split_locked`]) are never written: the report counts them instead.

use std::collections::HashSet;

use rusqlite::{params, types::Value, Connection, OptionalExtension};

use crate::annot_text::{read_shape, text_notes};
use crate::ops::Inv;
use crate::params::{category_of, PVal, ParamSet, ParamType};
use crate::{AnnoShape, AnnoView, Edit, Error, Model};

/// Where a hit lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FindField {
    /// Text of an annotation note.
    Note,
    RoomName,
    RoomNumber,
    /// Free text property of an element (`prop`): mark, comments.
    Prop,
    /// Value of a text parameter.
    Param,
}

/// What to search for and where.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FindOpts {
    pub query: String,
    pub match_case: bool,
    pub whole_word: bool,
    /// Only this category: "annotation", "room", "door", "window", a parameter category or a
    /// table name for rows without one.
    pub category: Option<String>,
    /// Only elements and notes of this level (rows without a level never match).
    pub level: Option<i64>,
}

/// Stable identity of a hit, to replace chosen hits only.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HitKey {
    pub field: FindField,
    pub table: String,
    pub id: i64,
    pub key: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FindHit {
    pub field: FindField,
    /// Table of the owner row ("anno", "room", "opening", "wall", ...).
    pub table: String,
    pub id: i64,
    /// Property key or parameter name; "name" / "number" for rooms, empty for notes.
    pub key: String,
    pub param: Option<i64>,
    pub text: String,
    /// Byte ranges of the matches inside `text`.
    pub spans: Vec<(usize, usize)>,
    pub category: String,
    pub level: Option<i64>,
    /// Plan, section or legend of a note.
    pub view: Option<AnnoView>,
    /// Where to look at it in the plan (metres), when the row has a place.
    pub pos: Option<(f64, f64)>,
    /// The element is pinned: replace skips it.
    pub pinned: bool,
    /// The value is locked (parameter lock): replace skips it.
    pub locked: bool,
}

impl FindHit {
    pub fn hit_key(&self) -> HitKey {
        HitKey {
            field: self.field,
            table: self.table.clone(),
            id: self.id,
            key: self.key.clone(),
        }
    }

    /// Replace leaves this value alone.
    pub fn read_only(&self) -> bool {
        self.pinned || self.locked
    }
}

/// Outcome of a replace (or of its dry run).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReplaceReport {
    /// Occurrences replaced.
    pub replaced: usize,
    /// Distinct elements (rows) changed.
    pub elements: usize,
    /// Values skipped because their element is pinned.
    pub pinned: usize,
    /// Values skipped because the value is locked.
    pub locked: usize,
    /// Values skipped because the result is not valid (a note left empty, a write refused).
    pub invalid: usize,
}

fn fold(c: char, match_case: bool) -> char {
    if match_case {
        return c;
    }
    let mut l = c.to_lowercase();
    match (l.next(), l.next()) {
        (Some(x), None) => x,
        _ => c,
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Non-overlapping byte ranges of `query` in `text`. Whole word: a word character at the edge of
/// the query must not touch another word character in the text.
pub fn find_spans(
    text: &str,
    query: &str,
    match_case: bool,
    whole_word: bool,
) -> Vec<(usize, usize)> {
    let q: Vec<char> = query.chars().map(|c| fold(c, match_case)).collect();
    if q.is_empty() {
        return vec![];
    }
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let (first_word, last_word) = (is_word(q[0]), is_word(q[q.len() - 1]));
    let mut out = vec![];
    let mut i = 0;
    while i + q.len() <= chars.len() {
        let hit = (0..q.len()).all(|k| fold(chars[i + k].1, match_case) == q[k]);
        if hit {
            let end_i = i + q.len();
            let left_ok = !whole_word || !first_word || i == 0 || !is_word(chars[i - 1].1);
            let right_ok =
                !whole_word || !last_word || end_i == chars.len() || !is_word(chars[end_i].1);
            if left_ok && right_ok {
                let end = chars.get(end_i).map_or(text.len(), |c| c.0);
                out.push((chars[i].0, end));
                i = end_i;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn replace_spans(text: &str, spans: &[(usize, usize)], with: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for &(a, b) in spans {
        out.push_str(&text[at..a]);
        out.push_str(with);
        at = b;
    }
    out.push_str(&text[at..]);
    out
}

/// The changes a replace makes to `hits` and its report; `only` limits it to chosen hits.
fn plan_changes(
    hits: Vec<FindHit>,
    with: &str,
    only: Option<&[HitKey]>,
) -> (Vec<(FindHit, String)>, ReplaceReport) {
    let mut rep = ReplaceReport::default();
    let mut changes = vec![];
    let mut rows = HashSet::new();
    let only: Option<HashSet<&HitKey>> = only.map(|k| k.iter().collect());
    for h in hits {
        if only.as_ref().is_some_and(|k| !k.contains(&h.hit_key())) {
            continue;
        }
        if h.pinned {
            rep.pinned += 1;
            continue;
        }
        if h.locked {
            rep.locked += 1;
            continue;
        }
        let new = replace_spans(&h.text, &h.spans, with);
        if new == h.text {
            continue;
        }
        if h.field == FindField::Note && new.trim().is_empty() {
            rep.invalid += 1;
            continue;
        }
        rep.replaced += h.spans.len();
        rows.insert((h.table.clone(), h.id));
        changes.push((h, new));
    }
    rep.elements = rows.len();
    (changes, rep)
}

/// What a replace of `hits` by `with` would report, without touching the model (a window
/// keeps its hit list and asks again on every keystroke of the replacement).
pub fn replace_report(hits: &[FindHit], with: &str, only: Option<&[HitKey]>) -> ReplaceReport {
    plan_changes(hits.to_vec(), with, only).1
}

/// The text of `h` after the replacement (what the hover preview shows).
pub fn replaced_text(h: &FindHit, with: &str) -> String {
    replace_spans(&h.text, &h.spans, with)
}

fn level_of(db: &Connection, table: &str, id: i64) -> Result<Option<i64>, Error> {
    if table == "opening" {
        return Ok(db
            .query_row(
                "SELECT w.level FROM opening o JOIN wall w ON w.id=o.wall WHERE o.id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?);
    }
    if !table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Ok(None);
    }
    let has: i64 = db.query_row(
        "SELECT count(*) FROM pragma_table_info(?1) WHERE name='level'",
        [table],
        |r| r.get(0),
    )?;
    if has == 0 {
        return Ok(None);
    }
    Ok(db
        .query_row(&format!("SELECT level FROM {table} WHERE id=?1"), [id], |r| {
            r.get::<_, Option<i64>>(0)
        })
        .optional()?
        .flatten())
}

/// A point in the plan to look at for a row: wall middle, opening centre, room point.
fn pos_of(db: &Connection, table: &str, id: i64) -> Result<Option<(f64, f64)>, Error> {
    let sql = match table {
        "wall" => "SELECT (ax+bx)/2, (ay+by)/2 FROM wall WHERE id=?1",
        "room" => "SELECT px, py FROM room WHERE id=?1",
        "opening" => {
            let row: Option<(f64, f64, f64, f64, f64)> = db
                .query_row(
                    "SELECT w.ax, w.ay, w.bx, w.by, o.along+o.width/2
                     FROM opening o JOIN wall w ON w.id=o.wall WHERE o.id=?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )
                .optional()?;
            return Ok(row.map(|(ax, ay, bx, by, at)| {
                let len = (bx - ax).hypot(by - ay).max(1e-9);
                (ax + (bx - ax) * at / len, ay + (by - ay) * at / len)
            }));
        }
        _ => return Ok(None),
    };
    Ok(db
        .query_row(sql, [id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?)
}

/// What a candidate row knows about itself before the shared filters run.
struct Cand<'a> {
    field: FindField,
    table: &'a str,
    id: i64,
    key: &'a str,
    param: Option<i64>,
    text: &'a str,
    view: Option<AnnoView>,
    pos: Option<(f64, f64)>,
    /// `Some` when the caller knows the level (None inside means "no level").
    level: Option<Option<i64>>,
    category: Option<&'a str>,
}

impl Model {
    fn push_hit(&self, o: &FindOpts, out: &mut Vec<FindHit>, c: Cand) -> Result<(), Error> {
        let spans = find_spans(c.text, &o.query, o.match_case, o.whole_word);
        if spans.is_empty() {
            return Ok(());
        }
        let category = match c.category {
            Some(x) => x.to_string(),
            None => category_of(&self.db, c.table, c.id)?
                .map_or_else(|| c.table.to_string(), str::to_string),
        };
        if o.category.as_deref().is_some_and(|w| w != category) {
            return Ok(());
        }
        let level = match c.level {
            Some(l) => l,
            None => level_of(&self.db, c.table, c.id)?,
        };
        if o.level.is_some() && o.level != level {
            return Ok(());
        }
        let pos = match c.pos {
            Some(p) => Some(p),
            None => pos_of(&self.db, c.table, c.id)?,
        };
        out.push(FindHit {
            field: c.field,
            table: c.table.to_string(),
            id: c.id,
            key: c.key.to_string(),
            param: c.param,
            text: c.text.to_string(),
            spans,
            category,
            level,
            view: c.view,
            pos,
            pinned: self.is_pinned(c.table, c.id)?,
            locked: false,
        });
        Ok(())
    }

    /// Marks the hits whose value is locked; the one lock rule of every batch writer.
    fn mark_locked(&self, hits: &mut [FindHit]) -> Result<(), Error> {
        let edits: Vec<Edit> = hits
            .iter()
            .filter(|h| !h.key.is_empty())
            .map(|h| Edit::ParamValue {
                param: h.key.clone(),
                kind: h.table.clone(),
                id: h.id,
                value: None,
            })
            .collect();
        let (_, locked) = self.split_locked(&edits)?;
        if locked.is_empty() {
            return Ok(());
        }
        for h in hits.iter_mut() {
            let k = (h.table.clone(), h.id, h.key.trim().to_string());
            h.locked = !h.key.is_empty() && locked.contains(&k);
        }
        Ok(())
    }

    /// All text values containing the query, in a stable order (notes, rooms, properties,
    /// parameters; by id inside each).
    pub fn find_text(&self, o: &FindOpts) -> Result<Vec<FindHit>, Error> {
        let mut out = vec![];
        if o.query.is_empty() {
            return Ok(out);
        }
        for (id, view, t) in text_notes(&self.db)? {
            let level = match view {
                AnnoView::Plan(l) => Some(l),
                _ => None,
            };
            self.push_hit(
                o,
                &mut out,
                Cand {
                    field: FindField::Note,
                    table: "anno",
                    id,
                    key: "",
                    param: None,
                    text: &t.text,
                    view: Some(view),
                    pos: Some(t.pos),
                    level: Some(level),
                    category: Some("annotation"),
                },
            )?;
        }
        let rooms: Vec<(i64, i64, String, String, f64, f64)> = {
            let mut st = self
                .db
                .prepare_cached("SELECT id,level,name,number,px,py FROM room ORDER BY id")?;
            let v = st
                .query_map([], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                })?
                .collect::<Result<_, _>>()?;
            v
        };
        for (id, level, name, number, px, py) in &rooms {
            for (field, key, text) in [
                (FindField::RoomName, "name", name),
                (FindField::RoomNumber, "number", number),
            ] {
                self.push_hit(
                    o,
                    &mut out,
                    Cand {
                        field,
                        table: "room",
                        id: *id,
                        key,
                        param: None,
                        text,
                        view: None,
                        pos: Some((*px, *py)),
                        level: Some(Some(*level)),
                        category: Some("room"),
                    },
                )?;
            }
        }
        let props: Vec<(String, i64, String, String)> = {
            let mut st = self.db.prepare_cached(
                "SELECT kind,ref,key,value FROM prop
                 WHERE lower(key) IN ('mark','comments') AND typeof(value)='text'
                 ORDER BY kind,ref,key",
            )?;
            let v = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<Result<_, _>>()?;
            v
        };
        for (kind, id, key, text) in &props {
            self.push_hit(
                o,
                &mut out,
                Cand {
                    field: FindField::Prop,
                    table: kind,
                    id: *id,
                    key,
                    param: None,
                    text,
                    view: None,
                    pos: None,
                    level: None,
                    category: None,
                },
            )?;
        }
        for d in self.params()? {
            if d.spec.ptype != ParamType::Text {
                continue;
            }
            let mut vals: Vec<_> = self.param_values(d.id)?.into_iter().collect();
            vals.sort_by(|a, b| a.0.cmp(&b.0));
            for ((kind, id), v) in vals {
                let PVal::Text(text) = v else { continue };
                self.push_hit(
                    o,
                    &mut out,
                    Cand {
                        field: FindField::Param,
                        table: &kind,
                        id,
                        key: &d.spec.name,
                        param: Some(d.id),
                        text: &text,
                        view: None,
                        pos: None,
                        level: None,
                        category: None,
                    },
                )?;
            }
        }
        self.mark_locked(&mut out)?;
        Ok(out)
    }

    fn replace_changes(
        &self,
        o: &FindOpts,
        with: &str,
        only: Option<&[HitKey]>,
    ) -> Result<(Vec<(FindHit, String)>, ReplaceReport), Error> {
        Ok(plan_changes(self.find_text(o)?, with, only))
    }

    /// Dry run of [`Model::replace_text`]: the counts the user sees before applying.
    pub fn replace_preview(
        &self,
        o: &FindOpts,
        with: &str,
        only: Option<&[HitKey]>,
    ) -> Result<ReplaceReport, Error> {
        Ok(self.replace_changes(o, with, only)?.1)
    }

    /// Replaces the query by `with` in every writable hit (or only in `only`) as one undo step.
    /// Nothing to replace: no step is written.
    pub fn replace_text(
        &mut self,
        o: &FindOpts,
        with: &str,
        only: Option<&[HitKey]>,
    ) -> Result<ReplaceReport, Error> {
        let (changes, mut rep) = self.replace_changes(o, with, only)?;
        if changes.is_empty() {
            return Ok(rep);
        }
        let (replaced, elements, refused) = self.atomic("find_replace", |m| {
            let mut done = 0;
            let mut rows = HashSet::new();
            let mut refused = 0;
            for (h, new) in &changes {
                // each write is a savepoint: a refused one leaves no trace
                match m.write_hit(h, new) {
                    Ok(()) => {
                        done += h.spans.len();
                        rows.insert((h.table.clone(), h.id));
                    }
                    Err(_) => refused += 1,
                }
            }
            if done == 0 {
                return Err(Error::Invalid("find: no value could be replaced".into()));
            }
            Ok((done, rows.len(), refused))
        })?;
        rep.replaced = replaced;
        rep.elements = elements;
        rep.invalid += refused;
        Ok(rep)
    }

    fn write_hit(&mut self, h: &FindHit, new: &str) -> Result<(), Error> {
        match h.field {
            FindField::Note => {
                let AnnoShape::Text(mut t) = read_shape(&self.db, h.id)? else {
                    return Err(Error::Invalid(format!("annotation {} is not a note", h.id)));
                };
                t.text = new.to_string();
                self.update_anno(h.id, &AnnoShape::Text(t))
            }
            FindField::RoomName | FindField::RoomNumber => {
                let col = if h.field == FindField::RoomName {
                    "name"
                } else {
                    "number"
                };
                let id = h.id;
                self.op("find_room", |db| {
                    let inv = Inv::default().restore(db, "room", "id", id)?;
                    let n = db.execute(
                        &format!("UPDATE room SET {col}=?2 WHERE id=?1"),
                        params![id, new],
                    )?;
                    if n == 0 {
                        return Err(Error::Invalid(format!("no such room {id}")));
                    }
                    Ok(((), id, inv))
                })
            }
            FindField::Prop => {
                let (kind, id, key) = (h.table.clone(), h.id, h.key.clone());
                self.op("find_prop", |db| {
                    let old: Option<(Value, String)> = db
                        .query_row(
                            "SELECT value, unit FROM prop WHERE kind=?1 AND ref=?2 AND key=?3",
                            params![kind, id, key],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        .optional()?;
                    let Some((v, u)) = old else {
                        return Err(Error::Invalid(format!("no such property {kind} {id} {key}")));
                    };
                    let inv = Inv::default().sql(
                        "INSERT OR REPLACE INTO prop(kind,ref,key,value,unit) VALUES (?1,?2,?3,?4,?5)",
                        vec![
                            Value::Text(kind.clone()),
                            Value::Integer(id),
                            Value::Text(key.clone()),
                            v,
                            Value::Text(u.clone()),
                        ],
                    );
                    // an empty replacement keeps the row and its unit
                    db.execute(
                        "UPDATE prop SET value=?4 WHERE kind=?1 AND ref=?2 AND key=?3",
                        params![kind, id, key, new],
                    )?;
                    Ok(((), id, inv))
                })
            }
            FindField::Param => {
                let param = h
                    .param
                    .ok_or_else(|| Error::Invalid("find: parameter hit without id".into()))?;
                self.set_param_values(&[ParamSet {
                    param,
                    kind: h.table.clone(),
                    id: h.id,
                    value: Some(PVal::Text(new.to_string())),
                }])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LayerSpec, OpeningSpec, ParamSpec, RoomSpec, TagHost, TextNote, WallSpec};

    struct Fx {
        m: Model,
        level: i64,
        room: i64,
        opening: i64,
        note: i64,
        param: i64,
    }

    fn opts(q: &str) -> FindOpts {
        FindOpts {
            query: q.into(),
            ..Default::default()
        }
    }

    fn fixture() -> Fx {
        let mut m = Model::open(":memory:").unwrap();
        let level = m.add_level("L1", 0.0).unwrap();
        let t = m
            .add_wall_type(
                "w",
                &[LayerSpec {
                    material: "brick".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let wall = m
            .add_walls(&[WallSpec {
                a: (0.0, 0.0),
                b: (6.0, 0.0),
                level,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            }])
            .unwrap()[0];
        let opening = m
            .add_opening(
                wall,
                &OpeningSpec {
                    along: 3.0,
                    width: 0.9,
                    sill: 0.0,
                    height: 2.1,
                    kind: "door".into(),
                },
            )
            .unwrap();
        m.set_mark(TagHost::Opening(opening), "Door 7").unwrap();
        let room = m
            .add_room(&RoomSpec {
                level,
                name: "Office".into(),
                number: "A-101".into(),
                point: (3.0, 2.0),
                height: 3.0,
            })
            .unwrap();
        let note = m
            .add_anno(
                AnnoView::Plan(level),
                &AnnoShape::Text(TextNote::new((1.0, 1.0), "Office door to check")),
                None,
                None,
            )
            .unwrap();
        let param = m
            .add_param(&ParamSpec {
                name: "Remarks".into(),
                ptype: ParamType::Text,
                per_type: false,
                unit: String::new(),
                group: String::new(),
                guid: None,
                categories: vec!["room".into()],
            })
            .unwrap();
        m.set_param_values(&[ParamSet {
            param,
            kind: "room".into(),
            id: room,
            value: Some(PVal::Text("Office furniture".into())),
        }])
        .unwrap();
        Fx {
            m,
            level,
            room,
            opening,
            note,
            param,
        }
    }

    #[test]
    fn find_hits_notes_rooms_params() {
        let f = fixture();
        let hits = f.m.find_text(&opts("office")).unwrap();
        let got: Vec<(FindField, i64)> = hits.iter().map(|h| (h.field, h.id)).collect();
        assert_eq!(
            got,
            vec![
                (FindField::Note, f.note),
                (FindField::RoomName, f.room),
                (FindField::Param, f.room),
            ]
        );
        assert_eq!(hits[0].level, Some(f.level));
        assert_eq!(hits[2].param, Some(f.param));
        assert_eq!(hits[2].category, "room");
        assert!(hits.iter().all(|h| !h.read_only()));
        let marks = f.m.find_text(&opts("door 7")).unwrap();
        assert_eq!(marks.len(), 1);
        assert_eq!((marks[0].field, marks[0].id), (FindField::Prop, f.opening));
        assert_eq!(marks[0].category, "door");
        let at = marks[0].pos.unwrap();
        assert!((at.0 - 3.45).abs() < 1e-9 && at.1.abs() < 1e-9, "{at:?}");
        assert_eq!(hits[1].pos, Some((3.0, 2.0)));
        assert_eq!(hits[0].pos, Some((1.0, 1.0)));
        assert_eq!(marks[0].level, Some(f.level));
        let num = f.m.find_text(&opts("A-101")).unwrap();
        assert_eq!(num[0].field, FindField::RoomNumber);
        assert!(f.m.find_text(&opts("")).unwrap().is_empty());
        assert!(f.m.find_text(&opts("zzz")).unwrap().is_empty());
        let by_cat = FindOpts {
            category: Some("room".into()),
            ..opts("office")
        };
        assert_eq!(f.m.find_text(&by_cat).unwrap().len(), 2);
        let other_level = FindOpts {
            level: Some(f.level + 99),
            ..opts("office")
        };
        assert!(f.m.find_text(&other_level).unwrap().is_empty());
        let this_level = FindOpts {
            level: Some(f.level),
            ..opts("office")
        };
        assert_eq!(f.m.find_text(&this_level).unwrap().len(), 3);
    }

    #[test]
    fn whole_word_and_case() {
        assert_eq!(find_spans("Door door doors", "door", false, false).len(), 3);
        let t = "Door door doors";
        assert_eq!(find_spans(t, "door", true, false), vec![(5, 9), (10, 14)]);
        assert_eq!(find_spans("Door door doors", "door", false, true), vec![(0, 4), (5, 9)]);
        assert_eq!(find_spans("Door door doors", "door", true, true), vec![(5, 9)]);
        assert_eq!(find_spans("a_door", "door", false, true), vec![]);
        assert_eq!(find_spans("aaaa", "aa", false, false), vec![(0, 2), (2, 4)]);
        // multibyte text keeps byte offsets valid
        assert_eq!(find_spans("\u{00e9}t\u{00e9} T", "t", false, true), vec![(6, 7)]);
        assert_eq!(find_spans("(D-1)", "D-1", true, true), vec![(1, 4)]);
        let f = fixture();
        let mut o = opts("OFFICE");
        o.match_case = true;
        assert!(f.m.find_text(&o).unwrap().is_empty());
        o.query = "Office".into();
        o.whole_word = true;
        assert_eq!(f.m.find_text(&o).unwrap().len(), 3);
        o.query = "Offic".into();
        assert!(f.m.find_text(&o).unwrap().is_empty());
    }

    #[test]
    fn replace_all_one_undo_counts() {
        let mut f = fixture();
        let before = f.m.op_top().unwrap();
        let o = opts("office");
        let preview = f.m.replace_preview(&o, "Studio", None).unwrap();
        assert_eq!(f.m.op_top().unwrap(), before, "a preview writes nothing");
        let rep = f.m.replace_text(&o, "Studio", None).unwrap();
        assert_eq!(preview, rep);
        assert_eq!(
            rep,
            ReplaceReport {
                replaced: 3,
                elements: 2,
                ..Default::default()
            }
        );
        assert_eq!(f.m.op_top().unwrap(), before + 1, "one journal step");
        assert!(f.m.find_text(&o).unwrap().is_empty());
        let studio = f.m.find_text(&opts("Studio")).unwrap();
        assert_eq!(studio.len(), 3);
        assert!(studio.iter().any(|h| h.text == "Studio furniture"));
        assert!(studio.iter().any(|h| h.text == "Studio door to check"));
        let mark = FindOpts {
            query: "Door".into(),
            ..Default::default()
        };
        f.m.replace_text(&mark, "Gate", None).unwrap();
        assert_eq!(f.m.find_text(&opts("Gate 7")).unwrap().len(), 1);
        f.m.undo().unwrap();
        assert_eq!(f.m.find_text(&opts("Gate")).unwrap().len(), 0);
        f.m.undo().unwrap();
        assert_eq!(f.m.find_text(&o).unwrap().len(), 3, "one undo restores all");
        assert_eq!(f.m.op_top().unwrap(), before);
        // nothing to replace: no step
        let none = f.m.replace_text(&opts("zzz"), "x", None).unwrap();
        assert_eq!(none, ReplaceReport::default());
        assert_eq!(f.m.op_top().unwrap(), before);
    }

    #[test]
    fn replace_inside_step_is_one_undo() {
        // the window runs the replace inside the editor's step (`run_op`)
        let mut f = fixture();
        let before = f.m.op_top().unwrap();
        let ((), wrote) = f
            .m
            .step(|m| m.replace_text(&opts("office"), "Lab", None).map(|_| ()))
            .unwrap();
        assert!(wrote);
        assert_eq!(f.m.op_top().unwrap(), before + 1);
        assert_eq!(f.m.find_text(&opts("Lab")).unwrap().len(), 3);
        f.m.undo().unwrap();
        assert_eq!(f.m.find_text(&opts("office")).unwrap().len(), 3);
        assert!(f.m.find_text(&opts("Lab")).unwrap().is_empty());
        assert_eq!(f.m.op_top().unwrap(), before);
    }

    fn prop_row(f: &Fx, key: &str) -> Option<(String, String)> {
        f.m.db
            .query_row(
                "SELECT value, unit FROM prop WHERE kind='opening' AND ref=?1 AND key=?2",
                params![f.opening, key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .unwrap()
    }

    #[test]
    fn replace_prop_with_empty_keeps_row_and_unit() {
        let mut f = fixture();
        f.m.db
            .execute(
                "INSERT INTO prop(kind,ref,key,value,unit) VALUES ('opening',?1,'comments','EI30','min')",
                [f.opening],
            )
            .unwrap();
        let mut o = opts("EI30");
        o.whole_word = true;
        let rep = f.m.replace_text(&o, "", None).unwrap();
        assert_eq!(rep.replaced, 1);
        assert_eq!(
            prop_row(&f, "comments"),
            Some((String::new(), "min".to_string())),
            "row and unit survive an empty replacement"
        );
    }

    #[test]
    fn empty_replace_undo_restores_value() {
        let mut f = fixture();
        f.m.db
            .execute(
                "INSERT INTO prop(kind,ref,key,value,unit) VALUES ('opening',?1,'comments','EI30','min')",
                [f.opening],
            )
            .unwrap();
        let before = f.m.op_top().unwrap();
        f.m.replace_text(&opts("EI30"), "", None).unwrap();
        assert_eq!(f.m.op_top().unwrap(), before + 1);
        f.m.undo().unwrap();
        assert_eq!(
            prop_row(&f, "comments"),
            Some(("EI30".to_string(), "min".to_string()))
        );
        assert_eq!(f.m.op_top().unwrap(), before);
    }

    #[test]
    fn replace_only_chosen_hits() {
        let mut f = fixture();
        let hits = f.m.find_text(&opts("office")).unwrap();
        let keys = [hits[1].hit_key()];
        let rep = f.m.replace_text(&opts("office"), "Lab", Some(&keys)).unwrap();
        assert_eq!((rep.replaced, rep.elements), (1, 1));
        assert_eq!(f.m.find_text(&opts("office")).unwrap().len(), 2);
        assert_eq!(f.m.find_text(&opts("Lab")).unwrap()[0].field, FindField::RoomName);
    }

    #[test]
    fn locked_value_is_refused_and_counted() {
        let mut f = fixture();
        f.m.set_param_lock(&[("room", f.room, "Remarks")]).unwrap();
        let hits = f.m.find_text(&opts("office")).unwrap();
        let locked: Vec<FindField> = hits.iter().filter(|h| h.locked).map(|h| h.field).collect();
        assert_eq!(locked, vec![FindField::Param], "only the locked parameter value");
        assert!(hits.iter().all(|h| !h.pinned));
        let before = f.m.op_top().unwrap();
        let rep = f.m.replace_text(&opts("office"), "Lab", None).unwrap();
        assert_eq!(
            rep,
            ReplaceReport {
                replaced: 2,
                elements: 2,
                locked: 1,
                ..Default::default()
            }
        );
        assert_eq!(f.m.op_top().unwrap(), before + 1);
        let kept = f.m.find_text(&opts("Office")).unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].field, FindField::Param);
        assert_eq!(
            f.m.param_value(f.param, "room", f.room).unwrap(),
            Some(PVal::Text("Office furniture".into()))
        );
        // only the locked value chosen: no step, the refusal is reported
        let keys = [kept[0].hit_key()];
        let top = f.m.op_top().unwrap();
        let one = f.m.replace_text(&opts("office"), "Lab", Some(&keys)).unwrap();
        assert_eq!((one.replaced, one.locked), (0, 1));
        assert_eq!(f.m.op_top().unwrap(), top);
    }

    #[test]
    fn pinned_element_is_refused_and_counted() {
        let mut f = fixture();
        f.m.pin(&[("opening", f.opening)]).unwrap();
        let marks = f.m.find_text(&opts("Door 7")).unwrap();
        assert_eq!(marks.len(), 1);
        assert!(marks[0].pinned && !marks[0].locked);
        let top = f.m.op_top().unwrap();
        let rep = f.m.replace_text(&opts("Door 7"), "x", None).unwrap();
        assert_eq!(
            rep,
            ReplaceReport {
                pinned: 1,
                ..Default::default()
            }
        );
        assert_eq!(f.m.op_top().unwrap(), top, "nothing written, no step");
        assert_eq!(f.m.find_text(&opts("Door 7")).unwrap().len(), 1);
        // a pinned room: its name and its parameter are both skipped, the note is replaced
        f.m.pin(&[("room", f.room)]).unwrap();
        let rep = f.m.replace_text(&opts("office"), "Lab", None).unwrap();
        assert_eq!((rep.replaced, rep.elements, rep.pinned), (1, 1, 2));
        assert_eq!(f.m.find_text(&opts("Office")).unwrap().len(), 2);
    }

    #[test]
    fn note_left_empty_is_skipped_not_written() {
        let mut f = fixture();
        let o = FindOpts {
            query: "Office door to check".into(),
            ..Default::default()
        };
        let before = f.m.op_top().unwrap();
        let rep = f.m.replace_text(&o, "", None).unwrap();
        assert_eq!((rep.replaced, rep.invalid), (0, 1));
        assert_eq!(f.m.op_top().unwrap(), before);
        assert_eq!(f.m.find_text(&o).unwrap().len(), 1);
    }
}
