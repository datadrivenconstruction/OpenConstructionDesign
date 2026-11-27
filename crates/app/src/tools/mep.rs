// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Networks: runs (duct, pipe, tray, conduit) — a chain of points, Enter or a click on a network;
//! elbows and tees are placed by the model; fixtures — library components connected to a system;
//! connect/disconnect; auto-route. Each action is a single `run_op`.

use std::sync::Arc;

use super::ui::num;
use super::{component, Active, Kind, Opts};
use crate::editor::{Editor, OpFn};
use ddc_geom::{v2, V2};
use ddc_model::{
    routed_points, CompCategory, DeviceSpec, MepDomain, MepHit, MepSection, MepShape, MepSnapSet,
    Model, PipeMaterial, RouteEnd, RouteSpec, RunSpec, SystemKind,
};
use super::mep_color::{active_theme, system_dropdown, system_items, Mark, SWATCH_PX};
use crate::units::{fmt_num_lang, Units};
use ddc_ui::i18n::{Catalog, Lang};
use ddc_ui::theme::control;
use ddc_ui::widgets;
use egui::{self, Color32};

/// Network tool state.
#[derive(Default)]
pub struct MepTool {
    /// Snapshot of pick points for a model revision.
    snap: Option<(u64, Arc<MepSnapSet>)>,
    /// What is under the cursor.
    pub hit: Option<MepHit>,
    first: Option<MepHit>,
    last: Option<MepHit>,
    /// System to connect to; None — the fixture is placed unconnected.
    pub system: Option<i64>,
}

impl MepTool {
    pub fn clear_path(&mut self) {
        self.first = None;
        self.last = None;
    }
    pub fn drop_last(&mut self) {
        self.last = None;
    }
}

/// Domain a run tool draws.
fn tool_domain(kind: Kind) -> MepDomain {
    match kind {
        Kind::Duct => MepDomain::Duct,
        Kind::Pipe => MepDomain::Pipe,
        _ => MepDomain::CableTray,
    }
}

/// Name key and kind of the system created when the user picked none.
fn default_system(kind: Kind) -> (&'static str, SystemKind) {
    match kind {
        Kind::Duct => ("mep.sys.duct", SystemKind::SupplyAir),
        Kind::Pipe => ("mep.sys.pipe", SystemKind::ColdWater),
        _ => ("mep.sys.tray", SystemKind::Power),
    }
}

/// Systems of the model the tool can draw into.
fn fitting_systems(ed: &Editor, kind: Kind) -> Vec<ddc_model::MepSystem> {
    let d = tool_domain(kind);
    ed.model
        .mep_systems()
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s.kind.domains().contains(&d))
        .collect()
}

/// System the run is drawn into: the picked one, else the default for the tool (created by name).
fn system(ed: &Editor, a: &Active) -> (String, SystemKind) {
    let picked = a
        .mep
        .system
        .and_then(|id| fitting_systems(ed, a.kind).into_iter().find(|s| s.id == id));
    match picked {
        Some(s) => (s.name, s.kind),
        None => {
            let (key, kind) = default_system(a.kind);
            (a.t(key), kind)
        }
    }
}

/// Pipe material chosen in the options bar.
pub fn material(o: &Opts) -> PipeMaterial {
    PipeMaterial::ALL
        .get(o.pipe_mat as usize)
        .copied()
        .unwrap_or(PipeMaterial::Steel)
}

/// Duct section from the options: round when `duct_round` (width is the diameter), else rectangle.
fn duct_shape(o: &Opts) -> MepShape {
    if o.duct_round {
        MepShape::Round { d: o.width }
    } else {
        MepShape::Rect {
            w: o.width,
            h: o.depth,
        }
    }
}

/// "Family : Type" labels for the fixture tool.
pub fn device_labels(kind: Kind) -> Vec<String> {
    let wanted: &[CompCategory] = match kind {
        Kind::Terminal => &[CompCategory::AirTerminal],
        Kind::Fixture => &[CompCategory::Plumbing],
        Kind::Light => &[CompCategory::Lighting],
        Kind::Equipment => &[CompCategory::Equipment],
        _ => &[CompCategory::Electrical, CompCategory::Equipment],
    };
    ddc_model::library()
        .unwrap_or_default()
        .iter()
        .filter(|f| wanted.contains(&f.category))
        .flat_map(|f| {
            f.types
                .iter()
                .map(move |t| format!("{} : {}", f.name, t.name))
        })
        .collect()
}

