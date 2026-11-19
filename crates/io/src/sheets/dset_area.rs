// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The area and finishes schedules of a drawing set, built from the model: the area schedule
//! counts rooms by the measurement standard of the project's norm pack (`area_std`), the
//! finishes schedule lists the finishes of every room (`finishes`). Both name the pack and the
//! standard in their title, so a printed sheet says by what rule its numbers were made.

use std::sync::OnceLock;

use ddc_model::checks::norm_pack::{NormPack, PackSet};
use ddc_model::{AreaMethod, AreaReport, FinishSource, Model};

use super::table::Table;
use super::SheetError;

/// Keys of the two schedules built here.
pub const AREAS: &str = "areas";
pub const FINISHES: &str = "finishes";

type Text<'a> = &'a dyn Fn(&str) -> String;

fn me(e: ddc_model::Error) -> SheetError {
    SheetError(e.to_string())
}

fn packs() -> &'static PackSet {
    static SET: OnceLock<PackSet> = OnceLock::new();
    SET.get_or_init(PackSet::embedded)
}

/// The pack of the project: the stored choice, else the catalog's default for the language.
pub fn project_pack(m: &Model, text: Text) -> Result<NormPack, SheetError> {
    let set = packs();
    let stored = m.norm_locale().map_err(me)?.pack;
    let default = text("dset.area.default_pack");
    let id = if set.has(&stored) {
        stored
    } else if set.has(&default) {
        default
    } else {
        "generic".to_string()
    };
    set.resolve(&id).map_err(me)
}

fn num(v: f64, sep: &str) -> String {
    format!("{v:.2}").replace('.', sep)
}

fn area_title(r: &AreaReport, text: Text) -> String {
    let std = text(&format!("dset.area.std.{}", r.method.key()));
    format!("{} ({}, {})", text("dset.area.title"), std, r.pack)
}

/// The area schedule table of a report.
pub fn area_table(r: &AreaReport, text: Text) -> Table {
    let sep = text("dset.area.decimal");
    let unit = text("sheets.paper.area_unit");
    let mut header: Vec<String> = ["storey", "use", "rooms", "net"]
        .iter()
        .map(|k| text(&format!("dset.area.col.{k}")))
        .collect();
    header.push(format!("{}, {unit}", text(&format!("dset.area.std.{}", r.method.key()))));
    let mut rows = vec![];
    let mut rooms = 0;
    for s in &r.storeys {
        for u in &s.uses {
            rows.push(vec![
                s.name.clone(),
                text(&format!("dset.area.use.{}", u.class)),
                u.rooms.to_string(),
                num(u.net, &sep),
                num(u.counted, &sep),
            ]);
        }
        let net: f64 = s.uses.iter().map(|u| u.net).sum();
        let counted: f64 = s.uses.iter().map(|u| u.counted).sum();
        rows.push(vec![
            s.name.clone(),
            text("dset.area.total_storey"),
            s.rooms.to_string(),
            num(net, &sep),
            num(counted, &sep),
        ]);
        let geo = match r.method {
            AreaMethod::Gross => s.gross.map(|v| ("dset.area.gross", v)),
            AreaMethod::GrossInternal => s.gross_internal.map(|v| ("dset.area.gia", v)),
            _ => None,
        };
        if let Some((k, v)) = geo {
            rows.push(vec![s.name.clone(), text(k), String::new(), String::new(), num(v, &sep)]);
        }
        rooms += s.rooms;
    }
    let net: f64 = r.storeys.iter().flat_map(|s| &s.uses).map(|u| u.net).sum();
    rows.push(vec![
        text("dset.area.total"),
        String::new(),
        rooms.to_string(),
        num(net, &sep),
        num(r.total, &sep),
    ]);
    if r.fallback {
        rows.push(vec![text("dset.area.fallback"), String::new(), String::new(), String::new(), String::new()]);
    }
    Table { title: area_title(r, text), header, rows, swatches: vec![] }
}

/// The area schedule of the model; no rooms: an empty table.
pub fn areas(m: &Model, text: Text) -> Result<Table, SheetError> {
    let pack = project_pack(m, text)?;
    Ok(area_table(&m.area_report(&pack).map_err(me)?, text))
}

fn finish_name(id: &str, text: Text) -> String {
    let key = format!("dset.finish.{id}");
    let t = text(&key);
    if t == key { id.to_string() } else { t }
}

/// The floor finish of every room tag of `d` in the sheet's language.
pub(super) fn localize_finish(d: &mut ddc_model::PlanView, text: Text) {
    for p in &mut d.prims {
        match p {
            ddc_model::ViewPrim::RoomTag { finish: Some(f), .. } => *f = finish_name(f, text),
            ddc_model::ViewPrim::Tag { tag } => {
                if let Some(f) = &mut tag.text.finish {
                    *f = finish_name(f, text);
                }
            }
            _ => {}
        }
    }
}

/// The finishes schedule of the model; no rooms: an empty table.
pub fn finishes(m: &Model, text: Text) -> Result<Table, SheetError> {
    let pack = project_pack(m, text)?;
    let sep = text("dset.area.decimal");
    let header: Vec<String> = [
        "storey", "number", "name", "floor", "walls", "ceiling", "floor_area", "wall_area", "basis",
    ]
    .iter()
    .map(|k| text(&format!("dset.finish.col.{k}")))
    .collect();
    let rows = m
        .room_finishes(&pack)
        .map_err(me)?
        .into_iter()
        .map(|f| {
            let basis = match f.source {
                FinishSource::Param => "param",
                FinishSource::Pack => "pack",
                FinishSource::Rule => "rule",
            };
            vec![
                f.level_name,
                f.number,
                f.name,
                finish_name(&f.floor, text),
                finish_name(&f.walls, text),
                finish_name(&f.ceiling, text),
                num(f.floor_area, &sep),
                num(f.wall_area, &sep),
                text(&format!("dset.finish.basis.{basis}")),
            ]
        })
        .collect();
    Ok(Table { title: text("dset.finish.title"), header, rows, swatches: vec![] })
}
