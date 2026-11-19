//! The receipt an Apply leaves in the chat: what changed by category, the touched elements, and
//! the two things a person wants next: see them (Show) and take exactly this apply back (Undo).

use super::kit::{self, space, Button};
use super::{Action, Cx, Entry};
use crate::ai::plan::ElemDiff;
use crate::ai::preview;
use crate::editor::Sel;
use crate::shell::EditorApp;
use ddc_model::{ElKey, RowKind};
use ddc_ui::theme::Role;
use egui::{self, RichText, Ui};
use std::collections::{BTreeMap, HashSet};

/// Touched elements kept per receipt (and per stored chat row).
pub const IDS_CAP: usize = 500;
/// Categories named on the card; the rest fold into "+N more".
pub const TOP_CATS: usize = 3;
/// Tables whose rows are elements; types, parameters and settings are not counted.
const ELEMENT_TABLES: [&str; 14] = [
    "wall",
    "opening",
    "curtain_wall",
    "slab",
    "room",
    "mep_run",
    "comp",
    "col",
    "beam",
    "ceiling",
    "mesh",
    "railing",
    "roof",
    "stair",
];

/// One applied plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    /// Undo-log row the Apply wrote; the receipt can undo only while it is the newest.
    pub op_seq: i64,
    /// Random stamp of that row: an undone apply's number is reused by the next edit, the stamp
    /// is not. 0 (old rows, unknown) never matches, so such a receipt cannot undo.
    pub stamp: i64,
    pub n: usize,
    /// (table, changes), most first.
    pub cats: Vec<(String, usize)>,
    /// Created and changed elements, at most `IDS_CAP`.
    pub ids: Vec<ElKey>,
    pub undone: bool,
    /// Draft families this apply wrote; the person's "Make working" promotes them.
    pub drafts: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    Ready,
    Undone,
    /// A later edit sits on top of the undo stack.
    Later,
}

impl Receipt {
    /// Counts the checked items' element rows. `None` when the apply touched no element.
    pub fn summarize(op_seq: i64, diff: &[ElemDiff], checked: &[u32]) -> Option<Receipt> {
        let mut by: BTreeMap<&str, usize> = BTreeMap::new();
        let mut ids: Vec<ElKey> = Vec::new();
        let mut seen: HashSet<&ElKey> = HashSet::new();
        let mut n = 0;
        for e in diff {
            let Some(item) = e.item else { continue };
            if !checked.contains(&item) || !ELEMENT_TABLES.contains(&e.key.0.as_str()) {
                continue;
            }
            if !seen.insert(&e.key) {
                continue;
            }
            n += 1;
            *by.entry(e.key.0.as_str()).or_default() += 1;
            if e.kind != RowKind::Deleted && ids.len() < IDS_CAP {
                ids.push(e.key.clone());
            }
        }
        if n == 0 {
            return None;
        }
        let mut cats: Vec<(String, usize)> = by.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        cats.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Some(Receipt {
            op_seq,
            stamp: 0,
            n,
            cats,
            ids,
            undone: false,
            drafts: Vec::new(),
        })
    }

    /// The receipt of the apply that wrote journal row `head` = (seq, stamp).
    pub fn of_head(mut self, head: (i64, i64)) -> Receipt {
        if head.0 == self.op_seq {
            self.stamp = head.1;
        }
        self
    }

    /// The categories named on the card and the changes left in the others.
    pub fn top(&self) -> (&[(String, usize)], usize) {
        let k = self.cats.len().min(TOP_CATS);
        let more = self.cats[k..].iter().map(|c| c.1).sum();
        (&self.cats[..k], more)
    }

    /// `top`: see [`undo_head`].
    pub fn gate(&self, top: Option<(i64, i64)>) -> Gate {
        if self.undone {
            Gate::Undone
        } else if self.stamp != 0 && top == Some((self.op_seq, self.stamp)) {
            Gate::Ready
        } else {
            Gate::Later
        }
    }

