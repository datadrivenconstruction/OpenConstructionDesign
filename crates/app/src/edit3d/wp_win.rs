//! `mgr.workplane`: the "Work plane" window. Shows where 3D points are placed now and sets the
//! plane on a level with an offset, on a reference plane, on a picked face or through three
//! points. The plane is view state: no model edit, no undo step.

use super::plane::{Source, WorkPlane, World};
use super::{Edit3d, Tool};
use crate::editor::{Editor, OpFn};
use ddc_render::glam::DVec3;
use std::sync::Arc;
use crate::shell::EditorApp;
use crate::ui_kit as kit;
use ddc_ui::theme::space;
use ddc_ui::widgets;
use ddc_ui::Catalog;
use std::cell::RefCell;

#[derive(Default)]
pub struct Win {
    pub open: bool,
    /// Index into the level list and the offset above it, m.
    pub level: usize,
    pub offset: f64,
    /// Index into the reference planes.
    pub rp: usize,
    /// Index into the named planes of the project.
    pub named: usize,
    /// Name the current plane is saved under.
    pub name: String,
}

thread_local! {
    static WIN: RefCell<Win> = RefCell::new(Win::default());
}

/// What the window asks for this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Act {
    Level,
    Ref,
    Face,
    Points,
    Reset,
    /// Work on the chosen named plane.
    Named,
    /// Save the current plane under `Win::name` (same name: replaced).
    Save,
    /// Delete the chosen named plane.
    Delete,
}

/// Named plane `id` of the project.
fn named(ed: &Editor, id: i64) -> Option<ddc_model::NamedPlane> {
    ed.model.work_planes().ok()?.into_iter().find(|p| p.id == id)
}

/// One plane of the named list: the saved planes, then every level (horizontal) and every
/// straight grid axis (vertical), read from the model so they follow its edits.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub source: Source,
    pub name: String,
    pub plane: World,
}

impl Entry {
    /// Saved by the user (the levels and grids are the model's own).
    pub fn saved(&self) -> bool {
        matches!(self.source, Source::Named(_))
    }
}

fn level_plane(z: f64) -> World {
    World {
        origin: DVec3::new(0.0, 0.0, z),
        u: DVec3::X,
        n: DVec3::Z,
    }
}

fn grid_plane(g: &ddc_model::Grid) -> Option<World> {
    let (a, b) = (g.spec.a, g.spec.b);
    let u = DVec3::new(b.0 - a.0, b.1 - a.1, 0.0);
    if g.spec.bulge != 0.0 || u.length() < 1e-9 {
        return None;
    }
    let u = u.normalize();
    Some(World {
        origin: DVec3::new(a.0, a.1, 0.0),
        u,
        n: u.cross(DVec3::Z),
    })
}

fn saved_plane(p: &ddc_model::NamedPlane) -> World {
    World {
        origin: DVec3::from_array(p.origin),
        u: DVec3::from_array(p.u),
        n: DVec3::from_array(p.n),
    }
}

/// The named list as the model stands now.
pub fn entries(ed: &Editor, cat: &Catalog) -> Vec<Entry> {
    let mut v: Vec<Entry> = ed
        .model
        .work_planes()
        .unwrap_or_default()
        .iter()
        .map(|p| Entry { source: Source::Named(p.id), name: p.name.clone(), plane: saved_plane(p) })
        .collect();
    v.extend(ed.levels().into_iter().map(|(id, name, z)| Entry {
        source: Source::Level(id, 0.0),
        name: cat.tf("view_cube.wp.level_plane", &[("level", &name)]),
        plane: level_plane(z),
    }));
    for g in ed.model.grids().unwrap_or_default() {
        if let Some(plane) = grid_plane(&g) {
            let name = cat.tf("view_cube.wp.grid", &[("name", &g.name)]);
            v.push(Entry { source: Source::Grid(g.id), name, plane });
        }
    }
    v
}

/// The plane `source` names in the model now: a moved grid axis, a raised level or a replaced
/// saved plane give the new place; `None` for a face, a reference plane or a plane gone.
pub fn resolve(ed: &Editor, source: Source) -> Option<World> {
    match source {
        Source::Named(id) => named(ed, id).map(|p| saved_plane(&p)),
        Source::Level(id, off) => {
            let z = ed.levels().into_iter().find(|l| l.0 == id)?.2;
            Some(level_plane(z + f64::from(off)))
        }
        Source::Grid(id) => ed.model.grids().ok()?.iter().find(|g| g.id == id).and_then(grid_plane),
        Source::Face | Source::Ref(_) => None,
    }
}

