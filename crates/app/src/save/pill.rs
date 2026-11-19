//! The status bar's source pill (`IFC up to date`, `IFC: 12 edits since`; a click saves) and the
//! running save for the tasks pill. The text is read once per change of the journal, not per frame.

use crate::jobs::JobView;
use crate::shell::EditorApp;

/// What the pill says, from the saved mark and the edits after it.
pub fn label(c: &ddc_ui::Catalog, edits: i64) -> String {
    if edits <= 0 {
        c.t("save.pill.up_to_date")
    } else {
        c.tf("save.pill.edits", &[("n", edits.to_string().as_str())])
    }
}

/// Edits the IFC target does not hold, or `None` when this project has no target yet.
pub fn edits(a: &EditorApp) -> Option<i64> {
    let row = a.ed.model.save_target("ifc").ok().flatten()?;
    let mut n = a.ed.model.edits_since(row.last_journal).ok()?;
    // an undo followed by new edits can land on the same journal number
    if n == 0 && a.save.saved_mark.is_some_and(|m| a.ed.edited_since(m)) {
        n = 1;
    }
    Some(n)
}

pub fn text(a: &EditorApp) -> Option<String> {
    let key = (a.ed.project_id(), a.ed.model.change_head());
    let mut cache = a.save.pill.borrow_mut();
    if let Some((k, v)) = cache.as_ref() {
        if *k == key {
            return v.clone();
        }
    }
    let on = a
        .ed
        .model
        .save_target("ifc")
        .ok()
        .flatten()
        .is_some_and(|r| r.over_source);
    let v = edits(a).map(|n| {
        let l = label(&a.sh.cat, n);
        if on {
            format!("{l} · {}", a.sh.cat.t("save.pill.replaces"))
        } else {
            l
        }
    });
    *cache = Some((key, v.clone()));
    v
}

/// This frame's picture of a running save.
pub fn view(a: &EditorApp) -> Option<JobView> {
    let r = a.save.running.as_ref()?;
    let file = r
        .target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned());
    Some(r.job.view(super::job::STAGES, file))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_label_follows_the_count_in_both_languages() {
        for lang in [ddc_ui::Lang::Ru, ddc_ui::Lang::En] {
            let c = ddc_ui::Catalog::new(lang);
            assert_eq!(label(&c, 0), c.t("save.pill.up_to_date"));
            let l = label(&c, 12);
            assert!(l.contains("12") && !l.contains('{'), "{l}");
        }
    }
}
