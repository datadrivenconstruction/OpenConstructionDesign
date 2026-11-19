// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Linked booleans and voids as typed requests. A boolean result is its own form whose tree is
//! `Bool { op, a, b }` built from the current operand forms; the operands stay separate forms and
//! the link (`mass_bool`) recomputes the result in the same operation whenever an operand is
//! edited. Every request is one undo step and is refused with a reason (model unchanged) when an
//! operand is not a solid or the result is empty or invalid.
//!
//! The link table is created by schema step 42 ([`migrate`], additive), on open of any older file.

use ddc_geom::csg::{Body, BoolOp};
use ddc_geom::csg2::MIN_FACES;
use ddc_geom::csg_check::{checked_boolean_noted, BoolReason};
use rusqlite::{params, Connection};

use super::{elev, element_body, prepare, read, sync_hosts, world, write, Form, MassSpec, TOL};
use crate::ops::Inv;
use crate::{ElementWarning, Error, Model};

const DDL: &str = "CREATE TABLE IF NOT EXISTS mass_bool(result INTEGER PRIMARY KEY, \
    op TEXT NOT NULL, a INTEGER NOT NULL, b INTEGER NOT NULL)";

/// Schema step 42: creates the link table; safe to repeat.
pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    db.execute_batch(DDL)?;
    Ok(())
}

/// Volume below this (m^3) means a void does not reach its host.
const EMPTY_VOLUME: f64 = 1e-9;

/// `a op b` over two forms; the result is a new form, `a` and `b` are kept.
#[derive(Clone, Debug, PartialEq)]
pub struct BoolRequest {
    pub op: BoolOp,
    pub a: i64,
    pub b: i64,
    /// Name of the result form; default "<op> <a>-<b>".
    pub name: Option<String>,
}

/// A stored link: `result` is recomputed from `a op b`.
#[derive(Clone, Debug, PartialEq)]
pub struct BoolLink {
    pub result: i64,
    pub op: BoolOp,
    pub a: i64,
    pub b: i64,
}

/// A void form that cuts one host element (wall, slab, roof, column, beam, form).
#[derive(Clone, Debug, PartialEq)]
pub struct VoidRequest {
    pub host: (String, i64),
    pub level: i64,
    pub form: Form,
    pub name: String,
}

fn links(db: &Connection, filter: &str, id: i64) -> Result<Vec<BoolLink>, Error> {
    let sql = format!("SELECT result,op,a,b FROM mass_bool WHERE {filter} ORDER BY result");
    let rows: Vec<(i64, String, i64, i64)> = db
        .prepare(&sql)?
        .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<_, _>>()?;
    rows.into_iter()
        .map(|(result, op, a, b)| {
            let op = BoolOp::parse(&op)
                .ok_or_else(|| Error::Invalid(format!("boolean link {result}: bad op {op}")))?;
            Ok(BoolLink { result, op, a, b })
        })
        .collect()
}

fn refused(op: BoolOp, a: i64, b: i64, r: &BoolReason) -> Error {
    Error::Invalid(format!("{} of forms {a} and {b} refused: {r}", op.name()))
}

/// Result spec from the current operand forms: on the level of `a`, `b` shifted by the level
/// difference. `old` (the stored result) keeps face numbers stable across recomputes.
fn result_spec(
    db: &Connection,
    op: BoolOp,
    a: i64,
    b: i64,
    old: Option<&MassSpec>,
    name: &str,
) -> Result<MassSpec, Error> {
    if a == b {
        return Err(Error::Invalid("boolean: operands must be two different forms".into()));
    }
    let (sa, sb) = (read(db, a)?, read(db, b)?);
    if sa.void || sb.void {
        return Err(Error::Invalid(
            "boolean: void forms cut hosts; combine solid forms only".into(),
        ));
    }
    checked_boolean_noted(&world(db, &sa)?, &world(db, &sb)?, op, TOL)
        .map_err(|r| refused(op, a, b, &r))?;
    let dz = elev(db, sb.level)? - elev(db, sa.level)?;
    let fb = if dz != 0.0 {
        Form::Move {
            base: Box::new(sb.form),
            d: (0.0, 0.0, dz),
        }
    } else {
        sb.form
    };
    let spec = MassSpec {
        level: sa.level,
        name: name.to_string(),
        void: false,
        inplace: old.map_or(sa.inplace, |o| o.inplace),
        form: Form::Bool {
            op,
            a: Box::new(sa.form),
            b: Box::new(fb),
        },
    };
    prepare(db, &spec, old.map(|o| &o.form))
}

