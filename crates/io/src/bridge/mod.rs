// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Out-of-process import bridges (docs/plugins/BRIDGE_PROTOCOL.md).
//! sig — minisign-compatible check · discover — installed bridges, trust root · package — signed
//! file list, private per-run copy · run — process, timeouts, caps, cancel · map — frames → native rows. The whole import is one undoable model operation.

pub mod discover;
pub mod limit;
pub mod map;
pub mod package;
pub mod run;
pub mod sig;

use std::path::Path;
use web_time::{Duration, Instant};

pub use ddc_bridge_proto as proto;
pub use discover::{
    config_dir, confirm_developer_key, default_dir, developer_keys_path, discover, for_ext,
    installed, load_developer_keys, Bridge, Policy, TrustStore,
};
pub use map::Report;
pub use run::{Limits, RunOptions};

use ddc_model::Model;

#[derive(Debug, Clone, PartialEq)]
pub enum BridgeError {
    Untrusted(String),
    Spawn(String),
    Protocol(String),
    /// Well-formed frames with impossible content (unknown references, bad sizes).
    Data(String),
    Crashed(String),
    Timeout(String),
    Cancelled,
    /// A total cap (bytes, elements, vertices) was exceeded.
    TooLarge(String),
    Model(String),
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Untrusted(m) => write!(f, "module is not trusted: {m}"),
            Self::Spawn(m) => write!(f, "module could not start: {m}"),
            Self::Protocol(m) => write!(f, "module protocol error: {m}"),
            Self::Data(m) => write!(f, "module sent invalid data: {m}"),
            Self::Crashed(m) => write!(f, "module stopped unexpectedly: {m}"),
            Self::Timeout(m) => write!(f, "module timed out: {m}"),
            Self::Cancelled => write!(f, "import cancelled"),
            Self::TooLarge(m) => write!(f, "source is too large: {m}"),
            Self::Model(m) => write!(f, "model: {m}"),
        }
    }
}

impl std::error::Error for BridgeError {}

/// Longest time package verification (hashing the private copy) may take.
pub const VERIFY_TIMEOUT: Duration = Duration::from_secs(120);

/// Identity of an input file for the guid namespace: its canonical path (case-folded on Windows).
pub fn file_identity(input: &Path) -> String {
    let p = std::fs::canonicalize(input).unwrap_or_else(|_| input.to_path_buf());
    let s = p.to_string_lossy().to_string();
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s
    }
}

/// Runs `exe` on `input` and imports everything as one undoable operation; on any error the
/// model is unchanged. The caller has checked trust (see [`import_trusted`]).
pub fn import_file(
    model: &mut Model,
    exe: &Path,
    input: &Path,
    o: &RunOptions,
    progress: &mut dyn FnMut(&proto::Progress),
) -> Result<Report, BridgeError> {
    let mut failure: Option<BridgeError> = None;
    let stamp = ddc_model::source::file::FileStamp::of(input).ok();
    let res = model.batch_walls_edit("wall_import_bridge", |db, snaps| {
        let mut m = map::Mapper::new(db, snaps, &file_identity(input))
            .with(&input.to_string_lossy(), Default::default())
            .with_stamp(stamp);
        let r = run::run(exe, input, o, &mut |f| m.frame(f, progress)).and_then(|_| m.finish());
        match r {
            Ok(()) => Ok(m.report),
            Err(e) => {
                failure = Some(e);
                Err(ddc_model::Error::Invalid("bridge import failed".into()))
            }
        }
    });
    match (res, failure) {
        (Ok(r), _) => Ok(r),
        (Err(_), Some(e)) => Err(e),
        (Err(e), None) => Err(BridgeError::Model(e.to_string())),
    }
}

/// Runs `exe` and only collects its frames (no model access): for a background thread; the caller
/// then applies them with [`import_frames`] on the model's thread.
pub fn collect(
    exe: &Path,
    input: &Path,
    o: &RunOptions,
    progress: &mut dyn FnMut(&proto::Progress),
) -> Result<Vec<proto::Frame>, BridgeError> {
    let mut out = vec![];
    run::run(exe, input, o, &mut |f| {
        if let proto::Frame::Progress(p) = &f {
            progress(p);
        }
        out.push(f);
        Ok(())
    })?;
    Ok(out)
}

/// Verifies the package in `dir`, runs its private copy and collects frames: everything a
/// background import does off the UI thread.
pub fn collect_trusted(
    dir: &Path,
    trust: &TrustStore,
    input: &Path,
    o: &RunOptions,
    progress: &mut dyn FnMut(&proto::Progress),
) -> Result<Vec<proto::Frame>, BridgeError> {
    let staged = package::prepare(dir, trust, Some(Instant::now() + VERIFY_TIMEOUT))
        .map_err(BridgeError::Untrusted)?;
    let r = collect(&staged.exe, input, o, progress);
    drop(staged);
    r
}

/// Imports frames from [`collect`] of `input` as one undoable operation; on any error the model
/// is unchanged.
pub fn import_frames(
    model: &mut Model,
    input: &Path,
    frames: Vec<proto::Frame>,
) -> Result<Report, BridgeError> {
    import_frames_as(model, input, frames, &ImportAs::default())
}

