// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The owner goal end to end: an assistant (scripted, no network) builds a project from a
//! brief, revises it, makes the drawing set; every step is one Apply and one undo step; the
//! result goes out as PDF, DXF, IFC4 and IFC2x3, and every
//! output is read back and counted against the model and its schedules. `ddc run` runs a Lua
//! script on the saved project into a new file.
//!
//! `DDC_BENCH_RECORD=1` appends the measured step times to the bench ledger.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_ai::fake::{Script, ScriptedProvider};
use ddc_ai::Mode;
use ddc_app::editor::Editor;
use ddc_app::sheet_view::schedule_table;
use ddc_io::ifc::{export_ifc_file_with, validate_ifc, IfcOptions, IfcSchema};
use ddc_io::sheets::{
    check_set, compose_sheet, export_sheets_pdf, mark_box, page_key, Ctx, LengthUnits, Mark,
    PlacedKind, Region, Table, LAYERS,
};
use ddc_ui::{Catalog, Lang};
use e2e_ai_common::*;
use e2e_common::{count, temp_file};
use serde_json::{json, Value as J};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

/// One typology at a time: the step times are then this flow's own, and memory stays bounded.
static SERIAL: Mutex<()> = Mutex::new(());

/// Generous ceilings (ms) of a debug build; the ledger keeps the real numbers.
const BUDGET: [(&str, f64); 8] = [
    ("draft", 60_000.0),
    ("apply", 90_000.0),
    ("revise", 60_000.0),
    ("revise_apply", 90_000.0),
    ("dset_draft", 120_000.0),
    ("dset_apply", 120_000.0),
    ("pdf_dxf", 120_000.0),
    ("ifc", 120_000.0),
];

struct Case {
    typology: &'static str,
    floors: u32,
    /// Words of the spec, the drawing set and its catalog.
    ru: bool,
    brief: &'static str,
}

const CASES: [Case; 4] = [
    Case {
        typology: "house",
        floors: 2,
        ru: false,
        brief: "A two-storey family house with a pitched roof.",
    },
    Case {
        typology: "apartment_section",
        floors: 9,
        ru: true,
        brief: "A nine-storey residential section with a stair and lift core.",
    },
    Case {
        typology: "office",
        floors: 3,
        ru: false,
        brief: "A three-storey office building with open plan floors.",
    },
    Case {
        typology: "school",
        floors: 2,
        ru: false,
        brief: "A two-storey primary school with classrooms along a corridor.",
    },
];

fn editor() -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.reload().unwrap();
    ed
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn tool_ok(p: &ScriptedProvider, step: &str) -> J {
    let results = tool_results(p);
    let (last, err) = results.last().unwrap_or_else(|| panic!("{step}: no tool result"));
    assert!(!err, "{step}: {last}");
    let v = parsed(last);
    assert!(v.get("unparsed").is_none(), "{step}: {last}");
    v
}

fn openings(ed: &Editor, kind: &str) -> i64 {
    ed.model
        .sql()
        .query_row("SELECT count(*) FROM opening WHERE kind=?1", [kind], |r| {
            r.get(0)
        })
        .unwrap()
}

/// Element counts of the model under the names the project builder reports them.
fn model_count(ed: &Editor, key: &str) -> i64 {
    match key {
        "doors" => openings(ed, "door"),
        "windows" => openings(ed, "window"),
        k => {
            let table = k.strip_suffix('s').unwrap_or_else(|| panic!("count key {k}"));
            count(&ed.model, table)
        }
    }
}

/// Every count the builder promised is what the model holds after Apply.
fn assert_counts(ed: &Editor, reply: &J, step: &str) -> BTreeMap<String, i64> {
    let counts = reply["project"]["counts"]
        .as_object()
        .unwrap_or_else(|| panic!("{step}: no counts in {reply}"));
    assert!(counts.len() >= 9, "{step}: {counts:?}");
    let mut out = BTreeMap::new();
    for (k, v) in counts {
        let want = v.as_i64().unwrap();
        // a two-storey house has no lift shaft; everything else every typology has
        assert!(want > 0 || k == "shafts", "{step}: {k} is {want}");
        assert_eq!(model_count(ed, k), want, "{step}: {k} in the model");
        out.insert(k.clone(), want);
    }
    out
}

