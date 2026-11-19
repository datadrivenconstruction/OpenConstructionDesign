// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Mesh exchange in the editor (docs/design/IO_FORMATS.md): STL/OBJ (and, in builds with
//! `dxf-export`, DXF) export of the whole model or the selection from the glTF export bodies,
//! and reading glTF/STEP/STL/OBJ files on a worker for the open and import paths.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ddc_io::meshx::{MeshSet, Units};
use ddc_ui::widgets;
use eframe::egui;

use crate::editor::Editor;
use crate::jobs::Progress;
use crate::shell::EditorApp;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fmt {
    Stl,
    Obj,
    #[cfg(feature = "dxf-export")]
    Dxf,
    /// Exact solids (docs/IO4.md).
    Step,
    /// 3MF core with the watertight check.
    ThreeMf,
}

impl Fmt {
    pub fn ext(self) -> &'static str {
        match self {
            Fmt::Stl => "stl",
            Fmt::Obj => "obj",
            #[cfg(feature = "dxf-export")]
            Fmt::Dxf => "dxf",
            Fmt::Step => "step",
            Fmt::ThreeMf => "3mf",
        }
    }

    pub fn of_ext(e: &str) -> Option<Self> {
        match e.to_ascii_lowercase().as_str() {
            "stl" => Some(Fmt::Stl),
            "obj" => Some(Fmt::Obj),
            #[cfg(feature = "dxf-export")]
            "dxf" => Some(Fmt::Dxf),
            "step" | "stp" => Some(Fmt::Step),
            "3mf" => Some(Fmt::ThreeMf),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Fmt::Stl => "STL",
            Fmt::Obj => "OBJ",
            #[cfg(feature = "dxf-export")]
            Fmt::Dxf => "DXF",
            Fmt::Step => "STEP",
            Fmt::ThreeMf => "3MF",
        }
    }
}

/// Options only some formats read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Extra {
    pub schema: ddc_io::step::write::Schema,
    pub policy: ddc_io::threemf::Policy,
}

/// What a finished export wrote.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub parts: usize,
    pub triangles: usize,
    /// Bodies that could not be built exactly, one line each.
    pub issues: Vec<String>,
    /// STEP: solids written with exact faces, and the notes of those written as planar faces.
    pub exact: usize,
    pub faceted: Vec<String>,
}

/// Element keys (`<table>:<id>`) of the selection.
pub fn selection_keys(ed: &Editor) -> HashSet<String> {
    ed.selection
        .iter()
        .filter_map(|s| crate::phase_ui::sel_ref(*s))
        .map(|(t, id)| format!("{t}:{id}"))
        .collect()
}

/// The bodies the glTF export builds, flattened; `only`: keep these element keys.
pub fn mesh_set(
    ed: &mut Editor,
    title: &str,
    only: Option<&HashSet<String>>,
) -> Result<(MeshSet, Vec<String>), String> {
    let x = crate::gltf_export::build(ed, title)?;
    let set = MeshSet::from_gltf(&x.gltf);
    let set = match only {
        Some(k) => set.only(k),
        None => set,
    };
    Ok((set, x.issues))
}

/// Writes `path` (OBJ also writes the `.mtl` next to it).
pub fn write(
    ed: &mut Editor,
    title: &str,
    path: &Path,
    fmt: Fmt,
    units: Units,
    only: Option<&HashSet<String>>,
) -> Result<Report, String> {
    write_with(ed, title, path, fmt, units, only, Extra::default())
}