/// How an import relates to earlier ones.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportAs {
    /// File identity of an earlier import (`SourceInfo::file_key`): an updated copy of the
    /// project at another path is recorded under it. None — this input's identity.
    pub file_key: Option<String>,
    /// Guid namespace of an earlier import (`SourceInfo::source`) to merge into, whatever
    /// reader produced it. None — the namespace an earlier import of this file used, else new.
    pub source: Option<String>,
    /// Re-import policy (delete unedited elements the source no longer has).
    pub policy: ddc_model::source::Policy,
}

/// [`import_frames`] with a source namespace and merge policy ("re-import updated file").
pub fn import_frames_as(
    model: &mut Model,
    input: &Path,
    frames: Vec<proto::Frame>,
    how: &ImportAs,
) -> Result<Report, BridgeError> {
    import_frames_with(model, input, frames, how, None)
}

/// [`import_frames`] counting frames written (N of all) with what they hold on `ctl`; a
/// cancel rolls the whole import back.
pub fn import_frames_ctl(
    model: &mut Model,
    input: &Path,
    frames: Vec<proto::Frame>,
    ctl: &crate::job::Progress,
) -> Result<Report, BridgeError> {
    ctl.set_total(frames.len() as u64);
    ctl.set(0);
    import_frames_with(model, input, frames, &ImportAs::default(), Some(ctl))
}

/// Catalog key of what a frame carries, for the progress line.
fn frame_key(f: &proto::Frame) -> &'static str {
    use proto::Frame as F;
    match f {
        F::Levels(_) | F::Views(_) => "job.kind.level",
        F::Grids(_) => "job.kind.grid",
        F::Materials(_) | F::Types(_) | F::ParamDefs(_) => "job.kind.types",
        F::Elements(_) | F::SourceIds(_) | F::WallTops(_) | F::SlabSlopes(_) => {
            "job.kind.elements"
        }
        F::CurtainWalls(_) | F::CurtainLines(_) => "job.kind.curtain_wall",
        F::Members(_) | F::Prisms(_) => "job.kind.elements",
        F::MepRuns(_) => "job.kind.mep",
        F::Ceilings(_) => "job.kind.ceiling",
        F::Properties(_) => "job.kind.props",
        _ => "job.kind.other",
    }
}

/// [`import_frames`] over frames the caller keeps (redo, shared via `Arc`): one frame is
/// copied at a time, the whole list is never duplicated.
pub fn import_frames_kept(
    model: &mut Model,
    input: &Path,
    frames: &[proto::Frame],
) -> Result<Report, BridgeError> {
    import_frames_with(model, input, frames.iter().cloned(), &ImportAs::default(), None)
}

fn import_frames_with(
    model: &mut Model,
    input: &Path,
    frames: impl IntoIterator<Item = proto::Frame>,
    how: &ImportAs,
    ctl: Option<&crate::job::Progress>,
) -> Result<Report, BridgeError> {
    let mut failure: Option<BridgeError> = None;
    let key = how.file_key.clone().unwrap_or_else(|| file_identity(input));
    let stamp = ddc_model::source::file::FileStamp::of(input).ok();
    let res = model.batch_walls_edit("wall_import_bridge", |db, snaps| {
        let mut m = map::Mapper::new(db, snaps, &key)
            .with(&input.to_string_lossy(), how.policy)
            .with_stamp(stamp)
            .into_source(how.source.as_deref());
        let r = frames
            .into_iter()
            .try_for_each(|f| {
                if let Some(c) = ctl {
                    if c.is_cancelled() {
                        return Err(BridgeError::Cancelled);
                    }
                    c.set_detail(frame_key(&f));
                }
                let r = m.frame(f, &mut |_| {});
                if let Some(c) = ctl {
                    c.add(1);
                }
                r
            })
            .and_then(|_| m.finish());
        match r {
            Ok(()) => Ok(m.report),
            Err(e) => {
                failure = Some(e);
                Err(ddc_model::Error::Invalid("bridge import failed".into()))
            }
        }
    });
    match (res, failure) {
        (Ok(r), _) => Ok(r),
        (Err(_), Some(e)) => Err(e),
        (Err(e), None) => Err(BridgeError::Model(e.to_string())),
    }
}

/// Verifies the package, copies it into a private folder, checks the copy and runs it
/// ([`import_file`]); the installed files are never executed.
pub fn import_trusted(
    model: &mut Model,
    bridge: &Bridge,
    trust: &TrustStore,
    input: &Path,
    o: &RunOptions,
    progress: &mut dyn FnMut(&proto::Progress),
) -> Result<Report, BridgeError> {
    let staged = package::prepare(&bridge.dir, trust, Some(Instant::now() + VERIFY_TIMEOUT))
        .map_err(BridgeError::Untrusted)?;
    let r = import_file(model, &staged.exe, input, o, progress);
    drop(staged);
    r
}

/// User-facing line when no trusted module reads `ext` (no product names, only the extension).
pub fn missing_module_message(ext: &str) -> String {
    let ext = ext.trim_start_matches('.').to_ascii_lowercase();
    match ext.as_str() {
        "rvt" | "rfa" => ".rvt/.rfa module not installed".to_string(),
        _ => format!(".{ext} module not installed"),
    }
}
