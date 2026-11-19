// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Streaming export: entity lines go to an `io::Write` as they are made, so the file's size is not
//! held in memory, and the first write error stops the job. Also what was and was not exported.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufWriter, Write};
use std::path::Path;
use std::sync::Arc;

use ddc_model::Model;
use sha2::{Digest, Sha256};

use super::guids::{ext_kind, Guids};
use super::run;
use crate::ifc::plan_file;
use crate::ifc::schema_spec::IfcSchema;
use crate::ifc::step::{header, Step, EPOCH_STAMP, TRAILER};
use crate::job::{Progress, CANCELLED};

/// Destination of a streaming [`Step`]. `add` has no error path, so an error is kept here, later
/// lines are dropped, and the exporter stops at its next [`Step::alive`].
pub(crate) struct Sink {
    out: BufWriter<Box<dyn Write>>,
    err: Option<String>,
    ctl: Option<Arc<Progress>>,
    bytes: u64,
    /// First entity per quoted GlobalId, which [`Step::find_rooted`] would otherwise scan for.
    rooted: HashMap<String, u32>,
}

impl Sink {
    pub(crate) fn new(out: Box<dyn Write>, file_name: &str, ctl: Option<Arc<Progress>>) -> Self {
        let mut s = Self {
            out: BufWriter::with_capacity(1 << 20, out),
            err: None,
            ctl,
            bytes: 0,
            rooted: HashMap::new(),
        };
        s.raw(&header(file_name, IfcSchema::Ifc4x3, EPOCH_STAMP));
        s
    }

    fn raw(&mut self, text: &str) {
        if self.err.is_some() {
            return;
        }
        match self.out.write_all(text.as_bytes()) {
            Ok(()) => self.bytes += text.len() as u64,
            Err(e) => self.err = Some(e.to_string()),
        }
    }

    pub(crate) fn line(&mut self, n: u32, first: Option<&str>, line: &str) {
        if let Some(c) = &self.ctl {
            if self.err.is_none() && c.is_cancelled() {
                self.err = Some(CANCELLED.into());
            }
            c.add(1);
        }
        if let Some(f) = first.filter(|f| f.len() == 24 && f.starts_with('\'')) {
            self.rooted.entry(f.to_string()).or_insert(n);
        }
        self.raw(line);
        self.raw("\n");
    }

    pub(crate) fn find_rooted(&self, guids: &HashSet<String>) -> HashMap<String, u32> {
        guids
            .iter()
            .filter_map(|g| self.rooted.get(g).map(|n| (g.clone(), *n)))
            .collect()
    }

    pub(crate) fn stopped(&self) -> Option<&str> {
        self.err.as_deref()
    }

    /// Writes the end marker and flushes; any earlier error comes back here.
    fn finish(mut self) -> Result<u64, String> {
        self.raw(TRAILER);
        if let Some(e) = self.err.take() {
            return Err(e);
        }
        self.out.flush().map_err(|e| e.to_string())?;
        Ok(self.bytes)
    }
}

/// Model table of an element kind -> kinds the exporter gives its rows. Used only to report: a
/// table here with rows but no matching export is listed as not written.
const TRACKED: &[(&str, &[&str])] = &[
    ("wall", &["wall"]),
    ("opening", &["opening", "door", "window"]),
    ("slab", &["slab"]),
    ("room", &["space"]),
    ("curtain_wall", &["curtain"]),
    ("grid", &["grid"]),
    ("comp", &["comp"]),
    ("ceiling", &["ceiling"]),
    ("roof", &["roof"]),
    ("mesh", &["mesh"]),
    ("col", &["col"]),
    ("beam", &["beam"]),
    ("mep_run", &["mep-run"]),
    ("ramp", &["ramp"]),
    ("solid", &["solid"]),
    ("zone", &["zone"]),
    ("toposolid", &["toposolid"]),
    ("topo_pad", &["pad"]),
    ("skylight", &["skylight"]),
    ("soffit", &["soffit"]),
    ("sweep", &["sweep"]),
    ("rebar_set", &["rebar"]),
    ("iso_footing", &["g6/footing"]),
    ("strip_footing", &["g6/strip_footing"]),
    ("raft", &["g6/raft"]),
    ("brace", &["g6/brace"]),
    ("truss", &["g6/truss"]),
    ("beam_system", &["g6/beam_system"]),
    ("stair", &["stair"]),
    ("railing", &["railing"]),
    ("mass", &["mass"]),
];

/// Tables whose rows reach the file as generic proxies, not as their own class.
const PROXY: [&str; 2] = ["mesh", "solid"];