fn catalog(ru: bool) -> Catalog {
    Catalog::new(if ru { Lang::Ru } else { Lang::En })
}

/// Drawing set sheets in the order of the sheet list.
fn set_sheets(ed: &Editor) -> Vec<i64> {
    let order: Vec<i64> = ed.model.sheets().unwrap().into_iter().map(|s| s.0).collect();
    let mut ids: Vec<i64> = ed
        .model
        .drawing_set_sheets()
        .unwrap()
        .into_iter()
        .map(|s| s.0)
        .collect();
    ids.sort_by_key(|id| order.iter().position(|o| o == id));
    ids
}

/// IFC entities of each class in a STEP text.
fn ifc_classes(text: &str) -> BTreeMap<String, usize> {
    let mut out: BTreeMap<String, usize> = BTreeMap::new();
    for line in text.lines() {
        let Some((_, rest)) = line.split_once('=') else {
            continue;
        };
        let Some((class, _)) = rest.split_once('(') else {
            continue;
        };
        let class = class.trim();
        if class.starts_with("IFC") {
            *out.entry(class.to_string()).or_default() += 1;
        }
    }
    out
}

/// A class and its standard case subtype (IFC2x3 writes walls as the latter).
fn ifc_count(c: &BTreeMap<String, usize>, class: &str) -> i64 {
    let std = format!("{class}STANDARDCASE");
    c.iter()
        .filter(|(k, _)| k.as_str() == class || k.as_str() == std)
        .map(|(_, v)| *v as i64)
        .sum()
}

fn layer_of(e: &ddc_io::dxf::Entity) -> Option<&str> {
    use ddc_io::dxf::Entity as E;
    Some(match e {
        E::Line(x) => &x.common.layer,
        E::LwPolyline(x) => &x.common.layer,
        E::Text(x) => &x.common.layer,
        E::MText(x) => &x.common.layer,
        _ => return None,
    })
}

fn dxf_texts(d: &ddc_io::dxf::DxfDocument) -> Vec<String> {
    d.entities
        .iter()
        .filter_map(|e| match e {
            ddc_io::dxf::Entity::Text(t) => Some(t.text.clone()),
            ddc_io::dxf::Entity::MText(t) => Some(ddc_io::dxf::mtext_plain(&t.text)),
            _ => None,
        })
        .collect()
}

fn stamp_layer() -> usize {
    LAYERS.iter().position(|l| *l == "Stamp").unwrap()
}

struct Outputs {
    dir: PathBuf,
}

