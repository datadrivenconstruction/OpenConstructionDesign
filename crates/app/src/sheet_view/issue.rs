//! Issue: title-block cell editing from the sheet, a revision on several sheets at once. Project fields are edited
//! only in the project info window; here is this sheet's value on top of them.

use super::props::{pick_row, text_row};
use super::*;
use ddc_ui::widgets::{Button, TextField};

/// Title-block cell edit by double click on the sheet.
pub struct StampEdit {
    sheet: i64,
    key: String,
    label: String,
    buf: String,
    /// Value from the project info: a hint in an empty field.
    shared: Option<String>,
    focused: bool,
}

/// Title-block cell under sheet point `p`, mm. The field holds the sheet's value; empty means the project value
/// or an automatic value.
pub(super) fn stamp_edit_at(
    ed: &Editor,
    sheet: i64,
    def: &SheetDef,
    p: [f64; 2],
    ru: bool,
) -> Option<StampEdit> {
    let cells = io::stamp_cells(&def.standard, &def.paper, def.landscape).ok()?;
    let c = cells.into_iter().find(|c| {
        let r = c.rect;
        p[0] >= r[0] && p[0] <= r[0] + r[2] && p[1] >= r[1] && p[1] <= r[1] + r[3]
    })?;
    let own = def
        .fields
        .get(&c.key)
        .filter(|v| !v.trim().is_empty())
        .cloned();
    let project = ed.model.project_fields().unwrap_or_default();
    let shared = io::stamp_from_project(&def.standard, &project)
        .get(&c.key)
        .cloned();
    // not every cell has a label: in some profiles it is a separate neighbouring cell
    let label = if c.label.is_empty() {
        msg(ru, &format!("sheets.stamp.field.{}", c.key), &[])
    } else {
        c.label.clone()
    };
    Some(StampEdit {
        sheet,
        key: c.key,
        label,
        buf: own.unwrap_or_default(),
        shared,
        focused: false,
    })
}

fn save_stamp(ed: &mut Editor, e: &StampEdit) -> Result<(), String> {
    let (sheet, key, value) = (e.sheet, e.key.clone(), e.buf.trim().to_string());
    let def = ed.model.sheet(sheet).map_err(|x| x.to_string())?;
    // no undo step is needed without changes
    if def.fields.get(&key).map_or("", |v| v.trim()) == value {
        return Ok(());
    }
    op(ed, move |m| {
        let mut d = m.sheet(sheet)?;
        if value.is_empty() {
            d.fields.remove(&key);
        } else {
            d.fields.insert(key.clone(), value.clone());
        }
        m.update_sheet(sheet, &d)
    })
}

pub(super) fn stamp_window(
    ctx: &egui::Context,
    ed: &mut Editor,
    ru: bool,
    e: &mut StampEdit,
) -> bool {
    let (mut keep, mut open, mut save) = (true, true, false);
    let mut failed = None;
    kit::Dialog::new(
        "sheet_stamp",
        msg(ru, "sheets.stamp.title", &[]),
        kit::DialogSize::Form,
    )
    .open(&mut open)
    .show(ctx, |ui| {
        let lbl = ui.label(RichText::new(&e.label).strong());
        let hint = e
            .shared
            .clone()
            .unwrap_or_else(|| msg(ru, "sheets.stamp.auto", &[]));
        let th = ddc_ui::theme::of(ui.ctx());
        let w = ui.available_width();
        let r = TextField::new(&mut e.buf)
            .id_salt("sheet.stamp.value")
            .hint(&hint)
            .labelled_by(lbl.id, &e.label)
            .width(w)
            .show(ui, &th)
            .response;
        if !e.focused {
            r.request_focus();
            e.focused = true;
        }
        kit::dialog_footer(ui, |ui| {
            if kit::primary_action(ui, msg(ru, "sheets.ok", &[]), None) {
                save = true;
            }
            if kit::cancel_button(ui, msg(ru, "sheets.cancel", &[])).clicked() {
                keep = false;
            }
            if Button::quiet(&msg(ru, "sheets.project.open", &[]))
                .show(ui, &th)
                .clicked()
            {
                with(|s| s.open_project = true);
            }
        });
    });
    if save {
        match save_stamp(ed, e) {
            Ok(()) => keep = false,
            Err(m) => failed = Some(m),
        }
    }
    if let Some(m) = failed {
        ed.status = Some(m);
    }
    keep && open
}

/// Default issue date: today by the computer clock, DD.MM.YYYY.
fn today() -> String {
    let (y, m, d) = io::today_local().map_or_else(
        || {
            let secs = web_time::SystemTime::now()
                .duration_since(web_time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs() as i64);
            civil(secs.div_euclid(86_400))
        },
        |(y, m, d)| (i64::from(y), m, d),
    );
    format!("{d:02}.{m:02}.{y}")
}

