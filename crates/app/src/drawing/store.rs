//! Placed drawings in the project file. The list lives in the session (undo included); before
//! every save it is written to the model's `underlay` table and the written file is checked
//! for it, and every open or new model loads the list back. A stored drawing that cannot be
//! read stays in the file untouched and is reported; it is never dropped by a later save.
//!
//! One source of truth for "unsaved": [`Drawings::unsaved_count`] compares each drawing's
//! signature (placement, layers and edit generation, which undo restores) with the one the
//! project file holds. The table is rewritten only when the bytes a save would store differ.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ddc_io::drawing_model::{blob, BlockRules};
use ddc_model::{Model, UnderlayRow};
use ddc_ui::notify::Level;
use serde_json::{json, Value};

use super::{Drawing, Drawings, LayerState};
use crate::shell::EditorApp;

/// Encoded geometry key: edit generation, primitive count and list version. The version is
/// part of it so an edit that forgot to move the generation is still written.
type BlobKey = (u64, usize, u64);

/// What the session knows about the stored copy.
#[derive(Default)]
pub struct Stored {
    /// Model (editor project id) this list belongs to.
    bound: Option<u64>,
    /// What the model's table holds: (list signature, list version, content hash).
    written: Option<(u64, u64, u64)>,
    /// Signature of every drawing the project file holds, by drawing id.
    saved: Option<HashMap<u64, u64>>,
    /// What the save under way wrote: becomes `saved` when it lands.
    saving: Option<HashMap<u64, u64>>,
    /// List signature seen on the last frame: a change is an edit for autosave.
    seen: Option<u64>,
    /// Rows that could not be read: written back as they are.
    unloaded: Vec<UnderlayRow>,
    /// Source file hash by drawing id.
    hashes: HashMap<u64, Option<String>>,
    /// Encoded geometry by drawing id.
    blobs: HashMap<u64, (BlobKey, Arc<Vec<u8>>)>,
}

/// FNV-1a 64 of a file, hex; `None` when it cannot be read.
pub fn file_hash(p: &Path) -> Option<String> {
    let mut f = std::fs::File::open(p).ok()?;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        for &b in &buf[..n] {
            h ^= b as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
    }
    Some(format!("{h:016x}"))
}

fn layers_json(ls: &[LayerState]) -> String {
    let v: Vec<Value> = ls
        .iter()
        .map(|l| {
            json!({
                "name": l.name,
                "visible": l.visible,
                "frozen": l.frozen,
                "file_color": l.file_color,
                "color": l.color,
                "halftone": l.halftone,
                "count": l.count,
            })
        })
        .collect();
    Value::Array(v).to_string()
}

fn rgb(v: &Value) -> Option<[u8; 3]> {
    let a = v.as_array()?;
    let c = |k: usize| a.get(k)?.as_u64().filter(|x| *x <= 255).map(|x| x as u8);
    (a.len() == 3).then_some(())?;
    Some([c(0)?, c(1)?, c(2)?])
}

fn parse_layers(s: &str) -> Result<Vec<LayerState>, String> {
    let v: Vec<Value> = serde_json::from_str(s).map_err(|e| format!("layers: {e}"))?;
    v.iter()
        .map(|l| {
            let flag = |k: &str| l.get(k).and_then(Value::as_bool);
            let name = l.get("name").and_then(Value::as_str);
            let name = name.ok_or_else(|| "layers: no name".to_string())?;
            Ok(LayerState {
                name: name.to_string(),
                visible: flag("visible").unwrap_or(true),
                frozen: flag("frozen").unwrap_or(false),
                file_color: l.get("file_color").and_then(rgb),
                color: l.get("color").and_then(rgb),
                halftone: flag("halftone").unwrap_or(false),
                count: l.get("count").and_then(Value::as_u64).unwrap_or(0) as usize,
            })
        })
        .collect()
}

/// Everything a stored row of `d` depends on; geometry by its edit generation.
fn item_sig(d: &Drawing) -> u64 {
    let mut h = DefaultHasher::new();
    (&d.name, &d.path, d.level, d.visible, d.halftone).hash(&mut h);
    [d.unit, d.offset.x, d.offset.y, d.rot].map(f64::to_bits).hash(&mut h);
    for l in &d.layers {
        (&l.name, l.visible, l.frozen, l.file_color, l.color, l.halftone, l.count).hash(&mut h);
    }
    (d.gen, blob::entity_count(&d.u)).hash(&mut h);
    h.finish()
}

