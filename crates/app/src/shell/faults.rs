//! Clickable fault counters of the status bar: errors (red) and warnings (yellow) for the whole
//! model. The list is derived from model warnings on demand, cached by model revision and
//! language, never stored. Worded refusals use the status line only and are never counted here.
use super::warn::warning_kind;
use super::EditorApp;
use crate::editor::{Editor, Sel};
use crate::status::Severity;
use crate::ui_kit as kit;
use ddc_model::ElementWarning;
use egui::{self, RichText};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tone {
    /// Something is not built or is broken: sorted first.
    Error,
    /// Built, with a caveat.
    Warning,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Fault {
    pub tone: Tone,
    pub table: &'static str,
    pub id: i64,
    pub level: Option<i64>,
    pub level_name: String,
    pub category: String,
    pub reason: String,
}

impl Fault {
    /// `level > category > element`; a missing level is skipped.
    pub fn path(&self) -> String {
        let el = format!("#{}", self.id);
        if self.level_name.is_empty() {
            format!("{} > {el}", self.category)
        } else {
            format!("{} > {} > {el}", self.level_name, self.category)
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub errors: u64,
    pub warnings: u64,
}

pub fn counts(list: &[Fault]) -> Counts {
    let mut c = Counts::default();
    for f in list {
        match f.tone {
            Tone::Error => c.errors += 1,
            Tone::Warning => c.warnings += 1,
        }
    }
    c
}

/// Tones that get a counter: a tone with zero is hidden.
pub fn visible_tones(c: Counts) -> Vec<Tone> {
    let mut v = Vec::new();
    if c.errors > 0 {
        v.push(Tone::Error);
    }
    if c.warnings > 0 {
        v.push(Tone::Warning);
    }
    v
}

/// Error: the element or part of it is not built at all. Warning: built, with a caveat.
pub fn tone_of(w: &ElementWarning) -> Tone {
    if w.message.starts_with("ramp cannot be built") {
        return Tone::Error;
    }
    match warning_kind(w) {
        "sketch" | "dim_ref" => Tone::Error,
        _ => Tone::Warning,
    }
}

/// Current level first, then errors before warnings, then by element for a stable list.
pub fn order(list: &mut [Fault], current_level: i64) {
    list.sort_by_key(|f| (f.level != Some(current_level), f.tone, f.table, f.id));
}

/// What a click on a counter does.
#[derive(Debug, PartialEq, Eq)]
pub enum Click {
    Nothing,
    /// Index into the list: select, frame and explain.
    One(usize),
    List,
}

pub fn click_plan(list: &[Fault], tone: Tone) -> Click {
    let mut it = list.iter().enumerate().filter(|(_, f)| f.tone == tone);
    match (it.next(), it.next()) {
        (None, _) => Click::Nothing,
        (Some((i, _)), None) => Click::One(i),
        _ => Click::List,
    }
}

/// Status line text for a focused fault: the reason with its path.
pub fn explain(f: &Fault) -> String {
    format!("{} ({})", f.reason, f.path())
}

/// Switches level and selects the element. Kinds without a selection (ramp, zone) return false.
pub fn select_fault(ed: &mut Editor, f: &Fault) -> bool {
    let sel = match f.table {
        "wall" => Sel::Wall(f.id),
        "slab" => Sel::Slab(f.id),
        "opening" => Sel::Opening(f.id),
        "curtain_wall" => Sel::Curtain(f.id),
        _ => return false,
    };
    if let Some(l) = f.level {
        ed.set_level(l);
    }
    ed.selection = vec![sel];
    true
}

/// Builds the sorted list from deduplicated model warnings.
pub fn build(
    ws: &[ElementWarning],
    current_level: i64,
    level_of: impl Fn(&str, i64) -> Option<i64>,
    level_name: impl Fn(i64) -> String,
    category: impl Fn(&str) -> String,
    reason: impl Fn(&ElementWarning) -> String,
) -> Vec<Fault> {
    let mut out: Vec<Fault> = ws
        .iter()
        .map(|w| {
            let level = level_of(w.table, w.id);
            Fault {
                tone: tone_of(w),
                table: w.table,
                id: w.id,
                level,
                level_name: level.map(&level_name).unwrap_or_default(),
                category: category(w.table),
                reason: reason(w),
            }
        })
        .collect();
    order(&mut out, current_level);
    out
}

#[derive(Default)]
pub struct State {
    key: Option<(u64, bool, i64)>,
    pub list: Vec<Fault>,
    pub open: bool,
}

fn model_level(ed: &Editor, table: &str, id: i64) -> Option<i64> {
    if !matches!(table, "wall" | "slab" | "ramp" | "curtain_wall" | "opening") {
        return None;
    }
    ed.model
        .sql()
        .query_row(
            &format!("SELECT level FROM {table} WHERE id=?1"),
            [id],
            |r| r.get(0),
        )
        .ok()
}

impl EditorApp {
    /// Rebuilds the list when the model revision, the language or the current level changed.
    pub(super) fn refresh_faults(&mut self) {
        let key = (
            self.ed.revision,
            self.sh.cat.lang == ddc_ui::Lang::Ru,
            self.ed.level,
        );
        if self.faults.key == Some(key) {
            return;
        }
        let c = &self.sh.cat;
        let ed = &self.ed;
        let names = |id: i64| -> String {
            ed.model
                .sql()
                .query_row("SELECT name FROM level WHERE id=?1", [id], |r| r.get(0))
                .unwrap_or_default()
        };
        let cat_name = |t: &str| -> String {
            let k = format!("faults.cat.{t}");
            let s = c.t(&k);
            if s == k {
                t.to_string()
            } else {
                s
            }
        };
        let list = build(
            &self.warn_uniq,
            ed.level,
            |t, id| model_level(ed, t, id),
            names,
            cat_name,
            super::warn::warning_text,
        );
        self.faults.list = list;
        self.faults.key = Some(key);
    }

    /// Click on a counter: one fault is selected, framed and explained, several open the list.
    pub(super) fn fault_click(&mut self, tone: Tone) {
        match click_plan(&self.faults.list, tone) {
            Click::Nothing => {}
            Click::List => self.faults.open = true,
            Click::One(i) => {
                let f = self.faults.list[i].clone();
                self.focus_fault(&f);
            }
        }
    }

    fn focus_fault(&mut self, f: &Fault) {
        select_fault(&mut self.ed, f);
        self.show_warning(f.table, f.id);
        let sev = match f.tone {
            Tone::Error => Severity::Error,
            Tone::Warning => Severity::Warning,
        };
        self.ed.say(sev, explain(f));
    }

    pub(super) fn faults_window(&mut self, ctx: &egui::Context) {
        let mut open = self.faults.open;
        let mut pick: Option<usize> = None;
        let c = &self.sh.cat;
        let th = &self.sh.theme;
        let list = &self.faults.list;
        kit::Dialog::new(
            "ri3.faults",
            c.tf("faults.title", &[("n", &list.len().to_string())]),
            kit::DialogSize::Tool,
        )
        .size([460.0, 400.0])
        .first_pos(ctx.screen_rect().right_bottom() + egui::vec2(-500.0, -460.0))
        .open(&mut open)
        .show(ctx, |ui| {
            if list.is_empty() {
                kit::empty_note(ui, c.t("faults.none"), None);
                return;
            }
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (i, f) in list.iter().enumerate() {
                        let col = match f.tone {
                            Tone::Error => th.c("error"),
                            Tone::Warning => th.c("warning"),
                        };
                        ui.horizontal(|ui| {
                            let (r, _) = ui
                                .allocate_exact_size(egui::vec2(10.0, 16.0), egui::Sense::hover());
                            ui.painter().circle_filled(r.center(), 4.0, col);
                            if ui.selectable_label(false, f.path()).clicked() {
                                pick = Some(i);
                            }
                        });
                        ui.indent(("ri3.reason", i), |ui| {
                            ui.label(RichText::new(&f.reason).color(th.c("text.secondary")));
                        });
                    }
                });
        });
        self.faults.open = open;
        if let Some(i) = pick {
            let f = self.faults.list[i].clone();
            self.focus_fault(&f);
        }
    }
}

/// One frame of the open fault list with `n` fixture faults for the layout contract.
#[cfg(test)]
pub(crate) fn layout_frame(app: &mut EditorApp, ctx: &egui::Context, n: usize) {
    let ru = app.sh.cat.lang == ddc_ui::Lang::Ru;
    let (category, reason) = if ru {
        (
            "\u{421}\u{442}\u{435}\u{43d}\u{44b}",
            "\u{421}\u{442}\u{435}\u{43d}\u{430} \u{43d}\u{435} \u{441}\u{43e}\u{435}\u{434}\u{438}\
             \u{43d}\u{435}\u{43d}\u{430} \u{441} \u{441}\u{43e}\u{441}\u{435}\u{434}\u{43d}\u{438}\
             \u{43c}\u{438}",
        )
    } else {
        ("Walls", "Wall is not joined to its neighbours")
    };
    app.faults.list = (0..n)
        .map(|i| Fault {
            tone: if i % 2 == 0 { Tone::Error } else { Tone::Warning },
            table: "wall",
            id: 100 + i as i64,
            level: None,
            level_name: "Ground".into(),
            category: category.into(),
            reason: reason.into(),
        })
        .collect();
    app.faults.open = true;
    app.faults_window(ctx);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(table: &'static str, id: i64, m: &str) -> ElementWarning {
        ElementWarning {
            table,
            id,
            message: m.into(),
        }
    }

    fn fault(tone: Tone, table: &'static str, id: i64, level: Option<i64>) -> Fault {
        Fault {
            tone,
            table,
            id,
            level,
            level_name: String::new(),
            category: table.into(),
            reason: "r".into(),
        }
    }

    fn mk(ws: &[ElementWarning], cur: i64, lv: Option<i64>) -> Vec<Fault> {
        build(
            ws,
            cur,
            |_, _| lv,
            |l| format!("L{l}"),
            |t| t.into(),
            |w| w.message.clone(),
        )
    }

    #[test]
    fn status_counts_split_by_tone() {
        let ws = [
            w("ramp", 1, "ramp cannot be built: too steep"),
            w("wall", 2, "overlaps wall 3: x"),
            w("wall", 3, "duplicate instance"),
            w("slab", 4, "open loop in sketch"),
        ];
        let c = counts(&mk(&ws, 1, Some(1)));
        assert_eq!(
            c,
            Counts {
                errors: 2,
                warnings: 2
            }
        );
        assert_eq!(visible_tones(c), vec![Tone::Error, Tone::Warning]);
    }

    #[test]
    fn zero_tone_hidden() {
        assert!(visible_tones(Counts::default()).is_empty());
        let w2 = Counts {
            errors: 0,
            warnings: 2,
        };
        assert_eq!(visible_tones(w2), vec![Tone::Warning]);
        let e1 = Counts {
            errors: 1,
            warnings: 0,
        };
        assert_eq!(visible_tones(e1), vec![Tone::Error]);
    }

    #[test]
    fn single_fault_click_selects_and_explains() {
        let ws = [
            w("wall", 7, "duplicate instance"),
            w("ramp", 2, "ramp cannot be built: x"),
        ];
        let list = mk(&ws, 1, None);
        // errors sort first: ramp is 0, wall is 1
        assert_eq!(click_plan(&list, Tone::Error), Click::One(0));
        assert_eq!(click_plan(&list, Tone::Warning), Click::One(1));
        let mut ed = Editor::new_in_memory().unwrap();
        assert!(select_fault(&mut ed, &list[1]));
        assert_eq!(ed.selection, vec![Sel::Wall(7)]);
        assert!(!select_fault(&mut ed, &list[0]));
        let s = explain(&list[1]);
        assert!(s.contains("duplicate instance") && s.contains("#7"));
    }

    #[test]
    fn several_faults_open_the_list_and_none_does_nothing() {
        let list = [
            fault(Tone::Warning, "wall", 1, None),
            fault(Tone::Warning, "wall", 2, None),
        ];
        assert_eq!(click_plan(&list, Tone::Warning), Click::List);
        assert_eq!(click_plan(&list, Tone::Error), Click::Nothing);
    }

    #[test]
    fn fault_list_orders_context_then_severity() {
        let mut v = vec![
            fault(Tone::Error, "wall", 1, Some(2)),
            fault(Tone::Warning, "wall", 2, Some(1)),
            fault(Tone::Warning, "wall", 3, Some(2)),
            fault(Tone::Error, "wall", 4, Some(1)),
        ];
        order(&mut v, 1);
        let ids: Vec<i64> = v.iter().map(|f| f.id).collect();
        assert_eq!(ids, vec![4, 2, 1, 3]);
    }

    #[test]
    fn path_is_level_category_element() {
        let mut f = fault(Tone::Error, "wall", 5, Some(1));
        f.level_name = "L1".into();
        f.category = "Wall".into();
        assert_eq!(f.path(), "L1 > Wall > #5");
        f.level_name.clear();
        assert_eq!(f.path(), "Wall > #5");
    }
}