/// Outer section size in plan (preview band width), m.
pub fn plan_width(a: &Active) -> f64 {
    match a.kind {
        Kind::Pipe => material(&a.opts).by_dn(a.opts.dn).map_or(0.06, |s| s.od),
        Kind::Conduit => a.opts.dn as f64 / 1000.0,
        _ => a.opts.width,
    }
}

pub fn run(a: &Active, sys: i64, pts: &[V2]) -> Result<RunSpec, ddc_model::Error> {
    let z = a.elevation + a.opts.z;
    let p3: Vec<(f64, f64, f64)> = pts.iter().map(|p| (p.x, p.y, z)).collect();
    let rect = MepShape::Rect {
        w: a.opts.width,
        h: a.opts.depth,
    };
    let round = MepShape::Round {
        d: a.opts.dn as f64 / 1000.0,
    };
    let mut r = match a.kind {
        Kind::Pipe => RunSpec::pipe(sys, a.level, material(&a.opts), a.opts.dn, p3)?,
        Kind::Duct => RunSpec::new(
            sys,
            a.level,
            MepSection::new(MepDomain::Duct, duct_shape(&a.opts)),
            p3,
        ),
        Kind::Conduit => RunSpec::new(sys, a.level, MepSection::new(MepDomain::Conduit, round), p3),
        _ => RunSpec::new(
            sys,
            a.level,
            MepSection::new(MepDomain::CableTray, rect),
            p3,
        ),
    };
    r.slope = a.opts.slope;
    r.section.insulation = a.opts.insulation;
    Ok(r)
}

fn op(
    ed: &mut Editor,
    a: &Active,
    f: impl Fn(&mut Model) -> Result<(), ddc_model::Error> + Send + Sync + 'static,
) -> bool {
    match ed.run_op(OpFn(Arc::new(f))) {
        Ok(()) => {
            ed.status = Some(if a.kind == Kind::Disconnect {
                a.t("mep.disconnected")
            } else {
                a.cat.tf(
                    "tool.done",
                    &[("what", &a.t(&format!("tool.{}", a.kind.key())))],
                )
            });
            ed.tool.layer.sync(&ed.model, ed.level, ed.revision);
            true
        }
        Err(e) => {
            ed.status = Some(format!(
                "{}: {}",
                a.t("tool.err.model"),
                crate::mep_browser::err_text(&a.cat, &e)
            ));
            false
        }
    }
}

/// Cursor snap to a network: connection, segment end, point on the axis of its own network kind.
pub fn snap(ed: &Editor, a: &mut Active, p: V2) {
    a.mep.hit = None;
    if !a.kind.snaps_network() {
        return;
    }
    let set = match &a.mep.snap {
        Some((rev, s)) if *rev == ed.revision => s.clone(),
        _ => {
            let Ok(s) = ed.model.mep_snap_set() else {
                return;
            };
            let s = Arc::new(s);
            a.mep.snap = Some((ed.revision, s.clone()));
            s
        }
    };
    let kind = a.kind;
    let want = a.mep.first.as_ref().and_then(MepHit::system);
    let keep = move |d: MepDomain, sys: Option<i64>| {
        let fits = match kind {
            Kind::Duct => d == MepDomain::Duct,
            Kind::Pipe => d == MepDomain::Pipe,
            Kind::Tray | Kind::Conduit => {
                matches!(d, MepDomain::CableTray | MepDomain::Conduit)
            }
            _ => true,
        };
        fits && (sys.is_none() || want.is_none() || sys == want)
    };
    // a network below the floor (drainage) is also pickable from the level plan
    let zs = (a.elevation - 1.0, a.above.map_or(f64::INFINITY, |l| l.1));
    let (x, y) = (p.x, p.y);
    a.mep.hit = set.pick((x, y), ed.tool.radius, zs, &keep);
    if let Some(h) = &a.mep.hit {
        let (hx, hy, _) = h.at();
        a.cur = v2(hx, hy);
    }
}

/// Chain click before a point is added; true — a point on a network finishes the run.
pub fn on_point(a: &mut Active) -> bool {
    let hit = a.mep.hit.clone();
    let ends = hit.is_some() && !a.pts.is_empty();
    if a.pts.is_empty() {
        a.mep.first = hit.clone();
    }
    a.mep.last = hit;
    ends
}

