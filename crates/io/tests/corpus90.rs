// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Editing smoke for one imported sample; prints one `LEDGER {json}` line.
//! Ignored: needs DDC_CORPUS_FILE=<path> (.ifc, .dxf; .rvt/.rfa also need DDC_E2E_PACKAGE,
//! DDC_DEV_SEED_FILE, DDC_DEV_KEY_ID_FILE). Without it the test returns at once. Samples stay
//! outside the repo.
//! Phases: import → counts → rendered → per-category edits → undo×N exact → additions referencing
//! imported geometry → save/reopen → IFC export/re-import. Each phase runs under catch_unwind.

use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::time::Instant;

use ddc_io::bridge::{self, package, sig, RunOptions, TrustStore};
use ddc_io::{dxf, ifc};
use ddc_model::{
    DimKind, DimRef, DimSpec, LayerSpec, Model, OpeningSpec, RoomSpec, SlabSpec, Swing, Target,
    WallSpec,
};
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{json, Map, Value};

const NATIVE: &[&str] = &[
    "wall", "opening", "slab", "room", "room_sep", "curtain_wall", "comp", "col", "beam", "roof",
    "stair", "railing", "ceiling", "shaft", "grid", "mep_run", "dim",
];
const DRAWING: &[&str] = &["d_line", "d_polyline", "d_arc", "d_text", "d_insert"];

fn count(m: &Model, sql: &str) -> i64 {
    m.sql().query_row(sql, [], |r| r.get(0)).unwrap_or(-1)
}

fn tables(m: &Model) -> Vec<String> {
    let mut st = m
        .sql()
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap();
    let v = st
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    v
}

fn counts(m: &Model) -> BTreeMap<String, i64> {
    let mut out = BTreeMap::new();
    for t in tables(m) {
        if t == "op" || t == "meta" || t.starts_with("sqlite") {
            continue;
        }
        let n = count(m, &format!("SELECT count(*) FROM \"{t}\""));
        if n > 0 {
            out.insert(t, n);
        }
    }
    out
}

/// Every row of every model table as text (op log excluded): exact-state comparison.
fn dump(m: &Model) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    for t in tables(m) {
        if t == "op" || t == "meta" || t.starts_with("sqlite") {
            continue;
        }
        let mut st = m.sql().prepare(&format!("SELECT * FROM \"{t}\"")).unwrap();
        let n = st.column_count();
        let mut q = st.query([]).unwrap();
        let mut rows = vec![];
        while let Some(r) = q.next().unwrap() {
            let v: Vec<String> = (0..n)
                .map(|i| format!("{:?}", r.get_ref(i).unwrap()))
                .collect();
            rows.push(v.join("|"));
        }
        rows.sort();
        out.insert(t, rows);
    }
    out
}

fn diff(a: &BTreeMap<String, Vec<String>>, b: &BTreeMap<String, Vec<String>>) -> Vec<String> {
    let mut d = vec![];
    for k in a.keys().chain(b.keys()).collect::<std::collections::BTreeSet<_>>() {
        let (x, y) = (a.get(k), b.get(k));
        if x != y {
            let (nx, ny) = (x.map_or(0, Vec::len), y.map_or(0, Vec::len));
            d.push(format!("{k}: {nx}->{ny} rows"));
        }
    }
    d
}

fn ids(m: &Model, sql: &str) -> Vec<i64> {
    let mut st = m.sql().prepare(sql).unwrap();
    let v = st
        .query_map([], |r| r.get::<_, i64>(0))
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    v
}

fn ops(m: &Model) -> i64 {
    count(m, "SELECT count(*) FROM op")
}

