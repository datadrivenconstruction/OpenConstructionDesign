// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Component library in one window: sources (built-in, in project, my library) and
//! categories on the left, cards with thumbnails (offscreen render, cached by family hash) in the centre,
//! the selection on the right: type sizes, parameters with values, format readiness. Double click
//! or "Place" activates the "Component" tool with this type; a card can be dragged onto the plan
//! or into 3D (the host is chosen from the family's base). "Parameters…" opens the parameter editor.
use std::collections::HashMap;
use std::sync::Arc;

use super::param_editor::{kind_label_l, value_text, Draft};
use crate::complib::{Entry, Ready, Source};
use crate::tools::component::Pick;
use crate::ui_kit as kit;
use ddc_geom::component::{thumb, Family, Hosting, Lod, Value};
use ddc_ui::theme::{icon_size, space, stroke, Role};
use ddc_ui::widgets::{self, Cell, Column, RowState, Table, TextField, Tone};
use ddc_ui::{Catalog, Theme};
use egui::{self, Align, Layout};

/// Width of the count column of the source and category lists.
const COUNT_W: f32 = 40.0;
/// Width of the left pane (sources, categories).
const SIDE_W: f32 = 176.0;
/// Width of the right pane (the picked family).
const DETAILS_W: f32 = 300.0;
/// Card of the grid and its thumbnail.
const CARD_W: f32 = 132.0;
const CARD_H: f32 = 168.0;
const THUMB: u32 = thumb::SIZE;
/// Thumbnail of the picked type in the details pane.
const BIG_THUMB: u32 = 128;
/// New thumbnails rendered per frame; the rest follow on the next frames.
const THUMBS_PER_FRAME: usize = 6;
/// Width of the kind column of the parameter table.
const KIND_W: f32 = 84.0;
/// Width of the formula-mark column of the parameter table.
const MARK_W: f32 = 28.0;

#[derive(Clone, Debug, Default)]
pub struct Library {
    pub query: String,
    pub source: Source,
    /// Category filter (`Category::name`); None — all.
    pub category: Option<String>,
    pub pick: Option<String>,
    /// Picked type of the picked family; None — its first.
    pub ty: Option<String>,
    /// "Load into project" was pressed: (family, definition) — handled by the browser.
    pub load: Option<(String, Arc<str>)>,
    /// Double-click or «Place»: arm the Component tool with this type.
    pub place: Option<Pick>,
    /// «Open folder» of an empty user library.
    pub open_dir: bool,
    /// Command id requested by an empty-state action (run by the browser).
    pub cmd: Option<&'static str>,
}

/// Action of the empty list: with no query there is nothing to reset, so offer to load a family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmptyAction {
    LoadFamily,
    ResetSearch,
}

pub fn empty_action(query: &str) -> EmptyAction {
    if query.trim().is_empty() {
        EmptyAction::LoadFamily
    } else {
        EmptyAction::ResetSearch
    }
}