/// Write the run. Ends on a network set the system and elevation: the system comes from the segment,
/// a start on a segment carries the whole run at its elevation; a port or another elevation
/// makes a vertical joint.
pub fn draw(ed: &mut Editor, a: &mut Active, pts: &[V2]) -> bool {
    let (first, last) = (a.mep.first.clone(), a.mep.last.clone());
    let (name, kind) = draw_system(ed, a, [first.as_ref(), last.as_ref()]);
    let mut spec = match run(a, 0, pts) {
        Ok(s) => s,
        Err(e) => {
            ed.status = Some(format!("{}: {e}", a.t("tool.err.model")));
            return false;
        }
    };
    if first.is_some() || last.is_some() {
        let z = match &first {
            Some(h @ (MepHit::RunEnd { .. } | MepHit::RunMid { .. })) => h.at().2,
            _ => a.elevation + a.opts.z,
        };
        let plan: Vec<(f64, f64)> = pts.iter().map(|p| (p.x, p.y)).collect();
        spec.points = routed_points(
            &plan,
            z,
            a.opts.slope,
            first.as_ref().map(|h| h.at().2),
            last.as_ref().map(|h| h.at().2),
            spec.min_leg(),
        );
        spec.slope = 0.0;
    }
    let done = op(ed, a, move |m| m.mep_draw(&name, kind, &spec).map(|_| ()));
    if done {
        a.mep.clear_path();
    }
    done
}

/// System a drawn run goes into: the network at an end, else the picked or default one.
pub(crate) fn draw_system(ed: &Editor, a: &Active, ends: [Option<&MepHit>; 2]) -> (String, SystemKind) {
    let on_net = ends.into_iter().flatten().find_map(MepHit::system);
    let found = on_net.and_then(|id| {
        let all = ed.model.mep_systems().unwrap_or_default();
        all.into_iter().find(|s| s.id == id)
    });
    match found {
        Some(s) => (s.name, s.kind),
        None => system(ed, a),
    }
}

/// Run drawn in 3D: the section of the run it starts on, else the tool options; axis points as
/// given (absolute z). An end on a network at another elevation gets a vertical joint (as in plan).
pub(crate) fn spec_3d(
    ed: &Editor,
    a: &Active,
    first: Option<&MepHit>,
    last: Option<&MepHit>,
    pts: &[(f64, f64, f64)],
) -> Result<(String, SystemKind, RunSpec), ddc_model::Error> {
    let (name, kind) = draw_system(ed, a, [first, last]);
    let from_run = match first {
        Some(MepHit::RunEnd { run, .. } | MepHit::RunMid { run, .. }) => ed.model.mep_run(*run).ok(),
        _ => None,
    };
    let mut spec = match from_run {
        Some(r) => RunSpec {
            level: a.level,
            slope: 0.0,
            caps: (false, false),
            ..r
        },
        None => run(a, 0, &[])?,
    };
    let mut p = pts.to_vec();
    let n = p.len();
    if let (Some(h), true) = (last, n >= 2) {
        let (prev, end, e) = (p[n - 2], p[n - 1], h.at().2);
        if (e - end.2).abs() > 1e-9 {
            let plan = [(prev.0, prev.1), (end.0, end.1)];
            let tail = routed_points(&plan, end.2, 0.0, None, Some(e), spec.min_leg());
            p.pop();
            p.extend(tail.into_iter().skip(1));
        }
    }
    spec.points = p;
    spec.slope = 0.0;
    Ok((name, kind, spec))
}

/// Point-tool click: fixture, connect, disconnect.
pub fn click_point(ed: &mut Editor, a: &mut Active, q: V2) {
    match a.kind {
        Kind::Connect => connect(ed, a),
        Kind::Disconnect => disconnect(ed, a),
        _ => place(ed, a, q),
    }
}

fn place(ed: &mut Editor, a: &mut Active, q: V2) {
    let label = a.type_name().to_string();
    let Some((family, ty)) = label.split_once(" : ") else {
        ed.status = Some(a.t("tool.err.no_library"));
        return;
    };
    let at = match component::tool_spec(ed, a, q, family) {
        Ok(s) => s,
        Err(m) => {
            ed.status = Some(m);
            return;
        }
    };
    let d = DeviceSpec {
        family: family.to_string(),
        ty: ty.to_string(),
        place: at,
        system: a.mep.system,
        flow: None,
    };
    op(ed, a, move |m| m.mep_place(&d).map(|_| ()));
}

