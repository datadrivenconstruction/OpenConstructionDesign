//! Ramp: axis clicked bottom to top, Enter finishes. Length follows from rise and slope:
//! the last axis segment is extended, a long flight is split by landings.

use super::opt_field;
use super::ui::poly_any;
use super::{tup, Active, Opts, Rec};
use crate::editor::Editor;
use crate::units::{format_length, Units};
use ddc_geom::ramp::RampDef;
use ddc_geom::V2;
use ddc_model::RampSpec;
use ddc_ui::i18n::Catalog;
use egui::{self, Color32, FontId, Pos2, Stroke, Vec2};

fn spec(a: &Active, pts: &[V2]) -> RampSpec {
    let o = &a.opts;
    let top_level = if o.to_top { a.above.map(|l| l.0) } else { None };
    let mut s = RampSpec::new(a.level, top_level, pts.iter().map(|p| tup(*p)).collect());
    s.base_offset = o.base_off;
    s.top_offset = if top_level.is_some() { 0.0 } else { o.top_off };
    s.width = o.width;
    s.slope = o.slope / 100.0;
    s.thickness = o.thick;
    s.landing = o.landing;
    s.max_run = o.max_run;
    s
}

pub fn rec(a: &Active, pts: &[V2]) -> Result<Rec, String> {
    let s = spec(a, pts);
    Ok(Rec::new("ramp", move |m| {
        m.add_ramp(&s)?;
        Ok(1)
    }))
}

#[allow(clippy::too_many_arguments)]
pub fn preview(
    p: &egui::Painter,
    ed: &Editor,
    a: &Active,
    bad: Color32,
    fill: Color32,
    st: Stroke,
    font: &FontId,
    to_s: &dyn Fn(V2) -> Pos2,
) {
    if a.pts.is_empty() {
        return;
    }
    let mut path = a.pts.clone();
    path.push(a.cur);
    let o = &a.opts;
    let base = a.elevation + o.base_off;
    let top = match (o.to_top, a.above) {
        (true, Some((_, z))) => z,
        _ => base + o.top_off,
    };
    let def = RampDef {
        path: path.clone(),
        base_z: base,
        rise: top - base,
        width: o.width,
        slope: o.slope / 100.0,
        thickness: o.thick,
        landing: o.landing,
        max_run: o.max_run,
    };
    let s = |v: &[V2]| v.iter().map(|q| to_s(*q)).collect::<Vec<Pos2>>();
    let at = to_s(a.cur) + Vec2::new(12.0, -12.0);
    let Ok(r) = def.layout() else {
        p.add(egui::Shape::line(s(&path), Stroke::new(st.width, bad)));
        p.text(
            at,
            egui::Align2::LEFT_BOTTOM,
            a.t("core_tools.ramp_bad"),
            font.clone(),
            bad,
        );
        return;
    };
    for f in &r.flights {
        poly_any(p, &s(&f.ring(o.width)), fill, st);
    }
    for l in &r.landings {
        poly_any(p, &s(&l.ring), fill, st);
    }
    let plan = r.plan(r.top_z() + 1.0);
    p.add(egui::Shape::line(s(&plan.run_line), st));
    for (x, y) in plan.head {
        p.line_segment([to_s(x), to_s(y)], st);
    }
    let q = r.qty();
    let u = ed.tool.units;
    let label = format!(
        "{}  ↑{}  {:.1} %",
        format_length(q.run, u),
        format_length(q.rise, u),
        o.slope
    );
    p.text(at, egui::Align2::LEFT_BOTTOM, label, font.clone(), st.color);
}

pub fn options(ui: &mut egui::Ui, cat: &Catalog, o: &mut Opts, has_above: bool, u: Units) {
    opt_field::len(ui, &cat.t("opt.width"), &mut o.width, (0.3, 20.0), u);
    opt_field::percent(ui, &cat.t("opt.slope_pct"), &mut o.slope, (0.5, 100.0));
    opt_field::len(ui, &cat.t("sp.thickness"), &mut o.thick, (0.02, 2.0), u);
    opt_field::len(ui, &cat.t("core_tools.landing"), &mut o.landing, (0.3, 20.0), u);
    opt_field::len(ui, &cat.t("core_tools.max_run"), &mut o.max_run, (0.5, 200.0), u);
    opt_field::len(ui, &cat.t("opt.base_off"), &mut o.base_off, (-10.0, 50.0), u);
    ui.add_enabled_ui(has_above, |ui| {
        opt_field::toggle(ui, &cat.t("core_tools.to_top"), &mut o.to_top);
    });
    if !o.to_top {
        opt_field::len(ui, &cat.t("core_tools.height"), &mut o.top_off, (0.01, 100.0), u);
    }
}
