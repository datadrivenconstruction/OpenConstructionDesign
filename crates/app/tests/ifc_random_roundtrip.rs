//! Seeded round trip of the IFC patch save: random edits through `Editor::run_op`, save beside
//! the source, reopen. The source stays byte for byte, kept elements keep their GlobalIds and
//! edited values, deleted ones are gone, added ones are there, and untouched walls keep their
//! statements byte for byte. A failure prints the seed, the step and the operation.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use ddc_app::editor::{Editor, OpFn};
use ddc_io::ifc::import_ifc_file;
use ddc_io::ifc::patch::{check_patched, patch_file, PatchError, DOUBT_TYPES};
use ddc_model::source::{show, SqlValue};
use ddc_model::{Edit, Model, OpeningSpec, Target, WallSpec};

const SCHEMAS: [&str; 3] = ["IFC2X3", "IFC4", "IFC4X3_ADD2"];
const WALLS: usize = 5;
const STEPS: usize = 30;
const TOL: f64 = 1e-4;
const PROP: &str = "Own.Fire";

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn f(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * (self.next() % 10_000) as f64 / 10_000.0
    }
}

fn g(n: usize) -> String {
    format!("{n:022}")
}

/// First id of the statements of wall `i`; the block spans 50 ids.
fn base(i: usize) -> usize {
    200 + 50 * i
}

fn wall_guid(i: usize) -> String {
    g(1000 + 10 * i)
}