impl Drawings {
    /// Signature of every drawing now, by id.
    fn sig_map(&self) -> HashMap<u64, u64> {
        self.list.iter().map(|d| (d.id, item_sig(d))).collect()
    }

    /// Signature of the whole stored list (order included).
    pub fn stored_sig(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.store.unloaded.len().hash(&mut h);
        for d in &self.list {
            (d.id, item_sig(d)).hash(&mut h);
        }
        h.finish()
    }

    /// Drawings the project file does not hold as they are now: placed, edited, moved or
    /// with layer changes since the last save or open, plus removed ones. Undo back to the
    /// saved state counts as none.
    pub fn unsaved_count(&self) -> usize {
        let Some(saved) = &self.store.saved else {
            return self.list.len();
        };
        let mut live = HashSet::new();
        let mut n = 0;
        for d in &self.list {
            live.insert(d.id);
            if saved.get(&d.id) != Some(&item_sig(d)) {
                n += 1;
            }
        }
        n + saved.keys().filter(|id| !live.contains(*id)).count()
    }

    /// Once a frame: (edited since the last frame, differs from the saved file).
    pub fn watch(&mut self) -> (bool, bool) {
        let sig = self.stored_sig();
        let edited = self.store.seen.is_some_and(|s| s != sig);
        self.store.seen = Some(sig);
        (edited, self.unsaved_count() > 0)
    }

    /// The list as it is now is what the project file holds (just loaded).
    fn mark_saved_now(&mut self) {
        self.store.saved = Some(self.sig_map());
    }

    /// Placed drawings plus the stored ones that could not be read.
    pub fn stored_count(&self) -> usize {
        self.list.len() + self.store.unloaded.len()
    }

    /// Stored rows that could not be read (kept in the file).
    pub fn unloaded(&self) -> &[UnderlayRow] {
        &self.store.unloaded
    }

    fn row(&mut self, k: usize) -> UnderlayRow {
        let version = self.version;
        let d = &self.list[k];
        let hash = self
            .store
            .hashes
            .entry(d.id)
            .or_insert_with(|| d.path.as_deref().and_then(file_hash))
            .clone();
        let n = blob::entity_count(&d.u);
        let key = (d.gen, n, version);
        let geom = match self.store.blobs.get(&d.id) {
            Some((v, b)) if *v == key => b.clone(),
            _ => {
                let b = Arc::new(blob::encode(&d.u));
                self.store.blobs.insert(d.id, (key, b.clone()));
                b
            }
        };
        UnderlayRow {
            name: d.name.clone(),
            source: d.path.as_ref().map(|p| p.display().to_string()),
            source_hash: hash,
            level: d.level,
            unit: d.unit,
            offset: (d.offset.x, d.offset.y),
            rot: d.rot,
            visible: d.visible,
            halftone: d.halftone,
            layers: layers_json(&d.layers),
            entities: n as i64,
            geom: (*geom).clone(),
        }
    }

    /// Rows to store, in list order; unreadable stored rows follow unchanged.
    pub fn rows(&mut self) -> Vec<UnderlayRow> {
        let live: HashSet<u64> = self.list.iter().map(|d| d.id).collect();
        self.store.blobs.retain(|id, _| live.contains(id));
        let mut out: Vec<UnderlayRow> = (0..self.list.len()).map(|k| self.row(k)).collect();
        out.extend(self.store.unloaded.iter().cloned());
        out
    }

    /// Writes the list into `m` unless `m` already holds exactly these rows; `true` when it
    /// wrote. An error means `m` does not hold the drawings: a save must not go on.
    pub fn persist(&mut self, m: &mut Model) -> Result<bool, String> {
        let (sig, version) = (self.stored_sig(), self.version);
        let want = self.stored_count() as i64;
        let holds = |m: &Model| m.underlay_totals().ok().map(|t| t.0) == Some(want);
        if let Some((s, v, _)) = self.store.written {
            if s == sig && v == version && holds(m) {
                return Ok(false);
            }
        }
        let rows = self.rows();
        let h = rows_hash(&rows);
        if self.store.written.is_some_and(|w| w.2 == h) && holds(m) {
            self.store.written = Some((sig, version, h));
            return Ok(false);
        }
        m.set_underlays(&rows).map_err(|e| e.to_string())?;
        self.store.written = Some((sig, version, h));
        Ok(true)
    }

