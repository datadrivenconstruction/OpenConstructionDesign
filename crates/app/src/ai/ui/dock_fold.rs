//! Opening the assistant must leave the canvas its room: below `CANVAS_MIN` the Properties
//! panel folds while the assistant is open, then the browser if that is not enough; both come
//! back when it closes. Layout only; nothing here touches a draft, Apply or the model.

use super::PANEL_W;
use crate::prefs::Prefs;
use crate::shell::fold::RAIL_W as RAIL;

/// Narrowest canvas the assistant may leave, in points.
pub const CANVAS_MIN: f32 = 960.0;

#[derive(Default, Clone, Copy, Debug, PartialEq)]
pub struct Fold {
    /// The assistant was open on the last call: a fold happens once per opening.
    seen_open: bool,
    /// This module folded Properties and owes it back.
    folded: bool,
    /// This module folded the browser and owes it back.
    folded_browser: bool,
}

impl Fold {
    pub fn folded(&self) -> bool {
        self.folded
    }
}

/// Width left for the views with the assistant `ai_w` wide and the docks as in `p`.
pub fn canvas_width(screen: f32, p: &Prefs, ai_w: f32) -> f32 {
    let side = |open: bool, w: f32| if open { w } else { RAIL };
    screen - side(p.browser_open, p.browser_width) - side(p.props_open, p.props_width) - ai_w
}

/// Once per opening: fold Properties when the canvas would drop under `CANVAS_MIN`, then the
/// browser when it still would; on closing, give them back. A person who reopens a panel while
/// the assistant is open keeps it.
pub fn sync(fold: &mut Fold, p: &mut Prefs, screen: f32, open: bool) {
    // reopened by hand: the setting is the person's again, and a later close of theirs stands
    if fold.folded && p.props_open {
        fold.folded = false;
        p.props_folded = false;
    }
    if fold.folded_browser && p.browser_open {
        fold.folded_browser = false;
        p.browser_folded = false;
    }
    if open && !fold.seen_open {
        fold.seen_open = true;
        let ai_w = p.ai_panel_w.clamp(PANEL_W.0, PANEL_W.1);
        if p.props_open && canvas_width(screen, p, ai_w) < CANVAS_MIN {
            p.props_open = false;
            p.props_folded = true;
            fold.folded = true;
        }
        if p.browser_open && canvas_width(screen, p, ai_w) < CANVAS_MIN {
            p.browser_open = false;
            p.browser_folded = true;
            fold.folded_browser = true;
        }
    } else if !open && fold.seen_open {
        fold.seen_open = false;
        if fold.folded {
            fold.folded = false;
            p.props_folded = false;
            p.props_open = true;
        }
        if fold.folded_browser {
            fold.folded_browser = false;
            p.browser_folded = false;
            p.browser_open = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_window_keeps_properties() {
        let (mut f, mut p) = (Fold::default(), Prefs::default());
        sync(&mut f, &mut p, 3000.0, true);
        assert!(p.props_open && !f.folded());
    }

    #[test]
    fn folds_once_and_restores_on_close() {
        let (mut f, mut p) = (Fold::default(), Prefs::default());
        sync(&mut f, &mut p, 1600.0, true);
        assert!(!p.props_open && f.folded());
        sync(&mut f, &mut p, 1600.0, true);
        assert!(!p.props_open);
        sync(&mut f, &mut p, 1600.0, false);
        assert!(p.props_open && !f.folded());
    }

    #[test]
    fn a_manual_reopen_is_kept_while_open() {
        let (mut f, mut p) = (Fold::default(), Prefs::default());
        sync(&mut f, &mut p, 1600.0, true);
        p.props_open = true;
        sync(&mut f, &mut p, 1600.0, true);
        assert!(p.props_open);
    }

    #[test]
    fn a_save_while_folded_keeps_the_persons_setting() {
        let (mut f, mut p) = (Fold::default(), Prefs::default());
        sync(&mut f, &mut p, 1600.0, true);
        assert!(!p.props_open);
        // the panel still open when the app saves and quits: the next start has Properties
        assert!(Prefs::parse(&p.text()).props_open);
        sync(&mut f, &mut p, 1600.0, false);
        assert!(p.props_open && Prefs::parse(&p.text()).props_open);
    }

    #[test]
    fn a_close_after_a_manual_reopen_stands() {
        let (mut f, mut p) = (Fold::default(), Prefs::default());
        sync(&mut f, &mut p, 1600.0, true);
        p.props_open = true;
        sync(&mut f, &mut p, 1600.0, true);
        p.props_open = false;
        sync(&mut f, &mut p, 1600.0, true);
        assert!(!Prefs::parse(&p.text()).props_open, "saved as the person left it");
        sync(&mut f, &mut p, 1600.0, false);
        assert!(!p.props_open, "closing the assistant does not undo their close");
    }

    #[test]
    fn closed_properties_stay_closed() {
        let (mut f, mut p) = (Fold::default(), Prefs::default());
        p.props_open = false;
        sync(&mut f, &mut p, 1600.0, true);
        sync(&mut f, &mut p, 1600.0, false);
        assert!(!p.props_open);
    }
}