/// Export kind and row of a GlobalId request, for the ones that carry the row in the kind text.
fn normal_kind(kind: &str, id: i64) -> Option<(String, i64)> {
    if let Some(rest) = kind.strip_prefix("g6/") {
        let mut it = rest.split('/');
        let (k, row) = (it.next()?, it.next()?.parse().ok()?);
        return Some((format!("g6/{k}"), row));
    }
    if let Some(row) = kind.strip_prefix("rebar-") {
        return Some(("rebar".to_string(), row.parse().ok()?));
    }
    Some((kind.to_string(), id))
}

#[derive(Clone, Debug, PartialEq)]
pub struct NotExported {
    pub table: String,
    pub id: i64,
}

/// What one streaming export did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExportStats {
    pub bytes: u64,
    /// Rows written, by model table.
    pub written: BTreeMap<String, u64>,
    pub elements: u64,
    pub proxies: u64,
    /// Elements under a GlobalId from an imported file / a generated one.
    pub ids_kept: u64,
    pub ids_new: u64,
    /// GlobalIds several rows claimed, resolved by generating a new id.
    pub ids_conflict: u64,
    /// Rows of tracked tables that are not in the file.
    pub not_written: Vec<NotExported>,
    pub warnings: Vec<String>,
    /// GlobalIds of the elements in `written`.
    pub element_guids: Vec<String>,
    /// Every GlobalId in the file.
    pub all_guids: Vec<String>,
}

fn table_ids(model: &Model, table: &str) -> Result<Vec<i64>, String> {
    let e = |e: rusqlite::Error| format!("db: {e}");
    let sql = model.sql();
    let has: i64 = sql
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
            [table],
            |r| r.get(0),
        )
        .map_err(e)?;
    if has == 0 {
        return Ok(vec![]);
    }
    let mut st = sql
        .prepare(&format!("SELECT id FROM {table} ORDER BY id"))
        .map_err(e)?;
    let ids = st
        .query_map([], |r| r.get(0))
        .map_err(e)?
        .collect::<Result<Vec<i64>, _>>()
        .map_err(e)?;
    Ok(ids)
}

fn stats_of(model: &Model, g: &Guids, bytes: u64) -> Result<ExportStats, String> {
    let mut out = ExportStats {
        bytes,
        ..Default::default()
    };
    let mut issued: HashMap<(String, i64), &str> = HashMap::new();
    for (kind, id, guid) in g.issued() {
        out.all_guids.push(guid.to_string());
        if let Some(k) = normal_kind(kind, id) {
            issued.entry(k).or_insert(guid);
        }
    }
    out.all_guids.sort();
    // solids with a semantic class reach the file as that class, not as proxies
    let classed: HashSet<i64> = if table_ids(model, "solid")?.is_empty() {
        HashSet::new()
    } else {
        let e = |e: rusqlite::Error| format!("db: {e}");
        let sql = model.sql();
        let mut st = sql
            .prepare("SELECT id FROM solid WHERE ifc_class IS NOT NULL")
            .map_err(e)?;
        let ids = st.query_map([], |r| r.get(0)).map_err(e)?;
        ids.collect::<Result<_, _>>().map_err(e)?
    };
    for (table, kinds) in TRACKED {
        for id in table_ids(model, table)? {
            let hit = kinds
                .iter()
                .find_map(|k| issued.get(&((*k).to_string(), id)));
            let Some(guid) = hit else {
                out.not_written.push(NotExported {
                    table: (*table).to_string(),
                    id,
                });
                continue;
            };
            *out.written.entry((*table).to_string()).or_default() += 1;
            out.elements += 1;
            if PROXY.contains(table) && !(*table == "solid" && classed.contains(&id)) {
                out.proxies += 1;
            }
            if g.is_imported(guid) {
                out.ids_kept += 1;
            } else {
                out.ids_new += 1;
            }
            out.element_guids.push((*guid).to_string());
        }
    }
    out.warnings = g.warnings.clone();
    out.ids_conflict = out
        .warnings
        .iter()
        .filter(|w| w.contains("GlobalId"))
        .count() as u64;
    Ok(out)
}

/// Exports `model` to `out`. On an error whatever reached `out` is not a valid file: the caller
/// drops it. `file_name` goes into the file header.
pub fn export_ifc_to(
    model: &Model,
    out: Box<dyn Write>,
    file_name: &str,
    ctl: Option<Arc<Progress>>,
) -> Result<ExportStats, String> {
    let step = Step::streaming(Sink::new(out, file_name, ctl));
    let (step, g) = run(model, step)?;
    step.alive()?;
    let sink = step.into_sink().ok_or("export lost its sink")?;
    let bytes = sink.finish()?;
    stats_of(model, &g, bytes)
}

