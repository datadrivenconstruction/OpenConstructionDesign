//! Hand-written fixtures: every product with geometry imports (natively or as a mesh), its
//! data lands in `prop`, re-import updates by GlobalId.

use super::*;
use std::fmt::Write as _;

const HEAD: &str = "#1=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);
#2=IFCSIUNIT(*,.PLANEANGLEUNIT.,$,.RADIAN.);
#3=IFCDIMENSIONALEXPONENTS(0,0,0,0,0,0,0);
#4=IFCMEASUREWITHUNIT(IFCPLANEANGLEMEASURE(0.017453292519943295),#2);
#5=IFCCONVERSIONBASEDUNIT(#3,.PLANEANGLEUNIT.,'DEGREE',#4);
#6=IFCUNITASSIGNMENT((#1,#5));
#7=IFCPROJECT('0000000000000000000P01',$,'P',$,$,$,$,(#13),#6);
#8=IFCCARTESIANPOINT((0.,0.,0.));
#9=IFCAXIS2PLACEMENT3D(#8,$,$);
#10=IFCLOCALPLACEMENT($,#9);
#11=IFCBUILDINGSTOREY('0000000000000000000S01',$,'L1',$,$,#10,$,$,.ELEMENT.,0.);
#12=IFCDIRECTION((0.,1.));
#13=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.E-05,#9,#12);
";

fn r(v: f64) -> String {
    let s = format!("{v:?}");
    if s.contains('.') || s.contains('e') {
        s
    } else {
        format!("{s}.")
    }
}

fn pt(p: &[f64]) -> String {
    p.iter().map(|v| r(*v)).collect::<Vec<_>>().join(",")
}

struct B {
    s: String,
    n: u32,
    g: u32,
    contained: Vec<u32>,
}

impl B {
    fn new() -> B {
        B {
            s: String::new(),
            n: 100,
            g: 0,
            contained: vec![],
        }
    }
    fn add(&mut self, e: &str) -> u32 {
        self.n += 1;
        writeln!(self.s, "#{}={};", self.n, e).unwrap();
        self.n
    }
    fn guid(&mut self) -> String {
        self.g += 1;
        format!("'{:0>22}'", self.g)
    }
    fn point(&mut self, p: &[f64]) -> u32 {
        self.add(&format!("IFCCARTESIANPOINT(({}))", pt(p)))
    }
    fn dir(&mut self, d: &[f64]) -> u32 {
        self.add(&format!("IFCDIRECTION(({}))", pt(d)))
    }
    fn axis3(&mut self, o: [f64; 3], z: Option<[f64; 3]>, x: Option<[f64; 3]>) -> u32 {
        let p = self.point(&o);
        let z = z.map_or("$".into(), |z| format!("#{}", self.dir(&z)));
        let x = x.map_or("$".into(), |x| format!("#{}", self.dir(&x)));
        self.add(&format!("IFCAXIS2PLACEMENT3D(#{p},{z},{x})"))
    }
    fn place(&mut self, o: [f64; 3]) -> u32 {
        let a = self.axis3(o, None, None);
        self.add(&format!("IFCLOCALPLACEMENT(#10,#{a})"))
    }
    fn pos2(&mut self, o: [f64; 2]) -> u32 {
        let p = self.point(&o);
        self.add(&format!("IFCAXIS2PLACEMENT2D(#{p},$)"))
    }
    fn rect(&mut self, c: [f64; 2], x: f64, y: f64) -> u32 {
        let p = self.pos2(c);
        self.add(&format!(
            "IFCRECTANGLEPROFILEDEF(.AREA.,$,#{p},{},{})",
            r(x),
            r(y)
        ))
    }
    fn polyline(&mut self, pts: &[&[f64]]) -> u32 {
        let ids: Vec<String> = pts.iter().map(|p| format!("#{}", self.point(p))).collect();
        self.add(&format!("IFCPOLYLINE(({}))", ids.join(",")))
    }
    fn extrude(&mut self, prof: u32, pos: u32, depth: f64) -> u32 {
        let d = self.dir(&[0.0, 0.0, 1.0]);
        self.add(&format!(
            "IFCEXTRUDEDAREASOLID(#{prof},#{pos},#{d},{})",
            r(depth)
        ))
    }
    fn rep(&mut self, ident: &str, kind: &str, items: &[u32]) -> u32 {
        let l: Vec<String> = items.iter().map(|i| format!("#{i}")).collect();
        self.add(&format!(
            "IFCSHAPEREPRESENTATION(#13,'{ident}','{kind}',({}))",
            l.join(",")
        ))
    }
    /// Product with the given representations; `tail` = arguments after Representation.
    fn product(&mut self, class: &str, name: &str, place: u32, reps: &[u32], tail: &str) -> u32 {
        let pds = if reps.is_empty() {
            "$".to_string()
        } else {
            let l: Vec<String> = reps.iter().map(|i| format!("#{i}")).collect();
            format!(
                "#{}",
                self.add(&format!("IFCPRODUCTDEFINITIONSHAPE($,$,({}))", l.join(",")))
            )
        };
        let g = self.guid();
        let id = self.add(&format!("{class}({g},$,'{name}',$,$,#{place},{pds}{tail})"));
        self.contained.push(id);
        id
    }
    fn body(&mut self, class: &str, name: &str, o: [f64; 3], items: &[u32], tail: &str) -> u32 {
        let pl = self.place(o);
        let rep = self.rep("Body", "Tessellation", items);
        self.product(class, name, pl, &[rep], tail)
    }
    fn cube_points(&mut self) -> u32 {
        self.add(
            "IFCCARTESIANPOINTLIST3D(((0.,0.,0.),(1.,0.,0.),(1.,1.,0.),(0.,1.,0.),(0.,0.,1.),(1.,0.,1.),(1.,1.,1.),(0.,1.,1.)))",
        )
    }
    fn tri_cube(&mut self) -> u32 {
        let p = self.cube_points();
        self.add(&format!(
            "IFCTRIANGULATEDFACESET(#{p},$,.T.,((1,3,2),(1,4,3),(5,6,7),(5,7,8),(1,2,6),(1,6,5),(2,3,7),(2,7,6),(3,4,8),(3,8,7),(4,1,5),(4,5,8)),$)"
        ))
    }
    fn cube_faces(&mut self) -> Vec<u32> {
        let c: Vec<u32> = [
            [0., 0., 0.],
            [1., 0., 0.],
            [1., 1., 0.],
            [0., 1., 0.],
            [0., 0., 1.],
            [1., 0., 1.],
            [1., 1., 1.],
            [0., 1., 1.],
        ]
        .iter()
        .map(|p| self.point(p))
        .collect();
        let quads = [
            [0, 3, 2, 1],
            [4, 5, 6, 7],
            [0, 1, 5, 4],
            [1, 2, 6, 5],
            [2, 3, 7, 6],
            [3, 0, 4, 7],
        ];
        quads
            .iter()
            .map(|q| {
                let l = self.add(&format!(
                    "IFCPOLYLOOP((#{},#{},#{},#{}))",
                    c[q[0]], c[q[1]], c[q[2]], c[q[3]]
                ));
                let b = self.add(&format!("IFCFACEOUTERBOUND(#{l},.T.)"));
                self.add(&format!("IFCFACE((#{b}))"))
            })
            .collect()
    }
    fn finish(&self, schema: &str) -> String {
        let l: Vec<String> = self.contained.iter().map(|i| format!("#{i}")).collect();
        format!(
            "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION(('ViewDefinition [x]'),'2;1');\nFILE_NAME('t.ifc','2026-01-01T00:00:00',(''),(''),'','','');\nFILE_SCHEMA(('{schema}'));\nENDSEC;\nDATA;\n{HEAD}{}#99=IFCRELCONTAINEDINSPATIALSTRUCTURE('0000000000000000000R99',$,$,$,({}),#11);\nENDSEC;\nEND-ISO-10303-21;\n",
            self.s,
            l.join(",")
        )
    }
}

fn plan_of(src: &str) -> Plan {
    plan_bytes(src.as_bytes()).unwrap()
}

fn count(m: &Model, t: &str) -> i64 {
    m.sql()
        .query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))
        .unwrap()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

/// Eight bodies of different geometry kinds, all furniture.
fn mesh_kinds() -> B {
    let mut b = B::new();
    let t = b.tri_cube();
    b.body("IFCFURNISHINGELEMENT", "tri", [0.0, 0.0, 0.0], &[t], ",$,$");
    let p = b.cube_points();
    let faces: Vec<String> = [
        [1, 4, 3, 2],
        [5, 6, 7, 8],
        [1, 2, 6, 5],
        [2, 3, 7, 6],
        [3, 4, 8, 7],
        [4, 1, 5, 8],
    ]
    .iter()
    .map(|q| {
        format!(
            "#{}",
            b.add(&format!(
                "IFCINDEXEDPOLYGONALFACE(({},{},{},{}))",
                q[0], q[1], q[2], q[3]
            ))
        )
    })
    .collect();
    let pf = b.add(&format!(
        "IFCPOLYGONALFACESET(#{p},.T.,({}),$)",
        faces.join(",")
    ));
    b.body(
        "IFCFURNISHINGELEMENT",
        "poly",
        [2.0, 0.0, 0.0],
        &[pf],
        ",$,$",
    );
    let fs = b.cube_faces();
    let l: Vec<String> = fs.iter().map(|f| format!("#{f}")).collect();
    let sh = b.add(&format!("IFCCLOSEDSHELL(({}))", l.join(",")));
    let br = b.add(&format!("IFCFACETEDBREP(#{sh})"));
    b.body(
        "IFCFURNISHINGELEMENT",
        "brep",
        [4.0, 0.0, 0.0],
        &[br],
        ",$,$",
    );
    let os = b.add(&format!("IFCOPENSHELL(({}))", l.join(",")));
    let sm = b.add(&format!("IFCSHELLBASEDSURFACEMODEL((#{os}))"));
    b.body(
        "IFCFURNISHINGELEMENT",
        "surface",
        [6.0, 0.0, 0.0],
        &[sm],
        ",$,$",
    );
    // mapped twice: moved, then moved and scaled
    let t2 = b.tri_cube();
    let mr = b.rep("Body", "Tessellation", &[t2]);
    let mo = b.axis3([0.0; 3], None, None);
    let map = b.add(&format!("IFCREPRESENTATIONMAP(#{mo},#{mr})"));
    for (x, k) in [(10.0, "$"), (20.0, "2.")] {
        let o = b.point(&[0.0, 0.0, 0.0]);
        let op = b.add(&format!(
            "IFCCARTESIANTRANSFORMATIONOPERATOR3D($,$,#{o},{k},$)"
        ));
        let mi = b.add(&format!("IFCMAPPEDITEM(#{map},#{op})"));
        b.body(
            "IFCFURNISHINGELEMENT",
            "mapped",
            [x, 0.0, 0.0],
            &[mi],
            ",$,$",
        );
    }
    let d = b.polyline(&[&[0.0, 0.0, 0.0], &[0.0, 0.0, 2.0]]);
    let sd = b.add(&format!("IFCSWEPTDISKSOLID(#{d},0.1,$,$,$)"));
    b.body("IFCPIPESEGMENT", "pipe", [30.0, 0.0, 0.0], &[sd], ",$,$");
    let pr = b.rect([1.5, 0.5], 1.0, 1.0);
    let pos = b.axis3([0.0; 3], None, None);
    let ao = b.point(&[0.0, 0.0, 0.0]);
    let ad = b.dir(&[0.0, 1.0, 0.0]);
    let a1 = b.add(&format!("IFCAXIS1PLACEMENT(#{ao},#{ad})"));
    let rv = b.add(&format!("IFCREVOLVEDAREASOLID(#{pr},#{pos},#{a1},360.)"));
    b.body(
        "IFCFURNISHINGELEMENT",
        "revolved",
        [40.0, 0.0, 0.0],
        &[rv],
        ",$,$",
    );
    b
}

#[test]
fn every_geometry_kind_becomes_an_editable_mesh() {
    let src = mesh_kinds().finish("IFC4");
    let p = plan_of(&src);
    assert_eq!(p.meshes.len(), 8, "{:?}", p.report.skipped);
    assert!(p.report.skipped.is_empty(), "{:?}", p.report.skipped);
    let bb = |i: usize| {
        let m = &p.meshes[i];
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for q in &m.positions {
            for k in 0..3 {
                lo[k] = lo[k].min(q[k]);
                hi[k] = hi[k].max(q[k]);
            }
        }
        (lo, hi, m.indices.len() / 3)
    };
    for i in 0..4 {
        let (lo, hi, n) = bb(i);
        assert!(
            close(hi[0] - lo[0], 1.0) && close(hi[2] - lo[2], 1.0),
            "{i}: {lo:?} {hi:?}"
        );
        assert_eq!(n, 12, "{i}");
    }
    let (lo, hi, _) = bb(5);
    assert!(
        close(lo[0], 20.0) && close(hi[0], 22.0),
        "scaled map {lo:?} {hi:?}"
    );
    let (lo, hi, _) = bb(6);
    assert!(
        close(hi[2] - lo[2], 2.0) && close(hi[0] - lo[0], 0.2),
        "disk {lo:?} {hi:?}"
    );
    let (lo, hi, _) = bb(7);
    assert!(
        close(hi[0] - lo[0], 4.0) && close(hi[1] - lo[1], 1.0),
        "revolved {lo:?} {hi:?}"
    );
    assert_eq!(p.meshes[6].category, "Pipe");
    assert_eq!(p.meshes[0].category, "Furniture");

    let mut m = Model::open(":memory:").unwrap();
    let rep = import_ifc(&mut m, src.as_bytes()).unwrap();
    assert_eq!(rep.meshes, 8);
    let all = m.imported_meshes(None).unwrap();
    assert_eq!(all.len(), 8);
    assert_eq!(all[0].name, "tri");
    let props = m.props("mesh", all[0].id).unwrap();
    assert!(props.iter().any(|p| p.0 == "IFC.Class"
        && p.1 == rusqlite::types::Value::Text("IFCFURNISHINGELEMENT".into())));
    assert!(m.ext_guid("mesh", all[0].id).unwrap().is_some());
    m.move_mesh(all[0].id, (1.0, 0.0, 0.0)).unwrap();
    m.undo().unwrap();
    m.undo().unwrap();
    assert!(m.imported_meshes(None).unwrap().is_empty());
}