    /// The stored chat row text.
    pub fn to_row(&self) -> String {
        let cats: Vec<_> = self.cats.iter().map(|(k, n)| serde_json::json!([k, n])).collect();
        let ids: Vec<_> = self.ids.iter().map(|(k, id)| serde_json::json!([k, id])).collect();
        serde_json::json!({
            "v": 1, "seq": self.op_seq, "stamp": self.stamp, "n": self.n,
            "cats": cats, "ids": ids, "undone": self.undone, "drafts": self.drafts,
        })
        .to_string()
    }

    pub fn from_row(text: &str) -> Option<Receipt> {
        let v: serde_json::Value = serde_json::from_str(text).ok()?;
        let pairs = |key: &str| -> Option<Vec<(String, i64)>> {
            v.get(key)?
                .as_array()?
                .iter()
                .map(|p| Some((p.get(0)?.as_str()?.to_string(), p.get(1)?.as_i64()?)))
                .collect()
        };
        let cats = pairs("cats")?
            .into_iter()
            .map(|(k, n)| (k, n.max(0) as usize))
            .collect();
        let mut ids = pairs("ids")?;
        ids.truncate(IDS_CAP);
        Some(Receipt {
            op_seq: v.get("seq")?.as_i64()?,
            stamp: v.get("stamp").and_then(|s| s.as_i64()).unwrap_or(0),
            n: v.get("n")?.as_u64()? as usize,
            cats,
            ids,
            undone: v.get("undone")?.as_bool()?,
            drafts: v
                .get("drafts")
                .and_then(|d| d.as_array())
                .map(|d| d.iter().filter_map(|n| n.as_str().map(str::to_string)).collect())
                .unwrap_or_default(),
        })
    }
}

/// Selection for the elements that have one (roofs, stairs and railings pick no row).
pub fn selection_of(ids: &[ElKey]) -> Vec<Sel> {
    ids.iter().filter_map(|k| preview::hide_of(k).0).collect()
}

/// Show: selects the touched elements and frames them. False when none can be selected.
pub fn show(app: &mut EditorApp, ids: &[ElKey]) -> bool {
    let sel = selection_of(ids);
    if sel.is_empty() {
        return false;
    }
    app.ed.selection = sel;
    crate::open::summary::frame_selection(app);
    true
}

/// The newest receipt that has not been undone.
pub fn latest(log: &[Entry]) -> Option<i64> {
    log.iter().rev().find_map(|e| match e {
        Entry::Applied(r) if !r.undone => Some(r.op_seq),
        _ => None,
    })
}

/// The journal row (seq, stamp) Ctrl+Z would take back now; `None` when the editor's next undo
/// step owns no journal row (empty stack after a reopen, a section box or underlay step on top).
pub fn undo_head(ed: &crate::editor::Editor) -> Option<(i64, i64)> {
    if !ed.top_owns_ops() {
        return None;
    }
    ed.model.ai_option_head().ok().filter(|h| h.0 > 0)
}

/// Undo: takes back the receipt's own apply, only while it is the top of the undo stack.
pub fn undo(app: &mut EditorApp, log: &mut [Entry], op_seq: i64) -> bool {
    let top = undo_head(&app.ed);
    let Some(r) = log.iter_mut().rev().find_map(|e| match e {
        Entry::Applied(r) if r.op_seq == op_seq && !r.undone => Some(r),
        _ => None,
    }) else {
        return false;
    };
    if r.gate(top) != Gate::Ready {
        return false;
    }
    app.ed.undo();
    if app.ed.model.ai_option_head().ok() == Some((op_seq, r.stamp)) {
        return false;
    }
    r.undone = true;
    true
}