impl Outputs {
    fn new(tag: &str) -> Self {
        let dir = temp_file(&format!("goal_{tag}"));
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }
    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

impl Drop for Outputs {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The drawing set: frame, stamp, schedules, PDF pages and DXF sheets, all read back.
fn check_drawings(
    ed: &Editor,
    c: &Case,
    ids: &[i64],
    counts: &BTreeMap<String, i64>,
    out: &Outputs,
) {
    let t = c.typology;
    let cat = catalog(c.ru);
    let text = |k: &str| cat.tf(k, &[]);
    let sched = |k: &str| -> Option<Table> { schedule_table(ed, k) };
    let ctx = |index: Option<(usize, usize)>| Ctx {
        units: LengthUnits::Metric,
        schedules: &sched,
        index,
        paper: None,
        text: &text,
        refs: None,
    };
    let rep = check_set(&ed.model, ids, &ctx(None)).unwrap();
    assert_eq!(rep.sheets, ids.len());
    assert!(rep.outside.is_empty(), "{t}: outside the frame {:?}", rep.outside);
    assert!(rep.overlaps.is_empty(), "{t}: overlapping views {:?}", rep.overlaps);
    assert_eq!(rep.tag_overlaps, 0, "{t}: tags on top of each other {:?}", rep.tag_pairs);
    let storeys = (counts["levels"] - 1) as usize;
    // a plan of every storey, a section, four elevations
    assert!(rep.views >= storeys + 1 + 4, "{t}: {} views", rep.views);
    assert!(rep.schedules >= 3, "{t}: {} schedules", rep.schedules);
    // every level marked on the section and the four elevations at least
    let levels = counts["levels"] as usize;
    assert!(rep.level_marks >= levels * 5, "{t}: {} level marks", rep.level_marks);
    assert!(rep.hatches > 0, "{t}: cut materials not hatched");
    // every room, door and window carries a tag
    let tags: usize = ed
        .model
        .level_list()
        .unwrap()
        .iter()
        .map(|l| ed.model.tags(l.0).unwrap().len())
        .sum();
    let tagged = counts["rooms"] + counts["doors"] + counts["windows"];
    assert!(tags as i64 >= tagged, "{t}: {tags} tags for {tagged} elements");

    // schedules: the model's rows, and every element once across the sheet parts
    let mut rows: BTreeMap<String, usize> = BTreeMap::new();
    for &id in ids {
        for (_, p) in ed.model.schedule_places(id).unwrap() {
            let (k, range) = page_key(&p.schedule);
            let n = match range {
                Some(r) => r.len(),
                None => schedule_table(ed, k).map_or(0, |x| x.rows.len()),
            };
            *rows.entry(k.to_string()).or_default() += n;
        }
    }
    for (k, model) in [
        ("rooms", counts["rooms"]),
        ("doors", counts["doors"]),
        ("windows", counts["windows"]),
    ] {
        let table = schedule_table(ed, k).unwrap();
        assert_eq!(table.rows.len() as i64, model, "{t}: {k} schedule rows");
        assert_eq!(rows.get(k).copied().unwrap_or(0) as i64, model, "{t}: {k} on sheets");
    }
    for (k, model) in [("walls", counts["walls"]), ("slabs", counts["slabs"])] {
        let table = schedule_table(ed, k).unwrap();
        assert_eq!(table.rows.len() as i64, model, "{t}: {k} schedule rows");
    }

    // every sheet: views inside the frame, a stamp with its number, the right paper
    let stamp = stamp_layer();
    let mut pdf_numbers = vec![];
    for (n, &id) in ids.iter().enumerate() {
        let s = ed.model.sheet(id).unwrap();
        let comp = compose_sheet(&ed.model, id, &ctx(Some((n + 1, ids.len())))).unwrap();
        let reg = Region::of(&s.standard, &s.paper).unwrap();
        let mut lines = 0;
        let mut stamp_texts = vec![];
        for (_, m) in &comp.drawing.marks {
            match m {
                Mark::Line { layer, .. } if *layer == stamp => {
                    lines += 1;
                    let b = mark_box(m);
                    assert!(
                        b[0] >= reg.frame[0] - 0.5 && b[2] <= reg.frame[2] + 0.5,
                        "{t} {}: stamp line outside the frame {b:?}",
                        s.number
                    );
                }
                Mark::Text { s: x, layer, .. } if *layer == stamp => stamp_texts.push(x.clone()),
                _ => {}
            }
        }
        assert!(lines >= 10, "{t} {}: stamp has {lines} lines", s.number);
        assert!(
            stamp_texts.iter().any(|x| x.contains(&s.number)),
            "{t} {}: number not in the stamp {stamp_texts:?}",
            s.number
        );
        let views = comp
            .placed
            .iter()
            .filter(|p| matches!(p.kind, PlacedKind::Viewport(_) | PlacedKind::Schedule(_)))
            .count();
        // the cover carries the sheet list; every other sheet a view or a schedule
        assert!(n == 0 || views > 0, "{t} {}: empty sheet", s.number);
        assert!(comp.warnings.is_empty(), "{t} {}: {:?}", s.number, comp.warnings);

        // the sheet as DXF, written and read back from disk
        let doc = comp.drawing.to_dxf();
        let path = out.path(&format!("sheet_{n}.dxf"));
        std::fs::write(&path, ddc_io::dxf::write_string(&doc)).unwrap();
        let back = ddc_io::dxf::read_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(back.entities.len(), doc.entities.len(), "{t} {}: DXF entities", s.number);
        let on_stamp = back
            .entities
            .iter()
            .filter(|e| layer_of(e) == Some("Stamp"))
            .count();
        assert!(on_stamp >= lines, "{t} {}: {on_stamp} stamp entities", s.number);
        assert!(
            dxf_texts(&back).iter().any(|x| x.contains(&s.number)),
            "{t} {}: number not in the DXF",
            s.number
        );
        pdf_numbers.push(s.number);
    }

    // the whole set as one PDF: a page a sheet, each with its stamp and number
    let pdf = export_sheets_pdf(&ed.model, ids, &ctx(None)).unwrap();
    let path = out.path("set.pdf");
    std::fs::write(&path, &pdf).unwrap();
    let f = ddc_io::pdf::import::PdfFile::parse(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(f.page_count(), ids.len(), "{t}: PDF pages");
    for (i, number) in pdf_numbers.iter().enumerate() {
        let page = f.page(i).unwrap();
        let stamp = page.layers.iter().position(|l| l == "Stamp");
        let stamp = stamp.unwrap_or_else(|| panic!("{t} page {i}: no stamp layer"));
        assert!(
            page.shapes.iter().filter(|s| s.layer == stamp).count() > 0,
            "{t} page {i}: no stamp lines"
        );
        assert!(
            page.texts.iter().any(|x| x.string.contains(number.as_str())),
            "{t} page {i}: sheet {number} not printed"
        );
    }
}

/// IFC4 and IFC2x3 written by the product path, validated and counted against the model.
fn check_ifc(ed: &Editor, c: &Case, counts: &BTreeMap<String, i64>, out: &Outputs) {
    let t = c.typology;
    for schema in [IfcSchema::Ifc4, IfcSchema::Ifc2x3] {
        let name = schema.header_name();
        let path = out.path(&format!("{t}_{name}.ifc"));
        let opts = IfcOptions {
            schema,
            timestamp: Some("2026-11-09T12:00:00".to_string()),
            ..Default::default()
        };
        export_ifc_file_with(&ed.model, &path, "model.ifc", None, &opts).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let rep = validate_ifc(&bytes);
        assert!(rep.is_clean(), "{t} {name}: {}\n{}", rep.errors.len(), rep.brief(20));
        assert_eq!(rep.schema, name, "{t}: schema");
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains(&format!("FILE_SCHEMA(('{name}'))")), "{t}: header");
        let cl = ifc_classes(&text);
        for (class, key) in [
            ("IFCWALL", "walls"),
            ("IFCDOOR", "doors"),
            ("IFCWINDOW", "windows"),
            ("IFCSPACE", "rooms"),
            ("IFCBUILDINGSTOREY", "levels"),
            ("IFCSTAIR", "stairs"),
            ("IFCROOF", "roofs"),
        ] {
            assert_eq!(ifc_count(&cl, class), counts[key], "{t} {name}: {class} vs {key}");
        }
        assert!(ifc_count(&cl, "IFCSLAB") >= counts["slabs"], "{t} {name}: slabs");
    }
}

fn record(c: &Case, steps: &[(&str, f64)], counts: &BTreeMap<String, i64>) {
    let step_ms: serde_json::Map<String, J> = steps
        .iter()
        .map(|(k, v)| (k.to_string(), json!(v.round())))
        .collect();
    let line = json!({
        "bench": "e2e_goal",
        "typology": c.typology,
        "floors": c.floors + 1,
        "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "unix": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
        "ms": step_ms,
        "counts": counts,
    });
    eprintln!("e2e_goal {line}");
    if std::env::var("DDC_BENCH_RECORD").as_deref() != Ok("1") {
        return;
    }
    // DDC_BENCH_LEDGER names the file; the default stays inside the build output.
    let p = std::env::var_os("DDC_BENCH_LEDGER").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/bench/ledger.jsonl"),
        std::path::PathBuf::from,
    );
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&p).unwrap();
    writeln!(f, "{line}").unwrap();
}