/// Before a direct edit of form `id`: a linked result is driven by its operands only.
pub(crate) fn guard_edit(db: &Connection, id: i64) -> Result<(), Error> {
    if let Some(l) = links(db, "result=?1", id)?.first() {
        return Err(Error::Invalid(format!(
            "form {id} is the {} of forms {} and {}: edit an operand or release the link",
            l.op.name(),
            l.a,
            l.b
        )));
    }
    Ok(())
}

/// After operand `id` was written: recompute the results that use it (and theirs) in the same
/// operation. A result that would become empty or invalid fails the whole edit.
pub(crate) fn follow(db: &Connection, id: i64, inv: Inv) -> Result<Inv, Error> {
    follow_at(db, id, inv, 0)
}

fn follow_at(db: &Connection, id: i64, mut inv: Inv, depth: usize) -> Result<Inv, Error> {
    let deps = links(db, "a=?1 OR b=?1", id)?;
    if deps.is_empty() {
        return Ok(inv);
    }
    if depth > 64 {
        return Err(Error::Invalid("boolean: chain of linked results too deep".into()));
    }
    for l in deps {
        let old = read(db, l.result)?;
        let s = result_spec(db, l.op, l.a, l.b, Some(&old), &old.name)
            .map_err(|e| Error::Invalid(format!("linked result {}: {e}", l.result)))?;
        if s == old {
            continue;
        }
        inv = inv.restore(db, "mass", "id", l.result)?;
        write(db, &s, Some(l.result))?;
        inv = sync_hosts(db, l.result, inv)?;
        inv = follow_at(db, l.result, inv, depth + 1)?;
    }
    Ok(inv)
}

/// Form `id` is deleted: links that use it go; a result keeps its last tree as a plain form.
pub(crate) fn unlink(db: &Connection, id: i64, mut inv: Inv) -> Result<Inv, Error> {
    for col in ["result", "a", "b"] {
        inv = inv.restore(db, "mass_bool", col, id)?;
        db.execute(&format!("DELETE FROM mass_bool WHERE {col}=?1"), [id])?;
    }
    Ok(inv)
}

impl Model {
    /// Result body of a boolean request without writing (preview before apply).
    pub fn preview_boolean(&self, r: &BoolRequest) -> Result<Body, Error> {
        let s = result_spec(&self.db, r.op, r.a, r.b, None, "")?;
        world(&self.db, &s)
    }

    /// New linked result form `a op b`; operands are kept. One undo step.
    pub fn apply_boolean(&mut self, r: &BoolRequest) -> Result<i64, Error> {
        let name = r
            .name
            .clone()
            .unwrap_or_else(|| format!("{} {}-{}", r.op.name(), r.a, r.b));
        let s = result_spec(&self.db, r.op, r.a, r.b, None, &name)?;
        let (op, a, b) = (r.op, r.a, r.b);
        self.op("mass_bool", move |db| {
            let id = write(db, &s, None)?;
            db.execute(
                "INSERT INTO mass_bool(result,op,a,b) VALUES (?1,?2,?3,?4)",
                params![id, op.name(), a, b],
            )?;
            let inv = Inv::default()
                .delete("mass_bool", "result", id)
                .delete("mass_paint", "mass", id)
                .delete("mass", "id", id);
            Ok((id, id, inv))
        })
    }

    /// Receipt lines of linked boolean results in the model warnings channel: a large operand
    /// pair the mesh path refused (planar path used instead), or a link the current operands
    /// no longer support. Recomputed from the operands; small pairs never take the mesh path.
    pub fn bool_warnings(&self) -> Result<Vec<ElementWarning>, Error> {
        let mut out = vec![];
        for l in links(&self.db, "result>=?1", i64::MIN)? {
            let (sa, sb) = (read(&self.db, l.a)?, read(&self.db, l.b)?);
            let (wa, wb) = (world(&self.db, &sa)?, world(&self.db, &sb)?);
            if wa.polys.len() + wb.polys.len() < MIN_FACES {
                continue;
            }
            let lines = match checked_boolean_noted(&wa, &wb, l.op, TOL) {
                Ok((_, notes)) => notes.iter().map(|n| n.what.to_string()).collect(),
                Err(r) => vec![r.to_string()],
            };
            out.extend(lines.into_iter().map(|m| ElementWarning {
                table: "mass",
                id: l.result,
                message: format!("{} of forms {} and {}: {m}", l.op.name(), l.a, l.b),
            }));
        }
        Ok(out)
    }