/// [`export_ifc_to`] a new file at `path`. The caller fsyncs and checks it.
pub fn export_ifc_file(
    model: &Model,
    path: &Path,
    file_name: &str,
    ctl: Option<Arc<Progress>>,
) -> Result<ExportStats, String> {
    let f = File::create(path).map_err(|e| e.to_string())?;
    export_ifc_to(model, Box::new(f), file_name, ctl)
}

/// [`export_ifc_file`] with the schema, element types, common property sets and header of
/// `opts`: the same result as [`super::export_ifc_with`], written to a file. The streamed body
/// goes to a `.part` file next to `path` first; `path` appears only when the file is complete.
pub fn export_ifc_file_with(
    model: &Model,
    path: &Path,
    file_name: &str,
    ctl: Option<Arc<Progress>>,
    opts: &super::post::IfcOptions,
) -> Result<ExportStats, String> {
    let part = std::path::PathBuf::from(format!("{}.part", path.display()));
    let done: Result<ExportStats, String> = (|| {
        // IFC2X3 (or the tessellated option) gets no advanced bodies
        let (mut stats, note) =
            super::adv::with_options(opts, || export_ifc_file(model, &part, file_name, ctl))?;
        stats.warnings.extend(note);
        let text = std::fs::read_to_string(&part).map_err(|e| e.to_string())?;
        let salt = super::post::project_salt(model);
        let (out, more) = super::post::apply(&text, opts, file_name, &salt)?;
        stats.warnings.extend(more);
        crate::ifc::check_step(&out)?;
        std::fs::write(path, &out).map_err(|e| e.to_string())?;
        stats.bytes = out.len() as u64;
        Ok(stats)
    })();
    let _ = std::fs::remove_file(&part);
    done
}

/// Exports to `path` and returns each row's GlobalId, by `ext_ref` kind and row id.
pub(in crate::ifc) fn export_rows(
    model: &Model,
    path: &Path,
    file_name: &str,
    ctl: Option<Arc<Progress>>,
) -> Result<HashMap<(String, i64), String>, String> {
    let f = File::create(path).map_err(|e| e.to_string())?;
    let step = Step::streaming(Sink::new(Box::new(f), file_name, ctl));
    let (step, g) = run(model, step)?;
    step.alive()?;
    step.into_sink().ok_or("export lost its sink")?.finish()?;
    let row = |(k, i, u): (&str, i64, &str)| Some(((ext_kind(k)?.to_string(), i), u.to_string()));
    Ok(g.issued().filter_map(row).collect())
}

/// First-argument GlobalIds of a file (rooted entities), by one pass over its lines.
fn rooted_in_file(path: &Path) -> Result<HashSet<String>, String> {
    let f = File::open(path).map_err(|e| e.to_string())?;
    let mut rd = std::io::BufReader::with_capacity(1 << 20, f);
    let mut buf = String::new();
    let mut out = HashSet::new();
    loop {
        buf.clear();
        if rd.read_line(&mut buf).map_err(|e| e.to_string())? == 0 {
            return Ok(out);
        }
        let Some(a) = buf.find('(') else { continue };
        let Some(first) = buf[a + 1..].split(',').next() else {
            continue;
        };
        if first.len() == 24 && first.starts_with('\'') && first.ends_with('\'') {
            out.insert(first[1..23].to_string());
        }
    }
}

/// The deep check before a file replaces an imported source: it reads back through the real
/// importer, every written element's GlobalId is in it, and nothing unknown is.
pub fn verify_ifc_over_source(
    path: &Path,
    stats: &ExportStats,
    ctl: &Arc<Progress>,
) -> Result<(), String> {
    let plan = plan_file(path, ctl).map_err(|e| e.to_string())?;
    let in_file = rooted_in_file(path)?;
    if let Some(g) = stats.element_guids.iter().find(|g| !in_file.contains(*g)) {
        return Err(format!("element {g} is not in the file"));
    }
    let known: HashSet<&str> = stats.all_guids.iter().map(String::as_str).collect();
    if let Some(e) = plan
        .elems
        .iter()
        .find(|e| crate::ifc::is_global_id(&e.guid) && !known.contains(e.guid.as_str()))
    {
        return Err(format!("unexpected element {} in the file", e.guid));
    }
    let seen = plan.walls.len()
        + plan.slabs.len()
        + plan.meshes.len()
        + plan.comps.len()
        + plan.members.len()
        + plan.elems.len();
    if stats.elements > 0 && seen == 0 {
        return Err("the file reads back with no elements".into());
    }
    Ok(())
}

