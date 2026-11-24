// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Windowless run on a real file: `ddc-app --file <path> [...]`. One pass imports the file,
//! optionally places an underlay, saves and reopens a project, exports a workbook and
//! dry-runs its re-import, renders frames, and prints one JSON report. Also the base for
//! agents that drive the editor: the report schema below is stable (bump `SCHEMA` on a break).

use std::path::{Path, PathBuf};
use web_time::Instant;

use ddc_geom::v2;
use ddc_model::Model;
use serde_json::{json, Value};

use crate::drawing::{read_drawing, Drawings, Origin};
use crate::editor::{Editor, OpFn};

/// Report schema id; keys are only added within one id.
pub const SCHEMA: &str = "ddc-headless-report/1";

pub const HELP: &str = "\
ddc-app --file <path> [options]     run on a real file without a window

  --file <path>         .ifc .ifczip .rvt (bridge) .dwg .dxf (underlay) .ddc (project)
                        .glb .gltf .step .stp .stl .obj (meshes; STL/OBJ units guessed)
  --underlay <path>     also place a .dwg/.dxf underlay on the current level
  --save <out.ddc>      save the project, reopen it, compare counts
  --xlsx               export a workbook, dry-run re-import it (nothing applied)
  --import-xlsx <book>  apply the edits of a workbook exported earlier from this project
                        (open the saved .ddc); a rejected cell or an unconfirmed mass or
                        type change fails the run and applies nothing. New rows get their
                        ids written back into the book, as the window does.
  --xlsx-delete-missing rows removed from the book delete their elements
  --export <out>        write the model; format by extension: .ifc .glb .gltf .xlsx
                        .csv, .pdf (all sheets in one file), .stl (binary), .obj (+ .mtl,
                        a group per element key). Repeatable.
  --mesh-units m|mm     units of .stl/.obj exports (default m)
  --ifc-schema <s>      IFC2X3, IFC4 or IFC4X3 for .ifc exports; default: the schema of an
                        IFC input, IFC4X3 for any other input
  --export-sheets <dir> one PDF per sheet into <dir>
  --screenshot <out.png> render the view canvas; --view plan|3d|section|both|all (default
                        plan; both also writes <out>_3d.png, all also <out>_3d.png and
                        <out>_section.png), --size WxH (default 1600x1000).
                        plan: the ground floor fitted; 3d: perspective from the south-west
                        on white; section: vertical cut along the long axis at mid-depth
  --level <n|name>      plan level: storey from the ground floor (0, 1, -1…) or a name
  --press <command-id>  (windowed --screenshot, not with --file) run a command, e.g.
                        file.import, before the capture; unknown id exits 1. Native file
                        dialogs never open in screenshot mode: pickers return nothing, the
                        in-app window shows. Use --file with --export for files.
  --press-frames <N>    frames rendered after the press, 1..=60 (default 8)
  --no-recovery         (any start, also env DDC_NO_RECOVERY=1) no crash-recovery dialog,
                        no session mark: for scripted runs
  --report-json <out>   write the report there; '-' or absent: stdout
  --help                this text

Steps run in this order: import, underlay, --import-xlsx, --save, --xlsx, --export,
--export-sheets, --screenshot. Outputs never overwrite an input.

Exit code: 0 on success, 1 on any error (the report then has ok=false and error, and the
steps that ran before it). A skipped sheet output (see `skipped`) is not an error.

