// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The patch writer in the save pipeline. Any doubt of `ddc_io::ifc::patch`, including a finished
//! patch that fails its read-back, makes this a full rewrite, and the report says which and why.
//! A rewrite never takes the source's place: over the source, a doubt ends in a copy beside it.

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use ddc_io::ifc::patch::{
    base_rebase, check_patched, current_hash, has_changes_with, patch_file, PatchError,
    DOUBT_STRUCTURE,
};
use ddc_io::ifc::{verify_ifc_over_source, ExportStats};
use ddc_model::source::file::hash_file;
use ddc_model::source::SourceRebase;
use ddc_model::Model;

use super::job::{Backend, Rewrite, WritePlan};
use super::SourceRef;
use crate::jobs::{Progress, CANCELLED};

/// Kind column of the report row that announces a fallback.
const FALLBACK_KIND: &str = "patch";
/// Report reason: the source was left alone and the file is a copy beside it.
pub const SOURCE_KEPT: &str = "ifc_patch.fallback.source_kept";

#[derive(Default)]
struct State {
    fell_back: bool,
    rows: Vec<(String, String)>,
    /// The source as this save read it; empty when it is not the file the model came from.
    read_hash: String,
    /// How the base moves if this file takes the source's place.
    rebase: Option<SourceRebase>,
    ctl: Option<Arc<Progress>>,
}

pub struct Patch {
    source: SourceRef,
    over_source: bool,
    state: Mutex<State>,
}

impl Patch {
    pub fn new(source: SourceRef, over_source: bool) -> Self {
        Self {
            source,
            over_source,
            state: Mutex::default(),
        }
    }

    fn set(&self, s: State) {
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = s;
    }

    /// Hash of the source, when it is still the file the model came from.
    fn read_source(&self, snap: &Model, c: &Arc<Progress>) -> String {
        if !self.over_source {
            return String::new();
        }
        current_hash(snap, &self.source.key, &self.source.path, Some(c)).unwrap_or_default()
    }

    fn fell_back(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .fell_back
    }

    /// The patched file, or why a rewrite must make it instead.
    fn try_patch(
        &self,
        snap: &Model,
        out: &Path,
        c: &Arc<Progress>,
    ) -> Result<(ExportStats, Vec<(String, String)>, Option<SourceRebase>), PatchError> {
        let mut p = patch_file(snap, &self.source.key, &self.source.path, out, Some(c))?;
        if let Some(e) = &p.expect {
            if let Err(why) = check_patched(out, e, Some(c)) {
                if c.is_cancelled() {
                    return Err(PatchError::Failed(CANCELLED.into()));
                }
                return Err(PatchError::Doubt {
                    key: DOUBT_STRUCTURE,
                    detail: format!("read-back: {why}"),
                });
            }
        }
        let mut rebase = None;
        if self.over_source {
            // without the move the edits only stay pending, and the next save writes them again
            match base_rebase(snap, &self.source.key, &p) {
                Ok(r) => rebase = Some(r),
                Err(e) => p.stats.warnings.push(format!("source base not moved: {e}")),
            }
        }
        Ok((p.stats, p.will_not, rebase))
    }
}