fn connect(ed: &mut Editor, a: &mut Active) {
    let system = a.mep.system;
    match a.mep.hit.clone() {
        Some(MepHit::Port(p)) => {
            op(ed, a, move |m| {
                m.mep_attach(p.comp, &p.name, system).map(|_| ())
            });
        }
        Some(MepHit::RunEnd { run, end, .. }) => {
            op(ed, a, move |m| m.mep_connect_end(run, end, None));
        }
        _ => ed.status = Some(a.t("mep.err.no_target")),
    }
}

fn disconnect(ed: &mut Editor, a: &mut Active) {
    match a.mep.hit.clone() {
        Some(MepHit::Port(p)) => {
            op(ed, a, move |m| m.mep_detach(p.comp, &p.name));
        }
        Some(MepHit::RunEnd { run, end, .. }) => {
            op(ed, a, move |m| m.mep_detach_end(run, end));
        }
        _ => ed.status = Some(a.t("mep.err.no_target")),
    }
}

/// Section of a free auto-route from the selected system's network kind and the options.
fn template(ed: &Editor, a: &Active) -> Option<RunSpec> {
    let id = a.mep.system?;
    let kind = ed
        .model
        .mep_systems()
        .ok()?
        .into_iter()
        .find(|s| s.id == id)?
        .kind;
    let mut spec = match kind.domains()[0] {
        MepDomain::Pipe => {
            RunSpec::pipe(0, a.level, material(&a.opts), a.opts.dn, vec![]).ok()?
        }
        MepDomain::Duct => RunSpec::new(
            0,
            a.level,
            MepSection::new(MepDomain::Duct, duct_shape(&a.opts)),
            vec![],
        ),
        _ => RunSpec::new(
            0,
            a.level,
            MepSection::new(
                MepDomain::Conduit,
                MepShape::Round {
                    d: a.opts.dn as f64 / 1000.0,
                },
            ),
            vec![],
        ),
    };
    spec.section.insulation = a.opts.insulation;
    Some(spec)
}

/// Auto-route: first click is the start, second the end; ends on a network give the system and section.
pub fn route_click(ed: &mut Editor, a: &mut Active, q: V2) {
    let Some(&p0) = a.pts.first() else {
        a.mep.first = a.mep.hit.clone();
        a.pts.push(q);
        return;
    };
    let free = |p: V2| RouteEnd::Free { x: p.x, y: p.y };
    let from = a.mep.first.as_ref().map_or(free(p0), MepHit::route_end);
    let to = a.mep.hit.as_ref().map_or(free(q), MepHit::route_end);
    // only a segment supplies the system: a connection has none
    let on_run = matches!(from, RouteEnd::Run { .. }) || matches!(to, RouteEnd::Run { .. });
    if !on_run && a.mep.system.is_none() {
        ed.status = Some(a.t("mep.err.no_system"));
        return;
    }
    let spec = RouteSpec {
        from,
        to,
        z: a.elevation + a.opts.z,
        slope: a.opts.slope,
        level: a.level,
        system: a.mep.system,
        template: template(ed, a),
    };
    if op(ed, a, move |m| m.mep_route(&spec).map(|_| ())) {
        a.pts.clear();
        a.mep.clear_path();
    }
}

/// Option bar for tools without their own block in `ui::options_row`.
pub fn options(
    ui: &mut egui::Ui,
    ed: &Editor,
    kind: Kind,
    cat: &Catalog,
    o: &mut Opts,
    t: &mut MepTool,
    m: &str,
    paper: Color32,
) {
    if kind == Kind::Conduit {
        ui.label(cat.t("mep_units.diameter"));
        size_dropdown(ui, cat, ed.tool.units, "tool_conduit", &mut o.dn, &CONDUIT_DN);
    }
    if !matches!(kind, Kind::Connect | Kind::Disconnect) {
        num(ui, &cat.t("opt.z"), &mut o.z, m, (-5.0, 30.0), ed.tool.units);
    }
    if kind == Kind::Route {
        num(ui, &cat.t("opt.slope_pct"), &mut o.slope, " %", (0.0, 10.0), ed.tool.units);
    }
    if matches!(kind, Kind::Conduit | Kind::Disconnect) {
        return;
    }
    let systems = ed.model.mep_systems().unwrap_or_default();
    if t.system
        .is_some_and(|id| !systems.iter().any(|s| s.id == id))
    {
        t.system = None;
    }
    let rows = system_items(&systems, &cat.t("mep_units.system.none"), Mark::Dash);
    system_label(ui, cat.t("mep.system"));
    let th = active_theme(ui);
    system_dropdown(ui, &th, "tool_mep_system", &mut t.system, &rows, paper);
}

