//! Rebar sets and analytical members of the element contract (E6): plan outlines for picking,
//! the properties panel (layout, bar type, measured quantities; axis offsets and releases), and
//! the delete items. Both families are hosted: they never move on their own, their reasons say so.

use super::{Family, Rings};
use crate::geom_props::{Effect, Row, Unit};
use crate::editor::{Editor, OpFn, Sel};
use crate::scene_ids::{EXT_ANALYTICAL, EXT_REBAR};
use crate::units::{display_length, fmt_num, Units};
use crate::{tr, ui_kit};
use ddc_model::{
    AnRelease, AnalyticalAdj, Error, Model, RebarLayout, RebarPath, RebarSetSpec,
};
use std::collections::BTreeSet;
use std::sync::Arc;

type Outlines = Vec<(i64, Vec<Vec<ddc_geom::V2>>)>;

fn to_rings(v: Outlines) -> Vec<(i64, Rings)> {
    v.into_iter()
        .map(|(id, rings)| {
            let rings = rings
                .into_iter()
                .map(|r| r.into_iter().map(|p| (p.x, p.y)).collect())
                .collect();
            (id, rings)
        })
        .collect()
}

/// Plan outlines of the rebar sets drawn on a level.
pub fn set_footprints(m: &Model, level: i64) -> Result<Vec<(i64, Rings)>, Error> {
    Ok(to_rings(crate::rebar_scene::pick_sets(m, level)))
}

/// Plan outlines of the analytical members of a level, keyed by the member id of the 3D scene.
pub fn member_footprints(m: &Model, level: i64) -> Result<Vec<(i64, Rings)>, Error> {
    Ok(to_rings(crate::rebar_scene::pick_members(m, level)))
}

/// The host kind and id behind an analytical selection id; None for other families.
pub fn axis_host(f: &Family, id: i64) -> Option<(&'static str, i64)> {
    (f.table == "analytical_adj")
        .then(|| crate::rebar_scene::an_split(id))
        .flatten()
}

/// Selection id of the axis an adjustment row belongs to: the row id of `analytical_adj` is not
/// the id the 3D scene and the selection carry for that axis.
pub fn axis_key(m: &Model, row: i64) -> Option<i64> {
    m.sql()
        .query_row(
            "SELECT kind, ref FROM analytical_adj WHERE id=?1",
            [row],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )
        .ok()
        .map(|(kind, host)| crate::rebar_scene::an_id(&kind, host))
}

/// Row id of the adjustment of a host, if it has one.
fn adjustment_row(m: &Model, kind: &str, host: i64) -> Option<i64> {
    m.sql()
        .query_row(
            "SELECT id FROM analytical_adj WHERE kind=?1 AND ref=?2",
            rusqlite::params![kind, host],
            |r| r.get(0),
        )
        .ok()
}

/// Items of a delete over the selection, hosted rows first so a host deleted in the same action
/// cannot take them away before their turn. A selected axis is deleted by dropping its
/// adjustment, which puts it back on its host; an axis with none has nothing to delete.
pub fn delete_items(ed: &Editor, sel: &[Sel]) -> Result<Vec<(String, i64)>, String> {
    let (mut hosted, mut rest, mut skipped) = (vec![], vec![], 0usize);
    for s in sel {
        match *s {
            Sel::Ext(EXT_REBAR, id) => hosted.push(("rebar_set".to_string(), id)),
            Sel::Ext(EXT_ANALYTICAL, id) => {
                let row = crate::rebar_scene::an_split(id)
                    .and_then(|(kind, host)| adjustment_row(&ed.model, kind, host));
                match row {
                    Some(r) => hosted.push(("analytical_adj".to_string(), r)),
                    None => skipped += 1,
                }
            }
            other => rest.extend(crate::clip_tools::sel_item(other)),
        }
    }
    hosted.extend(rest);
    if hosted.is_empty() {
        let key = if skipped > 0 {
            "elem.axis.nothing_to_delete"
        } else {
            "elem.no.not_yet"
        };
        return Err(tr(key, &[]));
    }
    Ok(hosted)
}

