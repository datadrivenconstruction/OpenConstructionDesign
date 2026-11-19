//! A new stair in the IFC patch: the stair, its flights and landings appended as products with one
//! aggregation, the stair alone contained in its storey, railings as their own elements, every
//! source line byte for byte. Nested parts still refuse. An edited stair or ramp keeps the lines of
//! the parts that did not change, rewrites the changed ones, and removes the parts it lost.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use ddc_geom::stair::Shape;
use ddc_io::ifc::patch::{
    base_rebase, check_patched, patch_file, PatchError, PatchOutput, DOUBT_NESTED, DOUBT_SHARED,
    REASON_SCHEMA,
};
use ddc_io::ifc::{export_ifc, import_ifc_file};
use ddc_model::{
    MepDomain, MepSection, MepShape, Model, RailingHost, RailingSpec, RampSpec, RunSpec,
    StairSpec, SystemKind,
};

const SCHEMAS: [&str; 2] = ["IFC4", "IFC4X3_ADD2"];
const LAST: u32 = 57;

/// Millimetres, one building with storeys L1 at 3000 and L2 at 6000, a wall on L1.
fn fixture(schema: &str) -> String {
    let wall = if schema == "IFC2X3" { "" } else { ",$" };
    format!(
        "ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [CoordinationView]'),'2;1');
FILE_NAME('f.ifc','2020-01-01T00:00:00',(''),(''),'','','');
FILE_SCHEMA(('{schema}'));
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
#19=IFCBUILDING('0000000000000000000019',#4,'B',$,$,#12,$,$,.ELEMENT.,$,$,$);
#25=IFCRELAGGREGATES('0000000000000000000025',#4,$,$,#3,(#19));
#13=IFCCARTESIANPOINT((0.,0.,3000.));
#14=IFCAXIS2PLACEMENT3D(#13,$,$);
#15=IFCLOCALPLACEMENT(#12,#14);
#16=IFCBUILDINGSTOREY('0000000000000000000002',#4,'L1',$,$,#15,$,$,.ELEMENT.,3000.);
#42=IFCCARTESIANPOINT((0.,0.,6000.));
#43=IFCAXIS2PLACEMENT3D(#42,$,$);
#44=IFCLOCALPLACEMENT(#12,#43);
#45=IFCBUILDINGSTOREY('0000000000000000000045',#4,'L2',$,$,#44,$,$,.ELEMENT.,6000.);
#26=IFCRELAGGREGATES('0000000000000000000026',#4,$,$,#19,(#16,#45));
#20=IFCCARTESIANPOINT((1000.,2000.,0.));
#21=IFCDIRECTION((0.,0.,1.));
#22=IFCDIRECTION((0.,1.,0.));
#23=IFCAXIS2PLACEMENT3D(#20,#21,#22);
#24=IFCLOCALPLACEMENT(#15,#23);
#30=IFCCARTESIANPOINT((0.,0.));
#31=IFCCARTESIANPOINT((4000.,0.));
#32=IFCPOLYLINE((#30,#31));
#33=IFCSHAPEREPRESENTATION(#18,'Axis','Curve2D',(#32));
#34=IFCCARTESIANPOINT((2000.,0.));
#35=IFCAXIS2PLACEMENT2D(#34,$);
#36=IFCRECTANGLEPROFILEDEF(.AREA.,$,#35,4000.,250.);
#37=IFCAXIS2PLACEMENT3D(#10,$,$);
#38=IFCEXTRUDEDAREASOLID(#36,#37,#21,2800.);
#39=IFCSHAPEREPRESENTATION(#17,'Body','SweptSolid',(#38));
#40=IFCPRODUCTDEFINITIONSHAPE($,$,(#33,#39));
#41=IFCWALLSTANDARDCASE('0000000000000000000003',#4,'W',$,$,#24,#40,${wall});
#50=IFCMATERIAL('Brick');
#52=IFCMATERIALLAYER(#50,250.,$);
#54=IFCMATERIALLAYERSET((#52),'Ext 250');
#55=IFCMATERIALLAYERSETUSAGE(#54,.AXIS2.,.POSITIVE.,-125.);
#56=IFCRELASSOCIATESMATERIAL('0000000000000000000004',#4,$,$,(#41),#55);
#57=IFCRELCONTAINEDINSPATIALSTRUCTURE('0000000000000000000005',#4,$,$,(#41),#16);
ENDSEC;
END-ISO-10303-21;
"
    )
}

struct Fx {
    dir: PathBuf,
    src: PathBuf,
    out: PathBuf,
    m: Model,
    key: String,
    text: String,
}

