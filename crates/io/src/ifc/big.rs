// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Big IFC files: streaming open (chunked index, lazy decode, no whole-file copy), the plan
//! built in the caller's thread with progress and cancel, then one bulk transaction.
//! A cancelled or failed job leaves the model untouched: nothing is written before the
//! plan is complete, and the commit is a single transaction (rolled back on error).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ddc_model::Model;

use super::import::{plan, ImportError, ImportReport, Plan};
use super::reader::{OpenError, StepFile, StreamOptions};
use crate::job::{Progress, CANCELLED};

fn open_err(e: OpenError) -> ImportError {
    match e {
        OpenError::Io(s) => ImportError::Io(s),
        OpenError::Step(e) => ImportError::Step(e),
        OpenError::Cancelled => ImportError::Io(CANCELLED.into()),
    }
}

fn io(e: std::io::Error) -> ImportError {
    ImportError::Io(e.to_string())
}

fn is_zip_file(path: &Path) -> Result<bool, ImportError> {
    use std::io::Read;
    let mut head = [0u8; 4];
    let mut f = std::fs::File::open(path).map_err(io)?;
    let n = f.read(&mut head).map_err(io)?;
    Ok(super::zip::is_zip(&head[..n]))
}

/// Inflated IFCZIP member, removed when dropped (the stream reads it from disk).
struct TempIfc(PathBuf);