// ---- rebar set ----

/// A number of the set layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Field {
    Count,
    Spacing,
    ZoneEnd,
    ZoneEndStep,
    ZoneMidStep,
    MinorSpacing,
    Leg,
}

impl Field {
    fn key(self) -> &'static str {
        match self {
            Field::Count => "elem.rebar.field.count",
            Field::Spacing => "elem.rebar.field.spacing",
            Field::ZoneEnd => "elem.rebar.field.zone_end",
            Field::ZoneEndStep => "elem.rebar.field.zone_end_step",
            Field::ZoneMidStep => "elem.rebar.field.zone_mid_step",
            Field::MinorSpacing => "elem.rebar.field.minor_spacing",
            Field::Leg => "elem.rebar.field.leg",
        }
    }

    /// Range in metres; None: a whole number.
    fn range(self) -> Option<(f64, f64)> {
        match self {
            Field::Count => None,
            Field::Spacing | Field::MinorSpacing | Field::ZoneEndStep | Field::ZoneMidStep => {
                Some((0.01, 2.0))
            }
            Field::ZoneEnd => Some((0.05, 5.0)),
            Field::Leg => Some((0.0, 5.0)),
        }
    }
}

/// The numbers of a layout with their values (metres, or a count); a free bar has none.
pub fn fields(l: &RebarLayout) -> Vec<(Field, f64)> {
    match l {
        RebarLayout::Long { count, leg, .. } => {
            vec![(Field::Count, *count as f64), (Field::Leg, *leg)]
        }
        RebarLayout::Stirrup { rule, .. } => match rule {
            RebarPath::Fixed(n) => vec![(Field::Count, *n as f64)],
            RebarPath::MaxSpacing(s) | RebarPath::Spacing(s) => vec![(Field::Spacing, *s)],
            RebarPath::Zones { end, end_s, mid_s } => vec![
                (Field::ZoneEnd, *end),
                (Field::ZoneEndStep, *end_s),
                (Field::ZoneMidStep, *mid_s),
            ],
        },
        RebarLayout::Area {
            spacing,
            minor_spacing,
            ..
        } => vec![
            (Field::Spacing, *spacing),
            (Field::MinorSpacing, *minor_spacing),
        ],
        RebarLayout::Free { .. } => vec![],
    }
}

/// False when the layout has no such field.
pub fn set_field(l: &mut RebarLayout, f: Field, v: f64) -> bool {
    let n = v.round().max(1.0) as usize;
    match (l, f) {
        (RebarLayout::Long { count, .. }, Field::Count) => *count = n,
        (RebarLayout::Long { leg, .. }, Field::Leg) => *leg = v,
        (RebarLayout::Stirrup { rule, .. }, f) => match (rule, f) {
            (RebarPath::Fixed(c), Field::Count) => *c = n,
            (RebarPath::MaxSpacing(s) | RebarPath::Spacing(s), Field::Spacing) => *s = v,
            (RebarPath::Zones { end, .. }, Field::ZoneEnd) => *end = v,
            (RebarPath::Zones { end_s, .. }, Field::ZoneEndStep) => *end_s = v,
            (RebarPath::Zones { mid_s, .. }, Field::ZoneMidStep) => *mid_s = v,
            _ => return false,
        },
        (RebarLayout::Area { spacing, .. }, Field::Spacing) => *spacing = v,
        (RebarLayout::Area { minor_spacing, .. }, Field::MinorSpacing) => *minor_spacing = v,
        _ => return false,
    }
    true
}

#[derive(Clone, Copy, Debug)]
pub enum Edit {
    Type(i64),
    Field(Field, f64),
}