fn goal(c: &Case) {
    let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let t = c.typology;
    let lang = if c.ru { "ru" } else { "en" };
    let mut ed = editor();
    assert_eq!(count(&ed.model, "level"), 1, "a new project has its ground level");
    let spec = json!({"typology": t, "floors": c.floors, "lang": lang});
    let done = || vec![Script::Text("done".to_string())];
    let provider = ScriptedProvider::new(vec![
        vec![tool("generate_project", json!({"spec": spec}))],
        done(),
        vec![tool("revise_project", json!({"changes": {"floors": c.floors + 1}}))],
        done(),
        vec![tool("create_drawing_set", json!({"title": "Drawings", "language": lang}))],
        done(),
    ]);
    let mut steps: Vec<(&str, f64)> = vec![];
    let mut s = start(&mut ed, cfg(provider.clone()));

    // brief -> a drafted project, nothing in the model yet
    let t0 = Instant::now();
    turn(&mut s, &mut ed, c.brief, Mode::Propose);
    steps.push(("draft", ms(t0)));
    let reply = tool_ok(&provider, "generate_project");
    assert_eq!(reply["project"]["typology"], t, "{reply}");
    assert_eq!(count(&ed.model, "wall"), 0, "{t}: nothing reaches the model before Apply");
    assert_eq!(plan_of(&s).items.len(), 1, "{t}: one plan item");
    let t0 = Instant::now();
    apply_and_check(&mut s, &mut ed);
    steps.push(("apply", ms(t0)));
    let first = assert_counts(&ed, &reply, &format!("{t} generate"));
    assert_eq!(first["levels"], i64::from(c.floors) + 1, "{t}: storeys and the roof level");

    // one floor more: the applied project changes in place
    let walls0 = e2e_common::ids(&ed.model, "wall");
    let t0 = Instant::now();
    turn(&mut s, &mut ed, "Add one more floor.", Mode::Propose);
    steps.push(("revise", ms(t0)));
    let rev = tool_ok(&provider, "revise_project");
    assert_eq!(rev["changes"]["deleted"], 0, "{t}: {rev}");
    let t0 = Instant::now();
    apply_and_check(&mut s, &mut ed);
    steps.push(("revise_apply", ms(t0)));
    let counts = assert_counts(&ed, &rev, &format!("{t} revise"));
    assert_eq!(counts["levels"], first["levels"] + 1, "{t}: one level more");
    assert!(counts["walls"] > first["walls"], "{t}: walls of the new floor");
    let walls1 = e2e_common::ids(&ed.model, "wall");
    assert!(walls0.iter().all(|w| walls1.contains(w)), "{t}: wall ids kept");

    // the drawing set, drafted on the copy and applied as one step
    let t0 = Instant::now();
    turn(&mut s, &mut ed, "Make the drawing set.", Mode::Propose);
    steps.push(("dset_draft", ms(t0)));
    let ds = tool_ok(&provider, "create_drawing_set");
    assert!(ed.model.sheets().unwrap().is_empty(), "{t}: no sheet before Apply");
    let t0 = Instant::now();
    apply_and_check(&mut s, &mut ed);
    steps.push(("dset_apply", ms(t0)));
    let ids = set_sheets(&ed);
    assert_eq!(ids.len() as i64, ds["sheets"].as_i64().unwrap(), "{t}: sheets as drafted");
    // the elements did not change by making drawings
    for (k, v) in &counts {
        assert_eq!(model_count(&ed, k), *v, "{t}: {k} after the drawing set");
    }

    let out = Outputs::new(t);
    let t0 = Instant::now();
    check_drawings(&ed, c, &ids, &counts, &out);
    steps.push(("pdf_dxf", ms(t0)));

    // the model itself as DXF, read back
    let doc = ddc_io::dxf::export_from(&ed.model).unwrap();
    let path = out.path("model.dxf");
    std::fs::write(&path, ddc_io::dxf::write_string(&doc)).unwrap();
    let back = ddc_io::dxf::read_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(!doc.entities.is_empty(), "{t}: model DXF is empty");
    assert_eq!(back.entities.len(), doc.entities.len(), "{t}: model DXF entities");

    let t0 = Instant::now();
    check_ifc(&ed, c, &counts, &out);
    steps.push(("ifc", ms(t0)));

    record(c, &steps, &counts);
    for (k, v) in &steps {
        let ceil = BUDGET.iter().find(|b| b.0 == *k).unwrap().1;
        assert!(*v < ceil, "{t}: {k} took {v:.0} ms (ceiling {ceil:.0})");
    }
}