impl Backend for Patch {
    fn name(&self) -> &'static str {
        "patch"
    }

    fn used(&self) -> &'static str {
        if self.fell_back() {
            "rewrite"
        } else {
            "patch"
        }
    }

    fn late_rows(&self) -> Vec<(String, String)> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .rows
            .clone()
    }

    fn must_keep_target(&self) -> bool {
        let (read, fell_back, ctl) = {
            let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (s.read_hash.clone(), s.fell_back, s.ctl.clone())
        };
        // contract 6.3: a rewrite goes to a copy, whatever the doubt was
        if fell_back || read.is_empty() {
            return true;
        }
        let stop = || ctl.as_ref().is_some_and(|c| c.is_cancelled());
        hash_file(&self.source.path, &stop).map_or(true, |h| h != read)
    }

    fn source_rebase(&self) -> Option<SourceRebase> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .rebase
            .clone()
    }

    fn plan(&self, snap: &Model, c: &Arc<Progress>) -> Result<WritePlan, String> {
        let unchanged = self.over_source
            && matches!(
                has_changes_with(snap, &self.source.key, &self.source.path, Some(c)),
                Ok(false)
            );
        Ok(WritePlan {
            nothing_to_write: unchanged,
            ..Default::default()
        })
    }

    fn write(
        &self,
        plan: &WritePlan,
        snap: &Model,
        out: &Path,
        file_name: &str,
        c: &Arc<Progress>,
    ) -> Result<ExportStats, String> {
        self.set(State::default());
        let read_hash = self.read_source(snap, c);
        let ctl = Some(c.clone());
        match self.try_patch(snap, out, c) {
            Ok((stats, rows, rebase)) => {
                self.set(State {
                    fell_back: false,
                    rows,
                    read_hash,
                    rebase,
                    ctl,
                });
                Ok(stats)
            }
            Err(PatchError::Failed(e)) => Err(e),
            Err(PatchError::Doubt { key, detail }) => {
                let _ = fs::remove_file(out);
                let mut stats = Rewrite.write(plan, snap, out, file_name, c)?;
                stats
                    .warnings
                    .push(format!("patch fell back to rewrite: {detail}"));
                self.set(State {
                    fell_back: true,
                    rows: vec![(FALLBACK_KIND.to_string(), key.to_string())],
                    read_hash,
                    rebase: None,
                    ctl,
                });
                Ok(stats)
            }
        }
    }

    fn verify(
        &self,
        stats: &ExportStats,
        out: &Path,
        over_source: bool,
        c: &Arc<Progress>,
    ) -> Result<(), String> {
        if self.fell_back() {
            return Rewrite.verify(stats, out, over_source, c);
        }
        // the read-back of the patch ran in `write`, where it can still choose a rewrite
        if over_source && !stats.all_guids.is_empty() {
            verify_ifc_over_source(out, stats, c)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::save::job::{run, Outcome, SaveRequest, BACKUP_DIR};
    use crate::save::written_file;
    use std::path::PathBuf;
    use std::time::Duration;

    const FILE: &str = "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC2X3'));\nENDSEC;\nDATA;\n\
#1=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);\n#2=IFCUNITASSIGNMENT((#1));\n\
#3=IFCPROJECT('0000000000000000000001',$,'P',$,$,$,$,$,#2);\nENDSEC;\nEND-ISO-10303-21;\n";

    fn setup(tag: &str) -> (PathBuf, PathBuf, String) {
        let d = std::env::temp_dir().join(format!("ddc-o6-app-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let src = d.join("a.ifc");
        fs::write(&src, FILE).unwrap();
        let db = d.join("work.ddc");
        let mut m = Model::open(db.to_str().unwrap()).unwrap();
        ddc_io::ifc::import_ifc_file(&mut m, &src).unwrap();
        let key = m.sources().unwrap().remove(0).source;
        (d, src, key)
    }

    fn save(db: PathBuf, target: PathBuf, src: &Path, key: &str, over: bool) -> Outcome {
        let source = SourceRef {
            key: key.to_string(),
            path: src.to_path_buf(),
            is_ifc: true,
        };
        let req = SaveRequest {
            snapshot: db,
            target,
            backend: Arc::new(Patch::new(source, over)),
            over_source: over,
            last_hash: String::new(),
            last_journal: None,
            replace_wait: Duration::from_secs(1),
        };
        let c = Progress::new();
        match run(&req, &c, &mut |_| Ok(())) {
            Ok(o) => o,
            Err(f) => panic!("save failed: {}", f.message),
        }
    }

    #[test]
    fn a_stale_source_falls_back_and_the_report_says_so() {
        let (d, src, key) = setup("stale");
        fs::write(&src, FILE.replace("'P'", "'Q'")).unwrap();
        let o = save(d.join("work.ddc"), d.join("out.ifc"), &src, &key, false);
        assert_eq!(o.report.backend, "rewrite");
        assert!(o
            .report
            .not_written
            .iter()
            .any(|r| r.reason == "ifc_patch.fallback.source_changed"));
        assert!(o.report.warnings.iter().any(|w| w.contains("fell back")));
    }

    #[test]
    fn a_source_changed_on_disk_is_not_overwritten_and_a_copy_is_written() {
        let (d, src, key) = setup("kept");
        let theirs = FILE.replace("'P'", "'Q'");
        fs::write(&src, &theirs).unwrap();
        let o = save(d.join("work.ddc"), src.clone(), &src, &key, true);
        assert_eq!(fs::read_to_string(&src).unwrap(), theirs);
        assert!(o.source_kept);
        assert_eq!(
            o.report.target,
            d.join("a-edited.ifc").display().to_string()
        );
        assert!(d.join("a-edited.ifc").is_file());
        assert!(!d.join(BACKUP_DIR).exists());
        assert!(o.report.not_written.iter().any(|r| r.reason == SOURCE_KEPT));
        assert!(o
            .report
            .not_written
            .iter()
            .any(|r| r.reason == "ifc_patch.fallback.source_changed"));
    }

    fn assert_kept_with_a_copy(d: &Path, src: &Path, o: &Outcome, why: &str) {
        assert_eq!(fs::read_to_string(src).unwrap(), FILE, "{why}");
        assert!(o.source_kept, "{why}");
        assert!(o.rebase.is_none(), "{why}: the source did not move");
        assert_eq!(o.report.backend, "rewrite", "{why}");
        assert_eq!(
            written_file(src, o),
            d.join("a-edited.ifc"),
            "{why}: the strip opens the copy"
        );
        assert!(d.join("a-edited.ifc").is_file(), "{why}");
        assert!(!d.join(BACKUP_DIR).exists(), "{why}");
        for reason in [SOURCE_KEPT, why] {
            assert!(
                o.report.not_written.iter().any(|r| r.reason == reason),
                "{reason} in {:?}",
                o.report.not_written
            );
        }
    }

    #[test]
    fn a_doubt_about_shared_rows_over_the_unchanged_source_keeps_it() {
        let (d, src, key) = setup("types");
        let m = Model::open(d.join("work.ddc").to_str().unwrap()).unwrap();
        m.sql()
            .execute("DELETE FROM meta WHERE key LIKE 'src_shared_hash:%'", [])
            .unwrap();
        drop(m);
        let o = save(d.join("work.ddc"), src.clone(), &src, &key, true);
        assert_kept_with_a_copy(&d, &src, &o, ddc_io::ifc::patch::DOUBT_TYPES);
    }

    #[test]
    fn a_second_source_in_the_project_keeps_the_unchanged_source() {
        let (d, src, key) = setup("sources");
        let m = Model::open(d.join("work.ddc").to_str().unwrap()).unwrap();
        m.sql()
            .execute(
                "INSERT INTO src_file(source,file_key,path,format,reader,reader_version,\
                 first_import,imported_at) VALUES ('other','k','o.ifc','ifc','r','1',1,1)",
                [],
            )
            .unwrap();
        drop(m);
        let o = save(d.join("work.ddc"), src.clone(), &src, &key, true);
        assert_kept_with_a_copy(&d, &src, &o, ddc_io::ifc::patch::DOUBT_SOURCES);
    }

    #[test]
    fn a_zipped_source_keeps_its_bytes_and_the_copy_is_plain_ifc() {
        let (d, _, key) = setup("zip");
        let zipped = d.join("a.ifczip");
        fs::write(&zipped, b"PK\x03\x04zipped").unwrap();
        let mut m = Model::open(d.join("work.ddc").to_str().unwrap()).unwrap();
        // content, so the empty-over-full guard (d0b31e36) lets the save reach the patch
        m.create("proxy").unwrap();
        let hash = hash_file(&zipped, &|| false).unwrap();
        assert!(m.note_source_written(&key, &hash).unwrap());
        drop(m);
        let o = save(d.join("work.ddc"), zipped.clone(), &zipped, &key, true);
        assert_eq!(fs::read(&zipped).unwrap(), b"PK\x03\x04zipped");
        assert!(o.source_kept);
        assert_eq!(written_file(&zipped, &o), d.join("a-edited.ifc"));
        let why = |k: &str| o.report.not_written.iter().any(|r| r.reason == k);
        assert!(why(ddc_io::ifc::patch::DOUBT_ZIP));
        assert!(!why("ifc_patch.fallback.source_changed"), "the file did not change");
    }

    #[test]
    fn no_edits_copy_the_source_exactly() {
        let (d, src, key) = setup("copy");
        let o = save(d.join("work.ddc"), d.join("out.ifc"), &src, &key, false);
        assert_eq!(o.report.backend, "patch");
        assert_eq!(fs::read(d.join("out.ifc")).unwrap(), FILE.as_bytes());
    }

    #[test]
    fn no_edits_over_the_source_write_nothing() {
        let (d, src, key) = setup("over");
        let o = save(d.join("work.ddc"), src.clone(), &src, &key, true);
        assert_eq!(o.report.backend, "patch");
        assert!(!d.join(BACKUP_DIR).exists());
        assert_eq!(fs::read(&src).unwrap(), FILE.as_bytes());
    }
}