/// Writes one change of a set; the rows of the properties table and the panel share it.
pub fn edit_set(m: &mut Model, id: i64, e: Edit) -> Result<(), Error> {
    let mut s = m
        .rebar_set(id)?
        .ok_or_else(|| Error::Invalid("rebar set not found".into()))?;
    match e {
        Edit::Type(t) => s.rtype = t,
        Edit::Field(f, v) => {
            if !set_field(&mut s.layout, f, v) {
                return Err(Error::Invalid("rebar: no such field".into()));
            }
        }
    }
    m.update_rebar_set(id, &s)
}

/// One change of a set as one undo step; a refusal reaches the status line.
fn commit_set(ed: &mut Editor, id: i64, e: Edit) {
    let op = OpFn(Arc::new(move |m: &mut Model| edit_set(m, id, e)));
    ed.status = Some(match ed.run_op(op) {
        Ok(()) => tr("geom.changed", &[]),
        Err(e) => tr("geom.not_changed", &[("e", e.to_string().as_str())]),
    });
}

fn spec_of(m: &Model, id: i64) -> Result<RebarSetSpec, Error> {
    m.rebar_set(id)?
        .ok_or_else(|| Error::Invalid("rebar set not found".into()))
}

fn field_of(m: &Model, id: i64, f: Field) -> Result<f64, Error> {
    fields(&spec_of(m, id)?.layout)
        .into_iter()
        .find(|x| x.0 == f)
        .map(|x| x.1)
        .ok_or_else(|| Error::Invalid("this layout has no such number".into()))
}

/// The face whose cover the properties table edits; the other faces keep their values.
fn main_face(host_kind: &str) -> &'static str {
    match host_kind {
        "wall" => "face",
        "slab" | "footing" | "raft" => "bottom",
        _ => "side",
    }
}

fn cover_of(m: &Model, id: i64) -> Result<f64, Error> {
    let s = spec_of(m, id)?;
    let face = main_face(&s.host_kind);
    if !ddc_model::rebar_default_covers(&s.host_kind).iter().any(|c| c.0 == face) {
        return Err(Error::Invalid("this host has no cover".into()));
    }
    let stored = m
        .sql()
        .query_row(
            "SELECT cover FROM rebar_cover WHERE host_kind=?1 AND host=?2 AND face=?3",
            rusqlite::params![s.host_kind, s.host, face],
            |r| r.get::<_, f64>(0),
        )
        .ok();
    Ok(stored.unwrap_or_else(|| {
        ddc_model::rebar_default_covers(&s.host_kind)
            .iter()
            .find(|c| c.0 == face)
            .map_or(0.03, |c| c.1)
    }))
}

fn set_cover(m: &mut Model, id: i64, v: f64) -> Result<(), Error> {
    let s = spec_of(m, id)?;
    m.set_rebar_covers(
        &[(s.host_kind.clone(), s.host)],
        &[(main_face(&s.host_kind).to_string(), v)],
    )
}

/// Properties-table rows of a rebar set: bar type, spacing or count (only the one its layout
/// has), and the cover of its host. A change regenerates the bars. The panel shows the rows
/// whose `get` succeeds.
pub fn geom_rows() -> Vec<Row> {
    let row = |key: &'static str,
               unit: Unit,
               lo: f64,
               hi: f64,
               delta: f64,
               flds: &'static [&'static str],
               get: crate::geom_props::Get,
               set: crate::geom_props::Set| Row {
        kind: "rebar_set",
        key,
        unit,
        lo,
        hi,
        delta,
        effect: Effect::Scene,
        fields: flds,
        in_panel: true,
        get,
        set,
    };
    vec![
        row(
            "geom.rebar_type",
            Unit::Type("rebar_type"),
            0.0,
            1e12,
            0.0,
            &["rtype"],
            |m, id| Ok(spec_of(m, id)?.rtype as f64),
            |m, id, v| edit_set(m, id, Edit::Type(v.round() as i64)),
        ),
        row(
            "geom.rebar_spacing",
            Unit::Len,
            0.01,
            2.0,
            -0.1,
            &["spacing"],
            |m, id| field_of(m, id, Field::Spacing),
            |m, id, v| edit_set(m, id, Edit::Field(Field::Spacing, v)),
        ),
        row(
            "geom.rebar_count",
            Unit::Count,
            1.0,
            999.0,
            2.0,
            &["count"],
            |m, id| field_of(m, id, Field::Count),
            |m, id, v| edit_set(m, id, Edit::Field(Field::Count, v)),
        ),
        row(
            "geom.rebar_cover",
            Unit::Len,
            0.0,
            0.5,
            0.01,
            &["cover"],
            cover_of,
            set_cover,
        ),
    ]
}