fn walls_without_layers() -> B {
    let mut b = B::new();
    // rectangle body only
    let pr = b.rect([2.5, 0.0], 5.0, 0.3);
    let pos = b.axis3([0.0; 3], None, None);
    let x = b.extrude(pr, pos, 3.0);
    b.body("IFCWALL", "rect", [0.0; 3], &[x], ",$,$");
    // parallelogram footprint, no axis
    let pl = b.polyline(&[
        &[0.0, 10.0],
        &[4.0, 10.0],
        &[5.0, 10.3],
        &[1.0, 10.3],
        &[0.0, 10.0],
    ]);
    let ap = b.add(&format!("IFCARBITRARYCLOSEDPROFILEDEF(.AREA.,$,#{pl})"));
    let pos = b.axis3([0.0; 3], None, None);
    let x = b.extrude(ap, pos, 3.0);
    b.body("IFCWALL", "oblique", [0.0; 3], &[x], ",$,$");
    // axis given, body off-centre
    let ax = b.polyline(&[&[0.0, 0.0], &[5.0, 0.0]]);
    let axr = b.rep("Axis", "Curve2D", &[ax]);
    let pr = b.rect([2.5, 0.1], 5.0, 0.3);
    let pos = b.axis3([0.0; 3], None, None);
    let x = b.extrude(pr, pos, 3.0);
    let br = b.rep("Body", "SweptSolid", &[x]);
    let pl = b.place([0.0, 20.0, 0.0]);
    b.product("IFCWALL", "offset", pl, &[axr, br], ",$,$");
    b
}

#[test]
fn walls_without_layer_set_or_axis_are_native() {
    let p = plan_of(&walls_without_layers().finish("IFC4"));
    assert_eq!(p.walls.len(), 3, "{:?}", p.report);
    let w = &p.walls;
    assert_eq!(
        (w[0].a, w[0].b, w[0].height, w[0].offset0),
        ((0.0, 0.0), (5.0, 0.0), 3.0, None)
    );
    assert_eq!((w[1].a, w[1].b), ((0.5, 10.15), (4.5, 10.15)));
    assert_eq!(
        (w[2].a, w[2].b, w[2].offset0),
        ((0.0, 20.0), (5.0, 20.0), Some(-0.05))
    );
    for x in w {
        assert_eq!(
            p.types[x.wall_type].layers,
            vec![("Material".to_string(), 0.3)]
        );
    }
    assert_eq!(p.types.len(), 1);
    assert!(p.meshes.is_empty());
}

fn psets() -> B {
    let mut b = B::new();
    let t = b.tri_cube();
    let fe = b.body(
        "IFCFURNISHINGELEMENT",
        "Chair",
        [0.0; 3],
        &[t],
        ",'T1',.CHAIR.",
    );
    let pa = b.add("IFCPROPERTYSINGLEVALUE('A',$,IFCINTEGER(1),$)");
    let pb = b.add("IFCPROPERTYSINGLEVALUE('B',$,IFCLABEL('t'),$)");
    let g = b.guid();
    let tps = b.add(&format!("IFCPROPERTYSET({g},$,'Pset_T',$,(#{pa},#{pb}))"));
    let g = b.guid();
    let ty = b.add(&format!(
        "IFCFURNISHINGELEMENTTYPE({g},$,'Chair type',$,$,(#{tps}),$,$,$,.CHAIR.)"
    ));
    let g = b.guid();
    b.add(&format!("IFCRELDEFINESBYTYPE({g},$,$,$,(#{fe}),#{ty})"));
    let pb2 = b.add("IFCPROPERTYSINGLEVALUE('B',$,IFCLABEL('i'),$)");
    let g = b.guid();
    let ips = b.add(&format!("IFCPROPERTYSET({g},$,'Pset_T',$,(#{pb2}))"));
    let g = b.guid();
    b.add(&format!(
        "IFCRELDEFINESBYPROPERTIES({g},$,$,$,(#{fe}),#{ips})"
    ));
    let len = b.add("IFCPROPERTYSINGLEVALUE('Len',$,IFCLENGTHMEASURE(2.5),$)");
    let flag = b.add("IFCPROPERTYSINGLEVALUE('Flag',$,IFCBOOLEAN(.T.),$)");
    let en = b.add("IFCPROPERTYENUMERATEDVALUE('E',$,(IFCLABEL('a'),IFCLABEL('b')),$)");
    let g = b.guid();
    let xps = b.add(&format!(
        "IFCPROPERTYSET({g},$,'Pset_X',$,(#{len},#{flag},#{en}))"
    ));
    let qa = b.add("IFCQUANTITYAREA('Area',$,$,4.,$)");
    let g = b.guid();
    let qto = b.add(&format!("IFCELEMENTQUANTITY({g},$,'Qto_X',$,$,(#{qa}))"));
    // a set of definitions in one relation
    let g = b.guid();
    b.add(&format!(
        "IFCRELDEFINESBYPROPERTIES({g},$,$,$,(#{fe}),(#{xps},#{qto}))"
    ));
    let mat = b.add("IFCMATERIAL('Oak',$,$)");
    let g = b.guid();
    b.add(&format!(
        "IFCRELASSOCIATESMATERIAL({g},$,$,$,(#{fe}),#{mat})"
    ));
    let cs = b.add("IFCCLASSIFICATION($,$,$,'Uniclass',$,$,$)");
    let cr = b.add(&format!(
        "IFCCLASSIFICATIONREFERENCE($,'Pr_40','Furniture',#{cs},$,$)"
    ));
    let g = b.guid();
    b.add(&format!(
        "IFCRELASSOCIATESCLASSIFICATION({g},$,$,$,(#{fe}),#{cr})"
    ));
    b
}

#[test]
fn property_sets_quantities_type_material_classification_go_to_prop() {
    use rusqlite::types::Value as V;
    let mut m = Model::open(":memory:").unwrap();
    let rep = import_ifc(&mut m, psets().finish("IFC4").as_bytes()).unwrap();
    assert_eq!(rep.meshes, 1);
    let id = m.imported_meshes(None).unwrap()[0].id;
    let p: HashMap<String, (V, String)> = m
        .props("mesh", id)
        .unwrap()
        .into_iter()
        .map(|(k, v, u)| (k, (v, u)))
        .collect();
    let get = |k: &str| p.get(k).cloned().unwrap_or_else(|| panic!("{k}: {p:?}"));
    assert_eq!(get("IFC.Type").0, V::Text("Chair type".into()));
    assert_eq!(get("IFC.Tag").0, V::Text("T1".into()));
    assert_eq!(get("IFC.PredefinedType").0, V::Text("CHAIR".into()));
    assert_eq!(get("Pset_T.A").0, V::Integer(1));
    assert_eq!(
        get("Pset_T.B").0,
        V::Text("i".into()),
        "instance overrides type"
    );
    assert_eq!(get("Pset_X.Len"), (V::Real(2.5), "m".into()));
    assert_eq!(get("Pset_X.Flag").0, V::Integer(1));
    assert_eq!(get("Pset_X.E").0, V::Text("a; b".into()));
    assert_eq!(get("Qto_X.Area"), (V::Real(4.0), "m2".into()));
    assert_eq!(get("Material.Name").0, V::Text("Oak".into()));
    assert_eq!(
        get("Classification.Uniclass").0,
        V::Text("Pr_40 Furniture".into())
    );
    m.set_prop("mesh", id, "Pset_X.Len", Some((V::Real(3.0), "m")))
        .unwrap();
    assert_eq!(
        m.props("mesh", id)
            .unwrap()
            .iter()
            .find(|x| x.0 == "Pset_X.Len")
            .unwrap()
            .1,
        V::Real(3.0)
    );
}

type PMap = HashMap<String, (rusqlite::types::Value, String)>;

/// Imports a chair whose Pset has the given property lines; `{0}`, `{1}`.. in a line become
/// the ids of the `units` entities (added first).
fn own_unit_props(units: &[&str], props: &[&str]) -> PMap {
    let mut b = B::new();
    let t = b.tri_cube();
    let fe = b.body("IFCFURNISHINGELEMENT", "Chair", [0.0; 3], &[t], ",'T1',.CHAIR.");
    let mut uids: Vec<u32> = vec![];
    for u in units {
        let mut u = u.to_string();
        for (i, id) in uids.iter().enumerate() {
            u = u.replace(&format!("{{{i}}}"), &id.to_string());
        }
        uids.push(b.add(&u));
    }
    let ids: Vec<String> = props
        .iter()
        .map(|p| {
            let mut p = p.to_string();
            for (i, id) in uids.iter().enumerate() {
                p = p.replace(&format!("{{{i}}}"), &id.to_string());
            }
            format!("#{}", b.add(&p))
        })
        .collect();
    let g = b.guid();
    let ps = b.add(&format!("IFCPROPERTYSET({g},$,'Pset_U',$,({}))", ids.join(",")));
    let g = b.guid();
    b.add(&format!("IFCRELDEFINESBYPROPERTIES({g},$,$,$,(#{fe}),#{ps})"));
    let mut m = Model::open(":memory:").unwrap();
    import_ifc(&mut m, b.finish("IFC4").as_bytes()).unwrap();
    let id = m.imported_meshes(None).unwrap()[0].id;
    m.props("mesh", id)
        .unwrap()
        .into_iter()
        .map(|(k, v, u)| (k, (v, u)))
        .collect()
}

fn real_of(p: &PMap, k: &str) -> (f64, String) {
    match p.get(k) {
        Some((rusqlite::types::Value::Real(r), u)) => (*r, u.clone()),
        o => panic!("{k}: {o:?} in {p:?}"),
    }
}

const MM: &str = "IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.)";

#[test]
fn pset_single_value_own_unit_mm_converted() {
    let p = own_unit_props(
        &[MM],
        &[
            "IFCPROPERTYSINGLEVALUE('Raw',$,IFCREAL(2.5),#{0})",
            "IFCPROPERTYSINGLEVALUE('Typed',$,IFCLENGTHMEASURE(2.5),#{0})",
        ],
    );
    for k in ["Pset_U.Raw", "Pset_U.Typed"] {
        let (v, u) = real_of(&p, k);
        assert!((v - 0.0025).abs() < 1e-12, "{k} {v}");
        assert_eq!(u, "m");
    }
}

#[test]
fn pset_bounded_value_own_unit() {
    let p = own_unit_props(
        &[MM],
        &["IFCPROPERTYBOUNDEDVALUE('Rng',$,IFCREAL(20.),IFCREAL(10.),#{0},$)"],
    );
    let (t, u) = &p["Pset_U.Rng"];
    let rusqlite::types::Value::Text(t) = t else { panic!("{t:?}") };
    let (lo, hi) = t.split_once("..").unwrap_or_else(|| panic!("{t}"));
    assert!((lo.parse::<f64>().unwrap() - 0.01).abs() < 1e-12, "{t}");
    assert!((hi.parse::<f64>().unwrap() - 0.02).abs() < 1e-12, "{t}");
    assert_eq!(u, "m");
}

#[test]
fn pset_massmeasure_gram_project_unit() {
    let p = own_unit_props(
        &["IFCSIUNIT(*,.MASSUNIT.,$,.GRAM.)"],
        &[
            "IFCPROPERTYSINGLEVALUE('Own',$,IFCMASSMEASURE(2500.),#{0})",
            "IFCPROPERTYSINGLEVALUE('Proj',$,IFCMASSMEASURE(2.5),$)",
        ],
    );
    let (v, u) = real_of(&p, "Pset_U.Own");
    assert!((v - 2.5).abs() < 1e-12, "{v}");
    assert_eq!(u, "kg");
    assert_eq!(real_of(&p, "Pset_U.Proj"), (2.5, "kg".into()));
}

#[test]
fn pset_own_unit_conversion_based_and_dollar_fallback() {
    let p = own_unit_props(
        &[
            "IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.)",
            "IFCMEASUREWITHUNIT(IFCLENGTHMEASURE(0.0254),#{0})",
            "IFCCONVERSIONBASEDUNIT(#3,.LENGTHUNIT.,'INCH',#{1})",
        ],
        &[
            "IFCPROPERTYSINGLEVALUE('In',$,IFCREAL(10.),#{2})",
            "IFCPROPERTYSINGLEVALUE('Proj',$,IFCLENGTHMEASURE(2.5),$)",
        ],
    );
    let (v, u) = real_of(&p, "Pset_U.In");
    assert!((v - 0.254).abs() < 1e-12, "{v}");
    assert_eq!(u, "m");
    assert_eq!(real_of(&p, "Pset_U.Proj"), (2.5, "m".into()));
}

fn text_of_prop(p: &PMap, k: &str) -> (String, String) {
    match p.get(k) {
        Some((rusqlite::types::Value::Text(t), u)) => (t.clone(), u.clone()),
        o => panic!("{k}: {o:?} in {p:?}"),
    }
}

#[test]
fn pset_list_value_own_unit() {
    let p = own_unit_props(
        &[MM],
        &["IFCPROPERTYLISTVALUE('L',$,(IFCREAL(10.),IFCREAL(20.)),#{0})"],
    );
    let (t, u) = text_of_prop(&p, "Pset_U.L");
    let n: Vec<f64> = t.split("; ").map(|s| s.parse().unwrap()).collect();
    assert_eq!(n.len(), 2, "{t}");
    assert!((n[0] - 0.01).abs() < 1e-12 && (n[1] - 0.02).abs() < 1e-12, "{t}");
    assert_eq!(u, "m");
}

#[test]
fn pset_enum_value_unit_label_kept() {
    let p = own_unit_props(
        &[MM, "IFCPROPERTYENUMERATION('E',(IFCREAL(10.),IFCREAL(20.)),#{0})"],
        &["IFCPROPERTYENUMERATEDVALUE('En',$,(IFCREAL(10.),IFCREAL(20.)),#{1})"],
    );
    let (t, u) = text_of_prop(&p, "Pset_U.En");
    assert_eq!(t, "0.01; 0.02");
    assert_eq!(u, "m");
}

#[test]
fn pset_list_dollar_unit_project_fallback() {
    let p = own_unit_props(
        &[],
        &["IFCPROPERTYLISTVALUE('L',$,(IFCLENGTHMEASURE(1.5),IFCLENGTHMEASURE(2.5)),$)"],
    );
    let (t, u) = text_of_prop(&p, "Pset_U.L");
    assert_eq!(t, "1.5; 2.5");
    assert_eq!(u, "m");
}

fn reimport_src(h: f64, with_chair: bool) -> String {
    let mut b = B::new();
    let pr = b.rect([2.5, 0.0], 5.0, 0.3);
    let pos = b.axis3([0.0; 3], None, None);
    let x = b.extrude(pr, pos, h);
    b.body("IFCWALL", "w", [0.0; 3], &[x], ",$,$");
    let t = b.tri_cube();
    let pl = b.place([1.0, 1.0, 0.0]);
    let rp = b.rep("Body", "Tessellation", &[t]);
    if with_chair {
        b.product("IFCFURNISHINGELEMENT", "c", pl, &[rp], ",$,$");
    }
    b.finish("IFC4")
}

