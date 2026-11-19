//! `ddc publish`: a `.ddc` project becomes a `.ddcrev` revision package (contract `ddc-erp-link/1`).
//!
//! Steps, each timed in the report: open, geometry (bodies of the scene regrouped per element),
//! rows (`elements.parquet`), diff against the parent, sheets, IFC, manifest, zip, re-read.
//! The contract schemas validate manifest, diff, brief and every row before anything is written.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use web_time::Instant;

use ddc_io::contract::{validate, Doc};
use ddc_io::elements_parquet::{collect, read_parquet, write_parquet, ElementRow, Geo, GeoMap, GEOMETRY_NONE};
use serde_json::{json, Map, Value};

use super::glb::{self, Body, Item};
use super::{diff, package};
use crate::editor::Editor;

#[derive(Clone, Debug, Default)]
pub struct PublishArgs {
    pub file: PathBuf,
    pub out: PathBuf,
    pub parent: Option<PathBuf>,
    pub sheets: bool,
    pub ifc: bool,
    pub force: bool,
    /// `D1`..`D5`; the model does not record it.
    pub depth: Option<String>,
    pub name: Option<String>,
    pub author: Option<String>,
    pub message: Option<String>,
    pub project_id: Option<String>,
    pub erp_url: Option<String>,
    /// A `ddc-brief/1` file: the revision then counts as generated from it.
    pub brief: Option<PathBuf>,
    pub imported_from: Option<String>,
    /// Fixed identity and time, for reproducible packages.
    pub revision_id: Option<String>,
    pub created_at: Option<String>,
}

/// kind: `input` (a file cannot be read or is not what it says), `parent`, `output`, `contract`
/// (our own output fails the schema: a bug, nothing is written), `internal`.
#[derive(Debug)]
pub struct PubError {
    pub kind: &'static str,
    pub msg: String,
    pub details: Option<Value>,
}

fn fail<T>(kind: &'static str, msg: impl Into<String>) -> Result<T, PubError> {
    Err(PubError { kind, msg: msg.into(), details: None })
}

#[derive(Debug)]
pub struct Report {
    pub revision_id: String,
    pub out: PathBuf,
    pub bytes: u64,
    pub files: Vec<Value>,
    pub counts: Value,
    pub notes: Vec<Value>,
    pub steps: Vec<Value>,
    pub keys_saved: bool,
}

/// Elements the scene holds, regrouped: (table, id) -> triangles in GLB space.
fn regroup(ed: &mut Editor, notes: &mut Vec<Value>) -> Result<BTreeMap<(String, i64), Body>, PubError> {
    // the app registers its 3D body providers at start; headless, footings, rafts and ramps have no body without them
    crate::edit3d::struct3d::register();
    crate::edit3d::ramp3d::register();
    let ex = crate::gltf_export::build(ed, "publish").map_err(|e| PubError { kind: "internal", msg: format!("scene: {e}"), details: None })?;
    for i in ex.issues {
        notes.push(json!({"step": "geometry", "what": i}));
    }
    let mut out: BTreeMap<(String, i64), Body> = BTreeMap::new();
    let mut anonymous = 0usize;
    for n in &ex.gltf.nodes {
        let d = &n.extras["ddc"];
        let (Some(t), Some(id)) = (d["table"].as_str(), d["id"].as_i64()) else {
            anonymous += 1;
            continue;
        };
        let m = &ex.gltf.meshes[n.mesh];
        let b = out.entry((t.to_string(), id)).or_default();
        match n.matrix {
            Some(k) => {
                let det = k[0] * (k[5] * k[10] - k[6] * k[9]) - k[4] * (k[1] * k[10] - k[2] * k[9]) + k[8] * (k[1] * k[6] - k[2] * k[5]);
                let place = |p: [f32; 3]| {
                    [
                        k[0] * p[0] + k[4] * p[1] + k[8] * p[2] + k[12],
                        k[1] * p[0] + k[5] * p[1] + k[9] * p[2] + k[13],
                        k[2] * p[0] + k[6] * p[1] + k[10] * p[2] + k[14],
                    ]
                };
                b.add(&m.positions, &m.triangles, &place, det < 0.0);
            }
            None => {
                let t = n.translation;
                b.add(&m.positions, &m.triangles, &|p| [p[0] + t[0], p[1] + t[1], p[2] + t[2]], false);
            }
        }
    }
    if anonymous > 0 {
        notes.push(json!({"step": "geometry", "what": format!("{anonymous} scene bodies belong to no element and are not in the package")}));
    }
    Ok(out)
}

