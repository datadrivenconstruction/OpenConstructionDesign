//! Refusals before the first click: a transform reads the capabilities of the selection when a
//! button is drawn or a tool starts, not after the user has already pointed at the canvas.
use super::{of_sel, Cap, XfCaps};
use crate::editor::Sel;
use crate::modify_tools::Op;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XfOp {
    Move,
    Copy,
    Rotate,
    Mirror,
    Array,
}

/// The capability a plan tool maps to; None for tools that don't transform the selection.
pub fn xf_op(op: Op) -> Option<XfOp> {
    Some(match op {
        Op::Move => XfOp::Move,
        Op::Copy => XfOp::Copy,
        Op::Rotate => XfOp::Rotate,
        Op::Mirror | Op::MirrorDraw => XfOp::Mirror,
        Op::Array => XfOp::Array,
        _ => return None,
    })
}

pub fn cap(x: &XfCaps, op: XfOp) -> Cap<()> {
    match op {
        XfOp::Move => x.mv,
        XfOp::Copy => x.copy,
        XfOp::Rotate => x.rotate,
        XfOp::Mirror => x.mirror,
        XfOp::Array => x.array,
    }
}

/// A panel never moves its wall, so it is refused for every transform.
pub const PANEL: &str = "caps.panel";
/// The family row says Yes but the tool does not handle the kind.
pub const UNSUPPORTED: &str = "caps.unsupported";

/// i18n key of the reason `s` cannot take `op`.
pub fn reason(s: Sel, op: XfOp) -> &'static str {
    if matches!(s, Sel::Panel(..)) {
        return PANEL;
    }
    match of_sel(s).map(|f| cap(&f.xf, op)) {
        Some(Cap::No(r)) => r,
        _ => UNSUPPORTED,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Gate {
    /// Nothing to skip, or nothing selected.
    All,
    /// Some of the selection is skipped; the rest runs.
    Partial {
        allowed: Vec<Sel>,
        skipped: Vec<Sel>,
        why: &'static str,
    },
    /// Nothing in the selection can take the transform.
    Refused {
        kinds: Vec<Sel>,
        why: &'static str,
    },
}

/// `handled` is the tool's own answer for a kind: a family row that says No while the tool does
/// the work anyway is not refused.
pub fn gate(items: &[Sel], op: XfOp, handled: impl Fn(Sel) -> bool) -> Gate {
    let (allowed, skipped): (Vec<Sel>, Vec<Sel>) = items.iter().partition(|s| handled(**s));
    let Some(&first) = skipped.first() else {
        return Gate::All;
    };
    let why = reason(first, op);
    if allowed.is_empty() {
        Gate::Refused {
            kinds: skipped,
            why,
        }
    } else {
        Gate::Partial {
            allowed,
            skipped,
            why,
        }
    }
}

/// Status text of a refusal: what was selected and why it stays as it is.
pub fn refusal(why: &str, kinds: &str) -> String {
    let reason = crate::tr(why, &[]);
    crate::tr("caps.refused", &[("kinds", kinds), ("reason", reason.as_str())])
}

/// Tooltip of a disabled command, None while the selection can take it.
pub fn blocked_tip(ed: &crate::editor::Editor, op: Op) -> Option<String> {
    match crate::modify_tools::gate_for(op, &ed.selection) {
        Gate::Refused { kinds, why } => Some(refusal(why, &super::panel_rule::kinds(ed, &kinds))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;
    use crate::modify_tools::Lane;
    use crate::status::Severity;

    // a319e363 gave the stair every transform: a rebar set is the kind still refused alone
    fn stair() -> Sel {
        Sel::Ext(crate::scene_ids::EXT_REBAR, 1)
    }

    #[test]
    fn reason_reads_the_family_row() {
        assert_eq!(reason(stair(), XfOp::Rotate), super::super::REBAR_HOST);
        // annotations take every transform since d0f9a638: the mass keeps a design refusal
        assert_eq!(reason(Sel::Mass(1), XfOp::Rotate), super::super::MASS_TURN);
        assert_eq!(reason(Sel::Anno(1), XfOp::Rotate), UNSUPPORTED);
        assert_eq!(reason(Sel::Panel(1, 2), XfOp::Move), PANEL);
        assert_eq!(reason(Sel::Opening(1), XfOp::Rotate), super::super::HOSTED);
        assert_eq!(reason(Sel::Wall(1), XfOp::Move), UNSUPPORTED);
    }

    #[test]
    fn gate_splits_allowed_and_skipped() {
        let h = |s: Sel| matches!(s, Sel::Wall(_));
        assert_eq!(gate(&[], XfOp::Move, h), Gate::All);
        assert_eq!(gate(&[Sel::Wall(1)], XfOp::Move, h), Gate::All);
        assert!(matches!(
            gate(&[stair()], XfOp::Move, h),
            Gate::Refused { .. }
        ));
        match gate(&[Sel::Wall(1), stair()], XfOp::Move, h) {
            Gate::Partial {
                allowed, skipped, ..
            } => {
                assert_eq!(allowed, vec![Sel::Wall(1)]);
                assert_eq!(skipped, vec![stair()]);
            }
            g => panic!("{g:?}"),
        }
    }

    #[test]
    fn a_tool_that_does_the_work_is_not_refused_by_a_no_row() {
        assert_eq!(gate(&[Sel::Opening(1)], XfOp::Rotate, |_| true), Gate::All);
    }

    #[test]
    fn rotate_on_a_stair_refuses_at_start_with_the_row_reason() {
        let mut ed = Editor::new_in_memory().unwrap();
        ed.selection = vec![stair()];
        let (head, steps) = (ed.model.change_head(), ed.undo_len());
        let mut lane = Lane::default();
        lane.start(Op::Rotate, &mut ed);
        assert!(!crate::interact::sessions::modify::running(&ed), "no tool waits for a click");
        assert_eq!(ed.status_severity(), Some(Severity::Refusal));
        let kinds = super::super::panel_rule::kinds(&ed, &[stair()]);
        assert_eq!(
            ed.status,
            Some(refusal(super::super::REBAR_HOST, &kinds))
        );
        assert_eq!((ed.model.change_head(), ed.undo_len()), (head, steps));
    }

    #[test]
    fn a_wall_and_a_stair_start_on_the_wall_and_name_the_stair() {
        let mut ed = Editor::new_in_memory().unwrap();
        crate::modify_tools::tests::walls(&mut ed, &[((0.0, 0.0), (4.0, 0.0))]);
        let w = Sel::Wall(ed.walls[0].id);
        ed.selection = vec![w, stair()];
        let mut lane = Lane::default();
        lane.start(Op::Move, &mut ed);
        let t = crate::interact::sessions::modify::tool(&ed);
        assert_eq!(t.map(|t| t.targets.clone()), Some(vec![w]));
        assert_eq!(ed.status_severity(), Some(Severity::Warning));
        let name = crate::tr(&format!("selcat.{}", crate::selection::category(&ed, stair())), &[]);
        assert!(ed.status.unwrap().contains(&name));
    }

    #[test]
    fn the_ribbon_blocks_what_cannot_start() {
        let mut ed = Editor::new_in_memory().unwrap();
        ed.selection = vec![stair()];
        assert!(blocked_tip(&ed, Op::Rotate).is_some());
        assert!(blocked_tip(&ed, Op::Offset).is_none());
        ed.selection.clear();
        assert!(blocked_tip(&ed, Op::Rotate).is_none());
    }
}
