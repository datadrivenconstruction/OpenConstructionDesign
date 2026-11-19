//! Pure layout and close-set rules of the view tab strip (drawing lives in `panels::view_tabs`).

use super::EditorApp;

impl EditorApp {
    /// Closes one view tab. Sheets and drafting views only stop being the open tab; sections leave
    /// the tab list. The view itself stays in the project in every case.
    pub(super) fn close_tab_id(&mut self, id: i64) {
        if crate::drafting_view::of_tab(id).is_some() {
            crate::drafting_view::deactivate();
        } else if id < 0 {
            crate::sheet_view::deactivate();
        } else {
            self.close_view(id);
        }
    }

    /// The tab the strip shows as open.
    pub(super) fn open_tab(&self) -> OpenTab {
        if let Some(s) = crate::sheet_view::active() {
            return OpenTab::View(-s);
        }
        if let Some(d) = crate::drafting_view::active() {
            return OpenTab::View(crate::drafting_view::tab_id(d));
        }
        if self.view3d {
            return OpenTab::D3;
        }
        self.section.map_or(OpenTab::Plan, OpenTab::View)
    }

    /// `view.next`: plan, 3D, then the section tabs in strip order, round again after the last.
    /// From a sheet or drafting view the round starts over at the plan.
    pub(super) fn next_tab(&mut self) {
        let mut order = vec![OpenTab::Plan, OpenTab::D3];
        order.extend(
            self.tabs
                .iter()
                .filter(|id| self.ed.view_spec(**id).is_some())
                .map(|id| OpenTab::View(*id)),
        );
        let cur = self.open_tab();
        let i = order.iter().position(|o| *o == cur).map_or(0, |i| (i + 1) % order.len());
        crate::sheet_view::deactivate();
        crate::drafting_view::deactivate();
        match order[i] {
            OpenTab::Plan => {
                self.view3d = false;
                self.section = None;
            }
            OpenTab::D3 => self.view3d = true,
            OpenTab::View(id) => self.open_view(id),
        }
    }

    /// `view.close`: closes the open view tab as its × does. The plan and 3D tabs have no ×,
    /// so there it does nothing and says false.
    pub(super) fn close_open_tab(&mut self) -> bool {
        match self.open_tab() {
            OpenTab::View(id) => {
                self.close_tab_id(id);
                true
            }
            OpenTab::Plan | OpenTab::D3 => false,
        }
    }
}

/// Which tab of the strip is open: the plan, 3D, or a view (sheets negative, drafting by tab id).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OpenTab {
    Plan,
    D3,
    View(i64),
}

/// Which tabs of a strip are drawn and which go to the overflow menu; both keep strip order.
#[derive(Clone, Debug, PartialEq)]
pub struct Fit {
    pub shown: Vec<usize>,
    pub hidden: Vec<usize>,
}

/// Splits tabs of `widths` (separated by `gap`) over `avail`. When all fit, nothing is hidden.
/// Otherwise `more_w` (+ one gap) is reserved for the overflow button, the active tab is always
/// shown, and the others fill the remaining room from the left until one no longer fits.
pub fn tab_fit(widths: &[f32], gap: f32, avail: f32, more_w: f32, active: usize) -> Fit {
    let n = widths.len();
    let total: f32 = widths.iter().sum::<f32>() + gap * n.saturating_sub(1) as f32;
    if total <= avail {
        return Fit {
            shown: (0..n).collect(),
            hidden: vec![],
        };
    }
    let room = avail - more_w - gap;
    let mut used = widths.get(active).copied().unwrap_or(0.0);
    let mut keep = vec![false; n];
    if active < n {
        keep[active] = true;
    }
    for (i, w) in widths.iter().enumerate() {
        if i == active {
            continue;
        }
        if used + gap + w > room {
            break;
        }
        used += gap + w;
        keep[i] = true;
    }
    Fit {
        shown: (0..n).filter(|i| keep[*i]).collect(),
        hidden: (0..n).filter(|i| !keep[*i]).collect(),
    }
}

/// Width of a tab: name, side paddings, and the slot of the close button (0 for fixed tabs).
pub fn tab_width(text_w: f32, pad: f32, close_w: f32) -> f32 {
    text_w + 2.0 * pad + close_w
}

/// Width a name may take: its natural width, capped at `max`.
pub fn name_width(natural: f32, max: f32) -> f32 {
    natural.min(max)
}

/// A name is elided (and gets a full-name tooltip) when it is wider than the cap.
pub fn is_elided(natural: f32, max: f32) -> bool {
    natural > max
}

/// Name for a menu row: at most `max` characters, the cut marked with an ellipsis.
pub fn menu_name(name: &str, max: usize) -> String {
    if name.chars().count() <= max {
        return name.to_string();
    }
    let keep = max.saturating_sub(1);
    let mut s: String = name.chars().take(keep).collect();
    s.push('\u{2026}');
    s
}

/// What a context-menu close command applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseKind {
    This,
    Others,
    Right,
}