/// Short name of the current plane for the trace labels in plan and section views.
pub fn label(e: &Edit3d, ed: &Editor, cat: &Catalog) -> String {
    match e.plane.source {
        Source::Named(id) => named(ed, id).map_or_else(|| cat.t("view_cube.wp.face"), |p| p.name),
        Source::Grid(id) => match ed.model.grids().unwrap_or_default().into_iter().find(|g| g.id == id) {
            Some(g) => cat.tf("view_cube.wp.grid", &[("name", &g.name)]),
            None => cat.t("view_cube.wp.face"),
        },
        Source::Ref(id) => ref_name(ed, id, cat),
        Source::Level(..) if e.plane.is_horizontal() => {
            let (id, off) = level_of(e, ed);
            let name = ed.levels().into_iter().find(|l| l.0 == id).map(|l| l.1).unwrap_or_default();
            let off = crate::units::display_length(off, ed.tool.units);
            cat.tf("view_cube.wp.level", &[("level", &name), ("offset", &off)])
        }
        _ => cat.t("view_cube.wp.face"),
    }
}

/// Level the plane lies on, else the active one; offset of the plane above that level.
fn level_of(e: &Edit3d, ed: &Editor) -> (i64, f64) {
    // a plane nobody set follows the active level; its stored level id may predate the model
    if !e.plane_user {
        return (ed.level, 0.0);
    }
    match e.plane.source {
        Source::Level(id, _) if e.plane.is_horizontal() => {
            let lv = ed.levels();
            let el = lv.iter().find(|l| l.0 == id).map_or(ed.elevation, |l| l.2);
            (id, ed.elevation + f64::from(e.plane.origin.z) - el)
        }
        _ => (ed.level, 0.0),
    }
}

/// Plain sentence of the current plane.
pub fn describe(e: &Edit3d, ed: &Editor, cat: &Catalog) -> String {
    match e.plane.source {
        Source::Level(..) if e.plane.is_horizontal() || !e.plane_user => {
            let (id, off) = level_of(e, ed);
            let name = ed.levels().into_iter().find(|l| l.0 == id).map(|l| l.1).unwrap_or_default();
            if off.abs() < 1e-6 {
                return cat.tf("cmd_buttons.wpw.now.on_level", &[("level", &name)]);
            }
            let off = crate::units::display_length(off, ed.tool.units);
            cat.tf("wpw.now.level", &[("level", &name), ("offset", &off)])
        }
        Source::Ref(id) => {
            cat.tf("wpw.now.ref", &[("name", &ref_name(ed, id, cat))])
        }
        Source::Named(_) | Source::Grid(_) => cat.tf("view_cube.wp.now_named", &[("name", &label(e, ed, cat))]),
        _ => cat.t("wpw.now.face"),
    }
}

/// Name of reference plane `id`; an unnamed one is called by its id.
fn ref_name(ed: &Editor, id: i64, cat: &Catalog) -> String {
    match ed.model.ref_plane(id) {
        Ok((n, ..)) if !n.trim().is_empty() => n,
        _ => cat.tf("wpw.ref_n", &[("n", &id.to_string())]),
    }
}