/// [`write`] with the STEP schema and the 3MF watertight policy.
pub fn write_with(
    ed: &mut Editor,
    title: &str,
    path: &Path,
    fmt: Fmt,
    units: Units,
    only: Option<&HashSet<String>>,
    x: Extra,
) -> Result<Report, String> {
    let (set, mut issues) = mesh_set(ed, title, only)?;
    let (mut exact, mut faceted) = (0, vec![]);
    if set.parts.is_empty() {
        return Err(match only {
            Some(_) => "nothing to export: the selection has no 3D body".into(),
            None => "nothing to export: the model has no 3D body".into(),
        });
    }
    match fmt {
        Fmt::Stl => ddc_io::stl::write_file(&set, units, path)?,
        Fmt::Obj => {
            let o = ddc_io::obj::ObjOptions { units, y_up: true };
            ddc_io::obj::write_file(&set, path, o)?
        }
        #[cfg(feature = "dxf-export")]
        Fmt::Dxf => {
            let levels: Vec<(String, f64)> = ed
                .model
                .level_list()
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|l| (l.1, l.2))
                .collect();
            let o = ddc_io::mesh_dxf::DxfOptions::standard(&set, &levels, units);
            ddc_io::mesh_dxf::write_file(&set, &o, path)?;
        }
        Fmt::Step => {
            let w = ddc_io::step::write::WriteOptions {
                schema: x.schema,
                units,
                stamp: String::new(),
            };
            let o = ddc_io::exact::build::Opts::default();
            // `solid:<id>` and `freeform:<id>` parts are rebuilt exactly, the rest are polyhedra
            let r = ddc_io::exact::model::write_step(&ed.model, &set, false, title, &w, &o, path)?;
            exact = r.exact;
            faceted = r.notes;
        }
        Fmt::ThreeMf => {
            let unit = match units {
                Units::Millimetre => ddc_io::threemf::Unit::Millimeter,
                Units::Metre => ddc_io::threemf::Unit::Meter,
            };
            let o = ddc_io::threemf::Options {
                unit,
                policy: x.policy,
                title: title.to_string(),
            };
            let r = ddc_io::threemf::write_file(&set, &o, path)?;
            issues.extend(r.leaky().map(|p| format!("{}: not watertight ({} open, {} bad edges)", p.name, p.open_edges, p.bad_edges)));
        }
    }
    Ok(Report {
        parts: set.parts.len(),
        triangles: set.triangle_count(),
        issues,
        exact,
        faceted,
    })
}

/// The export options dialog.
#[derive(Clone, Debug, PartialEq)]
pub struct Dlg {
    pub fmt: Fmt,
    pub mm: bool,
    pub selection: bool,
    pub extra: Extra,
}

pub fn register(a: &mut EditorApp) {
    a.register_command("data.export_stl", open_stl);
    a.register_command("data.export_obj", open_obj);
    a.register_command("data.export_step", |a| open(a, Fmt::Step));
    a.register_command("data.export_3mf", |a| open(a, Fmt::ThreeMf));
}

fn open_stl(a: &mut EditorApp) {
    open(a, Fmt::Stl);
}

fn open_obj(a: &mut EditorApp) {
    open(a, Fmt::Obj);
}

fn open(a: &mut EditorApp, fmt: Fmt) {
    let selection = !selection_keys(&a.ed).is_empty();
    // STL, STEP and 3MF go to fabrication and printing, which count in millimetres
    a.data.mesh = Some(Dlg {
        fmt,
        mm: matches!(fmt, Fmt::Stl | Fmt::Step | Fmt::ThreeMf),
        selection,
        extra: Extra::default(),
    });
}

/// Opens the dialog the way a command does (layout probes).
pub fn open_for_layout(a: &mut EditorApp, fmt: Fmt) {
    open(a, fmt);
}

