//! A big IFC file patched in place of a rewrite: thousands of property edits, moves, deletes and
//! new walls, and afterwards every edit reads back, every untouched statement is byte for byte
//! the one of the source, deleted walls are gone with their relations, the source file is never
//! written and a cancelled patch leaves nothing behind.
//!
//! Debug runs use 20 000 walls; the 100 000 wall run is `#[ignore]`:
//! `cargo test --release -p ddc_io --test ifc_patch_big -- --ignored --nocapture`.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use ddc_io::ifc::patch::{check_patched, patch_file, PatchError, PatchOutput};
use ddc_io::ifc::{import_ifc_file, write_ifc_file};
use ddc_io::job::Progress;
use ddc_model::source::SqlValue;
use ddc_model::{LayerSpec, Model, Target, WallSpec};

const WALLS: usize = 20_000;
/// Wall time allowed to `patch_file` on 20 000 walls in a debug build.
const DEBUG_PATCH_BUDGET: Duration = Duration::from_secs(150);
/// Wall time allowed to `patch_file` on 100 000 walls in a release build.
const RELEASE_100K_PATCH_BUDGET: Duration = Duration::from_secs(60);

const SHARED_MATERIAL_REL: u32 = 56;
const SHARED_CONTAINMENT_REL: u32 = 57;
const FIRST: u32 = 1000;
const BLOCK: u32 = 20;
const WALL_AT: u32 = 9;
const MARK_AT: u32 = 10;
const COMMENTS_AT: u32 = 11;
const PSET_AT: u32 = 12;
const REL_AT: u32 = 13;

fn base(i: usize) -> u32 {
    FIRST + BLOCK * i as u32
}

fn guid(k: usize) -> String {
    format!("{:022}", 10_000_000 + k)
}

fn wall_guid(i: usize) -> String {
    guid(3 * i)
}

/// What the edits do, by wall index: the same rules at every size.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Fate {
    Mark,
    Comments,
    Moved,
    Deleted,
    Untouched,
}

fn fate(i: usize) -> Fate {
    match (i % 20, i % 40, i % 100) {
        (0, _, _) => Fate::Mark,
        (10, _, _) => Fate::Comments,
        (_, 5, _) => Fate::Moved,
        (_, _, 7) => Fate::Deleted,
        _ => Fate::Untouched,
    }
}

fn count(n: usize, f: Fate) -> usize {
    (0..n).filter(|i| fate(*i) == f).count()
}

fn adds(n: usize) -> usize {
    n / 200
}

