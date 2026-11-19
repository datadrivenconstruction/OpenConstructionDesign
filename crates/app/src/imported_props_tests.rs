// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Imported property editing: one undo step per edit (one or many elements), `<varies>` rows,
//! every row listed, new properties reach the IFC patch, bad numbers write nothing.

use super::*;
use crate::editor::bodies::BodyKind;
use ddc_geom::mesh::Mesh;
use ddc_geom::v3;
use rusqlite::params;

const EN: DecimalMark = DecimalMark::Point;
const RU: DecimalMark = DecimalMark::Comma;

fn mesh(ed: &mut Editor) -> i64 {
    let m = Mesh {
        positions: vec![v3(0.0, 0.0, 0.0), v3(1.0, 0.0, 0.0), v3(0.0, 1.0, 0.0)],
        triangles: vec![[0, 1, 2]],
        face_of: vec![0],
    };
    let (pb, ib) = ddc_model::mesh_blobs(&m);
    let level = ed.level;
    ed.model
        .batch_walls("wall_ifp1_test_mesh", |db| {
            db.execute(
                "INSERT INTO mesh(level,category,name,positions,indices) VALUES (?1,'Furniture','m',?2,?3)",
                (level, pb, ib),
            )?;
            Ok(db.last_insert_rowid())
        })
        .unwrap()
}

fn put(ed: &Editor, id: i64, key: &str, v: Sql, unit: &str) {
    ed.model
        .sql()
        .execute(
            "INSERT OR REPLACE INTO prop(kind,ref,key,value,unit) VALUES ('mesh',?1,?2,?3,?4)",
            params![id, key, v, unit],
        )
        .unwrap();
}

fn get(ed: &Editor, kind: &str, id: i64, key: &str) -> Option<Sql> {
    ed.model
        .props(kind, id)
        .unwrap()
        .into_iter()
        .find(|p| p.0 == key)
        .map(|p| p.1)
}

fn ops(ed: &Editor) -> i64 {
    ed.model
        .sql()
        .query_row("SELECT count(*) FROM op", [], |r| r.get(0))
        .unwrap()
}

fn select(ed: &mut Editor, ids: &[i64]) -> Vec<Item> {
    ed.selection = ids.iter().map(|&id| Sel::Body(BodyKind::Mesh, id)).collect();
    selected(ed)
}

fn row<'a>(rows: &'a [Row], key: &str) -> &'a Row {
    rows.iter().find(|r| r.key == key).unwrap()
}

fn real(v: Option<Sql>) -> f64 {
    match v {
        Some(Sql::Real(x)) => x,
        other => panic!("not a real: {other:?}"),
    }
}