fn hex(s: &str) -> Vec<u8> {
    let s = s.trim();
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn sign_text(sk: &SigningKey, id: [u8; 8], data: &[u8], trusted: &str) -> String {
    let s = sk.sign(&sig::blake2b512(data)).to_bytes();
    let mut a = b"ED".to_vec();
    a.extend_from_slice(&id);
    a.extend_from_slice(&s);
    let mut g = s.to_vec();
    g.extend_from_slice(trusted.as_bytes());
    format!(
        "untrusted comment: developer signature\n{}\ntrusted comment: {trusted}\n{}\n",
        sig::b64_encode(&a),
        sig::b64_encode(&sk.sign(&g).to_bytes())
    )
}

/// Same install as bridge_rvt_e2e: private bridges folder, developer-signed manifest.
fn install_bridge(root: &Path) -> Result<(PathBuf, TrustStore), String> {
    let env = |k: &str| std::env::var(k).map_err(|_| format!("{k} not set"));
    let pkg = PathBuf::from(env("DDC_E2E_PACKAGE")?);
    let seed = hex(&std::fs::read_to_string(env("DDC_DEV_SEED_FILE")?).map_err(|e| e.to_string())?);
    let idb = hex(&std::fs::read_to_string(env("DDC_DEV_KEY_ID_FILE")?).map_err(|e| e.to_string())?);
    let sk = SigningKey::from_bytes(&seed.try_into().map_err(|_| "seed")?);
    let id: [u8; 8] = idb.try_into().map_err(|_| "key id")?;
    let _ = std::fs::remove_dir_all(root);
    let bridges = root.join("bridges");
    let dst = bridges.join(pkg.file_name().unwrap());
    std::fs::create_dir_all(&dst).map_err(|e| e.to_string())?;
    let man = std::fs::read(pkg.join(package::MANIFEST)).map_err(|e| e.to_string())?;
    let mf = package::parse_manifest(&man).map_err(|e| format!("{e:?}"))?;
    for f in &mf.files {
        std::fs::copy(pkg.join(&f.path), dst.join(&f.path)).map_err(|e| e.to_string())?;
    }
    std::fs::write(dst.join(package::MANIFEST), &man).map_err(|e| e.to_string())?;
    let signature = sign_text(&sk, id, &man, &package::trusted_comment(&mf));
    std::fs::write(dst.join(package::SIGNATURE), signature).map_err(|e| e.to_string())?;
    let mut pk = b"Ed".to_vec();
    pk.extend_from_slice(&id);
    pk.extend_from_slice(&sk.verifying_key().to_bytes());
    let keys = root.join("config").join("developer_keys.txt");
    let line = format!("untrusted comment: developer key\n{}\n", sig::b64_encode(&pk));
    bridge::confirm_developer_key(&keys, &line).map_err(|e| format!("{e:?}"))?;
    let mut trust = TrustStore::official();
    trust.add_developer(bridge::load_developer_keys(&keys, Some(&bridges)));
    Ok((bridges, trust))
}

fn panic_text(e: Box<dyn std::any::Any + Send>) -> String {
    e.downcast_ref::<String>()
        .cloned()
        .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_else(|| "panic".into())
}

/// Runs a phase; a panic becomes `Err("PANIC: …")`.
fn guard<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(e) => Err(format!("PANIC: {}", panic_text(e))),
    }
}

/// Imports `path` into `m`; returns (warnings, skipped map, extra report fields).
fn import(m: &mut Model, path: &Path, root: &Path) -> Result<(Vec<String>, BTreeMap<String, usize>, Value), String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "ifc" => {
            let r = ifc::import_ifc_file(m, path).map_err(|e| format!("{e:?}"))?;
            Ok((vec![], r.skipped.clone(), json!({"schema": r.schema, "length_unit": r.length_unit})))
        }
        "dxf" => {
            let doc = dxf::read_file(path).map_err(|e| format!("{e:?}"))?;
            let r = dxf::import_into(m, &doc).map_err(|e| format!("{e:?}"))?;
            let mut sk = BTreeMap::new();
            if r.skipped_unknown > 0 {
                sk.insert("unknown entities".into(), r.skipped_unknown);
            }
            if r.skipped_inserts > 0 {
                sk.insert("inserts".into(), r.skipped_inserts);
            }
            Ok((vec![], sk, json!({"circles": r.circles, "attributes": r.attributes})))
        }
        "rvt" | "rfa" => {
            let (bridges, trust) = install_bridge(root)?;
            let found = bridge::discover(&bridges, &trust);
            let b = bridge::for_ext(&found, &ext).ok_or("bridge not trusted")?;
            let r = bridge::import_trusted(m, b, &trust, path, &RunOptions::default(), &mut |_| {})
                .map_err(|e| e.to_string())?;
            Ok((r.warnings.clone(), r.skipped.clone(), json!({"warning_count": r.warning_count, "meshes": r.meshes})))
        }
        e => Err(format!("unsupported extension .{e}")),
    }
}

