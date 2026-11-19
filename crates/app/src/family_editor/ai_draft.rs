//! A family the assistant drafted, open as a tab of the family editor: the check report of the
//! text (click a finding to see its lines), the changes against the earlier version by statement
//! with a reject button each, and a load that stays blocked while the report has errors.

use std::time::{Duration, Instant};

use ddc_geom::component::doc::{op_diff, FamDoc, OpChange};
use ddc_geom::component::validate::{validate, Limits, Report, Severity};
use ddc_ui::{Catalog, Theme};
use eframe::egui::{self, RichText};

use super::{Session, State, Tab};

/// The report follows the text this long after the last edit.
const RECHECK: Duration = Duration::from_millis(300);

pub struct Meta {
    /// The text this draft is compared with (the project family it edits; empty for a new one).
    pub base: String,
    cache: Option<(String, Report)>,
    changed_at: Instant,
    seen: String,
    /// Lines of the finding the person clicked.
    pub focus: Vec<usize>,
    /// Rounds of the assistant's write, check, fix loop and its estimated tokens.
    pub repair: (u32, u64),
}

impl Meta {
    pub fn new(base: String) -> Self {
        Meta {
            base,
            cache: None,
            changed_at: Instant::now(),
            seen: String::new(),
            focus: vec![],
            repair: (0, 0),
        }
    }

    /// The report of `text`, computed now.
    pub fn report(&mut self, text: &str) -> &Report {
        if self.cache.as_ref().map_or(true, |c| c.0 != text) {
            self.cache = Some((text.to_string(), validate(text, &Limits::default())));
        }
        &self.cache.as_ref().expect("just set").1
    }

    /// Why a load must wait: the first error of the report.
    pub fn blocked(&mut self, text: &str) -> Option<String> {
        let r = self.report(text);
        if r.ok {
            return None;
        }
        r.findings
            .iter()
            .find(|f| f.severity == Severity::Error)
            .map(|f| format!("{} (line {}): {}", f.code, f.lines.first().copied().unwrap_or(0), f.msg))
    }

    pub fn changes(&self, doc: &FamDoc) -> Vec<OpChange> {
        op_diff(&FamDoc::new(self.base.clone()), doc.text())
    }

    /// Report for drawing: follows an edit after `RECHECK`, meanwhile the old one stays.
    fn shown(&mut self, text: &str, ctx: &egui::Context) -> (&Report, bool) {
        if self.seen != text {
            self.seen = text.to_string();
            self.changed_at = Instant::now();
        }
        let fresh = self.cache.as_ref().is_some_and(|c| c.0 == text);
        let mut wait = false;
        if !fresh {
            if self.cache.is_none() || self.changed_at.elapsed() >= RECHECK {
                self.report(text);
            } else {
                wait = true;
                ctx.request_repaint_after(RECHECK);
            }
        }
        (&self.cache.as_ref().expect("computed").1, wait)
    }

    pub fn touched(&mut self) {
        self.changed_at = Instant::now();
    }
}

/// Opens the drafts as tabs; the family a draft edits is linked, so a load replaces it.
pub fn open(st: &mut State, ed: &crate::editor::Editor, drafts: Vec<(String, String, Option<String>, (u32, u64))>) {
    for (_name, text, base_family, repair) in drafts {
        let fam = base_family.as_ref().and_then(|b| {
            ed.model
                .families()
                .ok()?
                .into_iter()
                .find(|f| &f.1 == b)
                .map(|f| f.0)
        });
        let base = fam.and_then(|id| ed.model.family_def(id).ok()).unwrap_or_default();
        let mut s = Session::new(FamDoc::new(text), fam);
        let mut meta = Meta::new(base);
        meta.repair = repair;
        s.ai = Some(meta);
        st.docs.push(s);
        st.active = st.docs.len() - 1;
        st.open = true;
    }
}