#[test]
fn edit_imported_prop_one_undo() {
    let mut ed = Editor::new_in_memory().unwrap();
    let a = mesh(&mut ed);
    put(&ed, a, "Pset_A.Fire", Sql::Text("EI 30".into()), "");
    put(&ed, a, "Pset_A.Width", Sql::Real(0.25), "m");
    let items = select(&mut ed, &[a]);
    assert_eq!(items, vec![("mesh", a)]);
    let rows = rows_of(&ed.model, &items, Units::Metric);
    let fire = row(&rows, "Pset_A.Fire").clone();
    assert_eq!((fire.value.as_deref(), fire.ty), (Some("EI 30"), Ty::Text));

    let n0 = ops(&ed);
    assert_eq!(commit(&mut ed, &items, &fire, "EI 90"), Ok(true));
    assert_eq!(ops(&ed), n0 + 1, "one journal entry");
    assert_eq!(get(&ed, "mesh", a, "Pset_A.Fire"), Some(Sql::Text("EI 90".into())));
    ed.undo();
    assert_eq!(get(&ed, "mesh", a, "Pset_A.Fire"), Some(Sql::Text("EI 30".into())));

    // the same text again is no edit at all
    let n1 = ops(&ed);
    assert_eq!(commit(&mut ed, &items, &fire, "EI 30"), Ok(false));
    assert_eq!(ops(&ed), n1);

    // a length row is typed in display units (mm) and stored in metres, unit kept
    let width = row(&rows, "Pset_A.Width").clone();
    assert_eq!(
        (width.value.as_deref(), width.label.as_str(), width.ty),
        (Some("250"), units::unit_label(Units::Metric), Ty::Real)
    );
    assert_eq!(commit(&mut ed, &items, &width, "300"), Ok(true));
    assert!((real(get(&ed, "mesh", a, "Pset_A.Width")) - 0.3).abs() < 1e-12);
    let unit: String = ed
        .model
        .sql()
        .query_row(
            "SELECT unit FROM prop WHERE kind='mesh' AND ref=?1 AND key='Pset_A.Width'",
            [a],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(unit, "m");
    ed.undo();
    assert_eq!(real(get(&ed, "mesh", a, "Pset_A.Width")), 0.25);
}

#[test]
fn three_bodies_same_key_one_undo() {
    let mut ed = Editor::new_in_memory().unwrap();
    let ids: Vec<i64> = (0..3).map(|_| mesh(&mut ed)).collect();
    for &id in &ids {
        put(&ed, id, "Pset_A.Load", Sql::Real(1.5), "kg");
    }
    let items = select(&mut ed, &ids);
    assert_eq!(items.len(), 3);
    let rows = rows_of(&ed.model, &items, Units::Metric);
    let load = row(&rows, "Pset_A.Load").clone();
    assert_eq!(
        (load.value.as_deref(), load.label.as_str(), load.ty),
        (Some("1,5"), "kg", Ty::Real)
    );
    let n0 = ops(&ed);
    assert_eq!(commit(&mut ed, &items, &load, "2,5 kg"), Ok(true));
    assert_eq!(ops(&ed), n0 + 1, "three elements, one journal entry");
    for &id in &ids {
        assert_eq!(real(get(&ed, "mesh", id, "Pset_A.Load")), 2.5);
    }
    ed.undo();
    for &id in &ids {
        assert_eq!(real(get(&ed, "mesh", id, "Pset_A.Load")), 1.5, "one undo reverts all");
    }
}

#[test]
fn mixed_values_show_varies() {
    let p = |k: &str, v: Sql, u: &str| (k.to_string(), v, u.to_string());
    let a = vec![
        p("Pset_A.Fire", Sql::Text("A".into()), ""),
        p("Pset_A.Grade", Sql::Text("X".into()), ""),
        p("Pset_A.Only", Sql::Text("o".into()), ""),
        p("Q.Len", Sql::Real(1.0), "m"),
    ];
    let b = vec![
        p("Pset_A.Fire", Sql::Text("B".into()), ""),
        p("Pset_A.Grade", Sql::Text("X".into()), ""),
        p("Q.Len", Sql::Real(1.0), "ft"),
    ];
    let rows = common_rows(&[a, b], Units::Metric);
    let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(keys, ["Pset_A.Fire", "Pset_A.Grade", "Q.Len"], "only shared keys");
    assert_eq!(row(&rows, "Pset_A.Fire").value, None, "differing values: <varies>");
    assert_eq!(row(&rows, "Pset_A.Grade").value.as_deref(), Some("X"));
    assert_eq!(row(&rows, "Q.Len").unit, None, "differing units: read-only");
    assert!(!tr("imported_props.varies", &[]).is_empty());

    // through the editor: an empty <varies> field writes nothing, a value writes both at once
    let mut ed = Editor::new_in_memory().unwrap();
    let (x, y) = (mesh(&mut ed), mesh(&mut ed));
    put(&ed, x, "Pset_A.Fire", Sql::Text("A".into()), "");
    put(&ed, y, "Pset_A.Fire", Sql::Text("B".into()), "");
    let items = select(&mut ed, &[x, y]);
    let fire = row(&rows_of(&ed.model, &items, Units::Metric), "Pset_A.Fire").clone();
    assert_eq!(fire.value, None);
    let n0 = ops(&ed);
    assert_eq!(commit(&mut ed, &items, &fire, ""), Ok(false));
    assert_eq!(ops(&ed), n0);
    assert_eq!(commit(&mut ed, &items, &fire, "EI 60"), Ok(true));
    assert_eq!(ops(&ed), n0 + 1);
    let fire = row(&rows_of(&ed.model, &items, Units::Metric), "Pset_A.Fire").clone();
    assert_eq!(fire.value.as_deref(), Some("EI 60"));
    ed.undo();
    assert_eq!(get(&ed, "mesh", x, "Pset_A.Fire"), Some(Sql::Text("A".into())));
    assert_eq!(get(&ed, "mesh", y, "Pset_A.Fire"), Some(Sql::Text("B".into())));
}

#[test]
fn sixty_props_all_listed() {
    let list: Vec<Prop> = (0..60)
        .map(|i| (format!("Pset_B.P{i:02}"), Sql::Integer(i), String::new()))
        .collect();
    let rows = common_rows(&[list], Units::Metric);
    assert_eq!(rows.len(), 60);
    assert!(rows.len() > FILTER_AT, "the filter box shows");
    let all = filter_rows(&rows, "");
    assert_eq!(all.len(), 60, "nothing hidden");
    let keyed: Vec<(String, usize)> = all.iter().map(|&i| (rows[i].key.clone(), i)).collect();
    let groups = prop_rows::group_props(&keyed);
    assert_eq!(groups.len(), 1);
    assert_eq!((groups[0].0.as_str(), groups[0].1.len()), ("Pset_B", 60));
    assert_eq!(filter_rows(&rows, " p05 "), vec![5]);
    assert_eq!(filter_rows(&rows, "59").len(), 1, "value or key");
    assert_eq!(row(&rows, "Pset_B.P59").ty, Ty::Int);
}

/// A wall with an own property set (millimetre file).
const FIXTURE: &str = "ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [CoordinationView]'),'2;1');
FILE_NAME('f.ifc','2020-01-01T00:00:00',(''),(''),'','','');
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
#41=IFCWALLSTANDARDCASE('0000000000000000000003',#4,'W',$,$,#24,#40,$,$);
#50=IFCMATERIAL('Brick');
#52=IFCMATERIALLAYER(#50,250.,$);
#54=IFCMATERIALLAYERSET((#52),'Ext 250');
#55=IFCMATERIALLAYERSETUSAGE(#54,.AXIS2.,.POSITIVE.,-125.);
#56=IFCRELASSOCIATESMATERIAL('0000000000000000000004',#4,$,$,(#41),#55);
#57=IFCRELCONTAINEDINSPATIALSTRUCTURE('0000000000000000000005',#4,$,$,(#41),#16);
#100=IFCPROPERTYSINGLEVALUE('Fire',$,IFCLABEL('EI 30'),$);
#101=IFCPROPERTYSINGLEVALUE('Thick',$,IFCLENGTHMEASURE(250.),$);
#102=IFCPROPERTYSET('0000000000000000000100',#4,'Own',$,(#100,#101));
#103=IFCRELDEFINESBYPROPERTIES('0000000000000000000101',#4,$,$,(#41),#102);
ENDSEC;
END-ISO-10303-21;
";

#[test]
fn add_prop_journals_p_field_for_patch() {
    let dir = std::env::temp_dir().join(format!("ddc-ifp1-add-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (src, out) = (dir.join("a.ifc"), dir.join("out.ifc"));
    std::fs::write(&src, FIXTURE).unwrap();
    let db = dir.join("w.ddc");
    let _ = std::fs::remove_file(&db);
    let mut m = Model::open(db.to_str().unwrap()).unwrap();
    ddc_io::ifc::import_ifc_file(&mut m, &src).unwrap();
    let key = m.sources().unwrap().remove(0).source;
    let mut ed = Editor::new_in_memory().unwrap();
    ed.replace_model(m).unwrap();
    let (kind, id) = ed
        .model
        .element_by_guid(&key, "0000000000000000000003")
        .unwrap()
        .unwrap();
    assert_eq!(kind, "wall");
    let items: Vec<Item> = vec![("wall", id)];

    let n0 = ops(&ed);
    assert_eq!(
        add_prop(&mut ed, &items, " Own ", " Note ", "checked"),
        Ok("Own.Note".to_string())
    );
    assert_eq!(ops(&ed), n0 + 1, "one undo step");
    assert_eq!(get(&ed, "wall", id, "Own.Note"), Some(Sql::Text("checked".into())));
    let fields: Vec<String> = ed
        .model
        .local_changes(&key, false)
        .unwrap()
        .into_iter()
        .filter(|c| c.id == id)
        .flat_map(|c| c.fields.into_iter().map(|f| f.field))
        .collect();
    assert!(fields.iter().any(|f| f == "p:Own.Note"), "{fields:?}");

    // an edited existing row travels the same way
    let rows = rows_of(&ed.model, &items, Units::Metric);
    assert_eq!(commit(&mut ed, &items, row(&rows, "Own.Fire"), "EI 90"), Ok(true));

    ddc_io::ifc::patch::patch_file(&ed.model, &key, &src, &out, None).unwrap();
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.contains("IFCPROPERTYSINGLEVALUE('Fire',$,IFCLABEL('EI 90'),$)"));
    assert!(text.contains("'Note'") && text.contains("('checked')"), "{text}");

    // numbers become numbers, a bare name has no set
    assert_eq!(new_prop("", "Mark", "12", RU), Ok(("Mark".into(), Sql::Integer(12))));
    assert_eq!(new_prop("S", "Load", "1,5", RU), Ok(("S.Load".into(), Sql::Real(1.5))));
}

#[test]
fn bad_number_refused_nothing_written() {
    let mut ed = Editor::new_in_memory().unwrap();
    let a = mesh(&mut ed);
    put(&ed, a, "Pset_A.Load", Sql::Real(1.5), "kg");
    put(&ed, a, "Pset_A.Width", Sql::Real(0.25), "m");
    put(&ed, a, "Pset_A.Count", Sql::Integer(4), "");
    let items = select(&mut ed, &[a]);
    let rows = rows_of(&ed.model, &items, Units::Metric);
    let n0 = ops(&ed);
    for (key, text) in [
        ("Pset_A.Load", "abc"),
        ("Pset_A.Load", "1e999"),
        ("Pset_A.Load", "inf"),
        ("Pset_A.Load", "NaN"),
        ("Pset_A.Load", "  "),
        ("Pset_A.Width", "12 parsecs"),
        ("Pset_A.Width", "1e999"),
        ("Pset_A.Count", "four"),
    ] {
        let r = commit(&mut ed, &items, row(&rows, key), text);
        assert!(matches!(r, Err(Refuse::Number(_))), "{key} {text:?}: {r:?}");
    }
    // no name, or a non-finite number in «Add property»
    assert_eq!(add_prop(&mut ed, &items, "Pset_A", "  ", "1"), Err(Refuse::Name));
    assert!(matches!(
        add_prop(&mut ed, &items, "Pset_A", "X", "1e400"),
        Err(Refuse::Number(_))
    ));
    assert!(matches!(
        add_prop(&mut ed, &items, "Pset_A", "X", "-inf"),
        Err(Refuse::Number(_))
    ));
    assert_eq!(ops(&ed), n0, "nothing journaled");
    assert_eq!(real(get(&ed, "mesh", a, "Pset_A.Load")), 1.5);
    assert_eq!(real(get(&ed, "mesh", a, "Pset_A.Width")), 0.25);
    assert_eq!(get(&ed, "mesh", a, "Pset_A.Count"), Some(Sql::Integer(4)));
    assert_eq!(get(&ed, "mesh", a, "Pset_A.X"), None);
}

#[test]
fn remove_prop_one_undo() {
    let mut ed = Editor::new_in_memory().unwrap();
    let ids: Vec<i64> = (0..3).map(|_| mesh(&mut ed)).collect();
    for &id in &ids {
        put(&ed, id, "Pset_A.Fire", Sql::Text("EI 30".into()), "");
        put(&ed, id, "Pset_A.Width", Sql::Real(0.25), "m");
    }
    let items = select(&mut ed, &ids);
    let n0 = ops(&ed);
    assert_eq!(remove_prop(&mut ed, &items, "Pset_A.Fire"), Ok(()));
    assert_eq!(ops(&ed), n0 + 1, "three elements, one journal entry");
    for &id in &ids {
        assert_eq!(get(&ed, "mesh", id, "Pset_A.Fire"), None);
        assert!(get(&ed, "mesh", id, "Pset_A.Width").is_some(), "other rows stay");
    }
    ed.undo();
    for &id in &ids {
        assert_eq!(get(&ed, "mesh", id, "Pset_A.Fire"), Some(Sql::Text("EI 30".into())));
        assert_eq!(real(get(&ed, "mesh", id, "Pset_A.Width")), 0.25);
    }
    // a key nobody has writes nothing
    let n1 = ops(&ed);
    assert!(matches!(
        remove_prop(&mut ed, &items, "Pset_A.Nope"),
        Err(Refuse::Failed(_))
    ));
    assert_eq!(ops(&ed), n1);
}

#[test]
fn derived_rows_not_removable() {
    let mut ed = Editor::new_in_memory().unwrap();
    let a = mesh(&mut ed);
    let keys = ["IFC.Class", "Material.Name", "type.Name"];
    for k in keys {
        put(&ed, a, k, Sql::Text("x".into()), "");
        assert!(!removable(k), "{k}");
    }
    assert!(removable("Pset_A.Fire") && removable("Mark") && removable("Classification.Code"));
    let items = select(&mut ed, &[a]);
    let n0 = ops(&ed);
    for k in keys {
        assert_eq!(remove_prop(&mut ed, &items, k), Err(Refuse::Locked), "{k}");
        assert_eq!(get(&ed, "mesh", a, k), Some(Sql::Text("x".into())), "{k}");
    }
    assert_eq!(ops(&ed), n0);
}

#[test]
fn remove_then_undo_saves_nothing() {
    let dir = std::env::temp_dir().join(format!("ddc-ifp3-undo-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (src, out) = (dir.join("a.ifc"), dir.join("out.ifc"));
    std::fs::write(&src, FIXTURE).unwrap();
    let db = dir.join("w.ddc");
    let _ = std::fs::remove_file(&db);
    let mut m = Model::open(db.to_str().unwrap()).unwrap();
    ddc_io::ifc::import_ifc_file(&mut m, &src).unwrap();
    let key = m.sources().unwrap().remove(0).source;
    let mut ed = Editor::new_in_memory().unwrap();
    ed.replace_model(m).unwrap();
    let (_, id) = ed
        .model
        .element_by_guid(&key, "0000000000000000000003")
        .unwrap()
        .unwrap();
    let items: Vec<Item> = vec![("wall", id)];

    assert_eq!(remove_prop(&mut ed, &items, "Own.Fire"), Ok(()));
    assert!(ed.model.is_edited("wall", id).unwrap());
    ed.undo();
    assert_eq!(get(&ed, "wall", id, "Own.Fire"), Some(Sql::Text("EI 30".into())));
    assert!(!ed.model.is_edited("wall", id).unwrap(), "back at the base");
    let p = ddc_io::ifc::patch::patch_file(&ed.model, &key, &src, &out, None).unwrap();
    assert!(p.no_changes);

    // a removal reaches the file; the other property of the set stays
    assert_eq!(remove_prop(&mut ed, &items, "Own.Fire"), Ok(()));
    ddc_io::ifc::patch::patch_file(&ed.model, &key, &src, &out, None).unwrap();
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(!text.contains("'Fire'"), "{text}");
    assert!(text.contains("'Thick'"), "{text}");
}

/// A view row of an unknown kind: the step commits, the reload that follows fails.
fn break_reload(ed: &Editor) {
    ed.model
        .sql()
        .execute(
            "INSERT INTO view_def(kind,name,ax,ay,bx,by,depth,far_clip) VALUES ('bogus','x',0,0,1,0,1,0)",
            [],
        )
        .unwrap();
    assert!(ed.model.view_defs().is_err());
}

#[test]
fn remove_committed_reload_fail_is_warning() {
    let mut ed = Editor::new_in_memory().unwrap();
    let a = mesh(&mut ed);
    put(&ed, a, "Pset_A.Fire", Sql::Text("EI 30".into()), "");
    put(&ed, a, "Pset_A.Width", Sql::Real(0.25), "m");
    let items = select(&mut ed, &[a]);
    break_reload(&ed);
    let n0 = ops(&ed);
    assert_eq!(remove_prop(&mut ed, &items, "Pset_A.Fire"), Ok(()));
    assert_eq!(ops(&ed), n0 + 1, "the step is committed");
    assert_eq!(get(&ed, "mesh", a, "Pset_A.Fire"), None);
    assert!(get(&ed, "mesh", a, "Pset_A.Width").is_some());
    assert_eq!(ed.status, Some(tr("imported_prop_edit.warn.reload", &[])));
    ed.undo();
    assert_eq!(get(&ed, "mesh", a, "Pset_A.Fire"), Some(Sql::Text("EI 30".into())));
    // nothing committed (key nobody has): still a failure
    let n1 = ops(&ed);
    assert!(matches!(
        remove_prop(&mut ed, &items, "Pset_A.Nope"),
        Err(Refuse::Failed(_))
    ));
    assert_eq!(ops(&ed), n1);
}

#[test]
fn set_all_committed_reload_fail_is_warning() {
    let mut ed = Editor::new_in_memory().unwrap();
    let ids: Vec<i64> = (0..2).map(|_| mesh(&mut ed)).collect();
    for &id in &ids {
        put(&ed, id, "Pset_A.Load", Sql::Real(1.5), "kg");
    }
    let items = select(&mut ed, &ids);
    break_reload(&ed);
    let n0 = ops(&ed);
    assert_eq!(set_all(&mut ed, &items, "Pset_A.Load", Sql::Real(4.0), "kg"), Ok(()));
    assert_eq!(ops(&ed), n0 + 1);
    for &id in &ids {
        assert_eq!(real(get(&ed, "mesh", id, "Pset_A.Load")), 4.0);
    }
    assert_eq!(ed.status, Some(tr("imported_prop_edit.warn.reload", &[])));
    // add_prop keeps the warning instead of the plain «done» text
    ed.status = None;
    assert!(add_prop(&mut ed, &items, "Pset_A", "Note", "x").is_ok());
    assert_eq!(ed.status, Some(tr("imported_prop_edit.warn.reload", &[])));
}

#[test]
fn readonly_row_has_remove_menu() {
    let mut ed = Editor::new_in_memory().unwrap();
    let ids: Vec<i64> = (0..2).map(|_| mesh(&mut ed)).collect();
    put(&ed, ids[0], "Q.Len", Sql::Real(1.0), "m");
    put(&ed, ids[1], "Q.Len", Sql::Real(1.0), "ft");
    for &id in &ids {
        put(&ed, id, "IFC.Class", Sql::Text("IfcWall".into()), "");
    }
    let items = select(&mut ed, &ids);
    let rows = rows_of(&ed.model, &items, Units::Metric);
    let len = row(&rows, "Q.Len");
    assert_eq!(len.unit, None, "differing units: value read-only");
    assert!(row_removable(len), "but the row can be removed");
    assert!(!row_removable(row(&rows, "IFC.Class")), "derived rows stay locked");
    assert_eq!(remove_prop(&mut ed, &items, "Q.Len"), Ok(()));
    for &id in &ids {
        assert_eq!(get(&ed, "mesh", id, "Q.Len"), None);
    }
}

/// Typed numbers follow the mark passed in (never the thread's UI language): Russian `1,5` is
/// 1.5, English `1,250` is 1250, English `1,5` is refused; stored values are plain numbers.
#[test]
fn typed_numbers_follow_the_ui_mark() {
    let mut ed = Editor::new_in_memory().unwrap();
    let a = mesh(&mut ed);
    put(&ed, a, "Pset_A.Load", Sql::Real(2.0), "kg");
    put(&ed, a, "Pset_A.Width", Sql::Real(0.25), "m");
    let items = select(&mut ed, &[a]);
    let rows = rows_of_in(&ed.model, &items, Units::Metric, EN);
    let (load, width) = (row(&rows, "Pset_A.Load").clone(), row(&rows, "Pset_A.Width").clone());

    let n0 = ops(&ed);
    assert_eq!(commit_in(&mut ed, &items, &load, "1,5", RU), Ok(true));
    assert_eq!(get(&ed, "mesh", a, "Pset_A.Load"), Some(Sql::Real(1.5)));
    assert_eq!(ops(&ed), n0 + 1, "one undo step");
    ed.undo();
    assert_eq!(get(&ed, "mesh", a, "Pset_A.Load"), Some(Sql::Real(2.0)));

    let n1 = ops(&ed);
    assert_eq!(commit_in(&mut ed, &items, &load, "1,250", EN), Ok(true));
    assert_eq!(get(&ed, "mesh", a, "Pset_A.Load"), Some(Sql::Real(1250.0)));
    assert_eq!(ops(&ed), n1 + 1, "one undo step");
    ed.undo();
    assert_eq!(get(&ed, "mesh", a, "Pset_A.Load"), Some(Sql::Real(2.0)));

    // lengths: bare numbers are mm in metric display units
    assert_eq!(commit_in(&mut ed, &items, &width, "1,250", EN), Ok(true));
    assert!((real(get(&ed, "mesh", a, "Pset_A.Width")) - 1.25).abs() < 1e-12);
    ed.undo();
    assert_eq!(commit_in(&mut ed, &items, &width, "1,5", RU), Ok(true));
    assert!((real(get(&ed, "mesh", a, "Pset_A.Width")) - 0.0015).abs() < 1e-15);
    ed.undo();
    assert_eq!(real(get(&ed, "mesh", a, "Pset_A.Width")), 0.25);

    // ambiguous English comma and invalid text: refused, model and journal unchanged
    let n2 = ops(&ed);
    assert_eq!(commit_in(&mut ed, &items, &load, "1,5", EN), Err(Refuse::Comma("1,5".into())));
    assert_eq!(
        commit_in(&mut ed, &items, &width, " 1,5 mm ", EN),
        Err(Refuse::Comma("1,5 mm".into()))
    );
    for text in ["abc", "1,2,3", "1.2.3", "1e999", "inf", ""] {
        let r = commit_in(&mut ed, &items, &load, text, EN);
        assert!(matches!(r, Err(Refuse::Number(_)) | Ok(false)), "{text:?}: {r:?}");
    }
    assert!(matches!(
        commit_in(&mut ed, &items, &load, "12,3,4", RU),
        Err(Refuse::Number(_))
    ));
    assert!(matches!(
        add_prop_in(&mut ed, &items, "Pset_A", "X", "1,5", EN),
        Err(Refuse::Comma(_))
    ));
    assert_eq!(ops(&ed), n2, "nothing journaled");
    assert_eq!(get(&ed, "mesh", a, "Pset_A.Load"), Some(Sql::Real(2.0)));
    assert_eq!(real(get(&ed, "mesh", a, "Pset_A.Width")), 0.25);
    assert_eq!(get(&ed, "mesh", a, "Pset_A.X"), None);

    // new properties: the same rules, integers stay integers
    assert_eq!(new_prop("S", "N", "1,250", EN), Ok(("S.N".into(), Sql::Integer(1250))));
    assert_eq!(new_prop("S", "N", "1,250", RU), Ok(("S.N".into(), Sql::Real(1.25))));
    assert_eq!(new_prop("S", "N", "1 250", RU), Ok(("S.N".into(), Sql::Integer(1250))));
    assert_eq!(new_prop("S", "N", "EI 1,5", EN), Ok(("S.N".into(), Sql::Text("EI 1,5".into()))));
    assert_eq!(new_prop("S", "N", "1,5", EN), Err(Refuse::Comma("1,5".into())));

    // the refusal reads in both languages without switching the UI language
    for ru in [false, true] {
        let t = crate::tr_lang(ru, "decimal_input.err.comma", &[("text", "1,5")]);
        assert!(t.contains("1,5") && !t.contains("decimal_input."), "{ru}: {t}");
    }
}

/// Shown text uses the same mark, so an untouched field reads back as no edit.
#[test]
fn shown_numbers_read_back_with_the_same_mark() {
    let mut ed = Editor::new_in_memory().unwrap();
    let a = mesh(&mut ed);
    put(&ed, a, "Pset_A.Load", Sql::Real(1250.5), "kg");
    let items = select(&mut ed, &[a]);
    for (mark, shown) in [(EN, "1,250.5"), (RU, "1\u{a0}250,5")] {
        let rows = rows_of_in(&ed.model, &items, Units::Metric, mark);
        let load = row(&rows, "Pset_A.Load").clone();
        assert_eq!(load.value.as_deref(), Some(shown));
        assert_eq!(parse_value(shown, load.ty, "kg", Units::Metric, mark), Ok(Sql::Real(1250.5)));
        let n = ops(&ed);
        assert_eq!(commit_in(&mut ed, &items, &load, shown, mark), Ok(false));
        assert_eq!(ops(&ed), n);
    }
}

/// Two plain walls: their props reach the IFC export.
fn two_walls(ed: &mut Editor) -> (i64, i64) {
    let lv = ed.level;
    let layer = ddc_model::LayerSpec {
        material: "brick".into(),
        thickness: 0.2,
    };
    let t = ed.model.add_wall_type("xs48 test", &[layer]).unwrap();
    let mut wall = |y: f64| {
        ed.model
            .add_wall(&ddc_model::WallSpec {
                a: (0.0, y),
                b: (6.0, y),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap()
    };
    (wall(0.0), wall(5.0))
}

fn put_on(ed: &mut Editor, kind: &str, id: i64, key: &str, v: Sql, unit: &str) {
    ed.model.set_prop(kind, id, key, Some((v, unit))).unwrap();
}

fn unit_of(ed: &Editor, kind: &str, id: i64, key: &str) -> String {
    ed.model
        .props(kind, id)
        .unwrap()
        .into_iter()
        .find(|p| p.0 == key)
        .unwrap()
        .2
}

#[test]
fn mixed_text_and_number_keep_own_types() {
    let mut ed = Editor::new_in_memory().unwrap();
    let (a, b) = two_walls(&mut ed);
    put_on(&mut ed, "wall", a, "Pset_X.Len", Sql::Real(3.5), "m");
    put_on(&mut ed, "wall", b, "Pset_X.Len", Sql::Text("n/a".into()), "m");
    put_on(&mut ed, "wall", a, "Pset_X.Count", Sql::Integer(4), "");
    put_on(&mut ed, "wall", b, "Pset_X.Count", Sql::Text("many".into()), "");
    let items: Vec<Item> = vec![("wall", a), ("wall", b)];
    let rows = rows_of_in(&ed.model, &items, Units::Metric, EN);
    let len = row(&rows, "Pset_X.Len").clone();
    assert_eq!((len.ty, len.value.as_deref(), len.unit.as_deref()), (Ty::Mixed, None, Some("m")));

    // the number goes in by the length unit, the text stays text: nothing turns into text
    let n0 = ops(&ed);
    assert_eq!(commit_in(&mut ed, &items, &len, "4200", EN), Ok(true));
    assert_eq!(ops(&ed), n0 + 1, "two elements, one undo step");
    assert!((real(get(&ed, "wall", a, "Pset_X.Len")) - 4.2).abs() < 1e-12);
    assert_eq!(unit_of(&ed, "wall", a, "Pset_X.Len"), "m");
    assert_eq!(get(&ed, "wall", b, "Pset_X.Len"), Some(Sql::Text("4200".into())));

    // the IFC export keeps the measure type of the number
    let t = ddc_io::ifc::export_ifc(&ed.model).unwrap();
    assert!(t.contains("IFCLENGTHMEASURE(4.2"), "{t}");
    assert!(t.contains("IFCTEXT('4200')"), "{t}");

    // one undo reverts both elements to their own values and types
    ed.undo();
    assert_eq!(real(get(&ed, "wall", a, "Pset_X.Len")), 3.5);
    assert_eq!(get(&ed, "wall", b, "Pset_X.Len"), Some(Sql::Text("n/a".into())));

    // an integer stays an integer next to a text
    let count = row(&rows, "Pset_X.Count").clone();
    assert_eq!(count.ty, Ty::Mixed);
    assert_eq!(commit_in(&mut ed, &items, &count, "7", EN), Ok(true));
    assert_eq!(get(&ed, "wall", a, "Pset_X.Count"), Some(Sql::Integer(7)));
    assert_eq!(get(&ed, "wall", b, "Pset_X.Count"), Some(Sql::Text("7".into())));
    ed.undo();

    // a text the number element cannot read is refused for all: nothing written anywhere
    let n1 = ops(&ed);
    for text in ["abc", "1e999", "12 parsecs"] {
        let r = commit_in(&mut ed, &items, &len, text, EN);
        assert_eq!(r, Err(Refuse::Number(text.to_string())), "{text:?}");
        assert!(!r.unwrap_err().text().is_empty());
    }
    assert_eq!(
        commit_in(&mut ed, &items, &count, "seven", EN),
        Err(Refuse::Number("seven".into()))
    );
    assert_eq!(ops(&ed), n1, "nothing journaled");
    assert_eq!(real(get(&ed, "wall", a, "Pset_X.Len")), 3.5);
    assert_eq!(get(&ed, "wall", b, "Pset_X.Len"), Some(Sql::Text("n/a".into())));
    assert_eq!(get(&ed, "wall", a, "Pset_X.Count"), Some(Sql::Integer(4)));
    assert_eq!(get(&ed, "wall", b, "Pset_X.Count"), Some(Sql::Text("many".into())));
}

#[test]
fn all_numeric_selection_keeps_each_number_type() {
    let mut ed = Editor::new_in_memory().unwrap();
    let (x, y) = (mesh(&mut ed), mesh(&mut ed));
    put(&ed, x, "Pset_A.Load", Sql::Real(1.5), "kg");
    put(&ed, y, "Pset_A.Load", Sql::Integer(2), "kg");
    let items = select(&mut ed, &[x, y]);
    let load = row(&rows_of_in(&ed.model, &items, Units::Metric, EN), "Pset_A.Load").clone();
    assert_eq!((load.ty, load.value.as_deref()), (Ty::Real, None));
    let n0 = ops(&ed);
    assert_eq!(commit_in(&mut ed, &items, &load, "3 kg", EN), Ok(true));
    assert_eq!(ops(&ed), n0 + 1, "one undo step");
    assert_eq!(get(&ed, "mesh", x, "Pset_A.Load"), Some(Sql::Real(3.0)));
    assert_eq!(get(&ed, "mesh", y, "Pset_A.Load"), Some(Sql::Integer(3)));
    ed.undo();
    assert_eq!(get(&ed, "mesh", x, "Pset_A.Load"), Some(Sql::Real(1.5)));
    assert_eq!(get(&ed, "mesh", y, "Pset_A.Load"), Some(Sql::Integer(2)));
}

const PANEL: &str = "ddc.ifp1.props";

/// One frame of the panel; returns the cached rows it kept.
fn frame(ctx: &egui::Context, ed: &mut Editor) -> Arc<Vec<Row>> {
    let th = Theme::new(ddc_ui::Mode::Dark, ddc_ui::Density::Compact);
    let _ = ctx.run(egui::RawInput::default(), |c| {
        egui::CentralPanel::default().show(c, |ui| show(ui, &th, ed));
    });
    let st: State = ctx.data_mut(|d| d.get_temp(egui::Id::new(PANEL))).unwrap();
    st.rows
}

fn panel_ctx() -> egui::Context {
    let ctx = egui::Context::default();
    ddc_ui::theme::install_fonts(&ctx);
    let _ = ctx.run(Default::default(), |_| {});
    ctx
}

#[test]
fn rows_read_anew_when_an_older_revision_is_put_back() {
    let (ctx, mut ed) = (panel_ctx(), Editor::new_in_memory().unwrap());
    let a = mesh(&mut ed);
    put(&ed, a, "Pset_A.Fire", Sql::Text("EI 30".into()), "");
    let items = select(&mut ed, &[a]);
    let fire = row(&frame(&ctx, &mut ed), "Pset_A.Fire").clone();
    assert_eq!(fire.value.as_deref(), Some("EI 30"));
    let rev = ed.revision;
    assert_eq!(commit(&mut ed, &items, &fire, "EI 90"), Ok(true));
    ed.restore_revision(rev);
    let rows = frame(&ctx, &mut ed);
    assert_eq!(row(&rows, "Pset_A.Fire").value.as_deref(), Some("EI 90"), "keyed on cache_key");
}

#[test]
fn rows_cache_survives_an_unrelated_frame() {
    let (ctx, mut ed) = (panel_ctx(), Editor::new_in_memory().unwrap());
    let a = mesh(&mut ed);
    put(&ed, a, "Pset_A.Fire", Sql::Text("EI 30".into()), "");
    let _ = select(&mut ed, &[a]);
    let _ = frame(&ctx, &mut ed);
    let id = egui::Id::new(PANEL);
    ctx.data_mut(|d| d.get_temp_mut_or_default::<State>(id).rows = Arc::new(Vec::new()));
    assert!(frame(&ctx, &mut ed).is_empty(), "no reread without a model change");
}
