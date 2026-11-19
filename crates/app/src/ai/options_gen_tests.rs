use super::*;
use ddc_model::{CellDiff, RowDiff};
use std::sync::atomic::AtomicUsize;

const GIB: u64 = 1 << 30;

struct Fake {
    disk: u64,
    mem: u64,
    cores: usize,
    /// Disk reads from this call on report a full disk.
    full_after: usize,
    calls: AtomicUsize,
}

impl Fake {
    fn roomy() -> Fake {
        Fake::with(64 * GIB, 16 * GIB)
    }

    fn with(disk: u64, mem: u64) -> Fake {
        Fake {
            disk,
            mem,
            cores: 4,
            full_after: usize::MAX,
            calls: AtomicUsize::new(0),
        }
    }
}

impl Probe for Fake {
    fn free_disk(&self, _: &Path) -> Option<u64> {
        let k = self.calls.fetch_add(1, Ordering::SeqCst);
        Some(if k >= self.full_after { 0 } else { self.disk })
    }
    fn free_mem(&self) -> Option<u64> {
        Some(self.mem)
    }
    fn cores(&self) -> usize {
        self.cores
    }
}

struct Fx {
    dir: PathBuf,
    main: Model,
    base: PathBuf,
    level: i64,
}

impl Drop for Fx {
    fn drop(&mut self) {
        // the open main file would keep the folder on Windows
        if let Ok(m) = Model::open(":memory:") {
            self.main = m;
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn fixture(tag: &str) -> Fx {
    let dir = std::env::temp_dir().join(format!("ddc-optgen-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let ed = Editor::new_in_memory().unwrap();
    let level = ed.level;
    let main_path = dir.join("main.db");
    ed.model.snapshot_to(&main_path).unwrap();
    drop(ed);
    let main = Model::open(&main_path.to_string_lossy()).unwrap();
    let base = dir.join("base.db");
    Model::backup_file(&main_path, &base, &mut |_, _| true).unwrap();
    Fx {
        dir,
        main,
        base,
        level,
    }
}

fn out_dir(fx: &Fx) -> PathBuf {
    fx.dir.join("options")
}

fn run(fx: &Fx, a: J, probe: &dyn Probe) -> Result<GenReport, String> {
    let (req, _) = Request::parse(&a, fx.level)?;
    let head = fx.main.ai_option_head().unwrap();
    generate_options(
        &fx.main,
        head,
        &fx.base,
        &out_dir(fx),
        &req,
        ddc_gen::registry(),
        Units::Metric,
        probe,
    )
}

fn files(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .map(|r| {
            r.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Row counts of every table outside the `ai_*` ones (recipes and audit live there).
fn table_counts(m: &Model) -> Vec<(String, i64)> {
    let mut st = m
        .sql()
        .prepare(
            "SELECT name FROM sqlite_master WHERE type='table' \
             AND substr(name, 1, 3) <> 'ai_' ORDER BY name",
        )
        .unwrap();
    let names: Vec<String> = st
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    names
        .into_iter()
        .map(|n| {
            let c: i64 = m
                .sql()
                .query_row(&format!("SELECT count(*) FROM \"{n}\""), [], |r| r.get(0))
                .unwrap();
            (n, c)
        })
        .collect()
}

fn lhs4(seed: u64) -> J {
    json!({
        "request": {"generator": "column_grid"},
        "n": 4,
        "vary": {"x_span": {"min": 5.0, "max": 9.0}},
        "sampler": "lhs",
        "seed": seed,
    })
}

#[test]
fn four_options_deterministic_by_seed() {
    let (a, b) = (fixture("det-a"), fixture("det-b"));
    let ra = run(&a, lhs4(3), &Fake::roomy()).unwrap();
    let rb = run(&b, lhs4(3), &Fake::roomy()).unwrap();
    assert_eq!(ra.options.len(), 4, "{:?}", ra.dropped);
    let key = |r: &GenReport| -> Vec<(usize, u64, u64)> {
        r.options.iter().map(|o| (o.index, o.seed, o.diff_hash)).collect()
    };
    assert_eq!(key(&ra), key(&rb), "same seed, same options");
    let recs = a.main.ai_option_list(Some(SET_NAME)).unwrap();
    assert_eq!(recs.len(), 4);
    for (r, o) in recs.iter().zip(&ra.options) {
        assert_eq!(r.id, o.id);
        assert_eq!(r.status, OptionStatus::Draft);
        assert_eq!(r.generator.as_deref(), Some("column_grid"));
        assert!(o.path.is_file(), "materialised copy {}", o.path.display());
        assert!(o.created > 0);
    }
    let other = fixture("det-c");
    let rc = run(&other, lhs4(4), &Fake::roomy()).unwrap();
    assert_ne!(key(&ra), key(&rc), "another seed, other options");
}

#[test]
fn duplicate_options_dropped() {
    let fx = fixture("dup");
    // no vary: seeds 1..4 give three variants, the fourth repeats the first
    let a = json!({"request": {"generator": "column_grid"}, "n": 4, "seed": 1});
    let r = run(&fx, a, &Fake::roomy()).unwrap();
    assert_eq!(r.options.len(), 3, "{:?}", r.dropped);
    assert_eq!(r.dropped.len(), 1);
    assert_eq!(r.dropped[0].0, 3);
    let hashes: HashSet<u64> = r.options.iter().map(|o| o.diff_hash).collect();
    assert_eq!(hashes.len(), 3);
    assert_eq!(fx.main.ai_option_list(None).unwrap().len(), 3);
    assert!(
        !files(&out_dir(&fx)).iter().any(|f| f.starts_with("gen-")),
        "dropped copies are removed"
    );
}

#[test]
fn diff_hash_follows_rows() {
    let row = |after: &str| RowDiff {
        table: "wall".into(),
        rowid: 7,
        kind: RowKind::Changed,
        owner: Some(("wall".into(), 7)),
        cells: vec![CellDiff {
            col: "height".into(),
            before: Some("3".into()),
            after: Some(after.into()),
        }],
    };
    let d = |after: &str| ModelDiff {
        rows: vec![row(after)],
        truncated: false,
    };
    assert_eq!(diff_hash(&d("4")), diff_hash(&d("4")));
    assert_ne!(diff_hash(&d("4")), diff_hash(&d("5")));
}

#[test]
fn n_capped_by_disk_check() {
    let fx = fixture("cap");
    let per = copy_bytes(file_bytes(&fx.base));
    let probe = Fake::with(DISK_RESERVE + 2 * per + 1, 16 * GIB);
    let r = run(&fx, lhs4(3), &probe).unwrap();
    assert_eq!(r.options.len(), 2);
    assert!(r.capped.iter().any(|c| c.contains("fit the disk check")), "{:?}", r.capped);
    assert_eq!(
        r.dropped.iter().filter(|d| d.1 == "over the disk check").count(),
        2
    );
    assert_eq!(fx.main.ai_option_list(None).unwrap().len(), 2);
}

#[test]
fn n_over_eight_clamped_with_reason() {
    let a = json!({"request": {"generator": "column_grid"}, "n": 12});
    let (req, why) = Request::parse(&a, 1).unwrap();
    assert_eq!(req.n, MAX_OPTIONS);
    assert!(why.unwrap().contains("capped at 8"));
    let zero = json!({"request": {"generator": "column_grid"}, "n": 0});
    assert!(Request::parse(&zero, 1).is_err());
    let (ok, why) = Request::parse(&json!({"request": {"generator": "g"}}), 1).unwrap();
    assert_eq!((ok.n, why), (DEFAULT_N, None));
}

#[test]
fn disk_guard_refuses_and_leaves_no_partial_files() {
    let fx = fixture("guard");
    let e = run(&fx, lhs4(3), &Fake::with(DISK_RESERVE + MIB, 16 * GIB)).unwrap_err();
    assert!(e.contains("not enough free disk"), "{e}");
    assert!(!out_dir(&fx).exists(), "refusal writes nothing");
    let e = run(&fx, lhs4(3), &Fake::with(64 * GIB, MEM_RESERVE + MIB)).unwrap_err();
    assert!(e.contains("not enough free memory"), "{e}");
    assert!(!out_dir(&fx).exists());
    assert!(fx.main.ai_option_list(None).unwrap().is_empty());

    // the disk fills after the first copy: the rest are refused and leave no file
    let mut probe = Fake::roomy();
    probe.cores = 1;
    probe.full_after = 2;
    let r = run(&fx, lhs4(3), &probe).unwrap();
    assert_eq!(r.options.len(), 1);
    assert_eq!(
        r.dropped
            .iter()
            .filter(|d| d.1.starts_with("disk check before the copy"))
            .count(),
        3
    );
    let left = files(&out_dir(&fx));
    assert_eq!(left, vec![format!("opt-{}.db", r.options[0].id)], "{left:?}");
}

#[test]
fn main_model_untouched_after_generating() {
    let fx = fixture("main");
    let before = table_counts(&fx.main);
    let top = fx.main.op_top().unwrap();
    let head = fx.main.ai_option_head().unwrap();
    let r = run(&fx, lhs4(5), &Fake::roomy()).unwrap();
    assert!(!r.options.is_empty());
    assert_eq!(table_counts(&fx.main), before, "element tables unchanged");
    assert_eq!(fx.main.op_top().unwrap(), top, "no undo step");
    assert_eq!(fx.main.ai_option_head().unwrap(), head);
    let recs = fx.main.ai_option_list(None).unwrap();
    assert_eq!(recs.len(), r.options.len());
    assert!(recs.iter().all(|r| r.base_head == head.0 && r.base_stamp == head.1));
}

#[test]
fn unknown_generator_is_refused() {
    let fx = fixture("unknown");
    let a = json!({"request": {"generator": "nope"}, "n": 2});
    assert!(run(&fx, a, &Fake::roomy()).unwrap_err().contains("no generator"));
    assert!(!out_dir(&fx).exists());
}

#[test]
fn options_survive_reopen_in_project_file() {
    let fx = fixture("reopen");
    let main_path = fx.dir.join("main.db");
    let before = table_counts(&fx.main);
    let (req, _) = Request::parse(&lhs4(9), fx.level).unwrap();
    let head = fx.main.ai_option_head().unwrap();
    let store = OptionStore::open(&main_path).unwrap();
    let r = generate_options(
        &store,
        head,
        &fx.base,
        &out_dir(&fx),
        &req,
        ddc_gen::registry(),
        Units::Metric,
        &Fake::roomy(),
    )
    .unwrap();
    assert!(!r.options.is_empty());
    drop(store);
    let ids: Vec<i64> = r.options.iter().map(|o| o.id).collect();
    // a fresh connection, as after closing and reopening the project
    let again = OptionStore::open(&main_path).unwrap();
    let got: Vec<i64> = again.list(None).unwrap().iter().map(|o| o.id).collect();
    assert_eq!(got, ids);
    assert_eq!(table_counts(&fx.main), before, "element tables unchanged");
    for o in &r.options {
        assert!(o.path.is_file(), "copy next to the project: {:?}", o.path);
    }
}

#[test]
fn generate_options_never_touches_project_file() {
    let fx = fixture("nowrite");
    let main_path = fx.dir.join("main.db");
    let session = out_dir(&fx);
    let (dir, db) = options_keep::locate(&session);
    assert_ne!(db, main_path);
    let bytes = fs::read(&main_path).unwrap();
    let (req, _) = Request::parse(&lhs4(11), fx.level).unwrap();
    let head = fx.main.ai_option_head().unwrap();
    fs::create_dir_all(&dir).unwrap();
    let store = OptionStore::open(&db).unwrap();
    let r = generate_options(
        &store,
        head,
        &fx.base,
        &dir,
        &req,
        ddc_gen::registry(),
        Units::Metric,
        &Fake::roomy(),
    )
    .unwrap();
    assert!(!r.options.is_empty());
    assert_eq!(store.list(None).unwrap().len(), r.options.len());
    assert!(fx.main.ai_option_list(None).unwrap().is_empty(), "no recipe rows in the project");
    assert_eq!(fs::read(&main_path).unwrap(), bytes, "project file bytes unchanged");
}

#[test]
fn generated_recipes_go_through_the_freeze_gate_unchanged() {
    let fx = fixture("freeze-gate");
    let rep = run(&fx, lhs4(5), &Fake::roomy()).unwrap();
    assert!(!rep.options.is_empty(), "{:?}", rep.dropped);
    for o in &rep.options {
        let r = fx.main.ai_option_get(o.id).unwrap().unwrap();
        let env: J = serde_json::from_str(&r.envelope).unwrap();
        assert_eq!(env["op"], "generate", "the generator recipe is stored as made");
        assert_eq!(crate::ai::option_freeze::check_frozen(&fx.main, o.id), Ok(()));
    }
}