/// IFC4 in millimetres: `n` walls along +Y, each with its own geometry, a property set `Own`
/// with `Mark` and `Comments`, and its own relation; the layer usage and the containment are
/// shared by all of them.
fn big_ifc(n: usize) -> String {
    let mut t = String::with_capacity(n * 1700 + 4096);
    t.push_str(
        "ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [CoordinationView]'),'2;1');
FILE_NAME('big.ifc','2020-01-01T00:00:00',(''),(''),'','','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);
#2=IFCUNITASSIGNMENT((#1));
#3=IFCPROJECT('0000000000000000000001',#4,'P',$,$,$,$,(#7),#2);
#4=IFCOWNERHISTORY(#5,#6,$,.ADDED.,$,$,$,0);
#5=IFCPERSONANDORGANIZATION(#8,#9,$);
#6=IFCAPPLICATION(#9,'1','a','a');
#8=IFCPERSON($,$,'a',$,$,$,$,$);
#9=IFCORGANIZATION($,'o',$,$,$);
#10=IFCCARTESIANPOINT((0.,0.,0.));
#11=IFCAXIS2PLACEMENT3D(#10,$,$);
#7=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.E-5,#11,$);
#17=IFCGEOMETRICREPRESENTATIONSUBCONTEXT('Body','Model',*,*,*,*,#7,$,.MODEL_VIEW.,$);
#18=IFCGEOMETRICREPRESENTATIONSUBCONTEXT('Axis','Model',*,*,*,*,#7,$,.MODEL_VIEW.,$);
#12=IFCLOCALPLACEMENT($,#11);
#13=IFCCARTESIANPOINT((0.,0.,3000.));
#14=IFCAXIS2PLACEMENT3D(#13,$,$);
#15=IFCLOCALPLACEMENT(#12,#14);
#16=IFCBUILDINGSTOREY('0000000000000000000002',#4,'L1',$,$,#15,$,$,.ELEMENT.,3000.);
#21=IFCDIRECTION((0.,0.,1.));
#22=IFCDIRECTION((0.,1.,0.));
#30=IFCCARTESIANPOINT((0.,0.));
#31=IFCCARTESIANPOINT((4000.,0.));
#34=IFCCARTESIANPOINT((2000.,0.));
#35=IFCAXIS2PLACEMENT2D(#34,$);
#37=IFCAXIS2PLACEMENT3D(#10,$,$);
#50=IFCMATERIAL('Brick');
#52=IFCMATERIALLAYER(#50,250.,$);
#54=IFCMATERIALLAYERSET((#52),'Ext 250');
#55=IFCMATERIALLAYERSETUSAGE(#54,.AXIS2.,.POSITIVE.,-125.);
",
    );
    let mut walls = String::with_capacity(n * 8);
    for i in 0..n {
        let b = base(i);
        let (x, y) = (6000 * (i % 200), 6000 * (i / 200));
        let (g, gs, gr) = (guid(3 * i), guid(3 * i + 1), guid(3 * i + 2));
        let _ = write!(
            t,
            "#{p}=IFCCARTESIANPOINT(({x}.,{y}.,0.));
#{a}=IFCAXIS2PLACEMENT3D(#{p},#21,#22);
#{lp}=IFCLOCALPLACEMENT(#15,#{a});
#{pl}=IFCPOLYLINE((#30,#31));
#{ar}=IFCSHAPEREPRESENTATION(#18,'Axis','Curve2D',(#{pl}));
#{pr}=IFCRECTANGLEPROFILEDEF(.AREA.,$,#35,4000.,250.);
#{so}=IFCEXTRUDEDAREASOLID(#{pr},#37,#21,2800.);
#{br}=IFCSHAPEREPRESENTATION(#17,'Body','SweptSolid',(#{so}));
#{pd}=IFCPRODUCTDEFINITIONSHAPE($,$,(#{ar},#{br}));
#{w}=IFCWALLSTANDARDCASE('{g}',#4,'W{i}',$,$,#{lp},#{pd},$,$);
#{mk}=IFCPROPERTYSINGLEVALUE('Mark',$,IFCLABEL('M{i}'),$);
#{cm}=IFCPROPERTYSINGLEVALUE('Comments',$,IFCLABEL('C{i}'),$);
#{ps}=IFCPROPERTYSET('{gs}',#4,'Own',$,(#{mk},#{cm}));
#{rl}=IFCRELDEFINESBYPROPERTIES('{gr}',#4,$,$,(#{w}),#{ps});
",
            p = b,
            a = b + 1,
            lp = b + 2,
            pl = b + 3,
            ar = b + 4,
            pr = b + 5,
            so = b + 6,
            br = b + 7,
            pd = b + 8,
            w = b + WALL_AT,
            mk = b + MARK_AT,
            cm = b + COMMENTS_AT,
            ps = b + PSET_AT,
            rl = b + REL_AT,
        );
        if i > 0 {
            walls.push(',');
        }
        let _ = write!(walls, "#{}", b + WALL_AT);
    }
    let _ = write!(
        t,
        "#{SHARED_MATERIAL_REL}=IFCRELASSOCIATESMATERIAL('0000000000000000000004',#4,$,$,({walls}),#55);
#{SHARED_CONTAINMENT_REL}=IFCRELCONTAINEDINSPATIALSTRUCTURE('0000000000000000000005',#4,$,$,({walls}),#16);
ENDSEC;
END-ISO-10303-21;
"
    );
    t
}

fn statements(t: &str) -> Vec<&str> {
    t.split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

fn id_of(stmt: &str) -> Option<u32> {
    let rest = stmt.strip_prefix('#')?;
    let n = rest.find('=')?;
    rest[..n].trim().parse().ok()
}

/// Every `#n` outside strings of a statement.
fn refs_of(stmt: &str) -> Vec<u32> {
    let b = stmt.as_bytes();
    let (mut i, mut in_str, mut out) = (0, false, vec![]);
    while i < b.len() {
        match b[i] {
            b'\'' => in_str = !in_str,
            b'#' if !in_str => {
                let n = b[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
                out.push(stmt[i + 1..i + 1 + n].parse().unwrap());
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// Every `#n` outside strings has a statement, and no id is repeated.
fn assert_refs_valid(t: &str) {
    let all = statements(t);
    let ids: HashSet<u32> = all.iter().filter_map(|s| id_of(s)).collect();
    assert_eq!(
        ids.len(),
        all.iter().filter_map(|s| id_of(s)).count(),
        "repeated id"
    );
    for s in &all {
        // the id before `=` is the statement's own, not a reference
        let body = s.find('=').map_or(*s, |k| &s[k..]);
        for r in refs_of(body) {
            assert!(ids.contains(&r), "#{r} is not in the file: {s:.80}");
        }
    }
}

type Xy = (f64, f64, f64, f64);

fn wall_xy(m: &Model, id: i64) -> Xy {
    m.sql()
        .query_row("SELECT ax,ay,bx,by FROM wall WHERE id=?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .unwrap()
}

fn close(a: Xy, b: Xy) -> bool {
    [a.0 - b.0, a.1 - b.1, a.2 - b.2, a.3 - b.3]
        .iter()
        .all(|d| d.abs() < 1e-6)
}

fn prop(m: &Model, id: i64, key: &str) -> Option<String> {
    m.sql()
        .query_row(
            "SELECT value FROM prop WHERE kind='wall' AND ref=?1 AND key=?2",
            rusqlite::params![id, key],
            |r| r.get::<_, String>(0),
        )
        .ok()
}

fn mark(i: usize) -> String {
    format!("MK-{i}")
}

fn comments(i: usize) -> String {
    format!("CM-{i}")
}

/// The model before the patch: the big file imported, then edited.
struct Prep {
    dir: PathBuf,
    src: PathBuf,
    out: PathBuf,
    m: Model,
    key: String,
    text: String,
    moved: Vec<(usize, Xy)>,
    added: Vec<Xy>,
    n: usize,
    secs: Vec<(&'static str, f64)>,
}

fn lap(secs: &mut Vec<(&'static str, f64)>, what: &'static str, t: &mut Instant) {
    secs.push((what, t.elapsed().as_secs_f64()));
    *t = Instant::now();
}

/// A model of `n` walls is written to IFC, the file is imported as the source, and then the
/// edits of `fate` and `adds` are made.
fn prepare(tag: &str, n: usize) -> Prep {
    let mut secs = vec![];
    let mut t = Instant::now();
    let dir = std::env::temp_dir().join(format!("ddc-tst14-{tag}-{n}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("out")).unwrap();
    let src = dir.join("big.ifc");
    let text = big_ifc(n);
    fs::write(&src, &text).unwrap();
    lap(&mut secs, "generate", &mut t);

    // the writer reads the generated file back as a model of its own: it must hold the same walls
    {
        let mut g = Model::open(dir.join("gen.ddc").to_str().unwrap()).unwrap();
        import_ifc_file(&mut g, &src).unwrap();
        let k: i64 = g
            .sql()
            .query_row("SELECT count(*) FROM wall", [], |r| r.get(0))
            .unwrap();
        assert_eq!(k as usize, n, "the generated file imports with every wall");
        write_ifc_file(&g, &dir.join("rewritten.ifc")).unwrap();
        lap(&mut secs, "write_ifc_file", &mut t);
    }

    let mut m = Model::open(dir.join("w.ddc").to_str().unwrap()).unwrap();
    import_ifc_file(&mut m, &src).unwrap();
    let key = m.sources().unwrap().remove(0).source;
    lap(&mut secs, "import", &mut t);

    let mut p = Prep {
        out: dir.join("out").join("out.ifc"),
        dir,
        src,
        m,
        key,
        text,
        moved: vec![],
        added: vec![],
        n,
        secs,
    };
    edit(&mut p);
    lap(&mut p.secs, "edit", &mut t);
    p
}

fn edit(p: &mut Prep) {
    let n = p.n;
    let mut moves = vec![];
    let mut ids = HashMap::new();
    for i in 0..n {
        let f = fate(i);
        if f == Fate::Untouched {
            continue;
        }
        let (kind, id) = p.m.element_by_guid(&p.key, &wall_guid(i)).unwrap().unwrap();
        assert_eq!(kind, "wall");
        ids.insert(i, id);
        match f {
            Fate::Mark => p
                .m
                .set_source_prop(&kind, id, "Own.Mark", SqlValue::Text(mark(i)), "")
                .unwrap(),
            Fate::Comments => p
                .m
                .set_source_prop(&kind, id, "Own.Comments", SqlValue::Text(comments(i)), "")
                .unwrap(),
            Fate::Moved => moves.push(i),
            Fate::Deleted => p.m.delete_wall(id).unwrap(),
            Fate::Untouched => {}
        }
    }
    let targets: Vec<Target> = moves.iter().map(|i| Target::Wall(ids[i])).collect();
    let before: Vec<Xy> = moves.iter().map(|i| wall_xy(&p.m, ids[i])).collect();
    p.m.move_elements(&targets, (1.0, 0.5)).unwrap();
    for (k, i) in moves.iter().enumerate() {
        let now = wall_xy(&p.m, ids[i]);
        let b = before[k];
        assert!(
            close(now, (b.0 + 1.0, b.1 + 0.5, b.2 + 1.0, b.3 + 0.5)),
            "wall {i} moved in the model"
        );
        p.moved.push((*i, now));
    }

    let level: i64 = p
        .m
        .sql()
        .query_row("SELECT id FROM level", [], |r| r.get(0))
        .unwrap();
    let t = p
        .m
        .add_wall_type(
            "tst14",
            &[LayerSpec {
                material: "concrete".into(),
                thickness: 0.2,
            }],
        )
        .unwrap();
    for k in 0..adds(n) {
        let (ax, ay) = (-20.0, 6.0 * k as f64);
        let id = p
            .m
            .add_wall(&WallSpec {
                a: (ax, ay),
                b: (ax + 4.0, ay),
                level,
                base_offset: 0.0,
                height: 2.5,
                wall_type: t,
                offset0: None,
            })
            .unwrap();
        p.added.push(wall_xy(&p.m, id));
    }
}

/// The patched file and what is needed to judge it.
struct Big {
    n: usize,
    dir: PathBuf,
    src: PathBuf,
    out: PathBuf,
    text: String,
    moved: Vec<(usize, Xy)>,
    added: Vec<Xy>,
    out_info: PatchOutput,
    patch_time: Duration,
    check_time: Duration,
}

fn patch_it(p: Prep) -> Big {
    let mut t = Instant::now();
    let info = patch_file(&p.m, &p.key, &p.src, &p.out, None).unwrap();
    let patch_time = t.elapsed();
    t = Instant::now();
    let e = info.expect.as_ref().expect("the big patch changes the file");
    check_patched(&p.out, e, None).unwrap();
    let check_time = t.elapsed();
    let mut line = format!("BIGTIMES n={}", p.n);
    for (what, s) in &p.secs {
        let _ = write!(line, " {what}={s:.2}s");
    }
    eprintln!(
        "{line} patch={:.2}s check={:.2}s out={} bytes",
        patch_time.as_secs_f64(),
        check_time.as_secs_f64(),
        info.stats.bytes
    );
    Big {
        n: p.n,
        dir: p.dir,
        src: p.src,
        out: p.out,
        text: p.text,
        moved: p.moved,
        added: p.added,
        out_info: info,
        patch_time,
        check_time,
    }
}

fn big() -> &'static Big {
    static B: OnceLock<Big> = OnceLock::new();
    B.get_or_init(|| patch_it(prepare("shared", WALLS)))
}

/// The output opened as a fresh model.
fn reread(b: &Big) -> (Model, String) {
    let copy = b.dir.join("back.ifc");
    fs::copy(&b.out, &copy).unwrap();
    let mut m = Model::open(b.dir.join("back.ddc").to_str().unwrap()).unwrap();
    let t = Instant::now();
    import_ifc_file(&mut m, &copy).unwrap();
    eprintln!("BIGTIMES reimport={:.2}s", t.elapsed().as_secs_f64());
    let key = m.sources().unwrap().remove(0).source;
    (m, key)
}

fn check_edits_read_back(b: &Big) {
    let (back, key) = reread(b);
    let (mut seen_mark, mut seen_comments, mut kept) = (0, 0, 0);
    for i in 0..b.n {
        let f = fate(i);
        let row = back.element_by_guid(&key, &wall_guid(i)).unwrap();
        if f == Fate::Deleted {
            assert!(row.is_none(), "deleted wall {i} is back");
            continue;
        }
        let (_, id) = row.unwrap_or_else(|| panic!("wall {i} is missing after the patch"));
        let want_mark = if f == Fate::Mark { mark(i) } else { format!("M{i}") };
        let want_comments = if f == Fate::Comments {
            comments(i)
        } else {
            format!("C{i}")
        };
        assert_eq!(prop(&back, id, "Own.Mark").as_deref(), Some(&*want_mark), "wall {i}");
        assert_eq!(
            prop(&back, id, "Own.Comments").as_deref(),
            Some(&*want_comments),
            "wall {i}"
        );
        match f {
            Fate::Mark => seen_mark += 1,
            Fate::Comments => seen_comments += 1,
            _ => kept += 1,
        }
    }
    assert_eq!(seen_mark, count(b.n, Fate::Mark));
    assert_eq!(seen_comments, count(b.n, Fate::Comments));
    assert!(kept > 0);

    for (i, xy) in &b.moved {
        let (_, id) = back.element_by_guid(&key, &wall_guid(*i)).unwrap().unwrap();
        assert!(close(wall_xy(&back, id), *xy), "wall {i} moved: {:?} vs {xy:?}", wall_xy(&back, id));
    }
    assert_eq!(b.moved.len(), count(b.n, Fate::Moved));

    let total: i64 = back
        .sql()
        .query_row("SELECT count(*) FROM wall", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        total as usize,
        b.n - count(b.n, Fate::Deleted) + adds(b.n),
        "walls after the patch"
    );
    for xy in &b.added {
        let found: i64 = back
            .sql()
            .query_row(
                "SELECT count(*) FROM wall WHERE abs(ax-?1)<1e-6 AND abs(ay-?2)<1e-6 AND abs(bx-?3)<1e-6 AND abs(by-?4)<1e-6",
                rusqlite::params![xy.0, xy.1, xy.2, xy.3],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(found, 1, "new wall at {xy:?}");
    }
    assert_eq!(b.added.len(), adds(b.n));
}

fn check_untouched_bytes(b: &Big) {
    let out = fs::read_to_string(&b.out).unwrap();
    let have: HashSet<&str> = statements(&out).into_iter().collect();
    let lost: Vec<u32> = statements(&b.text)
        .into_iter()
        .filter(|s| !have.contains(s))
        .filter_map(id_of)
        .collect();
    let (dels, edited) = (
        count(b.n, Fate::Deleted),
        count(b.n, Fate::Mark) + count(b.n, Fate::Comments),
    );

    // what changed is exactly what was edited: its property, its host, the wall's own block
    let mut deleted_walls = HashSet::new();
    let mut expect: HashSet<u32> = HashSet::new();
    for i in 0..b.n {
        let at = base(i);
        match fate(i) {
            Fate::Mark => {
                expect.insert(at + MARK_AT);
            }
            Fate::Comments => {
                expect.insert(at + COMMENTS_AT);
            }
            Fate::Moved => {
                expect.insert(at + WALL_AT);
            }
            Fate::Deleted => {
                deleted_walls.insert(at + WALL_AT);
            }
            Fate::Untouched => {}
        }
    }
    let mut lost_set = HashSet::new();
    for id in &lost {
        if *id == SHARED_MATERIAL_REL || *id == SHARED_CONTAINMENT_REL {
            assert!(dels > 0, "#{id}: a shared list changed without a delete");
            continue;
        }
        assert!(*id >= FIRST, "#{id}: a shared statement of the header changed");
        let i = ((*id - FIRST) / BLOCK) as usize;
        assert!(i < b.n, "#{id} is outside the walls");
        if fate(i) == Fate::Deleted {
            continue;
        }
        lost_set.insert(*id);
    }
    assert_eq!(
        lost_set, expect,
        "statements of untouched elements changed, or an edit left its statement as it was"
    );

    // deleted walls: wall, set and relation are gone, nothing refers to them, the shared lists
    // still hold every other wall
    let present: HashSet<u32> = statements(&out).iter().filter_map(|s| id_of(s)).collect();
    let guids_left: HashSet<String> = (0..b.n)
        .filter(|i| fate(*i) != Fate::Deleted)
        .map(wall_guid)
        .collect();
    for i in (0..b.n).filter(|i| fate(*i) == Fate::Deleted) {
        let at = base(i);
        for off in [WALL_AT, MARK_AT, COMMENTS_AT, PSET_AT, REL_AT] {
            assert!(!present.contains(&(at + off)), "wall {i}: #{} is still in the file", at + off);
        }
        assert!(!out.contains(&wall_guid(i)), "wall {i} guid is still in the file");
        assert!(!guids_left.contains(&wall_guid(i)));
    }
    for s in statements(&out) {
        let body = s.find('=').map_or(s, |k| &s[k..]);
        if !body.contains('#') {
            continue;
        }
        for r in refs_of(body) {
            assert!(!deleted_walls.contains(&r), "#{r} of a deleted wall is still referred to: {s:.80}");
        }
    }
    for rel in [SHARED_MATERIAL_REL, SHARED_CONTAINMENT_REL] {
        let line = statements(&out)
            .into_iter()
            .find(|s| id_of(s) == Some(rel))
            .unwrap_or_else(|| panic!("#{rel} is gone"));
        let held: HashSet<u32> = refs_of(&line[line.find('=').unwrap()..]).into_iter().collect();
        for i in (0..b.n).filter(|i| fate(*i) != Fate::Deleted) {
            assert!(held.contains(&(base(i) + WALL_AT)), "#{rel} lost wall {i}");
        }
    }
    assert_refs_valid(&out);

    // new walls are appended above everything that was there
    let top = statements(&b.text).iter().filter_map(|s| id_of(s)).max().unwrap();
    let new_walls: Vec<u32> = statements(&out)
        .iter()
        .filter(|s| s.contains("=IFCWALL("))
        .filter_map(|s| id_of(s))
        .collect();
    assert_eq!(new_walls.len(), adds(b.n));
    assert!(new_walls.iter().all(|id| *id > top));
    assert_eq!(b.out_info.added as usize, adds(b.n));
    assert!(b.out_info.will_not.is_empty(), "{:?}", b.out_info.will_not);
    assert!(b.out_info.edited as usize >= count(b.n, Fate::Moved));
    assert!(edited > 0);

    // the source file was only read, and the output is the one file written
    assert_eq!(fs::read_to_string(&b.src).unwrap(), b.text, "the source file changed");
    let files: Vec<_> = fs::read_dir(b.out.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(files.len(), 1, "files next to the output: {files:?}");
    assert_eq!(Path::new(&files[0]), Path::new(b.out.file_name().unwrap()));
}

#[test]
fn patch_big_edits_read_back() {
    check_edits_read_back(big());
}

#[test]
fn patch_big_untouched_bytes_identical() {
    let b = big();
    check_untouched_bytes(b);
    eprintln!(
        "BIGTIMES budget patch={:.2}s of {:?}, check={:.2}s",
        b.patch_time.as_secs_f64(),
        DEBUG_PATCH_BUDGET,
        b.check_time.as_secs_f64()
    );
    if cfg!(debug_assertions) {
        assert!(b.patch_time < DEBUG_PATCH_BUDGET, "patch took {:?}", b.patch_time);
    }
}

#[test]
fn patch_big_cancel_leaves_nothing() {
    let p = prepare("cancel", WALLS);
    let ctl = Progress::new();
    let reached = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let watcher = {
        let (ctl, reached, done) = (ctl.clone(), reached.clone(), done.clone());
        std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                if ctl.fraction() >= 0.5 {
                    reached.store(true, Ordering::Relaxed);
                    ctl.cancel();
                    return;
                }
                std::thread::yield_now();
            }
        })
    };
    let r = patch_file(&p.m, &p.key, &p.src, &p.out, Some(&ctl));
    done.store(true, Ordering::Relaxed);
    watcher.join().unwrap();

    assert!(reached.load(Ordering::Relaxed), "the patch ended before half of the work");
    match r {
        Err(PatchError::Failed(_)) => {}
        Err(e) => panic!("a cancel is a failure, not a doubt: {e}"),
        Ok(_) => panic!("the patch finished after the cancel"),
    }
    assert!(!p.out.exists(), "a cancelled patch left its output");
    let left: Vec<_> = fs::read_dir(p.out.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(left.is_empty(), "temp files left: {left:?}");
    assert_eq!(fs::read_to_string(&p.src).unwrap(), p.text, "the source changed");
}

/// 100 000 walls, release build: `cargo test --release -p ddc_io --test ifc_patch_big -- --ignored --nocapture`.
#[test]
#[ignore]
fn patch_100k_release_budget() {
    let b = patch_it(prepare("k100", 100_000));
    assert!(
        b.patch_time < RELEASE_100K_PATCH_BUDGET,
        "patch took {:?}, budget {:?}",
        b.patch_time,
        RELEASE_100K_PATCH_BUDGET
    );
    check_edits_read_back(&b);
    check_untouched_bytes(&b);
}