/// Millimetres, a level L1 with five parallel walls (even ones hold a door in an opening), a
/// slab, an own property set per wall, one set shared by all, a second level L2, a class the
/// importer skips, a comment and a statement over two lines.
fn fixture(schema: &str) -> String {
    let (wall, door) = if schema == "IFC2X3" {
        ("", "")
    } else {
        (",$", ",$,$,$")
    };
    let mut s: Vec<String> = vec![
        "ISO-10303-21;".into(),
        "HEADER;".into(),
        "FILE_DESCRIPTION(('ViewDefinition [CoordinationView]'),'2;1');".into(),
        "FILE_NAME('f.ifc','2020-01-01T00:00:00',(''),(''),'','','');".into(),
        format!("FILE_SCHEMA(('{schema}'));"),
        "ENDSEC;".into(),
        "DATA;".into(),
        "/* five walls along +Y, 4000 long, 3000 apart */".into(),
    ];
    let fixed = [
        "#1=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);",
        "#2=IFCUNITASSIGNMENT((#1));",
        "#3=IFCPROJECT('0000000000000000000001',#4,'P',$,$,$,$,(#7),#2);",
        "#4=IFCOWNERHISTORY(#5,#6,$,.ADDED.,$,$,$,0);",
        "#5=IFCPERSONANDORGANIZATION(#8,#9,$);",
        "#6=IFCAPPLICATION(#9,'1','a','a');",
        "#8=IFCPERSON($,$,'a',$,$,$,$,$);",
        "#9=IFCORGANIZATION($,'o',$,$,$);",
        "#10=IFCCARTESIANPOINT((0.,0.,0.));",
        "#11=IFCAXIS2PLACEMENT3D(#10,$,$);",
        "#7=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.E-5,#11,$);",
        "#17=IFCGEOMETRICREPRESENTATIONSUBCONTEXT('Body','Model',*,*,*,*,#7,$,.MODEL_VIEW.,$);",
        "#18=IFCGEOMETRICREPRESENTATIONSUBCONTEXT('Axis','Model',*,*,*,*,#7,$,.MODEL_VIEW.,$);",
        "#12=IFCLOCALPLACEMENT($,#11);",
        "#13=IFCCARTESIANPOINT((0.,0.,3000.));",
        "#14=IFCAXIS2PLACEMENT3D(#13,$,$);",
        "#15=IFCLOCALPLACEMENT(#12,#14);",
        "#16=IFCBUILDINGSTOREY('0000000000000000000002',#4,'L1',$,$,#15,$,$,.ELEMENT.,3000.);",
        "#21=IFCDIRECTION((0.,0.,1.));",
        "#22=IFCDIRECTION((0.,1.,0.));",
        "#34=IFCCARTESIANPOINT((2000.,0.));",
        "#35=IFCAXIS2PLACEMENT2D(#34,$);",
        "#37=IFCAXIS2PLACEMENT3D(#10,$,$);",
        "#66=IFCDIRECTION((0.,-1.,0.));",
        "#67=IFCDIRECTION((1.,0.,0.));",
        "#68=IFCAXIS2PLACEMENT3D(#10,#66,#67);",
        "#50=IFCMATERIAL('Brick');",
        "#52=IFCMATERIALLAYER(#50,250.,$);",
        "#54=IFCMATERIALLAYERSET((#52),'Ext 250');",
        "#55=IFCMATERIALLAYERSETUSAGE(#54,.AXIS2.,.POSITIVE.,-125.);",
        "#81=IFCRECTANGLEPROFILEDEF(.AREA.,$,#35,4000.,2000.);",
        "#82=IFCEXTRUDEDAREASOLID(#81,#37,#21,200.);",
        "#83=IFCSHAPEREPRESENTATION(#17,'Body','SweptSolid',(#82));",
        "#84=IFCPRODUCTDEFINITIONSHAPE($,$,(#83));",
        "#110=IFCPROPERTYSINGLEVALUE('Grade',$,IFCLABEL('A'),$);",
        "#111=IFCPROPERTYSET('0000000000000000000110',#4,'Common',$,(#110));",
        "#150=IFCCARTESIANPOINT((0.,0.,6000.));",
        "#151=IFCAXIS2PLACEMENT3D(#150,$,$);",
        "#152=IFCLOCALPLACEMENT(#12,#151);",
    ];
    s.extend(fixed.iter().map(|x| x.to_string()));
    s.push(format!(
        "#153=IFCBUILDINGSTOREY('{}',#4,'L2',$,$,#152,$,$,.ELEMENT.,6000.);",
        g(2)
    ));
    s.push(format!(
        "#80=IFCSLAB('{}',#4,'S',$,$,#15,#84,$,.FLOOR.);",
        g(1100)
    ));
    let mut walls = vec![];
    let mut contained = vec!["#80".to_string()];
    for i in 0..WALLS {
        let b = base(i);
        let x = 1000 + 3000 * i;
        let gw = wall_guid(i);
        for l in [
            format!("#{b}=IFCCARTESIANPOINT(({x}.,2000.,0.));"),
            format!("#{}=IFCAXIS2PLACEMENT3D(#{b},#21,#22);", b + 1),
            format!("#{}=IFCLOCALPLACEMENT(#15,#{});", b + 2, b + 1),
            format!("#{}=IFCCARTESIANPOINT((0.,0.));", b + 3),
            format!("#{}=IFCCARTESIANPOINT((4000.,0.));", b + 4),
            format!("#{}=IFCPOLYLINE((#{},#{}));", b + 5, b + 3, b + 4),
            format!(
                "#{}=IFCSHAPEREPRESENTATION(#18,'Axis','Curve2D',(#{}));",
                b + 6,
                b + 5
            ),
            format!(
                "#{}=IFCRECTANGLEPROFILEDEF(.AREA.,$,#35,4000.,250.);",
                b + 8
            ),
            format!("#{}=IFCEXTRUDEDAREASOLID(#{},#37,#21,2800.);", b + 9, b + 8),
            format!(
                "#{}=IFCSHAPEREPRESENTATION(#17,'Body','SweptSolid',(#{}));",
                b + 10,
                b + 9
            ),
            format!(
                "#{}=IFCPRODUCTDEFINITIONSHAPE($,$,(#{},#{}));",
                b + 11,
                b + 6,
                b + 10
            ),
            format!(
                "#{}=IFCWALLSTANDARDCASE('{gw}',#4,'W{i}',$,$,#{},#{},${wall});",
                b + 12,
                b + 2,
                b + 11
            ),
            format!(
                "#{}=IFCPROPERTYSINGLEVALUE('Fire',$,IFCLABEL('EI 30'),$);",
                b + 26
            ),
            format!(
                "#{}=IFCPROPERTYSET('{}',#4,'Own',$,(#{}));",
                b + 27,
                g(3000 + i),
                b + 26
            ),
            format!(
                "#{}=IFCRELDEFINESBYPROPERTIES('{}',#4,$,$,(#{}),#{});",
                b + 28,
                g(5000 + 10 * i + 2),
                b + 12,
                b + 27
            ),
        ] {
            s.push(l);
        }
        walls.push(format!("#{}", b + 12));
        contained.push(format!("#{}", b + 12));
        if i % 2 == 0 {
            for l in [
                format!("#{}=IFCCARTESIANPOINT((1000.,200.,0.));", b + 13),
                format!("#{}=IFCAXIS2PLACEMENT3D(#{},$,$);", b + 14, b + 13),
                format!("#{}=IFCLOCALPLACEMENT(#{},#{});", b + 15, b + 2, b + 14),
                format!("#{}=IFCCARTESIANPOINT((450.,1050.));", b + 16),
                format!("#{}=IFCAXIS2PLACEMENT2D(#{},$);", b + 17, b + 16),
                format!(
                    "#{}=IFCRECTANGLEPROFILEDEF(.AREA.,$,#{},900.,2100.);",
                    b + 18,
                    b + 17
                ),
                format!("#{}=IFCEXTRUDEDAREASOLID(#{},#68,#21,400.);", b + 19, b + 18),
                format!(
                    "#{}=IFCSHAPEREPRESENTATION(#17,'Body','SweptSolid',(#{}));",
                    b + 20,
                    b + 19
                ),
                format!("#{}=IFCPRODUCTDEFINITIONSHAPE($,$,(#{}));", b + 21, b + 20),
                format!(
                    "#{}=IFCOPENINGELEMENT('{}',#4,'O{i}',$,$,#{},#{},${wall});",
                    b + 22,
                    g(1001 + 10 * i),
                    b + 15,
                    b + 21
                ),
                format!(
                    "#{}=IFCRELVOIDSELEMENT('{}',#4,$,$,#{},#{});",
                    b + 23,
                    g(5000 + 10 * i),
                    b + 12,
                    b + 22
                ),
                format!(
                    "#{}=IFCDOOR('{}',#4,'D{i}',$,$,#{},$,$,2100.,900.{door});",
                    b + 24,
                    g(1002 + 10 * i),
                    b + 15
                ),
                format!(
                    "#{}=IFCRELFILLSELEMENT('{}',#4,$,$,#{},#{});",
                    b + 25,
                    g(5000 + 10 * i + 1),
                    b + 22,
                    b + 24
                ),
            ] {
                s.push(l);
            }
            contained.push(format!("#{}", b + 24));
        }
    }
    s.push(format!(
        "#56=IFCRELASSOCIATESMATERIAL('{}',#4,$,$,({}),#55);",
        g(4000),
        walls.join(",")
    ));
    s.push(format!(
        "#57=IFCRELCONTAINEDINSPATIALSTRUCTURE('{}',#4,$,$,({}),#16);",
        g(4001),
        contained.join(",")
    ));
    s.push(format!(
        "#112=IFCRELDEFINESBYPROPERTIES('{}',#4,$,$,({},#80),#111);",
        g(4002),
        walls.join(",")
    ));
    s.push("#900=IFCANNOTATION('0000000000000000000900',$,'kept',$,$,$,$);".into());
    s.push("#901=IFCPROPERTYSINGLEVALUE('Spread',\n   'over two lines',IFCLABEL('x'),$);".into());
    s.push("ENDSEC;".into());
    s.push("END-ISO-10303-21;".into());
    s.join("\n") + "\n"
}

