//! Plan grips and flips for a placed door or window: slide along its wall, flip hand, flip facing.

use crate::editor::{Editor, OpFn, Sel};
use crate::grips::{Change, Grip, Kind, Role};
use crate::shell::EditorApp;
use ddc_geom::{v2, V2};
use ddc_model::{Error, Model, OpeningSpec, Swing};
use std::sync::Arc;

/// Distance of the flip handles from the opening centre, m.
const FACING_OFF: f64 = 0.55;
const HAND_OFF: f64 = 0.3;
const EPS: f64 = 1e-6;

struct Row {
    wall: i64,
    spec: OpeningSpec,
    swing: Swing,
}

fn row(m: &Model, id: i64) -> Result<Row, Error> {
    Ok(m.sql().query_row(
        "SELECT wall, along, width, sill, height, kind, swing_side, swing_hinge \
         FROM opening WHERE id=?1",
        [id],
        |r| {
            Ok(Row {
                wall: r.get(0)?,
                spec: OpeningSpec {
                    along: r.get(1)?,
                    width: r.get(2)?,
                    sill: r.get(3)?,
                    height: r.get(4)?,
                    kind: r.get(5)?,
                },
                swing: Swing {
                    side: r.get(6)?,
                    hinge: r.get(7)?,
                },
            })
        },
    )?)
}

/// Unit direction and length of a straight wall; None for an arc or a degenerate wall.
fn axis(m: &Model, wall: i64) -> Result<Option<(V2, V2, f64)>, Error> {
    if m.wall_bulge(wall)? != 0.0 {
        return Ok(None);
    }
    let (a, b): (V2, V2) =
        m.sql()
            .query_row("SELECT ax, ay, bx, by FROM wall WHERE id=?1", [wall], |r| {
                Ok((v2(r.get(0)?, r.get(1)?), v2(r.get(2)?, r.get(3)?)))
            })?;
    let len = (b - a).len();
    Ok((len > 1e-9).then(|| (a, (b - a) * (1.0 / len), len)))
}

fn left(u: V2) -> V2 {
    v2(-u.y, u.x)
}

/// Grips of one opening: move along the wall; doors and windows also get the two flips.
/// Voids and openings in arc walls keep the slide drag and the panel only.
pub fn grips(m: &Model, id: i64, view: &str) -> Result<Vec<Grip>, Error> {
    let r = row(m, id)?;
    let Some((a, u, _)) = axis(m, r.wall)? else {
        return Ok(vec![]);
    };
    let c = a + u * (r.spec.along + r.spec.width / 2.0);
    let mk = |role, pos| Grip {
        kind: Kind::Opening,
        id,
        role,
        pos,
        view: view.to_string(),
    };
    let mut out = vec![mk(Role::Move, c)];
    if r.spec.kind != "void" {
        out.push(mk(Role::FlipFacing, c + left(u) * FACING_OFF));
        out.push(mk(Role::FlipHand, c + u * (r.spec.width / 2.0 + HAND_OFF)));
    }
    Ok(out)
}

/// The slid opening must not overlap a neighbour in the same wall.
fn check_free(m: &Model, id: i64, wall: i64, s: &OpeningSpec) -> Result<(), Error> {
    let n: i64 = m.sql().query_row(
        "SELECT count(*) FROM opening WHERE wall=?1 AND id<>?2 AND along < ?4 AND along + width > ?3",
        rusqlite::params![wall, id, s.along + EPS, s.along + s.width - EPS],
        |r| r.get(0),
    )?;
    if n > 0 {
        return Err(Error::Invalid(format!(
            "opening {id} would overlap another opening"
        )));
    }
    Ok(())
}

pub fn compute(m: &Model, g: &Grip, to: V2) -> Result<Change, Error> {
    let id = g.id;
    let r = row(m, id)?;
    let (a, u, len) = axis(m, r.wall)?
        .ok_or_else(|| Error::Invalid(format!("opening {id}: wall {} is curved", r.wall)))?;
    let (mut spec, mut swing) = (r.spec, r.swing);
    match g.role {
        Role::Move => {
            let t = (to - a).dot(u) - spec.width / 2.0;
            spec.along = t.clamp(0.0, (len - spec.width).max(0.0));
            check_free(m, id, r.wall, &spec)?;
        }
        Role::FlipHand => swing.hinge = !swing.hinge,
        Role::FlipFacing => swing.side = !swing.side,
        _ => return Err(Error::Invalid("not an opening grip".into())),
    }
    let p0 = a + u * spec.along;
    Ok(Change::Opening(id, spec.clone(), swing, p0, p0 + u * spec.width))
}

