// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A new level in the IFC patch: appended as a storey of the file's one building, everything
//! else byte for byte, and a file whose building is not unambiguous refuses.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use ddc_io::ifc::import_ifc_file;
use ddc_io::ifc::patch::{
    check_patched, patch_file, PatchError, PatchOutput, DOUBT_BUILDING, DOUBT_EDIT,
};
use ddc_model::{GridSpec, LayerSpec, Model, WallSpec};

const SCHEMAS: [&str; 3] = ["IFC2X3", "IFC4", "IFC4X3_ADD2"];
const WALL: &str = "0000000000000000000003";
const STOREYS_REL: &str = "#26=IFCRELAGGREGATES('0000000000000000000026',#4,$,$,#19,(#16));";

/// Millimetres, one building with one storey L1 at 3000 aggregated by `storeys_rel`, a wall on it.
/// `extra` goes in before the end of DATA.
fn fixture(schema: &str, storeys_rel: &str, extra: &str) -> String {
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
{storeys_rel}
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
#41=IFCWALLSTANDARDCASE('{WALL}',#4,'W',$,$,#24,#40,${wall});
#50=IFCMATERIAL('Brick');
#52=IFCMATERIALLAYER(#50,250.,$);
#54=IFCMATERIALLAYERSET((#52),'Ext 250');
#55=IFCMATERIALLAYERSETUSAGE(#54,.AXIS2.,.POSITIVE.,-125.);
#56=IFCRELASSOCIATESMATERIAL('0000000000000000000004',#4,$,$,(#41),#55);
#57=IFCRELCONTAINEDINSPATIALSTRUCTURE('0000000000000000000005',#4,$,$,(#41),#16);
{extra}
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

fn setup(tag: &str, schema: &str, text: String) -> Fx {
    let dir = std::env::temp_dir().join(format!("ddc-lvl-{tag}-{schema}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let src = dir.join("a.ifc");
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
}

fn statements(t: &str) -> Vec<String> {
    t.split(';')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Statements of `src` missing from `out`, byte for byte.
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

/// Every `#n` outside strings has a statement.
fn assert_refs_valid(t: &str) {
    let ids: HashSet<u32> = statements(t).iter().filter_map(|s| id_of(s)).collect();
    let data = &t[t.find("DATA;").unwrap()..];
    let b = data.as_bytes();
    let (mut i, mut in_str) = (0, false);
    while i < b.len() {
        match b[i] {
            b'\'' => in_str = !in_str,
            b'#' if !in_str => {
                let n = b[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
                let id: u32 = data[i + 1..i + 1 + n].parse().unwrap();
                assert!(ids.contains(&id), "#{id} is not in the file");
            }
            _ => {}
        }
        i += 1;
    }
}

/// Ids of the output statements of `ty` that are not in the source.
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

fn doubt_key(r: Result<PatchOutput, PatchError>) -> Option<&'static str> {
    match r {
        Err(PatchError::Doubt { key, .. }) => Some(key),
        _ => None,
    }
}

fn plain(tag: &str, schema: &str) -> Fx {
    setup(tag, schema, fixture(schema, STOREYS_REL, ""))
}

#[test]
fn new_level_appended_as_storey() {
    for s in SCHEMAS {
        let mut f = plain("add", s);
        f.m.add_level("L2", 6.0).unwrap();
        let p = f.done();
        assert_eq!(p.added, 1, "{s}");
        let out = f.output();

        let storeys = new_of(&f.text, &out, "IFCBUILDINGSTOREY");
        assert_eq!(storeys.len(), 1, "{s}");
        let n = storeys[0];
        assert!(n > 57, "{s}: appended above the highest id");
        let line = stmt_of(&out, n);
        assert!(line.contains("'L2'"), "{s}: {line}");
        assert!(line.contains(".ELEMENT."), "{s}: {line}");
        if s == "IFC2X3" {
            assert!(line.contains(",#4,'L2'"), "{s}: owner history filled");
        }
        // placed relative to the building's placement
        let pl: Vec<u32> = new_of(&f.text, &out, "IFCLOCALPLACEMENT");
        assert_eq!(pl.len(), 1, "{s}");
        assert!(stmt_of(&out, pl[0]).contains("IFCLOCALPLACEMENT(#12,"), "{s}");
        assert!(line.contains(&format!(",#{},", pl[0])), "{s}: {line}");
        // the building's storey aggregation is extended
        assert!(
            stmt_of(&out, 26).ends_with(&format!("#19,(#16,#{n}))")),
            "{s}: {}",
            stmt_of(&out, 26)
        );
        assert!(new_of(&f.text, &out, "IFCRELAGGREGATES").is_empty(), "{s}");
        assert_refs_valid(&out);
        assert_eq!(p.linked.iter().filter(|l| l.0 == "level").count(), 1, "{s}");

        let back = f.reread();
        let (name, elev): (String, f64) = back
            .sql()
            .query_row(
                "SELECT name, elevation FROM level ORDER BY elevation DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(name, "L2", "{s}");
        assert!((elev - 6.0).abs() < 1e-6, "{s}: elevation {elev}");
        let levels: i64 = back
            .sql()
            .query_row("SELECT count(*) FROM level", [], |r| r.get(0))
            .unwrap();
        assert_eq!(levels, 2, "{s}");
    }
}

#[test]
fn element_on_new_level_contained() {
    for s in SCHEMAS {
        let mut f = plain("on", s);
        let level = f.m.add_level("L2", 6.0).unwrap();
        let t =
            f.m.add_wall_type(
                "new",
                &[LayerSpec {
                    material: "concrete".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        f.m.add_wall(&WallSpec {
            a: (10.0, 0.0),
            b: (14.0, 0.0),
            level,
            base_offset: 0.0,
            height: 2.5,
            wall_type: t,
            offset0: None,
        })
        .unwrap();
        let p = f.done();
        assert_eq!(p.added, 2, "{s}");
        let out = f.output();
        let storey = new_of(&f.text, &out, "IFCBUILDINGSTOREY");
        let wall = new_of(&f.text, &out, "IFCWALL");
        assert_eq!((storey.len(), wall.len()), (1, 1), "{s}");
        assert!(
            out.contains(&format!("(#{}),#{});", wall[0], storey[0])),
            "{s}: contained in the new storey"
        );
        assert!(!out.contains(&format!("(#{}),#16);", wall[0])), "{s}");
        assert_refs_valid(&out);

        let back = f.reread();
        let on: String = back
            .sql()
            .query_row(
                "SELECT l.name FROM wall w JOIN level l ON l.id=w.level \
                 WHERE abs(w.ax-10.0)<1e-6 AND abs(w.bx-14.0)<1e-6",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(on, "L2", "{s}");
    }
}

#[test]
fn untouched_lines_byte_identical() {
    for s in SCHEMAS {
        let mut f = plain("same", s);
        f.m.add_level("L2", 6.0).unwrap();
        f.done();
        let (src, out) = (f.text.clone(), f.output());
        let only = vec![STOREYS_REL.trim_end_matches(';').to_string()];
        assert_eq!(lost(&src, &out), only, "{s}");
        // the source up to the end of DATA, with the one extended line, then the appended lines
        let new26 = format!("{};", stmt_of(&out, 26));
        let swapped = src.replacen(STOREYS_REL, &new26, 1);
        let k = swapped.rfind("ENDSEC;").unwrap();
        assert!(out.starts_with(&swapped[..k]), "{s}: prefix changed");
        assert!(out.ends_with(&swapped[k..]), "{s}: tail changed");
        let appended = &out[k..out.len() - (swapped.len() - k)];
        for st in statements(appended) {
            assert!(id_of(&st).is_some_and(|i| i > 57), "{s}: {st}");
        }
    }
}

#[test]
fn shared_aggregates_rel_gets_new_rel() {
    let s = "IFC4";
    let shared = "#26=IFCRELAGGREGATES('0000000000000000000026',#4,$,$,#19,(#16,#29));";
    let part = "#29=IFCBUILDINGELEMENTPROXY('0000000000000000000029',#4,'part',$,$,$,$,$,$);";
    let mut f = setup("shared", s, fixture(s, shared, part));
    f.m.add_level("L2", 6.0).unwrap();
    f.done();
    let out = f.output();
    assert_eq!(lost(&f.text, &out), Vec::<String>::new(), "the shared relation stays");
    let storey = new_of(&f.text, &out, "IFCBUILDINGSTOREY");
    let rels = new_of(&f.text, &out, "IFCRELAGGREGATES");
    assert_eq!((storey.len(), rels.len()), (1, 1));
    assert!(
        stmt_of(&out, rels[0]).ends_with(&format!("#19,(#{}))", storey[0])),
        "{}",
        stmt_of(&out, rels[0])
    );
    assert_refs_valid(&out);
}

#[test]
fn two_buildings_refused() {
    let s = "IFC4";
    let text = fixture(s, STOREYS_REL, "").replace(
        "#25=IFCRELAGGREGATES('0000000000000000000025',#4,$,$,#3,(#19));",
        "#25=IFCRELAGGREGATES('0000000000000000000025',#4,$,$,#3,(#19,#60));\n\
         #60=IFCBUILDING('0000000000000000000060',#4,'B2',$,$,#12,$,$,.ELEMENT.,$,$,$);",
    );
    let mut f = setup("two", s, text);
    f.m.add_level("L2", 6.0).unwrap();
    assert_eq!(doubt_key(f.patch()), Some(DOUBT_BUILDING));
}

#[test]
fn grid_still_refused() {
    let mut f = plain("grid", "IFC4");
    f.m.add_grid(&GridSpec::line("A", (3.0, 0.0), (3.0, 10.0))).unwrap();
    assert_eq!(doubt_key(f.patch()), Some(DOUBT_EDIT));
}
