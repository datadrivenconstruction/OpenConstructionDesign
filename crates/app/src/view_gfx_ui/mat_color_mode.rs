// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! "Color by material" for the whole model: every element takes the colour its main material
//! shows in the realistic style (row colour, else class colour), and the viewport's legend panel
//! lists the materials with their counts. Built on the assistant's view colouring, so Reset view
//! (and the legend's rows) work the same way.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use ddc_model::material_look::resolve;

use crate::ai::rows;
use crate::ai::sets::Key;
use crate::ai::view_tools::{legend, Bucket, Legend, ViewCmd};
use crate::editor::Editor;

thread_local! {
    /// What the materials window asked for; run by `frame`.
    static ASK: Cell<Option<bool>> = const { Cell::new(None) };
    /// The `by` text of the legend this mode made (localised), to tell it from other colourings.
    static BY: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Main material of every element: the one with the largest volume, then area (ties by name).
pub fn material_groups(ed: &Editor) -> BTreeMap<String, Vec<Key>> {
    let mut main: HashMap<(&'static str, i64), (String, f64, f64)> = HashMap::new();
    for e in rows::element_materials(&ed.model) {
        if e.material.trim().is_empty() {
            continue;
        }
        let slot = main.entry((e.table, e.id)).or_insert_with(|| (e.material.clone(), -1.0, -1.0));
        let better = (e.volume, e.area) > (slot.1, slot.2)
            || ((e.volume, e.area) == (slot.1, slot.2) && e.material < slot.0);
        if better {
            *slot = (e.material.clone(), e.volume, e.area);
        }
    }
    let mut groups: BTreeMap<String, Vec<Key>> = BTreeMap::new();
    for r in rows::rows(ed).iter() {
        let k = rows::key_of(r);
        let name = main.get(&(k.table(), k.id)).map(|m| m.0.clone()).unwrap_or_default();
        groups.entry(name).or_default().push(k);
    }
    groups
}

/// Legend by material: rows are named with the localised material label and coloured as the
/// material looks in 3D; the palette colour stays for materials with no class and no colour.
pub fn material_legend(
    groups: BTreeMap<String, Vec<Key>>,
    colors: &HashMap<String, u32>,
    label: &dyn Fn(&str) -> String,
    by: &str,
) -> Legend {
    let mut lg = legend(by, groups);
    for r in &mut lg.rows {
        if r.bucket != Bucket::Value {
            continue;
        }
        if let Some(c) = resolve(&r.value, colors.get(&r.value).copied()).map(|x| x.rgb) {
            r.rgb = c;
        }
        r.value = label(&r.value);
    }
    lg
}

/// The mode is the view's colouring now.
pub fn on() -> bool {
    let by = BY.with(|b| b.borrow().clone());
    !by.is_empty() && crate::ai::view_state::legend().is_some_and(|l| l.by == by)
}

/// Asks to turn the mode on or off at the next frame.
pub fn request(on: bool) {
    ASK.with(|a| a.set(Some(on)));
}

/// Runs a pending request; called every frame with the app.
pub fn frame(app: &mut crate::shell::EditorApp) {
    if let Some(want) = ASK.with(Cell::take) {
        set(app, want);
    }
}

fn set(app: &mut crate::shell::EditorApp, want: bool) {
    use crate::ai::view_state::apply;
    if !want {
        if on() {
            apply(app, &ViewCmd::Reset);
            BY.with(|b| b.borrow_mut().clear());
        }
        return;
    }
    let colors: HashMap<String, u32> = app
        .ed
        .model
        .material_rows()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|m| m.spec.color.map(|c| (m.spec.name, c)))
        .collect();
    let by = super::t("view_gfx.mat.color_mode_by");
    let lg = material_legend(material_groups(&app.ed), &colors, &super::materials_win::label, &by);
    BY.with(|b| *b.borrow_mut() = by);
    apply(app, &ViewCmd::Color(Arc::new(lg)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(cat: &'static str, id: i64) -> Key {
        Key { cat, id }
    }

    #[test]
    fn legend_takes_the_material_look_colour_and_the_label() {
        let mut g: BTreeMap<String, Vec<Key>> = BTreeMap::new();
        g.insert("steel S355".into(), vec![k("beam", 1), k("beam", 2)]);
        g.insert("Concrete C30/37".into(), vec![k("wall", 1)]);
        g.insert("zzz unknown".into(), vec![k("wall", 2)]);
        g.insert(String::new(), vec![k("wall", 3)]);
        let mut colors = HashMap::new();
        colors.insert("Concrete C30/37".to_string(), 0x123456u32);
        let lg = material_legend(g, &colors, &|n: &str| format!("<{n}>"), "material");
        let row = |v: &str| lg.rows.iter().find(|r| r.value == v).unwrap_or_else(|| panic!("{v}"));
        let steel = ddc_model::material_look::resolve("steel S355", None).unwrap().rgb;
        assert_eq!(row("<steel S355>").rgb, steel);
        assert_eq!(row("<Concrete C30/37>").rgb, 0x123456, "a row colour wins");
        // no class and no colour: the palette colour stays and the name is still shown
        assert_eq!(lg.rows.iter().filter(|r| r.bucket == Bucket::Value).count(), 3);
        assert_eq!(lg.rows.iter().filter(|r| r.bucket == Bucket::Empty).count(), 1);
        assert_eq!(lg.count(), 5);
    }

    #[test]
    fn groups_use_the_main_material_of_each_element() {
        let mut ed = Editor::new_in_memory().unwrap();
        let (l, t) = (ed.level, ed.types[0].id);
        ed.model
            .add_walls(&[ddc_model::WallSpec {
                a: (0.0, 0.0),
                b: (4.0, 0.0),
                level: l,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            }])
            .unwrap();
        let g = material_groups(&ed);
        let walls: usize = g.values().flatten().filter(|k| k.cat == "wall").count();
        assert_eq!(walls, 1, "each element is in exactly one group");
        assert!(g.keys().any(|n| !n.is_empty()), "the wall has a material: {:?}", g.keys().collect::<Vec<_>>());
    }
}