pub fn write(m: &mut Model, id: i64, spec: &OpeningSpec, swing: Swing) -> Result<(), Error> {
    m.update_opening_with(id, spec, swing)
}

/// Flips the hinge side (`hand`) or the facing of doors and windows. The ribbon wraps it in
/// one journal record (`flip`); the assistant's Apply already runs inside one.
pub fn flip_each(m: &mut Model, ids: &[i64], hand: bool) -> Result<(), Error> {
    for &id in ids {
        let mut s = m.opening_swing(id)?;
        if hand {
            s.hinge = !s.hinge;
        } else {
            s.side = !s.side;
        }
        m.set_opening_swing(id, s)?;
    }
    Ok(())
}

/// `flip_each` as one journal record, so one undo step undoes every flip.
pub fn flip(m: &mut Model, ids: &[i64], hand: bool) -> Result<(), Error> {
    m.atomic("wall_opening_swing", |m| flip_each(m, ids, hand))
}

/// Flips every selected door or window (hinge side or facing) in one undo step.
/// Returns the number of openings it changed.
pub fn flip_selected(ed: &mut Editor, hand: bool) -> usize {
    let ids: Vec<i64> = ed
        .selection
        .iter()
        .filter_map(|s| match s {
            Sel::Opening(id) => ed.opening(*id),
            _ => None,
        })
        .filter(|o| o.kind != crate::opening_tool::Kind::Void)
        .map(|o| o.id)
        .collect();
    if ids.is_empty() {
        ed.status = Some(crate::tr("opening_grips.flip.none", &[]));
        return 0;
    }
    let n = ids.len();
    let f = OpFn(Arc::new(move |m: &mut Model| flip(m, &ids, hand)));
    match ed.run_op(f) {
        Ok(()) => {
            ed.status = Some(crate::tr("opening_grips.flip.done", &[("n", &n.to_string())]));
            n
        }
        Err(e) => {
            ed.status = Some(crate::tr("opening_grips.flip.fail", &[("why", &e.to_string())]));
            0
        }
    }
}