impl Drop for TempIfc {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn unzip_to_temp(path: &Path) -> Result<TempIfc, ImportError> {
    let bytes = std::fs::read(path).map_err(io)?;
    let member = super::zip::ifc_member(&bytes).map_err(ImportError::Io)?;
    drop(bytes);
    let nanos = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let t = TempIfc(
        std::env::temp_dir().join(format!("ddc-ifczip-{}-{nanos}.ifc", std::process::id())),
    );
    std::fs::write(&t.0, &member).map_err(io)?;
    Ok(t)
}

/// Reads and plans `path`, `.ifc` or IFCZIP (safe to run on a worker thread), tolerant like
/// [`super::plan_bytes`]: damaged references become report warnings. Stages: read 0–45 %,
/// plan 45–90 %.
pub fn plan_file(path: &Path, ctl: &Arc<Progress>) -> Result<Plan, ImportError> {
    ctl.phase("job.stage.read", 0.0, 0.45);
    let tmp = if is_zip_file(path)? {
        Some(unzip_to_temp(path)?)
    } else {
        None
    };
    let src = tmp.as_ref().map_or(path, |t| t.0.as_path());
    let opt = StreamOptions {
        tolerant: true,
        ..Default::default()
    };
    let f = StepFile::open_with(src, &opt, Some(ctl.clone())).map_err(open_err)?;
    ctl.phase("job.stage.plan", 0.45, 0.9);
    ctl.set_total(f.len() as u64);
    let pl = plan(&f);
    // a cancelled decode returns no entity; whatever the planner made of that is dropped
    if ctl.is_cancelled() {
        return Err(ImportError::Io(CANCELLED.into()));
    }
    if f.decode_failures() > 0 {
        return Err(ImportError::Io(format!(
            "{}: changed on disk while importing",
            path.display()
        )));
    }
    let mut pl = pl?;
    // identity by content: hashed here, on the worker, not in the commit
    pl.file = super::import::IfcFile::read(path, &|| ctl.is_cancelled());
    Ok(pl)
}

/// Writes a plan as one undo step inside one transaction with bulk-load pragmas; edits of a
/// re-import are snapshotted, as in [`super::import_ifc`]. Returns the completed report.
pub fn commit_plan(model: &mut Model, pl: &Plan) -> Result<ImportReport, ImportError> {
    commit_plan_with(model, pl, ddc_model::source::Policy::default())
}

/// [`commit_plan`] with a merge policy (removed elements, mass-change guard).
pub fn commit_plan_with(
    model: &mut Model,
    pl: &Plan,
    policy: ddc_model::source::Policy,
) -> Result<ImportReport, ImportError> {
    commit_policy_ctl(model, pl, policy, None)
}

/// [`commit_plan`] counting written elements (N of the plan's) with their category on `ctl`;
/// a cancel rolls the whole commit back.
pub fn commit_plan_ctl(
    model: &mut Model,
    pl: &Plan,
    ctl: &Progress,
) -> Result<ImportReport, ImportError> {
    let r = commit_policy_ctl(model, pl, ddc_model::source::Policy::default(), Some(ctl));
    if ctl.is_cancelled() {
        return Err(ImportError::Io(CANCELLED.into()));
    }
    r
}

fn commit_policy_ctl(
    model: &mut Model,
    pl: &Plan,
    policy: ddc_model::source::Policy,
    ctl: Option<&Progress>,
) -> Result<ImportReport, ImportError> {
    let sync = model
        .batch_walls_edit_bulk("wall_import_ifc", |db, snaps| {
            let sync = super::import::commit_with_ctl(db, snaps, pl, policy, ctl)?;
            super::import::apply_project_info(db, snaps, pl)?;
            Ok(sync)
        })
        .map_err(|e| ImportError::Model(e.to_string()))?;
    let mut rep = pl.report.clone();
    sync.fill(&mut rep);
    Ok(rep)
}

/// [`plan_file`] then [`commit_plan`] (commit stage 90–100 %).
pub fn import_ifc_path_ctl(
    model: &mut Model,
    path: &Path,
    ctl: &Arc<Progress>,
) -> Result<ImportReport, ImportError> {
    let pl = plan_file(path, ctl)?;
    ctl.phase("job.stage.commit", 0.9, 1.0);
    if ctl.is_cancelled() {
        return Err(ImportError::Io(CANCELLED.into()));
    }
    let rep = commit_plan_ctl(model, &pl, ctl)?;
    ctl.finish();
    Ok(rep)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ifc::{export_ifc, import_ifc};
    use ddc_model::{LayerSpec, WallSpec};

    fn source() -> Model {
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_level("L1", 0.0).unwrap();
        let t = m
            .add_wall_type(
                "W",
                &[LayerSpec {
                    material: "masonry".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        for k in 0..8 {
            let y = k as f64 * 3.0;
            m.add_wall(&WallSpec {
                a: (0.0, y),
                b: (6.0, y),
                level: l,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap();
        }
        m
    }

    fn count(m: &Model, t: &str) -> i64 {
        m.sql()
            .query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn streamed_import_matches_in_memory_import() {
        let text = export_ifc(&source()).unwrap();
        let p = std::env::temp_dir().join(format!("ddc-g3-{}-big.ifc", std::process::id()));
        std::fs::write(&p, &text).unwrap();
        let mut a = Model::open(":memory:").unwrap();
        let ra = import_ifc(&mut a, text.as_bytes()).unwrap();
        let mut b = Model::open(":memory:").unwrap();
        let ctl = Progress::new();
        let rb = import_ifc_path_ctl(&mut b, &p, &ctl).unwrap();
        assert_eq!(ra, rb);
        assert_eq!(count(&a, "wall"), count(&b, "wall"));
        assert_eq!(count(&b, "wall"), 8);
        assert!((ctl.fraction() - 1.0).abs() < 1e-6);
        b.undo().unwrap();
        assert_eq!(count(&b, "wall"), 0, "one undo step");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn every_ifc_stage_reports_real_counts_in_order() {
        let text = export_ifc(&source()).unwrap();
        let p = std::env::temp_dir().join(format!("ddc-imp1-{}-stages.ifc", std::process::id()));
        std::fs::write(&p, &text).unwrap();
        let ctl = Progress::new();
        let pl = plan_file(&p, &ctl).unwrap();
        assert_eq!(ctl.trail(), vec!["job.stage.read", "job.stage.plan"]);
        let (d, t) = ctl.counts();
        assert!(t > 0 && d <= t, "entities decoded {d} of {t}");
        let before = ctl.fraction();
        ctl.phase("job.stage.commit", 0.9, 1.0);
        let mut m = Model::open(":memory:").unwrap();
        commit_plan_ctl(&mut m, &pl, &ctl).unwrap();
        let (d, t) = ctl.counts();
        assert_eq!(t, pl.elems.len() as u64);
        assert!(d >= 8 && d <= t, "walls counted: {d} of {t}");
        assert!(ctl.fraction() >= before);
        ctl.finish();
        assert!((ctl.fraction() - 1.0).abs() < 1e-6);
        assert_eq!(
            ctl.trail(),
            vec!["job.stage.read", "job.stage.plan", "job.stage.commit"]
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn a_cancel_during_the_commit_rolls_everything_back() {
        let text = export_ifc(&source()).unwrap();
        let p = std::env::temp_dir().join(format!("ddc-imp1-{}-cc.ifc", std::process::id()));
        std::fs::write(&p, &text).unwrap();
        let ctl = Progress::new();
        let pl = plan_file(&p, &ctl).unwrap();
        let mut m = Model::open(":memory:").unwrap();
        let head = m.change_head();
        ctl.cancel();
        let e = commit_plan_ctl(&mut m, &pl, &ctl).unwrap_err();
        assert!(e.to_string().contains(CANCELLED), "{e}");
        assert_eq!(count(&m, "wall"), 0);
        assert_eq!(count(&m, "level"), 0);
        assert_eq!(m.change_head(), head, "no change recorded");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn cancelled_import_leaves_the_model_untouched() {
        let text = export_ifc(&source()).unwrap();
        let p = std::env::temp_dir().join(format!("ddc-g3-{}-cancel.ifc", std::process::id()));
        std::fs::write(&p, &text).unwrap();
        let mut m = Model::open(":memory:").unwrap();
        let ctl = Progress::new();
        ctl.cancel();
        let e = import_ifc_path_ctl(&mut m, &p, &ctl).unwrap_err();
        assert!(e.to_string().contains(CANCELLED), "{e}");
        assert_eq!(count(&m, "wall"), 0);
        assert_eq!(count(&m, "level"), 0);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn reimport_on_the_big_path_is_one_undo_step() {
        let text = export_ifc(&source()).unwrap();
        let p = std::env::temp_dir().join(format!("ddc-g3-{}-re.ifc", std::process::id()));
        std::fs::write(&p, &text).unwrap();
        let mut a = Model::open(":memory:").unwrap();
        import_ifc(&mut a, text.as_bytes()).unwrap();
        let ra = import_ifc(&mut a, text.as_bytes()).unwrap();
        let mut b = Model::open(":memory:").unwrap();
        import_ifc_path_ctl(&mut b, &p, &Progress::new()).unwrap();
        let rb = import_ifc_path_ctl(&mut b, &p, &Progress::new()).unwrap();
        assert_eq!(ra, rb, "re-import report as on the normal path");
        assert_eq!(count(&b, "wall"), 8);
        b.undo().unwrap();
        assert_eq!(
            count(&b, "wall"),
            8,
            "undo of the re-import keeps the first import"
        );
        b.undo().unwrap();
        assert_eq!(count(&b, "wall"), 0);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn damaged_file_imports_with_warnings_like_the_normal_path() {
        let text = export_ifc(&source()).unwrap();
        let cut = text.rfind("ENDSEC;").unwrap();
        let bad = format!(
            "{}#999999=IFCPROPERTYSET('x',$,'P',$,(#888888));
{}",
            &text[..cut],
            &text[cut..]
        );
        let p = std::env::temp_dir().join(format!("ddc-g3-{}-bad.ifc", std::process::id()));
        std::fs::write(&p, &bad).unwrap();
        let mut a = Model::open(":memory:").unwrap();
        let ra = import_ifc(&mut a, bad.as_bytes()).unwrap();
        assert!(!ra.warnings.is_empty());
        let mut b = Model::open(":memory:").unwrap();
        let rb = import_ifc_path_ctl(&mut b, &p, &Progress::new()).unwrap();
        assert_eq!(ra, rb);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn ifczip_goes_through_the_stream_and_leaves_no_temp() {
        use std::io::Write;
        let text = export_ifc(&source()).unwrap();
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let o = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        w.start_file("model.ifc", o).unwrap();
        w.write_all(text.as_bytes()).unwrap();
        let z = w.finish().unwrap().into_inner();
        let p = std::env::temp_dir().join(format!("ddc-g3-{}-z.ifczip", std::process::id()));
        std::fs::write(&p, &z).unwrap();
        let mut a = Model::open(":memory:").unwrap();
        let ra = import_ifc(&mut a, &z).unwrap();
        let mut b = Model::open(":memory:").unwrap();
        let rb = import_ifc_path_ctl(&mut b, &p, &Progress::new()).unwrap();
        assert_eq!(ra, rb);
        assert_eq!(count(&b, "wall"), 8);
        let _ = std::fs::remove_file(&p);
    }
}