fn open(path: &Path) -> Result<Editor, PubError> {
    let mut ed = Editor::new_in_memory().map_err(|e| PubError { kind: "internal", msg: e.to_string(), details: None })?;
    match ed.open_project(path) {
        Ok(()) => Ok(ed),
        Err(ddc_model::ProjectError::Locked(_)) => {
            ed.open_project_read_only(path).map_err(|e| PubError { kind: "input", msg: format!("{}: {e}", path.display()), details: None })?;
            Ok(ed)
        }
        Err(e) => fail("input", format!("{}: {e}", path.display())),
    }
}

fn ms(t: Instant) -> u64 {
    t.elapsed().as_millis() as u64
}

fn civil(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

pub fn iso_utc(secs: i64) -> String {
    let (y, m, d) = civil(secs.div_euclid(86_400));
    let r = secs.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", r / 3600, r % 3600 / 60, r % 60)
}

fn now_iso() -> String {
    let s = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    iso_utc(s)
}

pub fn new_uuid() -> Result<String, String> {
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b).map_err(|e| format!("no random source: {e}"))?;
    b[6] = b[6] & 0x0f | 0x40;
    b[8] = b[8] & 0x3f | 0x80;
    let h = glb::hex(&b);
    Ok(format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32]))
}

fn sheet_file(name: &str, used: &mut Vec<String>) -> String {
    let mut s: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' { c } else { '_' }).collect();
    while s.starts_with('.') {
        s.remove(0);
    }
    if s.is_empty() {
        s = "sheet".into();
    }
    let base = s.clone();
    let mut n = 1;
    while used.contains(&s) {
        n += 1;
        s = format!("{base}-{n}");
    }
    used.push(s.clone());
    s
}

fn temp_dir() -> Result<PathBuf, PubError> {
    let t = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let d = std::env::temp_dir().join(format!("ddc-publish-{}-{t}", std::process::id()));
    std::fs::create_dir_all(&d).map_err(|e| PubError { kind: "output", msg: format!("{}: {e}", d.display()), details: None })?;
    Ok(d)
}

fn role_of(path: &str) -> &'static str {
    match path {
        "model.ddc" => "source",
        "geometry.glb" => "geometry",
        "elements.parquet" => "elements",
        "diff.json" => "diff",
        "brief.json" => "brief",
        "issues.bcfzip" => "issues",
        "model.ifc" => "ifc",
        _ => "sheet",
    }
}

fn read_parent(p: &Path) -> Result<(String, Vec<ElementRow>), PubError> {
    let perr = |m: String| PubError { kind: "parent", msg: format!("{}: {m}", p.display()), details: None };
    let m = package::verify(p).map_err(perr)?;
    let id = m["revision_id"].as_str().ok_or_else(|| perr("the manifest has no revision_id".into()))?.to_string();
    let all = package::read_all(p).map_err(perr)?;
    let rows = read_parquet(all.get("elements.parquet").ok_or_else(|| perr("no elements.parquet".into()))?.clone()).map_err(perr)?;
    Ok((id, rows))
}