/// System picker of the duct, pipe and tray tools: the model's own systems of the right kind;
/// the first entry draws into a new system with the default name.
pub fn run_system(
    ui: &mut egui::Ui,
    ed: &Editor,
    kind: Kind,
    cat: &Catalog,
    t: &mut MepTool,
    paper: Color32,
) {
    let systems = fitting_systems(ed, kind);
    if t.system.is_some_and(|id| !systems.iter().any(|s| s.id == id)) {
        t.system = None;
    }
    let rows = system_items(&systems, &cat.t("mep_units.system.new"), Mark::Plus);
    system_label(ui, cat.t("mep.system"));
    let th = active_theme(ui);
    system_dropdown(ui, &th, "tool_run_system", &mut t.system, &rows, paper);
}

/// The system label; in a wrapping row it starts a new line together with its fixed-width
/// dropdown when the line has no room for both.
fn system_label(ui: &mut egui::Ui, text: String) {
    field_label(ui, text, control::DROPDOWN_W + SWATCH_PX + ddc_ui::theme::space::XS);
}

/// A label with a field `need` wide after it: in a wrapping row both start a new line when
/// the rest of the line has no room for them.
fn field_label(ui: &mut egui::Ui, text: String, need: f32) {
    let g = egui::WidgetText::from(text.as_str()).into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        egui::TextStyle::Body,
    );
    let row = g.size().x + 2.0 * ui.spacing().item_spacing.x + need;
    // a wrapping Ui reports the whole line as available; what is left is before the wrap
    let started = ui.cursor().min.x > ui.max_rect().left() + 0.5;
    let room = ui.available_rect_before_wrap();
    if ui.layout().main_wrap && started && room.max.x - room.min.x < row {
        ui.end_row();
    }
    ui.label(text);
}

/// Kit dropdown at the height of the options-bar row it sits in.
fn bar_dropdown(ui: &mut egui::Ui, id: &str, sel: &mut usize, items: &[&str], w: Option<f32>) -> bool {
    let th = active_theme(ui);
    let row_h = ui.spacing().interact_size.y;
    widgets::field::dropdown_h(ui, &th, id, sel, items, w, Some(row_h)).changed()
}

/// Nominal sizes of the conduit tool, mm.
const CONDUIT_DN: [u32; 7] = [16, 20, 25, 32, 40, 50, 63];

/// Size text in the project units: "20 mm" or "0.79 in", the number in the catalog language.
pub(crate) fn size_label(dn: u32, units: Units, cat: &Catalog) -> String {
    let num = |v: f64, d: usize| fmt_num_lang(v, d, cat.lang);
    match units {
        Units::Metric => format!("{}\u{a0}{}", num(f64::from(dn), 0), cat.t("mep_units.unit.mm")),
        Units::Imperial => format!("{}\u{a0}{}", num(f64::from(dn) / 25.4, 2), cat.t("mep_units.unit.in")),
    }
}

/// Size list texts in the order of `dns`.
pub(crate) fn size_items(dns: &[u32], units: Units, cat: &Catalog) -> Vec<String> {
    dns.iter().map(|&d| size_label(d, units, cat)).collect()
}

/// Kit dropdown over `dns` (mm); a current size missing from the list is kept in it.
fn size_dropdown(
    ui: &mut egui::Ui,
    cat: &Catalog,
    units: Units,
    id: &str,
    dn: &mut u32,
    dns: &[u32],
) -> bool {
    let mut list = dns.to_vec();
    if !list.contains(dn) {
        list.push(*dn);
        list.sort_unstable();
    }
    let items = size_items(&list, units, cat);
    let refs: Vec<&str> = items.iter().map(String::as_str).collect();
    let mut sel = list.iter().position(|d| d == dn).unwrap_or(0);
    let changed = bar_dropdown(ui, id, &mut sel, &refs, Some(control::DROPDOWN_W * 0.6));
    if changed {
        *dn = list[sel];
    }
    changed
}