/// Elements without any drawable geometry, per category.
fn unrendered(m: &Model) -> Map<String, Value> {
    let mut out = Map::new();
    let mut in_plan = std::collections::HashSet::new();
    for lv in ids(m, "SELECT id FROM level") {
        let spec = ddc_model::PlanViewSpec { level: lv, cut_height: 1.0, grids: vec![] };
        if let Ok(v) = m.plan_view(&spec) {
            for p in &v.prims {
                if let ddc_model::ViewPrim::Cut { wall, .. } = p {
                    in_plan.insert(*wall);
                }
            }
        }
    }
    let walls = ids(m, "SELECT id FROM wall");
    let no3d: Vec<i64> = ids(m, "SELECT w.id FROM wall w WHERE NOT EXISTS (SELECT 1 FROM wall_qty q WHERE q.wall=w.id AND q.volume>0)");
    let bad_w = walls.iter().filter(|w| !in_plan.contains(w) && no3d.contains(w)).count();
    out.insert("wall".into(), json!(bad_w));
    out.insert("wall_not_in_plan_1m".into(), json!(walls.iter().filter(|w| !in_plan.contains(w)).count()));
    let mut bad_s = 0;
    for s in ids(m, "SELECT id FROM slab") {
        if !m.slab_mesh(s).map(|x| x.volume() > 0.0).unwrap_or(false) {
            bad_s += 1;
        }
    }
    out.insert("slab".into(), json!(bad_s));
    out.insert("mesh".into(), json!(count(m, "SELECT count(*) FROM mesh WHERE length(positions)=0 OR length(indices)=0")));
    out.insert("room_zero_area".into(), json!(count(m, "SELECT count(*) FROM room r WHERE NOT EXISTS (SELECT 1 FROM room_qty q WHERE q.room=r.id AND q.area>0)")));
    out.insert("opening_orphan".into(), json!(count(m, "SELECT count(*) FROM opening o WHERE NOT EXISTS (SELECT 1 FROM wall w WHERE w.id=o.wall)")));
    out
}

fn step(log: &mut Map<String, Value>, name: &str, r: Result<(), String>) {
    log.insert(name.into(), match r {
        Ok(()) => json!("ok"),
        Err(e) => json!(e.chars().take(200).collect::<String>()),
    });
}

fn wall_row(m: &Model, id: i64) -> Option<(i64, i64, (f64, f64), (f64, f64), f64)> {
    m.sql()
        .query_row("SELECT level, wtype, ax, ay, bx, by, height FROM wall WHERE id=?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?, (r.get(2)?, r.get(3)?), (r.get(4)?, r.get(5)?), r.get(6)?))
        })
        .ok()
}