pub fn publish(a: &PublishArgs, progress: &mut dyn FnMut(&str)) -> Result<Report, PubError> {
    let mut steps = vec![];
    let mut notes: Vec<Value> = vec![];
    if !a.file.is_file() {
        return fail("input", format!("file not found: {}", a.file.display()));
    }
    if a.out.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() != Some("ddcrev") {
        return fail("output", "the output must be a .ddcrev file");
    }
    if a.out.exists() && !a.force {
        return fail("output", format!("{} exists: pass --force to replace it", a.out.display()));
    }
    let brief_bytes = match &a.brief {
        Some(p) => Some(std::fs::read(p).map_err(|e| PubError { kind: "input", msg: format!("{}: {e}", p.display()), details: None })?),
        None => None,
    };
    let brief: Option<Value> = match &brief_bytes {
        Some(b) => {
            let v: Value = serde_json::from_slice(b).map_err(|e| PubError { kind: "input", msg: format!("brief is not JSON: {e}"), details: None })?;
            validate(Doc::Brief, &v).map_err(|e| PubError { kind: "input", msg: "the brief does not follow ddc-brief/1".into(), details: Some(json!(e)) })?;
            Some(v)
        }
        None => None,
    };
    // the model does not record the depth: the flag, else the brief, else D2 (said in the log)
    let depth = a.depth.clone().or_else(|| brief.as_ref().and_then(|b| b["depth_level"].as_str().map(String::from))).unwrap_or_else(|| "D2".into());
    if !["D1", "D2", "D3", "D4", "D5"].contains(&depth.as_str()) {
        return fail("input", format!("--depth must be D1..D5, not '{depth}'"));
    }
    if a.depth.is_none() && brief.as_ref().is_none_or(|b| b["depth_level"].as_str().is_none()) {
        notes.push(json!({"step": "manifest", "what": "depth_level D2: the model does not record it, pass --depth to set it"}));
    }
    let parent = match &a.parent {
        Some(p) => {
            if p == &a.out {
                return fail("parent", "the parent and the output are the same file");
            }
            Some(read_parent(p)?)
        }
        None => None,
    };
    let revision_id = match &a.revision_id {
        Some(r) => r.clone(),
        None => new_uuid().map_err(|e| PubError { kind: "internal", msg: e, details: None })?,
    };
    let created_at = a.created_at.clone().unwrap_or_else(now_iso);
    let work = temp_dir()?;
    let r = build(a, &mut steps, &mut notes, progress, &work, &depth, brief_bytes, brief, parent, &revision_id, &created_at);
    let _ = std::fs::remove_dir_all(&work);
    r
}

