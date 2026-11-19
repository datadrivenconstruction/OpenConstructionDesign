// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Golden round trips of the IFC patch writer, on one fixture in IFC2X3, IFC4 and IFC4X3:
//! what no edit touches stays byte for byte, the result reads back as the expected model, every
//! reference resolves, and whatever cannot be trusted refuses instead of writing half a result.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use ddc_io::ifc::patch::{
    base_rebase, check_patched, has_changes, patch_file, PatchError, PatchOutput, DOUBT_EDIT,
    DOUBT_SOURCE, DOUBT_TYPES, DOUBT_ZIP, REASON_PROP,
};
use ddc_io::ifc::{export_ifc, import_ifc_file, plan_file};
use ddc_io::job::Progress;
use ddc_model::source::SqlValue;
use ddc_model::{CompSpec, LayerSpec, Model, OpeningSpec, Target, WallSpec};

const SCHEMAS: [&str; 3] = ["IFC2X3", "IFC4", "IFC4X3_ADD2"];

const WALL: &str = "0000000000000000000003";
const OPENING: &str = "0000000000000000000006";
const DOOR: &str = "0000000000000000000008";
const SLAB: &str = "0000000000000000000010";
const SKIPPED: &str = "#900=IFCANNOTATION('0000000000000000000900',$,'kept',$,$,$,$);";
const MULTILINE: &str =
    "#901=IFCPROPERTYSINGLEVALUE('Spread',\n   'over two lines',IFCLABEL('x'),$);";

/// Millimetres, a wall with a door in it, a slab, one own and one shared property set, a class the
/// importer skips, a comment and a statement over two lines.
fn fixture(schema: &str) -> String {
    // attributes added to these classes after IFC2X3
    let (wall, door) = if schema == "IFC2X3" {
        ("", "")
    } else {
        (",$", ",$,$,$")
    };
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
#13=IFCCARTESIANPOINT((0.,0.,3000.));
#14=IFCAXIS2PLACEMENT3D(#13,$,$);
#15=IFCLOCALPLACEMENT(#12,#14);
#16=IFCBUILDINGSTOREY('0000000000000000000002',#4,'L1',$,$,#15,$,$,.ELEMENT.,3000.);
/* wall along +Y from (1000,2000), 4000 long */
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
#57=IFCRELCONTAINEDINSPATIALSTRUCTURE('0000000000000000000005',#4,$,$,(#41,#70,#80),#16);
#60=IFCCARTESIANPOINT((1000.,200.,0.));
#61=IFCAXIS2PLACEMENT3D(#60,$,$);
#62=IFCLOCALPLACEMENT(#24,#61);
#63=IFCCARTESIANPOINT((450.,1050.));
#64=IFCAXIS2PLACEMENT2D(#63,$);
#65=IFCRECTANGLEPROFILEDEF(.AREA.,$,#64,900.,2100.);
#66=IFCDIRECTION((0.,-1.,0.));
#67=IFCDIRECTION((1.,0.,0.));
#68=IFCAXIS2PLACEMENT3D(#10,#66,#67);
#69=IFCEXTRUDEDAREASOLID(#65,#68,#21,400.);
#71=IFCSHAPEREPRESENTATION(#17,'Body','SweptSolid',(#69));
#72=IFCPRODUCTDEFINITIONSHAPE($,$,(#71));
#73=IFCOPENINGELEMENT('{OPENING}',#4,'O',$,$,#62,#72,${wall});
#74=IFCRELVOIDSELEMENT('0000000000000000000007',#4,$,$,#41,#73);
#70=IFCDOOR('{DOOR}',#4,'D',$,$,#62,$,$,2100.,900.{door});
#75=IFCRELFILLSELEMENT('0000000000000000000009',#4,$,$,#73,#70);
#81=IFCRECTANGLEPROFILEDEF(.AREA.,$,#35,4000.,2000.);
#82=IFCEXTRUDEDAREASOLID(#81,#37,#21,200.);
#83=IFCSHAPEREPRESENTATION(#17,'Body','SweptSolid',(#82));
#84=IFCPRODUCTDEFINITIONSHAPE($,$,(#83));
#80=IFCSLAB('{SLAB}',#4,'S',$,$,#15,#84,$,.FLOOR.);
#100=IFCPROPERTYSINGLEVALUE('Fire',$,IFCLABEL('EI 30'),$);
#101=IFCPROPERTYSINGLEVALUE('Thick',$,IFCLENGTHMEASURE(250.),$);
#102=IFCPROPERTYSET('0000000000000000000100',#4,'Own',$,(#100,#101));
#103=IFCRELDEFINESBYPROPERTIES('0000000000000000000101',#4,$,$,(#41),#102);
#110=IFCPROPERTYSINGLEVALUE('Grade',$,IFCLABEL('A'),$);
#111=IFCPROPERTYSET('0000000000000000000110',#4,'Common',$,(#110));
#112=IFCRELDEFINESBYPROPERTIES('0000000000000000000111',#4,$,$,(#41,#80),#111);
{SKIPPED}
{MULTILINE}
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
}

fn setup(tag: &str, schema: &str) -> Fx {
    setup_text(tag, schema, &fixture(schema))
}

/// The fixture plus a quantity set of the wall alone and one the wall shares with the slab.
fn with_quantities(schema: &str) -> String {
    fixture(schema).replace(
        SKIPPED,
        &format!(
            "#120=IFCQUANTITYAREA('Area',$,$,10.);
#121=IFCELEMENTQUANTITY('0000000000000000000120',#4,'QtoOwn',$,$,(#120));
#122=IFCRELDEFINESBYPROPERTIES('0000000000000000000121',#4,$,$,(#41),#121);
#123=IFCQUANTITYLENGTH('Length',$,$,4000.);
#124=IFCELEMENTQUANTITY('0000000000000000000124',#4,'QtoShared',$,$,(#123));
#125=IFCRELDEFINESBYPROPERTIES('0000000000000000000125',#4,$,$,(#41,#80),#124);
{SKIPPED}"
        ),
    )
}