/// select → move → rotate → copy → change type/param → delete for samples of each category.
fn edits(m: &mut Model) -> Map<String, Value> {
    let mut out = Map::new();
    let e = |r: Result<(), ddc_model::Error>| r.map_err(|e| e.to_string());
    let cats: Vec<(&str, Vec<Target>)> = vec![
        ("wall", ids(m, "SELECT id FROM wall ORDER BY id LIMIT 2").into_iter().map(Target::Wall).collect()),
        ("opening", ids(m, "SELECT id FROM opening ORDER BY id LIMIT 2").into_iter().map(Target::Opening).collect()),
        ("slab", ids(m, "SELECT id FROM slab ORDER BY id LIMIT 2").into_iter().map(Target::Slab).collect()),
        ("room", ids(m, "SELECT id FROM room ORDER BY id LIMIT 2").into_iter().map(Target::Room).collect()),
        ("curtain_wall", ids(m, "SELECT id FROM curtain_wall ORDER BY id LIMIT 2").into_iter().map(Target::Curtain).collect()),
        ("comp", ids(m, "SELECT id FROM comp ORDER BY id LIMIT 2").into_iter().map(Target::Component).collect()),
        ("dim", ids(m, "SELECT id FROM dim ORDER BY id LIMIT 2").into_iter().map(Target::Dim).collect()),
    ];
    for (cat, ts) in cats {
        if ts.is_empty() {
            continue;
        }
        let mut log = Map::new();
        let one = &ts[..1];
        let c = (0.0, 0.0);
        step(&mut log, "move", guard(|| e(m.move_elements(one, (0.5, 0.25)))));
        step(&mut log, "rotate", guard(|| e(m.rotate_elements(one, c, std::f64::consts::FRAC_PI_6, false).map(|_| ()))));
        step(&mut log, "copy", guard(|| e(m.copy_elements(&ts, &[(3.0, 0.0)]).map(|_| ()))));
        match one[0] {
            Target::Wall(w) => {
                let other = ids(m, &format!("SELECT id FROM wall_type WHERE id <> (SELECT wtype FROM wall WHERE id={w}) LIMIT 1"));
                step(&mut log, "type", guard(|| match other.first() {
                    Some(&t) => e(m.set_wall_type(w, t)),
                    None => Err("no other wall type".into()),
                }));
                step(&mut log, "param", guard(|| {
                    let (_, _, a, b, _) = wall_row(m, w).ok_or("wall gone")?;
                    e(m.set_wall_axis(w, a, (b.0 + 0.1, b.1)))
                }));
            }
            Target::Opening(o) => {
                step(&mut log, "param", guard(|| e(m.set_opening_swing(o, Swing { side: true, hinge: true }))));
            }
            Target::Slab(s) => {
                step(&mut log, "param", guard(|| e(m.set_slab_slope(s, None))));
            }
            _ => {}
        }
        let del = guard(|| match one[0] {
            Target::Wall(i) => e(m.delete_wall(i)),
            Target::Opening(i) => e(m.delete_opening(i)),
            Target::Slab(i) => e(m.delete_slab(i)),
            Target::Room(i) => e(m.delete_room(i)),
            Target::Curtain(i) => e(m.delete_curtain_wall(i)),
            Target::Component(i) => e(m.delete_component(i)),
            Target::Dim(i) => e(m.delete_dim(i)),
            _ => Err("no delete".into()),
        });
        step(&mut log, "delete", del);
        out.insert(cat.into(), Value::Object(log));
    }
    // drawing entities: only style/delete exist in the model API
    let line = ids(m, "SELECT id FROM d_line LIMIT 1");
    if let Some(&l) = line.first() {
        let mut log = Map::new();
        step(&mut log, "move", Err("no move/rotate/copy API for drawing entities".into()));
        step(&mut log, "delete", guard(|| e(m.delete_entity(ddc_model::Entity::Line, l))));
        out.insert("d_line".into(), Value::Object(log));
    }
    out
}