/// Applies `act` to the 3D editor; `false` with the status said when it cannot.
pub fn apply(a: &mut EditorApp, w: &Win, act: Act) -> bool {
    let ru = a.sh.cat.lang == ddc_ui::Lang::Ru;
    match act {
        Act::Level => {
            let lv = a.ed.levels();
            let Some(l) = lv.get(w.level) else {
                return false;
            };
            let e = &mut a.view.e3;
            let mut p = WorkPlane::level(l.0, w.offset as f32);
            // scene z counts from the active level
            p.origin.z = (l.2 + w.offset - a.ed.elevation) as f32;
            e.plane = p;
            // the active level at offset 0 is the default plane: it follows level changes
            e.plane_user = !(l.0 == a.ed.level && w.offset == 0.0);
            e.plane_elev = a.ed.elevation;
        }
        Act::Reset => {
            let e = &mut a.view.e3;
            e.plane_user = false;
            e.plane = WorkPlane::level(a.ed.level, 0.0);
        }
        Act::Ref => {
            let rps = a.ed.model.ref_planes().unwrap_or_default();
            let Some(wp) = rps.get(w.rp).and_then(|r| WorkPlane::vertical(r.0, r.1, r.2)) else {
                a.ed.status = Some(super::tr(ru, "e3d.wp.no_ref"));
                return false;
            };
            let e = &mut a.view.e3;
            e.plane = wp;
            e.plane_user = true;
            e.plane_elev = a.ed.elevation;
        }
        Act::Face | Act::Points => {
            a.view3d = true;
            a.view.e3.set_tool(if act == Act::Face { Tool::Face } else { Tool::Pick3 });
            return true;
        }
        Act::Named => {
            let list = entries(&a.ed, &a.sh.cat);
            let Some(p) = list.get(w.named) else {
                a.ed.status = Some(a.sh.cat.t("view_cube.wp.none"));
                return false;
            };
            a.view.e3.set_world_plane(&p.plane, p.source, a.ed.elevation);
        }
        Act::Save => return save(a, &w.name),
        Act::Delete => {
            let list = entries(&a.ed, &a.sh.cat);
            let Some(Source::Named(id)) = list.get(w.named).map(|p| p.source) else {
                return false;
            };
            let op = OpFn(Arc::new(move |m: &mut ddc_model::Model| {
                m.delete_work_plane(id).map(|_| ())
            }));
            if let Err(e) = a.ed.run_op(op) {
                let e = e.to_string();
                a.ed.status = Some(a.sh.cat.tf("view_cube.wp.failed", &[("e", &e)]));
                return false;
            }
            // the plane stays where it is; it only loses its name
            let e3 = &mut a.view.e3;
            if e3.plane.source == Source::Named(id) {
                e3.plane.source = Source::Face;
            }
            a.ed.status = Some(a.sh.cat.t("view_cube.wp.deleted"));
            return true;
        }
    }
    a.view3d = true;
    a.ed.status = Some(super::tr(ru, "e3d.wp.set"));
    true
}

/// Saves the current plane as named plane `name` (one undo step) and works on it.
pub fn save(a: &mut EditorApp, name: &str) -> bool {
    let e = &a.view.e3;
    let pl = e.world_plane().unwrap_or_else(|| e.plane.to_world(a.ed.elevation));
    let (o, u, n) = (pl.origin.to_array(), pl.u.to_array(), pl.n.to_array());
    let Some(p) = ddc_model::NamedPlane::new(0, name, o, u, n) else {
        a.ed.status = Some(a.sh.cat.t("view_cube.wp.need_name"));
        return false;
    };
    let key = p.name.to_lowercase();
    let op = OpFn(Arc::new(move |m: &mut ddc_model::Model| m.save_work_plane(&p).map(|_| ())));
    if let Err(e) = a.ed.run_op(op) {
        let e = e.to_string();
        a.ed.status = Some(a.sh.cat.tf("view_cube.wp.failed", &[("e", &e)]));
        return false;
    }
    let list = a.ed.model.work_planes().unwrap_or_default();
    let Some(p) = list.iter().find(|p| p.name.to_lowercase() == key) else {
        return false;
    };
    a.view.e3.set_world_plane(&pl, Source::Named(p.id), a.ed.elevation);
    a.ed.status = Some(a.sh.cat.tf("view_cube.wp.saved", &[("name", &p.name)]));
    true
}

