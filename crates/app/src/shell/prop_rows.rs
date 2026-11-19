//! Properties panel blocks: element header, type block set apart from the instance, list of
//! variants behind a mixed value, imported properties in groups.
use super::panels::common;
use crate::editor::Editor;
use crate::ui_kit;
use crate::units::{display_length, edit_length, Units};
use ddc_ui::icons::IconCache;
use ddc_ui::theme::{space, Role};
use ddc_ui::widgets::shell_kit;
use ddc_ui::{Catalog, Theme};
use egui::{self, RichText};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Element header card: icon, name, type line, number with copy.
pub fn header(
    ui: &mut egui::Ui,
    th: &Theme,
    icons: &mut IconCache,
    icon: &str,
    parts: &super::prop_head::HeaderParts,
) {
    super::prop_head::header_card(ui, th, icons, icon, parts);
}

/// Caption above a group of rows.
pub fn block_title(ui: &mut egui::Ui, th: &Theme, text: &str) {
    ui.add_space(space::SM);
    shell_kit::section_title(ui, th, text);
    ui.add_space(space::XXS);
}

/// Type parameters in a sunken block of their own: read-only here (the type picker's footer
/// edits the type), and the caption says how many elements that changes.
pub fn type_block(
    ui: &mut egui::Ui,
    th: &Theme,
    c: &Catalog,
    rows: &[(&str, String)],
    affects: Option<u64>,
) {
    ui.add_space(space::SM);
    // the inset rows keep the panel's value and unit columns: the key column gives up the left
    // inset, and the unit column (short, left-aligned text) reaches the right edge
    let kw = ui_kit::shared_key_w(ui) - space::SM;
    let pad = space::SM as i8;
    egui::Frame::new()
        .fill(th.c("bg.sunken"))
        .corner_radius(th.radius("md"))
        .inner_margin(egui::Margin { left: pad, right: 0, top: pad, bottom: pad })
        .show(ui, |ui| {
            shell_kit::section_title(ui, th, &c.t("props.type_block"));
            if let Some(n) = affects {
                ui.label(
                    RichText::new(c.plural("props.affects", n))
                        .font(th.font(Role::Caption))
                        .color(th.c("text.secondary")),
                );
            }
            ui.add_space(space::XXS);
            ui_kit::props_at(ui, "sel.type", kw, |ui| {
                for (k, v) in rows {
                    ui_kit::kv_num(ui, k, v.clone());
                }
            });
        });
}

/// Distinct values of a mixed field, most frequent first (ties in order of appearance).
pub fn variants(vals: &[String]) -> Vec<(String, usize)> {
    let mut at: HashMap<&str, usize> = HashMap::new();
    let mut out: Vec<(String, usize)> = vec![];
    for v in vals {
        let i = *at.entry(v.as_str()).or_insert_with(|| {
            out.push((v.clone(), 0));
            out.len() - 1
        });
        out[i].1 += 1;
    }
    out.sort_by(|a, b| b.1.cmp(&a.1));
    out
}

/// «3000 × 12, 3150 × 4» for the tooltip, at most `max` entries then «…».
pub fn variants_text(vals: &[String], max: usize) -> String {
    let v = variants(vals);
    let mut s = v
        .iter()
        .take(max)
        .map(|(k, n)| format!("{k} × {n}"))
        .collect::<Vec<_>>()
        .join(", ");
    if v.len() > max {
        s += ", …";
    }
    s
}

/// Last computed value for a selection: a big selection must not be formatted every frame.
pub struct Cached<K, T> {
    slot: Option<(K, Vec<i64>, Arc<T>)>,
}

impl<K, T> Default for Cached<K, T> {
    fn default() -> Self {
        Self { slot: None }
    }
}

impl<K: PartialEq, T> Cached<K, T> {
    pub fn get(&mut self, key: K, ids: &[i64], build: impl FnOnce() -> T) -> Arc<T> {
        if let Some((k, i, v)) = &self.slot {
            if *k == key && i == ids {
                return v.clone();
            }
        }
        let v = Arc::new(build());
        self.slot = Some((key, ids.to_vec(), v.clone()));
        v
    }
}

/// What the properties panel shows for selected walls.
pub struct WallVals {
    pub common_type: Option<i64>,
    pub first_type: Option<i64>,
    pub common_height: Option<String>,
    pub length: Option<Option<String>>,
    pub volume: f64,
    pub thickness: Option<Option<String>>,
    pub layers: Option<Option<String>>,
    pub height_spread: Option<String>,
    pub length_spread: Option<String>,
}