Report (JSON object, schema \"ddc-headless-report/1\"; null where a step did not run):
  schema, ok, error, file, kind (ifc|rvt|dwg|dxf|ddc|mesh)
  import_ms, open_ms (same value; name shared with --accept), size_bytes, total_ms,
  peak_memory_bytes
  counts      {level, wall_type, wall, opening, slab, curtain_wall, room, grid, column,
               footing, strip_footing, raft, beam, brace, roof, ceiling, stair, ramp, railing, mass, mesh, component,
               mep_run, toposolid, sheet}   all keys always present, integers
  underlay    {file, entities, lines, arcs, texts, closed, fills, inserts, layers}
  save        {path, ms, bytes, reopen_ms, reopen_match (element counts and stored
              drawings), underlays, underlay_entities}
  xlsx       {export_ms, bytes, dry_run_ms, added, changed, deleted, rejected}
  render      {size:[w,h], plan_frame_ms, view3d_frame_ms, section_frame_ms (steady, median,
              readback included), plan_first_ms, view3d_first_ms, section_first_ms (first
              image), gpu_open_ms, since_start_ms, png:[paths], plan_level, section:{name, a,
              b, depth}}
              frame ms = mean of 8 offscreen frames including read-back
  warnings    [{table, id, problem}]  model check findings; never fatal
  import_xlsx {path, ms, added, changed, deleted, rejected, rejections:[{sheet, row,
               column, reason}], applied, reexported}
  exports     [{path, format, ms, bytes, warnings:[text], schema (ifc),
               nodes, meshes (glb/gltf), sheets (pdf)}]
  sheets      {dir, ms, files:[names], report}
  skipped     [{step, why}]  outputs not written and why (a .pdf export or --export-sheets
              of a project without sheets); the other steps still run
";

pub struct FileArgs {
    pub file: PathBuf,
    pub report: Option<PathBuf>,
    pub screenshot: Option<PathBuf>,
    pub view: String,
    pub size: (u32, u32),
    /// `--level`: the plan level of the screenshot (`report_shot::plan_level`).
    pub level: Option<String>,
    pub underlay: Option<PathBuf>,
    pub save: Option<PathBuf>,
    pub xlsx: bool,
    pub import_xlsx: Option<PathBuf>,
    pub xlsx_delete_missing: bool,
    pub exports: Vec<PathBuf>,
    pub ifc_schema: Option<ddc_io::ifc::IfcSchema>,
    pub export_sheets: Option<PathBuf>,
    /// Units of .stl/.obj exports (`--mesh-units m|mm`, metres by default).
    pub mesh_units: ddc_io::meshx::Units,
}

/// Export extensions `--export` takes.
pub const EXPORT_FORMATS: [&str; 8] = ["ifc", "glb", "gltf", "xlsx", "csv", "pdf", "stl", "obj"];

/// `--export` takes `ext`: [`EXPORT_FORMATS`], and `dxf` in builds with `dxf-export`.
fn export_ext_ok(ext: &str) -> bool {
    EXPORT_FORMATS.contains(&ext) || crate::mesh_io::Fmt::of_ext(ext).is_some()
}

/// `--no-recovery` or a set `DDC_NO_RECOVERY` (not "0"): a scripted start.
pub fn no_recovery(argv: &[String]) -> bool {
    argv.iter().any(|s| s == "--no-recovery")
        || std::env::var("DDC_NO_RECOVERY").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// `Ok(None)`: no `--file`, the normal start. `Err`: a flag is malformed.
pub fn parse(argv: &[String]) -> Result<Option<FileArgs>, String> {
    let get = |name: &str| -> Result<Option<String>, String> {
        match argv.iter().position(|s| s == name) {
            None => Ok(None),
            Some(i) => argv
                .get(i + 1)
                .filter(|v| !v.starts_with("--") || v.as_str() == "-")
                .cloned()
                .map(Some)
                .ok_or_else(|| format!("{name} needs a value")),
        }
    };
    let Some(file) = get("--file")? else {
        return Ok(None);
    };
    if argv.iter().any(|s| s == "--press") {
        return Err("--press is for the windowed --screenshot start; with --file use --export".into());
    }
    let mut exports = vec![];
    for (i, s) in argv.iter().enumerate() {
        if s != "--export" {
            continue;
        }
        let v = argv
            .get(i + 1)
            .filter(|v| !v.starts_with("--"))
            .ok_or("--export needs a value")?;
        let p = PathBuf::from(v);
        if !export_ext_ok(&ext(&p)) {
            return Err(format!(
                "--export {v}: expected one of .{}",
                EXPORT_FORMATS.join(" .")
            ));
        }
        exports.push(p);
    }
    let ifc_schema = match get("--ifc-schema")? {
        None => None,
        Some(s) => Some(
            ddc_io::ifc::IfcSchema::from_user(&s)
                .ok_or_else(|| format!("--ifc-schema {s}: expected IFC2X3, IFC4 or IFC4X3"))?,
        ),
    };
    let view = get("--view")?.unwrap_or_else(|| "plan".into());
    if !crate::report_shot::VIEWS.contains(&view.as_str()) {
        return Err(format!("--view {view}: expected plan, 3d, section, both or all"));
    }
    let size = match get("--size")? {
        None => crate::report_shot::SIZE,
        Some(s) => s
            .split_once('x')
            .and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)))
            .filter(|(w, h)| *w > 0 && *h > 0)
            .ok_or_else(|| format!("--size {s}: expected WxH"))?,
    };
    let mesh_units = match get("--mesh-units")? {
        None => ddc_io::meshx::Units::Metre,
        Some(u) => ddc_io::meshx::Units::parse(&u)
            .ok_or_else(|| format!("--mesh-units {u}: expected m or mm"))?,
    };
    let path = |v: Option<String>| v.map(PathBuf::from);
    Ok(Some(FileArgs {
        file: PathBuf::from(file),
        report: path(get("--report-json")?).filter(|p| p.as_os_str() != "-"),
        screenshot: path(get("--screenshot")?),
        view,
        size,
        level: get("--level")?,
        underlay: path(get("--underlay")?),
        save: path(get("--save")?),
        xlsx: argv.iter().any(|s| s == "--xlsx"),
        import_xlsx: path(get("--import-xlsx")?),
        xlsx_delete_missing: argv.iter().any(|s| s == "--xlsx-delete-missing"),
        exports,
        ifc_schema,
        export_sheets: path(get("--export-sheets")?),
        mesh_units,
    }))
}

