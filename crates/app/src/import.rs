//! "File -> Import": formats from installed bridges and user messages.
//! UI text contains only file extensions.

use std::path::{Path, PathBuf};

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;

use ddc_io::bridge::{self, proto::Frame, Bridge, Report, TrustStore};

/// Extensions always shown in the list (only an external module reads them).
pub const EXPECTED: &[&[&str]] = &[&["rvt", "rfa"]];

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// «.rvt/.rfa».
    pub label: String,
    pub formats: Vec<String>,
    /// Folder of the trusted bridge package; None means the module is not installed or not signed.
    pub dir: Option<PathBuf>,
    /// Why it is unavailable (for the tooltip).
    pub note: Option<String>,
    /// The module is there but its signature did not check out.
    pub untrusted: bool,
}

pub struct Bridges {
    pub list: Vec<Bridge>,
    pub trust: TrustStore,
}

impl Bridges {
    /// Keys: built-in official ones plus those confirmed in developer mode (a file in the
    /// settings folder, not the bridges folder; `DDC_BRIDGE_DIR` does not affect keys).
    pub fn load() -> Self {
        let (list, trust) = bridge::installed();
        Self { list, trust }
    }

    /// Developer mode: adds a key only after explicit user confirmation
    /// (called from the confirmation dialog), then re-reads the bridges.
    pub fn confirm_developer_key(&mut self, key_line: &str) -> Result<String, String> {
        let p = bridge::developer_keys_path().ok_or_else(|| crate::tr("edit_panels.imp.no_settings", &[]))?;
        let c = |x: &Path| std::fs::canonicalize(x).unwrap_or_else(|_| x.to_path_buf());
        if bridge::default_dir()
            .is_some_and(|d| p.parent().map(c).is_some_and(|pp| pp.starts_with(c(&d))))
        {
            return Err(crate::tr("edit_panels.imp.keys_inside", &[]));
        }
        let k = bridge::confirm_developer_key(&p, key_line)?;
        *self = Self::load();
        Ok(key_id(&k))
    }
}

/// Key identifier for the confirmation dialog (16 hex digits).
pub fn key_id(k: &bridge::sig::PublicKey) -> String {
    k.key_id.iter().rev().map(|b| format!("{b:02X}")).collect()
}

/// A key from text (a base64 string or a `.pub` file), for showing before confirmation.
pub fn parse_key(text: &str) -> Result<String, String> {
    bridge::sig::PublicKey::parse(text).map(|k| key_id(&k))
}

fn label(formats: &[String]) -> String {
    let v: Vec<String> = formats.iter().map(|f| format!(".{f}")).collect();
    v.join("/")
}

pub fn missing(formats: &[&str]) -> String {
    missing_lang(formats, crate::ui_is_ru())
}

fn missing_lang(formats: &[&str], ru: bool) -> String {
    let f: Vec<String> = formats.iter().map(|s| s.to_string()).collect();
    crate::tr_lang(ru, "edit_panels.imp.missing", &[("label", label(&f).as_str())])
}

/// Menu items: trusted bridges, then expected formats that are missing.
pub fn entries(bridges: &[Bridge]) -> Vec<Entry> {
    entries_lang(bridges, crate::ui_is_ru())
}

/// As `entries`, with the notes in an explicit language.
fn entries_lang(bridges: &[Bridge], ru: bool) -> Vec<Entry> {
    let mut out: Vec<Entry> = bridges
        .iter()
        .filter(|b| b.trust.is_ok() && !b.formats.is_empty())
        .map(|b| Entry {
            label: label(&b.formats),
            formats: b.formats.clone(),
            dir: Some(b.dir.clone()),
            note: None,
            untrusted: false,
        })
        .collect();
    for exp in EXPECTED {
        if exp
            .iter()
            .any(|e| out.iter().any(|x| x.formats.iter().any(|f| f == e)))
        {
            continue;
        }
        let untrusted = bridges
            .iter()
            .any(|b| b.trust.is_err() && exp.iter().any(|e| b.formats.iter().any(|f| f == e)));
        out.push(Entry {
            label: label(&exp.iter().map(|s| s.to_string()).collect::<Vec<_>>()),
            formats: exp.iter().map(|s| s.to_string()).collect(),
            dir: None,
            untrusted,
            note: Some(if untrusted {
                crate::tr_lang(ru, "edit_panels.imp.unsigned", &[("missing", missing_lang(exp, ru).as_str())])
            } else {
                missing_lang(exp, ru)
            }),
        });
    }
    out
}

/// The item for a file by extension.
pub fn entry_for<'a>(entries: &'a [Entry], path: &Path) -> Option<&'a Entry> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    entries.iter().find(|e| e.formats.contains(&ext))
}