fn setup_text(tag: &str, schema: &str, text: &str) -> Fx {
    let dir = std::env::temp_dir().join(format!("ddc-o6-{tag}-{schema}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let src = dir.join("a.ifc");
    fs::write(&src, text).unwrap();
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
    }
}

impl Fx {
    fn row(&self, guid: &str) -> (String, i64) {
        self.m.element_by_guid(&self.key, guid).unwrap().unwrap()
    }

    fn patch(&self) -> Result<PatchOutput, PatchError> {
        patch_file(&self.m, &self.key, &self.src, &self.out, None)
    }

    fn done(&self) -> PatchOutput {
        let p = self.patch().unwrap();
        if let Some(e) = &p.expect {
            check_patched(&self.out, e, None).unwrap();
        }
        p
    }

    fn output(&self) -> String {
        fs::read_to_string(&self.out).unwrap()
    }

    /// The output as a fresh model.
    fn reread(&self) -> (Model, String) {
        let copy = self.dir.join("back.ifc");
        fs::copy(&self.out, &copy).unwrap();
        let mut m = Model::open(self.dir.join("back.ddc").to_str().unwrap()).unwrap();
        import_ifc_file(&mut m, &copy).unwrap();
        let key = m.sources().unwrap().remove(0).source;
        (m, key)
    }
}

/// Statements of a STEP text, each with the bytes it was written in.
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
    assert_eq!(
        ids.len(),
        statements(t).iter().filter_map(|s| id_of(s)).count(),
        "repeated id"
    );
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

fn wall_xy(m: &Model, id: i64) -> (f64, f64, f64, f64) {
    m.sql()
        .query_row("SELECT ax,ay,bx,by FROM wall WHERE id=?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .unwrap()
}

fn close(a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)) -> bool {
    [a.0 - b.0, a.1 - b.1, a.2 - b.2, a.3 - b.3]
        .iter()
        .all(|d| d.abs() < 1e-6)
}

#[test]
fn zero_edits_are_byte_identical() {
    for s in SCHEMAS {
        let f = setup("zero", s);
        let p = f.done();
        assert!(p.no_changes, "{s}");
        assert_eq!(fs::read(&f.out).unwrap(), fs::read(&f.src).unwrap(), "{s}");
    }
}

#[test]
fn one_property_edit_changes_only_its_line() {
    for s in SCHEMAS {
        let mut f = setup("prop", s);
        let (kind, id) = f.row(WALL);
        f.m.set_source_prop(&kind, id, "Own.Fire", SqlValue::Text("EI 90".into()), "")
            .unwrap();
        f.done();
        let (src, out) = (fixture(s), f.output());
        assert_eq!(
            lost(&src, &out),
            ["#100=IFCPROPERTYSINGLEVALUE('Fire',$,IFCLABEL('EI 30'),$)"],
            "{s}"
        );
        assert!(
            out.contains("#100=IFCPROPERTYSINGLEVALUE('Fire',$,IFCLABEL('EI 90'),$);"),
            "{s}"
        );
        assert_eq!(
            src.len(),
            out.len(),
            "{s}: same length, same bytes elsewhere"
        );
        assert_refs_valid(&out);
    }
}

#[test]
fn a_length_property_is_written_in_the_units_of_the_file() {
    let mut f = setup("len", "IFC4");
    let (kind, id) = f.row(WALL);
    f.m.set_source_prop(&kind, id, "Own.Thick", SqlValue::Real(0.3), "m")
        .unwrap();
    f.done();
    assert!(f
        .output()
        .contains("#101=IFCPROPERTYSINGLEVALUE('Thick',$,IFCLENGTHMEASURE(300.),$);"));
}

#[test]
fn editing_a_shared_set_leaves_the_other_element_alone() {
    for s in SCHEMAS {
        let mut f = setup("shared", s);
        let (kind, id) = f.row(WALL);
        f.m.set_source_prop(&kind, id, "Common.Grade", SqlValue::Text("B".into()), "")
            .unwrap();
        f.done();
        let (src, out) = (fixture(s), f.output());
        assert!(
            out.contains("#110=IFCPROPERTYSINGLEVALUE('Grade',$,IFCLABEL('A'),$);"),
            "{s}"
        );
        assert!(
            out.contains("#111=IFCPROPERTYSET('0000000000000000000110',#4,'Common',$,(#110));"),
            "{s}"
        );
        assert!(
            out.contains(",(#80),#111);"),
            "{s}: the slab keeps the old set"
        );
        let gone = lost(&src, &out);
        assert_eq!(gone.len(), 1, "{s}: {gone:?}");
        assert!(gone[0].starts_with("#112="), "{s}");
        assert_refs_valid(&out);

        let (back, key) = f.reread();
        let value = |guid: &str| -> String {
            let (k, i) = back.element_by_guid(&key, guid).unwrap().unwrap();
            back.sql()
                .query_row(
                    "SELECT value FROM prop WHERE kind=?1 AND ref=?2 AND key='Common.Grade'",
                    rusqlite::params![k, i],
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert_eq!(value(WALL), "B", "{s}");
        assert_eq!(value(SLAB), "A", "{s}");
    }
}

#[test]
fn a_move_replaces_only_the_host_and_what_sits_in_it() {
    for s in SCHEMAS {
        let mut f = setup("move", s);
        let (_, id) = f.row(WALL);
        let before = wall_xy(&f.m, id);
        f.m.move_elements(&[Target::Wall(id)], (1.0, 0.5)).unwrap();
        let moved = wall_xy(&f.m, id);
        assert!(
            close(
                moved,
                (
                    before.0 + 1.0,
                    before.1 + 0.5,
                    before.2 + 1.0,
                    before.3 + 0.5
                )
            ),
            "{s}"
        );
        let p = f.done();
        assert!(p.edited >= 1, "{s}");
        assert!(p.will_not.is_empty(), "{s}: {:?}", p.will_not);

        let (src, out) = (fixture(s), f.output());
        let gone: HashSet<u32> = lost(&src, &out).iter().filter_map(|x| id_of(x)).collect();
        assert_eq!(gone, HashSet::from([41, 73, 70]), "{s}");
        assert!(out.contains(SKIPPED), "{s}");
        assert!(out.contains(MULTILINE), "{s}");
        assert!(
            out.contains("/* wall along +Y from (1000,2000), 4000 long */"),
            "{s}"
        );
        assert_refs_valid(&out);

        let (back, key) = f.reread();
        let (_, wall) = back.element_by_guid(&key, WALL).unwrap().unwrap();
        assert!(close(wall_xy(&back, wall), moved), "{s}");
        assert!(
            back.element_by_guid(&key, OPENING).unwrap().is_some(),
            "{s}"
        );
    }
}

#[test]
fn deleting_a_host_takes_its_dependents_and_nothing_else() {
    for s in SCHEMAS {
        let mut f = setup("delete", s);
        let (_, id) = f.row(WALL);
        f.m.delete_wall(id).unwrap();
        f.done();
        let (src, out) = (fixture(s), f.output());
        for gone in [
            WALL,
            OPENING,
            DOOR,
            "0000000000000000000004",
            "0000000000000000000007",
            "0000000000000000000009",
        ] {
            assert!(!out.contains(gone), "{s}: {gone} is still in the file");
        }
        assert!(
            out.contains("(#80),#16);"),
            "{s}: containment keeps the slab"
        );
        assert!(
            out.contains("(#80),#111);"),
            "{s}: the shared set keeps the slab"
        );
        assert!(out.contains(SKIPPED), "{s}");
        let gone: HashSet<u32> = lost(&src, &out).iter().filter_map(|x| id_of(x)).collect();
        assert_eq!(
            gone,
            HashSet::from([41, 56, 57, 73, 74, 70, 75, 103, 112]),
            "{s}"
        );
        assert_refs_valid(&out);

        let c = Progress::new();
        let plan = plan_file(&f.out, &c).unwrap();
        assert!(plan.walls.is_empty(), "{s}");
        assert!(plan.elems.iter().any(|e| e.guid == SLAB), "{s}");
    }
}

#[test]
fn a_new_wall_is_appended_above_the_highest_id_and_nothing_is_lost() {
    for s in SCHEMAS {
        let mut f = setup("add", s);
        let level: i64 =
            f.m.sql()
                .query_row("SELECT id FROM level", [], |r| r.get(0))
                .unwrap();
        let t =
            f.m.add_wall_type(
                "new",
                &[LayerSpec {
                    material: "concrete".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let w =
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
        assert_eq!(p.added, 1, "{s}");

        let (src, out) = (fixture(s), f.output());
        assert_eq!(lost(&src, &out), Vec::<String>::new(), "{s}");
        let added: Vec<u32> = statements(&out)
            .iter()
            .filter(|x| x.contains("=IFCWALL("))
            .filter_map(|x| id_of(x))
            .collect();
        assert_eq!(added.len(), 1, "{s}");
        assert!(added[0] > 901, "{s}");
        assert!(
            out.contains(&format!("(#{}),#16);", added[0])),
            "{s}: contained in the storey"
        );
        assert_refs_valid(&out);

        let (back, _) = f.reread();
        let n: i64 = back
            .sql()
            .query_row("SELECT count(*) FROM wall", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2, "{s}");
        let found: i64 = back
            .sql()
            .query_row(
                "SELECT count(*) FROM wall WHERE abs(ax-?1)<1e-6 AND abs(ay-?2)<1e-6 AND abs(bx-?3)<1e-6 AND abs(by-?4)<1e-6",
                rusqlite::params![10.0, 0.0, 14.0, 0.0],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(found, 1, "{s}: at {:?}", wall_xy(&f.m, w));
    }
}

#[test]
fn levels_types_and_materials_changed_since_the_import_refuse_the_patch() {
    let edits = [
        "UPDATE wall_type_layer SET thickness=thickness+0.1",
        "UPDATE wall_type SET name='renamed'",
        "UPDATE level SET elevation=elevation+1",
    ];
    for (i, sql) in edits.iter().enumerate() {
        let f = setup(&format!("types{i}"), "IFC4");
        assert_eq!(has_changes(&f.m, &f.key, &f.src), Ok(false), "{sql}");
        assert!(f.m.sql().execute(sql, []).unwrap() > 0, "{sql}");
        let doubt = |r: Result<bool, PatchError>| {
            matches!(
                r,
                Err(PatchError::Doubt {
                    key: DOUBT_TYPES,
                    ..
                })
            )
        };
        assert!(doubt(has_changes(&f.m, &f.key, &f.src)), "{sql}");
        assert!(
            matches!(
                f.patch(),
                Err(PatchError::Doubt {
                    key: DOUBT_TYPES,
                    ..
                })
            ),
            "{sql}"
        );
    }
}

#[test]
fn a_material_changed_since_the_import_refuses_the_patch() {
    let f = setup("material", "IFC4");
    let n = f.m.sql().execute("UPDATE material SET note='edited'", []);
    if n.is_ok_and(|n| n > 0) {
        assert!(matches!(
            f.patch(),
            Err(PatchError::Doubt {
                key: DOUBT_TYPES,
                ..
            })
        ));
    }
}

#[test]
fn a_move_takes_the_quantities_off_the_moved_element() {
    for s in SCHEMAS {
        let mut f = setup_text("qmove", s, &with_quantities(s));
        let (_, id) = f.row(WALL);
        f.m.move_elements(&[Target::Wall(id)], (1.0, 0.0)).unwrap();
        let p = f.done();
        assert!(
            p.stats.warnings.iter().any(|w| w.contains("quantity")),
            "{s}"
        );
        let out = f.output();
        for gone in ["#120=", "#121=", "#122=", "QtoOwn"] {
            assert!(!out.contains(gone), "{s}: {gone}");
        }
        assert!(
            out.contains("#123=IFCQUANTITYLENGTH('Length',$,$,4000.);"),
            "{s}: the slab still uses it"
        );
        assert!(out.contains("#124=IFCELEMENTQUANTITY("), "{s}");
        assert!(
            out.contains(",(#80),#124);"),
            "{s}: only the slab holds the shared set"
        );
        assert_refs_valid(&out);
        f.reread();
    }
}

#[test]
fn removing_an_opening_takes_the_quantities_off_its_host() {
    let mut f = setup_text("qopen", "IFC4", &with_quantities("IFC4"));
    let (_, opening) = f.row(OPENING);
    f.m.sql()
        .execute("DELETE FROM opening WHERE id=?1", [opening])
        .unwrap();
    f.done();
    let out = f.output();
    assert!(!out.contains("#121="));
    assert!(out.contains(",(#80),#124);"));
    assert!(out.contains(&format!("'{WALL}'")));
    assert_refs_valid(&out);
}

#[test]
fn a_property_edit_keeps_the_quantities() {
    let text = with_quantities("IFC4");
    let mut f = setup_text("qprop", "IFC4", &text);
    let (kind, id) = f.row(WALL);
    f.m.set_source_prop(&kind, id, "Own.Fire", SqlValue::Text("EI 90".into()), "")
        .unwrap();
    f.done();
    assert_eq!(lost(&text, &f.output()).len(), 1);
}

#[test]
fn a_changed_source_refuses() {
    let f = setup("stale", "IFC4");
    fs::write(&f.src, fixture("IFC4").replace("'kept'", "'other'")).unwrap();
    assert!(matches!(
        f.patch(),
        Err(PatchError::Doubt {
            key: DOUBT_SOURCE,
            ..
        })
    ));
}

/// What the save pipeline does when a patch takes the source's place.
fn put_over_source(f: &Fx) -> PatchOutput {
    let p = f.done();
    let r = base_rebase(&f.m, &f.key, &p).unwrap();
    fs::copy(&f.out, &f.src).unwrap();
    let hash = ddc_model::source::file::hash_file(&f.src, &|| false).unwrap();
    assert!(f.m.note_source_written(&f.key, &hash).unwrap());
    f.m.apply_source_rebase(&f.key, &r).unwrap();
    p
}

#[test]
fn saving_over_the_source_moves_the_base_so_a_second_save_has_nothing_to_do() {
    for s in SCHEMAS {
        let mut f = setup("twice-prop", s);
        let (kind, id) = f.row(WALL);
        f.m.set_source_prop(&kind, id, "Own.Fire", SqlValue::Text("EI 90".into()), "")
            .unwrap();
        put_over_source(&f);
        assert_eq!(
            has_changes(&f.m, &f.key, &f.src),
            Ok(false),
            "{s}: property"
        );
        assert!(f.m.local_changes(&f.key, true).unwrap().is_empty(), "{s}");

        let mut f = setup("twice-move", s);
        let (_, id) = f.row(WALL);
        f.m.move_elements(&[Target::Wall(id)], (1.0, 0.5)).unwrap();
        put_over_source(&f);
        assert_eq!(has_changes(&f.m, &f.key, &f.src), Ok(false), "{s}: move");

        let mut f = setup("twice-delete", s);
        let (_, id) = f.row(WALL);
        f.m.delete_wall(id).unwrap();
        put_over_source(&f);
        assert_eq!(has_changes(&f.m, &f.key, &f.src), Ok(false), "{s}: delete");
    }
}

#[test]
fn a_new_wall_written_over_the_source_is_not_new_the_second_time() {
    let mut f = setup("twice-add", "IFC4");
    let level: i64 =
        f.m.sql()
            .query_row("SELECT id FROM level", [], |r| r.get(0))
            .unwrap();
    let t =
        f.m.add_wall_type(
            "new",
            &[LayerSpec {
                material: "concrete".into(),
                thickness: 0.2,
            }],
        )
        .unwrap();
    let w =
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
    let p = put_over_source(&f);
    assert_eq!(p.linked.len(), 1);
    assert_eq!(has_changes(&f.m, &f.key, &f.src), Ok(false));
    // from here on it is an element like any other: an edit of it is seen, once
    f.m.move_elements(&[Target::Wall(w)], (1.0, 0.0)).unwrap();
    assert_eq!(has_changes(&f.m, &f.key, &f.src), Ok(true));
}

/// Run with `--features test-hooks`.
#[cfg(feature = "test-hooks")]
#[test]
fn a_blocked_link_is_tried_again_on_the_element_in_the_file_and_never_recreates_it() {
    use ddc_io::ifc::patch::{set_links_blocked, REASON_LINK};
    let mut f = setup("link-retry", "IFC4");
    let level: i64 =
        f.m.sql()
            .query_row("SELECT id FROM level", [], |r| r.get(0))
            .unwrap();
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
    let material_rels = |f: &Fx| {
        fs::read_to_string(&f.src)
            .unwrap()
            .matches("IFCRELASSOCIATESMATERIAL(")
            .count()
    };
    let before = material_rels(&f);

    set_links_blocked(true);
    let p = put_over_source(&f);
    assert!(!p.pending_links.is_empty(), "the link is blocked");
    assert!(p.will_not.iter().any(|(_, r)| r == REASON_LINK));
    let guid = p.pending_links[0].0.clone();
    let elements = |f: &Fx| fs::read_to_string(&f.src).unwrap().matches(&format!("'{guid}'")).count();
    assert_eq!(elements(&f), 1);
    assert_eq!(material_rels(&f), before);
    assert_eq!(has_changes(&f.m, &f.key, &f.src), Ok(true), "not saved");

    // still blocked: the same element, no second one, the link still listed
    let p = put_over_source(&f);
    assert_eq!(p.added, 0);
    assert_eq!(elements(&f), 1);
    assert!(p.pending_links.iter().any(|(g, _)| *g == guid));
    assert!(p.will_not.iter().any(|(_, r)| r == REASON_LINK));
    assert!(!f.m.pending_links(&f.key).unwrap().is_empty());
    assert_eq!(has_changes(&f.m, &f.key, &f.src), Ok(true), "not saved");
    assert_eq!(material_rels(&f), before);

    // unblocked: the link is written on the same element and the list is empty
    set_links_blocked(false);
    let p = put_over_source(&f);
    assert!(p.pending_links.is_empty());
    assert_eq!(p.added, 0);
    assert_eq!(elements(&f), 1);
    assert!(material_rels(&f) > before, "the link is in the file");
    assert!(f.m.pending_links(&f.key).unwrap().is_empty());
    assert_eq!(has_changes(&f.m, &f.key, &f.src), Ok(false));
}

/// Run with `--features test-hooks`.
#[cfg(feature = "test-hooks")]
#[test]
fn a_blocked_fill_link_is_written_later_on_the_same_door() {
    use ddc_io::ifc::patch::{set_links_blocked, REASON_LINK};
    let mut f = setup("fill-retry", "IFC4");
    let (_, wall) = f.row(WALL);
    f.m.add_opening(
        wall,
        &OpeningSpec {
            along: 2.0,
            width: 0.9,
            sill: 0.0,
            height: 2.1,
            kind: "door".into(),
        },
    )
    .unwrap();
    let count = |f: &Fx, what: &str| {
        fs::read_to_string(&f.src).unwrap().matches(what).count()
    };
    let (doors, fills) = (count(&f, "IFCDOOR("), count(&f, "IFCRELFILLSELEMENT("));

    set_links_blocked(true);
    let p = put_over_source(&f);
    set_links_blocked(false);
    assert!(p.will_not.iter().any(|(_, r)| r == REASON_LINK));
    assert!(p.pending_links.iter().any(|(_, r)| r.starts_with("IFCRELFILLSELEMENT")));
    assert_eq!(count(&f, "IFCDOOR("), doors + 1);
    assert_eq!(count(&f, "IFCRELFILLSELEMENT("), fills);
    assert!(!f.m.pending_links(&f.key).unwrap().is_empty());
    assert_eq!(has_changes(&f.m, &f.key, &f.src), Ok(true), "not saved");

    // still blocked: no second door, the link still listed
    set_links_blocked(true);
    let p = put_over_source(&f);
    set_links_blocked(false);
    assert_eq!(p.added, 0);
    assert_eq!(count(&f, "IFCDOOR("), doors + 1);
    assert!(!p.pending_links.is_empty());
    assert!(!f.m.pending_links(&f.key).unwrap().is_empty());
    assert_eq!(has_changes(&f.m, &f.key, &f.src), Ok(true), "not saved");

    // unblocked: one fill relation, one door, nothing pending
    let p = put_over_source(&f);
    assert!(p.pending_links.is_empty());
    assert_eq!(p.added, 0);
    assert_eq!(count(&f, "IFCDOOR("), doors + 1);
    assert_eq!(count(&f, "IFCRELFILLSELEMENT("), fills + 1);
    assert!(f.m.pending_links(&f.key).unwrap().is_empty());
    assert_eq!(has_changes(&f.m, &f.key, &f.src), Ok(false));

    // and a further save adds nothing
    let p = put_over_source(&f);
    assert_eq!(p.added, 0);
    assert_eq!(count(&f, "IFCRELFILLSELEMENT("), fills + 1);
}

#[test]
fn an_edit_made_during_the_save_stays_a_change() {
    let mut f = setup("during", "IFC4");
    let (kind, id) = f.row(WALL);
    f.m.set_source_prop(&kind, id, "Own.Fire", SqlValue::Text("EI 90".into()), "")
        .unwrap();
    let p = f.done();
    let r = base_rebase(&f.m, &f.key, &p).unwrap();
    f.m.set_source_prop(&kind, id, "Own.Fire", SqlValue::Text("EI 120".into()), "")
        .unwrap();
    f.m.apply_source_rebase(&f.key, &r).unwrap();
    let c = f.m.local_changes(&f.key, false).unwrap();
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].fields.len(), 1);
    assert_eq!(c[0].fields[0].base, serde_json::json!("EI 90"));
}

#[test]
fn an_edit_the_file_could_not_hold_stays_a_change() {
    let mut f = setup("unheld", "IFC2X3");
    let (kind, id) = f.row(WALL);
    f.m.set_source_prop(&kind, id, "Own.Thick", SqlValue::Text("wide".into()), "")
        .unwrap();
    put_over_source(&f);
    assert_eq!(has_changes(&f.m, &f.key, &f.src), Ok(true));
}

#[test]
fn a_saved_edit_is_not_pending_beside_one_the_file_could_not_hold() {
    for s in SCHEMAS {
        let mut f = setup("unheld-one", s);
        let (kind, id) = f.row(WALL);
        f.m.set_source_prop(&kind, id, "Own.Fire", SqlValue::Text("EI 90".into()), "")
            .unwrap();
        f.m.set_source_prop(&kind, id, "Own.Thick", SqlValue::Text("wide".into()), "")
            .unwrap();
        let p = put_over_source(&f);
        assert_eq!(p.will_not.len(), 1, "{s}");
        let c = f.m.local_changes(&f.key, false).unwrap();
        let left: Vec<&str> = c
            .iter()
            .flat_map(|c| c.fields.iter().map(|f| f.field.as_str()))
            .collect();
        assert_eq!(left, ["p:Own.Thick"], "{s}: only the unwritten edit stays");
        assert_eq!(has_changes(&f.m, &f.key, &f.src), Ok(true), "{s}");
    }
}

#[test]
fn undoing_a_delete_after_the_save_writes_the_element_back() {
    let mut f = setup("undo-delete", "IFC4");
    let (_, id) = f.row(WALL);
    f.m.delete_wall(id).unwrap();
    put_over_source(&f);
    assert!(!fs::read_to_string(&f.src).unwrap().contains(WALL));
    f.m.undo().unwrap();
    assert_eq!(has_changes(&f.m, &f.key, &f.src), Ok(true));
    f.done();
    assert!(f.output().contains(WALL));
}

#[test]
fn a_zipped_source_is_a_doubt_of_its_own() {
    let f = setup("zip", "IFC4");
    fs::write(&f.src, b"PK\x03\x04zipped").unwrap();
    let hash = ddc_model::source::file::hash_file(&f.src, &|| false).unwrap();
    assert!(f.m.note_source_written(&f.key, &hash).unwrap());
    assert!(matches!(
        has_changes(&f.m, &f.key, &f.src),
        Err(PatchError::Doubt { key: DOUBT_ZIP, .. })
    ));
}

#[test]
fn an_inexpressible_edit_is_listed_and_the_line_stays() {
    let mut f = setup("inexpressible", "IFC2X3");
    let (kind, id) = f.row(WALL);
    f.m.set_source_prop(&kind, id, "Own.Thick", SqlValue::Text("wide".into()), "")
        .unwrap();
    let p = f.done();
    assert_eq!(p.will_not, [(kind, REASON_PROP.to_string())]);
    assert_eq!(f.output().len(), fixture("IFC2X3").len());
    assert!(lost(&fixture("IFC2X3"), &f.output()).is_empty());
}

#[test]
fn a_link_edit_is_a_doubt() {
    let mut f = setup("doubt", "IFC4");
    let (_, id) = f.row(WALL);
    let t =
        f.m.add_wall_type(
            "other",
            &[LayerSpec {
                material: "wood".into(),
                thickness: 0.1,
            }],
        )
        .unwrap();
    f.m.sql()
        .execute("UPDATE wall SET wtype=?1 WHERE id=?2", [t, id])
        .unwrap();
    assert!(matches!(
        f.patch(),
        Err(PatchError::Doubt {
            key: DOUBT_EDIT,
            ..
        })
    ));
}

#[test]
fn a_crlf_file_keeps_its_line_ends() {
    let mut f = setup("crlf", "IFC4");
    let text = fixture("IFC4").replace('\n', "\r\n");
    let mut m = Model::open(f.dir.join("crlf.ddc").to_str().unwrap()).unwrap();
    fs::write(&f.src, &text).unwrap();
    import_ifc_file(&mut m, &f.src).unwrap();
    f.key = m.sources().unwrap().remove(0).source;
    f.m = m;
    let (kind, id) = f.row(WALL);
    f.m.set_source_prop(&kind, id, "Fresh.Note", SqlValue::Text("n".into()), "")
        .unwrap();
    f.done();
    let out = f.output();
    assert!(out.contains("'Fresh'"));
    assert_eq!(out.matches('\n').count(), out.matches("\r\n").count());
    assert!(lost(&text, &out).is_empty());
    assert_refs_valid(&out);
}

fn prop_value(m: &Model, key: &str, guid: &str, prop: &str) -> Option<String> {
    let (k, i) = m.element_by_guid(key, guid).unwrap().unwrap();
    // measures are stored as REAL: read every value as text
    m.sql()
        .query_row(
            "SELECT CAST(value AS TEXT) FROM prop WHERE kind=?1 AND ref=?2 AND key=?3",
            rusqlite::params![k, i, prop],
            |r| r.get(0),
        )
        .ok()
}

#[test]
fn removed_prop_line_gone_rest_byte_identical() {
    for s in SCHEMAS {
        let mut f = setup("rmline", s);
        let (kind, id) = f.row(WALL);
        assert_eq!(
            f.m.remove_source_props(&[(kind.as_str(), id)], "Own.Fire")
                .unwrap(),
            1
        );
        f.done();
        let (src, out) = (fixture(s), f.output());
        assert!(!out.contains("'Fire'"), "{s}: the line is gone");
        assert!(
            out.contains("#102=IFCPROPERTYSET('0000000000000000000100',#4,'Own',$,(#101));"),
            "{s}"
        );
        assert_eq!(
            lost(&src, &out),
            [
                "#100=IFCPROPERTYSINGLEVALUE('Fire',$,IFCLABEL('EI 30'),$)",
                "#102=IFCPROPERTYSET('0000000000000000000100',#4,'Own',$,(#100,#101))"
            ],
            "{s}: nothing else changes"
        );
        assert_refs_valid(&out);
        let (back, bk) = f.reread();
        assert_eq!(prop_value(&back, &bk, WALL, "Own.Fire"), None, "{s}");
        assert!(prop_value(&back, &bk, WALL, "Own.Thick").is_some(), "{s}");
    }
}

#[test]
fn removing_every_prop_of_a_set_drops_its_relation_and_set() {
    let mut f = setup("rmall", "IFC4");
    let (kind, id) = f.row(WALL);
    f.m.remove_source_props(&[(kind.as_str(), id)], "Own.Fire")
        .unwrap();
    f.m.remove_source_props(&[(kind.as_str(), id)], "Own.Thick")
        .unwrap();
    f.done();
    let (src, out) = (fixture("IFC4"), f.output());
    let gone = lost(&src, &out);
    let ids: Vec<u32> = gone.iter().filter_map(|s| id_of(s)).collect();
    assert_eq!(ids, [100, 101, 102, 103], "{gone:?}");
    assert!(out.contains("#112=IFCRELDEFINESBYPROPERTIES"));
    assert_refs_valid(&out);
    let (back, bk) = f.reread();
    assert_eq!(prop_value(&back, &bk, WALL, "Own.Fire"), None);
    assert_eq!(prop_value(&back, &bk, WALL, "Own.Thick"), None);
}

#[test]
fn removing_from_shared_set_leaves_other_element() {
    for s in SCHEMAS {
        // the shared set holds one property: the wall leaves the relation, the slab keeps all
        let mut f = setup("rmshare1", s);
        let (kind, id) = f.row(WALL);
        f.m.remove_source_props(&[(kind.as_str(), id)], "Common.Grade")
            .unwrap();
        f.done();
        let (src, out) = (fixture(s), f.output());
        let gone = lost(&src, &out);
        assert_eq!(gone.len(), 1, "{s}: {gone:?}");
        assert!(gone[0].starts_with("#112="), "{s}");
        assert!(
            out.contains("#110=IFCPROPERTYSINGLEVALUE('Grade',$,IFCLABEL('A'),$);"),
            "{s}"
        );
        assert!(out.contains("#111=IFCPROPERTYSET("), "{s}");
        assert!(out.contains(",(#80),#111);"), "{s}");
        assert_refs_valid(&out);
        let (back, bk) = f.reread();
        assert_eq!(prop_value(&back, &bk, WALL, "Common.Grade"), None, "{s}");
        assert_eq!(
            prop_value(&back, &bk, SLAB, "Common.Grade").as_deref(),
            Some("A"),
            "{s}"
        );

        // two properties: the wall gets its own set without the removed one
        let text = fixture(s)
            .replace(
                "#111=IFCPROPERTYSET('0000000000000000000110',#4,'Common',$,(#110));",
                "#113=IFCPROPERTYSINGLEVALUE('Extra',$,IFCLABEL('E'),$);\n#111=IFCPROPERTYSET('0000000000000000000110',#4,'Common',$,(#110,#113));",
            );
        let mut f = setup_text("rmshare2", s, &text);
        let (kind, id) = f.row(WALL);
        f.m.remove_source_props(&[(kind.as_str(), id)], "Common.Grade")
            .unwrap();
        f.done();
        let out = f.output();
        assert!(
            out.contains("#111=IFCPROPERTYSET('0000000000000000000110',#4,'Common',$,(#110,#113));"),
            "{s}: the shared set is untouched"
        );
        assert!(out.contains("#110=IFCPROPERTYSINGLEVALUE('Grade'"), "{s}");
        let gone = lost(&text, &out);
        assert_eq!(gone.len(), 1, "{s}: {gone:?}");
        assert!(gone[0].starts_with("#112="), "{s}");
        assert!(out.contains(",(#80),#111);"), "{s}: the slab keeps the old set");
        assert_refs_valid(&out);
        let (back, bk) = f.reread();
        assert_eq!(prop_value(&back, &bk, WALL, "Common.Grade"), None, "{s}");
        assert_eq!(
            prop_value(&back, &bk, WALL, "Common.Extra").as_deref(),
            Some("E"),
            "{s}"
        );
        assert_eq!(
            prop_value(&back, &bk, SLAB, "Common.Grade").as_deref(),
            Some("A"),
            "{s}"
        );
        assert_eq!(
            prop_value(&back, &bk, SLAB, "Common.Extra").as_deref(),
            Some("E"),
            "{s}"
        );
    }
}

#[test]
fn remove_then_undo_saves_nothing() {
    let mut f = setup("rmundo", "IFC4");
    let (kind, id) = f.row(WALL);
    f.m.remove_source_props(&[(kind.as_str(), id)], "Own.Fire")
        .unwrap();
    f.m.undo().unwrap();
    let p = f.done();
    assert!(p.no_changes);
    assert_eq!(fs::read(&f.out).unwrap(), fs::read(&f.src).unwrap());
}

/// Our own export of two tables, the first turned: the source of the placement-only tests.
fn comp_fixture() -> String {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let fam = m.load_library_family("Table").unwrap();
    let t = m.comp_type_id(fam, "1600x800").unwrap().unwrap();
    let mut a = CompSpec::at(lv, t, 1.0, 3.0);
    a.rotation = 0.3;
    let b = CompSpec::at(lv, t, 5.0, 3.0);
    m.add_components(&[a, b]).unwrap();
    export_ifc(&m).unwrap()
}

/// The imported fixture, its text and its components as (row, GlobalId), west to east.
fn comp_setup(tag: &str) -> (Fx, String, Vec<(i64, String)>) {
    let text = comp_fixture();
    let f = setup_text(tag, "IFC4", &text);
    let comps: Vec<(i64, String)> = {
        let mut st = f
            .m
            .sql()
            .prepare(
                "SELECT c.id, e.guid FROM comp c JOIN ext_ref e ON e.kind='comp' AND e.ref=c.id \
                 ORDER BY c.x",
            )
            .unwrap();
        let rows = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows
    };
    assert_eq!(comps.len(), 2);
    (f, text, comps)
}

/// The statement of the product with GlobalId `guid`.
fn product_line<'a>(stmts: &'a [String], guid: &str) -> &'a str {
    stmts
        .iter()
        .find(|s| s.contains(&format!("('{guid}'")))
        .unwrap()
}

/// Top-level arguments of one statement.
fn top_args(stmt: &str) -> Vec<String> {
    let body = &stmt[stmt.find('(').unwrap() + 1..stmt.rfind(')').unwrap()];
    let (mut out, mut cur, mut depth, mut in_str) = (vec![], String::new(), 0, false);
    for ch in body.chars() {
        match ch {
            '\'' => in_str = !in_str,
            '(' if !in_str => depth += 1,
            ')' if !in_str => depth -= 1,
            ',' if !in_str && depth == 0 => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(ch);
    }
    out.push(cur);
    out
}

fn is_frame_value(stmt: &str) -> bool {
    stmt.contains("IFCPROPERTYSINGLEVALUE('Local'")
        || stmt.contains("IFCPROPERTYSINGLEVALUE('World'")
}

#[test]
fn comp_move_patches_placement_only() {
    let (mut f, src, comps) = comp_setup("comp-move");
    let (id, guid) = comps[0].clone();
    let before = f.m.component(id).unwrap().at;
    f.m.move_elements(&[Target::Component(id)], (1.0, 0.5))
        .unwrap();
    let p = f.done();
    assert_eq!(p.edited, 1);
    assert!(p.will_not.is_empty(), "{:?}", p.will_not);

    let out = f.output();
    let product = id_of(product_line(&statements(&src), &guid)).unwrap();
    let gone = lost(&src, &out);
    let ids: HashSet<u32> = gone.iter().filter_map(|x| id_of(x)).collect();
    // the product line and the two frame values of its own set, nothing else
    assert_eq!(ids.len(), 3, "{gone:?}");
    assert!(ids.contains(&product), "{gone:?}");
    for g in gone.iter().filter(|g| id_of(g) != Some(product)) {
        assert!(is_frame_value(g), "{g}");
    }
    assert_refs_valid(&out);

    let (back, key) = f.reread();
    let (_, row) = back.element_by_guid(&key, &guid).unwrap().unwrap();
    let at = back.component(row).unwrap().at;
    assert!(
        (at.0 - before.0 - 1.0).abs() < 1e-6 && (at.1 - before.1 - 0.5).abs() < 1e-6,
        "{at:?} from {before:?}"
    );
}

#[test]
fn comp_rotate_keeps_representation_bytes() {
    let (mut f, src, comps) = comp_setup("comp-rotate");
    let (id, guid) = comps[0].clone();
    let before = f.m.component(id).unwrap();
    f.m.rotate_elements(
        &[Target::Component(id)],
        (before.at.0, before.at.1),
        0.5,
        false,
    )
    .unwrap();
    let p = f.done();
    assert_eq!(p.edited, 1);
    assert!(p.will_not.is_empty(), "{:?}", p.will_not);

    let out = f.output();
    let old = top_args(product_line(&statements(&src), &guid));
    let new = top_args(product_line(&statements(&out), &guid));
    assert_eq!(old.len(), new.len());
    assert_ne!(old[5], new[5], "the placement moves");
    assert_eq!(old[6], new[6], "the representation stays byte for byte");
    for i in (0..old.len()).filter(|i| *i != 5) {
        assert_eq!(old[i], new[i], "argument {i}");
    }
    assert_refs_valid(&out);

    let (back, key) = f.reread();
    let (_, row) = back.element_by_guid(&key, &guid).unwrap().unwrap();
    let c = back.component(row).unwrap();
    assert!(
        (c.rotation - before.rotation - 0.5).abs() < 1e-6,
        "{}",
        c.rotation
    );
    assert!((c.at.0 - before.at.0).abs() < 1e-6 && (c.at.1 - before.at.1).abs() < 1e-6);
}

#[test]
fn comp_mirror_still_falls_back() {
    let (mut f, src, comps) = comp_setup("comp-mirror");
    let (id, _) = comps[0].clone();
    let at = f.m.component(id).unwrap().at;
    f.m.mirror_elements(
        &[Target::Component(id)],
        (at.0 + 2.0, 0.0),
        (at.0 + 2.0, 1.0),
        false,
    )
    .unwrap();
    assert!(f.m.component(id).unwrap().mirror);
    assert!(matches!(
        f.patch(),
        Err(PatchError::Doubt {
            key: DOUBT_EDIT,
            ..
        })
    ));
    assert!(!f.out.exists(), "a doubt writes nothing");
    assert_eq!(fs::read_to_string(&f.src).unwrap(), src);

    // a move together with any column outside the placement keeps the doubt as well
    let (g, _, comps) = comp_setup("comp-vals");
    let (id, _) = comps[0].clone();
    g.m.sql()
        .execute(
            "UPDATE comp SET x=x+1.0, vals=vals||' ' WHERE id=?1",
            [id],
        )
        .unwrap();
    assert!(matches!(
        g.patch(),
        Err(PatchError::Doubt {
            key: DOUBT_EDIT,
            ..
        })
    ));
}

#[test]
fn untouched_lines_byte_identical_after_place_edit() {
    let (mut f, src, comps) = comp_setup("comp-untouched");
    let (id, guid) = comps[0].clone();
    f.m.move_elements(&[Target::Component(id)], (-0.25, 2.0))
        .unwrap();
    f.done();
    let out = f.output();
    let (old, new) = (statements(&src), statements(&out));
    let product = id_of(product_line(&old, &guid)).unwrap();
    let have: HashSet<&String> = new.iter().collect();
    let known: HashSet<&String> = old.iter().collect();

    // every statement but the product line and its two frame values is there byte for byte
    let changed: Vec<&String> = old.iter().filter(|s| !have.contains(s)).collect();
    assert_eq!(changed.len(), 3, "{changed:?}");
    for s in changed {
        assert!(id_of(s) == Some(product) || is_frame_value(s), "lost: {s}");
    }
    let other = product_line(&old, &comps[1].1).to_string();
    assert!(have.contains(&other), "the other table is untouched");

    // what the patch appends is only the new placement chain: no shape is copied
    let max_old = old.iter().filter_map(|s| id_of(s)).max().unwrap();
    for s in new.iter().filter(|s| !known.contains(s)) {
        let n = id_of(s).unwrap();
        if n <= max_old {
            assert!(n == product || is_frame_value(s), "rewritten: {s}");
            continue;
        }
        let ty = &s[s.find('=').unwrap() + 1..s.find('(').unwrap()];
        assert!(
            matches!(
                ty,
                "IFCLOCALPLACEMENT" | "IFCAXIS2PLACEMENT3D" | "IFCCARTESIANPOINT" | "IFCDIRECTION"
            ),
            "appended: {s}"
        );
    }
    let head = &src[..src.find("DATA;").unwrap()];
    assert!(out.starts_with(head), "the header stays");
}

/// The fixture plus a classification system and four references: the wall's own, one the slab and
/// the door share, and one code the opening already holds.
fn class_text(schema: &str) -> String {
    let (c4, r4) = if schema == "IFC2X3" {
        ("", "")
    } else {
        (",$,$,$", ",$,$")
    };
    let lines = format!(
        "#200=IFCCLASSIFICATION('Org','1',$,'Uniclass'{c4});
#201=IFCCLASSIFICATIONREFERENCE($,'Ss_25','Walls',#200{r4});
#202=IFCCLASSIFICATIONREFERENCE($,'Ss_30','Floors',#200{r4});
#203=IFCRELASSOCIATESCLASSIFICATION('0000000000000000000203',#4,$,$,(#41),#201);
#204=IFCRELASSOCIATESCLASSIFICATION('0000000000000000000204',#4,$,$,(#80,#70),#202);
#205=IFCCLASSIFICATIONREFERENCE($,'Ss_40',$,#200{r4});
#206=IFCRELASSOCIATESCLASSIFICATION('0000000000000000000206',#4,$,$,(#73),#205);"
    );
    fixture(schema).replace(SKIPPED, &format!("{lines}\n{SKIPPED}"))
}

fn class_setup(tag: &str, schema: &str) -> (Fx, String) {
    let text = class_text(schema);
    (setup_text(tag, schema, &text), text)
}

fn class_prop(f: &Fx, guid: &str) -> Option<String> {
    let (back, bk) = f.reread();
    prop_value(&back, &bk, guid, "Classification.Uniclass")
}

fn ref_tail(schema: &str) -> &'static str {
    if schema == "IFC2X3" {
        ""
    } else {
        ",$,$"
    }
}

#[test]
fn class_code_edit_swaps_own_reference() {
    for s in SCHEMAS {
        let (mut f, src) = class_setup("clsown", s);
        let (kind, id) = f.row(WALL);
        f.m.set_source_prop(&kind, id, "Classification.Uniclass", SqlValue::Text("Ss_99".into()), "")
            .unwrap();
        f.done();
        let out = f.output();
        assert_eq!(
            lost(&src, &out),
            [format!("#201=IFCCLASSIFICATIONREFERENCE($,'Ss_25','Walls',#200{})", ref_tail(s))],
            "{s}"
        );
        assert!(
            out.contains(&format!(
                "#201=IFCCLASSIFICATIONREFERENCE($,'Ss_99',$,#200{});",
                ref_tail(s)
            )),
            "{s}"
        );
        assert_refs_valid(&out);
        assert_eq!(class_prop(&f, WALL).as_deref(), Some("Ss_99"), "{s}");
    }
}

#[test]
fn shared_reference_moves_element_only() {
    for s in SCHEMAS {
        let (mut f, src) = class_setup("clsshared", s);
        let (kind, id) = f.row(SLAB);
        f.m.set_source_prop(&kind, id, "Classification.Uniclass", SqlValue::Text("Ss_77".into()), "")
            .unwrap();
        f.done();
        let out = f.output();
        assert_eq!(
            lost(&src, &out),
            ["#204=IFCRELASSOCIATESCLASSIFICATION('0000000000000000000204',#4,$,$,(#80,#70),#202)"],
            "{s}: only the shared relation line changes"
        );
        assert!(
            out.contains("#204=IFCRELASSOCIATESCLASSIFICATION('0000000000000000000204',#4,$,$,(#70),#202);"),
            "{s}: the shared reference stays, the relation loses the slab"
        );
        assert!(out.contains("'Ss_77',$,#200"), "{s}: one new reference");
        assert_eq!(out.matches("IFCCLASSIFICATIONREFERENCE(").count(), 4, "{s}");
        assert_refs_valid(&out);
        assert_eq!(class_prop(&f, SLAB).as_deref(), Some("Ss_77"), "{s}");
        assert_eq!(class_prop(&f, DOOR).as_deref(), Some("Ss_30 Floors"), "{s}");
    }
}

#[test]
fn existing_code_reference_reused() {
    for s in SCHEMAS {
        let (mut f, src) = class_setup("clsreuse", s);
        let (kind, id) = f.row(SLAB);
        f.m.set_source_prop(&kind, id, "Classification.Uniclass", SqlValue::Text("Ss_40".into()), "")
            .unwrap();
        f.done();
        let out = f.output();
        assert_eq!(out.matches("IFCCLASSIFICATIONREFERENCE(").count(), 3, "{s}: none added");
        assert!(
            out.contains("#206=IFCRELASSOCIATESCLASSIFICATION('0000000000000000000206',#4,$,$,(#73,#80),#205);"),
            "{s}"
        );
        assert_eq!(lost(&src, &out).len(), 2, "{s}: both relations only");
        assert_refs_valid(&out);
        assert_eq!(class_prop(&f, SLAB).as_deref(), Some("Ss_40"), "{s}");
    }
}

#[test]
fn empty_code_drops_from_relation() {
    for s in SCHEMAS {
        let (mut f, src) = class_setup("clsdrop", s);
        let (kw, iw) = f.row(WALL);
        let (kd, id) = f.row(DOOR);
        f.m.remove_source_props(&[(kw.as_str(), iw)], "Classification.Uniclass")
            .unwrap();
        f.m.remove_source_props(&[(kd.as_str(), id)], "Classification.Uniclass")
            .unwrap();
        f.done();
        let out = f.output();
        assert!(!out.contains("#203="), "{s}: empty relation removed");
        assert!(out.contains("(#80),#202);"), "{s}: the slab stays on the shared one");
        // the door lives on its opening's row (69ac077f): the opening's own code goes too
        assert!(!out.contains("#206="), "{s}: the opening's relation removed");
        assert_eq!(lost(&src, &out).len(), 3, "{s}");
        assert_refs_valid(&out);
        assert_eq!(class_prop(&f, WALL), None, "{s}");
        assert_eq!(class_prop(&f, DOOR), None, "{s}");
        assert!(class_prop(&f, SLAB).is_some(), "{s}");
    }
}

#[test]
fn a_classification_is_added_to_an_element_without_one() {
    let s = "IFC4";
    let text = class_text(s).replace("(#80,#70),#202", "(#80),#202");
    let mut f = setup_text("clsadd", s, &text);
    let (kind, id) = f.row(DOOR);
    f.m.set_source_prop(&kind, id, "Classification.Uniclass", SqlValue::Text("Ss_88".into()), "")
        .unwrap();
    f.done();
    let out = f.output();
    assert!(lost(&text, &out).is_empty());
    assert_eq!(class_prop(&f, DOOR).as_deref(), Some("Ss_88"));
}

#[test]
fn untouched_lines_byte_identical_after_class_edit() {
    let (mut f, src) = class_setup("clsuntouched", "IFC4");
    let (kind, id) = f.row(SLAB);
    f.m.set_source_prop(&kind, id, "Classification.Uniclass", SqlValue::Text("Ss_77".into()), "")
        .unwrap();
    f.done();
    let out = f.output();
    let (old, new) = (statements(&src), statements(&out));
    let known: HashSet<&String> = old.iter().collect();
    let have: HashSet<&String> = new.iter().collect();
    let changed: Vec<&String> = old.iter().filter(|s| !have.contains(s)).collect();
    assert_eq!(changed.len(), 1, "{changed:?}");
    assert_eq!(id_of(changed[0]), Some(204));
    let max_old = old.iter().filter_map(|s| id_of(s)).max().unwrap();
    for s in new.iter().filter(|s| !known.contains(s)) {
        let n = id_of(s).unwrap();
        if n <= max_old {
            assert_eq!(n, 204, "rewritten: {s}");
            continue;
        }
        let ty = &s[s.find('=').unwrap() + 1..s.find('(').unwrap()];
        assert!(
            matches!(ty, "IFCCLASSIFICATIONREFERENCE" | "IFCRELASSOCIATESCLASSIFICATION"),
            "appended: {s}"
        );
    }
    let head = &src[..src.find("DATA;").unwrap()];
    assert!(out.starts_with(head));
}

/// A new wall whose material link the file cannot hold, saved over the source: the wall is in the
/// file, its link stays pending.
#[cfg(feature = "test-hooks")]
fn pending_link_fixture(tag: &str) -> Fx {
    use ddc_io::ifc::patch::set_links_blocked;
    let mut f = setup(tag, "IFC4");
    let level: i64 =
        f.m.sql()
            .query_row("SELECT id FROM level", [], |r| r.get(0))
            .unwrap();
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
    set_links_blocked(true);
    put_over_source(&f);
    set_links_blocked(false);
    assert!(!f.m.pending_links(&f.key).unwrap().is_empty());
    f
}

/// Run with `--features test-hooks`.
#[cfg(feature = "test-hooks")]
#[test]
fn unwritable_link_drop_clears_dirty() {
    let mut f = pending_link_fixture("drop-dirty");
    assert_eq!(has_changes(&f.m, &f.key, &f.src), Ok(true), "pending counts as unsaved");
    let n = f.m.drop_pending_links(&f.key, Some("wall")).unwrap();
    assert!(n > 0);
    assert!(f.m.pending_links(&f.key).unwrap().is_empty());
    assert_eq!(has_changes(&f.m, &f.key, &f.src), Ok(false), "not dirty forever");
    assert_eq!(f.m.drop_pending_links(&f.key, None).unwrap(), 0, "nothing left to drop");
}

/// Run with `--features test-hooks`.
#[cfg(feature = "test-hooks")]
#[test]
fn pending_link_survives_reimport() {
    let mut f = pending_link_fixture("reimport");
    let before = f.m.pending_links(&f.key).unwrap();
    import_ifc_file(&mut f.m, &f.src).unwrap();
    assert_eq!(f.m.pending_links(&f.key).unwrap(), before);
}

/// Run with `--features test-hooks`.
#[cfg(feature = "test-hooks")]
#[test]
fn drop_is_one_undo() {
    let mut f = pending_link_fixture("drop-undo");
    let before = f.m.pending_links(&f.key).unwrap();
    let steps = f.m.sql().query_row("SELECT COUNT(*) FROM op", [], |r| r.get::<_, i64>(0)).unwrap();
    f.m.drop_pending_links(&f.key, None).unwrap();
    let after = f.m.sql().query_row("SELECT COUNT(*) FROM op", [], |r| r.get::<_, i64>(0)).unwrap();
    assert_eq!(after, steps + 1, "one journal step");
    f.m.undo().unwrap();
    assert_eq!(f.m.pending_links(&f.key).unwrap(), before);
    assert_eq!(has_changes(&f.m, &f.key, &f.src), Ok(true));
}

/// The fixture plus two wall type objects: `Ext 250` related to `first`, `Alt 300` to `second`
/// (None: no relation at all).
fn with_types(schema: &str, first: &str, second: Option<&str>) -> String {
    let rel2 = second.map_or(String::new(), |m| {
        format!("#133=IFCRELDEFINESBYTYPE('0000000000000000000133',#4,$,$,({m}),#132);\n")
    });
    fixture(schema).replace(
        SKIPPED,
        &format!(
            "#130=IFCWALLTYPE('0000000000000000000130',#4,'Ext 250',$,$,$,$,$,$,.STANDARD.);
#131=IFCRELDEFINESBYTYPE('0000000000000000000131',#4,$,$,({first}),#130);
#132=IFCWALLTYPE('0000000000000000000132',#4,'Alt 300',$,$,$,$,$,$,.STANDARD.);
{rel2}{SKIPPED}"
        ),
    )
}

/// The wall now has a new wall type row named `name`.
fn retype(f: &mut Fx, name: &str) {
    let (_, id) = f.row(WALL);
    let t =
        f.m.add_wall_type(
            name,
            &[LayerSpec {
                material: "Brick".into(),
                thickness: 0.3,
            }],
        )
        .unwrap();
    f.m.sql()
        .execute("UPDATE wall SET wtype=?1 WHERE id=?2", [t, id])
        .unwrap();
}

fn rel_lines(t: &str) -> Vec<String> {
    statements(t)
        .into_iter()
        .filter(|s| s.contains("IFCRELDEFINESBYTYPE("))
        .collect()
}

#[test]
fn change_type_moves_element_between_type_rels() {
    for s in SCHEMAS {
        let mut f = setup_text("typemove", s, &with_types(s, "#41,#70", Some("#80")));
        retype(&mut f, "Alt 300");
        let p = f.done();
        assert!(p.will_not.is_empty(), "{s}: {:?}", p.will_not);
        let out = f.output();
        let rels = rel_lines(&out);
        assert!(
            rels.contains(&"#131=IFCRELDEFINESBYTYPE('0000000000000000000131',#4,$,$,(#70),#130)".to_string()),
            "{s}: {rels:?}"
        );
        assert!(
            rels.contains(&"#133=IFCRELDEFINESBYTYPE('0000000000000000000133',#4,$,$,(#80,#41),#132)".to_string()),
            "{s}: {rels:?}"
        );
        assert_eq!(rels.len(), 2, "{s}: {rels:?}");
        assert_refs_valid(&out);
    }
}

#[test]
fn last_member_drops_old_rel() {
    for s in SCHEMAS {
        let mut f = setup_text("typelast", s, &with_types(s, "#41", Some("#80")));
        retype(&mut f, "Alt 300");
        f.done();
        let out = f.output();
        assert!(!out.contains("#131="), "{s}");
        assert!(!out.contains("0000000000000000000131"), "{s}");
        let rels = rel_lines(&out);
        assert_eq!(rels.len(), 1, "{s}: {rels:?}");
        assert!(rels[0].ends_with("(#80,#41),#132)"), "{s}: {rels:?}");
        assert!(out.contains("#130=IFCWALLTYPE("), "{s}");
        assert_refs_valid(&out);
    }
}

#[test]
fn target_type_without_rel_gets_new_rel() {
    for s in SCHEMAS {
        let mut f = setup_text("typenew", s, &with_types(s, "#41,#70", None));
        retype(&mut f, "Alt 300");
        let p = f.done();
        let out = f.output();
        let rels = rel_lines(&out);
        assert_eq!(rels.len(), 2, "{s}: {rels:?}");
        assert!(
            rels.contains(&"#131=IFCRELDEFINESBYTYPE('0000000000000000000131',#4,$,$,(#70),#130)".to_string()),
            "{s}: {rels:?}"
        );
        let fresh: Vec<&String> = rels.iter().filter(|r| !r.starts_with("#131=")).collect();
        assert!(
            fresh[0].ends_with(",(#41),#132)"),
            "{s}: {fresh:?}"
        );
        assert!(p.edited >= 1, "{s}");
        assert_refs_valid(&out);
        check_patched(&f.out, p.expect.as_ref().unwrap(), None).unwrap();
    }
}

#[test]
fn type_created_here_still_doubts() {
    let mut f = setup_text("typedoubt", "IFC4", &with_types("IFC4", "#41,#70", Some("#80")));
    retype(&mut f, "Made here");
    match f.patch() {
        Err(PatchError::Doubt { key, detail }) => {
            assert_eq!(key, DOUBT_EDIT);
            assert!(detail.contains("type not in the file"), "{detail}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn untouched_lines_byte_equal_after_type_change() {
    for s in SCHEMAS {
        let src = with_types(s, "#41,#70", Some("#80"));
        let mut f = setup_text("typebytes", s, &src);
        retype(&mut f, "Alt 300");
        f.done();
        let out = f.output();
        let gone: HashSet<u32> = lost(&src, &out).iter().filter_map(|x| id_of(x)).collect();
        assert!(gone.contains(&131) && gone.contains(&133), "{s}: {gone:?}");
        let allowed = HashSet::from([41, 70, 73, 131, 133]);
        assert!(gone.is_subset(&allowed), "{s}: {gone:?}");
        for keep in [SKIPPED, MULTILINE, "#130=IFCWALLTYPE(", "#132=IFCWALLTYPE(", "#57=IFCREL"] {
            assert!(out.contains(keep), "{s}: {keep}");
        }
        assert!(out.contains("/* wall along +Y from (1000,2000), 4000 long */"), "{s}");
    }
}

/// `with_types` where `Alt 300` also has its 300 mm layer set.
fn with_layers(schema: &str) -> String {
    with_types(schema, "#41,#70", Some("#80")).replace(
        SKIPPED,
        &format!(
            "#141=IFCMATERIALLAYER(#50,300.,$);
#142=IFCMATERIALLAYERSET((#141),'Alt 300');
#143=IFCRELASSOCIATESMATERIAL('0000000000000000000143',#4,$,$,(#132),#142);
{SKIPPED}"
        ),
    )
}

fn material_rels(t: &str) -> Vec<String> {
    statements(t)
        .into_iter()
        .filter(|s| s.contains("IFCRELASSOCIATESMATERIAL("))
        .collect()
}

/// The usage the wall's (`#41`) material relation points at, as a statement.
fn wall_usage(t: &str) -> String {
    let rels = material_rels(t);
    let rel = rels.iter().find(|r| r.contains("(#41)")).expect("a relation of the wall");
    let end = rel.rsplit('#').next().unwrap().trim_end_matches(')');
    let id: u32 = end.parse().unwrap();
    statements(t).into_iter().find(|s| id_of(s) == Some(id)).unwrap()
}

#[test]
fn type_change_moves_material_association() {
    for s in SCHEMAS {
        let mut f = setup_text("matmove", s, &with_layers(s));
        retype(&mut f, "Alt 300");
        let p = f.done();
        assert!(p.will_not.is_empty(), "{s}: {:?}", p.will_not);
        let out = f.output();
        let usage = wall_usage(&out);
        assert!(usage.contains("IFCMATERIALLAYERSETUSAGE(#142,"), "{s}: {usage}");
        assert!(!usage.starts_with("#55="), "{s}: the old usage is not the wall's");
        // the new set is centred like the old one
        assert!(usage.contains("-150."), "{s}: {usage}");
        assert!(
            !material_rels(&out).iter().any(|r| r.contains("(#41)") && r.ends_with(",#55)")),
            "{s}"
        );
        assert_refs_valid(&out);
    }
}

#[test]
fn type_change_layer_thickness_matches_body() {
    for s in SCHEMAS {
        let mut f = setup_text("matthick", s, &with_layers(s));
        retype(&mut f, "Alt 300");
        f.done();
        let out = f.output();
        let usage = wall_usage(&out);
        let set: u32 = usage["IFCMATERIALLAYERSETUSAGE(#".len() + usage.find('=').unwrap() + 1..]
            .split(',')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let all = statements(&out);
        let set_stmt = all.iter().find(|x| id_of(x) == Some(set)).unwrap();
        let layers: Vec<u32> = set_stmt
            .split(|c| c == '#' || c == ',' || c == ')')
            .filter_map(|x| x.parse().ok())
            .collect();
        let layer_total: f64 = layers
            .iter()
            .filter_map(|l| all.iter().find(|x| id_of(x) == Some(*l)))
            .filter(|x| x.contains("IFCMATERIALLAYER("))
            .map(|x| x.split(',').nth(1).unwrap().parse::<f64>().unwrap())
            .sum();
        assert!((layer_total - 300.0).abs() < 1e-6, "{s}: {layer_total}");
        let thick: Vec<f64> = all
            .iter()
            .filter(|x| x.contains("IFCRECTANGLEPROFILEDEF("))
            .filter_map(|x| x.rsplit(',').next()?.trim_end_matches(')').parse().ok())
            .collect();
        assert!(thick.iter().any(|t| (t - layer_total).abs() < 1e-6), "{s}: {thick:?}");
    }
}

#[test]
fn old_material_rel_dropped_when_empty() {
    for s in SCHEMAS {
        let mut f = setup_text("matdrop", s, &with_layers(s));
        retype(&mut f, "Alt 300");
        f.done();
        let out = f.output();
        assert!(!out.contains("#56="), "{s}");
        assert!(!out.contains("'0000000000000000000004'"), "{s}");
        // the new type's own association is untouched
        assert!(out.contains("#143=IFCRELASSOCIATESMATERIAL("), "{s}");
        assert_refs_valid(&out);
    }
}

/// A second new wall whose link is also pending, in the same table.
#[cfg(feature = "test-hooks")]
fn two_pending_walls(tag: &str) -> (Fx, Vec<ddc_model::source::PendingLink>) {
    use ddc_io::ifc::patch::set_links_blocked;
    let mut f = setup(tag, "IFC4");
    let level: i64 =
        f.m.sql()
            .query_row("SELECT id FROM level", [], |r| r.get(0))
            .unwrap();
    let t =
        f.m.add_wall_type(
            "new",
            &[LayerSpec {
                material: "concrete".into(),
                thickness: 0.2,
            }],
        )
        .unwrap();
    for y in [0.0, 5.0] {
        f.m.add_wall(&WallSpec {
            a: (10.0, y),
            b: (14.0, y),
            level,
            base_offset: 0.0,
            height: 2.5,
            wall_type: t,
            offset0: None,
        })
        .unwrap();
    }
    set_links_blocked(true);
    put_over_source(&f);
    set_links_blocked(false);
    let list = f.m.pending_links(&f.key).unwrap();
    assert!(list.len() >= 2 && list.iter().all(|l| l.table == "wall"));
    (f, list)
}

/// Run with `--features test-hooks`.
#[cfg(feature = "test-hooks")]
#[test]
fn drop_one_link_keeps_other_pending_in_table() {
    let (mut f, list) = two_pending_walls("drop-one");
    let n = f.m.drop_pending_link(&f.key, &list[0].guid, None).unwrap();
    assert_eq!(n, list[0].rels.len());
    let left = f.m.pending_links(&f.key).unwrap();
    assert_eq!(left.len(), list.len() - 1);
    assert!(left.iter().all(|l| l.guid != list[0].guid));
    assert_eq!(has_changes(&f.m, &f.key, &f.src), Ok(true), "the other link is still unsaved");
}

/// Run with `--features test-hooks`.
#[cfg(feature = "test-hooks")]
#[test]
fn drop_status_counts_links() {
    let (mut f, list) = two_pending_walls("drop-count");
    let all: usize = list.iter().map(|l| l.rels.len()).sum();
    let first = f.m.drop_pending_link(&f.key, &list[0].guid, None).unwrap();
    let rest = f.m.drop_pending_link(&f.key, &list[1].guid, None).unwrap();
    assert_eq!(first, list[0].rels.len());
    assert_eq!(rest, list[1].rels.len());
    assert!(first + rest <= all && first > 0 && rest > 0);
    assert_eq!(f.m.drop_pending_link(&f.key, &list[0].guid, None).unwrap(), 0, "already dropped");
}

/// Run with `--features test-hooks`.
#[cfg(feature = "test-hooks")]
#[test]
fn drop_one_link_one_undo() {
    let (mut f, list) = two_pending_walls("drop-one-undo");
    let steps = f.m.sql().query_row("SELECT COUNT(*) FROM op", [], |r| r.get::<_, i64>(0)).unwrap();
    f.m.drop_pending_link(&f.key, &list[0].guid, None).unwrap();
    let after = f.m.sql().query_row("SELECT COUNT(*) FROM op", [], |r| r.get::<_, i64>(0)).unwrap();
    assert_eq!(after, steps + 1, "one journal step");
    f.m.undo().unwrap();
    assert_eq!(f.m.pending_links(&f.key).unwrap(), list);
}