/// Days since 1970-01-01 -> (year, month, day) in the Gregorian calendar.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m as u32, d)
}

/// Issue a revision onto sheets: current, all, or a set, in one undo step.
pub struct Issue {
    mark: String,
    date: String,
    description: String,
    author: String,
    /// 0 current sheet, 1 all sheets, 2 set.
    scope: u8,
    set: Option<i64>,
    err: String,
}

impl Issue {
    pub(super) fn new(ed: &Editor, active: Option<i64>) -> Self {
        let author = ed
            .model
            .project_fields()
            .unwrap_or_default()
            .remove("author")
            .unwrap_or_default();
        Issue {
            mark: String::new(),
            date: today(),
            description: String::new(),
            author,
            scope: if active.is_some() { 0 } else { 1 },
            set: None,
            err: String::new(),
        }
    }

    fn ids(&self, ed: &Editor) -> Vec<i64> {
        match self.scope {
            0 => active().into_iter().collect(),
            1 => ed
                .model
                .sheets()
                .unwrap_or_default()
                .into_iter()
                .map(|s| s.0)
                .collect(),
            _ => ed
                .model
                .sheet_sets()
                .unwrap_or_default()
                .into_iter()
                .find(|s| Some(s.0) == self.set)
                .map(|s| s.2)
                .unwrap_or_default(),
        }
    }
}

fn commit_issue(ed: &mut Editor, i: &Issue, ids: Vec<i64>) -> Result<(), String> {
    let rev = Revision {
        mark: i.mark.trim().to_string(),
        date: i.date.trim().to_string(),
        description: i.description.trim().to_string(),
        author: i.author.trim().to_string(),
    };
    op(ed, move |m| m.issue_revision(&ids, &rev))
}