/// What the model measures on the bars of a set.
#[derive(Clone, Debug, PartialEq)]
pub struct Live {
    pub bars: usize,
    pub length: f64,
    pub weight: f64,
    pub marks: Vec<String>,
    pub issues: Vec<&'static str>,
}

pub fn measure(m: &Model, id: i64) -> Result<Live, Error> {
    let built = m.rebar_built()?;
    let mine = || built.bars.iter().filter(|b| b.set == id);
    let marks: BTreeSet<&str> = mine().map(|b| b.mark.as_str()).collect();
    Ok(Live {
        bars: mine().count(),
        length: mine().map(|b| b.length).sum(),
        weight: mine().map(|b| b.weight()).sum(),
        marks: marks.into_iter().map(str::to_string).collect(),
        issues: built
            .issues
            .iter()
            .filter(|i| i.0 == id)
            .map(|i| i.1)
            .collect(),
    })
}

#[derive(Clone, Copy)]
enum Num {
    Len(f64, f64),
    Count,
}

/// A number row that keeps the dragged value until the model revision changes; Some when the
/// edit is finished and differs from the model.
fn drag_row(
    ui: &mut egui::Ui,
    rev: u64,
    u: Units,
    id: egui::Id,
    label: &str,
    cur: f64,
    num: Num,
) -> Option<f64> {
    let mut v = ui
        .data(|d| d.get_temp::<(u64, f64)>(id))
        .filter(|b| b.0 == rev)
        .map_or(cur, |b| b.1);
    let done = match num {
        Num::Len(lo, hi) => ui_kit::kv_field(
            ui,
            id,
            label,
            &mut v,
            ui_kit::NumKind::Len(u),
            lo,
            hi,
            crate::units::unit_label(u),
        ),
        Num::Count => ui_kit::kv_field(ui, id, label, &mut v, ui_kit::NumKind::COUNT, 1.0, 999.0, ""),
    };
    ui.data_mut(|d| d.insert_temp(id, (rev, v)));
    (done && (v - cur).abs() > 1e-9).then_some(v)
}

/// Panel part after the header: layout and bar type, then the measured quantities.
pub fn panel_set(ui: &mut egui::Ui, ed: &mut Editor, id: i64) {
    let spec = match ed.model.rebar_set(id) {
        Ok(Some(s)) => s,
        _ => {
            ui.label(tr("elem.rebar.missing", &[]));
            return;
        }
    };
    layout_section(ui, ed, id, &spec);
    quantities(ui, ed, id);
}

fn layout_section(ui: &mut egui::Ui, ed: &mut Editor, id: i64, spec: &RebarSetSpec) {
    if !ui_kit::heading(ui, &tr("elem.rebar.layout", &[])) {
        return;
    }
    let (u, rev) = (ed.tool.units, ed.revision);
    let mut pending: Option<Edit> = None;
    ui_kit::props(ui, &format!("elem.rebar.layout.{id}"), |ui| {
        let host = format!(
            "{} #{}",
            tr(&format!("rebar.host.{}", spec.host_kind), &[]),
            spec.host
        );
        ui_kit::kv(ui, &tr("elem.rebar.host", &[]), host);
        let kind = tr(&format!("elem.rebar.kind.{}", spec.layout.name()), &[]);
        ui_kit::kv(ui, &tr("elem.rebar.kind", &[]), kind);
        // bar type, spacing and count are rows of the properties table above
        for (f, cur) in fields(&spec.layout)
            .into_iter()
            .filter(|x| !matches!(x.0, Field::Count | Field::Spacing))
        {
            let num = f.range().map_or(Num::Count, |(lo, hi)| Num::Len(lo, hi));
            let key = egui::Id::new(("elem.rebar.field", id, f));
            if let Some(v) = drag_row(ui, rev, u, key, &tr(f.key(), &[]), cur, num) {
                pending = Some(Edit::Field(f, v));
            }
        }
    });
    if let Some(e) = pending {
        commit_set(ed, id, e);
    }
}