pub fn register(app: &mut EditorApp) {
    app.register_command("edit.flip_hand", |a| {
        flip_selected(&mut a.ed, true);
    });
    app.register_command("edit.flip_facing", |a| {
        flip_selected(&mut a.ed, false);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grips::{apply, grips as all_grips};
    use ddc_model::WallSpec;

    fn spec(along: f64, kind: &str) -> OpeningSpec {
        OpeningSpec {
            along,
            width: 0.9,
            sill: 0.0,
            height: 2.1,
            kind: kind.into(),
        }
    }

    /// Editor with one 10 m wall and two openings: a door at 1.0 and a window at 6.0.
    fn setup() -> (Editor, i64, i64, i64) {
        let mut e = Editor::new_in_memory().unwrap();
        let (lv, wt) = (e.level, e.types[0].id);
        let w = e
            .model
            .add_wall(&WallSpec {
                a: (0.0, 0.0),
                b: (10.0, 0.0),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wt,
                offset0: None,
            })
            .unwrap();
        let d = e.model.add_opening(w, &spec(1.0, "door")).unwrap();
        let o = e.model.add_opening(w, &spec(6.0, "window")).unwrap();
        e.reload().unwrap();
        (e, w, d, o)
    }

    fn find(m: &Model, id: i64, role: Role) -> Grip {
        all_grips(m, Kind::Opening, id)
            .unwrap()
            .into_iter()
            .find(|g| g.role == role)
            .unwrap_or_else(|| panic!("{role:?}"))
    }

    #[test]
    fn door_and_window_have_move_and_both_flips_void_only_move() {
        let (mut e, w, d, _) = setup();
        let v = e.model.add_opening(w, &spec(8.0, "void")).unwrap();
        let roles = |m: &Model, id| -> Vec<Role> {
            all_grips(m, Kind::Opening, id)
                .unwrap()
                .into_iter()
                .map(|g| g.role)
                .collect()
        };
        let r = roles(&e.model, d);
        assert!(r.contains(&Role::Move) && r.contains(&Role::FlipHand));
        assert!(r.contains(&Role::FlipFacing));
        assert_eq!(roles(&e.model, v), vec![Role::Move]);
        e.model.set_wall_bulge(w, 0.3).unwrap();
        assert!(all_grips(&e.model, Kind::Opening, d).unwrap().is_empty());
    }

    #[test]
    fn move_grip_slides_along_wall_clamped_and_refuses_overlap() {
        let (mut e, _, d, _) = setup();
        let g = find(&e.model, d, Role::Move);
        assert!((g.pos.x - 1.45).abs() < 1e-9 && g.pos.y.abs() < 1e-9);
        apply(&mut e.model, &g, v2(3.45, 1.0)).unwrap();
        assert!((row(&e.model, d).unwrap().spec.along - 3.0).abs() < 1e-9);
        let g = find(&e.model, d, Role::Move);
        apply(&mut e.model, &g, v2(-5.0, 0.0)).unwrap();
        assert!(row(&e.model, d).unwrap().spec.along.abs() < 1e-9);
        let g = find(&e.model, d, Role::Move);
        apply(&mut e.model, &g, v2(99.0, 0.0)).unwrap();
        let end = row(&e.model, d).unwrap().spec.along;
        assert!((end - 9.1).abs() < 1e-9, "clamped to the wall end: {end}");
        // onto the window at 6.0..6.9
        assert!(compute(&e.model, &g, v2(6.45, 0.0)).is_err());
    }

    #[test]
    fn flip_grips_toggle_swing_and_keep_position() {
        let (mut e, _, d, _) = setup();
        let g = find(&e.model, d, Role::FlipHand);
        apply(&mut e.model, &g, g.pos).unwrap();
        let r = row(&e.model, d).unwrap();
        assert_eq!(r.swing, Swing { side: false, hinge: true });
        assert!((r.spec.along - 1.0).abs() < 1e-9);
        let g = find(&e.model, d, Role::FlipFacing);
        apply(&mut e.model, &g, g.pos).unwrap();
        assert_eq!(row(&e.model, d).unwrap().swing, Swing { side: true, hinge: true });
    }

    #[test]
    fn selected_opening_shows_grips_and_grip_flip_is_one_undo_step() {
        let (mut e, _, d, _) = setup();
        e.selection = vec![Sel::Opening(d)];
        let vis = crate::grips::visible(&e);
        assert!(vis.iter().any(|g| g.kind == Kind::Opening && g.role == Role::FlipHand));
        let g = find(&e.model, d, Role::FlipHand);
        let f = OpFn(Arc::new(move |m: &mut Model| apply(m, &g, g.pos)));
        e.run_op(f).unwrap();
        assert!(e.opening(d).unwrap().flip.hinge);
        e.undo();
        assert_eq!(e.model.opening_swing(d).unwrap(), Swing::default());
        e.redo();
        assert!(e.opening(d).unwrap().flip.hinge);
    }

    #[test]
    fn shortcut_flip_hits_all_selected_in_one_step_and_skips_voids() {
        let (mut e, w, d, o) = setup();
        let v = e.model.add_opening(w, &spec(8.0, "void")).unwrap();
        e.reload().unwrap();
        assert_eq!(flip_selected(&mut e, false), 0, "nothing selected");
        e.selection = vec![Sel::Opening(d), Sel::Opening(o), Sel::Opening(v)];
        assert_eq!(flip_selected(&mut e, false), 2);
        let sw = |e: &Editor, id| e.model.opening_swing(id).unwrap();
        assert!(sw(&e, d).side && sw(&e, o).side && !sw(&e, v).side);
        assert!(!sw(&e, d).hinge);
        e.undo();
        assert_eq!(sw(&e, d), Swing::default());
        assert_eq!(sw(&e, o), Swing::default());
        e.redo();
        assert!(sw(&e, d).side && sw(&e, o).side);
        assert_eq!(flip_selected(&mut e, true), 2);
        assert!(sw(&e, d).hinge);
    }

    #[test]
    fn strings_exist_in_both_locales_without_cyrillic_in_en() {
        for k in ["opening_grips.flip_hand", "opening_grips.flip_facing", "opening_grips.flip.done", "opening_grips.flip.none"] {
            let en = ddc_ui::i18n::Catalog::new(ddc_ui::i18n::Lang::En);
            let (a, b) = (
                en.t_in(ddc_ui::i18n::Lang::En, k),
                en.t_in(ddc_ui::i18n::Lang::Ru, k),
            );
            assert!(a != k && b != k, "{k}");
            assert!(!a.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c)), "{k}");
        }
    }
}