impl WallVals {
    pub fn build(ed: &Editor, ids: &[i64], u: Units) -> Self {
        let set: HashSet<i64> = ids.iter().copied().collect();
        let walls: Vec<&crate::editor::Wall> =
            ed.walls.iter().filter(|w| set.contains(&w.id)).collect();
        let types: Vec<i64> = walls.iter().map(|w| w.wall_type).collect();
        let by_id: HashMap<i64, &crate::editor::WallType> =
            ed.types.iter().map(|t| (t.id, t)).collect();
        let of_type = |f: &dyn Fn(&crate::editor::WallType) -> String| -> Vec<String> {
            types
                .iter()
                .filter_map(|t| by_id.get(t))
                .map(|t| f(t))
                .collect()
        };
        let heights: Vec<String> = walls.iter().map(|w| edit_length(w.height, u)).collect();
        let lens: Vec<String> = walls
            .iter()
            .map(|w| display_length(super::wall_length(w), u))
            .collect();
        let common_height = common(&heights).flatten();
        let length = common(&lens);
        Self {
            common_type: common(&types).flatten(),
            first_type: types.first().copied(),
            height_spread: common_height.is_none().then(|| variants_text(&heights, 4)),
            length_spread: length
                .clone()
                .flatten()
                .is_none()
                .then(|| variants_text(&lens, 4)),
            common_height,
            length,
            volume: walls.iter().map(|w| w.volume).sum(),
            thickness: common(&of_type(&|t| display_length(t.thickness(), u))),
            layers: common(&of_type(&|t| t.layers.len().to_string())),
        }
    }
}

/// What the properties panel shows for selected openings: width, height, sill.
pub struct OpeningVals {
    pub common: [Option<String>; 3],
    pub spread: [Option<String>; 3],
}

impl OpeningVals {
    pub fn build(ed: &Editor, ids: &[i64], u: Units) -> Self {
        let set: HashSet<i64> = ids.iter().copied().collect();
        let ops: Vec<_> = ed.openings.iter().filter(|o| set.contains(&o.id)).collect();
        let vals: [Vec<String>; 3] = [
            ops.iter().map(|o| edit_length(o.width, u)).collect(),
            ops.iter().map(|o| edit_length(o.height, u)).collect(),
            ops.iter().map(|o| edit_length(o.sill, u)).collect(),
        ];
        let common_of = |v: &Vec<String>| common(v).flatten();
        let spread_of = |v: &Vec<String>| common_of(v).is_none().then(|| variants_text(v, 4));
        Self {
            common: [
                common_of(&vals[0]),
                common_of(&vals[1]),
                common_of(&vals[2]),
            ],
            spread: [
                spread_of(&vals[0]),
                spread_of(&vals[1]),
                spread_of(&vals[2]),
            ],
        }
    }
}

/// «Set.Name» split at the first separator; one between digits belongs to a value («1.5m», «1:50»).
fn set_split(k: &str) -> Option<(&str, &str)> {
    let digit = |c: Option<char>| c.is_some_and(|c| c.is_ascii_digit());
    let at = k.char_indices().find(|&(i, c)| {
        matches!(c, '.' | '/' | ':')
            && !(digit(k[..i].chars().next_back()) && digit(k[i + c.len_utf8()..].chars().next()))
    })?;
    let (g, n) = (&k[..at.0], &k[at.0 + at.1.len_utf8()..]);
    (!g.is_empty() && !n.is_empty()).then_some((g, n))
}

/// Imported properties under their set names: «Set.Name» goes to group «Set», the rest to "".
/// Groups keep the order of first appearance.
pub fn group_props<T: Clone>(rows: &[(String, T)]) -> Vec<(String, Vec<(String, T)>)> {
    let mut out: Vec<(String, Vec<(String, T)>)> = vec![];
    for (k, v) in rows {
        let (g, name) = match set_split(k) {
            Some((g, n)) => (g.to_string(), n.to_string()),
            None => (String::new(), k.clone()),
        };
        match out.iter_mut().find(|e| e.0 == g) {
            Some(e) => e.1.push((name, v.clone())),
            None => out.push((g, vec![(name, v.clone())])),
        }
    }
    out
}

// ---- parameter value lock (docs/design/PANELS.md 1.5)

/// A lockable parameter value: (table of the row, row id, parameter name).
pub type LockKey = (String, i64, String);

thread_local! {
    /// Parameter row under the pointer or in focus in the last frame, with the selected element
    /// that showed it: Ctrl+L acts on it.
    static HOT: std::cell::RefCell<Option<((String, i64), LockKey)>> =
        const { std::cell::RefCell::new(None) };
}