/// Background import: package verification (with a timeout), launching its private copy and parsing frames
/// in its own thread; the model changes only on the UI thread after [`Job::poll`] (one undo operation).
pub struct Job {
    pub dir: PathBuf,
    pub input: PathBuf,
    pub cancel: Arc<AtomicBool>,
    /// (done, total) from Progress frames.
    pub progress: (u64, u64),
    rx: Receiver<Msg>,
}

enum Msg {
    Progress(u64, u64),
    Done(Result<Vec<Frame>, String>),
}

impl Job {
    pub fn start(dir: PathBuf, trust: TrustStore, input: PathBuf) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        let (d, i) = (dir.clone(), input.clone());
        let o = bridge::RunOptions {
            cancel: Some(cancel.clone()),
            ..Default::default()
        };
        let tx_panic = tx.clone();
        // collects frames only; the model is changed on the main thread when they are applied
        let _ = crate::jobs::spawn_guarded(
            "import-collect",
            move || {
                let r = bridge::collect_trusted(&d, &trust, &i, &o, &mut |p| {
                    let _ = tx.send(Msg::Progress(p.done, p.total));
                });
                let _ = tx.send(Msg::Done(r.map_err(|e| e.to_string())));
            },
            move |msg| {
                let _ = tx_panic.send(Msg::Done(Err(msg)));
            },
        );
        Self {
            dir,
            input,
            cancel,
            progress: (0, 0),
            rx,
        }
    }

    /// A job frozen at `progress` for the `--import-demo` screenshots.
    pub fn demo(input: PathBuf, progress: (u64, u64)) -> Self {
        let (tx, rx) = channel();
        // a dropped sender reads as a dead worker; the screenshot process exits soon anyway
        std::mem::forget(tx);
        Self {
            dir: PathBuf::new(),
            input,
            cancel: Arc::new(AtomicBool::new(false)),
            progress,
            rx,
        }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Some means the thread finished: frames or an error text.
    pub fn poll(&mut self) -> Option<Result<Vec<Frame>, String>> {
        loop {
            match self.rx.try_recv() {
                Ok(Msg::Progress(d, t)) => self.progress = (d, t),
                Ok(Msg::Done(r)) => return Some(r),
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => {
                    return Some(Err(crate::tr("edit_panels.imp.aborted", &[])))
                }
            }
        }
    }
}

/// The status line of a finished module import, in the interface language. The numbers come
/// from the model (the banner's source); the reader's report only adds the re-import details.
pub fn summary(m: &ddc_model::Model, r: &Report) -> String {
    let c = crate::open::summary::from_model(m, "").counts();
    summary_with(&c, r, &|k, a| crate::tr(k, a))
}