fn quantities(ui: &mut egui::Ui, ed: &Editor, id: i64) {
    // the measure walks every bar of the model: once per revision, not per frame
    let key = egui::Id::new(("elem.rebar.live", id));
    let rev = ed.revision;
    let cached = ui
        .data(|d| d.get_temp::<(u64, Result<Live, String>)>(key))
        .filter(|c| c.0 == rev);
    let got = match cached {
        Some(c) => c.1,
        None => {
            let r = measure(&ed.model, id).map_err(|e| e.to_string());
            ui.data_mut(|d| d.insert_temp(key, (rev, r.clone())));
            r
        }
    };
    if !ui_kit::heading(ui, &tr("elem.rebar.quantities", &[])) {
        return;
    }
    let l = match got {
        Ok(l) => l,
        Err(e) => {
            ui.label(tr("elem.rebar.not_built", &[("e", e.as_str())]));
            return;
        }
    };
    let u = ed.tool.units;
    ui_kit::props(ui, &format!("elem.rebar.live.{id}"), |ui| {
        ui_kit::kv_num(ui, &tr("elem.rebar.bars", &[]), l.bars.to_string());
        ui_kit::kv_num(ui, &tr("elem.rebar.length", &[]), display_length(l.length, u));
        let weight = tr("elem.rebar.kg", &[("v", fmt_num(l.weight, 1).as_str())]);
        ui_kit::kv_num(ui, &tr("elem.rebar.weight", &[]), weight);
        if !l.marks.is_empty() {
            let shown = if l.marks.len() > 4 {
                format!("{}, \u{2026}", l.marks[..3].join(", "))
            } else {
                l.marks.join(", ")
            };
            ui_kit::kv(ui, &tr("elem.rebar.marks", &[]), shown);
        }
    });
    let warn = ui.visuals().warn_fg_color;
    for code in &l.issues {
        ui.colored_label(warn, tr(&format!("rebar.issue.{code}"), &[]));
    }
}

// ---- analytical member ----

/// Offset `i` of an adjustment: 0..3 start x, y, z; 3..6 end x, y, z.
pub fn offset(a: &AnalyticalAdj, i: usize) -> f64 {
    let p = if i < 3 { a.start } else { a.end };
    [p.0, p.1, p.2][i % 3]
}

pub fn with_offset(a: &AnalyticalAdj, i: usize, v: f64) -> AnalyticalAdj {
    let mut a = *a;
    let p = if i < 3 { &mut a.start } else { &mut a.end };
    match i % 3 {
        0 => p.0 = v,
        1 => p.1 = v,
        _ => p.2 = v,
    }
    a
}

fn release_key(r: Option<AnRelease>) -> &'static str {
    match r {
        None => "elem.axis.rel.default",
        Some(AnRelease::Fixed) => "elem.axis.rel.fixed",
        Some(AnRelease::Pinned) => "elem.axis.rel.pinned",
    }
}

fn release_row(
    ui: &mut egui::Ui,
    salt: (&str, i64),
    label: &str,
    cur: Option<AnRelease>,
) -> Option<Option<AnRelease>> {
    let opts = [None, Some(AnRelease::Fixed), Some(AnRelease::Pinned)];
    let names: Vec<String> = opts.iter().map(|o| tr(release_key(*o), &[])).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut at = opts.iter().position(|o| *o == cur).unwrap_or(0);
    let th = ddc_ui::theme::of(ui.ctx());
    let changed = ui_kit::kv_widget(ui, label, |ui| {
        ddc_ui::widgets::field::dropdown(ui, &th, salt, &mut at, &names, Some(ui_kit::WIDGET_W)).changed()
    });
    changed.then(|| opts[at])
}