fn setup(tag: &str, schema: &str) -> Fx {
    let dir =
        std::env::temp_dir().join(format!("ddc-parts-{tag}-{schema}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let src = dir.join("a.ifc");
    let text = fixture(schema);
    fs::write(&src, &text).unwrap();
    let mut m = Model::open(dir.join("w.ddc").to_str().unwrap()).unwrap();
    import_ifc_file(&mut m, &src).unwrap();
    let key = m.sources().unwrap().remove(0).source;
    let out = dir.join("out.ifc");
    Fx {
        dir,
        src,
        out,
        m,
        key,
        text,
    }
}

impl Fx {
    fn patch(&self) -> Result<PatchOutput, PatchError> {
        patch_file(&self.m, &self.key, &self.src, &self.out, None)
    }

    fn done(&self) -> PatchOutput {
        let p = self.patch().unwrap();
        check_patched(&self.out, p.expect.as_ref().unwrap(), None).unwrap();
        p
    }

    fn output(&self) -> String {
        fs::read_to_string(&self.out).unwrap()
    }

    fn reread(&self) -> Model {
        let copy = self.dir.join("back.ifc");
        fs::copy(&self.out, &copy).unwrap();
        let mut m = Model::open(self.dir.join("back.ddc").to_str().unwrap()).unwrap();
        import_ifc_file(&mut m, &copy).unwrap();
        m
    }

    fn level(&self, name: &str) -> i64 {
        self.m
            .sql()
            .query_row("SELECT id FROM level WHERE name=?1", [name], |r| r.get(0))
            .unwrap()
    }

    /// An L-shaped stair: two flights and one landing.
    fn stair(&mut self, base: i64, top: i64) -> i64 {
        let mut s = StairSpec::straight(base, top, (20.0, 0.0), 0.0);
        s.shape = Shape::L {
            left: true,
            first: Some(8),
        };
        self.m.add_stair(&s).unwrap()
    }

    /// A ramp one metre high on a 20 m path: two flights and one landing.
    fn ramp(&mut self, level: i64) -> i64 {
        let mut s = RampSpec::new(level, None, vec![(20.0, 5.0), (40.0, 5.0)]);
        s.top_offset = 1.0;
        self.m.add_ramp(&s).unwrap()
    }

    /// What the save pipeline does when a patch takes the source's place; `edit` changes the
    /// written text before it becomes the source.
    fn over_source(&mut self, edit: impl Fn(&str) -> String) {
        let p = self.done();
        let r = base_rebase(&self.m, &self.key, &p).unwrap();
        let text = edit(&self.output());
        fs::write(&self.src, &text).unwrap();
        let hash = ddc_model::source::file::hash_file(&self.src, &|| false).unwrap();
        assert!(self.m.note_source_written(&self.key, &hash).unwrap());
        self.m.apply_source_rebase(&self.key, &r).unwrap();
        self.text = text;
    }
}

fn statements(t: &str) -> Vec<String> {
    t.split(';')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn lost(src: &str, out: &str) -> Vec<String> {
    let have: HashSet<String> = statements(out).into_iter().collect();
    statements(src)
        .into_iter()
        .filter(|s| !have.contains(s))
        .collect()
}

fn id_of(stmt: &str) -> Option<u32> {
    let s = stmt
        .rsplit('\n')
        .find(|l| l.starts_with('#'))
        .unwrap_or(stmt);
    let rest = s.strip_prefix('#')?;
    let n = rest.find('=')?;
    rest[..n].trim().parse().ok()
}

/// `#n` references after the `=` of a statement, outside strings.
fn refs_in(stmt: &str) -> Vec<u32> {
    let body = &stmt[stmt.find('=').map_or(0, |i| i + 1)..];
    let b = body.as_bytes();
    let (mut out, mut i, mut in_str) = (vec![], 0, false);
    while i < b.len() {
        match b[i] {
            b'\'' => in_str = !in_str,
            b'#' if !in_str => {
                let n = b[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
                out.push(body[i + 1..i + 1 + n].parse().unwrap());
                i += n;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

fn assert_refs_valid(t: &str) {
    let all = statements(t);
    let ids: HashSet<u32> = all.iter().filter_map(|s| id_of(s)).collect();
    for s in all.iter().filter(|s| id_of(s).is_some()) {
        for r in refs_in(s) {
            assert!(ids.contains(&r), "#{r} is not in the file");
        }
    }
}

fn new_of(src: &str, out: &str, ty: &str) -> Vec<u32> {
    let old: HashSet<u32> = statements(src).iter().filter_map(|s| id_of(s)).collect();
    statements(out)
        .iter()
        .filter(|s| s.contains(&format!("={ty}(")))
        .filter_map(|s| id_of(s))
        .filter(|i| !old.contains(i))
        .collect()
}

fn stmt_of(t: &str, id: u32) -> String {
    statements(t)
        .into_iter()
        .find(|s| id_of(s) == Some(id))
        .unwrap()
}

fn count(t: &str, ty: &str) -> usize {
    t.matches(&format!("={ty}(")).count()
}

fn landings(t: &str) -> usize {
    statements(t)
        .iter()
        .filter(|s| s.contains("=IFCSLAB(") && s.contains(".LANDING."))
        .count()
}

/// Statements of `ty` that reference `id`.
fn holding(t: &str, ty: &str, id: u32) -> Vec<String> {
    statements(t)
        .into_iter()
        .filter(|s| s.contains(&format!("={ty}(")) && refs_in(s).contains(&id))
        .collect()
}

fn joined(ids: &[u32]) -> String {
    ids.iter()
        .map(|i| format!("#{i}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn doubt_key(r: Result<PatchOutput, PatchError>) -> Option<&'static str> {
    match r {
        Err(PatchError::Doubt { key, .. }) => Some(key),
        _ => None,
    }
}

#[test]
fn new_stair_with_flights_patched_not_rewritten() {
    for s in SCHEMAS {
        let mut f = setup("add", s);
        let (l1, l2) = (f.level("L1"), f.level("L2"));
        let st = f.stair(l1, l2);
        f.m.add_stair_railings(st, &RailingSpec::new(l1, RailingHost::Path(vec![])))
            .unwrap();
        let lay = f.m.stair_layout(st).unwrap();
        let (nf, nl) = (lay.flights.len(), lay.landings.len());
        assert_eq!((nf, nl), (2, 1), "{s}: an L stair");
        let p = f.done();
        assert!(!p.no_changes, "{s}");
        assert_eq!(p.added as usize, 1 + nf + nl + 2, "{s}: stair, parts, railings");
        let out = f.output();
        assert_eq!(lost(&f.text, &out), Vec::<String>::new(), "{s}: nothing rewritten");

        let stair = new_of(&f.text, &out, "IFCSTAIR");
        assert_eq!(stair.len(), 1, "{s}");
        let stair = stair[0];
        assert!(stair > LAST, "{s}");
        let flights = new_of(&f.text, &out, "IFCSTAIRFLIGHT");
        assert_eq!(flights.len(), nf, "{s}");
        let slabs: Vec<u32> = new_of(&f.text, &out, "IFCSLAB")
            .into_iter()
            .filter(|i| stmt_of(&out, *i).contains(".LANDING."))
            .collect();
        assert_eq!(slabs.len(), nl, "{s}");

        // one aggregation from the stair to all its parts
        let mut parts: Vec<u32> = flights.iter().chain(&slabs).copied().collect();
        parts.sort_unstable();
        let rels = new_of(&f.text, &out, "IFCRELAGGREGATES");
        assert_eq!(rels.len(), 1, "{s}");
        let agg = stmt_of(&out, rels[0]);
        assert!(
            agg.ends_with(&format!("#{stair},({}))", joined(&parts))),
            "{s}: {agg}"
        );
        // the whole is contained in its storey, the parts are not
        let held = holding(&out, "IFCRELCONTAINEDINSPATIALSTRUCTURE", stair);
        assert_eq!(held.len(), 1, "{s}");
        assert!(held[0].ends_with(&format!("(#{stair}),#16)")), "{s}: {}", held[0]);
        for part in &parts {
            assert!(
                holding(&out, "IFCRELCONTAINEDINSPATIALSTRUCTURE", *part).is_empty(),
                "{s}: part #{part} contained"
            );
        }
        // railings are elements of their own, each contained
        let rails = new_of(&f.text, &out, "IFCRAILING");
        assert_eq!(rails.len(), 2, "{s}");
        for r in &rails {
            assert_eq!(
                holding(&out, "IFCRELCONTAINEDINSPATIALSTRUCTURE", *r).len(),
                1,
                "{s}: railing #{r}"
            );
            assert!(holding(&out, "IFCRELAGGREGATES", *r).is_empty(), "{s}");
        }
        // riser height in the file's millimetres
        let fl = stmt_of(&out, flights[0]);
        let args: Vec<&str> = fl[fl.find('(').unwrap() + 1..fl.len() - 1].split(',').collect();
        let riser: f64 = args[10].trim().parse().unwrap();
        assert!((riser - lay.riser * 1000.0).abs() < 1e-3, "{s}: {fl}");
        assert_refs_valid(&out);
        assert!(p.linked.iter().any(|l| l.0 == "stair"), "{s}");
        assert_eq!(p.linked.iter().filter(|l| l.0 == "railing").count(), 2, "{s}");
    }

    // a schema that cannot hold the stair's body keeps the file and says so
    let mut f = setup("x3", "IFC2X3");
    let (l1, l2) = (f.level("L1"), f.level("L2"));
    f.stair(l1, l2);
    let p = f.done();
    assert!(p
        .will_not
        .iter()
        .any(|w| w.0 == "stair" && w.1 == REASON_SCHEMA));
    let out = f.output();
    assert!(new_of(&f.text, &out, "IFCSTAIR").is_empty());
    assert!(new_of(&f.text, &out, "IFCSTAIRFLIGHT").is_empty());
}

#[test]
fn reopen_same_stair_parts_count() {
    for s in SCHEMAS {
        let mut f = setup("back", s);
        let (l1, l2) = (f.level("L1"), f.level("L2"));
        let st = f.stair(l1, l2);
        f.m.add_stair_railings(st, &RailingSpec::new(l1, RailingHost::Path(vec![])))
            .unwrap();
        let lay = f.m.stair_layout(st).unwrap();
        f.done();
        let out = f.output();
        let full = export_ifc(&f.m).unwrap();
        for ty in ["IFCSTAIR", "IFCSTAIRFLIGHT", "IFCRAILING", "IFCMEMBER"] {
            assert_eq!(count(&out, ty), count(&full, ty), "{s}: {ty} as in a full export");
        }
        assert_eq!(landings(&out), landings(&full), "{s}: landings");

        let back = f.reread();
        let ids: Vec<i64> = {
            let mut q = back.sql().prepare("SELECT id FROM stair").unwrap();
            let v = q
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            v
        };
        assert_eq!(ids.len(), 1, "{s}");
        let again = back.stair_layout(ids[0]).unwrap();
        assert_eq!(again.flights.len(), lay.flights.len(), "{s}");
        assert_eq!(again.landings.len(), lay.landings.len(), "{s}");
        let rails: i64 = back
            .sql()
            .query_row("SELECT count(*) FROM railing", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rails, 2, "{s}");
        let t = export_ifc(&back).unwrap();
        assert_eq!(count(&t, "IFCSTAIRFLIGHT"), count(&full, "IFCSTAIRFLIGHT"), "{s}");
        assert_eq!(landings(&t), landings(&full), "{s}");
    }
}

#[test]
fn untouched_lines_byte_identical() {
    for s in SCHEMAS {
        let mut f = setup("same", s);
        let (l1, l2) = (f.level("L1"), f.level("L2"));
        let st = f.stair(l1, l2);
        f.m.add_stair_railings(st, &RailingSpec::new(l1, RailingHost::Path(vec![])))
            .unwrap();
        f.done();
        let (src, out) = (f.text.clone(), f.output());
        assert_eq!(lost(&src, &out), Vec::<String>::new(), "{s}");
        let k = src.rfind("ENDSEC;").unwrap();
        assert!(out.starts_with(&src[..k]), "{s}: prefix changed");
        assert!(out.ends_with(&src[k..]), "{s}: tail changed");
        let appended = &out[k..out.len() - (src.len() - k)];
        for st in statements(appended) {
            assert!(id_of(&st).is_some_and(|i| i > LAST), "{s}: {st}");
        }
    }
}

#[test]
fn nested_parts_refused() {
    let mut f = setup("nest", "IFC4");
    let l1 = f.level("L1");
    let sys = f.m.add_mep_system("SA-1", SystemKind::SupplyAir).unwrap();
    let duct = MepSection::new(MepDomain::Duct, MepShape::Rect { w: 0.4, h: 0.2 });
    f.m.add_mep_run(&RunSpec::new(
        sys,
        l1,
        duct,
        vec![(0.0, 0.0, 5.7), (6.0, 0.0, 5.7)],
    ))
    .unwrap();
    assert_eq!(doubt_key(f.patch()), Some(DOUBT_NESTED));
    // the reason has its text in both languages
    let ui = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../ui/data/i18n");
    for lang in ["en", "ru"] {
        let t = fs::read_to_string(ui.join(lang).join("ifc_patch.json")).unwrap();
        assert!(t.contains(&format!("\"{DOUBT_NESTED}\"")), "{lang}");
    }
}

#[test]
fn stair_on_new_level() {
    for s in SCHEMAS {
        let mut f = setup("lvl", s);
        let l3 = f.m.add_level("L3", 9.0).unwrap();
        let l4 = f.m.add_level("L4", 12.0).unwrap();
        let st = f.stair(l3, l4);
        let lay = f.m.stair_layout(st).unwrap();
        let kids = lay.flights.len() + lay.landings.len();
        let p = f.done();
        assert_eq!(p.added as usize, 2 + 1 + kids, "{s}: storeys, stair, parts");
        let out = f.output();

        let storeys = new_of(&f.text, &out, "IFCBUILDINGSTOREY");
        assert_eq!(storeys.len(), 2, "{s}");
        let on = *storeys
            .iter()
            .find(|i| stmt_of(&out, **i).contains("'L3'"))
            .unwrap();
        let stair = new_of(&f.text, &out, "IFCSTAIR");
        assert_eq!(stair.len(), 1, "{s}");
        let held = holding(&out, "IFCRELCONTAINEDINSPATIALSTRUCTURE", stair[0]);
        assert_eq!(held.len(), 1, "{s}");
        assert!(held[0].ends_with(&format!("(#{}),#{on})", stair[0])), "{s}: {}", held[0]);
        // the storeys extend the building's aggregation; the parts get one of their own
        assert_eq!(new_of(&f.text, &out, "IFCRELAGGREGATES").len(), 1, "{s}");
        let agg = holding(&out, "IFCRELAGGREGATES", stair[0]);
        assert_eq!(agg.len(), 1, "{s}");
        assert_eq!(refs_in(&agg[0]).len(), 1 + kids, "{s}: stair, parts");
        assert_refs_valid(&out);

        let back = f.reread();
        let (level, n): (String, i64) = back
            .sql()
            .query_row(
                "SELECT l.name, count(*) FROM stair s JOIN level l ON l.id=s.level",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((level.as_str(), n), ("L3", 1), "{s}");
    }
}

/// Ids of the `ty` statements, ascending.
fn ids_of(t: &str, ty: &str) -> Vec<u32> {
    let mut v: Vec<u32> = statements(t)
        .iter()
        .filter(|s| s.contains(&format!("={ty}(")))
        .filter_map(|s| id_of(s))
        .collect();
    v.sort_unstable();
    v
}

fn landing_ids(t: &str) -> Vec<u32> {
    let mut v: Vec<u32> = statements(t)
        .iter()
        .filter(|s| s.contains("=IFCSLAB(") && s.contains(".LANDING."))
        .filter_map(|s| id_of(s))
        .collect();
    v.sort_unstable();
    v
}

fn named_line(t: &str, ty: &str, name: &str) -> String {
    statements(t)
        .into_iter()
        .find(|s| s.contains(&format!("={ty}(")) && s.contains(&format!("'{name}'")))
        .unwrap()
}

/// Top-level arguments of a statement without nested lists.
fn args_of(stmt: &str) -> Vec<String> {
    stmt[stmt.find('(').unwrap() + 1..stmt.len() - 1]
        .split(',')
        .map(|a| a.trim().to_string())
        .collect()
}

/// Every statement the output lost is of one of `types`.
fn lost_only(src: &str, out: &str, types: &[&str], s: &str) {
    for t in lost(src, out) {
        assert!(
            types.iter().any(|k| t.contains(&format!("={k}("))),
            "{s}: {t}"
        );
    }
}

/// Text with `line` (given the id after the last one) put before the end of DATA.
fn with_line(t: &str, line: impl Fn(u32) -> String) -> String {
    let max = statements(t).iter().filter_map(|s| id_of(s)).max().unwrap();
    let k = t.rfind("ENDSEC;").unwrap();
    format!("{}{}\n{}", &t[..k], line(max + 1), &t[k..])
}

fn stair_id(m: &Model) -> i64 {
    m.sql()
        .query_row("SELECT id FROM stair", [], |r| r.get(0))
        .unwrap()
}

fn ramp_id(m: &Model) -> i64 {
    m.sql()
        .query_row("SELECT id FROM ramp", [], |r| r.get(0))
        .unwrap()
}

fn risers(l: &ddc_geom::stair::Stair) -> Vec<String> {
    l.flights.iter().map(|x| x.risers.to_string()).collect()
}

#[test]
fn edit_stair_riser_count_patches_parts() {
    for s in SCHEMAS {
        let mut f = setup("rise", s);
        let (l1, l2) = (f.level("L1"), f.level("L2"));
        let st = f.stair(l1, l2);
        f.over_source(str::to_string);
        let before = f.text.clone();
        let stair = ids_of(&before, "IFCSTAIR")[0];
        let was = f.m.stair_layout(st).unwrap();
        let mut sp = f.m.stair(st).unwrap();
        sp.max_riser = 0.15;
        f.m.update_stair(st, &sp).unwrap();
        let lay = f.m.stair_layout(st).unwrap();
        assert_ne!(risers(&was), risers(&lay), "{s}: the riser count changes");
        assert_eq!(lay.flights.len(), 2, "{s}");

        let p = f.done();
        assert_eq!(p.added, 0, "{s}: no product added");
        let out = f.output();
        // the parts keep their ids and the aggregation its line
        assert_eq!(ids_of(&out, "IFCSTAIRFLIGHT"), ids_of(&before, "IFCSTAIRFLIGHT"), "{s}");
        assert_eq!(landing_ids(&out), landing_ids(&before), "{s}");
        assert_eq!(
            holding(&out, "IFCRELAGGREGATES", stair),
            holding(&before, "IFCRELAGGREGATES", stair),
            "{s}"
        );
        for (i, fl) in lay.flights.iter().enumerate() {
            let line = named_line(&out, "IFCSTAIRFLIGHT", &format!("Flight {}", i + 1));
            let a = args_of(&line);
            assert_eq!(a[8], fl.risers.to_string(), "{s}: {line}");
            let riser: f64 = a[10].parse().unwrap();
            assert!((riser - lay.riser * 1000.0).abs() < 1e-3, "{s}: {line}");
        }
        lost_only(
            &before,
            &out,
            &["IFCSTAIR", "IFCSTAIRFLIGHT", "IFCSLAB", "IFCPROPERTYSINGLEVALUE"],
            s,
        );
        assert_refs_valid(&out);

        let back = f.reread();
        let id = stair_id(&back);
        assert!((back.stair(id).unwrap().max_riser - 0.15).abs() < 1e-9, "{s}");
        assert_eq!(risers(&back.stair_layout(id).unwrap()), risers(&lay), "{s}");
    }
}

#[test]
fn unchanged_part_lines_byte_identical() {
    for s in SCHEMAS {
        let mut f = setup("keep", s);
        let (l1, l2) = (f.level("L1"), f.level("L2"));
        let st = f.stair(l1, l2);
        f.over_source(str::to_string);
        let before = f.text.clone();
        let stair = ids_of(&before, "IFCSTAIR")[0];
        // the material is data only: no part's shape changes
        let mut sp = f.m.stair(st).unwrap();
        sp.material = "Steel".into();
        f.m.update_stair(st, &sp).unwrap();

        let p = f.done();
        assert_eq!(p.added, 0, "{s}");
        let out = f.output();
        let mut parts = ids_of(&before, "IFCSTAIRFLIGHT");
        parts.extend(landing_ids(&before));
        assert_eq!(parts.len(), 3, "{s}");
        for id in &parts {
            assert_eq!(stmt_of(&out, *id), stmt_of(&before, *id), "{s}: part #{id}");
            for r in refs_in(&stmt_of(&before, *id)) {
                assert_eq!(stmt_of(&out, r), stmt_of(&before, r), "{s}: #{r} of part #{id}");
            }
        }
        assert_eq!(
            holding(&out, "IFCRELAGGREGATES", stair),
            holding(&before, "IFCRELAGGREGATES", stair),
            "{s}"
        );
        // the whole and its one data value changed, nothing else
        let gone = lost(&before, &out);
        assert_eq!(gone.len(), 2, "{s}: {gone:?}");
        assert!(gone.iter().any(|t| id_of(t) == Some(stair)), "{s}: {gone:?}");
        assert!(gone.iter().any(|t| t.contains("'Material'")), "{s}: {gone:?}");
        assert!(out.contains("'Steel'"), "{s}");
        assert_refs_valid(&out);

        let back = f.reread();
        assert_eq!(back.stair(stair_id(&back)).unwrap().material, "Steel", "{s}");
    }
}

#[test]
fn removed_flight_dropped_from_rel() {
    for s in SCHEMAS {
        let mut f = setup("drop", s);
        let (l1, l2) = (f.level("L1"), f.level("L2"));
        let st = f.stair(l1, l2);
        f.over_source(str::to_string);
        let before = f.text.clone();
        let stair = ids_of(&before, "IFCSTAIR")[0];
        let first = id_of(&named_line(&before, "IFCSTAIRFLIGHT", "Flight 1")).unwrap();
        let second = id_of(&named_line(&before, "IFCSTAIRFLIGHT", "Flight 2")).unwrap();
        let landing = landing_ids(&before);
        assert_eq!(landing.len(), 1, "{s}");
        let mut sp = f.m.stair(st).unwrap();
        sp.shape = Shape::Straight;
        f.m.update_stair(st, &sp).unwrap();
        let lay = f.m.stair_layout(st).unwrap();
        assert_eq!((lay.flights.len(), lay.landings.len()), (1, 0), "{s}");

        let p = f.done();
        assert_eq!(p.added, 0, "{s}");
        let out = f.output();
        assert_eq!(ids_of(&out, "IFCSTAIRFLIGHT"), vec![first], "{s}: the first flight stays");
        assert!(landing_ids(&out).is_empty(), "{s}");
        let ids: HashSet<u32> = statements(&out).iter().filter_map(|t| id_of(t)).collect();
        for gone in [second, landing[0]] {
            assert!(!ids.contains(&gone), "{s}: #{gone} still there");
            // its body goes with it: nothing else used it
            let body = *refs_in(&stmt_of(&before, gone)).last().unwrap();
            assert!(!ids.contains(&body), "{s}: body #{body} of #{gone}");
        }
        // the one aggregation now lists the one flight
        let agg = holding(&out, "IFCRELAGGREGATES", stair);
        assert_eq!(agg.len(), 1, "{s}");
        assert!(agg[0].ends_with(&format!("#{stair},(#{first}))")), "{s}: {}", agg[0]);
        assert!(new_of(&before, &out, "IFCRELAGGREGATES").is_empty(), "{s}");
        assert!(stmt_of(&out, stair).contains(".STRAIGHT_RUN_STAIR."), "{s}");
        assert_refs_valid(&out);

        let back = f.reread();
        let again = back.stair_layout(stair_id(&back)).unwrap();
        assert_eq!((again.flights.len(), again.landings.len()), (1, 0), "{s}");
    }
}

#[test]
fn shared_part_refused() {
    // a flight that is also a part of the wall
    let mut f = setup("shared", "IFC4");
    let (l1, l2) = (f.level("L1"), f.level("L2"));
    let st = f.stair(l1, l2);
    f.over_source(|t| {
        let fl = ids_of(t, "IFCSTAIRFLIGHT")[0];
        with_line(t, |n| {
            format!("#{n}=IFCRELAGGREGATES('0000000000000000000099',$,$,$,#41,(#{fl}));")
        })
    });
    let mut sp = f.m.stair(st).unwrap();
    sp.max_riser = 0.15;
    f.m.update_stair(st, &sp).unwrap();
    assert_eq!(doubt_key(f.patch()), Some(DOUBT_SHARED));

    // a flight to be removed that another relation names
    let mut f = setup("named", "IFC4");
    let (l1, l2) = (f.level("L1"), f.level("L2"));
    let st = f.stair(l1, l2);
    f.over_source(|t| {
        let fl = id_of(&named_line(t, "IFCSTAIRFLIGHT", "Flight 2")).unwrap();
        with_line(t, |n| {
            format!("#{n}=IFCRELCONNECTSELEMENTS('0000000000000000000098',$,$,$,$,#{fl},#41);")
        })
    });
    let mut sp = f.m.stair(st).unwrap();
    sp.shape = Shape::Straight;
    f.m.update_stair(st, &sp).unwrap();
    assert_eq!(doubt_key(f.patch()), Some(DOUBT_SHARED));

    let ui = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../ui/data/i18n");
    for lang in ["en", "ru"] {
        let t = fs::read_to_string(ui.join(lang).join("ifc_patch.json")).unwrap();
        assert!(t.contains(&format!("\"{DOUBT_SHARED}\"")), "{lang}");
    }
}

#[test]
fn new_ramp_with_flights_patched() {
    for s in SCHEMAS {
        let mut f = setup("ramp", s);
        let l1 = f.level("L1");
        let rp = f.ramp(l1);
        let lay = f.m.ramp_layout(rp).unwrap();
        let (nf, nl) = (lay.flights.len(), lay.landings.len());
        assert_eq!((nf, nl), (2, 1), "{s}");
        let p = f.done();
        assert_eq!(p.added as usize, 1 + nf + nl, "{s}: ramp and parts");
        let out = f.output();
        assert_eq!(lost(&f.text, &out), Vec::<String>::new(), "{s}: nothing rewritten");

        let ramp = new_of(&f.text, &out, "IFCRAMP");
        assert_eq!(ramp.len(), 1, "{s}");
        let ramp = ramp[0];
        let flights = new_of(&f.text, &out, "IFCRAMPFLIGHT");
        assert_eq!(flights.len(), nf, "{s}");
        let slabs: Vec<u32> = new_of(&f.text, &out, "IFCSLAB")
            .into_iter()
            .filter(|i| stmt_of(&out, *i).contains(".LANDING."))
            .collect();
        assert_eq!(slabs.len(), nl, "{s}");
        let mut parts: Vec<u32> = flights.iter().chain(&slabs).copied().collect();
        parts.sort_unstable();
        let rels = new_of(&f.text, &out, "IFCRELAGGREGATES");
        assert_eq!(rels.len(), 1, "{s}");
        let agg = stmt_of(&out, rels[0]);
        assert!(
            agg.ends_with(&format!("#{ramp},({}))", joined(&parts))),
            "{s}: {agg}"
        );
        let held = holding(&out, "IFCRELCONTAINEDINSPATIALSTRUCTURE", ramp);
        assert_eq!(held.len(), 1, "{s}");
        assert!(held[0].ends_with(&format!("(#{ramp}),#16)")), "{s}: {}", held[0]);
        for part in &parts {
            assert!(
                holding(&out, "IFCRELCONTAINEDINSPATIALSTRUCTURE", *part).is_empty(),
                "{s}: part #{part} contained"
            );
        }
        assert_refs_valid(&out);
        assert!(p.linked.iter().any(|l| l.0 == "ramp"), "{s}");

        let back = f.reread();
        let again = back.ramp_layout(ramp_id(&back)).unwrap();
        assert_eq!(again.flights.len(), nf, "{s}");
    }
}

#[test]
fn edit_ramp_patched() {
    for s in SCHEMAS {
        let mut f = setup("ramp-edit", s);
        let l1 = f.level("L1");
        let rp = f.ramp(l1);
        f.over_source(str::to_string);
        let before = f.text.clone();
        let ramp = ids_of(&before, "IFCRAMP")[0];
        let flights = ids_of(&before, "IFCRAMPFLIGHT");
        assert_eq!(flights.len(), 2, "{s}");
        let mut sp = f.m.ramp(rp).unwrap();
        sp.width = 1.5;
        f.m.update_ramp(rp, &sp).unwrap();

        let p = f.done();
        assert_eq!(p.added, 0, "{s}");
        let out = f.output();
        assert_eq!(ids_of(&out, "IFCRAMPFLIGHT"), flights, "{s}: the flights keep their ids");
        for id in &flights {
            assert_ne!(stmt_of(&out, *id), stmt_of(&before, *id), "{s}: flight #{id} rewritten");
        }
        assert_eq!(
            holding(&out, "IFCRELAGGREGATES", ramp),
            holding(&before, "IFCRELAGGREGATES", ramp),
            "{s}"
        );
        lost_only(
            &before,
            &out,
            &["IFCRAMP", "IFCRAMPFLIGHT", "IFCSLAB", "IFCPROPERTYSINGLEVALUE"],
            s,
        );
        assert_refs_valid(&out);

        let back = f.reread();
        assert!((back.ramp(ramp_id(&back)).unwrap().width - 1.5).abs() < 1e-9, "{s}");
    }
}

/// Run with `--features test-hooks`.
#[cfg(feature = "test-hooks")]
#[test]
fn new_stair_part_failed_rel_is_retried() {
    use ddc_io::ifc::patch::set_links_blocked;
    let mut f = setup("part-retry", "IFC4");
    let (l1, l2) = (f.level("L1"), f.level("L2"));
    let st = f.stair(l1, l2);
    f.m.sql()
        .execute("UPDATE stair SET material='concrete' WHERE id=?1", [st])
        .unwrap();
    set_links_blocked(true);
    f.over_source(|t| t.to_string());
    set_links_blocked(false);
    // every failed link is kept under the stair, the parts' ones included
    let pend = f.m.pending_links(&f.key).unwrap();
    assert_eq!(pend.len(), 1, "{pend:?}");
    assert!(pend[0].rels.len() > 1, "the parts' links are kept too: {:?}", pend[0].rels);
    let parts_of = |t: &str| -> Vec<u32> {
        statements(t)
            .iter()
            .filter(|s| {
                s.contains("=IFCSTAIRFLIGHT(") || (s.contains("=IFCSLAB(") && s.contains(".LANDING."))
            })
            .filter_map(|s| id_of(s))
            .collect()
    };
    let parts = parts_of(&f.text);
    assert_eq!(parts.len(), 3);
    for id in &parts {
        assert!(holding(&f.text, "IFCRELASSOCIATESMATERIAL", *id).is_empty(), "#{id} blocked");
    }
    assert!(!f.m.pending_links(&f.key).unwrap().is_empty());

    // unblocked: the part links are written on the parts in the file
    f.over_source(|t| t.to_string());
    let out = f.text.clone();
    assert_eq!(count(&out, "IFCSTAIR"), 1, "nothing is appended twice");
    assert_eq!(parts_of(&out), parts);
    for id in &parts {
        assert_eq!(holding(&out, "IFCRELASSOCIATESMATERIAL", *id).len(), 1, "#{id}");
    }
    assert_refs_valid(&out);
    assert!(f.m.pending_links(&f.key).unwrap().is_empty());
}