/// Families matching the search (name or category), grouped by category.
pub fn grouped(fams: &[Family], query: &str) -> Vec<(String, Vec<usize>)> {
    let q = query.trim().to_lowercase();
    let mut out: Vec<(String, Vec<usize>)> = vec![];
    for (i, f) in fams.iter().enumerate() {
        let cat = f.category.name().to_string();
        if !q.is_empty() && !f.name.to_lowercase().contains(&q) && !cat.contains(&q) {
            continue;
        }
        match out.iter_mut().find(|(c, _)| *c == cat) {
            Some((_, v)) => v.push(i),
            None => out.push((cat, vec![i])),
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// i18n key of a family category; `None` keeps the raw name.
fn category_key(c: &str) -> Option<&'static str> {
    Some(match c {
        "furniture" => "type_dialog.libcat.furniture",
        "generic" => "type_dialog.libcat.generic",
        "casework" => "type_dialog.libcat.casework",
        "plumbing" => "type_dialog.libcat.plumbing",
        "lighting" => "type_dialog.libcat.lighting",
        "equipment" => "type_dialog.libcat.equipment",
        "mechanical" => "type_dialog.libcat.mechanical",
        "electrical" => "type_dialog.libcat.electrical",
        "air_terminal" => "library.cat.air_terminal",
        "door" => "library.cat.door",
        "window" => "library.cat.window",
        "stairs" => "library.cat.stairs",
        "structural" => "library.cat.structural",
        "pipe_fitting" => "library.cat.pipe_fitting",
        "duct_fitting" => "library.cat.duct_fitting",
        "planting" => "library.cat.planting",
        "entourage" => "library.cat.entourage",
        "vehicle" => "library.cat.vehicle",
        _ => return None,
    })
}

pub fn category_ru(c: &str) -> String {
    category_key(c).map_or_else(|| c.to_string(), |k| crate::tr_lang(true, k, &[]))
}

/// Category name in the language of the catalog.
pub fn category_label(cat: &Catalog, c: &str) -> String {
    category_key(c).map_or_else(|| c.to_string(), |k| cat.t(k))
}

/// Hosting of a family in the language of the catalog.
pub fn host_label(cat: &Catalog, h: Hosting) -> String {
    cat.t(match h {
        Hosting::None => "library.host.none",
        Hosting::Wall => "library.host.wall",
        Hosting::Floor => "library.host.floor",
        Hosting::Ceiling => "library.host.ceiling",
        Hosting::Face => "library.host.face",
    })
}

/// IFC class of a family in the language of the catalog; an unknown class stays as written.
pub fn ifc_label(cat: &Catalog, class: &str) -> String {
    let key = match class {
        "IfcFurniture" => "library.ifc.furniture",
        "IfcBuildingElementProxy" => "library.ifc.proxy",
        "IfcSanitaryTerminal" => "library.ifc.sanitary",
        "IfcLightFixture" => "library.ifc.light",
        "IfcAirTerminal" => "library.ifc.air_terminal",
        "IfcOutlet" => "library.ifc.outlet",
        "IfcElectricDistributionBoard" => "library.ifc.board",
        "IfcSpaceHeater" => "library.ifc.heater",
        "IfcSwitchingDevice" => "library.ifc.switch",
        "IfcUnitaryEquipment" => "library.ifc.unitary",
        "IfcAirTerminalBox" => "library.ifc.air_box",
        "IfcCovering" => "library.ifc.covering",
        "IfcGeographicElement" => "library.ifc.geographic",
        "IfcMember" => "library.ifc.member",
        "IfcRailing" => "library.ifc.railing",
        "IfcShadingDevice" => "library.ifc.shading",
        "IfcSlab" => "library.ifc.slab",
        "IfcDoor" => "library.ifc.door",
        "IfcWindow" => "library.ifc.window",
        "IfcStair" => "library.ifc.stair",
        "IfcPipeFitting" => "library.ifc.pipe_fitting",
        "IfcDuctFitting" => "library.ifc.duct_fitting",
        "IfcBeam" => "library.ifc.beam",
        "IfcColumn" => "library.ifc.column",
        "IfcElectricAppliance" => "library.ifc.appliance",
        "IfcFireSuppressionTerminal" => "library.ifc.fire_terminal",
        "IfcHeatExchanger" => "library.ifc.heat_exchanger",
        "IfcPump" => "library.ifc.pump",
        "IfcTank" => "library.ifc.tank",
        "IfcTransportElement" => "library.ifc.transport",
        "IfcValve" => "library.ifc.valve",
        "IfcWasteTerminal" => "library.ifc.waste_terminal",
        "IfcMedicalDevice" => "library.ifc.medical",
        other => return other.to_string(),
    };
    cat.t(key)
}

/// Widget id of the search field.
pub fn search_id() -> egui::Id {
    egui::Id::new("c.lib.search")
}

fn matches(f: &Family, q: &str) -> bool {
    let has = |s: &str| s.to_lowercase().contains(q);
    crate::fam_label::matches(&f.name, q)
        || f.category.name().contains(q)
        || has(&f.ifc.0)
        || f.types.iter().any(|t| has(&t.name))
        || f.params.iter().any(|p| has(&p.name))
}

/// Entries of the picked source and category that match the query (name, category, IFC class,
/// type or parameter name), by category, then name.
pub fn filtered(entries: &[Entry], lib: &Library) -> Vec<usize> {
    let mut out = in_source(entries, lib);
    out.retain(|&i| {
        lib.category
            .as_deref()
            .map_or(true, |c| c == entries[i].fam.category.name())
    });
    out
}

/// Entries of the picked source that match the query, any category.
fn in_source(entries: &[Entry], lib: &Library) -> Vec<usize> {
    let q = lib.query.trim().to_lowercase();
    let mut out: Vec<usize> = (0..entries.len())
        .filter(|&i| entries[i].source == lib.source && (q.is_empty() || matches(&entries[i].fam, &q)))
        .collect();
    let key = |i: usize| (entries[i].fam.category.name(), entries[i].fam.name.as_str());
    out.sort_by(|&a, &b| key(a).cmp(&key(b)));
    out
}

/// The picked entry of the picked source.
pub fn picked<'a>(entries: &'a [Entry], lib: &Library) -> Option<&'a Entry> {
    let n = lib.pick.as_deref()?;
    entries.iter().find(|e| e.source == lib.source && e.fam.name == n)
}

/// What a drag or a double-click of `e` places: its type `ty` (else the first one); the
/// definition goes along for a family the project does not have yet.
pub fn pick_of(e: &Entry, ty: Option<&str>) -> Option<Pick> {
    let t = e
        .fam
        .types
        .iter()
        .find(|t| Some(t.name.as_str()) == ty)
        .or(e.fam.types.first())?;
    Some(Pick {
        family: e.fam.name.clone(),
        ty: t.name.clone(),
        def: (e.source != Source::Project).then(|| e.def.clone()),
    })
}

/// The window over the gathered entries; `false` closes it. `edit` receives the draft for the
/// parameter editor.
pub fn show(
    ctx: &egui::Context,
    th: &Theme,
    cat: &Catalog,
    entries: &[Entry],
    lib: &mut Library,
    edit: &mut Option<Draft>,
) -> bool {
    let mut open = true;
    kit::Dialog::new(
        "component_library",
        cat.t("type_dialog.library.component_library"),
        kit::DialogSize::Workspace,
    )
    .stable()
    .open(&mut open)
    .show(ctx, |ui| {
        search(ui, th, cat, lib);
        ui.add_space(space::SM);
        let picked = picked(entries, lib);
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            let foot = footer(ui, cat, picked, lib, edit);
            ui.separator();
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                ui.horizontal_top(|ui| {
                    let h = ui.available_height();
                    let pane = |ui: &mut egui::Ui, w: f32, add: &mut dyn FnMut(&mut egui::Ui)| {
                        ui.allocate_ui_with_layout(egui::vec2(w, h), Layout::top_down(Align::Min), |ui| {
                            ui.set_min_size(egui::vec2(w, h));
                            add(ui)
                        })
                        .response
                        .rect
                    };
                    pane(ui, SIDE_W, &mut |ui| sources(ui, th, cat, entries, lib));
                    ui.separator();
                    let mid = (ui.available_width() - DETAILS_W - space::MD).max(CARD_W);
                    let cards = pane(ui, mid, &mut |ui| grid(ui, th, cat, entries, lib));
                    ui.separator();
                    let w = ui.available_width();
                    let det = pane(ui, w, &mut |ui| details(ui, th, cat, picked, lib));
                    note_rects(ui.ctx(), [cards, det, foot]);
                });
            });
        });
    });
    open
}

/// Test hook: remembers the list, details and footer rects of the frame.
#[cfg(test)]
fn note_rects(ctx: &egui::Context, r: [egui::Rect; 3]) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("ux63a.rects"), r));
}

#[cfg(not(test))]
fn note_rects(_ctx: &egui::Context, _r: [egui::Rect; 3]) {}

/// Test hook: where the card named `label` was drawn.
#[cfg(test)]
fn note_card(ctx: &egui::Context, label: &str, r: egui::Rect) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("c.lib.card").with(label), r));
}