/// The card: quiet, one title line, categories, a collapsed element list, two small actions.
pub fn card(
    ui: &mut Ui,
    cx: &Cx<'_>,
    r: &Receipt,
    top: Option<(i64, i64)>,
    acts: &mut Vec<Action>,
    hot: &mut Option<Vec<ElKey>>,
) {
    let (th, cat) = (cx.th, cx.cat);
    kit::card(ui, th, |ui| {
        ui.label(
            RichText::new(cat.plural("ai.receipt.title", r.n as u64))
                .font(th.font(Role::BodyStrong))
                .color(th.c("text.primary")),
        );
        let (named, more) = r.top();
        let mut parts: Vec<String> = named
            .iter()
            .map(|(t, n)| format!("{}: {n}", super::ctx_chips::cat_name(cat, t)))
            .collect();
        if more > 0 {
            parts.push(cat.tf("ai.receipt.more", &[("n", &more.to_string())]));
        }
        kit::caption(ui, th, &parts.join("  \u{b7}  "));
        if !r.ids.is_empty() {
            egui::CollapsingHeader::new(
                RichText::new(cat.t("ai.receipt.list"))
                    .font(th.font(Role::Caption))
                    .color(th.c("text.secondary")),
            )
            .id_salt(("ddc.ai.receipt", r.op_seq))
            .default_open(false)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
                    for k in &r.ids {
                        let text = format!("{} #{}", super::ctx_chips::cat_name(cat, &k.0), k.1);
                        let row = ui.add(
                            egui::Label::new(
                                RichText::new(text)
                                    .font(th.font(Role::Label))
                                    .color(th.c("text.primary")),
                            )
                            .sense(egui::Sense::hover()),
                        );
                        if row.hovered() {
                            *hot = Some(vec![k.clone()]);
                        }
                    }
                });
            });
        }
        ui.add_space(space::XS);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::SM;
            // undone: the created elements are gone, selecting them would pick nothing
            let can_show = !r.undone && !selection_of(&r.ids).is_empty();
            let tip = cat.t("ai.receipt.show.tip");
            let b = Button::secondary(&cat.t("ai.receipt.show"))
                .small()
                .enabled(can_show)
                .tooltip(&tip, "")
                .show(ui, th);
            if b.hovered() && can_show {
                *hot = Some(r.ids.clone());
            }
            if b.clicked() {
                acts.push(Action::ShowApplied(r.ids.clone()));
            }
            let gate = r.gate(top);
            let (label, tip) = match gate {
                Gate::Ready => ("ai.receipt.undo", cat.t("ai.receipt.undo.tip")),
                Gate::Undone => ("ai.receipt.undone", String::new()),
                Gate::Later => ("ai.receipt.undo", cat.t("ai.receipt.later")),
            };
            if Button::secondary(&cat.t(label))
                .small()
                .enabled(gate == Gate::Ready)
                .tooltip(&tip, "")
                .show(ui, th)
                .clicked()
            {
                acts.push(Action::UndoApplied(r.op_seq));
            }
        });
        if !r.drafts.is_empty() {
            let note = cat.tf("ai.receipt.draft_family", &[("name", &r.drafts.join(", "))]);
            kit::caption(ui, th, &note);
            let tip = cat.t("ai.receipt.promote.tip");
            if Button::secondary(&cat.t("ai.receipt.promote"))
                .small()
                .enabled(!r.undone)
                .tooltip(&tip, "")
                .show(ui, th)
                .clicked()
            {
                acts.push(Action::PromoteFamilies(r.op_seq));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{Editor, OpFn};
    use ddc_model::Model;
    use std::sync::Arc;

    fn row(table: &str, id: i64, kind: RowKind, item: Option<u32>) -> ElemDiff {
        ElemDiff {
            key: (table.to_string(), id),
            kind,
            item,
            level: None,
            label: String::new(),
            cells: Vec::new(),
        }
    }

    fn app() -> EditorApp {
        EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into())
    }

    fn edit(a: &mut EditorApp, name: &'static str) -> i64 {
        a.ed.run_op(OpFn(Arc::new(move |m: &mut Model| m.add_level(name, 3.0).map(|_| ()))))
            .unwrap();
        a.ed.model.last_op_seq().unwrap().unwrap()
    }

    fn sample() -> Vec<ElemDiff> {
        let mut d = Vec::new();
        for i in 1..=8 {
            d.push(row("wall", i, RowKind::Created, Some(1)));
        }
        for i in 1..=4 {
            d.push(row("opening", i, RowKind::Changed, Some(1)));
        }
        d.push(row("slab", 1, RowKind::Deleted, Some(1)));
        d.push(row("room", 1, RowKind::Changed, Some(1)));
        d.push(row("beam", 1, RowKind::Changed, Some(1)));
        d
    }

    #[test]
    fn counts_by_kind_top_three_and_more() {
        let r = Receipt::summarize(5, &sample(), &[1]).unwrap();
        assert_eq!(r.n, 15);
        assert_eq!(r.cats[0], ("wall".to_string(), 8));
        assert_eq!(r.cats[1], ("opening".to_string(), 4));
        let (named, more) = r.top();
        assert_eq!(named.len(), 3);
        assert_eq!(named[2], ("beam".to_string(), 1));
        assert_eq!(more, 2, "room and slab fold into +N more");
        // a deleted element is counted but cannot be shown
        assert_eq!(r.ids.len(), 14);
        assert!(!r.ids.contains(&("slab".to_string(), 1)));
    }

    #[test]
    fn types_and_secondary_rows_are_not_counted() {
        let mut d = sample();
        d.push(row("wall_type", 1, RowKind::Created, Some(1)));
        d.push(row("wall", 99, RowKind::Changed, None));
        let r = Receipt::summarize(5, &d, &[1]).unwrap();
        assert_eq!(r.n, 15);
        assert!(Receipt::summarize(5, &[row("wall_type", 1, RowKind::Created, Some(1))], &[1]).is_none());
    }

    #[test]
    fn ids_are_capped() {
        let d: Vec<_> = (0..IDS_CAP as i64 + 40)
            .map(|i| row("wall", i, RowKind::Created, Some(1)))
            .collect();
        let r = Receipt::summarize(1, &d, &[1]).unwrap();
        assert_eq!(r.n, IDS_CAP + 40);
        assert_eq!(r.ids.len(), IDS_CAP);
    }

    #[test]
    fn partial_apply_counts_only_checked_items() {
        let mut d = sample();
        d.push(row("door", 1, RowKind::Created, Some(2)));
        d.push(row("col", 7, RowKind::Created, Some(2)));
        d.push(row("col", 8, RowKind::Created, Some(3)));
        let r = Receipt::summarize(9, &d, &[2]).unwrap();
        assert_eq!(r.n, 1);
        assert_eq!(r.cats, vec![("col".to_string(), 1)]);
        assert_eq!(r.ids, vec![("col".to_string(), 7)]);
    }

    #[test]
    fn show_selects_exactly_the_touched_elements() {
        let ids = vec![
            ("wall".to_string(), 3),
            ("opening".to_string(), 4),
            ("roof".to_string(), 5),
        ];
        assert_eq!(selection_of(&ids), vec![Sel::Wall(3), Sel::Opening(4)]);
        let mut a = app();
        a.ed.selection = vec![Sel::Wall(99)];
        assert!(!show(&mut a, &[("roof".to_string(), 5)]));
        assert_eq!(a.ed.selection, vec![Sel::Wall(99)], "nothing to select keeps the selection");
    }

    #[test]
    fn undo_takes_back_exactly_the_apply() {
        let mut a = app();
        let earlier = edit(&mut a, "before");
        let seq = edit(&mut a, "applied");
        assert!(seq > earlier);
        let mut log = vec![Entry::Applied(receipt_of(&a, seq))];
        assert_eq!(latest(&log), Some(seq));
        assert!(undo(&mut a, &mut log, seq));
        assert_eq!(a.ed.model.last_op_seq().unwrap(), Some(earlier), "the earlier edit stays");
        assert!(matches!(&log[0], Entry::Applied(r) if r.undone));
        assert_eq!(latest(&log), None);
        assert!(!undo(&mut a, &mut log, seq), "a second press does nothing");
    }

    #[test]
    fn undo_refused_after_a_later_edit() {
        let mut a = app();
        let seq = edit(&mut a, "applied");
        let mut r = receipt_of(&a, seq);
        assert_eq!(r.gate(undo_head(&a.ed)), Gate::Ready);
        edit(&mut a, "later");
        let top = undo_head(&a.ed);
        assert_eq!(r.gate(top), Gate::Later);
        let mut log = vec![Entry::Applied(r.clone())];
        let head = a.ed.model.change_head();
        assert!(!undo(&mut a, &mut log, seq));
        assert_eq!(a.ed.model.change_head(), head, "the model is untouched");
        assert!(matches!(&log[0], Entry::Applied(x) if !x.undone));
        r.undone = true;
        assert_eq!(r.gate(top), Gate::Undone);
    }

    fn receipt_of(a: &EditorApp, seq: i64) -> Receipt {
        let head = a.ed.model.ai_option_head().unwrap();
        let r = Receipt::summarize(seq, &sample(), &[1]).unwrap().of_head(head);
        assert_ne!(r.stamp, 0, "the journal row carries a stamp");
        r
    }

    #[test]
    fn reused_journal_number_is_not_the_apply() {
        let mut a = app();
        edit(&mut a, "before");
        let seq = edit(&mut a, "applied");
        let mut log = vec![Entry::Applied(receipt_of(&a, seq))];
        // Ctrl+Z outside the card, then an unrelated edit takes the freed number
        a.ed.undo();
        let other = edit(&mut a, "unrelated");
        assert_eq!(other, seq, "the undo log reuses the number");
        let head = a.ed.model.change_head();
        assert!(!undo(&mut a, &mut log, seq), "the person's edit is not undone");
        assert_eq!(a.ed.model.change_head(), head);
        assert_eq!(a.ed.model.last_op_seq().unwrap(), Some(other));
        assert!(matches!(&log[0], Entry::Applied(r) if r.gate(undo_head(&a.ed)) == Gate::Later));
    }

    #[test]
    fn journal_row_without_an_editor_step_cannot_undo() {
        // after a reopen the journal row is still there but the editor stack is empty
        let mut a = app();
        let seq = edit(&mut a, "applied");
        let r = receipt_of(&a, seq);
        let mut b = app();
        b.ed.model.add_level("applied", 3.0).unwrap();
        let mut stored = r.clone();
        stored.op_seq = b.ed.model.last_op_seq().unwrap().unwrap();
        stored.stamp = b.ed.model.ai_option_head().unwrap().1;
        assert_eq!(undo_head(&b.ed), None);
        assert_eq!(stored.gate(undo_head(&b.ed)), Gate::Later);
        let at = stored.op_seq;
        let mut log = vec![Entry::Applied(stored)];
        assert!(!undo(&mut b, &mut log, at));
        assert!(b.ed.model.last_op_seq().unwrap().is_some(), "the row stays");
    }

    #[test]
    fn row_round_trip_and_bad_rows() {
        let mut r = Receipt::summarize(5, &sample(), &[1]).unwrap().of_head((5, 41));
        assert_eq!(r.stamp, 41);
        r.undone = true;
        assert_eq!(Receipt::from_row(&r.to_row()), Some(r));
        assert_eq!(Receipt::from_row("not json"), None);
        assert_eq!(Receipt::from_row("{\"v\":1}"), None);
        // a row written before stamps loads, but can never undo
        let old = "{\"v\":1,\"seq\":5,\"n\":1,\"cats\":[[\"wall\",1]],\"ids\":[[\"wall\",1]],\"undone\":false}";
        let o = Receipt::from_row(old).unwrap();
        assert_eq!(o.stamp, 0);
        assert_eq!(o.gate(Some((5, 0))), Gate::Later);
    }
}
