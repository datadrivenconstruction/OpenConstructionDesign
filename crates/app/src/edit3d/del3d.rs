//! Delete of 3D picks that have no editor selection (stairs, railings, core and extension
//! families): every pick is deleted through the model, or refused with a reason.

use super::pick::{Cat, Target};
use crate::editor::{Editor, OpFn};
use crate::scene_ids::{EXT_ROOF_EDGE, EXT_SOLID};
use ddc_model::Model;
use std::sync::Arc;

const NOTHING: &str = "edit3d.del.nothing";

/// Picks this module owns: no editor selection, not solids or roof edges (their own flows).
pub fn owned(t: &Target) -> bool {
    t.sel().is_none() && !matches!(t.cat, Cat::Ext(EXT_SOLID) | Cat::Ext(EXT_ROOF_EDGE))
}

/// Deletes the owned picks in one undo step. False: there were none. Otherwise the status line
/// says what was deleted or why nothing was, and the model is untouched on a refusal.
pub fn delete_targets(ed: &mut Editor, ts: &[Target]) -> bool {
    let mine: Vec<&Target> = ts.iter().filter(|t| owned(t)).collect();
    if mine.is_empty() {
        return false;
    }
    let mut items: Vec<(&'static str, i64)> = vec![];
    let mut why = String::new();
    for t in &mine {
        match crate::elem::of_cat(t.cat) {
            Some(f) => match f.xf.delete {
                crate::elem::Cap::Yes(()) => items.push((f.table, t.id)),
                crate::elem::Cap::No(r) => why = crate::tr(r, &[]),
            },
            None => why = crate::tr("edit3d.del.no_family", &[]),
        }
    }
    if items.is_empty() {
        let why = if why.is_empty() {
            crate::tr("edit3d.del.nothing", &[])
        } else {
            why
        };
        ed.status = Some(crate::tr("elem.not_deleted", &[("what", why.as_str())]));
        return true;
    }
    let n = items.len();
    // nothing deleted must fail the op: `run_op` would otherwise push an undo step that
    // undoes the previous edit
    let op = OpFn(Arc::new(move |m: &mut Model| {
        let h = m.change_head();
        m.delete_items("del3d", &items)?;
        if m.change_head() == h {
            return Err(ddc_model::Error::Invalid(NOTHING.into()));
        }
        Ok(())
    }));
    ed.status = Some(match ed.run_op(op) {
        Ok(()) => crate::tr("elem.deleted", &[("what", n.to_string().as_str())]),
        Err(ddc_model::Error::Invalid(k)) if k == NOTHING => {
            let why = crate::tr(NOTHING, &[]);
            crate::tr("elem.not_deleted", &[("what", why.as_str())])
        }
        Err(e) => crate::tr("elem.not_deleted", &[("what", e.to_string().as_str())]),
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom_props::tests::fixture;
    use crate::scene_ids::{EXT_ANALYTICAL, EXT_CORE_BASE, EXT_REBAR};

    #[test]
    fn delete3d_every_cat_changes_or_refuses() {
        let mut fx = fixture();
        let cats = [
            (Cat::Stair, fx.ids["stair"]),
            (Cat::Railing, fx.ids["railing"]),
            (Cat::Ext(EXT_REBAR), 987_654),
            (Cat::Ext(EXT_ANALYTICAL), 987_654),
            (Cat::Ext(0x7777), 1),
        ];
        for (cat, id) in cats {
            let head = fx.ed.model.change_head();
            let pushes = fx.ed.pushes;
            fx.ed.status = None;
            let t = Target::new(cat, id);
            // a319e363: a stair or railing pick is an editor selection, the selection delete owns it
            if let Some(s) = t.sel() {
                assert!(!delete_targets(&mut fx.ed, &[t]), "{cat:?}");
                fx.ed.selection = vec![s];
                fx.ed.delete_selection();
            } else {
                assert!(delete_targets(&mut fx.ed, &[t]), "{cat:?}");
            }
            let said = fx.ed.status.is_some();
            let changed = fx.ed.model.change_head() != head;
            assert!(changed || said, "{cat:?}: silent no-op");
            if matches!(cat, Cat::Stair | Cat::Railing) {
                assert!(changed, "{cat:?} must be deleted");
                fx.ed.undo();
                let back = if cat == Cat::Stair {
                    fx.ed.model.stair(id).is_ok()
                } else {
                    fx.ed.model.railing_path(id).is_ok()
                };
                assert!(back, "{cat:?}: undo restores the row");
            } else {
                assert!(!changed, "{cat:?}: refusal must not touch the model");
                assert_eq!(fx.ed.pushes, pushes, "{cat:?}: refusal must not add an undo step");
            }
        }
    }

    #[test]
    fn selectable_picks_are_not_owned() {
        assert!(!delete_targets(&mut fixture().ed, &[Target::new(Cat::Wall, 1)]));
        // a zone has an editor selection: the selection delete owns it
        let zone = Target::new(Cat::Ext(EXT_CORE_BASE + 2), 1);
        assert!(!delete_targets(&mut fixture().ed, &[zone]));
    }
}
