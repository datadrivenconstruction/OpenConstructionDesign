//! Exchange safety, IFC edited copy: whatever the patch writer does (patches, copies, refuses),
//! the source file keeps its bytes and its modification time. The only file it may create is the
//! output the caller names.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ddc_io::ifc::patch::{check_patched, patch_file, PatchError};
use ddc_io::ifc::{file_sha256, import_ifc_file, write_ifc_file};
use ddc_model::{LayerSpec, Model, OpeningSpec, SlabSpec, Target, WallSpec};

struct Fx {
    dir: PathBuf,
    src: PathBuf,
    out: PathBuf,
    m: Model,
    key: String,
}

fn lay(material: &str, thickness: f64) -> LayerSpec {
    LayerSpec {
        material: material.into(),
        thickness,
    }
}

/// An exported file, imported again so that the model knows it as its source.
fn setup(tag: &str) -> Fx {
    let dir = std::env::temp_dir().join(format!("ddc-exchange-copy-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let mut a = Model::open(":memory:").unwrap();
    let l1 = a.add_level("L1", 0.0).unwrap();
    let wt = a
        .add_wall_type("W200", &[lay("brick", 0.1), lay("insulation", 0.1)])
        .unwrap();
    let st = a.add_slab_type("S250", &[lay("concrete", 0.25)]).unwrap();
    let p = [(0.0, 0.0), (5.0, 0.0), (5.0, 4.0), (0.0, 4.0)];
    let mut first = 0;
    for k in 0..4 {
        let id = a
            .add_wall(&WallSpec {
                a: p[k],
                b: p[(k + 1) % 4],
                level: l1,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wt,
                offset0: None,
            })
            .unwrap();
        if k == 0 {
            first = id;
        }
    }
    a.add_opening(
        first,
        &OpeningSpec {
            along: 1.0,
            width: 0.9,
            sill: 0.0,
            height: 2.1,
            kind: "door".into(),
        },
    )
    .unwrap();
    a.add_slab(&SlabSpec {
        level: l1,
        slab_type: st,
        offset: 0.0,
        outer: p.to_vec(),
        holes: vec![],
    })
    .unwrap();

    let src = dir.join("source.ifc");
    write_ifc_file(&a, &src).unwrap();
    let mut m = Model::open(dir.join("work.ddc").to_str().unwrap()).unwrap();
    import_ifc_file(&mut m, &src).unwrap();
    let key = m.sources().unwrap().remove(0).source;
    Fx {
        out: dir.join("copy.ifc"),
        dir,
        src,
        m,
        key,
    }
}

/// Everything about the source file that an unwanted write would change.
#[derive(Debug, PartialEq)]
struct Stamp {
    bytes: Vec<u8>,
    sha: String,
    modified: SystemTime,
    len: u64,
}

fn stamp(p: &Path) -> Stamp {
    let md = fs::metadata(p).unwrap();
    Stamp {
        bytes: fs::read(p).unwrap(),
        sha: file_sha256(p).unwrap(),
        modified: md.modified().unwrap(),
        len: md.len(),
    }
}

fn wall_id(m: &Model) -> i64 {
    m.sql()
        .query_row("SELECT id FROM wall ORDER BY id LIMIT 1", [], |r| r.get(0))
        .unwrap()
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn a_copy_without_edits_leaves_the_source_alone_and_equals_it() {
    let f = setup("noedit");
    let before = stamp(&f.src);
    let p = patch_file(&f.m, &f.key, &f.src, &f.out, None)
        .unwrap_or_else(|e| panic!("patch without edits failed: {e:?}"));
    assert!(p.no_changes, "no edit was made, yet the patch reports changes");
    assert_eq!(stamp(&f.src), before, "the source changed during an unedited copy");
    assert_eq!(fs::read(&f.out).unwrap(), before.bytes, "the copy differs from the source");
    assert_ne!(f.out, f.src);
    let _ = fs::remove_dir_all(&f.dir);
}

#[test]
fn an_edited_copy_is_written_beside_the_source_and_the_source_keeps_its_bytes() {
    let mut f = setup("edit");
    let before = stamp(&f.src);
    let names_before = files_in(&f.dir);
    let id = wall_id(&f.m);
    f.m.move_elements(&[Target::Wall(id)], (1.0, 0.5)).unwrap();

    match patch_file(&f.m, &f.key, &f.src, &f.out, None) {
        Ok(p) => {
            if let Some(e) = &p.expect {
                check_patched(&f.out, e, None)
                    .unwrap_or_else(|e| panic!("the patched copy fails its read-back: {e}"));
            }
            assert!(f.out.exists(), "the patch reported success but wrote no copy");
        }
        // a refusal is allowed (the caller then rewrites to a new file); it must not touch the source
        Err(PatchError::Doubt { .. } | PatchError::Failed(_)) => {}
    }
    assert_eq!(stamp(&f.src), before, "the source changed while the edited copy was made");
    let mut expect = names_before;
    if f.out.exists() {
        expect.push("copy.ifc".into());
        expect.sort();
    }
    let after = files_in(&f.dir);
    for n in &after {
        assert!(
            expect.contains(n) || n.starts_with("work.ddc"),
            "the patch left an unexpected file {n} in {:?}",
            f.dir
        );
    }
    let _ = fs::remove_dir_all(&f.dir);
}

#[test]
fn a_source_changed_since_the_import_refuses_and_stays_as_it_is() {
    let mut f = setup("changed");
    let id = wall_id(&f.m);
    f.m.move_elements(&[Target::Wall(id)], (1.0, 0.0)).unwrap();
    // somebody else rewrote the file after we imported it
    let mut text = fs::read_to_string(&f.src).unwrap();
    text.push_str("\n/* edited elsewhere */\n");
    fs::write(&f.src, &text).unwrap();
    let before = stamp(&f.src);

    let r = patch_file(&f.m, &f.key, &f.src, &f.out, None);
    assert!(
        r.is_err(),
        "a patch over a source that changed since the import must refuse"
    );
    assert_eq!(stamp(&f.src), before, "the refused patch touched the source");
    let _ = fs::remove_dir_all(&f.dir);
}

#[test]
fn naming_the_source_as_the_output_never_destroys_it() {
    let f = setup("same");
    let before = stamp(&f.src);
    let _ = patch_file(&f.m, &f.key, &f.src, &f.src, None);
    let after = fs::read(&f.src).unwrap();
    assert_eq!(
        after, before.bytes,
        "output == source emptied or rewrote the original ({} -> {} bytes)",
        before.bytes.len(),
        after.len()
    );
    let _ = fs::remove_dir_all(&f.dir);
}

#[test]
fn hard_link_target_refused() {
    let f = setup("hardlink");
    let link = f.dir.join("linked.ifc");
    if fs::hard_link(&f.src, &link).is_err() {
        let _ = fs::remove_dir_all(&f.dir);
        return; // the file system has no hard links
    }
    let before = stamp(&f.src);
    let r = patch_file(&f.m, &f.key, &f.src, &link, None);
    assert!(
        matches!(&r, Err(PatchError::Failed(m)) if m.contains("source file")),
        "a hard link to the source must be refused, got {:?}",
        r.map(|_| ())
    );
    assert_eq!(stamp(&f.src), before, "a refused copy touched the source");
    let _ = fs::remove_dir_all(&f.dir);
}

#[test]
fn the_source_named_again_in_another_spelling_is_refused_before_any_copy() {
    let f = setup("spelling");
    let before = stamp(&f.src);
    let listed = files_in(&f.dir);
    // the same file through a detour (and, on Windows, in upper case)
    let detour = f.dir.join(".").join(f.src.file_name().unwrap());
    let mut spellings = vec![f.src.clone(), detour];
    if cfg!(windows) {
        let name = f.src.file_name().unwrap().to_string_lossy().to_uppercase();
        spellings.push(f.dir.join(name));
    }
    for out in &spellings {
        let r = patch_file(&f.m, &f.key, &f.src, out, None);
        assert!(
            matches!(&r, Err(PatchError::Failed(m)) if m.contains("source file")),
            "{}: output == source must be refused, got {:?}",
            out.display(),
            r.map(|_| ())
        );
    }
    assert_eq!(stamp(&f.src), before, "a refused copy touched the source");
    assert_eq!(files_in(&f.dir), listed, "a refused copy created a file");
    let _ = fs::remove_dir_all(&f.dir);
}

#[test]
fn a_missing_source_is_an_error_and_creates_no_output() {
    let f = setup("missing");
    let gone = f.dir.join("gone.ifc");
    let r = patch_file(&f.m, &f.key, &gone, &f.out, None);
    assert!(r.is_err(), "patching a file that does not exist must fail");
    assert!(!gone.exists(), "the patch created the missing source");
    let _ = fs::remove_dir_all(&f.dir);
}