#[cfg(not(test))]
fn note_card(_ctx: &egui::Context, _label: &str, _r: egui::Rect) {}

/// Search field on top, same look as the browser search.
fn search(ui: &mut egui::Ui, th: &Theme, cat: &Catalog, lib: &mut Library) {
    let hint = cat.t("type_dialog.library.search_families");
    let tip = cat.t("library.search_clear");
    let w = ui.available_width();
    TextField::new(&mut lib.query)
        .id(search_id())
        .hint(&hint)
        .labelled_by(search_id().with("lbl"), &hint)
        .width(w)
        .search("find", space::SM, space::SM + icon_size::SM + space::SM, &tip)
        .show(ui, th);
}

/// Footer at the bottom of the dialog: Load is the primary, Place and Parameters are quiet, the
/// drag hint fills the rest. Returns its rect.
fn footer(
    ui: &mut egui::Ui,
    cat: &Catalog,
    picked: Option<&Entry>,
    lib: &mut Library,
    edit: &mut Option<Draft>,
) -> egui::Rect {
    let out = ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let loaded = picked.is_some_and(|e| e.in_project);
            let why = if picked.is_none() {
                cat.t("type_dialog.library.pick_a_family")
            } else {
                cat.t("type_dialog.library.family_is_already_in")
            };
            let load = cat.t("type_dialog.library.load_into_project");
            let r = kit::primary_button(ui, load, picked.is_some() && !loaded);
            if r.on_disabled_hover_text(why).clicked() {
                if let Some(e) = picked {
                    lib.load = Some((e.fam.name.clone(), e.def.clone()));
                }
            }
            let what = picked.and_then(|e| pick_of(e, lib.ty.as_deref()));
            let place = ui
                .add_enabled_ui(what.is_some(), |ui| kit::secondary_button(ui, cat.t("complib.place")))
                .inner;
            if place.on_hover_text(cat.t("complib.place_tip")).clicked() {
                lib.place = what;
            }
            let params = cat.t("type_dialog.library.parameters");
            let quiet = ui
                .add_enabled_ui(picked.is_some(), |ui| kit::secondary_button(ui, params))
                .inner;
            if quiet.clicked() {
                if let Some(e) = picked {
                    *edit = Some(Draft::from_family(&e.fam));
                }
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                kit::elided_weak(ui, &cat.t("complib.drag_hint"));
            });
        });
    });
    out.response.rect
}

/// Row picked by the keyboard in a table of `n` rows: `m` steps from `on`.
fn step(on: Option<usize>, m: isize, n: usize) -> usize {
    let last = n as isize - 1;
    let from = on.map_or(if m > 0 { -1 } else { last + 1 }, |p| p as isize);
    (from + m).clamp(0, last) as usize
}

/// One-column list with a count column; returns the clicked or keyboard-picked row.
fn pick_list(
    ui: &mut egui::Ui,
    th: &Theme,
    id: &str,
    title: &str,
    rows: &[(String, usize)],
    on: Option<usize>,
) -> Option<usize> {
    let cols = [
        Column::text(title).sortable(false),
        Column::text("").fixed(COUNT_W).sortable(false),
    ];
    let mut sort = None;
    let out = Table::new(id, &cols, rows.len()).show(
        ui,
        th,
        &mut sort,
        |r| if Some(r) == on { RowState::Selected } else { RowState::Normal },
        |r, c| {
            if c == 0 {
                Cell::text(rows[r].0.clone())
            } else {
                Cell::muted(rows[r].1.to_string())
            }
        },
    );
    out.clicked.or(out.moved.map(|m| step(on, m, rows.len())))
}

/// Left pane: sources, then the categories of the picked source.
fn sources(ui: &mut egui::Ui, th: &Theme, cat: &Catalog, entries: &[Entry], lib: &mut Library) {
    egui::ScrollArea::vertical()
        .id_salt("c.lib.side")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let rows: Vec<(String, usize)> = Source::ALL
                .iter()
                .map(|s| (cat.t(s.key()), entries.iter().filter(|e| e.source == *s).count()))
                .collect();
            let on = Source::ALL.iter().position(|s| *s == lib.source);
            if let Some(r) = pick_list(ui, th, "c.lib.src", &cat.t("complib.source"), &rows, on) {
                if Source::ALL[r] != lib.source {
                    lib.source = Source::ALL[r];
                    lib.category = None;
                    lib.pick = None;
                    lib.ty = None;
                }
            }
            ui.add_space(space::MD);
            let idx = in_source(entries, lib);
            let mut cats: Vec<(&'static str, usize)> = vec![];
            for &i in &idx {
                let c = entries[i].fam.category.name();
                match cats.iter_mut().find(|x| x.0 == c) {
                    Some(x) => x.1 += 1,
                    None => cats.push((c, 1)),
                }
            }
            let mut rows = vec![(cat.t("complib.all"), idx.len())];
            rows.extend(cats.iter().map(|(c, n)| (category_label(cat, c), *n)));
            let on = match lib.category.as_deref() {
                None => Some(0),
                Some(c) => cats.iter().position(|x| x.0 == c).map(|p| p + 1),
            };
            let title = cat.t("complib.categories");
            if let Some(r) = pick_list(ui, th, "c.lib.cats", &title, &rows, on) {
                lib.category = r.checked_sub(1).map(|r| cats[r].0.to_string());
            }
        });
}

/// Centre pane: the cards of the filtered entries.
fn grid(ui: &mut egui::Ui, th: &Theme, cat: &Catalog, entries: &[Entry], lib: &mut Library) {
    egui::ScrollArea::vertical()
        .id_salt("c.lib.list")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let idx = filtered(entries, lib);
            if idx.is_empty() {
                let none_here = !entries.iter().any(|e| e.source == lib.source);
                if lib.source == Source::User && none_here {
                    empty_user(ui, th, cat, lib);
                } else {
                    empty_list(ui, th, cat, lib);
                }
                return;
            }
            let mut budget = THUMBS_PER_FRAME;
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(space::SM, space::SM);
                for i in idx {
                    let e = &entries[i];
                    let on = lib.pick.as_deref() == Some(e.fam.name.as_str());
                    let r = card(ui, th, cat, e, on, &mut budget);
                    if r.clicked() || r.drag_started() {
                        if !on {
                            lib.ty = None;
                        }
                        lib.pick = Some(e.fam.name.clone());
                    }
                    if r.double_clicked() {
                        lib.place = pick_of(e, None);
                    }
                }
            });
        });
}