/// One change of the axis of a host as one undo step; an empty adjustment puts the axis back on
/// its host.
pub fn commit_axis(ed: &mut Editor, kind: &'static str, host: i64, adj: AnalyticalAdj) {
    let op = OpFn(Arc::new(move |m: &mut Model| {
        m.set_analytical_adj(kind, &[host], adj)
    }));
    ed.status = Some(match ed.run_op(op) {
        Ok(()) => tr("geom.changed", &[]),
        Err(e) => tr("geom.not_changed", &[("e", e.to_string().as_str())]),
    });
}

/// Panel part of a selected analytical member: offsets of its two ends and their releases.
pub fn panel_axis(ui: &mut egui::Ui, ed: &mut Editor, id: i64) {
    let Some((kind, host)) = crate::rebar_scene::an_split(id) else {
        ui.label(tr("elem.axis.no_adjust", &[]));
        return;
    };
    if !ui_kit::heading(ui, &tr("elem.axis.adjust", &[])) {
        return;
    }
    let (u, rev) = (ed.tool.units, ed.revision);
    // one query per model revision, not per frame
    let ckey = egui::Id::new(("elem.axis.adj", id));
    let stored = match ui
        .data(|d| d.get_temp::<(u64, Option<AnalyticalAdj>)>(ckey))
        .filter(|c| c.0 == rev)
    {
        Some(c) => c.1,
        None => {
            let v = ed.model.analytical_adj(kind, host).ok().flatten();
            ui.data_mut(|d| d.insert_temp(ckey, (rev, v)));
            v
        }
    };
    let adj = stored.unwrap_or_default();
    let mut pending: Option<AnalyticalAdj> = None;
    ui_kit::props(ui, &format!("elem.axis.{id}"), |ui| {
        let state = if stored.is_some() {
            "elem.axis.adjusted"
        } else {
            "elem.axis.follows"
        };
        ui_kit::kv(ui, &tr("elem.axis.state", &[]), tr(state, &[]));
        for i in 0..6 {
            let key = egui::Id::new(("elem.axis.off", id, i));
            let label = tr(&format!("elem.axis.off.{i}"), &[]);
            let num = Num::Len(-10.0, 10.0);
            if let Some(v) = drag_row(ui, rev, u, key, &label, offset(&adj, i), num) {
                pending = Some(with_offset(&adj, i, v));
            }
        }
        if let Some(r) = release_row(
            ui,
            ("elem.axis.rel_a", id),
            &tr("elem.axis.rel_start", &[]),
            adj.release_start,
        ) {
            pending = Some(AnalyticalAdj {
                release_start: r,
                ..adj
            });
        }
        if let Some(r) = release_row(
            ui,
            ("elem.axis.rel_b", id),
            &tr("elem.axis.rel_end", &[]),
            adj.release_end,
        ) {
            pending = Some(AnalyticalAdj {
                release_end: r,
                ..adj
            });
        }
    });
    let reset = ui
        .add_enabled(
            stored.is_some(),
            egui::Button::new(tr("elem.axis.reset", &[])),
        )
        .on_hover_text(tr("elem.axis.hint", &[]))
        .clicked();
    if reset {
        pending = Some(AnalyticalAdj::default());
    }
    if let Some(a) = pending {
        commit_axis(ed, kind, host, a);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom_props::tests::fixture;
    use ddc_model::RebarHook;

    fn stirrups(ed: &mut Editor, beam: i64, rule: RebarPath) -> (i64, i64) {
        let ty = ed
            .model
            .add_rebar_type("E6 test 8", 0.008, "A500", 0.032)
            .unwrap();
        let id = ed
            .model
            .add_rebar_sets(&[RebarSetSpec {
                host_kind: "beam".into(),
                host: beam,
                rtype: ty,
                layout: RebarLayout::Stirrup {
                    rule,
                    hook: RebarHook::H135,
                },
                mark: None,
            }])
            .unwrap()[0];
        ed.reload().unwrap();
        (id, ty)
    }

    #[test]
    fn main_face_is_a_cover_face_of_every_host() {
        for (kind, _) in ddc_model::REBAR_HOSTS {
            let face = main_face(kind);
            assert!(
                ddc_model::rebar_default_covers(kind).iter().any(|c| c.0 == face),
                "{kind}: {face}"
            );
        }
    }

    #[test]
    fn layout_numbers_round_trip_per_form() {
        let mut l = RebarLayout::Stirrup {
            rule: RebarPath::MaxSpacing(0.2),
            hook: RebarHook::H135,
        };
        assert_eq!(fields(&l), vec![(Field::Spacing, 0.2)]);
        assert!(set_field(&mut l, Field::Spacing, 0.1));
        assert_eq!(fields(&l), vec![(Field::Spacing, 0.1)]);
        assert!(
            !set_field(&mut l, Field::Count, 3.0),
            "no count on a spacing rule"
        );
        let mut z = RebarLayout::Stirrup {
            rule: RebarPath::Zones {
                end: 0.5,
                end_s: 0.1,
                mid_s: 0.2,
            },
            hook: RebarHook::H135,
        };
        assert!(set_field(&mut z, Field::ZoneMidStep, 0.25));
        assert_eq!(fields(&z).len(), 3);
        assert_eq!(fields(&z)[2], (Field::ZoneMidStep, 0.25));
        let mut f = RebarLayout::Stirrup {
            rule: RebarPath::Fixed(4),
            hook: RebarHook::H135,
        };
        assert!(set_field(&mut f, Field::Count, 6.4));
        assert_eq!(fields(&f), vec![(Field::Count, 6.0)]);
    }

    #[test]
    fn axis_offsets_address_the_right_end() {
        let a = AnalyticalAdj::default();
        let b = with_offset(&a, 4, 0.3);
        assert_eq!(b.end, (0.0, 0.3, 0.0));
        assert_eq!(b.start, (0.0, 0.0, 0.0));
        assert_eq!(offset(&b, 4), 0.3);
        assert_eq!(offset(&b, 1), 0.0);
    }

    #[test]
    fn editing_a_set_changes_its_bars_in_one_undo_step() {
        let mut fx = fixture();
        let beam = fx.ids["beam"];
        let ed = &mut fx.ed;
        let (id, ty) = stirrups(ed, beam, RebarPath::MaxSpacing(0.2));
        let n0 = ed.model.rebar_bars(None).unwrap().len();
        assert!(n0 > 0);

        commit_set(ed, id, Edit::Field(Field::Spacing, 0.1));
        assert!(
            ed.model.rebar_bars(None).unwrap().len() > n0,
            "denser stirrups"
        );
        ed.undo();
        assert_eq!(ed.model.rebar_bars(None).unwrap().len(), n0);

        let fat = ed
            .model
            .add_rebar_type("E6 test 16", 0.016, "A500", 0.032)
            .unwrap();
        commit_set(ed, id, Edit::Type(fat));
        assert!(ed
            .model
            .rebar_bars(None)
            .unwrap()
            .iter()
            .all(|b| b.rtype == fat));
        ed.undo();
        assert!(ed
            .model
            .rebar_bars(None)
            .unwrap()
            .iter()
            .all(|b| b.rtype == ty));
    }

    #[test]
    fn delete_takes_the_set_with_a_wall_and_one_undo_restores_both() {
        let mut fx = fixture();
        let (beam, wall) = (fx.ids["beam"], fx.ids["wall"]);
        let ed = &mut fx.ed;
        let (id, _) = stirrups(ed, beam, RebarPath::MaxSpacing(0.2));
        let n0 = ed.model.rebar_bars(None).unwrap().len();
        ed.selection = vec![Sel::Wall(wall), Sel::Ext(EXT_REBAR, id)];
        ed.delete_selection();
        assert!(ed.model.rebar_set(id).unwrap().is_none());
        assert!(ed.model.rebar_bars(None).unwrap().is_empty());
        ed.undo();
        assert!(ed.model.rebar_set(id).unwrap().is_some());
        assert_eq!(ed.model.rebar_bars(None).unwrap().len(), n0);
    }

    #[test]
    fn deleting_an_axis_resets_it_and_an_unadjusted_axis_refuses() {
        let mut fx = fixture();
        let beam = fx.ids["beam"];
        let ed = &mut fx.ed;
        let sel = Sel::Ext(EXT_ANALYTICAL, crate::rebar_scene::an_id("beam", beam));

        let head = ed.model.change_head();
        ed.selection = vec![sel];
        ed.delete_selection();
        assert_eq!(ed.model.change_head(), head, "a refusal changes nothing");
        let why = tr("elem.axis.nothing_to_delete", &[]);
        assert!(ed.status.as_deref().is_some_and(|s| s.contains(&why)));

        let adj = with_offset(&AnalyticalAdj::default(), 2, 0.1);
        commit_axis(ed, "beam", beam, adj);
        assert_eq!(ed.model.analytical_adj("beam", beam).unwrap(), Some(adj));
        ed.selection = vec![sel];
        ed.delete_selection();
        assert!(ed.model.analytical_adj("beam", beam).unwrap().is_none());
        ed.undo();
        assert_eq!(ed.model.analytical_adj("beam", beam).unwrap(), Some(adj));
    }

    #[test]
    fn hosted_families_refuse_transforms_with_their_reason() {
        let set = Sel::Ext(EXT_REBAR, 1);
        let why = super::super::hosted_refusal(&[Sel::Wall(1), set]).unwrap();
        assert_eq!(why, tr(super::super::REBAR_HOST, &[]));
        assert!(super::super::hosted_refusal(&[Sel::Wall(1)]).is_none());
    }

    #[test]
    fn hosted_codes_are_picked_by_their_edges() {
        use crate::editor::structx::{code_of, edge_only, ROOF_CODE};
        assert!(edge_only(EXT_REBAR));
        assert!(edge_only(EXT_ANALYTICAL));
        assert!(edge_only(ROOF_CODE));
        assert!(!edge_only(code_of("raft").unwrap()));
    }

    #[test]
    fn an_axis_is_selected_in_3d_by_its_own_id_not_by_the_row() {
        use crate::edit3d::pick::{Cat, Target};
        let mut fx = fixture();
        let beam = fx.ids["beam"];
        let ed = &mut fx.ed;
        commit_axis(ed, "beam", beam, with_offset(&AnalyticalAdj::default(), 2, 0.1));
        let row = adjustment_row(&ed.model, "beam", beam).unwrap();
        let key = axis_key(&ed.model, row).unwrap();
        assert_eq!(key, crate::rebar_scene::an_id("beam", beam));
        assert_ne!(key, row);
        let sel = Sel::Ext(EXT_ANALYTICAL, key);
        let t = Target::new(Cat::Ext(EXT_ANALYTICAL), key);
        assert!(t.sel() == Some(sel) && Target::from_sel(sel) == Some(t));
        assert_eq!(super::super::of_sel(sel).map(|f| f.table), Some("analytical_adj"));
        // Delete from that selection drops the row, whatever its id
        ed.selection = vec![sel];
        ed.delete_selection();
        assert!(adjustment_row(&ed.model, "beam", beam).is_none());
        ed.undo();
        assert_eq!(adjustment_row(&ed.model, "beam", beam), Some(row));
        assert!(axis_key(&ed.model, row + 1000).is_none());
    }
}