/// Rows per report category, the same list the command line counts.
pub fn counts(m: &Model) -> Vec<(&'static str, i64)> {
    ddc_model::category_counts(m)
}

fn counts_json(c: &[(&'static str, i64)]) -> Value {
    Value::Object(c.iter().map(|(k, n)| (k.to_string(), json!(n))).collect())
}

fn ms(t: Instant) -> f64 {
    (t.elapsed().as_secs_f64() * 1e5).round() / 100.0
}

/// Peak working set of this process, bytes; `None` where it cannot be asked.
pub fn peak_memory() -> Option<u64> {
    #[cfg(windows)]
    {
        #[repr(C)]
        struct Counters {
            cb: u32,
            page_faults: u32,
            peak_working_set: usize,
            working_set: usize,
            rest: [usize; 6],
        }
        #[link(name = "kernel32")]
        extern "system" {
            fn GetCurrentProcess() -> isize;
            fn K32GetProcessMemoryInfo(h: isize, c: *mut Counters, cb: u32) -> i32;
        }
        let mut c = Counters {
            cb: std::mem::size_of::<Counters>() as u32,
            page_faults: 0,
            peak_working_set: 0,
            working_set: 0,
            rest: [0; 6],
        };
        // SAFETY: `c` is a properly sized, writable PROCESS_MEMORY_COUNTERS.
        let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) };
        (ok != 0).then_some(c.peak_working_set as u64)
    }
    #[cfg(not(windows))]
    {
        let s = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = s.lines().find(|l| l.starts_with("VmHWM:"))?;
        let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kb * 1024)
    }
}