pub fn window(a: &mut EditorApp, ctx: &egui::Context) {
    let n_sel = selection_keys(&a.ed).len();
    let (cat, th) = (&a.sh.cat, &a.sh.theme);
    let Some(d) = a.data.mesh.as_mut() else {
        return;
    };
    let (mut open, mut go, mut cancel) = (true, false, false);
    let title = cat.tf("mx.title", &[("fmt", d.fmt.label())]);
    crate::ui_kit::Dialog::new("mx", title, crate::ui_kit::DialogSize::Form)
        .id(egui::Id::new("ddc.mx"))
        .open(&mut open)
        .show(ctx, |ui| {
            crate::ui_kit::section_title(ui, cat.t("mx.scope"));
            let all = cat.t("mx.scope.all");
            let sel = cat.tf("mx.scope.sel", &[("n", &n_sel.to_string())]);
            if n_sel == 0 {
                d.selection = false;
                widgets::radio(ui, th, true, &all);
                ui.add_enabled_ui(false, |ui| widgets::radio(ui, th, false, &sel));
            } else if let Some(i) = widgets::radio_group(ui, th, &[&all, &sel], usize::from(d.selection)) {
                d.selection = i == 1;
            }
            crate::ui_kit::section_title(ui, cat.t("mx.units"));
            let (m, mm) = (cat.t("mx.units.m"), cat.t("mx.units.mm"));
            if let Some(i) = widgets::radio_group(ui, th, &[&m, &mm], usize::from(d.mm)) {
                d.mm = i == 1;
            }
            if d.fmt == Fmt::Step {
                use ddc_io::step::write::Schema;
                crate::ui_kit::section_title(ui, cat.t("mx.schema"));
                let (a214, a242) = (cat.t("mx.schema.ap214"), cat.t("mx.schema.ap242"));
                let cur = usize::from(d.extra.schema == Schema::Ap242);
                if let Some(i) = widgets::radio_group(ui, th, &[&a214, &a242], cur) {
                    d.extra.schema = if i == 1 { Schema::Ap242 } else { Schema::Ap214 };
                }
            }
            if d.fmt == Fmt::ThreeMf {
                use ddc_io::threemf::Policy;
                crate::ui_kit::section_title(ui, cat.t("mx.watertight"));
                let keys = ["mx.watertight.report", "mx.watertight.skip", "mx.watertight.require"];
                let labels: Vec<String> = keys.iter().map(|k| cat.t(k)).collect();
                let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
                const ALL: [Policy; 3] = [Policy::Report, Policy::Skip, Policy::Require];
                let cur = ALL.iter().position(|p| *p == d.extra.policy).unwrap_or(0);
                if let Some(i) = widgets::radio_group(ui, th, &refs, cur) {
                    d.extra.policy = ALL[i];
                }
            }
            crate::ui_kit::dialog_footer(ui, |ui| {
                go = crate::ui_kit::primary_action(ui, cat.t("mx.export"), None);
                cancel = crate::ui_kit::cancel_button(ui, cat.t("dlg.cancel")).clicked();
            });
        });
    if go {
        let d = a.data.mesh.take().expect("dialog state");
        save(a, &d);
    } else if !open || cancel {
        a.data.mesh = None;
    }
}

fn status(a: &mut EditorApp, key: &str, args: &[(&str, &str)]) {
    a.ed.status = Some(a.sh.cat.tf(key, args));
}

fn with_ext(p: PathBuf, ext: &str) -> PathBuf {
    if p.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext)) {
        return p;
    }
    let mut s = p.into_os_string();
    s.push(".");
    s.push(ext);
    s.into()
}

fn save(a: &mut EditorApp, d: &Dlg) {
    let name = a.ed.model.project_info().map(|p| p.name).unwrap_or_default();
    let stem = crate::data_ui::safe_stem(&name);
    let ext = d.fmt.ext();
    let Some(p) = crate::file_pick::Dlg::new()
        .set_title(a.sh.cat.tf("mx.title", &[("fmt", d.fmt.label())]))
        .add_filter(d.fmt.label(), &[ext])
        .set_file_name(format!("{stem}.{ext}"))
        .save_file()
    else {
        return;
    };
    let path = with_ext(p, ext);
    let only = d.selection.then(|| selection_keys(&a.ed));
    let units = if d.mm { Units::Millimetre } else { Units::Metre };
    let title = if name.is_empty() { stem } else { name };
    let fmt = d.fmt.label();
    match write_with(&mut a.ed, &title, &path, d.fmt, units, only.as_ref(), d.extra) {
        Ok(r) => {
            for i in r.issues.iter().chain(&r.faceted) {
                eprintln!("{fmt}: {i}");
            }
            let shown = path.display().to_string();
            if let (true, Some(first)) = (r.issues.is_empty(), r.faceted.first()) {
                let (e, n) = (r.exact.to_string(), r.faceted.len().to_string());
                status(a, "mx.faceted", &[("fmt", fmt), ("path", &shown), ("exact", &e), ("n", &n), ("first", first)]);
            } else if r.issues.is_empty() {
                let (p, t) = (r.parts.to_string(), r.triangles.to_string());
                status(a, "mx.saved", &[("fmt", fmt), ("path", &shown), ("parts", &p), ("tris", &t)]);
            } else {
                let n = r.issues.len().to_string();
                status(a, "mx.partial", &[("fmt", fmt), ("path", &shown), ("n", &n)]);
            }
        }
        Err(e) => status(a, "mx.failed", &[("fmt", fmt), ("error", &e)]),
    }
}