/// Name of a family in the UI language: built-in families have a catalog entry (`fam.name.*`,
/// else `complib.fam.<name in snake case>`); project and user families show their own name.
pub fn family_label(cat: &Catalog, name: &str) -> String {
    let own = crate::fam_label::family(cat.lang, name);
    if own != name {
        return own;
    }
    let slug: String = name
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    let key = format!("complib.fam.{slug}");
    let text = cat.t(&key);
    if text == key { name.to_string() } else { text }
}

/// Accessible name of a card: the name and the number of types.
pub fn card_label(cat: &Catalog, f: &Family) -> String {
    cat.tf(
        "complib.card",
        &[("name", &family_label(cat, &f.name)), ("n", &f.types.len().to_string())],
    )
}

/// Tooltip of a card: the full name (the card elides it), category and number of types.
pub fn card_tip(cat: &Catalog, f: &Family) -> String {
    cat.tf(
        "complib.card_tip",
        &[
            ("name", &family_label(cat, &f.name)),
            ("category", &category_label(cat, f.category.name())),
            ("n", &f.types.len().to_string()),
        ],
    )
}

/// One card: thumbnail, elided name, category; click picks, drag carries a `Pick` onto a view.
fn card(
    ui: &mut egui::Ui,
    th: &Theme,
    cat: &Catalog,
    e: &Entry,
    on: bool,
    budget: &mut usize,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(CARD_W, CARD_H), egui::Sense::click_and_drag());
    let label = card_label(cat, &e.fam);
    note_card(ui.ctx(), &label, rect);
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, &label));
    let s = th.sem();
    let (fill, line) = if on {
        (s.selection_bg, s.accent)
    } else if resp.hovered() {
        (ddc_ui::theme::over_rgba(s.bg_hover, s.bg_panel), s.border_strong)
    } else {
        (s.bg_panel, s.border_subtle)
    };
    let p = ui.painter();
    p.rect(rect, th.radius("lg"), fill, egui::Stroke::new(stroke::HAIRLINE, line), egui::StrokeKind::Inside);
    let side = THUMB as f32;
    let img = egui::Rect::from_center_size(
        egui::pos2(rect.center().x, rect.top() + space::SM + side / 2.0),
        egui::vec2(side, side),
    );
    paint_thumb(ui, th, (e.hash, THUMB), img, budget, || type_rgba(&e.fam, None, THUMB));
    let text = egui::Rect::from_min_max(
        egui::pos2(rect.left() + space::SM, img.bottom() + space::SM),
        egui::pos2(rect.right() - space::SM, rect.bottom() - space::SM),
    );
    // A child ui, not a scope: a scope would advance the wrapped row's cursor and stagger the cards.
    let mut tui = ui.new_child(egui::UiBuilder::new().max_rect(text));
    tui.spacing_mut().item_spacing.y = space::XXS;
    widgets::elided_label(&mut tui, &family_label(cat, &e.fam.name), th.font(Role::Small), s.text_primary);
    kit::elided_weak(&mut tui, &category_label(cat, e.fam.category.name()));
    if let Some(pk) = pick_of(e, None) {
        resp.dnd_set_drag_payload(pk);
    }
    if resp.dragged() {
        ghost(ui.ctx(), th, &family_label(cat, &e.fam.name));
    }
    resp.on_hover_text(card_tip(cat, &e.fam))
}

/// Label under the pointer while a card is dragged, on the menu layer above every view.
fn ghost(ctx: &egui::Context, th: &Theme, text: &str) {
    let Some(at) = ctx.pointer_hover_pos() else { return };
    let layer = egui::LayerId::new(ddc_ui::z::Z::Menu.order(), egui::Id::new("c.lib.ghost"));
    let p = ctx.layer_painter(layer);
    let s = th.sem();
    let g = p.layout_no_wrap(text.to_owned(), th.font(Role::Small), s.text_primary);
    let pad = egui::vec2(space::SM, space::SM);
    let r = egui::Rect::from_min_size(at + egui::vec2(space::MD, space::MD), g.size() + pad * 2.0);
    p.rect(r, th.radius("lg"), s.bg_elevated, egui::Stroke::new(stroke::HAIRLINE, s.border_strong), egui::StrokeKind::Inside);
    p.galley(r.min + pad, g, s.text_primary);
}

/// RGBA thumbnail of type `ty` (else the first) of `f`; None when it builds no body.
pub fn type_rgba(f: &Family, ty: Option<&str>, size: u32) -> Option<Vec<u8>> {
    let tv = f
        .types
        .iter()
        .find(|t| Some(t.name.as_str()) == ty)
        .or(f.types.first())
        .map(|t| t.values.clone())
        .unwrap_or_default();
    let r = f.resolve(&tv, &[]).ok()?;
    let b = f.build(&r, Lod::Medium).ok()?;
    (!b.mesh.triangles.is_empty()).then(|| thumb::render(&b.mesh, size, [176, 160, 140]))
}

type Thumbs = HashMap<(u64, u32), Option<egui::TextureHandle>>;