    fn drawing_of(&mut self, r: &UnderlayRow) -> Result<Drawing, String> {
        let u = blob::decode(&r.geom)?;
        let n = blob::entity_count(&u) as i64;
        if n != r.entities {
            return Err(format!("{n} primitives, {} expected", r.entities));
        }
        let layers = parse_layers(&r.layers)?;
        let idx = layers
            .iter()
            .enumerate()
            .map(|(k, l)| (l.name.clone(), k))
            .collect();
        Ok(Drawing {
            id: self.new_id(),
            name: r.name.clone(),
            path: r.source.as_ref().map(PathBuf::from),
            level: r.level,
            u,
            unit: r.unit,
            offset: ddc_geom::v2(r.offset.0, r.offset.1),
            rot: r.rot,
            layers,
            visible: r.visible,
            halftone: r.halftone,
            idx,
            cur_layer: String::new(),
            dirty: false,
            gen: 0,
            saved_gen: 0,
            next_gen: 0,
            parked: HashMap::new(),
        })
    }

    /// Stored rows, placed after anything already in the list; returns the notes to show
    /// (unreadable rows, missing or changed sources).
    pub fn load_rows(&mut self, rows: Vec<UnderlayRow>) -> Vec<(Level, String)> {
        let mut notes = vec![];
        let mut loaded = vec![];
        let mut bytes = vec![];
        for r in rows {
            match self.drawing_of(&r) {
                Ok(d) => {
                    match d.path.as_deref().map(|p| (p.exists(), p)) {
                        Some((false, _)) => notes.push((
                            Level::Info,
                            crate::tr("dlg.dw.store.source_missing", &[("name", d.name.as_str())]),
                        )),
                        Some((true, p)) if r.source_hash.is_some() => {
                            if file_hash(p) != r.source_hash {
                                let name = d.name.as_str();
                                let msg =
                                    crate::tr("dlg.dw.store.source_changed", &[("name", name)]);
                                notes.push((Level::Info, msg));
                            }
                        }
                        _ => {}
                    }
                    self.store.hashes.insert(d.id, r.source_hash.clone());
                    bytes.push((d.id, blob::entity_count(&d.u), r.geom));
                    loaded.push(d);
                }
                Err(why) => {
                    eprintln!("drawing store: row \"{}\" not read: {why}", r.name);
                    notes.push((Level::Warning, unloaded_msg(&crate::tr, &r.name)));
                    self.store.unloaded.push(r);
                }
            }
        }
        self.list.extend(loaded);
        self.touch();
        // the stored bytes stand for the unchanged geometry: no encoding to rewrite it
        let version = self.version;
        for (id, n, geom) in bytes {
            self.store.blobs.insert(id, ((0, n, version), Arc::new(geom)));
        }
        notes
    }

    /// A list for the model `m` (project `id`): what it stores, nothing else.
    pub fn for_model(m: &Model, id: u64, rules: BlockRules) -> (Drawings, Vec<(Level, String)>) {
        let mut ds = Drawings {
            rules,
            ..Default::default()
        };
        let notes = match m.underlays() {
            Ok(rows) => ds.load_rows(rows),
            Err(e) => {
                // the table cannot be read: unbound, so no save can overwrite it
                eprintln!("drawing store: table not read: {e}");
                return (ds, vec![(Level::Error, crate::tr("dlg.dw.store.unreadable", &[]))]);
            }
        };
        ds.store.bound = Some(id);
        ds.mark_saved_now();
        if let Ok(rows) = m.underlays() {
            // what the table holds now is exactly the loaded rows
            let h = rows_hash(&rows);
            ds.store.written = Some((ds.stored_sig(), ds.version, h));
        }
        (ds, notes)
    }
}

fn toast(a: &mut EditorApp, notes: Vec<(Level, String)>) {
    for (lv, msg) in notes {
        a.sh.toasts.push(lv, msg, None);
    }
}

/// The model was replaced by another project (after the unsaved-work answer): the list
/// becomes what the new model stores. Block rules are user settings and stay.
pub fn reload(a: &mut EditorApp) {
    let rules = std::mem::take(&mut a.plan.drawings.rules);
    let (ds, notes) = Drawings::for_model(&a.ed.model, a.ed.project_id(), rules);
    a.plan.drawings = ds;
    toast(a, notes);
}

