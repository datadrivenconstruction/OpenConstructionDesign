//! End-to-end: spawn the sample bridge → import → exact counts and quantities; broken streams,
//! crash, hang and cancel leave the model untouched; signed discovery.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ddc_io::bridge::proto::{self, Curve, Element, Frame, Top};
use ddc_io::bridge::{self, package, sig, BridgeError, RunOptions, TrustStore};
use ddc_model::Model;
use ed25519_dalek::{Signer, SigningKey};

const EXE: &str = env!("CARGO_BIN_EXE_ddc-bridge-sample");
const TABLES: [&str; 13] = [
    "level",
    "wall_type",
    "wall",
    "opening",
    "slab_type",
    "slab",
    "slab_vertex",
    "room",
    "mesh",
    "ext_ref",
    "prop",
    "wall_qty",
    "op",
];

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ddc-bridge-test-{}-{name}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    d
}

fn input() -> PathBuf {
    let p = scratch("in").join("project.rvt");
    std::fs::write(&p, b"any bytes: the sample ignores content").unwrap();
    p
}

fn counts(m: &Model) -> Vec<i64> {
    TABLES
        .iter()
        .map(|t| {
            m.sql()
                .query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))
                .unwrap()
        })
        .collect()
}

fn opts(args: &[&str]) -> RunOptions {
    RunOptions {
        extra_args: args.iter().map(|s| s.to_string()).collect(),
        ..RunOptions::default()
    }
}

fn import(m: &mut Model, o: &RunOptions) -> Result<bridge::Report, BridgeError> {
    bridge::import_file(m, EXE.as_ref(), &input(), o, &mut |_| {})
}