/// Cached texture of `key` (family hash, size); a new one is rendered while `budget` lasts,
/// the rest on the next frames. Nothing to draw: a sunken square.
fn paint_thumb(
    ui: &egui::Ui,
    th: &Theme,
    key: (u64, u32),
    rect: egui::Rect,
    budget: &mut usize,
    make: impl FnOnce() -> Option<Vec<u8>>,
) {
    let ctx = ui.ctx();
    let id = egui::Id::new("c.lib.thumbs");
    let mut tex = ctx.data_mut(|d| d.get_temp_mut_or_default::<Thumbs>(id).get(&key).cloned());
    if tex.is_none() && *budget > 0 {
        *budget -= 1;
        let n = thumb::clamp_size(key.1) as usize;
        let t = make().map(|px| {
            let img = egui::ColorImage::from_rgba_unmultiplied([n, n], &px);
            ctx.load_texture(format!("c.lib.thumb.{:x}.{}", key.0, key.1), img, egui::TextureOptions::LINEAR)
        });
        ctx.data_mut(|d| d.get_temp_mut_or_default::<Thumbs>(id).insert(key, t.clone()));
        tex = Some(t);
    } else if tex.is_none() {
        ctx.request_repaint();
    }
    match tex.flatten() {
        Some(t) => {
            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            ui.painter().image(t.id(), rect, uv, egui::Color32::WHITE); // design-lint: data (identity image tint)
        }
        None => {
            ui.painter().rect_filled(rect, th.radius("lg"), th.sem().bg_sunken);
        }
    }
}

/// The user library is empty: where to put families, and a button to open that folder.
fn empty_user(ui: &mut egui::Ui, th: &Theme, cat: &Catalog, lib: &mut Library) {
    let hit = widgets::surface::empty_state(
        ui,
        th,
        None,
        &cat.t("complib.user_empty"),
        &cat.t("complib.user_empty_hint"),
        Some(&cat.t("complib.open_folder")),
    );
    if hit {
        lib.open_dir = true;
    }
}

/// Nothing to list: with no query offer to load a family, else to reset the search.
fn empty_list(ui: &mut egui::Ui, th: &Theme, cat: &Catalog, lib: &mut Library) {
    let act = empty_action(&lib.query);
    let key = match act {
        EmptyAction::LoadFamily => "empty.library.load",
        EmptyAction::ResetSearch => "empty.library.action",
    };
    let hit = widgets::surface::empty_state(
        ui,
        th,
        None,
        &cat.t("empty.library.title"),
        &cat.t("empty.library.hint"),
        Some(&cat.t(key)),
    );
    if hit {
        match act {
            EmptyAction::LoadFamily => lib.cmd = Some("file.import"),
            EmptyAction::ResetSearch => lib.query.clear(),
        }
    }
}

/// Format readiness as a badge: text and tone.
fn ready_badge(cat: &Catalog, r: Ready) -> (String, Tone) {
    match r {
        Ready::Full => (cat.t("complib.ready.full"), Tone::Success),
        Ready::Body => (cat.t("complib.ready.body"), Tone::Accent),
        Ready::No => (cat.t("complib.ready.no"), Tone::Danger),
    }
}

/// Value of parameter `name` in type `ty`, else its default.
fn value_of<'a>(f: &'a Family, ty: Option<&str>, name: &str, default: &'a Value) -> &'a Value {
    f.types
        .iter()
        .find(|t| Some(t.name.as_str()) == ty)
        .or(f.types.first())
        .and_then(|t| t.values.iter().find(|v| v.0 == name).map(|v| &v.1))
        .unwrap_or(default)
}

/// Right pane: thumbnail of the picked type, name, meta, state and format badges, types,
/// parameters with their values.
fn details(ui: &mut egui::Ui, th: &Theme, cat: &Catalog, picked: Option<&Entry>, lib: &mut Library) {
    let Some(e) = picked else {
        kit::empty_note(ui, cat.t("type_dialog.library.pick_a_family"), None);
        return;
    };
    let f = &e.fam;
    let ru = cat.lang == ddc_ui::Lang::Ru;
    egui::ScrollArea::vertical()
        .id_salt("c.lib.details")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let s = th.sem();
            let ty = lib.ty.clone().or_else(|| f.types.first().map(|t| t.name.clone()));
            let side = BIG_THUMB as f32;
            let (img, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), side), egui::Sense::hover());
            let img = egui::Rect::from_center_size(img.center(), egui::vec2(side, side));
            let key = (e.type_hash(ty.as_deref().unwrap_or("")), BIG_THUMB);
            let mut budget = 1;
            paint_thumb(ui, th, key, img, &mut budget, || type_rgba(f, ty.as_deref(), BIG_THUMB));
            ui.add_space(space::SM);
            widgets::elided_label(ui, &family_label(cat, &f.name), th.font(Role::Title), s.text_primary);
            let meta = format!(
                "{} · {} · {}",
                category_label(cat, f.category.name()),
                host_label(cat, f.hosting),
                ifc_label(cat, &f.ifc.0)
            );
            kit::elided_weak(ui, &meta);
            ui.add_space(space::SM);
            ui.horizontal_wrapped(|ui| {
                widgets::badge(ui, th, &cat.t(e.source.key()), Tone::Neutral);
                if e.in_project && e.source != Source::Project {
                    widgets::badge(ui, th, &cat.t("library.in_project"), Tone::Accent);
                }
                if e.edited {
                    widgets::badge(ui, th, &cat.t("complib.edited"), Tone::Warning)
                        .on_hover_text(cat.t("complib.edited_tip"));
                }
            });
            ui.horizontal_wrapped(|ui| {
                for (fmt, r) in e.formats.list() {
                    let (word, tone) = ready_badge(cat, r);
                    widgets::badge(ui, th, fmt, tone)
                        .on_hover_text(cat.tf("complib.format_tip", &[("fmt", fmt), ("state", &word)]));
                }
            });
            ui.add_space(space::SM);
            let types: Vec<(String, usize)> = f
                .types
                .iter()
                .map(|t| (crate::fam_label::type_name(cat.lang, &t.name), t.values.len()))
                .collect();
            let on = f.types.iter().position(|t| Some(&t.name) == ty.as_ref());
            if let Some(r) = pick_list(ui, th, "c.lib.types", &cat.t("type_dialog.library.types"), &types, on) {
                lib.ty = Some(f.types[r].name.clone());
            }
            ui.add_space(space::SM);
            let mark = cat.t("library.formula_mark");
            let tip = cat.t("library.formula_tip");
            let (c_name, c_val) = (cat.t("library.col_name"), cat.t("complib.value"));
            let cols = [
                Column::text(&c_name).sortable(false),
                Column::text(&c_val).fixed(KIND_W).sortable(false),
                Column::text(&mark).fixed(MARK_W).sortable(false),
            ];
            let mut sort = None;
            Table::new("c.lib.params", &cols, f.params.len()).show(
                ui,
                th,
                &mut sort,
                |_| RowState::Normal,
                |r, c| {
                    let p = &f.params[r];
                    match c {
                        0 => Cell::text(p.name.clone()),
                        1 => Cell::muted(value_text(p.kind, value_of(f, ty.as_deref(), &p.name, &p.default)))
                            .with_tip(kind_label_l(ru, p.kind)),
                        _ if p.formula.is_some() => Cell::muted(mark.clone()).with_tip(tip.clone()),
                        _ => Cell::text(""),
                    }
                },
            );
        });
}