fn statements(t: &str) -> Vec<String> {
    t.split(';')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn id_of(stmt: &str) -> Option<usize> {
    let s = stmt
        .rsplit('\n')
        .find(|l| l.starts_with('#'))
        .unwrap_or(stmt);
    let rest = s.strip_prefix('#')?;
    let n = rest.find('=')?;
    rest[..n].trim().parse().ok()
}

/// Every `#n` outside strings has a statement, and no id repeats.
fn refs_problem(t: &str) -> Option<String> {
    let st = statements(t);
    let ids: HashSet<usize> = st.iter().filter_map(|s| id_of(s)).collect();
    if ids.len() != st.iter().filter_map(|s| id_of(s)).count() {
        return Some("repeated id".into());
    }
    let data = &t[t.find("DATA;")?..];
    let b = data.as_bytes();
    let (mut i, mut in_str) = (0, false);
    while i < b.len() {
        match b[i] {
            // a doubled quote inside a string is an escaped quote
            b'\'' if in_str && b.get(i + 1) == Some(&b'\'') => i += 1,
            b'\'' => in_str = !in_str,
            // a comment outside strings: its quotes and `#` mean nothing
            b'/' if !in_str && b.get(i + 1) == Some(&b'*') => match data[i + 2..].find("*/") {
                Some(n) => i += 2 + n + 1,
                None => return Some("comment without an end".into()),
            },
            b'#' if !in_str => {
                let n = b[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
                let id: usize = data[i + 1..i + 1 + n].parse().ok()?;
                if !ids.contains(&id) {
                    return Some(format!("#{id} is not in the file"));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn hash_bytes(b: &[u8]) -> u64 {
    // FNV-1a
    b.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, c| {
        (h ^ *c as u64).wrapping_mul(0x100_0000_01b3)
    })
}

/// Hash of the rows an op can change: geometry tables, levels, types and source properties.
fn fingerprint(m: &Model) -> u64 {
    let mut s = String::new();
    for t in ["wall", "opening", "slab", "level", "wall_type"] {
        let mut st = m
            .sql()
            .prepare(&format!("SELECT * FROM \"{t}\" ORDER BY id"))
            .unwrap();
        let n = st.column_count();
        let mut rows = st.query([]).unwrap();
        while let Some(r) = rows.next().unwrap() {
            for i in 0..n {
                s.push_str(&format!("{:?}|", r.get::<_, rusqlite::types::Value>(i).unwrap()));
            }
            s.push('\n');
        }
    }
    for id in ids(m, "wall") {
        if let Ok(params) = m.source_params("wall", id) {
            for p in params {
                s.push_str(&format!("{id}:{}={}\n", p.key, show(&p.value)));
            }
        }
    }
    hash_bytes(s.as_bytes())
}

/// `#n` references outside strings in one statement.
fn refs_in(st: &str) -> Vec<usize> {
    let b = st.as_bytes();
    let (mut i, mut in_str, mut v) = (0, false, vec![]);
    while i < b.len() {
        match b[i] {
            b'\'' => in_str = !in_str,
            b'#' if !in_str => {
                let n = b[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
                if let Ok(id) = st[i + 1..i + 1 + n].parse() {
                    v.push(id);
                }
            }
            _ => {}
        }
        i += 1;
    }
    v
}

type Quad = (f64, f64, f64, f64, String);

fn wall_quad(m: &Model, id: i64) -> Quad {
    m.sql()
        .query_row(
            "SELECT w.ax,w.ay,w.bx,w.by,l.name FROM wall w JOIN level l ON l.id=w.level \
             WHERE w.id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap()
}

fn quads(m: &Model) -> Vec<Quad> {
    let ids = ids(m, "wall");
    ids.into_iter().map(|i| wall_quad(m, i)).collect()
}

fn near(a: &Quad, b: &Quad) -> bool {
    a.4 == b.4
        && (a.0 - b.0).abs() < TOL
        && (a.1 - b.1).abs() < TOL
        && (a.2 - b.2).abs() < TOL
        && (a.3 - b.3).abs() < TOL
}

fn ids(m: &Model, table: &str) -> Vec<i64> {
    let mut st = m
        .sql()
        .prepare(&format!("SELECT id FROM \"{table}\" ORDER BY id"))
        .unwrap();
    let v = st
        .query_map([], |r| r.get::<_, i64>(0))
        .unwrap()
        .map(|x| x.unwrap())
        .collect();
    v
}

fn count(m: &Model, table: &str) -> i64 {
    m.sql()
        .query_row(&format!("SELECT count(*) FROM \"{table}\""), [], |r| r.get(0))
        .unwrap()
}

fn prop_text(m: &Model, id: i64) -> Option<String> {
    m.source_params("wall", id)
        .unwrap()
        .into_iter()
        .find(|p| p.key == PROP)
        .map(|p| show(&p.value))
}

fn run(ed: &mut Editor, f: impl Fn(&mut Model) -> Result<(), ddc_model::Error> + Send + Sync + 'static) -> Result<(), ddc_model::Error> {
    ed.run_op(OpFn(Arc::new(f)))
}

/// The save was refused as a whole; a level change may be one the writer cannot express.
struct Refused(String);

struct Case<'a> {
    seed: u64,
    schema: &'a str,
    log: Vec<String>,
}

impl Case<'_> {
    fn fail(&self, msg: &str) -> ! {
        let step = self.log.len().saturating_sub(1);
        panic!(
            "seed {} {} step {} op {:?}: {msg}\nops so far: {:#?}",
            self.seed,
            self.schema,
            step,
            self.log.last(),
            self.log
        );
    }
}

fn refused_changed_nothing(c: &Case, ed: &Editor, src: &Path, before: (u64, u64)) {
    let now = (fingerprint(&ed.model), hash_bytes(&fs::read(src).unwrap()));
    if now.0 != before.0 {
        c.fail("a refused op changed the model");
    }
    if now.1 != before.1 {
        c.fail("a refused op changed the source file");
    }
}

fn python_problems(file: &Path) -> Option<HashSet<String>> {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/validate_ifc.py");
    if !script.exists() {
        return None;
    }
    let py = if cfg!(windows) { "python" } else { "python3" };
    let o = Command::new(py).arg("-I").arg(&script).arg(file).output().ok()?;
    if o.status.code()? > 1 {
        return None;
    }
    let text = String::from_utf8_lossy(&o.stdout).to_string();
    Some(
        text.lines()
            .map(|l| {
                l.chars()
                    .filter(|c| !c.is_ascii_digit() && *c != '#')
                    .collect::<String>()
            })
            .filter(|l| !l.trim().is_empty())
            .collect(),
    )
}

fn run_seed(seed: u64, schema: &str, levels: bool) -> Result<bool, Refused> {
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "ddc-tst7-{schema}-{seed}-{}-{}",
        levels as u8,
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let src = dir.join("a.ifc");
    fs::write(&src, fixture(schema)).unwrap();
    let before = fs::read(&src).unwrap();
    let before_hash = hash_bytes(&before);

    let mut c = Case {
        seed,
        schema,
        log: vec![],
    };
    let mut ed = Editor::new_in_memory().unwrap();
    ed.model = Model::open(dir.join("w.ddc").to_str().unwrap()).unwrap();
    {
        let p = src.clone();
        run(&mut ed, move |m| {
            import_ifc_file(m, &p)
                .map(|_| ())
                .map_err(|e| ddc_model::Error::Invalid(e.to_string()))
        })
        .unwrap_or_else(|e| c.fail(&format!("import: {e}")));
    }
    let key = ed.model.sources().unwrap().remove(0).source;

    // source walls: db id -> index in the fixture
    let mut index = std::collections::HashMap::new();
    for (id, (guid, _)) in ed.model.source_ids("wall").unwrap() {
        if let Some(i) = (0..WALLS).find(|i| wall_guid(*i) == guid) {
            index.insert(id, i);
        }
    }
    if index.len() != WALLS {
        c.fail(&format!("import found {} of {WALLS} walls", index.len()));
    }
    let all_guids: Vec<(String, String)> = {
        let mut v = vec![];
        for kind in ["wall", "opening", "slab"] {
            for (_, (guid, _)) in ed.model.source_ids(kind).unwrap() {
                v.push((kind.to_string(), guid));
            }
        }
        v
    };
    let levels_ids = ids(&ed.model, "level");
    let wtype: i64 = ed
        .model
        .sql()
        .query_row("SELECT min(id) FROM wall_type", [], |r| r.get(0))
        .unwrap();

    let mut geo: HashSet<i64> = HashSet::new(); // walls with a geometry or host edit
    let mut propped: HashSet<i64> = HashSet::new();
    let mut level_used = false;
    let mut rng = Rng::new(seed);
    for step in 0..STEPS {
        let alive = ids(&ed.model, "wall");
        let pick = rng.below(alive.len().max(1));
        let wid = alive.get(pick).copied();
        let kind = rng.below(15);
        let kind = if !levels && kind >= 13 { 0 } else { kind };
        let (dx, dy, ang) = (rng.f(-1.0, 1.0), rng.f(-1.0, 1.0), rng.f(-3.0, 3.0));
        let (ax, ay, len) = (rng.f(20.0, 40.0), rng.f(-30.0, 30.0), rng.f(3.0, 5.0));
        let (along, other) = (rng.f(0.5, 3.0), rng.below(levels_ids.len()));
        let n = rng.below(900) + 100;
        let Some(wid) = wid else {
            // nothing to edit: add a wall
            c.log.push(format!("{step}: add wall (no walls left)"));
            let (lv, t) = (levels_ids[0], wtype);
            let snap = (fingerprint(&ed.model), hash_bytes(&fs::read(&src).unwrap()));
            let r = run(&mut ed, move |m| {
                m.add_wall(&WallSpec {
                    a: (ax, ay),
                    b: (ax + len, ay),
                    level: lv,
                    base_offset: 0.0,
                    height: 2.5,
                    wall_type: t,
                    offset0: None,
                })
                .map(|_| ())
            });
            if let Err(e) = r {
                c.log.push(format!("   skipped: {e}"));
                refused_changed_nothing(&c, &ed, &src, snap);
            }
            continue;
        };
        let (x, y) = {
            let q = wall_quad(&ed.model, wid);
            ((q.0 + q.2) / 2.0, (q.1 + q.3) / 2.0)
        };
        let snap = (fingerprint(&ed.model), hash_bytes(&fs::read(&src).unwrap()));
        let r = match kind {
            0..=2 => {
                c.log.push(format!("{step}: move wall {wid} by ({dx:.3},{dy:.3})"));
                geo.insert(wid);
                run(&mut ed, move |m| {
                    m.move_elements(&[Target::Wall(wid)], (dx, dy))
                })
            }
            3..=4 => {
                c.log.push(format!("{step}: rotate wall {wid} by {ang:.3}"));
                geo.insert(wid);
                run(&mut ed, move |m| {
                    m.rotate_elements(&[Target::Wall(wid)], (x, y), ang, false)
                        .map(|_| ())
                })
            }
            5..=7 => {
                c.log.push(format!("{step}: set {PROP} of wall {wid} to EI {n}"));
                if !index.contains_key(&wid) {
                    c.log.push("   skipped: not a source wall".into());
                    continue;
                }
                propped.insert(wid);
                run(&mut ed, move |m| {
                    m.set_source_prop(
                        "wall",
                        wid,
                        PROP,
                        SqlValue::Text(format!("EI {n}")),
                        "",
                    )
                })
            }
            8 => {
                c.log.push(format!("{step}: delete wall {wid}"));
                geo.insert(wid);
                run(&mut ed, move |m| m.delete_wall(wid))
            }
            9..=10 => {
                c.log.push(format!("{step}: add wall at ({ax:.2},{ay:.2}) len {len:.2}"));
                let (lv, t) = (levels_ids[0], wtype);
                run(&mut ed, move |m| {
                    m.add_wall(&WallSpec {
                        a: (ax, ay),
                        b: (ax + len, ay),
                        level: lv,
                        base_offset: 0.0,
                        height: 2.5,
                        wall_type: t,
                        offset0: None,
                    })
                    .map(|_| ())
                })
            }
            11..=12 => {
                c.log.push(format!("{step}: add door to wall {wid} along {along:.2}"));
                geo.insert(wid);
                run(&mut ed, move |m| {
                    m.add_opening(
                        wid,
                        &OpeningSpec {
                            along,
                            width: 0.9,
                            sill: 0.0,
                            height: 2.1,
                            kind: "door".into(),
                        },
                    )
                    .map(|_| ())
                })
            }
            _ => {
                let lv = levels_ids[other];
                c.log.push(format!("{step}: wall {wid} to level {lv}"));
                geo.insert(wid);
                level_used = true;
                run(&mut ed, move |m| {
                    let (level, wall_type, a, b, base_offset, height, offset0, bulge): (
                        i64,
                        i64,
                        (f64, f64),
                        (f64, f64),
                        f64,
                        f64,
                        Option<f64>,
                        f64,
                    ) = m
                        .sql()
                        .query_row(
                            "SELECT level,wtype,ax,ay,bx,by,base_offset,height,offset0,bulge \
                             FROM wall WHERE id=?1",
                            [wid],
                            |r| {
                                Ok((
                                    r.get(0)?,
                                    r.get(1)?,
                                    (r.get(2)?, r.get(3)?),
                                    (r.get(4)?, r.get(5)?),
                                    r.get(6)?,
                                    r.get(7)?,
                                    r.get(8)?,
                                    r.get(9)?,
                                ))
                            },
                        )
                        .map_err(|e| ddc_model::Error::Invalid(e.to_string()))?;
                    let _ = level;
                    m.apply_edits(&[Edit::Wall {
                        id: wid,
                        spec: WallSpec {
                            a,
                            b,
                            level: lv,
                            base_offset,
                            height,
                            wall_type,
                            offset0,
                        },
                        bulge,
                    }])
                })
            }
        };
        if let Err(e) = r {
            // a refused edit leaves the model and the source file as they were
            c.log.push(format!("   skipped: {e}"));
            refused_changed_nothing(&c, &ed, &src, snap);
        }
    }
    c.log.push("save".into());

    // expectations from the edited model
    let alive: Vec<(String, String, bool)> = all_guids
        .iter()
        .map(|(k, gd)| {
            (
                k.clone(),
                gd.clone(),
                ed.model.element_by_guid(&key, gd).unwrap().is_some(),
            )
        })
        .collect();
    let want_quads = quads(&ed.model);
    let want_counts = ["wall", "opening", "slab"].map(|t| count(&ed.model, t));
    let mut want_by_guid = vec![];
    for (k, gd, ok) in &alive {
        if k == "wall" && *ok {
            let (_, id) = ed.model.element_by_guid(&key, gd).unwrap().unwrap();
            want_by_guid.push((gd.clone(), wall_quad(&ed.model, id), prop_text(&ed.model, id)));
        }
    }

    let out = dir.join("a-edited.ifc");
    let patched = patch_file(&ed.model, &key, &src, &out, None);
    // the source never changes, whatever the result
    let after = fs::read(&src).unwrap();
    if after != before || hash_bytes(&after) != before_hash {
        c.fail("the source file changed");
    }
    let p = match patched {
        Ok(p) => p,
        // only a changed level row is a known reason to refuse the whole save
        Err(PatchError::Doubt { key: k, detail })
            if level_used
                && k == DOUBT_TYPES
                && matches!(ed.model.shared_rows_changed(&key), Ok(Some(true))) =>
        {
            let _ = fs::remove_dir_all(&dir);
            return Err(Refused(format!("{k}: {detail}")));
        }
        Err(e) => c.fail(&format!("patch failed: {e}")),
    };
    if !p.will_not.is_empty() {
        c.fail(&format!("edits the writer will not make: {:?}", p.will_not));
    }
    if let Some(e) = &p.expect {
        if let Err(why) = check_patched(&out, e, None) {
            c.fail(&format!("read-back check: {why}"));
        }
    }
    let (src_text, out_text) = (fixture(schema), fs::read_to_string(&out).unwrap());
    if let Some(why) = refs_problem(&out_text) {
        c.fail(&why);
    }

    // statements nobody edited stay byte for byte
    let have: HashSet<String> = statements(&out_text).into_iter().collect();
    let mut promised: Vec<(usize, Option<usize>)> = vec![]; // (block base, line exempt)
    for (id, i) in &index {
        if !geo.contains(id) {
            promised.push((base(*i), propped.contains(id).then(|| base(*i) + 26)));
        }
    }
    for st in statements(&src_text) {
        let Some(id) = id_of(&st) else { continue };
        let in_block = id >= 200;
        let keep = if in_block {
            promised
                .iter()
                .any(|(b, ex)| id >= *b && id < b + 50 && Some(id) != *ex)
        } else {
            // the relations over the walls are checked below by what they point at
            !matches!(id, 56 | 57 | 112)
        };
        if keep && !have.contains(&st) {
            c.fail(&format!("untouched statement changed or lost: {st}"));
        }
    }
    // the relations over all walls still point at every surviving source wall
    let out_stmts = statements(&out_text);
    // #57 holds the walls of L1 only: a wall moved to another level is contained there instead
    let level_of = |id: i64| -> Option<i64> {
        ed.model
            .sql()
            .query_row("SELECT level FROM wall WHERE id=?1", [id], |r| r.get(0))
            .ok()
    };
    let l1: Option<i64> = ed
        .model
        .sql()
        .query_row("SELECT id FROM level WHERE name='L1'", [], |r| r.get(0))
        .ok();
    let mut wall_ids = vec![];
    for (k, gd, ok) in &alive {
        if k != "wall" || !*ok {
            continue;
        }
        let quoted = format!("'{gd}'");
        let line = out_stmts
            .iter()
            .find(|s| s.contains(&quoted) && s.contains("IFCWALL"))
            .unwrap_or_else(|| c.fail(&format!("wall {gd} has no statement in the output")));
        let (_, mid) = ed.model.element_by_guid(&key, gd).unwrap().unwrap();
        let on_l1 = l1.is_none() || level_of(mid) == l1;
        wall_ids.push((
            id_of(line).unwrap_or_else(|| c.fail(&format!("no id: {line}"))),
            on_l1,
        ));
    }
    for rel in [56usize, 57, 112] {
        match out_stmts.iter().find(|s| id_of(s) == Some(rel)) {
            Some(line) => {
                let refs = refs_in(line);
                for (w, on_l1) in &wall_ids {
                    if rel == 57 && !*on_l1 {
                        let held = out_stmts.iter().any(|s| {
                            s.contains("IFCRELCONTAINEDINSPATIALSTRUCTURE") && refs_in(s).contains(w)
                        });
                        if refs.contains(w) || !held {
                            c.fail(&format!("wall #{w} moved off L1 is not contained in its level"));
                        }
                    } else if !refs.contains(w) {
                        c.fail(&format!("relation #{rel} lost wall #{w}: {line}"));
                    }
                }
            }
            None if wall_ids.is_empty() => {}
            None => c.fail(&format!("relation #{rel} is gone while walls remain")),
        }
    }
    for needle in [
        "/* five walls along +Y, 4000 long, 3000 apart */",
        "#900=IFCANNOTATION(",
        "'over two lines'",
    ] {
        if !out_text.contains(needle) {
            c.fail(&format!("lost {needle}"));
        }
    }

    // reopen
    let back_file = dir.join("back.ifc");
    fs::copy(&out, &back_file).unwrap();
    let mut back = Model::open(dir.join("back.ddc").to_str().unwrap()).unwrap();
    import_ifc_file(&mut back, &back_file).unwrap_or_else(|e| c.fail(&format!("reopen: {e}")));
    let bkey = back.sources().unwrap().remove(0).source;

    for (k, gd, ok) in &alive {
        let found = back.element_by_guid(&bkey, gd).unwrap().is_some();
        if found != *ok {
            c.fail(&format!(
                "{k} {gd}: expected {} after reopen",
                if *ok { "present" } else { "gone" }
            ));
        }
    }
    let got_counts = ["wall", "opening", "slab"].map(|t| count(&back, t));
    if got_counts != want_counts {
        c.fail(&format!("counts wall/opening/slab {got_counts:?}, want {want_counts:?}"));
    }
    for (gd, q, prop) in &want_by_guid {
        let (_, id) = back.element_by_guid(&bkey, gd).unwrap().unwrap();
        let got = wall_quad(&back, id);
        if !near(&got, q) {
            c.fail(&format!("wall {gd}: {got:?}, want {q:?}"));
        }
        if &prop_text(&back, id) != prop {
            c.fail(&format!(
                "wall {gd}: {PROP} {:?}, want {prop:?}",
                prop_text(&back, id)
            ));
        }
    }
    let mut got = quads(&back);
    for w in &want_quads {
        match got.iter().position(|g| near(g, w)) {
            Some(i) => {
                got.swap_remove(i);
            }
            None => c.fail(&format!("wall {w:?} is not in the reopened file")),
        }
    }
    if !got.is_empty() {
        c.fail(&format!("extra walls after reopen: {got:?}"));
    }

    // the external validator: the save adds no problem the source did not have
    if let (Some(a), Some(b)) = (python_problems(&src), python_problems(&out)) {
        let new: Vec<_> = b.difference(&a).collect();
        if !new.is_empty() {
            c.fail(&format!("validate_ifc.py: new problems {new:?}"));
        }
    }

    let _ = fs::remove_dir_all(&dir);
    Ok(level_used)
}

/// Runs the seeds; returns (saves refused for a changed level, runs).
fn run_all(seeds: std::ops::RangeInclusive<u64>) -> (usize, usize) {
    let (mut excused, mut total) = (0, 0);
    for seed in seeds {
        for schema in SCHEMAS {
            total += 1;
            if let Err(Refused(why)) = run_seed(seed, schema, true) {
                excused += 1;
                eprintln!("seed {seed} {schema}: level change refused ({why}); rerun without it");
                if let Err(Refused(w)) = run_seed(seed, schema, false) {
                    panic!("seed {seed} {schema}: refused without level edits: {w}");
                }
            }
        }
    }
    eprintln!("excuse rate: {excused} of {total} runs refused for a changed level");
    (excused, total)
}

#[test]
fn three_seeds_round_trip() {
    run_all(1..=3);
}

#[test]
fn refused_op_changes_nothing() {
    let dir = std::env::temp_dir().join(format!("ddc-tst12-refused-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let src = dir.join("a.ifc");
    fs::write(&src, fixture("IFC4")).unwrap();
    let mut ed = Editor::new_in_memory().unwrap();
    ed.model = Model::open(dir.join("w.ddc").to_str().unwrap()).unwrap();
    {
        let p = src.clone();
        run(&mut ed, move |m| {
            import_ifc_file(m, &p)
                .map(|_| ())
                .map_err(|e| ddc_model::Error::Invalid(e.to_string()))
        })
        .unwrap();
    }
    let wid = ids(&ed.model, "wall")[0];
    let (model0, file0) = (fingerprint(&ed.model), hash_bytes(&fs::read(&src).unwrap()));
    // an op refused before it writes
    let r = run(&mut ed, |_| Err(ddc_model::Error::Invalid("refused".into())));
    assert!(r.is_err(), "the failing op must be refused");
    assert_eq!(fingerprint(&ed.model), model0, "model changed by a refused op");
    // an opening the model itself may reject
    let r = run(&mut ed, move |m| {
        m.add_opening(
            wid,
            &OpeningSpec {
                along: 1.0,
                width: -1.0,
                sill: 0.0,
                height: -2.0,
                kind: "door".into(),
            },
        )
        .map(|_| ())
    });
    if r.is_err() {
        assert_eq!(fingerprint(&ed.model), model0, "model changed by a refused opening");
    }
    assert_eq!(hash_bytes(&fs::read(&src).unwrap()), file0, "source file changed");
    let _ = fs::remove_dir_all(&dir);
}

/// A closure that writes and then fails: run_op rolls back what it wrote.
#[test]
fn op_failing_after_a_write_changes_nothing() {
    let dir = std::env::temp_dir().join(format!("ddc-tst12-partial-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let src = dir.join("a.ifc");
    fs::write(&src, fixture("IFC4")).unwrap();
    let mut ed = Editor::new_in_memory().unwrap();
    ed.model = Model::open(dir.join("w.ddc").to_str().unwrap()).unwrap();
    {
        let p = src.clone();
        run(&mut ed, move |m| {
            import_ifc_file(m, &p)
                .map(|_| ())
                .map_err(|e| ddc_model::Error::Invalid(e.to_string()))
        })
        .unwrap();
    }
    let wid = ids(&ed.model, "wall")[0];
    let model0 = fingerprint(&ed.model);
    let r = run(&mut ed, move |m| {
        m.move_elements(&[Target::Wall(wid)], (1.0, 1.0))?;
        Err(ddc_model::Error::Invalid("refused".into()))
    });
    assert!(r.is_err());
    assert_eq!(fingerprint(&ed.model), model0, "model changed by a refused op");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn refs_problem_ignores_quotes_in_comments() {
    let file = |body: &str| {
        format!("ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\n{body}\nENDSEC;\nEND-ISO-10303-21;\n")
    };
    // a quote and a reference inside a comment
    assert_eq!(refs_problem(&file("/* it's #999 */\n#1=IFCA(#2);\n#2=IFCB($);")), None);
    // an escaped quote inside a string
    assert_eq!(refs_problem(&file("#1=IFCA('x''#888',#2);\n#2=IFCB($);")), None);
    // a lone quote in a comment must not hide a real missing reference
    let p = refs_problem(&file("/* don't */\n#1=IFCA(#77);"));
    assert!(p.is_some_and(|p| p.contains("#77")), "missing #77 not found");
    // a comment marker inside a string is not a comment
    let p = refs_problem(&file("#1=IFCA('/*',#78);\n#2=IFCB($);"));
    assert!(p.is_some_and(|p| p.contains("#78")), "missing #78 not found");
}

#[test]
#[ignore = "long: 30 seeds"]
fn thirty_seeds_round_trip() {
    let (excused, total) = run_all(1..=30);
    assert!(
        excused * 5 < total,
        "{excused} of {total} runs were excused as refused level changes (limit 20%)"
    );
}