/// Window body (apart from the frame, for the layout probes). Returns the action picked.
pub fn body(ui: &mut egui::Ui, w: &mut Win, e: &Edit3d, ed: &Editor, cat: &Catalog) -> Option<Act> {
    let th = ddc_ui::theme::of(ui.ctx());
    let ru = cat.lang == ddc_ui::Lang::Ru;
    let mut act = None;
    ui.add(egui::Label::new(cat.t("wpw.lead")).wrap());
    ui.add_space(space::XS);
    kit::props(ui, "wpw.now", |ui| kit::kv(ui, &cat.t("wpw.now"), describe(e, ed, cat)));
    ui.add_space(space::SM);
    kit::section_title(ui, cat.t("wpw.on_level"));
    let lv = ed.levels();
    let names: Vec<&str> = lv.iter().map(|l| l.1.as_str()).collect();
    w.level = w.level.min(names.len().saturating_sub(1));
    let lbl = cat.t("wpw.level");
    kit::labeled_row(ui, &lbl, |ui| {
        widgets::dropdown(ui, &th, "wpw.level", &mut w.level, &names, None);
    });
    let off = cat.t("wpw.offset");
    kit::labeled_row_id(ui, &off, |ui, lid| {
        let id = egui::Id::new("wpw.offset");
        kit::len_field(ui, &th, id, &mut w.offset, ed.tool.units, -1000.0, 1000.0, ru, lid, &off);
    });
    if kit::secondary_button(ui, cat.t("wpw.set_level")).clicked() {
        act = Some(Act::Level);
    }
    ui.add_space(space::SM);
    kit::section_title(ui, cat.t("wpw.on_ref"));
    let rps = ed.model.ref_planes().unwrap_or_default();
    if rps.is_empty() {
        kit::empty_note(ui, cat.t("e3d.wp.no_ref"), Some(&cat.t("wpw.no_ref_hint")));
    } else {
        let items: Vec<String> = rps.iter().map(|r| ref_name(ed, r.0, cat)).collect();
        let items: Vec<&str> = items.iter().map(String::as_str).collect();
        w.rp = w.rp.min(items.len() - 1);
        kit::labeled_row(ui, &cat.t("wpw.ref"), |ui| {
            widgets::dropdown(ui, &th, "wpw.ref", &mut w.rp, &items, None);
        });
        if kit::secondary_button(ui, cat.t("wpw.set_ref")).clicked() {
            act = Some(Act::Ref);
        }
    }
    ui.add_space(space::SM);
    kit::section_title(ui, cat.t("view_cube.wp.named"));
    let list = entries(ed, cat);
    if list.is_empty() {
        kit::empty_note(ui, cat.t("view_cube.wp.none"), Some(&cat.t("view_cube.wp.none_hint")));
    } else {
        let items: Vec<&str> = list.iter().map(|p| p.name.as_str()).collect();
        w.named = w.named.min(items.len() - 1);
        kit::labeled_row(ui, &cat.t("view_cube.wp.plane"), |ui| {
            widgets::dropdown(ui, &th, "wpw.named", &mut w.named, &items, None);
        });
        // levels and grid axes come from the model: only saved planes are deleted here
        let saved = list[w.named].saved();
        ui.horizontal_wrapped(|ui| {
            if kit::secondary_button(ui, cat.t("view_cube.wp.use")).clicked() {
                act = Some(Act::Named);
            }
            if kit::danger_button(ui, cat.t("view_cube.wp.delete"), saved).clicked() {
                act = Some(Act::Delete);
            }
        });
    }
    let name = cat.t("view_cube.wp.name");
    let hint = cat.t("view_cube.wp.name_hint");
    kit::labeled_row_id(ui, &name, |ui, lid| {
        widgets::TextField::new(&mut w.name)
            .id_salt("wpw.name")
            .labelled_by(lid, &name)
            .hint(&hint)
            .show(ui, &th);
    });
    let can = !w.name.trim().is_empty();
    if ui.add_enabled_ui(can, |ui| kit::secondary_button(ui, cat.t("view_cube.wp.save"))).inner.clicked() {
        act = Some(Act::Save);
    }
    ui.add_space(space::SM);
    kit::section_title(ui, cat.t("wpw.pick"));
    ui.horizontal_wrapped(|ui| {
        if kit::secondary_button(ui, cat.t("wpw.face")).clicked() {
            act = Some(Act::Face);
        }
        if kit::secondary_button(ui, cat.t("wpw.points")).clicked() {
            act = Some(Act::Points);
        }
    });
    kit::dialog_footer(ui, |ui| {
        if kit::secondary_button(ui, cat.t("wpw.reset")).clicked() {
            act = Some(Act::Reset);
        }
    });
    act
}