/// A mesh file read on a worker, waiting for the UI thread to write it into the model.
pub type Read = (MeshSet, Vec<String>);

/// True for the extensions the mesh readers take.
pub fn is_mesh(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ddc_io::meshx::READ_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Worker side: read and tessellate.
pub fn read(path: &Path, c: &Arc<Progress>) -> Result<Arc<Read>, String> {
    let r = ddc_io::meshx::read_file(path, Some(c))?;
    if c.is_cancelled() {
        return Err(crate::jobs::CANCELLED.to_string());
    }
    c.finish();
    Ok(Arc::new(r))
}

/// The status line of a finished mesh import; true when there were notes.
pub fn summary(file: &str, st: &ddc_io::meshx::ImportStats, warns: &[String]) -> (String, bool) {
    let (n, t) = (st.meshes.to_string(), st.triangles.to_string());
    match warns.first() {
        None => (crate::tr("mx.imported", &[("file", file), ("n", &n), ("tris", &t)]), false),
        Some(first) => {
            let w = warns.len().to_string();
            let text = crate::tr("mx.imported_notes", &[("file", file), ("n", &n), ("tris", &t), ("w", &w), ("first", first)]);
            (text, true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Sel;
    use crate::wall_tool::{Event, Mode};
    use ddc_geom::v2;

    fn room() -> Editor {
        let mut ed = Editor::new_in_memory().unwrap();
        ed.set_mode(Mode::Rect);
        ed.handle(Event::Press(v2(0.0, 0.0)));
        ed.handle(Event::Release(v2(6.0, 4.0)));
        ed.set_mode(Mode::Select);
        assert_eq!(ed.walls.len(), 4);
        ed
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ddc-mx-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join(name)
    }

    fn same_box(a: ([f64; 3], [f64; 3]), b: ([f64; 3], [f64; 3]), tol: f64) {
        for k in 0..3 {
            assert!((a.0[k] - b.0[k]).abs() < tol && (a.1[k] - b.1[k]).abs() < tol, "{a:?} vs {b:?}");
        }
    }

    /// Export STL/OBJ, read back through the import path: triangles and box equal; OBJ keeps
    /// the element key of every wall as its group.
    #[test]
    fn stl_and_obj_round_trip_through_import() {
        let mut ed = room();
        let (set, _) = mesh_set(&mut ed, "room", None).unwrap();
        assert_eq!(set.parts.len(), 4);
        let want = (set.triangle_count(), set.bbox().unwrap());
        for (fmt, units) in [(Fmt::Stl, Units::Metre), (Fmt::Stl, Units::Millimetre), (Fmt::Obj, Units::Metre)] {
            let p = tmp(&format!("room-{}-{units:?}.{}", fmt.ext(), fmt.ext()));
            let r = write(&mut ed, "room", &p, fmt, units, None).unwrap();
            assert_eq!(r.triangles, want.0);
            let (back, _) = ddc_io::meshx::read_file(&p, None).unwrap();
            assert_eq!(back.triangle_count(), want.0, "{fmt:?}");
            same_box(back.bbox().unwrap(), want.1, 1e-4);
            if fmt == Fmt::Obj {
                let keys: HashSet<&str> = back.parts.iter().map(|p| p.key.as_str()).collect();
                for w in &ed.walls {
                    assert!(keys.contains(format!("wall:{}", w.id).as_str()));
                }
            }
            let mut m = ddc_model::Model::open(":memory:").unwrap();
            let st = ddc_io::meshx::import_into(&mut m, &back, "back", None).unwrap();
            assert_eq!(st.triangles, want.0);
        }
    }

    #[test]
    fn selection_exports_only_the_chosen_wall() {
        let mut ed = room();
        let id = ed.walls[0].id;
        ed.selection = vec![Sel::Wall(id)];
        let keys = selection_keys(&ed);
        let p = tmp("one.stl");
        let r = write(&mut ed, "room", &p, Fmt::Stl, Units::Metre, Some(&keys)).unwrap();
        assert_eq!(r.parts, 1);
        let (all, _) = mesh_set(&mut ed, "room", None).unwrap();
        let one = all.parts.iter().find(|p| p.key == format!("wall:{id}")).unwrap();
        assert_eq!(r.triangles, one.triangles.len());
        ed.selection.clear();
        let none = HashSet::new();
        let e = write(&mut ed, "room", &tmp("none.stl"), Fmt::Stl, Units::Metre, Some(&none)).unwrap_err();
        assert!(e.contains("selection"), "{e}");
    }

    /// STEP keeps the solid's exact faces next to the faceted walls; 3MF holds every body and
    /// finds them all watertight.
    #[test]
    fn step_and_3mf_from_the_editor() {
        use ddc_geom::arc::BulgeRing;
        use ddc_geom::gsolid::{Form, Frame, Path3, Section};
        use ddc_geom::math::v3;
        let mut ed = room();
        let level = ed.model.level_list().unwrap()[0].0;
        let circle = BulgeRing {
            pts: vec![v2(0.1, 0.0), v2(-0.1, 0.0)],
            bulges: vec![1.0, 1.0],
        };
        let form = Form::Sweep {
            profile: Section { outer: circle, holes: vec![] },
            path: Path3 {
                pts: vec![v3(1.0, 1.0, 1.0), v3(3.0, 1.0, 1.0), v3(4.0, 2.0, 1.0)],
                bulges: vec![0.0, std::f64::consts::FRAC_PI_8.tan()],
                smooth: None,
            },
        };
        let spec = ddc_model::SolidSpec {
            level,
            name: "pipe".into(),
            void: false,
            material: String::new(),
            frame: Frame::default(),
            form,
            ifc: None,
        };
        let id = ed.model.add_solid(&spec).unwrap();
        let vol = ed.model.solid_quantities(id).unwrap().unwrap().volume;
        let p = tmp("room.step");
        let x = Extra {
            schema: ddc_io::step::write::Schema::Ap242,
            ..Extra::default()
        };
        let r = write_with(&mut ed, "room", &p, Fmt::Step, Units::Millimetre, None, x).unwrap();
        assert_eq!((r.exact, r.faceted.len()), (1, 0), "{:?}", r.faceted);
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("AP242_MANAGED_MODEL_BASED_3D_ENGINEERING"));
        let (doc, warns) = ddc_io::step::read_exact(text.as_bytes()).unwrap();
        assert!(warns.is_empty(), "{warns:?}");
        assert_eq!(doc.parts.len(), 5, "four walls and the pipe");
        let curved = |q: &&ddc_io::exact::Part| q.body.histogram().contains_key(&ddc_io::exact::FaceKind::Nurbs);
        let pipe = doc.parts.iter().find(curved).expect("a part with B-spline faces");
        let v = ddc_io::exact::volume(&pipe.body).unwrap();
        assert!((v - vol).abs() <= 1e-3 * vol, "{v} vs {vol}");
        let p = tmp("room.3mf");
        let r = write(&mut ed, "room", &p, Fmt::ThreeMf, Units::Millimetre, None).unwrap();
        assert_eq!(r.parts, 5);
        assert!(r.issues.is_empty(), "{:?}", r.issues);
        assert!(std::fs::read(&p).unwrap().starts_with(b"PK"));
    }

    #[test]
    fn summary_names_the_file_and_the_notes() {
        let st = ddc_io::meshx::ImportStats { meshes: 3, triangles: 36, materials: 1, ids: vec![] };
        let (t, warn) = summary("a.step", &st, &[]);
        assert!(t.contains("a.step") && t.contains("36") && !warn, "{t}");
        let (t, warn) = summary("a.step", &st, &["2 faces skipped".into()]);
        assert!(t.contains("2 faces skipped") && warn, "{t}");
    }
}