/// Pipe material and DN from the catalog of that material.
pub fn pipe_options(ui: &mut egui::Ui, cat: &Catalog, o: &mut Opts, units: Units) {
    let cur = material(o);
    field_label(ui, cat.t("mep.material"), control::DROPDOWN_W);
    let labels: Vec<String> = PipeMaterial::ALL
        .iter()
        .map(|m| cat.t(&format!("mep.mat.{}", m.name())))
        .collect();
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let mut sel = PipeMaterial::ALL.iter().position(|m| *m == cur).unwrap_or(0);
    let changed = bar_dropdown(ui, "tool_pipe_mat", &mut sel, &refs, None);
    if changed {
        o.pipe_mat = sel as u8;
        o.dn = ddc_model::nearest_dn(PipeMaterial::ALL[sel], o.dn).unwrap_or(o.dn);
    }
    field_label(ui, cat.t("mep_units.dn"), control::DROPDOWN_W * 0.6);
    let dns: Vec<u32> = material(o).sizes().iter().map(|s| s.dn).collect();
    size_dropdown(ui, cat, units, "tool_dn", &mut o.dn, &dns);
}

#[cfg(test)]
mod tests {
    use super::*;

    const NBSP: char = '\u{a0}';

    #[test]
    fn size_items_follow_the_unit_setting_and_language() {
        let en = Catalog::new(Lang::En);
        let ru = Catalog::new(Lang::Ru);
        assert_eq!(size_items(&[16, 20], Units::Metric, &en), vec![format!("16{NBSP}mm"), format!("20{NBSP}mm")]);
        assert_eq!(size_label(20, Units::Metric, &ru), format!("20{NBSP}{}", ru.t("mep_units.unit.mm")));
        assert_ne!(ru.t("mep_units.unit.mm"), en.t("mep_units.unit.mm"));
        assert_eq!(size_label(25, Units::Imperial, &en), format!("0.98{NBSP}in"));
        assert_eq!(size_label(25, Units::Imperial, &ru), format!("0,98{NBSP}{}", ru.t("mep_units.unit.in")));
    }

    #[test]
    fn size_list_order_is_stable() {
        let en = Catalog::new(Lang::En);
        let a = size_items(&CONDUIT_DN, Units::Metric, &en);
        assert_eq!(a, size_items(&CONDUIT_DN, Units::Metric, &en));
        assert_eq!(a.len(), CONDUIT_DN.len());
        assert!(CONDUIT_DN.windows(2).all(|w| w[0] < w[1]));
        assert!(a[0].starts_with("16") && a[6].starts_with("63"));
    }

    #[test]
    fn the_bar_texts_come_from_the_catalog_in_both_languages() {
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            for k in [
                "mep_units.unit.mm",
                "mep_units.unit.in",
                "mep_units.dn",
                "mep_units.diameter",
                "mep_units.system.new",
                "mep_units.system.none",
            ] {
                assert_ne!(c.t(k), k, "{k} missing in {lang:?}");
            }
        }
    }

    #[test]
    fn the_bar_holds_no_raw_widget_and_no_literal_text() {
        let src = include_str!("mep.rs");
        let code = &src[..src.find("#[cfg(test)]").expect("test module")];
        assert!(!code.contains("egui::ComboBox"), "raw ComboBox in the bar");
        assert!(!code.contains("ComboBox::"), "raw ComboBox in the bar");
        for pat in [".width(", "ui.label(\""] {
            assert!(!code.contains(pat), "{pat} in tools/mep.rs");
        }
        assert!(code.contains("widgets::field::dropdown"));
        assert!(code.contains("control::DROPDOWN_W"));
    }

    #[test]
    fn material_index_falls_back_to_steel() {
        let mut o = Opts::for_kind(Kind::Pipe, 3.0);
        assert_eq!(material(&o), PipeMaterial::Steel);
        o.pipe_mat = 1;
        assert_eq!(material(&o), PipeMaterial::Copper);
        o.pipe_mat = 200;
        assert_eq!(material(&o), PipeMaterial::Steel);
    }

    #[test]
    fn duct_shape_follows_the_round_switch() {
        let mut o = Opts::for_kind(Kind::Duct, 3.0);
        assert!(matches!(duct_shape(&o), MepShape::Rect { .. }));
        o.duct_round = true;
        o.width = 0.315;
        assert_eq!(duct_shape(&o), MepShape::Round { d: 0.315 });
    }
}