fn ext(p: &Path) -> String {
    p.extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// True when both paths name the same existing file, or the same path text.
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

fn is_drawing(e: &str) -> bool {
    e == "dwg" || e == "dxf"
}

/// State handed to the render step, which needs a graphics device and lives in `main`.
pub struct Loaded {
    pub ed: Editor,
    pub drawings: Drawings,
}

fn place_underlay(dr: &mut Drawings, ed: &Editor, path: &Path) -> Result<Value, String> {
    let doc = read_drawing(path)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    dr.add(
        &doc,
        &name,
        Some(path.to_path_buf()),
        ed.level,
        None,
        Origin::MinToZero,
        v2(0.0, 0.0),
    );
    let d = dr.list.last().ok_or("underlay was not placed")?;
    Ok(underlay_json(std::slice::from_ref(d), name))
}

/// Primitive counts of `ds` summed; the report's `underlay` object.
fn underlay_json(ds: &[crate::drawing::Drawing], file: String) -> Value {
    let (mut l, mut a, mut t, mut c, mut f, mut i, mut layers) = (0, 0, 0, 0, 0, 0, 0);
    for d in ds {
        let u = &d.u;
        l += u.lines.len();
        a += u.arcs.len();
        t += u.texts.len();
        c += u.closed.len();
        f += u.fills.len();
        i += u.inserts.len();
        layers += d.layers.len();
    }
    json!({
        "file": file, "entities": l + a + t + c + f + i,
        "lines": l, "arcs": a, "texts": t, "closed": c, "fills": f, "inserts": i,
        "layers": layers,
    })
}

/// The drawings a project stores, as the window shows them on open; `Null` when it has none.
fn stored_underlays(dr: &mut Drawings, ed: &Editor) -> Result<Value, String> {
    let rows = ed.model.underlays().map_err(|e| e.to_string())?;
    if rows.is_empty() {
        return Ok(Value::Null);
    }
    let n = rows.len();
    let _notes = dr.load_rows(rows);
    if dr.list.len() != n {
        return Err(format!("{} of {n} stored drawings could not be read", n - dr.list.len()));
    }
    let names: Vec<&str> = dr.list.iter().map(|d| d.name.as_str()).collect();
    Ok(underlay_json(&dr.list, names.join(", ")))
}

fn import_ifc(ed: &mut Editor, path: &Path) -> Result<(), String> {
    let p = path.to_path_buf();
    let seeded = ed.level;
    ed.run_op(OpFn(std::sync::Arc::new(move |m: &mut Model| {
        ddc_io::ifc::import_ifc_path(m, &p)
            .map_err(|e| ddc_model::Error::Invalid(e.to_string()))?;
        // the file's own storeys replace the default one, as when opened in the window
        crate::open::build::tidy(m, seeded);
        Ok(())
    })))
    .map_err(|e| e.to_string())?;
    if let Some(l) = crate::editor::imported_level(&ed.model) {
        ed.level = l;
    }
    Ok(())
}

fn import_rvt(ed: &mut Editor, path: &Path) -> Result<(), String> {
    let br = crate::import::Bridges::load();
    let entries = crate::import::entries(&br.list);
    let dir = crate::import::entry_for(&entries, path)
        .and_then(|e| e.dir.clone())
        .ok_or("no trusted bridge module for .rvt")?;
    let o = ddc_io::bridge::RunOptions::default();
    let frames = ddc_io::bridge::collect_trusted(&dir, &br.trust, path, &o, &mut |_| {})
        .map_err(|e| e.to_string())?;
    ed.apply_import(path, frames).map(drop)
}

fn open_ddc(ed: &mut Editor, path: &Path) -> Result<(), String> {
    match ed.open_project(path) {
        Ok(()) => Ok(()),
        Err(ddc_model::ProjectError::Locked(_)) => {
            ed.open_project_read_only(path).map_err(|e| e.to_string())
        }
        Err(e) => Err(e.to_string()),
    }
}

fn save_and_reopen(ed: &mut Editor, dr: &mut Drawings, target: &Path) -> Result<Value, String> {
    // placed drawings live in `dr` until written into the model, as the window's save does
    dr.persist(&mut ed.model)?;
    let before = counts(&ed.model);
    let under = ed.model.underlay_totals().map_err(|e| e.to_string())?;
    let t = Instant::now();
    ed.save_project_as(target).map_err(|e| e.to_string())?;
    let save_ms = ms(t);
    let bytes = std::fs::metadata(target).map(|m| m.len()).unwrap_or(0);
    let t = Instant::now();
    let mut again = Editor::new_in_memory().map_err(|e| e.to_string())?;
    open_ddc(&mut again, target)?;
    let reopen_ms = ms(t);
    let under_again = again.model.underlay_totals().map_err(|e| e.to_string())?;
    let same = counts(&again.model) == before
        && under_again == under
        && under.0 >= dr.list.len() as i64;
    Ok(json!({
        "path": target.display().to_string(), "ms": save_ms, "bytes": bytes,
        "reopen_ms": reopen_ms, "reopen_match": same,
        "underlays": under_again.0, "underlay_entities": under_again.1,
    }))
}

fn xlsx_dry_run(ed: &mut Editor) -> Result<Value, String> {
    let book = std::env::temp_dir().join(format!("ddc_headless_{}.xlsx", std::process::id()));
    let t = Instant::now();
    ddc_io::xlsx::export_xlsx(&ed.model, &book)?;
    let export_ms = ms(t);
    let bytes = std::fs::metadata(&book).map(|m| m.len()).unwrap_or(0);
    let o = ddc_io::xlsx::ImportOptions {
        apply: false,
        delete_missing: false,
        new_params: false,
    };
    let t = Instant::now();
    let r = ddc_io::xlsx::import_xlsx_with(&mut ed.model, &book, o);
    let dry_ms = ms(t);
    let _ = std::fs::remove_file(&book);
    let r = r?;
    let c = r.counts();
    Ok(json!({
        "export_ms": export_ms, "bytes": bytes, "dry_run_ms": dry_ms,
        "added": c.added, "changed": c.changed, "deleted": c.deleted,
        "rejected": r.rejected.len(),
    }))
}

/// Applies the edits of `book` (a workbook of this project) as the window's Apply does.
/// Fills `rep["import_xlsx"]` also when it fails.
fn import_book(
    ed: &mut Editor,
    book: &Path,
    delete_missing: bool,
    rep: &mut serde_json::Map<String, Value>,
) -> Result<(), String> {
    use crate::shell::xlsx_win::{apply_preview_in, Applied, XlsxPreview};
    let t = Instant::now();
    let mut pv = XlsxPreview::load(&mut ed.model, book, delete_missing)?;
    pv.rev = ed.content_snap();
    let c = pv.report.counts();
    let rejections: Vec<Value> = pv
        .report
        .rejected
        .iter()
        .map(|r| json!({"sheet": r.sheet, "row": r.row, "column": r.column, "reason": r.reason}))
        .collect();
    let n_rejected = rejections.len();
    let mut v = json!({
        "path": book.display().to_string(), "ms": null,
        "added": c.added, "changed": c.changed, "deleted": c.deleted,
        "rejected": n_rejected, "rejections": rejections, "applied": false, "reexported": false,
    });
    let done = if n_rejected > 0 {
        let first = &pv.report.rejected[0];
        Err(format!(
            "--import-xlsx: {n_rejected} cell(s) rejected, nothing applied; first: {} row {} {}: {}",
            first.sheet, first.row, first.column, first.reason
        ))
    } else {
        match apply_preview_in(ed, &mut pv, ddc_io::xlsx::Units::Metric, crate::xl_text::Lang::En, "en") {
            Ok(Applied::Done { reexported }) => {
                v["applied"] = json!(true);
                v["reexported"] = json!(reexported);
                Ok(())
            }
            Ok(Applied::Stale) => Err("--import-xlsx: the model changed while the book was read".into()),
            Err(e) => Err(format!("--import-xlsx: {e}")),
        }
    };
    v["ms"] = json!(ms(t));
    rep.insert("import_xlsx".into(), v);
    done
}

/// Project name, else the input file stem: the title of a glTF scene.
fn title_of(ed: &Editor, input: &Path) -> String {
    let name = ed.model.project_info().map(|p| p.name).unwrap_or_default();
    if !name.trim().is_empty() {
        return name;
    }
    input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "model".into())
}