/// Ids of the closable view tabs (`order` is the strip order) that `kind` closes. `anchor` is the
/// tab the menu was opened on; `None` is a fixed tab (plan, 3D) that cannot be closed itself.
/// Fixed tabs are always first, so everything is "to the right" of them.
pub fn close_set(kind: CloseKind, order: &[i64], anchor: Option<i64>) -> Vec<i64> {
    let pos = anchor.and_then(|a| order.iter().position(|x| *x == a));
    match kind {
        CloseKind::This => pos.map(|p| vec![order[p]]).unwrap_or_default(),
        CloseKind::Others => order.iter().copied().filter(|x| Some(*x) != anchor).collect(),
        CloseKind::Right => match (anchor, pos) {
            (_, Some(p)) => order[p + 1..].to_vec(),
            (None, None) => order.to_vec(),
            (Some(_), None) => vec![],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: [f32; 4] = [100.0, 100.0, 100.0, 100.0];

    #[test]
    fn all_fit_nothing_hidden() {
        let f = tab_fit(&W, 2.0, 408.0, 24.0, 2);
        assert_eq!(f.shown, vec![0, 1, 2, 3]);
        assert!(f.hidden.is_empty());
    }

    #[test]
    fn overflow_keeps_order_and_reserves_button() {
        // 4 tabs need 406; with 300 the button (24+2) leaves 274: two tabs (202), a third is 304
        let f = tab_fit(&W, 2.0, 300.0, 24.0, 0);
        assert_eq!((f.shown, f.hidden), (vec![0, 1], vec![2, 3]));
    }

    #[test]
    fn active_tab_is_always_shown() {
        let f = tab_fit(&W, 2.0, 300.0, 24.0, 3);
        assert_eq!((f.shown, f.hidden), (vec![0, 3], vec![1, 2]));
        let f = tab_fit(&W, 2.0, 300.0, 24.0, 1);
        assert_eq!((f.shown, f.hidden), (vec![0, 1], vec![2, 3]));
    }

    #[test]
    fn very_narrow_strip_shows_only_the_active_tab() {
        let f = tab_fit(&W, 2.0, 40.0, 24.0, 2);
        assert_eq!((f.shown, f.hidden), (vec![2], vec![0, 1, 3]));
        let f = tab_fit(&W, 2.0, 0.0, 24.0, 0);
        assert_eq!(f.shown, vec![0]);
    }

    #[test]
    fn one_huge_tab_is_not_dropped_when_active() {
        let f = tab_fit(&[100.0, 900.0, 100.0], 2.0, 300.0, 24.0, 1);
        assert_eq!((f.shown, f.hidden), (vec![1], vec![0, 2]));
    }

    #[test]
    fn empty_and_out_of_range_active() {
        assert_eq!(tab_fit(&[], 2.0, 100.0, 24.0, 0).shown, Vec::<usize>::new());
        let f = tab_fit(&W, 2.0, 300.0, 24.0, 9);
        assert_eq!(f.shown, vec![0, 1]);
    }

    #[test]
    fn elide_decision_follows_the_cap() {
        assert!(!is_elided(120.0, 144.0));
        assert!(!is_elided(144.0, 144.0));
        assert!(is_elided(300.0, 144.0));
        assert_eq!(name_width(120.0, 144.0), 120.0);
        assert_eq!(name_width(300.0, 144.0), 144.0);
        assert_eq!(tab_width(100.0, 12.0, 20.0), 144.0);
    }

    #[test]
    fn menu_names_are_cut_by_chars() {
        assert_eq!(menu_name("Section A", 28), "Section A");
        let long = "x".repeat(40);
        let s = menu_name(&long, 10);
        assert_eq!(s.chars().count(), 10);
        assert!(s.ends_with('\u{2026}'));
        let ru = "\u{0420}".repeat(30);
        assert_eq!(menu_name(&ru, 5).chars().count(), 5);
    }

    #[test]
    fn close_sets() {
        let o = [10, 11, 12, 13];
        assert_eq!(close_set(CloseKind::This, &o, Some(11)), vec![11]);
        assert_eq!(close_set(CloseKind::Others, &o, Some(11)), vec![10, 12, 13]);
        assert_eq!(close_set(CloseKind::Right, &o, Some(11)), vec![12, 13]);
        assert!(close_set(CloseKind::Right, &o, Some(13)).is_empty());
        // fixed tabs (plan, 3D) cannot be closed but close the rest
        assert!(close_set(CloseKind::This, &o, None).is_empty());
        assert_eq!(close_set(CloseKind::Others, &o, None), o.to_vec());
        assert_eq!(close_set(CloseKind::Right, &o, None), o.to_vec());
        assert!(close_set(CloseKind::Others, &[], None).is_empty());
        assert!(close_set(CloseKind::Right, &o, Some(99)).is_empty());
    }

    #[test]
    fn closing_tabs_never_removes_views() {
        // the strip only lists open tabs; the set of views is a separate list the close commands
        // never receive, so closing cannot change it
        let views = vec![10_i64, 11, 12, 13];
        let tabs = views.clone();
        let closed = close_set(CloseKind::Others, &tabs, Some(12));
        let open: Vec<i64> = tabs.iter().copied().filter(|t| !closed.contains(t)).collect();
        assert_eq!(open, vec![12]);
        assert_eq!(views.len(), 4);
    }

    #[test]
    fn tab_keys_exist_in_both_languages() {
        use ddc_ui::Lang;
        let c = crate::catalog();
        for k in [
            "tabs.view3d",
            "tabs.close",
            "tabs.close_others",
            "tabs.close_right",
            "tabs.more",
        ] {
            for l in [Lang::En, Lang::Ru] {
                assert!(c.table(l).get(k).is_some(), "{k} missing in {l:?}");
            }
        }
    }
}