#[test]
fn reimport_updates_rows_by_global_id() {
    let mut m = Model::open(":memory:").unwrap();
    let r1 = import_ifc(&mut m, reimport_src(3.0, true).as_bytes()).unwrap();
    assert_eq!(
        (r1.walls, r1.meshes, r1.updated, r1.unchanged),
        (1, 1, 0, 0)
    );
    let r2 = import_ifc(&mut m, reimport_src(3.0, true).as_bytes()).unwrap();
    assert_eq!((r2.updated, r2.unchanged, r2.missing), (0, 2, 0));
    assert_eq!(
        (count(&m, "wall"), count(&m, "mesh"), count(&m, "wall_type")),
        (1, 1, 1)
    );
    let r3 = import_ifc(&mut m, reimport_src(3.5, false).as_bytes()).unwrap();
    assert_eq!((r3.updated, r3.unchanged, r3.missing), (1, 0, 1));
    let h = |m: &Model| -> f64 {
        m.sql()
            .query_row("SELECT height FROM wall", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!((count(&m, "wall"), h(&m)), (1, 3.5));
    m.undo().unwrap();
    assert_eq!((count(&m, "wall"), h(&m), count(&m, "mesh")), (1, 3.0, 1));
    assert!(m
        .props("wall", 1)
        .unwrap()
        .iter()
        .any(|p| p.0 == "IFC.GlobalId"));
}

#[test]
fn reimport_keeps_converted_mesh_element() {
    use ddc_geom::member::Shape;
    use ddc_geom::mesh_fit::Fit;
    let mut m = Model::open(":memory:").unwrap();
    import_ifc(&mut m, reimport_src(3.0, true).as_bytes()).unwrap();
    let mesh: i64 = m
        .sql()
        .query_row("SELECT id FROM mesh", [], |r| r.get(0))
        .unwrap();
    // the chair cube (1..2 × 1..2 × 0..1) promoted by the user to a native column
    let fit = Fit::Column {
        at: ddc_geom::v2(1.5, 1.5),
        z0: 0.0,
        z1: 1.0,
        shape: Shape::Rect { b: 1.0, h: 1.0 },
        rotation: 0.0,
    };
    let r = m.convert_meshes(&[(mesh, fit)], false).unwrap();
    assert_eq!(r.done[0].table, "col");
    let col = r.done[0].id;
    m.sql()
        .execute(
            "INSERT INTO prop(kind,ref,key,value,unit) VALUES ('col',?1,'Note','mine','')",
            [col],
        )
        .unwrap();
    let r2 = import_ifc(&mut m, reimport_src(3.0, true).as_bytes()).unwrap();
    assert_eq!((r2.updated, r2.unchanged, r2.missing), (0, 2, 0));
    assert_eq!(
        (count(&m, "mesh"), count(&m, "col")),
        (0, 1),
        "no duplicate mesh"
    );
    let refs = |kind: &str| -> Vec<i64> {
        let mut st = m
            .sql()
            .prepare("SELECT ref FROM ext_ref WHERE kind=?1")
            .unwrap();
        let v: Vec<i64> = st
            .query_map([kind], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        v
    };
    assert_eq!(
        refs("col"),
        vec![col],
        "GlobalId stays on the converted element"
    );
    assert!(refs("mesh").is_empty());
    assert!(
        m.props("col", col).unwrap().iter().any(|p| p.0 == "Note"),
        "user data stays"
    );
    assert!(m
        .props("col", col)
        .unwrap()
        .iter()
        .any(|p| p.0 == "IFC.GlobalId"));
    m.undo().unwrap();
    assert_eq!((count(&m, "mesh"), count(&m, "col")), (0, 1));
}

#[test]
fn reimport_keeps_local_prop_edit_on_converted_mesh() {
    use ddc_geom::member::Shape;
    use ddc_geom::mesh_fit::Fit;
    let mut m = Model::open(":memory:").unwrap();
    import_ifc(&mut m, reimport_src(3.0, true).as_bytes()).unwrap();
    let mesh: i64 = m
        .sql()
        .query_row("SELECT id FROM mesh", [], |r| r.get(0))
        .unwrap();
    let fit = Fit::Column {
        at: ddc_geom::v2(1.5, 1.5),
        z0: 0.0,
        z1: 1.0,
        shape: Shape::Rect { b: 1.0, h: 1.0 },
        rotation: 0.0,
    };
    let col = m.convert_meshes(&[(mesh, fit)], false).unwrap().done[0].id;
    m.sql()
        .execute(
            "UPDATE prop SET value='mine' WHERE kind='col' AND ref=?1 AND key='IFC.Class'",
            [col],
        )
        .unwrap();
    let class = |m: &Model| -> String {
        m.sql()
            .query_row(
                "SELECT value FROM prop WHERE kind='col' AND ref=?1 AND key='IFC.Class'",
                [col],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!(class(&m), "mine");
    for _ in 0..2 {
        import_ifc(&mut m, reimport_src(3.0, true).as_bytes()).unwrap();
        assert_eq!(class(&m), "mine", "a local edit on a kept row survives a re-import");
    }
    m.undo().unwrap();
    assert_eq!(class(&m), "mine");
}

fn members() -> B {
    let mut b = B::new();
    let p2 = b.pos2([0.0, 0.0]);
    let i = b.add(&format!(
        "IFCISHAPEPROFILEDEF(.AREA.,'HEA200',#{p2},0.2,0.19,0.0065,0.01,$,$,$)"
    ));
    let pos = b.axis3([0.0; 3], None, None);
    let x = b.extrude(i, pos, 3.0);
    b.body("IFCCOLUMN", "c1", [1.0, 1.0, 0.0], &[x], ",$,.COLUMN.");
    let rp = b.rect([0.0, 0.0], 0.2, 0.4);
    let pos = b.axis3(
        [0.0, 0.0, 3.0],
        Some([1.0, 0.0, 0.0]),
        Some([0.0, 1.0, 0.0]),
    );
    let x = b.extrude(rp, pos, 6.0);
    b.body("IFCBEAM", "b1", [0.0; 3], &[x], ",$,.BEAM.");
    let tri = b.polyline(&[&[0.0, 0.0], &[0.3, 0.0], &[0.0, 0.3], &[0.0, 0.0]]);
    let ap = b.add(&format!(
        "IFCARBITRARYCLOSEDPROFILEDEF(.AREA.,'tri',#{tri})"
    ));
    let pos = b.axis3([0.0; 3], None, None);
    let x = b.extrude(ap, pos, 2.5);
    b.body("IFCCOLUMN", "c2", [5.0, 5.0, 0.0], &[x], ",$,$");
    b
}

#[test]
fn columns_and_beams_become_native_members() {
    use ddc_model::MemberShape as S;
    let src = members().finish("IFC4");
    let p = plan_of(&src);
    assert_eq!(p.members.len(), 3, "{:?}", p.report);
    let c = &p.members[0];
    assert!(c.column);
    assert_eq!(
        c.shape,
        S::I {
            h: 0.19,
            b: 0.2,
            tw: 0.0065,
            tf: 0.01
        }
    );
    assert_eq!(
        (c.a, c.b, c.rotation),
        ([1.0, 1.0, 0.0], [1.0, 1.0, 3.0], 0.0)
    );
    let bm = &p.members[1];
    assert!(!bm.column);
    assert_eq!(bm.shape, S::Rect { b: 0.2, h: 0.4 });
    assert_eq!((bm.a, bm.b), ([0.0, 0.0, 3.0], [6.0, 0.0, 3.0]));
    assert!(bm.rotation.abs() < 1e-9, "{}", bm.rotation);
    let S::Poly(t) = &p.members[2].shape else {
        panic!("{:?}", p.members[2].shape)
    };
    assert_eq!(t.len(), 3);
    // the axis passes through the bounding-box centre of the section
    assert_eq!((p.members[2].a[0], p.members[2].a[1]), (5.15, 5.15));
    let mut m = Model::open(":memory:").unwrap();
    let rep = import_ifc(&mut m, src.as_bytes()).unwrap();
    assert_eq!((rep.columns, rep.beams), (2, 1));
    assert_eq!(
        (
            count(&m, "col"),
            count(&m, "beam"),
            count(&m, "member_type")
        ),
        (2, 1, 3)
    );
}

fn clipped_wall() -> B {
    let mut b = B::new();
    let ax = b.polyline(&[&[0.0, 0.0], &[5.0, 0.0]]);
    let axr = b.rep("Axis", "Curve2D", &[ax]);
    let pr = b.rect([2.5, 0.0], 5.0, 0.2);
    let pos = b.axis3([0.0; 3], None, None);
    let x = b.extrude(pr, pos, 4.0);
    let n = 1.0f64.hypot(0.2);
    let pa = b.axis3([0.0, 0.0, 3.0], Some([-0.2 / n, 0.0, 1.0 / n]), None);
    let plane = b.add(&format!("IFCPLANE(#{pa})"));
    let hs = b.add(&format!("IFCHALFSPACESOLID(#{plane},.F.)"));
    let cl = b.add(&format!(
        "IFCBOOLEANCLIPPINGRESULT(.DIFFERENCE.,#{x},#{hs})"
    ));
    let br = b.rep("Body", "Clipping", &[cl]);
    let pl = b.place([0.0; 3]);
    b.product("IFCWALL", "sloped", pl, &[axr, br], ",$,$");
    // the same body as a generic product: the mesh is cut too
    b.body(
        "IFCBUILDINGELEMENTPROXY",
        "cut",
        [0.0, 5.0, 0.0],
        &[cl],
        ",$,$",
    );
    b
}

#[test]
fn half_space_clipping_gives_wall_top_profile() {
    let src = clipped_wall().finish("IFC4");
    let p = plan_of(&src);
    assert_eq!(p.walls.len(), 1, "{:?}", p.report);
    let w = &p.walls[0];
    assert_eq!(w.top.as_deref(), Some(&[(0.0, 3.0), (5.0, 4.0)][..]));
    assert_eq!(w.height, 4.0);
    let zmax = p.meshes[0]
        .positions
        .iter()
        .map(|q| q[2])
        .fold(0.0, f64::max);
    let at0 = p.meshes[0]
        .positions
        .iter()
        .filter(|q| q[0].abs() < 1e-9)
        .map(|q| q[2])
        .fold(0.0, f64::max);
    assert!(close(zmax, 4.0) && close(at0, 3.0), "{zmax} {at0}");
    let mut m = Model::open(":memory:").unwrap();
    import_ifc(&mut m, src.as_bytes()).unwrap();
    let t: Option<String> = m
        .sql()
        .query_row("SELECT top_profile FROM wall", [], |r| r.get(0))
        .unwrap();
    assert!(t.is_some());
}

fn roof_and_ceiling() -> B {
    let mut b = B::new();
    let pr = b.rect([2.0, 2.0], 4.0, 4.0);
    let pos = b.axis3([0.0; 3], None, None);
    let x = b.extrude(pr, pos, 0.02);
    b.body("IFCCOVERING", "ceil", [0.0, 0.0, 2.7], &[x], ",$,.CEILING.");
    let pl = b.place([0.0; 3]);
    let roof = b.product("IFCROOF", "roof", pl, &[], ",$,.SHED_ROOF.");
    let (s, c) = (0.5f64, 0.75f64.sqrt());
    let pr = b.rect([2.0, 3.0], 4.0, 6.0);
    let pos = b.axis3([0.0, 0.0, 3.0], Some([-s, 0.0, c]), Some([c, 0.0, s]));
    let x = b.extrude(pr, pos, 0.2);
    let pl = b.place([0.0; 3]);
    let rep = b.rep("Body", "SweptSolid", &[x]);
    let sl = b.product("IFCSLAB", "part", pl, &[rep], ",$,.ROOF.");
    b.contained.pop();
    let pr = b.rect([10.0, 3.0], 4.0, 6.0);
    let pos = b.axis3([0.0, 0.0, 3.0], None, None);
    let x = b.extrude(pr, pos, 0.2);
    let pl = b.place([0.0; 3]);
    let rep = b.rep("Body", "SweptSolid", &[x]);
    let flat = b.product("IFCSLAB", "flat", pl, &[rep], ",$,.ROOF.");
    b.contained.pop();
    let g = b.guid();
    b.add(&format!(
        "IFCRELAGGREGATES({g},$,$,$,#{roof},(#{sl},#{flat}))"
    ));
    b
}

#[test]
fn ceilings_and_planar_roof_parts_are_native() {
    let src = roof_and_ceiling().finish("IFC4");
    let p = plan_of(&src);
    assert_eq!(
        (p.ceilings.len(), p.roofs.len(), p.slabs.len()),
        (1, 1, 1),
        "{:?}",
        p.report
    );
    assert!(p.report.skipped.is_empty(), "{:?}", p.report.skipped);
    assert!(p.meshes.is_empty());
    assert_eq!(p.ceilings[0].height, 2.7);
    let r = &p.roofs[0];
    let ddc_model::RoofKind::Footprint { slopes, .. } = &r.kind else {
        panic!("{:?}", r.kind)
    };
    let sl: Vec<_> = slopes
        .iter()
        .map(|s| s.map(|d| (d * 1e6).round() / 1e6))
        .collect();
    assert_eq!(sl, vec![None, None, None, Some(30.0)]);
    assert!(close(r.offset, 3.0 + 0.2 * 0.75f64.sqrt()), "{}", r.offset);
    let mut m = Model::open(":memory:").unwrap();
    let rep = import_ifc(&mut m, src.as_bytes()).unwrap();
    assert_eq!((rep.roofs, rep.ceilings, rep.slabs), (1, 1, 1));
    assert_eq!(
        (count(&m, "roof"), count(&m, "ceiling"), count(&m, "slab")),
        (1, 1, 1)
    );
}

#[test]
fn schema_variants_and_damaged_references_are_tolerated() {
    for schema in [
        "IFC2X3_TC1",
        "IFC4X1",
        "IFC4X2",
        "IFC4X3_ADD2",
        "IFC4_ADD2_TC1",
    ] {
        let p = plan_of(&mesh_kinds().finish(schema));
        assert_eq!(p.meshes.len(), 8, "{schema}");
    }
    let mut b = mesh_kinds();
    b.s.push_str("#105=IFCCARTESIANPOINT((9.,9.,9.));\n");
    let r = b.rep("Body", "Tessellation", &[99999]);
    let pl = b.place([50.0, 0.0, 0.0]);
    b.product("IFCFURNISHINGELEMENT", "dangling", pl, &[r], ",$,$");
    let mut m = Model::open(":memory:").unwrap();
    let rep = import_ifc(&mut m, b.finish("IFC4").as_bytes()).unwrap();
    assert_eq!(rep.meshes, 8);
    assert!(
        rep.warnings.iter().any(|w| w.contains("#105")),
        "{:?}",
        rep.warnings
    );
    assert!(
        rep.warnings.iter().any(|w| w.contains("99999")),
        "{:?}",
        rep.warnings
    );
    assert_eq!(rep.skipped.values().sum::<usize>(), 1, "{:?}", rep.skipped);
}

fn arcs() -> B {
    let mut b = B::new();
    let body = |b: &mut B| {
        let pr = b.rect([0.0, 0.0], 0.2, 0.2);
        let pos = b.axis3([0.0; 3], None, None);
        let x = b.extrude(pr, pos, 3.0);
        b.rep("Body", "SweptSolid", &[x])
    };
    let pl = b.add("IFCCARTESIANPOINTLIST2D(((0.,0.),(5.,0.),(6.,1.),(5.,2.)))");
    let ipc = b.add(&format!(
        "IFCINDEXEDPOLYCURVE(#{pl},(IFCLINEINDEX((1,2)),IFCARCINDEX((2,3,4))),.F.)"
    ));
    let ax = b.rep("Axis", "Curve2D", &[ipc]);
    let br = body(&mut b);
    let p = b.place([0.0; 3]);
    b.product("IFCWALL", "indexed", p, &[ax, br], ",$,$");
    let line = b.polyline(&[&[0.0, 0.0], &[5.0, 0.0]]);
    let s1 = b.add(&format!(
        "IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#{line})"
    ));
    let cp = b.pos2([5.0, 1.0]);
    let circ = b.add(&format!("IFCCIRCLE(#{cp},1.)"));
    let tc = b.add(&format!(
        "IFCTRIMMEDCURVE(#{circ},(IFCPARAMETERVALUE(270.)),(IFCPARAMETERVALUE(90.)),.T.,.PARAMETER.)"
    ));
    let s2 = b.add(&format!("IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#{tc})"));
    let cc = b.add(&format!("IFCCOMPOSITECURVE((#{s1},#{s2}),.F.)"));
    let ax = b.rep("Axis", "Curve2D", &[cc]);
    let br = body(&mut b);
    let p = b.place([0.0, 10.0, 0.0]);
    b.product("IFCWALL", "composite", p, &[ax, br], ",$,$");
    b
}

#[test]
fn indexed_and_composite_curve_arcs_are_exact() {
    let p = plan_of(&arcs().finish("IFC4"));
    assert_eq!(p.walls.len(), 4, "{:?}", p.report);
    for (k, y) in [(0, 0.0), (2, 10.0)] {
        let (l, a) = (&p.walls[k], &p.walls[k + 1]);
        assert_eq!((l.a, l.b, l.bulge), ((0.0, y), (5.0, y), 0.0));
        assert_eq!((a.a, a.b), ((5.0, y), (5.0, y + 2.0)));
        assert!((a.bulge - 1.0).abs() < 1e-9, "{k}: {}", a.bulge);
    }
}

fn georef_src() -> String {
    let mut b = B::new();
    let crs = b.add("IFCPROJECTEDCRS('EPSG:25832',$,'ETRS89',$,$,'32N',$)");
    b.add(&format!(
        "IFCMAPCONVERSION(#13,#{crs},500000.,5600000.,100.,0.,1.,$)"
    ));
    let pl = b.place([0.0; 3]);
    b.product(
        "IFCSITE",
        "Site",
        pl,
        &[],
        ",$,.ELEMENT.,(52,30,0,0),(13,24,0,0),34.,$,$",
    );
    b.contained.pop();
    let mut s = b.finish("IFC4");
    s = s.replace("#12=IFCDIRECTION((0.,1.));", "#12=IFCDIRECTION((-1.,1.));");
    s
}

#[test]
fn map_conversion_and_true_north_become_project_props() {
    use rusqlite::types::Value as V;
    let mut m = Model::open(":memory:").unwrap();
    import_ifc(&mut m, georef_src().as_bytes()).unwrap();
    let p: HashMap<String, V> = m
        .props("project", 0)
        .unwrap()
        .into_iter()
        .map(|x| (x.0, x.1))
        .collect();
    assert_eq!(p["Georef.Eastings"], V::Real(500000.0));
    assert_eq!(p["Georef.Northings"], V::Real(5600000.0));
    assert_eq!(p["Georef.CRS"], V::Text("EPSG:25832".into()));
    assert_eq!(p["Georef.XAxisRotation"], V::Real(90.0));
    let V::Real(tn) = p["Georef.TrueNorth"] else {
        panic!()
    };
    assert!(close(tn, 45.0), "{tn}");
    let V::Real(lat) = p["Site.Latitude"] else {
        panic!()
    };
    assert!(close(lat, 52.5));
}

#[test]
fn ifczip_imports() {
    use std::io::Write;
    let src = psets().finish("IFC4");
    let mut w = ::zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let o = ::zip::write::SimpleFileOptions::default()
        .compression_method(::zip::CompressionMethod::Deflated);
    w.start_file("model.ifc", o).unwrap();
    w.write_all(src.as_bytes()).unwrap();
    let z = w.finish().unwrap().into_inner();
    let mut m = Model::open(":memory:").unwrap();
    assert_eq!(import_ifc(&mut m, &z).unwrap().meshes, 1);
}

#[test]
fn mutated_fixtures_never_panic() {
    let srcs = [
        mesh_kinds().finish("IFC4"),
        psets().finish("IFC4"),
        members().finish("IFC4"),
        clipped_wall().finish("IFC4"),
        roof_and_ceiling().finish("IFC4"),
        arcs().finish("IFC4"),
        walls_without_layers().finish("IFC4"),
    ];
    let alphabet = b"#$*'(),.;=0123456789-E";
    let mut seed = 0x9E3779B97F4A7C15u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for src in &srcs {
        let base = src.as_bytes();
        for _ in 0..150 {
            let mut b = base.to_vec();
            for _ in 0..1 + next() % 4 {
                let i = (next() as usize) % b.len();
                b[i] = alphabet[(next() as usize) % alphabet.len()];
            }
            if let Ok(pl) = plan_bytes(&b) {
                let mut m = Model::open(":memory:").unwrap();
                let _ = m.batch_walls_edit("wall_import_ifc", |db, s| {
                    commit::commit_with(db, s, &pl, Default::default())
                });
            }
        }
    }
}

/// Synthetic ~100 MB file; `cargo test --release -- --ignored large_file`.
#[test]
#[ignore]
fn large_file_reads_fast() {
    let mut b = B::new();
    let t = b.tri_cube();
    let mr = b.rep("Body", "Tessellation", &[t]);
    let mo = b.axis3([0.0; 3], None, None);
    let map = b.add(&format!("IFCREPRESENTATIONMAP(#{mo},#{mr})"));
    let mut k = 0usize;
    while b.s.len() < 100_000_000 {
        k += 1;
        if k % 4 == 0 {
            let pr = b.rect([2.5, 0.0], 5.0, 0.3);
            let pos = b.axis3([0.0; 3], None, None);
            let x = b.extrude(pr, pos, 3.0);
            b.body(
                "IFCWALL",
                "w",
                [(k % 1000) as f64 * 6.0, (k / 1000) as f64, 0.0],
                &[x],
                ",$,$",
            );
        } else {
            let o = b.point(&[0.0, 0.0, 0.0]);
            let op = b.add(&format!(
                "IFCCARTESIANTRANSFORMATIONOPERATOR3D($,$,#{o},$,$)"
            ));
            let mi = b.add(&format!("IFCMAPPEDITEM(#{map},#{op})"));
            let e = b.body(
                "IFCFURNISHINGELEMENT",
                "f",
                [k as f64, 0.0, 0.0],
                &[mi],
                ",$,$",
            );
            let pv = b.add("IFCPROPERTYSINGLEVALUE('Mark',$,IFCLABEL('x'),$)");
            let g = b.guid();
            let ps = b.add(&format!("IFCPROPERTYSET({g},$,'Pset_F',$,(#{pv}))"));
            let g = b.guid();
            b.add(&format!(
                "IFCRELDEFINESBYPROPERTIES({g},$,$,$,(#{e}),#{ps})"
            ));
        }
    }
    let src = b.finish("IFC4");
    let t0 = std::time::Instant::now();
    let f = StepFile::parse_tolerant(src.as_bytes()).unwrap();
    let parse = t0.elapsed();
    let pl = super::plan(&f).unwrap();
    drop(f);
    let read = t0.elapsed();
    let mut m = Model::open(":memory:").unwrap();
    let t1 = std::time::Instant::now();
    m.batch_walls_edit("wall_import_ifc", |db, s| {
        commit::commit_with(db, s, &pl, Default::default())
    })
    .unwrap();
    let write = t1.elapsed();
    eprintln!(
        "large_file: {} MB, {} products, parse {:?}, parse+plan {:?}, write {:?}",
        src.len() / 1_000_000,
        pl.elems.len(),
        parse,
        read,
        write
    );
    // parse budget 3 s; the full plan (tessellation, properties) only guards against blow-ups
    if !cfg!(debug_assertions) {
        assert!(parse.as_secs_f64() <= 3.0, "{parse:?}");
        assert!(read.as_secs_f64() <= 15.0, "{read:?}");
    }
}

/// A 5 m wall (rectangle body) and openings given as (centre, sill, height) boxes 1 m wide.
fn wall_with_openings(boxes: &[([f64; 2], f64, f64)]) -> String {
    let mut b = B::new();
    let pr = b.rect([2.5, 0.0], 5.0, 0.3);
    let pos = b.axis3([0.0; 3], None, None);
    let x = b.extrude(pr, pos, 3.0);
    let w = b.body("IFCWALL", "w", [0.0; 3], &[x], ",$,$");
    for &(c, z, h) in boxes {
        let pr = b.rect(c, 1.0, 1.0);
        let pos = b.axis3([0.0; 3], None, None);
        let x = b.extrude(pr, pos, h);
        let o = b.body(
            "IFCOPENINGELEMENT",
            "o",
            [0.0, 0.0, z],
            &[x],
            ",$,.OPENING.",
        );
        b.contained.pop();
        let g = b.guid();
        b.add(&format!("IFCRELVOIDSELEMENT({g},$,$,$,#{w},#{o})"));
    }
    b.finish("IFC4")
}

#[test]
fn opening_past_its_wall_is_cut_back_not_fatal() {
    // past the end, below the floor, entirely beyond the wall
    let src = wall_with_openings(&[
        ([5.0, 0.0], 1.0, 1.0),
        ([1.0, 0.0], -0.5, 2.6),
        ([9.0, 0.0], 1.0, 1.0),
    ]);
    let p = plan_of(&src);
    let o: Vec<_> = p
        .openings
        .iter()
        .map(|o| (o.along, o.width, o.sill, o.height))
        .collect();
    assert_eq!(o.len(), 2, "{:?}", p.report);
    assert!(
        close(o[0].0, 4.5) && close(o[0].1, 0.5) && close(o[0].2, 1.0) && close(o[0].3, 1.0),
        "{o:?}"
    );
    assert!(
        close(o[1].0, 0.5) && close(o[1].1, 1.0) && close(o[1].2, 0.0) && close(o[1].3, 2.1),
        "{o:?}"
    );
    let cut = p
        .report
        .warnings
        .iter()
        .filter(|w| w.contains("cut back"))
        .count();
    assert_eq!(cut, 2, "{:?}", p.report.warnings);
    assert_eq!(
        p.report.skipped.get("IFCOPENINGELEMENT: outside host wall"),
        Some(&1),
        "{:?}",
        p.report.skipped
    );
    let mut m = Model::open(":memory:").unwrap();
    let rep = import_ifc(&mut m, src.as_bytes()).unwrap();
    assert_eq!((rep.walls, rep.openings), (1, 2));
    assert_eq!((count(&m, "wall"), count(&m, "opening")), (1, 2));
}

#[test]
fn bad_slab_outline_is_repaired_with_a_warning() {
    let mut b = B::new();
    let outer = b.polyline(&[
        &[0.0, 0.0],
        &[2.0, 0.0],
        &[4.0, 0.0],
        &[4.0, 0.0],
        &[4.0, 4.0],
        &[0.0, 4.0],
        &[0.0, 0.0],
    ]);
    let hole = b.polyline(&[&[10.0, 10.0], &[11.0, 10.0], &[11.0, 11.0], &[10.0, 10.0]]);
    let ap = b.add(&format!(
        "IFCARBITRARYPROFILEDEFWITHVOIDS(.AREA.,$,#{outer},(#{hole}))"
    ));
    let pos = b.axis3([0.0; 3], None, None);
    let x = b.extrude(ap, pos, 0.2);
    b.body("IFCSLAB", "s", [0.0, 0.0, -0.2], &[x], ",$,.FLOOR.");
    let src = b.finish("IFC4");
    let p = plan_of(&src);
    assert_eq!(p.slabs.len(), 1, "{:?}", p.report);
    assert!(p.slabs[0].holes.is_empty());
    assert_eq!(p.slabs[0].outer.len(), 4, "{:?}", p.slabs[0].outer);
    assert!(
        p.report
            .warnings
            .iter()
            .any(|w| w.contains("outline repaired")),
        "{:?}",
        p.report.warnings
    );
    let mut m = Model::open(":memory:").unwrap();
    import_ifc(&mut m, src.as_bytes()).unwrap();
    assert_eq!(count(&m, "slab"), 1);
}

#[test]
fn open_space_is_closed_by_separators_along_its_outline() {
    let mut b = B::new();
    for (c, x, y) in [([2.5, 0.0], 5.0, 0.2), ([0.0, 2.0], 0.2, 4.0)] {
        let pr = b.rect(c, x, y);
        let pos = b.axis3([0.0; 3], None, None);
        let e = b.extrude(pr, pos, 3.0);
        b.body("IFCWALL", "w", [0.0; 3], &[e], ",$,$");
    }
    let pr = b.rect([2.5, 2.0], 4.8, 3.8);
    let pos = b.axis3([0.0; 3], None, None);
    let e = b.extrude(pr, pos, 2.7);
    b.body(
        "IFCSPACE",
        "101",
        [0.0; 3],
        &[e],
        ",'Office',.ELEMENT.,.SPACE.,$",
    );
    let src = b.finish("IFC4");
    let mut m = Model::open(":memory:").unwrap();
    let rep = import_ifc(&mut m, src.as_bytes()).unwrap();
    assert_eq!((rep.walls, rep.rooms), (2, 1), "{rep:?}");
    assert_eq!(count(&m, "room_sep"), 2, "{:?}", rep.warnings);
    let id: i64 = m
        .sql()
        .query_row("SELECT id FROM room", [], |r| r.get(0))
        .unwrap();
    let q = m.room_quantities(id).unwrap().expect("room closed");
    assert!(close(q.area, 4.8 * 3.8), "{}", q.area);
    assert!(
        rep.warnings
            .iter()
            .any(|w| w.contains("closed by separators")),
        "{:?}",
        rep.warnings
    );
    // a re-import finds the room closed: no more separators
    import_ifc(&mut m, src.as_bytes()).unwrap();
    assert_eq!(count(&m, "room_sep"), 2);
}

#[test]
fn export_writes_members_roofs_ceilings_and_meshes_and_reimports_them_exactly() {
    use ddc_model::{
        BeamJoin, BeamSpec, CeilingSpec, ColumnSpec, Just, LayerSpec, MemberShape, RoofKind,
        RoofSpec,
    };
    let mut m = Model::open(":memory:").unwrap();
    import_ifc(&mut m, mesh_kinds().finish("IFC4").as_bytes()).unwrap();
    let level: i64 = m
        .sql()
        .query_row("SELECT id FROM level", [], |r| r.get(0))
        .unwrap();
    let top = m.add_level("L2", 3.5).unwrap();
    let layer = |material: &str, thickness| LayerSpec {
        material: material.into(),
        thickness,
    };
    let st = m
        .add_slab_type("roof 250", &[layer("tiles", 0.05), layer("wood", 0.2)])
        .unwrap();
    let ct = m
        .add_slab_type("gypsum 20", &[layer("gypsum", 0.02)])
        .unwrap();
    let rect = m
        .add_member_type("C30", &MemberShape::Rect { b: 0.3, h: 0.4 }, "concrete")
        .unwrap();
    let ibeam = m
        .add_member_type(
            "I300",
            &MemberShape::I {
                h: 0.3,
                b: 0.15,
                tw: 0.007,
                tf: 0.01,
            },
            "steel",
        )
        .unwrap();
    let col = ColumnSpec {
        level,
        top_level: Some(top),
        member_type: rect,
        structural: true,
        at: (1.0, 1.0),
        base_offset: 0.0,
        top_offset: -0.4,
        rotation: 0.3,
        top_at: None,
    };
    m.add_column(&col).unwrap();
    let beam = BeamSpec {
        level,
        member_type: ibeam,
        a: (0.0, 0.0, 3.0),
        b: (4.0, 0.5, 3.0),
        bulge: 0.0,
        yjust: Just::Center,
        zjust: Just::Pos,
        rotation: 0.0,
        start_ext: 0.0,
        end_ext: 0.1,
        join_start: BeamJoin::Auto,
        join_end: BeamJoin::Butt,
    };
    m.add_beam(&beam).unwrap();
    let ring = vec![(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)];
    let roof = RoofSpec {
        level,
        slab_type: st,
        offset: 3.0,
        overhang: 0.3,
        fascia: 0.0,
        kind: RoofKind::Footprint {
            ring: ring.clone(),
            slopes: vec![Some(30.0), None, Some(30.0), None],
            mansard: None,
        },
    };
    m.add_roof(&roof).unwrap();
    let ceil = CeilingSpec {
        level,
        slab_type: ct,
        height: 2.7,
        outer: ring.clone(),
        holes: vec![vec![(1.0, 1.0), (1.0, 2.0), (2.0, 2.0), (2.0, 1.0)]],
    };
    m.add_ceiling(&ceil).unwrap();

    let text = crate::ifc::export_ifc(&m).unwrap();
    for class in [
        "IFCCOLUMN(",
        "IFCBEAM(",
        "IFCROOF(",
        "IFCCOVERING(",
        "IFCBUILDINGELEMENTPROXY(",
    ] {
        assert!(text.contains(class), "{class}");
    }
    assert_eq!(text.matches("IFCBUILDINGELEMENTPROXY(").count(), 8);

    let mut back = Model::open(":memory:").unwrap();
    let rep = import_ifc(&mut back, text.as_bytes()).unwrap();
    assert!(
        rep.warnings.iter().all(|w| !w.contains("DDC_")),
        "{:?}",
        rep.warnings
    );
    assert_eq!(
        (rep.columns, rep.beams, rep.roofs, rep.ceilings, rep.meshes),
        (1, 1, 1, 1, 8),
        "{rep:?}"
    );
    let one = |t: &str| -> i64 {
        back.sql()
            .query_row(&format!("SELECT id FROM {t}"), [], |r| r.get(0))
            .unwrap()
    };
    let lv = |name: &str| -> i64 {
        back.sql()
            .query_row("SELECT id FROM level WHERE name=?1", [name], |r| r.get(0))
            .unwrap()
    };
    let (l1, l2) = (lv("L1"), lv("L2"));
    let c2 = back.column(one("col")).unwrap();
    assert_eq!(
        c2,
        ColumnSpec {
            level: l1,
            top_level: Some(l2),
            member_type: c2.member_type,
            ..col
        }
    );
    let b2 = back.beam(one("beam")).unwrap();
    assert_eq!(
        b2,
        BeamSpec {
            level: l1,
            member_type: b2.member_type,
            ..beam
        }
    );
    let types = back.member_types().unwrap();
    assert!(types.iter().any(|t| t.1 == "C30" && t.3 == "concrete"));
    assert!(types.iter().any(|t| t.1 == "I300" && t.3 == "steel"));
    let r2 = back.roof(one("roof")).unwrap();
    assert_eq!(
        r2,
        RoofSpec {
            level: l1,
            slab_type: r2.slab_type,
            ..roof
        }
    );
    let roof_type: String = back
        .sql()
        .query_row(
            "SELECT name FROM slab_type WHERE id=?1",
            [r2.slab_type],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(roof_type, "roof 250");
    let cid = one("ceiling");
    let h: f64 = back
        .sql()
        .query_row("SELECT height FROM ceiling WHERE id=?1", [cid], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(close(h, 2.7), "{h}");
    let rings = back.ceiling_rings(cid).unwrap();
    assert_eq!(rings.len(), 2, "{rings:?}");
    let (ma, mb) = (
        m.imported_meshes(None).unwrap(),
        back.imported_meshes(None).unwrap(),
    );
    assert_eq!(ma.len(), mb.len());
    for (x, y) in ma.iter().zip(&mb) {
        assert_eq!((&x.name, &x.category), (&y.name, &y.category));
        assert_eq!(x.mesh.triangles, y.mesh.triangles);
        for (p, q) in x.mesh.positions.iter().zip(&y.mesh.positions) {
            assert!(close(p.x, q.x) && close(p.y, q.y) && close(p.z, q.z));
        }
    }
}

// ---- re-import is a merge with the last import (src_*), not an overwrite ----

fn wall_height(m: &Model) -> f64 {
    m.sql()
        .query_row("SELECT height FROM wall", [], |r| r.get(0))
        .unwrap()
}

/// `n` walls 3 m apart; element GlobalIds start after `g0`.
fn many_walls(n: usize, g0: u32, h: f64) -> String {
    let mut b = B::new();
    b.g = g0;
    for i in 0..n {
        let y = 3.0 * i as f64;
        let pr = b.rect([2.5, y], 5.0, 0.3);
        let pos = b.axis3([0.0; 3], None, None);
        let x = b.extrude(pr, pos, h);
        b.body("IFCWALL", "w", [0.0, 0.0, 0.0], &[x], ",$,$");
    }
    b.finish("IFC4")
}

#[test]
fn local_edit_survives_reimport() {
    let mut m = Model::open(":memory:").unwrap();
    import_ifc(&mut m, reimport_src(3.0, true).as_bytes()).unwrap();
    assert!(count(&m, "src_base") >= 2, "the import wrote a base");
    assert_eq!(count(&m, "src_file"), 1);
    // edited here, file unchanged: the edit stays
    m.sql().execute("UPDATE wall SET height=4.0", []).unwrap();
    let r = import_ifc(&mut m, reimport_src(3.0, true).as_bytes()).unwrap();
    assert_eq!(
        wall_height(&m),
        4.0,
        "re-import must not overwrite a local edit"
    );
    assert!(r.kept_local >= 1);
    assert_eq!(r.conflicts, 0);
    // edited here and in the file: a conflict, the local value stays
    let r = import_ifc(&mut m, reimport_src(3.5, true).as_bytes()).unwrap();
    assert_eq!(wall_height(&m), 4.0);
    assert!(r.conflicts >= 1);
    assert!(!m.conflicts(None).unwrap().is_empty());
    // the edit is visible as an edit of the source row
    let w: i64 = m
        .sql()
        .query_row("SELECT id FROM wall", [], |r| r.get(0))
        .unwrap();
    assert!(m.is_edited("wall", w).unwrap());
}

#[test]
fn file_only_change_is_taken_and_undone_with_the_import() {
    let mut m = Model::open(":memory:").unwrap();
    import_ifc(&mut m, reimport_src(3.0, true).as_bytes()).unwrap();
    let r = import_ifc(&mut m, reimport_src(3.5, true).as_bytes()).unwrap();
    assert_eq!(
        (wall_height(&m), r.updated, r.kept_local, r.conflicts),
        (3.5, 1, 0, 0)
    );
    m.undo().unwrap();
    assert_eq!(wall_height(&m), 3.0);
}

#[test]
fn user_property_survives_reimport() {
    let mut m = Model::open(":memory:").unwrap();
    import_ifc(&mut m, reimport_src(3.0, true).as_bytes()).unwrap();
    let w: i64 = m
        .sql()
        .query_row("SELECT id FROM wall", [], |r| r.get(0))
        .unwrap();
    m.sql()
        .execute(
            "INSERT INTO prop(kind,ref,key,value,unit) VALUES ('wall',?1,'Note','mine','')",
            [w],
        )
        .unwrap();
    import_ifc(&mut m, reimport_src(3.5, true).as_bytes()).unwrap();
    assert!(
        m.props("wall", w).unwrap().iter().any(|p| p.0 == "Note"),
        "user data stays"
    );
    assert!(m
        .props("wall", w)
        .unwrap()
        .iter()
        .any(|p| p.0 == "IFC.GlobalId"));
}

#[test]
fn element_deleted_here_stays_deleted() {
    let mut m = Model::open(":memory:").unwrap();
    import_ifc(&mut m, reimport_src(3.0, true).as_bytes()).unwrap();
    m.sql().execute("DELETE FROM mesh", []).unwrap();
    let r = import_ifc(&mut m, reimport_src(3.0, true).as_bytes()).unwrap();
    assert_eq!(count(&m, "mesh"), 0, "not resurrected by the file");
    assert_eq!(r.conflicts, 0);
    assert_eq!(count(&m, "wall"), 1);
}

#[test]
fn mass_change_is_refused_unless_allowed() {
    let mut m = Model::open(":memory:").unwrap();
    import_ifc(&mut m, many_walls(24, 0, 3.0).as_bytes()).unwrap();
    assert!(count(&m, "src_base") >= ddc_model::source::GUARD_MIN as i64);
    let before = count(&m, "wall");
    let e = import_ifc(&mut m, many_walls(24, 1000, 3.0).as_bytes()).unwrap_err();
    assert!(
        e.to_string().contains(ddc_model::source::MASS_CHANGE),
        "{e}"
    );
    assert_eq!(count(&m, "wall"), before, "refused import changes nothing");
    let policy = ddc_model::source::Policy {
        allow_mass_change: true,
        ..Default::default()
    };
    import_plan(&mut m, plan_of(&many_walls(24, 1000, 3.0)), policy).unwrap();
}

#[test]
fn file_is_recorded_by_content_and_a_moved_copy_is_the_same_source() {
    use ddc_model::source::file::hash_bytes;
    let d = std::env::temp_dir().join(format!("ddc-o2-ifc-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let src = reimport_src(3.0, true);
    let a = d.join("a.ifc");
    std::fs::write(&a, &src).unwrap();
    let mut m = Model::open(":memory:").unwrap();
    import_ifc_file(&mut m, &a).unwrap();
    let s = m.sources().unwrap();
    assert_eq!(s.len(), 1);
    let f = m.source_file(&s[0].source).unwrap().unwrap();
    assert_eq!(f.content_hash, hash_bytes(src.as_bytes()));
    assert_eq!(f.size, src.len() as i64);
    assert!(f.mtime > 0 && f.path.ends_with("a.ifc") && f.format == "ifc");
    // renamed copy, edited in the file: same source, one record
    let b = d.join("renamed.ifc");
    std::fs::write(&b, reimport_src(3.5, true)).unwrap();
    import_ifc_file(&mut m, &b).unwrap();
    let s = m.sources().unwrap();
    assert_eq!((s.len(), s[0].imports), (1, 2));
    assert!(s[0].path.ends_with("renamed.ifc"));
    assert_ne!(
        m.source_file(&s[0].source).unwrap().unwrap().content_hash,
        f.content_hash
    );
    let _ = std::fs::remove_dir_all(&d);
}

fn plan_with_hosted_comp() -> Plan {
    let mut pl = plan_of(&reimport_src(3.0, false));
    pl.comps.push(PComp {
        def: "family Chair
category furniture
host wall
param W length type 1m
               ref L x ~ -400mm
ref R x ~ 400mm
ref C x = 0
eq L C R
dim W L R
               extrude B rect(L, 0, R, 1m) z(0, 1m)
type \"Chair\" W=1m
"
        .into(),
        type_name: "Chair".into(),
        type_vals: "{}".into(),
        vals: "{}".into(),
        level: 0,
        local: ([1.0, 0.5, 0.0, 0.0], false),
        world: ([1.0, 0.5, 0.0, 0.0], false),
        host: PCompHost::Wall(0),
    });
    pl.elems.push(PElem {
        kind: "comp",
        index: 0,
        guid: "0000000000000000000H01".into(),
        props: vec![],
    });
    pl
}

fn dangling_hosts(m: &Model) -> i64 {
    m.sql()
        .query_row(
            "SELECT count(*) FROM comp WHERE host_wall IS NOT NULL \
             AND host_wall NOT IN (SELECT id FROM wall)",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn component_on_a_wall_deleted_here_leaves_no_dangling_host() {
    let mut m = Model::open(":memory:").unwrap();
    import_plan(&mut m, plan_with_hosted_comp(), Default::default()).unwrap();
    assert_eq!((count(&m, "wall"), count(&m, "comp")), (1, 1));
    let w: i64 = m
        .sql()
        .query_row("SELECT id FROM wall", [], |r| r.get(0))
        .unwrap();
    // the model removes what stands on a wall together with it
    m.delete_wall(w).unwrap();
    assert_eq!(count(&m, "comp"), 0);
    // the file changed the component only: the wall stays deleted, so does what stood on it
    let mut again = plan_with_hosted_comp();
    again.comps[0].vals = "{\"a\":1}".into();
    import_plan(&mut m, again, Default::default()).unwrap();
    assert_eq!(count(&m, "wall"), 0, "the deleted wall is not resurrected");
    assert_eq!(
        count(&m, "comp"),
        0,
        "no hosted component survives without its wall"
    );
    assert_eq!(dangling_hosts(&m), 0);
    let links: i64 = m
        .sql()
        .query_row(
            "SELECT count(*) FROM ext_ref e WHERE e.kind='comp' AND e.ref NOT IN (SELECT id FROM comp)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        links <= 1,
        "only the link that records the deletion may remain"
    );
}

/// Floor slab 10 x 6 m, 0.2 m thick, top at 0; `tail` = Tag, PredefinedType.
fn floor_slab(b: &mut B, tail: &str) -> u32 {
    let pr = b.rect([5.0, 3.0], 10.0, 6.0);
    let pos = b.axis3([0.0; 3], None, None);
    let x = b.extrude(pr, pos, 0.2);
    b.body("IFCSLAB", "s", [0.0, 0.0, -0.2], &[x], tail)
}

/// Opening with body `items` at `o` voiding `host`.
fn void_in(b: &mut B, host: u32, o: [f64; 3], items: &[u32]) -> u32 {
    let o = b.body("IFCOPENINGELEMENT", "o", o, items, ",$,.OPENING.");
    b.contained.pop();
    let g = b.guid();
    b.add(&format!("IFCRELVOIDSELEMENT({g},$,$,$,#{host},#{o})"));
    o
}

/// Vertical rectangular shaft w x d centred at `c`, 1.2 m deep.
fn shaft(b: &mut B, c: [f64; 2], w: f64, d: f64) -> u32 {
    let pr = b.rect(c, w, d);
    let pos = b.axis3([0.0; 3], None, None);
    b.extrude(pr, pos, 1.2)
}

fn slab_area(m: &Model) -> (f64, f64) {
    let id: i64 = m
        .sql()
        .query_row("SELECT id FROM slab", [], |r| r.get(0))
        .unwrap();
    let q = m.slab_quantities(id).unwrap();
    (q.iter().map(|l| l.area).sum(), q.iter().map(|l| l.volume).sum())
}

#[test]
fn slab_void_opening_becomes_hole_and_reduces_area() {
    let mut b = B::new();
    let s = floor_slab(&mut b, ",$,.FLOOR.");
    let x = shaft(&mut b, [2.5, 2.75], 1.0, 1.5);
    void_in(&mut b, s, [0.0, 0.0, -0.7], &[x]);
    // an upright block, 1 x 1 at (7, 1)
    let bp = b.axis3([7.0, 1.0, -0.7], None, None);
    let blk = b.add(&format!("IFCBLOCK(#{bp},1.,1.,1.2)"));
    void_in(&mut b, s, [0.0; 3], &[blk]);
    // across the slab edge: reported, the slab stays
    let x = shaft(&mut b, [10.0, 3.0], 1.0, 1.0);
    void_in(&mut b, s, [0.0, 0.0, -0.7], &[x]);
    let src = b.finish("IFC4");
    let p = plan_of(&src);
    assert_eq!(p.slabs.len(), 1, "{:?}", p.report);
    assert_eq!(p.slabs[0].holes.len(), 2, "{:?}", p.slabs[0].holes);
    assert_eq!(p.report.host_openings, 2);
    assert!(
        p.report
            .warnings
            .iter()
            .any(|w| w.contains("across the slab edge")),
        "{:?}",
        p.report.warnings
    );
    assert_eq!(
        p.report
            .skipped
            .get("IFCOPENINGELEMENT: slab or roof opening not cut"),
        Some(&1),
        "{:?}",
        p.report.skipped
    );
    assert!(!p.report.skipped.keys().any(|k| k.contains("not a wall")));
    let mut m = Model::open(":memory:").unwrap();
    import_ifc(&mut m, src.as_bytes()).unwrap();
    let (a, v) = slab_area(&m);
    assert!((a - 57.5).abs() < 1e-6, "{a}");
    assert!((v - 57.5 * 0.2).abs() < 1e-6, "{v}");
}

#[test]
fn circular_slab_opening_becomes_polygonal_hole() {
    let mut b = B::new();
    let s = floor_slab(&mut b, ",$,.FLOOR.");
    let c = b.pos2([5.0, 3.0]);
    let pr = b.add(&format!("IFCCIRCLEPROFILEDEF(.AREA.,$,#{c},0.5)"));
    let pos = b.axis3([0.0; 3], None, None);
    let x = b.extrude(pr, pos, 1.2);
    void_in(&mut b, s, [0.0, 0.0, -0.7], &[x]);
    let src = b.finish("IFC4");
    let p = plan_of(&src);
    assert_eq!(p.slabs[0].holes.len(), 1, "{:?}", p.report);
    let h = &p.slabs[0].holes[0];
    assert!(h.len() >= 8, "{h:?}");
    assert!(
        h.iter()
            .all(|q| ((q.0 - 5.0).hypot(q.1 - 3.0) - 0.5).abs() < 1e-3),
        "{h:?}"
    );
    let mut m = Model::open(":memory:").unwrap();
    import_ifc(&mut m, src.as_bytes()).unwrap();
    let (a, _) = slab_area(&m);
    let circle = std::f64::consts::PI * 0.25;
    assert!(a < 60.0 - 0.9 * circle && a > 60.0 - circle - 1e-6, "{a}");
}

#[test]
fn roof_void_opening_is_cut() {
    // flat roof slab: a slab hole
    let mut b = B::new();
    let s = floor_slab(&mut b, ",$,.ROOF.");
    let x = shaft(&mut b, [5.0, 3.0], 1.0, 1.0);
    void_in(&mut b, s, [0.0, 0.0, -0.7], &[x]);
    let p = plan_of(&b.finish("IFC4"));
    assert_eq!(p.slabs.len(), 1, "{:?}", p.report);
    assert_eq!(p.slabs[0].holes.len(), 1);

    // sloped roof part: a host opening of the roof
    let sloped = |cut: bool| {
        let mut b = B::new();
        let pr = b.rect([5.0, 3.0], 10.0, 6.0);
        let pos = b.axis3([0.0, 0.0, 3.0], Some([-0.3, 0.0, 1.0]), None);
        let x = b.extrude(pr, pos, 0.2);
        let r = b.body("IFCSLAB", "r", [0.0; 3], &[x], ",$,.ROOF.");
        if cut {
            let x = shaft(&mut b, [4.5, 3.0], 1.0, 1.0);
            void_in(&mut b, r, [0.0, 0.0, 2.0], &[x]);
        }
        b.finish("IFC4")
    };
    let src = sloped(true);
    let p = plan_of(&src);
    assert_eq!(p.roofs.len(), 1, "{:?}", p.report);
    assert_eq!(p.roof_holes.len(), 1, "{:?}", p.report);
    assert_eq!(p.report.host_openings, 1);
    let area = |src: &str| {
        let mut m = Model::open(":memory:").unwrap();
        import_ifc(&mut m, src.as_bytes()).unwrap();
        let id: i64 = m
            .sql()
            .query_row("SELECT id FROM roof", [], |r| r.get(0))
            .unwrap();
        (m.roof_quantities(id).unwrap().layers[0].2, m)
    };
    let (a1, mut m) = area(&src);
    let (a0, _) = area(&sloped(false));
    assert_eq!(count(&m, "host_opening"), 1);
    assert!(a1 < a0 - 0.9, "{a0} {a1}");
    // a re-import does not repeat the opening
    import_ifc(&mut m, src.as_bytes()).unwrap();
    assert_eq!(count(&m, "host_opening"), 1);
    // undo removes it with the import
    let mut m = Model::open(":memory:").unwrap();
    import_ifc(&mut m, src.as_bytes()).unwrap();
    m.undo().unwrap();
    assert_eq!((count(&m, "host_opening"), count(&m, "roof")), (0, 0));
}

#[test]
fn roof_reimport_replaces_changed_ifc_opening_keeps_manual_one() {
    let sloped = |x_at: f64| {
        let mut b = B::new();
        let pr = b.rect([5.0, 3.0], 10.0, 6.0);
        let pos = b.axis3([0.0, 0.0, 3.0], Some([-0.3, 0.0, 1.0]), None);
        let x = b.extrude(pr, pos, 0.2);
        let r = b.body("IFCSLAB", "r", [0.0; 3], &[x], ",$,.ROOF.");
        let x = shaft(&mut b, [x_at, 3.0], 1.0, 1.0);
        void_in(&mut b, r, [0.0, 0.0, 2.0], &[x]);
        b.finish("IFC4")
    };
    let sketches = |m: &Model| -> Vec<String> {
        let mut st = m
            .sql()
            .prepare("SELECT data FROM sketch WHERE host_kind='host_opening' ORDER BY data")
            .unwrap();
        st.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .flatten()
            .collect()
    };
    let mut m = Model::open(":memory:").unwrap();
    import_ifc(&mut m, sloped(4.5).as_bytes()).unwrap();
    let roof: i64 = m
        .sql()
        .query_row("SELECT id FROM roof", [], |r| r.get(0))
        .unwrap();
    // an opening drawn by hand stays through the re-import
    m.sql()
        .execute(
            "INSERT INTO host_opening(host_kind, host, dormer) VALUES ('roof', ?1, 0)",
            [roof],
        )
        .unwrap();
    let manual = m.sql().last_insert_rowid();
    m.sql()
        .execute(
            "INSERT INTO sketch(host_kind, host, data) VALUES ('host_opening', ?1, 'manual')",
            [manual],
        )
        .unwrap();
    let before = sketches(&m);
    assert_eq!(before.len(), 2, "{before:?}");
    import_ifc(&mut m, sloped(6.5).as_bytes()).unwrap();
    let after = sketches(&m);
    assert_eq!(count(&m, "host_opening"), 2, "{after:?}");
    assert!(after.contains(&"manual".to_string()), "{after:?}");
    let moved: Vec<_> = after.iter().filter(|d| !before.contains(d)).collect();
    assert_eq!(moved.len(), 1, "stale opening was kept: {after:?}");
    // one undo restores the previous opening
    m.undo().unwrap();
    assert_eq!(sketches(&m), before);
}

#[test]
fn slab_opening_through_nested_boolean_results_is_cut() {
    let mut b = B::new();
    let s = floor_slab(&mut b, ",$,.FLOOR.");
    let x = shaft(&mut b, [2.5, 2.75], 1.0, 1.5);
    let r1 = b.add(&format!("IFCBOOLEANCLIPPINGRESULT(.DIFFERENCE.,#{x},#{x})"));
    let r2 = b.add(&format!("IFCBOOLEANRESULT(.DIFFERENCE.,#{r1},#{x})"));
    void_in(&mut b, s, [0.0, 0.0, -0.7], &[r2]);
    let src = b.finish("IFC4");
    let p = plan_of(&src);
    assert_eq!(p.slabs.len(), 1, "{:?}", p.report);
    assert_eq!(p.slabs[0].holes.len(), 1, "{:?}", p.report.warnings);
}

#[test]
fn inclined_slab_opening_is_reported_and_slab_kept() {
    let mut b = B::new();
    let s = floor_slab(&mut b, ",$,.FLOOR.");
    let pr = b.rect([5.0, 3.0], 1.0, 1.0);
    let pos = b.axis3([0.0; 3], Some([0.5, 0.0, 1.0]), None);
    let x = b.extrude(pr, pos, 1.2);
    void_in(&mut b, s, [0.0, 0.0, -0.7], &[x]);
    let src = b.finish("IFC4");
    let p = plan_of(&src);
    assert_eq!(p.slabs.len(), 1, "{:?}", p.report);
    assert!(p.slabs[0].holes.is_empty());
    assert_eq!(p.report.host_openings, 0);
    assert!(
        p.report
            .warnings
            .iter()
            .any(|w| w.contains("inclined opening") && w.contains("host stays whole")),
        "{:?}",
        p.report.warnings
    );
    assert_eq!(
        p.report
            .skipped
            .get("IFCOPENINGELEMENT: slab or roof opening not cut"),
        Some(&1)
    );
    let mut m = Model::open(":memory:").unwrap();
    import_ifc(&mut m, src.as_bytes()).unwrap();
    assert!((slab_area(&m).0 - 60.0).abs() < 1e-6);
}

#[test]
fn profile_voids_and_slab_openings_combine() {
    let mut b = B::new();
    let outer = b.polyline(&[
        &[0.0, 0.0],
        &[10.0, 0.0],
        &[10.0, 6.0],
        &[0.0, 6.0],
        &[0.0, 0.0],
    ]);
    let hole = b.polyline(&[
        &[1.0, 1.0],
        &[2.0, 1.0],
        &[2.0, 2.0],
        &[1.0, 2.0],
        &[1.0, 1.0],
    ]);
    let ap = b.add(&format!(
        "IFCARBITRARYPROFILEDEFWITHVOIDS(.AREA.,$,#{outer},(#{hole}))"
    ));
    let pos = b.axis3([0.0; 3], None, None);
    let x = b.extrude(ap, pos, 0.2);
    let s = b.body("IFCSLAB", "s", [0.0, 0.0, -0.2], &[x], ",$,.FLOOR.");
    // the profile void again as an opening element: already cut, not doubled
    let x = shaft(&mut b, [1.5, 1.5], 1.0, 1.0);
    void_in(&mut b, s, [0.0, 0.0, -0.7], &[x]);
    let x = shaft(&mut b, [6.0, 3.0], 1.0, 1.0);
    void_in(&mut b, s, [0.0, 0.0, -0.7], &[x]);
    let src = b.finish("IFC4");
    let p = plan_of(&src);
    assert_eq!(p.slabs[0].holes.len(), 2, "{:?}", p.report);
    assert_eq!(p.report.host_openings, 2);
    let mut m = Model::open(":memory:").unwrap();
    import_ifc(&mut m, src.as_bytes()).unwrap();
    assert!((slab_area(&m).0 - 58.0).abs() < 1e-6);
}

/// A model with a wall, a stair and its two railings, exported as the "file".
fn stair_model() -> Model {
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("L1", 0.0).unwrap();
    let l2 = m.add_level("L2", 3.5).unwrap();
    let st = m
        .add_stair(&ddc_model::StairSpec::straight(l1, l2, (0.0, 0.0), 0.0))
        .unwrap();
    m.add_stair_railings(
        st,
        &ddc_model::RailingSpec::new(l1, ddc_model::RailingHost::Stair { stair: st, side: 0 }),
    )
    .unwrap();
    m
}

/// Re-import that deletes what the file no longer has (`import_ifc` only reports it).
fn reimport_deleting(m: &mut Model, src: &str) -> ImportReport {
    let policy = ddc_model::source::Policy {
        delete_removed: true,
        ..Default::default()
    };
    import_plan(m, plan_of(src), policy).unwrap()
}

#[test]
fn removed_stair_deleted_on_reimport() {
    let mut a = stair_model();
    let with = crate::ifc::export_ifc(&a).unwrap();
    let mut m = Model::open(":memory:").unwrap();
    import_ifc(&mut m, with.as_bytes()).unwrap();
    assert_eq!((count(&m, "stair"), count(&m, "railing")), (1, 2));
    a.sql().execute("DELETE FROM railing", []).unwrap();
    a.sql().execute("DELETE FROM stair", []).unwrap();
    let without = crate::ifc::export_ifc(&a).unwrap();
    let r = reimport_deleting(&mut m, &without);
    assert_eq!((count(&m, "stair"), count(&m, "railing")), (0, 0));
    assert_eq!(count(&m, "railing_vertex"), 0);
    assert_eq!(r.conflicts, 0);
    m.undo().unwrap();
    assert_eq!((count(&m, "stair"), count(&m, "railing")), (1, 2));
}

#[test]
fn edited_removed_stair_is_conflict() {
    let mut a = stair_model();
    let with = crate::ifc::export_ifc(&a).unwrap();
    let mut m = Model::open(":memory:").unwrap();
    import_ifc(&mut m, with.as_bytes()).unwrap();
    m.sql()
        .execute("UPDATE stair SET width=width+0.1", [])
        .unwrap();
    a.sql().execute("DELETE FROM railing", []).unwrap();
    a.sql().execute("DELETE FROM stair", []).unwrap();
    let without = crate::ifc::export_ifc(&a).unwrap();
    let r = reimport_deleting(&mut m, &without);
    assert_eq!(count(&m, "stair"), 1, "edited here: not deleted");
    assert!(r.conflicts >= 1);
    assert!(!m.conflicts(None).unwrap().is_empty());
}

/// The "file" after the stair is removed but its railings stay as sketch railings in place.
fn file_stair_gone_railings_kept(a: &mut Model) -> String {
    let ids: Vec<i64> = {
        let mut st = a.sql().prepare("SELECT id FROM railing ORDER BY id").unwrap();
        let rows = st.query_map([], |r| r.get(0)).unwrap();
        rows.map(|x| x.unwrap()).collect()
    };
    for id in ids {
        // the rail axis sits at height - rail_h / 2 above the path
        let spec = a.railing(id).unwrap();
        let h = spec.height - spec.rail.1 / 2.0;
        let pts: Vec<(f64, f64, f64)> = a
            .railing_layout(id)
            .unwrap()
            .rail
            .iter()
            .map(|p| (p.x, p.y, p.z - h))
            .collect();
        a.sql()
            .execute("UPDATE railing SET stair=NULL, side=NULL WHERE id=?1", [id])
            .unwrap();
        a.set_railing_path(id, &pts).unwrap();
    }
    a.sql().execute("DELETE FROM stair", []).unwrap();
    crate::ifc::export_ifc(a).unwrap()
}

fn rail_points(m: &Model) -> Vec<Vec<(f64, f64, f64)>> {
    let ids: Vec<i64> = {
        let mut st = m.sql().prepare("SELECT id FROM railing ORDER BY id").unwrap();
        let rows = st.query_map([], |r| r.get(0)).unwrap();
        rows.map(|x| x.unwrap()).collect()
    };
    ids.iter()
        .map(|id| {
            m.railing_layout(*id)
                .unwrap()
                .rail
                .iter()
                .map(|p| (p.x, p.y, p.z))
                .collect()
        })
        .collect()
}

#[test]
fn railing_in_file_survives_stair_removal() {
    let mut a = stair_model();
    let with = crate::ifc::export_ifc(&a).unwrap();
    let mut m = Model::open(":memory:").unwrap();
    import_ifc(&mut m, with.as_bytes()).unwrap();
    let before = rail_points(&m);
    let without = file_stair_gone_railings_kept(&mut a);
    let r = reimport_deleting(&mut m, &without);
    assert_eq!((count(&m, "stair"), count(&m, "railing")), (0, 2));
    assert_eq!(count(&m, "railing_vertex") > 0, true);
    assert_eq!(r.conflicts, 0);
    let stairs: i64 = m
        .sql()
        .query_row("SELECT count(*) FROM railing WHERE stair IS NOT NULL", [], |r| r.get(0))
        .unwrap();
    assert_eq!(stairs, 0);
    let after = rail_points(&m);
    assert_eq!(before.len(), after.len());
    m.undo().unwrap();
    assert_eq!((count(&m, "stair"), count(&m, "railing")), (1, 2));
}

#[test]
fn edited_railing_of_removed_stair_is_conflict() {
    let mut a = stair_model();
    let with = crate::ifc::export_ifc(&a).unwrap();
    let mut m = Model::open(":memory:").unwrap();
    import_ifc(&mut m, with.as_bytes()).unwrap();
    m.sql()
        .execute("UPDATE railing SET height=height+0.1", [])
        .unwrap();
    a.sql().execute("DELETE FROM railing", []).unwrap();
    a.sql().execute("DELETE FROM stair", []).unwrap();
    let without = crate::ifc::export_ifc(&a).unwrap();
    let r = reimport_deleting(&mut m, &without);
    assert_eq!(count(&m, "stair"), 1, "railing edited here: stair kept");
    assert_eq!(count(&m, "railing"), 2);
    assert!(r.conflicts >= 1);
    assert!(!m.conflicts(None).unwrap().is_empty());
}

#[test]
fn edited_railing_in_file_detaches_with_edit() {
    let mut a = stair_model();
    let with = crate::ifc::export_ifc(&a).unwrap();
    let mut m = Model::open(":memory:").unwrap();
    import_ifc(&mut m, with.as_bytes()).unwrap();
    let before = rail_points(&m);
    m.sql()
        .execute("UPDATE railing SET height=height+0.1", [])
        .unwrap();
    let without = file_stair_gone_railings_kept(&mut a);
    reimport_deleting(&mut m, &without);
    assert_eq!((count(&m, "stair"), count(&m, "railing")), (0, 2));
    let sketch: i64 = m
        .sql()
        .query_row(
            "SELECT count(*) FROM railing r WHERE stair IS NULL AND height > 0.95 \
             AND (SELECT count(*) FROM railing_vertex v WHERE v.railing=r.id) >= 2",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(sketch, 2, "file path taken, local height kept");
    let after = rail_points(&m);
    for (b, n) in before.iter().zip(&after) {
        assert_eq!(b.len(), n.len());
        for (p, q) in b.iter().zip(n) {
            assert!(close(p.2 + 0.1, q.2), "{p:?} vs {q:?}");
        }
    }
    m.undo().unwrap();
    assert_eq!((count(&m, "stair"), count(&m, "railing")), (1, 2));
}

#[test]
fn detached_railing_keeps_absolute_z() {
    let mut a = stair_model();
    let with = crate::ifc::export_ifc(&a).unwrap();
    let mut m = Model::open(":memory:").unwrap();
    import_ifc(&mut m, with.as_bytes()).unwrap();
    let before = rail_points(&m);
    let top = before.iter().flatten().map(|p| p.2).fold(f64::MIN, f64::max);
    assert!(top > 3.0, "railing climbs to the upper level");
    let without = file_stair_gone_railings_kept(&mut a);
    reimport_deleting(&mut m, &without);
    let after = rail_points(&m);
    assert_eq!(before.len(), after.len());
    for (b, n) in before.iter().zip(&after) {
        assert_eq!(b.len(), n.len());
        for (p, q) in b.iter().zip(n) {
            assert!(close(p.0, q.0) && close(p.1, q.1) && close(p.2, q.2), "{p:?} vs {q:?}");
        }
    }
}

/// Railings hosted by the only stair of `m` (and the total railing count).
fn rails_on_stair(m: &Model) -> (i64, i64) {
    m.sql()
        .query_row(
            "SELECT (SELECT count(*) FROM railing r JOIN stair s ON s.id=r.stair), \
             (SELECT count(*) FROM railing)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
}

/// Imports `with`, edits the stair here, imports `without` (stair gone) and takes the file's side.
fn take_file_for_stair(a: &mut Model, without: impl FnOnce(&mut Model) -> String) -> Model {
    let with = crate::ifc::export_ifc(a).unwrap();
    let mut m = Model::open(":memory:").unwrap();
    import_ifc(&mut m, with.as_bytes()).unwrap();
    assert_eq!(rails_on_stair(&m), (2, 2), "imported railings are attached to their stair");
    m.sql().execute("UPDATE stair SET width=width+0.1", []).unwrap();
    let file = without(a);
    reimport_deleting(&mut m, &file);
    let c = m.conflicts(None).unwrap();
    let c = c.iter().find(|c| c.kind == "stair" && c.field == "*").expect("stair conflict");
    m.resolve_conflicts(&[(c.id, ddc_model::source::Pick::Theirs)]).unwrap();
    m
}

#[test]
fn take_file_keeps_railings_in_file() {
    let mut a = stair_model();
    let mut m = take_file_for_stair(&mut a, file_stair_gone_railings_kept);
    assert_eq!((count(&m, "stair"), count(&m, "railing")), (0, 2));
    let hosted: i64 = m
        .sql()
        .query_row("SELECT count(*) FROM railing WHERE stair IS NOT NULL", [], |r| r.get(0))
        .unwrap();
    assert_eq!(hosted, 0);
    assert!(count(&m, "railing_vertex") > 0);
    m.undo().unwrap();
    assert_eq!((count(&m, "stair"), count(&m, "railing")), (1, 2));
    // the choice detached them: its one undo step puts them back on their stair
    assert_eq!(rails_on_stair(&m), (2, 2));
}

/// While the stair waits for the choice its railings stay on it; keeping ours keeps them there.
#[test]
fn railings_wait_on_a_conflicted_stair() {
    let mut a = stair_model();
    let with = crate::ifc::export_ifc(&a).unwrap();
    let mut m = Model::open(":memory:").unwrap();
    import_ifc(&mut m, with.as_bytes()).unwrap();
    m.sql().execute("UPDATE stair SET width=width+0.1", []).unwrap();
    let file = file_stair_gone_railings_kept(&mut a);
    reimport_deleting(&mut m, &file);
    assert_eq!(count(&m, "stair"), 1, "edited here: kept for the choice");
    assert_eq!(rails_on_stair(&m), (2, 2), "the re-import leaves them on their stair");
    let c = m.conflicts(None).unwrap();
    let c = c.iter().find(|c| c.kind == "stair" && c.field == "*").expect("stair conflict");
    m.resolve_conflicts(&[(c.id, ddc_model::source::Pick::Mine)]).unwrap();
    assert_eq!(rails_on_stair(&m), (2, 2), "keeping the stair keeps its railings");
    // the same file again leaves them there
    reimport_deleting(&mut m, &file);
    assert_eq!(rails_on_stair(&m), (2, 2));
}

#[test]
fn take_file_drops_railings_not_in_file() {
    let mut a = stair_model();
    let m = take_file_for_stair(&mut a, |a| {
        a.sql().execute("DELETE FROM railing", []).unwrap();
        a.sql().execute("DELETE FROM stair", []).unwrap();
        crate::ifc::export_ifc(a).unwrap()
    });
    assert_eq!((count(&m, "stair"), count(&m, "railing")), (0, 0));
}

#[test]
fn take_file_for_stair_keeps_railing_edited_here() {
    // railings edited here and dropped by the file have their own conflict: taking the
    // file's side of the stair must not decide it for them
    let mut a = stair_model();
    let with = crate::ifc::export_ifc(&a).unwrap();
    let mut m = Model::open(":memory:").unwrap();
    import_ifc(&mut m, with.as_bytes()).unwrap();
    m.sql().execute("UPDATE stair SET width=width+0.1", []).unwrap();
    m.sql().execute("UPDATE railing SET height=height+0.1", []).unwrap();
    a.sql().execute("DELETE FROM railing", []).unwrap();
    a.sql().execute("DELETE FROM stair", []).unwrap();
    let file = crate::ifc::export_ifc(&a).unwrap();
    reimport_deleting(&mut m, &file);
    let cs = m.conflicts(None).unwrap();
    let c = cs.iter().find(|c| c.kind == "stair" && c.field == "*").expect("stair conflict");
    m.resolve_conflicts(&[(c.id, ddc_model::source::Pick::Theirs)]).unwrap();
    assert_eq!((count(&m, "stair"), count(&m, "railing")), (0, 2));
    assert!(m.conflicts(None).unwrap().iter().any(|c| c.kind == "railing"));
}

#[test]
fn detached_railing_save_reimport_stable() {
    let mut a = stair_model();
    let mut m = take_file_for_stair(&mut a, file_stair_gone_railings_kept);
    let before = rail_points(&m);
    let out = crate::ifc::export_ifc(&m).unwrap();
    let mut n = Model::open(":memory:").unwrap();
    import_ifc(&mut n, out.as_bytes()).unwrap();
    assert_eq!(count(&n, "railing"), 2);
    for (b, g) in before.iter().zip(&rail_points(&n)) {
        for (p, q) in b.iter().zip(g) {
            assert!(close(p.0, q.0) && close(p.1, q.1) && close(p.2, q.2), "{p:?} vs {q:?}");
        }
    }
    // importing the same file again into the original model neither duplicates nor loses them
    let r = import_ifc(&mut m, out.as_bytes()).unwrap();
    assert_eq!((count(&m, "railing"), r.conflicts), (2, 0));
}

#[test]
fn foreign_mesh_stair_untouched() {
    let mut b = B::new();
    let t = b.tri_cube();
    let pl = b.place([1.0, 1.0, 0.0]);
    let rp = b.rep("Body", "Tessellation", &[t]);
    b.product("IFCSTAIR", "s", pl, &[rp], ",$,$");
    let src = b.finish("IFC4");
    let mut m = Model::open(":memory:").unwrap();
    import_ifc(&mut m, src.as_bytes()).unwrap();
    assert_eq!((count(&m, "mesh"), count(&m, "stair")), (1, 0));
    // a stair drawn here has no link to the file
    let l1 = m.add_level("L9", 0.0).unwrap();
    let l2 = m.add_level("L10", 3.0).unwrap();
    m.add_stair(&ddc_model::StairSpec::straight(l1, l2, (0.0, 0.0), 0.0))
        .unwrap();
    import_ifc(&mut m, src.as_bytes()).unwrap();
    assert_eq!((count(&m, "mesh"), count(&m, "stair")), (1, 1));
}

/// Furniture type with a map and a property set; two occurrences map it, one turned 90°.
fn typed_maps(class: &str) -> B {
    let mut b = B::new();
    let t = b.tri_cube();
    let mr = b.rep("Body", "Tessellation", &[t]);
    let mo = b.axis3([0.0; 3], None, None);
    let map = b.add(&format!("IFCREPRESENTATIONMAP(#{mo},#{mr})"));
    let pv = b.add("IFCPROPERTYSINGLEVALUE('Mark',$,IFCLABEL('T-7'),$)");
    let g = b.guid();
    let ps = b.add(&format!("IFCPROPERTYSET({g},$,'Pset_Maker',$,(#{pv}))"));
    let g = b.guid();
    let ty = b.add(&format!(
        "{class}TYPE({g},$,'Crate 1m',$,$,(#{ps}),(#{map}),$,$,.NOTDEFINED.)"
    ));
    let mut occ = vec![];
    for (x, turned) in [(5.0, false), (9.0, true)] {
        let o = b.point(&[0.0, 0.0, 0.0]);
        let ax = if turned {
            let d = b.dir(&[0.0, 1.0, 0.0]);
            format!("#{d}")
        } else {
            "$".into()
        };
        let op = b.add(&format!("IFCCARTESIANTRANSFORMATIONOPERATOR3D({ax},$,#{o},$,$)"));
        let mi = b.add(&format!("IFCMAPPEDITEM(#{map},#{op})"));
        occ.push(b.body(class, "crate", [x, 0.0, 0.0], &[mi], ",$,$"));
    }
    let l: Vec<String> = occ.iter().map(|i| format!("#{i}")).collect();
    let g = b.guid();
    b.add(&format!("IFCRELDEFINESBYTYPE({g},$,$,$,({}),#{ty})", l.join(",")));
    b
}

#[test]
fn foreign_type_maps_become_one_family_with_placed_instances() {
    let src = typed_maps("IFCFURNITURE").finish("IFC4");
    let p = plan_of(&src);
    assert_eq!(p.comps.len(), 2, "{:?}", p.report.as_mesh);
    assert!(p.meshes.is_empty());
    assert_eq!(p.report.type_families, 2);
    assert_eq!(p.comps[0].def, p.comps[1].def, "one family per map");
    let f = ddc_geom::component::Family::parse(&p.comps[0].def).unwrap();
    assert_eq!(f.name, "Crate 1m");
    assert_eq!(f.category, ddc_geom::component::Category::Furniture);
    assert_eq!(f.ifc.0, "IfcFurniture");
    assert!(f.param("Pset_Maker_Mark").is_some());
    let (a, b) = (p.comps[0].world.0, p.comps[1].world.0);
    assert!((a[0] - 5.0).abs() < 1e-9 && a[3].abs() < 1e-9, "{a:?}");
    assert!((b[0] - 9.0).abs() < 1e-9, "{b:?}");
    // the operator's x axis (0,1,0) turns the instance by 90°
    assert!((b[3] - std::f64::consts::FRAC_PI_2).abs() < 1e-9, "{b:?}");
    let mut m = Model::open(":memory:").unwrap();
    import_ifc(&mut m, src.as_bytes()).unwrap();
    assert_eq!(count(&m, "comp_family"), 1);
    assert_eq!(count(&m, "comp"), 2);
    let q = m.component_quantities(m.components(None).unwrap()[0]).unwrap();
    assert!((q.volume - 1.0).abs() < 1e-6, "{}", q.volume);
}

#[test]
fn type_drafts_offer_doors_as_hosted_openings() {
    let src = typed_maps("IFCDOOR").finish("IFC4");
    let f = StepFile::parse(src.as_bytes()).unwrap();
    let d = super::type_drafts(&f).unwrap();
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].class, "IFCDOOR");
    assert_eq!(d[0].spec.hosting, ddc_geom::component::Hosting::Wall);
    let text = ddc_geom::component::from_mesh::family_text(&d[0].parts, &d[0].spec).unwrap();
    let fam = ddc_geom::component::Family::parse(&text).unwrap();
    assert!(fam.param("Sill").is_some() && fam.solids.iter().any(|s| s.void));
    assert_eq!(fam.ifc.0, "IfcDoor");
}

/// One triangulated face set of closed axis-aligned boxes (low, high corners).
fn boxes(b: &mut B, bs: &[([f64; 3], [f64; 3])]) -> u32 {
    const C: [[usize; 3]; 8] = [
        [0, 0, 0],
        [1, 0, 0],
        [1, 1, 0],
        [0, 1, 0],
        [0, 0, 1],
        [1, 0, 1],
        [1, 1, 1],
        [0, 1, 1],
    ];
    const T: [[usize; 3]; 12] = [
        [1, 3, 2],
        [1, 4, 3],
        [5, 6, 7],
        [5, 7, 8],
        [1, 2, 6],
        [1, 6, 5],
        [2, 3, 7],
        [2, 7, 6],
        [3, 4, 8],
        [3, 8, 7],
        [4, 1, 5],
        [4, 5, 8],
    ];
    let (mut pts, mut tris) = (vec![], vec![]);
    for (k, (lo, hi)) in bs.iter().enumerate() {
        for c in C {
            let p: Vec<f64> = (0..3).map(|i| if c[i] == 0 { lo[i] } else { hi[i] }).collect();
            pts.push(format!("({})", pt(&p)));
        }
        for t in T {
            let i: Vec<String> = t.iter().map(|x| (x + 8 * k).to_string()).collect();
            tris.push(format!("({})", i.join(",")));
        }
    }
    let p = b.add(&format!("IFCCARTESIANPOINTLIST3D(({}))", pts.join(",")));
    b.add(&format!(
        "IFCTRIANGULATEDFACESET(#{p},$,.T.,({}),$)",
        tris.join(",")
    ))
}

#[test]
fn tessellated_slab_prism_is_a_slab_and_a_stepped_one_a_listed_mesh() {
    let mut b = B::new();
    let x = boxes(&mut b, &[([0.0, 0.0, -0.2], [6.0, 4.0, 0.0])]);
    b.body("IFCSLAB", "flat", [0.0; 3], &[x], ",$,.FLOOR.");
    let x = boxes(
        &mut b,
        &[([10.0, 0.0, -0.4], [12.0, 2.0, -0.2]), ([10.0, 0.0, -0.2], [11.0, 1.0, 0.0])],
    );
    b.body("IFCSLAB", "stepped", [0.0; 3], &[x], ",$,.FLOOR.");
    let src = b.finish("IFC4");
    let p = plan_of(&src);
    assert_eq!((p.slabs.len(), p.meshes.len()), (1, 1), "{:?}", p.report);
    let s = &p.slabs[0];
    assert_eq!(s.offset, 0.0);
    assert!((fit::area(&s.outer) - 24.0).abs() < 1e-9, "{:?}", s.outer);
    assert_eq!(p.slab_types[s.slab_type].layers[0].1, 0.2);
    let why = "tessellated body is not a planar prism";
    assert_eq!(p.report.as_mesh.get(&format!("IFCSLAB: {why}")), Some(&1));
    assert!(p
        .report
        .not_native()
        .contains(&("IFCSLAB".into(), 1, format!("mesh: {why}"))));
    let mut m = Model::open(":memory:").unwrap();
    import_ifc(&mut m, src.as_bytes()).unwrap();
    assert_eq!((count(&m, "slab"), count(&m, "mesh")), (1, 1));
}

#[test]
fn tessellated_and_sideways_spaces_are_rooms() {
    let mut b = B::new();
    let x = boxes(&mut b, &[([0.0, 0.0, 0.0], [4.0, 3.0, 2.7])]);
    b.body("IFCSPACE", "1", [0.0; 3], &[x], ",'Bedroom',.ELEMENT.,.SPACE.,$");
    // profile in the vertical plane, extruded along x
    let pr = b.rect([2.0, 1.35], 4.0, 2.7);
    let pos = b.axis3([10.0, 0.0, 0.0], Some([1.0, 0.0, 0.0]), Some([0.0, 1.0, 0.0]));
    let x = b.extrude(pr, pos, 3.0);
    b.body("IFCSPACE", "2", [0.0; 3], &[x], ",'Stair',.ELEMENT.,.SPACE.,$");
    let p = plan_of(&b.finish("IFC4"));
    assert_eq!(p.rooms.len(), 2, "{:?}", p.report);
    for (r, a) in p.rooms.iter().zip([12.0, 12.0]) {
        assert!((r.height - 2.7).abs() < 1e-9, "{r:?}");
        assert!((fit::area(&r.ring) - a).abs() < 1e-9, "{r:?}");
    }
    assert!(p.report.skipped.is_empty(), "{:?}", p.report.skipped);
}

#[test]
fn a_window_in_a_slab_opening_is_a_mesh_with_its_reason() {
    let mut b = B::new();
    let s = floor_slab(&mut b, ",$,.FLOOR.");
    let x = shaft(&mut b, [2.5, 2.75], 1.0, 1.0);
    let o = void_in(&mut b, s, [0.0, 0.0, -0.7], &[x]);
    let t = b.tri_cube();
    let w = b.body("IFCWINDOW", "sky", [2.0, 2.25, -0.2], &[t], ",$,1.,1.,.SKYLIGHT.,$,$");
    let g = b.guid();
    b.add(&format!("IFCRELFILLSELEMENT({g},$,$,$,#{o},#{w})"));
    let p = plan_of(&b.finish("IFC4"));
    assert_eq!((p.slabs.len(), p.meshes.len()), (1, 1), "{:?}", p.report);
    let why = "fills a slab or roof opening; doors and windows are wall-hosted";
    assert_eq!(p.report.as_mesh.get(&format!("IFCWINDOW: {why}")), Some(&1));
}

/// Stair (no body) of one 10-riser flight body stopping a riser short, a stringer and a rail
/// 0.9 m above the nosings on the y = 0 edge.
fn fitted_stair() -> String {
    let mut b = B::new();
    let steps: Vec<([f64; 3], [f64; 3])> = (0..9)
        .map(|i| {
            let x = i as f64 * 0.28;
            ([x, 0.0, 0.0], [x + 0.28, 1.2, (i + 1) as f64 * 0.17])
        })
        .collect();
    let fl = boxes(&mut b, &steps);
    let fl = b.body("IFCSTAIRFLIGHT", "f", [0.0; 3], &[fl], ",$,10,9,0.17,0.28,.STRAIGHT.");
    let sg = boxes(&mut b, &[([0.0, 1.2, 0.0], [2.52, 1.25, 1.53])]);
    let sg = b.body("IFCMEMBER", "s", [0.0; 3], &[sg], ",$,.STRINGER.");
    let rl = boxes(&mut b, &[([0.0, -0.025, 1.5], [2.52, 0.025, 2.43])]);
    let rl = b.body("IFCRAILING", "r", [0.0; 3], &[rl], ",$,.HANDRAIL.");
    b.contained.truncate(b.contained.len() - 3);
    let pl = b.place([0.0; 3]);
    let st = b.product("IFCSTAIR", "st", pl, &[], ",$,.STRAIGHT_RUN_STAIR.");
    let g = b.guid();
    b.add(&format!(
        "IFCRELAGGREGATES({g},$,$,$,#{st},(#{fl},#{sg},#{rl}))"
    ));
    b.finish("IFC4")
}

#[test]
fn a_stair_with_stringer_and_railing_is_fitted_and_the_railing_hosted() {
    let src = fitted_stair();
    let p = plan_of(&src);
    assert_eq!((p.stairs.len(), p.railings.len()), (1, 1), "{:?}", p.report);
    assert_eq!(p.meshes.len(), 1, "the stringer stays a mesh");
    assert_eq!(
        p.report.as_mesh.get("IFCMEMBER: stringer of a fitted stair"),
        Some(&1)
    );
    let r = &p.railings[0];
    assert_eq!(r.host.map(|h| h.0), Some(0));
    assert!(r.spec.height > 0.7 && r.spec.height <= 0.9 + 1e-9, "{}", r.spec.height);
    let mut m = Model::open(":memory:").unwrap();
    let rep = import_ifc(&mut m, src.as_bytes()).unwrap();
    assert_eq!((rep.stairs, rep.railings), (1, 1), "{:?}", rep.warnings);
    assert_eq!((count(&m, "stair"), count(&m, "railing")), (1, 1));
}

#[test]
fn tessellated_footings_are_native_footings() {
    let mut b = B::new();
    let x = boxes(
        &mut b,
        &[([-0.9, -0.9, -1.0], [0.9, 0.9, -0.7]), ([-0.5, -0.5, -0.7], [0.5, 0.5, -0.4])],
    );
    b.body("IFCFOOTING", "pad", [5.0, 5.0, 0.0], &[x], ",$,.PAD_FOOTING.");
    let x = boxes(&mut b, &[([0.0, -0.3, -0.4], [6.0, 0.3, 0.0])]);
    b.body("IFCFOOTING", "strip", [0.0; 3], &[x], ",$,.STRIP_FOOTING.");
    let src = b.finish("IFC4");
    let p = plan_of(&src);
    assert_eq!(
        (p.footings.len(), p.strip_footings.len(), p.meshes.len()),
        (1, 1, 0),
        "{:?}",
        p.report
    );
    let f = &p.footings[0].spec;
    assert_eq!(f.at, (5.0, 5.0));
    assert_eq!(f.top_offset, -0.4);
    let st: Vec<_> = f.steps.iter().map(|s| (s.w, s.l, s.h)).collect();
    assert_eq!(st, vec![(1.8, 1.8, 0.3), (1.0, 1.0, 0.3)]);
    let s = &p.strip_footings[0].spec;
    assert_eq!((s.a, s.b, s.width, s.depth), ((0.0, 0.0), (6.0, 0.0), 0.6, 0.4));
    let mut m = Model::open(":memory:").unwrap();
    let rep = import_ifc(&mut m, src.as_bytes()).unwrap();
    assert_eq!(rep.footings, 2);
    assert_eq!((count(&m, "iso_footing"), count(&m, "strip_footing")), (1, 1));
}

/// Own export of five levels with footings comes back with five levels and native footings.
#[test]
fn own_export_round_trips_levels_and_footings() {
    use ddc_model::{FootingSpec, FootingStep, StripFootingSpec};
    let mut src = Model::open(":memory:").unwrap();
    let lv: Vec<i64> = (0..5)
        .map(|i| src.add_level(&format!("L{}", i + 1), i as f64 * 3.6).unwrap())
        .collect();
    let step = |w: f64, l: f64| FootingStep { w, l, h: 0.3 };
    let pad = |at: (f64, f64), rotation: f64, steps: Vec<FootingStep>| FootingSpec {
        level: lv[0],
        column: None,
        at,
        rotation,
        round: false,
        steps,
        top_offset: -0.1,
        material: "Concrete".into(),
    };
    src.add_footings(&[
        pad((0.0, 0.0), 0.3, vec![step(1.8, 1.2), step(1.0, 0.6)]),
        pad((6.0, 0.0), 0.0, vec![step(1.5, 1.5)]),
    ])
    .unwrap();
    src.add_strip_footings(&[StripFootingSpec {
        level: lv[0],
        wall: None,
        a: (0.0, 5.0),
        b: (8.0, 5.0),
        bulge: 0.0,
        width: 0.6,
        depth: 0.4,
        ecc: 0.0,
        top_offset: 0.0,
        material: "Concrete".into(),
    }])
    .unwrap();
    let text = crate::ifc::export_ifc(&src).unwrap();
    assert_eq!(text.matches("=IFCBUILDINGSTOREY(").count(), 5);
    assert_eq!(text.matches("=IFCFOOTING(").count(), 3);
    let mut dst = Model::open(":memory:").unwrap();
    let rep = import_ifc(&mut dst, text.as_bytes()).unwrap();
    assert_eq!((rep.levels, rep.footings, rep.meshes), (5, 3, 0), "{:?}", rep.as_mesh);
    assert_eq!(count(&dst, "level"), 5);
    assert_eq!((count(&dst, "iso_footing"), count(&dst, "strip_footing")), (2, 1));
    let vol = |m: &Model| -> f64 {
        m.struct_part_bodies(None).unwrap().iter().map(|p| p.volume).sum()
    };
    assert!((vol(&src) - vol(&dst)).abs() < 1e-6, "{} {}", vol(&src), vol(&dst));
    let ids = |m: &Model| -> Vec<i64> {
        let mut st = m.sql().prepare("SELECT id FROM iso_footing ORDER BY x").unwrap();
        st.query_map([], |r| r.get(0)).unwrap().flatten().collect()
    };
    for (a, b) in ids(&src).into_iter().zip(ids(&dst)) {
        let (a, b) = (src.footing(a).unwrap(), dst.footing(b).unwrap());
        assert_eq!(a.steps, b.steps);
        assert!((a.rotation - b.rotation).abs() < 1e-9, "{} {}", a.rotation, b.rotation);
        assert!((a.at.0 - b.at.0).abs() < 1e-9 && (a.at.1 - b.at.1).abs() < 1e-9);
        assert!((a.top_offset - b.top_offset).abs() < 1e-9);
        assert_eq!(b.material, "Concrete");
    }
    // a second import of the same file updates in place
    import_ifc(&mut dst, text.as_bytes()).unwrap();
    assert_eq!((count(&dst, "iso_footing"), count(&dst, "strip_footing")), (2, 1));
    assert_eq!(count(&dst, "level"), 5);
}