/// Writes one `--export` file; its report entry.
fn export_one(
    ed: &mut Editor,
    dr: &Drawings,
    a: &FileArgs,
    out: &Path,
) -> Result<Value, String> {
    let t = Instant::now();
    let fmt = ext(out);
    if let Some(d) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let mut v = json!({"path": out.display().to_string(), "format": fmt});
    let warnings: Vec<String> = match fmt.as_str() {
        "ifc" => {
            use ddc_io::ifc::{IfcOptions, IfcSchema};
            let schema = a
                .ifc_schema
                .or_else(|| IfcSchema::of_file(&a.file))
                .unwrap_or(IfcSchema::Ifc4x3);
            let name = out.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let o = IfcOptions { schema, ..Default::default() };
            let st = ddc_io::ifc::export_ifc_file_with(&ed.model, out, &name, None, &o)?;
            v["schema"] = json!(schema.header_name());
            st.warnings
        }
        "glb" | "gltf" => {
            let title = title_of(ed, &a.file);
            let x = crate::gltf_export::build(ed, &title)?;
            if x.gltf.nodes.is_empty() {
                return Err(format!("--export {}: the model has nothing to show in 3D", out.display()));
            }
            if fmt == "glb" {
                ddc_io::gltf::write_glb_file(&x.gltf, out)?;
            } else {
                let text = ddc_io::gltf::write_gltf(&x.gltf)?;
                std::fs::write(out, text).map_err(|e| format!("{}: {e}", out.display()))?;
            }
            v["nodes"] = json!(x.gltf.nodes.len());
            v["meshes"] = json!(x.gltf.meshes.len());
            x.issues
        }
        "pdf" => {
            let ids: Vec<i64> = ed.model.sheets().map_err(|e| e.to_string())?.iter().map(|s| s.0).collect();
            if ids.is_empty() {
                return Err(format!("--export {}: the project has no sheets", out.display()));
            }
            let p = crate::sheet_view::Print::new(None);
            let done = print_run(ed, &p, &ids, out);
            if !done.clean || done.files == 0 {
                return Err(format!("--export {}: {}", out.display(), done.report));
            }
            v["sheets"] = json!(ids.len());
            vec![]
        }
        "xlsx" => {
            ddc_io::xlsx::export_xlsx(&ed.model, out)?;
            vec![]
        }
        "csv" => {
            let text = ddc_io::table_csv::csv_full(&ed.model)?;
            std::fs::write(out, text).map_err(|e| format!("{}: {e}", out.display()))?;
            vec![]
        }
        e if crate::mesh_io::Fmt::of_ext(e).is_some() => {
            let f = crate::mesh_io::Fmt::of_ext(e).expect("checked");
            let title = title_of(ed, &a.file);
            let r = crate::mesh_io::write(ed, &title, out, f, a.mesh_units, None)
                .map_err(|e| format!("--export {}: {e}", out.display()))?;
            v["elements"] = json!(r.parts);
            v["triangles"] = json!(r.triangles);
            r.issues
        }
        _ => return Err(format!("--export {}: unknown format", out.display())),
    };
    let bytes = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
    if bytes == 0 {
        return Err(format!("--export {}: nothing was written", out.display()));
    }
    v["bytes"] = json!(bytes);
    v["warnings"] = json!(warnings);
    v["ms"] = json!(ms(t));
    Ok(v)
}