/// New elements referencing an imported wall (host/snap to its end and faces).
fn additions(m: &mut Model) -> Map<String, Value> {
    let mut log = Map::new();
    let e = |r: Result<i64, ddc_model::Error>| r.map(|_| ()).map_err(|e| e.to_string());
    let w = ids(m, "SELECT id FROM wall ORDER BY (bx-ax)*(bx-ax)+(by-ay)*(by-ay) DESC LIMIT 1");
    let host = w.first().and_then(|&w| wall_row(m, w).map(|r| (w, r)));
    let lv = ids(m, "SELECT id FROM level ORDER BY elevation LIMIT 1").first().copied();
    let Some(level) = host.map(|h| h.1 .0).or(lv) else {
        step(&mut log, "all", Err("no level in model".into()));
        return log;
    };
    let (a, b, h, wt, wid) = match host {
        Some((w, (_, t, a, b, h))) => (a, b, h, t, Some(w)),
        None => {
            let t = ids(m, "SELECT id FROM wall_type LIMIT 1").first().copied();
            let t = match t {
                Some(t) => t,
                None => match m.add_wall_type("Corpus 200", &[LayerSpec { material: "Concrete".into(), thickness: 0.2 }]) {
                    Ok(t) => t,
                    Err(er) => {
                        step(&mut log, "wall_type", Err(er.to_string()));
                        return log;
                    }
                },
            };
            ((0.0, 0.0), (5.0, 0.0), 3.0, t, None)
        }
    };
    log.insert("host".into(), json!(if wid.is_some() { "imported wall" } else { "none (fresh)" }));
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = (dx * dx + dy * dy).sqrt().max(1e-9);
    let (nx, ny) = (-dy / len, dx / len);
    let h = if h > 0.5 { h } else { 3.0 };
    step(&mut log, "wall", guard(|| e(m.add_wall(&WallSpec {
        a: b, b: (b.0 + nx * 3.0, b.1 + ny * 3.0), level, base_offset: 0.0, height: h, wall_type: wt, offset0: None,
    }))));
    if let Some(w) = wid {
        step(&mut log, "door", guard(|| e(m.add_opening(w, &OpeningSpec {
            along: (len / 2.0 - 0.45).max(0.05), width: 0.9_f64.min(len * 0.5), sill: 0.0, height: 2.1_f64.min(h * 0.9), kind: "door".into(),
        }))));
        step(&mut log, "dimension", guard(|| e(m.add_dim(&DimSpec {
            level, kind: DimKind::Linear,
            refs: vec![DimRef::WallEnd { wall: w, face: 0, end: 0 }, DimRef::WallEnd { wall: w, face: 0, end: 1 }],
            dir: (0.0, 0.0), side: 1.0, offset: 1.0, tier: 0, auto: false,
        }))));
    }
    let st = ids(m, "SELECT id FROM slab_type LIMIT 1").first().copied();
    let st = match st {
        Some(s) => Ok(s),
        None => m.add_slab_type("Corpus slab 200", &[LayerSpec { material: "Concrete".into(), thickness: 0.2 }]).map_err(|e| e.to_string()),
    };
    let (x0, x1) = (a.0.min(b.0) - 1.0, a.0.max(b.0) + 1.0);
    let (y0, y1) = (a.1.min(b.1) - 1.0, a.1.max(b.1) + 1.0);
    step(&mut log, "slab", guard(|| {
        let st = st.clone()?;
        e(m.add_slab(&SlabSpec { level, slab_type: st, offset: 0.0, outer: vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)], holes: vec![] }))
    }));
    step(&mut log, "room", guard(|| e(m.add_room(&RoomSpec {
        level, name: "Corpus room".into(), number: "C1".into(),
        point: ((a.0 + b.0) / 2.0 + nx * 0.5, (a.1 + b.1) / 2.0 + ny * 0.5), height: h,
    }))));
    log
}