/// Opens the window on the level and offset of the current plane.
fn run(a: &mut EditorApp) {
    let (id, off) = level_of(&a.view.e3, &a.ed);
    let level = a.ed.levels().iter().position(|l| l.0 == id).unwrap_or(0);
    WIN.with(|w| *w.borrow_mut() = Win { open: true, level, offset: off, rp: 0, ..Default::default() });
}

pub fn open_for_layout(a: &mut EditorApp) {
    run(a);
}

pub fn is_open() -> bool {
    WIN.with(|w| w.borrow().open)
}

/// Called every frame.
pub fn frame(a: &mut EditorApp, ctx: &egui::Context) {
    let mut w = WIN.with(|w| std::mem::take(&mut *w.borrow_mut()));
    if !w.open {
        return;
    }
    let mut open = true;
    let mut act = None;
    let title = a.sh.cat.t("cmd.mgr.workplane");
    kit::Dialog::new("workplane", title, kit::DialogSize::Tool)
        .open(&mut open)
        .scroll_body()
        .show(ctx, |ui| act = body(ui, &mut w, &a.view.e3, &a.ed, &a.sh.cat));
    // picking a face or points happens in the 3D view: the window steps aside
    if let Some(x) = act {
        if apply(a, &w, x) && matches!(x, Act::Face | Act::Points) {
            open = false;
        }
    }
    w.open = open;
    WIN.with(|s| *s.borrow_mut() = w);
}

