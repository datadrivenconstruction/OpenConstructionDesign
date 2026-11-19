// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Material card section for imported meshes: gives the selected meshes this material, or all
//! meshes of the first one's category. The realistic style shows the material's texture on them.
//! Every change is one undo step through `run`.

use ddc_model::MeshScope;

use super::{run, t};
use crate::editor::bodies::BodyKind;
use crate::editor::{Editor, Sel};

/// Ids of the imported meshes in the selection.
fn selected_meshes(ed: &Editor) -> Vec<i64> {
    ed.selection
        .iter()
        .filter_map(|s| match s {
            Sel::Body(BodyKind::Mesh, id) => Some(*id),
            _ => None,
        })
        .collect()
}

enum Act {
    Element(Vec<String>, Option<String>),
    Category(String, Option<String>),
}

pub(super) fn section(ui: &mut egui::Ui, ed: &mut Editor, material: &str) {
    let ids = selected_meshes(ed);
    ui.separator();
    ui.label(egui::RichText::new(t("mat_mesh.mesh.title")).strong());
    let Some(&first) = ids.first() else {
        ui.weak(t("mat_mesh.mesh.none_selected"));
        return;
    };
    let category = ed.model.mesh_category(first).ok().flatten();
    let (own, by_cat) = ed.model.mesh_material_of(first).unwrap_or_default();
    let shown = own.clone().or(by_cat.clone());
    ui.label(match &shown {
        Some(m) => crate::tr("mat_mesh.mesh.current", &[("material", m)]),
        None => t("mat_mesh.mesh.current_none"),
    });
    let n = ids.len().to_string();
    let mut act = None;
    ui.horizontal_wrapped(|ui| {
        if ui
            .button(crate::tr("mat_mesh.mesh.assign", &[("n", &n)]))
            .on_hover_text(t("mat_mesh.mesh.assign_tip"))
            .clicked()
        {
            act = Some(Act::Element(
                ids.iter().map(i64::to_string).collect(),
                Some(material.to_string()),
            ));
        }
        if let Some(c) = &category {
            if ui
                .button(crate::tr("mat_mesh.mesh.assign_cat", &[("category", c)]))
                .on_hover_text(t("mat_mesh.mesh.assign_cat_tip"))
                .clicked()
            {
                act = Some(Act::Category(c.clone(), Some(material.to_string())));
            }
        }
        if own.is_some()
            && ui
                .button(t("mat_mesh.mesh.clear"))
                .on_hover_text(t("mat_mesh.mesh.clear_tip"))
                .clicked()
        {
            act = Some(Act::Element(ids.iter().map(i64::to_string).collect(), None));
        }
        if by_cat.is_some() && category.is_some() && ui.button(t("mat_mesh.mesh.clear_cat")).clicked() {
            act = category.clone().map(|c| Act::Category(c, None));
        }
    });
    if let Some(a) = act {
        let (scope, keys, m) = match a {
            Act::Element(k, m) => (MeshScope::Element, k, m),
            Act::Category(c, m) => (MeshScope::Category, vec![c], m),
        };
        run(ed, move |model| {
            model.set_mesh_material(scope, &keys, m.as_deref())
        });
    }
}