fn run(path: &Path) -> Value {
    let mut rec = Map::new();
    rec.insert("file".into(), json!(path.to_string_lossy()));
    rec.insert("size".into(), json!(std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)));
    let work = std::env::temp_dir().join(format!("ddc-corpus90-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&work);
    let mut m = Model::open(":memory:").unwrap();
    let t = Instant::now();
    let imp = guard(|| import(&mut m, path, &work.join("bridge")));
    rec.insert("import_secs".into(), json!((t.elapsed().as_secs_f64() * 1000.0).round() / 1000.0));
    let (warnings, skipped, extra) = match imp {
        Ok(x) => x,
        Err(er) => {
            rec.insert("import".into(), json!(er.chars().take(400).collect::<String>()));
            return Value::Object(rec);
        }
    };
    rec.insert("import".into(), json!("ok"));
    rec.insert("report".into(), extra);
    let mut codes: BTreeMap<String, usize> = BTreeMap::new();
    for w in &warnings {
        *codes.entry(w.split(':').next().unwrap_or(w).trim().to_string()).or_default() += 1;
    }
    rec.insert("warnings".into(), json!(codes));
    rec.insert("skipped".into(), json!(skipped));
    let c = counts(&m);
    let native: i64 = NATIVE.iter().filter_map(|k| c.get(*k)).sum();
    let mesh = c.get("mesh").copied().unwrap_or(0);
    let drawing: i64 = DRAWING.iter().filter_map(|k| c.get(*k)).sum();
    let skip: i64 = skipped.values().map(|&v| v as i64).sum();
    let total = native + mesh + skip;
    rec.insert("native".into(), json!(native));
    rec.insert("mesh".into(), json!(mesh));
    rec.insert("drawing".into(), json!(drawing));
    rec.insert("skipped_total".into(), json!(skip));
    rec.insert("native_pct".into(), json!(if total > 0 { (1000.0 * native as f64 / total as f64).round() / 10.0 } else { -1.0 }));
    let mut mesh_cat = BTreeMap::new();
    if let Ok(mut st) = m.sql().prepare("SELECT category, count(*) FROM mesh GROUP BY category") {
        for (k, n) in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))).unwrap().flatten() {
            mesh_cat.insert(k, n);
        }
    }
    rec.insert("mesh_categories".into(), json!(mesh_cat));
    rec.insert("counts".into(), json!(c));
    rec.insert("unrendered".into(), match guard(|| Ok(unrendered(&m))) {
        Ok(u) => Value::Object(u),
        Err(er) => json!(er),
    });
    rec.insert("integrity_issues".into(), json!(guard(|| m.check_integrity().map(|v| v.len()).map_err(|e| e.to_string())).unwrap_or(usize::MAX)));

    // edits, then undo everything back to the imported state
    let base = dump(&m);
    let ops0 = ops(&m);
    let ed = edits(&mut m);
    rec.insert("edits".into(), Value::Object(ed));
    let n = ops(&m) - ops0;
    let mut undo_err = None;
    for _ in 0..n {
        if let Err(er) = guard(|| m.undo().map_err(|e| e.to_string())) {
            undo_err = Some(er);
            break;
        }
    }
    let d = diff(&base, &dump(&m));
    rec.insert("undo".into(), json!({"ops": n, "error": undo_err, "exact": d.is_empty(), "diff": d.iter().take(8).collect::<Vec<_>>()}));

    let add = additions(&mut m);
    rec.insert("additions".into(), Value::Object(add));

    // save → reopen → compare
    let file = work.join("saved.ddc");
    let _ = std::fs::remove_file(&file);
    let before = dump(&m);
    let sv = guard(|| {
        m.snapshot_to(&file).map_err(|e| e.to_string())?;
        let r = Model::open(file.to_str().unwrap()).map_err(|e| e.to_string())?;
        Ok(diff(&before, &dump(&r)))
    });
    rec.insert("save_reopen".into(), match sv {
        Ok(d) => json!({"exact": d.is_empty(), "diff": d.iter().take(8).collect::<Vec<_>>()}),
        Err(er) => json!({"error": er}),
    });

    // export IFC → re-import → counts stable
    let key = ["wall", "opening", "slab", "room", "curtain_wall", "level", "comp", "col", "beam", "roof", "stair"];
    let ex = guard(|| {
        let t = Instant::now();
        let text = ifc::export_ifc(&m)?;
        let secs = t.elapsed().as_secs_f64();
        let p = work.join("rt.ifc");
        std::fs::write(&p, &text).map_err(|e| e.to_string())?;
        let mut r = Model::open(":memory:").unwrap();
        ifc::import_ifc_file(&mut r, &p).map_err(|e| format!("{e:?}"))?;
        let (a, b) = (counts(&m), counts(&r));
        let mut diffs = Map::new();
        for k in key {
            let (x, y) = (a.get(k).copied().unwrap_or(0), b.get(k).copied().unwrap_or(0));
            if x != y {
                diffs.insert(k.into(), json!([x, y]));
            }
        }
        let lost_mesh = a.get("mesh").copied().unwrap_or(0);
        Ok(json!({"bytes": text.len(), "secs": (secs * 1000.0).round() / 1000.0, "stable": diffs.is_empty(), "diff": diffs, "mesh_in_model": lost_mesh, "mesh_after": b.get("mesh").copied().unwrap_or(0)}))
    });
    rec.insert("ifc_roundtrip".into(), ex.unwrap_or_else(|er| json!({"error": er})));
    let _ = std::fs::remove_dir_all(&work);
    Value::Object(rec)
}

/// The corpus file from the env value; unset or blank means skip.
fn corpus_file(v: Option<String>) -> Option<String> {
    v.filter(|s| !s.trim().is_empty())
}

#[test]
fn corpus90_skips_without_env_value() {
    assert_eq!(corpus_file(None), None);
    assert_eq!(corpus_file(Some("  ".into())), None);
    assert_eq!(corpus_file(Some("a.ifc".into())).as_deref(), Some("a.ifc"));
}

#[test]
#[ignore = "needs DDC_CORPUS_FILE"]
fn corpus90_sample() {
    let Some(file) = corpus_file(std::env::var("DDC_CORPUS_FILE").ok()) else {
        eprintln!("corpus90: DDC_CORPUS_FILE not set, skipped");
        return;
    };
    std::panic::set_hook(Box::new(|_| {}));
    let v = run(Path::new(&file));
    println!("LEDGER {}", serde_json::to_string(&v).unwrap());
}
