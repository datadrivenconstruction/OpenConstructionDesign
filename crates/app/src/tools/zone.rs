// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Zone: clicks collect rooms, Enter creates. A room belongs to only one zone.

use super::ui::poly_any;
use super::{Active, Opts, Rec};
use crate::editor::Editor;
use ddc_geom::V2;
use ddc_model::ZoneSpec;
use ddc_ui::i18n::Catalog;
use egui::{self, Color32, Pos2, Stroke};

/// Room under the point: add to the set or remove.
pub fn toggle(ed: &mut Editor, a: &mut Active, q: V2) {
    let Some(id) = ed.room_pick(q) else {
        ed.status = Some(a.t("tool.err.pick"));
        return;
    };
    if let Some(i) = a.picked.iter().position(|x| *x == id) {
        a.picked.remove(i);
        return;
    }
    if ed.model.zone_of_room(id).ok().flatten().is_some() {
        ed.status = Some(a.t("tool.err.zone_taken"));
        return;
    }
    a.picked.push(id);
}

pub fn rec(a: &Active) -> Result<Rec, String> {
    if a.picked.is_empty() {
        return Err(a.t("hint.zone.place"));
    }
    let mut rooms = a.picked.clone();
    rooms.sort_unstable();
    let name = a.opts.name.trim().to_string();
    let base = a.t("tool.zone.name");
    Ok(Rec::new("zone", move |m| {
        let name = if name.is_empty() {
            format!("{base} {}", m.next_zone_number(&base)?)
        } else {
            name.clone()
        };
        m.add_zone(&ZoneSpec {
            name,
            rooms: rooms.clone(),
        })?;
        Ok(1)
    }))
}

/// Collected rooms and the room under the cursor.
pub fn preview(
    p: &egui::Painter,
    ed: &Editor,
    a: &Active,
    fill: Color32,
    st: Stroke,
    to_s: &dyn Fn(V2) -> Pos2,
) {
    let hover = ed.room_pick(a.cur);
    for r in &ed.rooms {
        if !a.picked.contains(&r.id) && hover != Some(r.id) {
            continue;
        }
        if let Some(o) = r.rings.first() {
            let s: Vec<Pos2> = o.iter().map(|q| to_s(*q)).collect();
            poly_any(p, &s, fill, st);
        }
    }
}

pub fn options(ui: &mut egui::Ui, cat: &Catalog, o: &mut Opts) {
    ui.label(cat.t("core_tools.zone.name"));
    ui.add(
        egui::TextEdit::singleline(&mut o.name)
            .desired_width(140.0)
            .hint_text(cat.t("tool.zone.name")),
    );
}
