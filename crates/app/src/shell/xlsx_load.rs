// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Reading a book for the import preview on a worker: the model is read through its own
//! connection and never changed, so the window stays responsive and a cancel loses nothing.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use web_time::Instant;

use super::xlsx_job::Snap;
use ddc_io::xlsx::{self, Report, Watch};

/// What the worker hands back: the report and the file stamp taken before the read.
pub type Loaded = (Report, Watch);

pub struct XlsxLoad {
    pub path: PathBuf,
    pub delete_missing: bool,
    /// Re-read of the open preview (its result is merged into it), not a first open.
    pub reload: bool,
    /// Conflict choices and by-key decisions the read was made with: a result read with
    /// choices the preview no longer holds is not shown (see `xlsx_reloaded`).
    pub take: xlsx::Take,
    pub decisions: xlsx::KeyDecisions,
    /// Where the model stood when the worker started.
    pub rev: Snap,
    pub started: Instant,
    cancel: Arc<AtomicBool>,
    /// Fraction of the read as f32 bits, fed by the import hook.
    progress: Arc<AtomicU32>,
    rx: Receiver<Result<Loaded, String>>,
}

impl XlsxLoad {
    /// Starts the worker. A model without a file is copied to a temp file first (removed
    /// when the worker ends).
    pub fn start(
        model: &ddc_model::Model,
        path: &Path,
        delete_missing: bool,
        rev: Snap,
    ) -> Result<Self, String> {
        let (take, dec) = (xlsx::Take::default(), xlsx::KeyDecisions::default());
        Self::start_with(model, path, delete_missing, rev, take, dec, false)
    }

    /// `start` with the user's conflict choices (`take`) and by-key decisions kept across
    /// the read.
    pub fn start_with(
        model: &ddc_model::Model,
        path: &Path,
        delete_missing: bool,
        rev: Snap,
        take: xlsx::Take,
        decisions: xlsx::KeyDecisions,
        reload: bool,
    ) -> Result<Self, String> {
        static N: AtomicU32 = AtomicU32::new(0);
        let (db, own) = match model.db_path() {
            Some(db) => (db, false),
            None => {
                let tmp = std::env::temp_dir().join(format!(
                    "ddc_xl_load_{}_{}.ddc",
                    std::process::id(),
                    N.fetch_add(1, Ordering::Relaxed)
                ));
                model.backup_to(&tmp).map_err(|e| e.to_string())?;
                (tmp.to_string_lossy().into_owned(), true)
            }
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicU32::new(0));
        let (tx, rx) = mpsc::channel();
        let (c, p, book) = (cancel.clone(), progress.clone(), path.to_path_buf());
        let (take_w, dec_w) = (take.clone(), decisions.clone());
        let tx_panic = tx.clone();
        // the worker reads into a copy of the model; the result is applied on the main thread
        let spawned = crate::jobs::spawn_guarded("xl-load", move || {
            struct Cleanup(Option<String>);
            impl Drop for Cleanup {
                fn drop(&mut self) {
                    if let Some(db) = self.0.take() {
                        for suffix in ["", "-wal", "-shm", "-journal"] {
                            let _ = std::fs::remove_file(format!("{db}{suffix}"));
                        }
                    }
                }
            }
            let _cleanup = Cleanup(own.then(|| db.clone()));
            let hook: xlsx::Hook = Arc::new(move |f| {
                p.store(f.to_bits(), Ordering::Relaxed);
                !c.load(Ordering::Relaxed)
            });
            // stamped before the read: a save during the read is seen as a change afterwards
            let watch = Watch::new(&book);
            let out = match ddc_model::Model::open_copy(&db) {
                Ok(mut m) => super::xlsx_win::read_hooked(
                    &mut m,
                    &book,
                    delete_missing,
                    &take_w,
                    &dec_w,
                    Some(hook),
                )
                .map(|r| (r, watch)),
                Err(e) => Err(e.to_string()),
            };
            // the link write after a good read finds the fingerprint cached
            if out.is_ok() {
                super::xlsx_relink::warm(&book);
            }
            let _ = tx.send(out);
        }, move |msg| {
            let _ = tx_panic.send(Err(msg));
        });
        if let Err(e) = spawned {
            eprintln!("xlsx load thread: {e}");
        }
        Ok(Self {
            path: path.to_path_buf(),
            delete_missing,
            reload,
            take,
            decisions,
            rev,
            started: Instant::now(),
            cancel,
            progress,
            rx,
        })
    }

    /// A running read that never reports a result, for the layout contract.
    #[cfg(test)]
    pub(crate) fn parked(path: &Path) -> Self {
        let (tx, rx) = mpsc::channel();
        // a dropped sender would read as a crashed worker
        std::mem::forget(tx);
        Self {
            path: path.to_path_buf(),
            delete_missing: false,
            reload: false,
            take: xlsx::Take::default(),
            decisions: xlsx::KeyDecisions::default(),
            rev: None,
            started: Instant::now(),
            cancel: Arc::default(),
            progress: Arc::default(),
            rx,
        }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn cancelling(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// 0.0..1.0 for the bar.
    pub fn fraction(&self) -> f32 {
        f32::from_bits(self.progress.load(Ordering::Relaxed)).clamp(0.0, 1.0)
    }

    /// The result once the worker is done.
    pub fn poll(&self) -> Option<Result<Loaded, String>> {
        match self.rx.try_recv() {
            Ok(o) => Some(o),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(crate::jobs::internal_error())),
        }
    }
}

/// True for the error text of a read stopped by its hook.
pub fn is_cancelled(e: &str) -> bool {
    e == ddc_io::job::CANCELLED
}