/// Forget the hot row: called before the parameter rows are drawn.
pub fn lock_hot_clear() {
    HOT.with(|h| *h.borrow_mut() = None);
}

pub fn lock_hot_set(owner: (&str, i64), key: LockKey) {
    HOT.with(|h| *h.borrow_mut() = Some(((owner.0.to_string(), owner.1), key)));
}

/// Lock toggle at the end of a parameter row.
pub fn lock_toggle(ui: &mut egui::Ui, locked: bool) -> egui::Response {
    let tip = if locked {
        crate::tr("locked_values.locked_tip", &[])
    } else {
        format!("{}  ·  Ctrl+L", crate::tr("fe.lock", &[]))
    };
    let glyph = if locked { "\u{1f512}" } else { "\u{1f513}" };
    ui.selectable_label(locked, glyph).on_hover_text(tip)
}

/// Locks or unlocks one parameter value as one undo step; the status says which.
pub fn toggle_lock(ed: &mut Editor, key: &LockKey) -> bool {
    let locked = ed.model.param_locked(&key.0, key.1, &key.2).unwrap_or(false);
    let k = key.clone();
    let f = move |m: &mut ddc_model::Model| {
        let it = [(k.0.as_str(), k.1, k.2.as_str())];
        let r = if locked {
            m.clear_param_lock(&it)
        } else {
            m.set_param_lock(&it)
        };
        r.map(|_| ())
    };
    match ed.run_op(crate::editor::OpFn(Arc::new(f))) {
        Ok(()) => {
            let msg = if locked { "locked_values.unlocked" } else { "locked_values.locked" };
            ed.status = Some(crate::tr(msg, &[("name", key.2.as_str())]));
            true
        }
        Err(e) => {
            ed.status = Some(e.to_string());
            false
        }
    }
}

/// Ctrl+L: the hot row of the element still selected, else a hint.
fn lock_cmd(a: &mut super::EditorApp) {
    lock_hot_row(&mut a.ed);
}

fn lock_hot_row(ed: &mut Editor) -> bool {
    let sel = ed
        .selection
        .first()
        .and_then(|s| crate::phase_ui::sel_ref(*s));
    let hot = HOT.with(|h| h.borrow().clone());
    match (sel, hot) {
        (Some((k, id)), Some((owner, key))) if owner.0 == k && owner.1 == id => {
            toggle_lock(ed, &key)
        }
        _ => {
            ed.status = Some(crate::tr("locked_values.no_row", &[]));
            false
        }
    }
}

pub fn register(app: &mut super::EditorApp) {
    app.register_command("props.param_lock", lock_cmd);
}

#[cfg(test)]
mod lock_tests {
    use super::*;
    use ddc_model::{Edit, PVal, ParamSpec, ParamType};

    fn editor() -> (Editor, i64, i64) {
        let mut e = Editor::new_in_memory().unwrap();
        crate::schedule::test_room(&mut e);
        e.model
            .add_param(&ParamSpec {
                name: "Fire".into(),
                ptype: ParamType::Text,
                per_type: false,
                unit: String::new(),
                group: String::new(),
                guid: None,
                categories: vec!["wall".into()],
            })
            .unwrap();
        e.reload().unwrap();
        let (w1, w2) = (e.walls[0].id, e.walls[1].id);
        (e, w1, w2)
    }

    fn fire(id: i64, v: &str) -> Edit {
        Edit::ParamValue {
            param: "Fire".into(),
            kind: "wall".into(),
            id,
            value: Some(PVal::Text(v.into())),
        }
    }

    fn value(e: &Editor, id: i64) -> Option<PVal> {
        let d = e.model.param_by_name("Fire").unwrap().unwrap();
        e.model.param_value(d.id, "wall", id).unwrap()
    }

    fn key(id: i64) -> LockKey {
        ("wall".into(), id, "Fire".into())
    }

    #[test]
    fn lock_one_undo() {
        let (mut e, w, _) = editor();
        assert!(toggle_lock(&mut e, &key(w)));
        assert!(e.model.param_locked("wall", w, "Fire").unwrap());
        e.undo();
        assert!(!e.model.param_locked("wall", w, "Fire").unwrap());
        e.redo();
        assert!(e.model.param_locked("wall", w, "Fire").unwrap());
        // unlock is one step too
        assert!(toggle_lock(&mut e, &key(w)));
        assert!(!e.model.param_locked("wall", w, "Fire").unwrap());
        e.undo();
        assert!(e.model.param_locked("wall", w, "Fire").unwrap());
    }