pub(super) fn issue_window(
    ctx: &egui::Context,
    app: &mut EditorApp,
    ru: bool,
    i: &mut Issue,
) -> bool {
    let sets = app.ed.model.sheet_sets().unwrap_or_default();
    let (mut keep, mut open, mut go) = (true, true, false);
    let has_active = active().is_some();
    let ids = i.ids(&app.ed);
    let none = msg(ru, "sheets.print.none", &[]);
    kit::Dialog::new(
        "sheet_issue",
        msg(ru, "cmd.view.issue_revision", &[]),
        kit::DialogSize::Form,
    )
    .open(&mut open)
    .show(ctx, |ui| {
        let th = ddc_ui::theme::of(ui.ctx());
        let th = &*th;
        let auto = msg(ru, "sheets.stamp.auto", &[]);
        let k = |key: &str| msg(ru, key, &[]);
        text_row(ui, th, &k("sheets.issue.mark"), "sheet.issue.mark", &mut i.mark, &auto, false);
        text_row(ui, th, &k("sheets.issue.date"), "sheet.issue.date", &mut i.date, "", false);
        text_row(
            ui,
            th,
            &k("sheets.issue.description"),
            "sheet.issue.desc",
            &mut i.description,
            "",
            false,
        );
        text_row(ui, th, &k("sheets.issue.author"), "sheet.issue.author", &mut i.author, "", false);
        // scopes that can be chosen now: current needs an open sheet, set needs a set
        let mut scopes: Vec<(u8, String)> = Vec::new();
        if has_active {
            scopes.push((0, k("sheets.issue.current")));
        }
        scopes.push((1, k("sheets.issue.all")));
        if !sets.is_empty() {
            scopes.push((2, k("sheets.issue.set")));
        }
        if !scopes.iter().any(|s| s.0 == i.scope) {
            i.scope = 1;
        }
        let caps: Vec<&str> = scopes.iter().map(|s| s.1.as_str()).collect();
        let mut at = scopes.iter().position(|s| s.0 == i.scope).unwrap_or(0);
        if pick_row(ui, th, &k("sheets.issue.target"), "sheet.issue.scope", &mut at, &caps, "") {
            i.scope = scopes[at].0;
        }
        if i.scope == 2 {
            if i.set.is_none() {
                i.set = sets.first().map(|s| s.0);
            }
            let names: Vec<&str> = sets.iter().map(|s| s.1.as_str()).collect();
            let mut at = sets.iter().position(|s| Some(s.0) == i.set).unwrap_or(0);
            if pick_row(ui, th, tr(ru, "sheet_props.issue.set_name"), "sheet.issue.set", &mut at, &names, "") {
                i.set = sets.get(at).map(|s| s.0);
            }
        }
        if !i.err.is_empty() {
            kit::error_note(ui, &i.err);
        }
        kit::dialog_footer(ui, |ui| {
            let label = msg(ru, "sheets.issue.go", &[("n", &ids.len().to_string())]);
            let why = ids.is_empty().then_some(none.as_str());
            if kit::primary_action(ui, label, why) {
                go = true;
            }
            if kit::cancel_button(ui, msg(ru, "sheets.cancel", &[])).clicked() {
                keep = false;
            }
        });
    });
    if go {
        let n = ids.len().to_string();
        match commit_issue(&mut app.ed, i, ids) {
            Ok(()) => {
                let m = msg(ru, "sheets.issue.done", &[("n", &n)]);
                app.sh.toasts.push(Level::Success, m, None);
                keep = false;
            }
            Err(e) => i.err = e,
        }
    }
    keep && open
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell_point(def: &SheetDef, key: &str) -> [f64; 2] {
        let cells = io::stamp_cells(&def.standard, &def.paper, def.landscape).unwrap();
        let c = cells.iter().find(|c| c.key == key).unwrap();
        [c.rect[0] + 1.0, c.rect[1] + 1.0]
    }

    #[test]
    fn stamp_cell_edits_this_sheet_only_and_hints_the_project_value() {
        let mut ed = Editor::new_in_memory().unwrap();
        let a = ed.model.add_sheet(&SheetDef::new("A-101", "a")).unwrap();
        let def = ed.model.sheet(a).unwrap();
        let at = cell_point(&def, "legal_owner");
        assert!(stamp_edit_at(&ed, a, &def, [1.0, 1.0], true).is_none());
        let mut e = stamp_edit_at(&ed, a, &def, at, true).unwrap();
        assert!(e.buf.is_empty() && e.shared.is_none());

        e.buf = "Own".into();
        save_stamp(&mut ed, &e).unwrap();
        let own = |ed: &Editor| {
            ed.model
                .sheet(a)
                .unwrap()
                .fields
                .get("legal_owner")
                .cloned()
        };
        assert_eq!(own(&ed).as_deref(), Some("Own"));
        ed.undo();
        assert_eq!(own(&ed), None);
        ed.redo();

        // saving without changes adds no undo step
        let revision = ed.revision;
        save_stamp(&mut ed, &e).unwrap();
        assert_eq!(ed.revision, revision);

        // the project info value is only a hint; the field holds just the sheet's value
        ed.model
            .sql()
            .execute(
                "INSERT INTO prop(kind, ref, key, value, unit)
                 VALUES ('project_info', 0, 'client', 'Shared', '')",
                [],
            )
            .unwrap();
        let def = ed.model.sheet(a).unwrap();
        let again = stamp_edit_at(&ed, a, &def, at, true).unwrap();
        assert_eq!(again.buf, "Own");
        assert_eq!(again.shared.as_deref(), Some("Shared"));
    }

    #[test]
    fn gost_cell_without_label_gets_its_name_from_the_catalog() {
        let mut ed = Editor::new_in_memory().unwrap();
        let mut d = SheetDef::new("A-101", "a");
        d.standard = "gost".into();
        let a = ed.model.add_sheet(&d).unwrap();
        let def = ed.model.sheet(a).unwrap();
        let at = cell_point(&def, "developer");
        let e = stamp_edit_at(&ed, a, &def, at, true).unwrap();
        assert_eq!(e.label, crate::app2_fx::fx("issue_developer"));
    }

    #[test]
    fn issue_goes_to_the_chosen_sheets_as_one_undo_step() {
        let mut ed = Editor::new_in_memory().unwrap();
        let a = ed.model.add_sheet(&SheetDef::new("A-101", "a")).unwrap();
        let b = ed.model.add_sheet(&SheetDef::new("A-102", "b")).unwrap();
        let c = ed.model.add_sheet(&SheetDef::new("A-103", "c")).unwrap();
        let s = ed.model.add_sheet_set("S", &[a, b]).unwrap();
        let mut i = Issue::new(&ed, None);
        assert_eq!(i.scope, 1);
        assert_eq!(i.ids(&ed), vec![a, b, c]);
        i.scope = 2;
        i.set = Some(s);
        i.description = "Release".into();
        let ids = i.ids(&ed);
        commit_issue(&mut ed, &i, ids).unwrap();
        let revs = |ed: &Editor, id: i64| ed.model.revisions(id).unwrap();
        assert_eq!(revs(&ed, a)[0].description, "Release");
        assert_eq!(revs(&ed, b).len(), 1);
        assert!(revs(&ed, c).is_empty());
        ed.undo();
        assert!(revs(&ed, a).is_empty() && revs(&ed, b).is_empty());
    }

    #[test]
    fn civil_date_is_gregorian() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(59), (1970, 3, 1));
        // 2024 is a leap year
        assert_eq!(civil(19_782), (2024, 2, 29));
        assert_eq!(civil(20_762), (2026, 11, 5));
    }
}