#[allow(clippy::too_many_arguments)]
fn build(
    a: &PublishArgs,
    steps: &mut Vec<Value>,
    notes: &mut Vec<Value>,
    progress: &mut dyn FnMut(&str),
    work: &Path,
    depth: &str,
    brief_bytes: Option<Vec<u8>>,
    brief: Option<Value>,
    parent: Option<(String, Vec<ElementRow>)>,
    revision_id: &str,
    created_at: &str,
) -> Result<Report, PubError> {
    let internal = |m: String| PubError { kind: "internal", msg: m, details: None };
    let t = Instant::now();
    let mut ed = open(&a.file)?;
    let clean_on_open = !ed.is_dirty();
    step("open", t, steps, progress);

    // geometry and keys
    let t = Instant::now();
    let bodies = regroup(&mut ed, notes)?;
    let mut glb_of: HashMap<(String, i64), (String, Body)> = HashMap::new();
    let mut geo = GeoMap::new();
    for ((table, id), body) in bodies {
        if !body.usable() {
            notes.push(json!({"step": "geometry", "what": format!("{table} {id}: no usable triangles")}));
            continue;
        }
        let (key, _) = ed.model.ensure_element_key(&table, id).map_err(|e| internal(format!("{table} {id}: {e}")))?;
        geo.insert((table.clone(), id), Geo { bbox: body.bbox(), hash: glb::geometry_hash(&body.positions, &body.tris) });
        glb_of.insert((table, id), (key, body));
    }
    step("geometry", t, steps, progress);

    // rows
    let t = Instant::now();
    let col = collect(&mut ed.model, &geo).map_err(|e| internal(format!("elements: {e}")))?;
    for n in &col.notes {
        notes.push(json!({"step": "elements", "key": n.key, "field": n.field, "what": n.what}));
    }
    let rows = col.rows;
    let by_key: HashMap<&str, &ElementRow> = rows.iter().map(|r| (r.key.as_str(), r)).collect();
    let mut items: Vec<Item> = vec![];
    for (_, (key, body)) in glb_of {
        match by_key.get(key.as_str()) {
            Some(r) if r.ifc_class != "IfcSpace" => items.push(Item { key, type_key: r.type_key.clone(), ifc_class: r.ifc_class.clone(), body }),
            Some(_) => {}
            None => notes.push(json!({"step": "geometry", "key": key, "what": "has a body but no element row, left out of the package"})),
        }
    }
    items.sort_by(|x, y| x.key.cmp(&y.key));
    let big = items.iter().filter(|i| i.body.bbox().iter().any(|c| c.abs() > 100_000.0)).count();
    if big > 0 {
        notes.push(json!({"step": "geometry", "what": format!("{big} elements lie more than 100 km from the origin: put the base point near the site")}));
    }
    let mut parquet = vec![];
    let pn = write_parquet(&rows, &mut parquet).map_err(|e| internal(format!("elements.parquet: {e}")))?;
    for n in pn {
        notes.push(json!({"step": "elements", "key": n.key, "field": n.field, "what": n.what}));
    }
    step("elements", t, steps, progress);

    let t = Instant::now();
    let built = glb::write(&items).map_err(|e| internal(format!("geometry.glb: {e}")))?;
    step("glb", t, steps, progress);

    // keys into the source file, then the model bytes
    let t = Instant::now();
    let keys_saved = clean_on_open && ed.is_dirty() && !ed.project.read_only() && {
        ed.save_project().map_err(|e| PubError { kind: "output", msg: format!("saving the minted keys into {}: {e}", a.file.display()), details: None })?;
        true
    };
    let copy = work.join("model.ddc");
    ed.save_project_as(&copy).map_err(|e| PubError { kind: "output", msg: format!("model copy: {e}"), details: None })?;
    let model_bytes = std::fs::read(&copy).map_err(|e| internal(format!("model copy: {e}")))?;
    step("model", t, steps, progress);

    // diff
    let t = Instant::now();
    let diff_json = match &parent {
        Some((pid, prows)) => Some(diff::diff(revision_id, Some(pid), prows, &rows)),
        None => None,
    };
    if let Some(d) = &diff_json {
        validate(Doc::Diff, d).map_err(|e| PubError { kind: "contract", msg: "diff.json fails ddc-diff/1".into(), details: Some(json!(e)) })?;
    }
    step("diff", t, steps, progress);

    // optional files
    let mut sheet_files: Vec<(String, Vec<u8>)> = vec![];
    if a.sheets {
        let t = Instant::now();
        let ids: Vec<i64> = ed.model.sheets().map_err(|e| internal(e.to_string()))?.iter().map(|s| s.0).collect();
        if ids.is_empty() {
            notes.push(json!({"step": "sheets", "what": "the project has no sheets"}));
        } else {
            let dir = work.join("sheets");
            std::fs::create_dir_all(&dir).map_err(|e| internal(e.to_string()))?;
            let mut p = crate::sheet_view::Print::new(None);
            p.separate = true;
            p.pdf = true;
            p.svg = false;
            let done = crate::headless_file::print_run(&ed, &p, &ids, &dir);
            if !done.clean || done.files == 0 {
                return fail("output", format!("sheets: {}", done.report));
            }
            let mut names: Vec<_> = std::fs::read_dir(&dir).map_err(|e| internal(e.to_string()))?.flatten().map(|e| e.path()).collect();
            names.sort();
            let mut used = vec![];
            for f in names.iter().filter(|f| f.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf"))) {
                let stem = f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                let bytes = std::fs::read(f).map_err(|e| internal(e.to_string()))?;
                sheet_files.push((format!("sheets/{}.pdf", sheet_file(&stem, &mut used)), bytes));
            }
        }
        step("sheets", t, steps, progress);
    }
    let mut ifc_bytes = None;
    if a.ifc {
        let t = Instant::now();
        let target = work.join("model.ifc");
        let o = ddc_io::ifc::IfcOptions { schema: ddc_io::ifc::IfcSchema::Ifc4x3, timestamp: None, ..Default::default() };
        let st = ddc_io::ifc::export_ifc_file_with(&ed.model, &target, "model.ifc", None, &o).map_err(|e| PubError { kind: "output", msg: format!("model.ifc: {e}"), details: None })?;
        for w in st.warnings.iter().take(50) {
            notes.push(json!({"step": "ifc", "what": w}));
        }
        ifc_bytes = Some(std::fs::read(&target).map_err(|e| internal(e.to_string()))?);
        step("ifc", t, steps, progress);
    }

    // manifest
    let t = Instant::now();
    let mut files: Vec<(String, Vec<u8>)> = vec![("model.ddc".into(), model_bytes), ("geometry.glb".into(), built.glb), ("elements.parquet".into(), parquet)];
    if let Some(d) = &diff_json {
        files.push(("diff.json".into(), pretty(d)));
    }
    if let Some(b) = brief_bytes {
        files.push(("brief.json".into(), b));
    }
    files.extend(sheet_files);
    if let Some(b) = ifc_bytes {
        files.push(("model.ifc".into(), b));
    }
    let listed: Vec<Value> = files.iter().map(|(p, d)| json!({"path": p, "role": role_of(p), "sha256": package::sha256(d), "bytes": d.len()})).collect();

    let spaces = rows.iter().filter(|r| r.ifc_class == "IfcSpace").count();
    let storeys: std::collections::BTreeSet<&str> = rows.iter().filter_map(|r| r.storey.as_deref()).collect();
    let types: std::collections::BTreeSet<&str> = rows.iter().map(|r| r.type_key.as_str()).collect();
    let counts = json!({"elements": rows.len() - spaces, "storeys": storeys.len(), "types": types.len(), "spaces": spaces, "triangles": built.triangles});

    let mut source = Map::new();
    if let (Some(b), Some(bj)) = (&brief_bytes_hash(&files), &brief) {
        source.insert("kind".into(), json!("generated"));
        source.insert("brief_sha256".into(), json!(b));
        let v = bj["version"].as_u64().filter(|v| *v >= 1).ok_or_else(|| PubError { kind: "input", msg: "the brief has no version number (version >= 1)".into(), details: None })?;
        source.insert("brief_version".into(), json!(v));
        let g = &bj["generator"];
        let mut gen = Map::new();
        gen.insert("template".into(), json!(g["template"].as_str().unwrap_or("universal")));
        gen.insert("version".into(), json!(env!("CARGO_PKG_VERSION")));
        if let Some(s) = g["seed"].as_u64() {
            gen.insert("seed".into(), json!(s));
        }
        source.insert("generator".into(), Value::Object(gen));
    } else if let Some(f) = &a.imported_from {
        source.insert("kind".into(), json!("imported"));
        source.insert("imported_from".into(), json!(f));
    } else {
        source.insert("kind".into(), json!("edited"));
    }

    let name = a
        .name
        .clone()
        .or_else(|| brief.as_ref().and_then(|b| b["name"].as_str().map(String::from)))
        .unwrap_or_else(|| a.file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
    let mut project_ref = Map::new();
    project_ref.insert("erp_project_id".into(), a.project_id.as_ref().map_or(Value::Null, |p| json!(p)));
    if let Some(u) = &a.erp_url {
        project_ref.insert("erp_url".into(), json!(u));
    }
    project_ref.insert("name".into(), json!(name.chars().take(200).collect::<String>()));
    let author = a
        .author
        .clone()
        .or_else(|| std::env::var("USERNAME").ok().or_else(|| std::env::var("USER").ok()))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "unknown".into());
    let mut manifest = json!({
        "schema": "ddc-revision/1",
        "revision_id": revision_id,
        "project_ref": project_ref,
        "parent_revision_id": parent.as_ref().map(|p| p.0.clone()),
        "created_at": created_at,
        "author": {"display_name": author.chars().take(200).collect::<String>()},
        "editor_version": env!("CARGO_PKG_VERSION"),
        "source": source,
        "depth_level": depth,
        "units": {"length": "m", "area": "m2", "volume": "m3", "mass": "kg", "angle": "deg"},
        "base_point": {"lat": null, "lon": null, "elevation_m": 0.0, "rotation_deg": 0.0},
        "counts": counts,
        "files": listed,
    });
    if let Some(b) = &brief {
        for k in ["locale", "country", "norm_pack"] {
            if let Some(v) = b[k].as_str() {
                manifest[k] = json!(v);
            }
        }
    }
    if let Some(m) = &a.message {
        manifest["message"] = json!(m.chars().take(2000).collect::<String>());
    }
    validate(Doc::Revision, &manifest).map_err(|e| {
        // a value the caller typed is the caller's error; anything else is ours
        let theirs = ["/project_ref", "/author", "/message", "/revision_id", "/created_at", "/depth_level"];
        let user = e.iter().all(|x| theirs.iter().any(|t| x.starts_with(t)));
        PubError { kind: if user { "input" } else { "contract" }, msg: "manifest.json fails ddc-revision/1".into(), details: Some(json!(e)) }
    })?;
    // rows are checked one by one, the way the ERP reads them
    for r in &rows {
        let v = row_json(r);
        validate(Doc::ElementRow, &v).map_err(|e| PubError { kind: "contract", msg: format!("row '{}' fails the element-row schema", r.key), details: Some(json!(e)) })?;
    }
    step("manifest", t, steps, progress);

    // zip, then read it back
    let t = Instant::now();
    let mut entries = vec![("manifest.json".to_string(), pretty(&manifest))];
    entries.extend(files);
    let zip = package::zip_bytes(&entries, &|n| n == "elements.parquet").map_err(|e| internal(format!("zip: {e}")))?;
    if let Some(d) = a.out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(d).map_err(|e| PubError { kind: "output", msg: format!("{}: {e}", d.display()), details: None })?;
    }
    let part = PathBuf::from(format!("{}.part", a.out.display()));
    std::fs::write(&part, &zip).map_err(|e| PubError { kind: "output", msg: format!("{}: {e}", part.display()), details: None })?;
    if let Err(e) = package::verify(&part) {
        let _ = std::fs::remove_file(&part);
        return fail("internal", format!("the written package does not match its manifest: {e}"));
    }
    std::fs::rename(&part, &a.out).map_err(|e| PubError { kind: "output", msg: format!("{}: {e}", a.out.display()), details: None })?;
    step("zip", t, steps, progress);

    if notes.len() > 200 {
        let n = notes.len();
        notes.truncate(200);
        notes.push(json!({"step": "all", "what": format!("{} more notes not shown", n - 200)}));
    }
    Ok(Report {
        revision_id: revision_id.into(),
        out: a.out.clone(),
        bytes: zip.len() as u64,
        files: manifest["files"].as_array().cloned().unwrap_or_default(),
        counts,
        notes: std::mem::take(notes),
        steps: std::mem::take(steps),
        keys_saved,
    })
}