#[test]
fn goal_house() {
    goal(&CASES[0]);
}

#[test]
fn goal_apartment_section_nine_floors() {
    goal(&CASES[1]);
}

#[test]
fn goal_office() {
    goal(&CASES[2]);
}

#[test]
fn goal_school() {
    goal(&CASES[3]);
}

const COLUMNS: &str = "local n = param('n', 2)
local p = profile.rect(0.3, 0.3)
for i = 1, n do
  part('column' .. i, extrude(p, vec3(i * 2, -3, 0), vec3(i * 2, -3, 3)))
end
";

/// `ddc run` on the generated, saved project: a new file with the script's columns, the input
/// untouched, the result still a clean IFC.
#[test]
fn ddc_run_scripts_a_generated_project_into_a_new_file() {
    let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut ed = editor();
    let spec = json!({"typology": "house", "floors": 2});
    let (mut s, provider) = run(&mut ed, vec![tool("generate_project", json!({"spec": spec}))]);
    tool_ok(&provider, "generate_project");
    apply_and_check(&mut s, &mut ed);
    let out = Outputs::new("cli");
    let proj = out.path("house.ddc");
    ed.model.save_project_to(&proj, &mut |_, _| Ok(())).unwrap();
    let before = std::fs::read(&proj).unwrap();
    let script = out.path("columns.lua");
    std::fs::write(&script, COLUMNS).unwrap();
    let new = out.path("house_columns.ddc");
    let arg = |p: &Path| p.as_os_str().to_owned();
    let argv: Vec<std::ffi::OsString> = vec![
        "run".into(),
        arg(&proj),
        arg(&script),
        "--out".into(),
        arg(&new),
        "--param".into(),
        "n=4".into(),
        "--json".into(),
        "--quiet".into(),
    ];
    let p = ddc_cli::args::parse(&argv).unwrap();
    let ddc_cli::args::Cmd::Run(a) = p.cmd else {
        panic!("not a run command");
    };
    let t0 = Instant::now();
    let (text, code) = ddc_cli::run::run(&a, p.json, p.quiet);
    let run_ms = ms(t0);
    assert_eq!(code, ddc_cli::EXIT_OK, "{text}");
    let v: J = serde_json::from_str(&text).unwrap();
    assert_eq!(v["created"], 4, "{v}");
    assert_eq!(std::fs::read(&proj).unwrap(), before, "the input is never touched");
    let loaded = ddc_cli::load::load(&new).unwrap();
    let m = &loaded.model;
    for table in ["wall", "opening", "room", "slab", "stair", "level"] {
        assert_eq!(count(m, table), count(&ed.model, table), "{table} kept");
    }
    assert_eq!(count(m, "solid"), count(&ed.model, "solid") + 4, "the columns");
    let ifc = ddc_io::ifc::export_ifc(m).unwrap();
    let rep = validate_ifc(ifc.as_bytes());
    assert!(rep.is_clean(), "{}", rep.brief(20));
    eprintln!("e2e_goal ddc run: {run_ms:.0} ms");
    assert!(run_ms < 60_000.0, "{run_ms} ms");
}