    #[test]
    fn locked_param_kept_by_bulk_edit() {
        let (mut e, w1, w2) = editor();
        e.apply_xlsx(vec![fire(w1, "EI15")], "").unwrap();
        toggle_lock(&mut e, &key(w1));
        e.apply_xlsx(vec![fire(w1, "EI60"), fire(w2, "EI60")], "")
            .unwrap();
        assert_eq!(value(&e, w1), Some(PVal::Text("EI15".into())));
        assert_eq!(value(&e, w2), Some(PVal::Text("EI60".into())));
        let kept = crate::tr("locked_values.kept", &[("n", "1")]);
        assert!(e.status.as_deref().unwrap_or("").contains(&kept), "{:?}", e.status);
        // only locked values: nothing to apply, still said
        e.apply_xlsx(vec![fire(w1, "EI90")], "").unwrap();
        assert_eq!(value(&e, w1), Some(PVal::Text("EI15".into())));
        assert!(e.status.as_deref().unwrap_or("").contains(&kept), "{:?}", e.status);
        // the entry with family-sheet cells drops it too
        e.apply_xlsx_props(vec![fire(w1, "EI90"), fire(w2, "EI30")], vec![], "")
            .unwrap();
        assert_eq!(value(&e, w1), Some(PVal::Text("EI15".into())));
        assert_eq!(value(&e, w2), Some(PVal::Text("EI30".into())));
    }

    #[test]
    fn unlocked_param_still_written() {
        let (mut e, w1, _) = editor();
        toggle_lock(&mut e, &key(w1));
        toggle_lock(&mut e, &key(w1));
        e.apply_xlsx(vec![fire(w1, "EI45")], "").unwrap();
        assert_eq!(value(&e, w1), Some(PVal::Text("EI45".into())));
    }

    #[test]
    fn ctrl_l_needs_a_hot_row_of_the_selected_element() {
        let (mut e, w1, w2) = editor();
        lock_hot_clear();
        e.selection = vec![crate::editor::Sel::Wall(w1)];
        assert!(!lock_hot_row(&mut e));
        assert_eq!(e.status, Some(crate::tr("locked_values.no_row", &[])));
        // a row left over from another element does nothing
        lock_hot_set(("wall", w2), key(w2));
        assert!(!lock_hot_row(&mut e));
        assert!(!e.model.param_locked("wall", w2, "Fire").unwrap());
        lock_hot_set(("wall", w1), key(w1));
        assert!(lock_hot_row(&mut e));
        assert!(e.model.param_locked("wall", w1, "Fire").unwrap());
        lock_hot_clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_count_and_order() {
        let v: Vec<String> = ["3150", "3000", "3000", "3000", "3150", "2800"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            variants(&v),
            vec![
                ("3000".to_string(), 3),
                ("3150".to_string(), 2),
                ("2800".to_string(), 1)
            ]
        );
        assert_eq!(variants_text(&v, 2), "3000 × 3, 3150 × 2, …");
        assert_eq!(variants_text(&v, 3), "3000 × 3, 3150 × 2, 2800 × 1");
    }

    #[test]
    fn variants_of_a_big_selection_stay_linear() {
        let v: Vec<String> = (0..200_000).map(|i| i.to_string()).collect();
        assert_eq!(variants(&v).len(), 200_000);
    }

    #[test]
    fn cached_rebuilds_only_for_a_new_key_or_selection() {
        let mut c: Cached<u64, u32> = Cached::default();
        let n = std::cell::Cell::new(0);
        let mut get = |k, ids: &[i64]| {
            c.get(k, ids, || {
                n.set(n.get() + 1);
                1
            })
        };
        get(1, &[1, 2]);
        get(1, &[1, 2]);
        assert_eq!(n.get(), 1);
        get(2, &[1, 2]);
        get(2, &[1]);
        assert_eq!(n.get(), 3);
    }

    #[test]
    fn a_value_with_a_point_is_not_a_group() {
        let rows = vec![("1.5m".to_string(), 1), ("Scale 1:50".to_string(), 2)];
        let g = group_props(&rows);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].0, "");
        assert_eq!(g[0].1[0].0, "1.5m");
    }

    #[test]
    fn props_group_by_set_name() {
        let rows = vec![
            ("Common.Fire".to_string(), 1),
            ("Name".to_string(), 2),
            ("Common.Load".to_string(), 3),
            ("Other/Mass".to_string(), 4),
        ];
        let g = group_props(&rows);
        assert_eq!(g.len(), 3);
        assert_eq!(g[0].0, "Common");
        assert_eq!(g[0].1[1], ("Load".to_string(), 3));
        assert_eq!(g[1].0, "");
        assert_eq!(g[2].0, "Other");
    }
}