/// The panel of an AI tab: report, then changes.
pub fn panel(ui: &mut egui::Ui, th: &Theme, cat: &Catalog, s: &mut Session) {
    let Some(mut meta) = s.ai.take() else {
        return;
    };
    let text = s.doc.text().to_string();
    let (report, waiting) = {
        let (r, w) = meta.shown(&text, ui.ctx());
        (r.clone(), w)
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new(cat.t("fe.ai.badge")).strong().color(th.c("accent")));
        if waiting {
            ui.label(RichText::new(cat.t("fe.ai.checking")).color(th.c("text.secondary")));
        } else if report.ok && report.findings.is_empty() {
            ui.label(cat.t("fe.ai.clean"));
        }
        if meta.repair.0 > 0 {
            let (n, t) = (meta.repair.0.to_string(), meta.repair.1.to_string());
            ui.label(cat.tf("fe.ai.repair", &[("n", &n), ("tokens", &t)]));
        }
        if ui.button(cat.t("fe.ai.check_values")).clicked() {
            s.tab = Tab::Flex;
        }
    });
    for f in &report.findings {
        let tag = if f.severity == Severity::Error { "!" } else { "~" };
        let at: Vec<String> = f.lines.iter().map(|l| l.to_string()).collect();
        let head = format!("{tag} {}  {}", cat.t(&f.key()), cat.tf("fe.ai.lines", &[("n", &at.join(", "))]));
        if ui.selectable_label(meta.focus == f.lines, head).on_hover_text(&f.msg).clicked() {
            meta.focus = f.lines.clone();
        }
    }
    let lines: Vec<&str> = text.lines().collect();
    for l in &meta.focus {
        if let Some(src) = lines.get(l.wrapping_sub(1)) {
            ui.label(RichText::new(format!("{l:>4}  {src}")).monospace());
        }
    }
    let changes = meta.changes(&s.doc);
    if !changes.is_empty() {
        ui.label(RichText::new(cat.tf("fe.ai.changes", &[("n", &changes.len().to_string())])).strong());
    }
    let mut reject = None;
    for (k, c) in changes.iter().enumerate() {
        let (key, st) = match c {
            OpChange::Added(n) => ("fe.ai.added", n),
            OpChange::Removed(o) => ("fe.ai.removed", o),
            OpChange::Changed { new, .. } => ("fe.ai.changed", new),
        };
        let color = match c {
            OpChange::Added(_) => th.c("success"),
            OpChange::Removed(_) => th.c("error"),
            OpChange::Changed { .. } => th.c("warning"),
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(cat.tf(key, &[("head", &st.head), ("name", &st.name)])).color(color));
            if ui.small_button(cat.t("fe.ai.reject")).clicked() {
                reject = Some(k);
            }
        });
    }
    if let Some(k) = reject {
        match s.doc.reject_op(&changes[k]) {
            Ok(()) => {
                meta.touched();
                s.refresh();
            }
            Err(e) => s.msg = Some((true, e.to_string())),
        }
    }
    s.ai = Some(meta);
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = "family Desk\nparam Width length type 1200mm\nextrude Top rect(0, 0, Width, 600mm) z(0, 40mm)\n";

    #[test]
    fn load_blocked_while_report_has_errors() {
        let mut m = Meta::new(OLD.into());
        let bad = OLD.replace("600mm)", "Nowhere)");
        let why = m.blocked(&bad).expect("blocked");
        assert!(why.starts_with("unknown_ref (line 3)"), "{why}");
        assert!(m.blocked(OLD).is_none());
    }

    #[test]
    fn changes_follow_the_text() {
        let m = Meta::new(OLD.into());
        let doc = FamDoc::new(OLD.replace("1200mm", "1400mm"));
        assert_eq!(m.changes(&doc).len(), 1);
        assert!(m.changes(&FamDoc::new(OLD)).is_empty());
    }

    /// A draft's load is one project step, as a hand-made family's.
    #[test]
    fn load_is_one_project_undo() {
        let mut ed = crate::editor::Editor::new_in_memory().unwrap();
        let before = ed.model.journal_len().unwrap();
        let r = super::super::load_set::commit(&mut ed, vec![(OLD.to_string(), false)]);
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(ed.model.journal_len().unwrap(), before + 1);
        // an edit of the loaded family is again exactly one step
        let r = super::super::load_set::commit(&mut ed, vec![(OLD.replace("1200mm", "1400mm"), false)]);
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(ed.model.journal_len().unwrap(), before + 2);
    }
}