/// [`summary`] with the counts and text source given, so each language can be checked.
pub fn summary_with(
    c: &ddc_io::import_summary::ImportCounts,
    r: &Report,
    t: &dyn Fn(&str, &[(&str, &str)]) -> String,
) -> String {
    let n = |v: usize| v.to_string();
    let mut s = t(
        "import_queue.sum.head",
        &[
            ("editable", &n(c.editable)),
            ("walls", &n(c.walls)),
            ("open", &n(c.openings)),
            ("slabs", &n(c.slabs)),
            ("rooms", &n(c.rooms)),
            ("other", &n(c.other)),
            ("meshes", &n(c.meshes)),
        ],
    );
    let mut part = |key: &str, v: usize| s += &t(key, &[("n", &n(v))]);
    let skipped: usize = r.skipped.values().sum();
    let parts = [
        ("import_queue.sum.unchanged", r.unchanged),
        ("import_queue.sum.updated", r.updated),
        ("import_queue.sum.missing", r.missing.len()),
        ("import_queue.sum.skipped", skipped),
        ("import_queue.sum.warnings", r.warning_count),
    ];
    for (key, v) in parts {
        if v > 0 {
            part(key, v);
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rvt_summary_has_no_cyrillic_in_en() {
        use ddc_ui::i18n::{Catalog, Lang};
        let r = Report {
            walls: 3,
            unchanged: 1,
            updated: 2,
            warning_count: 4,
            ..Report::default()
        };
        let cat = Catalog::new(Lang::En);
        let c = ddc_io::import_summary::ImportCounts {
            walls: 3,
            editable: 3,
            ..Default::default()
        };
        let text = summary_with(&c, &r, &|k, a| {
            let mut s = cat.t_in(Lang::En, k);
            for (n, v) in a {
                s = s.replace(&format!("{{{n}}}"), v);
            }
            s
        });
        assert!(text.contains("walls 3") && text.contains("warnings 4"), "{text}");
        assert!(text.is_ascii(), "{text}");
    }

    fn en(k: &str, a: &[(&str, &str)]) -> String {
        use ddc_ui::i18n::{Catalog, Lang};
        let mut s = Catalog::new(Lang::En).t_in(Lang::En, k);
        for (n, v) in a {
            s = s.replace(&format!("{{{n}}}"), v);
        }
        s
    }

    fn model_with(walls: usize, meshes: usize) -> crate::editor::Editor {
        let mut e = crate::editor::Editor::start_project().unwrap();
        for k in 0..walls {
            let spec = ddc_model::WallSpec {
                a: (0.0, k as f64),
                b: (5.0, k as f64),
                level: e.level,
                base_offset: 0.0,
                height: 3.0,
                wall_type: e.types[0].id,
                offset0: None,
            };
            e.model.add_wall(&spec).unwrap();
        }
        for _ in 0..meshes {
            e.model
                .sql()
                .execute(
                    "INSERT INTO mesh(level, category, name, positions, indices) VALUES (?1,'Furniture','m',x'',x'')",
                    rusqlite::params![e.level],
                )
                .unwrap();
        }
        e
    }

    /// Banner, toast and model rows read the same numbers, for each import kind.
    fn agree(walls: usize, meshes: usize, rep: &Report) {
        let e = model_with(walls, meshes);
        let s = crate::open::summary::from_model(&e.model, "a.rvt");
        let c = s.counts();
        let rows: i64 = e
            .model
            .sql()
            .query_row("SELECT count(*) FROM wall", [], |r| r.get(0))
            .unwrap();
        assert_eq!((c.walls as i64, c.editable), (rows, s.native_total()));
        assert_eq!((c.meshes, s.mesh_total()), (meshes, meshes));
        let toast = summary_with(&c, rep, &en);
        let head = format!("{} editable (walls {}", c.editable, c.walls);
        assert!(toast.contains(&head), "{toast}");
        assert!(toast.contains(&format!("{} as mesh", c.meshes)), "{toast}");
        assert!(!toast.contains("import_queue.sum"), "{toast}");
    }

    #[test]
    fn toast_equals_banner_counts_for_each_import_kind() {
        // fresh import, re-import with updates, mesh-only and zero-element models
        agree(3, 2, &Report::default());
        agree(
            2,
            0,
            &Report {
                walls: 99, // the report's own numbers never reach the line
                updated: 2,
                unchanged: 1,
                ..Report::default()
            },
        );
        agree(0, 4, &Report::default());
        agree(0, 0, &Report::default());
    }

    fn b(formats: &[&str], trusted: bool) -> Bridge {
        Bridge {
            name: "x".into(),
            version: "1".into(),
            dir: "x".into(),
            formats: formats.iter().map(|s| s.to_string()).collect(),
            trust: if trusted {
                Ok(String::new())
            } else {
                Err("no".into())
            },
        }
    }

    #[test]
    fn missing_module_is_listed_with_neutral_text() {
        // explicit language: other tests may switch the UI language at any time
        let e = entries_lang(&[], true);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].label, ".rvt/.rfa");
        assert_eq!(e[0].note.as_deref(), Some(crate::app2_fx::fx("import_module_missing").as_str()));
        assert!(entry_for(&e, Path::new("a/B.RVT")).unwrap().dir.is_none());
        let en = entries_lang(&[], false);
        assert_eq!(en[0].note.as_deref(), Some("Module .rvt/.rfa is not installed"));
        let e = entries_lang(&[b(&["rvt", "rfa"], false)], true);
        assert!(e[0].note.as_ref().unwrap().contains(&crate::app2_fx::fx("import_signature")));
        let en = entries_lang(&[b(&["rvt", "rfa"], false)], false);
        assert!(en[0].note.as_ref().unwrap().contains("signature not confirmed"));
    }

    #[test]
    fn job_reports_spawn_failure_without_blocking() {
        let mut j = Job::start(
            "no/such/module".into(),
            TrustStore::official(),
            "x.rvt".into(),
        );
        let t = web_time::Instant::now();
        let r = loop {
            if let Some(r) = j.poll() {
                break r;
            }
            assert!(t.elapsed().as_secs() < 10);
            std::thread::yield_now();
        };
        assert!(r.is_err());
    }

    #[test]
    fn trusted_bridge_provides_formats() {
        let e = entries(&[b(&["rvt", "rfa"], true), b(&["abc"], true)]);
        assert_eq!(e.len(), 2);
        assert_eq!(
            entry_for(&e, Path::new("x.rfa")).unwrap().dir.as_deref(),
            Some(Path::new("x"))
        );
        assert_eq!(entry_for(&e, Path::new("x.abc")).unwrap().label, ".abc");
    }
}