fn q(m: &Model, sql: &str) -> f64 {
    m.sql().query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn sample_project_imports_with_exact_counts_and_quantities() {
    let mut m = Model::open(":memory:").unwrap();
    let before = counts(&m);
    let mut seen = 0;
    let r = bridge::import_file(
        &mut m,
        EXE.as_ref(),
        &input(),
        &RunOptions::default(),
        &mut |p| {
            assert!(p.done <= p.total);
            seen += 1;
        },
    )
    .unwrap();
    assert!(seen >= 3);
    assert_eq!(r.bridge, "ddc-bridge-sample");
    assert_eq!(
        (
            r.levels,
            r.materials,
            r.wall_types,
            r.slab_types,
            r.family_types
        ),
        (2, 2, 2, 1, 2)
    );
    assert_eq!((r.walls, r.arc_walls), (10, 1));
    assert_eq!(r.wall_segments, 10, "arc wall stays one native arc wall");
    // arc through (2,2)-(5,3.5)-(8,2): r = 3.75, sweep = 2·asin(0.8); 0.15 thick, 3 high
    let arc_id = q(&m, "SELECT ref FROM ext_ref WHERE guid='w2-arc'") as i64;
    let len = 3.75 * 2.0 * 0.8f64.asin();
    assert!((m.wall_length(arc_id).unwrap() - len).abs() < 1e-9);
    assert!(m.wall_bulge(arc_id).unwrap() < 0.0, "clockwise a→b");
    let arc_vol = q(
        &m,
        &format!("SELECT SUM(volume) FROM wall_qty WHERE wall={arc_id}"),
    );
    assert!(
        (arc_vol - len * 0.15 * 3.0).abs() < 1e-9,
        "{arc_vol} vs {}",
        len * 0.45
    );
    assert_eq!((r.doors, r.windows, r.voids), (1, 1, 0));
    assert_eq!((r.slabs, r.rooms, r.meshes), (1, 2, 1));
    assert_eq!(r.properties, 6); // 2 own + door/window type name and parameter
    assert_eq!(r.unchanged, 0);
    assert_eq!((r.grids, r.skipped.get("grid")), (1, None));
    assert_eq!(r.warnings, vec!["sample: synthetic project".to_string()]);

    // millimetres → metres; heights from the top constraint
    assert_eq!(
        q(&m, "SELECT elevation FROM level WHERE name='Level 2'"),
        3.0
    );
    assert_eq!(
        q(&m, "SELECT count(*) FROM wall WHERE abs(height-3.0)<1e-12"),
        r.wall_segments as f64
    );
    // slab 10×6 minus 1×1 hole, 0.25 thick
    let sid = q(&m, "SELECT id FROM slab") as i64;
    let sq = m.slab_quantities(sid).unwrap();
    assert!((sq[0].area - 59.0).abs() < 1e-9 && (sq[0].volume - 14.75).abs() < 1e-9);
    // rooms bounded by 0.2 walls: 3.8×5.8 and 5.8×5.8
    let mut areas: Vec<f64> = [101, 102]
        .iter()
        .map(|n| {
            let id = q(&m, &format!("SELECT id FROM room WHERE number='{n}'")) as i64;
            m.room_quantities(id).unwrap().expect("room closed").area
        })
        .collect();
    areas.sort_by(f64::total_cmp);
    assert!(
        (areas[0] - 22.04).abs() < 1e-9 && (areas[1] - 33.64).abs() < 1e-9,
        "{areas:?}"
    );
    // masonry: two 10×6 rings (6.4 m² plan) + partition (1.16 m²), 3 m high, minus door and window
    let vols = m.material_volumes().unwrap();
    let masonry = vols.iter().find(|v| v.0 == "masonry").unwrap().1;
    let expect = (6.4 * 2.0 + 1.16) * 3.0 - (0.9 * 2.1 + 1.2 * 1.5) * 0.2;
    assert!(
        (masonry - expect).abs() < 1e-9,
        "masonry {masonry} vs {expect}"
    );
    let door: (i64, i64) = m
        .sql()
        .query_row(
            "SELECT swing_side, swing_hinge FROM opening WHERE kind='door'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(door, (1, 0));
    let rating: String = m
        .sql()
        .query_row(
            "SELECT p.value FROM prop p JOIN ext_ref e ON e.kind=p.kind AND e.ref=p.ref
             WHERE e.guid='w1-0' AND p.key='fire_rating'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rating, "REI60");
    let blob: Vec<u8> = m
        .sql()
        .query_row("SELECT positions FROM mesh", [], |r| r.get(0))
        .unwrap();
    assert_eq!(blob.len(), 8 * 3 * 8);

    // re-import: same guids → nothing duplicated
    let again = counts(&m);
    let r2 = import(&mut m, &RunOptions::default()).unwrap();
    assert_eq!(
        (r2.unchanged, r2.walls, r2.rooms, r2.levels, r2.wall_types),
        (17, 0, 0, 0, 0)
    );
    assert_eq!((r2.updated, r2.missing.len()), (0, 0));
    let mut after = counts(&m);
    after.pop();
    let mut again_no_op = again.clone();
    again_no_op.pop();
    assert_eq!(after, again_no_op);

    // one undo per import
    m.undo().unwrap();
    m.undo().unwrap();
    assert_eq!(counts(&m), before);
}

#[test]
fn broken_streams_fail_cleanly_and_leave_model_unchanged() {
    let mut m = Model::open(":memory:").unwrap();
    m.add_level("Existing", 0.0).unwrap();
    let before = counts(&m);
    let cases: [(&str, fn(&BridgeError) -> bool); 6] = [
        ("truncate", |e| matches!(e, BridgeError::Protocol(_))),
        (
            "oversize",
            |e| matches!(e, BridgeError::Protocol(m) if m.contains("exceeds")),
        ),
        (
            "malformed",
            |e| matches!(e, BridgeError::Protocol(m) if m.contains("element tag")),
        ),
        (
            "crash",
            |e| matches!(e, BridgeError::Crashed(m) if m.contains("3")),
        ),
        (
            "count",
            |e| matches!(e, BridgeError::Protocol(m) if m.contains("End reports")),
        ),
        ("crash-after-end", |e| matches!(e, BridgeError::Crashed(_))),
    ];
    for (fault, ok) in cases {
        let e = import(&mut m, &opts(&["--fault", fault])).unwrap_err();
        assert!(ok(&e), "{fault}: {e:?}");
        assert_eq!(counts(&m), before, "{fault} changed the model");
    }
    // one bad element is skipped with a warning; the rest of the stream imports
    let r = import(&mut m, &opts(&["--fault", "bad-host"])).unwrap();
    assert_eq!(r.skipped.get("element"), Some(&1));
    assert!(r.warnings.iter().any(|w| w.contains("unknown host")));
    m.undo().unwrap();
    assert_eq!(counts(&m), before);
    let e = bridge::import_file(
        &mut m,
        "no/such/bridge.exe".as_ref(),
        &input(),
        &RunOptions::default(),
        &mut |_| {},
    )
    .unwrap_err();
    assert!(matches!(e, BridgeError::Spawn(_)));
    assert_eq!(counts(&m), before);
}

#[test]
fn hang_times_out_and_cancel_stops_the_bridge() {
    let mut m = Model::open(":memory:").unwrap();
    let before = counts(&m);
    let t = Instant::now();
    let o = RunOptions {
        idle_timeout: Duration::from_millis(800),
        ..opts(&["--fault", "hang"])
    };
    let e = import(&mut m, &o).unwrap_err();
    assert!(matches!(e, BridgeError::Timeout(_)), "{e:?}");
    assert!(t.elapsed() < Duration::from_secs(10));
    assert_eq!(counts(&m), before);

    let flag = Arc::new(AtomicBool::new(false));
    let o = RunOptions {
        cancel: Some(flag.clone()),
        ..opts(&["--fault", "slow"])
    };
    let t = Instant::now();
    let e = bridge::import_file(&mut m, EXE.as_ref(), &input(), &o, &mut |_| {
        flag.store(true, Ordering::Relaxed)
    })
    .unwrap_err();
    assert_eq!(e, BridgeError::Cancelled);
    assert!(t.elapsed() < Duration::from_secs(5));
    assert_eq!(counts(&m), before);
}

#[test]
fn reimport_updates_changed_elements_in_one_undo_and_reports_missing() {
    let mut m = Model::open(":memory:").unwrap();
    import(&mut m, &opts(&["--walls", "3"])).unwrap();
    let arc = q(&m, "SELECT ref FROM ext_ref WHERE guid='w2-arc'") as i64;
    let x0 = q(&m, "SELECT ref FROM ext_ref WHERE guid='x-0'") as i64;
    let len0 = m.wall_length(arc).unwrap();
    let vol0 = q(&m, "SELECT SUM(volume) FROM wall_qty");
    let before = counts(&m);

    let r = import(&mut m, &opts(&["--walls", "3", "--fault", "edit"])).unwrap();
    assert_eq!((r.updated, r.walls), (2, 0));
    assert_eq!(r.missing, vec!["x-1".to_string()]);
    assert_eq!(r.unchanged, 17 + 3 - 1 - 2); // the grid counts too
                                             // in place: same rows, same ids
    assert_eq!(
        q(&m, "SELECT ref FROM ext_ref WHERE guid='w2-arc'") as i64,
        arc
    );
    assert_eq!(q(&m, &format!("SELECT ax FROM wall WHERE id={x0}")), 0.5);
    // mid raised to 4.0: r = (9 + 4)/4 = 3.25, half-angle asin(3/3.25)
    let r2 = 3.25f64;
    let len = r2 * 2.0 * (3.0 / r2).asin();
    assert!(
        (m.wall_length(arc).unwrap() - len).abs() < 1e-9,
        "{}",
        m.wall_length(arc).unwrap()
    );
    let vol = q(&m, "SELECT SUM(volume) FROM wall_qty");
    assert!((vol - vol0 - (len - len0) * 0.15 * 3.0).abs() < 1e-9);
    // missing element is kept, not deleted
    let mut now = counts(&m);
    now.pop();
    let mut was = before.clone();
    was.pop();
    assert_eq!(now, was);

    m.undo().unwrap();
    assert_eq!(counts(&m), before);
    assert!((m.wall_length(arc).unwrap() - len0).abs() < 1e-12);
    assert_eq!(q(&m, &format!("SELECT ax FROM wall WHERE id={x0}")), 0.0);
    assert!((q(&m, "SELECT SUM(volume) FROM wall_qty") - vol0).abs() < 1e-12);
}

#[test]
fn streams_many_elements_in_batches() {
    let mut m = Model::open(":memory:").unwrap();
    let t = Instant::now();
    let r = import(&mut m, &opts(&["--walls", "5000"])).unwrap();
    let dt = t.elapsed();
    // Budget is for optimized builds; unoptimized test builds run ~3-4x slower (per-op
    // phase/mass/relayout passes added ~10% in debug).
    let budget = Duration::from_secs(if cfg!(debug_assertions) { 8 } else { 2 });
    assert!(dt < budget, "5000 walls took {dt:?}");
    assert_eq!(r.walls, 5010);
    assert_eq!(
        q(&m, "SELECT count(*) FROM ext_ref WHERE guid LIKE 'x-%'"),
        5000.0
    );
}

fn sign_text(sk: &SigningKey, id: [u8; 8], data: &[u8], trusted: &str) -> String {
    let s = sk.sign(&sig::blake2b512(data)).to_bytes();
    let mut a = b"ED".to_vec();
    a.extend_from_slice(&id);
    a.extend_from_slice(&s);
    let mut g = s.to_vec();
    g.extend_from_slice(trusted.as_bytes());
    format!(
        "untrusted comment: test\n{}\ntrusted comment: {trusted}\n{}\n",
        sig::b64_encode(&a),
        sig::b64_encode(&sk.sign(&g).to_bytes())
    )
}

const KEY_ID: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];

fn test_key() -> (SigningKey, TrustStore) {
    let sk = SigningKey::from_bytes(&[5; 32]);
    let trust = TrustStore::from_keys(vec![sig::PublicKey::from_raw(
        KEY_ID,
        sk.verifying_key().to_bytes(),
    )]);
    (sk, trust)
}

fn exe_name() -> String {
    std::path::Path::new(EXE)
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string()
}

/// Package `<root>/<name>` with the sample bridge plus `extra` files, all in the signed manifest.
fn package(
    root: &str,
    version: &str,
    extra: &[(&str, &[u8])],
    comment: Option<&str>,
) -> (PathBuf, PathBuf) {
    let dir = scratch(root);
    let _ = std::fs::remove_dir_all(&dir);
    let sub = dir.join("sample");
    std::fs::create_dir_all(&sub).unwrap();
    let exe = exe_name();
    std::fs::copy(EXE, sub.join(&exe)).unwrap();
    let mut files = vec![exe.as_str()];
    for (n, b) in extra {
        std::fs::write(sub.join(n), b).unwrap();
        files.push(n);
    }
    let man =
        package::manifest_for(&sub, "sample", version, &exe, &["RVT", "rfa"], &files).unwrap();
    std::fs::write(sub.join(package::MANIFEST), &man).unwrap();
    let m = package::parse_manifest(man.as_bytes()).unwrap();
    let c = comment.map_or_else(|| package::trusted_comment(&m), str::to_string);
    let (sk, _) = test_key();
    std::fs::write(
        sub.join(package::SIGNATURE),
        sign_text(&sk, KEY_ID, man.as_bytes(), &c),
    )
    .unwrap();
    (dir, sub)
}

#[test]
fn only_signed_packages_are_listed_and_run() {
    let dir = scratch("bridges");
    let _ = std::fs::remove_dir_all(&dir);
    let sub = dir.join("sample");
    std::fs::create_dir_all(&sub).unwrap();
    let exe = exe_name();
    std::fs::copy(EXE, sub.join(&exe)).unwrap();
    let man =
        package::manifest_for(&sub, "sample", "1.0.0", &exe, &["RVT", "rfa"], &[&exe]).unwrap();
    std::fs::write(sub.join(package::MANIFEST), &man).unwrap();
    let (sk, trust) = test_key();
    let none = bridge::discover(&dir, &trust);
    assert_eq!(none.len(), 1);
    assert!(none[0].trust.is_err(), "unsigned must not be trusted");
    assert!(bridge::for_ext(&none, "rvt").is_none());
    assert_eq!(
        bridge::missing_module_message("RVT"),
        ".rvt/.rfa module not installed"
    );

    let m = package::parse_manifest(man.as_bytes()).unwrap();
    let good = sign_text(&sk, KEY_ID, man.as_bytes(), &package::trusted_comment(&m));
    std::fs::write(sub.join(package::SIGNATURE), &good).unwrap();
    let found = bridge::discover(&dir, &trust);
    let b = bridge::for_ext(&found, "rfa").expect("signed bridge listed");
    assert_eq!(b.formats, vec!["rvt", "rfa"]);
    assert_eq!(b.version, "1.0.0");
    let mut m = Model::open(":memory:").unwrap();
    let r = bridge::import_trusted(
        &mut m,
        b,
        &trust,
        &input(),
        &RunOptions::default(),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(r.walls, 10);
    // a different key does not trust it
    let other = TrustStore::from_keys(vec![sig::PublicKey::from_raw(
        KEY_ID,
        SigningKey::from_bytes(&[6; 32]).verifying_key().to_bytes(),
    )]);
    let e = bridge::import_trusted(
        &mut m,
        b,
        &other,
        &input(),
        &RunOptions::default(),
        &mut |_| {},
    )
    .unwrap_err();
    assert!(matches!(e, BridgeError::Untrusted(_)));
}

#[test]
fn executable_swapped_after_check_never_runs() {
    let (_, sub) = package("swap", "1.0.0", &[], None);
    let (_, trust) = test_key();
    let v = package::verify_manifest(&sub, &trust).unwrap();
    let staged = package::stage(&v, None).unwrap();
    assert!(!staged.dir.starts_with(&sub));
    // swap the installed executable: the staged, already hashed copy is what runs
    let exe = sub.join(exe_name());
    std::fs::write(&exe, b"MZ not the signed bridge").unwrap();
    let mut m = Model::open(":memory:").unwrap();
    let r = bridge::import_file(
        &mut m,
        &staged.exe,
        &input(),
        &RunOptions::default(),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(r.walls, 10);
    // the staged copy cannot be rewritten while it is in use
    #[cfg(windows)]
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .open(&staged.exe)
        .is_err());
    let staged_dir = staged.dir.clone();
    drop(staged);
    assert!(!staged_dir.exists(), "private copy removed after the run");
    // a later run of the swapped package is refused, the model is unchanged
    let before = counts(&m);
    let b = bridge::discover(&sub.parent().unwrap().to_path_buf(), &trust);
    let e = bridge::import_trusted(
        &mut m,
        &b[0],
        &trust,
        &input(),
        &RunOptions::default(),
        &mut |_| {},
    )
    .unwrap_err();
    assert!(
        matches!(&e, BridgeError::Untrusted(s) if s.contains("does not match")),
        "{e}"
    );
    assert_eq!(counts(&m), before);
}

#[test]
fn unsigned_library_in_package_is_refused_and_never_copied() {
    let (dir, sub) = package("dll", "1.0.0", &[("helper.dll", b"signed helper")], None);
    let (_, trust) = test_key();
    let v = package::verify_manifest(&sub, &trust).unwrap();
    let staged = package::stage(&v, None).unwrap();
    let mut names: Vec<String> = std::fs::read_dir(&staged.dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    let mut want = vec!["helper.dll".to_string(), exe_name()];
    want.sort();
    assert_eq!(names, want, "private copy holds only signed files");
    drop(staged);

    std::fs::write(sub.join("version.dll"), b"planted").unwrap();
    let e = package::verify_manifest(&sub, &trust).unwrap_err();
    assert!(e.contains("unsigned file in package: version.dll"), "{e}");
    let b = bridge::discover(&dir, &trust);
    assert!(b[0].trust.is_err());
    assert!(bridge::for_ext(&b, "rvt").is_none());
    std::fs::remove_file(sub.join("version.dll")).unwrap();
    // a signed library replaced after signing is caught on the copy
    std::fs::write(sub.join("helper.dll"), b"evil helper!!").unwrap();
    let v = package::verify_manifest(&sub, &trust).unwrap();
    assert!(package::stage(&v, None).unwrap_err().contains("helper.dll"));
}

#[test]
fn trusted_comment_must_match_manifest_and_policy_applies() {
    let (_, trust) = test_key();
    let (_, sub) = package(
        "comment",
        "1.0.0",
        &[],
        Some("ddc-bridge name=sample version=9.9.9 formats=rvt,rfa"),
    );
    let e = package::verify_manifest(&sub, &trust).unwrap_err();
    assert!(e.contains("trusted comment"), "{e}");

    let (_, sub) = package("policy", "1.2.0", &[], None);
    assert!(package::verify_manifest(&sub, &trust).is_ok());
    let mut t = trust.clone();
    t.policy.min_versions = vec![("sample".into(), "1.10".into())];
    assert!(package::verify_manifest(&sub, &t)
        .unwrap_err()
        .contains("minimum"));
    let mut t = trust.clone();
    t.policy.revoked_versions = vec![("sample".into(), "1.2.0".into())];
    assert!(package::verify_manifest(&sub, &t)
        .unwrap_err()
        .contains("revoked"));
    let mut t = trust.clone();
    t.policy.revoked_keys = vec![KEY_ID];
    assert!(package::verify_manifest(&sub, &t)
        .unwrap_err()
        .contains("revoked"));
    // verification has a deadline
    let v = package::verify_manifest(&sub, &trust).unwrap();
    let past = Instant::now() - Duration::from_millis(1);
    assert!(package::stage(&v, Some(past))
        .unwrap_err()
        .contains("timed out"));
}

#[test]
fn keys_never_come_from_the_bridges_dir() {
    let (sk, _) = test_key();
    let mut raw = b"Ed".to_vec();
    raw.extend_from_slice(&KEY_ID);
    raw.extend_from_slice(&sk.verifying_key().to_bytes());
    let line = sig::b64_encode(&raw);
    let cfg = scratch("cfg");
    let bridges = cfg.join("bridges");
    std::fs::create_dir_all(&bridges).unwrap();
    // planted next to the packages: ignored
    let planted = bridges.join("developer_keys.txt");
    std::fs::write(&planted, &line).unwrap();
    assert!(bridge::load_developer_keys(&planted, Some(&bridges)).is_empty());
    // confirmed by the user, stored in the config dir: used
    let keys = cfg.join("developer_keys.txt");
    let _ = std::fs::remove_file(&keys);
    let k =
        bridge::confirm_developer_key(&keys, &format!("untrusted comment: dev\n{line}\n")).unwrap();
    assert_eq!(
        bridge::load_developer_keys(&keys, Some(&bridges)),
        vec![k.clone()]
    );
    let mut t = TrustStore::official();
    assert_eq!(t.keys().len(), bridge::discover::OFFICIAL_KEYS.len());
    t.add_developer(vec![k]);
    let (_, sub) = package("devkey", "1.0.0", &[], None);
    assert!(package::verify_manifest(&sub, &t).is_ok());
    // the default bridges dir may move, the key file does not
    assert!(!bridge::developer_keys_path()
        .unwrap()
        .starts_with(bridge::default_dir().unwrap()));
}

#[test]
fn total_caps_stop_the_import_cleanly() {
    let mut m = Model::open(":memory:").unwrap();
    let before = counts(&m);
    for (lim, what) in [
        (
            bridge::Limits {
                elements: 100,
                ..Default::default()
            },
            "elements",
        ),
        (
            bridge::Limits {
                vertices: 50,
                ..Default::default()
            },
            "vertices",
        ),
        (
            bridge::Limits {
                bytes: 4096,
                ..Default::default()
            },
            "bytes",
        ),
    ] {
        let o = RunOptions {
            limits: lim,
            ..opts(&["--walls", "5000"])
        };
        let e = import(&mut m, &o).unwrap_err();
        assert!(
            matches!(&e, BridgeError::TooLarge(s) if s.contains(what)),
            "{e}"
        );
        let e = bridge::collect(EXE.as_ref(), &input(), &o, &mut |_| {}).unwrap_err();
        assert!(matches!(e, BridgeError::TooLarge(_)));
        assert_eq!(counts(&m), before);
    }
}

fn frames(units: f64, y0: f64, extra: Vec<Element>) -> Vec<Frame> {
    let wall = |g: &str| Element::Wall {
        guid: g.into(),
        level: "L1".into(),
        wall_type: "T1".into(),
        axis: Curve::Line {
            a: (0.0, y0),
            b: (4.0, y0),
        },
        base_offset: 0.0,
        top: Top::Unconnected { height: 3.0 },
        offset0: None,
    };
    let mut el = vec![wall("W1")];
    el.extend(extra);
    let n = el.len() as u64;
    vec![
        Frame::Hello(proto::Hello {
            protocol: proto::PROTOCOL,
            bridge: "b".into(),
            version: "1".into(),
            formats: vec!["rvt".into()],
            capabilities: vec![],
        }),
        Frame::Units(proto::Units { length_to_m: units }),
        Frame::Levels(vec![proto::Level {
            guid: "L1".into(),
            name: "L1".into(),
            elevation: 0.0,
        }]),
        Frame::Types(vec![proto::Type::Wall {
            guid: "T1".into(),
            name: "T".into(),
            layers: vec![proto::Layer {
                material: "m".into(),
                thickness: 0.2,
            }],
        }]),
        Frame::Elements(el),
        Frame::End(proto::Stats {
            elements: n,
            warnings: 0,
        }),
    ]
}

fn wall(g: &str, x: f64) -> Element {
    Element::Wall {
        guid: g.into(),
        level: "L1".into(),
        wall_type: "T1".into(),
        axis: Curve::Line {
            a: (0.0, x),
            b: (4.0, x),
        },
        base_offset: 0.0,
        top: Top::Unconnected { height: 3.0 },
        offset0: None,
    }
}

#[test]
fn frame_import_counts_every_frame_and_a_cancel_changes_nothing() {
    let a = scratch("ctl").join("a.rvt");
    std::fs::write(&a, b"a").unwrap();
    let mut m = Model::open(":memory:").unwrap();
    let c = ddc_io::job::Progress::new();
    c.phase("job.stage.commit", 0.8, 1.0);
    let fr = frames(1.0, 0.0, vec![wall("W2", 1.0)]);
    let n = fr.len() as u64;
    let r = bridge::import_frames_ctl(&mut m, &a, fr, &c).unwrap();
    assert_eq!(r.walls, 2);
    assert_eq!(c.counts(), (n, n));
    assert!((c.fraction() - 1.0).abs() < 1e-6);
    let mut m = Model::open(":memory:").unwrap();
    let head = m.change_head();
    let c = ddc_io::job::Progress::new();
    c.cancel();
    let e = bridge::import_frames_ctl(&mut m, &a, frames(1.0, 0.0, vec![]), &c).unwrap_err();
    assert!(matches!(e, BridgeError::Cancelled), "{e}");
    assert_eq!(q(&m, "SELECT count(*) FROM wall"), 0.0);
    assert_eq!(m.change_head(), head);
}

#[test]
fn guids_are_namespaced_per_source_file() {
    let mut m = Model::open(":memory:").unwrap();
    let (a, b) = (scratch("ns").join("a.rvt"), scratch("ns").join("b.rvt"));
    std::fs::write(&a, b"a").unwrap();
    std::fs::write(&b, b"b").unwrap();
    let r = bridge::import_frames(&mut m, &a, frames(1.0, 0.0, vec![wall("W2", 1.0)])).unwrap();
    assert_eq!(r.walls, 2);
    // the same guids from another file are other elements, nothing is overwritten
    let r = bridge::import_frames(&mut m, &b, frames(1.0, 5.0, vec![])).unwrap();
    assert_eq!((r.walls, r.unchanged, r.updated), (1, 0, 0));
    assert!(
        r.missing.is_empty(),
        "missing is per source: {:?}",
        r.missing
    );
    assert_eq!(q(&m, "SELECT count(*) FROM wall"), 3.0);
    assert_eq!(
        q(
            &m,
            "SELECT count(DISTINCT source) FROM ext_ref WHERE guid='W1'"
        ),
        2.0
    );
    // re-import of the first file: unchanged, W2 missing only there
    let r = bridge::import_frames(&mut m, &a, frames(1.0, 0.0, vec![])).unwrap();
    assert_eq!((r.walls, r.unchanged), (0, 1));
    assert_eq!(r.missing, vec!["W2".to_string()]);
}

#[test]
fn guid_reused_across_kinds_is_keyed_not_overwritten() {
    let mut m = Model::open(":memory:").unwrap();
    let a = scratch("kind").join("a.rvt");
    std::fs::write(&a, b"a").unwrap();
    let r = bridge::import_frames(&mut m, &a, frames(1.0, 0.0, vec![wall("L1", 2.0)])).unwrap();
    assert_eq!(r.walls, 2);
    // the level keeps its link; the wall is linked under `guid#table`
    assert_eq!(
        q(
            &m,
            "SELECT count(*) FROM ext_ref WHERE guid='L1' AND kind='level'"
        ),
        1.0
    );
    assert_eq!(
        q(
            &m,
            "SELECT count(*) FROM ext_ref WHERE guid='L1#wall' AND kind='wall'"
        ),
        1.0
    );
    // and is found again on re-import: nothing duplicated or updated
    let r = bridge::import_frames(&mut m, &a, frames(1.0, 0.0, vec![wall("L1", 2.0)])).unwrap();
    assert_eq!((r.walls, r.updated, r.unchanged), (0, 0, 2));
    assert!(r.missing.is_empty(), "{:?}", r.missing);
    assert_eq!(q(&m, "SELECT count(*) FROM wall"), 2.0);
}

#[test]
fn unit_scale_and_scaled_values_are_bounded() {
    let a = scratch("units").join("a.rvt");
    std::fs::write(&a, b"a").unwrap();
    for k in [0.0, -1.0, 1e6, f64::MAX] {
        let mut m = Model::open(":memory:").unwrap();
        let before = counts(&m);
        let e = bridge::import_frames(&mut m, &a, frames(k, 0.0, vec![])).unwrap_err();
        assert!(matches!(e, BridgeError::Data(_)), "{k}: {e}");
        assert_eq!(counts(&m), before);
    }
    let mut m = Model::open(":memory:").unwrap();
    let before = counts(&m);
    let e = bridge::import_frames(&mut m, &a, frames(1000.0, 0.0, vec![wall("W9", 1e306)]))
        .unwrap_err();
    assert!(
        matches!(&e, BridgeError::Data(s) if s.contains("scaling")),
        "{e}"
    );
    assert_eq!(counts(&m), before);
    // frames must start with Hello
    let mut f = frames(1.0, 0.0, vec![]);
    f.remove(0);
    assert!(matches!(
        bridge::import_frames(&mut m, &a, f).unwrap_err(),
        BridgeError::Protocol(_)
    ));
}

/// Budget: 100k walls import (spawn, decode, map, one regeneration) under 10 s in release.
/// `cargo test --release -p ddc-io --test bridge -- --ignored budget_100k`
#[test]
#[ignore]
fn budget_100k_walls_release() {
    let mut m = Model::open(":memory:").unwrap();
    let t = Instant::now();
    let r = import(&mut m, &opts(&["--walls", "100000"])).unwrap();
    let dt = t.elapsed();
    eprintln!("100k walls: {dt:?}");
    assert_eq!(r.walls, 100_010);
    assert!(dt < Duration::from_secs(10), "{dt:?}");
}

/// Minor-2 frames: a 6×3 m curtain wall (feet on the wire) with a door panel and a sloped slab;
/// re-import changes nothing, undo removes everything.
#[test]
fn curtain_wall_and_slab_slope_frames() {
    let ft = 0.3048;
    let mut f = frames(ft, 0.0, vec![]);
    let cw = proto::CurtainWall {
        guid: "CW".into(),
        level: "L1".into(),
        name: "Storefront".into(),
        axis: Curve::Line {
            a: (0.0, 10.0 / ft),
            b: (6.0 / ft, 10.0 / ft),
        },
        base_offset: 0.0,
        height: 3.0 / ft,
        u: vec![1.5 / ft, 3.0 / ft, 4.5 / ft],
        v: vec![1.0 / ft, 2.0 / ft],
        interior: proto::MullionSection::Rect {
            width: 0.05 / ft,
            depth: 0.15 / ft,
        },
        border: proto::MullionSection::Rect {
            width: 0.05 / ft,
            depth: 0.15 / ft,
        },
        panels: vec![proto::CurtainPanel {
            i: 1,
            j: 0,
            kind: proto::PanelKind::Door,
            name: "Door".into(),
            family_type: None,
            swing_side: true,
            swing_hinge: false,
        }],
    };
    f.insert(4, Frame::CurtainWalls(vec![cw]));
    f.insert(
        4,
        Frame::Types(vec![proto::Type::Slab {
            guid: "ST".into(),
            name: "floor".into(),
            layers: vec![proto::Layer {
                material: "m".into(),
                thickness: 0.2 / ft,
            }],
        }]),
    );
    let slab = Element::Slab {
        guid: "S1".into(),
        level: "L1".into(),
        slab_type: "ST".into(),
        offset: 3.0 / ft,
        outer: vec![
            (0.0, 0.0),
            (6.0 / ft, 0.0),
            (6.0 / ft, 4.0 / ft),
            (0.0, 4.0 / ft),
        ],
        holes: vec![],
    };
    let el = f
        .iter()
        .position(|x| matches!(x, Frame::Elements(_)))
        .unwrap();
    if let Frame::Elements(v) = &mut f[el] {
        v.push(slab);
    }
    f.insert(
        el,
        Frame::SlabSlopes(vec![proto::SlabSlope {
            slab: "S1".into(),
            point: (0.0, 0.0),
            gradient: (0.02, 0.0),
        }]),
    );
    if let Some(Frame::End(s)) = f.last_mut() {
        s.elements += 1;
    }
    let a = scratch("cw").join("a.rvt");
    std::fs::write(&a, b"a").unwrap();
    let mut m = Model::open(":memory:").unwrap();
    let r = bridge::import_frames(&mut m, &a, f.clone()).unwrap();
    assert_eq!(
        (r.curtain_walls, r.curtain_panels, r.curtain_doors),
        (1, 12, 1)
    );
    assert_eq!((r.slabs, r.sloped_slabs), (1, 1));
    let id = m.curtain_walls().unwrap()[0];
    let qt = m.curtain_quantities(id).unwrap();
    assert_eq!((qt.cells, qt.glazed, qt.doors), (12, 11, 1));
    assert!(
        (qt.glass_area - (16.1 - 1.45 * 0.925)).abs() < 1e-9,
        "{}",
        qt.glass_area
    );
    assert!((qt.mullion_length - 38.0).abs() < 1e-9);
    let s = q(&m, "SELECT id FROM slab") as i64;
    let mesh = m.slab_mesh(s).unwrap();
    assert!((mesh.volume() - 4.8).abs() < 1e-9);
    let zmax = mesh.positions.iter().map(|p| p.z).fold(f64::MIN, f64::max);
    assert!((zmax - 3.12).abs() < 1e-9, "{zmax}");
    let r = bridge::import_frames(&mut m, &a, f).unwrap();
    assert_eq!((r.curtain_walls, r.updated, r.slabs), (0, 0, 0));
    assert!(r.unchanged >= 3, "{}", r.unchanged);
    m.undo().unwrap();
    m.undo().unwrap();
    assert_eq!(q(&m, "SELECT count(*) FROM curtain_wall"), 0.0);
    assert_eq!(q(&m, "SELECT count(*) FROM curtain_panel"), 0.0);
}

#[test]
fn oversized_curtain_grid_falls_back_to_mesh() {
    let mut f = frames(1.0, 0.0, vec![]);
    let cw = proto::CurtainWall {
        guid: "CWBIG".into(),
        level: "L1".into(),
        name: "Big".into(),
        axis: Curve::Line {
            a: (0.0, 0.0),
            b: (40.0, 0.0),
        },
        base_offset: 0.0,
        height: 30.0,
        // 400 × 300 cells: over the model limit
        u: (1..400).map(|k| k as f64 * 0.1).collect(),
        v: (1..300).map(|k| k as f64 * 0.1).collect(),
        interior: proto::MullionSection::None,
        border: proto::MullionSection::Rect {
            width: 0.05,
            depth: 0.15,
        },
        panels: vec![],
    };
    f.insert(4, Frame::CurtainWalls(vec![cw]));
    let a = scratch("cwbig").join("a.rvt");
    std::fs::write(&a, b"a").unwrap();
    let mut m = Model::open(":memory:").unwrap();
    let r = bridge::import_frames(&mut m, &a, f).unwrap();
    assert_eq!((r.curtain_walls, r.meshes), (0, 1), "{r:?}");
    assert!(r.warnings.iter().any(|w| w.contains("CWBIG")), "{r:?}");
    assert_eq!(q(&m, "SELECT count(*) FROM curtain_wall"), 0.0);
}

#[test]
fn members_mep_runs_and_grids_import_as_native_rows() {
    let mut m = Model::open(":memory:").unwrap();
    let a = scratch("mem").join("a.rvt");
    std::fs::write(&a, b"a").unwrap();
    let mut f = frames(1.0, 0.0, vec![]);
    let sec = proto::MullionSection::Rect {
        width: 0.3,
        depth: 0.5,
    };
    let extra = vec![
        Frame::Grids(vec![proto::Grid {
            guid: "G1".into(),
            name: "A".into(),
            a: (0.0, -1.0),
            b: (0.0, 9.0),
        }]),
        Frame::Members(vec![
            proto::Member {
                guid: "C1".into(),
                level: "L1".into(),
                kind: proto::MemberKind::Column,
                name: "C 300x500".into(),
                a: [1.0, 1.0, 0.0],
                b: [1.0, 1.0, 3.0],
                section: sec,
                rotation: 0.0,
                structural: true,
                profile: vec![],
            },
            proto::Member {
                guid: "B1".into(),
                level: "L1".into(),
                kind: proto::MemberKind::Beam,
                name: "C 300x500".into(),
                a: [1.0, 1.0, 2.75],
                b: [5.0, 1.0, 2.75],
                section: sec,
                rotation: 0.0,
                structural: true,
                profile: vec![],
            },
        ]),
        Frame::Types(vec![proto::Type::Slab {
            guid: "ST".into(),
            name: "Ceiling 20".into(),
            layers: vec![proto::Layer {
                material: "m".into(),
                thickness: 0.02,
            }],
        }]),
        Frame::Ceilings(vec![proto::Ceiling {
            guid: "CE1".into(),
            level: "L1".into(),
            slab_type: "ST".into(),
            height: 2.6,
            outer: vec![(0.0, 0.0), (4.0, 0.0), (4.0, 3.0), (0.0, 3.0)],
            holes: vec![],
        }]),
        Frame::Members(vec![proto::Member {
            guid: "I1".into(),
            level: "L1".into(),
            kind: proto::MemberKind::Beam,
            name: "I 200".into(),
            a: [0.0, 3.0, 2.5],
            b: [4.0, 3.0, 2.5],
            section: sec,
            rotation: 0.0,
            structural: true,
            // I-shape outline about the centre line
            profile: vec![
                (-0.05, -0.1),
                (0.05, -0.1),
                (0.05, -0.09),
                (0.004, -0.09),
                (0.004, 0.09),
                (0.05, 0.09),
                (0.05, 0.1),
                (-0.05, 0.1),
                (-0.05, 0.09),
                (-0.004, 0.09),
                (-0.004, -0.09),
                (-0.05, -0.09),
            ],
        }]),
        Frame::MepRuns(vec![proto::MepRun {
            guid: "P1".into(),
            level: "L1".into(),
            domain: "pipe".into(),
            name: "Pipe".into(),
            section: proto::MullionSection::Circle { diameter: 0.05 },
            points: vec![[0.0, 2.0, 2.5], [4.0, 2.0, 2.5]],
        }]),
    ];
    let end = f.pop().unwrap();
    f.extend(extra);
    f.push(end);
    let r = bridge::import_frames(&mut m, &a, f.clone()).unwrap();
    assert_eq!(
        (r.grids, r.columns, r.beams, r.mep_runs, r.ceilings),
        (1, 1, 2, 1, 1)
    );
    assert_eq!(q(&m, "SELECT count(*) FROM member_type"), 2.0);
    let poly = q(
        &m,
        "SELECT count(*) FROM member_type WHERE shape LIKE 'poly%'",
    );
    assert_eq!(poly, 1.0);
    let ce = q(&m, "SELECT id FROM ceiling") as i64;
    assert_eq!(m.ceiling_rings(ce).unwrap()[0].len(), 4);
    assert!((q(&m, "SELECT height FROM ceiling") - 2.6).abs() < 1e-9);
    assert!(m.ceiling_mesh(ce).is_ok());
    assert_eq!(q(&m, "SELECT top_offset FROM col"), 3.0);
    assert!(m
        .member_mesh("beam", q(&m, "SELECT id FROM beam") as i64)
        .is_ok());
    assert!(!m.mep_meshes(None).unwrap().is_empty());
    // imported framing keeps its source length: no joins at columns
    assert_eq!(
        q(
            &m,
            "SELECT count(*) FROM beam WHERE join_start='none' AND join_end='none'"
        ),
        2.0
    );
    let r = bridge::import_frames(&mut m, &a, f).unwrap();
    assert_eq!((r.columns, r.updated, r.unchanged), (0, 0, 7));
    m.undo().unwrap();
    m.undo().unwrap();
    assert_eq!(
        q(&m, "SELECT count(*) FROM col") + q(&m, "SELECT count(*) FROM mep_run"),
        0.0
    );
    assert_eq!(
        q(&m, "SELECT count(*) FROM ceiling") + q(&m, "SELECT count(*) FROM ceiling_vertex"),
        0.0
    );
}

fn column_frame(guid: &str) -> Frame {
    Frame::Members(vec![proto::Member {
        guid: guid.into(),
        level: "L1".into(),
        kind: proto::MemberKind::Column,
        name: "C 300x500".into(),
        a: [1.0, 1.0, 0.0],
        b: [1.0, 1.0, 3.0],
        section: proto::MullionSection::Rect {
            width: 0.3,
            depth: 0.5,
        },
        rotation: 0.0,
        structural: true,
        profile: vec![],
    }])
}

/// A store imported before native members (every column a mesh row) re-imports without
/// duplicates: the mesh becomes a column, its properties move along, undo brings the mesh back.
#[test]
fn reimport_turns_old_mesh_rows_into_native_members() {
    let mut m = Model::open(":memory:").unwrap();
    let a = scratch("migrate").join("a.rvt");
    std::fs::write(&a, b"a").unwrap();
    let body = Element::Generic {
        guid: "C1".into(),
        level: Some("L1".into()),
        category: "Structural Columns".into(),
        name: "C".into(),
        positions: vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 3.0],
        ],
        indices: vec![0, 1, 2, 0, 1, 3],
    };
    let mut f = frames(1.0, 0.0, vec![body]);
    let end = f.pop().unwrap();
    f.push(Frame::Properties(vec![proto::Property {
        owner: "C1".into(),
        key: "Mark".into(),
        value: proto::Value::Text("A1".into()),
        unit: String::new(),
    }]));
    f.push(end);
    bridge::import_frames(&mut m, &a, f).unwrap();
    assert_eq!(q(&m, "SELECT count(*) FROM mesh"), 1.0);
    let mut f = frames(1.0, 0.0, vec![]);
    let end = f.pop().unwrap();
    f.push(column_frame("C1"));
    f.push(end);
    let r = bridge::import_frames(&mut m, &a, f.clone()).unwrap();
    assert_eq!((r.columns, r.migrated), (1, 1));
    assert!(r.missing.is_empty(), "{:?}", r.missing);
    assert_eq!(q(&m, "SELECT count(*) FROM mesh"), 0.0);
    assert_eq!(q(&m, "SELECT count(*) FROM col"), 1.0);
    assert_eq!(
        q(
            &m,
            "SELECT count(*) FROM prop WHERE kind='col' AND key='Mark'"
        ),
        1.0
    );
    assert_eq!(q(&m, "SELECT count(*) FROM prop WHERE kind='mesh'"), 0.0);
    assert_eq!(
        q(&m, "SELECT count(*) FROM ext_ref WHERE guid LIKE 'C1%'"),
        1.0
    );
    // the next re-import finds the column again
    let r = bridge::import_frames(&mut m, &a, f).unwrap();
    assert_eq!((r.columns, r.migrated), (0, 0));
    assert_eq!(q(&m, "SELECT count(*) FROM col"), 1.0);
    m.undo().unwrap();
    m.undo().unwrap();
    assert_eq!(q(&m, "SELECT count(*) FROM col"), 0.0);
    assert_eq!(q(&m, "SELECT count(*) FROM mesh"), 1.0);
    assert_eq!(q(&m, "SELECT count(*) FROM prop WHERE kind='mesh'"), 1.0);
    assert_eq!(
        q(
            &m,
            "SELECT count(*) FROM ext_ref WHERE guid='C1' AND kind='mesh'"
        ),
        1.0
    );
}

/// A ceiling with one sliver hole keeps its outline and good holes.
#[test]
fn ceiling_with_degenerate_hole_is_kept() {
    let mut m = Model::open(":memory:").unwrap();
    let a = scratch("cehole").join("a.rvt");
    std::fs::write(&a, b"a").unwrap();
    let mut f = frames(1.0, 0.0, vec![]);
    let end = f.pop().unwrap();
    f.push(Frame::Types(vec![proto::Type::Slab {
        guid: "ST".into(),
        name: "Ceiling 20".into(),
        layers: vec![proto::Layer {
            material: "m".into(),
            thickness: 0.02,
        }],
    }]));
    f.push(Frame::Ceilings(vec![proto::Ceiling {
        guid: "CE1".into(),
        level: "L1".into(),
        slab_type: "ST".into(),
        height: 2.6,
        outer: vec![(0.0, 0.0), (4.0, 0.0), (4.0, 3.0), (0.0, 3.0)],
        holes: vec![
            vec![(1.0, 1.0), (2.0, 1.0), (3.0, 1.0)],
            vec![(1.0, 1.0), (1.5, 1.0), (1.5, 1.5), (1.0, 1.5)],
        ],
    }]));
    f.push(end);
    let r = bridge::import_frames(&mut m, &a, f).unwrap();
    assert_eq!(r.ceilings, 1);
    let ce = q(&m, "SELECT id FROM ceiling") as i64;
    assert_eq!(m.ceiling_rings(ce).unwrap().len(), 2);
}

fn prism(guid: &str) -> proto::Prism {
    proto::Prism {
        guid: guid.into(),
        level: "L1".into(),
        name: "Panel".into(),
        category: "curtain panels".into(),
        material: "Glass".into(),
        o: [1.0, 2.0, 3.0],
        x: [1.0, 0.0, 0.0],
        y: [0.0, 1.0, 0.0],
        outline: vec![(-1.0, -0.5), (1.0, -0.5), (1.0, 0.5), (-1.0, 0.5)],
        depth: 0.1,
    }
}

fn with_prisms(extra: Vec<Element>, prisms: Vec<proto::Prism>) -> Vec<Frame> {
    let mut f = frames(1.0, 0.0, extra);
    let end = f.pop().unwrap();
    f.push(Frame::Prisms(prisms));
    f.push(end);
    f
}

#[test]
fn prism_becomes_a_native_solid_and_reimports_in_place() {
    let mut m = Model::open(":memory:").unwrap();
    let a = scratch("prism").join("a.rvt");
    std::fs::write(&a, b"a").unwrap();
    let f = with_prisms(vec![], vec![prism("PR1")]);
    let r = bridge::import_frames(&mut m, &a, f.clone()).unwrap();
    assert_eq!(r.solids, 1);
    assert_eq!(q(&m, "SELECT count(*) FROM mesh"), 0.0);
    assert_eq!(
        q(
            &m,
            "SELECT count(*) FROM solid WHERE kind='extrusion' AND void=0"
        ),
        1.0
    );
    let id = q(&m, "SELECT id FROM solid") as i64;
    let spec = m.solid(id).unwrap();
    assert_eq!(spec.material, "Glass");
    // volume and world height of the measured body
    assert!((q(&m, "SELECT volume FROM solid_qty") - 0.2).abs() < 1e-6);
    assert!((q(&m, "SELECT z0 FROM solid_qty") - 3.0).abs() < 1e-6);
    assert_eq!(
        q(
            &m,
            "SELECT count(*) FROM prop WHERE kind='solid' AND key='Category'"
        ),
        1.0
    );

    let r = bridge::import_frames(&mut m, &a, f).unwrap();
    assert_eq!(r.solids, 0);
    assert_eq!(q(&m, "SELECT count(*) FROM solid"), 1.0);
    assert_eq!(q(&m, "SELECT id FROM solid") as i64, id);
}

#[test]
fn prism_replaces_the_mesh_of_an_earlier_import_and_undoes() {
    let mut m = Model::open(":memory:").unwrap();
    let a = scratch("prismig").join("a.rvt");
    std::fs::write(&a, b"a").unwrap();
    let mesh = Element::Generic {
        guid: "PR1".into(),
        level: Some("L1".into()),
        name: "Panel".into(),
        category: "curtain panels".into(),
        positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        indices: vec![0, 1, 2],
    };
    bridge::import_frames(&mut m, &a, frames(1.0, 0.0, vec![mesh])).unwrap();
    assert_eq!(q(&m, "SELECT count(*) FROM mesh"), 1.0);
    let r = bridge::import_frames(&mut m, &a, with_prisms(vec![], vec![prism("PR1")])).unwrap();
    assert_eq!((r.solids, r.migrated), (1, 1));
    assert_eq!(q(&m, "SELECT count(*) FROM mesh"), 0.0);
    assert_eq!(q(&m, "SELECT count(*) FROM solid"), 1.0);
    m.undo().unwrap();
    assert_eq!(q(&m, "SELECT count(*) FROM mesh"), 1.0);
    assert_eq!(q(&m, "SELECT count(*) FROM solid"), 0.0);
}

#[test]
fn invalid_prism_is_skipped_with_a_warning() {
    let mut m = Model::open(":memory:").unwrap();
    let a = scratch("prismbad").join("a.rvt");
    std::fs::write(&a, b"a").unwrap();
    let mut bad = prism("PR1");
    bad.outline.truncate(2);
    let r = bridge::import_frames(&mut m, &a, with_prisms(vec![], vec![bad])).unwrap();
    assert_eq!(r.solids, 0);
    assert_eq!(r.skipped.get("prism: invalid"), Some(&1));
    assert_eq!(q(&m, "SELECT count(*) FROM solid"), 0.0);
}