/// A sheet print without a window, run to its end.
pub(crate) fn print_run(ed: &Editor, p: &crate::sheet_view::Print, ids: &[i64], target: &Path) -> crate::sheet_view::PrintDone {
    let mut run = crate::sheet_view::PrintRun::start(ed, p, ids, target);
    while !run.step(ed, false) {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    run.finish(false)
}

/// `--export-sheets`: a PDF per sheet into `dir`.
fn export_sheets(ed: &Editor, dir: &Path) -> Result<Value, String> {
    let t = Instant::now();
    let ids: Vec<i64> = ed.model.sheets().map_err(|e| e.to_string())?.iter().map(|s| s.0).collect();
    if ids.is_empty() {
        return Err("--export-sheets: the project has no sheets".into());
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut p = crate::sheet_view::Print::new(None);
    p.separate = true;
    p.pdf = true;
    let done = print_run(ed, &p, &ids, dir);
    if !done.clean || done.files == 0 {
        return Err(format!("--export-sheets: {}", done.report));
    }
    let mut files: Vec<String> = std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.to_ascii_lowercase().ends_with(".pdf"))
        .collect();
    files.sort();
    Ok(json!({"dir": dir.display().to_string(), "ms": ms(t), "files": files, "report": done.report}))
}

/// Everything except rendering. `render` gets the loaded state and returns the `render`
/// object of the report (called only with `--screenshot`). Each step's result goes into `rep`
/// as it ends, so a failure report still shows what ran.
fn execute(
    a: &FileArgs,
    render: &mut dyn FnMut(Loaded, &FileArgs) -> Result<Value, String>,
    rep: &mut serde_json::Map<String, Value>,
) -> Result<(), String> {
    let t0 = Instant::now();
    if !a.file.is_file() {
        return Err(format!("file not found: {}", a.file.display()));
    }
    let e = ext(&a.file);
    let kind = match e.as_str() {
        "ifc" | "ifczip" => "ifc",
        "rvt" => "rvt",
        "dwg" => "dwg",
        "dxf" => "dxf",
        "ddc" => "ddc",
        x if ddc_io::meshx::READ_EXTS.contains(&x) => "mesh",
        _ => return Err(format!("unsupported file type .{e}")),
    };
    rep.insert("kind".into(), json!(kind));
    let mut outs: Vec<(&str, &Path)> = [
        ("--save", &a.save),
        ("--report-json", &a.report),
        ("--screenshot", &a.screenshot),
        ("--export-sheets", &a.export_sheets),
    ]
    .into_iter()
    .filter_map(|(f, p)| Some((f, p.as_deref()?)))
    .collect();
    outs.extend(a.exports.iter().map(|p| ("--export", p.as_path())));
    let mut inputs: Vec<&Path> = vec![&a.file];
    inputs.extend(a.underlay.as_deref());
    inputs.extend(a.import_xlsx.as_deref());
    for (flag, out) in &outs {
        if inputs.iter().any(|i| same_file(out, i)) {
            return Err(format!("{flag} {} must not point at an input file", out.display()));
        }
    }
    for (i, p) in a.exports.iter().enumerate() {
        if a.exports[..i].iter().any(|q| same_file(p, q)) {
            return Err(format!("--export {} is named twice", p.display()));
        }
    }
    let size_bytes = std::fs::metadata(&a.file).map(|m| m.len()).unwrap_or(0);
    rep.insert("size_bytes".into(), json!(size_bytes));
    let mut ed = Editor::new_in_memory().map_err(|x| x.to_string())?;
    let mut dr = Drawings::new();
    let mut underlay = Value::Null;
    let t = Instant::now();
    match kind {
        "ifc" => import_ifc(&mut ed, &a.file)?,
        "rvt" => import_rvt(&mut ed, &a.file)?,
        "mesh" => {
            let (set, notes) = ddc_io::meshx::read_file(&a.file, None)?;
            let name = a.file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            ddc_io::meshx::import_into(&mut ed.model, &set, &name, None)?;
            ed.reload().map_err(|x| x.to_string())?;
            rep.insert("import_notes".into(), json!(notes));
        }
        "ddc" => {
            // a project is opened as a temp copy: opening may touch journal/lock files
            let copy = std::env::temp_dir().join(format!(
                "ddc_headless_in_{}_{}.ddc",
                std::process::id(),
                a.file.file_name().and_then(|n| n.to_str()).unwrap_or("x")
            ));
            std::fs::copy(&a.file, &copy).map_err(|x| format!("copy {}: {x}", a.file.display()))?;
            let r = open_ddc(&mut ed, &copy);
            let _ = std::fs::remove_file(&copy);
            r?;
            underlay = stored_underlays(&mut dr, &ed)?;
        }
        _ => underlay = place_underlay(&mut dr, &ed, &a.file)?,
    }
    let import_ms = ms(t);
    rep.insert("import_ms".into(), json!(import_ms));
    rep.insert("open_ms".into(), json!(import_ms));
    if let Some(u) = &a.underlay {
        if !is_drawing(&ext(u)) {
            return Err(format!("--underlay {}: expected .dwg or .dxf", u.display()));
        }
        underlay = place_underlay(&mut dr, &ed, u)?;
    }
    rep.insert("underlay".into(), underlay);
    // findings of the model check are reported, never fatal: the imported model is still usable
    let warnings: Vec<Value> = ed
        .model
        .check_integrity()
        .map_err(|x| x.to_string())?
        .into_iter()
        .map(|i| json!({ "table": i.table, "id": i.id, "problem": i.problem }))
        .collect();
    rep.insert("warnings".into(), Value::Array(warnings));
    if let Some(book) = &a.import_xlsx {
        import_book(&mut ed, book, a.xlsx_delete_missing, rep)?;
    }
    // counted after the book's edits: the model every later step writes
    rep.insert("counts".into(), counts_json(&counts(&ed.model)));
    if let Some(p) = &a.save {
        rep.insert("save".into(), save_and_reopen(&mut ed, &mut dr, p)?);
    }
    if a.xlsx {
        rep.insert("xlsx".into(), xlsx_dry_run(&mut ed)?);
    }
    // sheet outputs of a project without sheets are skipped with a note, not fatal
    let no_sheets = ed.model.sheets().map_err(|e| e.to_string())?.is_empty();
    let mut skipped = vec![];
    let mut skip = |step: String| {
        eprintln!("note: {step} skipped: the project has no sheets");
        skipped.push(json!({"step": step, "why": "the project has no sheets"}));
    };
    let mut written = vec![];
    for out in &a.exports {
        if no_sheets && ext(out) == "pdf" {
            skip(format!("--export {}", out.display()));
            continue;
        }
        match export_one(&mut ed, &dr, a, out) {
            Ok(v) => written.push(v),
            Err(e) => {
                rep.insert("exports".into(), Value::Array(written));
                return Err(e);
            }
        }
    }
    if !a.exports.is_empty() {
        rep.insert("exports".into(), Value::Array(written));
    }
    if let Some(dir) = &a.export_sheets {
        if no_sheets {
            skip("--export-sheets".into());
        } else {
            rep.insert("sheets".into(), export_sheets(&ed, dir)?);
        }
    }
    rep.insert("skipped".into(), Value::Array(skipped));
    if a.screenshot.is_some() {
        let v = render(Loaded { ed, drawings: dr }, a)?;
        rep.insert("render".into(), v);
    }
    rep.insert("total_ms".into(), json!(ms(t0)));
    Ok(())
}

/// The report before any step ran: every key present, null where a step did not run.
fn blank(a: &FileArgs) -> serde_json::Map<String, Value> {
    let v = json!({
        "schema": SCHEMA, "ok": false, "error": null,
        "file": a.file.display().to_string(), "kind": null,
        "import_ms": null, "open_ms": null, "size_bytes": null,
        "total_ms": null, "peak_memory_bytes": null,
        "counts": null, "underlay": null, "save": null, "xlsx": null, "render": null,
        "warnings": [],
        "import_xlsx": null, "exports": null, "sheets": null, "skipped": [],
    });
    match v {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    }
}

/// A command line `parse` refused: the failure report goes where `--report-json` points
/// (stdout without one), so a script reads ok=false instead of nothing.
pub fn report_parse_error(argv: &[String], err: &str) {
    let at = |f: &str| argv.iter().position(|s| s == f).and_then(|i| argv.get(i + 1));
    let file = at("--file").cloned().unwrap_or_default();
    let v = json!({"schema": SCHEMA, "ok": false, "error": err, "file": file});
    let text = serde_json::to_string_pretty(&v).unwrap_or_default();
    match at("--report-json").filter(|p| p.as_str() != "-" && !p.starts_with("--")) {
        Some(p) if !same_file(Path::new(p), Path::new(&file)) => {
            if let Err(e) = std::fs::write(p, &text) {
                eprintln!("report: {p}: {e}");
            }
        }
        _ => println!("{text}"),
    }
}

fn emit(a: &FileArgs, v: &Value) {
    let text = serde_json::to_string_pretty(v).unwrap_or_default();
    // a report aimed at the input goes to stdout: the input file is never overwritten
    match &a.report {
        Some(p) if !same_file(p, &a.file) => {
            if let Err(e) = std::fs::write(p, &text) {
                eprintln!("report: {}: {e}", p.display());
            }
        }
        _ => println!("{text}"),
    }
}

/// Runs the whole pass, writes the report, returns the process exit code (0 or 1).
pub fn run(
    a: &FileArgs,
    render: &mut dyn FnMut(Loaded, &FileArgs) -> Result<Value, String>,
) -> i32 {
    let mut rep = blank(a);
    // a panic anywhere in the pass still leaves a report with ok=false and the reason
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| execute(a, render, &mut rep)))
        .unwrap_or_else(|p| {
            let why = p
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "unknown".into());
            Err(format!("internal error: {why}"))
        });
    rep.insert("peak_memory_bytes".into(), json!(peak_memory()));
    let code = match r {
        Ok(()) => {
            rep.insert("ok".into(), json!(true));
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            rep.insert("error".into(), json!(e));
            1
        }
    };
    emit(a, &Value::Object(rep));
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sheet_outputs_of_a_project_without_sheets_are_skipped_not_fatal() {
        use ddc_model::{LayerSpec, WallSpec};
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_level("L0", 0.0).unwrap();
        let layer = LayerSpec { material: "masonry".into(), thickness: 0.2 };
        let t = m.add_wall_type("W", &[layer]).unwrap();
        let w = |a, b| WallSpec { a, b, level: l, base_offset: 0.0, height: 3.0, wall_type: t, offset0: None };
        m.add_wall(&w((0.0, 0.0), (6.0, 0.0))).unwrap();
        m.add_wall(&w((6.0, 0.0), (6.0, 4.0))).unwrap();
        let dir = std::env::temp_dir().join(format!("ddc-hl-nosheets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ifc = dir.join("in.ifc");
        std::fs::write(&ifc, ddc_io::ifc::export_ifc(&m).unwrap()).unwrap();
        let (pdf, csv, rep) = (dir.join("all.pdf"), dir.join("m.csv"), dir.join("rep.json"));
        let argv: Vec<String> = [
            "ddc-app", "--file", ifc.to_str().unwrap(), "--export", pdf.to_str().unwrap(),
            "--export-sheets", dir.join("sheets").to_str().unwrap(), "--export",
            csv.to_str().unwrap(), "--report-json", rep.to_str().unwrap(),
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let fa = parse(&argv).unwrap().unwrap();
        let code = run(&fa, &mut |_, _| Ok(Value::Null));
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&rep).unwrap()).unwrap();
        assert_eq!(code, 0, "{v}");
        assert_eq!(v["ok"], json!(true), "{v}");
        let skipped = v["skipped"].as_array().unwrap();
        assert_eq!(skipped.len(), 2, "{v}");
        assert!(skipped.iter().all(|s| s["why"] == json!("the project has no sheets")));
        assert!(!pdf.exists());
        // the steps after the skipped one still ran
        assert!(csv.is_file());
        assert_eq!(v["exports"].as_array().unwrap().len(), 1, "{v}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