/// The list belongs to the model in the editor. A model that arrived without [`reload`] (the
/// first one, or one swapped in by a path that kept the session) never costs a placed
/// drawing: an empty list loads what the model stores, a non-empty one is kept and counts as
/// unsaved until the next save writes it.
fn bind(a: &mut EditorApp) -> Result<(), Why> {
    let id = a.ed.project_id();
    if a.plan.drawings.store.bound == Some(id) {
        return Ok(());
    }
    let ds = &mut a.plan.drawings;
    if ds.list.is_empty() && ds.store.unloaded.is_empty() {
        let rows = a.ed.model.underlays().map_err(|e| {
            eprintln!("drawing store: table not read: {e}");
            Why::Read
        })?;
        let notes = a.plan.drawings.load_rows(rows);
        let ds = &mut a.plan.drawings;
        ds.store.bound = Some(id);
        ds.store.written = None;
        ds.mark_saved_now();
        toast(a, notes);
    } else {
        ds.store.bound = Some(id);
        ds.store.written = None;
        ds.store.saved = Some(HashMap::new());
    }
    Ok(())
}

/// Before a save, an autosave or a copy: the model holds every drawing, or the error says why
/// it does not.
pub fn persist(a: &mut EditorApp) -> Result<(), String> {
    #[cfg(test)]
    if FAIL_WRITE.with(std::cell::Cell::get) {
        return Err(not_saved_msg(&crate::tr, Why::Write));
    }
    bind(a).map_err(|w| not_saved_msg(&crate::tr, w))?;
    a.plan.drawings.persist(&mut a.ed.model).map_err(|e| {
        eprintln!("drawing store: not written: {e}");
        not_saved_msg(&crate::tr, Why::Write)
    })?;
    Ok(())
}