fn step(name: &str, t: Instant, steps: &mut Vec<Value>, progress: &mut dyn FnMut(&str)) {
    steps.push(json!({"step": name, "ms": ms(t)}));
    progress(name);
}

fn brief_bytes_hash(files: &[(String, Vec<u8>)]) -> Option<String> {
    files.iter().find(|(p, _)| p == "brief.json").map(|(_, d)| package::sha256(d))
}

fn pretty(v: &Value) -> Vec<u8> {
    let mut s = serde_json::to_string_pretty(v).unwrap_or_default();
    s.push('\n');
    s.into_bytes()
}

/// A row as the schema sees it (a missing hash is the all-zero hash).
pub fn row_json(r: &ElementRow) -> Value {
    let o = |s: &Option<String>| s.as_ref().map_or(Value::Null, |v| json!(v));
    let f = |v: Option<f64>| v.filter(|x| x.is_finite()).map_or(Value::Null, |x| json!(x));
    let hash = if r.geometry_hash.is_empty() { GEOMETRY_NONE } else { &r.geometry_hash };
    json!({
        "id": r.key, "stable_id": r.key, "element_key": r.key,
        "element_type": r.element_type, "ifc_class": r.ifc_class, "type_key": r.type_key, "name": r.name,
        "storey": o(&r.storey), "space": o(&r.space), "discipline": r.discipline, "material": o(&r.material),
        "phase": o(&r.phase), "space_usage": o(&r.space_usage), "geometry_hash": hash,
        "length_m": f(r.length_m), "area_m2": f(r.area_m2), "area_gross_m2": f(r.area_gross_m2),
        "volume_m3": f(r.volume_m3), "volume_gross_m3": f(r.volume_gross_m3), "weight_kg": f(r.weight_kg),
        "count": r.count.map_or(Value::Null, |c| json!(c)),
        "bbox_min_x": r.bbox[0], "bbox_min_y": r.bbox[1], "bbox_min_z": r.bbox[2],
        "bbox_max_x": r.bbox[3], "bbox_max_y": r.bbox[4], "bbox_max_z": r.bbox[5],
        "props": o(&r.props),
    })
}