/// States of the window drawn by `layout_probe`.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Case {
    /// Nothing picked.
    Fresh,
    /// A built-in family picked.
    Picked,
    /// A picked family with a very long name, already in the project.
    Long,
    /// A query that matches nothing.
    NoMatch,
}

/// Length of the long family name of `Case::Long`.
#[cfg(test)]
pub(crate) const LONG_NAME: usize = 90;

/// What one headless frame of the window drew.
#[cfg(test)]
pub(crate) struct Shot {
    pub probe: crate::layout_contract::Probe,
    /// List column, details column, footer row.
    pub rects: Option<[egui::Rect; 3]>,
    pub long_name: String,
}

/// The window drawn headless in an explicit language, for the layout contract and its tests.
#[cfg(test)]
pub(crate) fn layout_probe(key: &str, ru: bool, case: Case) -> Shot {
    use ddc_ui::{Density, Lang, Mode};
    let cat = Catalog::new(crate::flag_lang(ru));
    let m = ddc_model::Model::open(":memory:").expect("model");
    let (mut entries, _) = crate::complib::gather(&m, None);
    let long_name: String = "Very long family name "
        .repeat(6)
        .chars()
        .take(LONG_NAME)
        .collect();
    let mut lib = Library::default();
    match case {
        Case::Fresh => {}
        Case::Picked => lib.pick = Some(entries[1].fam.name.clone()),
        Case::Long => {
            entries[0].fam.name = long_name.clone();
            entries[0].in_project = true;
            lib.pick = Some(long_name.clone());
        }
        Case::NoMatch => lib.query = "zzzz".into(),
    }
    let mut th = Theme::new(Mode::Dark, Density::Compact);
    let rects = std::cell::Cell::new(None);
    let mut edit = None;
    let probe = crate::layout_contract::probe(key, |ctx| {
        let _ = th.apply(ctx);
        show(ctx, &th, &cat, &entries, &mut lib, &mut edit);
        rects.set(ctx.data(|d| d.get_temp::<[egui::Rect; 3]>(egui::Id::new("ux63a.rects"))));
    });
    Shot { probe, rects: rects.get(), long_name }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ui::Lang;
    use egui::accesskit::Role as Ak;

    const CASES: [Case; 4] = [Case::Fresh, Case::Picked, Case::Long, Case::NoMatch];

    fn cyr(s: &str) -> bool {
        s.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c))
    }

    fn catalog(ru: bool) -> Catalog {
        Catalog::new(crate::flag_lang(ru))
    }

    fn button(p: &crate::layout_contract::Probe, label: &str) -> egui::Rect {
        p.controls
            .iter()
            .find(|c| c.role == Ak::Button && c.label == label)
            .unwrap_or_else(|| panic!("no button {label:?} in {:?}", p.controls))
            .rect
    }

    #[test]
    fn production_code_uses_kit_widgets_only() {
        let src = include_str!("library_win.rs");
        let code = &src[..src.find("/// States of the window").unwrap()];
        for bad in [
            "egui::TextEdit",
            "egui::Grid",
            "selectable_label",
            ".button(",
            ".size([",
            "tr_str",
            "ru: bool",
            "ui_is_ru",
            "ui_lang_now",
        ] {
            assert!(!code.contains(bad), "library_win.rs still has {bad}");
        }
        for (i, l) in code.lines().enumerate() {
            let t = l.trim_start();
            if t.starts_with("//") {
                continue;
            }
            if let Some(at) = l.find("add_space(") {
                let first = l[at + "add_space(".len()..].chars().next().unwrap_or('x');
                assert!(!first.is_ascii_digit(), "line {}: literal spacing", i + 1);
            }
        }
    }

    #[test]
    fn every_case_keeps_the_layout_contract_in_both_languages() {
        for ru in [false, true] {
            for case in CASES {
                let shot = layout_probe("component_library", ru, case);
                let tag = format!("library {case:?} ru={ru}");
                crate::layout_contract::assert_contract(&tag, &shot.probe, false);
                let any = shot.probe.controls.iter().any(|c| cyr(&c.label));
                assert_eq!(any, ru, "{tag}: control labels follow the language");
            }
        }
    }

    #[test]
    fn long_name_is_cut_inside_the_list_column() {
        for ru in [false, true] {
            let shot = layout_probe("component_library", ru, Case::Long);
            assert_eq!(shot.long_name.chars().count(), LONG_NAME);
            let [list, details, _] = shot.rects.expect("frame noted its rects");
            assert!(list.right() < details.left(), "ru={ru}: {list:?} vs {details:?}");
            assert!(
                shot.probe.elided.iter().any(|t| t.trim() == shot.long_name),
                "ru={ru}: the long name is elided: {:?}",
                shot.probe.elided
            );
        }
    }

    #[test]
    fn footer_sits_at_the_dialog_bottom_and_does_not_move_with_the_pick() {
        for ru in [false, true] {
            let cat = catalog(ru);
            let (load, params) = (
                cat.t("type_dialog.library.load_into_project"),
                cat.t("type_dialog.library.parameters"),
            );
            let mut loads = vec![];
            for case in [Case::Fresh, Case::Picked, Case::Long] {
                let shot = layout_probe("component_library", ru, case);
                let inner = shot.probe.inner.expect("window drawn");
                let [_, details, foot] = shot.rects.expect("rects");
                assert!(
                    (inner.bottom() - foot.bottom()).abs() < space::SM,
                    "ru={ru} {case:?}: footer {foot:?} window {inner:?}"
                );
                assert!(foot.top() > details.top(), "ru={ru} {case:?}: footer under the columns");
                let (l, q) = (button(&shot.probe, &load), button(&shot.probe, &params));
                assert!(l.right() > q.right(), "ru={ru} {case:?}: primary is the far right one");
                assert!((l.center().y - q.center().y).abs() < 1.0, "ru={ru} {case:?}: one row");
                loads.push(l);
            }
            for l in &loads[1..] {
                assert!((l.left() - loads[0].left()).abs() < 0.5, "ru={ru}: Load moved {loads:?}");
                assert!((l.bottom() - loads[0].bottom()).abs() < 0.5, "ru={ru}: Load moved {loads:?}");
            }
        }
    }

    #[test]
    fn empty_search_offers_to_reset_in_the_language_of_the_window() {
        for ru in [false, true] {
            let shot = layout_probe("component_library", ru, Case::NoMatch);
            let want = catalog(ru).t("empty.library.action");
            button(&shot.probe, &want);
        }
    }

    #[test]
    fn labels_follow_the_catalog_language() {
        let (en, ru) = (catalog(false), catalog(true));
        for h in [Hosting::None, Hosting::Wall, Hosting::Floor, Hosting::Ceiling, Hosting::Face] {
            let (a, b) = (host_label(&en, h), host_label(&ru, h));
            assert!(!cyr(&a) && cyr(&b), "{h:?}: {a:?} {b:?}");
        }
        for c in ddc_geom::component::Category::ALL {
            let (a, b) = (category_label(&en, c.name()), category_label(&ru, c.name()));
            assert!(a != c.name() && !cyr(&a) && cyr(&b), "{c:?}: {a:?} {b:?}");
        }
        for f in ddc_model::library().unwrap() {
            let (a, b) = (ifc_label(&en, &f.ifc.0), ifc_label(&ru, &f.ifc.0));
            assert!(a != f.ifc.0 && !cyr(&a) && cyr(&b), "{}: {a:?} {b:?}", f.ifc.0);
        }
        assert_eq!(ifc_label(&ru, "IfcUnknownThing"), "IfcUnknownThing");
    }

    #[test]
    fn new_strings_exist_in_both_languages() {
        let (en, ru) = (catalog(false), catalog(true));
        let keys = |c: &Catalog, l: Lang| -> Vec<String> {
            c.table(l).keys().filter(|k| k.starts_with("library.")).cloned().collect()
        };
        let (ke, kr) = (keys(&en, Lang::En), keys(&ru, Lang::Ru));
        assert_eq!(ke, kr);
        assert!(ke.len() >= 20, "{ke:?}");
        for k in ke {
            let (a, b) = (en.t(&k), ru.t(&k));
            assert!(!a.is_empty() && !b.is_empty() && a != k && b != k, "{k}");
            if k != "library.formula_mark" {
                assert!(!cyr(&a) && cyr(&b), "{k}: {a:?} {b:?}");
            }
        }
    }

    fn entries() -> Vec<Entry> {
        let m = ddc_model::Model::open(":memory:").unwrap();
        crate::complib::gather(&m, None).0
    }

    fn esc_frame(ctx: &egui::Context, cat: &Catalog, fams: &[Entry], lib: &mut Library, esc: bool) -> bool {
        let events = if esc {
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }]
        } else {
            vec![]
        };
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 1000.0))),
            events,
            ..Default::default()
        };
        let mut th = Theme::new(ddc_ui::Mode::Dark, ddc_ui::Density::Compact);
        let mut keep = true;
        let mut edit = None;
        let _ = ctx.run(raw, |ctx| {
            let _ = th.apply(ctx);
            keep = show(ctx, &th, cat, fams, lib, &mut edit);
        });
        keep
    }

    #[test]
    fn escape_clears_the_search_and_keeps_the_window() {
        for ru in [false, true] {
            let cat = catalog(ru);
            let fams = entries();
            let ctx = egui::Context::default();
            ddc_ui::theme::install_fonts(&ctx);
            let _ = ctx.run(Default::default(), |_| {});
            let mut lib = Library { query: "tab".into(), ..Default::default() };
            // the window's first frames only measure it: focus is asked once it is on screen
            for _ in 0..2 {
                assert!(esc_frame(&ctx, &cat, &fams, &mut lib, false));
            }
            ctx.memory_mut(|m| m.request_focus(search_id()));
            for _ in 0..3 {
                assert!(esc_frame(&ctx, &cat, &fams, &mut lib, false));
            }
            assert!(ctx.memory(|m| m.has_focus(search_id())), "ru={ru}: focused");
            assert!(esc_frame(&ctx, &cat, &fams, &mut lib, true), "ru={ru}: window stays");
            assert!(lib.query.is_empty(), "ru={ru}: Esc clears");
            assert!(ctx.memory(|m| m.has_focus(search_id())), "ru={ru}: focus kept");
        }
    }

    #[test]
    fn empty_action_per_query_state() {
        assert_eq!(empty_action(""), EmptyAction::LoadFamily);
        assert_eq!(empty_action("  "), EmptyAction::LoadFamily);
        assert_eq!(empty_action("door"), EmptyAction::ResetSearch);
    }

    #[test]
    fn search_groups_by_category() {
        let fams = ddc_model::library().unwrap();
        let all = grouped(&fams, "");
        assert_eq!(all.iter().map(|g| g.1.len()).sum::<usize>(), fams.len());
        let t = grouped(&fams, "tab");
        assert!(t
            .iter()
            .flat_map(|g| &g.1)
            .any(|&i| fams[i].name == "Table"));
        assert!(grouped(&fams, "zzzz").is_empty());
        let f = grouped(&fams, "furniture");
        assert!(!f.is_empty() && f.iter().all(|g| g.0 == "furniture"));
    }

    #[test]
    fn filter_keeps_the_source_and_finds_by_class_type_and_parameter() {
        let es = entries();
        let mut lib = Library::default();
        let all = filtered(&es, &lib);
        assert!(!all.is_empty() && all.iter().all(|&i| es[i].source == Source::Builtin));
        lib.source = Source::Project;
        assert!(filtered(&es, &lib).is_empty(), "an empty project lists nothing");
        lib.source = Source::Builtin;
        lib.query = "ifclightfixture".into();
        let lights = filtered(&es, &lib);
        assert!(!lights.is_empty() && lights.iter().all(|&i| es[i].fam.ifc.0 == "IfcLightFixture"));
        let table = es.iter().find(|e| e.fam.name == "Table").unwrap();
        let p = &table.fam.params[0].name;
        lib.query = p.to_uppercase();
        assert!(filtered(&es, &lib).iter().any(|&i| es[i].fam.name == "Table"), "by parameter {p}");
        lib.query.clear();
        lib.category = Some("furniture".into());
        let f = filtered(&es, &lib);
        assert!(!f.is_empty() && f.iter().all(|&i| es[i].fam.category.name() == "furniture"));
    }

    #[test]
    fn pick_carries_the_type_and_the_definition_of_a_family_not_in_the_project() {
        let es = entries();
        let e = es.iter().find(|e| e.fam.types.len() > 1).expect("a family with two types");
        let second = e.fam.types[1].name.clone();
        let p = pick_of(e, Some(&second)).unwrap();
        assert_eq!((p.family.as_str(), p.ty.as_str()), (e.fam.name.as_str(), second.as_str()));
        assert_eq!(p.def.as_deref(), Some(&*e.def));
        assert_eq!(pick_of(e, Some("no such")).unwrap().ty, e.fam.types[0].name);
        let mut m = ddc_model::Model::open(":memory:").unwrap();
        m.load_family(&e.def).unwrap();
        let (es2, _) = crate::complib::gather(&m, None);
        let own = es2
            .iter()
            .find(|x| x.source == Source::Project && x.fam.name == e.fam.name)
            .unwrap();
        assert!(own.in_project && !own.edited);
        assert_eq!(pick_of(own, None).unwrap().def, None, "the project has it already");
    }

    #[test]
    fn cards_are_named_with_their_type_count_and_thumbnails_render() {
        for ru in [false, true] {
            let shot = layout_probe("component_library", ru, Case::Fresh);
            let cat = catalog(ru);
            let es = entries();
            let want = card_label(&cat, &es[filtered(&es, &Library::default())[0]].fam);
            assert!(shot.probe.controls.iter().any(|c| c.label == want), "ru={ru}: card {want:?}");
        }
        let es = entries();
        let lit = es.iter().filter(|e| type_rgba(&e.fam, None, THUMB).is_some()).count();
        assert_eq!(lit, es.len(), "every built-in family draws a thumbnail");
        let px = type_rgba(&es[0].fam, None, THUMB).unwrap();
        assert_eq!(px.len(), (THUMB * THUMB * 4) as usize);
        assert!(px.chunks(4).any(|p| p[3] > 0), "not blank");
    }

    #[test]
    fn builtin_family_names_are_english_in_en_and_russian_in_ru() {
        let (en, ru) = (catalog(false), catalog(true));
        let es = entries();
        let builtin: Vec<&Entry> = es.iter().filter(|e| e.source == Source::Builtin).collect();
        assert!(builtin.len() >= 40);
        for e in builtin {
            let n = &e.fam.name;
            assert_eq!(family_label(&en, n), *n, "en shows the family name as is");
            let r = family_label(&ru, n);
            assert!(cyr(&r), "{n}: no Russian name, got {r:?}");
        }
        assert_eq!(family_label(&ru, "My Custom Crate"), "My Custom Crate", "user families keep their name");
    }

    #[test]
    fn every_card_has_the_same_size_and_a_tooltip_text() {
        let es = entries();
        let shot = layout_probe("component_library", false, Case::Fresh);
        let cat = catalog(false);
        let sizes: Vec<egui::Vec2> = es
            .iter()
            .filter_map(|e| shot.probe.controls.iter().find(|c| c.label == card_label(&cat, &e.fam)))
            .map(|c| c.rect.size())
            .collect();
        assert!(sizes.len() >= 3, "{} cards on screen", sizes.len());
        assert!(sizes.iter().all(|s| *s == egui::vec2(CARD_W, CARD_H)), "{sizes:?}");
        for ru in [false, true] {
            let c = catalog(ru);
            for e in &es {
                let tip = card_tip(&c, &e.fam);
                assert!(tip.contains(&family_label(&c, &e.fam.name)) && !tip.contains("{"), "{tip}");
            }
        }
    }

    #[test]
    fn double_click_on_a_card_asks_to_place_its_first_type() {
        let cat = catalog(false);
        let es = entries();
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        let mut lib = Library::default();
        for _ in 0..3 {
            esc_frame(&ctx, &cat, &es, &mut lib, false);
        }
        let first = &es[filtered(&es, &lib)[0]];
        let label = card_label(&cat, &first.fam);
        let at = ctx
            .data(|d| d.get_temp::<egui::Rect>(egui::Id::new("c.lib.card").with(&label)))
            .expect("card drawn")
            .center();
        let ev = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        let mut th = Theme::new(ddc_ui::Mode::Dark, ddc_ui::Density::Compact);
        let mut edit = None;
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 1000.0));
        for events in [
            vec![egui::Event::PointerMoved(at)],
            vec![ev(true), ev(false)],
            vec![ev(true), ev(false)],
            vec![],
        ] {
            let raw = egui::RawInput { screen_rect: Some(screen), events, ..Default::default() };
            let _ = ctx.run(raw, |ctx| {
                let _ = th.apply(ctx);
                show(ctx, &th, &cat, &es, &mut lib, &mut edit);
            });
        }
        assert_eq!(lib.pick.as_deref(), Some(first.fam.name.as_str()));
        assert_eq!(lib.place, pick_of(first, None));
    }
}