pub fn register(a: &mut EditorApp) {
    a.register_command("mgr.workplane", run);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(lang: ddc_ui::Lang) -> EditorApp {
        crate::layout_contract::app_in(lang)
    }

    #[test]
    fn level_with_offset_lies_above_that_level_and_follows_nothing() {
        let mut a = app(ddc_ui::Lang::En);
        let l0 = a.ed.level;
        a.ed.add_level("Upper", 3.0).unwrap();
        a.ed.set_level(l0);
        let up = a.ed.levels().iter().position(|l| l.1 == "Upper").unwrap();
        let w = Win { open: true, level: up, offset: 1.2, rp: 0, ..Default::default() };
        assert!(apply(&mut a, &w, Act::Level));
        let e = &a.view.e3;
        assert!(e.plane.is_horizontal());
        assert!((e.plane.z() - 4.2).abs() < 1e-5, "{}", e.plane.z());
        assert!(e.plane_user);
        let cat = Catalog::new(ddc_ui::Lang::En);
        let said = describe(&a.view.e3, &a.ed, &cat);
        assert!(said.contains("Upper"), "{said}");
        assert!(a.view3d);
        assert!(apply(&mut a, &w, Act::Reset));
        assert!(!a.view.e3.plane_user);
        assert_eq!(a.view.e3.plane, WorkPlane::level(l0, 0.0));
    }

    #[test]
    fn active_level_at_zero_is_the_default_plane() {
        let mut a = app(ddc_ui::Lang::En);
        let i = a.ed.levels().iter().position(|l| l.0 == a.ed.level).unwrap();
        let w = Win { open: true, level: i, offset: 0.0, rp: 0, ..Default::default() };
        assert!(apply(&mut a, &w, Act::Level));
        assert!(!a.view.e3.plane_user);
    }

    #[test]
    fn reference_plane_is_vertical_and_missing_one_is_said() {
        let mut a = app(ddc_ui::Lang::En);
        let w = Win { open: true, level: 0, offset: 0.0, rp: 0, ..Default::default() };
        assert!(!apply(&mut a, &w, Act::Ref));
        assert!(a.ed.status.clone().unwrap_or_default().contains("reference"));
        a.ed.model.add_ref_plane("Axis K", (0.0, 0.0), (0.0, 5.0)).unwrap();
        a.ed.reload().unwrap();
        assert!(apply(&mut a, &w, Act::Ref));
        let p = a.view.e3.plane;
        assert!(!p.is_horizontal());
        assert!(matches!(p.source, Source::Ref(_)));
        let cat = Catalog::new(ddc_ui::Lang::Ru);
        assert!(describe(&a.view.e3, &a.ed, &cat).contains("Axis K"));
    }

    #[test]
    fn face_and_points_hand_over_to_the_3d_tools() {
        let mut a = app(ddc_ui::Lang::En);
        let w = Win::default();
        assert!(apply(&mut a, &w, Act::Face));
        assert_eq!(a.view.e3.tool, Tool::Face);
        assert!(apply(&mut a, &w, Act::Points));
        assert_eq!(a.view.e3.tool, Tool::Pick3);
        assert!(a.view3d);
    }

    #[test]
    fn the_command_opens_the_window_on_the_current_plane() {
        let mut a = app(ddc_ui::Lang::En);
        assert!(a.run("mgr.workplane"));
        assert!(is_open());
        WIN.with(|w| w.borrow_mut().open = false);
    }

    /// An inclined plane through three points, kept in the project under a name: it comes
    /// back on another level in the same place in the model, the save is one undo step, and a
    /// deleted plane keeps working without its name.
    #[test]
    fn named_plane_round_trips_through_the_project_and_undo() {
        let mut a = app(ddc_ui::Lang::En);
        let l0 = a.ed.level;
        a.ed.add_level("Upper", 3.0).unwrap();
        a.ed.set_level(l0);
        let p0 = DVec3::new(0.0, 0.0, 1.0);
        let n = DVec3::new(0.0, -1.0, 1.0).normalize();
        let pl = World { origin: p0, u: DVec3::X, n };
        a.view.e3.set_world_plane(&pl, Source::Face, a.ed.elevation);
        let w = Win { name: String::new(), ..Default::default() };
        assert!(!apply(&mut a, &w, Act::Save), "a name is needed");
        let before = a.ed.undo_len();
        let w = Win { name: " Ramp ".into(), ..Default::default() };
        assert!(apply(&mut a, &w, Act::Save));
        assert_eq!(a.ed.undo_len(), before + 1, "one undo step");
        let saved = a.ed.model.work_planes().unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].name, "Ramp");
        assert!(matches!(a.view.e3.plane.source, Source::Named(id) if id == saved[0].id));
        let cat = Catalog::new(ddc_ui::Lang::En);
        assert_eq!(label(&a.view.e3, &a.ed, &cat), "Ramp");
        // on the upper level the plane is the same plane of the model
        a.view.e3.plane_user = false;
        let up = a.ed.levels().into_iter().find(|l| l.1 == "Upper").unwrap().0;
        a.ed.set_level(up);
        assert!(apply(&mut a, &Win::default(), Act::Named));
        let back = a.view.e3.world_plane().unwrap();
        assert!((back.origin - p0).length() < 1e-5, "{:?}", back.origin);
        assert!((back.n - n).length() < 1e-5);
        assert!((f64::from(a.view.e3.plane.origin.z) - (1.0 - 3.0)).abs() < 1e-5, "scene z from Upper");
        // delete: one undo step, the plane stays and loses its name
        assert!(apply(&mut a, &Win::default(), Act::Delete));
        assert!(a.ed.model.work_planes().unwrap().is_empty());
        assert_eq!(a.view.e3.plane.source, Source::Face);
        assert!(a.view.e3.world_plane().is_some());
        a.ed.undo();
        assert_eq!(a.ed.model.work_planes().unwrap().len(), 1, "delete undone");
        a.ed.undo();
        assert!(a.ed.model.work_planes().unwrap().is_empty(), "save undone");
        // the list now starts with the levels, which are the model's and are not deleted here
        assert!(!apply(&mut a, &Win::default(), Act::Delete));
        assert!(apply(&mut a, &Win::default(), Act::Named));
        assert!(matches!(a.view.e3.plane.source, Source::Level(id, _) if id == l0));
    }

    /// Levels and straight grid axes are in the named list by themselves, and a plane taken
    /// from one moves when the level is raised or the axis is moved (one undo brings it back).
    #[test]
    fn levels_and_grid_axes_are_live_named_planes() {
        let mut a = app(ddc_ui::Lang::En);
        let l0 = a.ed.level;
        let spec = ddc_model::GridSpec {
            name: Some("B".into()),
            a: (2.0, 0.0),
            b: (2.0, 10.0),
            bulge: 0.0,
            base_level: None,
            top_level: None,
            bubble_start: true,
            bubble_end: false,
        };
        let s = spec.clone();
        a.ed.run_op(OpFn(Arc::new(move |m: &mut ddc_model::Model| m.add_grid(&s).map(|_| ())))).unwrap();
        let arc = ddc_model::GridSpec { name: Some("R".into()), bulge: 0.4, ..spec.clone() };
        a.ed.run_op(OpFn(Arc::new(move |m: &mut ddc_model::Model| m.add_grid(&arc).map(|_| ())))).unwrap();
        let cat = Catalog::new(ddc_ui::Lang::En);
        let list = entries(&a.ed, &cat);
        let names: Vec<&str> = list.iter().map(|e| e.name.as_str()).collect();
        let lname = a.ed.levels()[0].1.clone();
        assert!(names.contains(&format!("Level {lname}").as_str()), "{names:?}");
        assert!(names.contains(&"Grid B") && !names.contains(&"Grid R"), "an arc has no plane: {names:?}");
        let gi = names.iter().position(|n| *n == "Grid B").unwrap();
        let g = list[gi].plane;
        assert!(g.n.z.abs() < 1e-9 && (g.n.x.abs() - 1.0).abs() < 1e-9, "vertical along the axis");
        // work on the axis, then move it 3 m east: the plane and its plan trace follow
        let w = Win { named: gi, ..Default::default() };
        assert!(apply(&mut a, &w, Act::Named));
        crate::wp_views::sync(&mut a);
        assert_eq!(a.ed.work_plane.as_ref().unwrap().name, "Grid B");
        let id = a.ed.model.grids().unwrap().iter().find(|g| g.name == "B").unwrap().id;
        let moved = ddc_model::GridSpec { a: (5.0, 0.0), b: (5.0, 10.0), ..spec };
        a.ed.run_op(OpFn(Arc::new(move |m: &mut ddc_model::Model| m.update_grid(id, &moved)))).unwrap();
        crate::wp_views::sync(&mut a);
        let x = |a: &EditorApp| a.view.e3.world_plane().unwrap().origin.x;
        assert!((x(&a) - 5.0).abs() < 1e-9, "{}", x(&a));
        let tr = crate::wp_views::plan_segment(&a.ed, ddc_geom::v2(-20.0, -20.0), ddc_geom::v2(20.0, 20.0)).unwrap();
        assert!((tr.0.x - 5.0).abs() < 1e-6 && (tr.1.x - 5.0).abs() < 1e-6, "{tr:?}");
        a.ed.undo();
        crate::wp_views::sync(&mut a);
        assert!((x(&a) - 2.0).abs() < 1e-9, "undo moves it back");
        // a plane 1 m over an upper level rises with that level
        let up = a.ed.add_level("Upper", 3.0).unwrap();
        a.ed.set_level(l0);
        let ui = a.ed.levels().iter().position(|l| l.0 == up).unwrap();
        let w = Win { level: ui, offset: 1.0, ..Default::default() };
        assert!(apply(&mut a, &w, Act::Level));
        crate::wp_views::sync(&mut a);
        assert!((a.view.e3.world_plane().unwrap().origin.z - 4.0).abs() < 1e-5);
        a.ed.run_op(OpFn(Arc::new(move |m: &mut ddc_model::Model| m.update_level(up, "Upper", 3.5)))).unwrap();
        crate::wp_views::sync(&mut a);
        assert!((a.view.e3.world_plane().unwrap().origin.z - 4.5).abs() < 1e-5);
    }

    #[test]
    fn named_plane_strings_exist_in_both_languages() {
        let (en, ru) = (Catalog::new(ddc_ui::Lang::En), Catalog::new(ddc_ui::Lang::Ru));
        for k in [
            "view_cube.wp.named", "view_cube.wp.none", "view_cube.wp.none_hint", "view_cube.wp.plane",
            "view_cube.wp.use", "view_cube.wp.delete", "view_cube.wp.name", "view_cube.wp.name_hint",
            "view_cube.wp.save", "view_cube.wp.saved", "view_cube.wp.deleted", "view_cube.wp.need_name",
            "view_cube.wp.failed", "view_cube.wp.face", "view_cube.wp.level", "view_cube.wp.now_named",
            "view_cube.wp.level_plane", "view_cube.wp.grid",
            "snap.work_plane",
        ] {
            assert_ne!(en.t(k), k, "{k}");
            assert_ne!(ru.t(k), k, "{k}");
            assert_ne!(en.t(k), ru.t(k), "{k}");
        }
    }
}
