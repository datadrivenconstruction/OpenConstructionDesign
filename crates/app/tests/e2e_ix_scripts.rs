// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Every event script in `tests/ix/*.ixs` replays on an empty editor without a window and
//! its checks hold (INTERACTIONS.md §2).
use ddc_app::editor::Editor;
use ddc_app::interact::script::{parse, run};
use std::time::{Duration, Instant};

/// Budget of the whole set in a debug build.
const BUDGET: Duration = Duration::from_secs(3);

#[test]
fn all_scripts_pass() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ix");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "ixs"))
        .collect();
    files.sort();
    assert!(files.len() >= 20, "{} scripts", files.len());
    let mut bad = vec![];
    let mut spent = Duration::ZERO;
    for f in &files {
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        let src = std::fs::read_to_string(f).unwrap();
        let script = match parse(&src) {
            Ok(s) => s,
            Err(e) => {
                bad.push(format!("{name}:{e}"));
                continue;
            }
        };
        let mut ed = Editor::new_in_memory().unwrap();
        let t = Instant::now();
        let r = run(&mut ed, &script);
        spent += t.elapsed();
        for x in r.failures {
            bad.push(format!("{name}:{}: want {} got {}", x.line, x.want, x.got));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
    eprintln!("{} scripts in {:?}", files.len(), spent);
    assert!(spent <= BUDGET, "{spent:?} over {BUDGET:?}");
}

fn refused_door() -> Editor {
    let src = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ix/refusal_opening_wider.ixs"))
        .unwrap();
    let mut ed = Editor::new_in_memory().unwrap();
    let r = run(&mut ed, &parse(&src).unwrap());
    assert!(r.ok(), "{:?}", r.failures);
    ed
}

/// Wall 600, door 900: each fix on offer is one journal operation.
#[test]
fn refusal_fix_is_one_op() {
    let ed = refused_door();
    let offer = ed.ix.offer().cloned().expect("the refusal offers fixes");
    assert_eq!(offer.code.as_str(), "opening_wider_than_wall");
    assert_eq!(offer.fixes.len(), 2);
    assert_eq!(offer.args.len(), 2);

    // fix 1: width 500, the door goes in
    let mut ed = refused_door();
    let base = ed.pushes;
    assert!(ddc_app::refusals::set_opening_width(&mut ed, 500.0));
    assert_eq!(ed.pushes, base + 1, "width fix is one operation");
    assert_eq!(ed.openings.len(), 1);

    // fix 2: the wall grows to 1100, one operation, and the door fits afterwards
    let mut ed = refused_door();
    let id = ed.walls[0].id;
    let base = ed.pushes;
    assert!(ddc_app::refusals::extend_wall(&mut ed, id, 1100.0));
    assert_eq!(ed.pushes, base + 1, "extend fix is one operation");
    assert!(((ed.walls[0].b - ed.walls[0].a).len() - 1.1).abs() < 1e-9);
}