    pub fn boolean_link(&self, result: i64) -> Result<Option<BoolLink>, Error> {
        Ok(links(&self.db, "result=?1", result)?.into_iter().next())
    }

    /// Results recomputed when form `operand` changes.
    pub fn boolean_dependents(&self, operand: i64) -> Result<Vec<i64>, Error> {
        Ok(links(&self.db, "a=?1 OR b=?1", operand)?
            .into_iter()
            .map(|l| l.result)
            .collect())
    }

    /// The result stays a plain form with its tree; operands no longer drive it. One undo step.
    pub fn release_boolean(&mut self, result: i64) -> Result<(), Error> {
        if self.boolean_link(result)?.is_none() {
            return Err(Error::Invalid(format!("form {result} is not a linked boolean")));
        }
        self.op("mass_bool_release", |db| {
            let inv = Inv::default().restore(db, "mass_bool", "result", result)?;
            db.execute("DELETE FROM mass_bool WHERE result=?1", [result])?;
            Ok(((), result, inv))
        })
    }

    /// Void form joined to its host so the host (and its quantities) lose the cut volume.
    /// Refused when the void does not reach the host. One undo step.
    pub fn apply_void(&mut self, r: &VoidRequest) -> Result<i64, Error> {
        let spec = MassSpec {
            level: r.level,
            name: r.name.clone(),
            void: true,
            inplace: true,
            form: r.form.clone(),
        };
        let spec = prepare(&self.db, &spec, None)?;
        let host = element_body(&self.db, &r.host.0, r.host.1)?;
        let cut = host.intersect(&world(&self.db, &spec)?, TOL);
        if cut.is_empty() || cut.volume().abs() <= EMPTY_VOLUME {
            return Err(Error::Invalid(format!(
                "void misses {} {}: nothing to cut",
                r.host.0, r.host.1
            )));
        }
        let (kind, hid) = (r.host.0.clone(), r.host.1);
        self.compound("mass_void", |m| {
            let v = m.add_mass(&spec)?;
            m.cut_geometry(("mass", v), (kind.as_str(), hid))?;
            Ok(v)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LayerSpec, WallSpec};

    fn cube(level: i64, lo: (f64, f64, f64), hi: (f64, f64, f64)) -> MassSpec {
        MassSpec {
            level,
            name: "box".into(),
            void: false,
            inplace: false,
            form: Form::Cuboid { lo, hi },
        }
    }

    /// A 4x4x3 block and a 1x1 post through it.
    fn setup() -> (Model, i64, i64, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L0", 0.0).unwrap();
        let a = m.add_mass(&cube(lv, (0., 0., 0.), (4., 4., 3.))).unwrap();
        let b = m.add_mass(&cube(lv, (1., 1., -1.), (2., 2., 4.))).unwrap();
        (m, lv, a, b)
    }

    fn req(op: BoolOp, a: i64, b: i64) -> BoolRequest {
        BoolRequest {
            op,
            a,
            b,
            name: None,
        }
    }

    fn vol(m: &Model, id: i64) -> f64 {
        m.mass_body(id).unwrap().volume()
    }

    fn ops(m: &Model) -> i64 {
        m.db.query_row("SELECT count(*) FROM op", [], |r| r.get(0))
            .unwrap()
    }

    fn near(x: f64, y: f64) -> bool {
        (x - y).abs() < 1e-6
    }

    #[test]
    fn boolean_without_lazy_ddl() {
        let (mut m, _, a, b) = setup();
        let exists = |m: &Model| -> bool {
            m.db.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='mass_bool')",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert!(exists(&m), "table comes from the schema step");
        assert!(m.boolean_link(a).unwrap().is_none());
        let r = m.apply_boolean(&req(BoolOp::Subtract, a, b)).unwrap();
        assert_eq!(m.boolean_link(r).unwrap().unwrap().a, a);
        m.undo().unwrap();
        assert!(exists(&m));
        assert!(m.boolean_link(r).unwrap().is_none());
    }

    #[test]
    fn open_old_file_creates_mass_bool() {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c).unwrap();
        migrate(&c).unwrap();
        let n: i64 = c
            .query_row("SELECT count(*) FROM mass_bool", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn old_file_gets_mass_bool_on_open() {
        let d = std::env::temp_dir().join(format!("ddc-mass-bool-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("m.ddc");
        let ps = p.to_str().unwrap();
        let (a, b);
        {
            let mut m = Model::open(ps).unwrap();
            let lv = m.add_level("L0", 0.0).unwrap();
            a = m.add_mass(&cube(lv, (0., 0., 0.), (4., 4., 3.))).unwrap();
            b = m.add_mass(&cube(lv, (1., 1., -1.), (2., 2., 4.))).unwrap();
        }
        {
            // own connection: the model's authorizer turns DROP TABLE into a silent no-op
            let c = Connection::open(ps).unwrap();
            c.execute_batch("DROP TABLE mass_bool").unwrap();
            c.execute(
                "UPDATE meta SET value=?1 WHERE key='schema_version'",
                [crate::schema::VERSION - 1],
            )
            .unwrap();
        }
        {
            let mut m = Model::open(ps).unwrap();
            assert_eq!(m.schema_version().unwrap(), crate::schema::VERSION);
            assert!(m.boolean_link(a).unwrap().is_none());
            let r = m.apply_boolean(&req(BoolOp::Subtract, a, b)).unwrap();
            assert!(m.boolean_link(r).unwrap().is_some());
        }
        let m = Model::open(ps).unwrap();
        let n: i64 = m.db.query_row("SELECT count(*) FROM mass_bool", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1, "reopen is additive and keeps rows");
        drop(m);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn cut_keeps_tree_and_reevaluates() {
        let (mut m, lv, a, b) = setup();
        let r = m.apply_boolean(&req(BoolOp::Subtract, a, b)).unwrap();
        assert!(near(vol(&m, r), 45.0), "{}", vol(&m, r));
        assert!(matches!(
            m.mass(r).unwrap().form,
            Form::Bool {
                op: BoolOp::Subtract,
                ..
            }
        ));
        // operands are kept as their own forms
        assert_eq!(m.masses().unwrap(), vec![a, b, r]);
        assert!(near(vol(&m, a), 48.0) && near(vol(&m, b), 5.0));
        assert_eq!(
            m.boolean_link(r).unwrap(),
            Some(BoolLink {
                result: r,
                op: BoolOp::Subtract,
                a,
                b
            })
        );
        assert_eq!(m.boolean_dependents(b).unwrap(), vec![r]);
        // editing an operand recomputes the result in the same step
        let sb = m.mass(b).unwrap();
        m.update_mass(b, &cube(lv, (1., 1., -1.), (3., 2., 4.))).unwrap();
        assert!(near(vol(&m, r), 42.0), "{}", vol(&m, r));
        m.move_mass(b, (10.0, 0.0, 0.0)).unwrap();
        assert!(near(vol(&m, r), 48.0), "{}", vol(&m, r));
        m.undo().unwrap();
        m.undo().unwrap();
        assert_eq!(m.mass(b).unwrap(), sb);
        assert!(near(vol(&m, r), 45.0), "{}", vol(&m, r));
    }

    #[test]
    fn union_and_intersect_follow_operands_across_levels() {
        let (mut m, lv, a, _) = setup();
        let l1 = m.add_level("L1", 3.0).unwrap();
        // on L1, local z -3..0 is world 0..3: overlaps 1x4x3 of the block
        let c = m.add_mass(&cube(l1, (3., 0., -3.), (6., 4., 0.))).unwrap();
        let u = m.apply_boolean(&req(BoolOp::Union, a, c)).unwrap();
        let i = m.apply_boolean(&req(BoolOp::Intersect, a, c)).unwrap();
        assert!(near(vol(&m, u), 72.0), "{}", vol(&m, u));
        assert!(near(vol(&m, i), 12.0), "{}", vol(&m, i));
        let mut dep = m.boolean_dependents(c).unwrap();
        dep.sort();
        assert_eq!(dep, vec![u, i]);
        // a result can be an operand: the chain follows
        let post = m.add_mass(&cube(lv, (3.5, 0., 0.), (4.5, 1., 1.))).unwrap();
        let k = m.apply_boolean(&req(BoolOp::Subtract, i, post)).unwrap();
        assert!(near(vol(&m, k), 12.0 - 0.5), "{}", vol(&m, k));
        m.move_mass(c, (-1.0, 0.0, 0.0)).unwrap();
        assert!(near(vol(&m, i), 24.0), "{}", vol(&m, i));
        assert!(near(vol(&m, k), 24.0 - 0.5), "{}", vol(&m, k));
    }

    #[test]
    fn empty_intersection_refused_model_unchanged() {
        let (mut m, lv, a, _) = setup();
        let d = m.add_mass(&cube(lv, (10., 10., 0.), (11., 11., 1.))).unwrap();
        let (before, n) = (m.masses().unwrap(), ops(&m));
        let e = m
            .apply_boolean(&req(BoolOp::Intersect, a, d))
            .unwrap_err()
            .to_string();
        assert!(e.contains("intersect result is empty"), "{e}");
        let p = m
            .preview_boolean(&req(BoolOp::Intersect, a, d))
            .unwrap_err()
            .to_string();
        assert_eq!(p, e);
        assert_eq!(m.masses().unwrap(), before);
        assert_eq!(ops(&m), n);
        assert!(m.boolean_dependents(a).unwrap().is_empty());
        assert!(m.apply_boolean(&req(BoolOp::Union, a, a)).is_err());
    }

    #[test]
    fn operand_edit_that_empties_result_refused() {
        let (mut m, _, a, b) = setup();
        let r = m.apply_boolean(&req(BoolOp::Intersect, a, b)).unwrap();
        let (sb, sr, n) = (m.mass(b).unwrap(), m.mass(r).unwrap(), ops(&m));
        let e = m.move_mass(b, (20.0, 0.0, 0.0)).unwrap_err().to_string();
        assert!(e.contains("linked result") && e.contains("empty"), "{e}");
        assert_eq!(m.mass(b).unwrap(), sb);
        assert_eq!(m.mass(r).unwrap(), sr);
        assert_eq!(ops(&m), n);
    }

    #[test]
    fn undo_restores_operands_exactly() {
        let (mut m, lv, a, b) = setup();
        let (sa, sb, before) = (m.mass(a).unwrap(), m.mass(b).unwrap(), m.masses().unwrap());
        let r = m.apply_boolean(&req(BoolOp::Subtract, a, b)).unwrap();
        m.update_mass(a, &cube(lv, (0., 0., 0.), (5., 4., 3.))).unwrap();
        assert!(near(vol(&m, r), 57.0), "{}", vol(&m, r));
        m.undo().unwrap();
        assert_eq!(m.mass(a).unwrap(), sa);
        assert!(near(vol(&m, r), 45.0));
        m.undo().unwrap();
        assert_eq!(m.mass(a).unwrap(), sa);
        assert_eq!(m.mass(b).unwrap(), sb);
        assert_eq!(m.masses().unwrap(), before);
        assert!(m.mass(r).is_err());
        assert!(m.boolean_link(r).unwrap().is_none());
        assert!(m.boolean_dependents(a).unwrap().is_empty());
    }

    #[test]
    fn direct_edit_refused_release_and_delete_unlink() {
        let (mut m, _, a, b) = setup();
        let r = m.apply_boolean(&req(BoolOp::Subtract, a, b)).unwrap();
        let e = m.move_mass(r, (1.0, 0.0, 0.0)).unwrap_err().to_string();
        assert!(e.contains("edit an operand"), "{e}");
        m.release_boolean(r).unwrap();
        assert!(m.boolean_link(r).unwrap().is_none());
        m.move_mass(r, (1.0, 0.0, 0.0)).unwrap();
        m.undo().unwrap();
        m.undo().unwrap();
        assert!(m.boolean_link(r).unwrap().is_some());
        // deleting an operand keeps the result with its last tree
        m.delete_mass(b).unwrap();
        assert!(m.boolean_link(r).unwrap().is_none());
        assert!(near(vol(&m, r), 45.0));
        m.undo().unwrap();
        assert_eq!(m.boolean_dependents(b).unwrap(), vec![r]);
    }

    #[test]
    fn void_in_wall_quantities_drop() {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L0", 0.0).unwrap();
        let wt = m
            .add_wall_type(
                "wall",
                &[LayerSpec {
                    material: "brick".into(),
                    thickness: 0.3,
                }],
            )
            .unwrap();
        let w = m
            .add_wall(&WallSpec {
                a: (0., 0.),
                b: (10., 0.),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wt,
                offset0: None,
            })
            .unwrap();
        let qty = |m: &Model| -> f64 {
            m.db.query_row(
                "SELECT coalesce(sum(volume),0) FROM wall_qty WHERE wall=?1",
                [w],
                |r| r.get(0),
            )
            .unwrap()
        };
        let q0 = qty(&m);
        assert!(q0 > 0.0);
        let void = |lo, hi| VoidRequest {
            host: ("wall".into(), w),
            level: lv,
            form: Form::Cuboid { lo, hi },
            name: "opening".into(),
        };
        let n = ops(&m);
        let e = m
            .apply_void(&void((20., -1., 0.), (21., 1., 2.)))
            .unwrap_err()
            .to_string();
        assert!(e.contains("misses"), "{e}");
        assert_eq!(ops(&m), n);
        // 2 m long, through the 0.3 thickness, 2 m high
        let v = m.apply_void(&void((2., -1., 0.), (4., 1., 2.))).unwrap();
        assert!(m.mass(v).unwrap().void);
        let body = m.joined_body("wall", w).unwrap().volume();
        assert!(near(body, q0 - 1.2), "{body} vs {q0}");
        assert!(near(qty(&m), q0 - 1.2), "{} vs {q0}", qty(&m));
        assert_eq!(ops(&m), n + 1);
        m.undo().unwrap();
        assert!(near(qty(&m), q0));
        assert!(m.masses().unwrap().is_empty());
    }

    #[test]
    fn move_operand_reevaluates_result() {
        let (mut m, _lv, a, b) = setup();
        let r = m.apply_boolean(&req(BoolOp::Subtract, a, b)).unwrap();
        assert!(near(vol(&m, r), 45.0));
        let n = ops(&m);
        // the post now sticks out of the block's side: only a 0.5 x 1 x 3 piece is cut
        m.move_elements(&[crate::Target::Mass(b)], (2.5, 0.0)).unwrap();
        assert!(near(vol(&m, r), 46.5), "{}", vol(&m, r));
        assert!(near(vol(&m, a), 48.0) && near(vol(&m, b), 5.0));
        assert_eq!(ops(&m), n + 1, "operand and result move in one undo step");
        m.undo().unwrap();
        assert!(near(vol(&m, r), 45.0), "{}", vol(&m, r));
    }

    #[test]
    fn move_linked_result_refused() {
        let (mut m, _lv, a, b) = setup();
        let r = m.apply_boolean(&req(BoolOp::Subtract, a, b)).unwrap();
        let before = m.mass(r).unwrap();
        let n = ops(&m);
        let e = m
            .move_elements(&[crate::Target::Mass(r)], (1.0, 0.0))
            .unwrap_err()
            .to_string();
        assert!(e.contains("release the link"), "{e}");
        let e = m
            .lift_elements(&[crate::Target::Mass(r)], 1.0)
            .unwrap_err()
            .to_string();
        assert!(e.contains("release the link"), "{e}");
        assert_eq!(m.mass(r).unwrap(), before);
        assert_eq!(ops(&m), n);
        // after release the result is an ordinary form again
        m.release_boolean(r).unwrap();
        m.move_elements(&[crate::Target::Mass(r)], (1.0, 0.0)).unwrap();
    }

    #[test]
    fn mirror_copy_operand_keeps_link_on_original() {
        let (mut m, _lv, a, b) = setup();
        let r = m.apply_boolean(&req(BoolOp::Subtract, a, b)).unwrap();
        let link = m.boolean_link(r).unwrap();
        let n = ops(&m);
        // a mirror of a form is not supported: refused as a whole, links untouched
        assert!(m
            .mirror_elements(&[crate::Target::Mass(b)], (0.0, 0.0), (0.0, 1.0), true)
            .is_err());
        assert_eq!(ops(&m), n);
        assert_eq!(m.masses().unwrap(), vec![a, b, r]);
        // a plain copy is a free form: the original stays the operand
        let c = m
            .copy_elements(&[crate::Target::Mass(b)], &[(6.0, 0.0)])
            .unwrap();
        let crate::Target::Mass(c) = c[0] else {
            panic!("copy of a mass is a mass")
        };
        assert_ne!(c, b);
        assert_eq!(m.boolean_link(r).unwrap(), link);
        assert_eq!(m.boolean_dependents(b).unwrap(), vec![r]);
        assert!(m.boolean_dependents(c).unwrap().is_empty());
        assert!(m.boolean_link(c).unwrap().is_none());
        assert!(near(vol(&m, r), 45.0));
    }

    /// A large ring minus a bar across it: the mesh path builds the stored result (no planar
    /// boolean runs), the result is closed and its volume matches the analytic one.
    #[test]
    fn large_pair_result_comes_from_the_mesh_path() {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L0", 0.0).unwrap();
        let ring = m
            .add_mass(&MassSpec {
                level: lv,
                name: "ring".into(),
                void: false,
                inplace: false,
                form: Form::Revolve {
                    profile: vec![(0.5, 0.0), (1.0, 0.0), (1.0, 1.0), (0.5, 1.0)],
                    axis: (0.0, 0.0),
                    angle: std::f64::consts::TAU,
                    seg: 520,
                    ids: vec![],
                },
            })
            .unwrap();
        let bar = m
            .add_mass(&cube(lv, (-2.0, -0.2, 0.2), (2.0, 0.2, 0.8)))
            .unwrap();
        let planar = super::super::planar_bools();
        let r = m.apply_boolean(&req(BoolOp::Subtract, ring, bar)).unwrap();
        let body = m.mass_body(r).unwrap();
        assert_eq!(super::super::planar_bools(), planar, "planar boolean ran");
        body.check_closed(TOL).unwrap();
        body.validate(TOL).unwrap();
        // 520-gon areas; the bar removes two annulus strips |y| < 0.2, z in [0.2, 0.8]
        let n = 520.0;
        let poly = |r: f64| 0.5 * n * r * r * (std::f64::consts::TAU / n).sin();
        let k = 4000;
        let strip: f64 = (0..k)
            .map(|i| {
                let y = -0.2 + 0.4 * (i as f64 + 0.5) / k as f64;
                2.0 * ((1.0 - y * y).sqrt() - (0.25 - y * y).sqrt()) * 0.4 / k as f64
            })
            .sum();
        let want = poly(1.0) - poly(0.5) - strip * 0.6;
        let got = body.volume();
        assert!((got - want).abs() < 1e-3 * want, "{got} vs {want}");
        assert!(m.bool_warnings().unwrap().is_empty());
    }

    /// A large ring and a far tube whose path crosses itself: the mesh path refuses the pair,
    /// the planar path builds the union, and the refusal reaches the model warnings.
    #[test]
    fn mesh_path_refusal_reaches_model_warnings() {
        let (mut m, lv, a, b) = setup();
        let r0 = m.apply_boolean(&req(BoolOp::Subtract, a, b)).unwrap();
        assert!(m.bool_warnings().unwrap().is_empty());
        let mass = |form| MassSpec {
            level: lv,
            name: "big".into(),
            void: false,
            inplace: false,
            form,
        };
        let ring = m
            .add_mass(&mass(Form::Revolve {
                profile: vec![(0.5, 0.0), (1.0, 0.0), (1.0, 1.0), (0.5, 1.0)],
                axis: (0.0, 0.0),
                angle: std::f64::consts::TAU,
                seg: 520,
                ids: vec![],
            }))
            .unwrap();
        let tube = m
            .add_mass(&mass(Form::Sweep {
                profile: vec![(-0.1, -0.1), (0.1, -0.1), (0.1, 0.1), (-0.1, 0.1)],
                path: vec![
                    (10.0, 0.0, 0.5),
                    (14.0, 0.0, 0.5),
                    (14.0, 2.0, 0.5),
                    (12.0, -2.0, 0.5),
                ],
                ids: vec![],
            }))
            .unwrap();
        let r = m.apply_boolean(&req(BoolOp::Union, ring, tube)).unwrap();
        let w = m.bool_warnings().unwrap();
        assert_eq!(w.len(), 1, "{w:?}");
        assert_eq!((w[0].table, w[0].id), ("mass", r));
        assert!(w[0].message.contains("intersects itself"), "{}", w[0].message);
        assert!(w.iter().all(|w| w.id != r0));
    }
}
