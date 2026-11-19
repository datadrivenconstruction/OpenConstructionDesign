//! After a hand edit of a room or a storey that a stored project wrote, one toast offers
//! "Pin": the part becomes a geometry lock of the project, so a revision keeps it in place
//! and places the rest around it.

use super::project_plan::{self, PinAt};
use crate::shell::EditorApp;
use ddc_ui::notify::Level;
use std::cell::RefCell;
use std::collections::BTreeSet;

/// Command of the toast button; the shell runs [`fire`] for it.
pub const CMD: &str = "gen.pin";

type Part = (String, PinAt);

thread_local! {
    static SEEN: RefCell<BTreeSet<Part>> = const { RefCell::new(BTreeSet::new()) };
    static OFFER: RefCell<Option<Part>> = const { RefCell::new(None) };
}

/// The part to offer: changed by hand now and not at the last look, a room before a storey.
pub fn fresh(now: &BTreeSet<Part>, seen: &BTreeSet<Part>) -> Option<Part> {
    let new: Vec<&Part> = now.difference(seen).collect();
    new.iter()
        .find(|p| matches!(p.1, PinAt::Room { .. }))
        .or_else(|| new.first())
        .map(|p| (*p).clone())
}

/// Called after the model changed: a toast with "Pin" for a part freshly edited by hand.
pub fn after_change(a: &mut EditorApp) {
    let Ok(now) = project_plan::hand_edited(&a.ed.model) else {
        return;
    };
    let pick = SEEN.with(|s| {
        let pick = fresh(&now, &s.borrow());
        *s.borrow_mut() = now;
        pick
    });
    let Some(part) = pick else {
        return;
    };
    let key = match part.1 {
        PinAt::Room { .. } => "gen.pin.room",
        PinAt::Storey(_) => "gen.pin.storey",
    };
    let (text, label) = (crate::tr(key, &[]), crate::tr("gen.pin.action", &[]));
    OFFER.with(|o| *o.borrow_mut() = Some(part));
    a.sh.toasts.push_cmd(Level::Info, text, label, CMD);
}

/// The toast button: the offered part becomes a geometry lock of its project.
pub fn fire(a: &mut EditorApp) {
    let Some((pid, at)) = OFFER.with(|o| o.borrow_mut().take()) else {
        return;
    };
    let (level, text) = match project_plan::pin(&mut a.ed.model, &pid, &at) {
        Ok(_) => (Level::Success, crate::tr("gen.pin.done", &[])),
        Err(e) => (Level::Error, format!("{}: {e}", crate::tr("gen.pin.failed", &[]))),
    };
    a.sh.toasts.push(level, text, None);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(pid: &str, at: PinAt) -> Part {
        (pid.to_string(), at)
    }

    #[test]
    fn a_fresh_room_comes_before_a_storey_and_a_seen_part_is_not_offered_again() {
        let storey = part("P1", PinAt::Storey(0));
        let room = part("P1", PinAt::Room { storey: 1, index: 4 });
        let now: BTreeSet<Part> = [storey.clone(), room.clone()].into();
        assert_eq!(fresh(&now, &BTreeSet::new()), Some(room.clone()));
        assert_eq!(fresh(&now, &[room.clone()].into()), Some(storey.clone()));
        assert_eq!(fresh(&now, &now), None);
        assert_eq!(fresh(&BTreeSet::new(), &now), None);
    }
}