/// SHA-256 of a file, lower-case hex.
pub fn file_sha256(path: &Path) -> Result<String, String> {
    file_sha256_until(path, &|| false).map(|h| h.unwrap_or_default())
}

/// Like `file_sha256`, asking `stop` before each megabyte; `Ok(None)` when it said stop.
pub fn file_sha256_until(path: &Path, stop: &dyn Fn() -> bool) -> Result<Option<String>, String> {
    let mut f =
        std::io::BufReader::with_capacity(1 << 20, File::open(path).map_err(|e| e.to_string())?);
    let mut h = Sha256::new();
    loop {
        if stop() {
            return Ok(None);
        }
        let chunk = f.fill_buf().map_err(|e| e.to_string())?;
        if chunk.is_empty() {
            break;
        }
        h.update(chunk);
        let n = chunk.len();
        f.consume(n);
    }
    let mut out = String::with_capacity(64);
    for b in h.finalize() {
        out.push_str(&format!("{b:02x}"));
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ifc::{check_step, export_ifc, import_ifc};
    use ddc_model::{LayerSpec, WallSpec};
    use std::sync::Mutex;

    fn model() -> (Model, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m
            .add_wall_type(
                "ext",
                &[LayerSpec {
                    material: "brick".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let w = m
            .add_wall(&WallSpec {
                a: (0.0, 0.0),
                b: (4.0, 0.0),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap();
        (m, w)
    }

    #[derive(Clone, Default)]
    struct Shared(Arc<Mutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A disk with room for `left` bytes.
    struct FullDisk {
        left: usize,
    }

    impl Write for FullDisk {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            if self.left == 0 {
                return Err(std::io::Error::other("no space left on device"));
            }
            let n = b.len().min(self.left);
            self.left -= n;
            Ok(n)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn stream(m: &Model) -> (String, ExportStats) {
        let buf = Shared::default();
        let st = export_ifc_to(m, Box::new(buf.clone()), "model.ifc", None).unwrap();
        let text = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        (text, st)
    }

    #[test]
    fn streaming_gives_the_same_bytes_as_the_text_export() {
        let (m, _) = model();
        let (text, st) = stream(&m);
        assert_eq!(text, export_ifc(&m).unwrap());
        assert_eq!(st.bytes, text.len() as u64);
        check_step(&text).unwrap();
    }

    #[test]
    fn a_full_disk_fails_with_its_error_at_any_point() {
        let (m, _) = model();
        let (text, _) = stream(&m);
        // the writer buffer holds 1 MiB: a small file only fails on the final flush
        for left in [0, 1, text.len() / 2, text.len() - 1] {
            let e = export_ifc_to(&m, Box::new(FullDisk { left }), "model.ifc", None).unwrap_err();
            assert!(e.contains("no space"), "{left}: {e}");
        }
    }

    #[test]
    fn cancel_stops_the_export() {
        let (m, _) = model();
        let c = Progress::new();
        c.cancel();
        let e = export_ifc_to(&m, Box::new(Shared::default()), "x", Some(c)).unwrap_err();
        assert_eq!(e, CANCELLED);
    }

    #[test]
    fn elements_are_counted_and_kinds_without_an_exporter_are_reported() {
        let (m, w) = model();
        let (_, st) = stream(&m);
        assert_eq!(st.written.get("wall"), Some(&1));
        assert_eq!(st.elements, 1);
        assert_eq!(st.ids_new, 1);
        assert!(st.not_written.is_empty(), "{:?}", st.not_written);
        m.sql()
            .execute(
                "INSERT INTO ext_ref(source,guid,kind,ref) VALUES ('ifc:p','2kQ$9Xyz0123456789abcd','wall',?1)",
                [w],
            )
            .unwrap();
        let (_, st) = stream(&m);
        assert_eq!(
            (st.ids_kept, st.ids_new),
            (1, 0),
            "an imported id counts as kept"
        );
    }

    #[test]
    fn a_row_that_is_not_built_is_listed_and_does_not_vanish_silently() {
        let (m, _) = model();
        m.sql()
            .execute_batch(
                "INSERT INTO railing(id,level,height,post_spacing,baluster_spacing,post,baluster,
                     rail_w,rail_h,material)
                 VALUES (5,(SELECT min(id) FROM level),0.9,1.0,0.12,0.05,0.02,0.05,0.04,'Steel');",
            )
            .unwrap();
        let (_, st) = stream(&m);
        assert!(st.warnings.iter().any(|w| w.contains("railing 5")));
        assert_eq!(
            st.not_written,
            vec![NotExported {
                table: "railing".into(),
                id: 5
            }]
        );
    }

    #[test]
    fn normal_kinds_find_the_row_in_the_text() {
        assert_eq!(
            normal_kind("g6/raft/7/body", 0),
            Some(("g6/raft".into(), 7))
        );
        assert_eq!(normal_kind("g6/qto/x", 0), None);
        assert_eq!(normal_kind("rebar-12", 0), Some(("rebar".into(), 12)));
        assert_eq!(normal_kind("wall", 5), Some(("wall".into(), 5)));
    }

    #[test]
    fn the_hash_is_of_the_bytes() {
        let p = std::env::temp_dir().join(format!("ddc-o5-hash-{}.txt", std::process::id()));
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(
            file_sha256(&p).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_hash_stops_between_chunks_when_asked() {
        let p = std::env::temp_dir().join(format!("ddc-sv2-hash-{}.bin", std::process::id()));
        std::fs::write(&p, vec![7u8; 5 << 20]).unwrap();
        let polls = std::cell::Cell::new(0);
        let stop = || {
            polls.set(polls.get() + 1);
            polls.get() > 2
        };
        assert_eq!(file_sha256_until(&p, &stop).unwrap(), None);
        assert_eq!(polls.get(), 3, "stopped at the third chunk, not at the end");
        assert!(file_sha256_until(&p, &|| false).unwrap().is_some());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn the_deep_check_reads_a_real_file_back() {
        let (m, _) = model();
        let p = std::env::temp_dir().join(format!("ddc-o5-deep-{}.ifc", std::process::id()));
        let st = export_ifc_file(&m, &p, "x.ifc", None).unwrap();
        verify_ifc_over_source(&p, &st, &Progress::new()).unwrap();
        let mut wrong = st.clone();
        wrong.element_guids.push("2kQ$9Xyz0123456789abcd".into());
        assert!(verify_ifc_over_source(&p, &wrong, &Progress::new()).is_err());
    }

    // ---- identity across saves, contract section 6.8 (run GlobalIds of MEP: x1's own tests)

    fn wall_guid(text: &str) -> String {
        let l = text.lines().find(|l| l.contains("=IFCWALL(")).unwrap();
        let a = l.find('\'').unwrap();
        l[a + 1..a + 23].to_string()
    }

    #[test]
    fn an_imported_element_keeps_its_id_and_export_import_export_is_stable() {
        let (src, _) = model();
        let first = export_ifc(&src).unwrap();
        let mut dst = Model::open(":memory:").unwrap();
        import_ifc(&mut dst, first.as_bytes()).unwrap();
        let second = export_ifc(&dst).unwrap();
        assert_eq!(wall_guid(&first), wall_guid(&second));
        let mut again = Model::open(":memory:").unwrap();
        import_ifc(&mut again, second.as_bytes()).unwrap();
        assert_eq!(wall_guid(&second), wall_guid(&export_ifc(&again).unwrap()));
    }

    #[test]
    fn a_new_element_has_the_same_id_in_every_save() {
        let (m, _) = model();
        assert_eq!(
            wall_guid(&export_ifc(&m).unwrap()),
            wall_guid(&export_ifc(&m).unwrap())
        );
    }

    #[test]
    fn delete_then_undo_keeps_the_id() {
        let (mut m, w) = model();
        let before = wall_guid(&export_ifc(&m).unwrap());
        m.delete_wall(w).unwrap();
        m.undo().unwrap();
        assert_eq!(before, wall_guid(&export_ifc(&m).unwrap()));
    }

    #[test]
    fn a_copy_gets_its_own_id() {
        let (mut m, w) = model();
        let g = "2kQ$9Xyz0123456789abcd";
        m.sql()
            .execute(
                "INSERT INTO ext_ref(source,guid,kind,ref) VALUES ('ifc:p',?1,'wall',?2)",
                rusqlite::params![g, w],
            )
            .unwrap();
        let copied = m
            .copy_elements(&[ddc_model::Target::Wall(w)], &[(0.0, 5.0)])
            .unwrap();
        assert_eq!(copied.len(), 1);
        let text = export_ifc(&m).unwrap();
        let ids: Vec<&str> = text
            .lines()
            .filter(|l| l.contains("=IFCWALL("))
            .map(|l| {
                let a = l.find('\'').unwrap();
                &l[a + 1..a + 23]
            })
            .collect();
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1]);
        assert!(ids.contains(&g));
    }
}