#[cfg(test)]
thread_local! {
    /// Tests: the drawing store refuses to write on this thread.
    pub(crate) static FAIL_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Why drawings are not saved; the technical detail goes to the log, not to the user.
#[derive(Clone, Copy)]
enum Why {
    Read,
    Write,
}

type Say<'a> = &'a dyn Fn(&str, &[(&str, &str)]) -> String;

fn not_saved_msg(say: Say, why: Why) -> String {
    let key = match why {
        Why::Read => "dlg.dw.store.why_read",
        Why::Write => "dlg.dw.store.why_write",
    };
    let why = say(key, &[]);
    say("dlg.dw.store.not_saved", &[("why", why.as_str())])
}

fn unloaded_msg(say: Say, name: &str) -> String {
    say("dlg.dw.store.unloaded", &[("name", name)])
}

/// [`persist`] for a save to the project file: what it wrote becomes the saved state once
/// the save lands ([`saved`]).
pub fn begin_save(a: &mut EditorApp) -> Result<(), String> {
    persist(a)?;
    let ds = &mut a.plan.drawings;
    ds.store.saving = Some(ds.sig_map());
    // the plain Save skips a clean project: it must see drawing changes of this frame
    let dirty = ds.unsaved_count() > 0;
    a.ed.set_side_dirty(dirty);
    Ok(())
}

/// The save begun by [`begin_save`] landed in the project file.
pub fn saved(a: &mut EditorApp) {
    if let Some(s) = a.plan.drawings.store.saving.take() {
        a.plan.drawings.store.saved = Some(s);
    }
    let dirty = a.plan.drawings.unsaved_count() > 0;
    a.ed.set_side_dirty(dirty);
}

/// What the saved file must hold, and the message when it does not.
pub fn expected(a: &EditorApp) -> ((i64, i64), String) {
    let totals = a.ed.model.underlay_totals().unwrap_or((-1, -1));
    (totals, crate::tr("dlg.dw.store.missing_in_file", &[]))
}

/// The file at `path` holds `want` drawings and primitives.
pub fn check_file(path: &Path, want: (i64, i64), missing: &str) -> Result<(), String> {
    match ddc_model::saved_underlays(path) {
        Ok(got) if got == want => Ok(()),
        Ok(_) => Err(missing.to_string()),
        Err(e) => {
            eprintln!("drawing store: saved file not read back: {e}");
            Err(missing.to_string())
        }
    }
}

/// Once a frame: drawing changes count as unsaved work until undone or saved, and as edits
/// for the autosave snapshot.
pub fn frame(a: &mut EditorApp) {
    if bind(a).is_err() {
        return;
    }
    let (edited, dirty) = a.plan.drawings.watch();
    if edited {
        a.autosave.note_side_edit();
    }
    a.ed.set_side_dirty(dirty);
}

/// Order-sensitive hash of rows as stored.
fn rows_hash(rows: &[UnderlayRow]) -> u64 {
    let mut h = DefaultHasher::new();
    rows.len().hash(&mut h);
    for r in rows {
        (&r.name, &r.source, &r.source_hash, r.level).hash(&mut h);
        [r.unit, r.offset.0, r.offset.1, r.rot].map(f64::to_bits).hash(&mut h);
        (r.visible, r.halftone, &r.layers, r.entities, &r.geom).hash(&mut h);
    }
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_geom::v2;
    use ddc_io::drawing_model::fixture::sample_plan;

    fn placed() -> Drawings {
        let mut ds = Drawings::default();
        ds.add(&sample_plan(0), "d1", None, 1, None, super::super::Origin::AsIs, v2(2.0, 3.0));
        ds
    }

    #[test]
    fn rows_round_trip_layers_and_placement() {
        let mut ds = placed();
        ds.list[0].rot = 0.3;
        ds.list[0].halftone = true;
        ds.list[0].layers[0].visible = false;
        ds.list[0].layers[0].color = Some([10, 20, 30]);
        let rows = ds.rows();
        let mut back = Drawings::default();
        assert!(back.load_rows(rows).is_empty());
        let (a, b) = (&ds.list[0], &back.list[0]);
        assert_eq!(a.u, b.u);
        assert_eq!(a.layers, b.layers);
        assert_eq!((a.offset, a.rot, a.unit, a.level), (b.offset, b.rot, b.unit, b.level));
        assert_eq!((a.visible, a.halftone, &a.name), (b.visible, b.halftone, &b.name));
        assert_eq!(a.bbox(), b.bbox());
        assert!(!b.shown(&b.layers[0].name));
    }

    #[test]
    fn a_damaged_row_is_kept_and_written_back() {
        let mut ds = placed();
        let mut rows = ds.rows();
        rows[0].geom.truncate(10);
        let mut back = Drawings::default();
        let notes = back.load_rows(rows.clone());
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].0, Level::Warning);
        assert!(back.list.is_empty());
        assert_eq!(back.rows(), rows, "the row goes back as it was");
    }

    fn in_lang(l: ddc_ui::i18n::Lang) -> impl Fn(&str, &[(&str, &str)]) -> String {
        move |k, a| crate::tr_in(l, k, a)
    }

    #[test]
    fn store_messages_are_catalog_text_without_raw_errors() {
        use ddc_ui::i18n::Lang;
        for l in [Lang::En, Lang::Ru] {
            let say = in_lang(l);
            let raw = |s: &str| {
                !s.contains('{') && !s.contains("dlg.dw.") && !s.contains("sqlite") && !s.contains("primitives")
            };
            for w in [Why::Read, Why::Write] {
                let m = not_saved_msg(&say, w);
                assert!(raw(&m), "{m}");
            }
            let m = unloaded_msg(&say, "plan A");
            assert!(raw(&m) && m.contains("plan A"), "{m}");
            let m = say("dlg.dw.store.unreadable", &[]);
            assert!(raw(&m), "{m}");
        }
        let en = not_saved_msg(&in_lang(Lang::En), Why::Write);
        let ru = not_saved_msg(&in_lang(Lang::Ru), Why::Write);
        assert_ne!(en, ru);
    }

    #[test]
    fn signature_moves_with_layer_and_geometry_edits() {
        let mut ds = placed();
        let s0 = ds.stored_sig();
        ds.list[0].layers[0].frozen = !ds.list[0].layers[0].frozen;
        let s1 = ds.stored_sig();
        assert_ne!(s0, s1);
        ds.list[0].mark_edited();
        assert_ne!(s1, ds.stored_sig());
    }

    #[test]
    fn second_save_without_changes_leaves_no_journal_entry() {
        let mut m = Model::open(":memory:").unwrap();
        let mut ds = placed();
        let h0 = m.change_head();
        assert!(ds.persist(&mut m).unwrap());
        assert!(m.underlays_written_since(h0), "written as its own change kind");
        assert!(m.changes_since(h0).is_empty(), "not an element change");
        let head = m.change_head();
        ds.touch();
        assert!(!ds.persist(&mut m).unwrap(), "same content after a version move");
        assert_eq!(m.change_head(), head);
        assert!(!m.underlays_written_since(head));
    }

    #[test]
    fn a_layer_toggle_is_written_by_the_next_save() {
        let mut m = Model::open(":memory:").unwrap();
        let mut ds = placed();
        ds.persist(&mut m).unwrap();
        let head = m.change_head();
        ds.list[0].layers[0].visible = !ds.list[0].layers[0].visible;
        assert!(ds.persist(&mut m).unwrap());
        assert!(m.underlays_written_since(head));
        assert_eq!(m.underlays().unwrap()[0].layers, layers_json(&ds.list[0].layers));
    }

    /// A geometry edit that forgot to move the generation is still written: the list
    /// version is part of the encoded geometry key.
    #[test]
    fn an_edit_without_a_new_generation_is_still_written() {
        let mut m = Model::open(":memory:").unwrap();
        let mut ds = placed();
        ds.persist(&mut m).unwrap();
        let n = ds.list[0].u.lines.len();
        ds.list[0].u.lines.truncate(n - 1);
        ds.touch();
        assert!(ds.persist(&mut m).unwrap());
        let back = blob::decode(&m.underlays().unwrap()[0].geom).unwrap();
        assert_eq!(back.lines.len(), n - 1);
    }

    #[test]
    fn undo_back_to_the_saved_drawings_is_clean_and_edits_count_for_autosave() {
        let mut m = Model::open(":memory:").unwrap();
        placed().persist(&mut m).unwrap();
        let (mut ds, _) = Drawings::for_model(&m, 1, BlockRules::default());
        assert_eq!(ds.watch(), (false, false), "just loaded");
        let was = ds.list[0].layers[0].visible;
        ds.list[0].layers[0].visible = !was;
        assert_eq!(ds.watch(), (true, true), "a layer toggle is an edit");
        assert_eq!(ds.watch(), (false, true));
        let x = ds.list[0].offset.x;
        ds.list[0].offset.x = x + 1.0;
        assert!(ds.watch().0, "a move is an edit");
        ds.list[0].offset.x = x;
        ds.list[0].layers[0].visible = was;
        assert_eq!(ds.watch(), (true, false), "undone: same as saved");
        // a geometry edit and its undo: the generation comes back
        let g = ds.list[0].gen;
        ds.list[0].mark_edited();
        assert_eq!(ds.watch(), (true, true));
        assert_eq!(ds.unsaved_count(), 1);
        ds.list[0].gen = g;
        assert_eq!(ds.watch(), (true, false), "same content as the file");
        assert!(!ds.persist(&mut m).unwrap(), "same content is not rewritten");
    }

    #[test]
    fn placed_and_removed_drawings_are_unsaved() {
        let m = Model::open(":memory:").unwrap();
        let (mut ds, _) = Drawings::for_model(&m, 1, BlockRules::default());
        assert_eq!(ds.unsaved_count(), 0);
        ds.add(&sample_plan(0), "d1", None, 1, None, super::super::Origin::AsIs, v2(0.0, 0.0));
        assert_eq!(ds.unsaved_count(), 1, "placed, not in the file");
        ds.mark_saved_now();
        assert_eq!(ds.unsaved_count(), 0);
        ds.list.clear();
        assert_eq!(ds.unsaved_count(), 1, "removed from the file's list");
    }

    #[test]
    fn persist_writes_only_changes_and_fails_loudly() {
        let mut m = Model::open(":memory:").unwrap();
        let mut ds = placed();
        assert!(ds.persist(&mut m).unwrap());
        assert_eq!(m.underlays().unwrap().len(), 1);
        let head = m.change_head();
        assert!(!ds.persist(&mut m).unwrap());
        assert_eq!(m.change_head(), head, "nothing changed, nothing written");
        ds.list[0].offset.x = f64::NAN;
        assert!(ds.persist(&mut m).is_err());
        assert_eq!(m.underlays().unwrap().len(), 1, "the stored copy stays");
    }
}
